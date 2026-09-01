//! Integration coverage for the opt-in release check.
//!
//! `check_for_update()` shells out to `git ls-remote`, so the network-free
//! surface worth testing is everything downstream of it: the tag parser and
//! `evaluate`, which decide whether the app tells a user an update exists.
//! A false positive nags; a false negative hides a fix. Both are decided here.

use gitpulse_lib::updates::{evaluate, latest_stable_tag, parse_release_tag, parse_version};

/// Shape of one `git ls-remote --tags` line.
fn ls_remote(tags: &[&str]) -> String {
    tags.iter()
        .enumerate()
        .map(|(i, tag)| format!("{:040x}\trefs/tags/{tag}", i + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn parses_only_three_component_versions() {
    assert!(parse_version("1.2.3").is_some());
    assert!(parse_version("0.0.3").is_some());
    for rejected in ["1.2", "1.2.3.4", "1.2.x", "v1.2.3", "", "  ", "1.2.-3"] {
        assert!(
            parse_version(rejected).is_none(),
            "{rejected} must not parse"
        );
    }
}

#[test]
fn release_tags_accept_either_prefix_but_never_a_pre_release() {
    // The `v` is optional: a repository may tag either way.
    assert!(parse_release_tag("v1.2.3").is_some());
    assert!(parse_release_tag("1.2.3").is_some());
    // Offering a release candidate to someone on a stable build is a choice
    // the user never made, and this check has no way to ask.
    for pre in ["v1.2.3-rc.1", "1.2.3-beta", "v1.2.3+build.5", "v1.2.3-0"] {
        assert!(
            parse_release_tag(pre).is_none(),
            "{pre} must not count as stable"
        );
    }
}

#[test]
fn picks_the_highest_version_regardless_of_listing_order() {
    let output = ls_remote(&["v0.0.9", "v0.1.0", "v0.0.10", "v0.0.2"]);
    let (tag, _) = latest_stable_tag(&output)
        .expect("parses")
        .expect("a stable tag");
    // Lexical ordering would pick v0.0.9 over v0.0.10 here.
    assert_eq!(tag, "v0.1.0");
}

#[test]
fn ignores_tags_that_are_not_stable_releases() {
    let output = ls_remote(&[
        "v1.0.0",
        "v2.0.0-rc.1",
        "nightly",
        "release-3",
        "v1.0.0-alpha",
    ]);
    let (tag, _) = latest_stable_tag(&output)
        .expect("parses")
        .expect("a stable tag");
    assert_eq!(
        tag, "v1.0.0",
        "a pre-release must not be offered as an update"
    );
}

#[test]
fn a_peeled_tag_entry_resolves_to_the_tag_it_points_at() {
    // `--refs` suppresses `^{}` entries, but a git that ignores the flag must
    // not turn an annotated v2.0.0 into an unparseable name and hide the release.
    let output = ls_remote(&["v1.0.0", "v2.0.0", "v2.0.0^{}"]);
    let (tag, _) = latest_stable_tag(&output)
        .expect("parses")
        .expect("a stable tag");
    assert_eq!(tag, "v2.0.0");
    assert!(
        !tag.contains("^{}"),
        "the peel marker must not leak into the tag name"
    );
}

#[test]
fn an_empty_or_tagless_remote_is_not_an_error_but_has_no_answer() {
    assert_eq!(latest_stable_tag("").expect("parses"), None);
    assert_eq!(latest_stable_tag("   \n  \n").expect("parses"), None);
}

#[test]
fn refuses_to_scan_an_unbounded_tag_list() {
    // A hostile or runaway remote must be bounded, not scanned forever.
    let many = (0..100_000)
        .map(|i| format!("{i:040x}\trefs/tags/v0.0.{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let error = latest_stable_tag(&many).expect_err("must refuse");
    assert!(
        error.contains("refusing to scan"),
        "unexpected error: {error}"
    );
}

#[test]
fn malformed_lines_do_not_abort_the_scan() {
    let output = format!("garbage-without-a-tab\n{}", ls_remote(&["v1.4.0"]));
    let (tag, _) = latest_stable_tag(&output)
        .expect("parses")
        .expect("a stable tag");
    assert_eq!(tag, "v1.4.0");
}

#[test]
fn reports_an_update_only_when_the_remote_is_strictly_newer() {
    let newer = evaluate("1.0.0", &ls_remote(&["v1.0.1"]));
    assert!(newer.checked && newer.update_available);
    assert_eq!(newer.latest_version, "1.0.1");

    let same = evaluate("1.0.1", &ls_remote(&["v1.0.1"]));
    assert!(same.checked && !same.update_available);

    // A local build ahead of the remote is not an update.
    let ahead = evaluate("2.0.0", &ls_remote(&["v1.0.1"]));
    assert!(ahead.checked && !ahead.update_available);
}

#[test]
fn a_check_that_could_not_run_is_not_reported_as_up_to_date() {
    for (current, output) in [("not-a-version", "" as &str), ("1.0.0", "")] {
        let result = evaluate(current, output);
        assert!(
            !result.checked,
            "an unusable input must not count as a completed check"
        );
        assert!(!result.update_available);
        assert!(result.error.is_some(), "a failed check must say why");
        // The user still gets somewhere to look.
        assert!(result.release_url.starts_with("https://"));
    }
}
