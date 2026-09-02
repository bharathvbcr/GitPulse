//! DevCouncil's state store, natively.
//!
//! The harness owns execution; DevCouncil owns state. This crate is how the
//! harness reads and writes that state without a second copy of it — same
//! SQLite file, same schema, same semantics, so `dev tasks` and the harness
//! agree about who holds what while the migration is in progress.
//!
//! The load-bearing piece is the lease. It is the only thing standing between
//! two builders and the same working tree, and its correctness rests on one
//! detail that is easy to lose in a port: **mutual exclusion is enforced by a
//! partial unique index in the database, not by the check-then-insert in the
//! code.**
//!
//! ```sql
//! CREATE UNIQUE INDEX ux_task_leases_active
//!   ON task_leases (task_id) WHERE status = 'active'
//! ```
//!
//! The `active_for_task` check before an insert is an optimisation that
//! produces a friendly error. The index is what makes the race safe. A port
//! that keeps the check and drops the index would pass every single-threaded
//! test and hand two agents the same task in production.
//!
//! The second detail is lazy expiry: reading a lease whose TTL has passed marks
//! it stale as a side effect and reports no active lease. Expiry is therefore
//! observed on read rather than by a sweeper, and a reader must be prepared to
//! write.

use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};

pub mod json;
pub mod schema;

/// The lease-schema revision this build understands.
///
/// It is asserted by the Go client's health check rather than merely reported.
/// A harness binary and a store binary that disagree about the schema is the
/// failure that produces a confident wrong answer — a lease read through the
/// wrong column layout — so the mismatch is caught at the boundary instead of
/// surfacing later as data that looks valid.
pub const SCHEMA_VERSION: u32 = 1;

/// The ceiling on one task's agent-appended scope.
///
/// It bounds three things at once: the column, the argv the CLI boundary
/// carries it over, and how far an executor can widen its own plan before a
/// human has to look. 64 KiB is roughly a thousand paths — far past any honest
/// task, and far short of anything that threatens the process boundary.
pub const MAX_AGENT_APPENDED_SCOPE_BYTES: usize = 64 * 1024;

/// The outcome of replacing a task's agent-appended scope.
///
/// Every variant other than `Written` is a normal answer a caller branches on,
/// not a fault: a lease that lapsed mid-turn and a concurrent widening are both
/// things that happen while two builders work, and a caller that had to tell
/// them apart by reading an error string would eventually get it wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeWrite {
    /// The replacement was written.
    Written,
    /// No task with that id exists. Scope cannot be widened onto nothing.
    NoTask,
    /// The caller does not hold this task's live lease.
    NotLeased(LeaseCode),
    /// The column moved since the caller read it. Carries the current value so
    /// the caller can merge and retry without a second round trip.
    Stale { current: String },
}

/// Errors this store can produce. Each maps to an outcome an agent branches on
/// rather than a message it has to read.
#[derive(Debug)]
pub enum StoreError {
    /// Another agent holds the active lease.
    LeaseHeld { task_id: String, holder: String },
    /// The store could not be reached or the statement failed.
    Sql(rusqlite::Error),
    /// A stored timestamp could not be interpreted. Deliberately an error
    /// rather than a silent "treat as not expired": an unreadable expiry must
    /// not read as an indefinite lease.
    BadTimestamp { value: String },
    /// A scope replacement the caller offered is not something this store will
    /// hold: not a JSON array, or larger than one task's scope may be. It is an
    /// error rather than a truncation because the column is read back as the
    /// gate's authority over a working tree, and a half-written array would
    /// either fail to parse there or, worse, parse into a scope nobody wrote.
    BadScope { reason: String },
    /// The database could not be put in WAL mode. Distinct from `Sql` because
    /// it is the one failure that must never be papered over: the store's
    /// concurrency behaviour is stated in terms of WAL, so continuing in
    /// another journal mode would mean two builders contending under rules
    /// nobody wrote down. It names the mode actually in force so the reader
    /// does not have to go and ask the file.
    JournalMode { mode: String },
    /// The database named does not exist. Distinct from `Sql` because the
    /// caller that asks about a store it has not created is almost always a
    /// caller with a typo in its path, and `Connection::open` would have
    /// answered by manufacturing an empty database that reports itself
    /// healthy — a confident zero from a store nobody is using.
    MissingDatabase { path: String },
    /// The opened database's lease exclusion index is not the partial unique
    /// index this crate's mutual exclusion rests on. Fatal rather than a
    /// warning: without it `acquire` degrades to a check-then-insert that
    /// hands two builders the same task, and it does so silently.
    ExclusionIndex { detail: String },
    /// A TTL the store will not honour: zero, negative, or past the ceiling.
    /// Zero or negative would mint a lease that is born expired — the caller
    /// believes it holds a task it does not — and an unbounded one walks the
    /// epoch arithmetic off i64.
    BadTtl { ttl_seconds: i64 },
}

/// The longest TTL this store will mint, in seconds. Long enough for any real
/// working session; short enough that no arithmetic on it can overflow.
pub const MAX_TTL_SECONDS: i64 = 30 * 24 * 60 * 60;

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::LeaseHeld { task_id, holder } => {
                write!(f, "task {task_id} is held by {holder}")
            }
            StoreError::Sql(e) => write!(f, "store error: {e}"),
            StoreError::BadTimestamp { value } => {
                write!(f, "unreadable timestamp in store: {value:?}")
            }
            StoreError::BadScope { reason } => write!(f, "unusable scope value: {reason}"),
            StoreError::JournalMode { mode } => write!(
                f,
                "store could not be put in WAL mode (journal_mode is {mode:?}); \
                 concurrent builders need WAL to share this database"
            ),
            StoreError::MissingDatabase { path } => write!(
                f,
                "no store at {path:?}; it was not created, because a database \
                 conjured from a mistyped path answers every question with an \
                 empty-but-healthy store"
            ),
            StoreError::ExclusionIndex { detail } => write!(
                f,
                "the lease exclusion index is not the partial unique index on task_id \
                 ({detail}); without it two builders racing for one task both win"
            ),
            StoreError::BadTtl { ttl_seconds } => write!(
                f,
                "unusable ttl {ttl_seconds}s: must be between 1 and {MAX_TTL_SECONDS} seconds"
            ),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Sql(e)
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// How a lease token classifies, matching DevCouncil's four documented outcomes
/// so an agent can recover without parsing prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseCode {
    Valid,
    Expired,
    Invalid,
    HeldByOther,
}

impl LeaseCode {
    /// The wire value DevCouncil uses.
    pub fn as_str(self) -> &'static str {
        match self {
            LeaseCode::Valid => "valid",
            LeaseCode::Expired => "lease_expired",
            LeaseCode::Invalid => "invalid_lease",
            LeaseCode::HeldByOther => "lease_held_by_other",
        }
    }

    /// The action and tool an agent should reach for, mirroring DevCouncil's
    /// `suggested_action` / `suggested_tool` contract.
    pub fn recovery(self) -> Option<(&'static str, &'static str)> {
        match self {
            LeaseCode::Valid => None,
            LeaseCode::Expired | LeaseCode::Invalid => {
                Some(("checkout_again", "devcouncil_checkout_task"))
            }
            LeaseCode::HeldByOther => Some(("pick_other_task", "devcouncil_next_task")),
        }
    }
}

/// One lease row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub id: String,
    pub task_id: String,
    pub owner: String,
    pub agent: Option<String>,
    pub client_id: Option<String>,
    pub run_id: Option<String>,
    pub branch: Option<String>,
    pub token: String,
    pub status: String,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub released_at: Option<String>,
}

/// What a caller supplies when acquiring.
#[derive(Debug, Clone, Default)]
pub struct AcquireRequest {
    pub task_id: String,
    pub owner: String,
    pub agent: Option<String>,
    pub client_id: Option<String>,
    pub run_id: Option<String>,
    pub branch: Option<String>,
    /// None means a lease with no expiry. Prefer a TTL: a lease that cannot
    /// expire is a task that stays locked when its holder dies.
    pub ttl_seconds: Option<i64>,
    /// Steal an existing active lease. Reserved for human intervention.
    pub force: bool,
}

/// The store handle.
pub struct Store {
    conn: Connection,
    /// Injected clock, in whole seconds since the epoch. Tests drive expiry
    /// without sleeping.
    now_fn: Option<Box<dyn Fn() -> i64 + Send + Sync>>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").finish_non_exhaustive()
    }
}

impl Store {
    /// Opens (or creates) a store at `path` and ensures the schema is present.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::prepare(conn)
    }

    /// Opens a store that must already exist.
    ///
    /// The difference matters to anything that *asks about* a store rather than
    /// writing to one. `Connection::open` creates the file when it is absent,
    /// so a mistyped path used to produce a private, empty database that
    /// answered "healthy, zero leases" — and two harnesses configured with two
    /// spellings of the same path shared no exclusion at all while both
    /// reported fine. A store that is not there is an answer this returns
    /// rather than a state it manufactures.
    pub fn open_existing(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(StoreError::MissingDatabase {
                path: path.display().to_string(),
            });
        }
        // Checked *and* opened without SQLITE_OPEN_CREATE: the exists() test
        // above is for the message, and the flags are what make the guarantee,
        // so a file deleted between the two cannot slip through as a creation.
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| match e {
            rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::CannotOpen => {
                StoreError::MissingDatabase {
                    path: path.display().to_string(),
                }
            }
            other => StoreError::Sql(other),
        })?;
        Self::prepare(conn)
    }

    /// Opens an in-memory store, for tests.
    pub fn open_in_memory() -> Result<Self> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(conn: Connection) -> Result<Self> {
        // WAL lets readers proceed while a writer holds the write lock, and the
        // busy timeout absorbs the contention two builders produce. Both are
        // set before any statement runs.
        conn.busy_timeout(BUSY_TIMEOUT)?;
        ensure_wal(&conn)?;
        conn.execute_batch(schema::SCHEMA)?;
        // Applying the DDL is not evidence that it took: every statement in it
        // is `IF NOT EXISTS`, and for the exclusion index that match is on the
        // name alone. Reading back what the file actually holds is what turns
        // "the index must exist before the store is used concurrently" from a
        // comment into a precondition.
        schema::verify_exclusion_index(&conn)
            .map_err(|detail| StoreError::ExclusionIndex { detail })?;
        Ok(Store { conn, now_fn: None })
    }

    /// Replaces the clock. Tests use this to move past a TTL instantly.
    pub fn set_clock(&mut self, f: impl Fn() -> i64 + Send + Sync + 'static) {
        self.now_fn = Some(Box::new(f));
    }

    fn now_epoch(&self) -> i64 {
        match &self.now_fn {
            Some(f) => f(),
            None => SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        }
    }

    /// Formats an epoch second as the ISO-8601 string DevCouncil stores, so
    /// Python's `datetime.fromisoformat` reads it back unchanged.
    fn iso(&self, epoch: i64) -> String {
        format_iso_utc(epoch)
    }

    /// Returns the live active lease for a task, expiring it first if its TTL
    /// has passed.
    ///
    /// This is a *writing* read: a lease found past its expiry is marked stale
    /// before the call returns None. That matches the incumbent, and it is why
    /// expiry needs no background sweeper.
    pub fn active_lease(&self, task_id: &str) -> Result<Option<Lease>> {
        let lease = self.select_active(task_id)?;
        let Some(lease) = lease else {
            return Ok(None);
        };
        if let Some(expires) = lease.expires_at.as_deref() {
            let expires_epoch = parse_iso_utc(expires).ok_or_else(|| StoreError::BadTimestamp {
                value: expires.to_string(),
            })?;
            if self.now_epoch() > expires_epoch {
                self.mark(&lease.id, "stale")?;
                return Ok(None);
            }
        }
        Ok(Some(lease))
    }

    /// Acquires a lease.
    ///
    /// The pre-check produces a useful error; the partial unique index is what
    /// actually makes this safe. A racing writer that slips between the two
    /// gets a constraint violation, which is translated into the same
    /// `LeaseHeld` a caller would have seen from the check.
    pub fn acquire(&self, req: &AcquireRequest) -> Result<Lease> {
        // Validated before anything is written, and that ordering is the whole
        // of the guarantee: an unvalidated TTL used to overflow the epoch
        // arithmetic (panic in debug, absurd expiry in release) and a
        // non-positive one minted a lease that read as expired on first touch
        // while reporting success.
        //
        // It also has to come before the force-steal below. It used to sit
        // after it, so `--force true --ttl-seconds 0` revoked the incumbent's
        // lease and *then* refused the request: the requester got nothing, the
        // holder lost the task it was working on, and the only signal was an
        // error message about a TTL. A rejected request must leave the store
        // exactly as it found it.
        if let Some(ttl) = req.ttl_seconds {
            validate_ttl(ttl)?;
        }

        if let Some(active) = self.active_lease(&req.task_id)? {
            if !req.force {
                return Err(StoreError::LeaseHeld {
                    task_id: req.task_id.clone(),
                    holder: holder_of(&active),
                });
            }
            self.mark(&active.id, "stale")?;
        }

        let now = self.now_epoch();
        let lease = Lease {
            id: new_id(&self.conn)?,
            task_id: req.task_id.clone(),
            owner: req.owner.clone(),
            agent: req.agent.clone(),
            client_id: req.client_id.clone(),
            run_id: req.run_id.clone(),
            branch: req.branch.clone(),
            token: new_token(&self.conn)?,
            status: "active".to_string(),
            created_at: self.iso(now),
            expires_at: req.ttl_seconds.map(|ttl| self.iso(now.saturating_add(ttl))),
            released_at: None,
        };

        let inserted = self.conn.execute(
            "INSERT INTO task_leases
               (id, task_id, owner, agent, client_id, run_id, branch,
                lease_token, status, created_at, expires_at, released_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,NULL)",
            params![
                lease.id,
                lease.task_id,
                lease.owner,
                lease.agent,
                lease.client_id,
                lease.run_id,
                lease.branch,
                lease.token,
                lease.status,
                lease.created_at,
                lease.expires_at,
            ],
        );

        match inserted {
            Ok(_) => Ok(lease),
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                // Lost the race against another writer between the check above
                // and this insert. The index rejected the duplicate; report the
                // conflict callers already expect.
                let holder = self
                    .select_active(&req.task_id)?
                    .map(|l| holder_of(&l))
                    .unwrap_or_else(|| "another agent".to_string());
                Err(StoreError::LeaseHeld {
                    task_id: req.task_id.clone(),
                    holder,
                })
            }
            Err(e) => Err(StoreError::Sql(e)),
        }
    }

    /// Classifies a token, distinguishing expired from invalid from conflict.
    pub fn diagnose(&self, task_id: &str, token: &str) -> Result<LeaseCode> {
        if let Some(active) = self.active_lease(task_id)? {
            if active.token == token {
                return Ok(LeaseCode::Valid);
            }
            return Ok(LeaseCode::HeldByOther);
        }
        // No active lease. A token that names a lease which was released or
        // expired is a recoverable "check out again"; anything else is a token
        // this store has never issued.
        let prior: Option<String> = self
            .conn
            .query_row(
                "SELECT status FROM task_leases
                 WHERE task_id = ?1 AND lease_token = ?2
                 ORDER BY created_at DESC LIMIT 1",
                params![task_id, token],
                |row| row.get(0),
            )
            .optional()?;
        match prior.as_deref() {
            Some("stale") | Some("released") => Ok(LeaseCode::Expired),
            _ => Ok(LeaseCode::Invalid),
        }
    }

    /// True when the token holds the live lease.
    pub fn validate(&self, task_id: &str, token: &str) -> Result<bool> {
        Ok(self.diagnose(task_id, token)? == LeaseCode::Valid)
    }

    /// Releases a lease. Returns false when the token does not hold it, so a
    /// stale agent cannot release someone else's work.
    pub fn release(&self, task_id: &str, token: &str) -> Result<bool> {
        let Some(active) = self.active_lease(task_id)? else {
            return Ok(false);
        };
        if active.token != token {
            return Ok(false);
        }
        self.mark(&active.id, "released")?;
        Ok(true)
    }

    /// Pushes the expiry out from now. Returns None when the token does not
    /// hold the lease, or the lease already expired — renewal is deliberately
    /// not a way to resurrect a dead lease.
    pub fn renew(&self, task_id: &str, token: &str, ttl_seconds: i64) -> Result<Option<Lease>> {
        validate_ttl(ttl_seconds)?;
        let Some(active) = self.active_lease(task_id)? else {
            return Ok(None);
        };
        if active.token != token {
            return Ok(None);
        }
        let expires = self.iso(self.now_epoch().saturating_add(ttl_seconds));
        // The status predicate keeps a renewal racing a force-steal from
        // extending a row that was just marked stale.
        self.conn.execute(
            "UPDATE task_leases SET expires_at = ?1 WHERE id = ?2 AND status = 'active'",
            params![expires, active.id],
        )?;
        self.select_active(task_id)
    }

    /// Every currently-live lease, with expiry applied.
    pub fn active_leases(&self) -> Result<Vec<Lease>> {
        let mut out = self.sweep_expired()?;
        out.sort_by(|a, b| a.task_id.cmp(&b.task_id));
        Ok(out)
    }

    /// Applies lazy expiry across every row still marked active, and returns
    /// the ones that survived it.
    ///
    /// This is the single owner of "which leases are actually live", because
    /// the alternative is what was here before: `active_leases` applied expiry
    /// and `ready_tasks` read `status = 'active'` raw, so a lapsed lease still
    /// hid its task from the ready list. Expiry in this store is observed on
    /// read and there is no sweeper, so a lease nothing has touched keeps its
    /// stale `active` row indefinitely — and a task whose builder crashed was
    /// never re-offered until some *other* call happened to look at it.
    fn sweep_expired(&self) -> Result<Vec<Lease>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT task_id FROM task_leases WHERE status = 'active'")?;
        let task_ids: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<_, _>>()?;
        let mut out = Vec::new();
        for task_id in task_ids {
            if let Some(lease) = self.active_lease(&task_id)? {
                out.push(lease);
            }
        }
        Ok(out)
    }

    fn select_active(&self, task_id: &str) -> Result<Option<Lease>> {
        let lease = self
            .conn
            .query_row(
                "SELECT id, task_id, owner, agent, client_id, run_id, branch,
                        lease_token, status, created_at, expires_at, released_at
                 FROM task_leases WHERE task_id = ?1 AND status = 'active'",
                params![task_id],
                row_to_lease,
            )
            .optional()?;
        Ok(lease)
    }

    fn mark(&self, lease_id: &str, status: &str) -> Result<()> {
        let released = self.iso(self.now_epoch());
        self.conn.execute(
            "UPDATE task_leases SET status = ?1, released_at = ?2 WHERE id = ?3",
            params![status, released, lease_id],
        )?;
        Ok(())
    }

    /// Reads a task's scope: the planned files, commands, and prohibitions the
    /// gate enforces.
    ///
    /// Agent-appended fields are merged into the base ones rather than
    /// returned separately. DevCouncil lets an executor widen its own scope
    /// through `agent_appended_*`, and the write gate has to see the union or
    /// it would deny a write the task itself authorised — the split matters to
    /// whoever reviews the plan afterwards, not to the check.
    pub fn task(&self, task_id: &str) -> Result<Option<Task>> {
        // The last two have no `agent_appended_*` sibling, and that asymmetry
        // is deliberate rather than an omission. An executor may widen its own
        // *file* scope by arguing a blocked write through the override seam,
        // because which files a change touches is discovered while doing it.
        // Which requirement a task satisfies is not discovered that way — it is
        // the planner's judgement about why the task exists — and a task able
        // to append to this list could discharge a requirement by claiming it.
        const COLUMNS: [&str; 9] = [
            "planned_files_json",
            "agent_appended_planned_files_json",
            "allowed_commands_json",
            "agent_appended_allowed_commands_json",
            "expected_tests_json",
            "agent_appended_expected_tests_json",
            "forbidden_changes_json",
            "requirement_ids_json",
            "acceptance_criterion_ids_json",
        ];
        let row = self
            .conn
            .query_row(
                "SELECT id, title, status, difficulty, \
                 planned_files_json, agent_appended_planned_files_json, \
                 allowed_commands_json, agent_appended_allowed_commands_json, \
                 expected_tests_json, agent_appended_expected_tests_json, \
                 forbidden_changes_json, \
                 requirement_ids_json, acceptance_criterion_ids_json \
                 FROM tasks WHERE id = ?1",
                params![task_id],
                |r| {
                    let mut scope: Vec<String> = Vec::with_capacity(COLUMNS.len());
                    for index in 4..4 + COLUMNS.len() {
                        scope.push(r.get::<_, String>(index)?);
                    }
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        scope,
                    ))
                },
            )
            .optional()
            .map_err(StoreError::Sql)?;
        let Some((id, title, status, difficulty, scope)) = row else {
            return Ok(None);
        };

        // Every one of these columns is handed on as raw JSON text — merged
        // with its sibling and embedded unquoted into the reply — so a column
        // that is not a well-formed JSON array is a document this store would
        // otherwise emit broken, or worse, emit with an extra key spliced into
        // it. The write path refuses such a value, but this store shares its
        // file with DevCouncil and with anything else holding the path, so the
        // read asserts it too rather than trusting that every writer did.
        for (column, value) in COLUMNS.iter().zip(scope.iter()) {
            if let Err(why) = json::check_array(value.trim()) {
                return Err(StoreError::BadScope {
                    reason: format!("task {task_id}'s {column} is not a JSON array: {why}"),
                });
            }
        }

        Ok(Some(Task {
            id,
            title,
            status,
            difficulty,
            planned_files_json: merge_json_arrays(&scope[0], &scope[1]),
            allowed_commands_json: merge_json_arrays(&scope[2], &scope[3]),
            expected_tests_json: merge_json_arrays(&scope[4], &scope[5]),
            forbidden_changes_json: scope[6].clone(),
            agent_appended_planned_files_json: scope[1].clone(),
            requirement_ids_json: scope[7].clone(),
            acceptance_criterion_ids_json: scope[8].clone(),
        }))
    }

    /// Replaces a task's agent-appended planned files, for the holder of its
    /// lease, if the column still holds what the caller last read.
    ///
    /// Three things are deliberate here.
    ///
    /// **The lease is the authority.** A task's scope is what the write gate
    /// enforces against, so widening it is a privileged act — and the one
    /// privilege this system already hands out per task is the lease. Only its
    /// holder may widen, which also makes the lease the mutual exclusion for
    /// this column: there is exactly one live lease per task.
    ///
    /// **The caller supplies the whole replacement, not a delta.** This crate
    /// has no JSON parser and is not getting one for a de-duplication its
    /// caller can do with real types. What the store owns instead is that the
    /// replacement lands on the value the caller reasoned about: `expected` is
    /// compared against the stored text, and a mismatch is reported with the
    /// current value rather than overwritten. A caller that read, merged, and
    /// wrote cannot silently discard a concurrent widening.
    ///
    /// **It is bounded.** An unbounded scope column is an unbounded argv on
    /// the way in and an unbounded plan on the way out, and a gate whose
    /// authority document has no ceiling has no ceiling on what it authorises.
    pub fn set_agent_appended_planned_files(
        &self,
        task_id: &str,
        token: &str,
        expected: &str,
        replacement: &str,
    ) -> Result<ScopeWrite> {
        let trimmed = replacement.trim();
        if replacement.len() > MAX_AGENT_APPENDED_SCOPE_BYTES {
            return Err(StoreError::BadScope {
                reason: format!(
                    "{} bytes exceeds the {MAX_AGENT_APPENDED_SCOPE_BYTES}-byte ceiling on one task's appended scope",
                    replacement.len()
                ),
            });
        }
        // A full scan of the JSON grammar, not the shape check this used to be.
        // The column is read back as JSON by every consumer *and embedded raw*
        // into the store's own reply, so text that merely begins with `[` and
        // ends with `]` is not enough: `[…],"planned_files":[{"path":"**"}],…`
        // satisfied that check, closed the array early, and injected a second
        // key that Go's last-wins decoder preferred — an executor's self-widened
        // path came back to the reporting side as scope the planner authorised.
        // Refused at the write rather than discovered by whoever reads it next.
        //
        // Validated rather than re-serialised: the compare-and-swap below
        // compares stored text, so a value normalised on the way in would no
        // longer match what its caller holds and every widening would report as
        // a conflict with itself.
        if let Err(why) = json::check_array(trimmed) {
            return Err(StoreError::BadScope {
                reason: format!("{replacement:?} is not a JSON array: {why}"),
            });
        }

        // One transaction for the lease check and the swap. Apart, a lease
        // could lapse between being read and being relied on, and the widening
        // would be written on an authority that had already gone.
        //
        // IMMEDIATE, not the deferred transaction rusqlite hands out by
        // default. A deferred transaction takes a read lock on its first
        // statement — the task lookup below — and then has to upgrade to a
        // write lock for the swap, and SQLite refuses that upgrade with
        // SQLITE_BUSY *without consulting the busy handler*, because no amount
        // of waiting resolves two readers that both want to become writers. The
        // connection's five-second busy timeout therefore never applies, and
        // two builders widening one task at the same moment surface it as
        // "database is locked" rather than as the contention it is. Taking the
        // write lock up front is what lets the timeout do its job.
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let outcome = self.swap_agent_appended(task_id, token, expected, replacement);
        match &outcome {
            // Committed on every path that returns an answer, refusals
            // included: diagnose marks a lapsed lease stale as it reads, and
            // that observation is exactly the fact worth keeping.
            Ok(_) => self.conn.execute_batch("COMMIT")?,
            Err(_) => {
                let _ = self.conn.execute_batch("ROLLBACK");
            }
        }
        outcome
    }

    /// The body of the swap, inside a transaction its caller owns.
    fn swap_agent_appended(
        &self,
        task_id: &str,
        token: &str,
        expected: &str,
        replacement: &str,
    ) -> Result<ScopeWrite> {
        let exists: Option<i64> = self
            .conn
            .query_row("SELECT 1 FROM tasks WHERE id = ?1", params![task_id], |r| {
                r.get(0)
            })
            .optional()?;
        if exists.is_none() {
            return Ok(ScopeWrite::NoTask);
        }

        let code = self.diagnose(task_id, token)?;
        if code != LeaseCode::Valid {
            return Ok(ScopeWrite::NotLeased(code));
        }

        let updated = self.conn.execute(
            // Compared trimmed. The caller's `expected` reaches it through a
            // JSON decoder that reports a value's own bytes and not the
            // whitespace around it, so a column stored with padding would never
            // compare equal to what any caller can send back — the swap would
            // refuse forever and the widening could never be written.
            "UPDATE tasks SET agent_appended_planned_files_json = ?1
             WHERE id = ?2 AND trim(agent_appended_planned_files_json) = trim(?3)",
            params![replacement, task_id, expected],
        )?;
        if updated == 0 {
            let current: String = self.conn.query_row(
                "SELECT agent_appended_planned_files_json FROM tasks WHERE id = ?1",
                params![task_id],
                |r| r.get(0),
            )?;
            // Travels back to the caller embedded raw, so it carries the same
            // obligation as anything else this store emits unquoted: a column
            // some other writer left malformed must not be spliced into a reply
            // as though it were a value.
            if let Err(why) = json::check_array(current.trim()) {
                return Err(StoreError::BadScope {
                    reason: format!(
                        "task {task_id}'s stored appended scope is not a JSON array: {why}"
                    ),
                });
            }
            return Ok(ScopeWrite::Stale { current });
        }
        Ok(ScopeWrite::Written)
    }

    /// Lists tasks that are ready to be worked, excluding any already held.
    ///
    /// "Ready" is a status question and "held" is a lease question, and they
    /// are answered together here so a caller cannot act on a task that was
    /// claimed between the two reads.
    ///
    /// Like every other read of a lease in this store, it is a *writing* read:
    /// expiry is applied first. Without that, "held" meant `status = 'active'`
    /// as stored, so a builder that crashed left its task hidden from the ready
    /// list forever — the TTL had passed, but nothing had touched the row to
    /// notice. `devcouncil_next_task` calls this and nothing else, so the
    /// answer an agent got after a crash was "no unheld ready tasks".
    pub fn ready_tasks(&self) -> Result<Vec<String>> {
        self.sweep_expired()?;
        let mut stmt = self
            .conn
            .prepare(
                "SELECT t.id FROM tasks t \
                 WHERE t.status IN ('planned', 'ready', 'blocked') \
                 AND NOT EXISTS ( \
                   SELECT 1 FROM task_leases l \
                   WHERE l.task_id = t.id AND l.status = 'active' \
                 ) ORDER BY t.id",
            )
            .map_err(StoreError::Sql)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(StoreError::Sql)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(StoreError::Sql)?);
        }
        Ok(out)
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}

fn row_to_lease(row: &rusqlite::Row<'_>) -> rusqlite::Result<Lease> {
    Ok(Lease {
        id: row.get(0)?,
        task_id: row.get(1)?,
        owner: row.get(2)?,
        agent: row.get(3)?,
        client_id: row.get(4)?,
        run_id: row.get(5)?,
        branch: row.get(6)?,
        token: row.get(7)?,
        status: row.get(8)?,
        created_at: row.get(9)?,
        expires_at: row.get(10)?,
        released_at: row.get(11)?,
    })
}

fn holder_of(lease: &Lease) -> String {
    if !lease.owner.is_empty() {
        return lease.owner.clone();
    }
    lease
        .client_id
        .clone()
        .unwrap_or_else(|| "another agent".to_string())
}

/// How long a blocked statement waits before giving up.
///
/// It is both the connection's `busy_timeout` and the budget `ensure_wal`
/// spends, so a caller waits the same bounded time whichever lock it lands on.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Puts the connection in WAL mode, tolerating another process converting the
/// same database at the same moment.
///
/// This needs its own retry loop because `busy_timeout` does not cover it.
/// Switching the journal mode takes an exclusive lock on the database, and
/// SQLite does not run the busy handler while acquiring it — a process that
/// loses that race is handed SQLITE_BUSY immediately rather than waiting its
/// turn. On a database that is already WAL the pragma is a no-op and the race
/// cannot happen; on a *fresh* one every process opening it tries to convert
/// at once and all but one fail. That first-run stampede is precisely what a
/// multi-agent run does — several builders exec the store against a database
/// that does not exist yet — so the store absorbs it here rather than letting
/// "database is locked" escape to an agent that has no way to act on it.
///
/// The mode is read before it is written: the steady state is one lock-free
/// read and no conversion attempt at all. A busy conversion is retried until
/// the deadline, re-reading the mode first, because the ordinary way this
/// resolves is that whoever won finished the conversion and the database is
/// already WAL by the time we look again.
fn ensure_wal(conn: &Connection) -> Result<()> {
    let deadline = Instant::now() + BUSY_TIMEOUT;
    let mut backoff = Duration::from_millis(1);
    let mut mode = current_journal_mode(conn)?;

    loop {
        if mode.eq_ignore_ascii_case("wal") {
            return Ok(());
        }
        // An in-memory database cannot be WAL and has no second connection to
        // contend with, so its "memory" is the right answer rather than a
        // failed conversion to retry.
        if mode.eq_ignore_ascii_case("memory") {
            return Ok(());
        }

        match conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0)) {
            // The pragma answers with the mode now in force, so its own reply
            // is the confirmation — no re-read needed.
            Ok(returned) => mode = returned,
            Err(e) if is_busy(&e) => {}
            Err(e) => return Err(StoreError::Sql(e)),
        }
        if mode.eq_ignore_ascii_case("wal") {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(StoreError::JournalMode { mode });
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_millis(50));
        // Whoever held the lock may have finished the conversion for us.
        mode = current_journal_mode(conn)?;
    }
}

/// Reads the journal mode without attempting to change it. The query form of
/// the pragma takes no exclusive lock, which is what makes the check-first
/// path in `ensure_wal` free.
fn current_journal_mode(conn: &Connection) -> Result<String> {
    Ok(conn.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))?)
}

/// True when SQLite refused because someone else holds a lock. Matched on the
/// error code rather than the message: "database is locked" is prose that a
/// SQLite upgrade is free to reword.
fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked,
                ..
            },
            _
        )
    )
}

/// Identifiers come from SQLite's own CSPRNG rather than a hand-rolled one, so
/// the crate needs no random-number dependency.
fn new_id(conn: &Connection) -> Result<String> {
    let bytes: Vec<u8> = conn.query_row("SELECT randomblob(16)", [], |r| r.get(0))?;
    Ok(hex(&bytes))
}

fn new_token(conn: &Connection) -> Result<String> {
    let bytes: Vec<u8> = conn.query_row("SELECT randomblob(32)", [], |r| r.get(0))?;
    Ok(hex(&bytes))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Formats epoch seconds as `YYYY-MM-DDTHH:MM:SS+00:00`, which is what
/// `datetime.fromisoformat` in the incumbent parses.
/// A task's scope as the gate needs it.
///
/// The `*_json` fields are passed through as raw JSON text rather than parsed
/// here: this crate owns storage, and the consumer already has a JSON decoder.
/// Parsing them twice would mean two places that could disagree about shape.
#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub status: String,
    pub difficulty: Option<String>,
    pub planned_files_json: String,
    pub allowed_commands_json: String,
    pub expected_tests_json: String,
    pub forbidden_changes_json: String,
    /// The agent-appended planned files on their own, exactly as stored.
    ///
    /// `planned_files_json` above is the union the gate needs, and that union
    /// cannot answer the question the *reporting* side asks: which of these
    /// paths did the planner authorise, and which did an executor add to its
    /// own scope while it worked? Carrying the raw column beside the union is
    /// what lets a consumer keep those apart without re-reading the row.
    ///
    /// It is also the value a caller passes back as `expected` to
    /// [`Store::set_agent_appended_planned_files`], which compares stored text
    /// — so it is the untouched column, never a re-serialisation of it.
    pub agent_appended_planned_files_json: String,
    /// The requirements this task exists to satisfy, as the planner linked
    /// them.
    ///
    /// Carried because a requirement-coverage gate cannot be written without
    /// it: a task whose requirements this store did not report is
    /// indistinguishable from a task that satisfies none, and the two must
    /// never read the same.
    pub requirement_ids_json: String,
    /// The acceptance criteria this task is accountable for proving.
    ///
    /// Distinct from `expected_tests_json`, which is *how* the proof is run.
    /// This is *what* must hold, and a criterion no task owns is a behaviour
    /// nobody is accountable for building.
    pub acceptance_criterion_ids_json: String,
}

/// Concatenates two JSON arrays textually.
///
/// Deliberately not a parse-merge-reserialize: the caller already parses the
/// result into real types, and re-encoding here would break the compare-and-swap
/// on the appended column, which compares the store's own bytes.
///
/// Both inputs are required to be well-formed JSON arrays, which is why this
/// concatenation is sound: [`Store::task`] checks every column it reads through
/// [`json::check_array`] before merging, and the write path refuses a
/// replacement that is not one. Splicing text that had only been *shaped* like
/// an array is what let an injected `"planned_files"` key ride into the reply.
fn merge_json_arrays(base: &str, appended: &str) -> String {
    let a = base.trim();
    let b = appended.trim();
    let inner = |s: &str| -> String {
        s.strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let (ai, bi) = (inner(a), inner(b));
    match (ai.is_empty(), bi.is_empty()) {
        (true, true) => "[]".to_string(),
        (false, true) => a.to_string(),
        (true, false) => b.to_string(),
        (false, false) => format!("[{ai},{bi}]"),
    }
}

pub fn format_iso_utc(epoch: i64) -> String {
    let (y, mo, d, h, mi, s) = civil_from_epoch(epoch);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}+00:00")
}

/// Parses the timestamp shapes the store can contain: with or without an
/// offset, with or without fractional seconds. Returns None rather than a
/// guess, so an unreadable expiry surfaces as an error instead of an
/// accidentally-immortal lease.
/// Rejects a TTL this store will not mint. Zero or negative would create a
/// lease that is born expired while reporting success; anything past the
/// ceiling is refused rather than trusted with the epoch arithmetic.
fn validate_ttl(ttl_seconds: i64) -> Result<()> {
    if ttl_seconds <= 0 || ttl_seconds > MAX_TTL_SECONDS {
        return Err(StoreError::BadTtl { ttl_seconds });
    }
    Ok(())
}

pub fn parse_iso_utc(value: &str) -> Option<i64> {
    let text = value.trim();
    let bytes = text.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let num = |a: usize, b: usize| -> Option<i64> { text.get(a..b)?.parse::<i64>().ok() };
    let year = num(0, 4)?;
    let month = num(5, 7)?;
    let day = num(8, 10)?;
    let hour = num(11, 13)?;
    let minute = num(14, 16)?;
    let second = num(17, 19)?;
    if bytes[4] != b'-' || bytes[7] != b'-' || (bytes[10] != b'T' && bytes[10] != b' ') {
        return None;
    }

    // Digit-shaped but impossible calendar values used to sail through and
    // produce an epoch far in some direction — `9999-99-99T99:99:99` read as
    // an effectively immortal lease. The crate's own invariant is that an
    // unreadable expiry must not read as indefinite, so impossible means
    // unreadable.
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }

    let mut epoch =
        epoch_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;

    // Trailing offset, if any. A naive timestamp is read as UTC, matching the
    // incumbent's `replace(tzinfo=timezone.utc)`.
    let rest = &text[19..];
    let rest = rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    if let Some(sign_pos) = rest.find(['+', '-']) {
        let sign = if rest.as_bytes()[sign_pos] == b'+' {
            1
        } else {
            -1
        };
        let off = &rest[sign_pos + 1..];
        let oh: i64 = off.get(0..2)?.parse().ok()?;
        let om: i64 = off
            .get(3..5)
            .or_else(|| off.get(2..4))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        epoch -= sign * (oh * 3600 + om * 60);
    }
    Some(epoch)
}

/// Days from civil date, after Howard Hinnant's algorithm.
fn epoch_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_epoch(epoch: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = epoch.div_euclid(86_400);
    let secs = epoch.rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, secs / 3600, (secs % 3600) / 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};

    #[test]
    fn test_schema_version() {
        assert_eq!(SCHEMA_VERSION, 1);
    }

    #[test]
    fn test_acquire_mutual_exclusion_and_release() {
        let store = Store::open_in_memory().expect("open store");

        // First agent acquires lease
        let lease1 = store
            .acquire(&AcquireRequest {
                task_id: "TASK-001".to_string(),
                owner: "agent-1".to_string(),
                ttl_seconds: Some(300),
                ..Default::default()
            })
            .expect("acquire lease1");

        assert_eq!(lease1.task_id, "TASK-001");
        assert_eq!(lease1.owner, "agent-1");
        assert_eq!(lease1.status, "active");

        // Second agent fails to acquire same task
        let err = store
            .acquire(&AcquireRequest {
                task_id: "TASK-001".to_string(),
                owner: "agent-2".to_string(),
                ttl_seconds: Some(300),
                ..Default::default()
            })
            .expect_err("agent-2 must be rejected");

        match err {
            StoreError::LeaseHeld { task_id, holder } => {
                assert_eq!(task_id, "TASK-001");
                assert_eq!(holder, "agent-1");
            }
            _ => panic!("unexpected error: {:?}", err),
        }

        // Check lease status for agent-1 vs agent-2
        let code1 = store
            .diagnose("TASK-001", &lease1.token)
            .expect("diagnose lease1");
        assert_eq!(code1, LeaseCode::Valid);

        let code2 = store
            .diagnose("TASK-001", "invalid-token")
            .expect("diagnose invalid");
        assert_eq!(code2, LeaseCode::HeldByOther);

        // Release lease
        let released = store
            .release("TASK-001", &lease1.token)
            .expect("release lease1");
        assert!(released);

        // Second agent can now acquire
        let lease2 = store
            .acquire(&AcquireRequest {
                task_id: "TASK-001".to_string(),
                owner: "agent-2".to_string(),
                ttl_seconds: Some(300),
                ..Default::default()
            })
            .expect("acquire lease2");
        assert_eq!(lease2.owner, "agent-2");
    }

    #[test]
    fn test_renew_lease() {
        let clock = Arc::new(AtomicI64::new(1700000000));
        let clock_clone = clock.clone();

        let mut store = Store::open_in_memory().expect("open store");
        store.set_clock(move || clock_clone.load(Ordering::SeqCst));

        let lease = store
            .acquire(&AcquireRequest {
                task_id: "TASK-001".to_string(),
                owner: "agent-1".to_string(),
                ttl_seconds: Some(60),
                ..Default::default()
            })
            .expect("acquire");

        let exp1 = lease.expires_at.expect("expires_at");

        // Advance clock by 30 seconds and renew
        clock.fetch_add(30, Ordering::SeqCst);
        let renewed = store
            .renew("TASK-001", &lease.token, 60)
            .expect("renew")
            .expect("active lease returned");

        let exp2 = renewed.expires_at.expect("expires_at");
        assert_ne!(exp1, exp2);
    }

    #[test]
    fn test_lazy_expiry_via_clock() {
        let clock = Arc::new(AtomicI64::new(1700000000));
        let clock_clone = clock.clone();

        let mut store = Store::open_in_memory().expect("open store");
        store.set_clock(move || clock_clone.load(Ordering::SeqCst));

        let lease = store
            .acquire(&AcquireRequest {
                task_id: "TASK-001".to_string(),
                owner: "agent-1".to_string(),
                ttl_seconds: Some(60),
                ..Default::default()
            })
            .expect("acquire");

        // Before expiry: valid
        let code = store
            .diagnose("TASK-001", &lease.token)
            .expect("diagnose before expiry");
        assert_eq!(code, LeaseCode::Valid);

        // Advance past TTL (100 seconds)
        clock.fetch_add(100, Ordering::SeqCst);

        // On read: lazy expiry marks it expired
        let code_exp = store
            .diagnose("TASK-001", &lease.token)
            .expect("diagnose after expiry");
        assert_eq!(code_exp, LeaseCode::Expired);
    }

    #[test]
    fn test_timestamp_parsing_and_formatting() {
        let epoch = 1700000000; // 2023-11-14T22:13:20+00:00
        let formatted = format_iso_utc(epoch);
        assert_eq!(formatted, "2023-11-14T22:13:20+00:00");
        let parsed = parse_iso_utc(&formatted).expect("parse formatted");
        assert_eq!(parsed, epoch);

        // Space separator and subseconds
        let parsed_space = parse_iso_utc("2023-11-14 22:13:20.123456+00:00").expect("parse space");
        assert_eq!(parsed_space, epoch);
    }

    /// Inserts the minimum row `task` reads, with the scope columns a test names.
    fn plant_task(store: &Store, id: &str, planned: &str, appended: &str) {
        store
            .conn
            .execute(
                "INSERT INTO tasks (id, title, description, planned_files_json,
                                    agent_appended_planned_files_json, status)
                 VALUES (?1, 'planted', '', ?2, ?3, 'ready')",
                params![id, planned, appended],
            )
            .expect("plant task");
    }

    fn lease_for(store: &Store, id: &str) -> String {
        store
            .acquire(&AcquireRequest {
                task_id: id.to_string(),
                owner: "agent-1".to_string(),
                ttl_seconds: Some(300),
                ..Default::default()
            })
            .expect("acquire")
            .token
    }

    /// The union is what the gate reads and the raw column is what a reviewer
    /// reads, and this pins that they are both available from one row read.
    #[test]
    fn appended_scope_is_returned_both_merged_and_on_its_own() {
        let store = Store::open_in_memory().expect("open store");
        plant_task(&store, "TASK-1", r#"["src/a.go"]"#, "[]");
        let token = lease_for(&store, "TASK-1");

        let written = store
            .set_agent_appended_planned_files("TASK-1", &token, "[]", r#"["src/b.go"]"#)
            .expect("append");
        assert_eq!(written, ScopeWrite::Written);

        let task = store
            .task("TASK-1")
            .expect("read task")
            .expect("task exists");
        assert_eq!(task.planned_files_json, r#"["src/a.go","src/b.go"]"#);
        assert_eq!(task.agent_appended_planned_files_json, r#"["src/b.go"]"#);
    }

    /// Widening scope is a privileged act, and the lease is the privilege. Each
    /// way of not holding it is checked, because they are different codes and a
    /// caller recovers differently from each.
    #[test]
    fn only_the_lease_holder_may_widen_scope() {
        let clock = Arc::new(AtomicI64::new(1700000000));
        let clock_clone = clock.clone();
        let mut store = Store::open_in_memory().expect("open store");
        store.set_clock(move || clock_clone.load(Ordering::SeqCst));
        plant_task(&store, "TASK-1", "[]", "[]");

        // No lease has ever been taken on this task.
        assert_eq!(
            store
                .set_agent_appended_planned_files("TASK-1", "no-such-token", "[]", r#"["x"]"#)
                .expect("append"),
            ScopeWrite::NotLeased(LeaseCode::Invalid)
        );

        let token = lease_for(&store, "TASK-1");

        // Held, but by someone else.
        assert_eq!(
            store
                .set_agent_appended_planned_files("TASK-1", "someone-elses-token", "[]", r#"["x"]"#)
                .expect("append"),
            ScopeWrite::NotLeased(LeaseCode::HeldByOther)
        );

        // The right token, after the lease lapsed. A widening written here
        // would outlive the authority that asked for it.
        clock.fetch_add(1_000, Ordering::SeqCst);
        assert_eq!(
            store
                .set_agent_appended_planned_files("TASK-1", &token, "[]", r#"["x"]"#)
                .expect("append"),
            ScopeWrite::NotLeased(LeaseCode::Expired)
        );

        let task = store
            .task("TASK-1")
            .expect("read task")
            .expect("task exists");
        assert_eq!(task.agent_appended_planned_files_json, "[]");
    }

    /// The compare-and-swap. A caller that merged against a value someone else
    /// has since replaced must not write its merge over theirs.
    #[test]
    fn a_stale_expectation_refuses_and_hands_back_the_current_value() {
        let store = Store::open_in_memory().expect("open store");
        plant_task(&store, "TASK-1", "[]", "[]");
        let token = lease_for(&store, "TASK-1");

        store
            .set_agent_appended_planned_files("TASK-1", &token, "[]", r#"["first"]"#)
            .expect("first append");

        let outcome = store
            .set_agent_appended_planned_files("TASK-1", &token, "[]", r#"["second"]"#)
            .expect("second append");
        assert_eq!(
            outcome,
            ScopeWrite::Stale {
                current: r#"["first"]"#.to_string()
            }
        );

        let task = store
            .task("TASK-1")
            .expect("read task")
            .expect("task exists");
        assert_eq!(task.agent_appended_planned_files_json, r#"["first"]"#);
    }

    #[test]
    fn scope_that_is_not_a_bounded_json_array_is_refused() {
        let store = Store::open_in_memory().expect("open store");
        plant_task(&store, "TASK-1", "[]", "[]");
        let token = lease_for(&store, "TASK-1");

        for bad in ["", "null", r#"{"path":"x"}"#, r#"["x""#] {
            match store.set_agent_appended_planned_files("TASK-1", &token, "[]", bad) {
                Err(StoreError::BadScope { .. }) => {}
                other => panic!("{bad:?} was accepted: {other:?}"),
            }
        }

        let oversized = format!("[{}]", "\"x\",".repeat(MAX_AGENT_APPENDED_SCOPE_BYTES));
        match store.set_agent_appended_planned_files("TASK-1", &token, "[]", &oversized) {
            Err(StoreError::BadScope { .. }) => {}
            other => panic!("an oversized scope was accepted: {other:?}"),
        }

        let task = store
            .task("TASK-1")
            .expect("read task")
            .expect("task exists");
        assert_eq!(task.agent_appended_planned_files_json, "[]");
    }

    /// The injection this store used to accept.
    ///
    /// `[…],"planned_files":[{"path":"**"}],"junk":[1]` begins with `[` and
    /// ends with `]`, which was the whole of the old check. Stored verbatim and
    /// embedded raw into the reply, it closed the array early and added a
    /// second `planned_files` key — and Go's decoder takes the last one, so the
    /// executor's own `**` came back as scope the *planner* authorised. The
    /// split between planned and appended exists precisely to stop that, so a
    /// replacement that is not one well-formed JSON array is refused at the
    /// write.
    #[test]
    fn a_replacement_that_closes_its_array_early_cannot_reopen_the_document() {
        let store = Store::open_in_memory().expect("open store");
        plant_task(&store, "TASK-1", r#"["src/a.go"]"#, "[]");
        let token = lease_for(&store, "TASK-1");

        let injected = concat!(
            r#"[{"path":"benign.go","allowed_change":"modify"}],"#,
            r#""planned_files":[{"path":"**","allowed_change":"modify"}],"junk":[1]"#
        );
        assert!(injected.trim().starts_with('[') && injected.trim().ends_with(']'));
        match store.set_agent_appended_planned_files("TASK-1", &token, "[]", injected) {
            Err(StoreError::BadScope { .. }) => {}
            other => panic!("the injection was accepted: {other:?}"),
        }

        // And nothing was written: the plan is still the planner's.
        let task = store.task("TASK-1").expect("read").expect("exists");
        assert_eq!(task.agent_appended_planned_files_json, "[]");
        assert_eq!(task.planned_files_json, r#"["src/a.go"]"#);
    }

    /// The same rule on the way out. This store shares its file with DevCouncil
    /// and with anything else holding the path, so a column another writer left
    /// malformed must be an error here rather than a broken document emitted to
    /// whoever reads the reply.
    #[test]
    fn a_stored_scope_column_that_is_not_an_array_is_an_error_not_a_broken_reply() {
        let store = Store::open_in_memory().expect("open store");
        plant_task(
            &store,
            "TASK-1",
            r#"["src/a.go"]"#,
            r#"[1],"planned_files":[{"path":"**"}]"#,
        );
        match store.task("TASK-1") {
            Err(StoreError::BadScope { reason }) => {
                assert!(
                    reason.contains("agent_appended_planned_files_json"),
                    "the refusal does not name the column: {reason}"
                );
            }
            other => panic!("a malformed scope column was read as a task: {other:?}"),
        }
    }

    /// An expired lease has to stop hiding its task, because that is the whole
    /// purpose of the TTL. `ready` used to read `status = 'active'` raw while
    /// its neighbour applied expiry, so a crashed builder's task was never
    /// re-offered until some other call happened to touch the row — and
    /// `devcouncil_next_task` calls this and nothing else.
    #[test]
    fn an_expired_lease_stops_hiding_its_task_from_ready() {
        let mut store = Store::open_in_memory().expect("open store");
        let now = Arc::new(AtomicI64::new(1_700_000_000));
        let clock = Arc::clone(&now);
        store.set_clock(move || clock.load(Ordering::SeqCst));
        plant_task(&store, "TASK-1", "[]", "[]");

        assert_eq!(store.ready_tasks().expect("ready"), vec!["TASK-1"]);
        store
            .acquire(&AcquireRequest {
                task_id: "TASK-1".to_string(),
                owner: "builder-1".to_string(),
                ttl_seconds: Some(60),
                ..Default::default()
            })
            .expect("acquire");
        assert!(store.ready_tasks().expect("ready").is_empty());

        // The holder is gone and its TTL has passed. Nothing else has looked at
        // the row — that is the point.
        now.store(1_700_000_000 + 61, Ordering::SeqCst);
        assert_eq!(
            store.ready_tasks().expect("ready"),
            vec!["TASK-1"],
            "a task whose lease expired is still hidden from the ready list"
        );
    }

    /// A rejected request must leave the store exactly as it found it.
    ///
    /// The force-steal used to be written before the TTL was validated, so
    /// `--force true --ttl-seconds 0` revoked the incumbent and then refused the
    /// caller: agentA lost the task it was working on, agentB got nothing, and
    /// the only signal was an error message about a TTL.
    #[test]
    fn a_refused_ttl_does_not_cost_the_incumbent_its_lease() {
        let store = Store::open_in_memory().expect("open store");
        let held = store
            .acquire(&AcquireRequest {
                task_id: "TASK-1".to_string(),
                owner: "agentA".to_string(),
                ttl_seconds: Some(300),
                ..Default::default()
            })
            .expect("agentA acquires");

        for ttl in [0i64, -5, i64::MAX] {
            match store.acquire(&AcquireRequest {
                task_id: "TASK-1".to_string(),
                owner: "agentB".to_string(),
                ttl_seconds: Some(ttl),
                force: true,
                ..Default::default()
            }) {
                Err(StoreError::BadTtl { .. }) => {}
                other => panic!("ttl {ttl} was not refused: {other:?}"),
            }
            let active = store
                .active_lease("TASK-1")
                .expect("read")
                .expect("agentA still holds the task after a refused steal");
            assert_eq!(active.owner, "agentA");
            assert_eq!(active.token, held.token);
        }

        // A force with a usable TTL still steals; the fix is about ordering,
        // not about disabling the escape hatch.
        let stolen = store
            .acquire(&AcquireRequest {
                task_id: "TASK-1".to_string(),
                owner: "agentB".to_string(),
                ttl_seconds: Some(300),
                force: true,
                ..Default::default()
            })
            .expect("a valid forced acquire still works");
        assert_eq!(stolen.owner, "agentB");
    }

    /// `CREATE UNIQUE INDEX IF NOT EXISTS` matches on the name alone, so a
    /// database carrying a same-named index with any other definition made the
    /// DDL a silent no-op — and every acquire fell back to the check-then-insert
    /// the index exists to backstop. A 24-way race against such a database
    /// elected two winners while health answered ok, so opening it is refused.
    #[test]
    fn an_exclusion_index_with_another_definition_is_refused_at_open() {
        // Each impostor breaks exactly one of the three properties the
        // exclusion depends on, and each was reachable through the same
        // IF NOT EXISTS no-op.
        for impostor in [
            // Unique, but over the wrong column: two active leases on one task
            // have different ids, so both insert.
            "CREATE UNIQUE INDEX ux_task_leases_active ON task_leases (id)",
            // The right column, but not unique: it constrains nothing at all.
            "CREATE INDEX ux_task_leases_active ON task_leases (task_id) WHERE status = 'active'",
            // Unique and partial, over the wrong predicate: a released lease
            // would keep its task locked and an active one would not be
            // excluded.
            "CREATE UNIQUE INDEX ux_task_leases_active ON task_leases (task_id) WHERE status = 'stale'",
        ] {
            let conn = Connection::open_in_memory().expect("open");
            conn.execute_batch(schema::SCHEMA).expect("schema");
            conn.execute_batch(&format!("DROP INDEX ux_task_leases_active; {impostor};"))
                .expect("plant the impostor index");

            match Store::prepare(conn) {
                Err(StoreError::ExclusionIndex { detail }) => assert!(
                    detail.contains("ux_task_leases_active"),
                    "the refusal does not name the index: {detail}"
                ),
                other => panic!("{impostor:?} was accepted: {other:?}"),
            }
        }
    }

    /// The same check must not reject the real thing, including the spelling
    /// DevCouncil's SQLAlchemy emits — quoted identifiers, different spacing.
    #[test]
    fn the_incumbents_spelling_of_the_index_is_accepted() {
        let conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(schema::SCHEMA).expect("schema");
        conn.execute_batch(
            "DROP INDEX ux_task_leases_active;
             CREATE UNIQUE INDEX ux_task_leases_active
               ON task_leases (\"task_id\")
               WHERE status  ==  'active';",
        )
        .expect("plant the incumbent's spelling");
        Store::prepare(conn).expect("the incumbent's own index was refused");
    }

    /// A task id nothing was planned under gets a routable answer rather than a
    /// widening written onto a row that does not exist.
    #[test]
    fn widening_an_unknown_task_reports_no_task() {
        let store = Store::open_in_memory().expect("open store");
        assert_eq!(
            store
                .set_agent_appended_planned_files("TASK-NOPE", "t", "[]", r#"["x"]"#)
                .expect("append"),
            ScopeWrite::NoTask
        );
    }
}
