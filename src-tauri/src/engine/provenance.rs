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

    // Every failure below yields `None` with a reason, never 0. A distance of
    // zero is the strongest claim this type can make — "nothing has moved since
    // this commit was verified" — and handing that to a caller because git
    // could not answer is how a stale badge comes to read as a fresh one.
    let (distance, unmeasured_reason) = match Command::new("git")
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
    };

    let confidence = distance.map(|d| 1.0 / (1.0 + 0.1 * d as f32));
    let is_fresh = distance == Some(0);

    let verification = read_verification_note(repo_path, commit_sha).unwrap_or(None);
    let session = read_session_note(repo_path, commit_sha).unwrap_or(None);

    ProvenanceFreshness {
        commit_sha: commit_sha.to_string(),
        distance,
        confidence,
        is_fresh,
        unmeasured_reason,
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
