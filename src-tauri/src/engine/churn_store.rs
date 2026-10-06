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

const FILE_NAME: &str = "churn.v1.sqlite";
/// Bumped whenever a row's meaning changes; any other version is discarded.
const SCHEMA_VERSION: i64 = 1;
/// Rows kept on disk: 96 branches a repository, hundreds of base moves.
const PERSISTED_ROWS: usize = 50_000;
/// Rows kept in memory, the bound the process memo always had.
const MEMORY_ROWS: usize = 8_192;
/// How long a write waits for another GitPulse process holding the file.
const BUSY_TIMEOUT: Duration = Duration::from_millis(100);
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

/// Answers already measured for `tips` against `base`.
pub(crate) fn lookup(inputs: &Inputs, base: &str, tips: &[String]) -> HashMap<String, Churn> {
    let Some(fingerprint) = inputs.fingerprint else {
        return HashMap::new();
    };
    with_store(inputs.pinned, |store| {
        store.lookup(inputs.repo, fingerprint, base, tips)
    })
    .unwrap_or_default()
}

/// Keeps `measured`, provided the inputs read now are the ones read before
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
            match default_path().and_then(|path| Store::open(&path, PERSISTED_ROWS)) {
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

pub(crate) struct Store {
    conn: Connection,
    cap: usize,
    persistent: bool,
}

impl Store {
    /// Opens the file at `path`, creating it. A file this build cannot use —
    /// another schema version, or not a database at all — is a cache, so it
    /// is discarded and created again.
    pub(crate) fn open(path: &Path, cap: usize) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        match Self::open_once(path, cap) {
            Ok(store) => Ok(store),
            Err(first) => {
                for suffix in ["", "-wal", "-shm"] {
                    let mut doomed = path.as_os_str().to_owned();
                    doomed.push(suffix);
                    match std::fs::remove_file(&doomed) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => {
                            return Err(format!(
                                "{first}; and it could not be replaced: {}: {e}",
                                PathBuf::from(&doomed).display()
                            ))
                        }
                    }
                }
                Self::open_once(path, cap)
                    .map_err(|second| format!("{first}; recreated, and then {second}"))
            }
        }
    }

    fn open_once(path: &Path, cap: usize) -> Result<Self, String> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        conn.busy_timeout(BUSY_TIMEOUT)
            .map_err(|e| format!("cannot configure {}: {e}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| format!("{} is not usable: {e}", path.display()))?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|e| format!("{} is not usable: {e}", path.display()))?;
        let mut store = Self {
            conn,
            cap,
            persistent: true,
        };
        store
            .ensure_schema()
            .map_err(|e| format!("{} is not usable: {e}", path.display()))?;
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
            self.conn.execute_batch(
                "DROP TABLE IF EXISTS churn;
                 CREATE TABLE churn (
                     repo INTEGER NOT NULL,
                     inputs INTEGER NOT NULL,
                     base TEXT NOT NULL,
                     tip TEXT NOT NULL,
                     additions INTEGER NOT NULL,
                     deletions INTEGER NOT NULL,
                     files INTEGER NOT NULL,
                     ahead INTEGER NOT NULL,
                     behind INTEGER NOT NULL,
                     stored INTEGER NOT NULL,
                     PRIMARY KEY (repo, inputs, base, tip)
                 ) WITHOUT ROWID;
                 CREATE INDEX churn_stored ON churn (stored);",
            )?;
            self.conn
                .pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(())
    }

    fn lookup(
        &mut self,
        repo: u64,
        inputs: u64,
        base: &str,
        tips: &[String],
    ) -> Result<HashMap<String, Churn>, rusqlite::Error> {
        let mut found = HashMap::new();
        let mut query = self.conn.prepare_cached(
            "SELECT additions, deletions, files, ahead, behind FROM churn
             WHERE repo = ?1 AND inputs = ?2 AND base = ?3 AND tip = ?4",
        )?;
        for tip in tips {
            let row = query.query_row(params![repo as i64, inputs as i64, base, tip], |row| {
                Ok([
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ])
            });
            let counts = match row {
                Ok(counts) => counts,
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
                Churn {
                    additions,
                    deletions,
                    files_changed,
                    commits_ahead,
                    commits_behind,
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
                 (repo, inputs, base, tip, additions, deletions, files, ahead, behind, stored)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;
            let mut rows: Vec<(&String, &Churn)> = measured.iter().collect();
            rows.sort_by(|a, b| a.0.cmp(b.0));
            for (tip, churn) in rows {
                stored += 1;
                put.execute(params![
                    repo as i64,
                    inputs as i64,
                    base,
                    tip,
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
                "DELETE FROM churn WHERE (repo, inputs, base, tip) IN
                 (SELECT repo, inputs, base, tip FROM churn ORDER BY stored LIMIT ?1)",
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
        let repo = dir.path().canonicalize().unwrap();
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

        let global = repo.join(".git/global-attributes");
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
        let found = store.lookup(1, 2, "base", &tips).unwrap();
        assert_eq!(found.get("a"), Some(&churn(10)));
        assert_eq!(found.get("b"), None, "the oldest row was not evicted");
        assert!(found.contains_key("c") && found.contains_key("d"));
        // Every part of the key separates rows.
        assert!(store.lookup(9, 2, "base", &tips).unwrap().is_empty());
        assert!(store.lookup(1, 9, "base", &tips).unwrap().is_empty());
        assert!(store.lookup(1, 2, "other", &tips).unwrap().is_empty());
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
        assert!(store
            .lookup(1, 2, "base", &["a".to_string()])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_cache_that_cannot_be_created_is_reported() {
        let cache = tempfile::tempdir().unwrap();
        let blocker = cache.path().join("file");
        std::fs::write(&blocker, "").unwrap();
        let err = Store::open(&blocker.join(FILE_NAME), 10)
            .err()
            .expect("cannot open");
        assert!(err.contains("cannot create"), "{err}");
    }
}
