//! Git-native provenance notes under `refs/notes/gitpulse/*`.
//!
//! Stores verification records and agent session episodes directly in git notes,
//! surviving machine re-installs and syncing with git remotes.

use serde::{Deserialize, Serialize};
use std::process::Command;

pub const VERIFICATION_NOTES_REF: &str = "refs/notes/gitpulse/verification";
pub const SESSION_NOTES_REF: &str = "refs/notes/gitpulse/sessions";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VerificationNote {
    pub verdict: String,
    pub verified_at: i64,
    pub checked_by: String,
    pub task_id: Option<String>,
    pub details: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionEpisodeNote {
    pub session_id: String,
    pub actor_kind: String,
    pub transcript_path: Option<String>,
    pub created_at: i64,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvenanceFreshness {
    pub commit_sha: String,
    /// Commits between this one and the base, or `None` when it could not be
    /// measured.
    ///
    /// `None` is not zero. Zero means "nothing has moved since this was
    /// verified", which is the strongest possible statement; a failed
    /// measurement means nothing at all, and the two must never render the
    /// same. This field was a bare `u32` defaulting to 0 on failure, so an
    /// unreachable base branch — or a commit that is not an ancestor of it —
    /// reported maximum freshness.
    pub distance: Option<u32>,
    /// Decays with distance. `None` when distance could not be measured.
    pub confidence: Option<f32>,
    /// True only when the distance was measured *and* is zero.
    pub is_fresh: bool,
    /// Empty when the distance was measured; otherwise why it was not.
    pub unmeasured_reason: String,
    /// Whether the note refs could be read at all.
    ///
    /// False means we do not know whether this commit is verified — which is a
    /// different thing from knowing that it is not. Without this field, "this
    /// repository has never recorded a verification" and "its notes could not
    /// be read" arrive as the same empty answer, and a badge would have to
    /// render an unexamined commit as an unverified one.
    pub notes_readable: bool,
    pub verification: Option<VerificationNote>,
    pub session: Option<SessionEpisodeNote>,
}

/// Appends or replaces a verification note for a commit.
pub fn write_verification_note(
    repo_path: &str,
    commit_sha: &str,
    note: &VerificationNote,
) -> Result<(), String> {
    let payload = serde_json::to_string(note).map_err(|e| e.to_string())?;
    let status = Command::new("git")
        .args([
            "notes",
            &format!("--ref={VERIFICATION_NOTES_REF}"),
            "add",
            "-f",
            "-m",
            &payload,
            commit_sha,
        ])
        .current_dir(repo_path)
        .status()
        .map_err(|e| format!("Failed to execute git notes: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("git notes exited with code {status}"))
    }
}

/// Reads a verification note for a commit, if present.
pub fn read_verification_note(
    repo_path: &str,
    commit_sha: &str,
) -> Result<Option<VerificationNote>, String> {
    let output = Command::new("git")
        .args([
            "notes",
            &format!("--ref={VERIFICATION_NOTES_REF}"),
            "show",
            commit_sha,
        ])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to read git notes: {e}"))?;

    if !output.status.success() {
        return Ok(None);
    }

    let text = String::from_utf8_lossy(&output.stdout);
    match serde_json::from_str::<VerificationNote>(text.trim()) {
        Ok(note) => Ok(Some(note)),
        Err(_) => Ok(None),
    }
}

/// Appends or replaces a session episode note for a commit.
pub fn write_session_note(
    repo_path: &str,
    commit_sha: &str,
    note: &SessionEpisodeNote,
) -> Result<(), String> {
    let payload = serde_json::to_string(note).map_err(|e| e.to_string())?;
    let status = Command::new("git")
        .args([
            "notes",
            &format!("--ref={SESSION_NOTES_REF}"),
            "add",
            "-f",
            "-m",
            &payload,
            commit_sha,
        ])
        .current_dir(repo_path)
        .status()
        .map_err(|e| format!("Failed to execute git notes: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("git notes exited with code {status}"))
    }
}

/// Reads a session episode note for a commit, if present.
pub fn read_session_note(
    repo_path: &str,
    commit_sha: &str,
) -> Result<Option<SessionEpisodeNote>, String> {
    let output = Command::new("git")
        .args([
            "notes",
            &format!("--ref={SESSION_NOTES_REF}"),
            "show",
            commit_sha,
        ])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to read git notes: {e}"))?;

    if !output.status.success() {
        return Ok(None);
    }

    let text = String::from_utf8_lossy(&output.stdout);
    match serde_json::from_str::<SessionEpisodeNote>(text.trim()) {
        Ok(note) => Ok(Some(note)),
        Err(_) => Ok(None),
    }
}

/// Computes provenance freshness and confidence decay against a base branch.
pub fn compute_freshness(
    repo_path: &str,
    commit_sha: &str,
    base_branch: Option<&str>,
) -> ProvenanceFreshness {
    let base = base_branch.unwrap_or("HEAD");

    // Every failure yields `None` with a reason, never 0 — see
    // `measure_distance`, which owns that rule for both this and the batch
    // path so the two can never drift into disagreeing about it.
    let (distance, unmeasured_reason) = measure_distance(repo_path, commit_sha, base);

    let confidence = distance.map(|d| 1.0 / (1.0 + 0.1 * d as f32));
    let is_fresh = distance == Some(0);

    // `git notes show` exits non-zero both for "this commit has no note" and
    // for "the notes ref could not be read", so the read alone cannot tell the
    // two apart. Listing the refs can, and that distinction is the difference
    // between an unverified commit and an unexamined one.
    let notes_readable = noted_commits(repo_path, VERIFICATION_NOTES_REF).is_ok()
        && noted_commits(repo_path, SESSION_NOTES_REF).is_ok();
    let verification = read_verification_note(repo_path, commit_sha).unwrap_or(None);
    let session = read_session_note(repo_path, commit_sha).unwrap_or(None);

    ProvenanceFreshness {
        commit_sha: commit_sha.to_string(),
        distance,
        confidence,
        is_fresh,
        unmeasured_reason,
        notes_readable,
        verification,
        session,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn init_git_repo() -> tempfile::TempDir {
        let dir = tempdir().expect("tempdir");
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir.path())
            .output()
            .expect("git config");
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir.path())
            .output()
            .expect("git config");
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(dir.path())
            .output()
            .expect("initial commit");
        dir
    }

    #[test]
    fn write_and_read_verification_note_roundtrip() {
        let dir = init_git_repo();
        let path = dir.path().to_str().expect("utf8 path");

        let head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(path)
            .output()
            .expect("rev-parse");
        let sha = String::from_utf8_lossy(&head.stdout).trim().to_string();

        let note = VerificationNote {
            verdict: "pass".into(),
            verified_at: 1700000000,
            checked_by: "manvi".into(),
            task_id: Some("task-1".into()),
            details: Some("All tests green".into()),
        };

        write_verification_note(path, &sha, &note).expect("write note");
        let read = read_verification_note(path, &sha).expect("read note");
        assert_eq!(read, Some(note));

        let freshness = compute_freshness(path, &sha, None);
        assert_eq!(freshness.distance, Some(0));
        assert!(freshness.is_fresh);
        assert_eq!(freshness.confidence, Some(1.0));
    }
}

#[cfg(test)]
mod freshness_honesty_tests {
    use super::*;

    fn repo_with_commits(n: usize) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .expect("git");
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.name", "T"]);
        git(&["config", "user.email", "t@e.com"]);
        for i in 0..n {
            std::fs::write(dir.path().join(format!("f{i}.txt")), format!("{i}\n")).unwrap();
            git(&["add", "-A"]);
            git(&["commit", "-m", &format!("c{i}")]);
        }
        dir
    }

    fn head(dir: &std::path::Path) -> String {
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output()
            .expect("git");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// The regression this type exists to prevent.
    ///
    /// `distance` was a bare `u32` that defaulted to 0 whenever `rev-list`
    /// failed — an unreachable base, a commit that is not its ancestor, a
    /// missing repository. Zero is the strongest claim the type can make
    /// ("nothing has moved since this was verified"), so every failed
    /// measurement rendered as maximum freshness and confidence 1.0.
    #[test]
    fn an_unmeasurable_distance_is_not_a_distance_of_zero() {
        let dir = repo_with_commits(1);
        let sha = head(dir.path());

        let f = compute_freshness(
            dir.path().to_str().unwrap(),
            &sha,
            Some("no-such-branch-anywhere"),
        );
        assert_eq!(f.distance, None, "an unreachable base is not zero distance");
        assert_eq!(f.confidence, None, "no distance means no confidence");
        assert!(!f.is_fresh, "an unmeasured commit must never read as fresh");
        assert!(
            !f.unmeasured_reason.is_empty(),
            "the failure must explain itself"
        );
    }

    #[test]
    fn a_commit_at_the_tip_is_fresh() {
        let dir = repo_with_commits(1);
        let sha = head(dir.path());
        let f = compute_freshness(dir.path().to_str().unwrap(), &sha, Some("main"));
        assert_eq!(f.distance, Some(0));
        assert_eq!(f.confidence, Some(1.0));
        assert!(f.is_fresh);
        assert!(f.unmeasured_reason.is_empty());
    }

    #[test]
    fn confidence_decays_as_the_base_moves_ahead() {
        let dir = repo_with_commits(4);
        let out = Command::new("git")
            .args(["rev-parse", "HEAD~3"])
            .current_dir(dir.path())
            .output()
            .expect("git");
        let old = String::from_utf8_lossy(&out.stdout).trim().to_string();

        let f = compute_freshness(dir.path().to_str().unwrap(), &old, Some("main"));
        assert_eq!(f.distance, Some(3));
        assert!(!f.is_fresh, "three commits behind is not fresh");
        let c = f.confidence.expect("measured");
        assert!(c < 1.0 && c > 0.0, "confidence should decay, got {c}");
    }

    #[test]
    fn a_missing_repository_reports_why_rather_than_full_confidence() {
        let f = compute_freshness("/definitely/not/a/repo", "deadbeef", Some("main"));
        assert_eq!(f.distance, None);
        assert_eq!(f.confidence, None);
        assert!(!f.is_fresh);
        assert!(!f.unmeasured_reason.is_empty());
    }

    /// A note round-trips, and a commit with none is distinguishable from one
    /// whose note could not be read.
    #[test]
    fn a_verification_note_round_trips() {
        let dir = repo_with_commits(1);
        let repo = dir.path().to_str().unwrap();
        let sha = head(dir.path());

        assert_eq!(read_verification_note(repo, &sha).unwrap(), None);

        let note = VerificationNote {
            verdict: "passed".into(),
            verified_at: 1_788_000_000,
            checked_by: "ci.local".into(),
            task_id: Some("TASK-1".into()),
            details: Some("6 steps".into()),
        };
        write_verification_note(repo, &sha, &note).expect("write");
        assert_eq!(read_verification_note(repo, &sha).unwrap(), Some(note));

        // ...and it is in git, not only in our memory: a fresh clone would
        // carry it, which is the whole point of storing it here.
        let out = Command::new("git")
            .args(["notes", &format!("--ref={VERIFICATION_NOTES_REF}"), "list"])
            .current_dir(repo)
            .output()
            .expect("git");
        assert!(out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stdout).contains(&sha[..8]),
            "the note is not attached to the commit in git"
        );
    }
}

// --- batch measurement ------------------------------------------------

/// How many noted commits one batch will measure.
///
/// Each measurement is a `git rev-list` plus a `git notes show`, so an
/// unbounded batch is an unbounded fan-out of subprocesses driven by whatever
/// the caller happened to pass. Commits past the budget come back with
/// `distance: None` and a reason naming it, never as a measured zero: a capped
/// sample must never be presented as complete coverage.
pub const MAX_MEASURED_PER_BATCH: usize = 256;

/// Resolves revisions to commit shas in one pass.
///
/// `git cat-file --batch-check` reads revisions on stdin and answers one line
/// each, so a hundred branch tips cost one subprocess instead of a hundred.
/// A revision it cannot resolve answers `<input> missing`, which is reported
/// rather than dropped — a branch whose tip is not in this repository is a
/// thing we could not look at, not a thing we looked at and found clean.
fn resolve_revisions(repo_path: &str, revs: &[String]) -> Vec<Result<String, String>> {
    use std::io::Write;

    let mut child = match Command::new("git")
        .args(["cat-file", "--batch-check=%(objectname) %(objecttype)"])
        .current_dir(repo_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return revs
                .iter()
                .map(|_| Err(format!("could not run git cat-file: {e}")))
                .collect()
        }
    };

    // `^{commit}` peels annotated tags and rejects trees and blobs, so what
    // comes back is always something `rev-list` can walk from.
    let query: String = revs
        .iter()
        .map(|r| format!("{r}^{{commit}}\n"))
        .collect::<String>();
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(query.as_bytes());
    }
    drop(child.stdin.take());

    let out = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            return revs
                .iter()
                .map(|_| Err(format!("git cat-file failed: {e}")))
                .collect()
        }
    };

    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    revs.iter()
        .map(|rev| match lines.next() {
            None => Err(format!("git cat-file answered nothing for {rev:?}")),
            Some(line) => match line.split_once(' ') {
                Some((sha, "commit")) if sha.len() == 40 => Ok(sha.to_string()),
                _ => Err(format!("{rev} does not name a commit in this repository")),
            },
        })
        .collect()
}

/// Commits carrying a note on `notes_ref`, as one set.
///
/// `git notes list` answers `<note blob> <annotated commit>` for the whole ref
/// in a single call. Reading it up front is what makes a batch cheap: the
/// overwhelming majority of commits carry no note, and knowing which ones do
/// means no subprocess is spent on the ones that do not.
///
/// Returns `Err` when the listing itself failed. A ref that does not exist yet
/// is not a failure — it is a repository where nothing has been noted, and
/// answers an empty set.
fn noted_commits(
    repo_path: &str,
    notes_ref: &str,
) -> Result<std::collections::HashSet<String>, String> {
    let out = Command::new("git")
        .args(["notes", &format!("--ref={notes_ref}"), "list"])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("could not run git notes list: {e}"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // "no note found" / a missing ref is absence, not failure.
        if stderr.contains("Cannot load notes ref") || stderr.trim().is_empty() {
            return Ok(std::collections::HashSet::new());
        }
        return Err(format!("git notes list failed: {}", stderr.trim()));
    }

    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            line.split_once(' ')
                .map(|(_, commit)| commit.trim().to_string())
        })
        .collect())
}

/// Measures `commit_sha` against `base`, exactly as [`compute_freshness`] does.
fn measure_distance(repo_path: &str, commit_sha: &str, base: &str) -> (Option<u32>, String) {
    match Command::new("git")
        .args(["rev-list", "--count", &format!("{commit_sha}..{base}")])
        .current_dir(repo_path)
        .output()
    {
        Err(e) => (None, format!("could not run git rev-list: {e}")),
        Ok(out) if !out.status.success() => (
            None,
            format!(
                "git rev-list {commit_sha}..{base} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ),
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            match text.parse::<u32>() {
                Ok(n) => (Some(n), String::new()),
                Err(_) => (None, format!("git rev-list returned {text:?}, not a count")),
            }
        }
    }
}

/// Freshness for many revisions, in one pass.
///
/// Answers one entry per input, in input order, so a caller can zip the result
/// straight onto its own rows.
///
/// # Why unnoted commits are *unmeasured*, not measured
///
/// A commit carrying no provenance note has nothing to be fresh *about*: there
/// is no verification whose age could decay. Measuring its distance anyway
/// would spend a subprocess to produce a number the badge cannot use, and —
/// worse — would put a confident `distance: Some(0)` on a commit nobody ever
/// verified. Those entries come back with `distance: None` and a reason saying
/// so, which is what they are.
pub fn freshness_batch(
    repo_path: &str,
    revisions: &[String],
    base_branch: Option<&str>,
) -> Vec<ProvenanceFreshness> {
    freshness_batch_within(repo_path, revisions, base_branch, MAX_MEASURED_PER_BATCH)
}

/// [`freshness_batch`] with an explicit measurement budget.
///
/// Exists so the budget's behaviour is testable at a size a test can actually
/// build. A cap that only engages at 256 commits is a cap nothing ever
/// exercises, and an unexercised cap is how a truncated answer comes to be
/// presented as a complete one.
pub fn freshness_batch_within(
    repo_path: &str,
    revisions: &[String],
    base_branch: Option<&str>,
    budget: usize,
) -> Vec<ProvenanceFreshness> {
    let base = base_branch.unwrap_or("HEAD");
    let resolved = resolve_revisions(repo_path, revisions);

    // A failed listing is carried into every entry's reason rather than
    // silently becoming an empty set: "this repository has no verification
    // notes" and "we could not read its notes" are different facts, and only
    // the first one means the commits are genuinely unverified.
    let verified = noted_commits(repo_path, VERIFICATION_NOTES_REF);
    let sessioned = noted_commits(repo_path, SESSION_NOTES_REF);
    let listing_error = match (&verified, &sessioned) {
        (Err(e), _) | (_, Err(e)) => Some(e.clone()),
        _ => None,
    };
    let verified = verified.unwrap_or_default();
    let sessioned = sessioned.unwrap_or_default();

    let mut measured = 0usize;
    resolved
        .into_iter()
        .zip(revisions)
        .map(|(resolution, rev)| {
            let sha = match resolution {
                Ok(sha) => sha,
                Err(reason) => {
                    return ProvenanceFreshness {
                        commit_sha: rev.clone(),
                        distance: None,
                        confidence: None,
                        is_fresh: false,
                        unmeasured_reason: reason,
                        notes_readable: false,
                        verification: None,
                        session: None,
                    }
                }
            };

            let has_verification = verified.contains(&sha);
            let has_session = sessioned.contains(&sha);

            if let Some(err) = &listing_error {
                return ProvenanceFreshness {
                    commit_sha: sha,
                    distance: None,
                    confidence: None,
                    is_fresh: false,
                    unmeasured_reason: err.clone(),
                    notes_readable: false,
                    verification: None,
                    session: None,
                };
            }

            if !has_verification && !has_session {
                return ProvenanceFreshness {
                    commit_sha: sha,
                    distance: None,
                    confidence: None,
                    is_fresh: false,
                    unmeasured_reason: "not measured: this commit carries no provenance note"
                        .to_string(),
                    notes_readable: true,
                    verification: None,
                    session: None,
                };
            }

            if measured >= budget {
                return ProvenanceFreshness {
                    commit_sha: sha,
                    distance: None,
                    confidence: None,
                    is_fresh: false,
                    unmeasured_reason: format!(
                        "not measured: past this request's budget of {budget} noted commits"
                    ),
                    notes_readable: true,
                    verification: None,
                    session: None,
                };
            }
            measured += 1;

            let (distance, unmeasured_reason) = measure_distance(repo_path, &sha, base);
            ProvenanceFreshness {
                distance,
                confidence: distance.map(|d| 1.0 / (1.0 + 0.1 * d as f32)),
                is_fresh: distance == Some(0),
                unmeasured_reason,
                notes_readable: true,
                verification: if has_verification {
                    read_verification_note(repo_path, &sha).unwrap_or(None)
                } else {
                    None
                },
                session: if has_session {
                    read_session_note(repo_path, &sha).unwrap_or(None)
                } else {
                    None
                },
                commit_sha: sha,
            }
        })
        .collect()
}

#[cfg(test)]
mod batch_tests {
    use super::*;

    struct Repo(tempfile::TempDir);

    impl Repo {
        fn new(commits: usize) -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let repo = Repo(dir);
            repo.git(&["init", "-b", "main"]);
            repo.git(&["config", "user.name", "T"]);
            repo.git(&["config", "user.email", "t@e.com"]);
            for i in 0..commits {
                std::fs::write(repo.path().join(format!("f{i}")), format!("{i}\n")).unwrap();
                repo.git(&["add", "-A"]);
                repo.git(&["commit", "-m", &format!("c{i}")]);
            }
            repo
        }

        fn path(&self) -> &std::path::Path {
            self.0.path()
        }

        fn as_str(&self) -> &str {
            self.path().to_str().expect("utf8")
        }

        fn git(&self, args: &[&str]) {
            let out = Command::new("git")
                .args(args)
                .current_dir(self.path())
                .output()
                .expect("git");
            assert!(out.status.success(), "git {args:?}: {out:?}");
        }

        fn rev(&self, spec: &str) -> String {
            let out = Command::new("git")
                .args(["rev-parse", spec])
                .current_dir(self.path())
                .output()
                .expect("git");
            assert!(out.status.success(), "rev-parse {spec}: {out:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }

        fn verify(&self, spec: &str, verdict: &str) {
            let sha = self.rev(spec);
            write_verification_note(
                self.as_str(),
                &sha,
                &VerificationNote {
                    verdict: verdict.into(),
                    verified_at: 1_788_000_000,
                    checked_by: "ci.local".into(),
                    task_id: Some("TASK-1".into()),
                    details: None,
                },
            )
            .expect("write note");
        }
    }

    /// The invariant the whole batch path exists to keep.
    ///
    /// The cheap way to write this function is to measure every input and let
    /// unnoted commits fall out as `distance: Some(0), is_fresh: true` because
    /// they happen to be at the tip. That renders a commit nobody has ever
    /// verified with the same badge as one verified against this exact tree.
    #[test]
    fn an_unverified_commit_is_never_fresh_even_at_the_tip() {
        let repo = Repo::new(1);
        let tip = repo.rev("HEAD");

        let got = freshness_batch(repo.as_str(), std::slice::from_ref(&tip), Some("main"));
        assert_eq!(got.len(), 1);
        let f = &got[0];

        assert_eq!(f.commit_sha, tip);
        assert!(
            f.verification.is_none(),
            "nothing was ever verified in this repository"
        );
        assert!(
            !f.is_fresh,
            "a commit with no verification has no freshness to report"
        );
        assert_eq!(f.distance, None);
        assert_eq!(f.confidence, None);
        assert!(
            f.unmeasured_reason.contains("no provenance note"),
            "the reason must say why, got {:?}",
            f.unmeasured_reason
        );
    }

    #[test]
    fn a_verified_tip_is_fresh_and_a_verified_ancestor_decays() {
        let repo = Repo::new(4);
        repo.verify("HEAD", "passed");
        repo.verify("HEAD~3", "passed");

        let got = freshness_batch(
            repo.as_str(),
            &[repo.rev("HEAD"), repo.rev("HEAD~3")],
            Some("main"),
        );

        assert_eq!(got[0].distance, Some(0));
        assert!(got[0].is_fresh);
        assert_eq!(got[0].confidence, Some(1.0));
        assert_eq!(
            got[0].verification.as_ref().map(|v| v.verdict.as_str()),
            Some("passed")
        );

        assert_eq!(got[1].distance, Some(3));
        assert!(!got[1].is_fresh, "three commits behind is not fresh");
        let c = got[1].confidence.expect("measured");
        assert!(c < 1.0 && c > 0.0, "confidence should decay, got {c}");
        assert!(got[1].unmeasured_reason.is_empty());
    }

    /// The batch is a faster path to the same answer, not a different answer.
    #[test]
    fn the_batch_agrees_with_the_single_measurement() {
        let repo = Repo::new(3);
        repo.verify("HEAD~2", "passed");
        let sha = repo.rev("HEAD~2");

        let single = compute_freshness(repo.as_str(), &sha, Some("main"));
        let batched = freshness_batch(repo.as_str(), &[sha], Some("main"))
            .pop()
            .expect("one entry");

        assert_eq!(single, batched);
    }

    /// Pull requests arrive as ref names, not shas.
    #[test]
    fn a_ref_name_measures_the_same_as_the_sha_it_points_at() {
        let repo = Repo::new(2);
        repo.git(&["branch", "feature/x"]);
        repo.verify("HEAD", "passed");

        let by_name = freshness_batch(repo.as_str(), &["feature/x".to_string()], Some("main"));
        let by_sha = freshness_batch(repo.as_str(), &[repo.rev("HEAD")], Some("main"));

        assert_eq!(by_name, by_sha);
        assert_eq!(
            by_name[0].commit_sha,
            repo.rev("HEAD"),
            "the answer reports the resolved commit, not the name asked for"
        );
    }

    /// A ref that is not here must not silently shift the results under it.
    #[test]
    fn an_unresolvable_revision_reports_itself_and_holds_its_place() {
        let repo = Repo::new(1);
        repo.verify("HEAD", "passed");
        let tip = repo.rev("HEAD");

        let got = freshness_batch(
            repo.as_str(),
            &[
                "no-such-ref".to_string(),
                tip.clone(),
                "also-missing".to_string(),
            ],
            Some("main"),
        );

        assert_eq!(got.len(), 3, "one answer per input, always");
        assert_eq!(got[0].commit_sha, "no-such-ref");
        assert_eq!(got[0].distance, None);
        assert!(!got[0].is_fresh);
        assert!(got[0].unmeasured_reason.contains("no-such-ref"));

        assert_eq!(got[1].commit_sha, tip, "the resolvable one kept its slot");
        assert!(got[1].is_fresh);

        assert_eq!(got[2].commit_sha, "also-missing");
        assert!(!got[2].is_fresh);
    }

    #[test]
    fn the_measurement_budget_is_reported_rather_than_silently_applied() {
        let repo = Repo::new(3);
        repo.verify("HEAD", "passed");
        repo.verify("HEAD~1", "passed");
        repo.verify("HEAD~2", "passed");

        let revs = vec![repo.rev("HEAD"), repo.rev("HEAD~1"), repo.rev("HEAD~2")];
        let got = freshness_batch_within(repo.as_str(), &revs, Some("main"), 2);

        assert_eq!(got.len(), 3, "a capped batch still answers every input");
        assert_eq!(got[0].distance, Some(0));
        assert_eq!(got[1].distance, Some(1));

        assert_eq!(got[2].distance, None, "past the budget is not measured");
        assert!(!got[2].is_fresh);
        assert!(
            got[2].unmeasured_reason.contains("budget of 2"),
            "the cap must name itself, got {:?}",
            got[2].unmeasured_reason
        );
    }

    /// A session note alone is enough to be worth measuring: the commit was
    /// written by an agent, and how far the base has moved since is the whole
    /// question. It is still not *verified*.
    #[test]
    fn a_session_note_is_measured_but_is_not_a_verification() {
        let repo = Repo::new(2);
        let sha = repo.rev("HEAD");
        write_session_note(
            repo.as_str(),
            &sha,
            &SessionEpisodeNote {
                session_id: "S1".into(),
                actor_kind: "agent".into(),
                transcript_path: None,
                created_at: 1_788_000_000,
                summary: Some("wrote the batch path".into()),
            },
        )
        .expect("write session note");

        let f = freshness_batch(repo.as_str(), &[sha], Some("main"))
            .pop()
            .expect("one");
        assert_eq!(f.distance, Some(0), "a noted commit gets measured");
        assert!(f.session.is_some());
        assert!(
            f.verification.is_none(),
            "an agent having touched it is not a verification of it"
        );
    }

    #[test]
    fn an_empty_request_costs_nothing_and_answers_nothing() {
        let repo = Repo::new(1);
        assert!(freshness_batch(repo.as_str(), &[], Some("main")).is_empty());
    }

    /// A repository with no notes ref at all is the ordinary case, and must not
    /// look like a repository whose notes could not be read.
    #[test]
    fn a_repository_that_has_never_been_noted_reads_cleanly() {
        let repo = Repo::new(1);
        assert_eq!(
            noted_commits(repo.as_str(), VERIFICATION_NOTES_REF),
            Ok(std::collections::HashSet::new())
        );
    }

    /// "Never verified" and "we could not tell" must not be the same answer.
    ///
    /// `git notes show` exits non-zero for both, so anything inferring
    /// verification state from that read alone reports a repository with an
    /// unreadable notes ref as one where nothing was ever verified — a check
    /// that could not run, rendering as a check that ran and found nothing.
    #[test]
    fn an_unreadable_notes_ref_is_not_an_absence_of_notes() {
        let repo = Repo::new(1);
        let sha = repo.rev("HEAD");

        // A repository nobody has noted: readable, and genuinely empty.
        let clean = freshness_batch(repo.as_str(), std::slice::from_ref(&sha), Some("main"))
            .pop()
            .expect("one");
        assert!(clean.notes_readable, "an absent ref is readable emptiness");
        assert!(clean.verification.is_none());
        assert!(compute_freshness(repo.as_str(), &sha, Some("main")).notes_readable);

        // Now point the ref at an object that is not there.
        let refs = repo.path().join(".git/refs/notes/gitpulse");
        std::fs::create_dir_all(&refs).expect("mkdir");
        std::fs::write(
            refs.join("verification"),
            "0000000000000000000000000000000000000001\n",
        )
        .expect("write ref");

        let broken = freshness_batch(repo.as_str(), std::slice::from_ref(&sha), Some("main"))
            .pop()
            .expect("one");
        assert!(
            !broken.notes_readable,
            "a ref we could not load must not report as readable"
        );
        assert!(broken.verification.is_none());
        assert!(!broken.is_fresh);
        assert!(
            !broken.unmeasured_reason.is_empty(),
            "the failure must explain itself"
        );

        assert!(!compute_freshness(repo.as_str(), &sha, Some("main")).notes_readable);
    }

    #[test]
    fn an_unresolvable_revision_never_claims_the_notes_were_read() {
        let repo = Repo::new(1);
        let got = freshness_batch(repo.as_str(), &["nope".to_string()], Some("main"));
        assert!(
            !got[0].notes_readable,
            "we never got as far as looking at its notes"
        );
    }

    #[test]
    fn an_unmeasurable_base_is_reported_for_a_noted_commit() {
        let repo = Repo::new(1);
        repo.verify("HEAD", "passed");
        let f = freshness_batch(repo.as_str(), &[repo.rev("HEAD")], Some("no-such-base"))
            .pop()
            .expect("one");

        assert!(
            f.verification.is_some(),
            "the note is still readable even when the distance is not"
        );
        assert_eq!(f.distance, None);
        assert_eq!(f.confidence, None);
        assert!(!f.is_fresh);
        assert!(f.unmeasured_reason.contains("rev-list"));
    }
}
