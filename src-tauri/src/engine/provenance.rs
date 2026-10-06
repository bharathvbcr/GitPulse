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
use std::collections::HashMap;
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
    if let Some(reason) = &run.incomplete {
        // Named rather than assumed: "exceeded the cap" and "we could not read
        // it to the end" are different facts, and a note that is a prefix for
        // either reason is not the note.
        return Err(format!(
            "the note on {notes_ref} for {commit_sha} is incomplete (git notes show {}); \
             a prefix of it is not the note",
            reason.describe()
        ));
    }
    decode_note(notes_ref, commit_sha, &run.stdout)
}

/// A note's bytes as this app's JSON. One owner for `notes show` and for a
/// blob read in a batch, so the two can never disagree about a note.
fn decode_note<T: DeserializeOwned>(
    notes_ref: &str,
    commit_sha: &str,
    bytes: &[u8],
) -> Result<Option<T>, String> {
    let text = String::from_utf8_lossy(bytes);
    serde_json::from_str::<T>(text.trim())
        .map(Some)
        .map_err(|e| format!("the note on {notes_ref} for {commit_sha} did not decode: {e}"))
}

/// Reads note blobs in one `git cat-file --batch`, where each noted commit used
/// to cost a `git notes show`.
///
/// Every answer is taken by the size git declares, never by lines: a note may
/// hold any byte, newlines included. A blob git reports missing, an answer that
/// names another object, and a stream that stops short are each an `Err` for
/// the blobs they leave unread, never an empty note.
fn read_note_blobs(repo: &Path, oids: &[String]) -> HashMap<String, Result<Vec<u8>, String>> {
    let mut answers: HashMap<String, Result<Vec<u8>, String>> = HashMap::new();
    let mut sent: Vec<&str> = Vec::new();
    let mut query = String::new();
    for oid in oids {
        if answers.contains_key(oid) || sent.contains(&oid.as_str()) {
            continue;
        }
        let shaped = matches!(oid.len(), 40 | 64) && oid.bytes().all(|b| b.is_ascii_hexdigit());
        if !shaped {
            answers.insert(oid.clone(), Err(format!("{oid:?} is not a note blob id")));
            continue;
        }
        query.push_str(oid);
        query.push('\n');
        sent.push(oid);
    }
    if sent.is_empty() {
        return answers;
    }
    let run = match git_captured_with_stdin(repo, &["cat-file", "--batch"], query.as_bytes()) {
        Ok(run) => run,
        Err(e) => {
            unread(
                &mut answers,
                &sent,
                &format!("could not run git cat-file: {e}"),
            );
            return answers;
        }
    };
    if !run.success {
        let stderr = String::from_utf8_lossy(&run.stderr).trim().to_string();
        unread(
            &mut answers,
            &sent,
            &format!("git cat-file --batch failed: {stderr}"),
        );
        return answers;
    }
    if let Some(reason) = &run.incomplete {
        unread(
            &mut answers,
            &sent,
            &format!("git cat-file {}", reason.describe()),
        );
        return answers;
    }

    read_batch_answers(&run.stdout, &sent, &mut answers);
    answers
}

/// Where a body of the declared `size` starting at `pos` ends, if the stream
/// holds all of it and the newline git writes after it. A size is the
/// stream's own claim, so it is never trusted to fit in `usize` arithmetic.
fn body_end(out: &[u8], pos: usize, size: &str) -> Option<usize> {
    let end = pos.checked_add(size.parse::<usize>().ok()?)?;
    (out.get(end) == Some(&b'\n')).then_some(end)
}

/// Marks every blob in `rest` unread for the same reason.
fn unread(answers: &mut HashMap<String, Result<Vec<u8>, String>>, rest: &[&str], why: &str) {
    for oid in rest {
        answers.insert((*oid).to_string(), Err(why.to_string()));
    }
}

/// Splits a `cat-file --batch` stream into the answers to `sent`, in order.
/// Once the stream is out of step with its questions, nothing after that
/// point is attributed to any of them.
fn read_batch_answers(
    out: &[u8],
    sent: &[&str],
    answers: &mut HashMap<String, Result<Vec<u8>, String>>,
) {
    let mut pos = 0usize;
    for (index, oid) in sent.iter().enumerate() {
        let Some(end) = out[pos..].iter().position(|&b| b == b'\n') else {
            unread(
                answers,
                &sent[index..],
                "git cat-file answered nothing for this note",
            );
            return;
        };
        let header = String::from_utf8_lossy(&out[pos..pos + end]).into_owned();
        pos += end + 1;
        let fields: Vec<&str> = header.split(' ').collect();
        if fields.first() != Some(oid) {
            // Every later answer is now out of step with its question.
            unread(
                answers,
                &sent[index..],
                &format!("git cat-file answered {header:?} for note blob {oid}"),
            );
            return;
        }
        let answer = match fields.as_slice() {
            [_, "blob", size] => match body_end(out, pos, size) {
                Some(end) => {
                    let content = out[pos..end].to_vec();
                    pos = end + 1;
                    Ok(content)
                }
                None => {
                    unread(
                        answers,
                        &sent[index..],
                        &format!("git cat-file cut the note blob {oid} short"),
                    );
                    return;
                }
            },
            [_, "missing"] => Err(format!(
                "the note blob {oid} is missing from this repository"
            )),
            [_, kind, size] => match body_end(out, pos, size) {
                // Skipped by its size so the next answer stays in step.
                Some(end) => {
                    pos = end + 1;
                    Err(format!("{oid} is a {kind}, not a note blob"))
                }
                None => {
                    unread(
                        answers,
                        &sent[index..],
                        &format!("git cat-file answered {header:?}"),
                    );
                    return;
                }
            },
            _ => Err(format!(
                "git cat-file answered {header:?} for note blob {oid}"
            )),
        };
        answers.insert((*oid).to_string(), answer);
    }
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
    let repo = match validate_repo(repo_path) {
        Ok(repo) => repo,
        Err(e) => return ProvenanceFreshness::unexamined(commit_sha, e),
    };

    // The batch's own rows, asked for one revision, with a commit that
    // carries no note measured rather than skipped (see [`Unnoted`]). One
    // owner is what keeps the two paths from disagreeing; it also reads a
    // note only where the listing says there is one, and resolves the base
    // in the same `cat-file` as the commit.
    let asked = [commit_sha.to_string()];
    let mut row = freshness_rows(&repo, &asked, base_branch, 1, Unnoted::Measure)
        .pop()
        .unwrap_or_else(|| {
            ProvenanceFreshness::unexamined(commit_sha, "the measurement answered nothing")
        });
    // Answered under the caller's own spelling, as this path always has.
    row.commit_sha = commit_sha.to_string();
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procguard::LockedSpawn;
    use std::process::Command;
    use tempfile::tempdir;

    fn init_git_repo() -> tempfile::TempDir {
        let dir = tempdir().expect("tempdir");
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output_locked()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir.path())
            .output_locked()
            .expect("git config");
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir.path())
            .output_locked()
            .expect("git config");
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(dir.path())
            .output_locked()
            .expect("initial commit");
        crate::test_support::trust_repo(dir.path());
        dir
    }

    #[test]
    fn write_and_read_verification_note_roundtrip() {
        let dir = init_git_repo();
        let path = dir.path().to_str().expect("utf8 path");

        let head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(path)
            .output_locked()
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
    use crate::procguard::LockedSpawn;
    use std::process::Command;

    fn repo_with_commits(n: usize) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output_locked()
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
        crate::test_support::trust_repo(dir.path());
        dir
    }

    fn head(dir: &std::path::Path) -> String {
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output_locked()
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
            .output_locked()
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
            .output_locked()
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
    if let Some(reason) = &run.incomplete {
        let why = format!("git cat-file {}", reason.describe());
        for row in sent {
            answers[row] = Err(why.clone());
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
fn noted_commits(repo: &Path, notes_ref: &str) -> Result<HashMap<String, String>, String> {
    let ref_arg = ref_arg(notes_ref);
    let run = git_captured(repo, &["notes", &ref_arg, "list"])?;

    if !run.success {
        let stderr = String::from_utf8_lossy(&run.stderr).trim().to_string();
        // "no note found" / a missing ref is absence, not failure.
        if stderr.contains("Cannot load notes ref") || stderr.is_empty() {
            return Ok(HashMap::new());
        }
        return Err(format!("git notes list failed: {stderr}"));
    }
    // A truncated listing would report noted commits as unnoted, which reads
    // downstream as "this commit was never verified".
    if let Some(reason) = &run.incomplete {
        return Err(format!(
            "git notes list {} for {notes_ref}; \
             the set of noted commits would be incomplete",
            reason.describe()
        ));
    }

    // `<note blob> <annotated commit>`, one per line.
    Ok(String::from_utf8_lossy(&run.stdout)
        .lines()
        .filter_map(|line| {
            line.split_once(' ')
                .map(|(blob, commit)| (commit.trim().to_string(), blob.trim().to_string()))
        })
        .collect())
}

/// The base a distance is measured against when the caller does not name one.
const DEFAULT_BASE: &str = "HEAD";

/// Resolves one revision to a commit sha, or says why it could not be.
///
/// Goes through [`resolve_revisions`] rather than spelling out its own git
/// call. That path sends revisions on *stdin*, so nothing a caller passes ever
/// reaches an argument list where git could read it as an option or a
/// pathspec, and it already owns the blank, over-long and control-character
/// guards. A one-element batch costs exactly the subprocess a bespoke
/// `rev-parse` would have cost.
fn resolve_rev(repo: &Path, rev: &str) -> Result<String, String> {
    let one = [rev.to_string()];
    resolve_revisions(repo, &one, 1)
        .pop()
        .unwrap_or_else(|| Err(format!("git cat-file answered nothing for {rev:?}")))
}

/// Resolves the base a distance is measured against.
///
/// `None` means the caller did not name a base, which is documented to mean
/// [`DEFAULT_BASE`]. `Some("")` is not that: it is a base the caller *did*
/// name and git cannot resolve. `unwrap_or` fires only on `None`, so an empty
/// string arrived as the default and then as the empty half of a `sha..`
/// range — which git also reads as `sha..HEAD`, answering a measured number
/// for a request that named no measurable base.
fn resolve_base(repo: &Path, base_branch: Option<&str>) -> Result<String, String> {
    let base = base_branch.unwrap_or(DEFAULT_BASE);
    resolve_rev(repo, base)
        .map_err(|reason| format!("not measured: base {base:?} could not be resolved: {reason}"))
}

/// Measures `commit_sha` against `base`, exactly as [`compute_freshness`] does.
///
/// Both ends are object names git itself produced, and the argument list is
/// closed with `--`. Neither is tidiness. An unresolved revision reaches
/// `rev-list` as argv, where the empty string builds the range `..HEAD` —
/// which git resolves to `HEAD..HEAD` and answers `0` with a zero exit status,
/// the strongest freshness this type can express, for a commit nobody named.
/// Resolving first leaves nothing in the range for git to reinterpret, and
/// `--` leaves nothing after it that git could read as a path.
fn measure_distance(repo: &Path, commit_sha: &str, base: &str) -> (Option<u32>, String) {
    let range = format!("{commit_sha}..{base}");
    match git_captured(repo, &["rev-list", "--count", &range, "--"]) {
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
    let repo = match validate_repo(repo_path) {
        Ok(repo) => repo,
        Err(e) => {
            return revisions
                .iter()
                .map(|rev| ProvenanceFreshness::unexamined(rev.clone(), e.clone()))
                .collect()
        }
    };
    freshness_rows(&repo, revisions, base_branch, budget, Unnoted::Skip)
}

/// What a row does with a commit that carries no provenance note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unnoted {
    /// Answer it unmeasured: the branch and pull-request lists badge many
    /// rows, and a commit nobody verified has no verification to decay.
    Skip,
    /// Measure it anyway: one commit is in front of the reader, and "how far
    /// has the base moved since this" is a fair question either way.
    Measure,
}

/// Resolves `revisions` and the base in one `git cat-file` when the batch has
/// room for one more line, which it does for every request short of the
/// resolution cap.
fn resolve_with_base(
    repo: &Path,
    revisions: &[String],
    base_branch: Option<&str>,
) -> (Vec<Result<String, String>>, Result<String, String>) {
    if revisions.len() >= MAX_RESOLVED_PER_BATCH {
        return (
            resolve_revisions(repo, revisions, MAX_RESOLVED_PER_BATCH),
            resolve_base(repo, base_branch),
        );
    }
    let base_name = base_branch.unwrap_or(DEFAULT_BASE);
    let mut asked = revisions.to_vec();
    asked.push(base_name.to_string());
    let mut answers = resolve_revisions(repo, &asked, MAX_RESOLVED_PER_BATCH);
    let base = answers
        .pop()
        .unwrap_or_else(|| Err(format!("git cat-file answered nothing for {base_name:?}")))
        .map_err(|reason| {
            format!("not measured: base {base_name:?} could not be resolved: {reason}")
        });
    (answers, base)
}

/// The rows both [`compute_freshness`] and [`freshness_batch`] answer with.
fn freshness_rows(
    repo: &Path,
    revisions: &[String],
    base_branch: Option<&str>,
    budget: usize,
    unnoted: Unnoted,
) -> Vec<ProvenanceFreshness> {
    // One base for the whole batch, resolved once. Every row measured against
    // an unresolvable base reports the same reason rather than a number.
    let (resolved, base) = resolve_with_base(repo, revisions, base_branch);

    // A failed listing is carried into every entry's reason rather than
    // silently becoming an empty set: "this repository has no verification
    // notes" and "we could not read its notes" are different facts, and only
    // the first one means the commits are genuinely unverified.
    let verified = noted_commits(repo, VERIFICATION_NOTES_REF);
    let sessioned = noted_commits(repo, SESSION_NOTES_REF);
    let listing_error = match (&verified, &sessioned) {
        (Err(e), _) | (_, Err(e)) => Some(e.clone()),
        _ => None,
    };
    let verified = verified.unwrap_or_default();
    let sessioned = sessioned.unwrap_or_default();

    // First pass: what each row needs. Notes are not read yet, so every blob
    // the measured rows need can be fetched in one process below.
    let mut measured = 0usize;
    let plans: Vec<RowPlan> = resolved
        .into_iter()
        .zip(revisions)
        .map(|(resolution, rev)| {
            let sha = match resolution {
                Ok(sha) => sha,
                Err(reason) => {
                    return RowPlan::Done(Box::new(ProvenanceFreshness::unexamined(
                        rev.clone(),
                        reason,
                    )))
                }
            };

            // Without a listing, which commits carry a note is unknown. The
            // batch cannot measure what it cannot select; a single commit is
            // still measured and both notes asked for directly, but the
            // answer never claims the notes were read.
            let (verification, session) = match (&listing_error, unnoted) {
                (Some(err), Unnoted::Skip) => {
                    return RowPlan::Done(Box::new(ProvenanceFreshness::unexamined(
                        sha,
                        err.clone(),
                    )))
                }
                (Some(_), Unnoted::Measure) => (NoteSource::Ask, NoteSource::Ask),
                (None, _) => (
                    NoteSource::listed(verified.get(&sha)),
                    NoteSource::listed(sessioned.get(&sha)),
                ),
            };

            if verification == NoteSource::Absent
                && session == NoteSource::Absent
                && unnoted == Unnoted::Skip
            {
                return RowPlan::Done(Box::new(ProvenanceFreshness::unmeasured(
                    sha,
                    "not measured: this commit carries no provenance note",
                )));
            }

            if measured >= budget {
                return RowPlan::Done(Box::new(ProvenanceFreshness::unmeasured(
                    sha,
                    format!("not measured: past this request's budget of {budget} noted commits"),
                )));
            }
            measured += 1;
            RowPlan::Measure {
                sha,
                verification,
                session,
            }
        })
        .collect();

    // Second pass: every listed note blob the measured rows read, at once.
    let wanted: Vec<String> = plans
        .iter()
        .flat_map(|plan| match plan {
            RowPlan::Measure {
                verification,
                session,
                ..
            } => [verification.blob(), session.blob()],
            RowPlan::Done(_) => [None, None],
        })
        .flatten()
        .map(str::to_string)
        .collect();
    let blobs = if wanted.is_empty() {
        HashMap::new()
    } else {
        read_note_blobs(repo, &wanted)
    };

    // Third pass: measure and assemble.
    plans
        .into_iter()
        .map(|plan| {
            let (sha, verification, session) = match plan {
                RowPlan::Done(row) => return *row,
                RowPlan::Measure {
                    sha,
                    verification,
                    session,
                } => (sha, verification, session),
            };
            let (distance, unmeasured_reason) = match &base {
                Ok(base) => measure_distance(repo, &sha, base),
                Err(reason) => (None, reason.clone()),
            };
            // The listing says these notes are there. A read that fails now is
            // a note we could not get at, so the entry says the notes were not
            // readable rather than handing back a `None` that reads as "this
            // commit was never verified".
            let verification =
                verification.read::<VerificationNote>(repo, VERIFICATION_NOTES_REF, &sha, &blobs);
            let session = session.read::<SessionEpisodeNote>(repo, SESSION_NOTES_REF, &sha, &blobs);
            let unreadable = match (&listing_error, &verification, &session) {
                (Some(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => Some(e.clone()),
                _ => None,
            };
            if let Some(reason) = unreadable {
                // One commit in front of the reader keeps the note that was
                // read: a broken sessions ref does not make a verification
                // that loaded any less real, and `notes_readable: false` says
                // the rest is missing. A badge row keeps neither, as it
                // always has.
                let (verification, session) = match unnoted {
                    Unnoted::Measure => (verification.unwrap_or(None), session.unwrap_or(None)),
                    Unnoted::Skip => (None, None),
                };
                return ProvenanceFreshness {
                    distance,
                    confidence: distance.map(|d| 1.0 / (1.0 + 0.1 * d as f32)),
                    is_fresh: distance == Some(0),
                    verification,
                    session,
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

/// One row before its notes are read.
enum RowPlan {
    /// Boxed: a finished row is several times the size of a plan to measure.
    Done(Box<ProvenanceFreshness>),
    Measure {
        sha: String,
        verification: NoteSource,
        session: NoteSource,
    },
}

/// Where one row's note comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
enum NoteSource {
    /// The listing says there is none.
    Absent,
    /// The listing names the blob it lives in.
    Blob(String),
    /// No listing to consult: ask `git notes show` directly.
    Ask,
}

impl NoteSource {
    fn listed(blob: Option<&String>) -> Self {
        blob.map_or(Self::Absent, |oid| Self::Blob(oid.clone()))
    }

    fn blob(&self) -> Option<&str> {
        match self {
            Self::Blob(oid) => Some(oid),
            Self::Absent | Self::Ask => None,
        }
    }

    fn read<T: DeserializeOwned>(
        &self,
        repo: &Path,
        notes_ref: &str,
        sha: &str,
        blobs: &HashMap<String, Result<Vec<u8>, String>>,
    ) -> Result<Option<T>, String> {
        match self {
            Self::Absent => Ok(None),
            Self::Ask => read_note(repo, notes_ref, sha),
            Self::Blob(oid) => match blobs.get(oid) {
                Some(Ok(bytes)) => decode_note(notes_ref, sha, bytes),
                Some(Err(reason)) => Err(format!(
                    "the note on {notes_ref} for {sha} could not be read: {reason}"
                )),
                None => Err(format!(
                    "the note on {notes_ref} for {sha} was not read: its blob was never asked for"
                )),
            },
        }
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use crate::procguard::LockedSpawn;
    use std::process::Command;

    struct Repo(tempfile::TempDir);

    impl Repo {
        fn new(commits: usize) -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let repo = Repo(dir);
            repo.git(&["init", "-b", "main"]);
            crate::test_support::trust_repo(repo.path());
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
                .output_locked()
                .expect("git");
            assert!(out.status.success(), "git {args:?}: {out:?}");
        }

        fn rev(&self, spec: &str) -> String {
            let out = Command::new("git")
                .args(["rev-parse", spec])
                .current_dir(self.path())
                .output_locked()
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
            Ok(std::collections::HashMap::new())
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

    /// The guard the whole `Option<u32>` distance exists for.
    ///
    /// `format!("{commit_sha}..{base}")` with an empty commit builds the range
    /// `..HEAD`, which git resolves to `HEAD..HEAD` and answers `0` with a
    /// zero exit status. Nothing downstream can tell that apart from a commit
    /// measured against the tip and found level with it, so an argument naming
    /// no commit reported the strongest freshness this type can express.
    #[test]
    fn an_empty_commit_sha_is_unexamined_rather_than_maximally_fresh() {
        let repo = Repo::new(2);

        let f = compute_freshness(repo.as_str(), "", None);

        assert_eq!(f.distance, None, "nothing was measured");
        assert_eq!(f.confidence, None);
        assert!(!f.is_fresh, "an unnamed commit is not a fresh one");
        assert!(
            !f.unmeasured_reason.is_empty(),
            "the reason must say why nothing was measured"
        );
        assert!(
            !f.notes_readable,
            "no notes were read for a commit that was never resolved"
        );
    }

    /// `unwrap_or` fires on `None`, never on `Some("")`, so an empty base
    /// bypassed the documented `HEAD` default and became the empty half of a
    /// `sha..` range — which git also reads as `sha..HEAD`. A caller naming an
    /// unusable base therefore got a measured number back.
    #[test]
    fn an_empty_base_branch_is_refused_rather_than_silently_defaulting_to_head() {
        let repo = Repo::new(2);
        repo.verify("HEAD", "passed");
        let tip = repo.rev("HEAD");

        let f = compute_freshness(repo.as_str(), &tip, Some(""));

        assert_eq!(f.distance, None, "an unusable base measures nothing");
        assert_eq!(f.confidence, None);
        assert!(!f.is_fresh);
        assert!(
            f.verification.is_some(),
            "the note is still readable even when the distance is not"
        );
        assert!(
            f.notes_readable,
            "an unusable base costs the distance, not the notes"
        );
    }

    #[test]
    fn an_empty_base_branch_is_refused_for_every_row_of_a_batch() {
        let repo = Repo::new(2);
        repo.verify("HEAD", "passed");

        let got = freshness_batch(repo.as_str(), &[repo.rev("HEAD")], Some(""));

        assert_eq!(
            got[0].distance, None,
            "the batch must agree with the single"
        );
        assert_eq!(got[0].confidence, None);
        assert!(!got[0].is_fresh);
        assert!(
            got[0].unmeasured_reason.contains("base"),
            "the reason must name the base, got {:?}",
            got[0].unmeasured_reason
        );
    }

    /// A revision is resolved through `git cat-file --batch-check`, which
    /// reads it on stdin. Nothing a caller passes reaches an argument list
    /// where git could read it as an option or a pathspec, and the range that
    /// does reach one is built from two object names git itself produced.
    #[test]
    fn a_revision_spelled_like_a_flag_is_never_handed_to_git_as_one() {
        let repo = Repo::new(2);
        let tip = repo.rev("HEAD");

        for f in [
            compute_freshness(repo.as_str(), "--all", None),
            compute_freshness(repo.as_str(), &tip, Some("--all")),
            compute_freshness(repo.as_str(), "-n1", None),
        ] {
            assert_eq!(f.distance, None, "nothing measurable was named");
            assert!(!f.is_fresh);
            assert!(!f.unmeasured_reason.is_empty());
        }
    }

    /// `a..HEAD` where `a` is a tracked file is ambiguous to git, and a rev
    /// that is only a path is not a rev at all. Resolution refuses it before
    /// the range is built, so the answer is a reason rather than whatever git
    /// decided the argument was.
    #[test]
    fn a_path_shaped_revision_is_refused_before_the_range_is_built() {
        let repo = Repo::new(2);

        let f = compute_freshness(repo.as_str(), "f0", None);

        assert_eq!(f.distance, None);
        assert!(!f.is_fresh);
        assert!(
            f.unmeasured_reason.contains("f0"),
            "the reason must name what could not be resolved, got {:?}",
            f.unmeasured_reason
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
        // The base is now refused at resolution rather than by `rev-list`, so
        // the reason names the base the caller passed instead of the command
        // that would have used it. Naming the unusable input is the stronger
        // statement of the two.
        assert!(
            f.unmeasured_reason.contains("no-such-base"),
            "the reason must name the base that could not be resolved, got {:?}",
            f.unmeasured_reason
        );
    }

    /// Opening one commit used to cost seven processes: the commit and the
    /// base resolved separately, both notes refs listed, both notes asked for
    /// whether or not the listing had them, and the distance. A note the
    /// listing does not have is no longer asked for, and the base rides the
    /// commit's `cat-file`.
    #[test]
    fn a_single_commit_spends_a_process_only_on_what_it_reads() {
        let repo = Repo::new(3);
        let root = repo.path().canonicalize().unwrap();
        let spawned = || crate::engine::git_cli::spawn_log::spawns_in(&root);

        let unnoted = repo.rev("HEAD~1");
        let before = spawned().len();
        let answer = compute_freshness(repo.as_str(), &unnoted, Some("main"));
        assert_eq!(
            answer.distance,
            Some(1),
            "an unnoted commit is still measured here"
        );
        assert!(answer.notes_readable);
        let used = &spawned()[before..];
        assert_eq!(used.len(), 4, "{used:?}");
        assert!(
            !used.iter().any(|argv| argv.iter().any(|a| a == "show")),
            "{used:?}"
        );

        repo.verify("HEAD~2", "passed");
        let noted = repo.rev("HEAD~2");
        let before = spawned().len();
        let answer = compute_freshness(repo.as_str(), &noted, Some("main"));
        assert!(answer.verification.is_some());
        let used = &spawned()[before..];
        let count = |needle: &str| {
            used.iter()
                .filter(|argv| argv.iter().any(|a| a == needle))
                .count()
        };
        // The note the listing has, read by the blob it named.
        assert_eq!(count("show"), 0, "{used:?}");
        assert_eq!(count("--batch"), 1, "{used:?}");
        assert_eq!(used.len(), 5, "{used:?}");
    }

    /// Answers are taken by size, so a note holding newlines, a fake header
    /// line or a NUL cannot shift the next answer. A missing blob and an
    /// object that is not a blob are reported, and the stream stays in step
    /// past both.
    /// Streams no working git prints: an answer naming another object, a size
    /// past the end of memory, and a stream that stops between answers. Each
    /// leaves its blob and every later one unread rather than misattributed,
    /// and none panics.
    #[test]
    fn a_batch_stream_out_of_step_attributes_nothing_after_the_fault() {
        let (a, b, c) = ("a".repeat(40), "b".repeat(40), "c".repeat(40));
        let sent = [a.as_str(), b.as_str(), c.as_str()];
        let read = |stream: &[u8]| {
            let mut answers = HashMap::new();
            read_batch_answers(stream, &sent, &mut answers);
            assert_eq!(answers.len(), 3, "every blob is answered");
            answers
        };

        let swapped = format!("{a} blob 2\nhi\n{c} blob 2\nno\n{b} blob 2\nno\n");
        let answers = read(swapped.as_bytes());
        assert_eq!(answers[&a], Ok(b"hi".to_vec()));
        for oid in [&b, &c] {
            let err = answers[oid].as_ref().unwrap_err();
            assert!(err.contains("answered") && err.contains(&c), "{oid}: {err}");
        }

        let huge = format!("{a} blob {}\nhi\n", usize::MAX);
        let answers = read(huge.as_bytes());
        assert!(answers[&a].as_ref().unwrap_err().contains("short"));
        let huge_tree = format!("{a} tree {}\nhi\n", usize::MAX);
        let answers = read(huge_tree.as_bytes());
        assert!(answers[&a].as_ref().is_err_and(|e| e.contains("answered")));

        let stopped = format!("{a} missing\n{b} blob 1\nx\n");
        let answers = read(stopped.as_bytes());
        assert!(answers[&a].as_ref().unwrap_err().contains("missing"));
        assert_eq!(answers[&b], Ok(b"x".to_vec()));
        assert!(answers[&c].as_ref().unwrap_err().contains("nothing"));
    }

    #[test]
    fn note_blobs_are_read_by_size_and_every_failure_is_named() {
        let repo = Repo::new(2);
        let write_blob = |content: &[u8]| -> String {
            let mut child = Command::new("git")
                .args(["hash-object", "-w", "--stdin"])
                .current_dir(repo.path())
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn_locked()
                .unwrap();
            use std::io::Write;
            child.stdin.take().unwrap().write_all(content).unwrap();
            let out = child.wait_with_output().unwrap();
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        let tricky =
            b"line one\n0000000000000000000000000000000000000000 blob 3\nabc\n\0tail".to_vec();
        let plain = b"{\"x\":1}".to_vec();
        let a = write_blob(&tricky);
        let b = write_blob(&plain);
        let commit = repo.rev("HEAD");
        let missing = "1".repeat(40);
        let asked = vec![
            a.clone(),
            missing.clone(),
            commit.clone(),
            b.clone(),
            "not-an-oid\nHEAD".to_string(),
            a.clone(),
        ];
        let got = read_note_blobs(repo.path(), &asked);
        assert_eq!(got[&a], Ok(tricky));
        assert_eq!(
            got[&b],
            Ok(plain),
            "the answer after a non-blob kept its place"
        );
        assert!(got[&missing].as_ref().unwrap_err().contains("missing"));
        assert!(got[&commit].as_ref().unwrap_err().contains("is a commit"));
        assert!(got["not-an-oid\nHEAD"]
            .as_ref()
            .unwrap_err()
            .contains("not a note blob id"));
        assert_eq!(got.len(), 5, "a repeated id is asked once");
    }

    /// Many noted commits: every note is read in one process, and each one is
    /// exactly what `git notes show` says it is.
    #[test]
    fn a_batch_reads_every_note_in_one_process_and_matches_notes_show() {
        let repo = Repo::new(6);
        for n in 0..5 {
            repo.verify(
                &format!("HEAD~{n}"),
                if n % 2 == 0 { "passed" } else { "failed" },
            );
        }
        let revs: Vec<String> = (0..6).map(|n| repo.rev(&format!("HEAD~{n}"))).collect();
        let root = repo.path().canonicalize().unwrap();
        let before = crate::engine::git_cli::spawn_log::spawns_in(&root).len();
        let rows = freshness_batch(repo.as_str(), &revs, Some("main"));
        let used = crate::engine::git_cli::spawn_log::spawns_in(&root)[before..].to_vec();
        let count = |needle: &str| {
            used.iter()
                .filter(|argv| argv.iter().any(|a| a == needle))
                .count()
        };
        assert_eq!(count("--batch"), 1, "{used:?}");
        assert_eq!(count("show"), 0, "{used:?}");
        for (row, rev) in rows.iter().zip(&revs) {
            let shown = read_verification_note(repo.as_str(), rev).unwrap();
            assert_eq!(row.verification, shown, "{rev}");
            assert!(row.notes_readable);
        }
        assert!(
            rows[5].verification.is_none(),
            "the unnoted commit stays unnoted"
        );
    }

    /// A broken sessions ref beside a readable verification note. The single
    /// commit keeps the verification it read and says the notes were not all
    /// readable; the badge row keeps neither, as each did before the two
    /// shared one owner. One change: the single commit now names the failure
    /// in `unmeasured_reason`, where it used to leave the reason empty.
    #[test]
    fn a_broken_sessions_ref_keeps_the_single_commits_verification_only() {
        let repo = Repo::new(2);
        repo.verify("HEAD", "passed");
        let sha = repo.rev("HEAD");
        let refs = repo.path().join(".git/refs/notes/gitpulse");
        std::fs::create_dir_all(&refs).expect("mkdir");
        std::fs::write(
            refs.join("sessions"),
            "0000000000000000000000000000000000000001\n",
        )
        .expect("write ref");

        let single = compute_freshness(repo.as_str(), &sha, Some("main"));
        assert!(!single.notes_readable);
        assert!(single.verification.is_some(), "{single:?}");
        assert_eq!(single.distance, Some(0));
        assert!(!single.unmeasured_reason.is_empty(), "the failure is named");

        let row = freshness_batch(repo.as_str(), std::slice::from_ref(&sha), Some("main"))
            .pop()
            .expect("one");
        assert!(!row.notes_readable);
        assert!(row.verification.is_none(), "{row:?}");
        assert_eq!(
            row.distance, None,
            "a badge row cannot select without a listing"
        );
    }
}
