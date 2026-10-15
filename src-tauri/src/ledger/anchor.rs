//! Which repository a ledger belongs to, when its path no longer says.
//!
//! Every row names the repository path it was written under, and the foreign
//! row guard ([`super::refuse_foreign_rows`], and the same check inside
//! [`super::tail`]) refuses a ledger holding a row that names another path:
//! that is what stops a ledger copied from a different repository being read
//! as this one's history, and bind rows authored elsewhere being honoured here.
//! The same guard refused a repository that was only moved, because a moved
//! repository carries its ledger with it and every row then names the old path.
//!
//! # Identity
//!
//! A repository is identified by what lives in its common dir, never by where
//! it sits: its **root commit oids** (all of them, over every ref) and its
//! **configured remotes** (normalised, credentials stripped). The identity is
//! written into the ledger as an append-only record ([`ANCHOR`], and
//! [`REANCHOR`] when the path changed) while the ledger is at home, because a
//! ledger that never recorded who it belongs to cannot be told apart from one
//! that was copied in. No schema or writer-version change: the records are
//! ordinary rows, found through `idx_events_session` by [`MARKER`].
//!
//! # The rule
//!
//! Rows naming a path other than this repository's are accepted as a prior
//! location of it only when all of these hold, checked together in one write
//! transaction at the first transcript pass after the move
//! ([`settle`]):
//!
//! 1. the ledger carries an identity record, and this repository's identity can
//!    be read (a repository with no commit has no root and is refused);
//! 2. every recorded root commit is still a root of this repository
//!    (`recorded ⊆ current`: new roots may appear, a rewritten root is a
//!    different repository);
//! 3. the remotes do not contradict: when both sides name any, they share at
//!    least one (a remote added after the move is fine, a fork that points
//!    `origin` elsewhere is not);
//! 4. no prior path is still occupied by a Git repository of its own. A
//!    checkout that still exists at the old path is a copy or an independent
//!    clone, not a move, and is never absorbed. Nothing is run at a prior path:
//!    it comes from ledger content, which a checkout's writer controls.
//!
//! The prior paths are then recorded in a [`REANCHOR`] row naming the new path;
//! history is not rewritten. A row is foreign unless it names this path or a
//! path some re-anchor row *to this path* lists.
//!
//! # What this does not claim
//!
//! The ledger lives inside the checkout, so whoever can write the checkout can
//! write any row, including an identity record: the guard catches accidents
//! and copies, not a deliberate forger. Two independent clones of one upstream
//! have the same identity, so a ledger copied from one into the other is taken
//! for a move once the source is gone.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use rusqlite::{params, Connection, OptionalExtension};

use super::{
    canonical_repo, foreign_row_sql, home_spellings, insert, prepare, write_tx, ActorKind, Draft,
    LedgerError,
};

/// The action of the record written while a ledger is at home.
const ANCHOR: &str = "ledger.anchor";
/// The action of the record written when a ledger's repository changed path.
const REANCHOR: &str = "ledger.reanchor";
/// The `session_id` both records carry. Not a session: the one indexed column
/// (`idx_events_session`) that lets them be found without scanning the ledger,
/// which every foreign-row check on a re-anchored ledger would otherwise pay.
const MARKER: &str = "gitpulse-ledger-identity";
/// More prior paths than any real history has. A ledger naming more is not
/// one repository that moved.
const MAX_PRIOR_PATHS: usize = 64;
/// How long a repository whose identity could not be settled is left alone.
const RETRY_AFTER: Duration = Duration::from_secs(30);
/// Bound on the one `git rev-list` that reads the roots.
const READ_TIMEOUT: Duration = Duration::from_secs(20);

/// What a repository is, independent of where it is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Identity {
    pub roots: BTreeSet<String>,
    pub remotes: BTreeSet<String>,
}

impl Identity {
    /// The identity of the trusted repository at `repo_path`. `Err` for one
    /// that is untrusted, unreadable, or has no commit (and so no root).
    fn read(repo_path: &str) -> Result<Identity, String> {
        use crate::engine::git_cli;
        let repo = git_cli::validate_repo(repo_path)?;
        let roots: BTreeSet<String> = git_cli::git_text_with_timeout(
            &repo,
            &["rev-list", "--max-parents=0", "--all"],
            READ_TIMEOUT,
        )?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
        if roots.is_empty() {
            return Err("the repository has no commit, so no root commit to identify it by".into());
        }
        let config = git_cli::git_text(&repo, &["config", "--local", "--list"])?;
        Ok(Identity {
            roots,
            remotes: remotes_in(&config),
        })
    }

    fn to_detail(&self, from: &[String]) -> String {
        let mut detail = serde_json::json!({
            "roots": self.roots,
            "remotes": self.remotes,
        });
        if !from.is_empty() {
            detail["from"] = serde_json::json!(from);
        }
        detail.to_string()
    }

    fn from_detail(detail: &serde_json::Value) -> Option<Identity> {
        let set = |key: &str| -> Option<BTreeSet<String>> {
            detail
                .get(key)?
                .as_array()?
                .iter()
                .map(|value| value.as_str().map(str::to_string))
                .collect()
        };
        Some(Identity {
            roots: set("roots")?,
            remotes: set("remotes")?,
        })
    }
}

/// Whether `current` is the repository `recorded` was, and why not.
///
/// Roots decide; remotes only contradict. See the module rule.
fn same_repository(recorded: &Identity, current: &Identity) -> Result<(), String> {
    if recorded.roots.is_empty() {
        return Err("the recorded identity names no root commit".into());
    }
    if !recorded.roots.is_subset(&current.roots) {
        return Err(
            "a root commit the ledger was recorded under is not a root of this repository".into(),
        );
    }
    if !recorded.remotes.is_empty()
        && !current.remotes.is_empty()
        && recorded.remotes.is_disjoint(&current.remotes)
    {
        return Err("this repository's remotes share none with those the ledger was recorded under (a fork or another clone)".into());
    }
    Ok(())
}

/// The remote URLs in `git config --list` output, normalised.
fn remotes_in(config: &str) -> BTreeSet<String> {
    config
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            let name = key.strip_prefix("remote.")?.strip_suffix(".url")?;
            (!name.is_empty()).then(|| normalise_url(value))
        })
        .filter(|url| !url.is_empty())
        .collect()
}

/// A remote URL with credentials, case, a trailing slash and `.git` removed,
/// so the same upstream spelled two ways is one remote and no secret is kept.
fn normalise_url(url: &str) -> String {
    let url = url.trim();
    let without_user = match url.split_once("://") {
        Some((scheme, rest)) => {
            let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
            let host = authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host);
            format!("{scheme}://{host}/{path}")
        }
        // `user@host:path`: no scheme, so the userinfo ends at the first `@`
        // that precedes the first `:`.
        None => match (url.find('@'), url.find(':')) {
            (Some(at), Some(colon)) if at < colon => url[at + 1..].to_string(),
            _ => url.to_string(),
        },
    };
    let trimmed = without_user.trim_end_matches('/');
    trimmed
        .strip_suffix(".git")
        .unwrap_or(trimmed)
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

fn mismatch(reason: impl std::fmt::Display) -> LedgerError {
    LedgerError::new(
        "repository_mismatch",
        format!("the ledger contains an event attributed to a different repository: {reason}"),
    )
}

/// Every row's repository spelling that is not in `allowed` (sorted, distinct),
/// at most [`MAX_PRIOR_PATHS`] + 1.
fn foreign_paths(conn: &Connection, allowed: &[String]) -> Result<Vec<String>, LedgerError> {
    let sql = format!(
        "SELECT DISTINCT repo_path FROM events WHERE {} ORDER BY repo_path LIMIT {}",
        foreign_row_sql(allowed.len()),
        MAX_PRIOR_PATHS + 1
    );
    let failed = |e: rusqlite::Error| LedgerError::new("query_failed", e.to_string());
    let mut stmt = conn.prepare(&sql).map_err(failed)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(allowed), |row| row.get(0))
        .map_err(failed)?;
    rows.collect::<Result<Vec<String>, _>>().map_err(failed)
}

/// The prior locations of the repository at `home`, as its re-anchor rows
/// recorded them. Seeks `idx_events_session`; reads nothing else.
pub(super) fn aliases(conn: &Connection, home: &[String]) -> Result<Vec<String>, LedgerError> {
    let failed = |e: rusqlite::Error| LedgerError::new("query_failed", e.to_string());
    let mut stmt = conn
        .prepare("SELECT repo_path, detail_json FROM events WHERE session_id = ?1 AND action = ?2")
        .map_err(failed)?;
    let rows = stmt
        .query_map(params![MARKER, REANCHOR], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .map_err(failed)?;
    let mut prior = BTreeSet::new();
    for row in rows {
        let (to, detail) = row.map_err(failed)?;
        if !home.contains(&to) {
            continue;
        }
        let from = detail
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|value| value.get("from").cloned())
            .and_then(|value| serde_json::from_value::<Vec<String>>(value).ok())
            .unwrap_or_default();
        prior.extend(from);
    }
    Ok(prior.into_iter().collect())
}

/// The newest identity record: what the ledger last knew this repository to be.
fn latest_identity(conn: &Connection) -> Result<Option<Identity>, LedgerError> {
    let detail: Option<Option<String>> = conn
        .query_row(
            "SELECT detail_json FROM events WHERE session_id = ?1 AND action IN (?2, ?3)
             ORDER BY id DESC LIMIT 1",
            params![MARKER, ANCHOR, REANCHOR],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| LedgerError::new("query_failed", e.to_string()))?;
    Ok(detail
        .flatten()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| Identity::from_detail(&value)))
}

/// Whether `prior` is, today, a Git repository other than the one at `home`.
/// Filesystem only: a path from ledger content is never a place to run Git.
fn occupied(prior: &str, home: &Path) -> bool {
    match Path::new(prior).canonicalize() {
        Ok(real) => real != home && crate::engine::git_cli::is_git_repository(&real),
        Err(_) => false,
    }
}

fn record(
    conn: &Connection,
    repo_path: &str,
    action: &str,
    detail: String,
) -> Result<i64, LedgerError> {
    let draft = Draft {
        repo_path: repo_path.to_string(),
        actor_kind: Some(ActorKind::System),
        session_id: Some(MARKER.to_string()),
        action: action.to_string(),
        detail_json: Some(detail),
        ..Default::default()
    };
    insert(conn, &prepare(&draft)?)
}

/// Forgets what this process settled for `repo_path`, so a test can meet the
/// first open again.
#[cfg(test)]
pub(crate) fn forget(repo_path: &str) {
    states()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&super::ledger_path(repo_path));
}

/// What [`settle`] did, or is waiting out, for one ledger in this process.
enum State {
    Settled,
    Waiting {
        until: Instant,
        refusal: Option<LedgerError>,
    },
}

fn states() -> &'static Mutex<std::collections::HashMap<PathBuf, State>> {
    static STATES: OnceLock<Mutex<std::collections::HashMap<PathBuf, State>>> = OnceLock::new();
    STATES.get_or_init(Default::default)
}

/// Makes the ledger of `repo_path` carry its repository's identity, and takes
/// it over when it was moved here. `Ok(Some(prior paths))` only on the call
/// that re-anchored it, which is the one that reports it; `Ok(None)` when
/// nothing changed or nothing could be told; `Err` when the ledger holds rows
/// of a repository that is not this one.
///
/// Costs one Git read and one transaction the first time a process sees a
/// ledger, and nothing after: the result is cached. A ledger that could not be
/// settled is looked at again after [`RETRY_AFTER`], so that fixing the cause
/// (removing the stale copy) heals it without a restart.
pub(crate) fn settle(repo_path: &str) -> Result<Option<Vec<String>>, LedgerError> {
    let db = super::ledger_path(repo_path);
    {
        let states = states()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match states.get(&db) {
            // Settled once is not settled for good: a repository moved back to a
            // path this process already saw has foreign rows again. One indexed
            // probe tells the two apart without Git.
            Some(State::Settled)
                if super::with_conn(repo_path, |conn| {
                    super::refuse_foreign_rows(conn, repo_path)
                })
                .is_ok() =>
            {
                return Ok(None)
            }
            Some(State::Waiting { until, refusal }) if Instant::now() < *until => {
                return refusal.clone().map_or(Ok(None), Err);
            }
            _ => {}
        }
    }
    // Git runs before the ledger's lock is taken, never under it.
    let current = Identity::read(repo_path);
    let result = super::with_conn(repo_path, |conn| settle_in(conn, repo_path, &current));
    let next = match &result {
        Ok(_) if current.is_ok() => State::Settled,
        Ok(_) => State::Waiting {
            until: Instant::now() + RETRY_AFTER,
            refusal: None,
        },
        Err(e) if e.code == "repository_mismatch" => State::Waiting {
            until: Instant::now() + RETRY_AFTER,
            refusal: Some(e.clone()),
        },
        Err(_) => return result,
    };
    states()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(db, next);
    if let Ok(Some(from)) = &result {
        log::warn!(
            "ledger of {repo_path} re-anchored: its rows named {} earlier location(s) of this repository",
            from.len()
        );
    }
    result
}

fn settle_in(
    conn: &Connection,
    repo_path: &str,
    current: &Result<Identity, String>,
) -> Result<Option<Vec<String>>, LedgerError> {
    let failed = |e: rusqlite::Error| LedgerError::new("query_failed", e.to_string());
    let tx = write_tx(conn).map_err(failed)?;
    let home = home_spellings(repo_path);
    let mut allowed = home.clone();
    allowed.extend(aliases(&tx, &home)?);
    allowed.sort();
    allowed.dedup();
    let foreign = foreign_paths(&tx, &allowed)?;
    let latest = latest_identity(&tx)?;
    let canonical = canonical_repo(repo_path);

    if foreign.is_empty() {
        if let Ok(identity) = current {
            if latest.as_ref() != Some(identity) {
                record(&tx, &canonical, ANCHOR, identity.to_detail(&[]))?;
            }
        }
        tx.commit().map_err(failed)?;
        return Ok(None);
    }

    let identity = current.as_ref().map_err(|e| {
        mismatch(format!(
            "this repository's identity could not be read ({e})"
        ))
    })?;
    let recorded = latest.ok_or_else(|| {
        mismatch(
            "it records no repository identity, so a moved or copied repository cannot be told from a foreign one",
        )
    })?;
    same_repository(&recorded, identity).map_err(mismatch)?;
    if foreign.len() > MAX_PRIOR_PATHS {
        return Err(mismatch(format!(
            "it names more than {MAX_PRIOR_PATHS} other repository locations"
        )));
    }
    let home_dir = Path::new(&canonical);
    if let Some(live) = foreign.iter().find(|prior| occupied(prior, home_dir)) {
        return Err(mismatch(format!(
            "{live} is still a Git repository: this is a copy or another clone, not a move"
        )));
    }
    let cursor = record(&tx, &canonical, REANCHOR, identity.to_detail(&foreign))?;
    tx.commit().map_err(failed)?;
    super::announce(repo_path, cursor);
    Ok(Some(foreign))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(roots: &[&str], remotes: &[&str]) -> Identity {
        Identity {
            roots: roots.iter().map(|s| (*s).to_string()).collect(),
            remotes: remotes.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    #[test]
    fn urls_that_name_one_upstream_normalise_alike_and_keep_no_credentials() {
        let one = normalise_url("https://user:hunter2@GitHub.com/Acme/Repo.git/");
        assert_eq!(one, "https://github.com/acme/repo");
        assert_eq!(
            normalise_url("git@github.com:Acme/Repo.git"),
            "github.com:acme/repo"
        );
        assert!(!one.contains("hunter2") && !one.contains("user"));
        assert_eq!(normalise_url("/srv/git/repo.git"), "/srv/git/repo");
    }

    #[test]
    fn remotes_are_read_from_config_lines_only() {
        let config = "core.bare=false\nremote.origin.url=git@h:a/b.git\nremote.up.stream.url=https://h/c\nremote.origin.pushurl=https://x/y\nremote.origin.fetch=+refs/heads/*:refs/remotes/origin/*\n";
        let remotes = remotes_in(config);
        assert_eq!(
            remotes.into_iter().collect::<Vec<_>>(),
            vec!["h:a/b".to_string(), "https://h/c".to_string()]
        );
    }

    /// Every foreign-row check on a re-anchored ledger reads the re-anchor rows;
    /// that read must seek, not scan the ledger, or each gated mutation and hook
    /// process would pay for the ledger's whole history.
    #[test]
    fn the_re_anchor_lookups_seek_their_index() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_str().unwrap().to_string();
        let plans = super::super::with_conn(&repo, |conn| {
            let mut plans = Vec::new();
            for sql in [
                "SELECT repo_path, detail_json FROM events WHERE session_id = ?1 AND action = ?2",
                "SELECT detail_json FROM events WHERE session_id = ?1 AND action IN (?2, ?3)
                 ORDER BY id DESC LIMIT 1",
            ] {
                let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
                let width = stmt.parameter_count();
                let args: Vec<&str> = vec!["a"; width];
                let rows = stmt
                    .query_map(rusqlite::params_from_iter(args), |row| {
                        row.get::<_, String>(3)
                    })
                    .unwrap();
                plans.push(rows.map(Result::unwrap).collect::<Vec<_>>().join(" | "));
            }
            Ok(plans)
        })
        .unwrap();
        for plan in plans {
            assert!(plan.contains("idx_events_session"), "{plan}");
            assert!(!plan.contains("SCAN"), "{plan}");
        }
    }

    #[test]
    fn the_rule_roots_decide_and_remotes_only_contradict() {
        let recorded = identity(&["r1"], &["a"]);
        assert!(same_repository(&recorded, &identity(&["r1"], &["a"])).is_ok());
        // A new root (an orphan branch) and a new remote are growth.
        assert!(same_repository(&recorded, &identity(&["r1", "r2"], &["a", "b"])).is_ok());
        // A rewritten root is another repository.
        assert!(same_repository(&recorded, &identity(&["r9"], &["a"])).is_err());
        assert!(same_repository(&recorded, &identity(&["r1", "r2"], &["a"])).is_ok());
        // A fork: the same history behind a different remote.
        assert!(same_repository(&recorded, &identity(&["r1"], &["b"])).is_err());
        // No remote on either side says nothing against.
        assert!(same_repository(&recorded, &identity(&["r1"], &[])).is_ok());
        assert!(same_repository(&identity(&["r1"], &[]), &identity(&["r1"], &["b"])).is_ok());
        // Nothing recorded cannot vouch for anything.
        assert!(same_repository(&identity(&[], &["a"]), &identity(&["r1"], &["a"])).is_err());
    }
}
