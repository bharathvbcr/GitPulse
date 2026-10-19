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
//! The same detail JSON also carries a **lineage UUID** (minted once, carried
//! through every later record) and the **site**: the device and inode of the
//! checkout directory when the record was written. Old binaries read only
//! `roots`, `remotes` and `from` and ignore the rest; a record without them is
//! legacy and is given them on the next settle.
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
//! 4. when the record names a site, this checkout directory is that very
//!    directory. A rename keeps the inode; a sibling clone, a history-merging
//!    repository and any other copy are different directories, whether or not
//!    the original still exists. A record with no site (legacy, or a platform
//!    with no inode) falls back to the roots-only rule;
//! 5. no prior path is still occupied by a Git repository of its own. A
//!    checkout that still exists at the old path is a copy or an independent
//!    clone, not a move, and is never absorbed. Nothing is run at a prior path:
//!    it comes from ledger content, which a checkout's writer controls. The
//!    probe runs before the write transaction, never inside it, and the
//!    transaction only rechecks that the set of prior paths is the one probed.
//!
//! The prior paths are then recorded in a [`REANCHOR`] row naming the new path;
//! history is not rewritten. A row is foreign unless it names this path or a
//! path some re-anchor row *to this path* lists.
//!
//! # What this does not claim
//!
//! The ledger lives inside the checkout, so whoever can write the checkout can
//! write any row, including an identity record: the guard catches accidents
//! and copies, not a deliberate forger. The lineage UUID lives only in the
//! ledger, so it is identical in a ledger and in every copy of it and cannot
//! tell them apart; the site is what does. The site is not a marker in the
//! repository (nothing is written into `.git`), so a move that changes the
//! directory's inode (a cross-device `mv`, a restore from backup at a new path)
//! is refused like a copy, and an inode reused by a new checkout after the
//! original was deleted is indistinguishable from the original.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rusqlite::types::ValueRef;
use rusqlite::{params, Connection, OptionalExtension};

use super::redact;
use super::{
    canonical_repo, foreign_row_sql, home_spellings, insert, prepare, write_tx, ActorKind, Draft,
    LedgerError,
};
use crate::engine::ref_cache::mono_now;

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
/// More re-anchor rows addressed to one repository than any real history has,
/// and more prior paths in total. Past either, the ledger is not one
/// repository that moved, and a list this long would also outgrow what one
/// SQL expression may hold.
const MAX_ALIAS_ROWS: usize = 256;
const MAX_ALIASES: usize = 256;
/// Bound on checking whether the prior paths are still repositories. A path
/// comes from ledger content and may sit on a mount that never answers.
const OCCUPIED_BUDGET: Duration = Duration::from_secs(5);
/// How long a repository whose identity could not be settled is left alone.
/// Measured on `ref_cache::mono_now`, the repo's clock seam, so a test moves it
/// by hand instead of sleeping.
const RETRY_AFTER: Duration = Duration::from_secs(30);
/// Probes of prior paths that may be unanswered at once, across the process.
/// One per path (a retry reuses it), and no more than this in total.
const MAX_OUTSTANDING_PROBES: usize = 64;
/// How many times one settle re-reads the prior paths when the ledger gained
/// some between the probe and the transaction.
const SETTLE_PASSES: usize = 2;
/// The code of the internal "the prior paths changed under the probe" answer.
const STALE_PROBE: &str = "identity_probe_stale";
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

    fn to_detail(&self, from: &[String], binding: Option<&Binding>) -> String {
        let mut detail = serde_json::json!({
            "roots": self.roots,
            "remotes": self.remotes,
        });
        if !from.is_empty() {
            detail["from"] = serde_json::json!(from);
        }
        if let Some(binding) = binding {
            detail["uuid"] = serde_json::json!(binding.uuid);
            if let Some((dev, ino)) = binding.site {
                detail["site"] = serde_json::json!({ "dev": dev, "ino": ino });
            }
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

/// The device and inode of a checkout directory.
type Site = (u64, u64);

/// What an identity record says beyond roots and remotes: which ledger lineage
/// it is, and which directory it was written in.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Binding {
    uuid: String,
    site: Option<Site>,
}

impl Binding {
    /// `None` for a legacy record (no usable `uuid`). A `site` that is not two
    /// integers is absent, never half-read.
    fn from_detail(detail: &serde_json::Value) -> Option<Binding> {
        let uuid = detail.get("uuid")?.as_str()?;
        if uuid.is_empty() || uuid.len() > 64 || uuid.chars().any(char::is_control) {
            return None;
        }
        let site = detail
            .get("site")
            .and_then(|site| Some((site.get("dev")?.as_u64()?, site.get("ino")?.as_u64()?)));
        Some(Binding {
            uuid: uuid.to_string(),
            site,
        })
    }
}

/// What the ledger last recorded about its repository.
struct Recorded {
    identity: Identity,
    binding: Option<Binding>,
}

/// What this process reads of the repository before it touches the ledger.
struct Observed {
    identity: Result<Identity, String>,
    site: Option<Site>,
}

impl Observed {
    fn read(repo_path: &str) -> Observed {
        Observed {
            identity: Identity::read(repo_path),
            site: site_of(Path::new(&canonical_repo(repo_path))),
        }
    }
}

/// The site of the directory at `path`, where the platform has inodes.
fn site_of(path: &Path) -> Option<Site> {
    super::file_id(path).map(|id| (id.0, id.1))
}

/// A random version-4 UUID. A lineage name, never a credential: nothing is
/// authorised by knowing it.
fn new_uuid() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let part = |salt: u64| {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(salt);
        hasher.write_u64(super::ids::now_millis());
        hasher.finish().to_be_bytes()
    };
    let (high, low) = (part(1), part(2));
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&high);
    b[8..].copy_from_slice(&low);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
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
            // Redacted here, not only when the row is written: the ledger
            // stores what the redactor leaves, and a remote it would alter
            // must compare equal to its own stored form or a moved repository
            // is refused as a fork of itself.
            (!name.is_empty()).then(|| redact::text(&normalise_url(value)))
        })
        .filter(|url| !url.is_empty())
        .collect()
}

/// A remote URL with credentials, case, a trailing slash and `.git` removed,
/// so the same upstream spelled two ways is one remote and no secret is kept.
fn normalise_url(url: &str) -> String {
    // A query or fragment is never part of where a repository lives, and is
    // where tokens go: `https://host/x.git?token=...`.
    let url = url.trim();
    let url = url.split(['?', '#']).next().unwrap_or_default();
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
    // Lowercased before `.git` is cut: `REPO.GIT` is the same upstream.
    let lowered = without_user.to_ascii_lowercase();
    let trimmed = lowered.trim_end_matches('/');
    trimmed
        .strip_suffix(".git")
        .unwrap_or(trimmed)
        .trim_end_matches('/')
        .to_string()
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

/// The SQL that finds the re-anchor rows addressed to a repository's spellings.
fn aliases_sql() -> String {
    format!(
        "SELECT detail_json FROM events
         WHERE session_id = ?1 AND action = ?2 AND repo_path IN (?3, ?4)
         ORDER BY id LIMIT {}",
        MAX_ALIAS_ROWS + 1
    )
}

/// The prior locations of the repository at `home`, as its re-anchor rows
/// recorded them. Seeks `idx_events_session`; reads nothing else.
///
/// Bounded: no move writes more than [`MAX_PRIOR_PATHS`] paths in a row, and
/// no history has more than [`MAX_ALIAS_ROWS`] such rows or [`MAX_ALIASES`]
/// paths. A ledger that does is refused as foreign, not read to the end. A row
/// whose detail is not what [`settle`] writes grants nothing.
pub(super) fn aliases(conn: &Connection, home: &[String]) -> Result<Vec<String>, LedgerError> {
    let failed = |e: rusqlite::Error| LedgerError::new("query_failed", e.to_string());
    let Some(first) = home.first() else {
        return Ok(Vec::new());
    };
    let second = home.get(1).unwrap_or(first);
    let mut stmt = conn.prepare(&aliases_sql()).map_err(failed)?;
    let rows = stmt
        .query_map(params![MARKER, REANCHOR, first, second], |row| {
            Ok(match row.get_ref(0)? {
                ValueRef::Text(bytes) | ValueRef::Blob(bytes) => Some(bytes.to_vec()),
                _ => None,
            })
        })
        .map_err(failed)?;
    let mut prior = BTreeSet::new();
    for (seen, row) in rows.enumerate() {
        let detail = row.map_err(failed)?;
        if seen >= MAX_ALIAS_ROWS {
            return Err(mismatch(format!(
                "it holds more than {MAX_ALIAS_ROWS} re-anchor records for this repository"
            )));
        }
        let from = detail
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|value| value.get("from").cloned())
            .and_then(|value| serde_json::from_value::<Vec<String>>(value).ok())
            .unwrap_or_default();
        let too_long = from.len() > MAX_PRIOR_PATHS;
        prior.extend(from);
        if too_long || prior.len() > MAX_ALIASES {
            return Err(mismatch(
                "a re-anchor record names more locations than a repository has had",
            ));
        }
    }
    Ok(prior.into_iter().collect())
}

/// The newest identity record: what the ledger last knew this repository to be.
fn latest_identity(conn: &Connection) -> Result<Option<Recorded>, LedgerError> {
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
        .and_then(|value| {
            Some(Recorded {
                identity: Identity::from_detail(&value)?,
                binding: Binding::from_detail(&value),
            })
        }))
}

/// The real path of `prior` when it is, today, a Git repository; `None` when it
/// is not (or does not exist). Filesystem only: a path from ledger content is
/// never a place to run Git. Independent of which repository asks, so one
/// answer serves every caller.
fn repository_at(prior: &str) -> Option<PathBuf> {
    let real = Path::new(prior).canonicalize().ok()?;
    crate::engine::git_cli::is_git_repository(&real).then_some(real)
}

/// Whether a prior path names a place a repository could have been: absolute
/// and free of control characters, as `git_cli::validate_repo_path` demands.
/// A relative path would be resolved against this process's working directory.
fn is_location(prior: &str) -> bool {
    Path::new(prior).is_absolute() && !prior.chars().any(char::is_control)
}

/// Asks whether a path is a repository: `Some(real path)` when it is.
type Probe = Arc<dyn Fn(&str) -> Option<PathBuf> + Send + Sync>;

/// Where one probe stands.
enum Answer {
    Pending,
    Done(Option<PathBuf>),
    Died,
}

/// One outstanding probe of one path, shared by every caller that asks while
/// it is unanswered.
struct Slot {
    answer: Mutex<Answer>,
    ready: Condvar,
}

/// Probes prior paths on worker threads, at most one outstanding per path and
/// [`MAX_OUTSTANDING_PROBES`] in all. A mount that never answers costs one
/// parked thread, however many retries ask about it: later callers wait on
/// that probe's slot, and the thread removes it when it finally answers.
struct Prober {
    budget: Duration,
    probe: Probe,
    slots: Arc<Mutex<HashMap<String, Arc<Slot>>>>,
}

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Prober {
    fn new(budget: Duration, probe: Probe) -> Prober {
        Prober {
            budget,
            probe,
            slots: Arc::default(),
        }
    }

    /// The probe slot of `path`: the outstanding one, or a fresh one with its
    /// worker started.
    fn slot(&self, path: &str) -> Result<Arc<Slot>, String> {
        let mut slots = locked(&self.slots);
        if let Some(slot) = slots.get(path) {
            return Ok(Arc::clone(slot));
        }
        if slots.len() >= MAX_OUTSTANDING_PROBES {
            return Err(format!(
                "{MAX_OUTSTANDING_PROBES} earlier locations are still unanswered"
            ));
        }
        let slot = Arc::new(Slot {
            answer: Mutex::new(Answer::Pending),
            ready: Condvar::new(),
        });
        slots.insert(path.to_string(), Arc::clone(&slot));
        let (worker, registry, probe, key) = (
            Arc::clone(&slot),
            Arc::clone(&self.slots),
            Arc::clone(&self.probe),
            path.to_string(),
        );
        let started = std::thread::Builder::new()
            .name("ledger-anchor-occupied".into())
            .spawn(move || {
                let outcome =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| probe(&key)));
                *locked(&worker.answer) = outcome.map_or(Answer::Died, Answer::Done);
                worker.ready.notify_all();
                let mut slots = locked(&registry);
                if slots
                    .get(&key)
                    .is_some_and(|held| Arc::ptr_eq(held, &worker))
                {
                    slots.remove(&key);
                }
            });
        if let Err(e) = started {
            slots.remove(path);
            return Err(format!("could not check the earlier locations: {e}"));
        }
        Ok(slot)
    }

    /// The real path of `path` when it is a repository, or `Err` when the probe
    /// did not answer by `deadline` (or died): an unanswered question is a
    /// refusal, never "vacant".
    fn ask(&self, path: &str, deadline: Instant) -> Result<Option<PathBuf>, String> {
        let slot = self.slot(path)?;
        let wait = deadline.saturating_duration_since(Instant::now());
        let (answer, _) = slot
            .ready
            .wait_timeout_while(locked(&slot.answer), wait, |a| matches!(a, Answer::Pending))
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*answer {
            Answer::Done(found) => Ok(found.clone()),
            Answer::Died => Err("checking an earlier location failed".to_string()),
            Answer::Pending => Err(format!(
                "the earlier locations could not be checked within {:?}",
                self.budget
            )),
        }
    }

    /// The first of `paths` that is a repository other than `home`, or `Err`
    /// when the whole question could not be answered within the budget.
    fn first_occupied(&self, paths: &[String], home: &Path) -> Result<Option<String>, String> {
        let deadline = Instant::now() + self.budget;
        for path in paths {
            if self.ask(path, deadline)?.is_some_and(|real| real != home) {
                return Ok(Some(path.clone()));
            }
        }
        Ok(None)
    }

    /// Probes the prior paths a settle is about to judge, outside any ledger
    /// transaction. Only a plausible list is probed: one `settle_in` would
    /// refuse for its length or shape anyway is left unprobed.
    fn probe(&self, foreign: Vec<String>, home: &Path) -> Probed {
        if foreign.len() > MAX_PRIOR_PATHS || foreign.iter().any(|path| !is_location(path)) {
            return Probed::default();
        }
        let verdict = self.first_occupied(&foreign, home);
        Probed {
            checked: foreign.into_iter().collect(),
            verdict,
        }
    }
}

/// What probing the prior paths found, and which paths it covered.
struct Probed {
    checked: BTreeSet<String>,
    verdict: Result<Option<String>, String>,
}

impl Default for Probed {
    fn default() -> Self {
        Probed {
            checked: BTreeSet::new(),
            verdict: Ok(None),
        }
    }
}

fn prober() -> &'static Prober {
    static PROBER: OnceLock<Prober> = OnceLock::new();
    PROBER.get_or_init(|| Prober::new(OCCUPIED_BUDGET, Arc::new(repository_at)))
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
    settle_with(repo_path, prober())
}

fn settle_with(repo_path: &str, prober: &Prober) -> Result<Option<Vec<String>>, LedgerError> {
    let db = super::ledger_path(repo_path);
    // Read under the lock, acted on outside it: the probe below waits for the
    // ledger's own lock, and that wait must not hold every other repository's
    // settle behind this map.
    let cached = {
        let states = states()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match states.get(&db) {
            Some(State::Settled) => Some(None),
            Some(State::Waiting { until, refusal }) if mono_now() < *until => {
                Some(Some(refusal.clone()))
            }
            _ => None,
        }
    };
    match cached {
        // Settled once is not settled for good: a repository moved back to a
        // path this process already saw has foreign rows again. One indexed
        // probe tells the two apart without Git.
        Some(None) => {
            if super::with_conn(repo_path, |conn| {
                super::refuse_foreign_rows(conn, repo_path)
            })
            .is_ok()
            {
                return Ok(None);
            }
        }
        Some(Some(refusal)) => return refusal.map_or(Ok(None), Err),
        None => {}
    }
    // Git and the probe of prior paths run before the ledger's write
    // transaction is opened, never under it: a mount that never answers must
    // not hold every other writer for the probe's budget.
    let seen = Observed::read(repo_path);
    let home = PathBuf::from(canonical_repo(repo_path));
    let mut result = Err(LedgerError::new(STALE_PROBE, "the prior locations changed"));
    for _ in 0..SETTLE_PASSES {
        let foreign = match super::with_conn(repo_path, |conn| {
            Ok(foreign_of(conn, repo_path).unwrap_or_default())
        }) {
            Ok(foreign) => foreign,
            Err(e) => return Err(e),
        };
        let probed = prober.probe(foreign, &home);
        result = super::with_conn(repo_path, |conn| settle_in(conn, repo_path, &seen, &probed));
        if !matches!(&result, Err(e) if e.code == STALE_PROBE) {
            break;
        }
    }
    if matches!(&result, Err(e) if e.code == STALE_PROBE) {
        result = Err(mismatch(
            "the ledger kept gaining earlier locations while they were being checked",
        ));
    }
    let next = match &result {
        Ok(_) if seen.identity.is_ok() => State::Settled,
        Ok(_) => State::Waiting {
            until: mono_now() + RETRY_AFTER,
            refusal: None,
        },
        Err(e) if e.code == "repository_mismatch" => State::Waiting {
            until: mono_now() + RETRY_AFTER,
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

/// The spellings of other repositories' paths that rows of this ledger name and
/// no re-anchor record to this repository vouches for.
fn foreign_of(conn: &Connection, repo_path: &str) -> Result<Vec<String>, LedgerError> {
    let home = home_spellings(repo_path);
    let mut allowed = home.clone();
    allowed.extend(aliases(conn, &home)?);
    allowed.sort();
    allowed.dedup();
    foreign_paths(conn, &allowed)
}

/// Judges the ledger under one write transaction. `probed` was gathered before
/// the transaction opened; it is only trusted for the prior paths it covered,
/// and a ledger that has gained others since is answered [`STALE_PROBE`] for
/// the caller to probe again.
fn settle_in(
    conn: &Connection,
    repo_path: &str,
    seen: &Observed,
    probed: &Probed,
) -> Result<Option<Vec<String>>, LedgerError> {
    let failed = |e: rusqlite::Error| LedgerError::new("query_failed", e.to_string());
    let tx = write_tx(conn).map_err(failed)?;
    let foreign = foreign_of(&tx, repo_path)?;
    let latest = latest_identity(&tx)?;
    let canonical = canonical_repo(repo_path);
    let kept = latest.as_ref().and_then(|r| r.binding.as_ref());

    if foreign.is_empty() {
        if let Ok(identity) = &seen.identity {
            // The lineage is carried; a legacy record is given one here, and a
            // site that could not be read this time keeps the one recorded.
            let binding = Binding {
                uuid: kept.map_or_else(new_uuid, |b| b.uuid.clone()),
                site: seen.site.or(kept.and_then(|b| b.site)),
            };
            let current = latest
                .as_ref()
                .is_some_and(|r| &r.identity == identity && r.binding.as_ref() == Some(&binding));
            if !current {
                record(
                    &tx,
                    &canonical,
                    ANCHOR,
                    identity.to_detail(&[], Some(&binding)),
                )?;
            }
        }
        tx.commit().map_err(failed)?;
        return Ok(None);
    }

    let identity = seen.identity.as_ref().map_err(|e| {
        mismatch(format!(
            "this repository's identity could not be read ({e})"
        ))
    })?;
    let recorded = latest.as_ref().ok_or_else(|| {
        mismatch(
            "it records no repository identity, so a moved or copied repository cannot be told from a foreign one",
        )
    })?;
    same_repository(&recorded.identity, identity).map_err(mismatch)?;
    if let Some(Binding {
        site: Some(anchored),
        ..
    }) = &recorded.binding
    {
        match seen.site {
            Some(here) if here == *anchored => {}
            Some(_) => {
                return Err(mismatch(
                    "this checkout is not the directory the ledger was anchored in: a copy or another clone, not a move",
                ))
            }
            None => {
                return Err(mismatch(
                    "the ledger was anchored to a directory and this checkout's directory could not be read",
                ))
            }
        }
    }
    if foreign.len() > MAX_PRIOR_PATHS {
        return Err(mismatch(format!(
            "it names more than {MAX_PRIOR_PATHS} other repository locations"
        )));
    }
    if let Some(odd) = foreign.iter().find(|prior| !is_location(prior)) {
        return Err(mismatch(format!(
            "it names {odd:?}, which is no absolute repository path"
        )));
    }
    if foreign.iter().any(|prior| !probed.checked.contains(prior)) {
        return Err(LedgerError::new(
            STALE_PROBE,
            "the ledger gained earlier locations since they were probed",
        ));
    }
    match &probed.verdict {
        Err(unanswered) => return Err(mismatch(unanswered)),
        Ok(Some(live)) => {
            return Err(mismatch(format!(
                "{live} is still a Git repository: this is a copy or another clone, not a move"
            )))
        }
        Ok(None) => {}
    }
    let binding = Binding {
        uuid: recorded
            .binding
            .as_ref()
            .map_or_else(new_uuid, |b| b.uuid.clone()),
        site: seen.site,
    };
    let cursor = record(
        &tx,
        &canonical,
        REANCHOR,
        identity.to_detail(&foreign, Some(&binding)),
    )?;
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
                &aliases_sql(),
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

    /// A fresh repository directory name and the open ledger of it.
    fn home() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        (dir, path)
    }

    /// An ordinary event row of `repo_path`, stored under that spelling as is.
    fn row_of(conn: &Connection, repo_path: &str) -> i64 {
        let id = insert(
            conn,
            &prepare(&Draft {
                repo_path: "/placeholder".into(),
                action: "git.commit".into(),
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();
        conn.execute(
            "UPDATE events SET repo_path = ?1 WHERE id = ?2",
            params![repo_path, id],
        )
        .unwrap();
        id
    }

    fn reanchor_to(conn: &Connection, to: &str, from: &[String]) -> i64 {
        record(
            conn,
            to,
            REANCHOR,
            Identity::default().to_detail(from, None),
        )
        .unwrap()
    }

    /// What `settle_with` does around `settle_in`, minus the process cache: read
    /// the prior paths, probe them with no transaction open, then judge.
    fn judge(
        conn: &Connection,
        repo: &str,
        seen: &Observed,
        prober: &Prober,
    ) -> Result<Option<Vec<String>>, LedgerError> {
        let foreign = foreign_of(conn, repo).unwrap_or_default();
        let probed = prober.probe(foreign, Path::new(&canonical_repo(repo)));
        settle_in(conn, repo, seen, &probed)
    }

    fn real_prober() -> Prober {
        Prober::new(OCCUPIED_BUDGET, Arc::new(repository_at))
    }

    /// An observation of a repository with `recorded`'s identity and no site.
    fn seen(recorded: &Identity) -> Observed {
        Observed {
            identity: Ok(recorded.clone()),
            site: None,
        }
    }

    fn refusal(conn: &Connection, repo: &str) -> Option<String> {
        super::super::refuse_foreign_rows(conn, repo)
            .err()
            .map(|e| e.code.to_string())
    }

    /// Threat: a forged ledger lists thousands of prior paths. The guard must
    /// answer "foreign" (repository_mismatch), not die inside SQLite's
    /// expression-depth limit with a different error, and must not honour a
    /// record no real move could have written.
    #[test]
    fn a_re_anchor_naming_thousands_of_paths_is_refused_as_foreign() {
        let (_dir, repo) = home();
        super::super::with_conn(&repo, |conn| {
            let from: Vec<String> = (0..10_000).map(|i| format!("/gone/{i}")).collect();
            reanchor_to(conn, &repo, &from);
            row_of(conn, "/gone/0");
            assert_eq!(refusal(conn, &repo).as_deref(), Some("repository_mismatch"));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn a_re_anchor_naming_more_paths_than_a_move_can_is_not_honoured() {
        let (_dir, repo) = home();
        super::super::with_conn(&repo, |conn| {
            let from: Vec<String> = (0..=MAX_PRIOR_PATHS)
                .map(|i| format!("/gone/{i}"))
                .collect();
            reanchor_to(conn, &repo, &from);
            row_of(conn, "/gone/0");
            assert_eq!(refusal(conn, &repo).as_deref(), Some("repository_mismatch"));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn thousands_of_re_anchor_rows_to_this_repository_are_refused_as_foreign() {
        let (_dir, repo) = home();
        super::super::with_conn(&repo, |conn| {
            for i in 0..(2 * MAX_ALIAS_ROWS) {
                reanchor_to(conn, &repo, &[format!("/gone/{i}")]);
            }
            row_of(conn, "/gone/0");
            assert_eq!(refusal(conn, &repo).as_deref(), Some("repository_mismatch"));
            Ok(())
        })
        .unwrap();
    }

    /// A re-anchor row addressed to another repository grants this one nothing,
    /// however many there are, and one addressed here still does.
    #[test]
    fn re_anchor_rows_for_another_repository_grant_nothing() {
        let (_dir, repo) = home();
        super::super::with_conn(&repo, |conn| {
            for i in 0..1_000 {
                reanchor_to(conn, &format!("/other/{i}"), &["/elsewhere".to_string()]);
            }
            assert_eq!(
                aliases(conn, std::slice::from_ref(&repo)).unwrap(),
                Vec::<String>::new()
            );
            reanchor_to(conn, &repo, &["/elsewhere".to_string()]);
            assert_eq!(
                aliases(conn, std::slice::from_ref(&repo)).unwrap(),
                vec!["/elsewhere"]
            );
            Ok(())
        })
        .unwrap();
    }

    /// A re-anchor row whose detail is not what this module writes grants
    /// nothing and never breaks the answer: refused as foreign, not a query error.
    #[test]
    fn a_re_anchor_with_unreadable_detail_grants_nothing() {
        let (_dir, repo) = home();
        super::super::with_conn(&repo, |conn| {
            row_of(conn, "/gone");
            let details: [&str; 4] = [
                "CAST(x'ff66' AS TEXT)",
                "'{\"from\": \"/gone\"}'",
                "'{\"from\": [1, \"/gone\"]}'",
                "'not json'",
            ];
            for detail in details {
                let id = reanchor_to(conn, &repo, &[]);
                conn.execute(
                    &format!("UPDATE events SET detail_json = {detail} WHERE id = {id}"),
                    [],
                )
                .unwrap();
            }
            assert_eq!(refusal(conn, &repo).as_deref(), Some("repository_mismatch"));
            Ok(())
        })
        .unwrap();
    }

    /// Threat: a prior path that is not an absolute path is resolved against the
    /// process's working directory (or not at all) and so says nothing about
    /// whether the old location is gone: it is never a location of a repository.
    #[test]
    fn a_prior_path_that_is_no_absolute_path_is_never_taken_over() {
        for hostile in ["relative/dir", ".", "/old\nrepo", "/old\0repo", ""] {
            let (_dir, repo) = home();
            let recorded = identity(&["r1"], &[]);
            super::super::with_conn(&repo, |conn| {
                record(conn, &repo, ANCHOR, recorded.to_detail(&[], None)).unwrap();
                row_of(conn, hostile);
                let out = judge(conn, &repo, &seen(&recorded), &real_prober());
                assert_eq!(
                    out.err().map(|e| e.code.to_string()).as_deref(),
                    Some("repository_mismatch"),
                    "{hostile:?} was taken for an earlier location"
                );
                Ok(())
            })
            .unwrap();
        }
    }

    /// Threat: a prior path on a mount that never answers must not hang a pass.
    #[test]
    fn a_probe_that_never_answers_is_a_refusal_not_a_hang() {
        let (release, hold) = std::sync::mpsc::channel::<()>();
        let hold = Mutex::new(hold);
        let slow = Prober::new(
            Duration::from_millis(150),
            Arc::new(move |path: &str| {
                if path == "/net/slow" {
                    let _ = locked(&hold).recv_timeout(Duration::from_secs(30));
                }
                None
            }),
        );
        let started = Instant::now();
        let out = slow.first_occupied(
            &["/net/slow".to_string(), "/fast".to_string()],
            Path::new("/home"),
        );
        assert!(out.is_err(), "{out:?}");
        // Relaxed at most 4x (20 s), still short of the 30 s probe.
        assert!(started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(5)));
        drop(release);
        let found = Prober::new(
            Duration::from_secs(5),
            Arc::new(|path: &str| (path == "/b").then(|| PathBuf::from(path))),
        );
        assert_eq!(
            found.first_occupied(&["/a".to_string(), "/b".to_string()], Path::new("/home")),
            Ok(Some("/b".to_string()))
        );
        assert_eq!(
            found.first_occupied(&["/a".to_string()], Path::new("/home")),
            Ok(None)
        );
        // A prior path that is the repository's own home is no other repository.
        assert_eq!(
            found.first_occupied(&["/b".to_string()], Path::new("/b")),
            Ok(None)
        );
    }

    /// Residual: a hung mount left one parked thread per 30 s retry. At most one
    /// probe is outstanding per path, however often it is asked about, and a
    /// late answer is what every waiter then gets.
    #[test]
    fn a_hung_path_costs_one_probe_thread_however_often_it_is_asked() {
        let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (release, hold) = std::sync::mpsc::channel::<()>();
        let hold = Arc::new(Mutex::new(hold));
        let counted = Arc::clone(&started);
        let prober = Prober::new(
            Duration::from_millis(40),
            Arc::new(move |path: &str| {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = locked(&hold).recv_timeout(Duration::from_secs(30));
                Some(PathBuf::from(path))
            }),
        );
        let paths = ["/net/hung".to_string()];
        for _ in 0..8 {
            assert!(prober.first_occupied(&paths, Path::new("/home")).is_err());
        }
        assert_eq!(started.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(locked(&prober.slots).len(), 1);
        // The mount answers: the one probe finishes, its slot is released, and
        // the next ask is a fresh probe.
        release.send(()).unwrap();
        let deadline =
            Instant::now() + crate::test_support::coverage_relaxed(Duration::from_secs(5));
        while !locked(&prober.slots).is_empty() {
            assert!(
                Instant::now() < deadline,
                "the probe never released its slot"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(release);
    }

    /// The total is bounded too: more hung paths than the cap are refused
    /// rather than each given a thread.
    #[test]
    fn probes_outstanding_at_once_are_capped() {
        let (release, hold) = std::sync::mpsc::channel::<()>();
        let hold = Arc::new(Mutex::new(hold));
        let prober = Prober::new(
            Duration::from_millis(5),
            Arc::new(move |_: &str| {
                let _ = locked(&hold).recv_timeout(Duration::from_secs(30));
                None
            }),
        );
        let deadline = Instant::now() + Duration::from_millis(5);
        for i in 0..MAX_OUTSTANDING_PROBES {
            assert!(prober.ask(&format!("/hung/{i}"), deadline).is_err());
        }
        let over = prober.ask("/hung/over", deadline).unwrap_err();
        assert!(over.contains("still unanswered"), "{over}");
        assert_eq!(locked(&prober.slots).len(), MAX_OUTSTANDING_PROBES);
        drop(release);
    }

    /// Two processes settling one moved ledger: the second finds the first's
    /// re-anchor and adds nothing, on its own connection to the same file.
    #[test]
    fn a_second_connection_settling_a_moved_ledger_adds_no_second_record() {
        let (_dir, repo) = home();
        let recorded = identity(&["r1"], &["https://h/a"]);
        super::super::with_conn(&repo, |conn| {
            record(conn, &repo, ANCHOR, recorded.to_detail(&[], None)).unwrap();
            row_of(conn, "/gone");
            Ok(())
        })
        .unwrap();
        let db = super::super::ledger_path(&repo);
        let first = Connection::open(&db).unwrap();
        let second = Connection::open(&db).unwrap();
        let current = seen(&recorded);
        let prober = real_prober();
        assert_eq!(
            judge(&first, &repo, &current, &prober).unwrap(),
            Some(vec!["/gone".to_string()])
        );
        assert_eq!(judge(&second, &repo, &current, &prober).unwrap(), None);
        let count: i64 = first
            .query_row(
                "SELECT count(*) FROM events WHERE action = ?1",
                [REANCHOR],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    /// The foreign-row probe must keep seeking when the allowed list is as long
    /// as a ledger may legitimately make it.
    #[test]
    fn the_foreign_row_probe_seeks_with_a_full_prior_path_list() {
        let (_dir, repo) = home();
        for n in [3, 2 + MAX_PRIOR_PATHS, 2 + MAX_ALIASES] {
            let plan = super::super::with_conn(&repo, |conn| {
                let mut stmt = conn
                    .prepare(&format!(
                        "EXPLAIN QUERY PLAN SELECT 1 FROM events WHERE {} LIMIT 1",
                        foreign_row_sql(n)
                    ))
                    .unwrap();
                let args: Vec<String> = (0..n).map(|i| format!("/p/{i:05}")).collect();
                let rows = stmt
                    .query_map(rusqlite::params_from_iter(args), |row| {
                        row.get::<_, String>(3)
                    })
                    .unwrap();
                Ok(rows.map(Result::unwrap).collect::<Vec<_>>().join(" | "))
            })
            .unwrap();
            assert!(plan.contains("idx_events_repo_ts"), "n={n}: {plan}");
            assert!(!plan.contains("SCAN"), "n={n}: {plan}");
        }
    }

    /// Every spelling of a credential is dropped, and nothing of it reaches the
    /// ledger row, whatever the redactor does afterwards.
    #[test]
    fn no_spelling_of_a_credential_survives_normalisation() {
        for url in [
            "https://user:SECRETtok@host.test/x/y.git",
            "https://SECRETtok@host.test/x/y",
            "https://SECRETtok@host.test",
            "ssh://git:SECRETtok@host.test:22/x/y.git",
            "SECRETtok@host.test:x/y.git",
            "https://host.test/x/y.git?token=SECRETtok",
            "https://host.test/x/y.git#SECRETtok",
            "https://host.test/x/y.git?a=1#SECRETtok",
            "HTTPS://user:SECRETtok@[::1]:8443/x/y.git?access_token=SECRETtok",
            "file:///srv/git/y.git?token=SECRETtok",
        ] {
            let normal = normalise_url(url);
            assert!(!normal.contains("secrettok"), "{url} -> {normal}");
            assert!(!normal.contains("user"), "{url} -> {normal}");
            let config = format!("remote.up.stream.url={url}\nremote.o.pushurl={url}\n");
            let detail = Identity {
                roots: ["r1".to_string()].into(),
                remotes: remotes_in(&config),
            }
            .to_detail(&[], None);
            assert!(
                !detail.to_lowercase().contains("secrettok"),
                "{url}: {detail}"
            );
        }
    }

    #[test]
    fn spellings_of_one_upstream_are_equal_and_two_upstreams_are_not() {
        let one = [
            "https://user:tok@GitHub.com/Acme/Repo.git",
            "https://tok@github.com/acme/repo/",
            "https://github.com/acme/repo?x=1#frag",
            "HTTPS://GITHUB.COM/ACME/REPO.GIT/",
        ];
        for url in one {
            assert_eq!(normalise_url(url), "https://github.com/acme/repo", "{url}");
        }
        let different = [
            "https://github.com/acme/other",
            "https://gitlab.com/acme/repo",
            "https://github.com:8443/acme/repo",
            "ssh://github.com/acme/repo",
        ];
        for url in different {
            assert_ne!(normalise_url(url), "https://github.com/acme/repo", "{url}");
        }
    }

    /// What `settle` records must survive the write boundary's redactor: a
    /// remote that was altered on the way in would never again equal the live
    /// remote, and the moved repository would be refused for it.
    #[test]
    fn a_recorded_identity_reads_back_equal_after_the_redactor() {
        let identity = Identity {
            roots: ["4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_string()].into(),
            remotes: remotes_in(
                "remote.origin.url=https://gitlab.test/group/9f8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c/app.git\nremote.b.url=git@host.test:Team/Some-Repo_01.git\n",
            ),
        };
        let prepared = prepare(&Draft {
            repo_path: "/placeholder".into(),
            action: ANCHOR.into(),
            detail_json: Some(identity.to_detail(&[], None)),
            ..Default::default()
        })
        .unwrap();
        let stored: serde_json::Value = serde_json::from_str(&prepared.detail.unwrap()).unwrap();
        assert_eq!(Identity::from_detail(&stored), Some(identity));
    }

    /// A trusted repository of one commit, in a fresh directory.
    fn git_repo() -> (tempfile::TempDir, String) {
        let (dir, path) = home();
        let root = Path::new(&path);
        crate::test_support::git_in(root, &["init", "-b", "main"]);
        std::fs::write(root.join("seed.txt"), &path).unwrap();
        crate::test_support::git_in(root, &["add", "seed.txt"]);
        crate::test_support::git_in(root, &["commit", "-m", "seed"]);
        crate::test_support::trust_repo(root);
        (dir, path)
    }

    /// Threat to availability: one repository's busy ledger must not hold every
    /// other repository's identity check behind the process-wide state map.
    #[test]
    fn a_busy_ledger_does_not_stall_another_repositorys_settle() {
        let (_da, a) = git_repo();
        let (_db, b) = git_repo();
        settle(&a).unwrap();
        settle(&b).unwrap();
        // b is held for twice the (load-relaxed) limit plus the 300 ms head
        // start, so a settle of a that waited behind b always exceeds it.
        let limit = crate::test_support::coverage_relaxed(Duration::from_millis(1_000));
        let hold = limit * 2 + Duration::from_millis(300);
        let (locked_send, locked) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                super::super::with_conn(&b, |_| {
                    locked_send.send(()).unwrap();
                    std::thread::sleep(hold);
                    Ok(())
                })
                .unwrap();
            });
            locked.recv().unwrap();
            // Waits for b's ledger, as any pass over b does while it is busy.
            scope.spawn(|| settle(&b).unwrap());
            std::thread::sleep(Duration::from_millis(300));
            let started = Instant::now();
            settle(&a).unwrap();
            assert!(
                started.elapsed() < limit,
                "a's settle waited {:?} behind b",
                started.elapsed()
            );
        });
    }

    /// A git repository whose ledger holds a legacy identity record (as written
    /// before the lineage and site existed) and one row naming `prior`.
    fn moved_legacy_ledger(prior: &str) -> (tempfile::TempDir, String) {
        let (dir, repo) = git_repo();
        let identity = Identity::read(&repo).unwrap();
        super::super::with_conn(&repo, |conn| {
            record(conn, prior, ANCHOR, identity.to_detail(&[], None)).unwrap();
            row_of(conn, prior);
            Ok(())
        })
        .unwrap();
        (dir, repo)
    }

    /// Residual 1: the probe of prior paths (up to 5 s, on its own thread) ran
    /// inside `settle_in`'s write transaction, so a slow mount held the ledger's
    /// write lock for its budget. The probe here tries to take that lock, from
    /// another connection and through the registry's own, from the probe thread.
    #[test]
    fn the_probe_runs_with_no_ledger_lock_held() {
        let (_dir, repo) = moved_legacy_ledger("/gone/before");
        let write_lock_free = Arc::new(Mutex::new(None));
        let registry_free = Arc::new(Mutex::new(None));
        let (db, probed_repo) = (super::super::ledger_path(&repo), repo.clone());
        let (lock_seen, registry_seen) = (Arc::clone(&write_lock_free), Arc::clone(&registry_free));
        let prober = Prober::new(
            crate::test_support::coverage_relaxed(Duration::from_secs(3)),
            Arc::new(move |_: &str| {
                let other = Connection::open(&db).unwrap();
                other.busy_timeout(Duration::ZERO).unwrap();
                let took = other.execute_batch("BEGIN IMMEDIATE; ROLLBACK;").is_ok();
                *locked(&lock_seen) = Some(took);
                let reached = super::super::with_conn(&probed_repo, |_| Ok(())).is_ok();
                *locked(&registry_seen) = Some(reached);
                None
            }),
        );
        assert_eq!(
            settle_with(&repo, &prober).unwrap(),
            Some(vec!["/gone/before".to_string()])
        );
        assert_eq!(
            *locked(&write_lock_free),
            Some(true),
            "the write lock was held during the probe"
        );
        assert_eq!(
            *locked(&registry_free),
            Some(true),
            "the ledger connection was held during the probe"
        );
    }

    /// A prior path the ledger gained between the probe and the transaction was
    /// never probed: it is not taken on trust, and a fresh settle covers it.
    #[test]
    fn a_prior_path_the_probe_did_not_cover_is_not_taken_on_trust() {
        let (_dir, repo) = moved_legacy_ledger("/gone/one");
        super::super::with_conn(&repo, |conn| {
            row_of(conn, "/gone/two");
            Ok(())
        })
        .unwrap();
        let out = super::super::with_conn(&repo, |conn| {
            let probed = Probed {
                checked: ["/gone/one".to_string()].into(),
                verdict: Ok(None),
            };
            settle_in(conn, &repo, &Observed::read(&repo), &probed)
        });
        assert_eq!(out.unwrap_err().code, STALE_PROBE);
        assert_eq!(
            settle_with(&repo, &real_prober()).unwrap(),
            Some(vec!["/gone/one".to_string(), "/gone/two".to_string()])
        );
    }

    /// Residual 3: the 30 s heal runs on the repo's clock seam, with no sleep.
    #[test]
    fn a_refused_repository_is_looked_at_again_after_the_retry_interval() {
        let (_da, a) = git_repo();
        let (db, b) = home();
        crate::test_support::git_in(db.path(), &["clone", "-q", &a, &b]);
        crate::test_support::trust_repo(Path::new(&b));
        let identity = Identity::read(&b).unwrap();
        super::super::with_conn(&b, |conn| {
            record(conn, &a, ANCHOR, identity.to_detail(&[], None)).unwrap();
            row_of(conn, &a);
            Ok(())
        })
        .unwrap();
        let clock = crate::engine::ref_cache::ManualClock::install();
        let prober = real_prober();
        let live = settle_with(&b, &prober).unwrap_err();
        assert!(live.message.contains("copy or another clone"), "{live:?}");

        std::fs::remove_dir_all(&a).unwrap();
        clock.advance(RETRY_AFTER - Duration::from_secs(1));
        let waiting = settle_with(&b, &prober).unwrap_err();
        assert_eq!(waiting.message, live.message, "answered from the cache");
        clock.advance(Duration::from_secs(2));
        assert_eq!(settle_with(&b, &prober).unwrap(), Some(vec![a]));
    }

    fn bound(site: Option<Site>) -> String {
        let recorded = identity(&["r1"], &[]);
        recorded.to_detail(
            &[],
            Some(&Binding {
                uuid: "7f9c24e8-3b1a-4c5d-9e2f-0a1b2c3d4e5f".into(),
                site,
            }),
        )
    }

    fn observed_at(site: Option<Site>) -> Observed {
        Observed {
            identity: Ok(identity(&["r1"], &[])),
            site,
        }
    }

    /// The site decides between a move and a copy that has the same roots and
    /// remotes: a rename keeps the directory, anything else is another one.
    #[test]
    fn a_ledger_anchored_to_a_directory_follows_only_that_directory() {
        for (here, outcome) in [
            (Some((1, 2)), Ok(())),
            (Some((1, 3)), Err("another clone")),
            (Some((9, 2)), Err("another clone")),
            (None, Err("could not be read")),
        ] {
            let (_dir, repo) = home();
            super::super::with_conn(&repo, |conn| {
                record(conn, &repo, ANCHOR, bound(Some((1, 2)))).unwrap();
                row_of(conn, "/gone");
                let out = judge(conn, &repo, &observed_at(here), &real_prober());
                match outcome {
                    Ok(()) => assert_eq!(out.unwrap(), Some(vec!["/gone".to_string()])),
                    Err(why) => {
                        let e = out.unwrap_err();
                        assert!(e.message.contains(why), "{here:?}: {e:?}");
                        assert_eq!(e.code, "repository_mismatch");
                    }
                }
                Ok(())
            })
            .unwrap();
        }
    }

    /// The re-anchor record names the new directory, so a second move is judged
    /// against where the ledger last was, and the lineage is the same one.
    #[test]
    fn a_re_anchor_carries_the_lineage_and_the_new_site() {
        let (_dir, repo) = home();
        super::super::with_conn(&repo, |conn| {
            record(conn, "/gone", ANCHOR, bound(Some((1, 2)))).unwrap();
            row_of(conn, "/gone");
            judge(conn, &repo, &observed_at(Some((1, 2))), &real_prober()).unwrap();
            let latest = latest_identity(conn).unwrap().unwrap();
            assert_eq!(
                latest.binding,
                Some(Binding {
                    uuid: "7f9c24e8-3b1a-4c5d-9e2f-0a1b2c3d4e5f".into(),
                    site: Some((1, 2)),
                })
            );
            Ok(())
        })
        .unwrap();
    }

    /// Migration: a legacy record gets a lineage and a site on the next settle
    /// at home, once; a refresh keeps the lineage; a site that cannot be read
    /// this time keeps the recorded one.
    #[test]
    fn a_legacy_identity_record_is_given_a_lineage_on_the_next_settle() {
        let (_dir, repo) = home();
        let anchors = |conn: &Connection| -> i64 {
            conn.query_row(
                "SELECT count(*) FROM events WHERE action = ?1",
                [ANCHOR],
                |r| r.get(0),
            )
            .unwrap()
        };
        super::super::with_conn(&repo, |conn| {
            let recorded = identity(&["r1"], &[]);
            record(conn, &repo, ANCHOR, recorded.to_detail(&[], None)).unwrap();
            assert!(latest_identity(conn).unwrap().unwrap().binding.is_none());
            let prober = real_prober();
            judge(conn, &repo, &observed_at(Some((4, 5))), &prober).unwrap();
            let first = latest_identity(conn).unwrap().unwrap().binding.unwrap();
            assert_eq!(first.site, Some((4, 5)));
            assert_eq!(first.uuid.len(), 36);
            assert_eq!(
                first.uuid.as_bytes()[14],
                b'4',
                "a version-4 uuid: {}",
                first.uuid
            );
            assert_eq!(anchors(conn), 2);

            judge(conn, &repo, &observed_at(Some((4, 5))), &prober).unwrap();
            assert_eq!(anchors(conn), 2, "nothing new to record");

            let grown = Observed {
                identity: Ok(identity(&["r1", "r2"], &[])),
                site: None,
            };
            judge(conn, &repo, &grown, &prober).unwrap();
            let after = latest_identity(conn).unwrap().unwrap().binding.unwrap();
            assert_eq!(after, first, "lineage and site survive a refresh");
            assert_eq!(anchors(conn), 3);
            Ok(())
        })
        .unwrap();
    }

    /// Compatibility with a binary that predates the lineage: it reads `roots`,
    /// `remotes` and `from` out of the detail and nothing else, so the extra
    /// fields cannot change what it sees. `Identity::from_detail` and
    /// `aliases` are those readers, untouched by this change.
    #[test]
    fn a_detail_with_a_lineage_reads_back_through_the_readers_that_ignore_it() {
        let recorded = identity(&["r1"], &["h:a/b"]);
        let binding = Binding {
            uuid: "7f9c24e8-3b1a-4c5d-9e2f-0a1b2c3d4e5f".into(),
            site: Some((3, 4)),
        };
        let detail = recorded.to_detail(&["/old".to_string()], Some(&binding));
        let value: serde_json::Value = serde_json::from_str(&detail).unwrap();
        assert_eq!(Identity::from_detail(&value), Some(recorded));
        assert_eq!(Binding::from_detail(&value), Some(binding));
        let (_dir, repo) = home();
        super::super::with_conn(&repo, |conn| {
            record(conn, &repo, REANCHOR, detail.clone()).unwrap();
            assert_eq!(
                aliases(conn, std::slice::from_ref(&repo)).unwrap(),
                vec!["/old"]
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn a_lineage_that_is_not_a_plain_uuid_and_a_half_site_are_not_read() {
        let read = |detail: serde_json::Value| Binding::from_detail(&detail);
        assert_eq!(read(serde_json::json!({})), None);
        assert_eq!(read(serde_json::json!({"uuid": 7})), None);
        assert_eq!(read(serde_json::json!({"uuid": ""})), None);
        assert_eq!(read(serde_json::json!({"uuid": "x".repeat(65)})), None);
        assert_eq!(read(serde_json::json!({"uuid": "a\nb"})), None);
        let half = read(serde_json::json!({"uuid": "u", "site": {"dev": 1}})).unwrap();
        assert_eq!(half.site, None, "a half-read site is absent");
        let two = read(serde_json::json!({"uuid": "u", "site": {"dev": 1, "ino": 2}})).unwrap();
        assert_eq!(two.site, Some((1, 2)));
    }

    #[test]
    fn minted_lineages_are_distinct_version_4_uuids() {
        let ids: BTreeSet<String> = (0..64).map(|_| new_uuid()).collect();
        assert_eq!(ids.len(), 64);
        for id in ids {
            let parts: Vec<usize> = id.split('-').map(str::len).collect();
            assert_eq!(parts, [8, 4, 4, 4, 12], "{id}");
            assert!(
                id.chars().all(|c| c == '-' || c.is_ascii_hexdigit()),
                "{id}"
            );
            assert!(
                matches!(id.as_bytes()[19], b'8' | b'9' | b'a' | b'b'),
                "{id}"
            );
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
