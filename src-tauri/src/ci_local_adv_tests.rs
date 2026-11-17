//! Adversarial tests for the "never record a pass for a check that did not
//! happen" change: runner summaries, cargo scope shapes, and the verdict.

use super::*;

fn out(stdout: &str, stderr: &str) -> CapturedOutput {
    CapturedOutput {
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
        success: true,
        status_code: 0,
    }
}

fn named(name: &str, program: &str) -> CiStep {
    CiStep {
        name: name.into(),
        program: program.into(),
        args: Vec::new(),
    }
}

fn judge(name: &str, program: &str, stdout: &str, stderr: &str) -> CiStepResult {
    step_result(&named(name, program), Ok(out(stdout, stderr)), 1)
}

fn affected_of(paths: &[&str]) -> crate::codeintel::CodeintelAffectedTests {
    let items: Vec<_> = paths
        .iter()
        .map(|path| crate::codeintel::CodeintelAffectedTest {
            path: (*path).into(),
            depth: 1,
            symbols: vec!["probe".into()],
            reached_symbols: 1,
        })
        .collect();
    let shown = u32::try_from(items.len()).unwrap_or(u32::MAX);
    crate::codeintel::CodeintelAffectedTests {
        available: true,
        reason: None,
        targets: vec!["src/lib.rs".into()],
        tests: crate::codeintel::CodeintelResponse::ok(items, shown, shown, false),
        blast_radius: crate::codeintel::CodeintelBlastRadius {
            seeds: vec!["src/lib.rs".into()],
            unmatched_targets: Vec::new(),
            layers: crate::codeintel::CodeintelResponse::ok(Vec::new(), 0, 0, false),
            total_impacted: 1,
        },
        fail_closed: false,
        fail_closed_reason: None,
    }
}

// ---------------------------------------------------------------- runners

/// jest prints its summary to STDERR; only stdout is read, so a real
/// passing jest run is recorded as "could not run". `npm test` is whatever the
/// repo's package.json says, so any non-vitest repo hits this.
#[test]
fn jest_summary_on_stderr_is_a_real_pass() {
    let r = judge(
        "Frontend unit tests",
        "npm",
        "",
        "Test Suites: 1 passed, 1 total\nTests:       5 passed, 5 total\nSnapshots:   0 total\n",
    );
    assert_eq!(r.status, "passed", "{}", r.detail);
}

/// mocha `  5 passing (10ms)`.
#[test]
fn mocha_passing_is_a_real_pass() {
    let r = judge("Frontend unit tests", "npm", "\n  5 passing (10ms)\n", "");
    assert_eq!(r.status, "passed", "{}", r.detail);
}

/// `node --test` TAP-ish spec summary.
#[test]
fn node_test_runner_is_a_real_pass() {
    let r = judge(
        "Frontend unit tests",
        "npm",
        "ℹ tests 5\nℹ suites 0\nℹ pass 5\nℹ fail 0\nℹ cancelled 0\nℹ skipped 0\n",
        "",
    );
    assert_eq!(r.status, "passed", "{}", r.detail);
}

/// `bun test`.
#[test]
fn bun_test_is_a_real_pass() {
    let r = judge(
        "Frontend unit tests",
        "npm",
        "",
        " 5 pass\n 0 fail\n 5 expect() calls\nRan 5 tests across 1 file. [12.00ms]\n",
    );
    assert_eq!(r.status, "passed", "{}", r.detail);
}

/// playwright list reporter summary has no "Tests" prefix.
#[test]
fn playwright_is_a_real_pass() {
    let r = judge("Frontend unit tests", "npm", "  5 passed (3.2s)\n", "");
    assert_eq!(r.status, "passed", "{}", r.detail);
}

/// pytest -v, every test skipped (exit 0). The summary
/// `1 skipped` has no passed/failed count, so `rfind` walks up to a verbose
/// line whose TEST NAME `test_3_passed` reads as "3 passed".
#[test]
fn pytest_all_skipped_with_digit_passed_in_a_test_name_is_not_a_pass() {
    let pytest = "tests/test_a.py::test_3_passed_things SKIPPED (reason)      [100%]\n\n=== 1 skipped in 0.01s ===\n";
    let r = judge("Python tests", "pytest", pytest, "");
    assert_eq!(r.status, "failed", "false pass: {}", r.detail);
}

/// `-rs` short summary quotes a skip reason containing
/// "2 passed"; the real summary `1 skipped` has no count.
#[test]
fn pytest_skip_reason_text_is_not_a_pass_count() {
    let pytest = "=========== short test summary info ===========\nSKIPPED [1] tests/test_a.py:5: needs 2 passed runs first\n=== 1 skipped in 0.01s ===\n";
    let r = judge("Python tests", "pytest", pytest, "");
    assert_eq!(r.status, "failed", "false pass: {}", r.detail);
}

/// xfail-only suites exit 0 and executed tests, but are read as
/// "no summary".
#[test]
fn pytest_xfail_only_is_a_real_run() {
    let r = judge("Python tests", "pytest", "=== 2 xfailed in 0.02s ===\n", "");
    assert_eq!(r.status, "passed", "{}", r.detail);
}

/// OSC 8 hyperlink wrapped round "Tests": `strip_ansi` ends an
/// escape at the first ASCII letter, so it stops inside the URL and then eats
/// the `T` after the ST terminator.
#[test]
fn osc8_hyperlink_does_not_corrupt_the_summary_line() {
    let line = "\u{1b}]8;;file:///a/b.ts\u{1b}\\Tests\u{1b}]8;;\u{1b}\\  4 passed (4)\n";
    assert_eq!(executed_test_count("Frontend unit tests", line), Some(4));
}

/// ESC 7 (save cursor) is a two-byte escape; `strip_ansi` keeps
/// eating until a letter and swallows the `T`.
#[test]
fn two_byte_escape_does_not_swallow_text() {
    let line = "\u{1b}7Tests  4 passed (4)\n";
    assert_eq!(executed_test_count("Frontend unit tests", line), Some(4));
}

/// BEL-terminated OSC title (npm/vite set titles on a tty).
#[test]
fn bel_terminated_osc_title_does_not_swallow_text() {
    let line = "\u{1b}]0;vitest run\u{7}Tests  4 passed (4)\n";
    assert_eq!(executed_test_count("Frontend unit tests", line), Some(4));
}

// ------------------------------------------------------------------ holds

#[test]
fn vitest_log_line_before_the_summary_does_not_win() {
    let text = "stdout | a.test.ts > t\nTests 3 passed\n\n Test Files  1 skipped (1)\n      Tests  3 skipped (3)\n";
    assert_eq!(executed_test_count("Frontend unit tests", text), Some(0));
    let r = judge("Frontend unit tests", "npm", text, "");
    assert_eq!(r.status, "failed");
}

#[test]
fn cargo_test_named_test_result_passed_and_warnings_are_not_summaries() {
    let text = "warning: 3 passed through here\ntest tests::test_result_passed ... ok\n";
    assert_eq!(executed_test_count("Rust tests", text), None);
    let r = judge("Rust tests", "cargo", text, "");
    assert_eq!(r.status, "failed");
}

#[test]
fn cargo_multi_binary_doctest_and_quiet_sum_up() {
    let text = "\nrunning 3 tests\n...\ntest result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\n   Doc-tests foo\n\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
    assert_eq!(
        executed_test_count("Rust tests (2 affected)", text),
        Some(3)
    );
    assert_eq!(judge("Rust tests", "cargo", text, "").status, "passed");
}

#[test]
fn cargo_color_codes_do_not_hide_the_summary() {
    let text = "test result: \u{1b}[32mok\u{1b}[0m. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n";
    assert_eq!(executed_test_count("Rust tests", text), Some(7));
}

#[test]
fn cargo_nextest_summary_is_unreadable_and_fails_closed() {
    let text = "     Summary [   0.5s] 5 tests run: 5 passed, 0 skipped\n";
    assert_eq!(executed_test_count("Rust tests", text), None);
}

#[test]
fn pytest_shapes_that_do_parse() {
    for (text, want) in [
        ("=== 5 passed, 2 warnings in 0.1s ===\n", Some(5)),
        ("5 passed in 0.12s\n", Some(5)),
        ("=== no tests ran in 0.01s ===\n", Some(0)),
        (
            "PASSED a.py::test_x\n=== 1 failed, 4 passed in 1s ===\n",
            Some(5),
        ),
    ] {
        assert_eq!(executed_test_count("Python tests", text), want, "{text}");
    }
}

#[test]
fn ansi_and_cr_framing_around_a_real_summary_still_parses() {
    let text = "progress\r\n\u{1b}[2K\r\u{1b}[1m\u{1b}[32m      Tests  4 passed\u{1b}[39m (4)\u{1b}[22m\r\n";
    assert_eq!(executed_test_count("Frontend unit tests", text), Some(4));
}

#[test]
fn a_huge_noisy_output_keeps_its_summary() {
    let mut text = "noise line with 9 passed words\n".repeat(200_000);
    text.push_str("test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n");
    assert_eq!(executed_test_count("Rust tests", &text), Some(2));
    // An unterminated escape at the very end must not panic or hang.
    text.push('\u{1b}');
    assert_eq!(executed_test_count("Rust tests", &text), Some(2));
}

#[test]
fn a_non_test_step_is_never_judged_by_a_summary() {
    assert_eq!(
        judge("Rust lint (clippy)", "cargo", "", "").status,
        "passed"
    );
    assert_eq!(judge("Frontend build", "npm", "", "").status, "passed");
}

// ------------------------------------------------------------- rust_scope

/// cargo test target names keep hyphens (`tests/my-test.rs` is
/// `--test my-test`; verified: `--test my_test` is "no test target named").
/// A real pass becomes a failed step.
#[test]
fn hyphenated_integration_target_keeps_its_hyphen() {
    let scope = rust_scope(&["tests/my-test.rs".to_string()]).unwrap();
    let args = rust_test_args("Cargo.toml", &scope);
    assert!(args.contains(&"my-test".to_string()), "{args:?}");
}

/// `tests/sub/x.rs` is not a cargo target; it becomes the
/// substring filter `x`, which runs whatever test path contains an `x` and not
/// the integration target that includes the file. Expected: fail closed.
#[test]
fn nested_tests_dir_file_is_not_a_unit_filter() {
    assert!(rust_scope(&["tests/sub/x.rs".to_string()]).is_err());
}

/// `benches/` and `examples/` files are not unit tests.
#[test]
fn benches_and_examples_are_not_unit_filters() {
    assert!(rust_scope(&["benches/b.rs".to_string()]).is_err());
    assert!(rust_scope(&["examples/e.rs".to_string()]).is_err());
}

/// `.rs` has file_stem `.rs`, which passes the empty-stem guard.
#[test]
fn dot_rs_has_no_module_name() {
    assert!(rust_scope(&["src/.rs".to_string()]).is_err());
    assert!(rust_scope(&["src/1a.rs".to_string()]).is_err());
    assert!(rust_scope(&["src/a b.rs".to_string()]).is_err());
}

/// A unit-test file pulled in with `#[path = "x_tests.rs"]` yields a filter that
/// matches no test (the module is named by its `mod` line, not the file). That
/// is not narrowed away silently: the zero-executed rule reports it as
/// could-not-run.
#[test]
fn a_path_attribute_test_file_filter_that_matches_nothing_is_could_not_run() {
    assert_eq!(
        rust_scope(&["src/ci_local_adv_tests.rs".to_string()]),
        Ok(RustScope::UnitFilter("ci_local_adv_tests".into()))
    );
    let nothing = "running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out; finished in 0.00s\n";
    let r = judge("Rust tests (1 affected)", "cargo", nothing, "");
    assert_eq!(r.status, "failed", "{}", r.detail);
    assert!(r.detail.starts_with(COULD_NOT_RUN), "{}", r.detail);
}

#[test]
fn rust_scope_normalisation_that_holds() {
    assert_eq!(
        rust_scope(&[
            "./tests/a.rs".to_string(),
            "src-tauri/tests/a.rs".to_string()
        ]),
        Ok(RustScope::Integration(vec!["a".into()]))
    );
    assert_eq!(
        rust_scope(&["src/a-b.rs".to_string()]),
        Ok(RustScope::UnitFilter("a_b".into()))
    );
    assert!(rust_scope(&["tests/a.rs".to_string(), "src/b.rs".to_string()]).is_err());
    assert!(rust_scope(&["src/b.rs".to_string(), "src/c.rs".to_string()]).is_err());
    assert!(rust_scope(&["src/x/mod.rs".to_string()]).is_err());
}

#[test]
fn backslash_paths_normalise_through_the_planner() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname=\"t\"\nversion=\"1.2.3\"\n",
    )
    .unwrap();
    let plan = plan_ci_from_affected(
        dir.path(),
        &["src\\bar.rs".into()],
        &affected_of(&["src\\bar.rs"]),
    );
    let rust = plan
        .steps
        .iter()
        .find(|s| s.name.starts_with("Rust tests"))
        .unwrap();
    assert_eq!(
        rust.args.last().map(String::as_str),
        Some("bar"),
        "{:?}",
        rust.args
    );
}

// ----------------------------------------------------------- verdict/scope

/// a report with no steps is vacuously "all passed".
#[test]
fn a_report_with_no_steps_is_not_a_pass() {
    let report = summarize(Vec::new(), 0, CiTestScope::default());
    assert_ne!(run_verdict(&report), "passed");
}

#[test]
fn an_empty_affected_list_never_yields_a_narrowed_scope() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}").unwrap();
    let plan = plan_ci_from_affected(dir.path(), &["a.ts".into()], &affected_of(&[]));
    assert!(plan.test_scope.fail_closed);
    assert!(!plan.test_scope.is_narrowed());
}

#[test]
fn a_frontend_only_narrowing_drops_the_rust_suite_and_is_still_narrowed() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}").unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname=\"t\"\nversion=\"1.2.3\"\n",
    )
    .unwrap();
    let plan = plan_ci_from_affected(
        dir.path(),
        &["src/a.ts".into()],
        &affected_of(&["src/a.test.ts"]),
    );
    assert!(plan.test_scope.is_narrowed());
    assert!(!plan.steps.iter().any(|s| s.name.starts_with("Rust tests")));
}

// ------------------------------------------------------------ real runners

/// Real output of `node --test` (node v26.13.0) on a three-test file.
const NODE_TEST_PASS: &str = include_str!("ci_local_fixtures/node_test_pass.txt");
/// Real stdout and stderr of `bun test` (bun 1.4.4) on a two-test file; bun
/// prints its summary to stderr.
const BUN_TEST_STDOUT: &str = include_str!("ci_local_fixtures/bun_test_pass_stdout.txt");
const BUN_TEST_STDERR: &str = include_str!("ci_local_fixtures/bun_test_pass_stderr.txt");

#[test]
fn real_node_and_bun_test_output_parses_to_the_executed_count() {
    assert_eq!(
        executed_test_count("Frontend unit tests", NODE_TEST_PASS),
        Some(3)
    );
    let r = judge(
        "Frontend unit tests",
        "npm",
        BUN_TEST_STDOUT,
        BUN_TEST_STDERR,
    );
    assert_eq!(r.status, "passed", "{}", r.detail);
    let combined = format!("{BUN_TEST_STDOUT}\n{BUN_TEST_STDERR}");
    assert_eq!(
        executed_test_count("Frontend unit tests", &combined),
        Some(2)
    );
}
