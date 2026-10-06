//! Resolve classic BibTeX into LaTeX entries before comparing source. No PDF is
//! produced here; uncited database records cannot become false content changes.

use super::{
    expand::{self, Expanded},
    lexer::{self, Kind, Token},
};
use anyhow::{Context, Result, ensure};
use std::{collections::BTreeSet, fs, path::Path, process::Command, time::Instant};

pub(super) fn expand(document: &mut Expanded, directory: &Path, deadline: Instant) -> Result<()> {
    let tokens = &document.tokens;
    let mut citations = Vec::new();
    let mut seen = BTreeSet::new();
    let mut style = None;
    let mut databases = Vec::new();
    let mut commands = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if let Some(end) = expand::declaration_end(tokens, i) {
            i = end;
            continue;
        }
        let token = &tokens[i];
        ensure!(
            !token.command("addbibresource") && !token.command("printbibliography"),
            "biblatex/biber is not supported yet; use classic \\bibliography with BibTeX"
        );
        if token.command("bibliography") || token.command("bibliographystyle") {
            let (value, end) =
                lexer::group(tokens, i + 1).context("bibliography command requires a filename")?;
            ensure!(
                !value.iter().any(|t| matches!(t.kind, Kind::Command(_))),
                "dynamic bibliography filenames are not supported"
            );
            let value = lexer::source(value);
            if token.command("bibliographystyle") {
                style = Some(value.trim().to_owned());
            } else {
                databases.extend(value.split(',').map(str::trim).map(str::to_owned));
                commands.push((i, end));
            }
            i = end;
            continue;
        }
        if let Kind::Command(name) = &token.kind
            && [
                "cite",
                "citet",
                "citep",
                "citealp",
                "citealt",
                "citeauthor",
                "citeyear",
                "citeyearpar",
                "citenum",
                "nocite",
            ]
            .contains(&name.to_lowercase().as_str())
        {
            let mut end = i + 1;
            if tokens.get(end).is_some_and(|t| t.kind == Kind::Char('*')) {
                end += 1;
            }
            while let Some((_, next)) = lexer::optional(tokens, end) {
                end = next;
            }
            if let Some((keys, end)) = lexer::group(tokens, end) {
                let keys = lexer::source(keys);
                if seen.insert(keys.clone()) {
                    citations.push(keys);
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
    if commands.is_empty() {
        return Ok(());
    }
    ensure!(
        commands.len() == 1,
        "multiple \\bibliography commands are not supported in one document"
    );
    let style = style.context("\\bibliography requires a \\bibliographystyle declaration")?;
    let cwd = document
        .root
        .join(document.main.parent().unwrap_or(Path::new("")));
    for database in &databases {
        let mut path = cwd.join(database);
        if path.extension().is_none() {
            path.set_extension("bib");
        }
        let actual = fs::canonicalize(&path)
            .with_context(|| format!("reading bibliography {}", path.display()))?;
        ensure!(
            actual.starts_with(&document.root),
            "bibliography escapes the repository: {database}"
        );
    }
    let (start, end) = commands[0];
    let replacement = if citations.is_empty() {
        Vec::new()
    } else {
        fs::create_dir_all(directory)?;
        let directory = fs::canonicalize(directory)?;
        let mut aux = "\\relax\n".to_owned();
        for citation in citations {
            aux.push_str(&format!("\\citation{{{citation}}}\n"));
        }
        aux.push_str(&format!(
            "\\bibstyle{{{style}}}\n\\bibdata{{{}}}\n",
            databases.join(",")
        ));
        fs::write(directory.join("review.aux"), aux)?;
        let mut command = Command::new("bibtex");
        command.current_dir(cwd).arg(directory.join("review"));
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .context("bibliography comparison exceeded its timeout")?;
        crate::build::run_logged(command, &directory.join("build.log"), remaining)
            .context("resolving BibTeX entries for source comparison")?;
        let bbl = fs::read_to_string(directory.join("review.bbl"))
            .context("BibTeX did not generate UTF-8 LaTeX")?;
        let mut replacement = lexer::tokens(&bbl, &tokens[start].source.file)?;
        for token in &mut replacement {
            token.source = tokens[start].source.clone();
            token.base.clone_from(&tokens[start].base);
        }
        replacement
    };
    let mut out: Vec<Token> = tokens[..start].to_vec();
    out.extend(replacement);
    out.extend_from_slice(&tokens[end..]);
    document.tokens = out;
    Ok(())
}
