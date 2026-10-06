//! Run source comparison in a child process so timeouts also stop TeX dependencies.

use anyhow::{Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use tex_diff::{
    build,
    latex::{self, Input, Report},
};

pub(crate) const COMMAND: &str = "__source_compare";

#[derive(Parser)]
struct Worker {
    #[arg(long, requires = "old_main")]
    old_root: Option<PathBuf>,

    #[arg(long, requires = "old_root")]
    old_main: Option<PathBuf>,

    #[arg(long, requires = "new_main")]
    new_root: Option<PathBuf>,

    #[arg(long, requires = "new_root")]
    new_main: Option<PathBuf>,

    #[arg(long)]
    directory: PathBuf,

    #[arg(long)]
    old_label: String,

    #[arg(long)]
    new_label: String,

    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    timeout: u64,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct SourceFiles {
    pub(crate) old: Option<PathBuf>,
    pub(crate) new: Option<PathBuf>,
    pub(crate) unified: PathBuf,
}

pub(crate) struct Comparison {
    pub(crate) report: Report,
    pub(crate) sources: SourceFiles,
}

pub(crate) fn run() -> Result<()> {
    let worker = Worker::parse_from(std::env::args_os().skip(1));
    let review = latex::compare(
        worker
            .old_root
            .as_deref()
            .zip(worker.old_main.as_deref())
            .map(|(root, main)| Input { root, main }),
        worker
            .new_root
            .as_deref()
            .zip(worker.new_main.as_deref())
            .map(|(root, main)| Input { root, main }),
        &worker.directory,
        &worker.old_label,
        &worker.new_label,
        Duration::from_secs(worker.timeout),
    )?;
    let sources = SourceFiles {
        old: review.old_source,
        new: review.new_source,
        unified: review.unified_source,
    };

    fs::write(
        worker.directory.join("report.json"),
        serde_json::to_vec_pretty(&review.report)?,
    )
    .context("writing source comparison report")?;
    fs::write(
        worker.directory.join("source-files.json"),
        serde_json::to_vec_pretty(&sources)?,
    )
    .context("writing annotated source paths")?;

    Ok(())
}

pub(crate) fn compare(
    old: Option<Input<'_>>,
    new: Option<Input<'_>>,
    directory: &Path,
    log: &Path,
    labels: (&str, &str),
    timeout: Duration,
) -> Result<Comparison> {
    fs::create_dir_all(directory)?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg(COMMAND)
        .arg("--directory")
        .arg(directory)
        .arg("--old-label")
        .arg(labels.0)
        .arg("--new-label")
        .arg(labels.1)
        .arg("--timeout")
        .arg(timeout.as_secs().to_string());

    if let Some(input) = old {
        command
            .arg("--old-root")
            .arg(input.root)
            .arg("--old-main")
            .arg(input.main);
    }
    if let Some(input) = new {
        command
            .arg("--new-root")
            .arg(input.root)
            .arg("--new-main")
            .arg(input.main);
    }

    eprintln!("Parsing LaTeX and annotating sentences and objects");
    build::run_logged(command, log, timeout)?;

    let report_path = directory.join("report.json");
    let report = serde_json::from_slice(
        &fs::read(&report_path).with_context(|| format!("reading {}", report_path.display()))?,
    )
    .context("invalid source comparison report")?;
    let sources_path = directory.join("source-files.json");
    let sources = serde_json::from_slice(
        &fs::read(&sources_path).with_context(|| format!("reading {}", sources_path.display()))?,
    )
    .context("invalid annotated source paths")?;

    Ok(Comparison { report, sources })
}
