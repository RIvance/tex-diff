use crate::cli::Engine;
use anyhow::{Context, Result, ensure};
use std::{fs, path::Path, process::Command, time::Duration};
use tex_diff::build;

pub(crate) fn check(engine: &Engine, timeout: Duration) -> Result<()> {
    let directory = tempfile::tempdir()?;
    let selected = engine.name(&directory.path().join("main.tex"));
    let mut missing = Vec::new();
    for (program, flag) in [
        ("git", "--version"),
        ("latexmk", "-v"),
        (selected, "--version"),
    ] {
        let mut command = Command::new(program);
        command.arg(flag);
        if build::run_logged(
            command,
            &directory.path().join(format!("{program}.log")),
            Duration::from_secs(10),
        )
        .is_ok()
        {
            println!("{program}: available");
        } else {
            println!("{program}: missing");
            missing.push(program);
        }
    }
    ensure!(
        missing.is_empty(),
        "install {} and run tex-diff --doctor again",
        missing.join(", ")
    );
    fs::write(
        directory.path().join("main.tex"),
        "\\documentclass{article}\n\
         \\usepackage{xcolor}\n\
         \\begin{document}\\textcolor{red}{Removed.} \\textcolor{blue}{Added.}\
         \\end{document}\n",
    )?;
    if let Err(error) = build::compile(
        directory.path(),
        Path::new("main.tex"),
        &directory.path().join("build"),
        selected,
        timeout,
    ) {
        return Err(error).context(format!(
            "diagnostic files retained at {}",
            directory.keep().display()
        ));
    }
    println!("{selected}: compilation passed\nLaTeX parser: built in (pest)");
    Ok(())
}
