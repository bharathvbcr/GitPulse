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
    pub distance: u32,
    pub confidence: f32,
    pub is_fresh: bool,
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
    let count_output = Command::new("git")
        .args(["rev-list", "--count", &format!("{commit_sha}..{base}")])
        .current_dir(repo_path)
        .output();

    let distance = count_output
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<u32>()
                .ok()
        })
        .unwrap_or(0);

    let confidence = 1.0 / (1.0 + 0.1 * distance as f32);
    let is_fresh = distance == 0;

    let verification = read_verification_note(repo_path, commit_sha).unwrap_or(None);
    let session = read_session_note(repo_path, commit_sha).unwrap_or(None);

    ProvenanceFreshness {
        commit_sha: commit_sha.to_string(),
        distance,
        confidence,
        is_fresh,
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
        assert_eq!(freshness.distance, 0);
        assert!(freshness.is_fresh);
        assert_eq!(freshness.confidence, 1.0);
    }
}
