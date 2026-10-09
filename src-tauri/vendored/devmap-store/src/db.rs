use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::edge_index::GenerationEdges;
use devmap_analyze::model::*;
use devmap_analyze::DeadClusterScan;
use devmap_extract::model::*;
use devmap_extract::subprocess::GIT_HEAD_DEADLINE;
#[cfg(feature = "parse")]
use devmap_resolve::model::*;
use rusqlite::types::ToSql;
use rusqlite::{params_from_iter, Connection, Result};
use std::collections::{BTreeMap, BTreeSet};

mod generation;
mod maintenance;
mod migrate;
mod open;
mod pending;
mod query;
mod write;

/// A refusal this store raises itself — a future schema, a read-only file, a
/// NUL in a search query, a Python-era database handed to `--db` — carried in
/// `rusqlite::Error` so every `Result` in this module is one type.
///
/// `InvalidParameterName` carried these until 2026-09-07, and its `Display`
/// put "Invalid parameter name: " in front of every one of them — text about
/// a store, rendered as a complaint about a parameter. `ToSqlConversionFailure`
/// displays its boxed error bare (rusqlite 0.31 `error.rs`), so the reason is
/// the whole message.
#[derive(Debug)]
struct StoreRefusal(String);

impl std::fmt::Display for StoreRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StoreRefusal {}

/// Held while a connection opens and sets itself up: for a read-only
/// connection, its open and first read, the moment it maps the WAL index; for
/// a read-write one, the whole of `open_once`, through `enable_wal` and
/// `migrate`. Concurrent connections in one process setting the WAL index up
/// together livelocked on Windows: every one of them got SQLITE_PROTOCOL for
/// as long as they kept retrying (`concurrent_readers_cannot_enqueue_writer_work`
/// for readers, `concurrent_v19_openers_keep_pending_work_and_one_durable_identity`
/// for sixteen writers, which exhausted the 20 s retry deadline). Serialised,
/// the first sets it up and the rest find it ready. Other processes still
/// coordinate through SQLite's own locks, and everything after the open runs
/// concurrently.
static CONNECTION_SETUP: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn refusal(message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(StoreRefusal(message.into())))
}

/// What a reader is told about a damaged full-text index, and what to do.
///
/// Two causes produce the same symptom, and the remedy differs, so both are
/// named. Damage in `nodes_fts` is what `devmap repair --fts` rebuilds. But a
/// long-lived reader whose SQLite locks were stripped (fixed in 7cdd0249, still
/// true of any process started before it) sees a store that looks corrupt while
/// a fresh process reads the same file cleanly — and no repair reaches that
/// process's view.
fn fts_damage_reason(what: &str) -> String {
    format!(
        "{what}; rebuild the full-text index with `devmap repair --fts`. If a fresh \
         `devmap status` on this store reports it healthy, this process's view of the \
         store is stale rather than the index damaged: restart it. If the damage \
         survives the repair, the database itself is damaged: rebuild it with \
         `devmap build --full`"
    )
}

/// Name a corrupt read of the full-text index as that, not as a damaged database.
///
/// Applied only to statements that read `nodes_fts`, so a genuinely corrupt
/// symbol table is never sent to a repair that cannot touch it. Every other
/// error passes through unchanged.
fn fts_failure(error: rusqlite::Error) -> rusqlite::Error {
    if error.sqlite_error_code() != Some(rusqlite::ErrorCode::DatabaseCorrupt) {
        return error;
    }
    refusal(fts_damage_reason(&format!(
        "the full-text index (`nodes_fts`) could not be read: {error}"
    )))
}

/// Typed refusal when a store's stamped schema is not this binary's.
///
/// Carried inside `rusqlite::Error::ToSqlConversionFailure` so existing
/// `Result` signatures stay one type, but callers can downcast instead of
/// matching substrings of the Display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedSchema {
    pub found: i32,
    pub expected: i32,
    message: String,
}

impl UnsupportedSchema {
    fn new(store: &str, found: i32) -> Self {
        let expected = CURRENT_SCHEMA_VERSION;
        let remedy = if found > expected {
            "this devmap binary is older than the store; rebuild it with \
             `cargo build --release -p devmap-cli` or set DEVMAP_BINARY to a newer build"
                .to_string()
        } else if (1..=PYTHON_INDEX_SCHEMA_VERSION).contains(&found) {
            format!(
                "this is the Python engine's database (`.devcouncil/codeintel/index.sqlite`, \
                 schema {PYTHON_INDEX_SCHEMA_VERSION}), not a devmap store, and this kernel \
                 cannot convert it — point `--db` at `devmap.sqlite`"
            )
        } else {
            "run `devmap build` to migrate the store".to_string()
        };
        Self {
            found,
            expected,
            message: format!(
                "devmap store {store}: schema version {found} is not supported by this binary \
                 (schema {expected}); {remedy}"
            ),
        }
    }
}

impl std::fmt::Display for UnsupportedSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for UnsupportedSchema {}

fn unsupported_schema_error(store: &str, found: i32) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(UnsupportedSchema::new(store, found)))
}

/// Shared by [`Store::save_generation_timed`] and [`Store::restamp_latest_head`].
/// A stamp is provenance, not a git-object proof: `"unavailable"` and `"unknown"`
/// are valid, which is what the CLI writes when `rev-parse` fails.
fn validate_head_sha(head_sha: &str) -> Result<()> {
    if head_sha.is_empty() || head_sha.len() > 128 || head_sha.chars().any(char::is_whitespace) {
        return Err(refusal(
            "head_sha must be non-empty, whitespace-free, and at most 128 characters",
        ));
    }
    Ok(())
}

use crate::coverage::{CoverageGaps, DiscoveryRefusal};
use crate::schema::{CURRENT_SCHEMA_VERSION, PYTHON_INDEX_SCHEMA_VERSION};

/// Failed drain attempts after which a pending path stops being retried.
///
/// Public because the queue's hygiene is now a cross-crate contract: the daemon
/// bumps it, `devmap build` and `devmap repair --pending` drop rows that reach
/// it, and `status` names them. A test that asserts quarantine behaviour has to
/// be able to say what quarantined means without copying the number.
pub const MAX_PENDING_ATTEMPTS: u32 = 5;

/// `git rev-parse HEAD` through the kernel's one bounded runner.
///
/// This was the first bounded git call in the kernel — drain threads, kill at
/// [`GIT_HEAD_DEADLINE`] — written here because a hung git (network mount,
/// wedged hook) stalled every drain batch and CLI status behind it. Two more
/// runners grew beside it in `devmap-query`, one of them unbounded, and three
/// runners is how three disciplines drift; `devmap_extract::subprocess` is
/// the one now and this is a caller of it. What stays here is the contract:
/// `current_git_head`'s callers treat an unavailable head as "unavailable",
/// so a stalled git degrades honestly instead of wedging the daemon, and a
/// head that is not a hex identity is refused rather than stored.
fn run_git_head_with_deadline(program: &str, root: &Path) -> anyhow::Result<String> {
    use devmap_extract::subprocess::{git_with_program, run_bounded, Bounds, Failure};

    let mut command = git_with_program(std::ffi::OsStr::new(program), root);
    command.args(["rev-parse", "HEAD"]);
    let bounds = Bounds {
        deadline: GIT_HEAD_DEADLINE,
        stdout_cap: 4096,
        stderr_cap: 4096,
    };
    let captured = run_bounded(&mut command, bounds).map_err(|failure| match failure {
        Failure::Deadline { .. } => anyhow::anyhow!(
            "{program} rev-parse HEAD exceeded {GIT_HEAD_DEADLINE:?} and was killed"
        ),
        other => anyhow::anyhow!("{other}"),
    })?;
    if !captured.status.success() {
        anyhow::bail!(
            "git rev-parse HEAD failed for {:?}: {}",
            root,
            captured.stderr_trimmed()
        );
    }
    let head = captured.stdout_lossy().trim().to_string();
    if !(7..=64).contains(&head.len()) || !head.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("git returned an invalid HEAD identity for {:?}", root);
    }
    Ok(head)
}

pub fn current_git_head(root: &Path) -> anyhow::Result<String> {
    run_git_head_with_deadline("git", root)
}

/// Whether a pending-queue entry is a control token rather than a path.
///
/// The daemon's git-HEAD sentinel is `"\0devmap:git-head-changed"`. A leading
/// NUL cannot begin a real filesystem path, which makes the namespace safe, and
/// it lets the store recognise the token without depending on `devmap-serve` —
/// which depends on *this* crate, so the reverse edge cannot exist. Control
/// tokens are never path-normalised, never structurally reconciled, and are
/// retired by a full build like any other superseded work.
fn is_control_token(entry: &str) -> bool {
    entry.starts_with('\0')
}

/// What [`canonical_pending_entry`] could establish about a raw queue entry.
///
/// Four states, not two. "Outside the repository" is a positive claim that
/// costs the row its place in the queue, and it must not be the answer given
/// when the containment test itself could not run.
///
/// [`ControlToken`] is separate from [`Canonical`] so that `Canonical` means
/// "a repo-relative *path*" and nothing else. It used to be folded into
/// `Canonical`, which read as a harmless spelling detail right up until a
/// caller did the obvious thing and applied a path test to the result: the K7
/// build-cache check in [`Store::enqueue_pending_paths_under_root`] asked the
/// OS about `root.join("\0devmap:git-head-changed")`, a path string cannot
/// carry an interior NUL, and the resulting `Unreadable` verdict refused the
/// daemon's git-HEAD sentinel at the door. A branch is now required of every
/// caller, so the next path-shaped check cannot swallow a control token in
/// silence.
///
/// [`ControlToken`]: PendingEntry::ControlToken
/// [`Canonical`]: PendingEntry::Canonical
enum PendingEntry {
    /// The canonical repo-relative spelling of a path.
    Canonical(String),
    /// A control token, verbatim: see [`is_control_token`]. Not a path, so no
    /// path test may be applied to it.
    ControlToken(String),
    /// Structurally outside the repository: no retry can change this.
    Outside,
    /// Containment is unknown because `root.canonicalize()` failed — a symlink
    /// loop, a parent that lost `+x`, a stale handle on a network mount. The
    /// caller must not treat this as either answer.
    Undecidable(std::io::Error),
}

/// The canonical pending-queue spelling of `raw` relative to `root`.
///
/// Canonical means: repo-relative, forward slashes, no `.` or `..` components,
/// and `"."` for the root itself. Both queue producers now go through this, so
/// the watcher's absolute paths and the reconcile sweep's relative ones become
/// the same row instead of two rows for one file — see
/// [`Store::enqueue_pending_paths_under_root`].
fn canonical_pending_entry(root: &Path, raw: &str) -> PendingEntry {
    if is_control_token(raw) {
        return PendingEntry::ControlToken(raw.to_string());
    }
    // Let Path parse platform separators. Replacing backslashes corrupts both
    // legal Unix filenames and Windows canonical \\?\ prefixes.
    let candidate = Path::new(raw);

    let relative = if candidate.is_absolute() {
        // Compare against the canonical root as well: a symlinked temp
        // directory, or a `.`-rooted daemon, makes the lexical prefix test
        // fail on paths that are genuinely inside the tree.
        match candidate.strip_prefix(root) {
            Ok(stripped) => stripped.to_path_buf(),
            Err(_) => match root.canonicalize() {
                Ok(canonical) => match candidate.strip_prefix(&canonical) {
                    Ok(stripped) => stripped.to_path_buf(),
                    Err(_) => return PendingEntry::Outside,
                },
                // The rescue itself failed, so nothing here has established
                // where the entry lives. `.ok()` used to collapse this into
                // `None`, which the reconcile sweep deletes as a row that
                // escapes the root: a definite verdict from a check that never
                // ran, and the row is gone.
                Err(error) => return PendingEntry::Undecidable(error),
            },
        }
    } else {
        candidate.to_path_buf()
    };

    let mut parts: Vec<String> = Vec::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(part) => match part.to_str() {
                Some(text) => parts.push(text.to_string()),
                None => return PendingEntry::Outside,
            },
            std::path::Component::CurDir => {}
            // `..` can only ever climb out of the root from a relative entry,
            // and an absolute entry that needed it was already refused above.
            std::path::Component::ParentDir => return PendingEntry::Outside,
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return PendingEntry::Outside
            }
        }
    }
    PendingEntry::Canonical(if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    })
}

/// Whether a canonical pending entry can ever be processed, and why not.
///
/// `Err(reason)` means no number of retries will help — see
/// [`Store::reconcile_pending_paths`] for what that cost in practice.
fn classify_pending_entry(
    root: &Path,
    canonical: &str,
    indexed: &BTreeSet<String>,
    caches: &mut devmap_extract::CacheDirectoryCache,
) -> std::result::Result<(), String> {
    if canonical == "." {
        // The root itself: a whole-tree rescan the drain expands.
        return Ok(());
    }
    // K7: a path inside a tagged build cache is not source and never will be.
    // Discovery no longer walks these directories, so a queued row naming one
    // can only ever fail — and 47,000 of them were queued from two cargo output
    // trees on this repository before discovery learned to skip them.
    match caches.tagged_ancestor(root, canonical) {
        devmap_extract::CacheVerdict::Inside(cache) => {
            return Err(format!(
                "inside {cache}, a build cache marked with CACHEDIR.TAG"
            ));
        }
        // `canonical_pending_entry` is supposed to have made this repo-relative
        // already, so reaching here means the row was written by something that
        // bypassed it. Unprocessable either way — and now it says which rule the
        // path broke instead of being waved through as "not a build cache".
        devmap_extract::CacheVerdict::NotRepoRelative(why) => {
            return Err(format!("{why}, so it names nothing inside the repository"));
        }
        devmap_extract::CacheVerdict::Unreadable { .. } => {
            // This classifier only deletes definitively unprocessable work.
            // Keep an unexamined path pending, just as for an undecidable
            // source stat below. Admission and extraction still refuse the
            // unsafe marker; retaining the row cannot authorize a read.
            return Ok(());
        }
        devmap_extract::CacheVerdict::Outside => {}
    }
    let absolute = root.join(canonical);
    // What this path is, asked at the one owner the *cold walk* asks
    // (`devmap_extract::candidate_kind`). This used to be a second, stricter
    // rule spelled out locally: `symlink_metadata` plus "anything that is
    // neither a plain file nor a plain directory is garbage", which refused
    // every symlink — including the in-repository ones the cold walk indexes.
    // A cold build then indexed `src/util.py -> shared/util.py` and the next
    // drain of that path deleted the queued edit as unprocessable, leaving the
    // stored extraction stale while `status` reported fresh.
    match devmap_extract::candidate_kind(root, &absolute) {
        // The whole-subtree rescan the drain expands.
        devmap_extract::CandidateKind::Directory => Ok(()),
        devmap_extract::CandidateKind::File { bytes } => {
            if bytes > devmap_extract::MAX_SOURCE_BYTES {
                return Err(devmap_extract::DiscoverySkipReason::Oversized {
                    bytes,
                    limit: devmap_extract::MAX_SOURCE_BYTES,
                }
                .to_string());
            }
            if !devmap_extract::is_indexable_source(canonical) {
                return Err(devmap_extract::DiscoverySkipReason::NonSource.to_string());
            }
            Ok(())
        }
        // The walk never descends a link, so the files under the target are
        // indexed under their real names and only under those. Expanding this
        // row would put the same bytes in the graph twice, under a second path
        // no cold build ever produces.
        devmap_extract::CandidateKind::LinkedDirectory => Err(
            "a symlink to a directory, which discovery never descends; the files under it \
             are queued under their own names"
                .to_string(),
        ),
        // A socket, fifo or device. Never a source this build reads.
        devmap_extract::CandidateKind::Other => Err("not a regular file or directory".to_string()),
        // Refused by discovery — a link out of the repository, or one whose
        // target will not resolve to say either way. A `devmap build` writes no
        // rows for it, so there is work here only while the graph still claims
        // one: the drain has to remove it, exactly as a full build's output
        // would. With nothing claimed, the row is garbage and is dropped with
        // discovery's own wording rather than a file-type complaint that sends
        // the reader looking for a socket.
        devmap_extract::CandidateKind::Refused(reason) => {
            if still_claimed(canonical, indexed) {
                Ok(())
            } else {
                Err(reason.to_string())
            }
        }
        devmap_extract::CandidateKind::Absent => {
            // Absent. This is a deletion the drain must process only if the
            // graph still claims the path — or claims something beneath it,
            // which is how a removed directory reaches its indexed children.
            if still_claimed(canonical, indexed) {
                Ok(())
            } else {
                Err("no longer exists under the root and is not in the stored graph".to_string())
            }
        }
        // The stat could not run: ELOOP from a symlink loop in a parent,
        // EACCES from a parent that lost `+x`, EIO or ESTALE from a network
        // mount. None of those is evidence that the file is gone, and this
        // branch's verdict *deletes the row*. Every error used to land here
        // and be read as absence, so a stat that could not run silently
        // discarded queued work while `status` went on reporting fresh.
        //
        // Keep it. A transient failure is retried, and a path that keeps
        // failing is quarantined after `MAX_PENDING_ATTEMPTS`, which is a
        // visible state an operator can act on.
        devmap_extract::CandidateKind::Undecidable(_) => Ok(()),
    }
}

/// Does the stored graph still assert this path, or anything beneath it?
///
/// A removed directory reaches its indexed children through the prefix scan;
/// without it the drain would drop the row and leave the graph describing files
/// that are gone.
fn still_claimed(canonical: &str, indexed: &BTreeSet<String>) -> bool {
    if indexed.contains(canonical) {
        return true;
    }
    let prefix = format!("{canonical}/");
    indexed
        .range(prefix.clone()..)
        .next()
        .is_some_and(|entry| entry.starts_with(&prefix))
}

/// What [`Store::enqueue_pending_paths_under_root`] accepted and refused.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PendingEnqueueReport {
    /// Canonical entries actually queued, deduplicated and sorted.
    pub enqueued: Vec<String>,
    /// `(raw entry, why)` for entries refused as outside the repository. A
    /// refusal is reported rather than dropped: a watcher emitting paths from
    /// outside the tree is a bug in the watcher, and silently swallowing them
    /// is how it stays one.
    pub refused: Vec<(String, String)>,
}

/// What [`Store::convert_page_size`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageSizeConversion {
    pub before: i64,
    pub after: i64,
    /// False when the store was already at the target — reported rather than
    /// inferred from `before == after`, so "already correct" and "rewritten to
    /// the same value" stay distinguishable.
    pub converted: bool,
}

/// What [`Store::reconcile_pending_paths`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PendingReconcile {
    /// `(path, why)` for rows deleted as structurally unprocessable.
    pub dropped: Vec<(String, String)>,
    /// `(old spelling, canonical spelling)` for rows normalised in place.
    pub rewritten: Vec<(String, String)>,
    /// Rows left queued because they still name real work.
    pub retained: usize,
}

/// What a committed build proved about the pending queue.
///
/// The distinction is the fix for K1(e2): "which paths did this build write"
/// and "what did this build read" are different questions, and only the second
/// can retire a row that names a directory.
#[derive(Debug, Clone, Copy)]
pub enum PendingSupersede<'a> {
    /// A build with no `--affected` narrowing: it walked the whole tree, so it
    /// answered every request through the durable watermark captured before
    /// reading source. Wall-clock adjustments cannot move this boundary.
    WholeTreeThrough(&'a PendingWatermark),
    /// A narrowed build: it read only the paths it was handed, so only those
    /// rows are answered.
    IndexedPathsThrough(&'a [String], &'a PendingWatermark),
}

/// A position in one store's durable event stream, never a wall-clock time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingWatermark {
    epoch: String,
    revision: i64,
}

/// One pending row claimed for a drain attempt.
///
/// `queued_at` is diagnostic only. The store epoch and monotonic revision
/// distinguish re-enqueues even on equal or backwards wall-clock ticks.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingClaim {
    pub path: String,
    pub queued_at: f64,
    watermark: PendingWatermark,
}

#[cfg(test)]
#[path = "db/tests/agentic_queue_regressions.rs"]
mod agentic_queue_regressions;

/// Fail closed: poisoned mutex is an error, never a panic.
fn lock_conn(
    mutex: &Mutex<Connection>,
) -> std::result::Result<MutexGuard<'_, Connection>, rusqlite::Error> {
    mutex
        .lock()
        .map_err(|_: PoisonError<MutexGuard<'_, Connection>>| {
            refusal("store mutex poisoned — refusing to continue (fail-closed)")
        })
}

pub struct Store {
    conn: Mutex<Connection>,
    /// The file this store was opened from, when it has one.
    ///
    /// Remembered solely so [`Store::lock_writer`] can find the sibling
    /// `.writer.lock`. `None` for an in-memory store, which no other process
    /// can reach and therefore has nothing to serialise against.
    db_path: Option<std::path::PathBuf>,
    /// Whether this process can write the store at all.
    ///
    /// A store on a read-only mount, in a CI cache restored without write
    /// bits, or `chmod 444`'d by an operator is an ordinary store that can be
    /// *read*. SQLite opens such a file read-only without complaint and only
    /// fails at the first write — which, before this flag existed, was a
    /// header rewrite in [`Store::configure_connection`], so every query
    /// refused with "attempt to write a readonly database" and a readable map
    /// looked like no map at all. Recorded once at open so a write can be
    /// refused by name ([`Store::refuse_if_read_only`]) instead of by SQLite
    /// error code.
    read_only: bool,
    /// The latest generation's full edge set, kept for the life of that
    /// generation.
    ///
    /// `latest_edges` re-ran a two-JOIN, fully-ordered scan of every edge on
    /// **every** request. Measured on this repository (71,598 edges, warm
    /// daemon): `impact` and `trace` cost 99.6-160 ms *regardless of `--depth`*
    /// — depth 1, 3 and 8 all landed within noise of each other — because the
    /// cost is the load, not the traversal.
    ///
    /// Caching it also *reduces* memory rather than adding to it, which is the
    /// opposite of what it looks like. The uncached daemon allocated a fresh
    /// 71,598-edge vector per query and did not give the memory back: RSS went
    /// 531.6 MB after startup -> 625.2 MB after 6 queries -> 801.4 MB after 26,
    /// about 10 MB per query of allocator churn. One retained copy replaces an
    /// The latest generation's edges and the adjacency over them, keyed by
    /// generation id.
    ///
    /// The one memo of a generation's edges. It used to sit beside a second
    /// one holding the `Vec<StoredEdge>` it was built from, so the rows were
    /// retained for the life of the process on top of the index — ~100 MB of
    /// `String`s that only `latest_edges` ever read again. The index now holds
    /// the generation's *interned* text and addresses it by rank, so the rows
    /// are built on demand and only for the edges an answer contains, and one
    /// memo is enough.
    ///
    /// Keyed by generation id, so a build that commits a new generation
    /// invalidates it by construction — there is no separate invalidation path
    /// to forget to call. Only the newest generation is held, so the memory is
    /// bounded by one edge set and not by the number of generations retained.
    /// The index is unfiltered; `min_confidence` is applied per request against
    /// the same rounding rule the SQL used, so the answer is unchanged.
    edge_index: Mutex<Option<(u32, std::sync::Arc<GenerationEdges>)>>,
    /// `(generation, node_count, edge_count)` for the generation last asked
    /// about.
    ///
    /// `status` is the cheapest question the kernel answers and it scaled with
    /// the corpus — two `COUNT(*)`s over the generation's whole node and edge
    /// tables, per call, for numbers that cannot change while the generation
    /// stands. Rows are only inserted under a *new* generation id and only
    /// deleted a whole generation at a time, so the id is a complete key: a
    /// memo under it cannot go stale, it can only be replaced by a newer
    /// generation's. Only the newest asked-about generation is held, so this is
    /// three words of memory rather than a map that grows with history.
    generation_counts: Mutex<Option<(u32, usize, usize)>>,
    /// `(generation, symbols reachable through the full-text index)` for the
    /// generation last checked by `status`.
    ///
    /// Memoized for the reason `generation_counts` is — the count is a join
    /// over every symbol of the generation, 6.5 ms warm and 74 ms cold on this
    /// repository's 15,034, against a `status` that otherwise costs ~3 ms. Unlike
    /// those counts it is not immutable: index damage can arrive mid-generation.
    /// The per-call readability probe in `fts_health_locked` still runs, so
    /// what the memo can hide is a *partial* loss arriving after the first
    /// check, until the next generation or the next process.
    fts_reachable: Mutex<Option<(u32, usize)>>,
    /// The analysis status of the generation last asked about.
    ///
    /// Immutable for the same reason the counts are — a generation's
    /// `analysis_json` is written once, under a new id — so the id is a
    /// complete key. Reading it at all means going to the summary blob, which
    /// on the ScholarLM corpus is milliseconds; `devmap status` asks for it on
    /// every call and nothing else about it can change.
    generation_analysis_status: Mutex<Option<(u32, AnalysisStatus)>>,
    /// Last whole-tree source-freshness verdict from [`Store::status`].
    ///
    /// Query envelopes read this rather than re-walking the tree: status is the
    /// surface that verifies; queries disclose the last known verdict for the
    /// generation they answered from, or an explicit unverified reason when
    /// nothing has been checked in this process.
    source_freshness_cache: Mutex<Option<CachedSourceFreshness>>,
}

/// Process-local memo of the last [`Store::status`] source check.
#[derive(Debug, Clone)]
struct CachedSourceFreshness {
    generation_id: u32,
    fresh: Option<bool>,
    reason: Option<String>,
}

/// A held cross-process writer lock on one store (K13).
///
/// Released when dropped — and, because it is an `flock`, also when the holding
/// process dies. That is the whole reason for using one rather than a marker
/// file: a build killed with SIGKILL leaves nothing behind to clean up, whereas
/// a stale marker would wedge every later build until someone deleted it by
/// hand.
///
/// An in-memory store holds `file: None`. That is not a check being skipped: an
/// in-memory database is private to one process and one `Store`, whose own
/// mutex already serialises writers, so there is no second writer for a
/// cross-process lock to exclude.
#[derive(Debug)]
pub struct WriterLock {
    file: Option<devmap_extract::safe_fs::SafeFile>,
    path: Option<std::path::PathBuf>,
}

impl WriterLock {
    /// The lock file backing this guard, or `None` for an in-memory store.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Whether a real cross-process lock is held, as opposed to the in-memory
    /// no-op. Callers that need to *assert* exclusivity ask this rather than
    /// inferring it from the guard's existence.
    pub fn is_held(&self) -> bool {
        self.file.is_some()
    }
}

impl Drop for WriterLock {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            // Explicit rather than relying on close-releases-flock, so the
            // release is a statement in the code and not a side effect of drop
            // order. A failure here is not actionable — the descriptor closes
            // on the next line either way, which releases the lock.
            let _ = file.unlock();
        }
    }
}

/// What a query envelope discloses about whole-tree source freshness.
///
/// Owned here so the store can answer without depending on the query crate;
/// [`devmap_query::SourceFreshness`] is the wire twin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuerySourceFreshness {
    pub fresh: Option<bool>,
    pub generation_id: Option<u32>,
    pub reason: Option<String>,
}

impl QuerySourceFreshness {
    pub fn unverified(reason: impl Into<String>) -> Self {
        Self {
            fresh: None,
            generation_id: None,
            reason: Some(reason.into()),
        }
    }
}

/// Capped inventory of how the working tree differs from the indexed generation.
///
/// Present on status when source freshness is false because the tree differs.
/// Counts are complete; `sample_paths` is a capped listing so an operator can
/// act without opening the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceTreeDelta {
    pub added: usize,
    pub changed: usize,
    pub removed: usize,
    pub sample_paths: Vec<String>,
}

impl SourceTreeDelta {
    /// How many paths [`Store::status`] names in `sample_paths`.
    pub const SAMPLE: usize = 20;
}

#[derive(Debug, Clone)]
pub struct StoreStatus {
    pub db_path: String,
    pub latest_generation: Option<u32>,
    pub pending_count: usize,
    pub node_count: usize,
    pub edge_count: usize,
    pub degraded_reason: Option<String>,
    /// Current source bytes match the stored inventory. None means not verified.
    pub source_freshness: Option<bool>,
    /// When `source_freshness` is false because the tree differs: counts plus a
    /// capped path sample. Absent for other degradations and when fresh.
    pub source_delta: Option<SourceTreeDelta>,
    /// Stored parser/analyzer identity matches this binary. A parser-free reader
    /// leaves this unknown; source verification remains independent.
    pub analyzer_freshness: Option<bool>,
    pub quarantined_count: usize,
    /// Up to [`Store::DEGRADED_SAMPLE`] of the quarantined paths, oldest first.
    ///
    /// K1(g): the degraded reason used to be a bare count — "64 path(s)
    /// exceeded the retry threshold" — which tells an operator that something
    /// is stuck and nothing about what. On the store this was measured against,
    /// the 64 were paths under a *previous* location of the repository, a 30 MB
    /// vendored `parser.c` that can never fit under `MAX_SOURCE_BYTES`, and
    /// directories: every one of them diagnosable on sight, and none of them
    /// visible.
    ///
    /// A sample, and labelled as one. `quarantined_count` carries the true
    /// total, because a capped list that reads as the whole set is the failure
    /// this codebase treats as worse than a visible gap.
    pub quarantined_paths: Vec<String>,
    /// What the latest generation could not read, by path.
    ///
    /// `degraded_reason` has always carried the three *numbers* — "2 file(s)
    /// failed to parse, 1 recovered by pattern, 1 refused by discovery" — and
    /// nothing anywhere carried the paths, so an operator could not tell a
    /// correct refusal (a 30.6 MB vendored `parser.c` against a 1 MiB ceiling)
    /// from a broken one without opening the database by hand. Each list is
    /// capped at [`crate::COVERAGE_GAP_SAMPLE`] and carries its own total.
    pub coverage_gaps: CoverageGaps,
}

impl StoreStatus {
    /// Freshness is independent of whether the persisted graph can be queried.
    /// This verdict is only as recent as the source verification in Store::status.
    pub fn is_fresh(&self) -> bool {
        self.latest_generation.is_some()
            && self.pending_count == 0
            && self.degraded_reason.is_none()
            && self.source_freshness == Some(true)
            && self.analyzer_freshness == Some(true)
    }

    /// One contract for CLI, daemon and embedded readers. An absent generation
    /// or a pending queue must explain a false verdict even without store damage.
    pub fn freshness_reason(&self) -> Option<String> {
        if let Some(reason) = &self.degraded_reason {
            return Some(reason.clone());
        }
        if self.latest_generation.is_none() {
            return Some(
                "this store holds no generation: nothing has been indexed yet — run `devmap build`"
                    .to_string(),
            );
        }
        if self.pending_count > 0 {
            return Some(format!(
                "{} source change(s) are pending",
                self.pending_count
            ));
        }
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalCheckpointMode {
    Truncate,
    Passive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalCheckpointResult {
    pub mode: WalCheckpointMode,
    pub busy: i64,
    pub log_frames: i64,
    pub checkpointed_frames: i64,
}

/// A caller listing and the unfiltered total it is measured against, from one
/// generation.
///
/// `preview` reports how many callers the confidence floor excluded, as
/// `count_callers_of(.., 0.0) - callers.len()`. Taken from two separate reads
/// those numbers can describe two generations, and the subtraction is
/// saturating — so when the newer generation has fewer callers the difference
/// clamps to zero and `preview` reports that the floor hid nothing. "No
/// ambiguous callers" and "I counted a different corpus" then look identical,
/// which is the reading that lets an edit through as safe.
#[derive(Debug, Clone)]
pub struct CallersPage {
    pub generation: u32,
    /// Callers at or above the requested floor.
    pub callers: Vec<StoredEdge>,
    /// Callers at floor 0.0 — the denominator the floor is measured against.
    pub total_unfiltered: usize,
}

/// The dead-symbol rows and the analysis that qualifies them, from one
/// generation.
///
/// `dead_symbols` attaches a coverage disclosure derived from
/// `AnalysisSummary` to rows read separately. Two reads, two generations: the
/// disclosure could say the corpus was fully covered while the rows came from a
/// generation that was not, which is the exact combination that promotes a
/// finding from "look at this" to "safe to delete".
#[derive(Debug, Clone)]
pub struct DeadPage {
    pub generation: u32,
    pub analysis: Option<AnalysisDisclosure>,
    /// Abandoned cycles found in the same generation, or `None` when the
    /// generation predates the pass or its analysis could not be read.
    ///
    /// Carried on the page rather than fetched separately for the reason the
    /// disclosure is: a cluster list from one generation beside single-symbol
    /// rows from another is the "safe to delete" upgrade
    /// [`Self::analysis`] exists to prevent, one level out.
    ///
    /// Bounded at the source — `DEAD_CLUSTER_CAP` clusters of
    /// `DEAD_CLUSTER_MEMBER_CAP` members — so this is at most a few tens of
    /// kilobytes and needs no budget of its own. It is read with its own
    /// `json_extract` rather than folded into `AnalysisDisclosure`, which is
    /// parsed on many query paths that have no use for it.
    pub dead_clusters: Option<DeadClusterScan>,
    /// Non-exempt rows, ranked, at most the requested limit.
    pub rows: Vec<DeadSymbolReport>,
    /// Every non-exempt row in this generation, independent of the limit.
    ///
    /// The denominator, and the reason the limit is safe. `Response` carries
    /// `shown + hidden == total` and clients enforce it, so a bounded read that
    /// also shrank the count would not merely under-report — it would turn
    /// "66 of 80,000" into "66 of 66", which is the flattering reading of a
    /// list that was cut off.
    pub total_non_exempt: usize,
}

/// One consistent snapshot of a search: the matching rows, the count they were
/// drawn from, the repo root they resolve against, and the disclosure that says
/// how much of the repository the corpus behind them covers — all from the same
/// generation. See [`Store::search_page`] for why they must travel together.
#[derive(Debug, Clone)]
pub struct SearchPage {
    pub generation: u32,
    pub total: u32,
    pub rows: Vec<StoredSymbol>,
    pub repo_root: Option<String>,
    /// How much of the repository this generation actually read.
    ///
    /// Travels with the rows for the reason [`DeadPage`] states: a disclosure
    /// resolved by a second "latest" read could describe a generation the
    /// answer did not come from, and a coverage claim about the wrong corpus is
    /// worse than none. `None` means the analysis blob could not be read, which
    /// is itself a check that did not run — not a clean corpus.
    pub analysis: Option<AnalysisDisclosure>,
    /// Set when the page was cut by a path, language or kind filter. The
    /// counts here are the filtered corpus, not the name-match total — that
    /// stays in [`Self::total`].
    pub narrowing: Option<SearchNarrowing>,
}

/// Path, language and kind bounds applied to one keyword page, plus the
/// corpus sizes the answer echoes. Kinds are canonical `SymbolKind::as_str`
/// spellings. Paths are repository-relative and already checked.
#[derive(Debug, Clone)]
pub struct SearchNarrowing {
    pub paths: Vec<String>,
    pub languages: Vec<String>,
    pub kinds: Vec<String>,
    /// Files matching the path and language, or files holding the kind when
    /// only a kind was given.
    pub files: u32,
    /// Symbols matching path, language and kind.
    pub symbols: u32,
    /// Files in the generation, measured in SQL. A caller that has an analysis
    /// disclosure prefers that count when it is non-zero.
    pub corpus_files: u32,
    /// Symbols in the generation, measured in SQL.
    pub corpus_symbols: u32,
}

/// Caller-supplied keyword narrowing. Empty lists admit everything on that axis.
#[derive(Debug, Clone, Default)]
pub struct KeywordNarrowing {
    pub paths: Vec<String>,
    pub languages: Vec<String>,
    pub kinds: Vec<String>,
}

impl KeywordNarrowing {
    pub fn active(&self) -> bool {
        !self.paths.is_empty() || !self.languages.is_empty() || !self.kinds.is_empty()
    }
}

/// One literal site persisted for a generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredLiteral {
    pub file_path: String,
    pub line: u32,
    pub span_start: u32,
    pub value: String,
    pub qualified_name: String,
    pub symbol_name: String,
}

/// A counted page of literal sites from one generation. `rows` may be shorter
/// than `total` when the page cap cut the read; the caller still reports `total`.
#[derive(Debug, Clone)]
pub struct LiteralPage {
    pub total: u32,
    pub rows: Vec<StoredLiteral>,
}

/// Every file one generation indexed, as `(path, language)`; see
/// [`Store::all_symbols_page_with_files`].
pub type IndexedFiles = Vec<(String, String)>;

/// File parse state, touching edges, and coverage from one pinned generation.
#[derive(Debug, Clone)]
pub struct FileEdges {
    pub generation: u32,
    pub file: StoredFile,
    pub edges: Vec<StoredEdge>,
    pub analysis: Option<AnalysisDisclosure>,
}

/// Symbols declared in one file in the latest generation, with that
/// generation's identity so a caller can compare `head_sha` to the one they
/// asked for.
///
/// Travels as one page for the same reason [`SearchPage`] does: the rows and
/// the generation they came from must be one snapshot. A second "latest"
/// read for the head SHA could describe a different generation.
#[derive(Debug, Clone)]
pub struct FileSymbolsPage {
    pub generation: u32,
    pub head_sha: String,
    pub rows: Vec<StoredSymbol>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredSymbol {
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub path: String,
    pub span_start: usize,
    pub span_end: usize,
    pub is_exported: bool,
    /// Source identity from the same generation as the symbol row.
    pub content_hash: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredEdge {
    pub source_file: String,
    pub target_file: String,
    pub source_symbol: String,
    pub target_symbol: String,
    pub edge_kind: String,
    pub confidence: f32,
    /// `generation_edges.resolution` as stored: the resolver's evidence kind
    /// (`StoredResolutionKind::label`), or `None` on a row written before the
    /// column existed. Decoded by `edge_resolution`, which labels the `None`
    /// case as a reconstruction rather than a reading.
    pub resolution: Option<String>,
}

/// One generation's `paths` rows, ordered once so a path comparison is a `u32`
/// comparison.
///
/// The edge read orders ~100k rows on two path strings. Interning them here
/// costs one scan of a 1,567-row table and turns both keys into ranks whose
/// integer order *is* the byte order of the paths they stand for — see
/// [`edge_read_order`].
struct PathRanks {
    /// Paths in ascending byte order. A rank indexes this.
    ordered: Vec<String>,
    /// `paths.id` to its rank in [`Self::ordered`].
    rank_by_id: std::collections::HashMap<i64, u32>,
}

impl PathRanks {
    fn read(conn: &Connection) -> Result<Self> {
        let mut stmt = conn.prepare("SELECT id, path FROM paths")?;
        let mut rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>>>()?;
        // Byte order, which is what SQLite's default BINARY collation compares
        // and therefore what the `ORDER BY sp.path, tp.path` this replaces was.
        rows.sort_unstable_by(|left, right| left.1.cmp(&right.1));
        let rank_by_id = rows
            .iter()
            .enumerate()
            .map(|(rank, (id, _))| (*id, rank as u32))
            .collect();
        Ok(Self {
            ordered: rows.into_iter().map(|(_, path)| path).collect(),
            rank_by_id,
        })
    }

    /// The rank of a `paths.id`, or an error.
    ///
    /// An edge naming a path row that is not there is a **refusal**, not a
    /// dropped edge. The `INNER JOIN` this replaces answered the same question
    /// by omitting the row, so a store whose `paths` table had lost an entry
    /// answered "nothing depends on this" from a graph it had only partly
    /// read — the same failure `edge_kind_from_stored` refuses for an unknown
    /// kind. `generation_edges.source_file_id` is `REFERENCES paths(id)`, so a
    /// well-formed store cannot reach this.
    fn rank_of(&self, id: i64) -> Result<u32> {
        self.rank_by_id.get(&id).copied().ok_or_else(|| {
            refusal(format!(
                "generation edge names path id {id}, which is not in `paths`; \
                 the store is inconsistent and answering over the edges that \
                 remain would be a wrong answer rather than a partial one"
            ))
        })
    }

    fn path_of(&self, rank: u32) -> &str {
        &self.ordered[rank as usize]
    }

    /// How many distinct `paths` rows this generation's store holds.
    fn len(&self) -> usize {
        self.ordered.len()
    }
}

/// Everything about an edge that a generation stores, as one hashable value.
///
/// The identity a v18 validity range is keyed on: two rows with this tuple are
/// the same edge, and a build that re-derives it leaves the existing row alone.
/// Every column of `edge_rows` except the range itself and the row id is here,
/// deliberately — a column left out would let a build silently keep a row whose
/// stored value it no longer agrees with, which is the carry-forward staleness
/// the edge loop's comment describes and refuses.
///
/// `Cow` because the two sides come from different places: the resolved side
/// borrows out of `ResolutionResult` (no allocation for ~100k edges) and the
/// stored side owns what SQLite handed back. `Cow`'s `Eq` and `Hash` are the
/// underlying `str`'s, so borrowed and owned compare as the strings they are.
///
/// The confidence is the `f64` SQLite stores, compared by bit pattern: `f64` is
/// not `Eq`, and any rounding here would merge two rows the read path can tell
/// apart.
#[cfg(feature = "parse")]
#[derive(PartialEq, Eq, Hash)]
struct EdgeTuple<'a> {
    source_file_id: u32,
    target_file_id: u32,
    source_symbol: std::borrow::Cow<'a, str>,
    target_symbol: std::borrow::Cow<'a, str>,
    edge_kind: std::borrow::Cow<'a, str>,
    confidence: u64,
    resolution: Option<std::borrow::Cow<'a, str>>,
    candidate_total: Option<i64>,
}

/// The end of a bucket chain. `u32::MAX` rather than `Option<u32>` so the array
/// is four bytes an entry: it has one slot per resolved edge, and this store
/// writes 102,083 of them.
#[cfg(feature = "parse")]
const NO_MORE_IN_BUCKET: u32 = u32::MAX;

/// A 64-bit digest of a row's identity, for bucketing only.
///
/// **Never an answer.** Every candidate a bucket offers is compared field by
/// field against the row before it is treated as the same row, so two identities
/// that digest alike are still two identities. The digest exists because the
/// alternative — a `HashMap` keyed by the identity itself — stores the identity
/// twice, once in `resolution` and once in the map, and that second copy is
/// 160 bytes an edge. Measured by `verify.sh` gate 6, which bounds the kernel's
/// memory per unit of ambiguity fan-out: the map put the probe at 116-118 % of
/// its model against a 115 % cap, over five interleaved runs where the base
/// binary measured 100-103 %.
///
/// `DefaultHasher::new` seeds from fixed keys, not from `RandomState`, so one
/// binary buckets the same way on every run — the standard library guarantees
/// only that every `DefaultHasher` built by `new` agrees with every other, and
/// not that the digest survives a Rust upgrade. Nothing here needs more than
/// that: the digest is never stored, never compared across processes, and never
/// leaves this call. A build whose internal structure is the same run to run is
/// simply easier to reason about than one whose is not.
#[cfg(feature = "parse")]
fn identity_digest<T: std::hash::Hash>(identity: &T) -> u64 {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    identity.hash(&mut hasher);
    hasher.finish()
}

/// A 128-bit digest of a *multiset* of row identities, and its size.
///
/// One source file's contribution to a ranged relation, as one comparable
/// value. Two files' row sets are the same set exactly when their digests
/// agree — practically, not provably, and the size of that gap is the whole
/// safety argument, so it is stated rather than assumed:
///
/// * **Order-independent, multiplicity-aware.** The combiner is wrapping
///   addition, which makes the digest a function of the multiset alone. That
///   matters twice. The resolver's emission order within a file is not a
///   promise anyone has made, so an order-sensitive digest would report false
///   differences and quietly give back the saving. And `XOR` — the other
///   obvious combiner — would make a row cancel its own duplicate, so a file
///   holding a tuple twice and one holding it four times would digest alike.
///   475 edge tuples and 12,424 ledger tuples of this repository occur more
///   than once in a single generation, so that is a live case and not a
///   theoretical one.
/// * **128 bits, from two independent hashes.** `lo` and `hi` are
///   [`identity_digest`] of the identity and of the identity behind a
///   domain-separating salt — the same PRF on two different messages. A false
///   "unchanged" needs the changed multiset to preserve `rows`, `lo` and `hi`
///   at once; for row digests that behave as random 64-bit values that is
///   ~2^-128 per file per build, against ~1,600 files and one build per edit.
/// * **The count is carried, not derived.** It is a third field rather than a
///   convenience: `rows` alone catches every change that adds or removes rows,
///   which is most of them, without either sum being consulted.
///
/// The identity hashed is [`EdgeTuple`] / [`UnresolvedTuple`] itself, never a
/// hand-picked subset of their columns. That is the point of load in this whole
/// design: a column added to an identity is a column the digest covers on the
/// same commit, and there is no second list of "the fields that matter" to fall
/// out of step with the first. A digest over a subset would let a build keep a
/// row whose stored value it no longer agrees with — exactly the carry-forward
/// staleness `EdgeTuple`'s own doc comment refuses.
#[cfg(feature = "parse")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RowSetDigest {
    rows: u64,
    lo: u64,
    hi: u64,
}

/// Domain separation for [`RowSetDigest`]'s second hash.
///
/// Any value works as long as it is not the empty prefix; this one is
/// arbitrary. It is written into the hasher ahead of the identity, so `hi` is
/// the same PRF as `lo` over a different message rather than a transformation
/// of `lo` — a second 64 bits of entropy, not a second view of the first.
#[cfg(feature = "parse")]
const ROW_DIGEST_SALT: u64 = 0x9E37_79B9_7F4A_7C15;

#[cfg(feature = "parse")]
impl RowSetDigest {
    /// Fold one row's identity into the digest.
    fn absorb<T: std::hash::Hash>(&mut self, identity: &T) {
        self.rows = self.rows.wrapping_add(1);
        self.lo = self.lo.wrapping_add(identity_digest(identity));
        self.hi = self
            .hi
            .wrapping_add(identity_digest(&(ROW_DIGEST_SALT, identity)));
    }

    /// The three columns as SQLite stores them.
    ///
    /// SQLite has no unsigned integer type, so the same two's-complement
    /// round trip the extraction cache uses for its content hashes.
    fn to_columns(self) -> [i64; 3] {
        [self.rows as i64, self.lo as i64, self.hi as i64]
    }

    fn from_columns(rows: i64, lo: i64, hi: i64) -> Self {
        Self {
            rows: rows as u64,
            lo: lo as u64,
            hi: hi as u64,
        }
    }
}

/// Bucket `count` identities by digest, chaining collisions.
///
/// Returns `(buckets, chain)`: `buckets[digest]` is the newest index with that
/// digest and `chain[index]` the next one, or [`NO_MORE_IN_BUCKET`].
///
/// It takes the identity rather than a digest so that [`identity_digest`] is
/// the one function that decides how anything is bucketed. The alternative —
/// each caller digesting its own way on the way in — is a structure that can be
/// built under one rule and searched under another, and the symptom of that is
/// not a crash but a build that silently rewrites every row.
///
/// `identity_of` returns `None` for an index that is not part of this
/// generation. That index is in no bucket at all, so nothing can match it.
#[cfg(feature = "parse")]
fn bucket_identities<T: std::hash::Hash>(
    count: usize,
    identity_of: impl Fn(usize) -> Option<T>,
) -> (std::collections::HashMap<u64, u32>, Vec<u32>) {
    let mut buckets: std::collections::HashMap<u64, u32> =
        std::collections::HashMap::with_capacity(count);
    let mut chain: Vec<u32> = vec![NO_MORE_IN_BUCKET; count];
    for index in 0..count {
        let Some(identity) = identity_of(index) else {
            continue;
        };
        let digest = identity_digest(&identity);
        let index = index as u32;
        chain[index as usize] = buckets.insert(digest, index).unwrap_or(NO_MORE_IN_BUCKET);
    }
    (buckets, chain)
}

/// Consume the one candidate that *is* this row, and say whether there was one.
///
/// The bucket narrows the search; this comparison decides it. A digest is a
/// filter and never an answer, so every candidate a bucket offers is compared
/// field by field, and a collision merely costs a comparison that fails. The
/// candidate is then marked, which is what makes the whole structure a multiset
/// rather than a set: a row that occurs three times is three candidates, and the
/// three live rows claim them one at a time.
///
/// `identity_of` returns `None` for an index that is not part of this
/// generation. Those are unreachable through the buckets anyway — nothing put
/// them there — and the check is kept so that the two are one statement apart
/// and cannot drift into disagreeing.
#[cfg(feature = "parse")]
fn claim_matching_candidate<T: std::hash::Hash + PartialEq>(
    buckets: &std::collections::HashMap<u64, u32>,
    chain: &[u32],
    matched: &mut [bool],
    live: &T,
    identity_of: impl Fn(usize) -> Option<T>,
) -> bool {
    let mut cursor = buckets
        .get(&identity_digest(live))
        .copied()
        .unwrap_or(NO_MORE_IN_BUCKET);
    while cursor != NO_MORE_IN_BUCKET {
        let index = cursor as usize;
        cursor = chain[index];
        if matched[index] {
            continue;
        }
        let Some(candidate) = identity_of(index) else {
            continue;
        };
        if candidate == *live {
            matched[index] = true;
            return true;
        }
    }
    false
}

/// The identity of one resolved edge, as `edge_rows` stores it.
///
/// The one owner: `save_generation_with_metadata` calls this to decide what to
/// write and again to write it, so those two passes cannot come to disagree
/// about which rows they mean.
///
/// `kind_labels` is the interned `format!("{:?}", kind)` of every kind in the
/// generation. Formatting per *edge* instead is 102,083 heap allocations held
/// for the length of the write, for a value that takes one of a dozen values.
#[cfg(feature = "parse")]
fn edge_tuple<'a>(
    edge: &'a ResolvedEdge,
    kind_labels: &'a std::collections::HashMap<EdgeKind, String>,
    source_file_id: u32,
    target_file_id: u32,
) -> EdgeTuple<'a> {
    EdgeTuple {
        source_file_id,
        target_file_id,
        source_symbol: std::borrow::Cow::Borrowed(edge.source_symbol.as_str()),
        target_symbol: std::borrow::Cow::Borrowed(edge.target_symbol.as_str()),
        edge_kind: std::borrow::Cow::Borrowed(kind_labels[&edge.edge_kind].as_str()),
        // Compared by bit pattern, which is what SQLite stores and what the read
        // path compares. `f64` has no `Eq`, and rounding the key would let two
        // rows the reader can tell apart share one.
        confidence: edge.confidence.persist_real().to_bits(),
        // The evidence tier, so the read path does not have to guess it back out
        // of the row's file layout. NULL only for an edge built without a
        // resolution at all, which the resolver never produces —
        // `ResolvedEdge::new` takes one — and which the read path therefore
        // reports as `ResolutionSource::Reconstructed`.
        resolution: edge.resolution.as_ref().map(|resolution| {
            std::borrow::Cow::Borrowed(crate::edge_index::resolution_kind_label(resolution))
        }),
        // How many candidates the ambiguous rung actually weighed, which since
        // `AMBIGUOUS_FANOUT_CAP` is no longer the number of rows this site
        // produces. NULL for every other rung: a resolution that names one
        // target has no candidate list, and writing 1 there would make a certain
        // edge look like a one-candidate ambiguity.
        candidate_total: crate::edge_index::ambiguous_candidate_total(edge.resolution.as_deref()),
    }
}

/// The same identity for one row of the unresolved-call ledger.
///
/// Three of the six fields are ids since v22, which is what the row stores.
/// The two long ones went that way — a reason averages 130 bytes and a path 40
/// — so a live row read back for comparison costs three integers instead of
/// three strings, and the digest folded into `generation_file_digests` hashes
/// twenty-odd bytes a row instead of two hundred and seventy.
///
/// The ids are only an identity because `unresolved_texts` and `paths` are both
/// `AUTOINCREMENT`: an id never comes back meaning different text, so two
/// digests taken generations apart are comparable.
#[cfg(feature = "parse")]
#[derive(PartialEq, Eq, Hash)]
struct UnresolvedTuple<'a> {
    source_file_id: u32,
    source_symbol: std::borrow::Cow<'a, str>,
    callee_name: std::borrow::Cow<'a, str>,
    reason_id: i64,
    classification_id: i64,
    receiver: Option<std::borrow::Cow<'a, str>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFile {
    pub path: String,
    pub language: String,
    pub content_hash: u64,
    pub parse_outcome: ParseOutcome,
    pub engine: ExtractionEngine,
}

#[derive(Debug, Clone, Default)]
pub struct GenerationWriteOpts {
    /// Files whose rows must be rewritten (differential path). Empty = full rewrite of inputs.
    pub affected_paths: Vec<String>,
    /// Files known deleted from the working tree — must not remain as live nodes (N2).
    pub deleted_paths: Vec<String>,
    /// Start of measured build work. The store samples it while persisting the
    /// history row, so the duration includes generation writes. `None` remains
    /// SQL NULL rather than becoming an ambiguous numeric zero.
    pub build_started: Option<std::time::Instant>,
    /// Absolute root the sources were read from. Node paths are stored
    /// repo-relative, so without this a query process resolves them against its
    /// own working directory and every span read from elsewhere comes back
    /// empty. `None` stays NULL — "root unknown", never a wrong root.
    pub repo_root: Option<String>,
    /// Every path discovery refused for this generation, with its verdict.
    ///
    /// The whole inventory, not a delta: the generation stores what it could
    /// not read, and `discovery_refused_files` is `COUNT(*)` over these rows.
    /// The daemon's incremental drain, which never re-walks discovery, builds
    /// it by carrying the previous generation's rows minus every path in this
    /// batch's affected set and adding what this batch was turned away from.
    ///
    /// `None` means *this writer did not measure discovery* — a caller that
    /// supplied its own corpus, which is every test and the single-file preview
    /// path — and is not the same as `Some(vec![])`, a walk that ran and
    /// refused nothing. It is the same distinction
    /// [`devmap_analyze::DiscoveryCoverage::none`] draws, and
    /// `save_generation_with_metadata` refuses a generation whose two halves
    /// disagree about which of them it is.
    pub discovery_refusals: Option<Vec<DiscoveryRefusal>>,
    /// Compare every stored row, rather than only the files whose freshly
    /// resolved rows disagree with the digest the previous generation recorded.
    ///
    /// The escape hatch for the v19 scoping, and the switch the equivalence
    /// test in `digest_scoped_delta.rs` flips to prove the two paths write the
    /// same store. It is not the same lever as an empty affected set: a full
    /// rewrite re-*extracts* every file, which is minutes, while this keeps the
    /// incremental extraction and only re-derives which stored rows are still
    /// wanted, which is the ~200 ms the scoping saves. `devmap build
    /// --verify-rows` is the caller that sets it.
    ///
    /// A full rewrite implies it — with no previous generation to have written
    /// digests, and every row of the relation being replaced, there is nothing
    /// to scope by — so callers of that path need not also set it.
    pub verify_every_row: bool,
}

/// What one generation write spent, charged to the relation that spent it.
///
/// `persist:write` is one number, and on this repository it is 0.30 s of a
/// 1.10 s one-file incremental build. The relations under it have nothing in
/// common as fixes -- v18 put the edges and the unresolved ledger on validity
/// ranges and left the rest as full per-generation copies -- so a single span
/// cannot say which of them a build is waiting for, and the decision about the
/// next schema rung is exactly that question.
///
/// **Accumulated, not bracketed.** The node and full-text writes are
/// interleaved by construction: an FTS rowid is derived from the node ordinal
/// the same loop just produced, so separating them into two passes would mean
/// inventing a second ordinal counter and a second walk. Each field is instead
/// the sum of the spans that relation's statements were actually inside.
///
/// The consequence is that the parts **under-account** for the write by the
/// glue between them -- the transaction, the carry decision, the guards -- and
/// never over-account for it. A reader may sum them and compare the total to
/// `persist:write`; the remainder is real and unattributed, not missing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WriteBreakdown {
    /// `generation_file_rows` and the `file_payloads` behind it, including the
    /// three JSON serializations a fresh payload needs.
    pub file_rows: f64,
    /// `generation_nodes`, carried and fresh.
    pub nodes: f64,
    /// `nodes_fts` and `nodes_fts_map`, carried and fresh.
    pub fts: f64,
    /// The edge delta: the scan of live rows, the closes and the inserts.
    pub edges: f64,
    /// The unresolved-call ledger delta, the same three passes.
    pub unresolved: f64,
    /// `generation_file_digests` -- writing the per-file digests the *next*
    /// build scopes its two deltas by. Only the table write: computing a digest
    /// is part of the pass over the relation it describes, and is charged to
    /// `edges` and `unresolved` where the rows are.
    pub digests: f64,
    /// `generation_coverage_gaps`, including the carry-forward scan.
    pub gaps: f64,
    /// `generation_dead_symbols`.
    pub dead: f64,
    /// `build_history` and the aggregate queries it is computed from.
    pub history: f64,
    /// `tx.commit()` -- the durability the whole write is waiting for.
    pub commit: f64,
    /// Everything the ten spans above do not bracket, as a residual.
    ///
    /// Set once by [`Self::attribute_residual`] at the end of the write, as
    /// `total - (the ten named spans)`, so the split adds up to the write by
    /// construction rather than by anyone remembering to bracket a new
    /// statement.
    ///
    /// This is not a rounding term. It was 16% of a cold `persist:write` when
    /// it was first computed -- larger than every named span but two -- and it
    /// is 25% now, because the named spans got faster and this did not.
    ///
    /// What is in it, by reading the write rather than by measuring inside it:
    /// the `serde_json` of the `AnalysisSummary` and the `generations` insert
    /// that carries the ~1.09 MB of JSON it produces, the duplicate-path scan
    /// over every extraction, the transaction and `pending_state` setup, and --
    /// on an incremental write only -- the carry-forward decision, which reads
    /// the previous generation's file rows to choose what to reuse. The one
    /// part of that with a measurement is the `AnalysisSummary` *clone* that
    /// used to precede the serialization: removing it moved `persist:write`
    /// 0.671 s to 0.623 s. The rest is unmeasured and named here as unmeasured.
    ///
    /// A span cannot cover work nobody thought to bracket; subtraction covers
    /// exactly that work and nothing else. Attribute a piece of it properly and
    /// this number should fall by that much -- which is the check that the
    /// residual is real rather than a bucket that absorbs mistakes.
    pub other: f64,
}

impl WriteBreakdown {
    /// The split as labelled spans, in the order the write incurs them.
    ///
    /// One owner for the labels: the `--json` timings and any test that names a
    /// relation read them from here, so a field added to the struct and left
    /// out of the report is a compile-time omission rather than a silent one.
    pub fn parts(&self) -> Vec<(&'static str, f64)> {
        let Self {
            file_rows,
            nodes,
            fts,
            edges,
            unresolved,
            digests,
            gaps,
            dead,
            history,
            commit,
            other,
        } = *self;
        vec![
            ("file_rows", file_rows),
            ("nodes", nodes),
            ("fts", fts),
            ("edges", edges),
            ("unresolved", unresolved),
            ("digests", digests),
            ("gaps", gaps),
            ("dead", dead),
            ("history", history),
            ("commit", commit),
            ("other", other),
        ]
    }

    /// The ten bracketed spans, without the residual.
    ///
    /// Separate from [`Self::parts`] because the residual is *defined* as the
    /// total minus this, and a sum that included it would define `other` in
    /// terms of itself.
    fn charged_spans(&self) -> f64 {
        self.parts()
            .iter()
            .filter(|(label, _)| *label != "other")
            .map(|(_, secs)| secs)
            .sum()
    }

    /// Close the split against the write it describes.
    ///
    /// Called once, at the end of the write, with the wall time of the whole
    /// call. After it, `parts()` sums to `total` -- which is the property that
    /// makes the breakdown safe to optimise from: anything the named spans do
    /// not cover appears as `other` instead of vanishing.
    ///
    /// Saturating at zero. The spans are measured strictly inside the total, so
    /// a negative residual is not a slow write but a clock that went backwards,
    /// and reporting `0.0` for a span that cannot be believed is better than
    /// reporting a negative duration a caller will render.
    pub fn attribute_residual(&mut self, total: f64) {
        self.other = (total - self.charged_spans()).max(0.0);
    }
}

/// Charges the wall time it is alive for to one field of a [`WriteBreakdown`].
///
/// A guard rather than a closure taking the work, because the write path is a
/// sequence of statements interleaved with the bindings they produce: wrapping
/// a region in a closure would mean either re-indenting several hundred lines
/// or threading every binding out through a tuple. A guard costs one line at
/// the top of a block that is already there.
///
/// It charges on `Drop`, so a statement that fails is charged for the time it
/// took before failing. The alternative -- charging only on success -- would
/// leave the one build worth profiling as the one build with no profile.
///
/// Gated on `parse` because its only caller is: `save_generation_timed` is the
/// write path, which a build without grammars does not carry (see
/// [`Store::save_generation`]). With the feature off this is dead code, and
/// `cargo check -p devmap-store --no-default-features` warns about it.
/// [`WriteBreakdown`] itself stays ungated: it is public, an embedder that
/// reads a persisted map can name the type, and gating it would gate the
/// re-export too.
#[cfg(feature = "parse")]
struct Charge<'a> {
    sink: &'a mut f64,
    started: std::time::Instant,
}

#[cfg(feature = "parse")]
impl Drop for Charge<'_> {
    fn drop(&mut self) {
        *self.sink += self.started.elapsed().as_secs_f64();
    }
}

#[cfg(feature = "parse")]
fn charge(sink: &mut f64) -> Charge<'_> {
    Charge {
        sink,
        started: std::time::Instant::now(),
    }
}

/// Per name: its sites, and whether the per-name cap cut them.
pub type UnresolvedSitesByName = BTreeMap<String, (Vec<UnresolvedSiteRow>, bool)>;

/// One unresolved call site, as [`Store::unresolved_sites_naming`] returns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedSiteRow {
    pub source_file: String,
    pub source_symbol: String,
    /// The receiver expression as written, when the call had one.
    pub receiver: Option<String>,
    /// The ledger's own class: `uninferred_receiver`, `unresolved`,
    /// `external`, … — why the resolver did not bind it.
    pub classification: String,
    /// The name the site calls. Equal to the map key for a by-name read;
    /// carried because a by-caller read is keyed by the caller instead.
    pub callee_name: String,
}

/// Which ledger column a keyed read matches its keys against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LedgerKey {
    Callee,
    SourceSymbol,
    SourceFile,
}

/// One committed build, as recorded by [`Store::build_history`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildHistoryRow {
    pub generation_id: u32,
    pub built_at: i64,
    pub head_sha: String,
    pub files: u64,
    pub symbols: u64,
    pub edges: u64,
    pub dead_confident: u64,
    pub dead_ambiguous: u64,
    pub parse_failed: u64,
    pub languages_covered: u64,
    pub build_ms: Option<u64>,
    pub db_bytes: u64,
}

/// What [`Store::vacuum_if_needed`] did, and what it saw when it decided.
///
/// Returned rather than discarded because "declined to reclaim" and "reclaimed
/// nothing" leave an identical database behind, and telling them apart is the
/// difference between a healthy store and one growing forever. That is not
/// hypothetical: a stale freelist read made this function decline eight builds
/// in a row while a third of the file was free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VacuumOutcome {
    pub freelist_before: i64,
    pub page_count_before: i64,
    pub action: VacuumAction,
    /// The WAL checkpoint that decides whether the reclaim reached the file.
    ///
    /// K2: `PRAGMA incremental_vacuum` truncates the database, and in WAL mode
    /// that truncation is a WAL frame like any other — it does not touch the
    /// main file until a checkpoint folds it back. `vacuum_if_needed`
    /// checkpointed only *before* reading the page accounting, so the reclaim
    /// ran, reported pages, and left the file exactly as large as it found it:
    /// measured at 42% freelist, unchanged file size across eight builds, and a
    /// 109 MB WAL.
    ///
    /// `None` means the checkpoint could not be run at all. A `busy` other than
    /// zero means an active reader held the WAL and the truncation is still
    /// pending — the build reports that rather than discarding it, because
    /// "reclaimed and the file shrank" and "reclaimed and nothing moved" are
    /// otherwise indistinguishable from the outside.
    pub checkpoint: Option<WalCheckpointResult>,
    /// Free pages this call actually returned to the end of the file.
    ///
    /// Counted, not assumed. `PRAGMA incremental_vacuum(N)` frees **one page
    /// per row stepped**, and rusqlite 0.31's `execute_batch` steps exactly
    /// once (`lib.rs::execute_batch`: `stmt.step()?`, then straight to the
    /// tail) — so the pragma freed a single page per build while the action
    /// beside it reported the 65,536 it had been asked for. Measured on the
    /// live 701 MB store: four consecutive builds moved the freelist 116,116 ->
    /// 116,045 and the file never left 701 MB. Fully stepping the same pragma
    /// on a copy took the freelist to 49,545 and the file to 429 MB in 4.5 s.
    ///
    /// Carrying the count is what makes the difference visible: a request and a
    /// result that print identically cannot be told apart from a log.
    pub pages_freed: i64,
}

impl VacuumOutcome {
    /// Free pages as a percentage of the file when the decision was made.
    pub fn freelist_ratio(&self) -> f64 {
        if self.page_count_before <= 0 {
            return 0.0;
        }
        self.freelist_before as f64 / self.page_count_before as f64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VacuumAction {
    /// Below the threshold; nothing worth reclaiming.
    Declined,
    /// Bounded reclaim, asked to move up to `requested` free pages.
    ///
    /// `requested` is the ceiling, not the result: read
    /// [`VacuumOutcome::pages_freed`] for what actually moved. The two were
    /// conflated, and printing the request as though it were the outcome is how
    /// a one-page reclaim reported `incremental(65536 pages)` for four builds
    /// running while the file never shrank.
    Incremental { requested: i64 },
    /// Whole-file rewrite that also converts a legacy store to incremental
    /// mode, so this is the last time that store pays for one.
    FullConverting,
}

impl std::fmt::Display for VacuumAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Declined => write!(f, "declined"),
            Self::Incremental { requested } => write!(f, "incremental(<={requested} pages)"),
            Self::FullConverting => write!(f, "full+convert"),
        }
    }
}

/// Refuse a confidence threshold that no comparison can evaluate.
///
/// `min_confidence` is compared two ways in this file — in SQL for the
/// file-scoped query, and in Rust for the cached whole-generation one — and on
/// NaN they disagree completely rather than at a boundary: Rust admits every
/// edge, SQLite admits none. Whichever answered, the caller could not tell that
/// the filter had not run, because an empty edge list is also what a real
/// filter returns.
///
/// Refused rather than clamped or defaulted. NaN means the caller does not know
/// what it is asking for, and picking a threshold on its behalf publishes a
/// number nobody chose. Infinities are left alone: `>= inf` and `>= -inf` are
/// degenerate but both implementations agree on them, and so is any finite
/// value outside 0.0..=1.0 — an empty answer there is a filter that ran and
/// matched nothing, which is a real result.
/// A `usize` row cap as SQLite's `LIMIT` reads it.
///
/// SQLite takes `LIMIT` as a signed 64-bit value and treats a **negative** one
/// as *unbounded*. `limit as i64` therefore inverts the request for every
/// `usize` at or above `2^63`: `usize::MAX as i64` is `-1`, so a caller asking
/// for the largest cap it can name got no cap at all. Clamping keeps it a cap
/// — the largest one SQLite can express — and a caller that wanted everything
/// still gets everything.
///
/// One owner for the rule. Four bounded readers each carried their own copy of
/// this clamp and a fifth, `latest_unresolved`, was written without it; that is
/// the shape a shared helper exists to prevent.
/// What a WAL sidecar's link count says about it, read from an open handle.
#[derive(Debug, PartialEq, Eq)]
enum SidecarLinks {
    /// The ordinary case: one name, this one.
    Single,
    /// Deleted after it was opened — SQLite removes `-wal` when the last
    /// writer connection closes, so a reader racing a committing build sees
    /// this. It is the missing-sidecar case, and refusing it failed reads
    /// exactly while a build committed (`queries_succeed_while_a_build_is_committing`).
    Unlinked,
    /// A second name for the same inode: the alias the check exists to refuse,
    /// because WAL and writer ownership could then diverge.
    Aliased,
}

fn sidecar_links(count: u64) -> SidecarLinks {
    match count {
        0 => SidecarLinks::Unlinked,
        1 => SidecarLinks::Single,
        _ => SidecarLinks::Aliased,
    }
}

#[cfg(test)]
#[path = "db/tests/sidecar_link_tests.rs"]
mod sidecar_link_tests;

fn sqlite_limit(limit: usize) -> i64 {
    limit.min(i64::MAX as usize) as i64
}

fn stored_symbol_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredSymbol> {
    let name: String = row.get(0)?;
    let path: String = row.get(3)?;
    let (span_start, span_end) = checked_span(&path, &name, row.get(4)?, row.get(5)?)?;
    Ok(StoredSymbol {
        name,
        qualified_name: row.get(1)?,
        kind: row.get(2)?,
        path,
        span_start,
        span_end,
        is_exported: row.get::<_, i64>(6)? != 0,
        content_hash: row.get::<_, i64>(7)? as u64,
    })
}

fn append_narrowing(sql: &mut String, params: &mut Vec<Box<dyn ToSql>>, filter: &KeywordNarrowing) {
    append_paths(sql, params, &filter.paths);
    append_languages(sql, params, &filter.languages);
    append_kinds(sql, params, &filter.kinds);
}

/// Segment-boundary prefix, the same rule as `scope::under_prefix`: `frontend`
/// admits `frontend/app.tsx` and a file named `frontend`, never `frontend2/`.
fn append_paths(sql: &mut String, params: &mut Vec<Box<dyn ToSql>>, paths: &[String]) {
    if paths.is_empty() {
        return;
    }
    sql.push_str(" AND (");
    for (index, path) in paths.iter().enumerate() {
        if index > 0 {
            sql.push_str(" OR ");
        }
        sql.push_str("(p.path = ? OR substr(p.path, 1, length(?) + 1) = (? || '/'))");
        params.push(Box::new(path.clone()));
        params.push(Box::new(path.clone()));
        params.push(Box::new(path.clone()));
    }
    sql.push(')');
}

fn append_languages(sql: &mut String, params: &mut Vec<Box<dyn ToSql>>, languages: &[String]) {
    if languages.is_empty() {
        return;
    }
    sql.push_str(" AND lower(f.language) IN (");
    for (index, language) in languages.iter().enumerate() {
        if index > 0 {
            sql.push_str(", ");
        }
        sql.push('?');
        params.push(Box::new(language.clone()));
    }
    sql.push(')');
}

fn append_kinds(sql: &mut String, params: &mut Vec<Box<dyn ToSql>>, kinds: &[String]) {
    if kinds.is_empty() {
        return;
    }
    sql.push_str(" AND n.kind IN (");
    for (index, kind) in kinds.iter().enumerate() {
        if index > 0 {
            sql.push_str(", ");
        }
        sql.push('?');
        params.push(Box::new(kind.clone()));
    }
    sql.push(')');
}

fn distinct_kinds(
    conn: &Connection,
    generation: u32,
    filter: &KeywordNarrowing,
) -> Result<BTreeSet<String>> {
    let mut sql = String::from(
        "SELECT DISTINCT n.kind FROM generation_nodes n
         JOIN paths p ON p.id = n.file_id
         JOIN generation_files f
           ON f.generation_id = n.generation_id AND f.file_id = n.file_id
         WHERE n.generation_id = ?",
    );
    let mut params: Vec<Box<dyn ToSql>> = vec![Box::new(i64::from(generation))];
    append_paths(&mut sql, &mut params, &filter.paths);
    append_languages(&mut sql, &mut params, &filter.languages);
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params.iter()), |row| {
        row.get::<_, String>(0)
    })?;
    let mut present = BTreeSet::new();
    for row in rows {
        present.insert(row?);
    }
    Ok(present)
}

/// `?` placeholders, in order: generation, then the value bound(s).
fn literal_predicate(
    generation: u32,
    query: &str,
    exact: bool,
) -> Result<(String, Vec<Box<dyn ToSql>>)> {
    let mut params: Vec<Box<dyn ToSql>> =
        vec![Box::new(i64::from(generation)), Box::new(query.to_string())];
    if exact {
        return Ok(("l.generation_id = ? AND l.value = ?".to_string(), params));
    }
    if let Some(end) = prefix_successor(query) {
        params.push(Box::new(end));
        Ok((
            "l.generation_id = ? AND l.value >= ? AND l.value < ?".to_string(),
            params,
        ))
    } else {
        params.push(Box::new(query.to_string()));
        Ok((
            "l.generation_id = ? AND l.value >= ? AND instr(l.value, ?) = 1".to_string(),
            params,
        ))
    }
}

/// The smallest string strictly above every string that starts with `prefix`,
/// or `None` when incrementing the last non-0xFF byte is not valid UTF-8.
fn prefix_successor(prefix: &str) -> Option<String> {
    let mut bytes = prefix.as_bytes().to_vec();
    loop {
        let last = bytes.last_mut()?;
        if *last < 0xFF {
            *last += 1;
            return String::from_utf8(bytes).ok();
        }
        bytes.pop();
    }
}

/// The stored byte span of a symbol row, or a refusal naming the row.
///
/// S-11: three readers decoded the same two columns and two of them disagreed
/// with the third. `search_symbols` errored on a corrupt span while
/// `all_symbols` and `latest_clone_candidates` clamped it with `.max(0)` and
/// published `0..0` — a span that looks real, points at the top of the file,
/// and is indistinguishable from a zero-length symbol at offset 0. One corrupt
/// row therefore made `search` fail closed and the other two lie, which is the
/// exact shape "a check that could not run must not answer like one that ran"
/// exists to forbid. The loud policy wins: a span is a byte range into a file,
/// a negative start or an end before the start is not one, and a fabricated
/// range is worse than a refusal that names the symbol.
fn checked_span(path: &str, name: &str, start: i64, end: i64) -> Result<(usize, usize)> {
    let corrupt = || {
        refusal(format!(
            "stored span for symbol {name:?} in {path} is not a byte range: \
             span_start={start}, span_end={end}"
        ))
    };
    if end < start {
        return Err(corrupt());
    }
    let start = usize::try_from(start).map_err(|_| corrupt())?;
    let end = usize::try_from(end).map_err(|_| corrupt())?;
    Ok((start, end))
}

/// Decode the two `generation_files` columns that record how a file was read.
///
/// One owner: [`Store::latest_file`] and the `build_history` parse-failure
/// count both need them, and a quiet decode in either would report a store
/// fault as a fact about the code.
fn decode_stored_outcome(
    path: &str,
    parse_json: &str,
    engine_json: &str,
) -> Result<(ParseOutcome, ExtractionEngine)> {
    let parse_outcome = serde_json::from_str(parse_json).map_err(|error| {
        refusal(format!(
            "stored parse outcome for {path} is invalid: {error}"
        ))
    })?;
    let engine = serde_json::from_str(engine_json).map_err(|error| {
        refusal(format!(
            "stored extraction engine for {path} is invalid: {error}"
        ))
    })?;
    Ok((parse_outcome, engine))
}

/// Is a stored row a parse failure?
///
/// The same rule `devmap_extract::model::Extraction::is_parse_failure` applies
/// in memory, asked of the two columns that carry it. Rehydrating the whole
/// payload to call the canonical method would mean deserializing every
/// extraction in the generation — measured at 198 MiB on one corpus — to answer
/// a yes/no question, so the *rule* is restated over the stored fields and
/// `the_stored_parse_failure_rule_matches_the_canonical_classifier` fails if
/// the two ever disagree on a real corpus.
#[cfg(feature = "parse")]
fn stored_is_parse_failure(outcome: &ParseOutcome, engine: &ExtractionEngine) -> bool {
    matches!(outcome, ParseOutcome::Failed { .. })
        && !matches!(engine, ExtractionEngine::NotApplicable { .. })
}

/// The FTS5 `MATCH` expression for a user's search string, or an error naming
/// why the store cannot express it.
///
/// The rule is that the *whole* input is one quoted prefix phrase, so FTS5
/// operators, column filters, parentheses, wildcards and hyphens stay data
/// rather than becoming syntax. Doubling interior quotes is the FTS5 escape.
///
/// **An interior NUL breaks that rule, and the escape cannot fix it.** SQLite
/// hands the MATCH argument to FTS5's parser as a C string, so `"alpha\0beta"*`
/// is parsed as `"alpha` — the closing quote is beyond the terminator. The
/// observable result was `unterminated string` raised from inside SQLite: a
/// query surface leaking a parser error for an input the caller was entitled to
/// pass. Silently truncating at the NUL is worse — the search would then run on
/// a prefix of what was asked and report the answer as if it had run on all of
/// it. So the store refuses and says so.
///
/// One function rather than three copies: `search_fts`, `search_symbols` and
/// `count_search_symbols` each carried their own `replace('"', "\"\"")` and
/// their own `format!`, which is why the NUL hole existed in all three and
/// would have been closed in one.
fn fts_match_query(query: &str) -> Result<String> {
    if let Some(offset) = query.find('\0') {
        return Err(refusal(format!(
            "search query contains a NUL byte at offset {offset}; SQLite's \
             full-text parser reads the query as a C string, so no escaping \
             can carry one through"
        )));
    }
    // Each *word* is its own quoted prefix term, joined by AND — not the whole
    // input as one quoted phrase.
    //
    // A quoted FTS5 phrase requires its tokens to appear **adjacently and in
    // order** inside a single indexed column, and the indexed columns are
    // `name`, `qualified_name` and `path`. So `"optimizer AdamW step"*` asked
    // for a symbol literally *named* `optimizer AdamW step…`, which nothing is,
    // and the store answered `total: 0, truncated: false, hidden: 0` — a shape
    // indistinguishable from "this repository contains no such thing".
    //
    // Measured on this repository: `devmap_search "dead_symbols"` returned 8
    // rows and `devmap_search "dead symbols"` returned 0. Same corpus, same
    // generation, one space.
    //
    // A single-word query still produces exactly `"word"*`, byte for byte, so
    // every query that worked before produces the identical MATCH expression
    // and the identical rows.
    //
    // The quoting rule this function exists to enforce is untouched: each term
    // is quoted individually, so FTS5 operators, column filters, parentheses,
    // wildcards and hyphens inside a term stay data rather than becoming
    // syntax. `AND` is the only thing this function adds as syntax, and it adds
    // it *between* quoted terms where no user text can reach.
    let mut terms = query
        .split_whitespace()
        .map(|term| format!("\"{}\"*", term.replace('"', "\"\"")))
        .peekable();
    if terms.peek().is_none() {
        // Whitespace-only (or empty). No term to join, and an empty MATCH
        // expression is a syntax error rather than an empty result — so this
        // keeps the exact expression the single-phrase form produced, and with
        // it whatever SQLite already did about it.
        return Ok(format!("\"{}\"*", query.replace('"', "\"\"")));
    }
    Ok(terms.collect::<Vec<_>>().join(" AND "))
}

pub fn checked_min_confidence(value: f32) -> Result<f32> {
    if value.is_nan() {
        return Err(refusal(
            "min_confidence must be a number; got NaN, which no confidence \
             comparison can evaluate"
                .to_string(),
        ));
    }
    Ok(value)
}

/// Every table and column this binary's readers and writers address by name.
///
/// S-8: this list is the schema gate, and it was narrower than the schema it
/// claimed to assert — `generation_files.grammar_version`/`analyzer_version`
/// (v8) and `generation_unresolved.classification`/`receiver` (v10/v11) were
/// missing, so a store stamped at the current version without them opened
/// clean and failed at the first *write* instead of at the gate. A gate that
/// passes a store it cannot write to is worse than no gate: it moves the
/// failure from "this store is not usable" to a mid-build error naming a
/// column.
///
/// Completeness is enforced, not asserted:
/// `the_schema_gate_names_every_column_the_current_schema_creates` builds a
/// fresh store and fails if any column of these tables is missing here, so a
/// future migration cannot add a column and silently leave the gate behind.
/// FTS5's *shadow* tables (`nodes_fts_data`, `_idx`, `_content`, `_docsize`,
/// `_config`) are deliberately absent — SQLite owns their layout and it is not
/// this crate's to assert. `nodes_fts` itself is this crate's DDL and is
/// searched by column name, so it is asserted.
/// One stored extraction payload, as `file_payloads` holds it.
///
/// A struct rather than eight positional parameters: five of the eight are
/// `&str`, so a transposed pair would compile and store an engine description
/// in the parse-outcome column. Named fields make that a compile error.
#[cfg(feature = "parse")]
struct StoredPayload<'a> {
    file_id: u32,
    content_hash: i64,
    language: &'a str,
    grammar_version: &'a str,
    analyzer_version: &'a str,
    parse_outcome_json: &'a str,
    engine_json: &'a str,
    extraction_json: &'a str,
}

/// Relations the current schema creates that a reader must tolerate *absent*,
/// and so that `REQUIRED_SCHEMA` deliberately does not name.
///
/// `reader_compat` arrived without a `user_version` bump, so a schema-26
/// store written before it exists lacks it — and a reader cannot create it.
/// Requiring it would make every such store unreadable by the binaries that
/// introduced the floor, the opposite of what it is for. Its absence has a
/// defined meaning instead (exact-match admission), and its *contents* are
/// validated where they are read (`Store::recorded_reader_floor`), which
/// refuses a damaged row rather than reading it as absent.
///
/// Only the relation gate's test reads this list, so it exists only in test
/// builds; a release build would otherwise warn that it is never used.
#[cfg(test)]
pub(crate) const OPTIONAL_RELATIONS: &[&str] = &["reader_compat"];

const REQUIRED_SCHEMA: &[(&str, &[&str])] = &[
    ("paths", &["id", "path"]),
    (
        "generations",
        &["id", "created_at", "head_sha", "analysis_json", "repo_root"],
    ),
    (
        "generation_nodes",
        &[
            "generation_id",
            "ordinal",
            "file_id",
            "name",
            "qualified_name",
            "kind",
            "span_start",
            "span_end",
            "is_exported",
            "body_exact",
            "body_structural",
            "body_nodes",
        ],
    ),
    (
        "generation_files",
        &[
            "generation_id",
            "file_id",
            "language",
            "content_hash",
            "parse_outcome_json",
            "engine_json",
            "extraction_json",
            "grammar_version",
            "analyzer_version",
        ],
    ),
    (
        "generation_edges",
        &[
            "generation_id",
            "ordinal",
            "source_file_id",
            "target_file_id",
            "source_symbol",
            "target_symbol",
            "edge_kind",
            "confidence",
            "resolution",
            "candidate_total",
        ],
    ),
    // The v17 payload split's two base tables. They were validated only
    // transitively, through the `generation_files` view that joins them — so a
    // migration that produced the view over the wrong shape, or dropped one of
    // them, was caught by nothing until the first write. The S-8 drift test
    // iterates this list and asserts every *actual* column is required, which
    // by construction can never notice a *table* that is absent from it.
    (
        "file_payloads",
        &[
            "payload_id",
            "file_id",
            "content_hash",
            "language",
            "grammar_version",
            "analyzer_version",
            "parse_outcome_json",
            "engine_json",
            "extraction_json",
        ],
    ),
    (
        "generation_file_rows",
        &["generation_id", "file_id", "payload_id"],
    ),
    // v18's two base tables, listed for the same reason v17's are: the views
    // above are validated through `PRAGMA table_info`, which answers for a view
    // without saying anything about what it is a view *over*. A migration that
    // built the view over the wrong shape would pass the check above and fail at
    // the first write.
    (
        "edge_rows",
        &[
            "edge_id",
            "source_file_id",
            "target_file_id",
            "source_symbol",
            "target_symbol",
            "edge_kind",
            "confidence",
            "resolution",
            "candidate_total",
            "valid_from",
            "valid_to",
        ],
    ),
    (
        "unresolved_rows",
        &[
            "unresolved_id",
            "source_file_id",
            "source_symbol",
            "callee_name",
            "reason_id",
            "classification_id",
            "receiver",
            "valid_from",
            "valid_to",
        ],
    ),
    // v22's interning pool. Listed for the reason v17's and v18's base tables
    // are: the `generation_unresolved` view joins it, and `PRAGMA table_info`
    // on a view says nothing about what the view is *over*, so a migration that
    // built the pool with the wrong columns would pass the gate and fail at the
    // first write.
    ("unresolved_texts", &["id", "text"]),
    (
        "generation_coverage_gaps",
        &["generation_id", "gap", "path", "reason"],
    ),
    // v19's digest cache. Listed for the same reason the two above are: nothing
    // reads it but the write path, so a migration that created it with the
    // wrong columns would be caught by nothing until a build tried to record a
    // digest — and a build that cannot record one silently loses the scoping
    // rather than failing, which is the worst way for this table to be wrong.
    (
        "generation_file_digests",
        &[
            "generation_id",
            "file_id",
            "edge_rows",
            "edge_lo",
            "edge_hi",
            "unresolved_rows",
            "unresolved_lo",
            "unresolved_hi",
        ],
    ),
    (
        "generation_unresolved",
        &[
            "generation_id",
            "ordinal",
            "source_file",
            "source_symbol",
            "callee_name",
            "reason",
            "classification",
            "receiver",
        ],
    ),
    (
        "generation_dead_symbols",
        &[
            "generation_id",
            "ordinal",
            "file_path",
            "symbol_name",
            "confidence",
            "is_exempt",
            "exemption_reason",
        ],
    ),
    (
        "generation_literals",
        &[
            "generation_id",
            "file_id",
            "line",
            "span_start",
            "value",
            "qualified_name",
            "symbol_name",
        ],
    ),
    (
        "extraction_cache",
        &[
            "content_hash",
            "language",
            "grammar_version",
            "analyzer_version",
            "payload_json",
            "accessed_at",
        ],
    ),
    (
        "extraction_retry",
        &[
            "content_hash",
            "language",
            "attempts",
            "last_reason",
            "updated_at",
        ],
    ),
    (
        "pending_paths",
        &["path", "queued_at", "attempts", "revision"],
    ),
    (
        "pending_state",
        &["singleton", "epoch", "revision", "repo_root"],
    ),
    ("nodes_fts", &["name", "qualified_name", "path"]),
    ("nodes_fts_map", &["rowid_ref", "generation_id"]),
    (
        "build_history",
        &[
            "generation_id",
            "built_at",
            "head_sha",
            "files",
            "symbols",
            "edges",
            "dead_confident",
            "dead_ambiguous",
            "parse_failed",
            "languages_covered",
            "build_ms",
            "db_bytes",
        ],
    ),
];

impl Store {
    /// Page cache for a write connection, in KiB (negative = KiB, per SQLite).
    ///
    /// 64 MiB against SQLite's 2 MiB default. A generation write is a bulk
    /// insert that revisits index pages across the whole file — at 2 MiB the
    /// working set does not fit and the same pages are read, evicted and read
    /// again for the length of the transaction.
    /// Page size a store created by this code uses. See the pragma in
    /// `configure_connection` for the measurements behind it.
    ///
    /// Public because `devmap repair --page-size` converts an existing store to
    /// it, and a second copy of the number in the CLI is exactly the mirror that
    /// let `VACUUM_MAX_PAGES` drift.
    pub const PAGE_SIZE: i64 = 16384;

    const CACHE_SIZE_KIB: i32 = -65_536;

    /// How long any connection waits for a lock before giving up.
    ///
    /// S-7: `stored_schema_version` opens its own read-only connection and
    /// never passes through [`Self::configure_connection`], so the crate's
    /// contention policy was stated in one place and *inherited* in the other
    /// — rusqlite happens to default to the same five seconds, which is why
    /// the two agree today. An inherited default is not a policy: a
    /// dependency bump that changed it would silently give one reader a
    /// different wait from every other, and nothing would fail. Stated once
    /// and applied at both openers instead.
    const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
    /// Bursts from hundreds of sessions must be admitted behind a temporary
    /// writer. This bounds only pending-event admission; reader timeouts stay
    /// at five seconds. Refusal remains an error the daemon must reconcile.
    const PENDING_ADMISSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    /// How many quarantined paths [`Store::status`] names in its degraded
    /// reason. Bounded because the reason is a one-line diagnostic, not a
    /// dump — the honest total stays in `quarantined_count`.
    pub const DEGRADED_SAMPLE: usize = 5;

    /// Names bound into one [`Store::callers_of`] statement.
    ///
    /// Each name is a bind parameter, and SQLite's `SQLITE_MAX_VARIABLE_NUMBER`
    /// is 32,766 in the bundled build — so a single generated file with more
    /// changed symbols than that made the statement unpreparable. 512 is far
    /// below that ceiling rather than adjacent to it, because the ceiling is a
    /// compile-time option of whatever SQLite the binary links, and a bound
    /// derived from it would be a bound this crate does not control.
    ///
    /// This bounds the *statement*, not the answer: `callers_of` walks every
    /// chunk and returns the union. A cap on the result would manufacture false
    /// "nothing depends on this" verdicts, which is the one thing this query
    /// must never do.
    pub const MAX_CALLER_BATCH: usize = 512;
}

#[cfg(test)]
#[cfg(feature = "parse")]
#[path = "db/tests/carry_forward_tests.rs"]
mod carry_forward_tests;

#[cfg(test)]
#[path = "db/tests/connection_tests.rs"]
mod connection_tests;

#[cfg(test)]
#[path = "db/tests/bounded_claim_tests.rs"]
mod bounded_claim_tests;

// Gated `unix` at the module as well as at its test: the positive control that
// ran everywhere moved to `tests/git_head_validates.rs` (it needs only the
// public API), and with the stalled-git test compiled out on Windows the
// file's `use super::*` would have nothing left to name.
#[cfg(all(test, unix))]
#[path = "db/tests/git_head_tests.rs"]
mod git_head_tests;

#[cfg(test)]
#[cfg(feature = "parse")]
#[path = "db/tests/delta_bucket_tests.rs"]
mod delta_bucket_tests;
