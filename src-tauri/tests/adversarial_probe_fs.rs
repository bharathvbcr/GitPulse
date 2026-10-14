//! Probe: which `.git` spellings does this filesystem resolve to the real
//! `.git`? Decides whether the unicode cases in
//! `adversarial_write_primitives` are attacks or just odd directory names.

#![cfg(unix)]

#[test]
fn which_spellings_resolve_to_dot_git() {
    let dir = tempfile::TempDir::new().unwrap();
    let repo = dir.path().canonicalize().unwrap();
    std::fs::create_dir(repo.join(".git")).unwrap();
    std::fs::write(repo.join(".git/config"), "x").unwrap();
    let mut resolving = Vec::new();
    for name in [
        ".GIT", ".git.", ".git ", ".g\u{200c}it", ".g\u{200d}it", ".\u{feff}git", ".git\u{200b}",
        ".gi\u{0307}t", ".g\u{0131}t", "\u{200c}.git", ".git\u{fe0f}",
    ] {
        if std::fs::metadata(repo.join(name).join("config")).is_ok() {
            resolving.push(name.escape_unicode().to_string());
        }
    }
    println!("PROBE resolving spellings: {resolving:?}");
    assert!(
        resolving.iter().all(|n| n == "\\u{2e}\\u{47}\\u{49}\\u{54}"),
        "non-case spellings resolve to .git on this filesystem: {resolving:?}"
    );
}
