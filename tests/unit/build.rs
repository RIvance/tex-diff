use super::{LOG_TAIL_BYTES, MAX_LOG_ERRORS, log_diagnostics, log_tail, run_logged};
use std::{fs, io::Write};

#[test]
fn log_tail_reads_only_the_bounded_end_of_large_logs() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&vec![b'x'; (LOG_TAIL_BYTES * 4) as usize])
        .unwrap();
    writeln!(file).unwrap();
    for line in 0..30 {
        writeln!(file, "diagnostic {line}").unwrap();
    }

    let expected = (12..30)
        .map(|line| format!("diagnostic {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(log_tail(file.path()), expected);

    fs::write(file.path(), vec![b'x'; (LOG_TAIL_BYTES * 4) as usize]).unwrap();
    assert_eq!(log_tail(file.path()).len(), LOG_TAIL_BYTES as usize);
}

#[test]
fn log_tail_keeps_diagnostics_when_a_log_contains_invalid_utf8() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"bad byte: \xff\nLaTeX error: missing command\n")
        .unwrap();

    assert!(log_tail(file.path()).ends_with("LaTeX error: missing command"));
}

#[test]
fn diagnostics_show_early_errors_instead_of_later_citation_warnings() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    writeln!(
        file,
        "main.tex:42: Undefined control sequence.\nl.42 \\firstmissing\n"
    )
    .unwrap();
    writeln!(file, "! Missing $ inserted.\nl.60 x_1\n").unwrap();
    for _ in 0..1000 {
        writeln!(file, "LaTeX Warning: Citation `example' undefined.").unwrap();
    }

    let diagnostics = log_diagnostics(file.path());
    assert!(diagnostics.contains("main.tex:42: Undefined control sequence."));
    assert!(diagnostics.contains(r"l.42 \firstmissing"));
    assert!(diagnostics.contains("! Missing $ inserted."));
    assert!(diagnostics.contains("l.60 x_1"));
    assert!(!diagnostics.contains("Citation"));
}

#[test]
fn diagnostics_rejoin_wrapped_paths_and_line_numbers() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"bad byte: \xff\n\n/tmp/a-long-project/\nunified/main.tex:8\n50: You can't use a prefix with `the character @'.\n<to be read again>\n  @\nl.850 \\protected@edef\n").unwrap();

    let diagnostics = log_diagnostics(file.path());
    assert!(diagnostics.starts_with("/tmp/a-long-project/unified/main.tex:850:"));
    assert!(diagnostics.contains(r"l.850 \protected@edef"));
}

#[test]
fn successive_wrapped_errors_keep_both_locations_and_source_context() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    for _ in 0..100 {
        writeln!(file, "(loading a package)").unwrap();
    }
    file.write_all(b"/tmp/project/main.tex:2\n8: Undefined control sequence.\nl.28 \\firstmissing\n(../old-macros.tex)\n/tmp/project/main.tex:3\n6: Undefined control sequence.\nl.36 \\secondmissing\n\n").unwrap();
    let diagnostics = log_diagnostics(file.path());
    assert!(diagnostics.starts_with("/tmp/project/main.tex:28:"));
    assert!(diagnostics.contains("/tmp/project/main.tex:36:"));
    assert!(diagnostics.contains(r"l.28 \firstmissing"));
    assert!(diagnostics.contains(r"l.36 \secondmissing"));
    assert!(!diagnostics.contains("loading a package"));
}

#[test]
fn diagnostics_limit_errors_and_fall_back_to_the_tail_for_other_failures() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    for line in 1..=MAX_LOG_ERRORS + 1 {
        writeln!(file, "main.tex:{line}: Undefined control sequence.\n").unwrap();
    }
    let diagnostics = log_diagnostics(file.path());
    assert_eq!(
        diagnostics.matches("Undefined control sequence").count(),
        MAX_LOG_ERRORS
    );
    assert!(diagnostics.contains("Further errors are in the full log."));

    fs::write(file.path(), "latexmk: compiler executable not found\n").unwrap();
    assert_eq!(log_diagnostics(file.path()), log_tail(file.path()));
}

#[test]
#[cfg(unix)]
fn failed_commands_terminate_their_remaining_children() {
    use std::{process::Command, time::Duration};

    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("child.pid");
    let mut command = Command::new("sh");
    command
        .args([
            "-c",
            "sleep 30 &\nprintf '%s\\n' \"$!\" > \"$1\"\nexit 7",
            "tex-diff-test",
        ])
        .arg(&pid_file);
    let error = run_logged(
        command,
        &directory.path().join("build.log"),
        Duration::from_secs(5),
    )
    .unwrap_err();
    assert!(error.to_string().contains("failed"));
    let pid = fs::read_to_string(pid_file).unwrap();
    for _ in 0..20 {
        let output = Command::new("ps")
            .args(["-p", pid.trim(), "-o", "stat="])
            .output()
            .unwrap();
        let state = String::from_utf8_lossy(&output.stdout);
        if state.trim().is_empty() || state.trim().starts_with('Z') {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("failed command left child {pid} running");
}
