//! The load governor is asked in exactly the places it was designed to be.
//!
//! `sysload::admit` may refuse an agent launch, so where it is called is the
//! whole of "what pressure can refuse". The never-gated set (stop, kill,
//! cancel, save, commit, release, reconcile) must stay outside it: each
//! frees what it holds or preserves work, and refusing one at the moment the
//! machine is short of memory would be the failure the governor exists to
//! prevent. This pins the call sites by the function that holds them, so a
//! new `admit` call in a stop or release path fails here by name.
//!
//! `sysload::shed` has one caller too: the spawn gate. Shedding is a verdict
//! the existing admission classes consume, not a second scheduler.

mod common;

use std::path::{Path, PathBuf};

fn crate_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Source before its first `#[cfg(test)]` module: production code.
fn production(source: &str) -> &str {
    source
        .find("#[cfg(test)]\nmod tests")
        .map_or(source, |end| &source[..end])
}

/// A file that is only ever compiled as a `#[cfg(test)]` module of its own
/// (`tests.rs`, `*_tests.rs`). `production` cannot cut these: the attribute
/// sits in the parent, so the whole file is test code and may call
/// `sysload::shed` or `admit` to exercise the gate without being a caller of it.
fn is_test_module_file(file: &Path) -> bool {
    file.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "tests.rs" || name.ends_with("_tests.rs"))
}

/// `file:function` for every production line that contains one of `needles`,
/// the function being the nearest `fn` at or above the line.
fn call_sites(needles: &[&str]) -> Vec<String> {
    let mut files = Vec::new();
    rust_files(&crate_dir().join("src"), &mut files);
    let sysload = crate_dir().join("src").join("sysload");
    let mut sites = Vec::new();
    for file in &files {
        if file.starts_with(&sysload) || is_test_module_file(file) {
            continue;
        }
        let source = std::fs::read_to_string(file).unwrap();
        let mut current = String::from("<module>");
        for line in production(&source).lines() {
            let code = line.split("//").next().unwrap_or("");
            if let Some(at) = code.find("fn ") {
                let name = code[at + 3..]
                    .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .next()
                    .unwrap_or("");
                if !name.is_empty() {
                    current = name.to_string();
                }
            }
            if needles.iter().any(|needle| code.contains(needle)) {
                let rel = file
                    .strip_prefix(crate_dir())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                sites.push(format!("{rel}:{current}"));
            }
        }
    }
    sites.sort();
    sites.dedup();
    sites
}

#[test]
fn admit_is_asked_only_where_a_launch_starts() {
    let sites = call_sites(&["sysload::admit(", "admit_launch("]);
    assert_eq!(
        sites,
        [
            // The PTY of an agent CLI: the app's side of every spawn, whichever
            // backend holds the PTY, because the governor's "Admit anyway"
            // override lives in the app (docs/TERMINAL_HOST.md, row 9).
            "src/terminal/mod.rs:spawn_at",
            // The helper, and its two launch steps: before the checkout
            // probes, and before the capacity check and worktree build.
            "src/workbench/terminal_launch.rs:admit_launch",
            "src/workbench/terminal_launch.rs:ensure_room",
            "src/workbench/terminal_launch.rs:prepare_in",
        ],
        "an admit call moved, or a new one was added: the never-gated set (stop, kill, \
         cancel, save, commit, release, reconcile) must stay outside it"
    );
}

#[test]
fn shed_is_read_only_by_the_spawn_gate() {
    assert_eq!(
        call_sites(&["sysload::shed("]),
        ["src/engine/git_cli.rs:acquire_until"],
        "shedding rides the existing admission classes; a second reader of the verdict \
         would be a second scheduler"
    );
}

#[test]
fn the_never_gated_verbs_hold_no_admit_call() {
    for site in call_sites(&["sysload::admit(", "admit_launch("]) {
        let function = site.rsplit(':').next().unwrap();
        for verb in [
            "stop",
            "kill",
            "cancel",
            "save",
            "commit",
            "release",
            "reconcile",
        ] {
            assert!(
                !function.contains(verb),
                "{site} gates a never-gated verb ({verb})"
            );
        }
    }
}
