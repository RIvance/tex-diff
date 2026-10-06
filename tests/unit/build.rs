use super::{LOG_TAIL_BYTES, log_tail};
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
