//! Generate the checked-in examples using the real source-comparison CLI.

use anyhow::{Context, Result, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn run(program: &Path, cwd: &Path, arguments: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .current_dir(cwd)
        .args(arguments)
        .status()
        .with_context(|| format!("starting {}", program.display()))?;
    ensure!(status.success(), "{} failed: {status}", program.display());
    Ok(())
}

fn main() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let repo = directory.path();
    let project = Path::new(env!("CARGO_MANIFEST_DIR"));
    let executable = project.join("target/debug/tex-diff");
    ensure!(
        executable.is_file(),
        "run cargo build before generating the demo"
    );
    run(Path::new("git"), repo, &["init", "-q"])?;
    let preamble = "\\documentclass[10pt]{article}\n\
                    \\usepackage[margin=22mm]{geometry}\n\
                    \\title{Reviewing a LaTeX Project}\\author{tex-diff}\\date{}\n";
    let body = r"\maketitle
\section{An entire document}
This opening sentence is unchanged. The original method uses two processing stages.
All surrounding paragraphs remain in this review. A changed word colors its whole sentence.
\section{Structured content}
\begin{itemize}
\item The original implementation handles a basic document.
\item This unchanged item stays in place.
\end{itemize}
\begin{figure}[ht]\centering\input{pipeline}\caption{The processing pipeline}\end{figure}
\section{Unchanged appendix}
Every appendix paragraph is included. This final sentence is unchanged.
";
    let source = |body: &str| format!("{preamble}\\begin{{document}}\n{body}\n\\end{{document}}\n");
    fs::write(repo.join("main.tex"), source(body))?;
    fs::write(
        repo.join("pipeline.tex"),
        concat!(
            r"\setlength{\unitlength}{1pt}\begin{picture}(290,45)",
            r"\put(0,8){\framebox(105,30){Parse source}}",
            r"\put(110,23){\vector(1,0){60}}",
            r"\put(175,8){\framebox(105,30){Build PDF}}\end{picture}",
        ),
    )?;
    run(Path::new("git"), repo, &["add", "."])?;
    run(
        Path::new("git"),
        repo,
        &[
            "-c",
            "user.name=tex-diff example",
            "-c",
            "user.email=example@example.invalid",
            "commit",
            "-qm",
            "original document",
        ],
    )?;
    fs::write(
        repo.join("main.tex"),
        source(
            &body
                .replace(
                    "The original method uses two processing stages.",
                    "The revised method uses three processing stages.",
                )
                .replace(
                    "The original implementation handles a basic document.",
                    "The Rust implementation handles sentences, objects, and Git revisions.",
                ),
        ),
    )?;
    fs::write(
        repo.join("pipeline.tex"),
        concat!(
            r"\setlength{\unitlength}{1pt}\begin{picture}(390,45)",
            r"\put(0,8){\framebox(105,30){Parse source}}",
            r"\put(110,23){\vector(1,0){25}}",
            r"\put(140,8){\framebox(105,30){Mark changes}}",
            r"\put(250,23){\vector(1,0){25}}",
            r"\put(280,8){\framebox(105,30){Build PDF}}\end{picture}",
        ),
    )?;
    for (mode, name) in [("unified", "review"), ("side-by-side", "side-by-side")] {
        let pdf = project.join(format!("examples/{name}.pdf"));
        let report = project.join(format!("examples/{name}.json"));
        run(
            &executable,
            repo,
            &[
                "--mode",
                mode,
                "--no-open",
                "--output",
                pdf.to_str().context("example path is not UTF-8")?,
                "--report",
                report.to_str().context("report path is not UTF-8")?,
            ],
        )?;
        let preview: PathBuf = project.join(format!("examples/{name}"));
        run(
            Path::new("pdftoppm"),
            project,
            &[
                "-f",
                "1",
                "-singlefile",
                "-scale-to",
                "1400",
                "-png",
                pdf.to_str().context("PDF path is not UTF-8")?,
                preview.to_str().context("preview path is not UTF-8")?,
            ],
        )?;
        println!("Created {}", pdf.display());
    }
    if !std::env::args().any(|arg| arg == "--no-open") {
        #[cfg(target_os = "linux")]
        {
            Command::new("xdg-open")
                .arg(project.join("examples/review.pdf"))
                .spawn()?;
        }

        #[cfg(target_os = "macos")]
        {
            Command::new("open")
                .arg(project.join("examples/review.pdf"))
                .spawn()?;
        }
    }
    Ok(())
}
