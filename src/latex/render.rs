use super::{
    Change, ChangeType, Kind, Report, Review, lexer,
    model::{Document, Unit},
};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use similar::{Algorithm, DiffTag, capture_diff_slices_deadline};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

struct Writer<'a> {
    old: Option<&'a Document>,
    new: Option<&'a Document>,
    report: Report,
    deadline: Instant,
    old_marks: BTreeSet<usize>,
    new_marks: BTreeSet<usize>,
}

fn plain(unit: &Unit) -> String {
    if let Some(children) = &unit.children {
        format!(
            "{}{}{}{}",
            unit.leading,
            unit.head,
            children.iter().map(plain).collect::<Vec<_>>().join("\n "),
            unit.tail
        )
    } else {
        format!("{}{}{}{}", unit.leading, unit.head, unit.tex, unit.tail)
    }
}

fn color(side: ChangeType) -> &'static str {
    if side == ChangeType::Removed {
        "TexDiffRemovedColor"
    } else {
        "TexDiffAddedColor"
    }
}

fn recolor(tex: &str, side: ChangeType, figures: bool, preserve_layout: bool) -> Result<String> {
    let tokens = lexer::tokens(tex, Path::new("generated-diff.tex"))?;
    let mut out = String::new();
    let mut i = 0;
    while i < tokens.len() {
        let token = &tokens[i];
        if matches!(token.kind, lexer::Kind::Math(_)) {
            let (open, body, close) = lexer::math_parts(&token.raw);
            out.push_str(open);
            out.push_str(&recolor(body, side, false, preserve_layout)?);
            out.push_str(close);
            i += 1;
            continue;
        }
        if token.command("color") || token.command("textcolor") {
            let start = lexer::optional(&tokens, i + 1).map_or(i + 1, |(_, end)| end);
            if let Some((_, end)) = lexer::group(&tokens, start) {
                out.push_str(&format!(
                    "\\{}{{{}}}",
                    if token.command("color") {
                        "color"
                    } else {
                        "textcolor"
                    },
                    color(side)
                ));
                i = end;
                continue;
            }
        }
        if !preserve_layout
            && side == ChangeType::Removed
            && token.command("label")
            && let Some((label, end)) = lexer::group(&tokens, i + 1)
        {
            out.push_str(&format!(
                "\\label{{tex-diff-removed:{}}}",
                lexer::source(label)
            ));
            i = end;
            continue;
        }
        if figures && token.command("includegraphics") {
            let mut end = i + 1;
            if tokens
                .get(end)
                .is_some_and(|t| t.kind == lexer::Kind::Char('*'))
            {
                end += 1;
            }
            if let Some((_, next)) = lexer::optional(&tokens, end) {
                end = next;
            }
            if let Some((_, next)) = lexer::group(&tokens, end) {
                let image = lexer::source(&tokens[i..next]);
                let framed = if preserve_layout {
                    format!(
                        "\\smash{{\\rlap{{\\kern-0.8pt\\raisebox{{-0.8pt}}{{\\fbox{{\\phantom{{{image}}}}}}}}}}}{image}"
                    )
                } else {
                    format!("\\fbox{{{image}}}")
                };
                out.push_str(&format!(
                    "\\begingroup\\setlength{{\\fboxsep}}{{0pt}}\
                     \\setlength{{\\fboxrule}}{{0.8pt}}{framed}\\endgroup"
                ));
                i = next;
                continue;
            }
        }
        out.push_str(&token.raw);
        i += 1;
    }
    Ok(out)
}

fn marked_tex(tex: &str, side: ChangeType, figures: bool) -> Result<String> {
    let bindings = if side == ChangeType::Removed {
        "\\input{TEXDIFFASSETS/old-macros.tex}"
    } else {
        ""
    };
    Ok(format!(
        "\\begingroup{}\\def\\normalcolor{{\\color{{{}}}}}\\color{{{}}}\n{}\n\\endgroup",
        bindings,
        color(side),
        color(side),
        recolor(tex, side, figures, false)?
    ))
}

impl Writer<'_> {
    fn record(&mut self, unit: &Unit, side: ChangeType) {
        if side == ChangeType::Removed {
            self.old_marks.insert(unit.id);
        } else {
            self.new_marks.insert(unit.id);
        }
        if let Some(children) = &unit.children {
            for child in children {
                self.record(child, side);
            }
        } else {
            self.report.changes.push(Change {
                change: side,
                kind: unit.kind,
                text: unit.text.clone(),
                locations: unit.locations.clone(),
            });
        }
    }

    fn marked(&mut self, unit: &Unit, side: ChangeType, leading: bool) -> Result<String> {
        self.record(unit, side);
        let mut raw = plain(unit);
        if !leading {
            raw = raw[unit.leading.len()..].to_owned();
        }
        marked_tex(&raw, side, unit.kind == Kind::Figure)
    }

    fn title(&mut self, old: &Unit, new: &Unit) -> Result<String> {
        self.record(old, ChangeType::Removed);
        self.record(new, ChangeType::Added);
        let mut tex = new.leading.clone();
        for name in ["title", "author", "date"] {
            let old = self.old.and_then(|d| d.metadata.get(name));
            let new = self.new.and_then(|d| d.metadata.get(name));
            if old == new {
                continue;
            }
            let mut value = String::new();
            if let Some(old) = old {
                value.push_str(&marked_tex(old, ChangeType::Removed, true)?);
                value.push(' ');
            }
            if let Some(new) = new {
                value.push_str(&marked_tex(new, ChangeType::Added, true)?);
            }
            tex.push_str(&format!("\\{name}{{{value}}}\n"));
        }
        tex.push_str("\\maketitle\n");
        Ok(tex)
    }

    fn replacement(&mut self, a: &Unit, b: &Unit) -> Result<String> {
        if a.key == b.key {
            return Ok(plain(b));
        }
        if let (Some(old), Some(new)) = (&a.children, &b.children) {
            return Ok(format!(
                "{}{}{}{}",
                b.leading,
                b.head,
                self.sequence(old, new)?,
                b.tail
            ));
        }
        if a.identity == "maketitle" {
            return self.title(a, b);
        }
        if a.kind == Kind::Bibliography {
            self.record(a, ChangeType::Removed);
            self.record(b, ChangeType::Added);
            return Ok(format!(
                "{}{}{}\n {}",
                b.leading,
                b.head,
                marked_tex(&a.tex, ChangeType::Removed, false)?,
                marked_tex(&b.tex, ChangeType::Added, false)?
            ));
        }
        if a.leading == b.leading {
            return Ok(format!(
                "{}{}\n {}",
                b.leading,
                self.marked(a, ChangeType::Removed, false)?,
                self.marked(b, ChangeType::Added, false)?
            ));
        }
        Ok(format!(
            "{}\n {}",
            self.marked(a, ChangeType::Removed, true)?,
            self.marked(b, ChangeType::Added, true)?
        ))
    }

    fn sequence(&mut self, old: &[Unit], new: &[Unit]) -> Result<String> {
        ensure!(
            Instant::now() < self.deadline,
            "LaTeX source comparison exceeded its timeout"
        );
        let old_keys = old.iter().map(|n| &n.key).collect::<Vec<_>>();
        let new_keys = new.iter().map(|n| &n.key).collect::<Vec<_>>();
        let ops = capture_diff_slices_deadline(
            Algorithm::Myers,
            &old_keys,
            &new_keys,
            Some(self.deadline),
        );
        ensure!(
            Instant::now() < self.deadline,
            "LaTeX source comparison exceeded its timeout"
        );
        let mut out = String::new();
        for op in ops {
            let before = &old[op.old_range()];
            let after = &new[op.new_range()];
            if op.tag() == DiffTag::Equal {
                for unit in after {
                    out.push_str(&plain(unit));
                    out.push_str("\n ");
                }
                continue;
            }
            let old_identities = before
                .iter()
                .map(|n| (n.kind, &n.identity))
                .collect::<Vec<_>>();
            let new_identities = after
                .iter()
                .map(|n| (n.kind, &n.identity))
                .collect::<Vec<_>>();
            for structural in capture_diff_slices_deadline(
                Algorithm::Myers,
                &old_identities,
                &new_identities,
                Some(self.deadline),
            ) {
                let old = &before[structural.old_range()];
                let new = &after[structural.new_range()];
                if structural.tag() == DiffTag::Equal {
                    for (a, b) in old.iter().zip(new) {
                        out.push_str(&self.replacement(a, b)?);
                        out.push_str("\n ");
                    }
                } else {
                    for unit in old {
                        out.push_str(&self.marked(unit, ChangeType::Removed, true)?);
                        out.push_str("\n ");
                    }
                    for unit in new {
                        out.push_str(&self.marked(unit, ChangeType::Added, true)?);
                        out.push_str("\n ");
                    }
                }
            }
        }
        Ok(out)
    }
}

fn packages(preamble: &str) -> Result<BTreeMap<String, String>> {
    let tokens = lexer::tokens(preamble, Path::new("preamble.tex"))?;
    let mut result = BTreeMap::new();
    for (i, token) in tokens.iter().enumerate() {
        if !token.command("usepackage") && !token.command("RequirePackage") {
            continue;
        }
        let (options, start) = lexer::optional(&tokens, i + 1)
            .map_or((String::new(), i + 1), |(tokens, end)| {
                (format!("[{}]", lexer::source(tokens)), end)
            });
        if let Some((names, _)) = lexer::group(&tokens, start) {
            for name in lexer::source(names).split(',').map(str::trim) {
                result.insert(name.into(), format!("\\usepackage{options}{{{name}}}\n"));
            }
        }
    }
    Ok(result)
}

fn page_markup(tex: &str, side: ChangeType, figures: bool, inline: bool) -> Result<String> {
    let normal = if inline {
        String::new()
    } else {
        format!("\\def\\normalcolor{{\\color{{{}}}}}", color(side))
    };
    Ok(format!(
        "\\begingroup{normal}\\color{{{}}}{}\\endgroup{}",
        color(side),
        recolor(tex, side, figures, true)?,
        if inline { "{}" } else { " " }
    ))
}

fn page_unit(unit: &Unit, marks: &BTreeSet<usize>, side: ChangeType) -> Result<String> {
    if unit.kind == Kind::Sentence {
        let text = if marks.contains(&unit.id) {
            page_markup(&unit.tex, side, false, true)?
        } else {
            unit.tex.clone()
        };
        return Ok(format!(
            "{}{}{}{}",
            unit.leading, unit.page_leading, text, unit.page_trailing
        ));
    }
    if marks.contains(&unit.id) {
        if unit.kind == Kind::Math {
            // Keep color resets inside the original math scope. An outer
            // group can add a numbered blank line after a theorem's display.
            let tex = recolor(&unit.tex, side, false, true)?;
            let tokens = lexer::tokens(&tex, Path::new("math.tex"))?;
            if tokens.len() == 1 && matches!(tokens[0].kind, lexer::Kind::Math(_)) {
                let (open, body, close) = lexer::math_parts(&tex);
                return Ok(format!(
                    "{}{open}\\begingroup\\color{{{}}}{body}\\endgroup{close}",
                    unit.leading,
                    color(side)
                ));
            }
            if tokens.first().is_some_and(|token| token.command("begin"))
                && let Some((name, mut start)) = lexer::group(&tokens, 1)
            {
                let name = lexer::source(name);
                if [
                    "equation",
                    "equation*",
                    "displaymath",
                    "mathpar",
                    "mathparpagebreakable",
                ]
                .contains(&name.as_str())
                {
                    while let Some((_, end)) = lexer::optional(&tokens, start) {
                        start = end;
                    }
                    if let Some(end) = tokens.iter().rposition(|token| token.command("end")) {
                        let tag_color = if name.starts_with("equation") {
                            format!(
                                "\\AddToHookNext{{env/{name}/begin}}{{\\TexDiffMathTagColor{{{}}}}}",
                                color(side)
                            )
                        } else {
                            String::new()
                        };
                        return Ok(format!(
                            "{}{tag_color}{}\\begingroup\\color{{{}}}{}\\endgroup{}",
                            unit.leading,
                            lexer::source(&tokens[..start]),
                            color(side),
                            lexer::source(&tokens[start..end]),
                            lexer::source(&tokens[end..])
                        ));
                    }
                }
                return Ok(format!(
                    "{}\\AddToHookNext{{env/{name}/begin}}{{\\TexDiffAlignmentColor{{{}}}}}{tex}",
                    unit.leading,
                    color(side)
                ));
            }
        }
        if unit.identity == "maketitle" {
            return Ok(plain(unit));
        }
        if unit.kind == Kind::Heading {
            let tokens = lexer::tokens(&unit.tex, Path::new("heading.tex"))?;
            let mut start = 1;
            if tokens
                .get(start)
                .is_some_and(|t| t.kind == lexer::Kind::Char('*'))
            {
                start += 1;
            }
            while let Some((_, end)) = lexer::optional(&tokens, start) {
                start = end;
            }
            start = lexer::skip_space(&tokens, start);
            if let Some((body, end)) = lexer::group(&tokens, start) {
                return Ok(format!(
                    "{}{}{}{}",
                    unit.leading,
                    lexer::source(&tokens[..start + 1]),
                    page_markup(&lexer::source(body), side, false, true)?,
                    lexer::source(&tokens[end - 1..])
                ));
            }
        }
        if unit.kind == Kind::Bibliography {
            return Ok(format!(
                "{}{}{}{}",
                unit.leading,
                unit.head,
                page_markup(&unit.tex, side, false, true)?,
                unit.tail
            ));
        }
        let raw = plain(unit);
        return Ok(format!(
            "{}{}",
            unit.leading,
            page_markup(
                &raw[unit.leading.len()..],
                side,
                unit.kind == Kind::Figure,
                false
            )?
        ));
    }
    if let Some(children) = &unit.children {
        let children = children
            .iter()
            .map(|unit| page_unit(unit, marks, side))
            .collect::<Result<Vec<_>>>()?
            .join("");
        Ok(format!(
            "{}{}{}{}",
            unit.leading, unit.head, children, unit.tail
        ))
    } else {
        Ok(plain(unit))
    }
}

const SUPPORT: &str = include_str!("support.tex");

fn input_path(doc: &Document, asset_name: &str, side: &str) -> String {
    if doc.resources.is_empty() {
        return String::new();
    }
    let mut setup = format!(
        "\\makeatletter\\def\\input@path\
         {{{{{asset_name}/project-{side}/{}/}}{{{asset_name}/project-{side}/}}}}\
         \\makeatother\n",
        doc.main_directory.to_string_lossy().replace('\\', "/")
    );
    // input@path is a fallback after the engine's installed-file search. A
    // project-local package with an installed namesake therefore needs an
    // explicit substitution, preserving its logical package/class identity.
    // https://www.latex-project.org/help/documentation/ltfilehook-doc.pdf
    let local: Vec<_> = doc
        .resources
        .keys()
        .filter(|path| {
            path.parent() == Some(doc.main_directory.as_path())
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        ["sty", "cls", "clo", "def", "fd", "cfg"].contains(&extension)
                    })
                && doc
                    .resources
                    .keys()
                    .filter(|other| other.file_name() == path.file_name())
                    .count()
                    == 1
        })
        .collect();
    if !local.is_empty() {
        setup.push_str("\\makeatletter\n\\ifdefined\\declare@file@substitution\n");
        for path in local {
            setup.push_str(&format!(
                "\\declare@file@substitution{{{}}}{{{asset_name}/project-{side}/{}}}\n",
                path.file_name().unwrap().to_string_lossy(),
                path.to_string_lossy().replace('\\', "/")
            ));
        }
        setup.push_str(
            "\\else\n\
             \\PackageError{tex-diff}\
             {Local package overrides require LaTeX 2020-10-01 or newer}\
             {Update your LaTeX distribution to compile this review faithfully.}\n\
             \\fi\n\\makeatother\n",
        );
    }
    setup
}

fn job_binding(name: &str) -> String {
    fn encoded(values: impl Iterator<Item = u32>) -> String {
        let mut tex = "\\def\\jobname{}\n".to_owned();
        for value in values {
            tex.push_str(&format!(
                "\\begingroup\\catcode126=12\\relax\\lccode126={value}\\relax\
                 \\lowercase{{\\endgroup\\edef\\jobname{{\\jobname~}}}}\n"
            ));
        }
        tex
    }
    let unicode = encoded(name.chars().map(u32::from));
    let bytes = encoded(name.bytes().map(u32::from));
    format!(
        "\\ifdefined\\XeTeXrevision\n{unicode}\\else\\ifdefined\\directlua\n{unicode}\\else\n{bytes}\\fi\\fi\n"
    )
}

fn old_macro_bindings(old: Option<&Document>, base: &Document) -> String {
    let mut bindings = "\\makeatletter\n".to_owned();
    if let Some(old) = old
        && old.main_filename != base.main_filename
    {
        bindings.push_str(&job_binding(
            &old.main_filename.file_stem().unwrap().to_string_lossy(),
        ));
    }
    if let Some(old) = old {
        for definition in old.macros.values() {
            // A package has already executed its unchanged definitions. Running
            // those declarations again can corrupt its runtime state.
            if definition.raw.is_empty()
                || base
                    .macros
                    .get(&definition.name)
                    .is_some_and(|current| current.raw == definition.raw)
            {
                continue;
            }
            if definition.environment {
                bindings.push_str(&format!(
                    "\\expandafter\\let\\csname end{}\\endcsname\\relax\n",
                    definition.name
                ));
            }
            bindings.push_str(&format!(
                "\\expandafter\\let\\csname {}\\endcsname\\relax\n{}\n",
                definition.name,
                definition
                    .raw
                    .replace("\\renewcommand", "\\newcommand")
                    .replace("\\renewenvironment", "\\newenvironment")
                    .replace("\\gdef", "\\def")
                    .replace("\\xdef", "\\edef")
            ));
        }
    }
    bindings.push_str("\\makeatother\n");
    bindings
}

fn write_document(
    path: &Path,
    doc: &Document,
    preamble: &str,
    body: &str,
    inputs: &str,
    asset_name: &str,
) -> Result<()> {
    let tex = format!(
        "% Generated by tex-diff. Changes were found in LaTeX source.\n\
         {inputs}{preamble}{SUPPORT}\\begin{{document}}\n\
         {body}{}\n\\end{{document}}\n",
        doc.trailing
    )
    .replace("TEXDIFFASSETS", asset_name);
    fs::write(path, tex)?;
    Ok(())
}

fn native_source(assets: &Path, doc: &Document, side: &str) -> Result<(PathBuf, String)> {
    let root = assets.join(side);
    let master = doc.main_directory.join(&doc.main_filename);
    for (name, original) in &doc.resources {
        if name == &master {
            continue;
        }
        let target = root.join(name);
        fs::create_dir_all(target.parent().unwrap())?;
        fs::copy(original, target)?;
    }
    let directory = root.join(&doc.main_directory);
    fs::create_dir_all(&directory)?;
    let asset_path = std::iter::repeat_n("..", doc.main_directory.components().count() + 1)
        .collect::<Vec<_>>()
        .join("/");
    Ok((directory.join(&doc.main_filename), asset_path))
}

fn has_marked_title(units: &[Unit], marks: &BTreeSet<usize>) -> bool {
    units.iter().any(|u| {
        u.identity == "maketitle" && marks.contains(&u.id)
            || u.children
                .as_ref()
                .is_some_and(|children| has_marked_title(children, marks))
    })
}

fn write_page_source(
    doc: &Document,
    other: Option<&Document>,
    side: ChangeType,
    marks: &BTreeSet<usize>,
    assets: &Path,
    directory: &Path,
) -> Result<PathBuf> {
    let asset_name = assets
        .file_name()
        .context("exported assets directory has no filename")?
        .to_string_lossy();
    let side_name = if side == ChangeType::Removed {
        "old"
    } else {
        "new"
    };
    let body = doc
        .units
        .iter()
        .map(|u| page_unit(u, marks, side))
        .collect::<Result<Vec<_>>>()?
        .join("");
    let mut preamble = doc.preamble.clone();
    if has_marked_title(&doc.units, marks) {
        for (name, value) in &doc.metadata {
            if other.and_then(|d| d.metadata.get(name)) != Some(value) {
                preamble.push_str(&format!(
                    "\\{name}{{{}}}\n",
                    page_markup(value, side, true, true)?
                ));
            }
        }
    }
    let (path, native_assets) = native_source(assets, doc, side_name)?;
    write_document(
        &path,
        doc,
        &preamble,
        &body,
        &input_path(doc, &native_assets, side_name),
        &native_assets,
    )?;
    fs::write(
        directory.join(format!("{side_name}.tex")),
        fs::read_to_string(&path)?.replace(&native_assets, &asset_name),
    )?;
    Ok(path)
}

pub(super) fn generate(
    old: Option<&Document>,
    new: Option<&Document>,
    directory: &Path,
    old_label: &str,
    new_label: &str,
    deadline: Instant,
) -> Result<Review> {
    let base = new
        .or(old)
        .context("at least one LaTeX document is required")?;
    let mut warnings = BTreeSet::new();
    for doc in [old, new].into_iter().flatten() {
        warnings.extend(doc.warnings.iter().cloned());
    }
    let report = Report {
        schema_version: 2,
        old: old_label.into(),
        new: new_label.into(),
        changed: false,
        removed_sentences: 0,
        added_sentences: 0,
        removed_objects: 0,
        added_objects: 0,
        changes: Vec::new(),
        warnings: Vec::new(),
    };
    let mut writer = Writer {
        old,
        new,
        report,
        deadline,
        old_marks: BTreeSet::new(),
        new_marks: BTreeSet::new(),
    };
    let body = writer.sequence(
        old.map_or(&[], |d| d.units.as_slice()),
        new.map_or(&[], |d| d.units.as_slice()),
    )?;
    let mut preamble = base.preamble.clone();
    if let Some(old) = old {
        let current = packages(&preamble)?;
        for (name, command) in packages(&old.preamble)? {
            if !current.contains_key(&name) {
                preamble.push_str(&command);
            }
        }
    }
    let bindings = old_macro_bindings(old, base);
    let mut resources = BTreeMap::new();
    for (side, doc) in [("old", old), ("new", new)]
        .into_iter()
        .filter_map(|(side, doc)| doc.map(|doc| (side, doc)))
    {
        resources.extend(doc.resources.iter().map(|(name, path)| {
            (
                PathBuf::from(format!("project-{side}")).join(name),
                path.clone(),
            )
        }));
    }
    let mut hash = Sha256::new();
    // Resource lookup code is part of the immutable exported asset format.
    hash.update(b"tex-diff-source-assets-v3\0");
    hash.update(SUPPORT.as_bytes());
    hash.update(format!("{preamble}{body}{bindings}").as_bytes());
    for (doc, marks, side) in [
        (old, &writer.old_marks, ChangeType::Removed),
        (new, &writer.new_marks, ChangeType::Added),
    ] {
        if let Some(doc) = doc {
            for unit in &doc.units {
                hash.update(page_unit(unit, marks, side)?.as_bytes());
            }
        }
    }
    for doc in [old, new].into_iter().flatten() {
        hash.update(doc.preamble.as_bytes());
        hash.update(doc.units.iter().map(plain).collect::<String>().as_bytes());
        hash.update(doc.trailing.as_bytes());
    }
    for (name, path) in &resources {
        hash.update(name.as_os_str().as_encoded_bytes());
        hash.update(fs::read(path)?);
    }
    let hash = format!("{:x}", hash.finalize());
    let asset_name = format!("tex-diff-assets-{}", &hash[..16]);
    let assets = directory.join(&asset_name);
    fs::create_dir_all(&assets)?;
    fs::write(assets.join("old-macros.tex"), bindings)?;
    for doc in [old, new].into_iter().flatten() {
        for (name, path) in &doc.assets {
            fs::copy(path, assets.join(name))?;
        }
    }
    for (name, path) in resources {
        let target = assets.join(name);
        fs::create_dir_all(target.parent().unwrap())?;
        fs::copy(path, target)?;
    }
    let source = directory.join("review.tex");
    let mut inputs = input_path(base, &asset_name, if new.is_some() { "new" } else { "old" });
    if new.is_some()
        && let Some(old) = old
        && !old.resources.is_empty()
    {
        if inputs.is_empty() {
            inputs = input_path(old, &asset_name, "old");
        } else {
            inputs.push_str(&format!(
                "\\makeatletter\\g@addto@macro\\input@path\
                 {{{{{asset_name}/project-old/{}/}}{{{asset_name}/project-old/}}}}\
                 \\makeatother\n",
                old.main_directory.to_string_lossy().replace('\\', "/")
            ));
        }
    }
    write_document(&source, base, &preamble, &body, &inputs, &asset_name)?;
    let (unified_source, unified_assets) = native_source(&assets, base, "unified")?;
    fs::write(
        &unified_source,
        fs::read_to_string(&source)?.replace(&asset_name, &unified_assets),
    )?;
    let old_source = old
        .map(|doc| {
            write_page_source(
                doc,
                new,
                ChangeType::Removed,
                &writer.old_marks,
                &assets,
                directory,
            )
        })
        .transpose()?;
    let new_source = new
        .map(|doc| {
            write_page_source(
                doc,
                old,
                ChangeType::Added,
                &writer.new_marks,
                &assets,
                directory,
            )
        })
        .transpose()?;
    writer.report.warnings = warnings.into_iter().collect();
    writer.report.changed = !writer.report.changes.is_empty();
    for change in &writer.report.changes {
        match (change.change, change.kind == Kind::Sentence) {
            (ChangeType::Removed, true) => writer.report.removed_sentences += 1,
            (ChangeType::Added, true) => writer.report.added_sentences += 1,
            (ChangeType::Removed, false) => writer.report.removed_objects += 1,
            (ChangeType::Added, false) => writer.report.added_objects += 1,
        }
    }
    Ok(Review {
        source,
        unified_source,
        old_source,
        new_source,
        assets,
        report: writer.report,
    })
}
