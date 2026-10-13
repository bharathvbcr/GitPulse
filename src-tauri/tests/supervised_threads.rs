//! No long-lived background thread starts outside `supervisor`.
//!
//! A raw `std::thread` start that nothing joins loses its panic: the hook logs
//! it and the work simply stops. Every such thread starts through
//! `supervisor::{spawn_service, spawn_once, spawn_loop}`, which record a panic
//! as a degraded subsystem (Diagnostics) and restart or stop visibly. This
//! test fails when a production file starts a raw thread that is not listed
//! below with the reason it is exempt, and when a listed entry has gone stale.
//! Scoped threads (`thread::scope`) are bounded by their caller and not
//! matched.

mod common;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// file (relative to `src/`) -> (raw starts allowed, why they are exempt).
const ALLOWED: &[(&str, usize, &str)] = &[
    ("supervisor.rs", 3, "the supervisor itself"),
    ("procguard/mod.rs", 2, "below the supervisor: the shutdown hook runner and the signal watcher run while the process is exiting"),
    ("teardown.rs", 1, "a one-shot, deadline-bounded helper that is abandoned on timeout; its result is the report"),
    ("hooks/mod.rs", 1, "per-call budget worker in a short-lived hook process that exits right after"),
    ("ai/discovery.rs", 1, "per-call model listing joined by its caller, which reports a panic as an error"),
    ("engine/git_cli/thread_io.rs", 1, "per-command pipe drain joined by the command runner"),
    ("engine/git_reader.rs", 2, "scoped language-scan helpers (Builder::new + spawn_scoped, the only fallible named scoped spawn) joined by the caller, where a panic resumes; bounded by the scan, not long-lived"),
    ("harness/sidecar.rs", 1, "per-write stdin helper bounded by a deadline; its failure is the call's error"),
    ("commands/mod.rs", 1, "per-IPC-call off-thread work whose JoinError reaches the caller"),
    ("desktop/mod.rs", 1, "per-call off-thread work whose JoinError reaches the caller"),
    ("logging.rs", 1, "per-call log read whose JoinError reaches the caller"),
    ("workbench.rs", 4, "per-IPC-call off-thread work whose JoinError reaches the caller"),
];

/// Modules compiled only under `#[cfg(test)]`; each must say so where declared.
const TEST_ONLY: &[(&str, &str)] = &[
    ("alerts/stress.rs", "alerts/mod.rs"),
    ("watcher/registration_timing.rs", "watcher/mod.rs"),
    ("watcher/storm.rs", "watcher/mod.rs"),
];

fn src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn is_test_file(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name.ends_with("tests.rs")
        || name.ends_with("_tests.rs")
        || name == "test_support.rs"
        || rel.starts_with("test_support/")
        || TEST_ONLY.iter().any(|(file, _)| *file == rel)
}

const NEEDLES: &[&str] = &[
    "thread::spawn(",
    "thread::Builder::new(",
    "async_runtime::spawn",
    "tokio::spawn",
    "spawn_scoped",
];

/// Raw starts in the production part of a file: up to its `mod tests {`.
fn raw_starts(text: &str) -> usize {
    let mut count = 0;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("mod tests") || trimmed.starts_with("mod verification_note_tests") {
            break;
        }
        if trimmed.starts_with("//") {
            continue;
        }
        if NEEDLES.iter().any(|needle| line.contains(needle)) {
            count += 1;
        }
    }
    count
}

#[test]
fn every_long_lived_thread_starts_through_the_supervisor() {
    let mut files = Vec::new();
    rust_files(&src(), &mut files);
    assert!(files.len() > 100, "the scan found {} files", files.len());
    let allowed: BTreeMap<&str, usize> = ALLOWED.iter().map(|(f, n, _)| (*f, *n)).collect();
    let mut found: BTreeMap<String, usize> = BTreeMap::new();
    for path in files {
        let rel = path
            .strip_prefix(src())
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if is_test_file(&rel) {
            continue;
        }
        let n = raw_starts(&std::fs::read_to_string(&path).expect("read"));
        if n > 0 {
            found.insert(rel, n);
        }
    }
    let mut problems = Vec::new();
    for (file, n) in &found {
        match allowed.get(file.as_str()) {
            None => problems.push(format!(
                "{file}: {n} raw thread start(s) outside supervisor::spawn_*"
            )),
            Some(max) if n > max => problems.push(format!("{file}: {n} raw starts, {max} allowed")),
            Some(max) if n < max => problems.push(format!(
                "{file}: only {n} raw starts; lower its allowance from {max}"
            )),
            Some(_) => {}
        }
    }
    for file in allowed.keys() {
        if !found.contains_key(*file) {
            problems.push(format!(
                "{file}: listed as exempt but starts no raw thread; remove it"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn test_only_modules_are_declared_under_cfg_test() {
    for (file, parent) in TEST_ONLY {
        let module = Path::new(file)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let text = std::fs::read_to_string(src().join(parent)).expect("parent");
        let lines: Vec<&str> = text.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.trim() == format!("mod {module};"))
            .unwrap_or_else(|| panic!("{parent} no longer declares {module}"));
        assert!(
            lines[at - 1].trim() == "#[cfg(test)]",
            "{file} is exempt as test-only but {parent} declares it without #[cfg(test)]"
        );
    }
}
