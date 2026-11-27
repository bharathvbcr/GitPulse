//! Idempotent land identity tracking and revert detection (`revertOf`).
//!
//! Records `(repo_path, target_ref, source_oid) -> landed_oid` in the
//! `.devcouncil/ledger.sqlite` `land_identities` table so retries of an
//! already-rebased or already-squashed commit return `AlreadyLanded` instead of
//! re-applying or falsely conflicting, while also scanning up to
//! [`REVERT_SEARCH_COMMITS`] commits in `landed_oid..tip` for `"This reverts
//! commit <full-oid>"` (including revert-of-revert chains).

use std::collections::HashSet;
use std::path::Path;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::engine::git_cli::git_captured;
use crate::engine::git_writer::is_full_oid;
use crate::ipc_error::CommandError;

use super::{
    canonical_repo, has_table, ids, ledger_path, open_readonly, with_conn, write_tx, LedgerError,
};

/// Maximum number of commits walked between `landed_oid..tip` when checking
/// whether a landed commit was later reverted.
pub const REVERT_SEARCH_COMMITS: usize = 1000;

/// Exact prefix produced by `git revert` in commit messages.
const REVERT_PREFIX: &str = "This reverts commit ";

/// Maximum candidate rows inspected per `(target_ref, source_oid)` lookup.
const MAX_CANDIDATE_ROWS: usize = 32;

/// Match returned when `source_oid` has already been landed onto `target_ref`
/// and its recorded `landed_oid` is still reachable from the branch tip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandIdentityMatch {
    pub target_ref: String,
    pub source_oid: String,
    pub landed_oid: String,
    pub reverted_by: Option<String>,
    pub recorded_at: String,
}

fn normalize_target_ref(target_ref: &str) -> String {
    if target_ref.starts_with("refs/") {
        target_ref.to_string()
    } else {
        format!("refs/heads/{target_ref}")
    }
}

fn repo_key(repo: &Path) -> Result<String, LedgerError> {
    let raw = repo.to_str().ok_or_else(|| {
        LedgerError::new(
            "invalid_repo_path",
            format!("repository path is not valid UTF-8: {}", repo.display()),
        )
    })?;
    Ok(canonical_repo(raw))
}

fn validate_oid(label: &str, oid: &str) -> Result<(), LedgerError> {
    if !is_full_oid(oid) {
        return Err(LedgerError::new(
            "invalid_oid",
            format!("{label} {oid:?} is not a full Git object id"),
        ));
    }
    Ok(())
}

/// Persists a confirmed `(target_ref, source_oid) -> landed_oid` mapping in
/// `.devcouncil/ledger.sqlite`.
pub fn record_land_identity(
    repo: &Path,
    target_ref: &str,
    source_oid: &str,
    landed_oid: &str,
) -> Result<(), LedgerError> {
    record_row(repo, target_ref, source_oid, landed_oid, "confirmed", None)
}

/// Records an unconfirmed intent before CAS `update-ref` so a crash or split
/// between ref update and confirmation can still reconcile via `is_ancestor`.
pub(crate) fn record_land_intent(
    repo: &Path,
    target_ref: &str,
    source_oid: &str,
    landed_oid: &str,
    invocation_id: &str,
) -> Result<(), LedgerError> {
    record_row(
        repo,
        target_ref,
        source_oid,
        landed_oid,
        "intent",
        Some(invocation_id),
    )
}

/// Marks all intent rows for `invocation_id` as `confirmed` after CAS succeeds.
pub(crate) fn confirm_land_intent(repo: &Path, invocation_id: &str) -> Result<(), LedgerError> {
    update_intent_status(repo, invocation_id, "confirmed")
}

/// Marks all intent rows for `invocation_id` as `aborted` when CAS fails or is
/// rolled back so they are excluded from future lookups.
pub(crate) fn abort_land_intent(repo: &Path, invocation_id: &str) -> Result<(), LedgerError> {
    update_intent_status(repo, invocation_id, "aborted")
}

fn record_row(
    repo: &Path,
    target_ref: &str,
    source_oid: &str,
    landed_oid: &str,
    status: &str,
    invocation_id: Option<&str>,
) -> Result<(), LedgerError> {
    validate_oid("source_oid", source_oid)?;
    validate_oid("landed_oid", landed_oid)?;
    let canon = repo_key(repo)?;
    let normalized_ref = normalize_target_ref(target_ref);
    let now = ids::iso8601_utc(ids::now_millis());
    with_conn(&canon, |conn| {
        let tx = write_tx(conn)
            .map_err(|e| LedgerError::new("tx_failed", format!("begin land_identity tx: {e}")))?;
        tx.execute(
            "INSERT INTO land_identities (
               repo_path, target_ref, source_oid, landed_oid, status, invocation_id, recorded_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                canon,
                normalized_ref,
                source_oid,
                landed_oid,
                status,
                invocation_id,
                now
            ],
        )
        .map_err(|e| LedgerError::new("insert_failed", format!("land_identities insert: {e}")))?;
        tx.commit()
            .map_err(|e| LedgerError::new("commit_failed", format!("land_identities commit: {e}")))
    })
}

fn update_intent_status(
    repo: &Path,
    invocation_id: &str,
    new_status: &str,
) -> Result<(), LedgerError> {
    let canon = repo_key(repo)?;
    with_conn(&canon, |conn| {
        let tx = write_tx(conn)
            .map_err(|e| LedgerError::new("tx_failed", format!("begin land_identity tx: {e}")))?;
        tx.execute(
            "UPDATE land_identities SET status = ?1 WHERE invocation_id = ?2 AND status = 'intent'",
            params![new_status, invocation_id],
        )
        .map_err(|e| LedgerError::new("update_failed", format!("land_identities update: {e}")))?;
        tx.commit()
            .map_err(|e| LedgerError::new("commit_failed", format!("land_identities commit: {e}")))
    })
}

/// Checks whether `source_oid` has already been landed onto `target_ref` and
/// whether its `landed_oid` is still an ancestor of `tip`.
///
/// Confirmed records are checked newest-first, followed by unconfirmed intents.
/// Unreachable or pruned `landed_oid`s (e.g., after branch reset or an aborted
/// intent) are skipped. When a reachable `landed_oid` is found, up to
/// [`REVERT_SEARCH_COMMITS`] commits in `landed_oid..tip` are scanned for a
/// non-reverted `"This reverts commit <full-oid>"` commit.
pub fn check_already_landed(
    repo: &Path,
    target_ref: &str,
    source_oid: &str,
    tip: &str,
) -> Result<Option<LandIdentityMatch>, CommandError> {
    validate_oid("source_oid", source_oid).map_err(|e| CommandError::plain(e.to_string()))?;
    validate_oid("tip", tip).map_err(|e| CommandError::plain(e.to_string()))?;
    let canon = repo_key(repo).map_err(|e| CommandError::plain(e.to_string()))?;
    let normalized_ref = normalize_target_ref(target_ref);

    let db_path = ledger_path(&canon);
    let Some(conn) = open_readonly(&db_path).map_err(|e| CommandError::plain(e.to_string()))?
    else {
        return Ok(None);
    };
    if !has_table(&conn, "land_identities").map_err(|e| CommandError::plain(e.to_string()))? {
        return Ok(None);
    }

    let candidates = query_candidates(&conn, &canon, &normalized_ref, source_oid)
        .map_err(|e| CommandError::plain(e.to_string()))?;

    for (landed_oid, recorded_at) in candidates {
        if !is_ancestor_or_skip_missing(repo, &landed_oid, tip)? {
            continue;
        }
        let reverted_by = find_revert_of(repo, &landed_oid, tip)?;
        return Ok(Some(LandIdentityMatch {
            target_ref: normalized_ref,
            source_oid: source_oid.to_string(),
            landed_oid,
            reverted_by,
            recorded_at,
        }));
    }

    Ok(None)
}

fn query_candidates(
    conn: &Connection,
    repo_key: &str,
    target_ref: &str,
    source_oid: &str,
) -> Result<Vec<(String, String)>, LedgerError> {
    let mut stmt = conn
        .prepare(
            "SELECT landed_oid, recorded_at
             FROM land_identities
             WHERE repo_path = ?1
               AND target_ref = ?2
               AND source_oid = ?3
               AND status IN ('confirmed', 'intent')
             ORDER BY CASE status WHEN 'confirmed' THEN 0 ELSE 1 END, id DESC
             LIMIT ?4",
        )
        .map_err(|e| LedgerError::new("query_failed", format!("land_identities prepare: {e}")))?;
    let rows = stmt
        .query_map(
            params![repo_key, target_ref, source_oid, MAX_CANDIDATE_ROWS as i64],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(|e| LedgerError::new("query_failed", format!("land_identities query: {e}")))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(
            row.map_err(|e| LedgerError::new("query_failed", format!("land_identities row: {e}")))?,
        );
    }
    Ok(out)
}

fn is_ancestor_or_skip_missing(
    repo: &Path,
    ancestor: &str,
    descendant: &str,
) -> Result<bool, CommandError> {
    if ancestor == descendant {
        return Ok(true);
    }
    let run = git_captured(repo, &["merge-base", "--is-ancestor", ancestor, descendant])?
        .require_complete("git merge-base")?;
    match run.status_code {
        0 => Ok(true),
        1 => Ok(false),
        128 => {
            let stderr = String::from_utf8_lossy(&run.stderr);
            if stderr.contains("Not a valid object name")
                || stderr.contains("bad object")
                || stderr.contains("Invalid object")
            {
                Ok(false)
            } else {
                Err(CommandError::plain(format!(
                    "git merge-base --is-ancestor {ancestor} {descendant} failed: {}",
                    stderr.trim()
                )))
            }
        }
        code => Err(CommandError::plain(format!(
            "git merge-base --is-ancestor {ancestor} {descendant} exited {code}: {}",
            String::from_utf8_lossy(&run.stderr).trim()
        ))),
    }
}

/// Walks up to [`REVERT_SEARCH_COMMITS`] commits in `landed_oid..tip` newest-first
/// and returns the OID of the active commit reverting `landed_oid` (ignoring
/// reverts that were themselves reverted later).
pub(crate) fn find_revert_of(
    repo: &Path,
    landed_oid: &str,
    tip: &str,
) -> Result<Option<String>, CommandError> {
    if landed_oid == tip {
        return Ok(None);
    }
    let range = format!("{landed_oid}..{tip}");
    let limit = REVERT_SEARCH_COMMITS.to_string();
    let run = git_captured(
        repo,
        &[
            "log",
            "-z",
            "--format=%H%x00%B",
            "--max-count",
            &limit,
            &range,
        ],
    )?
    .require_complete("git log")?;
    if run.status_code != 0 {
        return Err(CommandError::plain(format!(
            "git log {range} failed (exit {}): {}",
            run.status_code,
            String::from_utf8_lossy(&run.stderr).trim()
        )));
    }

    let hex_len = landed_oid.len();
    let mut reverted_later: HashSet<String> = HashSet::new();
    let mut fields = run.stdout.split(|&b| b == 0);
    while let Some(raw_oid) = fields.next() {
        let oid = String::from_utf8_lossy(raw_oid).trim().to_string();
        if oid.is_empty() {
            continue;
        }
        let Some(raw_body) = fields.next() else {
            break;
        };
        if reverted_later.contains(&oid) {
            continue;
        }
        let body = String::from_utf8_lossy(raw_body);
        for target in extract_reverted_oids(&body, hex_len) {
            if target == landed_oid {
                return Ok(Some(oid));
            }
            reverted_later.insert(target);
        }
    }
    Ok(None)
}

fn extract_reverted_oids(body: &str, hex_len: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(idx) = rest.find(REVERT_PREFIX) {
        let after = &rest[idx + REVERT_PREFIX.len()..];
        if after.len() >= hex_len {
            let candidate = &after[..hex_len];
            let boundary_ok = after
                .as_bytes()
                .get(hex_len)
                .is_none_or(|b| !b.is_ascii_hexdigit());
            if boundary_ok
                && candidate
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            {
                out.push(candidate.to_string());
            }
        }
        rest = &rest[idx + REVERT_PREFIX.len()..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::git_cli::git_text;

    fn git(repo: &Path, args: &[&str]) -> String {
        git_text(repo, args)
            .unwrap_or_else(|e| panic!("git {args:?}: {}", e.message()))
            .trim()
            .to_string()
    }

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        crate::test_support::git_in(repo, &["init", "-q", "-b", "main", "."]);
        git(repo, &["config", "user.email", "test@gitpulse.local"]);
        git(repo, &["config", "user.name", "GitPulse Tester"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        git(repo, &["add", "base.txt"]);
        git(repo, &["commit", "-q", "-m", "initial"]);
        crate::test_support::trust_repo(repo);
        dir
    }

    #[test]
    fn record_and_check_already_landed_tracks_reverts_and_revert_of_revert() {
        let dir = init_repo();
        let repo = dir.path();
        let base_oid = git(repo, &["rev-parse", "HEAD"]);

        // Create a lane commit (source_oid) on a branch, then land a squashed commit onto main.
        git(repo, &["checkout", "-q", "-b", "lane-1"]);
        std::fs::write(repo.join("feature.txt"), "v1\n").unwrap();
        git(repo, &["add", "feature.txt"]);
        git(repo, &["commit", "-q", "-m", "lane 1 work"]);
        let source_oid = git(repo, &["rev-parse", "HEAD"]);

        git(repo, &["checkout", "-q", "main"]);
        std::fs::write(repo.join("feature.txt"), "v1\n").unwrap();
        git(repo, &["add", "feature.txt"]);
        git(repo, &["commit", "-q", "-m", "land lane 1"]);
        let landed_oid = git(repo, &["rev-parse", "HEAD"]);

        // Before recording: not yet known.
        assert!(check_already_landed(repo, "main", &source_oid, &landed_oid)
            .unwrap()
            .is_none());

        record_land_identity(repo, "main", &source_oid, &landed_oid).unwrap();

        let matched = check_already_landed(repo, "refs/heads/main", &source_oid, &landed_oid)
            .unwrap()
            .expect("landed match");
        assert_eq!(matched.landed_oid, landed_oid);
        assert_eq!(matched.reverted_by, None);

        // Revert the landed commit.
        git(repo, &["revert", "--no-edit", &landed_oid]);
        let revert_oid = git(repo, &["rev-parse", "HEAD"]);
        let matched = check_already_landed(repo, "main", &source_oid, &revert_oid)
            .unwrap()
            .expect("still already landed with revert explanation");
        assert_eq!(matched.reverted_by.as_deref(), Some(revert_oid.as_str()));

        // Revert the revert -> reverted_by clears!
        git(repo, &["revert", "--no-edit", &revert_oid]);
        let r2_oid = git(repo, &["rev-parse", "HEAD"]);
        let matched = check_already_landed(repo, "main", &source_oid, &r2_oid)
            .unwrap()
            .expect("revert-of-revert restores landed state");
        assert_eq!(matched.reverted_by, None);

        // Revert R2 -> R1 is active again, so reverted_by points to R1!
        git(repo, &["revert", "--no-edit", &r2_oid]);
        let r3_oid = git(repo, &["rev-parse", "HEAD"]);
        let matched = check_already_landed(repo, "main", &source_oid, &r3_oid)
            .unwrap()
            .expect("3rd revert restores revert_by");
        assert_eq!(matched.reverted_by.as_deref(), Some(revert_oid.as_str()));

        // If branch is force-reset past landed_oid, it is no longer on branch.
        assert!(check_already_landed(repo, "main", &source_oid, &base_oid)
            .unwrap()
            .is_none());
    }

    #[test]
    fn aborted_intent_is_ignored_and_confirmed_intent_matches() {
        let dir = init_repo();
        let repo = dir.path();
        let base_oid = git(repo, &["rev-parse", "HEAD"]);

        std::fs::write(repo.join("f.txt"), "x\n").unwrap();
        git(repo, &["add", "f.txt"]);
        git(repo, &["commit", "-q", "-m", "second"]);
        let c2 = git(repo, &["rev-parse", "HEAD"]);

        record_land_intent(repo, "main", &base_oid, &c2, "inv-abort").unwrap();
        abort_land_intent(repo, "inv-abort").unwrap();
        assert!(check_already_landed(repo, "main", &base_oid, &c2)
            .unwrap()
            .is_none());

        record_land_intent(repo, "main", &base_oid, &c2, "inv-ok").unwrap();
        confirm_land_intent(repo, "inv-ok").unwrap();
        let hit = check_already_landed(repo, "main", &base_oid, &c2)
            .unwrap()
            .expect("confirmed intent matches");
        assert_eq!(hit.landed_oid, c2);
    }
}
