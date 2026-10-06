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
    (
        "src/procguard/mod.rs",
        "the Windows arm of the tree kill (`taskkill /T /F`), which moved here \
         from `git_cli::kill_process_tree` when process-group teardown became \
         one owner. It kills children rather than doing work: a gate permit, a \
         timeout and an output cap are exactly what a shutdown must not wait \
         on. `procguard::spawn` itself never constructs a `Command` — it is \
         handed one the caller built inside the seam, so the rule this test \
         exists for is untouched",
    ),
    (
        "src/tool_install/mod.rs",
        "optional-tools install ladder (cargo/go/curl and binary probes). These \
         are not git; every `Command` is handed to `git_cli::run_bounded_capped` \
         so the timeout and stdout/stderr caps still apply. Holding a gate \
         permit for a multi-minute `cargo install` would starve every git call",
    ),
    (
        "src/tool_install/release.rs",
        "release-asset discovery for the same ladder; `Command`s are probes \
         handed to `run_bounded_capped`, not ungated spawns",
    ),
    (
        "src/devmap/cli.rs",
        "devmap binary invocation for map builds/queries; builds a `Command` \
         then hands it to `git_cli::run_bounded_capped`",
    ),
    (
        "src/tool_install/components.rs",
        "DevCouncil component discovery, the same probe ladder as \
         `tool_install/mod.rs`: its single `Command` is handed to \
         `git_cli::run_bounded_capped` with a 5s timeout and a 64 KiB cap, and \
         it runs in `std::env::temp_dir()` so a version probe can never read a \
         repository. Not git, and a gate permit held across a probe of every \
         optional component would starve the git calls behind it",
    ),
    (
        "src/engine/worktree_hooks.rs",
        "repository hook commands (`sh -c` / `cmd /C`) with the repository's \
         own env, run only after `repository_trust::require`. Each `Command` is \
         handed to `git_cli::run_bounded_capped` with a 15-minute deadline and a \
         1 MiB cap; it was `child.output()`, unbounded",
    ),
    (
        "src/secrets/run.rs",
        "the kingfisher secret scanner: `scrubbed_command` builds a `Command` \
         with `env_clear()` and a scrubbed environment, which the git seam's \
         helpers cannot express, then hands it to `git_cli::run_bounded_capped` \
         with the scan and version deadlines and a stdout cap",
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

/// Creating a process, or a descriptor that is about to be made `FD_CLOEXEC`,
/// must happen under `procguard::with_inheritance_lock`.
///
/// `std` has no `pipe2` on macOS or the BSDs, so its pipes are `pipe()`
/// followed by two `fcntl(F_SETFD, FD_CLOEXEC)` calls. A process created on
/// another thread in between inherits the pipe for life, and a stolen *write*
/// end means the owner's pipe never reaches EOF — the "could not be read to
/// the end" loss the app's own diagnostics reported against `git diff`,
/// `show`, `rev-list` and `for-each-ref`.
///
/// `Command::new` is covered by the seam above, which funnels every one of
/// those into `procguard::spawn`. The PTY is the other creator, and it is
/// invisible to that check: it builds a `CommandBuilder`, not a `Command`.
/// It is also the one whose children live for hours, so a theft there is not
/// repaid in the next millisecond. A raw `libc::pipe` is the third: it is not
/// close-on-exec at all until its own `fcntl` calls run. Asserted by shape
/// rather than trusted: each must name the lock on its own line.
/// Every child created from a `Command` — tests, benches and tools included —
/// takes the inheritance lock, because `clippy.toml` bans the three `std`
/// methods that create one and the only sanctioned ways round the ban are
/// `procguard::spawn` and `procguard::LockedSpawn`. This keeps the ban from
/// being deleted, and keeps the exemptions to the sites that were reviewed:
/// one test that forked without the lock kept another test's pipe open.
#[test]
fn every_command_spawn_is_banned_outside_the_locked_seams() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let config = std::fs::read_to_string(root.join("clippy.toml")).expect("clippy.toml");
    for method in ["spawn", "output", "status"] {
        let path = format!("\"std::process::Command::{method}\"");
        assert!(config.contains(&path), "clippy.toml no longer bans {path}");
    }

    let mut files = vec![root.join("build.rs")];
    for dir in ["src", "tests", "benches"] {
        collect_rs(&root.join(dir), &mut files);
    }
    // Split so this file does not match itself.
    const EXEMPTION: &str = concat!("#[allow(clippy::", "disallowed_methods)]");
    let mut exemptions: BTreeMap<String, usize> = BTreeMap::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read source");
        let count = text
            .lines()
            .filter(|line| line.trim_start().starts_with(EXEMPTION))
            .count();
        if count > 0 {
            let rel = file
                .strip_prefix(&root)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/");
            exemptions.insert(rel, count);
        }
    }
    let expected: BTreeMap<String, usize> = [
        // A build script is its own single-threaded process.
        ("build.rs", 2),
        // `procguard::spawn` and `LockedSpawn::spawn_locked`, both under the lock.
        ("src/procguard/mod.rs", 2),
    ]
    .into_iter()
    .map(|(file, count)| (file.to_string(), count))
    .collect();
    assert_eq!(
        exemptions, expected,
        "a new exemption from the spawn ban: route the child through \
         procguard::spawn or procguard::LockedSpawn instead"
    );
}

#[test]
fn pty_creation_runs_under_the_inheritance_lock() {
    const CREATORS: &[&str] = &[".openpty(", ".spawn_command(", "libc::pipe("];
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs(&root, &mut files);

    let mut unguarded: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut guarded = 0usize;
    for file in &files {
        let rel = file
            .strip_prefix(env!("CARGO_MANIFEST_DIR"))
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(file).expect("read source");
        for (line_no, line) in strip_test_items(&text) {
            if line.trim_start().starts_with("//") || !CREATORS.iter().any(|c| line.contains(c)) {
                continue;
            }
            if line.contains("with_inheritance_lock") {
                guarded += 1;
            } else {
                unguarded.entry(rel.clone()).or_default().push(line_no);
            }
        }
    }

    assert!(
        unguarded.is_empty(),
        "these create a process, a PTY or a pipe descriptor outside \
         `procguard::with_inheritance_lock`, so they can be handed — or hand \
         away — a descriptor that is not yet `FD_CLOEXEC`: {unguarded:?}",
    );
    // A scan that found nothing to check must not read like a scan that found
    // everything guarded.
    assert_eq!(
        guarded, 3,
        "expected the PTY's `openpty` and `spawn_command` and procguard's wake \
         pipe, and nothing else; found {guarded} guarded creation sites"
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
