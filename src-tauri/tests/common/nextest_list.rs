//! The listing half of cargo-nextest's custom-harness protocol, for a
//! `harness = false` target that runs as one test.
//!
//! nextest asks every test binary for its tests with `--list --format terse`
//! (all tests) and `--list --format terse --ignored` (the ignored ones), each
//! line `<name>: test` on stdout and nothing else there, and then runs each
//! test as `<name> --nocapture --exact`
//! (<https://nexte.st/docs/design/custom-test-harnesses/>). A target with its
//! own `main` that ignores its arguments answers a listing by running the whole
//! test and printing its progress, which nextest cannot parse, so it refuses the
//! entire run. Running is already right: the name and flags it passes are
//! ignored the same way `cargo test`'s are.
//!
//! Declared by `#[path]` in the five `harness = false` targets only, not from
//! `common/mod.rs`, so no other test crate compiles it.

/// Answers a `--list` query for the single test `name` and returns `true`, or
/// returns `false` when this is not a listing and the caller should run.
///
/// A target that cannot run on this platform lists itself as ignored, so
/// nextest reports it skipped instead of running its `main` to print a SKIP
/// line and exit 0, which would be counted as a pass.
pub fn answer_list(name: &str, applicable: bool) -> bool {
    let mut listing = false;
    let mut ignored = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--list" => listing = true,
            "--ignored" => ignored = true,
            _ => {}
        }
    }
    if !listing {
        return false;
    }
    // Ignored tests are a subset of all tests, as libtest lists them.
    if !ignored || !applicable {
        println!("{name}: test");
    }
    true
}
