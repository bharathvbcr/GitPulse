//! The hermetic test environment is only as good as its coverage: a suite that
//! does not run in it reads the developer's git config, `~/.claude`, trust
//! store and `tools.json`, and a raw `set_var` is a process-global write nothing
//! restores. Both are properties of the *source*, so they are asserted over the
//! source, the way `tests/spawn_seam.rs` asserts the spawn seam.

mod common;
#[path = "common/sleep_scan.rs"]
mod sleep_scan;

use std::collections::{BTreeMap, BTreeSet};
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

// ---------------------------------------------------------------------------
// Short sleeps in test code
// ---------------------------------------------------------------------------

/// The reasons a sub-100 ms sleep may stay in test code, each with the rule that
/// makes it safe. The test for each is the same: would the assertion that follows
/// be MORE likely to fail if the sleep were ten times longer, as it is on a host
/// that stretches short sleeps? Only a sleep for which the answer is no may be
/// listed, and a sleep that paces a burst and then asserts a count or a time
/// belongs in `clock-seam-needed` until it is converted, never in another bucket.
const SLEEP_REASONS: &[(&str, &str)] = &[
    (
        "bounded-poll",
        "inside a loop that re-checks a condition until a deadline; the loop, not \
         the sleep, decides (checked: the sleep must sit in a loop)",
    ),
    (
        "unbounded-poll",
        "inside a loop that re-checks a flag or condition with no clock of its own \
         (a peer thread, accept loop or sampler that ends when told to; the test's \
         own join or timeout bounds it); checked: the sleep must sit in a loop",
    ),
    (
        "hold",
        "keeps a resource, lock or process busy for a concurrency test; the length \
         is not asserted and a longer hold only widens the overlap",
    ),
    (
        "mtime-gap",
        "separates two file or clock timestamps; a longer gap only helps",
    ),
    (
        "production-backoff-parity",
        "the fixture mirrors a production retry or backoff pause",
    ),
    (
        "race-widen",
        "gives another thread time to reach a state before the test proceeds; the \
         assertion holds whether or not it did, so a stretch cannot break it",
    ),
    (
        "linger",
        "waits a fixed time for something NOT to happen after the awaited event \
         (a duplicate, a stray write); a stretch only strengthens the evidence",
    ),
    (
        "spacing",
        "spaces independent operations or paces a stress loop; every assertion \
         is on invariants that hold for any spacing",
    ),
    (
        "fixture-delay",
        "a stub peer, child process or fake that answers late on purpose; the \
         delay is the input, not a wait",
    ),
    (
        "clock-seam-needed",
        "paces behaviour the assertion depends on, and the code under test has no \
         clock to inject; named debt, not a pass",
    ),
];

/// The committed list of sub-100 ms (or unprovable) sleeps in test code.
const SLEEP_ALLOWLIST: &str = include_str!("common/sleep_allowlist.txt");

struct SleepAllowlist {
    /// Anchor -> (reason, 1-based line in the list file).
    sites: BTreeMap<String, (String, usize)>,
    /// Path prefixes whose sleeps another lane owns, with that lane's reason.
    prefixes: Vec<(String, String)>,
    /// Malformed or duplicate lines.
    problems: Vec<String>,
}

/// Whether some file or directory of the crate has a path starting with
/// `prefix` (a text prefix, so `tests/adv_m1_` names a family of files).
fn path_prefix_matches_a_file(prefix: &str) -> bool {
    let full = manifest(prefix);
    full.exists()
        || full
            .parent()
            .and_then(|dir| std::fs::read_dir(dir).ok())
            .is_some_and(|mut entries| {
                entries.any(|entry| {
                    entry.is_ok_and(|e| {
                        e.path()
                            .to_string_lossy()
                            .starts_with(&*full.to_string_lossy())
                    })
                })
            })
}

fn reason_is_known(reason: &str) -> bool {
    SLEEP_REASONS.iter().any(|(name, _)| *name == reason)
        || reason
            .strip_prefix("owned-elsewhere:")
            .is_some_and(|owner| !owner.trim().is_empty())
}

/// `site | path | function | ordinal | reason` and `prefix | path-prefix |
/// owned-elsewhere:<owner>`; `#` starts a comment.
fn parse_sleep_allowlist(text: &str) -> SleepAllowlist {
    let mut list = SleepAllowlist {
        sites: BTreeMap::new(),
        prefixes: Vec::new(),
        problems: Vec::new(),
    };
    for (index, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let at = index + 1;
        let fields: Vec<&str> = line.split('|').map(str::trim).collect();
        match fields.as_slice() {
            ["site", file, func, ordinal, reason] => {
                let key = format!("{file}|{func}|{ordinal}");
                if ordinal.parse::<usize>().is_err() {
                    list.problems
                        .push(format!("line {at}: ordinal `{ordinal}` is not a number"));
                } else if !reason_is_known(reason) {
                    list.problems
                        .push(format!("line {at}: unknown reason `{reason}`"));
                } else if list
                    .sites
                    .insert(key.clone(), (reason.to_string(), at))
                    .is_some()
                {
                    list.problems
                        .push(format!("line {at}: `{key}` is listed twice"));
                }
            }
            ["prefix", prefix, reason] if reason.starts_with("owned-elsewhere:") => {
                if !reason_is_known(reason) {
                    list.problems
                        .push(format!("line {at}: unknown reason `{reason}`"));
                } else if !path_prefix_matches_a_file(prefix) {
                    list.problems
                        .push(format!("line {at}: nothing under `{prefix}`"));
                } else {
                    list.prefixes.push((prefix.to_string(), reason.to_string()));
                }
            }
            _ => list
                .problems
                .push(format!("line {at}: cannot read `{line}`")),
        }
    }
    list
}

/// Every sleep in the crate's test code (sources and integration tests), flagged
/// or not. This file and the scanner carry sleeps in snippets only.
fn test_code_sleeps() -> Vec<sleep_scan::Site> {
    let mut files = Vec::new();
    rust_files(&manifest("src"), &mut files);
    rust_files(&manifest("tests"), &mut files);
    let this = manifest("tests/hermetic_contract.rs");
    let scanner = manifest("tests/common/sleep_scan.rs");
    files.retain(|file| *file != this && *file != scanner);
    sleep_scan::scan(&manifest(""), &files)
}

/// A host that stretches a 10 ms sleep to tens of milliseconds turns every test
/// that paces behaviour or asserts timing around a short sleep red with no change
/// to the code. Every sub-100 ms sleep in test code, and every one whose length
/// cannot be proved, is therefore listed in `tests/common/sleep_allowlist.txt`
/// with the reason it cannot flake; a new one fails here until it is converted to
/// a condition wait (`test_support::wait_until`) or listed with an honest reason.
#[test]
fn every_short_sleep_in_test_code_is_listed_with_a_reason() {
    let list = parse_sleep_allowlist(SLEEP_ALLOWLIST);
    let sites = test_code_sleeps();
    assert!(
        sites.len() > 300,
        "the scan saw only {} sleeps in test code; it has stopped finding them",
        sites.len()
    );
    let unlisted: Vec<String> = sites
        .iter()
        .filter(|site| site.flagged())
        .filter(|site| {
            !list.sites.contains_key(&site.key())
                && !list
                    .prefixes
                    .iter()
                    .any(|(prefix, _)| site.file.starts_with(prefix))
        })
        .map(|site| {
            format!(
                "site | {} | {} | {} | <reason>    # line {}{}: sleep({})",
                site.file,
                site.func,
                site.ordinal,
                site.line,
                if site.in_loop { " (in a loop)" } else { "" },
                site.arg
            )
        })
        .collect();
    assert!(
        unlisted.is_empty(),
        "{} test sleep(s) under 100 ms (or of unprovable length) are not in \
         tests/common/sleep_allowlist.txt. Wait on the condition with \
         `test_support::wait_until`, or add a line with one of {:?}:\n{}",
        unlisted.len(),
        SLEEP_REASONS
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>(),
        unlisted.join("\n")
    );
}

/// An allowlist entry that no longer matches a short sleep is a list that has
/// stopped checking: a converted sleep must leave the list in the same change.
#[test]
fn the_sleep_allowlist_names_only_sleeps_that_exist_and_reasons_that_hold() {
    let list = parse_sleep_allowlist(SLEEP_ALLOWLIST);
    assert!(list.problems.is_empty(), "{:#?}", list.problems);
    let sites: BTreeMap<String, sleep_scan::Site> = test_code_sleeps()
        .into_iter()
        .map(|site| (site.key(), site))
        .collect();
    let mut stale = Vec::new();
    let mut not_polls = Vec::new();
    for (key, (reason, line)) in &list.sites {
        match sites.get(key) {
            Some(site) if site.flagged() => {
                if matches!(reason.as_str(), "bounded-poll" | "unbounded-poll") && !site.in_loop {
                    not_polls.push(format!("line {line}: {key} is not inside a loop"));
                }
            }
            _ => stale.push(format!("line {line}: {key}")),
        }
    }
    assert!(
        stale.is_empty(),
        "these allowlist entries match no short sleep (converted, moved or renamed); \
         remove them:\n{}",
        stale.join("\n")
    );
    assert!(
        not_polls.is_empty(),
        "`bounded-poll` and `unbounded-poll` are for a sleep inside a loop that re-checks \
         a condition:\n{}",
        not_polls.join("\n")
    );
}

/// The scan is only worth having if it sees every spelling the suite contains
/// and refuses what it cannot judge, so its matcher is pinned on them directly.
#[test]
fn the_sleep_scanner_sees_every_spelling_and_fails_closed() {
    use sleep_scan::scan_snippet;
    let flagged = |source: &str| {
        scan_snippet(source)
            .into_iter()
            .filter(|site| site.flagged())
            .count()
    };
    // Spellings of a short sleep.
    for short in [
        "fn t() { thread::sleep(Duration::from_millis(5)); }",
        "fn t() { std::thread::sleep(std::time::Duration::from_millis(99)); }",
        "fn t() { sleep(Duration::from_micros(500)); }",
        "fn t() { sleep(Duration::from_nanos(1)); }",
        "fn t() { sleep(Duration::from_secs_f64(0.05)); }",
        "fn t() { sleep(Duration::from_secs_f32(0.099)); }",
        "async fn t() { tokio::time::sleep(Duration::from_millis(10)).await; }",
        "fn t() { clock.sleep(Duration::from_millis(10)); }",
        "fn t() { sleep(Duration::from_millis(1_0)); }",
        "fn t() { sleep(coverage_relaxed(Duration::from_millis(5))); }",
        "fn t() { sleep(Duration::from_millis(30) * 2); }",
        "fn t() { sleep(Duration::from_millis(40) + Duration::from_millis(40)); }",
        "fn t() { sleep(Duration::ZERO); }",
    ] {
        assert_eq!(flagged(short), 1, "{short}");
    }
    // Names resolve within the file, and the value decides.
    assert_eq!(
        flagged("const SETTLE: Duration = Duration::from_millis(20);\nfn t() { sleep(SETTLE); }"),
        1
    );
    assert_eq!(
        flagged("const POLL_MS: u64 = 5;\nfn t() { sleep(Duration::from_millis(POLL_MS)); }"),
        1
    );
    assert_eq!(
        flagged("fn t() { let nap = Duration::from_millis(7); sleep(nap); }"),
        1
    );
    assert_eq!(
        flagged("const SLOW: Duration = Duration::from_millis(250);\nfn t() { sleep(SLOW); }"),
        0
    );
    // A name defined twice with different values cannot be judged.
    assert_eq!(
        flagged(
            "fn a() { let nap = Duration::from_millis(500); }\n\
             fn b() { let nap = Duration::from_millis(5); sleep(nap); }"
        ),
        1
    );
    // Unprovable lengths fail closed.
    for unknown in [
        "fn t(hold: Duration) { sleep(hold); }",
        "fn t(ms: u64) { sleep(Duration::from_millis(ms)); }",
        "fn t() { sleep(POLL.min(left)); }",
        "fn t() { sleep(Duration::from_millis(rng.next() % 26)); }",
    ] {
        assert_eq!(flagged(unknown), 1, "{unknown}");
        assert!(scan_snippet(unknown)[0].nanos.is_none(), "{unknown}");
    }
    // Long enough, or not a sleep at all.
    for fine in [
        "fn t() { sleep(Duration::from_millis(100)); }",
        "fn t() { sleep(Duration::from_secs(1)); }",
        "fn t() { sleep(Duration::from_secs_f64(0.1)); }",
        "fn t() { sleep(crate::test_support::coverage_relaxed(Duration::from_millis(500))); }",
        "fn t() { sleep(Duration::from_millis(60) + Duration::from_millis(60)); }",
        "fn sleep(pid: i32) -> bool { pid > 0 }",
        "fn t() { sleeper(\"5\"); let sleepers = 1; }",
        "fn t() { // sleep(Duration::from_millis(5))\n}",
        "fn t() { let s = \"sleep(Duration::from_millis(5))\"; }",
        "fn t() { let s = r#\"sleep(Duration::from_millis(5))\"#; }",
        "fn t() { /* sleep(Duration::from_millis(5)) */ }",
    ] {
        assert_eq!(flagged(fine), 0, "{fine}");
    }
    // The anchor counts every sleep of a function, whatever its length, and
    // qualifies a method by its impl.
    let sites = scan_snippet(
        "fn a() { sleep(Duration::from_secs(1)); sleep(Duration::from_millis(5)); }\n\
         impl Drop for Guard { fn drop(&mut self) { sleep(Duration::from_millis(5)); } }\n\
         impl<T> Waiter<T> where T: Send { fn drop(&mut self) { sleep(Duration::from_millis(5)); } }",
    );
    let keys: Vec<String> = sites.iter().map(sleep_scan::Site::key).collect();
    assert_eq!(
        keys,
        [
            "snippet.rs|a|0",
            "snippet.rs|a|1",
            "snippet.rs|Guard::drop|0",
            "snippet.rs|Waiter::drop|0"
        ]
    );
    // `impl Trait` in a parameter list is not an impl block.
    let helper = scan_snippet("fn wait_for(mut done: impl FnMut() -> bool) { sleep(D); }");
    assert_eq!(helper[0].key(), "snippet.rs|wait_for|0");
    // Loops: a poll is in one, a plain wait and a Drop impl are not.
    let in_loop = |source: &str| scan_snippet(source)[0].in_loop;
    assert!(in_loop("fn t() { while !done() { sleep(D); } }"));
    assert!(in_loop("fn t() { for _ in 0..9 { sleep(D); } }"));
    assert!(in_loop("fn t() { let s = loop { sleep(D); }; }"));
    assert!(in_loop("fn t() { 'outer: loop { if x { sleep(D); } } }"));
    assert!(in_loop(
        "fn t() { while unsafe { probe() } != pid && now() < deadline { sleep(D); } }"
    ));
    assert!(in_loop(
        "fn t() { let s = (0..100).find_map(|_| { sleep(D); probe() }); }"
    ));
    assert!(!in_loop("fn t() { sleep(D); }"));
    assert!(!in_loop(
        "impl Drop for G { fn drop(&mut self) { sleep(D); } }"
    ));
    assert!(!in_loop("fn t() { v.for_each(|x| { sleep(D); }); }"));
}

/// What is test code decides what is scanned, so the boundary is pinned.
#[test]
fn the_sleep_scanner_scans_test_code_and_only_test_code() {
    let root = tempfile::TempDir::new().unwrap();
    let write = |rel: &str, text: &str| {
        let path = root.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    };
    let nap = "fn nap() { std::thread::sleep(std::time::Duration::from_millis(5)); }\n";
    let files = vec![
        write("src/prod.rs", nap),
        write(
            "src/bad.rs",
            &format!("#[cfg(not(test))]\nmod m {{ {nap} }}\n"),
        ),
        write(
            "src/anyof.rs",
            &format!("#[cfg(any(test, unix))]\nmod m {{ {nap} }}\n"),
        ),
        write(
            "src/inline.rs",
            &format!("fn p() {{}}\n#[cfg(test)]\nmod tests {{ {nap} }}\n"),
        ),
        write(
            "src/allof.rs",
            &format!("#[cfg(all(test, unix))]\nmod tests {{ {nap} }}\n"),
        ),
        write(
            "src/test_fn.rs",
            &format!("#[test]\nfn t() {{ {nap} }}\nfn after() {{}}\n"),
        ),
        write("src/host.rs", "#[cfg(test)]\nmod adv;\nmod real;\n"),
        write("src/host/adv.rs", nap),
        write("src/host/real.rs", nap),
        write("src/named_tests.rs", nap),
        write("src/mod_host/mod.rs", "#[cfg(test)]\nmod inner;\n"),
        write("src/mod_host/inner.rs", nap),
        write("tests/integration.rs", nap),
        write("src/test_support/helper.rs", nap),
    ];
    let seen: BTreeSet<String> = sleep_scan::scan(root.path(), &files)
        .into_iter()
        .map(|site| site.file)
        .collect();
    let expected: BTreeSet<String> = [
        "src/inline.rs",
        "src/allof.rs",
        "src/test_fn.rs",
        "src/host/adv.rs",
        "src/named_tests.rs",
        "src/mod_host/inner.rs",
        "tests/integration.rs",
        "src/test_support/helper.rs",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(seen, expected);
    assert!(sleep_scan::cfg_requires_test("test"));
    assert!(sleep_scan::cfg_requires_test("all(unix, test)"));
    assert!(sleep_scan::cfg_requires_test("any(test, all(test, unix))"));
    assert!(!sleep_scan::cfg_requires_test("any(test, unix)"));
    assert!(!sleep_scan::cfg_requires_test("not(test)"));
    assert!(!sleep_scan::cfg_requires_test("all(testing, unix)"));
}

/// A `prefix | .. | owned-elsewhere:..` line allowlists EVERY short sleep ever
/// added under it. One that covers no short sleep today is not holding a
/// conversion in flight, it is a standing exemption for the next sleep someone
/// adds to that file, with no test left to refuse it.
#[test]
fn an_owned_elsewhere_prefix_covers_a_sleep_that_exists_today() {
    let list = parse_sleep_allowlist(SLEEP_ALLOWLIST);
    let sites = test_code_sleeps();
    let idle: Vec<&str> = list
        .prefixes
        .iter()
        .map(|(prefix, _)| prefix.as_str())
        .filter(|prefix| {
            !sites
                .iter()
                .any(|site| site.flagged() && site.file.starts_with(prefix))
        })
        .collect();
    assert!(
        idle.is_empty(),
        "these owned-elsewhere prefixes cover no short sleep, so they silently allow every \
         future one: {idle:?}"
    );
}

/// A duration name is judged by the one definition the file has. A parameter of
/// another function that happens to share the name of a literal-valued binding
/// elsewhere in the file is not that definition: its value is the caller's, so
/// the sleep is unprovable and must be flagged.
#[test]
fn a_parameter_sharing_a_name_with_a_long_binding_elsewhere_is_unprovable() {
    let sites = sleep_scan::scan_snippet(
        "fn a() { let nap = Duration::from_millis(500); sleep(nap); }\n\
         fn b(nap: Duration) { sleep(nap); }",
    );
    let in_b = sites.iter().find(|site| site.func == "b").unwrap();
    assert!(
        in_b.flagged(),
        "`sleep(nap)` with `nap` a parameter was judged {:?} ns from another function's binding",
        in_b.nanos
    );
}
