//! Coordinate Git snapshots, source comparison, compilation, and review output.

use crate::{
    cli::{Cli, Mode},
    doctor,
    git::{Repository, Revision},
    output::{self, PdfDestination},
    viewer,
    worker::{self, SourceFiles},
};
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tex_diff::{
    build,
    latex::{self, Input, Report},
};

struct Snapshot {
    root: PathBuf,
    main: Option<PathBuf>,
    label: String,
}

impl Snapshot {
    fn input(&self) -> Option<Input<'_>> {
        self.main.as_deref().map(|main| Input {
            root: &self.root,
            main,
        })
    }
}

pub(crate) fn run(cli: &Cli) -> Result<bool> {
    if cli.doctor {
        doctor::check(&cli.engine, cli.timeout())?;
        return Ok(false);
    }

    let mut pdf_output = PdfDestination::new(cli.output.as_deref(), cli.save, !cli.no_pdf)?;
    if let Some(path) = &cli.tex_output {
        output::validate_source(path)?;
    }
    if let Some(path) = &cli.report {
        output::validate(path, "json", "--report")?;
    }

    let cwd = std::env::current_dir()?;
    let repo = Repository::discover(&cwd)?;
    let (old, new) = repo.select(cli.staged, &cli.revisions)?;
    let main = cli
        .main
        .as_ref()
        .or(cli.file.as_ref())
        .map(|path| repo.relative_path(&cwd, path))
        .transpose()?;
    let old_main = cli
        .old_main
        .as_ref()
        .map(|path| repo.relative_path(&cwd, path))
        .transpose()?;

    let workspace = tempfile::Builder::new().prefix("tex-diff-").tempdir()?;
    let report = match produce(
        cli,
        &repo,
        (&old, &new),
        main.as_deref(),
        old_main.as_deref(),
        workspace.path(),
        pdf_output.path(),
    ) {
        Ok(report) => report,
        Err(error) => {
            return Err(error).context(format!(
                "build files retained at {}",
                workspace.keep().display()
            ));
        }
    };

    pdf_output.retain();
    if cli.keep_build {
        eprintln!("Build files: {}", workspace.keep().display());
    }
    print_report(&report);

    let output = if cli.no_pdf {
        cli.tex_output
            .as_deref()
            .context("--no-pdf requires --tex-output")?
    } else {
        pdf_output.path()
    };
    println!("{}", output.display());

    if !cli.no_pdf && !cli.no_open {
        viewer::open_pdf(pdf_output.path(), cli.viewer.as_deref())?;
    }

    Ok(report.changed)
}

fn print_report(report: &Report) {
    if report.changed {
        eprintln!(
            "Sentences: {} removed, {} added. Objects: {} removed, {} added.",
            report.removed_sentences,
            report.added_sentences,
            report.removed_objects,
            report.added_objects,
        );
    } else {
        eprintln!("No content changes after LaTeX normalization.");
    }

    for warning in &report.warnings {
        eprintln!("Note: {warning}");
    }
}

fn produce(
    cli: &Cli,
    repo: &Repository,
    revisions: (&Revision, &Revision),
    main: Option<&Path>,
    old_main: Option<&Path>,
    workspace: &Path,
    pdf_output: &Path,
) -> Result<Report> {
    let (old_revision, new_revision) = revisions;
    eprintln!(
        "Comparing {} → {}",
        old_revision.label(),
        new_revision.label()
    );

    let old_root = workspace.join("old");
    let new_root = workspace.join("new");
    repo.snapshot(old_revision, &old_root)?;
    repo.snapshot(new_revision, &new_root)?;

    let new = Snapshot {
        main: find_main(&new_root, main, None)?,
        root: new_root,
        label: new_revision.label(),
    };
    let old = Snapshot {
        main: find_main(&old_root, old_main.or(main), new.main.as_deref())?,
        root: old_root,
        label: old_revision.label(),
    };
    ensure!(
        old.main.is_some() || new.main.is_some(),
        "no LaTeX document found; stage your main .tex file or use --main"
    );

    let source_dir = workspace.join("source");
    let comparison = worker::compare(
        old.input(),
        new.input(),
        &source_dir,
        &workspace.join("compare.log"),
        (&old.label, &new.label),
        cli.timeout(),
    )?;
    let pdf = if cli.no_pdf {
        None
    } else {
        Some(compile_pdf(
            cli,
            &old,
            &new,
            &comparison.sources,
            workspace,
        )?)
    };

    output::publish(
        &source_dir,
        pdf.as_deref().map(|path| (path, pdf_output)),
        cli.tex_output.as_deref(),
        cli.report.as_deref(),
    )?;

    Ok(comparison.report)
}

fn compile_pdf(
    cli: &Cli,
    old: &Snapshot,
    new: &Snapshot,
    sources: &SourceFiles,
    workspace: &Path,
) -> Result<PathBuf> {
    match cli.mode {
        Mode::Unified => {
            let base = if new.main.is_some() { new } else { old };
            let main = base.main.as_ref().context("missing review entry point")?;
            let engine = cli.engine.name(&base.root.join(main));
            eprintln!("Compiling the complete review document with {engine}");
            compile_review(&sources.unified, workspace, "review", engine, cli.timeout())
        }
        Mode::SideBySide => {
            let old_pdf = compile_side(old, sources.old.as_deref(), cli, workspace, "old")?;
            let new_pdf = compile_side(new, sources.new.as_deref(), cli, workspace, "new")?;
            let output = workspace.join("paired.pdf");
            let (old_pages, new_pages) = tex_diff::composition::pair(
                old_pdf.as_deref(),
                new_pdf.as_deref(),
                &output,
                &old.label,
                &new.label,
                Instant::now() + cli.timeout(),
            )?;
            eprintln!("Paired {old_pages} original pages and {new_pages} revised pages");
            Ok(output)
        }
    }
}

fn compile_side(
    snapshot: &Snapshot,
    source: Option<&Path>,
    cli: &Cli,
    workspace: &Path,
    side: &str,
) -> Result<Option<PathBuf>> {
    let Some(main) = &snapshot.main else {
        return Ok(None);
    };

    let engine = cli.engine.name(&snapshot.root.join(main));
    eprintln!("Compiling {side} pages with {engine}, retaining their own pagination");
    let source = source.with_context(|| format!("missing annotated {side} version"))?;
    compile_review(source, workspace, side, engine, cli.timeout()).map(Some)
}

fn compile_review(
    source: &Path,
    workspace: &Path,
    name: &str,
    engine: &str,
    timeout: Duration,
) -> Result<PathBuf> {
    let directory = source
        .parent()
        .context("annotated source has no parent directory")?;
    let main = source
        .file_name()
        .context("annotated source has no filename")?;
    let build_dir = workspace.join(format!("{name}-build"));
    let pdf = build::compile(directory, Path::new(main), &build_dir, engine, timeout)?;

    let alias = build_dir.join(format!("{name}.pdf"));
    if pdf != alias {
        fs::copy(&pdf, &alias)
            .with_context(|| format!("saving compiled review to {}", alias.display()))?;
    }

    Ok(alias)
}

fn find_main(
    snapshot: &Path,
    requested: Option<&Path>,
    preferred: Option<&Path>,
) -> Result<Option<PathBuf>> {
    if let Some(path) = requested {
        return Ok(snapshot.join(path).is_file().then(|| path.to_owned()));
    }
    if let Some(path) = preferred.filter(|p| snapshot.join(p).is_file()) {
        return Ok(Some(path.to_owned()));
    }
    let mut found = Vec::new();
    scan_tex(snapshot, snapshot, &mut found)?;
    found.sort();
    ensure!(
        found.len() <= 1,
        "multiple main documents found: {}; select one with --main (or --old-main)",
        found
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(found.pop())
}

fn scan_tex(root: &Path, directory: &Path, found: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            if !entry
                .file_name()
                .to_string_lossy()
                .starts_with("tex-diff-assets-")
            {
                scan_tex(root, &path, found)?;
            }
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("tex"))
        {
            ensure!(
                fs::metadata(&path)?.len() <= 16 * 1024 * 1024,
                "{} exceeds 16 MiB; select --main to skip unrelated large sources",
                path.display()
            );
            let source = fs::read_to_string(&path)
                .with_context(|| format!("reading UTF-8 source {}", path.display()))?;
            if latex::is_main(&source, &path)? {
                found.push(path.strip_prefix(root)?.to_owned());
            }
        }
    }
    Ok(())
}
