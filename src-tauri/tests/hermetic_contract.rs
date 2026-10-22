//! The hermetic test environment is only as good as its coverage: a suite that
//! does not run in it reads the developer's git config, `~/.claude`, trust
//! store and `tools.json`, and a raw `set_var` is a process-global write nothing
//! restores. Both are properties of the *source*, so they are asserted over the
//! source, the way `tests/spawn_seam.rs` asserts the spawn seam.

mod common;

use std::path::{Path, PathBuf};

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

fn manifest(sub: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(sub)
}

/// Every integration crate links the library without `cfg(test)`, so none of
/// them gets the hermetic environment from the library. Each one has to bring
/// it: by declaring `mod common;` (which includes it) or by including
/// `test_support/env.rs` itself.
#[test]
fn every_integration_test_crate_runs_in_the_hermetic_environment() {
    let mut files = Vec::new();
    let tests = manifest("tests");
    for entry in std::fs::read_dir(&tests).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
    assert!(files.len() > 40, "found only {} test crates", files.len());
    let outside: Vec<String> = files
        .iter()
        .filter(|path| {
            let text = std::fs::read_to_string(path).unwrap();
            let declares_common = text
                .lines()
                .any(|line| line.trim() == "mod common;" || line.trim() == "pub mod common;");
            let includes_env = text.lines().any(|line| {
                line.trim().starts_with("#[path =") && line.contains("test_support/env.rs")
            });
            !declares_common && !includes_env
        })
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(
        outside.is_empty(),
        "these test crates neither declare `mod common;` nor include \
         `src/test_support/env.rs`, so they run against the developer's home: {outside:?}"
    );
}

/// `set_var` and `remove_var` write one slot shared by every thread, and a test
/// that fails between the write and its undo leaves the value for every later
/// test. `test_support/env.rs` owns the only sanctioned form (`bind_env`, which
/// serializes and restores on drop).
#[test]
fn no_test_mutates_the_environment_outside_the_env_owner() {
    let mut files = Vec::new();
    rust_files(&manifest("src"), &mut files);
    rust_files(&manifest("tests"), &mut files);
    let owner = manifest("src/test_support/env.rs");
    let this = manifest("tests/hermetic_contract.rs");
    let mut offenders = Vec::new();
    for path in files {
        if path == owner || path == this {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for (index, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            if code.contains("set_var(") || code.contains("remove_var(") {
                offenders.push(format!("{}:{}: {}", path.display(), index + 1, code.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "raw environment mutation outside src/test_support/env.rs; use \
         `env_serial()` + `bind_env(..).set(..)` so it is serialized and restored:\n{}",
        offenders.join("\n")
    );
}

/// The integration-test side of `src/test_support.rs::hermetic_canary`: the two
/// state directories the library exposes resolve under this process's root, and
/// git reads no configuration from outside it.
#[test]
fn integration_tests_resolve_state_under_the_per_process_root() {
    let _serial = common::env::env_serial();
    let root = common::env::hermetic::root();
    for (what, path) in [
        (
            "default_config_dir",
            gitpulse_lib::tool_config::default_config_dir(),
        ),
        (
            "default_cache_dir",
            gitpulse_lib::tool_config::default_cache_dir(),
        ),
    ] {
        let path = path.unwrap_or_else(|| panic!("{what} did not resolve"));
        assert!(
            path.starts_with(&root),
            "{what} resolved to {}, outside {}",
            path.display(),
            root.display()
        );
    }
    assert_eq!(std::env::var("GIT_CONFIG_NOSYSTEM").as_deref(), Ok("1"));
    assert!(
        PathBuf::from(std::env::var_os("GIT_CONFIG_GLOBAL").unwrap()).starts_with(&root),
        "GIT_CONFIG_GLOBAL points outside the hermetic root"
    );
}

/// Lines (1-based, with their text) where an elapsed time is compared against a
/// `Duration::from_*` that is not wrapped in `coverage_relaxed`.
///
/// A limit is the text after the `<` up to the first `,`, `;`, `{` or `=>`, which ends the
/// comparison inside `assert!(elapsed < Duration::from_secs(5), "..")`. It is
/// read across lines because rustfmt breaks a long assertion so the `Duration`
/// sits on the line after `elapsed <`; a per-line check missed every one of
/// those. Only a comparison counts: a benchmark that reports `{elapsed:?}` has
/// no `<` after `elapsed` and is not flagged. Comment lines are skipped.
fn bare_wall_clock_limits(text: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let code = line.trim_start();
        if code.starts_with("//") {
            continue;
        }
        let Some(limit_start) = elapsed_comparison_end(code) else {
            continue;
        };
        let mut limit = String::from(&code[limit_start..]);
        for next in lines.iter().skip(index + 1).take(2) {
            if limit_end(&limit).is_some() {
                break;
            }
            limit.push(' ');
            limit.push_str(next.trim());
        }
        let limit = &limit[..limit_end(&limit).unwrap_or(limit.len())];
        if limit.contains("Duration::from_") && !limit.contains("coverage_relaxed") {
            found.push((index + 1, code.to_string()));
        }
    }
    found
}

/// Where a limit's text ends: the `,` that precedes an assertion message, the
/// `;` of a statement, the `{` of an `if`/`while` body, or a match arm's `=>`.
fn limit_end(text: &str) -> Option<usize> {
    text.find([',', ';', '{'])
        .into_iter()
        .chain(text.find("=>"))
        .min()
}

/// Byte offset just past the `<` of an `elapsed < ..` / `.elapsed() < ..` /
/// `x.elapsed < ..` comparison on this line, if there is one.
fn elapsed_comparison_end(code: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(found) = code[from..].find("elapsed") {
        let after = from + found + "elapsed".len();
        let rest = code[after..].trim_start_matches("()").trim_start();
        // `<` and `<=` compare; `<<` is a shift and never follows `elapsed`
        // in a limit.
        if let Some(tail) = rest.strip_prefix('<') {
            if !tail.starts_with('<') {
                return Some(code.len() - tail.len());
            }
        }
        from = after;
    }
    None
}

/// A wall-clock limit asserted as a bare `Duration::from_millis(400)` is a limit
/// nobody scaled for an instrumented or loaded runner, and when it includes a
/// process spawn it fails on contention rather than on a bug. Every such
/// assertion goes through `coverage_relaxed`, the one helper, so the scaling
/// rule lives in one place.
#[test]
fn no_wall_clock_assertion_bypasses_the_scaling_helper() {
    let mut files = Vec::new();
    rust_files(&manifest("src"), &mut files);
    rust_files(&manifest("tests"), &mut files);
    let owner = manifest("src/test_support/env.rs");
    let this = manifest("tests/hermetic_contract.rs");
    let bin = manifest("src/bin");
    let mut offenders = Vec::new();
    for path in files {
        // `src/bin/*` are separate crates with no access to the helper.
        if path == owner || path == this || path.starts_with(&bin) {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for (line, code) in bare_wall_clock_limits(&text) {
            offenders.push(format!("{}:{}: {}", path.display(), line, code));
        }
    }
    assert!(
        offenders.is_empty(),
        "wall-clock limits must be written `coverage_relaxed(Duration::from_..)`:\n{}",
        offenders.join("\n")
    );
}

/// The guard above is only worth having if it sees the shapes the suite really
/// contains, so its matcher is pinned on them directly.
#[test]
fn the_wall_clock_matcher_sees_single_line_and_wrapped_limits_only() {
    let flagged = |source: &str| bare_wall_clock_limits(source).len();
    // Single line, field or binding, either spelling of the elapsed time.
    assert_eq!(
        flagged("assert!(elapsed < Duration::from_secs(10), \"x\");"),
        1
    );
    assert_eq!(
        flagged("assert!(t.elapsed() < Duration::from_millis(5));"),
        1
    );
    assert_eq!(
        flagged("assert!(answer.elapsed < Duration::from_secs(10));"),
        1
    );
    // The rustfmt-wrapped form: the limit is on the next line.
    assert_eq!(
        flagged("assert!(\n    elapsed < std::time::Duration::from_secs(5),\n    \"slow\"\n);"),
        1
    );
    assert_eq!(
        flagged("assert!(\n    elapsed <\n        Duration::from_secs(5),\n    \"slow\"\n);"),
        1
    );
    assert_eq!(flagged("assert!(elapsed <= Duration::from_secs(5));"), 1);
    // Routed through the helper, in either layout.
    assert_eq!(
        flagged("assert!(elapsed < coverage_relaxed(Duration::from_secs(5)));"),
        0
    );
    assert_eq!(
        flagged(
            "assert!(\n    elapsed < coverage_relaxed(Duration::from_secs(5)),\n    \"slow\"\n);"
        ),
        0
    );
    // Not assertions: a deadline loop and a match guard compare against a variable.
    assert_eq!(
        flagged("while t.elapsed() < timeout {\n    let d = Duration::from_secs(1);\n}"),
        0
    );
    assert_eq!(
        flagged(
            "Ok(None) if started.elapsed() < timeout => {\n    sleep(Duration::from_millis(5));\n}"
        ),
        0
    );
    // Not limits: a reported time, a non-Duration bound, a Duration in the message,
    // and a comment.
    assert_eq!(flagged("println!(\"took {elapsed:?}\");"), 0);
    assert_eq!(
        flagged("assert!(elapsed < budget, \"over {:?}\", Duration::from_secs(1));"),
        0
    );
    assert_eq!(
        flagged("let elapsed = started.elapsed();\nlet wait = Duration::from_secs(5);"),
        0
    );
    assert_eq!(flagged("// assert!(elapsed < Duration::from_secs(5));"), 0);
}

/// Commits `count` empty commits dated `epoch` in a fresh repository whose only
/// global git config is the file at `global`, and returns how many entries
/// `HEAD`'s reflog holds afterwards.
fn reflog_entries_after_commits(global: &Path, epoch: u64, count: usize) -> usize {
    use gitpulse_lib::procguard::LockedSpawn;
    gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();
    let dir = tempfile::TempDir::new().unwrap();
    let date = format!("{epoch} +0000");
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .env("GIT_CONFIG_GLOBAL", global)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Test User")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test User")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .output_locked()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    git(&["init", "-q", "-b", "main"]);
    for i in 0..count {
        git(&["commit", "-q", "--allow-empty", "-m", &format!("c{i}")]);
    }
    git(&["reflog", "show", "--format=%H", "HEAD"])
        .lines()
        .count()
}

/// Git 2.54's `commit` runs `maintenance run --auto`, which expires reflog
/// entries older than the expiry once 100 of them have aged out, so a fixture
/// dated at a fixed epoch loses its history the day the epoch turns old. The
/// fixture here is dated a year before the epoch the suites use and carries
/// more than 100 entries, so it is past every expiry git has.
#[test]
fn a_git_config_written_through_the_helper_keeps_a_year_old_reflog_whole() {
    const A_YEAR: u64 = 365 * 24 * 60 * 60;
    const FIXTURE_EPOCH: u64 = 1_790_000_000;
    const COMMITS: usize = 130;
    let scratch = tempfile::TempDir::new().unwrap();
    let global = scratch.path().join("gitconfig");
    common::env::write_git_config(&global, "").unwrap();
    assert_eq!(
        reflog_entries_after_commits(&global, FIXTURE_EPOCH - A_YEAR, COMMITS),
        COMMITS,
        "auto-maintenance expired reflog entries of a fixture config written through the helper"
    );
}

/// The application's own git runs with `GIT_CONFIG_GLOBAL` stripped and reads
/// `$HOME/.gitconfig`, so the hermetic home has to carry the same pin.
#[test]
fn the_hermetic_home_pins_git_maintenance_for_the_applications_own_git() {
    for path in [
        common::env::hermetic::pinned_git_config(),
        PathBuf::from(std::env::var_os("HOME").unwrap()).join(".gitconfig"),
    ] {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            text.contains(common::env::GIT_MAINTENANCE_PIN),
            "{} does not pin git maintenance",
            path.display()
        );
    }
}

/// A test that points `GIT_CONFIG_GLOBAL` at a config of its own (`/dev/null`,
/// an empty file) drops the hermetic config and with it the maintenance pin.
/// Every file that assigns the variable goes through the helper.
#[test]
fn every_test_that_assigns_git_config_global_goes_through_the_pinning_helper() {
    let mut files = Vec::new();
    rust_files(&manifest("src"), &mut files);
    rust_files(&manifest("tests"), &mut files);
    let mut offenders = Vec::new();
    for file in files {
        if file.ends_with("test_support/env.rs") || file.ends_with("tests/hermetic_contract.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        if assigns_git_config_global(&text)
            && !text.contains("pinned_git_config")
            && !text.contains("write_git_config")
        {
            offenders.push(file.display().to_string());
        }
    }
    assert!(
        offenders.is_empty(),
        "these files assign GIT_CONFIG_GLOBAL without pinned_git_config()/write_git_config(): {offenders:?}"
    );
}

/// Whether the text assigns `GIT_CONFIG_GLOBAL` (as an `.env(..)`, `.set(..)` or
/// `set_var(..)` argument, possibly broken onto the next line by rustfmt), as
/// opposed to reading it or listing it among variables to strip.
fn assigns_git_config_global(text: &str) -> bool {
    text.match_indices("\"GIT_CONFIG_GLOBAL\"").any(|(at, _)| {
        let before = text[..at].trim_end();
        before.ends_with(".env(") || before.ends_with(".set(") || before.ends_with("set_var(")
    })
}
