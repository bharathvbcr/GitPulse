//! GitPulse writes no repository configuration, with one narrow, owner-approved
//! exception: the user-clicked, previewed, gated tuning apply.
//!
//! Owner decision (ft-7450105d, 2026-10-12): `core.untrackedCache` and
//! `core.fsmonitor` were the user's to enable, and nothing in GitPulse wrote
//! repo config:
//!
//! - every git child runs with `GIT_OPTIONAL_LOCKS=0`, which stops `git status`
//!   from refreshing the index, and the untracked cache is a feature of the
//!   index, so `-c core.untrackedCache=true` could neither help nor persist;
//! - `-c core.fsmonitor=true` starts a long-lived `git fsmonitor--daemon` per
//!   repository that nothing in GitPulse owns, supervises or stops, and that
//!   outlives the application.
//!
//! Owner decision (ft-6b68be32, 2026-11-06), "apply recommended git tuning too,
//! approved": GitPulse may APPLY recommended tuning, but only through a
//! user-clicked, previewed action behind the command gate. That reverses the
//! rule for that action alone, and this test is narrowed to exactly it, not
//! removed:
//!
//! - the one file that may build a config-writing argv, or name
//!   `feature.manyFiles`, is `storage/repo_tuning/advice.rs`, and the one file
//!   besides it that may name the tuning keys (to read them) is
//!   `storage/repo_tuning.rs`;
//! - in that file every `git config` argv is `git config --local <key> true`
//!   for `feature.manyFiles` or `core.fsmonitor` (never `core.splitIndex`,
//!   never `--global` or `--system`, never another value) or the undo
//!   `git config --local --unset <key>`; the only other commands are
//!   `git maintenance start|unregister` and `git fsmonitor--daemon stop`;
//! - every other production file still builds no `git config` write and sets
//!   neither key to anything but `false` (`storage/mod.rs` passes
//!   `core.fsmonitor=false` so a read cannot start the daemon).
//!
//! The watcher already treats the daemon's cookie writes as noise when the user
//! (or the apply) has enabled it. These properties are of the *source*, so they
//! are asserted over the source (the way `tests/spawn_seam.rs` asserts the spawn
//! seam). Reachability of the apply is pinned by `tests/tuning_apply_reach.rs`.
//!
//! One key is allowed (owner, 2026-11-03): `extensions.worktreeConfig = true`,
//! and only as git's own side effect. A sparse worktree needs its cone kept per
//! worktree, and `git sparse-checkout init` in a linked worktree writes that key
//! to the shared config itself; GitPulse still builds no `git config` write. The
//! engine test `a_sparse_creation_changes_the_shared_config_by_worktree_config_only`
//! holds the shared config to exactly that one addition.

mod common;

use std::path::{Path, PathBuf};

const KEYS: [&str; 3] = ["core.untrackedcache", "core.fsmonitor", "feature.manyfiles"];

/// The file that builds the apply's argv, and the one that reads the keys.
const TUNING_ARGV_FILE: &str = "storage/repo_tuning/advice.rs";
const TUNING_READ_FILE: &str = "storage/repo_tuning.rs";

/// The keys the apply may write, and the two `git maintenance start` writes
/// that its undo removes.
const WRITE_KEYS: [&str; 2] = ["feature.manyFiles", "core.fsmonitor"];
const UNSET_KEYS: [&str; 4] = [
    "feature.manyFiles",
    "core.fsmonitor",
    "maintenance.auto",
    "maintenance.strategy",
];

/// Read-only forms of `git config`, old flags and the `get` / `list`
/// subcommands. Anything else after `"config"` writes. (`npm config get cache`
/// shares the shape and is a read too.)
const READ_FLAGS: [&str; 5] = ["--get", "--list", "\"-l\"", "\"get\"", "\"list\""];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn is_test_file(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name == "tests.rs"
        || name.ends_with("_tests.rs")
        || name == "test_support.rs"
        || rel.starts_with("test_support/")
        || rel.contains("/tests/")
        || rel.starts_with("tests/")
        || rel == "alerts/stress.rs"
        || rel == "watcher/registration_timing.rs"
}

/// The production part of a file: up to its test module, without comment lines.
fn production_text(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("mod ") && trimmed.contains("test") {
            break;
        }
        if trimmed.starts_with("//") {
            out.push('\n');
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn line_of(text: &str, byte: usize) -> usize {
    text[..byte].bytes().filter(|b| *b == b'\n').count() + 1
}

/// What in `production` breaks the posture, one message per finding.
fn violations(production: &str) -> Vec<String> {
    let lower = production.to_ascii_lowercase();
    let mut found = Vec::new();

    for key in KEYS {
        for (at, _) in lower.match_indices(key) {
            if !lower[at + key.len()..].starts_with("=false") {
                found.push(format!(
                    "line {}: `{key}` is set to something other than `=false`",
                    line_of(production, at)
                ));
            }
        }
    }

    for needle in ["\"config\",", "\"config\")"] {
        for (at, _) in production.match_indices(needle) {
            let rest = &production[at + needle.len()..];
            let before = production[..at].trim_end();
            // `config` as an argv: the first element of an array
            // (`["config", ..]`) or `.arg("config")`. A path segment
            // (`.join("config")`), a lookup (`.get("config")`) and a table
            // row (`("config", Kind)`) are not.
            let is_argv = if needle.ends_with(',') {
                // `["config", "config.worktree"]` lists the git-dir files the
                // watcher classifies; it is a list of file names.
                before.ends_with('[') && !rest.trim_start().starts_with("\"config.worktree\"")
            } else {
                before.ends_with(".arg(")
            };
            if !is_argv {
                continue;
            }
            let end = rest.find([']', ';']).unwrap_or(rest.len());
            let span = &rest[..end];
            if !READ_FLAGS.iter().any(|flag| span.contains(flag)) {
                found.push(format!(
                    "line {}: `git config` argv that is not a read (--get*, --list)",
                    line_of(production, at)
                ));
            }
        }
    }
    found
}

/// The string literals of every `strings(&[...])` argv in `production`.
fn argv_literals(production: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for (at, _) in production.match_indices("strings(&[") {
        let rest = &production[at + "strings(&[".len()..];
        let end = rest.find("])").unwrap_or(rest.len());
        let tokens: Vec<String> = rest[..end]
            .split('"')
            .enumerate()
            .filter_map(|(i, part)| (i % 2 == 1).then(|| part.to_string()))
            .collect();
        out.push(tokens);
    }
    out
}

/// What in the tuning module's `production` text breaks the narrowed posture.
fn tuning_violations(production: &str) -> Vec<String> {
    let mut found = Vec::new();
    for flag in [
        "\"--global\"",
        "\"--system\"",
        "\"--add\"",
        "\"--replace-all\"",
        "\"--unset-all\"",
        "\"--file\"",
        "\"-f\"",
    ] {
        if production.contains(flag) {
            found.push(format!("names the forbidden flag {flag}"));
        }
    }
    for argv in argv_literals(production) {
        let words: Vec<&str> = argv.iter().map(String::as_str).collect();
        match words.as_slice() {
            ["git", "config", "--local", key, "true"] if WRITE_KEYS.contains(key) => {}
            ["git", "config", "--local", "--unset", key] if UNSET_KEYS.contains(key) => {}
            ["git", "maintenance", "start" | "unregister"] => {}
            ["git", "fsmonitor--daemon", "stop"] => {}
            other => found.push(format!("argv outside the narrowed set: {other:?}")),
        }
    }
    // `git config` must not be built any other way in this file.
    for needle in ["\"config\",", "\"config\")"] {
        for (at, _) in production.match_indices(needle) {
            let before = production[..at].trim_end();
            let window_start = production[..at]
                .char_indices()
                .rev()
                .nth(80)
                .map_or(0, |(i, _)| i);
            let in_strings = production[window_start..at].contains("strings(&[");
            let as_argv =
                before.ends_with('[') || before.ends_with(".arg(") || before.ends_with("\"git\",");
            if as_argv && !in_strings {
                found.push(format!(
                    "line {}: a `config` argv not built by strings(&[..])",
                    line_of(production, at)
                ));
            }
        }
    }
    found
}

#[test]
fn no_production_code_writes_repo_config_outside_the_tuning_apply() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(files.len() > 100, "found only {} source files", files.len());

    let mut scanned = 0;
    let mut report = Vec::new();
    for path in files {
        let rel = path
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if is_test_file(&rel) {
            continue;
        }
        scanned += 1;
        let text = std::fs::read_to_string(&path).expect("read source");
        let production = production_text(&text);
        if rel == TUNING_ARGV_FILE {
            // The owner-approved apply: the one file that builds its argv.
            for finding in tuning_violations(&production) {
                report.push(format!("{rel}: {finding}"));
            }
            continue;
        }
        if rel == TUNING_READ_FILE {
            // Names the keys to READ them from git's config list. It builds no
            // argv at all.
            for finding in violations(&production)
                .into_iter()
                .filter(|f| !f.contains("is set to something other than `=false`"))
            {
                report.push(format!("{rel}: {finding}"));
            }
            continue;
        }
        for finding in violations(&production) {
            report.push(format!("{rel}: {finding}"));
        }
    }
    assert!(scanned > 100, "scanned only {scanned} production files");
    assert!(
        report.is_empty(),
        "GitPulse must not write repo config or set core.untrackedCache / core.fsmonitor / \
         feature.manyFiles outside the owner-approved tuning apply (ft-7450105d, ft-6b68be32):\n{}",
        report.join("\n")
    );
}

/// The scanner is only worth its green if it goes red on the real shapes.
#[test]
fn the_scan_flags_each_way_of_setting_these_keys_and_writing_config() {
    let flagged = [
        r#"git(&["-c", "core.untrackedCache=true", "status"]);"#,
        r#"git(&["-c", "core.fsmonitor=true", "status"]);"#,
        r#"git(&["-c", "core.fsmonitor=/hook/path", "status"]);"#,
        r#"git(&["config", "core.untrackedCache", "true"]);"#,
        r#"cmd.arg("-c").arg("core.fsmonitor").arg("true");"#,
        r#"git(&["config", "user.name", "x"]);"#,
        r#"git(&["config", "--unset", "core.hooksPath"]);"#,
        r#"git(&["config", "--global", "core.editor", "vim"]);"#,
        "git(&[\n    \"config\",\n    \"--add\",\n    \"remote.origin.fetch\",\n    spec,\n]);",
        r#"cmd.arg("config").arg("alias.x").arg("!sh");"#,
    ];
    for source in flagged {
        assert!(!violations(source).is_empty(), "not flagged: {source}");
    }
}

#[test]
fn the_scan_allows_the_reads_the_app_makes_and_the_off_switch() {
    for source in [
        r#"git_text(repo, &["config", "--get-regexp", "-z", "^remote\\..*"]);"#,
        r#"git_text(repo, &["config", "-f", ".gitmodules", "--list", "-z"]);"#,
        "git_text(\n    repo,\n    &[\n        \"config\",\n        \"--get\",\n        \"remote.origin.url\",\n    ],\n);",
        r#"let mut argv = vec!["-c", "core.fsmonitor=false", "ls-files", "-z"];"#,
        r#"Some("config" | "config.worktree") => ChangeKind::Config,"#,
        r#"("config", ChangeKind::Config),"#,
        "/// `-c core.fsmonitor=true` starts a daemon\n// core.untrackedCache=true\nfn f() {}",
    ] {
        let production = production_text(source);
        assert_eq!(violations(&production), Vec::<String>::new(), "{source}");
    }
}

#[test]
fn the_scan_stops_at_the_test_module() {
    let text = "fn real() {}\n#[cfg(test)]\nmod tests {\n    fn t() { git(&[\"config\", \"core.fsmonitor\", \"true\"]); }\n}\n";
    assert!(violations(&production_text(text)).is_empty());
}

/// The narrowed check is only worth its green if it goes red on every way of
/// widening the apply.
#[test]
fn the_tuning_check_flags_each_way_of_widening_the_apply() {
    let flagged = [
        r#"strings(&["git", "config", "--local", "core.splitIndex", "true"])"#,
        r#"strings(&["git", "config", "--global", "feature.manyFiles", "true"])"#,
        r#"strings(&["git", "config", "--local", "feature.manyFiles", "false"])"#,
        r#"strings(&["git", "config", "--local", "core.fsmonitor", "/evil/hook"])"#,
        r#"strings(&["git", "config", "--local", "core.hooksPath", "/evil"])"#,
        r#"strings(&["git", "config", "--local", "--unset", "core.hooksPath"])"#,
        r#"strings(&["git", "maintenance", "stop"])"#,
        r#"strings(&["git", "config", "--local", "feature.manyFiles", "true", "x"])"#,
        r#"let args = ["--add"];"#,
        r#"git(&["config", "--local", "core.fsmonitor", "true"]);"#,
    ];
    for source in flagged {
        assert!(
            !tuning_violations(source).is_empty(),
            "not flagged: {source}"
        );
    }
}

#[test]
fn the_tuning_check_allows_exactly_the_apply_and_its_undo() {
    for source in [
        r#"strings(&["git", "config", "--local", "feature.manyFiles", "true"])"#,
        r#"strings(&["git", "config", "--local", "core.fsmonitor", "true"])"#,
        r#"strings(&["git", "maintenance", "start"])"#,
        r#"strings(&["git", "maintenance", "unregister"])"#,
        r#"strings(&["git", "fsmonitor--daemon", "stop"])"#,
        r#"strings(&["git", "config", "--local", "--unset", "maintenance.auto"])"#,
    ] {
        assert_eq!(tuning_violations(source), Vec::<String>::new(), "{source}");
    }
}

/// The real file, not a fixture: every key the apply writes is in the owner's
/// list, and `core.splitIndex` is never an argv element anywhere in it.
#[test]
fn the_real_tuning_file_passes_its_own_narrowed_check() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(TUNING_ARGV_FILE);
    let text = std::fs::read_to_string(&path).expect("tuning advice source");
    let production = production_text(&text);
    assert_eq!(tuning_violations(&production), Vec::<String>::new());
    let argvs = argv_literals(&production);
    assert!(argvs.len() >= 5, "found only {} argv builders", argvs.len());
    assert!(
        !argvs
            .iter()
            .flatten()
            .any(|w| w.to_ascii_lowercase().contains("splitindex")),
        "core.splitIndex must never be an argv element"
    );
}
