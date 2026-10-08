use super::lexer::{self, Kind, Token};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Component, Path, PathBuf},
    time::Instant,
};

#[derive(Clone, Debug)]
pub(super) struct Macro {
    pub name: String,
    pub args: usize,
    pub default: Option<Vec<Token>>,
    pub body: Vec<Token>,
    pub raw: String,
    pub expandable: bool,
    pub environment: bool,
}

pub(super) type Macros = BTreeMap<String, Macro>;

pub(super) struct Expanded {
    pub tokens: Vec<Token>,
    pub macros: Macros,
    pub warnings: Vec<String>,
    pub main: PathBuf,
    pub root: PathBuf,
}

pub(super) fn definition(tokens: &[Token], start: usize) -> Option<(Macro, usize)> {
    let Kind::Command(command) = &tokens.get(start)?.kind else {
        return None;
    };
    let new_command = [
        "newcommand",
        "renewcommand",
        "providecommand",
        "DeclareRobustCommand",
    ]
    .contains(&command.as_str());
    if !new_command && !["def", "gdef", "edef", "xdef"].contains(&command.as_str()) {
        return None;
    }
    let mut i = lexer::skip_space(tokens, start + 1);
    if tokens.get(i).is_some_and(|t| t.kind == Kind::Char('*')) {
        i += 1;
        i = lexer::skip_space(tokens, i);
    }
    let name = if let Some((name, end)) = lexer::group(tokens, i) {
        let Kind::Command(name) = &name.first()?.kind else {
            return None;
        };
        i = end;
        name.clone()
    } else {
        let Kind::Command(name) = &tokens.get(i)?.kind else {
            return None;
        };
        let name = name.clone();
        i += 1;
        name
    };
    let mut args = 0;
    let mut default = None;
    // A providecommand fallback may never run: classes and installed packages
    // can already define the command. Retain that decision for the compiler.
    let mut expandable = command != "providecommand";
    if new_command {
        if let Some((count, end)) = lexer::optional(tokens, i) {
            args = lexer::source(count).trim().parse().ok()?;
            i = end;
        }
        if let Some((value, end)) = lexer::optional(tokens, i) {
            default = Some(value.to_vec());
            i = end;
        }
    } else {
        while tokens
            .get(lexer::skip_space(tokens, i))
            .is_some_and(|t| t.kind != Kind::Open)
        {
            i = lexer::skip_space(tokens, i);
            if tokens.get(i)?.kind == Kind::Char('#') {
                let Kind::Char(c) = tokens.get(i + 1)?.kind else {
                    return None;
                };
                let n = c.to_digit(10)? as usize;
                expandable &= n == args + 1;
                args = args.max(n);
                i += 2;
            } else {
                expandable = false;
                i += 1;
            }
        }
        expandable &= command != "edef" && command != "xdef";
    }
    if args > 9 {
        return None;
    }
    let (body, end) = lexer::group(tokens, i)?;
    // Internal control sequences retain the catcodes they had when defined.
    // Writing them into the document body would tokenize @ differently, so
    // leave their execution to TeX, including those in optional defaults.
    expandable &= !body.iter().chain(default.iter().flatten()).any(|token| {
        matches!(
            &token.kind,
            Kind::Command(name) if name.contains('@') || name.starts_with("if") || [
                "else", "fi", "csname", "endcsname", "catcode", "newcommand",
                "renewcommand", "def", "edef", "gdef", "xdef", "global", "let",
                "futurelet", "loop", "repeat", "write", "input", "include",
                "begin", "end", "newcount", "advance",
            ].contains(&name.as_str())
        )
    });
    // Math shifts can legitimately be stored across separate definitions. Such
    // macros must be executed by TeX, rather than expanded as standalone prose.
    expandable &= lexer::tokens(&lexer::source(body), &tokens[start].source.file).is_ok();
    Some((
        Macro {
            name,
            args,
            default,
            body: body.to_vec(),
            raw: lexer::source(&tokens[start..end]),
            expandable,
            environment: false,
        },
        end,
    ))
}

pub(super) fn environment_definition(
    tokens: &[Token],
    start: usize,
) -> Option<([Macro; 2], usize)> {
    if !["newenvironment", "renewenvironment", "provideenvironment"]
        .iter()
        .any(|name| tokens.get(start).is_some_and(|token| token.command(name)))
    {
        return None;
    }
    let mut i = lexer::skip_space(tokens, start + 1);
    if tokens
        .get(i)
        .is_some_and(|token| token.kind == Kind::Char('*'))
    {
        i += 1;
    }
    let (name, end) = lexer::group(tokens, i)?;
    let name = lexer::source(name).trim().to_owned();
    i = end;
    let mut args = 0;
    let mut default = None;
    if let Some((count, end)) = lexer::optional(tokens, i) {
        args = lexer::source(count).trim().parse().ok()?;
        i = end;
    }
    if let Some((value, end)) = lexer::optional(tokens, i) {
        default = Some(value.to_vec());
        i = end;
    }
    let (begin, end) = lexer::group(tokens, i)?;
    let (finish, end) = lexer::group(tokens, end)?;
    Some((
        [
            Macro {
                name: name.clone(),
                args,
                default,
                body: begin.to_vec(),
                raw: lexer::source(&tokens[start..end]),
                expandable: false,
                environment: true,
            },
            Macro {
                name: format!("end{name}"),
                args: 0,
                default: None,
                body: finish.to_vec(),
                raw: String::new(),
                expandable: false,
                environment: false,
            },
        ],
        end,
    ))
}

pub(super) fn declaration_end(tokens: &[Token], start: usize) -> Option<usize> {
    definition(tokens, start)
        .map(|(_, end)| end)
        .or_else(|| environment_definition(tokens, start).map(|(_, end)| end))
}

#[derive(Clone, Copy)]
enum SourceKind {
    Document,
    Package,
}

struct Processor {
    root: PathBuf,
    cwd: PathBuf,
    stack: Vec<PathBuf>,
    macros: Macros,
    calls: Vec<String>,
    packages: HashSet<PathBuf>,
    warnings: Vec<String>,
    bytes: usize,
    emitted: usize,
    deadline: Instant,
}

impl Processor {
    fn check(&self) -> Result<()> {
        ensure!(
            Instant::now() < self.deadline,
            "LaTeX source comparison exceeded its timeout"
        );
        Ok(())
    }

    fn read(&mut self, path: &Path, kind: SourceKind) -> Result<Vec<Token>> {
        self.check()?;
        let actual = fs::canonicalize(path)
            .with_context(|| format!("reading included source {}", path.display()))?;
        ensure!(
            actual.starts_with(&self.root),
            "included source escapes the repository: {}",
            path.display()
        );
        ensure!(
            !self.stack.contains(&actual),
            "cyclic LaTeX input: {}",
            self.stack
                .iter()
                .chain(std::iter::once(&actual))
                .map(|p| p
                    .strip_prefix(&self.root)
                    .unwrap_or(p)
                    .display()
                    .to_string())
                .collect::<Vec<_>>()
                .join(" → ")
        );
        ensure!(
            self.stack.len() < 64,
            "LaTeX inputs are nested more than 64 levels"
        );
        let data = fs::read(&actual)?;
        self.bytes += data.len();
        ensure!(
            data.len() <= 16 * 1024 * 1024 && self.bytes <= 128 * 1024 * 1024,
            "LaTeX source exceeds the comparison size limit"
        );
        let source = std::str::from_utf8(&data).with_context(|| {
            format!(
                "{} is not UTF-8; convert the source encoding before comparison",
                path.display()
            )
        })?;
        let relative = actual.strip_prefix(&self.root)?;
        let mut tokens = match kind {
            SourceKind::Document => lexer::tokens(source, relative)?,
            SourceKind::Package => lexer::package_tokens(source, relative)?,
        };
        for token in &mut tokens {
            token.base = Some(self.cwd.strip_prefix(&self.root)?.to_owned());
        }
        self.stack.push(actual);
        let result = match kind {
            SourceKind::Document => self.process(&tokens),
            SourceKind::Package => self.collect_package(&tokens).map(|()| Vec::new()),
        };
        self.stack.pop();
        result
    }

    fn register(&mut self, definition: Macro, provide: bool) {
        if provide {
            self.macros
                .entry(definition.name.clone())
                .or_insert(definition);
        } else {
            self.macros.insert(definition.name.clone(), definition);
        }
    }

    fn packages(&mut self, tokens: &[Token], start: usize) -> Result<()> {
        let end = lexer::optional(tokens, start + 1).map_or(start + 1, |(_, end)| end);
        if let Some((packages, _)) = lexer::group(tokens, end) {
            for package in lexer::source(packages).split(',') {
                let local = self.cwd.join(format!("{}.sty", package.trim()));
                if local.is_file() && self.packages.insert(local.clone()) {
                    self.read(&local, SourceKind::Package)?;
                }
            }
        }
        Ok(())
    }
    // Scan stored definitions, without executing package implementation code.
    // In particular, do not expand runtime input commands, conditionals, hooks,
    // or definitions nested inside an environment's begin/end arguments.
    fn collect_package(&mut self, tokens: &[Token]) -> Result<()> {
        let mut i = 0;
        while i < tokens.len() {
            if i % 1024 == 0 {
                self.check()?;
            }
            let token = &tokens[i];
            if token.command("endinput") {
                break;
            }
            if let Some((definitions, end)) = environment_definition(tokens, i) {
                for definition in definitions {
                    self.register(definition, token.command("provideenvironment"));
                }
                i = end;
                continue;
            }
            if let Some((definition, end)) = definition(tokens, i) {
                self.register(definition, token.command("providecommand"));
                i = end;
                continue;
            }
            if token.command("RequirePackage") || token.command("usepackage") {
                self.packages(tokens, i)?;
            }
            if token.command("input") {
                let argument = if let Some((argument, end)) = lexer::group(tokens, i + 1) {
                    i = end;
                    argument
                } else {
                    let start = lexer::skip_space(tokens, i + 1);
                    let mut end = start;
                    while tokens
                        .get(end)
                        .is_some_and(|token| !token.is_space() && token.kind != Kind::Close)
                    {
                        end += 1;
                    }
                    i = end;
                    &tokens[start..end]
                };
                if !argument
                    .iter()
                    .any(|token| matches!(token.kind, Kind::Command(_)))
                {
                    let name = lexer::source(argument);
                    let mut local = self.cwd.join(name.trim().trim_matches('"'));
                    if local.extension().is_none() {
                        local.set_extension("tex");
                    }
                    // Non-local inputs belong to the TeX installation. Leave
                    // their resolution and execution to the actual compiler.
                    if local.is_file() {
                        self.read(&local, SourceKind::Package)?;
                    }
                }
                continue;
            }
            if token.kind == Kind::Open
                && let Some((_, end)) = lexer::group(tokens, i)
            {
                i = end;
                continue;
            }
            i += 1;
        }
        Ok(())
    }

    fn expand_macro(
        &mut self,
        tokens: &[Token],
        start: usize,
        definition: &Macro,
    ) -> Result<(Vec<Token>, usize)> {
        let token = &tokens[start];
        let name = &definition.name;
        ensure!(
            self.calls.len() < 64,
            "{}:{}: recursive macro expansion of \\{name} exceeds 64 levels",
            token.source.file.display(),
            token.source.line
        );
        let mut arguments = Vec::new();
        let mut end = start + 1;
        if let Some(default) = &definition.default {
            if let Some((argument, next)) = lexer::optional(tokens, end) {
                arguments.push(argument.to_vec());
                end = next;
            } else {
                arguments.push(default.clone());
            }
        }
        while arguments.len() < definition.args {
            if let Some((argument, next)) = lexer::group(tokens, end) {
                arguments.push(argument.to_vec());
                end = next;
            } else {
                end = lexer::skip_space(tokens, end);
                let argument = tokens
                    .get(end)
                    .with_context(|| format!("missing argument to \\{name}"))?
                    .clone();
                arguments.push(vec![argument]);
                end += 1;
            }
        }
        if definition.args == 0 {
            while tokens
                .get(end)
                .is_some_and(|t| t.kind == Kind::Space(false))
            {
                end += 1;
            }
        }
        let mut expansion = Vec::new();
        let mut b = 0;
        while b < definition.body.len() {
            if definition.body[b].kind == Kind::Char('#')
                && let Some(Token {
                    kind: Kind::Char(c),
                    ..
                }) = definition.body.get(b + 1)
                && let Some(n) = c.to_digit(10).filter(|n| *n > 0)
            {
                expansion.extend(
                    arguments
                        .get(n as usize - 1)
                        .context("macro refers to an undeclared parameter")?
                        .iter()
                        .cloned(),
                );
                b += 2;
            } else {
                let mut item = definition.body[b].clone();
                item.source = token.source.clone();
                item.base.clone_from(&token.base);
                expansion.push(item);
                b += 1;
            }
        }
        // Replacement parameters are substituted before recognizing
        // math. This also handles parameters inside $...$ correctly.
        let mut expansion = lexer::fragment_tokens(&lexer::source(&expansion), &token.source.file)?;
        for item in &mut expansion {
            item.source = token.source.clone();
            item.base.clone_from(&token.base);
        }
        self.calls.push(name.clone());
        let expanded = self.process(&expansion);
        self.calls.pop();
        Ok((expanded?, end))
    }

    fn process(&mut self, tokens: &[Token]) -> Result<Vec<Token>> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < tokens.len() {
            if i % 1024 == 0 {
                self.check()?;
            }
            let token = &tokens[i];
            if token.kind == Kind::Verbatim
                && token
                    .raw
                    .strip_prefix("\\begin")
                    .is_some_and(|s| s.trim_start().starts_with("{comment}"))
            {
                i += 1;
                continue;
            }
            if matches!(token.kind, Kind::Math(_)) {
                let (open, inner, close) = lexer::math_parts(&token.raw);
                let mut inner = lexer::fragment_tokens(inner, &token.source.file)?;
                for item in &mut inner {
                    item.source.line += token.source.line - 1;
                    item.base.clone_from(&token.base);
                }
                let expanded = self.process(&inner)?;
                let mut result = token.clone();
                result.raw = format!("{open}{}{close}", lexer::source(&expanded));
                out.push(result);
                i += 1;
                continue;
            }
            if let Some((definitions, end)) = environment_definition(tokens, i) {
                for definition in definitions {
                    self.register(definition, token.command("provideenvironment"));
                }
                out.extend_from_slice(&tokens[i..end]);
                i = end;
                continue;
            }
            if let Some((definition, end)) = definition(tokens, i) {
                self.register(definition, token.command("providecommand"));
                out.extend_from_slice(&tokens[i..end]);
                i = end;
                continue;
            }
            if token.kind == Kind::Open {
                let (inner, end) = lexer::group(tokens, i)
                    .context("unbalanced LaTeX group after input expansion")?;
                let saved = self.macros.clone();
                out.push(token.clone());
                out.extend(self.process(inner)?);
                out.push(tokens[end - 1].clone());
                self.macros = saved;
                i = end;
                continue;
            }
            if token.command("input") || token.command("include") {
                let (argument, end) = if let Some((argument, end)) = lexer::group(tokens, i + 1) {
                    (argument.to_vec(), end)
                } else {
                    let start = lexer::skip_space(tokens, i + 1);
                    let mut end = start;
                    while tokens
                        .get(end)
                        .is_some_and(|t| !t.is_space() && t.kind != Kind::Close)
                    {
                        end += 1;
                    }
                    ensure!(
                        end > start,
                        "{}:{}: missing input filename",
                        token.source.file.display(),
                        token.source.line
                    );
                    (tokens[start..end].to_vec(), end)
                };
                let expanded = self.process(&argument)?;
                let name = lexer::source(&expanded);
                let name = name.trim().trim_matches('"');
                ensure!(
                    !expanded.iter().any(|t| matches!(t.kind, Kind::Command(_))),
                    "{}:{}: input filename contains an unsupported dynamic command",
                    token.source.file.display(),
                    token.source.line
                );
                let mut path = self.cwd.join(name);
                if path.extension().is_none() {
                    path.set_extension("tex");
                }
                if token.command("include") {
                    let mut boundary = token.clone();
                    boundary.kind = Kind::Command("clearpage".into());
                    boundary.raw = "\\clearpage\n".into();
                    out.push(boundary);
                }
                out.extend(self.read(&path, SourceKind::Document)?);
                if token.command("include") {
                    let mut boundary = token.clone();
                    boundary.kind = Kind::Command("clearpage".into());
                    boundary.raw = "\\clearpage\n".into();
                    out.push(boundary);
                }
                i = end;
                continue;
            }
            if token.command("import")
                || token.command("subimport")
                || token.command("includefrom")
                || token.command("subincludefrom")
            {
                let (directory, end) =
                    lexer::group(tokens, i + 1).context("import requires a directory")?;
                let (file, end) =
                    lexer::group(tokens, end).context("import requires a filename")?;
                let directory = lexer::source(&self.process(directory)?);
                let file = lexer::source(&self.process(file)?);
                let old = self.cwd.clone();
                self.cwd = self.cwd.join(directory.trim());
                let mut path = self.cwd.join(file.trim());
                if path.extension().is_none() {
                    path.set_extension("tex");
                }
                let result = self.read(&path, SourceKind::Document);
                self.cwd = old;
                out.extend(result?);
                i = end;
                continue;
            }
            if token.command("usepackage") || token.command("RequirePackage") {
                self.packages(tokens, i)?;
            }
            if let Kind::Command(name) = &token.kind
                && let Some(definition) = self.macros.get(name).filter(|d| d.expandable).cloned()
            {
                let (expanded, end) = self.expand_macro(tokens, i, &definition)?;
                out.extend(expanded);
                i = end;
                continue;
            }

            if let Kind::Command(name) = &token.kind
                && self.macros.get(name).is_some_and(|d| !d.expandable)
            {
                let warning = format!(
                    "\\{name} uses TeX execution features; its definition \
                     is compared as a dependency without executing it"
                );
                if !self.warnings.contains(&warning) {
                    self.warnings.push(warning);
                }
            }
            out.push(token.clone());
            i += 1;
            self.emitted += 1;
            ensure!(
                self.emitted <= 2_000_000,
                "expanded LaTeX exceeds two million tokens"
            );
        }
        Ok(out)
    }
}

pub(super) fn load(root: &Path, main: &Path, deadline: Instant) -> Result<Expanded> {
    ensure!(
        !main.is_absolute() && main.components().all(|c| matches!(c, Component::Normal(_))),
        "main path must be repository-relative"
    );
    let root = fs::canonicalize(root)?;
    let mut processor = Processor {
        cwd: root.join(main.parent().unwrap_or(Path::new(""))),
        root: root.clone(),
        stack: Vec::new(),
        macros: Macros::new(),
        calls: Vec::new(),
        packages: HashSet::new(),
        warnings: Vec::new(),
        bytes: 0,
        emitted: 0,
        deadline,
    };
    let tokens = processor.read(&root.join(main), SourceKind::Document)?;
    Ok(Expanded {
        tokens,
        macros: processor.macros,
        warnings: processor.warnings,
        main: main.to_owned(),
        root,
    })
}
