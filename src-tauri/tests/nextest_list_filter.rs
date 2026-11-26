//! libtest's name-filter rule as the `harness = false` native targets apply it
//! (`common/nextest_list.rs`). Those targets have no harness to run unit tests
//! in, so the rule is tested here.

mod common;

#[allow(
    dead_code,
    reason = "answer_list reads the process arguments; only the rule is tested here"
)]
#[path = "common/nextest_list.rs"]
mod nextest_list;

use nextest_list::excluded_by_filter;

const NAME: &str = "native_quit_guard";

fn excluded(args: &[&str]) -> bool {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    excluded_by_filter(NAME, &args)
}

#[test]
fn no_filter_runs() {
    assert!(!excluded(&[]));
    assert!(!excluded(&["--test-threads=1", "--nocapture"]));
}

#[test]
fn a_filter_for_another_test_skips_it() {
    // What `cargo test -- insights::live_sessions --test-threads=1` hands every binary.
    assert!(excluded(&["insights::live_sessions", "--test-threads=1"]));
    assert!(excluded(&["--test-threads", "1", "engine::provenance"]));
}

#[test]
fn a_filter_naming_it_runs_it() {
    assert!(!excluded(&["native_quit"]));
    assert!(!excluded(&["insights", "quit_guard"]));
}

#[test]
fn nextest_runs_it_by_exact_name() {
    assert!(!excluded(&[NAME, "--nocapture", "--exact"]));
    assert!(excluded(&["native_quit", "--exact"]));
}

#[test]
fn an_option_value_is_not_read_as_a_filter() {
    // `1` and `json` are values, not filters.
    assert!(!excluded(&["--test-threads", "1"]));
    assert!(!excluded(&["--format", "json", "-Z", "unstable-options"]));
}

#[test]
fn skip_excludes_by_substring_or_exact_name() {
    assert!(excluded(&["--skip", "quit"]));
    assert!(excluded(&["--skip=native_quit_guard", "--exact"]));
    assert!(!excluded(&["--skip", "quit", "--exact"]));
    assert!(!excluded(&["--skip", "browser"]));
}

#[test]
fn an_unknown_option_runs() {
    // native_event_unwind re-executes itself with `--probe=<case>`.
    assert!(!excluded(&["--probe=window"]));
    assert!(!excluded(&["--new-libtest-option", "value"]));
    // A flag given a value is not one this recognises either.
    assert!(!excluded(&["--exact=yes", "other"]));
}

#[test]
fn a_dangling_skip_runs() {
    assert!(!excluded(&["--skip"]));
}
