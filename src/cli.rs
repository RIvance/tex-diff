use clap::{Parser, ValueEnum};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const AFTER_HELP: &str = concat!(
    "Examples:\n",
    "  tex-diff                         index → working tree, open a temporary PDF\n",
    "  tex-diff --save                   also keep tex-diff.pdf in the current directory\n",
    "  tex-diff --staged                 HEAD → index\n",
    "  tex-diff HEAD~2 -- paper.tex      commit → working tree\n",
    "  tex-diff v1 v2                    commit → commit\n",
    "  tex-diff main...feature           merge base → feature\n",
    "  tex-diff --no-open -o review.pdf   save PDF without opening a viewer\n",
    "  tex-diff --tex-output review.tex  also save editable LaTeX and its assets\n",
    "  tex-diff --doctor                 check your installation\n",
    "\n",
    "Comparison parses LaTeX source with pest; it does not compare compiled PDFs.\n",
    "The entire document is included, with removed text red and added text blue.",
);

#[derive(Clone, ValueEnum)]
pub(crate) enum Engine {
    Auto,
    Pdflatex,
    Xelatex,
    Lualatex,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Mode {
    Unified,
    SideBySide,
}

impl Engine {
    pub(crate) fn name(&self, source: &Path) -> &'static str {
        match self {
            Self::Pdflatex => "pdflatex",
            Self::Xelatex => "xelatex",
            Self::Lualatex => "lualatex",
            Self::Auto => {
                for line in fs::read_to_string(source)
                    .unwrap_or_default()
                    .lines()
                    .take(30)
                {
                    let line = line.to_lowercase();
                    if line.trim_start().starts_with('%')
                        && line.contains("!tex")
                        && line.contains("program")
                    {
                        if line.contains("xelatex") {
                            return "xelatex";
                        }
                        if line.contains("lualatex") {
                            return "lualatex";
                        }
                    }
                }
                "pdflatex"
            }
        }
    }
}

/// Review LaTeX source changes as one complete PDF, then open it in your viewer.
#[derive(Parser)]
#[command(
    version,
    after_help = AFTER_HELP
)]
pub(crate) struct Cli {
    /// Review layout: a combined document, or paired pages with each version's pagination.
    #[arg(long, value_enum, default_value = "unified")]
    pub(crate) mode: Mode,

    /// Compare HEAD (or the given base revision) against the index.
    #[arg(long, visible_alias = "cached")]
    pub(crate) staged: bool,

    /// Git revisions, including A..B or A...B.
    #[arg(value_name = "REV", num_args = 0..=2)]
    pub(crate) revisions: Vec<String>,

    /// Main document after -- (equivalent to --main).
    #[arg(last = true, value_name = "FILE", conflicts_with = "main")]
    pub(crate) file: Option<PathBuf>,

    /// Main document; otherwise automatically discovered.
    #[arg(long, short = 'm', value_name = "FILE")]
    pub(crate) main: Option<PathBuf>,

    /// Old entry point, when it was renamed.
    #[arg(long, value_name = "FILE")]
    pub(crate) old_main: Option<PathBuf>,

    /// Save the review PDF at a custom path (overrides --save).
    #[arg(long, short = 'o', value_name = "PDF")]
    pub(crate) output: Option<PathBuf>,

    /// Keep tex-diff.pdf in the current directory; --output chooses a custom path.
    #[arg(long)]
    pub(crate) save: bool,

    /// Save complete unified LaTeX, annotated old/new sources, and portable assets.
    #[arg(long, value_name = "TEX")]
    pub(crate) tex_output: Option<PathBuf>,

    /// Generate source only; requires --tex-output.
    #[arg(long, requires = "tex_output")]
    pub(crate) no_pdf: bool,

    /// Save changed sentences/objects and their source locations as JSON.
    #[arg(long, value_name = "JSON")]
    pub(crate) report: Option<PathBuf>,

    /// Engine; auto reads % !TEX program and otherwise uses pdflatex.
    #[arg(long, value_enum, default_value = "auto")]
    pub(crate) engine: Engine,

    /// Create the PDF without opening a desktop viewer.
    #[arg(long)]
    pub(crate) no_open: bool,

    /// PDF viewer executable; alternatively set TEX_DIFF_VIEWER.
    #[arg(long, value_name = "EXECUTABLE", conflicts_with = "no_open")]
    pub(crate) viewer: Option<PathBuf>,

    /// Check required tools and compile a small document outside any repository.
    #[arg(long)]
    pub(crate) doctor: bool,

    /// Deadline in seconds for source comparison and each compilation.
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u64).range(1..))]
    pub(crate) timeout: u64,

    /// Retain snapshots, generated source, and logs after success.
    #[arg(long)]
    pub(crate) keep_build: bool,

    /// Exit with 1 for changes, 0 for no changes; errors use 2.
    #[arg(long)]
    pub(crate) exit_code: bool,
}

impl Cli {
    pub(crate) fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout)
    }
}
