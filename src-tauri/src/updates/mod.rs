//! Opt-in release check for GitPulse itself.
//!
//! This module answers one question — "is there a newer published GitPulse
//! than the one running?" — and it answers it without adding a dependency and
//! without an HTTP client. The transport is `git ls-remote --tags` against the
//! project's own public repository, which works for three reasons:
//!
//!   * `git` is already a hard requirement of the application, so there is no
//!     new tool to install and no new attack surface;
//!   * the tag namespace of a public repo is readable anonymously, so the
//!     check needs no `gh` CLI, no token, and no API rate-limit budget;
//!   * [`crate::engine::git_cli`] already hardens every `git` invocation
//!     (terminal prompts disabled, credential helpers non-interactive,
//!     ambient `GIT_*` configuration stripped, output and wall-clock bounded),
//!     so this check inherits that instead of re-earning it.
//!
//! It is deliberately *opt-in*: nothing here runs until the user turns the
//! preference on or presses "Check now". The frontend owns that gate; this
//! module performs a check only when it is called.
//!
//! ## Fail-closed reporting
//!
//! [`UpdateCheck::checked`] separates "the check ran" from "the check found
//! nothing". A network failure, a missing `git`, or an unparseable tag list
//! returns `checked: false` with an `error`, never `update_available: false`.
//! An unreachable server must not be able to render as "you are up to date".

use crate::engine::git_cli::git_global_with_timeout;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Upstream repository, taken from `Cargo.toml`'s `repository` field so the
/// URL cannot drift from the package metadata.
pub const REPOSITORY_URL: &str = env!("CARGO_PKG_REPOSITORY");

/// Version of the running build. `scripts/check-release-version.mjs` gates
/// every release on this agreeing with `tauri.conf.json` and `package.json`,
/// so one constant is enough.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Wall-clock ceiling for the tag listing.
///
/// Deliberately *not* [`crate::engine::git_cli::NETWORK_TIMEOUT`] (30 min):
/// that budget exists for multi-gigabyte clones and fetches. A version check
/// transfers a few kilobytes of ref names, and one that has not answered in
/// twenty seconds has failed as far as the user is concerned.
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

/// Upper bound on ref lines parsed from one listing.
///
/// `git_cli` already caps the raw byte stream, but that cap is sized for git
/// data, not for this. A repository that somehow published a hundred thousand
/// tags should make this check give up loudly rather than spend the time.
const MAX_TAG_LINES: usize = 5_000;

/// Outcome of one update check, shaped for direct rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheck {
    /// Version of the running build.
    pub current_version: String,
    /// Newest stable published version, empty when the check did not run.
    pub latest_version: String,
    /// True only when a strictly newer stable release exists. Meaningless
    /// unless `checked` is true.
    pub update_available: bool,
    /// Where to send the user. Always populated, so "could not check" can
    /// still offer the releases page.
    pub release_url: String,
    /// Whether the check actually completed. False means the rest of the
    /// answer is unknown, not negative.
    pub checked: bool,
    /// Why the check could not run, when `checked` is false.
    pub error: Option<String>,
}

impl UpdateCheck {
    fn failed(error: String) -> Self {
        Self {
            current_version: CURRENT_VERSION.to_string(),
            latest_version: String::new(),
            update_available: false,
            release_url: releases_url(),
            checked: false,
            error: Some(error),
        }
    }
}

/// Landing page listing every published release.
pub fn releases_url() -> String {
    format!("{}/releases", REPOSITORY_URL.trim_end_matches(".git"))
}

/// Permalink for one release tag.
pub fn release_tag_url(tag: &str) -> String {
    format!(
        "{}/releases/tag/{}",
        REPOSITORY_URL.trim_end_matches(".git"),
        percent_encode_tag(tag)
    )
}

/// Percent-encodes every byte outside a URL-safe tag vocabulary, so a tag
/// name from the remote can only ever produce a well-formed link. Mirrors the
/// encoder in [`crate::github`]; `/` stays verbatim because slash-bearing
/// tags are real and encode to themselves.
fn percent_encode_tag(tag: &str) -> String {
    let mut out = String::with_capacity(tag.len());
    for byte in tag.bytes() {
        let safe = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/');
        if safe {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A parsed `MAJOR.MINOR.PATCH` release version.
///
/// Pre-release and build-metadata suffixes are not modelled, because tags
/// carrying them are never offered as updates (see [`parse_release_tag`]).
/// Ordering is the derived field order, which is exactly numeric precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

/// Parses a bare `MAJOR.MINOR.PATCH` version string.
///
/// Strict on purpose: exactly three numeric components, no suffix, no leading
/// `v`. Anything else returns `None` rather than a partially-guessed version,
/// because a misparse here silently changes which build a user is told to
/// install.
pub fn parse_version(text: &str) -> Option<Version> {
    let mut parts = text.split('.');
    let major = parts.next()?;
    let minor = parts.next()?;
    let patch = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let component = |raw: &str| -> Option<u64> {
        // `u64::from_str` accepts a leading `+`; a version component must be
        // plain digits, and an empty component must not parse.
        if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        raw.parse::<u64>().ok()
    };
    Some(Version {
        major: component(major)?,
        minor: component(minor)?,
        patch: component(patch)?,
    })
}

/// Parses one release tag (`v1.2.3` or `1.2.3`) into a version.
///
/// Pre-release tags (`v1.2.3-rc.1`, `v1.2.3+build`) return `None` and are
/// therefore never proposed as an update. Offering a release candidate to
/// someone running a stable build is a decision the user has not made, and
/// this check has no way to ask.
pub fn parse_release_tag(tag: &str) -> Option<Version> {
    let trimmed = tag.trim();
    let bare = trimmed.strip_prefix('v').unwrap_or(trimmed);
    if bare.contains('-') || bare.contains('+') {
        return None;
    }
    parse_version(bare)
}

/// Extracts the highest stable version from `git ls-remote --tags --refs`
/// output.
///
/// Each line is `<sha>\trefs/tags/<name>`. Lines that do not match that shape,
/// and tags that are not stable `MAJOR.MINOR.PATCH` releases, are skipped:
/// a repository is free to carry tags this scheme does not describe.
///
/// Returns the tag text alongside the version, so the caller links to the tag
/// the remote actually published rather than a reconstructed guess.
pub fn latest_stable_tag(ls_remote_output: &str) -> Result<Option<(String, Version)>, String> {
    let mut best: Option<(String, Version)> = None;
    let mut seen = 0usize;
    for line in ls_remote_output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        seen += 1;
        if seen > MAX_TAG_LINES {
            return Err(format!(
                "remote published more than {MAX_TAG_LINES} tags; refusing to scan further"
            ));
        }
        let Some((_sha, refname)) = line.split_once('\t') else {
            continue;
        };
        // `--refs` suppresses peeled `^{}` entries, but strip defensively so a
        // git that ignores the flag cannot produce a bogus tag name.
        let Some(tag) = refname.trim().strip_prefix("refs/tags/") else {
            continue;
        };
        let tag = tag.strip_suffix("^{}").unwrap_or(tag);
        let Some(version) = parse_release_tag(tag) else {
            continue;
        };
        match &best {
            Some((_, current)) if *current >= version => {}
            _ => best = Some((tag.to_string(), version)),
        }
    }
    Ok(best)
}

/// Builds the [`UpdateCheck`] for a known-good tag listing.
///
/// Split from [`check_for_update`] so the whole decision — including the
/// "remote has no stable tag at all" and "remote is behind us" branches — is
/// testable without touching the network.
pub fn evaluate(current_version: &str, ls_remote_output: &str) -> UpdateCheck {
    let Some(current) = parse_version(current_version) else {
        // Unreachable via CURRENT_VERSION (Cargo would reject a non-semver
        // package version), but this function is also the tested entry point.
        return UpdateCheck::failed(format!(
            "the running version '{current_version}' is not a MAJOR.MINOR.PATCH version"
        ));
    };
    let latest = match latest_stable_tag(ls_remote_output) {
        Ok(found) => found,
        Err(error) => return UpdateCheck::failed(error),
    };
    let Some((tag, version)) = latest else {
        return UpdateCheck::failed(
            "the repository published no stable release tag to compare against".into(),
        );
    };
    UpdateCheck {
        current_version: current_version.to_string(),
        latest_version: format!("{}.{}.{}", version.major, version.minor, version.patch),
        update_available: version > current,
        release_url: if version > current {
            release_tag_url(&tag)
        } else {
            releases_url()
        },
        checked: true,
        error: None,
    }
}

/// Runs the check against the upstream repository.
///
/// Never panics and never returns `Err`: every failure mode is reported in
/// the returned value with `checked: false`, because the caller's job is to
/// display an outcome, not to decide what a transport error means.
pub fn check_for_update() -> UpdateCheck {
    let args = ["ls-remote", "--tags", "--refs", REPOSITORY_URL];
    match git_global_with_timeout(&args, CHECK_TIMEOUT) {
        Ok(bytes) => evaluate(CURRENT_VERSION, &String::from_utf8_lossy(&bytes)),
        Err(error) => UpdateCheck::failed(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(tag: &str) -> String {
        format!("0123456789abcdef0123456789abcdef01234567\trefs/tags/{tag}\n")
    }

    #[test]
    fn parses_plain_versions() {
        assert_eq!(
            parse_version("1.2.3"),
            Some(Version {
                major: 1,
                minor: 2,
                patch: 3
            })
        );
        assert_eq!(
            parse_version("0.0.0"),
            Some(Version {
                major: 0,
                minor: 0,
                patch: 0
            })
        );
    }

    #[test]
    fn rejects_malformed_versions() {
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "1.2.x",
            "v1.2.3",
            "1..3",
            "1.2.",
            " 1.2.3",
            "+1.2.3",
            "1.2.-3",
            "01.02.03x",
        ] {
            assert_eq!(parse_version(bad), None, "expected {bad:?} to be rejected");
        }
    }

    #[test]
    fn leading_zero_components_still_parse() {
        // Not canonical semver, but numerically unambiguous; refusing it would
        // drop a legitimately published tag.
        assert_eq!(
            parse_version("01.02.03"),
            Some(Version {
                major: 1,
                minor: 2,
                patch: 3
            })
        );
    }

    #[test]
    fn release_tags_accept_optional_v_prefix() {
        assert!(parse_release_tag("v1.2.3").is_some());
        assert!(parse_release_tag("1.2.3").is_some());
        assert!(parse_release_tag("  v1.2.3  ").is_some());
    }

    #[test]
    fn prerelease_tags_are_never_candidates() {
        for tag in [
            "v1.2.3-rc.1",
            "v1.2.3-beta",
            "1.2.3+build.7",
            "v2.0.0-alpha",
        ] {
            assert_eq!(parse_release_tag(tag), None, "{tag} must not be offered");
        }
    }

    #[test]
    fn picks_the_highest_stable_tag() {
        let output = format!(
            "{}{}{}{}",
            line("v0.0.9"),
            line("v0.1.0"),
            line("v0.0.10"),
            line("v0.1.0-rc.1"),
        );
        let (tag, version) = latest_stable_tag(&output).unwrap().unwrap();
        assert_eq!(tag, "v0.1.0");
        assert_eq!(
            version,
            Version {
                major: 0,
                minor: 1,
                patch: 0
            }
        );
    }

    #[test]
    fn compares_numerically_not_lexically() {
        // "0.0.10" sorts before "0.0.9" as text; the check must not.
        let output = format!("{}{}", line("v0.0.9"), line("v0.0.10"));
        let (tag, _) = latest_stable_tag(&output).unwrap().unwrap();
        assert_eq!(tag, "v0.0.10");
    }

    #[test]
    fn ignores_unrelated_and_malformed_refs() {
        let output = format!(
            "{}{}{}{}{}",
            line("nightly"),
            line("release-2024-01"),
            "garbage without a tab\n",
            "abc\trefs/heads/main\n",
            line("v1.0.0"),
        );
        let (tag, _) = latest_stable_tag(&output).unwrap().unwrap();
        assert_eq!(tag, "v1.0.0");
    }

    #[test]
    fn strips_peeled_tag_suffix() {
        let output = line("v1.0.0^{}");
        let (tag, _) = latest_stable_tag(&output).unwrap().unwrap();
        assert_eq!(tag, "v1.0.0");
    }

    #[test]
    fn no_stable_tag_yields_none() {
        let output = format!("{}{}", line("nightly"), line("v1.0.0-rc.1"));
        assert_eq!(latest_stable_tag(&output).unwrap(), None);
    }

    #[test]
    fn empty_output_yields_none() {
        assert_eq!(latest_stable_tag("").unwrap(), None);
    }

    #[test]
    fn refuses_an_unbounded_tag_listing() {
        let output: String = (0..MAX_TAG_LINES + 1)
            .map(|i| line(&format!("t{i}")))
            .collect();
        let error = latest_stable_tag(&output).unwrap_err();
        assert!(error.contains("refusing to scan"), "unexpected: {error}");
    }

    #[test]
    fn reports_an_available_update() {
        let result = evaluate("0.0.3", &line("v0.1.0"));
        assert!(result.checked);
        assert!(result.update_available);
        assert_eq!(result.latest_version, "0.1.0");
        assert!(result.release_url.ends_with("/releases/tag/v0.1.0"));
        assert_eq!(result.error, None);
    }

    #[test]
    fn reports_up_to_date_on_an_exact_match() {
        let result = evaluate("0.0.3", &line("v0.0.3"));
        assert!(result.checked);
        assert!(!result.update_available);
        assert_eq!(result.latest_version, "0.0.3");
        assert!(result.release_url.ends_with("/releases"));
    }

    #[test]
    fn a_remote_behind_us_is_not_an_update() {
        // Running a locally-built newer version than anything published.
        let result = evaluate("0.2.0", &line("v0.1.0"));
        assert!(result.checked);
        assert!(!result.update_available);
    }

    #[test]
    fn a_failed_check_never_reads_as_up_to_date() {
        // The invariant this whole module exists to protect: `checked` is the
        // only field that distinguishes "no update" from "did not look".
        let failed = UpdateCheck::failed("network unreachable".into());
        assert!(!failed.checked);
        assert!(!failed.update_available);
        assert_eq!(failed.error.as_deref(), Some("network unreachable"));
        assert!(failed.release_url.ends_with("/releases"));

        let no_tags = evaluate("0.0.3", "");
        assert!(!no_tags.checked);
        assert!(no_tags.error.is_some());
    }

    #[test]
    fn a_bad_running_version_fails_the_check() {
        let result = evaluate("not-a-version", &line("v1.0.0"));
        assert!(!result.checked);
        assert!(result.error.unwrap().contains("MAJOR.MINOR.PATCH"));
    }

    #[test]
    fn tag_urls_are_percent_encoded() {
        assert!(release_tag_url("v1.0.0 rc").ends_with("/releases/tag/v1.0.0%20rc"));
        assert!(release_tag_url("v1.0.0<script>").contains("%3Cscript%3E"));
    }

    #[test]
    fn urls_drop_a_dot_git_suffix() {
        // Cargo's repository field may or may not carry `.git`; neither form
        // may produce `.../GitPulse.git/releases`.
        assert!(!releases_url().contains(".git/"));
        assert!(releases_url().ends_with("/releases"));
    }

    /// End-to-end against the real repository.
    ///
    /// `#[ignore]`d so the suite stays hermetic: CI must not fail because a
    /// runner has no network. Run it deliberately with
    /// `cargo test --manifest-path src-tauri/Cargo.toml updates::tests::live -- --ignored`
    /// after changing the transport.
    #[test]
    #[ignore = "requires network access to the upstream repository"]
    fn live_check_reaches_the_upstream_repository() {
        let result = check_for_update();
        assert!(result.checked, "live check failed: {:?}", result.error);
        assert!(
            parse_version(&result.latest_version).is_some(),
            "latest_version {:?} is not a version",
            result.latest_version
        );
        assert_eq!(result.current_version, CURRENT_VERSION);
    }

    #[test]
    fn the_shipped_version_constant_is_parseable() {
        assert!(
            parse_version(CURRENT_VERSION).is_some(),
            "CARGO_PKG_VERSION {CURRENT_VERSION} must be MAJOR.MINOR.PATCH"
        );
    }
}
