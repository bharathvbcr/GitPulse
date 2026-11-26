//! The listing half of cargo-nextest's custom-harness protocol, for a
//! `harness = false` target that runs as one test, and libtest's name filter.
//!
//! nextest asks every test binary for its tests with `--list --format terse`
//! (all tests) and `--list --format terse --ignored` (the ignored ones), each
//! line `<name>: test` on stdout and nothing else there, and then runs each
//! test as `<name> --nocapture --exact`
//! (<https://nexte.st/docs/design/custom-test-harnesses/>). A target with its
//! own `main` that ignores its arguments answers a listing by running the whole
//! test and printing its progress, which nextest cannot parse, so it refuses the
//! entire run.
//!
//! `cargo test -- <filter>` passes the filter to every test binary. libtest
//! runs only the tests whose name contains a filter. A `main` that ignored it
//! ran the whole target for any filter, so every filtered `cargo test` without
//! `--lib` or `--test <name>` started the native GUI suites. Each one opens
//! real AppKit windows on the person's display. [`excluded_by_filter`] applies
//! libtest's rule to the one test this target is.
//!
//! Declared by `#[path]` in the `harness = false` targets only, not from
//! `common/mod.rs`, so no other test crate compiles it.

/// Answers a `--list` query for the single test `name`, or skips a run that a
/// test-name filter excludes. Returns `true` when the caller must not run, and
/// `false` when it should.
///
/// A target that cannot run on this platform lists itself as ignored, so
/// nextest reports it skipped instead of running its `main` to print a SKIP
/// line and exit 0, which would be counted as a pass.
pub fn answer_list(name: &str, applicable: bool) -> bool {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let listing = args.iter().any(|arg| arg == "--list");
    if !listing {
        if excluded_by_filter(name, &args) {
            println!("SKIP {name}: no test-name filter matches it");
            return true;
        }
        return false;
    }
    let ignored = args.iter().any(|arg| arg == "--ignored");
    // Ignored tests are a subset of all tests, as libtest lists them.
    if !ignored || !applicable {
        println!("{name}: test");
    }
    true
}

/// libtest options that take a value, as `--opt value` or `--opt=value`.
const VALUE_OPTIONS: &[&str] = &[
    "--test-threads",
    "--skip",
    "--logfile",
    "--format",
    "--color",
    "--shuffle-seed",
    "-Z",
];

/// libtest options that take no value.
const FLAG_OPTIONS: &[&str] = &[
    "--exact",
    "--nocapture",
    "--no-capture",
    "--show-output",
    "--quiet",
    "-q",
    "--ignored",
    "--include-ignored",
    "--test",
    "--bench",
    "--list",
    "--force-run-in-process",
    "--report-time",
    "--ensure-time",
    "--shuffle",
    "--fail-fast",
];

/// Whether libtest's filters, given as `args`, leave out the single test `name`.
///
/// A test runs when there is no positional filter, or when its name contains
/// one (equals one under `--exact`). It is left out when a `--skip` pattern
/// matches it the same way. An option this does not recognise could take a
/// value that would then read as a filter, so any unrecognised option, such as
/// a target's own `--probe=<case>` child argument, means run.
pub fn excluded_by_filter(name: &str, args: &[String]) -> bool {
    let mut filters = Vec::new();
    let mut skips = Vec::new();
    let mut exact = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if arg == "--" {
            continue;
        }
        if !arg.starts_with('-') {
            filters.push(arg.as_str());
            continue;
        }
        let (option, inline) = match arg.split_once('=') {
            Some((option, value)) => (option, Some(value)),
            None => (arg.as_str(), None),
        };
        if VALUE_OPTIONS.contains(&option) {
            let value = match inline {
                Some(value) => Some(value),
                None => rest.next().map(String::as_str),
            };
            if option == "--skip" {
                match value {
                    Some(value) => skips.push(value),
                    None => return false,
                }
            }
        } else if FLAG_OPTIONS.contains(&option) && inline.is_none() {
            exact |= option == "--exact";
        } else {
            return false;
        }
    }
    let matches = |pattern: &str| {
        if exact {
            name == pattern
        } else {
            name.contains(pattern)
        }
    };
    let filtered_out = !filters.is_empty() && !filters.iter().any(|filter| matches(filter));
    filtered_out || skips.iter().any(|skip| matches(skip))
}
