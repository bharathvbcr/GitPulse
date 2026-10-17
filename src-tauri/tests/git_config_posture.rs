//! GitPulse never writes a user's repository configuration, and never turns on
//! the git performance features that live in it.
//!
//! Owner decision (ft-7450105d, 2026-10-12). `core.untrackedCache` and
//! `core.fsmonitor` are the user's to enable:
//!
//! - every git child runs with `GIT_OPTIONAL_LOCKS=0`, which stops `git status`
//!   from refreshing the index, and the untracked cache is a feature of the
//!   index, so `-c core.untrackedCache=true` could neither help nor persist;
//! - `-c core.fsmonitor=true` starts a long-lived `git fsmonitor--daemon` per
//!   repository that nothing in GitPulse owns, supervises or stops, and that
//!   outlives the application.
//!
//! The watcher already treats the daemon's cookie writes as noise when the user
//! has enabled it themselves. These properties are of the *source*, so they are
//! asserted over the source (the way `tests/spawn_seam.rs` asserts the spawn
//! seam): no production file builds a `git config` write argv, and none sets
//! either key to anything but `false` (`storage/mod.rs` passes
//! `core.fsmonitor=false` so a read cannot start the daemon).

mod common;

use std::path::{Path, PathBuf};

const KEYS: [&str; 2] = ["core.untrackedcache", "core.fsmonitor"];

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

#[test]
fn no_production_code_writes_repo_config_or_enables_untracked_cache_or_fsmonitor() {
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
        for finding in violations(&production_text(&text)) {
            report.push(format!("{rel}: {finding}"));
        }
    }
    assert!(scanned > 100, "scanned only {scanned} production files");
    assert!(
        report.is_empty(),
        "GitPulse must not write repo config or set core.untrackedCache / core.fsmonitor \
         (owner decision ft-7450105d); the user enables those themselves:\n{}",
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
