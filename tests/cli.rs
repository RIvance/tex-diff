use std::{
    cell::{Cell, RefCell},
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{Duration, Instant},
};
use tex_diff::latex::Report;

const PREAMBLE: &str = "\\documentclass{article}\n\\usepackage{graphicx}\n";

const BODY: &str = "An original sentence. This sentence remains.";

struct Fixture {
    temporary: tempfile::TempDir,
    repo: PathBuf,
    counter: Cell<usize>,
    retained: RefCell<Vec<PathBuf>>,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::Builder::new()
            .prefix("tex diff test ")
            .tempdir()
            .unwrap();
        let repo = temporary.path().join("project");
        fs::create_dir(&repo).unwrap();
        let fixture = Self {
            temporary,
            repo,
            counter: Cell::new(0),
            retained: RefCell::new(Vec::new()),
        };
        fixture.git(&["init", "-q", "-b", "main"]);
        fs::write(
            fixture.repo.join(".gitignore"),
            "*.pdf\n*.aux\n*.log\n*.fls\n*.fdb_latexmk\n",
        )
        .unwrap();
        fixture.write(BODY);
        fixture.git(&["add", "."]);
        fixture.commit("initial");
        fixture
    }

    fn git(&self, args: &[&str]) -> Vec<u8> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn commit(&self, message: &str) {
        self.git(&[
            "-c",
            "user.name=tex-diff tests",
            "-c",
            "user.email=tests@example.invalid",
            "commit",
            "-qm",
            message,
        ]);
    }

    fn write(&self, body: &str) {
        self.source("main.tex", PREAMBLE, body);
    }

    fn source(&self, name: &str, preamble: &str, body: &str) {
        fs::write(
            self.repo.join(name),
            format!("{preamble}\\begin{{document}}\n{body}\n\\end{{document}}\n"),
        )
        .unwrap();
    }

    fn execute(
        &self,
        args: &[&str],
        expected: i32,
        cwd: Option<&Path>,
        path: Option<&std::ffi::OsStr>,
        destination: Option<&Path>,
    ) -> (Output, PathBuf, PathBuf) {
        let number = self.counter.get() + 1;
        self.counter.set(number);
        let output = destination
            .map(Path::to_owned)
            .unwrap_or_else(|| self.temporary.path().join(format!("diff-{number}.pdf")));
        let report = self.temporary.path().join(format!("diff-{number}.json"));
        let status = self.git(&["status", "--porcelain=v1", "-z"]);
        let index = fs::read(self.repo.join(".git/index")).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_tex-diff"));
        command
            .current_dir(cwd.unwrap_or(&self.repo))
            .arg("--no-open")
            .arg("--output")
            .arg(&output)
            .arg("--report")
            .arg(&report)
            .args(args);
        if let Some(path) = path {
            command.env("PATH", path);
        }
        let result = command.output().unwrap();
        let stderr = String::from_utf8_lossy(&result.stderr);
        for line in stderr.lines() {
            if let Some((_, path)) = line
                .split_once("build files retained at ")
                .or_else(|| line.split_once("Build files: "))
            {
                self.retained.borrow_mut().push(PathBuf::from(
                    path.split_once(": ").map_or(path, |(path, _)| path),
                ));
            }
        }
        assert_eq!(result.status.code(), Some(expected), "{stderr}");
        assert_eq!(
            self.git(&["status", "--porcelain=v1", "-z"]),
            status,
            "repository status changed"
        );
        assert_eq!(
            fs::read(self.repo.join(".git/index")).unwrap(),
            index,
            "Git index changed"
        );
        if expected != 2 && !args.contains(&"--no-pdf") {
            let bytes = fs::read(&output).unwrap();
            assert!(bytes.starts_with(b"%PDF-") && bytes.windows(5).any(|w| w == b"%%EOF"));
        }
        (result, output, report)
    }

    fn diff(&self, args: &[&str]) -> Report {
        self.diff_code(args, 0)
    }

    fn diff_code(&self, args: &[&str], expected: i32) -> Report {
        let (_, _, report) = self.execute(args, expected, None, None, None);
        serde_json::from_slice(&fs::read(report).unwrap()).unwrap()
    }

    fn error(&self, args: &[&str]) -> String {
        String::from_utf8(self.execute(args, 2, None, None, None).0.stderr).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for path in self.retained.get_mut() {
            let _ = fs::remove_dir_all(path);
        }
    }
}

fn texts(report: &Report) -> Vec<&str> {
    report.changes.iter().map(|c| c.text.as_str()).collect()
}

fn png(path: &Path, rgb: [u8; 3]) {
    let mut encoder = png::Encoder::new(fs::File::create(path).unwrap(), 16, 16);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&rgb.repeat(16 * 16))
        .unwrap();
}

#[test]
fn staged_unstaged_and_head_versions_are_isolated() {
    let f = Fixture::new();
    f.write(&BODY.replace("original", "staged"));
    f.git(&["add", "main.tex"]);
    f.write(&BODY.replace("original", "unstaged"));
    assert_eq!(
        texts(&f.diff(&["--staged"])),
        ["An original sentence.", "An staged sentence."]
    );
    assert_eq!(
        texts(&f.diff(&[])),
        ["An staged sentence.", "An unstaged sentence."]
    );
    assert_eq!(
        texts(&f.diff(&["HEAD"])),
        ["An original sentence.", "An unstaged sentence."]
    );
    assert_eq!(f.diff(&["--cached", "HEAD"]).added_sentences, 1);
}

#[test]
fn comments_whitespace_and_nested_includes_have_no_visible_diff() {
    let f = Fixture::new();
    fs::create_dir(f.repo.join("parts")).unwrap();
    fs::write(
        f.repo.join("parts/body.tex"),
        "% invisible comment\n\\input{parts/content}\n",
    )
    .unwrap();
    fs::write(
        f.repo.join("parts/content.tex"),
        "An\n original   sentence.\nThis sentence remains.\n",
    )
    .unwrap();
    f.write("\\include{parts/body}");
    assert!(!f.diff(&["--exit-code"]).changed);
}

#[test]
fn rendered_preamble_macros_are_compared() {
    let f = Fixture::new();
    let body = "An \\choice{} sentence. This sentence remains.";
    f.source(
        "main.tex",
        &(PREAMBLE.to_owned() + "\\newcommand{\\choice}{original}\n"),
        body,
    );
    f.git(&["add", "."]);
    f.commit("macro");
    f.source(
        "main.tex",
        &(PREAMBLE.to_owned() + "\\newcommand{\\choice}{updated}\n"),
        body,
    );
    assert_eq!(
        texts(&f.diff(&[])),
        ["An original sentence.", "An updated sentence."]
    );
    f.source(
        "main.tex",
        &(PREAMBLE.to_owned()
            + "\\newcommand{\\unused}{invisible}\n\\newcommand{\\choice}{original}\n"),
        body,
    );
    assert!(!f.diff(&[]).changed);
}

#[test]
fn historical_commits_ranges_and_merge_bases_ignore_local_edits() {
    let f = Fixture::new();
    f.write(&BODY.replace("original", "committed"));
    f.git(&["add", "."]);
    f.commit("second");
    f.write(&BODY.replace("original", "working"));
    for args in [
        &["HEAD~1", "HEAD"][..],
        &["HEAD~1..HEAD"],
        &["HEAD~1...HEAD"],
        &["HEAD~1.."],
    ] {
        assert_eq!(
            texts(&f.diff(args)),
            ["An original sentence.", "An committed sentence."]
        );
    }
    f.git(&["checkout", "--", "main.tex"]);
    f.git(&["checkout", "-qb", "feature", "HEAD~1"]);
    f.write(&BODY.replace("original", "feature"));
    f.git(&["add", "."]);
    f.commit("feature");
    assert_eq!(
        texts(&f.diff(&["main...feature"])),
        ["An original sentence.", "An feature sentence."]
    );
}

#[test]
fn changed_figure_pixels_are_detected_and_renames_are_ignored() {
    let f = Fixture::new();
    let image = f.repo.join("figure.png");
    png(&image, [255, 0, 0]);
    f.write(&(BODY.to_owned() + "\n\\includegraphics[width=2cm]{figure.png}"));
    f.git(&["add", "."]);
    f.commit("image");
    png(&image, [0, 0, 255]);
    let report = f.diff(&[]);
    assert_eq!(
        (
            report.removed_sentences,
            report.added_sentences,
            report.removed_objects,
            report.added_objects
        ),
        (0, 0, 1, 1)
    );
    f.git(&["checkout", "--", "figure.png"]);
    fs::rename(&image, f.repo.join("renamed.png")).unwrap();
    f.write(&(BODY.to_owned() + "\n\\includegraphics[width=2cm]{renamed.png}"));
    assert!(!f.diff(&[]).changed);
}

#[test]
fn renamed_main_multiple_dots_subdirectories_and_file_argument_work() {
    let f = Fixture::new();
    fs::rename(f.repo.join("main.tex"), f.repo.join("paper.v1.tex")).unwrap();
    f.source(
        "paper.v1.tex",
        PREAMBLE,
        &BODY.replace("original", "renamed"),
    );
    assert_eq!(f.diff(&[]).added_sentences, 1);
    fs::create_dir(f.repo.join("nested")).unwrap();
    let (_, _, report) = f.execute(
        &["--old-main", "../main.tex", "--", "../paper.v1.tex"],
        0,
        Some(&f.repo.join("nested")),
        None,
        None,
    );
    let report: Report = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
    assert_eq!(report.removed_sentences, 1);
}

#[test]
fn exit_codes_and_invalid_revisions_are_clear() {
    let f = Fixture::new();
    assert!(!f.diff(&["--exit-code"]).changed);
    f.write(&BODY.replace("original", "updated"));
    assert!(f.diff_code(&["--exit-code"], 1).changed);
    assert!(f.error(&["missing-revision"]).contains("git rev-parse"));
    assert!(
        f.error(&["--staged", "HEAD", "HEAD"])
            .contains("at most one")
    );
    assert!(
        f.error(&["--staged", "HEAD..HEAD"])
            .contains("cannot be combined")
    );
}

#[test]
fn alternate_engines_and_magic_comments_work() {
    let f = Fixture::new();
    f.write(&BODY.replace("original", "updated"));
    for engine in ["xelatex", "lualatex"] {
        assert_eq!(f.diff(&["--engine", engine]).added_sentences, 1);
    }
    f.source(
        "main.tex",
        &("% !TEX program = xelatex\n".to_owned() + PREAMBLE),
        &BODY.replace("original", "updated"),
    );
    f.git(&["add", "."]);
    f.commit("engine hint");
    f.source(
        "main.tex",
        &("% !TEX program = xelatex\n".to_owned() + PREAMBLE),
        &BODY.replace("original", "hinted"),
    );
    assert_eq!(f.diff(&[]).added_sentences, 1);
}

#[test]
fn successful_builds_can_be_retained_for_inspection() {
    let f = Fixture::new();
    f.diff(&["--keep-build"]);
    let paths = f.retained.borrow();
    let retained = paths.last().unwrap();
    assert!(retained.join("source/review.tex").is_file());
    assert!(retained.join("review-build/review.pdf").is_file());
    assert!(retained.join("source/report.json").is_file());
    assert!(!retained.join("old-build").exists() && !retained.join("new-build").exists());
}

#[test]
fn ambiguous_roots_require_a_document_selection() {
    let f = Fixture::new();
    f.source("other.tex", PREAMBLE, "Other document.");
    assert!(f.error(&[]).contains("multiple main documents"));
    assert!(!f.diff(&["--", "main.tex"]).changed);
}

#[test]
fn bibliography_changes_are_built_and_compared() {
    let f = Fixture::new();
    f.write(r"This cites a book~\cite{book}.\bibliographystyle{plain}\bibliography{references}");
    let entry = "@book{book, author={Alice Example}, title={Original Result}, year={2020}, publisher={Test Press}}\n";
    fs::write(f.repo.join("references.bib"), entry).unwrap();
    f.git(&["add", "."]);
    f.commit("bibliography");
    fs::write(
        f.repo.join("references.bib"),
        entry.replace("Original", "Updated"),
    )
    .unwrap();
    let report = f.diff(&[]);
    assert!(
        report
            .changes
            .iter()
            .any(|c| c.text.contains("Original Result"))
    );
    assert!(
        report
            .changes
            .iter()
            .any(|c| c.text.contains("Updated Result"))
    );
}

#[test]
fn unborn_staged_branch_and_whole_document_deletion_work() {
    let f = Fixture::new();
    f.git(&["checkout", "--orphan", "unborn"]);
    let report = f.diff(&["--staged"]);
    assert_eq!((report.removed_sentences, report.added_sentences), (0, 2));
    f.git(&["checkout", "-q", "main"]);
    fs::remove_file(f.repo.join("main.tex")).unwrap();
    let report = f.diff(&[]);
    assert_eq!((report.removed_sentences, report.added_sentences), (2, 0));
}

#[test]
fn compilation_failure_preserves_existing_output_and_retains_logs() {
    let f = Fixture::new();
    f.write("\\firstmissingcommand\n\n\\secondmissingcommand");
    let output = f.temporary.path().join("existing.pdf");
    fs::write(&output, b"existing output").unwrap();
    let (result, _, _) = f.execute(&[], 2, None, None, Some(&output));
    assert_eq!(fs::read(&output).unwrap(), b"existing output");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("build files retained"));
    assert!(stderr.contains("Undefined control sequence."));
    assert!(stderr.contains(r"\firstmissingcommand"), "{stderr}");
    assert!(stderr.contains(r"\secondmissingcommand"), "{stderr}");
    let retained = f.retained.borrow();
    let build = retained.last().unwrap().join("review-build");
    let log = fs::read_to_string(build.join("main.log")).unwrap();
    assert!(log.contains(r"\firstmissingcommand") && log.contains(r"\secondmissingcommand"));
    assert!(
        build.join("main.pdf").is_file(),
        "TeX should continue to output a PDF even though compilation fails"
    );
    let commands = fs::read_to_string(build.join("build.log")).unwrap();
    assert!(commands.contains("-interaction=nonstopmode"));
    assert!(!commands.contains("-halt-on-error"));
}

#[test]
fn conflicted_index_is_rejected_without_mutation() {
    use std::io::Write;
    let f = Fixture::new();
    let oid = String::from_utf8(f.git(&["rev-parse", "HEAD:main.tex"])).unwrap();
    f.git(&["update-index", "--force-remove", "main.tex"]);
    let mut child = Command::new("git")
        .arg("-C")
        .arg(&f.repo)
        .args(["update-index", "--index-info"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    write!(
        input,
        "100644 {} 2\tmain.tex\n100644 {} 3\tmain.tex\n",
        oid.trim(),
        oid.trim()
    )
    .unwrap();
    drop(input);
    assert!(child.wait().unwrap().success());
    assert!(f.error(&[]).contains("unresolved merge conflicts"));
}

#[test]
#[cfg(unix)]
fn hung_compiler_and_its_child_process_are_timed_out() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let bin = f.temporary.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let compiler = bin.join("latexmk");
    fs::write(&compiler, "#!/bin/sh\nsleep 30\n").unwrap();
    fs::set_permissions(&compiler, fs::Permissions::from_mode(0o755)).unwrap();
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let paths = std::env::join_paths(paths).unwrap();
    let start = Instant::now();
    let (result, _, _) = f.execute(&["--timeout", "1"], 2, None, Some(&paths), None);
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(String::from_utf8_lossy(&result.stderr).contains("exceeded 1 seconds"));
}

#[test]
fn doctor_and_help_work_without_a_repository() {
    let dir = tempfile::tempdir().unwrap();
    for argument in ["--doctor", "--help", "--version"] {
        let output = Command::new(env!("CARGO_BIN_EXE_tex-diff"))
            .current_dir(dir.path())
            .arg(argument)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn output_parent_directories_are_created() {
    let f = Fixture::new();
    let output = f.temporary.path().join("nested/review/output.pdf");
    f.execute(&["--save"], 0, None, None, Some(&output));
    assert!(output.is_file());
    assert!(!f.repo.join("tex-diff.pdf").exists());
}

#[test]
fn default_pdfs_survive_exit_in_separate_temporary_directories() {
    let f = Fixture::new();
    f.write(&BODY.replace("original", "updated"));
    let existing = f.repo.join("tex-diff.pdf");
    fs::write(&existing, b"previous saved review").unwrap();
    let status = f.git(&["status", "--porcelain=v1", "-z"]);
    let index = fs::read(f.repo.join(".git/index")).unwrap();
    let mut reviews = Vec::new();
    for mode in ["unified", "side-by-side"] {
        let result = Command::new(env!("CARGO_BIN_EXE_tex-diff"))
            .current_dir(&f.repo)
            .args(["--no-open", "--mode", mode])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let pdf = PathBuf::from(String::from_utf8(result.stdout).unwrap().trim());
        assert!(pdf.starts_with(std::env::temp_dir()));
        let directory = pdf.parent().unwrap();
        assert!(
            directory
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("tex-diff-review-")
        );
        f.retained.borrow_mut().push(directory.to_owned());
        assert_eq!(pdf.file_name().unwrap(), "tex-diff.pdf");
        assert_eq!(fs::read_dir(directory).unwrap().count(), 1);
        let text = pdf_text(&pdf);
        for sentence in [
            "An original sentence.",
            "An updated sentence.",
            "This sentence remains.",
        ] {
            assert!(text.contains(sentence));
        }
        assert!(!reviews.contains(&pdf));
        reviews.push(pdf);
        assert_eq!(fs::read(&existing).unwrap(), b"previous saved review");
    }
    assert!(reviews.iter().all(|pdf| pdf.is_file()));
    assert_eq!(f.git(&["status", "--porcelain=v1", "-z"]), status);
    assert_eq!(fs::read(f.repo.join(".git/index")).unwrap(), index);
}

#[test]
fn save_writes_tex_diff_pdf_in_the_current_directory_in_both_modes() {
    let f = Fixture::new();
    f.write(&BODY.replace("original", "updated"));
    let nested = f.repo.join("nested");
    fs::create_dir(&nested).unwrap();
    let status = f.git(&["status", "--porcelain=v1", "-z"]);
    let index = fs::read(f.repo.join(".git/index")).unwrap();
    for (mode, cwd) in [("unified", &f.repo), ("side-by-side", &nested)] {
        let result = Command::new(env!("CARGO_BIN_EXE_tex-diff"))
            .current_dir(cwd)
            .args(["--save", "--no-open", "--mode", mode])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            String::from_utf8(result.stdout).unwrap().trim(),
            "tex-diff.pdf"
        );
        let text = pdf_text(&cwd.join("tex-diff.pdf"));
        for sentence in [
            "An original sentence.",
            "An updated sentence.",
            "This sentence remains.",
        ] {
            assert!(text.contains(sentence));
        }
    }
    assert_eq!(f.git(&["status", "--porcelain=v1", "-z"]), status);
    assert_eq!(fs::read(f.repo.join(".git/index")).unwrap(), index);
}

fn pdf_text(path: &Path) -> String {
    let output = Command::new("pdftotext")
        .arg(path)
        .arg("-")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn complete_unified_review_compiles_structures_and_retains_unchanged_pages() {
    let f = Fixture::new();
    let preamble = PREAMBLE.to_owned()
        + "\\usepackage{amsmath,enumitem,xcolor}\n\\title{A Complete Review}\\author{Example Author}\\date{}\n";
    let body = r"\maketitle
\section{Overview}
An original sentence. This sentence remains.
\textcolor{green}{The original colored sentence.}
\begin{enumerate}[label=\alph*.]
\item An original list item.
\item The unchanged second item.
\end{enumerate}
\begin{minipage}{0.9\linewidth}\emph{An original emphasized sentence. Another sentence remains.}\end{minipage}
\begin{align} x &= 1 \\ y &= 2 \end{align}
\begin{table}[ht]\centering\begin{tabular}{ll}Label & Value \\ original & 3\end{tabular}\caption{A complete table}\end{table}
A \verb|original % {raw}.| example.
\begin{verbatim}
original % { raw literal
\end{verbatim}
\newpage
\section{Unchanged appendix}
Every appendix paragraph is present. The very last unchanged sentence.
";
    f.source("main.tex", &preamble, body);
    f.git(&["add", "."]);
    f.commit("structured document");
    f.source(
        "main.tex",
        &preamble.replace("A Complete Review", "A Revised Review"),
        &body
            .replace("original", "updated")
            .replace("y &= 2", "y &= 4"),
    );
    let source = f.temporary.path().join("export/review.tex");
    let (_, pdf, report) = f.execute(
        &["--tex-output", source.to_str().unwrap()],
        0,
        None,
        None,
        None,
    );
    let text = pdf_text(&pdf);
    for expected in [
        "An original sentence.",
        "An updated sentence.",
        "This sentence remains.",
        "The unchanged second item.",
        "Every appendix paragraph is present.",
        "The very last unchanged sentence.",
        "A Complete Review",
        "A Revised Review",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in {text}");
    }
    assert_eq!(text.matches("The very last unchanged sentence.").count(), 1);
    let tex = fs::read_to_string(&source).unwrap();
    assert_eq!(tex.matches(r"\maketitle").count(), 1);
    assert_eq!(tex.matches(r"\begin{enumerate}").count(), 1);
    assert!(
        tex.contains(r"\textcolor{TexDiffRemovedColor}")
            && tex.contains(r"\textcolor{TexDiffAddedColor}")
    );
    let report: Report = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
    assert!(report.added_objects >= 4);
    let regenerated = tex_diff::build::compile(
        source.parent().unwrap(),
        Path::new("review.tex"),
        &f.temporary.path().join("export-build"),
        "pdflatex",
        Duration::from_secs(30),
    )
    .unwrap();
    assert!(pdf_text(&regenerated).contains("The very last unchanged sentence."));
}

#[test]
fn source_only_export_needs_no_latex_compiler_and_protects_original_source() {
    let f = Fixture::new();
    f.write(&BODY.replace("original", "updated"));
    let tex = f.temporary.path().join("source-only/review.tex");
    let bin = f.temporary.path().join("fake-compiler");
    fs::create_dir(&bin).unwrap();
    fs::write(bin.join("latexmk"), "#!/bin/sh\nexit 99\n").unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(bin.join("latexmk"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let paths = std::env::join_paths(paths).unwrap();
    let (_, pdf, report) = f.execute(
        &["--no-pdf", "--tex-output", tex.to_str().unwrap()],
        0,
        None,
        Some(&paths),
        None,
    );
    assert!(!pdf.exists());
    assert!(
        fs::read_to_string(&tex)
            .unwrap()
            .contains("An original sentence.")
    );
    assert!(
        serde_json::from_slice::<Report>(&fs::read(report).unwrap())
            .unwrap()
            .changed
    );
    let original = fs::read(f.repo.join("main.tex")).unwrap();
    assert!(
        f.error(&["--tex-output", "main.tex"])
            .contains("overwrite an existing LaTeX source")
    );
    assert_eq!(fs::read(f.repo.join("main.tex")).unwrap(), original);
}

#[test]
fn exported_figure_assets_preserve_both_versions_and_can_be_recompiled() {
    let f = Fixture::new();
    png(&f.repo.join("image.png"), [255, 128, 0]);
    f.write(&(BODY.to_owned() + r"\begin{figure}[ht]\includegraphics[width=2cm]{image}\caption{An image}\end{figure}"));
    f.git(&["add", "."]);
    f.commit("figure");
    png(&f.repo.join("image.png"), [0, 128, 255]);
    let source = f.temporary.path().join("portable/review.tex");
    f.diff(&["--tex-output", source.to_str().unwrap()]);
    let asset_dir = fs::read_dir(source.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.is_dir())
        .unwrap();
    assert_eq!(
        fs::read_dir(&asset_dir)
            .unwrap()
            .filter(|e| e
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|e| e == "png"))
            .count(),
        2
    );
    let pdf = tex_diff::build::compile(
        source.parent().unwrap(),
        Path::new("review.tex"),
        &f.temporary.path().join("portable-build"),
        "pdflatex",
        Duration::from_secs(30),
    )
    .unwrap();
    assert!(pdf_text(&pdf).contains("An image"));
}

#[test]
fn local_class_and_package_dependencies_survive_source_export() {
    let f = Fixture::new();
    fs::write(
        f.repo.join("custom.cls"),
        "\\NeedsTeXFormat{LaTeX2e}\\ProvidesClass{custom}\\LoadClass{article}\n",
    )
    .unwrap();
    fs::write(
        f.repo.join("custom.sty"),
        "\\ProvidesPackage{custom}\\newcommand{\\choice}{original}\n",
    )
    .unwrap();
    let preamble = "\\documentclass{custom}\\usepackage{custom}\n";
    f.source(
        "main.tex",
        preamble,
        r"An \choice{} sentence. This remains.",
    );
    f.git(&["add", "."]);
    f.commit("local dependencies");
    fs::write(
        f.repo.join("custom.sty"),
        "\\ProvidesPackage{custom}\\newcommand{\\choice}{updated}\n",
    )
    .unwrap();
    let source = f.temporary.path().join("portable-local/review.tex");
    let report = f.diff(&["--tex-output", source.to_str().unwrap()]);
    assert_eq!(report.added_sentences, 1);
    let pdf = tex_diff::build::compile(
        source.parent().unwrap(),
        Path::new("review.tex"),
        &f.temporary.path().join("local-build"),
        "pdflatex",
        Duration::from_secs(30),
    )
    .unwrap();
    assert!(pdf_text(&pdf).contains("An original sentence. An updated sentence."));
}

#[test]
fn split_math_package_environments_compile_in_both_review_modes() {
    let f = Fixture::new();
    fs::write(
        f.repo.join("splitmath.sty"),
        r"\ProvidesPackage{splitmath}
\def\unusedruntime{\input{file-that-does-not-exist}}
\newenvironment{mathpar}
  {$$\vbox\bgroup\ifmmode $\else\noindent $\displaystyle\fi}
  {\unskip\ifmmode $\fi\egroup $$\ignorespacesafterend}
",
    )
    .unwrap();
    let preamble = PREAMBLE.to_owned() + "\\usepackage{splitmath}\n";
    f.source(
        "main.tex",
        &preamble,
        r"Unchanged opening.\begin{mathpar}x=1\end{mathpar}\clearpage Entire appendix stays.",
    );
    f.git(&["add", "."]);
    f.commit("split math environments");
    f.source(
        "main.tex",
        &preamble,
        r"Unchanged opening.\begin{mathpar}x=2\end{mathpar}\clearpage Entire appendix stays.",
    );
    for mode in ["unified", "side-by-side"] {
        let (_, pdf, report) = f.execute(&["--mode", mode], 0, None, None, None);
        let report: Report = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
        assert_eq!(report.removed_objects, 1);
        assert_eq!(report.added_objects, 1);
        let text = pdf_text(&pdf);
        assert!(text.contains("Unchanged opening.") && text.contains("Entire appendix stays."));
        assert_eq!(lopdf::Document::load(pdf).unwrap().get_pages().len(), 2);
    }
}

#[test]
fn project_classes_and_packages_override_installed_namesakes_in_portable_reviews() {
    let f = Fixture::new();
    fs::write(
        f.repo.join("article.cls"),
        "\\ProvidesClass{article}\\LoadClass{report}\
         \\newcommand{\\localclassmark}{Local class marker.}\n",
    )
    .unwrap();
    fs::write(
        f.repo.join("xspace.sty"),
        "\\ProvidesPackage{xspace}\
         \\newenvironment{localenv}{\\par Local package marker. }{\\par}\n",
    )
    .unwrap();
    let preamble = "\\documentclass{article}\\usepackage{xspace}\n";
    f.source(
        "main.tex",
        preamble,
        concat!(
            r"\localclassmark\begin{localenv}An old sentence.\end{localenv}",
            r"\clearpage Entire appendix stays.",
        ),
    );
    f.git(&["add", "."]);
    f.commit("local standard-named dependencies");
    f.source(
        "main.tex",
        preamble,
        concat!(
            r"\localclassmark\begin{localenv}An new sentence.\end{localenv}",
            r"\clearpage Entire appendix stays.",
        ),
    );
    let source = f.temporary.path().join("portable-overrides/review.tex");
    for mode in ["unified", "side-by-side"] {
        let (_, pdf, _) = f.execute(
            &["--mode", mode, "--tex-output", source.to_str().unwrap()],
            0,
            None,
            None,
            None,
        );
        let text = pdf_text(&pdf);
        assert!(text.contains("Local class marker.") && text.contains("Local package marker."));
        assert!(text.contains("Entire appendix stays."));
    }
    let pdf = tex_diff::build::compile(
        source.parent().unwrap(),
        Path::new("review.tex"),
        &f.temporary.path().join("portable-override-build"),
        "pdflatex",
        Duration::from_secs(30),
    )
    .unwrap();
    let text = pdf_text(&pdf);
    assert!(text.contains("Local class marker.") && text.contains("Local package marker."));
}

#[test]
fn native_sources_preserve_nested_master_working_directories_and_runtime_resources() {
    let f = Fixture::new();
    fs::create_dir(f.repo.join("paper")).unwrap();
    fs::write(f.repo.join("runtime.txt"), "Parent resource marker.\n").unwrap();
    fs::write(f.repo.join("local.sty"), r"\ProvidesPackage{local}
\newread\localinput
\newenvironment{localenv}{\openin\localinput=../runtime.txt \read\localinput to\localtext\closein\localinput\localtext\par}{}
").unwrap();
    let preamble = "\\documentclass{article}\\usepackage{../local}\n";
    f.source(
        "paper/main.tex",
        preamble,
        r"\begin{localenv}An old sentence.\end{localenv}\clearpage Entire appendix stays.",
    );
    f.git(&["add", "."]);
    f.commit("nested master with runtime resource");
    f.source(
        "paper/main.tex",
        preamble,
        r"\begin{localenv}An new sentence.\end{localenv}\clearpage Entire appendix stays.",
    );
    for mode in ["unified", "side-by-side"] {
        let (_, pdf, _) = f.execute(
            &["--main", "paper/main.tex", "--mode", mode],
            0,
            None,
            None,
            None,
        );
        let text = pdf_text(&pdf);
        assert!(
            text.contains("Parent resource marker.") && text.contains("Entire appendix stays.")
        );
        assert_eq!(lopdf::Document::load(pdf).unwrap().get_pages().len(), 2);
    }
}

#[test]
fn uncited_bibliography_edits_are_ignored_and_changed_entries_are_not_duplicated() {
    let f = Fixture::new();
    f.write(r"A citation~\cite{book}.\bibliographystyle{plain}\bibliography{references}");
    let entry = "@book{book, author={Alice Example}, title={Original Result}, \
                 year={2020}, publisher={Test Press}}\n\
                 @book{unused, author={Bob Example}, title={Unused Work}, \
                 year={2021}, publisher={Test Press}}\n";
    fs::write(f.repo.join("references.bib"), entry).unwrap();
    f.git(&["add", "."]);
    f.commit("bibliography");
    fs::write(
        f.repo.join("references.bib"),
        entry.replace("Unused Work", "Ignored Update"),
    )
    .unwrap();
    assert!(!f.diff(&[]).changed);
    fs::write(
        f.repo.join("references.bib"),
        entry.replace("Original Result", "Updated Result"),
    )
    .unwrap();
    let source = f.temporary.path().join("bib-review.tex");
    let report = f.diff(&["--tex-output", source.to_str().unwrap()]);
    assert_eq!((report.removed_objects, report.added_objects), (1, 1));
    assert_eq!(
        fs::read_to_string(source)
            .unwrap()
            .matches(r"\bibitem{book}")
            .count(),
        1
    );
}

#[test]
fn invalid_report_destination_does_not_replace_an_existing_pdf() {
    let f = Fixture::new();
    let pdf = f.temporary.path().join("existing.pdf");
    fs::write(&pdf, b"previous review").unwrap();
    let blocker = f.temporary.path().join("blocker");
    fs::write(&blocker, b"not a directory").unwrap();
    f.execute(
        &["--tex-output", blocker.join("review.tex").to_str().unwrap()],
        2,
        None,
        None,
        Some(&pdf),
    );
    assert_eq!(fs::read(pdf).unwrap(), b"previous review");
}

#[test]
#[cfg(unix)]
fn default_run_opens_the_complete_pdf_in_the_selected_viewer() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.write(&BODY.replace("original", "updated"));
    let viewer = f.temporary.path().join("test viewer");
    let record = f.temporary.path().join("viewer.log");
    fs::write(
        &viewer,
        "#!/bin/sh\nprintf '%s\\n' \"$1\" > \"$TEX_DIFF_TEST_VIEWER_LOG\"\n",
    )
    .unwrap();
    fs::set_permissions(&viewer, fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tex-diff"))
        .current_dir(&f.repo)
        .arg("--viewer")
        .arg(&viewer)
        .env("TEX_DIFF_TEST_VIEWER_LOG", &record)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let pdf = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    assert!(pdf.starts_with(std::env::temp_dir()));
    assert!(!f.repo.join("tex-diff.pdf").exists());
    assert!(
        pdf.parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("tex-diff-review-")
    );
    f.retained
        .borrow_mut()
        .push(pdf.parent().unwrap().to_owned());
    assert_eq!(
        fs::read_to_string(&record).unwrap().trim(),
        fs::canonicalize(&pdf).unwrap().to_str().unwrap()
    );
    assert!(
        pdf_text(&pdf)
            .contains("An original sentence. An updated sentence. This sentence remains.")
    );
    fs::remove_file(&record).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tex-diff"))
        .current_dir(&f.repo)
        .arg("--no-open")
        .arg("--output")
        .arg(&pdf)
        .env("TEX_DIFF_VIEWER", &viewer)
        .env("TEX_DIFF_TEST_VIEWER_LOG", &record)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!record.exists());
}

fn page_text(path: &Path, page: usize) -> String {
    let output = Command::new("pdftotext")
        .args(["-f", &page.to_string(), "-l", &page.to_string()])
        .arg(path)
        .arg("-")
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn page_count(path: &Path) -> usize {
    lopdf::Document::load(path).unwrap().get_pages().len()
}

fn assert_unchanged_pagination(workspace: &Path) {
    assert_unchanged_pagination_with_engine(workspace, "pdflatex");
}

fn assert_unchanged_pagination_with_engine(workspace: &Path, engine: &str) {
    for side in ["old", "new"] {
        let original = tex_diff::build::compile(
            &workspace.join(side),
            Path::new("main.tex"),
            &workspace.join(format!("{side}-original")),
            engine,
            Duration::from_secs(30),
        )
        .unwrap();
        let annotated = workspace.join(format!("{side}-build/{side}.pdf"));
        assert_eq!(page_count(&original), page_count(&annotated));
        for page in 1..=page_count(&original) {
            assert_eq!(
                page_text(&original, page),
                page_text(&annotated, page),
                "pagination changed for {side} page {page}"
            );
        }
    }
}

#[test]
fn side_by_side_keeps_each_versions_page_count_and_content_on_each_page() {
    let f = Fixture::new();
    let old = concat!(
        r"\section{First page}\label{first}An original sentence. This sentence remains.",
        r"\newpage Second page unchanged. This refers to Section~\ref{first}.",
    );
    f.write(old);
    f.git(&["add", "."]);
    f.commit("two pages");
    f.write(
        &(old.replace("original", "updated")
            + r"\newpage Third added page. All its contents are present."),
    );
    let (_, review, _) = f.execute(
        &["--mode", "side-by-side", "--keep-build"],
        0,
        None,
        None,
        None,
    );
    let retained = f.retained.borrow();
    let workspace = retained.last().unwrap();
    assert_unchanged_pagination(workspace);
    assert_eq!(
        (
            page_count(&workspace.join("old-build/old.pdf")),
            page_count(&workspace.join("new-build/new.pdf")),
            page_count(&review)
        ),
        (2, 3, 3)
    );
    let first = page_text(&review, 1);
    assert!(
        first.contains("An original sentence.")
            && first.contains("An updated sentence.")
            && !first.contains("Second page unchanged.")
    );
    let second = page_text(&review, 2);
    assert_eq!(second.matches("Second page unchanged.").count(), 2);
    assert!(!second.contains("Third added page."));
    let third = page_text(&review, 3);
    assert!(
        third.contains("index - no page")
            && third.contains("Third added page.")
            && !third.contains("Second page unchanged.")
    );
}

#[test]
fn side_by_side_preserves_include_page_breaks_headings_figures_and_titles() {
    let f = Fixture::new();
    png(&f.repo.join("image.png"), [20, 200, 100]);
    let preamble = PREAMBLE.to_owned() + "\\title{Original title}\\author{Example}\\date{}\n";
    let body = concat!(
        r"\maketitle\section{Original heading}A sentence.",
        r"\input{intro}\include{chapter}\includegraphics[width=2cm]{image}",
    );
    fs::write(
        f.repo.join("intro.tex"),
        r"\emph{First emphasized sentence. Second emphasized sentence.}",
    )
    .unwrap();
    fs::write(f.repo.join("chapter.tex"), "The original chapter.\n").unwrap();
    f.source("main.tex", &preamble, body);
    f.git(&["add", "."]);
    f.commit("include pages");
    f.source(
        "main.tex",
        &preamble.replace("Original", "Updated"),
        &body.replace("Original", "Updated"),
    );
    fs::write(f.repo.join("chapter.tex"), "The updated chapter.\n").unwrap();
    png(&f.repo.join("image.png"), [200, 100, 20]);
    f.diff(&["--mode", "side-by-side", "--keep-build"]);
    let retained = f.retained.borrow();
    let workspace = retained.last().unwrap();
    assert_unchanged_pagination(workspace);
}

#[test]
fn side_by_side_supports_whole_document_addition_and_deletion() {
    let f = Fixture::new();
    f.git(&["checkout", "--orphan", "unborn"]);
    let (_, pdf, _) = f.execute(&["--staged", "--mode", "side-by-side"], 0, None, None, None);
    assert!(pdf_text(&pdf).contains("empty tree - no page"));
    f.git(&["checkout", "-q", "main"]);
    fs::remove_file(f.repo.join("main.tex")).unwrap();
    let (_, pdf, _) = f.execute(&["--mode", "side-by-side"], 0, None, None, None);
    let text = pdf_text(&pdf);
    assert!(text.contains("working tree - no page") && text.contains("An original sentence."));
}

#[test]
fn side_by_side_preserves_automatic_pagination_and_styled_sentence_spacing() {
    let f = Fixture::new();
    let body = (1..=45)
        .map(|number| {
            format!(
                "Paragraph {number} gives an original explanation of the method. \
                 \\emph{{The emphasis continues over more than one sentence. \
                 Its word spacing must stay intact.}} Several additional words \
                 explain the stable details and carry the paragraph across its \
                 ordinary line breaks. This final sentence belongs to the same paragraph.\n\n"
            )
        })
        .collect::<String>();
    f.write(&body);
    f.git(&["add", "."]);
    f.commit("automatic pages");
    f.write(&body.replacen("original", "modified", 1));
    f.diff(&["--mode", "side-by-side", "--keep-build"]);
    let retained = f.retained.borrow();
    let workspace = retained.last().unwrap();
    assert_unchanged_pagination(workspace);
}

#[test]
fn side_by_side_preserves_paragraphs_after_comment_blocks() {
    let f = Fixture::new();
    let paragraph = "This paragraph has several sentences. The surrounding prose fills the page. \
                     Comments must preserve paragraph boundaries. "
        .repeat(5);
    let body = (0..25)
        .map(|i| {
            format!("Paragraph {i}. {paragraph}\n% Ignored comment block.\n% Another comment.\n\n")
        })
        .collect::<String>();
    f.write(&body);
    f.git(&["add", "."]);
    f.commit("paragraphs separated by comments");
    f.write(&body.replacen("This paragraph", "That paragraph", 1));
    f.diff(&["--mode", "side-by-side", "--keep-build"]);
    let retained = f.retained.borrow();
    let workspace = retained.last().unwrap();
    assert_unchanged_pagination(workspace);
}

#[test]
fn side_by_side_preserves_numbered_theorem_displays_and_conditional_fallbacks() {
    let f = Fixture::new();
    let preamble = PREAMBLE.to_owned()
        + "\\usepackage{amsmath,amsthm,lineno}\\linenumbers\\newtheorem{thm}{Theorem}\\providecommand{\\TeX}{WRONG}\n";
    let body = (0..30)
        .map(|i| {
            let display = match i % 3 {
                0 => r"\[\begin{gathered}x+1=2\\y=3\end{gathered}\]",
                1 => r"\begin{align}x+1&=2\\y&=3\end{align}",
                _ => r"\begin{equation}x+1=2\end{equation}",
            };
            format!(
                "\\begin{{thm}}The \\TeX{{}} logo is preserved. \
                 Statement {i} has a displayed result.{display}\\end{{thm}}\n\
                 \\begin{{thm}}The following theorem starts immediately.\\end{{thm}}\n\n"
            )
        })
        .collect::<String>();
    f.source("main.tex", &preamble, &body);
    f.git(&["add", "."]);
    f.commit("numbered theorem displays");
    f.source("main.tex", &preamble, &body.replace("x+1", "x-1"));
    f.diff(&["--mode", "side-by-side", "--keep-build"]);
    let retained = f.retained.borrow();
    let workspace = retained.last().unwrap();
    assert_unchanged_pagination(workspace);
    for side in ["old", "new"] {
        assert!(!pdf_text(&workspace.join(format!("{side}-build/{side}.pdf"))).contains("WRONG"));
    }
}

#[test]
fn bibliography_citation_order_is_preserved_for_unsorted_styles() {
    let f = Fixture::new();
    f.write(r"First~\cite{z}. Then~\cite{a}.\bibliographystyle{unsrt}\bibliography{references}");
    fs::write(
        f.repo.join("references.bib"),
        "@book{a, author={Alice}, title={Alpha}, year={2020}, publisher={Press}}\n\
         @book{z, author={Zoe}, title={Zebra}, year={2021}, publisher={Press}}\n",
    )
    .unwrap();
    f.git(&["add", "."]);
    f.commit("unsorted bibliography");
    let (_, pdf, _) = f.execute(
        &["--mode", "side-by-side", "--keep-build"],
        0,
        None,
        None,
        None,
    );
    let text = pdf_text(&pdf);
    assert!(text.contains("First [1]. Then [2]."));
    let retained = f.retained.borrow();
    let workspace = retained.last().unwrap();
    let original = tex_diff::build::compile(
        &workspace.join("new"),
        Path::new("main.tex"),
        &workspace.join("new-original"),
        "pdflatex",
        Duration::from_secs(30),
    )
    .unwrap();
    assert_eq!(
        page_text(&original, 1),
        page_text(&workspace.join("new-build/new.pdf"), 1)
    );
}

#[test]
fn separate_preamble_and_renewed_macros_work_in_both_modes() {
    let f = Fixture::new();
    fs::write(
        f.repo.join("preamble.tex"),
        PREAMBLE.to_owned() + "\\newcommand{\\choice}{initial}\\renewcommand{\\choice}{original}\n",
    )
    .unwrap();
    fs::write(
        f.repo.join("main.tex"),
        "\\input{preamble}\\begin{document}An \\choice{} sentence. This remains.\\end{document}",
    )
    .unwrap();
    f.git(&["add", "."]);
    f.commit("separate preamble");
    fs::write(
        f.repo.join("preamble.tex"),
        PREAMBLE.to_owned() + "\\newcommand{\\choice}{initial}\\renewcommand{\\choice}{updated}\n",
    )
    .unwrap();
    for mode in ["unified", "side-by-side"] {
        assert_eq!(f.diff(&["--mode", mode]).added_sentences, 1);
    }
}

#[test]
fn internal_label_macros_compile_in_both_review_modes() {
    let f = Fixture::new();
    let preamble = concat!(
        "\\documentclass{article}\\usepackage{amsmath}\n",
        "\\newcounter{workrule}\\AtBeginDocument{\\let\\workrulelabel\\label}\n",
    );
    let body = concat!(
        r"Unchanged opening.\[\begin{array}{rc}",
        r"\workrule[\downarrow]{Val}&x=1\\\workrule{Var}&x=2\end{array}\]",
        r"Rules~\ref{rule:Val} and~\ref{rule:Var}.\clearpage Entire appendix stays.",
    );
    let old = concat!(
        r"\newcommand{\workrule}[2][\rightarrow]{",
        r"\refstepcounter{workrule}\workrulelabel{rule:#2}#1_{\theworkrule}}",
    );
    f.source("main.tex", &(preamble.to_owned() + old), body);
    f.git(&["add", "."]);
    f.commit("numbered rules");
    let new = concat!(
        r"\makeatletter\newcommand{\workrule}[2][\rightarrow]{",
        r"\protected@edef\@currentlabel{\textsf{#2}}\workrulelabel{rule:#2}#1}",
        r"\makeatother",
    );
    f.source("main.tex", &(preamble.to_owned() + new), body);
    for mode in ["unified", "side-by-side"] {
        let (_, pdf, report) = f.execute(&["--mode", mode, "--keep-build"], 0, None, None, None);
        let text = pdf_text(&pdf);
        assert!(text.contains("Unchanged opening.") && text.contains("Entire appendix stays."));
        assert!(text.contains("Val") && !text.contains("??"));
        let report: Report = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
        assert_eq!((report.removed_objects, report.added_objects), (1, 1));
        if mode == "side-by-side" {
            assert_unchanged_pagination(f.retained.borrow().last().unwrap());
        } else {
            let retained = f.retained.borrow();
            let log =
                fs::read_to_string(retained.last().unwrap().join("review-build/main.log")).unwrap();
            assert!(
                !log.contains("multiply defined"),
                "rule labels were duplicated"
            );
        }
    }
}

#[test]
fn removed_figures_keep_their_references_in_the_unified_review() {
    let f = Fixture::new();
    let preamble = PREAMBLE.to_owned() + r"\usepackage{hyperref}";
    let old = concat!(
        r"The entire introduction stays.\begin{figure}[ht]",
        r"\rule{30pt}{20pt}\caption{An original drawing}\label{old-drawing}\end{figure}",
        r"The original drawing is \autoref{old-drawing}; see page~\pageref{old-drawing}.",
        r"\clearpage The entire appendix stays.",
    );
    f.source("main.tex", &preamble, old);
    f.git(&["add", "."]);
    f.commit("referenced figure");
    f.source(
        "main.tex",
        &preamble,
        &old.replace("original", "updated")
            .replace("old-drawing", "new-drawing"),
    );
    let (_, pdf, _) = f.execute(&["--keep-build"], 0, None, None, None);
    let text = pdf_text(&pdf);
    assert!(text.contains("original drawing is Figure 1"), "{text}");
    assert!(text.contains("updated drawing is Figure 2"), "{text}");
    assert!(!text.contains("??"));
    let retained = f.retained.borrow();
    let log = fs::read_to_string(retained.last().unwrap().join("review-build/main.log")).unwrap();
    assert!(!log.contains("undefined references"));
}

#[test]
fn changed_commands_preserve_saved_boxes_in_both_review_modes() {
    let f = Fixture::new();
    let preamble = concat!(
        "\\documentclass{article}\\usepackage{amsmath,lineno}\\linenumbers\n",
        "\\newsavebox{\\savedgrammar}\n",
        "\\newcommand{\\storegrammar}[1]{%\n  \\sbox{\\savedgrammar}{#1}%\n",
        "  \\ifdim\\wd\\savedgrammar>0pt\\relax\\fi%\n}\n",
    );
    let body = concat!(
        "\\storegrammar{First unchanged grammar}\n\n",
        "\\[\\usebox{\\savedgrammar}\\]\\clearpage\n",
        "\\storegrammar{Second original grammar}\n\n",
        "\\[\\usebox{\\savedgrammar}\\]The entire appendix stays.",
    );
    f.source("main.tex", preamble, body);
    f.git(&["add", "."]);
    f.commit("stored grammars");
    f.source("main.tex", preamble, &body.replace("original", "updated"));
    for mode in ["unified", "side-by-side"] {
        let (_, pdf, _) = f.execute(&["--mode", mode, "--keep-build"], 0, None, None, None);
        let text = pdf_text(&pdf);
        assert!(text.contains("Second updated grammar"));
        if mode == "side-by-side" {
            assert!(text.contains("Second original grammar"));
            assert_unchanged_pagination(f.retained.borrow().last().unwrap());
            let (red, blue) =
                colored_pixels_on_page(&pdf, &f.temporary.path().join("box-colors"), 2);
            assert!(
                red > 50 && blue > 50,
                "saved box colors missing: red={red}, blue={blue}"
            );
        }
    }
}

#[test]
fn whole_lists_and_theorems_keep_their_pagination_when_added_or_removed() {
    let f = Fixture::new();
    let preamble = concat!(
        "\\documentclass{article}\\usepackage{amsmath,amsthm,lineno}\n",
        "\\linenumbers\\newtheorem{thm}{Theorem}\n",
    );
    let old = concat!(
        r"Opening sentence.\begin{itemize}\item First original item.",
        r"\item Second original item.\end{itemize}",
        r"\clearpage Every appendix sentence stays.",
    );
    f.source("main.tex", preamble, old);
    f.git(&["add", "."]);
    f.commit("whole list");
    let new = concat!(
        r"Opening sentence.\begin{thm}[An added theorem]",
        r"For every value $x$, the result is $x$.\end{thm}",
        r"\begin{proof}The result follows directly.\end{proof}",
        r"\clearpage Every appendix sentence stays.",
    );
    f.source("main.tex", preamble, new);
    f.diff(&["--mode", "side-by-side", "--keep-build"]);
    assert_unchanged_pagination(f.retained.borrow().last().unwrap());
}

fn colored_pixels(pdf: &Path, prefix: &Path) -> (usize, usize) {
    colored_pixels_on_page(pdf, prefix, 1)
}

fn colored_pixels_on_page(pdf: &Path, prefix: &Path, page: usize) -> (usize, usize) {
    let pixels = page_pixels(pdf, prefix, page);
    let red = pixels
        .chunks_exact(3)
        .filter(|p| p[0] > 130 && p[1] < 100 && p[2] < 100)
        .count();
    let blue = pixels
        .chunks_exact(3)
        .filter(|p| p[2] > 130 && p[0] < 100 && p[1] < 120)
        .count();
    (red, blue)
}

fn page_pixels(pdf: &Path, prefix: &Path, page: usize) -> Vec<u8> {
    page_pixels_with_resolution(pdf, prefix, page, 72)
}

fn page_pixels_with_resolution(pdf: &Path, prefix: &Path, page: usize, dpi: u32) -> Vec<u8> {
    let output = Command::new("pdftoppm")
        .args([
            "-f",
            &page.to_string(),
            "-singlefile",
            "-r",
            &dpi.to_string(),
            "-png",
        ])
        .arg(pdf)
        .arg(prefix)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let decoder = png::Decoder::new(std::io::BufReader::new(
        fs::File::open(prefix.with_extension("png")).unwrap(),
    ));
    let mut reader = decoder.read_info().unwrap();
    let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buffer).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgb);
    buffer.truncate(info.buffer_size());
    buffer
}

fn pixels_near(pixels: &[u8], color: [u8; 3]) -> usize {
    pixels
        .chunks_exact(3)
        .filter(|pixel| pixel.iter().zip(color).all(|(a, b)| a.abs_diff(b) <= 2))
        .count()
}

#[test]
fn grayscale_documents_keep_review_colors_saturated() {
    let f = Fixture::new();
    let preamble = PREAMBLE.to_owned() + r"\usepackage[gray]{xcolor}";
    let body = "The original conclusion includes every example and explains the complete document. This sentence remains.";
    f.source("main.tex", &preamble, body);
    f.git(&["add", "."]);
    f.commit("grayscale palette");
    f.source("main.tex", &preamble, &body.replace("original", "updated"));
    for mode in ["unified", "side-by-side"] {
        let (_, pdf, _) = f.execute(&["--mode", mode], 0, None, None, None);
        let (red, blue) = colored_pixels(&pdf, &f.temporary.path().join("gray-palette"));
        assert!(
            red > 50 && blue > 50,
            "review colors were muted: {red}, {blue}"
        );
    }
}

#[test]
fn original_colors_and_imported_graphics_are_muted_in_both_modes_and_all_engines() {
    let f = Fixture::new();
    png(&f.repo.join("palette.png"), [255, 0, 0]);
    png(&f.repo.join("changed.png"), [0, 255, 0]);
    let drawing = f.temporary.path().join("drawing");
    fs::create_dir(&drawing).unwrap();
    fs::write(
        drawing.join("drawing.tex"),
        concat!(
            r"\documentclass{article}\usepackage{xcolor,geometry}",
            r"\geometry{paperwidth=100pt,paperheight=100pt,margin=0pt}",
            r"\pagestyle{empty}\begin{document}\noindent",
            r"\textcolor{blue}{\rule{90pt}{80pt}}\end{document}",
        ),
    )
    .unwrap();
    let drawing_pdf = tex_diff::build::compile(
        &drawing,
        Path::new("drawing.tex"),
        &drawing.join("build"),
        "pdflatex",
        Duration::from_secs(30),
    )
    .unwrap();
    fs::copy(drawing_pdf, f.repo.join("palette.pdf")).unwrap();
    let preamble = PREAMBLE.to_owned()
        + concat!(
            r"\usepackage[dvipsnames]{xcolor}\usepackage{tikz}",
            r"\usepackage[colorlinks,linkcolor=red,urlcolor=blue]{hyperref}",
            r"\definecolor{originalreviewred}{RGB}{204,20,20}",
            r"\definecolor{originalreviewblue}{rgb}{0.05882,0.2,0.85098}",
            r"\definecolor{mutedgrayblue}{RGB}{104,112,120}",
            r"\colorlet{customcolor}{yellow!50!blue}",
            r"\colorlet{redalias}{red!100!black}\colorlet{nestedalias}{redalias}",
            r"\newsavebox{\logo}\sbox{\logo}{\includegraphics[width=30pt]{palette.png}}",
            r"\makeatletter\newcommand{\protectedpalette}{\iftrue",
            r"\textcolor{green}{A protected green label}\fi}\makeatother",
        );
    let body = concat!(
        r"\section{The complete unchanged page}",
        r"\textcolor{red}{An original red label.} ",
        r"\textcolor{blue}{An original blue label.} ",
        r"\textcolor{yellow}{An original yellow label.} ",
        r"\textcolor[HTML]{FF00FF}{An explicit color label.} ",
        r"\textcolor{originalreviewred}{A matching red label.} ",
        r"\textcolor{originalreviewblue}{A matching blue label.} ",
        r"\protectedpalette\ \textcolor{customcolor}{A mixed color label.} ",
        r"\par\noindent",
        r"\textcolor{red}{\rule{30pt}{10pt}}\quad",
        r"\textcolor{nestedalias}{\rule{30pt}{10pt}}\quad",
        r"{\color{red}\colorlet{currentalias}{.}\colorlet{anotheralias}{currentalias}",
        r"\color{anotheralias}\rule{30pt}{10pt}}\quad",
        r"\textcolor{blue}{\rule{30pt}{10pt}}\quad",
        r"{\color{red}\colorlet{redefinedalias}{.}",
        r"\definecolor{redefinedalias}{rgb}{0,0,1}",
        r"\color{redefinedalias}\rule{30pt}{10pt}}\quad",
        r"\textcolor{yellow}{\rule{30pt}{10pt}}\par",
        r"\textcolor[gray]{0.5}{\rule{30pt}{10pt}}\quad",
        r"\textcolor{black}{\rule{30pt}{10pt}}\quad",
        r"\textcolor{mutedgrayblue}{\rule{30pt}{10pt}}\par",
        r"\usebox{\logo}\quad\includegraphics[width=60pt]{palette.png}\quad",
        r"\includegraphics[width=60pt,angle=15]{palette.pdf}\par",
        r"\tikz\fill[red] (0,0) rectangle (1,1);",
        r"\quad\tikz\shade[top color=blue,bottom color=blue] (0,0) rectangle (1,1);",
        r"\clearpage An original sentence is shown here.\par",
        r"\begin{figure}[ht]\includegraphics[width=100pt]{changed.png}",
        r"\caption{A changed graphic}\end{figure}",
    );
    f.source("main.tex", &preamble, body);
    f.git(&["add", "."]);
    f.git(&["add", "-f", "palette.pdf"]);
    f.commit("colored document");
    f.source(
        "main.tex",
        &preamble,
        &body.replace("original sentence", "updated sentence"),
    );
    png(&f.repo.join("changed.png"), [255, 0, 255]);
    for engine in ["pdflatex", "xelatex", "lualatex"] {
        for mode in ["unified", "side-by-side"] {
            let (_, pdf, _) = f.execute(
                &["--mode", mode, "--engine", engine, "--keep-build"],
                0,
                None,
                None,
                None,
            );
            let prefix = f.temporary.path().join(format!("muted-{engine}-{mode}"));
            let pages = if mode == "side-by-side" {
                let retained = f.retained.borrow();
                let workspace = retained.last().unwrap();
                vec![
                    workspace.join("old-build/old.pdf"),
                    workspace.join("new-build/new.pdf"),
                ]
            } else {
                vec![pdf.clone()]
            };
            for page in pages {
                let pixels = page_pixels(&page, &prefix, 1);
                let chromatic = pixels
                    .chunks_exact(3)
                    .filter(|pixel| pixel.iter().max().unwrap() - pixel.iter().min().unwrap() > 65)
                    .count();
                assert_eq!(
                    chromatic, 0,
                    "original colors remain saturated in {engine} {mode}"
                );
                for (color, minimum) in [
                    ([159, 96, 96], 1400),
                    ([96, 96, 159], 1000),
                    ([159, 159, 96], 200),
                    ([128, 128, 128], 200),
                    ([0, 0, 0], 200),
                    ([104, 112, 120], 200),
                ] {
                    let count = pixels_near(&pixels, color);
                    assert!(
                        count > minimum,
                        "hue or lightness lost in {engine} {mode}: {color:?}, {count} pixels"
                    );
                }
                assert!(
                    pixels
                        .chunks_exact(3)
                        .filter(|pixel| pixel[0] < 230)
                        .count()
                        > 1000
                );
                assert!(page_text(&page, 1).contains("The complete unchanged page"));
            }
            let (red, blue) = colored_pixels_on_page(&pdf, &prefix, 2);
            assert!(
                red > 100 && blue > 100,
                "diff colors missing in {engine} {mode}"
            );
            let pixels = page_pixels(&pdf, &prefix, 2);
            let original_image_colors = pixels
                .chunks_exact(3)
                .filter(|pixel| {
                    pixel[1] > 130 && pixel[0] < 100 && pixel[2] < 100
                        || pixel[0] > 130 && pixel[2] > 130 && pixel[1] < 100
                })
                .count();
            assert_eq!(original_image_colors, 0, "changed image colors remain");
            if mode == "side-by-side" {
                assert_unchanged_pagination_with_engine(
                    f.retained.borrow().last().unwrap(),
                    engine,
                );
            }
        }
    }
}

#[test]
fn links_citations_and_references_keep_their_original_colors_in_both_modes_and_all_engines() {
    let f = Fixture::new();
    let preamble = PREAMBLE.to_owned()
        + concat!(
            r"\usepackage{xcolor}\colorlet{originalurlcolor}{red!50!blue}",
            r"\colorlet{originallinkcolor}{green!60!black}",
            r"\usepackage[colorlinks]{hyperref}",
            r"\hypersetup{urlcolor=originalurlcolor,linkcolor=originallinkcolor,",
            r"citecolor={[RGB]{23,75,201}}}",
        );
    let body = concat!(
        r"\section{Target}\label{target}\hypertarget{anchor}{}",
        r"\href{https://example.invalid}{\rule{30pt}{10pt}}\quad",
        r"\hyperlink{anchor}{\rule{30pt}{10pt}}\quad",
        r"\textbf{\cite{reference}}\quad\textbf{\autoref{target}}",
        r"\clearpage The original conclusion links to ",
        r"\href{https://example.invalid}{\textbf{the article}}, cites \textbf{\cite{reference}}, ",
        r"and refers to \textbf{\autoref{target}}.\par",
        r"\begin{thebibliography}{1}\bibitem[Reference2026]{reference}",
        r"The unchanged reference.\end{thebibliography}",
    );
    f.source("main.tex", &preamble, body);
    f.git(&["add", "."]);
    f.commit("colored links and citations");
    f.source(
        "main.tex",
        &preamble,
        &body.replace("original conclusion", "updated conclusion"),
    );
    for engine in ["pdflatex", "xelatex", "lualatex"] {
        for mode in ["unified", "side-by-side"] {
            let (_, pdf, _) = f.execute(
                &["--mode", mode, "--engine", engine, "--keep-build"],
                0,
                None,
                None,
                None,
            );
            let prefix = f.temporary.path().join(format!("links-{engine}-{mode}"));
            let pages = if mode == "side-by-side" {
                let retained = f.retained.borrow();
                let workspace = retained.last().unwrap();
                vec![
                    workspace.join("old-build/old.pdf"),
                    workspace.join("new-build/new.pdf"),
                ]
            } else {
                vec![pdf.clone()]
            };
            for page in pages {
                for number in [1, 2] {
                    let pixels = page_pixels_with_resolution(&page, &prefix, number, 144);
                    for color in [[128, 0, 128], [0, 153, 0], [23, 75, 201]] {
                        let count = pixels_near(&pixels, color);
                        assert!(
                            count > 20,
                            "link or citation color changed in {engine} {mode}, page {number}: {color:?}, {count} pixels"
                        );
                    }
                }
            }
            let (red, blue) = colored_pixels_on_page(&pdf, &prefix, 2);
            assert!(
                red > 50 && blue > 50,
                "diff colors missing in {engine} {mode}"
            );
            if mode == "side-by-side" {
                assert_unchanged_pagination_with_engine(
                    f.retained.borrow().last().unwrap(),
                    engine,
                );
            }
        }
    }
}

#[test]
fn changed_float_drawings_are_actually_red_and_blue_in_both_modes() {
    let f = Fixture::new();
    let body = concat!(
        r"This sentence remains.\begin{figure}[ht]\centering",
        r"\begin{picture}(150,40)\put(0,0){\framebox(140,35){Original drawing}}",
        r"\end{picture}\caption{A drawing}\end{figure}",
    );
    f.write(body);
    f.git(&["add", "."]);
    f.commit("drawing");
    f.write(&body.replace("Original", "Updated"));
    for mode in ["unified", "side-by-side"] {
        let (_, pdf, report) = f.execute(&["--mode", mode, "--keep-build"], 0, None, None, None);
        let report: Report = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
        assert_eq!((report.removed_sentences, report.added_sentences), (0, 0));
        if mode == "unified" {
            let (red, blue) = colored_pixels(&pdf, &f.temporary.path().join("unified-color"));
            assert!(
                red > 100 && blue > 100,
                "unified figure colors missing: red={red}, blue={blue}"
            );
        } else {
            let retained = f.retained.borrow();
            let workspace = retained.last().unwrap();
            let (red, blue) = colored_pixels(
                &workspace.join("old-build/old.pdf"),
                &f.temporary.path().join("old-color"),
            );
            assert!(red > 100 && blue == 0);
            let (red, blue) = colored_pixels(
                &workspace.join("new-build/new.pdf"),
                &f.temporary.path().join("new-color"),
            );
            assert!(blue > 100 && red == 0);
        }
    }
}

#[test]
fn figure_source_formatting_is_ignored_and_imported_assets_use_the_import_directory() {
    let f = Fixture::new();
    png(&f.repo.join("image.png"), [255, 0, 0]);
    f.write(r"\begin{figure}[ht]\centering\includegraphics[width=2cm]{image.png}\caption{A figure}\end{figure}");
    f.git(&["add", "."]);
    f.commit("image formatting");
    f.write(
        "\\begin {figure}[ht]\n\\centering\n\
         \\includegraphics [width=2cm] {image.png}\n\
         \\caption {A figure}\n\\end {figure}\n",
    );
    assert!(!f.diff(&[]).changed);
    fs::create_dir(f.repo.join("sections")).unwrap();
    png(&f.repo.join("sections/image.png"), [0, 0, 255]);
    fs::write(
        f.repo.join("sections/body.tex"),
        r"\includegraphics[width=2cm]{image.png}",
    )
    .unwrap();
    f.source(
        "main.tex",
        &(PREAMBLE.to_owned() + "\\usepackage{import}\n"),
        r"\import{sections/}{body.tex}",
    );
    f.git(&["add", "."]);
    f.commit("imported image");
    png(&f.repo.join("sections/image.png"), [0, 255, 0]);
    let report = f.diff(&[]);
    assert_eq!((report.removed_objects, report.added_objects), (1, 1));
}

#[test]
fn removed_local_packages_remain_available_to_removed_content() {
    let f = Fixture::new();
    fs::write(
        f.repo.join("local.sty"),
        "\\ProvidesPackage{local}\\newcommand{\\word}{\\iftrue Original\\else Hidden\\fi}\n",
    )
    .unwrap();
    f.source(
        "main.tex",
        &(PREAMBLE.to_owned() + "\\usepackage{local}\n"),
        r"A \word{} sentence.",
    );
    f.git(&["add", "."]);
    f.commit("local package");
    fs::remove_file(f.repo.join("local.sty")).unwrap();
    f.write("A new sentence.");
    let (_, pdf, _) = f.execute(&[], 0, None, None, None);
    let text = pdf_text(&pdf);
    assert!(text.contains("Original") && text.contains("A new sentence."));
}

#[test]
fn side_by_side_assembles_rotated_pages_at_their_original_dimensions() {
    let f = Fixture::new();
    let preamble = PREAMBLE.to_owned() + "\\usepackage{pdflscape}\n";
    let body =
        r"Portrait first page.\newpage\begin{landscape}Landscape second page.\end{landscape}";
    f.source("main.tex", &preamble, body);
    f.git(&["add", "."]);
    f.commit("landscape page");
    f.source("main.tex", &preamble, &body.replace("second", "updated"));
    let (_, pdf, _) = f.execute(&["--mode", "side-by-side"], 0, None, None, None);
    let document = lopdf::Document::load(&pdf).unwrap();
    let pages = document.get_pages();
    assert_eq!(pages.len(), 2);
    let bounds = |page: u32| {
        document
            .get_object(pages[&page])
            .unwrap()
            .as_dict()
            .unwrap()
            .get(b"MediaBox")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_float().unwrap())
            .collect::<Vec<_>>()
    };
    let portrait = bounds(1);
    let landscape = bounds(2);
    assert!(portrait[2] < landscape[2] && portrait[3] > landscape[3]);
    let text = page_text(&pdf, 2);
    assert!(text.contains("Landscape second page.") && text.contains("Landscape updated page."));
}

#[test]
fn title_images_are_included_and_both_versions_survive_source_export() {
    let f = Fixture::new();
    png(&f.repo.join("logo.png"), [255, 0, 0]);
    let preamble = PREAMBLE.to_owned()
        + r"\title{A title \includegraphics[width=1cm]{logo.png}}\author{}\date{}";
    f.source("main.tex", &preamble, r"\maketitle This remains.");
    f.git(&["add", "."]);
    f.commit("title logo");
    png(&f.repo.join("logo.png"), [0, 0, 255]);
    let source = f.temporary.path().join("title-review/review.tex");
    for mode in ["unified", "side-by-side"] {
        let report = f.diff(&["--mode", mode, "--tex-output", source.to_str().unwrap()]);
        assert_eq!((report.removed_objects, report.added_objects), (1, 1));
    }
    let pdf = tex_diff::build::compile(
        source.parent().unwrap(),
        Path::new("review.tex"),
        &f.temporary.path().join("title-export-build"),
        "pdflatex",
        Duration::from_secs(30),
    )
    .unwrap();
    assert!(pdf_text(&pdf).contains("This remains."));
}

#[test]
fn master_filenames_and_jobname_are_preserved_in_both_modes() {
    let f = Fixture::new();
    f.write(r"The document is \jobname. This remains.");
    f.git(&["add", "."]);
    f.commit("jobname context");
    fs::rename(f.repo.join("main.tex"), f.repo.join("paper.v1.tex")).unwrap();
    for mode in ["unified", "side-by-side"] {
        let (_, pdf, report) = f.execute(&["--mode", mode], 0, None, None, None);
        let text = pdf_text(&pdf);
        assert!(
            text.contains("The document is main.") && text.contains("The document is paper.v1."),
            "jobname context lost: {text}"
        );
        let report: Report = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
        assert_eq!((report.removed_sentences, report.added_sentences), (1, 1));
    }
}

#[test]
#[cfg(unix)]
fn source_worker_timeout_kills_bibtex_and_its_child_process() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.write(r"A citation~\cite{book}.\bibliographystyle{plain}\bibliography{references}");
    fs::write(
        f.repo.join("references.bib"),
        "@book{book, author={Alice}, title={Test}, year={2020}, publisher={Press}}\n",
    )
    .unwrap();
    let bin = f.temporary.path().join("fake-bibtex");
    fs::create_dir(&bin).unwrap();
    let pid_file = f.temporary.path().join("child.pid");
    let quoted = format!("'{}'", pid_file.to_string_lossy().replace('\'', "'\\''"));
    fs::write(
        bin.join("bibtex"),
        format!("#!/bin/sh\nsleep 30 &\nchild=$!\nprintf '%s\\n' \"$child\" > {quoted}\nwait\n"),
    )
    .unwrap();
    fs::set_permissions(bin.join("bibtex"), fs::Permissions::from_mode(0o755)).unwrap();
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let paths = std::env::join_paths(paths).unwrap();
    let start = Instant::now();
    let (result, _, _) = f.execute(&["--timeout", "1"], 2, None, Some(&paths), None);
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(String::from_utf8_lossy(&result.stderr).contains("exceeded"));
    let pid = fs::read_to_string(pid_file).unwrap();
    let output = Command::new("ps")
        .args(["-p", pid.trim(), "-o", "stat="])
        .output()
        .unwrap();
    let state = String::from_utf8_lossy(&output.stdout);
    assert!(
        state.trim().is_empty() || state.trim().starts_with('Z'),
        "BibTeX child is still running: {state}"
    );
}
