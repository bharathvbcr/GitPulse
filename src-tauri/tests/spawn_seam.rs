//! Every production subprocess must go through the gated seam in
//! `engine::git_cli`.
//!
//! The descriptor budget is only a budget for what passes through the gate.
//! A `Command::new` written anywhere else spawns outside it, and a workspace
//! with several repositories open walks back into the "Failed to spawn git
//! ...: Too many open files (os error 24)" storm that `limits::raise_open_
//! file_limit` and the gate exist between them to prevent — on a process whose
//! limit was raised, which is the version of the failure that looks impossible
//! from the log. It also loses the command timeout, the stdout/stderr caps,
//! the scrubbed `GIT_*` environment and the GUI-launch program lookup.
//!
//! So the absence is asserted rather than intended. This walks `src/`, strips
//! `#[cfg(test)]` items, and fails on any `Command::new` outside the
//! allowlist below — naming the file and line, and reporting how much it
//! actually read.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Files permitted to construct a `Command` directly, and why.
const ALLOWED: &[(&str, &str)] = &[
    (
        "src/engine/git_cli.rs",
        "the seam itself: it owns the gate, the timeout and the output caps",
    ),
    (
        "src/harness/sidecar.rs",
        "one long-lived sidecar per session; holding a gate permit for its \
         whole life would starve every git call behind it",
    ),
];

#[test]
fn no_production_code_spawns_outside_the_gated_seam() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs(&root, &mut files);
    assert!(
        files.len() > 50,
        "expected to walk the whole crate, only found {} files — a scan that \
         did not run must not read as a scan that passed",
        files.len()
    );

    let mut offenders: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut scanned_lines = 0usize;
    for file in &files {
        let rel = file
            .strip_prefix(env!("CARGO_MANIFEST_DIR"))
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(file).expect("read source");
        let production = strip_test_items(&text);
        scanned_lines += production.len();
        if ALLOWED.iter().any(|(allowed, _)| *allowed == rel) {
            continue;
        }
        for (line_no, line) in &production {
            if line.contains("Command::new") && !line.trim_start().starts_with("//") {
                offenders.entry(rel.clone()).or_default().push(*line_no);
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "these spawn outside `engine::git_cli`, so they are not covered by the \
         descriptor gate, the timeout or the output caps: {offenders:?}\n\
         Route them through `git`, `git_text`, `git_captured` or \
         `capture_command`, or add the file to ALLOWED with the reason.",
    );

    // A pass has to be able to say what it looked at: an allowlist that has
    // rotted into naming files that no longer exist would otherwise read the
    // same as a clean scan.
    for (allowed, _) in ALLOWED {
        assert!(
            root.parent().expect("manifest dir").join(allowed).is_file(),
            "ALLOWED names {allowed}, which is not a file any more"
        );
    }
    eprintln!(
        "scanned {} files, {scanned_lines} production lines, {} allowlisted",
        files.len(),
        ALLOWED.len()
    );
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Returns `(1-based line number, text)` for every line outside a
/// `#[cfg(test)]` item.
///
/// Brace-matched rather than "everything before the first `#[cfg(test)]`":
/// several modules here put production code *after* their test module, and a
/// truncating scan would report those files as clean without having read them.
fn strip_test_items(src: &str) -> Vec<(usize, &str)> {
    let lines: Vec<&str> = src.lines().collect();
    let mut kept = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim_start().starts_with("#[cfg(test)]") {
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim_start().starts_with("#[") {
                j += 1;
            }
            let mut depth: i32 = 0;
            let mut opened = false;
            while j < lines.len() {
                depth += lines[j].matches('{').count() as i32;
                depth -= lines[j].matches('}').count() as i32;
                if lines[j].contains('{') {
                    opened = true;
                }
                if opened && depth <= 0 {
                    break;
                }
                // A `#[cfg(test)] use ...;` has no braces at all.
                if !opened && lines[j].trim_end().ends_with(';') {
                    break;
                }
                j += 1;
            }
            i = j + 1;
            continue;
        }
        kept.push((i + 1, lines[i]));
        i += 1;
    }
    kept
}

/// The stripper is the load-bearing half of the check above: if it silently
/// swallowed production code, the scan would pass by not looking.
#[test]
fn the_test_stripper_keeps_production_code_on_both_sides_of_a_test_module() {
    let src = "fn before() {}\n\
               #[cfg(test)]\n\
               mod tests {\n\
               fn hidden() { let _ = 1; }\n\
               }\n\
               fn after() {}\n";
    let kept: Vec<&str> = strip_test_items(src).into_iter().map(|(_, l)| l).collect();
    assert_eq!(kept, vec!["fn before() {}", "fn after() {}"]);

    let attr = "#[cfg(test)]\nuse std::process::Command;\nfn after() {}\n";
    let kept: Vec<&str> = strip_test_items(attr).into_iter().map(|(_, l)| l).collect();
    assert_eq!(kept, vec!["fn after() {}"]);
}
