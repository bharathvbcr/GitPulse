//! Turn Kingfisher's envelope into the report the panel renders.
//!
//! Everything here is pure over its inputs — the Git listings arrive as a
//! [`GitView`] — so ordering, capping, grouping and classification are tested
//! without spawning anything. [`git_view`] is the one function that reads Git.

use super::parse::{
    Envelope, RawFinding, ScanCompleteness, SecretFinding, SecretLocation, SecretsReport,
};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path};

/// Rows carried across IPC. Kingfisher's output is already bounded by the
/// stdout cap, but tens of thousands of rows would cost the webview far more
/// than the scan did; past this the panel says how many it is not showing.
pub const MAX_FINDINGS: usize = 2_000;

/// What Git says about the working tree, read once per scan.
#[derive(Debug, Default, Clone)]
pub(crate) struct GitView {
    /// `git ls-files --cached`, repo-relative.
    pub tracked: HashSet<String>,
    /// `git ls-files --others --ignored --exclude-standard --directory`:
    /// ignored files, and wholly ignored directories with a trailing `/`.
    pub ignored: HashSet<String>,
}

impl GitView {
    fn is_ignored(&self, rel: &str) -> bool {
        if self.ignored.contains(rel) {
            return true;
        }
        // `--directory` collapses a wholly ignored directory to one `dir/`
        // entry, so a file deep inside `target/` is ignored by an ancestor.
        rel.match_indices('/')
            .any(|(at, _)| self.ignored.contains(&rel[..=at]))
    }
}

/// Read the two listings [`GitView`] needs. Both go through the trusted Git
/// seam; either failing makes the whole view unavailable rather than half
/// right, because a missing ignore list would turn every ignored file into a
/// confident "untracked".
pub(crate) fn git_view(repo: &Path) -> Result<GitView, String> {
    use crate::engine::git_cli::git;
    let tracked = git(repo, &["ls-files", "-z", "--cached"])?;
    let ignored = git(
        repo,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
        ],
    )?;
    Ok(GitView {
        tracked: nul_set(&tracked),
        ignored: nul_set(&ignored),
    })
}

fn nul_set(bytes: &[u8]) -> HashSet<String> {
    bytes
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// Kingfisher prints `<scan root>/<relative path>` using the root we passed.
/// Compared component-wise, so `/repo` never claims `/repo-other/x`.
fn relativize(root: &Path, printed: &str) -> Option<String> {
    let rel = Path::new(printed).strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            // `..` or a second root inside a "relative" remainder is not a
            // path under the scan root, whatever its prefix says.
            _ => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

/// Location for a repo-relative path. Structural answers (`.git`, nested
/// repositories) come from the filesystem and need no listing; the rest
/// need `view`, and are `Unknown` without it.
fn locate(
    root: &Path,
    rel: &str,
    view: Option<&GitView>,
    nested: &mut HashMap<String, bool>,
) -> SecretLocation {
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.contains(&".git") {
        return SecretLocation::GitMetadata;
    }
    // Any strict ancestor holding its own `.git` (directory for a nested
    // clone, file for a submodule or linked worktree) owns this path.
    let mut prefix = String::new();
    for part in &parts[..parts.len().saturating_sub(1)] {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(part);
        let is_repo = *nested
            .entry(prefix.clone())
            .or_insert_with(|| std::fs::symlink_metadata(root.join(&prefix).join(".git")).is_ok());
        if is_repo {
            return SecretLocation::NestedRepo;
        }
    }
    let Some(view) = view else {
        return SecretLocation::Unknown;
    };
    if view.tracked.contains(rel) {
        SecretLocation::Tracked
    } else if view.is_ignored(rel) {
        SecretLocation::Ignored
    } else {
        SecretLocation::Untracked
    }
}

/// Most actionable first: a credential in `.git/config` is read by every Git
/// command and pushed nowhere, a tracked one is (or will be) in history, an
/// unknown one might be either; ignored build output comes last.
fn rank(location: SecretLocation) -> u8 {
    match location {
        SecretLocation::GitMetadata => 0,
        SecretLocation::Tracked => 1,
        SecretLocation::Unknown => 2,
        SecretLocation::Untracked => 3,
        SecretLocation::NestedRepo => 4,
        SecretLocation::Ignored => 5,
        SecretLocation::Outside => 6,
    }
}

fn confidence_rank(confidence: &str) -> u8 {
    match confidence {
        "high" => 0,
        "medium" => 1,
        "low" => 2,
        _ => 3,
    }
}

/// Facts about the run that the envelope does not carry.
pub(crate) struct RunFacts {
    pub scanned_at_ms: u64,
    pub duration_ms: u64,
    pub nested_repos_scanned: bool,
    pub max_file_size_mb: u32,
}

/// Assemble the IPC report. `view` is `Err` when the Git listings could not
/// be read; the scan's findings still stand, located as far as the
/// filesystem alone allows.
pub(crate) fn assemble(
    envelope: Envelope,
    root: &Path,
    view: Result<GitView, String>,
    facts: RunFacts,
) -> SecretsReport {
    let view = view.ok();
    let mut nested = HashMap::new();
    // Keyed by rule as well: the hash is 32 bits, and scoping it to one rule
    // keeps a collision between unrelated credentials from pairing them.
    let mut groups: HashMap<(String, String), u32> = HashMap::new();
    let total = envelope.findings.len();

    let mut rows: Vec<SecretFinding> = envelope
        .findings
        .into_iter()
        .map(|raw: RawFinding| {
            let (path, location) = match relativize(root, &raw.path) {
                Some(rel) => {
                    let location = locate(root, &rel, view.as_ref(), &mut nested);
                    (rel, location)
                }
                None => (raw.path, SecretLocation::Outside),
            };
            // Ordinals follow first appearance in Kingfisher's own order, so
            // the same output always groups the same way.
            let secret_group = match raw.value_key {
                Some(key) => {
                    let next = groups.len() as u32 + 1;
                    *groups.entry((raw.rule_id.clone(), key)).or_insert(next)
                }
                None => 0,
            };
            SecretFinding {
                rule_id: raw.rule_id,
                rule_name: raw.rule_name,
                path,
                line: raw.line,
                confidence: raw.confidence,
                location,
                secret_group,
            }
        })
        .collect();

    // Sort before capping so the cap drops the least actionable rows.
    rows.sort_by(|a, b| {
        rank(a.location)
            .cmp(&rank(b.location))
            .then_with(|| confidence_rank(&a.confidence).cmp(&confidence_rank(&b.confidence)))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.rule_id.cmp(&b.rule_id))
    });
    rows.truncate(MAX_FINDINGS);

    let (ok, error) = if envelope.failed {
        (
            false,
            Some("Kingfisher reported that the scan failed. Findings below are a floor.".into()),
        )
    } else {
        (true, None)
    };

    SecretsReport {
        ok,
        error,
        diagnostic: None,
        kingfisher_present: true,
        kingfisher_version: envelope.version,
        nested_repos_scanned: facts.nested_repos_scanned,
        completeness: if envelope.failed {
            ScanCompleteness::Partial
        } else {
            envelope.completeness
        },
        findings: rows,
        findings_total: u32::try_from(total).unwrap_or(u32::MAX),
        findings_unreadable: envelope.unreadable,
        findings_truncated: envelope.omitted,
        git_status_known: view.is_some(),
        max_file_size_mb: facts.max_file_size_mb,
        scanned_at_ms: facts.scanned_at_ms,
        duration_ms: facts.duration_ms,
    }
}
