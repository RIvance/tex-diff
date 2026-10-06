use super::{check_path, parse_entries};
use std::path::Path;

#[test]
fn nul_delimited_paths_and_conflicts() {
    let entries = parse_entries(b"100644 abc 0\todd\tname\n.tex\0", true).unwrap();
    assert_eq!(entries[0].path, Path::new("odd\tname\n.tex"));
    assert!(parse_entries(b"100644 abc 2\tmain.tex\0", true).is_err());
}

#[test]
fn traversal_is_rejected() {
    assert!(check_path(Path::new("../main.tex")).is_err());
    assert!(check_path(Path::new("/main.tex")).is_err());
    assert!(check_path(Path::new("chapter/main.tex")).is_ok());
}
