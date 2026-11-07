//! Opt-in release check for GitPulse itself.
//!
//! This module answers one question — "is there a newer published GitPulse
//! than the one running?" — and it answers it without adding a dependency and
//! without an HTTP client. Candidates come from `git ls-remote --tags` against
//! the project's own public repository, which works for three reasons:
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
//! A tag is only a candidate. It is announced once its release is *published*,
//! confirmed by one bounded `HEAD` of the release's checksum asset
//! ([`release_marker_asset`]), because the release workflow runs for many
//! minutes after a tag is pushed and may never finish.
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
use crate::ipc_error::CommandError;
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
    pub(crate) fn failed(error: String) -> Self {
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
        crate::github::percent_encode_tag(tag)
    )
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

/// Extracts every stable version from `git ls-remote --tags --refs` output,
/// highest first.
///
/// Each line is `<sha>\trefs/tags/<name>`. Lines that do not match that shape,
/// and tags that are not stable `MAJOR.MINOR.PATCH` releases, are skipped:
/// a repository is free to carry tags this scheme does not describe.
///
/// Returns the tag text alongside the version, so the caller links to the tag
/// the remote actually published rather than a reconstructed guess.
pub fn stable_tags(ls_remote_output: &str) -> Result<Vec<(String, Version)>, String> {
    let mut found: Vec<(String, Version)> = Vec::new();
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
        match found.iter_mut().find(|(_, known)| *known == version) {
            None => found.push((tag.to_string(), version)),
            // Several spellings of one version (`1.5.0`, `v01.5.0`, `v1.5.0`):
            // keep the best-ranked one, the first listed among equals.
            Some(existing) => {
                if spelling_rank(tag, version) > spelling_rank(&existing.0, version) {
                    existing.0 = tag.to_string();
                }
            }
        }
    }
    found.sort_by_key(|(_, version)| std::cmp::Reverse(*version));
    Ok(found)
}

/// The highest stable tag in a listing, published or not. See [`stable_tags`].
pub fn latest_stable_tag(ls_remote_output: &str) -> Result<Option<(String, Version)>, String> {
    Ok(stable_tags(ls_remote_output)?.into_iter().next())
}

/// Prefix of the tags the release workflow publishes.
///
/// Coupled to `.github/workflows/release.yml`, which triggers on `v*` tags
/// only: a bare `1.2.3` tag never gets a release, so when both spellings of a
/// version exist [`stable_tags`] keeps this one.
const RELEASE_TAG_PREFIX: char = 'v';

/// How likely the release workflow published `tag`: the canonical
/// `v<major>.<minor>.<patch>` spelling (2), any other `v` spelling such as
/// `v01.5.0` (1), a bare spelling (0).
fn spelling_rank(tag: &str, version: Version) -> u8 {
    let canonical = format!(
        "{RELEASE_TAG_PREFIX}{}.{}.{}",
        version.major, version.minor, version.patch
    );
    if tag == canonical {
        2
    } else if tag.starts_with(RELEASE_TAG_PREFIX) {
        1
    } else {
        0
    }
}

/// How many tags newer than the running build are probed for a published
/// release. A tag with no release is the exception, not a queue; the bound
/// keeps a repository with many such tags from turning one check into many
/// network round trips.
///
/// Newer tags beyond the bound are unverified, and [`evaluate`] *fails closed*
/// on them (`checked: false`) rather than raising the bound or reporting up to
/// date: a published release may sit behind them.
const MAX_RELEASE_PROBES: usize = 5;

/// Name of the asset a *finished* release carries and a draft does not expose.
///
/// `scripts/check-release-assets.mjs` names it (`checksums`) and
/// `release-state.mjs` uploads it last, after every installer and before the
/// draft is published, so it is public exactly when users can install the
/// release. A test pins this format to that script.
pub fn release_marker_asset(version: Version) -> String {
    format!(
        "GitPulse_{}.{}.{}_SHA256SUMS.txt",
        version.major, version.minor, version.patch
    )
}

/// Whether the release for `tag` is published: its checksum asset answers an
/// anonymous request, which a draft's assets do not.
///
/// A pushed tag says nothing about that. The release workflow runs for many
/// minutes after the tag lands, may fail, and publishes only when green, so
/// announcing every tag told every user about builds nobody could install (and
/// a mistaken `v9.0.0` stayed announced until someone deleted it).
fn release_is_published(tag: &str, version: Version) -> Result<bool, CommandError> {
    let url = format!(
        "{}/releases/download/{}/{}",
        REPOSITORY_URL.trim_end_matches(".git"),
        crate::github::percent_encode_tag(tag),
        release_marker_asset(version)
    );
    crate::tool_install::release::http_head_ok(&url)
}

/// Builds the [`UpdateCheck`] for a known-good tag listing.
///
/// `published` answers whether the release behind a tag is installable. It is
/// asked only about tags newer than `current_version`, newest first, at most
/// [`MAX_RELEASE_PROBES`] of them; the first one it confirms is the update. If
/// it cannot answer, the whole check fails rather than guessing in either
/// direction: an unverifiable tag is neither announced nor ruled out. Likewise
/// when the budget is spent and newer tags remain unasked.
///
/// Split from [`check_for_update`] so the whole decision — including the
/// "remote has no stable tag at all" and "remote is behind us" branches — is
/// testable without touching the network.
pub fn evaluate(
    current_version: &str,
    ls_remote_output: &str,
    published: &dyn Fn(&str, Version) -> Result<bool, CommandError>,
) -> UpdateCheck {
    evaluate_detailed(current_version, ls_remote_output, published).check
}

/// [`evaluate`]'s answer plus whether it is the *deterministic* fail-closed
/// state: the probe budget was spent with newer tags unasked. Only that state
/// is a function of the listing alone, so only it may be remembered; a probe
/// that errored (`curl` missing, network down) can answer differently on the
/// next launch and is always re-run.
struct Evaluation {
    check: UpdateCheck,
    budget_exhausted: bool,
}

fn evaluate_detailed(
    current_version: &str,
    ls_remote_output: &str,
    published: &dyn Fn(&str, Version) -> Result<bool, CommandError>,
) -> Evaluation {
    let plain = |check| Evaluation {
        check,
        budget_exhausted: false,
    };
    let Some(current) = parse_version(current_version) else {
        // Unreachable via CURRENT_VERSION (Cargo would reject a non-semver
        // package version), but this function is also the tested entry point.
        return plain(UpdateCheck::failed(format!(
            "the running version '{current_version}' is not a MAJOR.MINOR.PATCH version"
        )));
    };
    let tags = match stable_tags(ls_remote_output) {
        Ok(found) => found,
        Err(error) => return plain(UpdateCheck::failed(error)),
    };
    let Some((_, newest)) = tags.first() else {
        return plain(UpdateCheck::failed(
            "the repository published no stable release tag to compare against".into(),
        ));
    };
    let up_to_date = |latest: Version| UpdateCheck {
        current_version: current_version.to_string(),
        latest_version: format!("{}.{}.{}", latest.major, latest.minor, latest.patch),
        update_available: false,
        release_url: releases_url(),
        checked: true,
        error: None,
    };
    if *newest <= current {
        return plain(up_to_date(*newest));
    }
    let newer: Vec<&(String, Version)> = tags
        .iter()
        .filter(|(_, version)| *version > current)
        .collect();
    for (tag, version) in newer.iter().copied().take(MAX_RELEASE_PROBES) {
        match published(tag, *version) {
            Ok(true) => {
                return plain(UpdateCheck {
                    current_version: current_version.to_string(),
                    latest_version: format!(
                        "{}.{}.{}",
                        version.major, version.minor, version.patch
                    ),
                    update_available: true,
                    release_url: release_tag_url(tag),
                    checked: true,
                    error: None,
                })
            }
            Ok(false) => {}
            Err(error) => {
                return plain(UpdateCheck::failed(format!(
                    "could not confirm that {tag} is a published release: {error}",
                    error = error.message()
                )))
            }
        }
    }
    let unverified = newer.len().saturating_sub(MAX_RELEASE_PROBES);
    if unverified > 0 {
        // Fails closed: a published release may sit behind the unpublished
        // tags that used the budget, so "up to date" is not known.
        return Evaluation {
            check: UpdateCheck::failed(format!(
                "{unverified} newer {} went unverified after {MAX_RELEASE_PROBES} unpublished tags; see the releases page",
                if unverified == 1 { "tag" } else { "tags" }
            )),
            budget_exhausted: true,
        };
    }
    // Every tag newer than this build was probed and none has a published
    // release: nothing is installable, so there is nothing to announce.
    plain(up_to_date(current))
}

/// How long a remembered fail-closed answer is trusted without re-probing.
const FAIL_CLOSED_BACKOFF_SECS: u64 = 24 * 60 * 60;

/// The remembered fail-closed answer. Lives in the platform cache directory
/// (state that can be rebuilt at any time), so deleting it costs one round of
/// probes and nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FailClosedRecord {
    /// Local version the answer was computed for.
    version: String,
    /// [`tip_fingerprint`] of the listing the answer was computed for.
    tip: String,
    /// Unix seconds when the probes ran.
    recorded_at: u64,
    /// The fail-closed reason, replayed verbatim.
    error: String,
}

/// Fingerprint of everything in the listing that could change the verdict:
/// every stable tag newer than `current_version`, with the commit it points
/// at. A new tag, a moved tag or a removed tag all change it; tags at or
/// below the running version cannot, so they do not.
///
/// FNV-1a over the sorted `sha\ttag` lines: dependency-free and stable across
/// Rust versions, which `DefaultHasher` does not promise for a value written
/// to disk.
fn tip_fingerprint(ls_remote_output: &str, current_version: &str) -> String {
    let current = parse_version(current_version);
    let mut lines: Vec<&str> = ls_remote_output
        .lines()
        .map(str::trim)
        .filter(|line| {
            let Some((_, refname)) = line.split_once('\t') else {
                return false;
            };
            let Some(tag) = refname.trim().strip_prefix("refs/tags/") else {
                return false;
            };
            let tag = tag.strip_suffix("^{}").unwrap_or(tag);
            match (parse_release_tag(tag), current) {
                (Some(version), Some(current)) => version > current,
                (Some(_), None) => true,
                _ => false,
            }
        })
        .collect();
    lines.sort_unstable();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for line in lines {
        for byte in line.bytes().chain(std::iter::once(b'\n')) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}

/// Where the fail-closed record lives, when the platform cache directory
/// resolves.
fn record_path() -> Option<std::path::PathBuf> {
    crate::tool_config::default_cache_dir().map(|dir| dir.join("update-check.json"))
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Reads the record, treating a missing, unreadable or corrupt file as "no
/// record": the caller then probes normally. The failure direction is always
/// toward probing, never toward a stale answer.
fn read_record(path: &std::path::Path) -> Option<FailClosedRecord> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// [`evaluate`] with the fail-closed back-off.
///
/// A deterministic fail-closed result (probe budget spent, newer tags
/// unasked) cannot change until the listing, the running version or the
/// published releases do, yet every launch repeated its up to
/// [`MAX_RELEASE_PROBES`] HTTP probes. So that result is recorded at
/// `record_path`, and a later call with the same version and the same tip
/// fingerprint, less than [`FAIL_CLOSED_BACKOFF_SECS`] later, skips the
/// probes and replays it. The replay is the same `checked: false` failure, so
/// the verdict does not loosen; any other outcome clears the record.
///
/// `now` is unix seconds, injected so the 24 h rule is testable. The
/// `ls-remote` that produced `ls_remote_output` is the caller's one per
/// launch; it is what detects a changed tip.
pub fn evaluate_with_backoff(
    current_version: &str,
    ls_remote_output: &str,
    published: &dyn Fn(&str, Version) -> Result<bool, CommandError>,
    record_path: Option<&std::path::Path>,
    now: u64,
) -> UpdateCheck {
    let tip = tip_fingerprint(ls_remote_output, current_version);
    if let Some(record) = record_path.and_then(read_record) {
        // `recorded_at > now` is a clock that moved backwards: not evidence of
        // recency, so it probes.
        let fresh =
            record.recorded_at <= now && now - record.recorded_at < FAIL_CLOSED_BACKOFF_SECS;
        if fresh && record.version == current_version && record.tip == tip {
            return UpdateCheck::failed(record.error);
        }
    }
    let evaluation = evaluate_detailed(current_version, ls_remote_output, published);
    if let Some(path) = record_path {
        if evaluation.budget_exhausted {
            let record = FailClosedRecord {
                version: current_version.to_string(),
                tip,
                recorded_at: now,
                error: evaluation.check.error.clone().unwrap_or_default(),
            };
            let written = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| {
                    serde_json::to_vec(&record)
                        .map_err(std::io::Error::other)
                        .and_then(|body| devmap_query::write_atomic(path, &body).map(|_| ()))
                });
            if let Err(error) = written {
                log::warn!(target: "updates", "could not record the fail-closed update check: {error}");
            }
        } else if let Err(error) = std::fs::remove_file(path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!(target: "updates", "could not clear the fail-closed update record: {error}");
            }
        }
    }
    evaluation.check
}

/// Runs the check against the upstream repository.
///
/// Never panics and never returns `Err`: every failure mode is reported in
/// the returned value with `checked: false`, because the caller's job is to
/// display an outcome, not to decide what a transport error means.
///
/// Exactly one `git ls-remote` per call; the HTTP probes behind it are
/// subject to the fail-closed back-off of [`evaluate_with_backoff`].
pub fn check_for_update() -> UpdateCheck {
    let args = ["ls-remote", "--tags", "--refs", REPOSITORY_URL];
    match git_global_with_timeout(&args, CHECK_TIMEOUT) {
        Ok(bytes) => evaluate_with_backoff(
            CURRENT_VERSION,
            &String::from_utf8_lossy(&bytes),
            &release_is_published,
            record_path().as_deref(),
            now_unix(),
        ),
        Err(error) => UpdateCheck::failed(error.into_message()),
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
            line("v1.2.9"),
            line("v1.3.0"),
            line("v1.2.10"),
            line("v1.3.0-rc.1"),
        );
        let (tag, version) = latest_stable_tag(&output).unwrap().unwrap();
        assert_eq!(tag, "v1.3.0");
        assert_eq!(
            version,
            Version {
                major: 1,
                minor: 3,
                patch: 0
            }
        );
    }

    #[test]
    fn compares_numerically_not_lexically() {
        // Multi-digit patch versions must compare numerically, not lexically.
        let output = format!("{}{}", line("v1.2.9"), line("v1.2.10"));
        let (tag, _) = latest_stable_tag(&output).unwrap().unwrap();
        assert_eq!(tag, "v1.2.10");
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
        let result = evaluate("0.0.3", &line("v1.2.3"), &|_, _| Ok(true));
        assert!(result.checked);
        assert!(result.update_available);
        assert_eq!(result.latest_version, "1.2.3");
        assert!(result.release_url.ends_with("/releases/tag/v1.2.3"));
        assert_eq!(result.error, None);
    }

    #[test]
    fn reports_up_to_date_on_an_exact_match() {
        let result = evaluate("0.0.3", &line("v0.0.3"), &|_, _| Ok(true));
        assert!(result.checked);
        assert!(!result.update_available);
        assert_eq!(result.latest_version, "0.0.3");
        assert!(result.release_url.ends_with("/releases"));
    }

    #[test]
    fn a_remote_behind_us_is_not_an_update() {
        // Running a locally-built newer version than anything published.
        let result = evaluate("2.0.0", &line("v1.2.3"), &|_, _| Ok(true));
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

        let no_tags = evaluate("0.0.3", "", &|_, _| Ok(true));
        assert!(!no_tags.checked);
        assert!(no_tags.error.is_some());
    }

    /// Regression (any pushed tag announced an update): `v1.3.0` is tagged and
    /// its release workflow has not published, or failed. The old check
    /// announced it to every user on the next poll.
    #[test]
    fn a_tag_with_no_published_release_is_not_announced() {
        let result = evaluate("1.2.0", &line("v1.3.0"), &|_, _| Ok(false));
        assert!(result.checked, "{result:?}");
        assert!(!result.update_available, "{result:?}");
        assert_eq!(result.latest_version, "1.2.0");
        assert!(result.release_url.ends_with("/releases"));
    }

    #[test]
    fn the_newest_published_release_wins_over_a_newer_unpublished_tag() {
        let listing = format!("{}{}{}", line("v1.2.1"), line("v1.5.0"), line("v1.3.0"));
        let asked = std::cell::RefCell::new(Vec::new());
        let result = evaluate("1.2.0", &listing, &|tag, _| {
            asked.borrow_mut().push(tag.to_string());
            Ok(tag == "v1.3.0")
        });
        assert!(result.update_available, "{result:?}");
        assert_eq!(result.latest_version, "1.3.0");
        assert!(result.release_url.ends_with("/releases/tag/v1.3.0"));
        assert_eq!(
            *asked.borrow(),
            ["v1.5.0", "v1.3.0"],
            "newest first, stopping at the first published"
        );
    }

    #[test]
    fn a_release_that_cannot_be_checked_fails_the_check_both_ways() {
        let result = evaluate("1.2.0", &line("v1.3.0"), &|_, _| {
            Err("curl is not installed".into())
        });
        assert!(
            !result.checked,
            "an unverifiable tag must not look like up to date"
        );
        assert!(
            !result.update_available,
            "an unverifiable tag must not be announced"
        );
        assert!(result.error.unwrap().contains("curl is not installed"));
    }

    /// Regression (RELEASE-01): the probe budget ran out with newer tags still
    /// unasked and the check fell through to "up to date".
    #[test]
    fn release_probes_are_bounded_and_an_exhausted_budget_fails_closed() {
        let listing: String = (0..40)
            .map(|patch| line(&format!("v1.3.{patch}")))
            .collect::<String>()
            + &line("v1.0.0");
        let probes = std::cell::Cell::new(0usize);
        let result = evaluate("1.2.0", &listing, &|_, _| {
            probes.set(probes.get() + 1);
            Ok(false)
        });
        assert_eq!(probes.get(), MAX_RELEASE_PROBES, "probing stays bounded");
        assert!(!result.checked, "{result:?}");
        assert!(!result.update_available, "{result:?}");
        assert!(result.latest_version.is_empty(), "{result:?}");
        let error = result.error.expect("a failed check carries its reason");
        assert!(error.contains("35 newer tags"), "unexpected: {error}");
        assert!(result.release_url.ends_with("/releases"));
    }

    /// The audit's failure: build 1.4.0, v1.6.0..v1.6.4 unpublished, v1.5.0
    /// published. The published release is out of probe reach, so the honest
    /// answer is "could not check" (the fail-closed option, not a larger bound).
    #[test]
    fn a_published_release_behind_the_probe_budget_is_not_reported_as_up_to_date() {
        let listing: String = ["v1.6.4", "v1.6.3", "v1.6.2", "v1.6.1", "v1.6.0", "v1.5.0"]
            .iter()
            .map(|tag| line(tag))
            .collect();
        let probes = std::cell::Cell::new(0usize);
        let result = evaluate("1.4.0", &listing, &|tag, _| {
            probes.set(probes.get() + 1);
            Ok(tag == "v1.5.0")
        });
        assert_eq!(probes.get(), MAX_RELEASE_PROBES);
        assert!(!result.checked, "{result:?}");
        assert!(!result.update_available, "{result:?}");
        assert!(result.error.unwrap().contains("1 newer tag"));
    }

    /// Exactly the budget's worth of newer tags, none published: every newer
    /// tag was asked about, so "nothing installable" is verified, not assumed.
    #[test]
    fn covering_every_newer_tag_within_the_budget_is_honestly_up_to_date() {
        let listing: String = (0..MAX_RELEASE_PROBES)
            .map(|patch| line(&format!("v1.3.{patch}")))
            .collect::<String>()
            + &line("v1.0.0");
        let probes = std::cell::Cell::new(0usize);
        let result = evaluate("1.2.0", &listing, &|_, _| {
            probes.set(probes.get() + 1);
            Ok(false)
        });
        assert_eq!(probes.get(), MAX_RELEASE_PROBES);
        assert!(result.checked && !result.update_available, "{result:?}");
        assert_eq!(result.latest_version, "1.2.0");
        assert_eq!(result.error, None);
    }

    /// Regression (RELEASE-02): the first spelling listed won, so a bare
    /// `1.5.0` ahead of `v1.5.0` made the probe ask for a release the `v*`
    /// workflow never publishes.
    #[test]
    fn two_spellings_of_one_version_probe_the_v_tag_the_workflow_publishes() {
        for listing in [
            format!("{}{}", line("1.5.0"), line("v1.5.0")),
            format!("{}{}", line("v1.5.0"), line("1.5.0")),
        ] {
            let tags = stable_tags(&listing).unwrap();
            assert_eq!(tags.len(), 1);
            assert_eq!(tags[0].0, "v1.5.0");

            let asked = std::cell::RefCell::new(Vec::new());
            let result = evaluate("1.4.0", &listing, &|tag, _| {
                asked.borrow_mut().push(tag.to_string());
                Ok(tag == "v1.5.0")
            });
            assert_eq!(*asked.borrow(), ["v1.5.0"]);
            assert!(result.update_available, "{result:?}");
            assert!(result.release_url.ends_with("/releases/tag/v1.5.0"));
        }
        // A version published only under the bare spelling is still probed.
        let bare = stable_tags(&line("1.6.0")).unwrap();
        assert_eq!(bare[0].0, "1.6.0");
    }

    #[test]
    fn newer_tags_are_never_probed_for_a_build_that_is_ahead() {
        let probes = std::cell::Cell::new(0usize);
        let behind = evaluate("2.0.0", &line("v1.9.9"), &|_, _| {
            probes.set(probes.get() + 1);
            Ok(true)
        });
        assert!(behind.checked && !behind.update_available);
        assert_eq!(
            probes.get(),
            0,
            "a tag not newer than this build is never probed"
        );
    }

    /// The marker is the file `scripts/release-state.mjs` uploads; a rename
    /// there that missed this would make every real release look unpublished.
    #[test]
    fn the_release_marker_asset_is_the_one_the_release_scripts_publish() {
        let script = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../scripts/check-release-assets.mjs"),
        )
        .expect("read check-release-assets.mjs");
        assert!(
            script.contains("checksums: `GitPulse_${version}_SHA256SUMS.txt`"),
            "scripts/check-release-assets.mjs no longer names the checksum asset this way"
        );
        assert_eq!(
            release_marker_asset(Version {
                major: 1,
                minor: 4,
                patch: 0
            }),
            "GitPulse_1.4.0_SHA256SUMS.txt"
        );
    }

    #[test]
    fn a_bad_running_version_fails_the_check() {
        let result = evaluate("not-a-version", &line("v1.0.0"), &|_, _| Ok(true));
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

    // ---- adversarial review (adv:) ----

    #[test]
    fn adv_tag_spelling_edges() {
        let tags = |l: &str| stable_tags(l).unwrap();
        // capital V, pre-release, build metadata, leading zeros, no refs/tags prefix
        assert!(tags(&line("V1.5.0")).is_empty());
        assert!(tags(&line("v1.6.0-rc.1")).is_empty());
        assert!(tags(&line("v1.6.0+meta")).is_empty());
        assert!(tags("0123456789abcdef\tv1.5.0\n").is_empty());
        assert!(tags("0123456789abcdef\trefs/heads/v1.5.0\n").is_empty());
        // peeled lines are stripped before the spelling merge: bare + peeled v collapse to the v tag
        let peeled = format!("{}{}", line("1.5.0"), line("v1.5.0^{}"));
        let t = tags(&peeled);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].0, "v1.5.0");
        // CRLF + whitespace + duplicates
        let crlf =
            "abc\trefs/tags/v1.5.0\r\n  abc\trefs/tags/v1.5.0  \r\nabc\trefs/tags/v1.5.0\r\n";
        let t = tags(crlf);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].0, "v1.5.0");
        // leading zeros: same version as v1.5.0 or rejected; must not yield two entries
        let lz = format!("{}{}", line("v01.5.0"), line("v1.5.0"));
        let t = tags(&lz);
        assert_eq!(
            t.len(),
            1,
            "leading-zero spelling duplicated a version: {t:?}"
        );
        assert_eq!(t[0].0, "v1.5.0");
    }

    #[test]
    fn adv_dedup_scales_to_the_tag_cap() {
        let n = MAX_TAG_LINES;
        let listing: String = (0..n).map(|i| line(&format!("v1.{i}.0"))).collect();
        let start = std::time::Instant::now();
        let t = stable_tags(&listing).unwrap();
        let took = start.elapsed();
        eprintln!("adv stable_tags n={n} took {took:?}");
        assert_eq!(t.len(), n);
        assert!(t.windows(2).all(|w| w[0].1 > w[1].1));
        let over: String = (0..=n).map(|i| line(&format!("v1.{i}.0"))).collect();
        assert!(stable_tags(&over).is_err(), "cap must fail loud");
    }

    #[test]
    fn adv_ordering_is_newest_first_regardless_of_listing_order() {
        let tagsv = ["v1.6.2", "v1.6.0", "v1.6.4", "1.6.1", "v1.6.3", "v1.5.0"];
        let listing: String = tagsv.iter().map(|t| line(t)).collect();
        let asked = std::cell::RefCell::new(Vec::new());
        let r = evaluate("1.4.0", &listing, &|tag, _| {
            asked.borrow_mut().push(tag.to_string());
            Ok(false)
        });
        assert_eq!(
            *asked.borrow(),
            ["v1.6.4", "v1.6.3", "v1.6.2", "1.6.1", "v1.6.0"]
        );
        assert!(!r.checked);
        // exactly-budget with one published among them is announced, from any position
        let r = evaluate("1.4.0", &listing, &|tag, _| Ok(tag == "v1.6.0"));
        assert!(r.checked && r.update_available, "{r:?}");
        assert_eq!(r.latest_version, "1.6.0");
    }

    #[test]
    fn adv_current_version_forms() {
        let listing = format!("{}{}", line("v1.5.0"), line("v1.4.0"));
        // `v` prefix on the running version is not a valid MAJOR.MINOR.PATCH: fails loud, never "current"
        let r = evaluate("v1.4.0", &listing, &|_, _| Ok(true));
        assert!(!r.checked && !r.update_available, "{r:?}");
        // pre-release build of the running version: must fail loud, never silently up to date
        let r = evaluate("1.5.0-dev", &listing, &|_, _| Ok(true));
        assert!(!r.checked && !r.update_available, "{r:?}");
    }

    // ---- fail-closed back-off ----

    const T0: u64 = 1_800_000_000;

    /// Listing for build 1.4.0 whose budget is spent: six newer unpublished
    /// tags, `tip` varying the sha of the newest one.
    fn exhausted_listing(tip: &str) -> String {
        let mut out = format!("{tip:0>40}\trefs/tags/v1.6.5\n");
        for tag in ["v1.6.4", "v1.6.3", "v1.6.2", "v1.6.1", "v1.6.0"] {
            out.push_str(&line(tag));
        }
        out
    }

    /// Runs one launch and returns (result, HTTP probe count).
    fn launch(
        version: &str,
        listing: &str,
        path: &std::path::Path,
        now: u64,
    ) -> (UpdateCheck, usize) {
        let probes = std::cell::Cell::new(0usize);
        let result = evaluate_with_backoff(
            version,
            listing,
            &|_, _| {
                probes.set(probes.get() + 1);
                Ok(false)
            },
            Some(path),
            now,
        );
        (result, probes.get())
    }

    fn record_file() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache").join("update-check.json");
        (dir, path)
    }

    #[test]
    fn a_repeat_launch_with_the_same_tip_makes_no_http_probes() {
        let (_dir, path) = record_file();
        let listing = exhausted_listing("1");
        let (first, probes) = launch("1.4.0", &listing, &path, T0);
        assert_eq!(probes, MAX_RELEASE_PROBES);
        assert!(!first.checked, "{first:?}");
        let (second, probes) = launch("1.4.0", &listing, &path, T0 + 60);
        assert_eq!(probes, 0, "the repeat launch must not probe");
        assert_eq!(second, first, "the recorded fail-closed state is replayed");
    }

    #[test]
    fn the_state_after_a_skipped_probe_is_fail_closed_never_up_to_date() {
        let (_dir, path) = record_file();
        let listing = exhausted_listing("1");
        launch("1.4.0", &listing, &path, T0);
        let (skipped, probes) = launch("1.4.0", &listing, &path, T0 + 1);
        assert_eq!(probes, 0);
        assert!(!skipped.checked && !skipped.update_available, "{skipped:?}");
        assert!(skipped.latest_version.is_empty(), "{skipped:?}");
        assert!(skipped.error.unwrap().contains("1 newer tag"));
    }

    #[test]
    fn a_changed_tip_re_probes() {
        let (_dir, path) = record_file();
        launch("1.4.0", &exhausted_listing("1"), &path, T0);
        let (result, probes) = launch("1.4.0", &exhausted_listing("2"), &path, T0 + 60);
        assert_eq!(probes, MAX_RELEASE_PROBES);
        assert!(!result.checked);
    }

    #[test]
    fn a_changed_local_version_re_probes() {
        let (_dir, path) = record_file();
        let listing = exhausted_listing("1");
        launch("1.4.0", &listing, &path, T0);
        let (_, probes) = launch("1.4.1", &listing, &path, T0 + 60);
        assert_eq!(probes, MAX_RELEASE_PROBES);
    }

    #[test]
    fn twenty_four_hours_later_re_probes() {
        let (_dir, path) = record_file();
        let listing = exhausted_listing("1");
        launch("1.4.0", &listing, &path, T0);
        let (_, probes) = launch("1.4.0", &listing, &path, T0 + FAIL_CLOSED_BACKOFF_SECS - 1);
        assert_eq!(probes, 0, "still inside the window");
        let (_, probes) = launch("1.4.0", &listing, &path, T0 + FAIL_CLOSED_BACKOFF_SECS);
        assert_eq!(probes, MAX_RELEASE_PROBES);
    }

    #[test]
    fn a_clock_that_moved_backwards_re_probes() {
        let (_dir, path) = record_file();
        let listing = exhausted_listing("1");
        launch("1.4.0", &listing, &path, T0);
        let (_, probes) = launch("1.4.0", &listing, &path, T0 - 10);
        assert_eq!(probes, MAX_RELEASE_PROBES);
    }

    #[test]
    fn a_corrupt_or_foreign_record_probes() {
        let listing = exhausted_listing("1");
        for garbage in [
            "",
            "not json",
            "{\"version\":\"1.4.0\"}",
            "{\"version\":1,\"tip\":2,\"recordedAt\":\"x\",\"error\":null}",
        ] {
            let (_dir, path) = record_file();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, garbage).unwrap();
            let (result, probes) = launch("1.4.0", &listing, &path, T0);
            assert_eq!(probes, MAX_RELEASE_PROBES, "{garbage:?} must probe");
            assert!(!result.checked && !result.update_available, "{result:?}");
        }
    }

    #[test]
    fn a_recovered_check_clears_the_record_and_a_probe_error_is_not_recorded() {
        let (_dir, path) = record_file();
        let listing = exhausted_listing("1");
        launch("1.4.0", &listing, &path, T0);
        assert!(path.exists());
        // A published release appears after the window: the check succeeds.
        let result = evaluate_with_backoff(
            "1.4.0",
            &listing,
            &|tag, _| Ok(tag == "v1.6.0" || tag == "v1.6.5"),
            Some(&path),
            T0 + FAIL_CLOSED_BACKOFF_SECS,
        );
        assert!(result.update_available, "{result:?}");
        assert!(!path.exists(), "a success clears the record");
        // A probe that errors is transient and must be retried next launch.
        let result = evaluate_with_backoff(
            "1.4.0",
            &listing,
            &|_, _| Err("curl is not installed".into()),
            Some(&path),
            T0,
        );
        assert!(!result.checked);
        assert!(!path.exists(), "an Err is never recorded");
    }

    #[test]
    fn tags_not_newer_than_the_build_do_not_change_the_tip() {
        let a = format!("{}{}", exhausted_listing("1"), line("v1.0.0"));
        let b = format!("{}{}", exhausted_listing("1"), line("v1.1.0"));
        assert_eq!(tip_fingerprint(&a, "1.4.0"), tip_fingerprint(&b, "1.4.0"));
        assert_ne!(
            tip_fingerprint(&exhausted_listing("1"), "1.4.0"),
            tip_fingerprint(&exhausted_listing("2"), "1.4.0")
        );
    }
}
