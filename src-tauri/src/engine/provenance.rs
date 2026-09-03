//! Git-native provenance notes under `refs/notes/gitpulse/*`.
//!
//! Stores verification records and agent session episodes directly in git notes,
//! surviving machine re-installs and syncing with git remotes.
//!
//! Every `git` this module runs goes through [`crate::engine::git_cli`] rather
//! than `Command::new`. That seam owns the spawn gate, the command timeout,
//! the stdout/stderr caps, the scrubbed environment and the GUI-launch program
//! lookup; a second, ungated spawn path here was a way for a workspace with
//! several repositories open to walk back into the "Too many open files" storm
//! that [`crate::limits`] and the gate exist to prevent.

use crate::engine::git_cli::{git, git_captured, git_captured_with_stdin, validate_repo};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

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

/// One `--ref=` argument, so the two note refs cannot drift into being spelled
/// differently at different call sites.
fn ref_arg(notes_ref: &str) -> String {
    format!("--ref={notes_ref}")
}

/// Writes `payload` as `commit_sha`'s note on `notes_ref`, replacing any note
/// already there.
///
/// Both note kinds share this: two copies of the same `git notes add` is two
/// places for a flag or a failure check to be got wrong in only one of them.
fn write_note(repo: &Path, notes_ref: &str, commit_sha: &str, payload: &str) -> Result<(), String> {
    let ref_arg = ref_arg(notes_ref);
    git(
        repo,
        &["notes", &ref_arg, "add", "-f", "-m", payload, commit_sha],
    )
    .map(|_| ())
    .map_err(|e| format!("git notes add failed: {e}"))
}

/// Reads and decodes `commit_sha`'s note on `notes_ref`.
///
/// Three outcomes, deliberately kept apart:
///
/// * `Ok(Some(note))` — a note is there and it decoded.
/// * `Ok(None)` — git looked, and this object carries no note.
/// * `Err(reason)` — we could not look, or looked and could not read what was
///   there: a spawn that failed, a stream cut off at the output cap, a note
///   that is not this app's JSON.
///
/// The third case used to be folded into the second. A commit whose note could
/// not be read is *unexamined*, not *unverified*, and answering `None` for it
/// puts a confident "no verification" badge on a commit that carries one.
fn read_note<T: DeserializeOwned>(
    repo: &Path,
    notes_ref: &str,
    commit_sha: &str,
) -> Result<Option<T>, String> {
    let ref_arg = ref_arg(notes_ref);
    let run = git_captured(repo, &["notes", &ref_arg, "show", commit_sha])?;
    if !run.success {
        let stderr = String::from_utf8_lossy(&run.stderr).trim().to_string();
        // The one non-zero exit that means absence rather than failure. Any
        // other one (a bad object, an unreadable ref, a broken repository) is
        // reported, so it cannot pass for "nothing was ever recorded here".
        if stderr.contains("no note found") {
            return Ok(None);
        }
        return Err(if stderr.is_empty() {
            format!("git notes show exited with status {}", run.status_code)
        } else {
            stderr
        });
    }
    if run.truncated {
        return Err(format!(
            "the note on {notes_ref} for {commit_sha} exceeded the output cap; \
             a prefix of it is not the note"
        ));
    }
    let text = String::from_utf8_lossy(&run.stdout);
    serde_json::from_str::<T>(text.trim())
        .map(Some)
        .map_err(|e| format!("the note on {notes_ref} for {commit_sha} did not decode: {e}"))
}

/// Appends or replaces a verification note for a commit.
pub fn write_verification_note(
    repo_path: &str,
    commit_sha: &str,
    note: &VerificationNote,
) -> Result<(), String> {
    let repo = validate_repo(repo_path)?;
    let payload = serde_json::to_string(note).map_err(|e| e.to_string())?;
    write_note(&repo, VERIFICATION_NOTES_REF, commit_sha, &payload)
}

/// Reads a verification note for a commit. See [`read_note`] for what each
/// outcome means — in particular, why a failed read is not `Ok(None)`.
pub fn read_verification_note(
    repo_path: &str,
    commit_sha: &str,
) -> Result<Option<VerificationNote>, String> {
    let repo = validate_repo(repo_path)?;
    read_note(&repo, VERIFICATION_NOTES_REF, commit_sha)
}

/// Appends or replaces a session episode note for a commit.
pub fn write_session_note(
    repo_path: &str,
    commit_sha: &str,
    note: &SessionEpisodeNote,
) -> Result<(), String> {
    let repo = validate_repo(repo_path)?;
    let payload = serde_json::to_string(note).map_err(|e| e.to_string())?;
    write_note(&repo, SESSION_NOTES_REF, commit_sha, &payload)
}

/// Reads a session episode note for a commit.
pub fn read_session_note(
    repo_path: &str,
    commit_sha: &str,
) -> Result<Option<SessionEpisodeNote>, String> {
    let repo = validate_repo(repo_path)?;
    read_note(&repo, SESSION_NOTES_REF, commit_sha)
}

impl ProvenanceFreshness {
    /// The answer for a commit nothing could be established about: no
    /// distance, no confidence, not fresh, and *not* "the notes were read".
    ///
    /// Every failure path builds its answer here rather than writing the
    /// struct out again, so none of them can quietly ship a `distance: 0` or
    /// a `notes_readable: true` that the failure did not earn.
    fn unexamined(commit_sha: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            commit_sha: commit_sha.into(),
            distance: None,
            confidence: None,
            is_fresh: false,
            unmeasured_reason: reason.into(),
            notes_readable: false,
            verification: None,
            session: None,
        }
    }

    /// As [`Self::unexamined`], for the cases where the notes *were* read and
    /// the distance simply was not measured — an unnoted commit, or one past
    /// the batch's measurement budget.
    fn unmeasured(commit_sha: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            notes_readable: true,
            ..Self::unexamined(commit_sha, reason)
        }
    }
}

/// Computes provenance freshness and confidence decay against a base branch.
pub fn compute_freshness(
    repo_path: &str,
    commit_sha: &str,
    base_branch: Option<&str>,
) -> ProvenanceFreshness {
    let base = base_branch.unwrap_or("HEAD");
    let repo = match validate_repo(repo_path) {
        Ok(repo) => repo,
        Err(e) => return ProvenanceFreshness::unexamined(commit_sha, e),
    };

    // Every failure yields `None` with a reason, never 0 — see
    // `measure_distance`, which owns that rule for both this and the batch
    // path so the two can never drift into disagreeing about it.
    let (distance, unmeasured_reason) = measure_distance(&repo, commit_sha, base);

    let confidence = distance.map(|d| 1.0 / (1.0 + 0.1 * d as f32));
    let is_fresh = distance == Some(0);

    // `git notes show` exits non-zero both for "this commit has no note" and
    // for "the notes ref could not be read", so the read alone cannot tell the
    // two apart. Listing the refs can, and that distinction is the difference
    // between an unverified commit and an unexamined one.
    let listable = noted_commits(&repo, VERIFICATION_NOTES_REF).is_ok()
        && noted_commits(&repo, SESSION_NOTES_REF).is_ok();
    let verification = read_note::<VerificationNote>(&repo, VERIFICATION_NOTES_REF, commit_sha);
    let session = read_note::<SessionEpisodeNote>(&repo, SESSION_NOTES_REF, commit_sha);
    // A read that failed is not an absence. Folding it into `None` while still
    // claiming the notes were readable is precisely the lie this flag exists
    // to prevent, so either failing read clears it.
    let notes_readable = listable && verification.is_ok() && session.is_ok();

    ProvenanceFreshness {
        commit_sha: commit_sha.to_string(),
        distance,
        confidence,
        is_fresh,
        unmeasured_reason,
        notes_readable,
        verification: verification.unwrap_or(None),
        session: session.unwrap_or(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
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
    use std::process::Command;

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

/// Upper bound on how many revisions one batch will even look up.
///
/// The revision list arrives from the webview and from MCP callers, so its
/// length is whatever the caller passed. Resolution is one `git cat-file` for
/// the whole list, but the query, the answer and the result vector all scale
/// with it. Rows past the bound answer with a reason naming it — never as a
/// measured zero, and never by silently shortening the caller's list.
pub const MAX_RESOLVED_PER_BATCH: usize = 4_096;

/// Longest revision this will put on the wire. Comfortably past a 40-character
/// sha or any real ref name.
const MAX_REVISION_BYTES: usize = 512;

/// Why `rev` cannot be sent to `git cat-file`, if it cannot.
///
/// `--batch-check` is a line protocol: one answer per line of input. A
/// revision carrying a newline would consume two answer lines and slide every
/// later revision's answer up a row — one commit's provenance rendered on
/// another commit's badge, with nothing anywhere reporting a problem. Refusing
/// the input is the only way to keep the answer stream aligned, so a control
/// character is rejected rather than stripped: repairing it would send a
/// lookup for a revision the caller never asked about.
fn unsendable_revision(rev: &str) -> Option<String> {
    if rev.trim().is_empty() {
        return Some("blank revision".to_string());
    }
    if rev.len() > MAX_REVISION_BYTES {
        return Some(format!(
            "{rev:.32}… is longer than the {MAX_REVISION_BYTES}-byte revision limit"
        ));
    }
    if let Some(c) = rev.chars().find(|c| c.is_control()) {
        return Some(format!(
            "{rev:?} contains a control character ({c:?}) and was not looked up"
        ));
    }
    None
}

/// Resolves revisions to commit shas in one pass.
///
/// `git cat-file --batch-check` reads revisions on stdin and answers one line
/// each, so a hundred branch tips cost one subprocess instead of a hundred.
/// A revision it cannot resolve answers `<input> missing`, which is reported
/// rather than dropped — a branch whose tip is not in this repository is a
/// thing we could not look at, not a thing we looked at and found clean.
///
/// Answers land in input order, one per input, whether or not the revision was
/// sendable: `sent` carries each answered line back to the row it belongs to,
/// so a refused revision costs its own row a reason and no other row anything.
///
/// `limit` is a parameter rather than a constant read in place so a test can
/// drive the cap at a size it can actually build: a bound that only engages at
/// four thousand revisions is a bound nothing ever exercises.
fn resolve_revisions(repo: &Path, revs: &[String], limit: usize) -> Vec<Result<String, String>> {
    let mut answers: Vec<Result<String, String>> = Vec::with_capacity(revs.len());
    let mut sent: Vec<usize> = Vec::new();
    let mut query = String::new();

    for (row, rev) in revs.iter().enumerate() {
        if row >= limit {
            answers.push(Err(format!(
                "not looked up: past this request's limit of {limit} revisions"
            )));
            continue;
        }
        match unsendable_revision(rev) {
            Some(reason) => answers.push(Err(reason)),
            None => {
                // `^{commit}` peels annotated tags and rejects trees and blobs,
                // so what comes back is always something `rev-list` can walk.
                query.push_str(rev);
                query.push_str("^{commit}\n");
                sent.push(row);
                // Replaced below by whatever git answered. Left as the honest
                // default so a short answer stream cannot leave a row looking
                // resolved.
                answers.push(Err(format!("git cat-file answered nothing for {rev:?}")));
            }
        }
    }
    if sent.is_empty() {
        return answers;
    }

    let run = match git_captured_with_stdin(
        repo,
        &["cat-file", "--batch-check=%(objectname) %(objecttype)"],
        query.as_bytes(),
    ) {
        Ok(run) => run,
        Err(e) => {
            for row in sent {
                answers[row] = Err(format!("could not run git cat-file: {e}"));
            }
            return answers;
        }
    };
    // A cut-off answer stream is not a short one: the lines that did arrive
    // may be complete, but there is no way to tell which row the cut fell in,
    // so none of the sent rows may claim a resolution from it.
    if run.truncated {
        for row in sent {
            answers[row] = Err("git cat-file output was truncated".to_string());
        }
        return answers;
    }

    let text = String::from_utf8_lossy(&run.stdout);
    for (line, row) in text.lines().zip(sent) {
        answers[row] = match line.split_once(' ') {
            Some((sha, "commit")) if sha.len() == 40 => Ok(sha.to_string()),
            _ => Err(format!(
                "{} does not name a commit in this repository",
                revs[row]
            )),
        };
    }
    answers
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
fn noted_commits(repo: &Path, notes_ref: &str) -> Result<HashSet<String>, String> {
    let ref_arg = ref_arg(notes_ref);
    let run = git_captured(repo, &["notes", &ref_arg, "list"])?;

    if !run.success {
        let stderr = String::from_utf8_lossy(&run.stderr).trim().to_string();
        // "no note found" / a missing ref is absence, not failure.
        if stderr.contains("Cannot load notes ref") || stderr.is_empty() {
            return Ok(HashSet::new());
        }
        return Err(format!("git notes list failed: {stderr}"));
    }
    // A truncated listing would report noted commits as unnoted, which reads
    // downstream as "this commit was never verified".
    if run.truncated {
        return Err(format!(
            "git notes list output for {notes_ref} was truncated; \
             the set of noted commits would be incomplete"
        ));
    }

    Ok(String::from_utf8_lossy(&run.stdout)
        .lines()
        .filter_map(|line| {
            line.split_once(' ')
                .map(|(_, commit)| commit.trim().to_string())
        })
        .collect())
}

/// Measures `commit_sha` against `base`, exactly as [`compute_freshness`] does.
fn measure_distance(repo: &Path, commit_sha: &str, base: &str) -> (Option<u32>, String) {
    let range = format!("{commit_sha}..{base}");
    match git_captured(repo, &["rev-list", "--count", &range]) {
        Err(e) => (None, format!("could not run git rev-list: {e}")),
        Ok(run) if !run.success => (
            None,
            format!(
                "git rev-list {range} failed: {}",
                String::from_utf8_lossy(&run.stderr).trim()
            ),
        ),
        Ok(run) => {
            let text = String::from_utf8_lossy(&run.stdout).trim().to_string();
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
    let repo = match validate_repo(repo_path) {
        Ok(repo) => repo,
        Err(e) => {
            return revisions
                .iter()
                .map(|rev| ProvenanceFreshness::unexamined(rev.clone(), e.clone()))
                .collect()
        }
    };
    let resolved = resolve_revisions(&repo, revisions, MAX_RESOLVED_PER_BATCH);

    // A failed listing is carried into every entry's reason rather than
    // silently becoming an empty set: "this repository has no verification
    // notes" and "we could not read its notes" are different facts, and only
    // the first one means the commits are genuinely unverified.
    let verified = noted_commits(&repo, VERIFICATION_NOTES_REF);
    let sessioned = noted_commits(&repo, SESSION_NOTES_REF);
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
                Err(reason) => return ProvenanceFreshness::unexamined(rev.clone(), reason),
            };

            let has_verification = verified.contains(&sha);
            let has_session = sessioned.contains(&sha);

            if let Some(err) = &listing_error {
                return ProvenanceFreshness::unexamined(sha, err.clone());
            }

            if !has_verification && !has_session {
                return ProvenanceFreshness::unmeasured(
                    sha,
                    "not measured: this commit carries no provenance note",
                );
            }

            if measured >= budget {
                return ProvenanceFreshness::unmeasured(
                    sha,
                    format!("not measured: past this request's budget of {budget} noted commits"),
                );
            }
            measured += 1;

            let (distance, unmeasured_reason) = measure_distance(&repo, &sha, base);
            // The listing says these notes are there. A read that fails now is
            // a note we could not get at, so the entry says the notes were not
            // readable rather than handing back a `None` that reads as "this
            // commit was never verified".
            let verification = if has_verification {
                read_note::<VerificationNote>(&repo, VERIFICATION_NOTES_REF, &sha)
            } else {
                Ok(None)
            };
            let session = if has_session {
                read_note::<SessionEpisodeNote>(&repo, SESSION_NOTES_REF, &sha)
            } else {
                Ok(None)
            };
            let unreadable = match (&verification, &session) {
                (Err(e), _) | (_, Err(e)) => Some(e.clone()),
                _ => None,
            };
            if let Some(reason) = unreadable {
                return ProvenanceFreshness {
                    distance,
                    confidence: distance.map(|d| 1.0 / (1.0 + 0.1 * d as f32)),
                    is_fresh: distance == Some(0),
                    ..ProvenanceFreshness::unexamined(sha, reason)
                };
            }

            ProvenanceFreshness {
                distance,
                confidence: distance.map(|d| 1.0 / (1.0 + 0.1 * d as f32)),
                is_fresh: distance == Some(0),
                unmeasured_reason,
                notes_readable: true,
                verification: verification.unwrap_or(None),
                session: session.unwrap_or(None),
                commit_sha: sha,
            }
        })
        .collect()
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use std::process::Command;

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
            noted_commits(repo.path(), VERIFICATION_NOTES_REF),
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

    /// `git cat-file --batch-check` answers one line per line of input, and the
    /// batch maps those answers back onto its rows by position. A revision
    /// carrying a newline puts two lines on the wire for one row, so every
    /// later row reads the answer belonging to the row before it — a badge
    /// rendering another commit's provenance, with nothing reporting a fault.
    ///
    /// The check is positional, not textual: it asserts the *second* row still
    /// resolves to the revision it asked for.
    #[test]
    fn a_revision_carrying_a_newline_cannot_shift_another_rows_answer() {
        let repo = Repo::new(2);
        let tip = repo.rev("HEAD");
        let parent = repo.rev("HEAD~1");
        assert_ne!(tip, parent, "the fixture needs two distinct commits");

        let got = freshness_batch(
            repo.as_str(),
            &["HEAD\nHEAD~1".to_string(), "HEAD".to_string()],
            Some("main"),
        );

        assert_eq!(got.len(), 2, "one answer per input, always");
        assert_eq!(
            got[1].commit_sha, tip,
            "the second row must answer for the revision it asked about"
        );
        assert_ne!(
            got[1].commit_sha, parent,
            "an injected newline must not slide another commit into this row"
        );
        assert!(
            got[0].unmeasured_reason.contains("control character"),
            "the refused row must say why, got {:?}",
            got[0].unmeasured_reason
        );
        assert!(
            !got[0].notes_readable,
            "a row we never looked up is unexamined"
        );
    }

    /// A note that is present but cannot be decoded is a note we could not
    /// read. Reporting it as `verification: None` while still claiming the
    /// notes were readable renders an unexamined commit as an unverified one.
    #[test]
    fn a_note_that_does_not_decode_is_unreadable_not_absent() {
        let repo = Repo::new(1);
        let sha = repo.rev("HEAD");
        repo.git(&[
            "notes",
            "--ref=refs/notes/gitpulse/verification",
            "add",
            "-f",
            "-m",
            "this is not the app's JSON",
            &sha,
        ]);

        let err = read_verification_note(repo.as_str(), &sha)
            .expect_err("a note that does not decode is not an absent note");
        assert!(err.contains("did not decode"), "got {err:?}");

        let single = compute_freshness(repo.as_str(), &sha, Some("main"));
        assert!(
            !single.notes_readable,
            "a note we could not read must not report as read"
        );
        assert!(single.verification.is_none());

        let batched = freshness_batch(repo.as_str(), std::slice::from_ref(&sha), Some("main"))
            .pop()
            .expect("one");
        assert!(
            !batched.notes_readable,
            "the batch must agree with the single measurement"
        );
        assert!(batched.verification.is_none());
        assert!(
            batched.unmeasured_reason.contains("did not decode"),
            "the batch must say why, got {:?}",
            batched.unmeasured_reason
        );
    }

    /// The resolution cap is exercised at a size a test can build, and rows
    /// past it say so rather than vanishing from the answer.
    #[test]
    fn revisions_past_the_resolution_limit_are_reported_not_dropped() {
        let repo = Repo::new(1);
        let tip = repo.rev("HEAD");
        let revs = vec![tip.clone(), tip.clone(), tip.clone()];

        let answers = resolve_revisions(repo.path(), &revs, 2);

        assert_eq!(answers.len(), revs.len(), "one answer per input, always");
        assert_eq!(answers[0].as_deref(), Ok(tip.as_str()));
        assert_eq!(answers[1].as_deref(), Ok(tip.as_str()));
        let reason = answers[2].as_ref().expect_err("past the limit");
        assert!(
            reason.contains("limit of 2 revisions"),
            "the cap must name itself, got {reason:?}"
        );
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
