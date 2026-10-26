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
    ("ledger/anchor.rs", 1, "Prober::slot's one-shot probe of one earlier location, at most one per path and 64 in all, abandoned at the caller's deadline; it runs under catch_unwind, so a panic is recorded in its slot and the caller reports a refusal"),
    ("hooks/mod.rs", 1, "per-call budget worker in a short-lived hook process that exits right after"),
    ("ai/discovery.rs", 1, "per-call model listing joined by its caller, which reports a panic as an error"),
    ("engine/git_cli/thread_io.rs", 1, "per-command pipe drain joined by the command runner"),
    ("engine/git_reader.rs", 2, "scoped language-scan helpers (Builder::new + spawn_scoped, the only fallible named scoped spawn) joined by the caller, where a panic resumes; bounded by the scan, not long-lived"),
    ("harness/sidecar.rs", 1, "per-write stdin helper bounded by a deadline; its failure is the call's error"),
    ("commands/mod.rs", 1, "per-IPC-call off-thread work whose JoinError reaches the caller"),
    ("desktop/mod.rs", 1, "per-call off-thread work whose JoinError reaches the caller"),
    ("logging.rs", 1, "per-call log read whose JoinError reaches the caller"),
    ("workbench.rs", 5, "per-IPC-call off-thread work, and admit's wait for a storage slot, whose JoinError reaches the caller"),
    ("terminal/executor.rs", 1, "an on-demand lane worker that runs every job under catch_unwind; a job that panics drops its reply, which its caller reports as an error, and the next submission starts a replacement if the pool is short"),
    ("terminal/mod.rs", 1, "hold_master, a #[doc(hidden)] hook for tests that holds a master lock until its MasterHold is released and joins the thread"),
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

/// A source file compiled only for tests: exactly `tests.rs`, `*_tests.rs`,
/// exactly `test_support.rs`, and anything under a directory named exactly
/// `test_support`. A bare `ends_with("tests.rs")` would also skip a production
/// `contests.rs`, and a prefix match a production `test_supports/pool.rs`.
fn is_test_file(rel: &str) -> bool {
    let mut parts = rel.rsplit('/');
    let name = parts.next().unwrap_or(rel);
    name == "tests.rs"
        || name.ends_with("_tests.rs")
        || name == "test_support.rs"
        || parts.any(|dir| dir == "test_support")
        || TEST_ONLY.iter().any(|(file, _)| *file == rel)
}

#[test]
fn only_the_named_shapes_are_test_files() {
    for test in [
        "tests.rs",
        "alerts/tests.rs",
        "diff/adversarial_tests.rs",
        "test_support.rs",
        "test_support/env.rs",
        "alerts/stress.rs",
    ] {
        assert!(is_test_file(test), "{test} is test-only");
    }
    for production in [
        "contests.rs",
        "a/contests.rs",
        "latests.rs",
        "supervisor.rs",
    ] {
        assert!(!is_test_file(production), "{production} is production");
    }
}

/// FAILS BY DESIGN (severity low): the "exact" matcher still skips any file or
/// directory whose name merely STARTS with `test_support`, so a production
/// `test_supports/pool.rs` or `test_support_bridge.rs` escapes the raw-spawn
/// scan the way `contests.rs` used to.
#[test]
fn a_test_support_prefix_alone_does_not_make_a_production_file_test_only() {
    for production in ["test_supports/pool.rs", "test_support_bridge.rs"] {
        assert!(
            !is_test_file(production),
            "{production} is skipped by the raw thread::spawn lint"
        );
    }
}

const NEEDLES: &[&str] = &[
    "thread::spawn(",
    "thread::Builder::new(",
    "async_runtime::spawn",
    "tokio::spawn",
    "spawn_scoped",
];

/// Strips an optional visibility prefix (`pub`, `pub(crate)`, `pub(super)`,
/// `pub(self)`, `pub(in path)`) from a trimmed line.
fn strip_visibility(trimmed: &str) -> &str {
    let Some(rest) = trimmed.strip_prefix("pub") else {
        return trimmed;
    };
    if let Some(inner) = rest.strip_prefix('(') {
        return inner
            .split_once(')')
            .map_or(trimmed, |(_, after)| after.trim_start());
    }
    match rest.strip_prefix(char::is_whitespace) {
        Some(after) => after.trim_start(),
        None => trimmed,
    }
}

/// Whether a trimmed line opens an inline test module, with any visibility.
fn opens_test_module(trimmed: &str) -> bool {
    let rest = strip_visibility(trimmed);
    ["mod tests", "mod verification_note_tests"]
        .iter()
        .any(|name| {
            rest.strip_prefix(name)
                .is_some_and(|after| after.trim_start().starts_with('{'))
        })
}

/// Raw starts in the production part of a file: up to its top-level test
/// module, of any visibility (`mod tests`, `pub(crate) mod tests`, ...). Files
/// keep further test-only modules after it, so a top-level one ends the scan. A
/// nested, indented test module (one inside a `cfg` module of platform code) is
/// skipped to its closing brace, which rustfmt puts at the module's own indent,
/// and the scan goes on: production code after it is still production code.
fn raw_starts(text: &str) -> usize {
    let mut count = 0;
    let mut skip_to: Option<String> = None;
    for line in text.lines() {
        if let Some(end) = &skip_to {
            if line == end {
                skip_to = None;
            }
            continue;
        }
        let trimmed = line.trim_start();
        if opens_test_module(trimmed) {
            let indent = &line[..line.len() - trimmed.len()];
            if indent.is_empty() {
                break;
            }
            if !trimmed.trim_end().ends_with('}') {
                skip_to = Some(format!("{indent}}}"));
            }
            continue;
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
            requires_test(lines[at - 1]),
            "{file} is exempt as test-only but {parent} declares it without #[cfg(test)]"
        );
    }
}

/// Whether a `cfg` attribute can only hold in a test build: `test` itself, or
/// an `all(..)` with `test` among its terms. `any(test, ..)` holds outside one.
fn requires_test(attribute: &str) -> bool {
    let attribute = attribute.trim();
    attribute == "#[cfg(test)]"
        || attribute
            .strip_prefix("#[cfg(all(")
            .and_then(|rest| rest.strip_suffix("))]"))
            .is_some_and(|terms| terms.split(',').any(|term| term.trim() == "test"))
}

#[test]
fn a_test_only_cfg_is_test_or_all_of_test_and_more() {
    assert!(requires_test("#[cfg(test)]"));
    assert!(requires_test("    #[cfg(all(test, unix))]"));
    assert!(requires_test("#[cfg(all(unix, test))]"));
    assert!(!requires_test("#[cfg(any(test, unix))]"));
    assert!(!requires_test("#[cfg(unix)]"));
    assert!(!requires_test("#[cfg(all(testing, unix))]"));
}

/// A test module nested in a platform `cfg` module hides nothing after it: the
/// production spawn below it is counted, its own test spawns are not, and a
/// top-level `mod tests` still ends the scan.
#[test]
fn the_scan_skips_a_nested_test_module_and_counts_what_follows() {
    let text = "mod sys {\n    fn a() { std::thread::spawn(|| {}); }\n    #[cfg(test)]\n    mod tests {\n        fn t() { std::thread::spawn(|| {}); }\n    }\n}\nfn b() { std::thread::Builder::new(); }\nmod tests {\n    fn t() { std::thread::spawn(|| {}); }\n}\n";
    assert_eq!(raw_starts(text), 2);
}

/// A top-level `#[cfg(test)] pub(crate) mod tests {` is test code whatever its
/// visibility: its raw spawns are not counted, a production spawn before it is,
/// and the other visibility forms behave the same. The same holds for a nested
/// test module of any visibility, after which production code still counts.
#[test]
fn a_test_module_of_any_visibility_is_skipped_and_production_around_it_counts() {
    let spawn = "    fn t() { std::thread::spawn(|| {}); }\n";
    for head in [
        "mod tests {",
        "pub mod tests {",
        "pub(crate) mod tests {",
        "pub(super) mod tests {",
        "pub(in crate::a) mod tests {",
        "mod verification_note_tests {",
        "pub(crate) mod verification_note_tests {",
    ] {
        let test_mod = format!("#[cfg(test)]\n{head}\n{spawn}}}\n");
        let top = format!("fn before() {{ std::thread::spawn(|| {{}}); }}\n{test_mod}");
        assert_eq!(raw_starts(&top), 1, "{head}");
        assert_eq!(raw_starts(&test_mod), 0, "{head}");
        let nested = format!(
            "mod sys {{\n    fn a() {{ std::thread::spawn(|| {{}}); }}\n    #[cfg(test)]\n    {head}\n{spawn}    }}\n    fn b() {{ std::thread::Builder::new(); }}\n}}\n"
        );
        assert_eq!(raw_starts(&nested), 2, "{head}");
    }
    assert_eq!(
        raw_starts("mod tests_helper {\n    fn t() { std::thread::spawn(|| {}); }\n}\n"),
        1
    );
}
