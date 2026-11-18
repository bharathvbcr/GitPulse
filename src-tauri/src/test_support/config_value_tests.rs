//! `git_config_value` must hand git back exactly the text it was given.
//!
//! The Windows failure this guards (`fatal: bad config line N`, ~40 tests)
//! needs a drive-letter path, which a non-Windows host cannot produce as a real
//! path. A backslash path is only text to git, though, so the round trip is
//! checked here on every host through `git config --file`.

use super::env::{git_config_value, write_git_config};
use crate::procguard::LockedSpawn;
use std::process::Command;

fn git_get(file: &std::path::Path) -> std::process::Output {
    let mut command = Command::new("git");
    super::pin_git(&mut command);
    command
        .args(["config", "--file"])
        .arg(file)
        .args(["--get", "probe.value"])
        .output_locked()
        .expect("spawn git")
}

fn read_back(value_text: &str) -> String {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("cfg");
    write_git_config(
        &file,
        &format!("[probe]\n\tvalue = {}\n", git_config_value(value_text)),
    )
    .unwrap();
    let output = git_get(&file);
    assert!(
        output.status.success(),
        "git rejected {value_text:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .trim_end_matches('\n')
        .to_string()
}

#[test]
fn a_windows_path_with_backslashes_round_trips() {
    for text in [
        r"C:\Users\runneradmin\AppData\Local\Temp\.tmpMAR7e4\people.mailmap",
        r"\\?\C:\Users\RUNNER~1\AppData\Local\Temp\x",
        r"\\server\share\dir\file",
        r"C:\Users\u\a\b\t\n\x",
    ] {
        assert_eq!(read_back(text), text);
    }
}

#[test]
fn quotes_comment_characters_and_spaces_survive() {
    for text in [
        r#"a "quoted" name"#,
        "dir #1/file;x",
        "  padded  ",
        "tab\there",
        "",
    ] {
        assert_eq!(read_back(text), text);
    }
}

#[test]
fn the_unquoted_spelling_is_what_git_rejects_or_corrupts() {
    // The control: without the helper a `\U` escape is not a config value.
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("cfg");
    write_git_config(&file, "[probe]\n\tvalue = C:\\Users\\x\n").unwrap();
    let output = git_get(&file);
    let got = String::from_utf8_lossy(&output.stdout);
    assert!(
        !output.status.success() || got.trim_end() != r"C:\Users\x",
        "raw backslashes round-tripped; the control proves nothing: {got:?}"
    );
}
