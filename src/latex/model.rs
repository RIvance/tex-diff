use super::{
    Kind as UnitKind, SourceLocation,
    expand::{self, Expanded, Macros},
    lexer::{self, Kind, Token},
};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub(super) struct Unit {
    pub id: usize,
    pub page_leading: String,
    pub page_trailing: String,
    pub kind: UnitKind,
    pub key: String,
    pub tex: String,
    pub text: String,
    pub locations: Vec<SourceLocation>,
    pub leading: String,
    pub head: String,
    pub tail: String,
    pub children: Option<Vec<Unit>>,
    pub identity: String,
}

#[derive(Clone, Debug)]
struct Atom {
    tex: String,
    text: String,
    key: String,
    prefix: Vec<String>,
    source: SourceLocation,
}

fn atoms_tex(atoms: &[Atom]) -> String {
    let mut tex = String::new();
    let mut context: Vec<String> = Vec::new();
    for atom in atoms {
        let shared = context
            .iter()
            .zip(&atom.prefix)
            .take_while(|(a, b)| a == b)
            .count();
        for _ in shared..context.len() {
            tex.push('}');
        }
        for value in &atom.prefix[shared..] {
            tex.push_str(value);
        }
        context.clone_from(&atom.prefix);
        tex.push_str(&atom.tex);
    }
    for _ in &context {
        tex.push('}');
    }
    tex
}

pub(super) struct Document {
    pub preamble: String,
    pub units: Vec<Unit>,
    pub trailing: String,
    pub macros: Macros,
    pub assets: BTreeMap<String, PathBuf>,
    pub warnings: Vec<String>,
    pub metadata: BTreeMap<String, String>,
    pub resources: BTreeMap<PathBuf, PathBuf>,
    pub main_directory: PathBuf,
    pub main_filename: PathBuf,
}

fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

pub(super) fn normalized(tokens: &[Token]) -> String {
    let mut out = String::new();
    for (i, token) in tokens.iter().enumerate() {
        if token.is_space() {
            let previous = tokens[..i].iter().rev().find(|t| !t.is_space());
            let next = tokens[i + 1..].iter().find(|t| !t.is_space());
            if next.is_some_and(|token| {
                matches!(
                    &token.kind,
                    Kind::Command(name) if [
                        "begin", "end", "caption", "label", "centering", "includegraphics",
                    ].contains(&name.as_str())
                )
            }) {
                continue;
            }
            if previous.is_some_and(|t| {
                matches!(t.kind, Kind::Char(c) if c.is_alphanumeric()) || t.kind == Kind::Close
            }) && next.is_some_and(|t| {
                matches!(t.kind, Kind::Char(c) if c.is_alphanumeric())
                    || matches!(t.kind, Kind::Command(_))
            }) {
                out.push(' ');
            }
        } else if matches!(token.kind, Kind::Math(_)) {
            let (open, body, close) = lexer::math_parts(&token.raw);
            out.push_str(open);
            out.push_str(&math_normalized(body));
            out.push_str(close);
        } else {
            out.push_str(&token.raw);
        }
    }
    out
}

fn math_normalized(source: &str) -> String {
    let Ok(tokens) = lexer::tokens(source, Path::new("math.tex")) else {
        return source.to_owned();
    };
    let mut out = String::new();
    let mut i = 0;
    while i < tokens.len() {
        if let Kind::Command(name) = &tokens[i].kind
            && [
                "text",
                "mbox",
                "intertext",
                "textnormal",
                "textrm",
                "textsf",
                "texttt",
            ]
            .contains(&name.as_str())
            && let Some((body, end)) = lexer::group(&tokens, i + 1)
        {
            out.push_str(&format!("\\{name}{{{}}}", normalized(body)));
            i = end;
            continue;
        }
        if !tokens[i].is_space() {
            out.push_str(&tokens[i].raw);
        }
        i += 1;
    }
    out
}

fn environment(tokens: &[Token], start: usize, command: &str) -> Option<(String, usize)> {
    if !tokens.get(start)?.command(command) {
        return None;
    }
    let (name, end) = lexer::group(tokens, start + 1)?;
    Some((lexer::source(name).trim().to_owned(), end))
}

fn command_end(tokens: &[Token], start: usize) -> usize {
    let mut end = start + 1;
    if tokens.get(end).is_some_and(|t| t.kind == Kind::Char('*')) {
        end += 1;
    }
    while let Some((_, next)) = lexer::optional(tokens, end) {
        end = next;
    }
    while let Some((_, next)) = lexer::group(tokens, end) {
        end = next;
    }
    end
}

fn environment_end(tokens: &[Token], start: usize, name: &str) -> Result<(usize, usize)> {
    let mut nested = 0;
    let mut i = start;
    while i < tokens.len() {
        if let Some((other, end)) = environment(tokens, i, "begin")
            && other == name
        {
            nested += 1;
            i = end;
            continue;
        }
        if let Some((other, end)) = environment(tokens, i, "end")
            && other == name
        {
            if nested == 0 {
                return Ok((i, end));
            }
            nested -= 1;
            i = end;
            continue;
        }
        i += 1;
    }
    anyhow::bail!("unclosed LaTeX environment {name}")
}

fn environment_arguments(tokens: &[Token], mut end: usize, name: &str) -> Result<usize> {
    while let Some((_, next)) = lexer::optional(tokens, end) {
        end = next;
    }
    let required = match name {
        "minipage" | "multicols" | "multicols*" | "spacing" | "thebibliography" => 1,
        "list" | "adjustwidth" | "adjustwidth*" => 2,
        _ => 0,
    };
    for _ in 0..required {
        end = lexer::group(tokens, end)
            .with_context(|| format!("environment {name} is missing a required argument"))?
            .1;
    }
    while let Some((_, next)) = lexer::optional(tokens, end) {
        end = next;
    }
    Ok(end)
}

fn opaque(name: &str) -> Option<UnitKind> {
    match name {
        "figure" | "figure*" | "tikzpicture" | "picture" | "pspicture" => Some(UnitKind::Figure),
        "table" | "table*" | "tabular" | "tabular*" | "tabularx" | "longtable" | "tabu" => {
            Some(UnitKind::Table)
        }
        "equation"
        | "equation*"
        | "align"
        | "align*"
        | "gather"
        | "gather*"
        | "multline"
        | "multline*"
        | "eqnarray"
        | "eqnarray*"
        | "displaymath"
        | "mathpar"
        | "mathparpagebreakable" => Some(UnitKind::Math),
        _ => None,
    }
}

struct Parser<'a> {
    document: &'a Expanded,
    assets: BTreeMap<String, PathBuf>,
    graphics_paths: Vec<PathBuf>,
    metadata: BTreeMap<String, Vec<Token>>,
    metadata_used: bool,
}

impl Parser<'_> {
    fn dependency(&self, tokens: &[Token]) -> String {
        fn visit(
            tokens: &[Token],
            macros: &Macros,
            job_name: &str,
            seen: &mut Vec<String>,
            out: &mut String,
        ) {
            for (i, token) in tokens.iter().enumerate() {
                if token.command("jobname") {
                    out.push_str(&format!("jobname:{job_name}"));
                }
                if let Kind::Command(name) = &token.kind
                    && !seen.contains(name)
                    && let Some(definition) = macros.get(name)
                {
                    seen.push(name.clone());
                    out.push_str(&format!(
                        "{name}:{}:{:?}:{}",
                        definition.args,
                        definition.default.as_deref().map(normalized),
                        normalized(&definition.body)
                    ));
                    visit(&definition.body, macros, job_name, seen, out);
                }
                if matches!(token.kind, Kind::Math(_)) {
                    let (_, inner, _) = lexer::math_parts(&token.raw);
                    if let Ok(tokens) = lexer::tokens(inner, &token.source.file) {
                        visit(&tokens, macros, job_name, seen, out);
                    }
                }
                if (token.command("begin") || token.command("end"))
                    && let Some((name, _)) = lexer::group(tokens, i + 1)
                {
                    let name = lexer::source(name).trim().to_owned();
                    let name = if token.command("end") {
                        format!("end{name}")
                    } else {
                        name
                    };
                    if !seen.contains(&name)
                        && let Some(definition) = macros.get(&name)
                    {
                        seen.push(name.clone());
                        out.push_str(&format!(
                            "{name}:{}:{:?}:{}",
                            definition.args,
                            definition.default.as_deref().map(normalized),
                            normalized(&definition.body)
                        ));
                        visit(&definition.body, macros, job_name, seen, out);
                    }
                }
            }
        }
        let mut out = String::new();
        visit(
            tokens,
            &self.document.macros,
            &self.document.main.file_stem().unwrap().to_string_lossy(),
            &mut Vec::new(),
            &mut out,
        );
        out
    }

    fn asset(&mut self, argument: &[Token]) -> Result<(String, String)> {
        let name = lexer::source(argument);
        let name = name.trim().trim_matches('"');
        ensure!(
            !argument.iter().any(|t| matches!(t.kind, Kind::Command(_))),
            "asset filename contains an unsupported dynamic command: {name}"
        );
        let main_directory = self
            .document
            .root
            .join(self.document.main.parent().unwrap_or(Path::new("")));
        let cwd = argument
            .first()
            .and_then(|t| t.base.as_ref())
            .map_or_else(|| main_directory.clone(), |p| self.document.root.join(p));
        let mut directories = vec![cwd.clone()];
        directories.extend(self.graphics_paths.iter().map(|p| cwd.join(p)));
        if cwd != main_directory {
            directories.push(main_directory.clone());
            directories.extend(self.graphics_paths.iter().map(|p| main_directory.join(p)));
        }
        let mut selected = None;
        for dir in directories {
            let base = dir.join(name);
            let paths = if base.extension().is_some() {
                vec![base]
            } else {
                ["pdf", "png", "jpg", "jpeg", "eps", "mps"]
                    .iter()
                    .map(|ext| base.with_extension(ext))
                    .collect()
            };
            if let Some(path) = paths.into_iter().find(|p| p.is_file()) {
                selected = Some(path);
                break;
            }
        }
        let path = selected.with_context(|| format!("cannot find figure asset {name}"))?;
        let actual = fs::canonicalize(&path)?;
        ensure!(
            actual.starts_with(&self.document.root),
            "asset points outside the repository: {name}"
        );
        let content = fs::read(&actual)?;
        let hash = format!("{:x}", Sha256::digest(&content));
        let extension = actual.extension().and_then(|e| e.to_str()).unwrap_or("bin");
        let file = format!("{hash}.{extension}");
        self.assets.insert(file.clone(), actual);
        Ok((format!("TEXDIFFASSETS/{file}"), hash))
    }

    fn rewrite(&mut self, tokens: &[Token]) -> Result<(String, String)> {
        let mut output = Vec::new();
        let mut key = Vec::new();
        let mut i = 0;
        while i < tokens.len() {
            if tokens[i].command("includegraphics") {
                let mut end = i + 1;
                if tokens.get(end).is_some_and(|t| t.kind == Kind::Char('*')) {
                    end += 1;
                }
                if let Some((_, next)) = lexer::optional(tokens, end) {
                    end = next;
                }
                let (argument, next) =
                    lexer::group(tokens, end).context("includegraphics requires a filename")?;
                let (path, hash) = self.asset(argument)?;
                let start = lexer::skip_space(tokens, end);
                output.extend_from_slice(&tokens[i..start + 1]);
                key.extend_from_slice(&tokens[i..start + 1]);
                let mut filename = tokens[start].clone();
                filename.kind = Kind::Char('a');
                filename.raw = path;
                output.push(filename.clone());
                filename.raw = format!("asset{hash}");
                key.push(filename);
                output.push(tokens[next - 1].clone());
                key.push(tokens[next - 1].clone());
                i = next;
            } else {
                output.push(tokens[i].clone());
                key.push(tokens[i].clone());
                i += 1;
            }
        }
        // Normalize whitespace using the original token boundaries, while replacing
        // file identities with content hashes for figure renames.
        let tex = lexer::source(&output);
        let mut semantic = normalized(&key);
        semantic.push_str(&self.dependency(tokens));
        Ok((tex, semantic))
    }

    fn inline(&mut self, tokens: &[Token], prefix: &[String]) -> Result<Vec<Atom>> {
        let mut result = Vec::new();
        let mut i = 0;
        let mut active_prefix = prefix.to_vec();
        while i < tokens.len() {
            let token = &tokens[i];
            if let Kind::Command(name) = &token.kind
                && !active_prefix.is_empty()
            {
                let declaration = [
                    "bfseries",
                    "mdseries",
                    "itshape",
                    "slshape",
                    "scshape",
                    "upshape",
                    "normalfont",
                    "rmfamily",
                    "sffamily",
                    "ttfamily",
                    "em",
                    "tiny",
                    "scriptsize",
                    "footnotesize",
                    "small",
                    "normalsize",
                    "large",
                    "Large",
                    "LARGE",
                    "huge",
                    "Huge",
                ]
                .contains(&name.as_str());
                if declaration || name == "color" {
                    let end = if name == "color" {
                        let start = lexer::optional(tokens, i + 1).map_or(i + 1, |(_, end)| end);
                        lexer::group(tokens, start)
                            .context("color requires a color name")?
                            .1
                    } else {
                        i + 1
                    };
                    active_prefix
                        .last_mut()
                        .unwrap()
                        .push_str(&format!("{} ", lexer::source(&tokens[i..end])));
                    i = end;
                    continue;
                }
            }
            if let Kind::Command(name) = &token.kind
                && [
                    "emph",
                    "textbf",
                    "textit",
                    "textsl",
                    "textsc",
                    "textrm",
                    "textsf",
                    "texttt",
                    "textnormal",
                    "underline",
                    "mbox",
                ]
                .contains(&name.as_str())
                && let Some((body, end)) = lexer::group(tokens, i + 1)
            {
                let mut stack = active_prefix.clone();
                stack.push(format!("\\{name}{{"));
                result.extend(self.inline(body, &stack)?);
                i = end;
                continue;
            }
            if token.kind == Kind::Open {
                let (body, end) = lexer::group(tokens, i).context("unbalanced inline group")?;
                let mut stack = active_prefix.clone();
                stack.push("{".into());
                result.extend(self.inline(body, &stack)?);
                i = end;
                continue;
            }
            if token.is_space() {
                if i > 0 && tokens[i - 1].is_word_command() {
                    i += 1;
                    continue;
                }
                result.push(Atom {
                    tex: " ".into(),
                    text: " ".into(),
                    key: " ".into(),
                    prefix: active_prefix.clone(),
                    source: token.source.clone(),
                });
                i += 1;
                continue;
            }
            let end = if matches!(token.kind, Kind::Command(_)) {
                command_end(tokens, i)
            } else {
                i + 1
            };
            let (mut tex, semantic) = self.rewrite(&tokens[i..end])?;
            if tokens[end - 1].is_word_command() {
                tex.push(' ');
            }
            let text = match &token.kind {
                Kind::Char(c) => c.to_string(),
                Kind::Command(name)
                    if ["%", "&", "_", "#", "{", "}", "$"].contains(&name.as_str()) =>
                {
                    name.clone()
                }
                Kind::Command(name) if name == "label" || name == "index" => String::new(),
                _ => lexer::source(&tokens[i..end]),
            };
            let key = if token.command("label") || token.command("index") {
                String::new()
            } else {
                semantic
            };
            result.push(Atom {
                tex,
                text,
                key,
                prefix: active_prefix.clone(),
                source: token.source.clone(),
            });
            i = end;
        }
        Ok(result)
    }

    fn sentence(&self, atoms: &[Atom], leading: String) -> Option<Unit> {
        let mut first = 0;
        let mut last = atoms.len();
        while first < last
            && atoms[first].text.trim().is_empty()
            && atoms[first].key.trim().is_empty()
            && atoms[first].tex.trim().is_empty()
        {
            first += 1;
        }
        while last > first
            && atoms[last - 1].text.trim().is_empty()
            && atoms[last - 1].key.trim().is_empty()
            && atoms[last - 1].tex.trim().is_empty()
        {
            last -= 1;
        }
        if first == last {
            return None;
        }
        let page_leading = atoms_tex(&atoms[..first]);
        let page_trailing = atoms_tex(&atoms[last..]);
        let atoms = &atoms[first..last];
        let tex = atoms_tex(atoms);
        let mut key = String::new();
        let mut text = String::new();
        let mut locations = Vec::new();
        for atom in atoms {
            text.push_str(&atom.text);
            if atom.key != " " && !atom.key.is_empty() {
                key.push_str(&format!(
                    "{:?}:",
                    atom.prefix
                        .iter()
                        .filter(|s| s.as_str() != "{")
                        .collect::<Vec<_>>()
                ));
            }
            key.push_str(&atom.key);
            if locations.last().is_none_or(|last: &SourceLocation| {
                last.file != atom.source.file || last.line != atom.source.line
            }) {
                locations.push(atom.source.clone());
            }
        }
        Some(Unit {
            id: 0,
            page_leading,
            page_trailing,
            kind: UnitKind::Sentence,
            key: digest(key.trim()),
            tex,
            text: text.split_whitespace().collect::<Vec<_>>().join(" "),
            locations,
            leading,
            head: String::new(),
            tail: String::new(),
            children: None,
            identity: "sentence".into(),
        })
    }

    fn flush(
        &mut self,
        pending: &mut Vec<Token>,
        leading: &mut String,
        units: &mut Vec<Unit>,
    ) -> Result<()> {
        if pending.is_empty() {
            return Ok(());
        }
        let atoms = self.inline(pending, &[])?;
        if atoms
            .iter()
            .all(|a| a.text.trim().is_empty() && a.key.trim().is_empty())
        {
            leading.push_str(&lexer::source(pending));
            pending.clear();
            return Ok(());
        }
        let mut first = 0;
        let mut i = 0;
        while i < atoms.len() {
            let value = atoms[i].text.as_str();
            if matches!(value, "." | "!" | "?" | "。" | "！" | "？") {
                let mut end = i + 1;
                while end < atoms.len()
                    && ([".", "!", "?", "\"", "'", "”", "’", ")", "]", "}"]
                        .contains(&atoms[end].text.as_str())
                        || atoms[end].text.is_empty() && atoms[end].key.is_empty())
                {
                    end += 1;
                }
                let token: String = atoms[first..=i]
                    .iter()
                    .rev()
                    .take_while(|a| a.text != " ")
                    .map(|a| a.text.as_str())
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                let abbreviation = value == "."
                    && (ABBREVIATIONS.contains(&token.to_lowercase().as_str())
                        || token.len() == 2 && token.as_bytes()[0].is_ascii_alphabetic());
                if !abbreviation
                    && (end == atoms.len()
                        || atoms[end].text == " "
                        || ["。", "！", "？"].contains(&value))
                {
                    if let Some(unit) = self.sentence(&atoms[first..end], std::mem::take(leading)) {
                        units.push(unit);
                    }
                    first = end;
                }
                i = end;
            } else {
                i += 1;
            }
        }
        if let Some(unit) = self.sentence(&atoms[first..], std::mem::take(leading)) {
            units.push(unit);
        }
        pending.clear();
        Ok(())
    }

    fn sequence(&mut self, tokens: &[Token]) -> Result<(Vec<Unit>, String)> {
        let mut units = Vec::new();
        let mut pending = Vec::new();
        let mut leading = String::new();
        let mut i = 0;
        while i < tokens.len() {
            let token = &tokens[i];
            if token.kind == Kind::Open {
                let (_, end) = lexer::group(tokens, i).context("unbalanced paragraph group")?;
                pending.extend_from_slice(&tokens[i..end]);
                i = end;
                continue;
            }
            if let Some(end) = expand::declaration_end(tokens, i) {
                self.flush(&mut pending, &mut leading, &mut units)?;
                leading.push_str(&lexer::source(&tokens[i..end]));
                i = end;
                continue;
            }
            if matches!(token.kind, Kind::Space(true)) || token.command("par") {
                self.flush(&mut pending, &mut leading, &mut units)?;
                leading.push_str("\n\n");
                i += 1;
                continue;
            }
            if let Some((name, open_end)) = environment(tokens, i, "begin") {
                self.flush(&mut pending, &mut leading, &mut units)?;
                let (close_start, end) = environment_end(tokens, open_end, &name)?;
                let open_end = environment_arguments(tokens, open_end, &name)?;
                let kind = opaque(&name);
                if let Some(kind) = kind {
                    let (tex, mut key) = self.rewrite(&tokens[i..end])?;
                    if kind == UnitKind::Math {
                        key = math_normalized(&tex) + &self.dependency(&tokens[i..end]);
                    }
                    units.push(Unit {
                        id: 0,
                        page_leading: String::new(),
                        page_trailing: String::new(),
                        kind,
                        key: digest(&key),
                        tex,
                        text: lexer::source(&tokens[i..end])
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" "),
                        locations: vec![token.source.clone()],
                        leading: std::mem::take(&mut leading),
                        head: String::new(),
                        tail: String::new(),
                        children: None,
                        identity: name,
                    });
                } else {
                    let (children, trailing) = if name == "thebibliography" {
                        self.bibliography(&tokens[open_end..close_start])?
                    } else {
                        self.sequence(&tokens[open_end..close_start])?
                    };
                    let head = lexer::source(&tokens[i..open_end]);
                    let tail = trailing + &lexer::source(&tokens[close_start..end]);
                    let identity = format!("{name}:{}", normalized(&tokens[i..open_end]));
                    let key = digest(&format!(
                        "{identity}:{}",
                        children
                            .iter()
                            .map(|n| n.key.as_str())
                            .collect::<Vec<_>>()
                            .join("|")
                    ));
                    units.push(Unit {
                        id: 0,
                        page_leading: String::new(),
                        page_trailing: String::new(),
                        kind: UnitKind::Block,
                        key,
                        tex: String::new(),
                        text: name.clone(),
                        locations: vec![token.source.clone()],
                        leading: std::mem::take(&mut leading),
                        head,
                        tail,
                        children: Some(children),
                        identity,
                    });
                }
                i = end;
                continue;
            }
            let command = if let Kind::Command(name) = &token.kind {
                Some(name.as_str())
            } else {
                None
            };
            if command.is_some_and(|n| {
                [
                    "part",
                    "chapter",
                    "section",
                    "subsection",
                    "subsubsection",
                    "paragraph",
                    "subparagraph",
                    "caption",
                    "title",
                    "author",
                    "date",
                ]
                .contains(&n)
            }) {
                self.flush(&mut pending, &mut leading, &mut units)?;
                let end = command_end(tokens, i);
                let (tex, key) = self.rewrite(&tokens[i..end])?;
                units.push(Unit {
                    id: 0,
                    page_leading: String::new(),
                    page_trailing: String::new(),
                    kind: UnitKind::Heading,
                    key: digest(&key),
                    text: lexer::source(&tokens[i..end]),
                    tex,
                    locations: vec![token.source.clone()],
                    leading: std::mem::take(&mut leading),
                    head: String::new(),
                    tail: String::new(),
                    children: None,
                    identity: command.unwrap().into(),
                });
                i = end;
                continue;
            }
            if token.command("maketitle") {
                self.flush(&mut pending, &mut leading, &mut units)?;
                self.metadata_used = true;
                let values = self.metadata.values().cloned().collect::<Vec<_>>();
                let key = values
                    .iter()
                    .map(|v| self.rewrite(v).map(|(_, key)| key))
                    .collect::<Result<Vec<_>>>()?
                    .join("|");
                let text = self
                    .metadata
                    .values()
                    .map(|v| lexer::source(v))
                    .collect::<Vec<_>>()
                    .join(" ");
                units.push(Unit {
                    id: 0,
                    page_leading: String::new(),
                    page_trailing: String::new(),
                    kind: UnitKind::Heading,
                    key: digest(&key),
                    tex: "\\maketitle".into(),
                    text,
                    locations: vec![token.source.clone()],
                    leading: std::mem::take(&mut leading),
                    head: String::new(),
                    tail: String::new(),
                    children: None,
                    identity: "maketitle".into(),
                });
                i += 1;
                continue;
            }
            if command.is_some_and(|n| {
                [
                    "item",
                    "clearpage",
                    "newpage",
                    "pagebreak",
                    "nopagebreak",
                    "vspace",
                    "hspace",
                    "smallskip",
                    "medskip",
                    "bigskip",
                    "noindent",
                    "centering",
                    "raggedright",
                    "raggedleft",
                    "bibliographystyle",
                ]
                .contains(&n)
            }) {
                self.flush(&mut pending, &mut leading, &mut units)?;
                let end = command_end(tokens, i);
                leading.push_str(&lexer::source(&tokens[i..end]));
                leading.push('\n');
                i = end;
                continue;
            }
            if token.kind == Kind::Verbatim && token.raw.starts_with("\\begin{comment}") {
                i += 1;
                continue;
            }
            if matches!(token.kind, Kind::Math(true))
                || token.kind == Kind::Verbatim && !token.raw.starts_with("\\verb")
                || token.command("includegraphics")
            {
                self.flush(&mut pending, &mut leading, &mut units)?;
                let end = if token.command("includegraphics") {
                    command_end(tokens, i)
                } else {
                    i + 1
                };
                let (tex, key) = self.rewrite(&tokens[i..end])?;
                units.push(Unit {
                    id: 0,
                    page_leading: String::new(),
                    page_trailing: String::new(),
                    kind: if token.command("includegraphics") {
                        UnitKind::Figure
                    } else if matches!(token.kind, Kind::Math(_)) {
                        UnitKind::Math
                    } else {
                        UnitKind::Block
                    },
                    key: digest(&key),
                    text: lexer::source(&tokens[i..end]),
                    tex,
                    locations: vec![token.source.clone()],
                    leading: std::mem::take(&mut leading),
                    head: String::new(),
                    tail: String::new(),
                    children: None,
                    identity: "object".into(),
                });
                i = end;
                continue;
            }
            pending.push(token.clone());
            i += 1;
        }
        self.flush(&mut pending, &mut leading, &mut units)?;
        Ok((units, leading))
    }

    fn bibliography(&mut self, tokens: &[Token]) -> Result<(Vec<Unit>, String)> {
        let mut entries = Vec::new();
        let mut i = 0;
        let mut leading = String::new();
        while i < tokens.len() {
            if !tokens[i].command("bibitem") {
                leading.push_str(&tokens[i].raw);
                i += 1;
                continue;
            }
            let start = i;
            let argument = lexer::optional(tokens, i + 1).map_or(i + 1, |(_, end)| end);
            let (name, head_end) =
                lexer::group(tokens, argument).context("bibitem requires a citation key")?;
            i = head_end;
            while i < tokens.len() && !tokens[i].command("bibitem") {
                i += 1;
            }
            let (tex, key) = self.rewrite(&tokens[head_end..i])?;
            entries.push(Unit {
                id: 0,
                page_leading: String::new(),
                page_trailing: String::new(),
                kind: UnitKind::Bibliography,
                key: digest(&key),
                text: lexer::source(&tokens[head_end..i])
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
                tex,
                locations: vec![tokens[start].source.clone()],
                leading: std::mem::take(&mut leading),
                head: lexer::source(&tokens[start..head_end]),
                tail: String::new(),
                children: None,
                identity: format!("bibitem:{}", lexer::source(name)),
            });
        }
        Ok((entries, leading))
    }
}

const ABBREVIATIONS: &[&str] = &[
    "dr.", "mr.", "mrs.", "ms.", "prof.", "fig.", "figs.", "eq.", "eqs.", "sec.", "secs.", "ch.",
    "chap.", "vol.", "no.", "pp.", "p.", "vs.", "etc.", "e.g.", "i.e.", "cf.", "al.", "approx.",
    "dept.", "resp.",
];

fn number_units(units: &mut [Unit], next: &mut usize) {
    for unit in units {
        unit.id = *next;
        *next += 1;
        if let Some(children) = &mut unit.children {
            number_units(children, next);
        }
    }
}

fn collect_resources(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<PathBuf, PathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            if !entry
                .file_name()
                .to_string_lossy()
                .starts_with("tex-diff-assets-")
            {
                collect_resources(root, &path, files)?;
            }
        } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            [
                "sty", "cls", "clo", "def", "fd", "cfg", "tex", "pdf", "png", "jpg", "jpeg", "eps",
                "otf", "ttf", "tfm", "vf", "enc", "map", "csv", "dat", "tsv", "txt",
            ]
            .contains(&e.to_lowercase().as_str())
        }) {
            if path.extension().is_some_and(|e| e == "tex") {
                let mut line = Vec::new();
                BufReader::new(fs::File::open(&path)?).read_until(b'\n', &mut line)?;
                if line.starts_with(b"% Generated by tex-diff.") {
                    continue;
                }
            }
            let actual = fs::canonicalize(&path)?;
            ensure!(
                actual.starts_with(root),
                "local TeX resource escapes the repository: {}",
                path.display()
            );
            files.insert(path.strip_prefix(root)?.to_owned(), actual);
        }
    }
    Ok(())
}

pub(super) fn document(expanded: &Expanded) -> Result<Document> {
    let tokens = &expanded.tokens;
    let mut start = None;
    let mut end = None;
    let mut i = 0;
    while i < tokens.len() {
        if let Some(next) = expand::declaration_end(tokens, i) {
            i = next;
            continue;
        }
        if let Some((name, next)) = environment(tokens, i, "begin")
            && name == "document"
        {
            start = Some((i, next));
        }
        if let Some((name, next)) = environment(tokens, i, "end")
            && name == "document"
        {
            end = Some((i, next));
        }
        i += 1;
    }
    let (begin, body_start) = start.context("main source has no \\begin{document}")?;
    let (body_end, _) = end.context("main source has no \\end{document}")?;
    ensure!(body_start <= body_end, "document environment is invalid");
    let mut parser = Parser {
        document: expanded,
        assets: BTreeMap::new(),
        graphics_paths: Vec::new(),
        metadata: BTreeMap::new(),
        metadata_used: false,
    };
    let mut i = 0;
    while i < begin {
        if let Some(end) = expand::declaration_end(tokens, i) {
            i = end;
            continue;
        }
        let token = &tokens[i];
        if token.command("graphicspath")
            && let Some((paths, _)) = lexer::group(tokens, i + 1)
        {
            let mut p = 0;
            while let Some((directory, next)) = lexer::group(paths, p) {
                parser.graphics_paths.push(lexer::source(directory).into());
                p = next;
            }
        }
        if let Kind::Command(name) = &token.kind
            && ["title", "author", "date"].contains(&name.as_str())
            && let Some((value, _)) = lexer::group(tokens, i + 1)
        {
            parser.metadata.insert(name.clone(), value.to_vec());
        }
        i += 1;
    }
    let (mut units, trailing) = parser.sequence(&tokens[body_start..body_end])?;
    number_units(&mut units, &mut 0);
    let values = parser.metadata.clone();
    let metadata = values
        .into_iter()
        .map(|(name, tokens)| {
            let value = if parser.metadata_used {
                parser.rewrite(&tokens)?.0
            } else {
                lexer::source(&tokens)
            };
            Ok((name, value))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut resources = BTreeMap::new();
    collect_resources(&expanded.root, &expanded.root, &mut resources)?;
    Ok(Document {
        preamble: lexer::source(&tokens[..begin]),
        units,
        trailing,
        macros: expanded.macros.clone(),
        assets: parser.assets,
        warnings: expanded.warnings.clone(),
        metadata,
        resources,
        main_directory: expanded.main.parent().unwrap_or(Path::new("")).to_owned(),
        main_filename: expanded
            .main
            .file_name()
            .context("main source needs a filename")?
            .into(),
    })
}
