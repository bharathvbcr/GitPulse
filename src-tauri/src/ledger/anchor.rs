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
//! is refused like a copy until a person adopts the ledger ([`adopt`]), and an
//! inode reused by a new checkout after the original was deleted is
//! indistinguishable from the original. Without inodes (non-unix) the roots
//! decide alone.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rusqlite::types::ValueRef;
use rusqlite::{params, Connection};

use super::redact;
use super::{
    canonical_repo, foreign_row_sql, home_spellings, insert, prepare, write_tx, ActorKind, Draft,
    LedgerError,
};
use crate::engine::ref_cache::{mono_is_manual, mono_now};

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
/// How many of the newest identity records are searched for the lineage and
/// site when the newest one has none (an older binary rewrote it).
const MAX_BINDING_LOOKBACK: usize = 32;
/// The budget of the last occupancy check, made inside the write transaction.
/// It is the longest the ledger's write lock is held for a probe, and a probe
/// that has not answered by then is a refusal. The window it cannot close: a
/// path that becomes a repository after this check and before the commit, a
/// few microseconds of SQLite work later, is taken over.
const RECHECK_BUDGET: Duration = Duration::from_millis(250);
/// The action of the audit row written when a person adopts a ledger.
const ADOPT: &str = "ledger.adopt";
/// What a site refusal tells the person to do. The words are the label of the
/// action in the Activity history panel (`ADOPT_LEDGER_LABEL`).
pub(crate) const ADOPT_HINT: &str = "If this checkout is the repository that was moved or copied here, use \"Adopt this ledger\" in Activity history to keep recording into it.";
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
            if let Some(site) = &binding.site {
                detail["site"] = site.to_json();
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

/// Whether `st_dev` names the same volume from one boot to the next. It does
/// not on macOS: the kernel numbers an APFS volume's device when it mounts it,
/// so a reboot can renumber it under an unchanged directory (reports:
/// awebai/oats#1261, garrytan/gbrain#5604, openclaw#158781; not reproduced
/// here). There the volume is named by its persistent UUID instead
/// (`getattrlist` `ATTR_VOL_UUID`, which `libc` exposes for apple targets), and
/// where that cannot be read the inode decides alone, with the device
/// advisory. Everywhere else the device number is stable and the rule keeps it.
const DEV_IS_STABLE: bool = cfg!(not(target_vendor = "apple"));

/// Where a checkout directory sits: the identity of the directory itself, not
/// of its path. A rename keeps it; a copy, a clone and a cross-device move do
/// not.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Site {
    dev: u64,
    ino: u64,
    /// The persistent volume UUID, where the platform has one (macOS).
    volume: Option<String>,
}

impl Site {
    #[cfg(test)]
    fn new(dev: u64, ino: u64) -> Site {
        Site {
            dev,
            ino,
            volume: None,
        }
    }

    /// Whether `other` is the very directory this site was taken in. The inode
    /// must match. Then the volume UUID decides when both sides have one;
    /// otherwise the device number decides only where `dev_stable` says it is
    /// stable across boots, and is advisory (a mismatch re-stamps the record,
    /// it is not a veto) where it is not.
    fn same_directory(&self, other: &Site, dev_stable: bool) -> bool {
        if self.ino != other.ino {
            return false;
        }
        match (&self.volume, &other.volume) {
            (Some(mine), Some(theirs)) => mine == theirs,
            _ => !dev_stable || self.dev == other.dev,
        }
    }

    fn to_json(&self) -> serde_json::Value {
        let mut site = serde_json::json!({ "dev": self.dev, "ino": self.ino });
        if let Some(volume) = &self.volume {
            site["vol"] = serde_json::json!(volume);
        }
        site
    }

    /// `None` when `site` is not two integers (and an optional short, printable
    /// volume name): never half-read.
    fn from_json(site: &serde_json::Value) -> Option<Site> {
        let volume = match site.get("vol") {
            None => None,
            Some(vol) => {
                let vol = vol.as_str()?;
                if vol.is_empty() || vol.len() > 64 || vol.chars().any(char::is_control) {
                    return None;
                }
                Some(vol.to_string())
            }
        };
        Some(Site {
            dev: site.get("dev")?.as_u64()?,
            ino: site.get("ino")?.as_u64()?,
            volume,
        })
    }
}

/// What an identity record says beyond roots and remotes: which ledger lineage
/// it is, and which directory it was written in.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Binding {
    uuid: String,
    site: Option<Site>,
}

/// Whether `text` is a canonical lowercase version-4 UUID: `8-4-4-4-12` hex
/// digits, version nibble 4, variant `8`, `9`, `a` or `b`.
fn is_lineage(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(at, byte)| match at {
            8 | 13 | 18 | 23 => *byte == b'-',
            14 => *byte == b'4',
            19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
            _ => matches!(byte, b'0'..=b'9' | b'a'..=b'f'),
        })
}

impl Binding {
    /// `None` for a legacy record (no `uuid`) and for one whose `uuid` is not a
    /// canonical version-4 UUID, as this module writes them: a ledger's content
    /// is not trusted to name its own lineage in any other shape. A malformed
    /// `site` is absent.
    fn from_detail(detail: &serde_json::Value) -> Option<Binding> {
        let uuid = detail.get("uuid")?.as_str()?;
        is_lineage(uuid).then(|| Binding {
            uuid: uuid.to_string(),
            site: detail.get("site").and_then(Site::from_json),
        })
    }
}

/// What the ledger last recorded about its repository.
struct Recorded {
    identity: Identity,
    /// The newest binding among the newest [`MAX_BINDING_LOOKBACK`] identity
    /// records, not only the newest record's own: an older binary that
    /// refreshes the identity writes it without one, and that must not read as
    /// a legacy ledger.
    binding: Option<Binding>,
    /// Whether the newest record itself carries that binding, so a stripped
    /// record is written over at home.
    newest_bound: bool,
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
    let id = super::file_id(path)?;
    Some(Site {
        dev: id.0,
        ino: id.1,
        volume: volume_uuid(path),
    })
}

/// The persistent UUID of the volume holding `path`, as `getattrlist(2)` names
/// it (`ATTR_VOL_UUID`: "the filesystem's persistent UUID"). `None` when the
/// call fails, and on every platform but macOS.
#[cfg(target_vendor = "apple")]
fn volume_uuid(path: &Path) -> Option<String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    /// What the call fills in for a volume attribute request: the length of the
    /// reply (itself included), then the requested attributes in order.
    #[repr(C, align(4))]
    struct Reply {
        length: u32,
        uuid: [u8; 16],
    }
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: an all-zero `attrlist` is the documented empty request; every
    // field set below is a plain integer.
    let mut request: libc::attrlist = unsafe { std::mem::zeroed() };
    request.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    request.volattr = libc::ATTR_VOL_INFO | libc::ATTR_VOL_UUID;
    let mut reply = Reply {
        length: 0,
        uuid: [0; 16],
    };
    // SAFETY: `path` is a NUL-terminated string that outlives the call;
    // `request` is a valid `attrlist`; `reply` is a writable buffer of exactly
    // the size passed, and the kernel writes no more than that.
    let status = unsafe {
        libc::getattrlist(
            path.as_ptr(),
            std::ptr::addr_of_mut!(request).cast(),
            std::ptr::addr_of_mut!(reply).cast(),
            std::mem::size_of::<Reply>(),
            0,
        )
    };
    let whole = usize::try_from(reply.length).is_ok_and(|n| n == std::mem::size_of::<Reply>());
    if status != 0 || !whole || reply.uuid == [0; 16] {
        return None;
    }
    Some(hyphenated(&reply.uuid))
}

#[cfg(not(target_vendor = "apple"))]
fn volume_uuid(_path: &Path) -> Option<String> {
    None
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
    hyphenated(&b)
}

/// 16 bytes as lowercase `8-4-4-4-12` hex.
fn hyphenated(bytes: &[u8; 16]) -> String {
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
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

/// The SQL that reads the newest identity records, newest first.
fn latest_identity_sql() -> String {
    format!(
        "SELECT detail_json FROM events WHERE session_id = ?1 AND action IN (?2, ?3)
         ORDER BY id DESC LIMIT {MAX_BINDING_LOOKBACK}"
    )
}

/// The newest identity record: what the ledger last knew this repository to
/// be, with the lineage and site of the newest of the newest
/// [`MAX_BINDING_LOOKBACK`] records that carries them. Past that many records
/// with none, the ledger reads as legacy (roots only), which is the one way a
/// run of that many stripped refreshes can reopen it.
fn latest_identity(conn: &Connection) -> Result<Option<Recorded>, LedgerError> {
    let failed = |e: rusqlite::Error| LedgerError::new("query_failed", e.to_string());
    let mut stmt = conn.prepare(&latest_identity_sql()).map_err(failed)?;
    let details = stmt
        .query_map(params![MARKER, ANCHOR, REANCHOR], |row| {
            row.get::<_, Option<String>>(0)
        })
        .map_err(failed)?;
    let mut identity = None;
    let mut binding = None;
    let mut newest_bound = false;
    for (seen, detail) in details.enumerate() {
        let value = detail
            .map_err(failed)?
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
        if seen == 0 {
            identity = value.as_ref().and_then(Identity::from_detail);
            if identity.is_none() {
                return Ok(None);
            }
        }
        if let Some(found) = value.as_ref().and_then(Binding::from_detail) {
            newest_bound = seen == 0;
            binding = Some(found);
            break;
        }
    }
    Ok(identity.map(|identity| Recorded {
        identity,
        binding,
        newest_bound,
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
        // Only an unanswered probe is shared. An answered one that its worker
        // has not yet released is stale by now: asking again is a new question.
        if let Some(slot) = slots.get(path) {
            if matches!(*locked(&slot.answer), Answer::Pending) {
                return Ok(Arc::clone(slot));
            }
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
            Answer::Pending => {
                Err("the earlier locations could not be checked in time".to_string())
            }
        }
    }

    /// The first of `paths` that is a repository other than `home`, or `Err`
    /// when the whole question could not be answered within the budget.
    fn first_occupied(&self, paths: &[String], home: &Path) -> Result<Option<String>, String> {
        self.first_occupied_within(paths, home, self.budget)
    }

    /// [`Self::first_occupied`] under an explicit budget.
    fn first_occupied_within(
        &self,
        paths: &[String],
        home: &Path,
        budget: Duration,
    ) -> Result<Option<String>, String> {
        let deadline = Instant::now() + budget;
        for path in paths {
            if self.ask(path, deadline)?.is_some_and(|real| real != home) {
                return Ok(Some(path.clone()));
            }
        }
        Ok(None)
    }

    /// The last occupancy check, made inside the write transaction on the same
    /// probe machinery (so a hung path still costs no extra thread), under the
    /// short [`RECHECK_BUDGET`]. An unanswered question is a refusal.
    fn recheck(&self, paths: &[String], home: &Path) -> Result<Option<String>, String> {
        self.first_occupied_within(paths, home, RECHECK_BUDGET.min(self.budget))
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

/// Forgets what this process settled for `repo_path`: a test meeting the first
/// open again, and an adoption that has just changed what the next settle finds.
fn forget_state(repo_path: &str) {
    states()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&super::ledger_path(repo_path));
}

#[cfg(test)]
pub(crate) fn forget(repo_path: &str) {
    forget_state(repo_path);
}

/// What [`settle`] did, or is waiting out, for one ledger in this process.
enum State {
    Settled,
    Waiting {
        until: Instant,
        /// Whether `until` was read off a test's manual clock. A deadline on one
        /// clock means nothing on the other, so an entry stamped on a clock that
        /// is no longer the current one is not waited out.
        manual: bool,
        refusal: Option<LedgerError>,
    },
}

impl State {
    fn waiting(refusal: Option<LedgerError>) -> State {
        State::Waiting {
            until: mono_now() + RETRY_AFTER,
            manual: mono_is_manual(),
            refusal,
        }
    }
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
            Some(State::Waiting {
                until,
                manual,
                refusal,
            }) if *manual == mono_is_manual() && mono_now() < *until => Some(Some(refusal.clone())),
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
    let seen = Observed::read(repo_path);
    let result = reconcile_passes(repo_path, &seen, prober, Mode::Settle);
    let next = match &result {
        Ok(_) if seen.identity.is_ok() => State::Settled,
        Ok(_) => State::waiting(None),
        Err(e) if e.code == "repository_mismatch" => State::waiting(Some(e.clone())),
        Err(e) => return Err(e.clone()),
    };
    states()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(db, next);
    let moved = match result? {
        Outcome::Moved(moved) => Some(moved.from),
        Outcome::Unchanged => None,
    };
    if let Some(from) = &moved {
        log::warn!(
            "ledger of {repo_path} re-anchored: its rows named {} earlier location(s) of this repository",
            from.len()
        );
    }
    Ok(moved)
}

/// Adopts the ledger of `repo_path`: takes over the rows of the earlier
/// locations although this checkout is not the directory the ledger was
/// anchored in (a cross-device move, a restore, a copy whose source is gone).
/// Everything else of the rule still holds: the same roots and remotes, no
/// earlier location still a repository, a plausible list of paths.
///
/// `confirm` is shown the earlier locations after a dry run found the rest of
/// the rule satisfied, outside any ledger lock, and must say yes; the adoption
/// then re-judges under the write transaction and writes, in that one
/// transaction, the re-anchor record (naming the new site) and an audit row
/// carrying both sites, both paths, the lineage, `actor` and the time. The
/// caller is responsible for the trust check and for `confirm` being a
/// decision a person made.
pub(super) fn adopt(
    repo_path: &str,
    actor: &str,
    confirm: &dyn Fn(&[String]) -> bool,
) -> Result<Vec<String>, LedgerError> {
    adopt_with(repo_path, actor, confirm, prober())
}

fn adopt_with(
    repo_path: &str,
    actor: &str,
    confirm: &dyn Fn(&[String]) -> bool,
    prober: &Prober,
) -> Result<Vec<String>, LedgerError> {
    let seen = Observed::read(repo_path);
    let nothing = || {
        LedgerError::new(
            "nothing_to_adopt",
            "this ledger holds no rows of another location of this repository, so there is nothing to adopt",
        )
    };
    let Outcome::Moved(preview) = reconcile_passes(repo_path, &seen, prober, Mode::Preview)? else {
        return Err(nothing());
    };
    if !confirm(&preview.from) {
        return Err(LedgerError::new(
            "adoption_declined",
            "the ledger was not adopted: the approval was not confirmed",
        ));
    }
    let adopted = reconcile_passes(repo_path, &seen, prober, Mode::Adopt(actor))?;
    forget_state(repo_path);
    match adopted {
        Outcome::Moved(moved) => {
            log::warn!(
                "ledger of {repo_path} adopted by {actor}: it now accepts {} earlier location(s) (lineage {})",
                moved.from.len(),
                moved.uuid
            );
            Ok(moved.from)
        }
        Outcome::Unchanged => Err(nothing()),
    }
}

/// What one judgement of the ledger is for.
#[derive(Clone, Copy)]
enum Mode<'a> {
    /// The automatic pass at the first transcript pass: the whole rule.
    Settle,
    /// An adoption's dry run: the rule without the site veto, nothing written.
    Preview,
    /// A person's adoption, by `actor`: the rule without the site veto, and the
    /// audit row.
    Adopt(&'a str),
}

impl Mode<'_> {
    fn waives_site(self) -> bool {
        !matches!(self, Mode::Settle)
    }
}

/// What a judgement found.
enum Outcome {
    /// Nothing to take over.
    Unchanged,
    /// Rows of earlier locations were (or, in a preview, would be) taken over.
    Moved(Moved),
}

struct Moved {
    from: Vec<String>,
    uuid: String,
}

/// Reads and probes the prior paths, then judges the ledger under one write
/// transaction. Git and the probe run before that transaction opens, never
/// inside it: a mount that never answers must not hold every other writer for
/// the probe's budget. The probe is made immediately before the transaction, and
/// the transaction checks again, briefly, before it commits.
fn reconcile_passes(
    repo_path: &str,
    seen: &Observed,
    prober: &Prober,
    mode: Mode<'_>,
) -> Result<Outcome, LedgerError> {
    let home = PathBuf::from(canonical_repo(repo_path));
    for _ in 0..SETTLE_PASSES {
        let foreign = super::with_conn(repo_path, |conn| {
            Ok(foreign_of(conn, repo_path).unwrap_or_default())
        })?;
        let probed = prober.probe(foreign, &home);
        let result = super::with_conn(repo_path, |conn| {
            reconcile(conn, repo_path, seen, &probed, prober, mode)
        });
        if !matches!(&result, Err(e) if e.code == STALE_PROBE) {
            return result;
        }
    }
    Err(mismatch(
        "the ledger kept gaining earlier locations while they were being checked",
    ))
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
fn reconcile(
    conn: &Connection,
    repo_path: &str,
    seen: &Observed,
    probed: &Probed,
    prober: &Prober,
    mode: Mode<'_>,
) -> Result<Outcome, LedgerError> {
    let failed = |e: rusqlite::Error| LedgerError::new("query_failed", e.to_string());
    let tx = write_tx(conn).map_err(failed)?;
    let foreign = foreign_of(&tx, repo_path)?;
    let latest = latest_identity(&tx)?;
    let canonical = canonical_repo(repo_path);
    let kept = latest.as_ref().and_then(|r| r.binding.as_ref());

    if foreign.is_empty() {
        if let (Mode::Settle, Ok(identity)) = (mode, &seen.identity) {
            // The lineage is carried; a legacy record is given one here, and a
            // site that could not be read this time keeps the one recorded. A
            // record an older binary rewrote without them is written again
            // with them.
            let binding = Binding {
                uuid: kept.map_or_else(new_uuid, |b| b.uuid.clone()),
                site: seen
                    .site
                    .clone()
                    .or_else(|| kept.and_then(|b| b.site.clone())),
            };
            let current = latest.as_ref().is_some_and(|r| {
                &r.identity == identity && r.newest_bound && r.binding.as_ref() == Some(&binding)
            });
            if !current {
                record(
                    &tx,
                    &canonical,
                    ANCHOR,
                    identity.to_detail(&[], Some(&binding)),
                )?;
            }
            tx.commit().map_err(failed)?;
        }
        return Ok(Outcome::Unchanged);
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
    let anchored = recorded.binding.as_ref().and_then(|b| b.site.as_ref());
    if let (false, Some(anchored)) = (mode.waives_site(), anchored) {
        match &seen.site {
            Some(here) if anchored.same_directory(here, DEV_IS_STABLE) => {}
            Some(_) => {
                return Err(mismatch(format!(
                    "this checkout is not the directory the ledger was anchored in: a copy or another clone, not a move. {ADOPT_HINT}"
                )))
            }
            None => {
                return Err(mismatch(format!(
                    "the ledger was anchored to a directory and this checkout's directory could not be read. {ADOPT_HINT}"
                )))
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
    let occupied = |live: String| {
        mismatch(format!(
            "{live} is still a Git repository: this is a copy or another clone, not a move"
        ))
    };
    match &probed.verdict {
        Err(unanswered) => return Err(mismatch(unanswered)),
        Ok(Some(live)) => return Err(occupied(live.clone())),
        Ok(None) => {}
    }
    // The answers above are from before the transaction. Ask once more, briefly,
    // so a path that became a repository since is not taken over; what remains
    // is the commit itself.
    match prober.recheck(&foreign, Path::new(&canonical)) {
        Err(unanswered) => return Err(mismatch(unanswered)),
        Ok(Some(live)) => return Err(occupied(live)),
        Ok(None) => {}
    }
    let binding = Binding {
        uuid: kept.map_or_else(new_uuid, |b| b.uuid.clone()),
        site: seen.site.clone(),
    };
    let moved = Moved {
        from: foreign.clone(),
        uuid: binding.uuid.clone(),
    };
    if matches!(mode, Mode::Preview) {
        return Ok(Outcome::Moved(moved));
    }
    let mut cursor = record(
        &tx,
        &canonical,
        REANCHOR,
        identity.to_detail(&foreign, Some(&binding)),
    )?;
    if let Mode::Adopt(actor) = mode {
        let detail = serde_json::json!({
            "from_paths": foreign,
            "to_path": canonical,
            "old_site": anchored.map(Site::to_json),
            "new_site": seen.site.as_ref().map(Site::to_json),
            "uuid": binding.uuid,
            "actor": actor,
            "at_ms": super::ids::now_millis(),
        });
        let audit = Draft {
            repo_path: canonical.clone(),
            actor_kind: Some(ActorKind::Human),
            actor_id: Some(actor.to_string()),
            action: ADOPT.to_string(),
            object: Some(canonical.clone()),
            detail_json: Some(detail.to_string()),
            ..Default::default()
        };
        cursor = insert(&tx, &prepare(&audit)?)?;
    }
    tx.commit().map_err(failed)?;
    super::announce(repo_path, cursor);
    Ok(Outcome::Moved(moved))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One automatic judgement of the ledger, as `settle` makes it.
    fn settle_in(
        conn: &Connection,
        repo_path: &str,
        seen: &Observed,
        probed: &Probed,
        prober: &Prober,
    ) -> Result<Option<Vec<String>>, LedgerError> {
        match reconcile(conn, repo_path, seen, probed, prober, Mode::Settle)? {
            Outcome::Moved(moved) => Ok(Some(moved.from)),
            Outcome::Unchanged => Ok(None),
        }
    }

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
            for sql in [&aliases_sql(), &latest_identity_sql()] {
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
        settle_in(conn, repo, seen, &probed, prober)
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
                // The first question is the one asked before the transaction. The
                // last one, asked inside it, is brief by design and is not this test.
                if locked(&lock_seen).is_some() {
                    return None;
                }
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
            settle_in(conn, &repo, &Observed::read(&repo), &probed, &real_prober())
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
            (Some(Site::new(1, 2)), Ok(())),
            (Some(Site::new(1, 3)), Err("another clone")),
            (
                Some(Site::new(9, 2)),
                if DEV_IS_STABLE {
                    Err("another clone")
                } else {
                    Ok(())
                },
            ),
            (None, Err("could not be read")),
        ] {
            let (_dir, repo) = home();
            super::super::with_conn(&repo, |conn| {
                record(conn, &repo, ANCHOR, bound(Some(Site::new(1, 2)))).unwrap();
                row_of(conn, "/gone");
                let out = judge(conn, &repo, &observed_at(here.clone()), &real_prober());
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
            record(conn, "/gone", ANCHOR, bound(Some(Site::new(1, 2)))).unwrap();
            row_of(conn, "/gone");
            judge(
                conn,
                &repo,
                &observed_at(Some(Site::new(1, 2))),
                &real_prober(),
            )
            .unwrap();
            let latest = latest_identity(conn).unwrap().unwrap();
            assert_eq!(
                latest.binding,
                Some(Binding {
                    uuid: "7f9c24e8-3b1a-4c5d-9e2f-0a1b2c3d4e5f".into(),
                    site: Some(Site::new(1, 2)),
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
            judge(conn, &repo, &observed_at(Some(Site::new(4, 5))), &prober).unwrap();
            let first = latest_identity(conn).unwrap().unwrap().binding.unwrap();
            assert_eq!(first.site, Some(Site::new(4, 5)));
            assert_eq!(first.uuid.len(), 36);
            assert_eq!(
                first.uuid.as_bytes()[14],
                b'4',
                "a version-4 uuid: {}",
                first.uuid
            );
            assert_eq!(anchors(conn), 2);

            judge(conn, &repo, &observed_at(Some(Site::new(4, 5))), &prober).unwrap();
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
            site: Some(Site::new(3, 4)),
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
    fn a_missing_or_malformed_lineage_and_a_half_site_are_not_read() {
        let read = |detail: serde_json::Value| Binding::from_detail(&detail);
        assert_eq!(read(serde_json::json!({})), None);
        assert_eq!(read(serde_json::json!({"uuid": 7})), None);
        assert_eq!(read(serde_json::json!({"uuid": ""})), None);
        assert_eq!(read(serde_json::json!({"uuid": "x".repeat(65)})), None);
        assert_eq!(read(serde_json::json!({"uuid": "a\nb"})), None);
        let uuid = "7f9c24e8-3b1a-4c5d-9e2f-0a1b2c3d4e5f";
        let half = read(serde_json::json!({"uuid": uuid, "site": {"dev": 1}})).unwrap();
        assert_eq!(half.site, None, "a half-read site is absent");
        let two = read(serde_json::json!({"uuid": uuid, "site": {"dev": 1, "ino": 2}})).unwrap();
        assert_eq!(two.site, Some(Site::new(1, 2)));
        let named =
            read(serde_json::json!({"uuid": uuid, "site": {"dev": 1, "ino": 2, "vol": "v"}}));
        assert_eq!(
            named.unwrap().site.unwrap().volume.as_deref(),
            Some("v"),
            "a volume name is kept"
        );
        let bad = read(serde_json::json!({"uuid": uuid, "site": {"dev": 1, "ino": 2, "vol": 7}}));
        assert_eq!(bad.unwrap().site, None, "a malformed volume voids the site");
    }

    /// Only canonical lowercase version-4 UUIDs are lineages, whatever else is
    /// printable and short.
    #[test]
    fn only_a_canonical_lowercase_version_4_uuid_is_a_lineage() {
        let good = "7f9c24e8-3b1a-4c5d-9e2f-0a1b2c3d4e5f";
        assert!(is_lineage(good));
        assert!(is_lineage(&new_uuid()));
        for bad in [
            "7F9C24E8-3B1A-4C5D-9E2F-0A1B2C3D4E5F",
            "7f9c24e8-3b1a-3c5d-9e2f-0a1b2c3d4e5f",
            "7f9c24e8-3b1a-4c5d-fe2f-0a1b2c3d4e5f",
            "7f9c24e83b1a4c5d9e2f0a1b2c3d4e5f",
            "7f9c24e8-3b1a-4c5d-9e2f-0a1b2c3d4e5",
            "7f9c24e8-3b1a-4c5d-9e2f-0a1b2c3d4e5fa",
            "7f9c24e8-3b1a-4c5d-9e2f-0a1b2c3d4e5g",
            "{7f9c24e8-3b1a-4c5d-9e2f-0a1b2c3d4e5f}",
            "",
        ] {
            assert!(!is_lineage(bad), "{bad:?}");
        }
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

    /// The newest binding is looked for past records an older binary wrote
    /// without one, a bounded number of them back; and a stripped newest record
    /// is written over at home.
    #[test]
    fn the_binding_is_found_past_stripped_records_up_to_the_lookback_bound() {
        for (stripped, found) in [
            (1, true),
            (MAX_BINDING_LOOKBACK - 1, true),
            (MAX_BINDING_LOOKBACK, false),
        ] {
            let (_dir, repo) = home();
            super::super::with_conn(&repo, |conn| {
                record(conn, &repo, ANCHOR, bound(Some(Site::new(1, 2)))).unwrap();
                for _ in 0..stripped {
                    old_binary_anchor(conn, &repo, &identity(&["r1"], &[]));
                }
                let latest = latest_identity(conn).unwrap().unwrap();
                assert_eq!(
                    latest.binding.is_some(),
                    found,
                    "{stripped} stripped records"
                );
                assert!(!latest.newest_bound, "the newest record carries nothing");
                Ok(())
            })
            .unwrap();
        }
    }

    #[test]
    fn a_stripped_newest_record_is_written_over_with_the_lineage_at_home() {
        let (_dir, repo) = home();
        super::super::with_conn(&repo, |conn| {
            record(conn, &repo, ANCHOR, bound(Some(Site::new(1, 2)))).unwrap();
            let before = latest_identity(conn).unwrap().unwrap().binding.unwrap();
            old_binary_anchor(conn, &repo, &identity(&["r1"], &[]));
            judge(
                conn,
                &repo,
                &observed_at(Some(Site::new(1, 2))),
                &real_prober(),
            )
            .unwrap();
            let after = latest_identity(conn).unwrap().unwrap();
            assert!(
                after.newest_bound,
                "the lineage is on the newest record again"
            );
            assert_eq!(after.binding.unwrap(), before);
            Ok(())
        })
        .unwrap();
    }

    /// Fable's required test: a legacy record (no lineage, no site) stamps its
    /// site on its first successful roots-only match, and is bound from then on.
    #[test]
    fn a_legacy_record_stamps_its_site_on_its_first_roots_only_match() {
        let (_dir, repo) = home();
        super::super::with_conn(&repo, |conn| {
            old_binary_anchor(conn, &repo, &identity(&["r1"], &[]));
            row_of(conn, "/gone");
            // Nothing is anchored, so the roots decide, and a directory of any site passes.
            judge(
                conn,
                &repo,
                &observed_at(Some(Site::new(4, 5))),
                &real_prober(),
            )
            .unwrap();
            let stamped = latest_identity(conn).unwrap().unwrap().binding.unwrap();
            assert_eq!(stamped.site, Some(Site::new(4, 5)));
            // From then on another directory with the same roots is refused.
            row_of(conn, "/gone-too");
            let other = judge(
                conn,
                &repo,
                &observed_at(Some(Site::new(4, 6))),
                &real_prober(),
            );
            assert!(other.unwrap_err().message.contains("another clone"));
            Ok(())
        })
        .unwrap();
    }

    /// The last check, inside the transaction, is short: a probe that has not
    /// answered by then is a refusal, not a long wait under the write lock.
    #[test]
    fn a_recheck_that_cannot_answer_in_time_is_a_refusal() {
        let (_dir, repo) = moved_legacy_ledger("/gone/slow");
        let (release, hold) = std::sync::mpsc::channel::<()>();
        let hold = Mutex::new(hold);
        let slow = Prober::new(
            Duration::from_secs(30),
            Arc::new(move |_: &str| {
                let _ = locked(&hold).recv_timeout(Duration::from_secs(30));
                None
            }),
        );
        let probed = Probed {
            checked: ["/gone/slow".to_string()].into(),
            verdict: Ok(None),
        };
        let started = Instant::now();
        let out = super::super::with_conn(&repo, |conn| {
            settle_in(conn, &repo, &Observed::read(&repo), &probed, &slow)
        });
        let e = out.unwrap_err();
        assert_eq!(e.code, "repository_mismatch");
        assert!(e.message.contains("in time"), "{e:?}");
        assert!(
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(5)),
            "the recheck waited {:?}",
            started.elapsed()
        );
        drop(release);
    }

    /// A repository, anchored in a directory it is no longer in (as after a
    /// cross-device move: same roots, another inode), with one row naming `prior`.
    fn anchored_elsewhere(prior: &str) -> (tempfile::TempDir, String) {
        let (dir, repo) = git_repo();
        let identity = Identity::read(&repo).unwrap();
        super::super::with_conn(&repo, |conn| {
            let elsewhere = bound(Some(Site::new(u64::MAX, u64::MAX)));
            let detail = identity.to_detail(&[], None);
            let mut value: serde_json::Value = serde_json::from_str(&detail).unwrap();
            let anchor: serde_json::Value = serde_json::from_str(&elsewhere).unwrap();
            value["uuid"] = anchor["uuid"].clone();
            value["site"] = anchor["site"].clone();
            record(conn, prior, ANCHOR, value.to_string()).unwrap();
            row_of(conn, prior);
            Ok(())
        })
        .unwrap();
        (dir, repo)
    }

    fn adopt_events(repo: &str) -> Vec<serde_json::Value> {
        let conn = Connection::open(super::super::ledger_path(repo)).unwrap();
        let mut stmt = conn
            .prepare("SELECT detail_json, actor_kind, actor_id FROM events WHERE action = ?1")
            .unwrap();
        stmt.query_map([ADOPT], |row| {
            let mut detail: serde_json::Value =
                serde_json::from_str(&row.get::<_, String>(0)?).unwrap();
            detail["row_actor_kind"] = row.get::<_, String>(1)?.into();
            detail["row_actor_id"] = row.get::<_, String>(2)?.into();
            Ok(detail)
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    /// Fable's required tests, at the ledger: a ledger anchored to another
    /// directory is refused (naming the adopt action), the person adopts it, the
    /// adoption is an audit row carrying both sites, both paths, the lineage, the
    /// actor and the time, and the ledger then records.
    #[test]
    fn an_adopted_ledger_heals_and_the_adoption_is_an_audit_row() {
        let (_dir, repo) = anchored_elsewhere("/gone/before");
        let prober = real_prober();
        let refused = settle_with(&repo, &prober).unwrap_err();
        assert_eq!(refused.code, "repository_mismatch");
        assert!(refused.message.contains("Adopt this ledger"), "{refused:?}");

        let asked = Mutex::new(Vec::new());
        let adopted = adopt_with(
            &repo,
            "user:test",
            &|earlier| {
                locked(&asked).extend(earlier.iter().cloned());
                true
            },
            &prober,
        )
        .unwrap();
        assert_eq!(adopted, vec!["/gone/before".to_string()]);
        assert_eq!(*locked(&asked), vec!["/gone/before".to_string()]);

        let events = adopt_events(&repo);
        assert_eq!(events.len(), 1, "{events:?}");
        let audit = &events[0];
        let here = site_of(Path::new(&canonical_repo(&repo))).unwrap();
        assert_eq!(audit["from_paths"], serde_json::json!(["/gone/before"]));
        assert_eq!(audit["to_path"], canonical_repo(&repo));
        assert_eq!(audit["old_site"], Site::new(u64::MAX, u64::MAX).to_json());
        assert_eq!(audit["new_site"], here.to_json());
        assert!(is_lineage(audit["uuid"].as_str().unwrap()));
        assert_eq!(audit["actor"], "user:test");
        assert_eq!(audit["row_actor_kind"], "human");
        assert_eq!(audit["row_actor_id"], "user:test");
        let at = audit["at_ms"].as_u64().unwrap();
        assert!(
            at.abs_diff(super::super::ids::now_millis()) < 60_000,
            "{at}"
        );

        // Re-stamped with the new site, healed at once, and no adoption again.
        let latest = latest_identity(&Connection::open(super::super::ledger_path(&repo)).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(latest.binding.unwrap().site, Some(here));
        assert_eq!(settle_with(&repo, &prober).unwrap(), None);
        let again = adopt_with(&repo, "user:test", &|_| true, &prober).unwrap_err();
        assert_eq!(again.code, "nothing_to_adopt");
        assert_eq!(adopt_events(&repo).len(), 1);
    }

    #[test]
    fn a_declined_adoption_changes_nothing() {
        let (_dir, repo) = anchored_elsewhere("/gone/before");
        let prober = real_prober();
        let declined = adopt_with(&repo, "user:test", &|_| false, &prober).unwrap_err();
        assert_eq!(declined.code, "adoption_declined");
        assert!(adopt_events(&repo).is_empty());
        let conn = Connection::open(super::super::ledger_path(&repo)).unwrap();
        let reanchors: i64 = conn
            .query_row(
                "SELECT count(*) FROM events WHERE action = ?1",
                [REANCHOR],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(reanchors, 0);
        assert!(settle_with(&repo, &prober).is_err(), "still refused");
    }

    /// Adopting waives only the site: a different history, and an earlier
    /// location that is still a repository, are still refused, and nothing is
    /// asked of the person for them.
    #[test]
    fn an_adoption_still_needs_the_roots_and_a_vacated_location() {
        let (_dir, repo) = anchored_elsewhere("/gone/live");
        let live = Prober::new(
            Duration::from_secs(5),
            Arc::new(|path: &str| (path == "/gone/live").then(|| PathBuf::from(path))),
        );
        let asked = std::cell::Cell::new(false);
        let out = adopt_with(
            &repo,
            "user:test",
            &|_| {
                asked.set(true);
                true
            },
            &live,
        );
        assert!(out.unwrap_err().message.contains("still a Git repository"));
        assert!(!asked.get());

        let (_other, foreign) = anchored_elsewhere("/gone/before");
        super::super::with_conn(&foreign, |conn| {
            // The ledger was anchored under a root this repository does not have.
            record(
                conn,
                "/gone/before",
                ANCHOR,
                identity(&["not-a-root-of-this-repository"], &[]).to_detail(
                    &[],
                    Some(&Binding {
                        uuid: new_uuid(),
                        site: None,
                    }),
                ),
            )
            .unwrap();
            Ok(())
        })
        .unwrap();
        let out = adopt_with(&foreign, "user:test", &|_| true, &real_prober());
        assert!(out.unwrap_err().message.contains("root commit"));
        assert!(adopt_events(&foreign).is_empty());
    }

    // ---- adversarial review of b17/723aab52-ledger-uuid ----

    /// A trusted one-commit repository at `path` (created), with its identity
    /// anchored and one event row under its own spelling, as a recording
    /// session leaves it. Returns the canonical spelling.
    fn real_repo_at(path: &Path) -> String {
        std::fs::create_dir_all(path).unwrap();
        let canonical = path.canonicalize().unwrap();
        crate::test_support::git_in(&canonical, &["init", "-b", "main"]);
        std::fs::write(canonical.join("seed.txt"), "seed").unwrap();
        crate::test_support::git_in(&canonical, &["add", "seed.txt"]);
        crate::test_support::git_in(&canonical, &["commit", "-m", "seed"]);
        crate::test_support::trust_repo(&canonical);
        let spelling = canonical.to_string_lossy().into_owned();
        settle(&spelling).unwrap();
        super::super::with_conn(&spelling, |conn| {
            row_of(conn, &spelling);
            Ok(())
        })
        .unwrap();
        spelling
    }

    /// The anchor-writing path of a binary that predates the lineage.
    fn old_binary_anchor(conn: &Connection, repo: &str, identity: &Identity) {
        record(conn, repo, ANCHOR, identity.to_detail(&[], None)).unwrap();
    }

    /// macOS assigns an APFS volume's `st_dev` at mount time, so a reboot can
    /// renumber it while the inode and the path stay the same (reports:
    /// awebai/oats#1261, garrytan/gbrain#5604, openclaw#158781). A checkout
    /// that is renamed while the app is closed and then opened after a reboot
    /// is the same directory, and must follow its ledger.
    #[test]
    fn adv_a_renumbered_device_does_not_make_the_same_directory_a_copy() {
        let (_dir, repo) = home();
        super::super::with_conn(&repo, |conn| {
            record(conn, &repo, ANCHOR, bound(Some(Site::new(16777231, 42)))).unwrap();
            row_of(conn, "/gone");
            let out = judge(
                conn,
                &repo,
                &observed_at(Some(Site::new(16777234, 42))),
                &real_prober(),
            );
            if DEV_IS_STABLE {
                // Where the device number is stable it is part of the identity.
                assert_eq!(out.unwrap_err().code, "repository_mismatch");
            } else {
                assert_eq!(out.unwrap(), Some(vec!["/gone".to_string()]));
                // The new device number is what the ledger now records.
                let now = latest_identity(conn).unwrap().unwrap().binding.unwrap();
                assert_eq!(now.site, Some(Site::new(16777234, 42)));
            }
            Ok(())
        })
        .unwrap();
    }

    /// The directory rule on both platform policies, so neither is only tested
    /// where it is the default: the inode always decides; a volume UUID decides
    /// next when both sides have one; with none, the device number decides only
    /// where it is stable.
    #[test]
    fn the_site_rule_is_inode_then_volume_then_a_device_only_where_it_is_stable() {
        let with_volume = |dev, ino, volume: &str| Site {
            dev,
            ino,
            volume: Some(volume.to_string()),
        };
        for dev_stable in [true, false] {
            assert!(Site::new(1, 2).same_directory(&Site::new(1, 2), dev_stable));
            assert!(!Site::new(1, 2).same_directory(&Site::new(1, 3), dev_stable));
            // A volume that is the same is the same whatever its device number.
            assert!(with_volume(1, 2, "v").same_directory(&with_volume(9, 2, "v"), dev_stable));
            // Another volume is another directory, with the inode the same.
            assert!(!with_volume(1, 2, "v").same_directory(&with_volume(1, 2, "w"), dev_stable));
        }
        assert!(!Site::new(1, 2).same_directory(&Site::new(9, 2), true));
        assert!(Site::new(1, 2).same_directory(&Site::new(9, 2), false));
        assert!(!with_volume(1, 2, "v").same_directory(&Site::new(9, 2), true));
        assert!(with_volume(1, 2, "v").same_directory(&Site::new(9, 2), false));
    }

    /// Where the platform names volumes, the site of a real directory carries
    /// the volume's UUID, the same one for two directories on one volume.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_real_directory_on_macos_names_its_volume() {
        let (_dir, repo) = home();
        let nested = Path::new(&repo).join("nested");
        std::fs::create_dir(&nested).unwrap();
        let here = site_of(Path::new(&repo)).unwrap();
        let there = site_of(&nested).unwrap();
        let volume = here.volume.clone().expect("a volume uuid");
        assert_eq!(volume.len(), 36, "{volume}");
        assert_eq!(there.volume, here.volume);
        assert_ne!(there.ino, here.ino);
        assert!(site_of(Path::new("/nonexistent/for/sure")).is_none());
    }

    /// An old binary that refreshes the identity record (a remote added, say)
    /// writes it without `uuid` and `site`. The next settle must not read that
    /// as a legacy ledger and mint a second lineage.
    #[test]
    fn adv_an_old_binary_refresh_does_not_lose_the_lineage() {
        let (_dir, repo) = home();
        super::super::with_conn(&repo, |conn| {
            record(conn, &repo, ANCHOR, bound(Some(Site::new(1, 2)))).unwrap();
            let before = latest_identity(conn).unwrap().unwrap().binding.unwrap();
            old_binary_anchor(conn, &repo, &identity(&["r1", "r2"], &[]));
            let grown = Observed {
                identity: Ok(identity(&["r1", "r2"], &[])),
                site: Some(Site::new(1, 2)),
            };
            judge(conn, &repo, &grown, &real_prober()).unwrap();
            let after = latest_identity(conn).unwrap().unwrap().binding.unwrap();
            assert_eq!(after.uuid, before.uuid, "the lineage was replaced");
            Ok(())
        })
        .unwrap();
    }

    /// The same strip, aimed at the site: after an old binary's refresh the
    /// newest record names no site, so a copy with the same roots takes the
    /// ledger on the roots-only rule this change was meant to retire.
    #[test]
    fn adv_an_old_binary_refresh_does_not_reopen_the_copy_hole() {
        let (_dir, repo) = home();
        super::super::with_conn(&repo, |conn| {
            record(conn, &repo, ANCHOR, bound(Some(Site::new(1, 2)))).unwrap();
            old_binary_anchor(conn, &repo, &identity(&["r1"], &["h:a/b"]));
            row_of(conn, "/gone");
            let copy = Observed {
                identity: Ok(identity(&["r1"], &["h:a/b"])),
                site: Some(Site::new(1, 99)),
            };
            let out = judge(conn, &repo, &copy, &real_prober());
            assert_eq!(
                out.as_ref().map_err(|e| e.code),
                Err("repository_mismatch"),
                "a copy in another directory took the ledger: {out:?}"
            );
            Ok(())
        })
        .unwrap();
    }

    /// `Binding::from_detail` accepts any short printable string as a lineage;
    /// the test above it claims that a lineage "that is not a plain uuid" is
    /// not read.
    #[test]
    fn adv_a_lineage_must_be_a_uuid() {
        for forged in ["../../etc/passwd", "not-a-uuid", "'; DROP TABLE events;--"] {
            let read = Binding::from_detail(&serde_json::json!({ "uuid": forged }));
            assert_eq!(read, None, "{forged:?} was read as a lineage");
        }
    }

    /// A prior path that becomes a repository after the probe answered "free"
    /// and before the transaction commits is taken over: the transaction only
    /// rechecks that the set of paths is the probed one, not that the answers
    /// still hold.
    #[test]
    fn adv_a_prior_path_occupied_after_the_probe_is_not_taken_over() {
        let (_da, prior) = home();
        let (_dir, repo) = moved_legacy_ledger(&prior);
        let home_path = PathBuf::from(canonical_repo(&repo));
        let probed = real_prober().probe(vec![prior.clone()], &home_path);
        assert_eq!(probed.verdict, Ok(None));
        crate::test_support::git_in(Path::new(&prior), &["init", "-b", "main"]);
        let out = super::super::with_conn(&repo, |conn| {
            settle_in(conn, &repo, &Observed::read(&repo), &probed, &real_prober())
        });
        assert_eq!(
            out.as_ref().map_err(|e| e.code),
            Err("repository_mismatch"),
            "{out:?}"
        );
    }

    /// `until` is measured on `mono_now`, a thread-local clock under test, but
    /// is stored in a process-global map. A refusal recorded under a
    /// `ManualClock` is an hour in the real future once the clock is dropped.
    #[test]
    fn adv_a_retry_deadline_stamped_on_the_manual_clock_does_not_outlive_it() {
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
        {
            let _clock = crate::engine::ref_cache::ManualClock::install();
            settle_with(&b, &real_prober()).unwrap_err();
        }
        // The refusal was cached on a clock that is gone. Once the cause is gone
        // too, the real clock must not still wait out the manual clock's hour.
        std::fs::remove_dir_all(&a).unwrap();
        assert_eq!(
            settle_with(&b, &real_prober()).unwrap(),
            Some(vec![a]),
            "a refusal stamped on the manual clock outlived it"
        );
    }

    /// And the other way: a real-clock refusal is not waited out by a manual
    /// clock that was installed afterwards.
    #[test]
    fn a_real_clock_refusal_is_not_served_to_a_manual_clock() {
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
        settle_with(&b, &real_prober()).unwrap_err();
        std::fs::remove_dir_all(&a).unwrap();
        let _clock = crate::engine::ref_cache::ManualClock::install();
        assert_eq!(settle_with(&b, &real_prober()).unwrap(), Some(vec![a]));
    }

    /// One ledger that names 64 hung paths must not be able to occupy the
    /// whole probe pool; it should cost one thread (held, kept as a check).
    #[test]
    fn adv_one_ledger_cannot_fill_the_probe_pool() {
        let (release, hold) = std::sync::mpsc::channel::<()>();
        let hold = Arc::new(Mutex::new(hold));
        let prober = Prober::new(
            Duration::from_millis(20),
            Arc::new(move |_: &str| {
                let _ = locked(&hold).recv_timeout(Duration::from_secs(30));
                None
            }),
        );
        let paths: Vec<String> = (0..MAX_PRIOR_PATHS).map(|i| format!("/hung/{i}")).collect();
        assert!(prober.first_occupied(&paths, Path::new("/home")).is_err());
        assert_eq!(locked(&prober.slots).len(), 1);
        drop(release);
    }

    /// A probe that panics is an unanswered question: a refusal, never "free".
    #[test]
    fn adv_a_panicking_probe_is_a_refusal() {
        let prober = Prober::new(
            Duration::from_secs(2),
            Arc::new(|_: &str| -> Option<PathBuf> { panic!("adv probe panic") }),
        );
        let out = prober.first_occupied(&["/x".to_string()], Path::new("/home"));
        assert!(out.is_err(), "{out:?}");
        let again = prober.probe(vec!["/x".to_string()], Path::new("/home"));
        assert!(again.verdict.is_err(), "{:?}", again.verdict);
    }

    /// Two connections migrating one legacy record at once mint one lineage.
    #[test]
    fn adv_concurrent_legacy_migrations_mint_one_lineage() {
        let (_dir, repo) = home();
        super::super::with_conn(&repo, |conn| {
            old_binary_anchor(conn, &repo, &identity(&["r1"], &[]));
            Ok(())
        })
        .unwrap();
        let db = super::super::ledger_path(&repo);
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    let conn = Connection::open(&db).unwrap();
                    conn.busy_timeout(Duration::from_secs(10)).unwrap();
                    barrier.wait();
                    judge(
                        &conn,
                        &repo,
                        &observed_at(Some(Site::new(4, 5))),
                        &real_prober(),
                    )
                    .unwrap();
                });
            }
        });
        let conn = Connection::open(&db).unwrap();
        let mut stmt = conn
            .prepare("SELECT detail_json FROM events WHERE action = ?1")
            .unwrap();
        let uuids: BTreeSet<String> = stmt
            .query_map([ANCHOR], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(|d| {
                let v: serde_json::Value = serde_json::from_str(&d.unwrap()).unwrap();
                Some(v.get("uuid")?.as_str()?.to_string())
            })
            .collect();
        assert_eq!(uuids.len(), 1, "{uuids:?}");
    }

    /// Settles a real repository after `mover` relocates its directory, and
    /// returns what `settle` made of the new place.
    fn settle_after(
        old: &Path,
        new: &Path,
        mover: impl FnOnce(),
    ) -> (String, Result<Option<Vec<String>>, LedgerError>) {
        let before = real_repo_at(old);
        mover();
        crate::test_support::trust_repo(&new.canonicalize().unwrap());
        (before, settle(&new.to_string_lossy()))
    }

    #[test]
    fn adv_case_only_rename_keeps_the_ledger() {
        let (parent, _) = home();
        let (old, new) = (
            parent.path().join("CaseRepo"),
            parent.path().join("caserepo"),
        );
        let (before, out) = settle_after(&old, &new, || std::fs::rename(&old, &new).unwrap());
        assert_eq!(out.unwrap(), Some(vec![before]));
    }

    #[cfg(unix)]
    #[test]
    fn adv_symlinked_path_to_a_moved_checkout_keeps_the_ledger() {
        let (parent, _) = home();
        let (old, new) = (parent.path().join("old"), parent.path().join("new"));
        let link = parent.path().join("link");
        let (before, out) = settle_after(&old, &new, || {
            std::fs::rename(&old, &new).unwrap();
            std::os::unix::fs::symlink(&new, &link).unwrap();
        });
        drop(out);
        let via_link = settle(&link.to_string_lossy());
        assert!(via_link.is_ok(), "{before}: {via_link:?}");
    }

    #[test]
    fn adv_parent_directory_rename_keeps_the_ledger() {
        let (parent, _) = home();
        let (old, new) = (
            parent.path().join("outer").join("repo"),
            parent.path().join("outer2").join("repo"),
        );
        let (before, out) = settle_after(&old, &new, || {
            std::fs::rename(parent.path().join("outer"), parent.path().join("outer2")).unwrap();
        });
        assert_eq!(out.unwrap(), Some(vec![before]));
    }

    #[test]
    fn adv_git_worktree_move_keeps_the_ledger() {
        let (parent, _) = home();
        let main = parent.path().join("main");
        let _ = real_repo_at(&main);
        let (old, new) = (parent.path().join("wt"), parent.path().join("wt-moved"));
        crate::test_support::git_in(
            &main.canonicalize().unwrap(),
            &["worktree", "add", "-q", &old.to_string_lossy(), "-b", "wt"],
        );
        let wt = old.canonicalize().unwrap().to_string_lossy().into_owned();
        crate::test_support::trust_repo(&old.canonicalize().unwrap());
        settle(&wt).unwrap();
        super::super::with_conn(&wt, |conn| {
            row_of(conn, &wt);
            Ok(())
        })
        .unwrap();
        crate::test_support::git_in(
            &main.canonicalize().unwrap(),
            &[
                "worktree",
                "move",
                &old.to_string_lossy(),
                &new.to_string_lossy(),
            ],
        );
        crate::test_support::trust_repo(&new.canonicalize().unwrap());
        let out = settle(&new.to_string_lossy());
        assert_eq!(out.unwrap(), Some(vec![wt]));
    }
}
