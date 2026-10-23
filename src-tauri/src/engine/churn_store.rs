//! Branch churn that outlives the process that measured it.
//!
//! `branch_stats` measures each branch with a `diff --shortstat` against the
//! default branch, up to 96 branches a call, and every launch used to measure
//! them all again: the memo was process memory. This keeps the answers in a
//! small SQLite file in the per-user cache directory.
//!
//! # When an answer may be reused
//!
//! Two commit ids do not determine the churn between them. Measured on git
//! 2.56 with one pair of commits: `diff.renames=false` turned 2 files
//! +21/-21 into 3 files +50/-50; an *uncommitted* `.gitattributes` or
//! `info/attributes` marking a file binary turned it into +1/-1; a replace
//! ref swapping the tip turned it into 1 file +1/-40. So:
//!
//! * **Attributes are pinned to the tip** with `--attr-source=<tip>`: a
//!   branch's churn is measured with the branch's own attributes. Worktree
//!   `.gitattributes` files, nested ones included, cannot be fingerprinted
//!   without walking the worktree on every call; pinned, they no longer
//!   matter. Before this, editing `.gitattributes` on one branch changed the
//!   churn shown for every other.
//! * **Everything else is in the key** ([`Inputs`]): the config that steers a
//!   diff, the replace refs, `shallow`, `info/grafts`, the attributes files
//!   outside the worktree, and the git version, since rename detection
//!   differs between releases.
//! * The inputs are read before and after a measurement, and an answer is
//!   kept only if they did not move in between.
//!
//! A git that rejects `--attr-source` as an unknown option is not pinned, and
//! its answers stay in process memory as they always did.
//!
//! # When a base that moved leaves an answer standing
//!
//! A branch's churn is a function of its tip and of the merge base between the
//! tip and the default branch, not of where the default branch is. A row is
//! therefore keyed by `(repository, inputs, tip)` and records the base it was
//! measured against. When the base has moved on to a descendant of that
//! recorded base and the branch's ahead count against the new base is the one
//! recorded, the branch's own commits are the same set (the ahead set can only
//! shrink as the base grows, so an equal count is an equal set), so the merge
//! base, the diff and the answer are the same: [`Kept::carried`] hands the
//! recorded measurement back with the behind count read fresh. Anything else (a
//! base that was rewound or replaced, a branch the base absorbed, counts git
//! could not give) is measured again. The caller proves the descent; this file
//! owns the rule for what follows from it.
//!
//! # What the file holds
//!
//! Commit ids, counts, and 64-bit hashes of the repository's location and of
//! the inputs: no path, ref name or file content. It is only written by the
//! app, which opts in at startup ([`persist_in_this_process`]); agent servers,
//! hooks and every test process keep to memory. Any failure to open or use
//! it falls back to memory for the rest of the process, logged once: a cache
//! must never be the reason branch stats fail.

use rusqlite::{params, Connection, OpenFlags};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use crate::engine::{git_cli, git_reader, ref_cache};
use crate::ledger::ids::fnv1a64;

/// One branch measured against a base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Churn {
    pub(crate) additions: usize,
    pub(crate) deletions: usize,
    pub(crate) files_changed: usize,
    pub(crate) commits_ahead: usize,
    pub(crate) commits_behind: usize,
}

pub(crate) const ZERO_CHURN: Churn = Churn {
    additions: 0,
    deletions: 0,
    files_changed: 0,
    commits_ahead: 0,
    commits_behind: 0,
};

/// The file is named for the schema, so two GitPulse builds of different
/// schema versions sharing one cache directory each keep their own file; with
/// one shared name, each start would drop and rebuild the other's table.
const FILE_NAME: &str = "churn.v2.sqlite";
/// Bumped whenever a row's meaning changes (and `FILE_NAME` with it); a file of
/// any other version is discarded and rebuilt empty (this is a cache of derived data, so losing it costs one
/// measurement per branch). Version 2 moved the base out of the key.
const SCHEMA_VERSION: i64 = 2;
/// Rows kept on disk: one per branch tip per repository and input state.
const PERSISTED_ROWS: usize = 50_000;
/// Rows kept in memory, the bound the process memo always had.
const MEMORY_ROWS: usize = 8_192;
/// How long a write waits for another GitPulse process holding the file.
const BUSY_TIMEOUT: Duration = Duration::from_millis(100);
/// How long an open waits for another process switching the file to WAL. The
/// first open of a session runs under the pinned store's initialiser, so this
/// is short; past it the session keeps churn in memory and the file is left
/// alone.
const WAL_PATIENCE: Duration = Duration::from_secs(2);
/// How long a git version probe is believed: a git upgraded under a running
/// app is noticed within this.
const PROBE_TTL: Duration = Duration::from_secs(60);
/// Largest attributes, grafts or shallow file folded into the key. Past it the
/// inputs are not fingerprinted and nothing is reused.
const MAX_INPUT_FILE_BYTES: u64 = 1024 * 1024;
/// Config that can change a `diff --shortstat` or the merge base it starts
/// from, besides every `diff.*` key. Over-inclusion costs a recomputation;
/// omission costs a wrong number. `core.attributesfile` is also covered by
/// the path and bytes of the file it names, so dropping it here changes no
/// key; it stays because the cost of listing it is nothing.
const DIFF_CONFIG_KEYS: [&str; 6] = [
    "core.attributesfile",
    "core.bigfilethreshold",
    "core.usereplacerefs",
    "merge.renamelimit",
    "attr.tree",
    "status.renamelimit",
];

static PERSIST: AtomicBool = AtomicBool::new(false);

/// Lets this process keep churn on disk. The app calls it once at startup;
/// nothing else does, so a test or an agent server never writes the user's
/// cache.
pub fn persist_in_this_process() {
    PERSIST.store(true, Ordering::Relaxed);
}

/// What one branch's churn is a function of besides its two commits, read
/// once per `branch_stats` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Inputs {
    repo: u64,
    /// `None` when some input could not be read: nothing is reused or kept.
    fingerprint: Option<u64>,
    /// Whether this git measures with the tip's attributes.
    pinned: bool,
}

impl Inputs {
    pub(crate) fn read(repo: &Path, base_oid: &str) -> Self {
        let probe = probe(repo, base_oid);
        let pinned = probe.as_ref().is_some_and(|p| p.attr_source);
        let (repo_id, fingerprint) = match crate::repository_trust::git_directories(repo) {
            Ok((private, common)) => (
                fnv1a64(common.to_string_lossy().as_bytes()),
                probe.and_then(|probe| fingerprint(repo, &private, &common, &probe)),
            ),
            Err(_) => (0, None),
        };
        Self {
            repo: repo_id,
            fingerprint,
            pinned,
        }
    }

    /// The global option that pins attributes to `tip`, when this git has it.
    pub(crate) fn pin(&self, tip: &str) -> Option<String> {
        self.pinned.then(|| format!("--attr-source={tip}"))
    }
}

/// A stored answer and the base it was measured against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Kept {
    /// The default branch's commit when `churn` was measured.
    pub(crate) base: String,
    pub(crate) churn: Churn,
}

impl Kept {
    /// This answer for a base that is a descendant of [`Self::base`] (the
    /// caller has established that) and gives the tip `now` = (ahead, behind).
    /// `None` when the ahead count moved: the base absorbed some of the
    /// branch's commits, so its merge base and diff may differ.
    pub(crate) fn carried(&self, now: (usize, usize)) -> Option<Churn> {
        (self.churn.commits_ahead == now.0).then_some(Churn {
            commits_ahead: now.0,
            commits_behind: now.1,
            ..self.churn
        })
    }
}

/// Answers already measured for `tips`, each with the base it was measured
/// against.
pub(crate) fn lookup(inputs: &Inputs, tips: &[String]) -> HashMap<String, Kept> {
    let Some(fingerprint) = inputs.fingerprint else {
        return HashMap::new();
    };
    with_store(inputs.pinned, |store| {
        store.lookup(inputs.repo, fingerprint, tips)
    })
    .unwrap_or_default()
}

/// Keeps `measured` against `base`, provided the inputs read now are the ones read before
/// measuring: a config or attributes change in between would otherwise be
/// stored under the key of the state before it.
pub(crate) fn keep(repo: &Path, before: &Inputs, base: &str, measured: &HashMap<String, Churn>) {
    let Some(fingerprint) = before.fingerprint else {
        return;
    };
    if measured.is_empty() {
        return;
    }
    #[cfg(test)]
    before_keep_hook();
    if Inputs::read(repo, base) != *before {
        return;
    }
    let _ = with_store(before.pinned, |store| {
        store.insert(before.repo, fingerprint, base, measured)
    });
}

/// Runs `body` on the store for pinned or unpinned answers. A store that
/// fails is replaced by memory for the rest of the process.
fn with_store<T>(
    pinned: bool,
    body: impl FnOnce(&mut Store) -> Result<T, rusqlite::Error>,
) -> Option<T> {
    #[cfg(test)]
    if pinned {
        if let Some(store) = TEST_STORE.with(|cell| cell.borrow().clone()) {
            let mut store = store.lock().unwrap_or_else(PoisonError::into_inner);
            return body(&mut store).ok();
        }
    }
    let slot = if pinned {
        pinned_store()
    } else {
        unpinned_store()
    };
    let mut store = slot.lock().unwrap_or_else(PoisonError::into_inner);
    match body(&mut store) {
        Ok(value) => Some(value),
        Err(error) if is_busy(&error) => None,
        Err(error) => {
            if store.persistent {
                log::warn!(
                    target: "churn",
                    "the branch churn cache failed ({error}); keeping churn in memory for this session"
                );
                *store = Store::memory(MEMORY_ROWS);
            }
            None
        }
    }
}

fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

fn pinned_store() -> &'static Mutex<Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(|| {
        let store = if PERSIST.load(Ordering::Relaxed) {
            match default_path()
                .map_err(OpenError::Failed)
                .and_then(|path| Store::open(&path, PERSISTED_ROWS))
            {
                Ok(store) => store,
                Err(reason) => {
                    log::warn!(
                        target: "churn",
                        "branch churn is kept in memory for this session: {reason}"
                    );
                    Store::memory(MEMORY_ROWS)
                }
            }
        } else {
            Store::memory(MEMORY_ROWS)
        };
        Mutex::new(store)
    })
}

fn unpinned_store() -> &'static Mutex<Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(Store::memory(MEMORY_ROWS)))
}

fn default_path() -> Result<PathBuf, String> {
    crate::tool_config::default_cache_dir()
        .map(|dir| dir.join(FILE_NAME))
        .ok_or_else(|| "no per-user cache directory (HOME / LOCALAPPDATA unset)".to_string())
}

/// Why [`Store::open`] failed. Neither kind has touched the file's contents.
#[derive(Debug)]
pub(crate) enum OpenError {
    /// Another connection held the file for longer than the open waits: the
    /// file is fine and in use, so it is kept, never recreated.
    Busy(String),
    Failed(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (Self::Busy(message) | Self::Failed(message)) = self;
        f.write_str(message)
    }
}

/// Why an open failed, and whether the file itself is to blame.
struct OpenFailure {
    message: String,
    /// The file is held by another connection (`SQLITE_BUSY`/`LOCKED`).
    busy: bool,
    /// The file is not a database, or is corrupt: only then is deleting it a
    /// repair rather than an attack on whoever else has it open.
    unreadable: bool,
}

impl OpenFailure {
    fn new(message: String, error: &rusqlite::Error) -> Self {
        let unreadable = matches!(
            error,
            rusqlite::Error::SqliteFailure(failure, _)
                if matches!(
                    failure.code,
                    rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt
                )
        );
        Self {
            message,
            busy: is_busy(error),
            unreadable,
        }
    }

    fn into_error(self) -> OpenError {
        if self.busy {
            OpenError::Busy(self.message)
        } else {
            OpenError::Failed(self.message)
        }
    }
}

pub(crate) struct Store {
    conn: Connection,
    cap: usize,
    persistent: bool,
}

impl Store {
    /// Opens the file at `path`, creating it. A file that is not a database,
    /// or is corrupt, is a cache, so it is discarded and created again. A
    /// schema from another version is rebuilt in place by `ensure_schema`.
    ///
    /// Anything else — above all `SQLITE_BUSY` from another process using the
    /// file, or a permission error — says nothing about the file's contents, so
    /// it fails this open and leaves the file alone for the process that has it.
    pub(crate) fn open(path: &Path, cap: usize) -> Result<Self, OpenError> {
        Self::open_within(path, cap, WAL_PATIENCE)
    }

    fn open_within(path: &Path, cap: usize, wal_patience: Duration) -> Result<Self, OpenError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| OpenError::Failed(format!("cannot create {}: {e}", dir.display())))?;
        }
        match Self::open_once(path, cap, wal_patience) {
            Ok(store) => Ok(store),
            Err(first) if !first.unreadable => Err(first.into_error()),
            Err(first) => {
                let first = first.message;
                for suffix in ["", "-wal", "-shm"] {
                    let mut doomed = path.as_os_str().to_owned();
                    doomed.push(suffix);
                    match std::fs::remove_file(&doomed) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => {
                            return Err(OpenError::Failed(format!(
                                "{first}; and it could not be replaced: {}: {e}",
                                PathBuf::from(&doomed).display()
                            )))
                        }
                    }
                }
                Self::open_once(path, cap, wal_patience).map_err(|second| {
                    OpenError::Failed(format!("{first}; recreated, and then {}", second.message))
                })
            }
        }
    }

    fn open_once(path: &Path, cap: usize, wal_patience: Duration) -> Result<Self, OpenFailure> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| OpenFailure::new(format!("cannot open {}: {e}", path.display()), &e))?;
        conn.busy_timeout(BUSY_TIMEOUT).map_err(|e| {
            OpenFailure::new(format!("cannot configure {}: {e}", path.display()), &e)
        })?;
        crate::sqlite_wal::enter_wal(&conn, wal_patience)
            .map_err(|e| OpenFailure::new(format!("{} is not usable: {e}", path.display()), &e))?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|e| OpenFailure::new(format!("{} is not usable: {e}", path.display()), &e))?;
        let mut store = Self {
            conn,
            cap,
            persistent: true,
        };
        store
            .ensure_schema()
            .map_err(|e| OpenFailure::new(format!("{} is not usable: {e}", path.display()), &e))?;
        Ok(store)
    }

    pub(crate) fn memory(cap: usize) -> Self {
        let conn = Connection::open_in_memory().expect("an in-memory database always opens");
        let mut store = Self {
            conn,
            cap,
            persistent: false,
        };
        store
            .ensure_schema()
            .expect("the schema applies to an empty in-memory database");
        store
    }

    fn ensure_schema(&mut self) -> Result<(), rusqlite::Error> {
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != SCHEMA_VERSION {
            // One writer rebuilds; a concurrent opener waits for it (the busy
            // handler) and then finds the version current. Without the
            // transaction two openers interleave DROP and CREATE.
            let tx = self
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let version: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            if version == SCHEMA_VERSION {
                return Ok(());
            }
            tx.execute_batch(
                "DROP TABLE IF EXISTS churn;
                 CREATE TABLE churn (
                     repo INTEGER NOT NULL,
                     inputs INTEGER NOT NULL,
                     tip TEXT NOT NULL,
                     base TEXT NOT NULL,
                     additions INTEGER NOT NULL,
                     deletions INTEGER NOT NULL,
                     files INTEGER NOT NULL,
                     ahead INTEGER NOT NULL,
                     behind INTEGER NOT NULL,
                     stored INTEGER NOT NULL,
                     PRIMARY KEY (repo, inputs, tip)
                 ) WITHOUT ROWID;
                 CREATE INDEX churn_stored ON churn (stored);",
            )?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
        }
        Ok(())
    }

    fn lookup(
        &mut self,
        repo: u64,
        inputs: u64,
        tips: &[String],
    ) -> Result<HashMap<String, Kept>, rusqlite::Error> {
        let mut found = HashMap::new();
        let mut query = self.conn.prepare_cached(
            "SELECT base, additions, deletions, files, ahead, behind FROM churn
             WHERE repo = ?1 AND inputs = ?2 AND tip = ?3",
        )?;
        for tip in tips {
            let row = query.query_row(params![repo as i64, inputs as i64, tip], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    [
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ],
                ))
            });
            let (base, counts) = match row {
                Ok(found) => found,
                Err(rusqlite::Error::QueryReturnedNoRows) => continue,
                Err(error) => return Err(error),
            };
            // A count no measurement could produce is a damaged row: a miss.
            let parsed: Option<Vec<usize>> =
                counts.iter().map(|&n| usize::try_from(n).ok()).collect();
            let Some([additions, deletions, files_changed, commits_ahead, commits_behind]) =
                parsed.and_then(|v| <[usize; 5]>::try_from(v).ok())
            else {
                continue;
            };
            found.insert(
                tip.clone(),
                Kept {
                    base,
                    churn: Churn {
                        additions,
                        deletions,
                        files_changed,
                        commits_ahead,
                        commits_behind,
                    },
                },
            );
        }
        Ok(found)
    }

    fn insert(
        &mut self,
        repo: u64,
        inputs: u64,
        base: &str,
        measured: &HashMap<String, Churn>,
    ) -> Result<(), rusqlite::Error> {
        let tx = self.conn.transaction()?;
        let mut stored: i64 =
            tx.query_row("SELECT COALESCE(MAX(stored), 0) FROM churn", [], |row| {
                row.get(0)
            })?;
        {
            let mut put = tx.prepare_cached(
                "INSERT OR REPLACE INTO churn
                 (repo, inputs, tip, base, additions, deletions, files, ahead, behind, stored)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;
            let mut rows: Vec<(&String, &Churn)> = measured.iter().collect();
            rows.sort_by(|a, b| a.0.cmp(b.0));
            for (tip, churn) in rows {
                stored += 1;
                put.execute(params![
                    repo as i64,
                    inputs as i64,
                    tip,
                    base,
                    count(churn.additions),
                    count(churn.deletions),
                    count(churn.files_changed),
                    count(churn.commits_ahead),
                    count(churn.commits_behind),
                    stored,
                ])?;
            }
        }
        let rows: i64 = tx.query_row("SELECT COUNT(*) FROM churn", [], |row| row.get(0))?;
        let over = rows - i64::try_from(self.cap).unwrap_or(i64::MAX);
        if over > 0 {
            tx.execute(
                "DELETE FROM churn WHERE (repo, inputs, tip) IN
                 (SELECT repo, inputs, tip FROM churn ORDER BY stored LIMIT ?1)",
                params![over],
            )?;
        }
        tx.commit()
    }

    #[cfg(test)]
    fn rows(&self) -> i64 {
        self.conn
            .query_row("SELECT COUNT(*) FROM churn", [], |row| row.get(0))
            .unwrap()
    }
}

/// A count as SQLite stores it. No measurement reaches `i64::MAX`; one that
/// claimed to is stored as a value [`Store::lookup`] reads back as damaged.
fn count(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(-1)
}

/// The git version, and whether it takes `--attr-source`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Probe {
    version: String,
    attr_source: bool,
}

/// Asked once per [`PROBE_TTL`] per process. A probe that could not run
/// (deferred under load, for one) is not remembered.
fn probe(repo: &Path, base_oid: &str) -> Option<Probe> {
    #[cfg(test)]
    if let Some(forced) = PROBE_OVERRIDE.with(|cell| cell.borrow().clone()) {
        return forced;
    }
    static LAST: Mutex<Option<(Instant, Probe)>> = Mutex::new(None);
    if let Some((at, probe)) = LAST.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
        if at.elapsed() < PROBE_TTL {
            return Some(probe.clone());
        }
    }
    let pin = format!("--attr-source={base_oid}");
    let probe = classify_probe(git_cli::git_text_shared(repo, &[&pin, "version"]), || {
        git_cli::git_text_shared(repo, &["version"])
    })?;
    *LAST.lock().unwrap_or_else(PoisonError::into_inner) = Some((Instant::now(), probe.clone()));
    Some(probe)
}

/// Reads a `git --attr-source=<oid> version` run. Only git naming the option
/// unknown means it lacks it; any other failure says nothing either way.
fn classify_probe(
    flagged: Result<String, String>,
    plain: impl FnOnce() -> Result<String, String>,
) -> Option<Probe> {
    let version = |text: &str| {
        let line = text.lines().next().unwrap_or("").trim();
        (!line.is_empty()).then(|| line.to_string())
    };
    match flagged {
        Ok(text) => Some(Probe {
            version: version(&text)?,
            attr_source: true,
        }),
        Err(error) if error.contains("unknown option: --attr-source") => Some(Probe {
            version: version(&plain().ok()?)?,
            attr_source: false,
        }),
        Err(_) => None,
    }
}

/// Hashes every input named in the module docs. `None` when one cannot be
/// read whole.
fn fingerprint(repo: &Path, private: &Path, common: &Path, probe: &Probe) -> Option<u64> {
    let mut canon = Vec::new();
    let mut field = |name: &str, bytes: &[u8]| {
        canon.extend_from_slice(name.as_bytes());
        canon.push(0);
        canon.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        canon.extend_from_slice(bytes);
    };
    field("git", probe.version.as_bytes());
    field("pinned", &[u8::from(probe.attr_source)]);

    let config = git_reader::config_snapshot(repo).ok()?;
    let steering: BTreeMap<&String, &String> = config
        .iter()
        .filter(|(key, _)| key.starts_with("diff.") || DIFF_CONFIG_KEYS.contains(&key.as_str()))
        .collect();
    for (key, value) in &steering {
        field(key, value.as_bytes());
    }

    let replace = ref_cache::git_text(
        repo,
        &["refs/replace"],
        &[
            "for-each-ref",
            "--format=%(refname)%00%(objectname)",
            "refs/replace/",
        ],
    )
    .ok()?;
    field("replace", replace.as_bytes());

    let mut files = vec![
        common.join("shallow"),
        common.join("info").join("grafts"),
        common.join("info").join("attributes"),
    ];
    if private != common {
        files.push(private.join("info").join("attributes"));
    }
    files.push(global_attributes(
        config.get("core.attributesfile").map(String::as_str),
    )?);
    for path in files {
        field(&path.to_string_lossy(), &file_input(&path)?);
    }
    Some(fnv1a64(&canon))
}

/// The attributes file outside the repository git reads: `core.attributesFile`
/// when set, else the XDG default.
fn global_attributes(configured: Option<&str>) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(value) = configured.map(str::trim).filter(|v| !v.is_empty()) {
        return Some(match value.strip_prefix("~/") {
            Some(rest) => home?.join(rest),
            None => PathBuf::from(value),
        });
    }
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|h| h.join(".config")))?;
    Some(xdg.join("git").join("attributes"))
}

/// A file's bytes, with absence as its own value. Too large or unreadable is
/// `None`: an input that cannot be read is not one that is known not to
/// matter.
fn file_input(path: &Path) -> Option<Vec<u8>> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > MAX_INPUT_FILE_BYTES => None,
        Ok(_) => std::fs::read(path).ok().map(|mut bytes| {
            bytes.insert(0, 1);
            bytes
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(vec![0]),
        Err(_) => None,
    }
}

#[cfg(test)]
thread_local! {
    static TEST_STORE: std::cell::RefCell<Option<std::sync::Arc<Mutex<Store>>>> =
        const { std::cell::RefCell::new(None) };
    static PROBE_OVERRIDE: std::cell::RefCell<Option<Option<Probe>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_KEEP: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn before_keep_hook() {
    BEFORE_KEEP.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::git_cli::spawn_log;
    use crate::engine::git_reader::{BranchStatsReport, GitReader};
    use crate::test_support::{git_in, trust_repo};
    use std::sync::Arc;

    /// A repository whose one branch's churn moves with every input the key
    /// covers: a rotated file (attributes), a rename (rename config).
    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        // Not `canonicalize`: its `\\?\` spelling is not one git resolves
        // `core.attributesFile` and the other paths below through.
        let repo = git_cli::canonicalize_plain(dir.path()).unwrap();
        git_in(&repo, &["init", "-q", "-b", "main"]);
        git_in(&repo, &["config", "user.email", "t@example.invalid"]);
        git_in(&repo, &["config", "user.name", "t"]);
        let lines: Vec<String> = (0..40).map(|i| format!("line {i}\n")).collect();
        std::fs::write(repo.join("a.txt"), lines.concat()).unwrap();
        std::fs::write(repo.join("keep.txt"), "x\n".repeat(30)).unwrap();
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "-qm", "base"]);
        git_in(&repo, &["checkout", "-qb", "topic"]);
        let rotated = [&lines[20..], &lines[..20]].concat().concat();
        std::fs::write(repo.join("a.txt"), rotated).unwrap();
        git_in(&repo, &["mv", "keep.txt", "kept.txt"]);
        std::fs::write(repo.join("kept.txt"), format!("{}y\n", "x\n".repeat(29))).unwrap();
        git_in(&repo, &["commit", "-qam", "topic"]);
        git_in(&repo, &["checkout", "-q", "main"]);
        trust_repo(&repo);
        (dir, repo)
    }

    fn rev(repo: &Path, name: &str) -> String {
        git_cli::git_text(repo, &["rev-parse", name])
            .unwrap()
            .trim()
            .to_string()
    }

    /// `diff --shortstat` processes run in `repo` so far.
    fn diffs(repo: &Path) -> usize {
        spawn_log::spawns_in(repo)
            .iter()
            .filter(|argv| argv.iter().any(|a| a == "--shortstat"))
            .count()
    }

    fn stats(repo: &Path) -> (BranchStatsReport, Churn) {
        let report = GitReader::branch_stats(repo.to_str().unwrap()).expect("branch stats");
        let topic = report
            .updates
            .iter()
            .find(|u| u.name == "topic")
            .expect("topic measured");
        let churn = Churn {
            additions: topic.additions,
            deletions: topic.deletions,
            files_changed: topic.files_changed,
            commits_ahead: topic.commits_ahead_of_base,
            commits_behind: topic.commits_behind_base,
        };
        (report, churn)
    }

    /// Makes this thread's pinned answers go to `store` until dropped.
    struct UseStore;
    impl UseStore {
        fn file(path: &Path) -> (Self, Arc<Mutex<Store>>) {
            let store = Arc::new(Mutex::new(Store::open(path, PERSISTED_ROWS).unwrap()));
            TEST_STORE.with(|cell| *cell.borrow_mut() = Some(Arc::clone(&store)));
            (Self, store)
        }
    }
    impl Drop for UseStore {
        fn drop(&mut self) {
            TEST_STORE.with(|cell| *cell.borrow_mut() = None);
        }
    }

    /// Forces this thread's git probe until dropped.
    struct ForceProbe;
    impl ForceProbe {
        fn set(probe: Option<Probe>) -> Self {
            PROBE_OVERRIDE.with(|cell| *cell.borrow_mut() = Some(probe));
            Self
        }
    }
    impl Drop for ForceProbe {
        fn drop(&mut self) {
            PROBE_OVERRIDE.with(|cell| *cell.borrow_mut() = None);
        }
    }

    /// What the cache exists for: a measurement made before a restart is not
    /// made again after it.
    #[test]
    fn churn_survives_a_restart() {
        let (_dir, repo) = fixture();
        let cache = tempfile::tempdir().unwrap();
        let path = cache.path().join(FILE_NAME);
        let before = diffs(&repo);
        let first = {
            let (_use, _store) = UseStore::file(&path);
            stats(&repo)
        };
        assert_eq!(diffs(&repo) - before, 1);
        assert_eq!(first.0.computed, 1);

        // A new connection to the same file: the next launch.
        let (_use, store) = UseStore::file(&path);
        let second = stats(&repo);
        assert_eq!(diffs(&repo) - before, 1, "measured again after a restart");
        assert_eq!(second.0.cached, 1);
        assert_eq!(second.1, first.1);
        assert_eq!(store.lock().unwrap().rows(), 1);
    }

    /// Each input the key covers, changed alone, forces a new measurement;
    /// put back, the earlier answer is found again.
    #[test]
    fn every_input_that_steers_the_diff_is_in_the_key() {
        let (_dir, repo) = fixture();
        let cache = tempfile::tempdir().unwrap();
        let (_use, _store) = UseStore::file(&cache.path().join(FILE_NAME));
        let measured = |label: &str, expect_new: bool| {
            let before = diffs(&repo);
            let (_, churn) = stats(&repo);
            assert_eq!(
                diffs(&repo) - before,
                usize::from(expect_new),
                "{label}: {}",
                if expect_new {
                    "not measured again"
                } else {
                    "measured again"
                }
            );
            churn
        };
        let baseline = measured("first", true);
        assert_eq!(measured("repeat", false), baseline);

        git_in(&repo, &["config", "diff.renames", "false"]);
        let unrenamed = measured("diff.renames", true);
        assert_ne!(unrenamed, baseline, "the premise: renames change the count");
        git_in(&repo, &["config", "--unset", "diff.renames"]);
        assert_eq!(measured("diff.renames restored", false), baseline);

        let info = repo.join(".git/info");
        std::fs::create_dir_all(&info).unwrap();
        std::fs::write(info.join("attributes"), "a.txt binary\n").unwrap();
        assert_ne!(measured("info/attributes", true), baseline);
        std::fs::remove_file(info.join("attributes")).unwrap();
        assert_eq!(measured("info/attributes removed", false), baseline);

        let tip = rev(&repo, "topic");
        let base = rev(&repo, "main");
        // Another commit on the base stands in for the tip; both oids stay.
        git_in(&repo, &["checkout", "-qb", "other"]);
        std::fs::write(repo.join("a.txt"), "other\n").unwrap();
        git_in(&repo, &["commit", "-qam", "other"]);
        let other = rev(&repo, "other");
        git_in(&repo, &["checkout", "-q", "main"]);
        git_in(&repo, &["branch", "-qD", "other"]);
        git_in(&repo, &["replace", &tip, &other]);
        assert_ne!(measured("replace ref", true), baseline);
        git_in(&repo, &["replace", "-d", &tip]);
        assert_eq!(measured("replace ref removed", false), baseline);

        std::fs::write(info.join("grafts"), format!("{tip} {base}\n")).unwrap();
        measured("info/grafts", true);
        std::fs::remove_file(info.join("grafts")).unwrap();
        measured("info/grafts removed", false);

        std::fs::write(repo.join(".git/shallow"), format!("{base}\n")).unwrap();
        measured("shallow", true);
        std::fs::remove_file(repo.join(".git/shallow")).unwrap();
        measured("shallow removed", false);

        let global = repo.join(".git").join("global-attributes");
        std::fs::write(&global, "").unwrap();
        git_in(
            &repo,
            &["config", "core.attributesFile", global.to_str().unwrap()],
        );
        measured("core.attributesFile set", true);
        std::fs::write(&global, "a.txt binary\n").unwrap();
        assert_ne!(measured("attributes file content", true), baseline);
        git_in(&repo, &["config", "--unset", "core.attributesFile"]);
        assert_eq!(measured("core.attributesFile unset", false), baseline);

        {
            let _upgraded = ForceProbe::set(Some(Probe {
                version: "git version 99.0.0".into(),
                attr_source: true,
            }));
            measured("another git version", true);
        }
        assert_eq!(measured("the git version back", false), baseline);
    }

    /// The pin: a worktree `.gitattributes` the branch does not carry neither
    /// changes the branch's churn nor costs a measurement; one the branch
    /// commits does change it.
    #[test]
    fn churn_is_measured_with_the_tips_own_attributes() {
        let (_dir, repo) = fixture();
        let cache = tempfile::tempdir().unwrap();
        let (_use, _store) = UseStore::file(&cache.path().join(FILE_NAME));
        let (_, baseline) = stats(&repo);

        std::fs::write(repo.join(".gitattributes"), "a.txt -diff\n").unwrap();
        let before = diffs(&repo);
        assert_eq!(stats(&repo).1, baseline);
        assert_eq!(
            diffs(&repo),
            before,
            "an uncommitted worktree file is not an input"
        );
        drop(_use);

        // Measured afresh with that file present: still the branch's number.
        let fresh = tempfile::tempdir().unwrap();
        let (_use, _store) = UseStore::file(&fresh.path().join(FILE_NAME));
        assert_eq!(stats(&repo).1, baseline);
        std::fs::remove_file(repo.join(".gitattributes")).unwrap();

        git_in(&repo, &["checkout", "-q", "topic"]);
        std::fs::write(repo.join(".gitattributes"), "a.txt -diff\n").unwrap();
        git_in(&repo, &["add", ".gitattributes"]);
        git_in(&repo, &["commit", "-qm", "attributes"]);
        git_in(&repo, &["checkout", "-q", "main"]);
        let (_, committed) = stats(&repo);
        assert!(
            committed.additions < baseline.additions,
            "the branch's own attributes apply: {committed:?} vs {baseline:?}"
        );
    }

    /// The number shown is what git itself answers for the pinned diff.
    #[test]
    fn a_kept_answer_is_what_git_measures() {
        let (_dir, repo) = fixture();
        let cache = tempfile::tempdir().unwrap();
        let (_use, _store) = UseStore::file(&cache.path().join(FILE_NAME));
        let (base, tip) = (rev(&repo, "main"), rev(&repo, "topic"));
        let pin = format!("--attr-source={tip}");
        let spec = format!("{base}...{tip}");
        stats(&repo);
        assert!(
            spawn_log::spawns_in(&repo)
                .iter()
                .any(|argv| argv.contains(&pin) && argv.iter().any(|a| a == "--shortstat")),
            "the diff was not pinned to the tip"
        );
        let (_, kept) = stats(&repo);
        let direct = git_cli::git_text(&repo, &[&pin, "diff", "--shortstat", &spec]).unwrap();
        let parsed = crate::analyzer::DiffChurn::parse_shortstat(&direct);
        assert_eq!(
            (kept.additions, kept.deletions, kept.files_changed),
            (parsed.additions, parsed.deletions, parsed.files_changed)
        );
        assert_eq!((kept.commits_ahead, kept.commits_behind), (1, 0));
    }

    /// Inputs that move while a branch is measured leave nothing kept: the
    /// answer may describe either state.
    #[test]
    fn an_answer_measured_while_its_inputs_moved_is_not_kept() {
        let (_dir, repo) = fixture();
        let cache = tempfile::tempdir().unwrap();
        let (_use, store) = UseStore::file(&cache.path().join(FILE_NAME));
        let moving = repo.clone();
        BEFORE_KEEP.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                git_in(&moving, &["config", "diff.renames", "false"]);
            }))
        });
        stats(&repo);
        BEFORE_KEEP.with(|hook| *hook.borrow_mut() = None);
        assert_eq!(store.lock().unwrap().rows(), 0);
    }

    /// A git without `--attr-source` measures as GitPulse always did, and
    /// nothing it measures reaches the file.
    #[test]
    fn an_older_git_measures_unpinned_and_keeps_nothing_on_disk() {
        let (_dir, repo) = fixture();
        let cache = tempfile::tempdir().unwrap();
        let (_use, store) = UseStore::file(&cache.path().join(FILE_NAME));
        let _old = ForceProbe::set(Some(Probe {
            version: "git version 2.39.5".into(),
            attr_source: false,
        }));
        let before = diffs(&repo);
        stats(&repo);
        assert!(spawn_log::spawns_in(&repo)
            .iter()
            .filter(|argv| argv.iter().any(|a| a == "--shortstat"))
            .all(|argv| !argv.iter().any(|a| a.starts_with("--attr-source"))));
        assert_eq!(
            store.lock().unwrap().rows(),
            0,
            "an unpinned answer was persisted"
        );
        // Still remembered for this process, as before.
        stats(&repo);
        assert_eq!(diffs(&repo) - before, 1);
    }

    /// An input that cannot be read whole is not one known not to matter:
    /// nothing is reused or kept while it stands.
    #[test]
    fn an_unreadable_input_disables_reuse() {
        let (_dir, repo) = fixture();
        let cache = tempfile::tempdir().unwrap();
        let (_use, store) = UseStore::file(&cache.path().join(FILE_NAME));
        let info = repo.join(".git/info");
        std::fs::create_dir_all(&info).unwrap();
        let huge = vec![b'#'; (MAX_INPUT_FILE_BYTES + 1) as usize];
        std::fs::write(info.join("attributes"), huge).unwrap();
        let before = diffs(&repo);
        stats(&repo);
        stats(&repo);
        assert_eq!(diffs(&repo) - before, 2);
        assert_eq!(store.lock().unwrap().rows(), 0);
    }

    /// The probe's verdict rests on git's own wording for an option it lacks,
    /// so that wording is checked against the git on this host.
    #[test]
    fn the_probe_reads_a_missing_option_from_gits_own_error() {
        let (_dir, repo) = fixture();
        let unknown =
            git_cli::git_text_shared(&repo, &["--attr-source-from-the-future=x", "version"])
                .unwrap_err();
        assert!(
            unknown.contains("unknown option: --attr-source-from-the-future"),
            "{unknown}"
        );
        let old = unknown.replace("--attr-source-from-the-future=x", "--attr-source=abc");
        assert_eq!(
            classify_probe(Err(old), || Ok("git version 2.39.5\n".into())),
            Some(Probe {
                version: "git version 2.39.5".into(),
                attr_source: false
            })
        );
        assert_eq!(
            classify_probe(Ok("git version 2.56.0\n".into()), || unreachable!()),
            Some(Probe {
                version: "git version 2.56.0".into(),
                attr_source: true
            })
        );
        // A probe that did not run says nothing: no verdict, nothing reused.
        let deferred = "git version deferred under load after 2.000s: …".to_string();
        assert_eq!(classify_probe(Err(deferred), || unreachable!()), None);
        assert_eq!(classify_probe(Ok(String::new()), || unreachable!()), None);
        assert_eq!(
            classify_probe(Err("unknown option: --attr-source=x".into()), || Err(
                "x".into()
            )),
            None
        );
    }

    /// One more commit on `main`, which `topic` has nothing to do with.
    fn advance(repo: &Path, label: &str) {
        git_in(repo, &["commit", "-q", "--allow-empty", "-m", label]);
    }

    /// Processes run in `repo` so far whose argv names `needle`.
    fn spawns_named(repo: &Path, needle: &str) -> usize {
        spawn_log::spawns_in(repo)
            .iter()
            .filter(|argv| argv.iter().any(|a| a == needle))
            .count()
    }

    /// The point of keying by tip: the default branch moving on for a reason
    /// that has nothing to do with a branch leaves its measurement standing.
    /// Ahead and behind are still read fresh (behind moved), nothing is
    /// measured, and at most one process asks whether the old base lies behind
    /// the new one.
    #[test]
    fn an_unrelated_base_advance_does_not_measure_the_branch_again() {
        let (_dir, repo) = fixture();
        let cache = tempfile::tempdir().unwrap();
        let (_use, store) = UseStore::file(&cache.path().join(FILE_NAME));
        let (first_report, first) = stats(&repo);
        assert_eq!(first_report.computed, 1);
        assert_eq!(first.commits_behind, 0);

        let diffs_before = diffs(&repo);
        let ancestry_before = spawns_named(&repo, "--is-ancestor");
        advance(&repo, "unrelated");
        advance(&repo, "unrelated again");
        let (report, second) = stats(&repo);
        assert_eq!(
            diffs(&repo) - diffs_before,
            0,
            "an unrelated advance was measured"
        );
        assert_eq!(report.computed, 0);
        assert_eq!(report.cached, 1);
        assert_eq!(
            (second.additions, second.deletions, second.files_changed),
            (first.additions, first.deletions, first.files_changed)
        );
        assert_eq!(second.commits_ahead, first.commits_ahead);
        assert_eq!(second.commits_behind, 2, "behind is read fresh");
        assert!(spawns_named(&repo, "--is-ancestor") - ancestry_before <= 1);
        assert_eq!(
            store.lock().unwrap().rows(),
            1,
            "the row is replaced in place"
        );

        // The answer was kept against the new base: the next call neither
        // measures nor asks about ancestry.
        let (diffs_before, ancestry_before) = (diffs(&repo), spawns_named(&repo, "--is-ancestor"));
        let (_, third) = stats(&repo);
        assert_eq!(third, second);
        assert_eq!(diffs(&repo) - diffs_before, 0);
        assert_eq!(spawns_named(&repo, "--is-ancestor") - ancestry_before, 0);
    }

    /// An advance does not hide a change to the inputs: the carried answer is
    /// only found under the inputs it was measured with.
    #[test]
    fn a_base_advance_does_not_carry_an_answer_across_a_changed_input() {
        let (_dir, repo) = fixture();
        let cache = tempfile::tempdir().unwrap();
        let (_use, _store) = UseStore::file(&cache.path().join(FILE_NAME));
        let (_, baseline) = stats(&repo);
        advance(&repo, "unrelated");
        git_in(&repo, &["config", "diff.renames", "false"]);
        let before = diffs(&repo);
        let (_, unrenamed) = stats(&repo);
        assert_eq!(diffs(&repo) - before, 1);
        assert_ne!(unrenamed.files_changed, baseline.files_changed);
    }

    /// A default branch put back (a force-push) is not a descendant of the
    /// base the answer was measured against: measured again.
    #[test]
    fn a_base_that_moved_backwards_measures_the_branch_again() {
        let (_dir, repo) = fixture();
        let cache = tempfile::tempdir().unwrap();
        let (_use, _store) = UseStore::file(&cache.path().join(FILE_NAME));
        advance(&repo, "to be rewound");
        let (_, first) = stats(&repo);
        git_in(&repo, &["reset", "-q", "--hard", "HEAD~1"]);
        let before = diffs(&repo);
        let (report, second) = stats(&repo);
        assert_eq!(diffs(&repo) - before, 1);
        assert_eq!(report.computed, 1);
        assert_eq!(second.commits_behind, 0);
        assert_eq!(
            (second.additions, second.deletions, second.files_changed),
            (first.additions, first.deletions, first.files_changed)
        );
    }

    /// A branch the base absorbed has a smaller ahead set, so its merge base
    /// moved: measured again, and it reads as merged.
    #[test]
    fn a_branch_merged_into_the_base_is_measured_again() {
        let (_dir, repo) = fixture();
        let cache = tempfile::tempdir().unwrap();
        let (_use, _store) = UseStore::file(&cache.path().join(FILE_NAME));
        stats(&repo);
        git_in(
            &repo,
            &["merge", "-q", "--no-ff", "-m", "merge topic", "topic"],
        );
        let before = diffs(&repo);
        let (_, merged) = stats(&repo);
        assert_eq!(diffs(&repo) - before, 1);
        // Behind by the merge commit itself.
        assert_eq!((merged.commits_ahead, merged.commits_behind), (0, 1));
        assert_eq!(
            (merged.additions, merged.deletions, merged.files_changed),
            (0, 0, 0)
        );
    }

    /// Part of a branch absorbed (its first commit merged, a second kept):
    /// the ahead count moved, so it is measured again, against the merge base
    /// that moved with it.
    #[test]
    fn a_branch_partly_absorbed_by_the_base_is_measured_again() {
        let (_dir, repo) = fixture();
        let cache = tempfile::tempdir().unwrap();
        let (_use, _store) = UseStore::file(&cache.path().join(FILE_NAME));
        git_in(&repo, &["checkout", "-q", "topic"]);
        std::fs::write(repo.join("second.txt"), "s\n").unwrap();
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "-qm", "second"]);
        git_in(&repo, &["checkout", "-q", "main"]);
        let (_, before_merge) = stats(&repo);
        assert_eq!(before_merge.commits_ahead, 2);
        git_in(&repo, &["merge", "-q", "--ff-only", "topic~1"]);
        let before = diffs(&repo);
        let (_, after) = stats(&repo);
        assert_eq!(diffs(&repo) - before, 1);
        assert_eq!(after.commits_ahead, 1);
        assert_eq!(after.files_changed, 1, "only the second commit is left");
    }

    /// A rebase puts the branch on a new tip: a new key, measured.
    #[test]
    fn a_rebased_branch_is_measured_again() {
        let (_dir, repo) = fixture();
        let cache = tempfile::tempdir().unwrap();
        let (_use, _store) = UseStore::file(&cache.path().join(FILE_NAME));
        stats(&repo);
        std::fs::write(repo.join("other.txt"), "o\n").unwrap();
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "-qm", "main moves"]);
        git_in(&repo, &["rebase", "-q", "main", "topic"]);
        git_in(&repo, &["checkout", "-q", "main"]);
        let before = diffs(&repo);
        let (report, rebased) = stats(&repo);
        assert_eq!(diffs(&repo) - before, 1);
        assert_eq!(report.computed, 1);
        assert_eq!((rebased.commits_ahead, rebased.commits_behind), (1, 0));
    }

    /// Two branches on one tip are one measurement before and after an
    /// advance.
    #[test]
    fn branches_sharing_a_tip_are_carried_together() {
        let (_dir, repo) = fixture();
        let cache = tempfile::tempdir().unwrap();
        let (_use, _store) = UseStore::file(&cache.path().join(FILE_NAME));
        git_in(&repo, &["branch", "twin", "topic"]);
        let report = GitReader::branch_stats(repo.to_str().unwrap()).unwrap();
        assert_eq!(report.updates.len(), 2);
        let before = diffs(&repo);
        advance(&repo, "unrelated");
        let report = GitReader::branch_stats(repo.to_str().unwrap()).unwrap();
        assert_eq!(
            (report.updates.len(), report.cached, report.computed),
            (2, 2, 0)
        );
        assert_eq!(diffs(&repo) - before, 0);
        assert!(report.updates.iter().all(|u| u.commits_behind_base == 1));
    }

    /// Past the per-call cap: the branches already measured are carried over
    /// an advance without a process each, however many there are.
    #[test]
    fn a_base_advance_carries_every_measured_branch_past_the_call_cap() {
        let (_dir, repo) = fixture();
        let cache = tempfile::tempdir().unwrap();
        let (_use, _store) = UseStore::file(&cache.path().join(FILE_NAME));
        let tree = rev(&repo, "main^{tree}");
        let main = rev(&repo, "main");
        for i in 0..100 {
            let commit = git_cli::git_text(
                &repo,
                &["commit-tree", &tree, "-p", &main, "-m", &format!("b{i}")],
            )
            .unwrap();
            git_in(&repo, &["branch", &format!("many-{i:03}"), commit.trim()]);
        }
        let path = repo.to_str().unwrap();
        let mut drained = GitReader::branch_stats(path).unwrap();
        for _ in 0..5 {
            if !drained.capped {
                break;
            }
            drained = GitReader::branch_stats(path).unwrap();
        }
        assert!(!drained.capped, "the 101 branches were not all measured");
        assert_eq!(drained.updates.len(), 101);

        let before = diffs(&repo);
        advance(&repo, "unrelated");
        let after = GitReader::branch_stats(path).unwrap();
        assert_eq!(
            diffs(&repo) - before,
            0,
            "a measured branch was measured again"
        );
        assert_eq!(
            (after.updates.len(), after.computed, after.capped),
            (101, 0, false)
        );
        assert!(after.updates.iter().all(|u| u.commits_behind_base == 1));
    }

    /// A cache written before the base left the key is rebuilt empty, not
    /// read with the old key's meaning.
    #[test]
    fn a_cache_from_before_the_base_left_the_key_is_rebuilt() {
        let cache = tempfile::tempdir().unwrap();
        let path = cache.path().join(FILE_NAME);
        let old = Connection::open(&path).unwrap();
        old.execute_batch(
            "CREATE TABLE churn (
                 repo INTEGER NOT NULL, inputs INTEGER NOT NULL,
                 base TEXT NOT NULL, tip TEXT NOT NULL,
                 additions INTEGER NOT NULL, deletions INTEGER NOT NULL,
                 files INTEGER NOT NULL, ahead INTEGER NOT NULL,
                 behind INTEGER NOT NULL, stored INTEGER NOT NULL,
                 PRIMARY KEY (repo, inputs, base, tip)
             ) WITHOUT ROWID;
             INSERT INTO churn VALUES (1, 2, 'old-base', 'a', 3, 4, 5, 1, 0, 1);
             PRAGMA user_version = 1;",
        )
        .unwrap();
        drop(old);

        let mut store = Store::open(&path, 10).unwrap();
        assert_eq!(store.rows(), 0, "an old row was kept");
        assert!(store.lookup(1, 2, &["a".to_string()]).unwrap().is_empty());
        store.insert(1, 2, "new-base", &one("a", 7)).unwrap();
        let found = store.lookup(1, 2, &["a".to_string()]).unwrap();
        assert_eq!(found["a"].base, "new-base");
    }

    /// What the build before the base left the key does on open: the oracle for
    /// a v1 binary sharing the cache directory.
    fn v1_binary_opens(dir: &Path) {
        let conn = Connection::open(dir.join("churn.v1.sqlite")).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        if version != 1 {
            conn.execute_batch(
                "DROP TABLE IF EXISTS churn;
                 CREATE TABLE churn (
                     repo INTEGER NOT NULL, inputs INTEGER NOT NULL,
                     base TEXT NOT NULL, tip TEXT NOT NULL,
                     additions INTEGER NOT NULL, deletions INTEGER NOT NULL,
                     files INTEGER NOT NULL, ahead INTEGER NOT NULL,
                     behind INTEGER NOT NULL, stored INTEGER NOT NULL,
                     PRIMARY KEY (repo, inputs, base, tip)
                 ) WITHOUT ROWID;",
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
        }
    }

    /// An older and a newer build sharing one cache directory must not drop
    /// each other's table on every start.
    #[test]
    fn builds_of_different_schema_versions_do_not_rebuild_each_others_cache() {
        let cache = tempfile::tempdir().unwrap();
        for round in 0..3 {
            v1_binary_opens(cache.path());
            let mut store = Store::open(&cache.path().join(FILE_NAME), 10).unwrap();
            if round > 0 {
                let found = store.lookup(1, 2, &["a".to_string()]).unwrap();
                assert_eq!(
                    found["a"].base, "base",
                    "round {round}: the older build's start emptied the newer cache"
                );
            }
            store.insert(1, 2, "base", &one("a", 7)).unwrap();
        }
        assert_ne!(FILE_NAME, "churn.v1.sqlite");
    }

    /// A git process in `repo` for the random-history oracle; `false` when it
    /// refused (a conflicting merge, an empty cherry-pick), which the caller
    /// then backs out of.
    fn try_git(repo: &Path, args: &[&str]) -> bool {
        use crate::procguard::LockedSpawn;
        let mut command = std::process::Command::new("git");
        command
            .current_dir(repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let child = command.spawn_locked().expect("spawn git");
        child
            .wait_with_output()
            .expect("wait for git")
            .status
            .success()
    }

    /// What `git` says a branch's churn against `main` is, asked without the
    /// cache: `--numstat` summed (a second route to what `--shortstat` counts)
    /// and the left-right count.
    fn fresh_churn(repo: &Path, name: &str) -> Churn {
        let spec = format!("main...{name}");
        let numstat = git_cli::git_text(repo, &["diff", "--numstat", &spec]).unwrap();
        let (mut additions, mut deletions, mut files_changed) = (0, 0, 0);
        for line in numstat.lines() {
            let mut cols = line.split('\t');
            additions += cols.next().unwrap().parse::<usize>().unwrap();
            deletions += cols.next().unwrap().parse::<usize>().unwrap();
            files_changed += 1;
        }
        let counts =
            git_cli::git_text(repo, &["rev-list", "--left-right", "--count", &spec]).unwrap();
        let (behind, ahead) = git_cli::parse_left_right_count(&counts);
        Churn {
            additions,
            deletions,
            files_changed,
            commits_ahead: ahead,
            commits_behind: behind,
        }
    }

    /// The carried answer is what git measures. A carried answer rests on
    /// "the ahead count is unchanged, so the commit set is" over a base that
    /// descends from the old one; this throws seeded random histories (commits,
    /// merges both ways, criss-cross merges, cherry-picks, rebases, rewinds,
    /// fast-forwards, and many empty advances of `main`) at it and compares
    /// every branch after every step with a fresh `diff` and `rev-list`.
    #[test]
    fn a_carried_answer_is_what_git_measures_over_random_histories() {
        let (mut carried_answers, mut ancestry_checks) = (0, 0);
        for seed in 1u64..=8 {
            let (_dir, repo) = fixture();
            let cache = tempfile::tempdir().unwrap();
            let (_use, _store) = UseStore::file(&cache.path().join(FILE_NAME));
            let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            let mut next = move |n: usize| -> usize {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state % n as u64) as usize
            };
            let mut branches = vec!["main".to_string(), "topic".to_string()];
            for step in 0..22 {
                let pick = branches[next(branches.len())].clone();
                let other = branches[next(branches.len())].clone();
                let what;
                match next(12) {
                    0..=2 => {
                        what = "empty advance of main";
                        try_git(&repo, &["checkout", "-q", "main"]);
                        try_git(&repo, &["commit", "-q", "--allow-empty", "-m", "adv"]);
                    }
                    3..=4 => {
                        what = "commit";
                        try_git(&repo, &["checkout", "-q", &pick]);
                        let file = format!("f{}.txt", next(3));
                        let body: String = (0..6).map(|i| format!("{} {}\n", i, next(4))).collect();
                        std::fs::write(repo.join(&file), body).unwrap();
                        try_git(&repo, &["add", "."]);
                        try_git(&repo, &["commit", "-q", "-m", "c"]);
                    }
                    5 => {
                        what = "new branch";
                        let name = format!("b{step}");
                        let at = format!("{other}~{}", next(2));
                        if try_git(&repo, &["branch", &name, &at])
                            || try_git(&repo, &["branch", &name, &other])
                        {
                            branches.push(name);
                        }
                    }
                    6..=7 => {
                        what = "merge";
                        try_git(&repo, &["checkout", "-q", &pick]);
                        if !try_git(&repo, &["merge", "-q", "--no-ff", "-m", "m", &other]) {
                            try_git(&repo, &["merge", "--abort"]);
                        }
                    }
                    8 => {
                        what = "cherry-pick";
                        try_git(&repo, &["checkout", "-q", &pick]);
                        if !try_git(&repo, &["cherry-pick", "-x", &other]) {
                            try_git(&repo, &["cherry-pick", "--abort"]);
                        }
                    }
                    9 => {
                        what = "rebase";
                        if pick != "main" && !try_git(&repo, &["rebase", "-q", "main", &pick]) {
                            try_git(&repo, &["rebase", "--abort"]);
                        }
                    }
                    10 => {
                        what = "fast-forward main";
                        try_git(&repo, &["checkout", "-q", "main"]);
                        try_git(&repo, &["merge", "-q", "--ff-only", &other]);
                    }
                    _ => {
                        what = "rewind main";
                        try_git(&repo, &["checkout", "-q", "main"]);
                        try_git(&repo, &["reset", "-q", "--hard", "HEAD~1"]);
                    }
                }
                try_git(&repo, &["checkout", "-q", "-f", "main"]);
                let report = GitReader::branch_stats(repo.to_str().unwrap()).expect("stats");
                carried_answers += report.cached;
                for update in &report.updates {
                    let reported = Churn {
                        additions: update.additions,
                        deletions: update.deletions,
                        files_changed: update.files_changed,
                        commits_ahead: update.commits_ahead_of_base,
                        commits_behind: update.commits_behind_base,
                    };
                    assert_eq!(
                        reported,
                        fresh_churn(&repo, &update.name),
                        "seed {seed} step {step} ({what}): {} differs from a fresh measurement",
                        update.name
                    );
                }
            }
            ancestry_checks += spawns_named(&repo, "--is-ancestor");
        }
        assert!(carried_answers > 0, "the histories never reused an answer");
        assert!(
            ancestry_checks > 0,
            "the histories never carried over a moved base"
        );
    }

    /// Points every stored row of `tip` at `base`, as a row written by another
    /// process (or kept before a failed write) would be.
    fn restamp(store: &Arc<Mutex<Store>>, tip: &str, base: &str) {
        store
            .lock()
            .unwrap()
            .conn
            .execute(
                "UPDATE churn SET base = ?1 WHERE tip = ?2",
                params![base, tip],
            )
            .unwrap();
    }

    /// More distinct stored bases than the carry cap: the cap bounds the
    /// ancestry processes, and the branches it leaves out are measured, not
    /// carried on trust.
    #[test]
    fn stored_bases_past_the_carry_cap_are_measured_not_carried() {
        let (_dir, repo) = fixture();
        let cache = tempfile::tempdir().unwrap();
        let (_use, store) = UseStore::file(&cache.path().join(FILE_NAME));
        let tree = rev(&repo, "main^{tree}");
        // The fixture's `topic` gets a base of its own (the commit it forked
        // from), so every stored base names exactly one tip: 7 distinct bases,
        // 4 carried, 3 measured, whichever 4 the call happens to ask about.
        let mut old_bases = vec![("topic".to_string(), rev(&repo, "topic"), rev(&repo, "main"))];
        for i in 0..6 {
            advance(&repo, &format!("m{i}"));
            let base = rev(&repo, "main");
            let commit = git_cli::git_text(
                &repo,
                &["commit-tree", &tree, "-p", &base, "-m", &format!("b{i}")],
            )
            .unwrap();
            git_in(&repo, &["branch", &format!("k{i}"), commit.trim()]);
            old_bases.push((format!("k{i}"), commit.trim().to_string(), base));
        }
        let path = repo.to_str().unwrap();
        GitReader::branch_stats(path).unwrap();
        for (_, tip, base) in &old_bases {
            restamp(&store, tip, base);
        }
        advance(&repo, "unrelated");
        let (diffs_before, ancestry_before) = (diffs(&repo), spawns_named(&repo, "--is-ancestor"));
        let report = GitReader::branch_stats(path).unwrap();
        assert!(
            spawns_named(&repo, "--is-ancestor") - ancestry_before <= 4,
            "more ancestry processes than the cap"
        );
        assert_eq!(
            diffs(&repo) - diffs_before,
            3,
            "the branches past the cap are measured, the others carried"
        );
        for update in &report.updates {
            let fresh = fresh_churn(&repo, &update.name);
            assert_eq!(
                (update.additions, update.deletions, update.files_changed),
                (fresh.additions, fresh.deletions, fresh.files_changed),
                "{}",
                update.name
            );
            assert_eq!(
                update.commits_behind_base, fresh.commits_behind,
                "{}",
                update.name
            );
        }
    }

    /// A stored base git cannot place (a shallow clone without it, a pruned
    /// commit) makes the ancestry question an error: measured, never carried.
    #[test]
    fn a_stored_base_git_cannot_place_is_measured_not_carried() {
        let (_dir, repo) = fixture();
        let cache = tempfile::tempdir().unwrap();
        let (_use, store) = UseStore::file(&cache.path().join(FILE_NAME));
        let (_, first) = stats(&repo);
        restamp(&store, &rev(&repo, "topic"), &"a".repeat(40));
        advance(&repo, "unrelated");
        let before = diffs(&repo);
        let (report, second) = stats(&repo);
        assert_eq!(diffs(&repo) - before, 1, "an unplaceable base was trusted");
        assert_eq!(report.computed, 1);
        assert_eq!(second.additions, first.additions);
        assert_eq!(second.commits_behind, 1);
    }

    /// A carry is written once, in one transaction, and a call that finds
    /// the carried rows current writes nothing.
    #[test]
    fn carried_answers_are_written_once_per_advance() {
        let (_dir, repo) = fixture();
        let cache = tempfile::tempdir().unwrap();
        let (_use, store) = UseStore::file(&cache.path().join(FILE_NAME));
        let tree = rev(&repo, "main^{tree}");
        let main = rev(&repo, "main");
        for i in 0..20 {
            let commit = git_cli::git_text(
                &repo,
                &["commit-tree", &tree, "-p", &main, "-m", &format!("b{i}")],
            )
            .unwrap();
            git_in(&repo, &["branch", &format!("many-{i:02}"), commit.trim()]);
        }
        let path = repo.to_str().unwrap();
        GitReader::branch_stats(path).unwrap();
        advance(&repo, "unrelated");
        let changes = |store: &Arc<Mutex<Store>>| store.lock().unwrap().conn.total_changes();
        let at_advance = changes(&store);
        GitReader::branch_stats(path).unwrap();
        let carried = changes(&store) - at_advance;
        assert_eq!(carried, 21, "the carry rewrote more than its rows");
        let settled = changes(&store);
        GitReader::branch_stats(path).unwrap();
        assert_eq!(changes(&store), settled, "a settled call wrote");
    }

    /// An input that moves after an answer was carried is still seen: the
    /// carried row sits under the fingerprint it was read with.
    #[test]
    fn a_carried_answer_does_not_survive_a_later_input_change() {
        let (_dir, repo) = fixture();
        let cache = tempfile::tempdir().unwrap();
        let (_use, _store) = UseStore::file(&cache.path().join(FILE_NAME));
        let (_, baseline) = stats(&repo);
        advance(&repo, "unrelated");
        let before = diffs(&repo);
        stats(&repo);
        assert_eq!(diffs(&repo) - before, 0, "the answer was carried");
        git_in(&repo, &["config", "diff.renames", "false"]);
        let before = diffs(&repo);
        let (_, unrenamed) = stats(&repo);
        assert_eq!(diffs(&repo) - before, 1);
        assert_ne!(unrenamed.files_changed, baseline.files_changed);
        std::fs::write(repo.join(".git/info/attributes"), "a.txt -diff\n").unwrap();
        let before = diffs(&repo);
        stats(&repo);
        assert_eq!(diffs(&repo) - before, 1, "an attributes change was missed");
    }

    #[test]
    fn a_carried_answer_keeps_the_measurement_and_takes_fresh_counts() {
        let kept = Kept {
            base: "old".into(),
            churn: Churn {
                additions: 5,
                deletions: 4,
                files_changed: 3,
                commits_ahead: 2,
                commits_behind: 1,
            },
        };
        assert_eq!(
            kept.carried((2, 9)),
            Some(Churn {
                commits_behind: 9,
                ..kept.churn
            })
        );
        assert_eq!(kept.carried((1, 9)), None, "the ahead set changed");
        assert_eq!(kept.carried((3, 9)), None);
    }

    fn churn(n: usize) -> Churn {
        Churn {
            additions: n,
            ..ZERO_CHURN
        }
    }

    fn one(tip: &str, n: usize) -> HashMap<String, Churn> {
        HashMap::from([(tip.to_string(), churn(n))])
    }

    #[test]
    fn the_store_keeps_its_newest_rows_within_its_cap() {
        let mut store = Store::memory(3);
        for (i, tip) in ["a", "b", "c"].iter().enumerate() {
            store.insert(1, 2, "base", &one(tip, i)).unwrap();
        }
        // Re-keeping "a" makes it the newest.
        store.insert(1, 2, "base", &one("a", 10)).unwrap();
        store.insert(1, 2, "base", &one("d", 4)).unwrap();
        assert_eq!(store.rows(), 3);
        let tips: Vec<String> = ["a", "b", "c", "d"].map(String::from).to_vec();
        let found = store.lookup(1, 2, &tips).unwrap();
        assert_eq!(found.get("a").map(|kept| kept.churn), Some(churn(10)));
        assert_eq!(found["a"].base, "base");
        assert_eq!(found.get("b"), None, "the oldest row was not evicted");
        assert!(found.contains_key("c") && found.contains_key("d"));
        // Every part of the key separates rows.
        assert!(store.lookup(9, 2, &tips).unwrap().is_empty());
        assert!(store.lookup(1, 9, &tips).unwrap().is_empty());
        // A later base replaces the row for its tip rather than adding one.
        store.insert(1, 2, "later", &one("a", 11)).unwrap();
        let found = store.lookup(1, 2, &["a".to_string()]).unwrap();
        assert_eq!(
            (found["a"].base.as_str(), found["a"].churn),
            ("later", churn(11))
        );
        assert_eq!(store.rows(), 3);
    }

    #[test]
    fn a_damaged_or_foreign_file_is_replaced_and_a_damaged_row_is_a_miss() {
        let cache = tempfile::tempdir().unwrap();
        let path = cache.path().join(FILE_NAME);
        std::fs::write(&path, b"not a database at all, just bytes").unwrap();
        let mut store = Store::open(&path, 10).expect("a damaged cache is replaced");
        store.insert(1, 2, "base", &one("a", 1)).unwrap();
        drop(store);

        // Another schema version: discarded, not misread.
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        drop(conn);
        let mut store = Store::open(&path, 10).unwrap();
        assert_eq!(store.rows(), 0);

        store.insert(1, 2, "base", &one("a", 1)).unwrap();
        store
            .conn
            .execute("UPDATE churn SET additions = -1", [])
            .unwrap();
        assert!(store.lookup(1, 2, &["a".to_string()]).unwrap().is_empty());
    }

    #[test]
    fn a_cache_that_cannot_be_created_is_reported() {
        let cache = tempfile::tempdir().unwrap();
        let blocker = cache.path().join("file");
        std::fs::write(&blocker, "").unwrap();
        let err = Store::open(&blocker.join(FILE_NAME), 10)
            .err()
            .expect("cannot open")
            .to_string();
        assert!(err.contains("cannot create"), "{err}");
    }

    /// A transient `SQLITE_BUSY` (another process holds the file) says nothing
    /// about the file's contents: the open fails and the file, with its rows,
    /// survives for whoever has it.
    #[test]
    fn a_busy_cache_is_kept_not_deleted() {
        let cache = tempfile::tempdir().unwrap();
        let path = cache.path().join(FILE_NAME);
        let mut store = Store::open(&path, 10).unwrap();
        store.insert(1, 2, "base", &one("a", 1)).unwrap();
        drop(store);

        // Another process using the cache exclusively for a while.
        let holder = Connection::open(&path).unwrap();
        holder
            .execute_batch("PRAGMA locking_mode = EXCLUSIVE; BEGIN EXCLUSIVE;")
            .unwrap();
        let busy = Store::open_within(&path, 10, Duration::from_millis(200))
            .err()
            .expect("the file is locked");
        assert!(matches!(busy, OpenError::Busy(_)), "{busy:?}");
        assert!(!busy.to_string().contains("recreated"), "{busy}");
        assert!(path.exists(), "a busy cache was deleted");
        drop(holder);

        let store = Store::open(&path, 10).expect("the cache is usable again");
        assert_eq!(store.rows(), 1, "the rows survived a busy open");
    }

    /// Several openers create one cache at the same instant, as the app and
    /// gitpulse-mcp can. Every one must get a usable store: `journal_mode=WAL`
    /// answers `SQLITE_BUSY` at once, without the busy handler, while a sibling
    /// is switching the same file.
    #[test]
    fn openers_racing_to_create_one_cache_all_get_a_store() {
        const OPENERS: usize = 6;
        let mut failures = Vec::new();
        for round in 0..40 {
            let cache = tempfile::tempdir().unwrap();
            let path = cache.path().join(FILE_NAME);
            let gate = std::sync::Barrier::new(OPENERS);
            let outcomes: Vec<Result<(), String>> = std::thread::scope(|scope| {
                let openers: Vec<_> = (0..OPENERS)
                    .map(|_| {
                        scope.spawn(|| {
                            gate.wait();
                            Store::open(&path, 10).map(drop).map_err(|e| e.to_string())
                        })
                    })
                    .collect();
                openers.into_iter().map(|o| o.join().unwrap()).collect()
            });
            failures.extend(
                outcomes
                    .into_iter()
                    .filter_map(Result::err)
                    .map(|e| format!("round {round}: {e}")),
            );
        }
        assert!(
            failures.is_empty(),
            "{} openers failed: {failures:#?}",
            failures.len()
        );
    }
}
