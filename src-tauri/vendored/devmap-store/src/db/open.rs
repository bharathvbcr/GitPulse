use super::{refusal, sidecar_links, SidecarLinks, Store, WriterLock, CONNECTION_SETUP};
use crate::schema::CURRENT_SCHEMA_VERSION;
use rusqlite::{Connection, Result, TransactionBehavior};
use std::path::Path;
use std::sync::Mutex;

impl Store {
    fn configure_connection(conn: &Connection) -> Result<()> {
        conn.busy_timeout(Self::BUSY_TIMEOUT)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        // `synchronous = NORMAL`, not the `FULL` default.
        //
        // This is a durability trade and worth stating plainly. Under WAL,
        // NORMAL stops fsync-ing on every commit and syncs at checkpoints
        // instead. The documented consequence is that a power loss or OS crash
        // (**not** a process crash — WAL still recovers from that) can lose the
        // most recent transactions. It cannot corrupt the database; that is the
        // difference between NORMAL and OFF, and why OFF is not used here.
        //
        // Losing the most recent transaction here costs a rebuild, not data.
        // Every row in this store is derived from files in the working tree: a
        // generation that vanishes is recomputed by the next `devmap build`,
        // which is exactly what happens today whenever the extraction schema
        // changes. Paying an fsync per commit to durably persist a cache of
        // something already durable on disk buys nothing.
        // 16 KiB pages, against SQLite's 4 KiB default.
        //
        // `extraction_json` averages 54 KB per file in this repository, which
        // is an overflow chain however it is stored — but the chain is ~14
        // pages at 4 KiB and ~4 at 16 KiB, and every page is a WAL frame that
        // has to be written and then checkpointed back into the database.
        //
        // That is where the write actually goes. A `sample` of the persist
        // phase puts it in `pwrite` (433 samples), WAL checkpoint (392) and
        // `fsync` (207), against `sqlite3BtreeInsert` (131): the cost is pages
        // reaching the disk, not rows being inserted. Fewer, larger pages move
        // the same bytes in fewer frames.
        //
        // Measured on this repository (1,533 files), interleaved, n=7, minimum
        // reported — page size is the only variable:
        //
        //            4 KiB     8 KiB    16 KiB    32 KiB
        //   cold     3.23 s    2.78 s    2.70 s    2.60 s
        //   incr     1.84 s    1.56 s    1.49 s    1.48 s
        //   write    1.02 s    0.81 s    0.74 s    0.79 s
        //   store     285 MB    290 MB    296 MB    312 MB
        //
        // 16 KiB is the knee: 32 KiB buys no more time and costs 9% more
        // store, and the 4% this one costs over the default is paid back in a
        // quarter of the write time.
        //
        // Like `auto_vacuum` below, this only takes on a database with no
        // tables yet — which is why it sits here, before `enable_wal` and
        // `migrate`. An existing 4 KiB store accepts the statement, ignores it,
        // and keeps 4 KiB;
        // `an_existing_small_page_store_opens_and_reads` pins that this is not
        // an error.
        //
        // It does **not** share auto_vacuum's conversion path, and an earlier
        // version of this comment claimed it did. `VACUUM` adopts a pending
        // `auto_vacuum`, but it cannot change `page_size` on a WAL database —
        // SQLite silently leaves the page size alone, which is exactly what
        // makes the wrong claim survive a test that only checks the store still
        // works. Measured: `PRAGMA page_size=16384; VACUUM;` on a 299 MB WAL
        // store returned page_size 4096.
        //
        // Converting an existing store means leaving WAL for the rewrite:
        //
        //     PRAGMA journal_mode=DELETE;
        //     PRAGMA page_size=16384;
        //     VACUUM;
        //     PRAGMA journal_mode=WAL;
        //
        // (2 s on that same store, 299 MB -> 296 MB.) That is deliberately not
        // done automatically: it takes an exclusive lock and drops the database
        // out of WAL for the duration, which is not something to do to somebody
        // else's store as a side effect of opening it. Existing stores keep
        // 4 KiB and keep working; new ones get 16 KiB.
        // `a_plain_vacuum_does_not_convert_an_existing_page_size` pins the
        // half that is easy to get wrong.
        conn.pragma_update(None, "page_size", Self::PAGE_SIZE)?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "cache_size", Self::CACHE_SIZE_KIB)?;
        // Pruning and vacuuming sort large intermediate result sets. On disk
        // those spill to temp files in the filesystem's temp directory, which
        // on this platform is neither the database's filesystem nor necessarily
        // fast.
        conn.pragma_update(None, "temp_store", "MEMORY")?;

        // Incremental auto-vacuum, so reclaim costs what the waste costs rather
        // than what the database costs. See [`Self::vacuum_if_needed`].
        //
        // This only takes effect on a database with no tables yet, which is why
        // it sits in `configure_connection` — called before `migrate` creates
        // the schema. On an existing mode-NONE store the statement is accepted
        // and ignored; that store is converted on its next full vacuum instead.
        // Read before set. Setting `auto_vacuum` rewrites the database header
        // even when the mode is already the one being set — measured with the
        // sqlite3 shell on a `chmod 444` store: every other pragma here is
        // silent, this one fails with "attempt to write a readonly database
        // (8)". A store this process can only read must not be refused by its
        // own open, so the write happens only when the mode actually differs;
        // and a read-only store whose mode differs keeps its mode, because
        // reclaim is the only thing that mode serves and reclaim is a write.
        const INCREMENTAL: i64 = 2;
        let auto_vacuum: i64 = conn.query_row("PRAGMA auto_vacuum", [], |row| row.get(0))?;
        if auto_vacuum != INCREMENTAL && !conn.is_readonly(rusqlite::MAIN_DB)? {
            conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
        }
        Ok(())
    }

    /// Put the database into WAL mode, tolerating a concurrent opener (SC28).
    ///
    /// Changing the journal mode needs an exclusive lock, and SQLite returns
    /// `SQLITE_BUSY` for it **without consulting the busy handler** — so the
    /// 5-second `busy_timeout` configured above does not cover this one
    /// statement. Several processes opening a brand-new store at once is
    /// exactly when that happens, and it surfaced as a bare "database is
    /// locked" from four racing builds.
    ///
    /// Losing the race is not an error: the winner sets WAL for everyone. So a
    /// busy result re-reads the mode, and succeeds if the database is already
    /// where it needs to be. Retries are bounded and the final failure is
    /// propagated — falling back to journal mode silently would leave readers
    /// blocking on every write, which is a performance cliff nobody would
    /// attribute to this.
    fn enable_wal(conn: &Connection) -> Result<()> {
        const ATTEMPTS: usize = 10;
        // Switching the journal mode is a write. A read-only store is read in
        // whatever mode it was left in — WAL if the writer finished cleanly,
        // rollback-journal otherwise — and both serve reads; retrying the
        // switch would spend the whole back-off below to report a mode this
        // process could never change.
        if conn.is_readonly(rusqlite::MAIN_DB)? {
            return Ok(());
        }
        let mut last: Option<rusqlite::Error> = None;
        for attempt in 0..ATTEMPTS {
            match conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0)) {
                Ok(mode) if mode.eq_ignore_ascii_case("wal") => return Ok(()),
                Ok(mode) => {
                    last = Some(refusal(format!("journal_mode is {mode}, not wal")));
                }
                Err(error) => last = Some(error),
            }
            // Another connection may have set it already while this one lost
            // the lock race.
            if let Ok(mode) =
                conn.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            {
                if mode.eq_ignore_ascii_case("wal") {
                    return Ok(());
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20 * (attempt as u64 + 1)));
        }
        Err(last.unwrap_or_else(|| refusal("could not enable WAL mode".to_string())))
    }

    fn validate_database_file(path: &Path) -> Result<()> {
        let metadata = match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(refusal(format!(
                    "cannot inspect database {}: {error}",
                    path.display()
                )))
            }
            Ok(_) => std::fs::metadata(path).map_err(|error| {
                refusal(format!(
                    "cannot resolve database {}: {error}",
                    path.display()
                ))
            })?,
        };
        if !metadata.is_file() {
            return Err(refusal(format!(
                "database {} is not a regular file",
                path.display()
            )));
        }
        #[cfg(unix)]
        let links = {
            use std::os::unix::fs::MetadataExt;
            metadata.nlink()
        };
        #[cfg(windows)]
        let links = devmap_extract::safe_fs::file_link_count(
            &std::fs::File::open(path).map_err(|error| refusal(error.to_string()))?,
        )
        .map_err(|error| {
            refusal(format!(
                "cannot inspect database hard links at {}: {error}",
                path.display()
            ))
        })?;
        #[cfg(any(unix, windows))]
        if links > 1 {
            return Err(refusal(format!("database {} has multiple hard links; use an independent store or SQLite backup so WAL and writer ownership cannot diverge", path.display())));
        }
        Ok(())
    }

    /// Open an existing, current-schema store for an embedding reader.
    ///
    /// Unlike `open`, this cannot create, migrate, repair indexes, switch the
    /// journal mode, or repair permissions. SQLite enforces the read boundary
    /// even when the application has write access to the file. A writer must
    /// upgrade an older store explicitly before an advisory reader can use it.
    pub fn open_read_only<P: AsRef<Path>>(db_path: P) -> Result<Self> {
        Self::retrying_lost_races(|| Self::open_read_only_once(db_path.as_ref()))
    }

    fn open_read_only_once(db_path: &Path) -> Result<Self> {
        let resolved = devmap_extract::safe_fs::resolve_file_alias(db_path)
            .map_err(|error| refusal(error.to_string()))?;
        let path = resolved.as_path();
        Self::validate_database_file(path)?;
        let metadata = std::fs::metadata(path).map_err(|error| {
            refusal(format!(
                "cannot inspect devmap store {}: {error}",
                path.display()
            ))
        })?;
        if !metadata.is_file() {
            return Err(refusal(format!(
                "devmap store {} is not a regular file",
                path.display()
            )));
        }
        let _sidecars = Self::checked_sidecars(path)?;
        let setup = CONNECTION_SETUP
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(Self::BUSY_TIMEOUT)?;
        let first_read = conn.query_row("PRAGMA user_version", [], |row| row.get(0));
        drop(setup);
        let stamped: i32 = match first_read {
            Ok(version) => version,
            Err(error) if path.is_file() && Self::directory_refused_the_wal(&error) => {
                conn = Self::open_immutable(path)?;
                conn.busy_timeout(Self::BUSY_TIMEOUT)?;
                conn.query_row("PRAGMA user_version", [], |row| row.get(0))?
            }
            Err(error) => return Err(error),
        };
        Self::admit_reader_schema(&conn, &path.display().to_string(), stamped)?;
        Self::configure_connection(&conn)?;
        Self::validate_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            edge_index: Mutex::new(None),
            generation_counts: Mutex::new(None),
            fts_reachable: Mutex::new(None),
            generation_analysis_status: Mutex::new(None),
            source_freshness_cache: Mutex::new(None),
            db_path: Some(path.to_path_buf()),
            read_only: true,
        })
    }

    /// Whether a read-only open may read a store stamped `stamped`.
    ///
    /// The current schema always. An older one never: a reader cannot migrate,
    /// and the columns it selects may not exist yet. A *newer* one only when
    /// the writer that stamped it recorded a reader floor at or below this
    /// binary's schema — the writer is the one party that knows whether its
    /// bump is safe for an older reader (see [`MIN_READER_SCHEMA_VERSION`]).
    /// No floor recorded means exact match, which is what every store written
    /// before the floor existed was read under.
    ///
    /// Admission is not the last word: `validate_schema` still runs, so a
    /// floor that claims compatibility over a store missing a column this
    /// binary selects is refused there rather than failing a later query.
    ///
    /// [`MIN_READER_SCHEMA_VERSION`]: crate::schema::MIN_READER_SCHEMA_VERSION
    pub(super) fn admit_reader_schema(conn: &Connection, store: &str, stamped: i32) -> Result<()> {
        if stamped == CURRENT_SCHEMA_VERSION {
            return Ok(());
        }
        if stamped < CURRENT_SCHEMA_VERSION {
            return Err(Self::unsupported_schema(store, stamped));
        }
        match Self::recorded_reader_floor(conn, stamped)? {
            Some(floor) if floor <= CURRENT_SCHEMA_VERSION => Ok(()),
            Some(floor) => Err(refusal(format!(
                "devmap store {store}: schema version {stamped} can be read only by a reader at \
                 schema {floor} or newer, and this one reads schema {CURRENT_SCHEMA_VERSION}; \
                 update the binary that embeds devmap-store (re-vendor or rebuild it)"
            ))),
            None => Err(Self::unsupported_schema(store, stamped)),
        }
    }

    /// How long [`Store::open`] keeps retrying `SQLITE_PROTOCOL`.
    ///
    /// SQLite returns it when a WAL-index lock race outlasts its own internal
    /// retries, which concurrent openers of one store can provoke; it is a lost
    /// race, not a damaged file, and the docs' remedy is to try again. On
    /// Windows eight threads opening one legacy store all lost it
    /// (`concurrent_openers_of_a_legacy_store_all_reach_the_current_schema`).
    /// Opening is safe to repeat from the top: `migrate` re-reads the schema
    /// version under the write lock, so a step another opener finished is not
    /// run twice.
    const PROTOCOL_RETRY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

    fn lost_a_locking_race(error: &rusqlite::Error) -> bool {
        matches!(
            error,
            rusqlite::Error::SqliteFailure(failure, _)
                if failure.code == rusqlite::ErrorCode::FileLockingProtocolFailed
        )
    }

    /// Run an open, retrying `SQLITE_PROTOCOL` within
    /// [`Store::PROTOCOL_RETRY_DEADLINE`]. Shared by every entry point that
    /// opens a connection: read-only opens lost the same race on Windows
    /// (`concurrent_readers_cannot_enqueue_writer_work`).
    fn retrying_lost_races(open: impl Fn() -> Result<Self>) -> Result<Self> {
        let deadline = std::time::Instant::now() + Self::PROTOCOL_RETRY_DEADLINE;
        let mut pause = std::time::Duration::from_millis(10);
        loop {
            match open() {
                Err(error)
                    if Self::lost_a_locking_race(&error)
                        && std::time::Instant::now() + pause < deadline =>
                {
                    std::thread::sleep(pause);
                    pause = (pause * 2).min(std::time::Duration::from_millis(500));
                }
                outcome => return outcome,
            }
        }
    }

    pub fn open<P: AsRef<Path>>(db_path: P) -> Result<Self> {
        Self::retrying_lost_races(|| Self::open_once(db_path.as_ref()))
    }

    fn open_once(db_path: &Path) -> Result<Self> {
        let resolved = devmap_extract::safe_fs::resolve_file_alias(db_path)
            .map_err(|error| refusal(error.to_string()))?;
        let path = resolved.as_path();
        Self::validate_database_file(path)?;
        // Before the connection exists: SQLite maps the `-shm` sidecar as it
        // opens a WAL database, with whatever mode the sidecar has, so a
        // repair after `Connection::open` is a repair the connection never
        // sees. See `repair_sidecar_modes`.
        Self::repair_sidecar_modes(path)?;
        // Held to the end of this open; see `CONNECTION_SETUP`.
        let _setup = CONNECTION_SETUP
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut conn = Connection::open(path)?;
        let store = path.display().to_string();

        // Decide whether this binary may touch the file *before* touching it.
        //
        // `enable_wal` used to run first, so pointing any devmap command at a
        // store this kernel cannot read — the Python engine's `index.sqlite` at
        // `user_version = 2` is the live instance PLAN.md §3.1 Class D names —
        // rewrote its header into WAL mode and left `-wal`/`-shm` beside it,
        // and only then printed the refusal. "Refuses rather than degrades on
        // mismatch" is not satisfied by a refusal that has already written.
        //
        // This can only refuse, never admit: `migrate` re-reads the version
        // itself, under the write lock, so a store migrated by another process
        // between these two reads is still handled there.
        let stamped: i32 = match conn.query_row("PRAGMA user_version", [], |row| row.get(0)) {
            Ok(stamped) => stamped,
            // A WAL-mode store in a directory this process cannot write has no
            // `-shm` and no way to create one, so even the first read fails
            // with `SQLITE_READONLY_DIRECTORY`. SQLite's documented answer for
            // that shape is an *immutable* read-only open: nothing can be
            // writing a file in a directory nobody can write to, so the shared
            // memory the WAL index needs can live in this process alone. Only
            // taken for a file that exists — a missing store in a read-only
            // directory is a missing store, and creating one is impossible
            // rather than immutable.
            Err(error) if path.is_file() && Self::directory_refused_the_wal(&error) => {
                conn = Self::open_immutable(path)?;
                conn.query_row("PRAGMA user_version", [], |row| row.get(0))?
            }
            Err(error) => return Err(error),
        };
        if !Self::schema_is_migratable(stamped) {
            return Err(Self::unsupported_schema(&store, stamped));
        }
        let read_only = conn.is_readonly(rusqlite::MAIN_DB)?;
        if read_only && stamped != CURRENT_SCHEMA_VERSION {
            // Migration is a write. A read-only store at an older schema can
            // neither be migrated nor, with the columns this kernel reads
            // missing, be answered from; say which, rather than letting the
            // first `ALTER TABLE` report a bare SQLite code.
            return Err(refusal(format!(
                "devmap store {store} is read-only and at schema {stamped}, which this kernel \
                 (schema {CURRENT_SCHEMA_VERSION}) would have to migrate before reading; make \
                 it writable and run `devmap build`, or rebuild it elsewhere"
            )));
        }

        Self::configure_connection(&conn)?;
        Self::enable_wal(&conn)?;
        if !read_only {
            Self::migrate(&mut conn, &store)?;
        } else {
            Self::validate_schema(&conn)?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
            edge_index: Mutex::new(None),
            generation_counts: Mutex::new(None),
            fts_reachable: Mutex::new(None),
            generation_analysis_status: Mutex::new(None),
            source_freshness_cache: Mutex::new(None),
            db_path: Some(path.to_path_buf()),
            read_only,
        })
    }

    /// `SQLITE_READONLY_DIRECTORY`: the database is read-only because the
    /// directory holding it is, so the `-shm` a WAL read needs cannot be made.
    /// Spelled out because `libsqlite3-sys` exposes the extended codes as bare
    /// integers, and this is the one [`Store::open`] must tell apart from every
    /// other read-only failure.
    const SQLITE_READONLY_DIRECTORY: i32 = 1544;

    pub(super) fn directory_refused_the_wal(error: &rusqlite::Error) -> bool {
        matches!(
            error,
            rusqlite::Error::SqliteFailure(failure, _)
                if failure.extended_code == Self::SQLITE_READONLY_DIRECTORY
        )
    }

    /// Open `path` read-only and immutable, for a store in a directory this
    /// process cannot write. See the fallback in [`Store::open`].
    pub(super) fn open_immutable(path: &Path) -> Result<Connection> {
        // A URI filename: `%`, `?` and `#` in the path would be read as URI
        // syntax, so they are percent-encoded — the only three characters the
        // SQLite URI grammar reserves inside the path component.
        let mut encoded = String::with_capacity(path.as_os_str().len() + 8);
        for byte in path.to_string_lossy().bytes() {
            match byte {
                b'%' => encoded.push_str("%25"),
                b'?' => encoded.push_str("%3F"),
                b'#' => encoded.push_str("%23"),
                other => encoded.push(other as char),
            }
        }
        Connection::open_with_flags(
            format!("file:{encoded}?immutable=1"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                | rusqlite::OpenFlags::SQLITE_OPEN_URI
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
    }

    /// Whether this store can only be read. See the `read_only` field.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Give a writable store's WAL sidecars the write bit the store has.
    ///
    /// SQLite creates `-wal` and `-shm` with the *database file's* mode. A
    /// read of a `chmod 444` store therefore leaves 444 sidecars behind, and
    /// when the operator later restores the store's write bit the sidecars
    /// keep theirs off — so the next build fails with "attempt to write a
    /// readonly database" against a file that is, by every check the
    /// operator would make, writable. Measured on 2026-09-06: a 444 store
    /// read once, `chmod 644`, then `devmap build` — code 8, `user_version`
    /// unchanged. The sidecars are this kernel's, so their mode is this
    /// kernel's to keep consistent. Best-effort and owner-only: a sidecar
    /// another user owns is left for that user, and the write that follows
    /// reports it.
    /// Validate both WAL sidecars and report the ones present.
    ///
    /// Inspected through their directory entries, never opened: this runs on
    /// every open, in processes that already hold connections to this store,
    /// and closing a descriptor on `-shm` releases every SQLite lock the
    /// process holds on it. See `safe_fs::inspect_regular` and
    /// `tests/a_second_open_keeps_the_first_connections_locks.rs`.
    fn checked_sidecars(
        db_path: &Path,
    ) -> Result<Vec<(std::path::PathBuf, devmap_extract::safe_fs::EntryStatus)>> {
        // Validate both siblings before SQLite or permission repair touches
        // either. A missing sibling is normal; an unsafe one is a refusal.
        let mut sidecars = Vec::new();
        for suffix in ["-wal", "-shm"] {
            let mut name = db_path.as_os_str().to_os_string();
            name.push(suffix);
            let path = std::path::PathBuf::from(name);
            match devmap_extract::safe_fs::inspect_regular(&path) {
                Ok(entry) => {
                    match sidecar_links(entry.links) {
                        SidecarLinks::Single => sidecars.push((path, entry)),
                        // A directory entry never reports zero links — a
                        // sidecar SQLite deleted between the lookup and the
                        // stat arrives as `NotFound` below — but the count is
                        // the classifier's to read, and an unlinked file is the
                        // missing-sibling case, not an alias.
                        SidecarLinks::Unlinked => {}
                        SidecarLinks::Aliased => {
                            return Err(refusal(format!(
                                "sidecar {} has multiple hard links",
                                path.display()
                            )));
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(refusal(format!(
                        "cannot safely inspect sidecar {}: {error}",
                        path.display()
                    )))
                }
            }
        }
        Ok(sidecars)
    }

    fn repair_sidecar_modes(db_path: &Path) -> Result<()> {
        #[cfg(unix)]
        {
            use devmap_extract::safe_fs::{Access, Creation, EntryStatus, SafeFile};
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let sidecars = Self::checked_sidecars(db_path)?;
            const OWNER_WRITE: u32 = 0o200;
            // Decided from directory entries, with no descriptor on the store:
            // this runs before every writable open, usually in a process that
            // already holds a connection to this store, and closing a handle on
            // it would release that connection's locks. See `checked_sidecars`.
            let own = match devmap_extract::safe_fs::inspect_regular(db_path) {
                Ok(entry) => entry,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(refusal(error.to_string())),
            };
            // Only our own writable database authorizes repairing our sidecars.
            if own.mode & OWNER_WRITE == 0 || !own.is_owned_by_current_user() {
                return Ok(());
            }
            let needing: Vec<(std::path::PathBuf, EntryStatus)> = sidecars
                .into_iter()
                .filter(|(_, entry)| entry.uid == own.uid && entry.mode & OWNER_WRITE == 0)
                .collect();
            if needing.is_empty() {
                return Ok(());
            }
            // The repair itself needs handles: `fchmod` and the replacement
            // checks below are descriptor operations. Reaching here means a
            // sidecar this user owns lacks the owner-write bit, which SQLite
            // never leaves on a sidecar a writable connection created — it
            // takes a read of a store that was read-only at the time. A
            // connection this process opened *then* and still holds is the one
            // whose locks these handles' close can release; that is named here
            // rather than handled, because the alternative is chmod by path.
            let same_entry = |file: &SafeFile, entry: &EntryStatus| -> Result<bool> {
                let held = file
                    .metadata()
                    .map_err(|error| refusal(error.to_string()))?;
                Ok(held.dev() == entry.dev && held.ino() == entry.ino)
            };
            let database = match SafeFile::open(db_path, Access::Read, Creation::Never) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(refusal(error.to_string())),
            };
            if !same_entry(&database, &own)? {
                return Err(refusal(format!(
                    "database {} was replaced while its sidecars were being examined",
                    db_path.display()
                )));
            }
            let mut repairs = Vec::new();
            for (path, entry) in needing {
                let file = match SafeFile::open(&path, Access::Read, Creation::Never) {
                    Ok(file) => file,
                    // Deleted by SQLite since it was inspected: nothing to repair.
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(refusal(error.to_string())),
                };
                if !same_entry(&file, &entry)? {
                    return Err(refusal(format!(
                        "sidecar {} was replaced while being examined",
                        path.display()
                    )));
                }
                let permissions = file
                    .metadata()
                    .map_err(|error| refusal(error.to_string()))?
                    .permissions();
                repairs.push((file, permissions));
            }
            if repairs.is_empty() {
                return Ok(());
            }
            // An immutable read cannot modify the database or its sidecars.
            // Reject unrelated/unsupported databases before any fchmod. The
            // ordinary open rechecks the live WAL view before migrating.
            let probe = Self::open_immutable(db_path)?;
            let stamped: i32 = probe.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            if !Self::schema_is_migratable(stamped) {
                return Err(Self::unsupported_schema(
                    &db_path.display().to_string(),
                    stamped,
                ));
            }
            if stamped == CURRENT_SCHEMA_VERSION {
                Self::validate_schema(&probe)?;
            } else if stamped == 0 {
                let objects: i64 = probe.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
                    [],
                    |row| row.get(0),
                )?;
                if objects != 0 {
                    return Err(refusal(
                        "cannot repair sidecar permissions for an unrecognized unstamped database",
                    ));
                }
            } else {
                // Historical stores need migration before full current-schema
                // validation. Confirm their original identity without writes.
                let (table, column) = if stamped == 3 {
                    ("extraction_cache", "content_hash")
                } else {
                    ("generations", "analysis_json")
                };
                if !Self::relation_is_table(&probe, table)?
                    || !Self::has_column(&probe, table, column)?
                {
                    return Err(refusal(
                        "cannot repair sidecar permissions without a recognized DevMap schema",
                    ));
                }
            }
            drop(probe);
            database
                .check_unchanged()
                .map_err(|error| refusal(error.to_string()))?;
            for (file, mut permissions) in repairs {
                file.require_owned()
                    .map_err(|error| refusal(error.to_string()))?;
                file.check_unchanged()
                    .map_err(|error| refusal(error.to_string()))?;
                // Restore only owner-write, never database group/other bits.
                permissions.set_mode(permissions.mode() | OWNER_WRITE);
                file.set_permissions(permissions)
                    .map_err(|error| refusal(error.to_string()))?;
            }
        }
        #[cfg(not(unix))]
        {
            let _ = db_path;
        }
        Ok(())
    }

    /// The one place a write against a read-only store is refused, so the
    /// refusal is the same sentence from every writer and names the store
    /// rather than an SQLite error code.
    pub(super) fn refuse_if_read_only(&self) -> Result<()> {
        if !self.read_only {
            return Ok(());
        }
        let store = self
            .db_path
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| ":memory:".to_string());
        Err(refusal(format!(
            "devmap store {store} is read-only: the file or its directory is not writable by \
             this process, so it can be queried but not rebuilt"
        )))
    }

    /// The file this store was opened from, or `None` for an in-memory store.
    ///
    /// `None` is a fact, not a failure: an in-memory database has no path that
    /// could be deleted, moved or locked, so a caller asking "is my store still
    /// there" has its answer.
    pub fn path(&self) -> Option<&Path> {
        self.db_path.as_deref()
    }

    /// Longest a writer waits for another process's writer lock before giving
    /// up and naming the holder.
    ///
    /// A full build of a large repository takes seconds, not minutes, so a
    /// minute is generous headroom rather than a guess. Bounded because an
    /// unbounded wait turns a crashed-but-not-dead holder into a hang with no
    /// diagnostic, which is strictly worse than a refusal that names a pid.
    pub const WRITER_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

    /// Poll interval while waiting for the writer lock. Short enough that a
    /// released lock is picked up promptly, long enough not to spin a core.
    const WRITER_LOCK_POLL: std::time::Duration = std::time::Duration::from_millis(25);

    /// Transaction behaviour for a generation write.
    ///
    /// K13: `Immediate`, matching the prunes, which already use it and document
    /// why — the write lock is taken at `BEGIN` rather than at whichever
    /// statement first needs it, so two writers queue on the busy handler
    /// instead of discovering the conflict partway through and failing an
    /// upgrade that SQLite does not retry.
    ///
    /// Exposed as a named constant because the effect is not observable: SQLite
    /// offers no way to read a transaction's behaviour back, so the policy is
    /// asserted directly rather than inferred from a race that reproduces only
    /// sometimes. The same reason `should_vacuum` and
    /// `should_retire_for_new_binary` are pure functions.
    pub const GENERATION_TX_BEHAVIOR: TransactionBehavior = TransactionBehavior::Immediate;

    /// Path of the advisory writer lock guarding `db_path`.
    pub fn writer_lock_path(db_path: &Path) -> std::path::PathBuf {
        let canonical = db_path.canonicalize().ok();
        let db_path = canonical.as_deref().unwrap_or(db_path);
        let mut name = db_path.file_name().map_or_else(
            || std::ffi::OsString::from("devmap-store"),
            |name| name.to_os_string(),
        );
        name.push(".writer.lock");
        match db_path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
            _ => std::path::PathBuf::from(name),
        }
    }

    /// Take the cross-process writer lock for the store at `db_path` (K13).
    ///
    /// There was no such lock. Two `devmap build` processes — or a build and
    /// the daemon's drain — raced on SQLite's five-second `busy_timeout` alone,
    /// and the loser surfaced `database is locked` after having already paid
    /// for a full extraction and resolution. That is the worst possible place
    /// to fail: all of the cost, none of the result, and an error message that
    /// names neither the other writer nor anything the caller can do.
    ///
    /// An `flock`, mirroring `protocol::lock_ipc_endpoint`: the kernel releases
    /// it when the holder dies, so no stale-lock cleanup exists to go wrong.
    /// `try_lock` in a bounded poll rather than the blocking `lock`, because a
    /// blocking wait cannot be given a deadline and a writer that hangs forever
    /// behind a wedged peer is not an improvement on one that fails.
    ///
    /// The holder writes its pid into the file, so the timeout can say who.
    pub fn lock_writer_at(db_path: &Path, wait: std::time::Duration) -> anyhow::Result<WriterLock> {
        use devmap_extract::safe_fs::{Access, Creation, SafeFile};
        let db_path = devmap_extract::safe_fs::resolve_file_alias(db_path)?;
        Self::validate_database_file(&db_path)?;
        let lock_path = Self::writer_lock_path(&db_path);
        let mut file = SafeFile::open(&lock_path, Access::ReadWrite, Creation::IfMissing).map_err(
            |error| {
                anyhow::anyhow!(
                    "cannot safely open writer lock {}: {error}",
                    lock_path.display()
                )
            },
        )?;

        Self::poll_writer_lock(|| file.try_lock(), wait, Self::WRITER_LOCK_POLL, &lock_path)?;

        // The prior holder may have updated its pid while this caller waited.
        // After taking the lock, validate ownership of the actual descriptor
        // and replace its diagnostic record without a stale content snapshot.
        use std::io::{Seek, Write};
        file.require_owned()?;
        file.rewind()?;
        file.set_len(0)?;
        file.write_all(std::process::id().to_string().as_bytes())?;
        file.flush()?;
        // Windows byte-range locks prohibit diagnostic reads of the locked
        // bytes. The separate owner file follows the same checked-write rule.
        #[cfg(windows)]
        devmap_extract::safe_fs::write(
            &lock_path.with_extension("lock.owner"),
            std::process::id().to_string().as_bytes(),
        )?;
        Ok(WriterLock {
            file: Some(file),
            path: Some(lock_path),
        })
    }

    /// The bounded `try_lock` poll behind [`Store::lock_writer_at`].
    ///
    /// Contention and a failed check are different events and must not share
    /// an answer. `Err(_busy)` matched both `TryLockError::WouldBlock` — some
    /// other process holds it, so wait — and `TryLockError::Error` — the lock
    /// call itself failed, so nothing at all is known about ownership. On a
    /// filesystem that does not implement `flock` (ENOLCK, EOPNOTSUPP) the
    /// second is what *every* attempt returns, so a build polled the full
    /// `wait` and then failed with "another devmap writer holds … (pid
    /// unknown)": a definite claim about a process that does not exist, made
    /// by a check that never ran, after a minute spent waiting for it.
    /// `protocol::lock_ipc_endpoint` refuses that collapse for the IPC
    /// endpoint; this is the same policy for the store's writer lock.
    ///
    /// The attempt arrives as a closure so this decision has exactly one
    /// owner and can be driven by a test — no filesystem refuses `flock` on
    /// demand, and an untestable policy is how the collapse survived here
    /// while being explicitly rejected one crate away.
    pub(super) fn poll_writer_lock<F>(
        mut attempt: F,
        wait: std::time::Duration,
        poll: std::time::Duration,
        lock_path: &Path,
    ) -> anyhow::Result<()>
    where
        F: FnMut() -> std::result::Result<(), std::fs::TryLockError>,
    {
        let deadline = std::time::Instant::now() + wait;
        loop {
            match attempt() {
                Ok(()) => return Ok(()),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if std::time::Instant::now() >= deadline {
                        let owner = Self::writer_lock_holder(lock_path);
                        anyhow::bail!(
                            "another devmap writer holds {lock_path:?} (pid {owner}); \
                             waited {wait:?}. Wait for it to finish, or stop that process."
                        );
                    }
                    std::thread::sleep(poll);
                }
                Err(std::fs::TryLockError::Error(error)) => anyhow::bail!(
                    "the devmap writer lock {lock_path:?} could not be taken: {error}; \
                     ownership is unknown, so no claim is made about another writer"
                ),
            }
        }
    }

    /// The pid a lock holder recorded in its lock file, or `"unknown"`.
    ///
    /// Diagnostic only: the lock is the `flock`, not the file's contents, so
    /// every failure here degrades the message rather than the exclusion.
    fn writer_lock_holder(lock_path: &Path) -> String {
        use devmap_extract::safe_fs::{Access, Creation, SafeFile};

        #[cfg(windows)]
        let owner_path = lock_path.with_extension("lock.owner");
        #[cfg(not(windows))]
        let owner_path = lock_path.to_path_buf();
        SafeFile::open(&owner_path, Access::Read, Creation::Never)
            .and_then(|mut handle| handle.read_text(64))
            .ok()
            .map(|holder| holder.trim().to_string())
            .filter(|pid| pid.parse::<u32>().is_ok_and(|pid| pid > 0))
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// [`Store::lock_writer_at`] for the file this store was opened from.
    ///
    /// An in-memory store returns an unheld guard — see [`WriterLock`]: it is
    /// private to this process and this `Store`, whose mutex already serialises
    /// its writers, so there is no second writer to exclude.
    pub fn lock_writer(&self, wait: std::time::Duration) -> anyhow::Result<WriterLock> {
        self.refuse_if_read_only()?;
        match &self.db_path {
            Some(path) => Self::lock_writer_at(path, wait),
            None => Ok(WriterLock {
                file: None,
                path: None,
            }),
        }
    }

    /// Open a store **without creating one**, for read commands.
    ///
    /// `Store::open` uses `Connection::open`, which creates the file — so every
    /// read was also a write. `devmap status` against a repository with no
    /// store left an empty database behind, and that file is what let
    /// `DevMapClient._start_daemon` spawn `devmap serve` on the *next* call,
    /// which built a generation in the background. An identical command then
    /// failed on the first invocation and succeeded on the second: "unavailable"
    /// was a race, not a state.
    ///
    /// A read answers from what exists, or reports that nothing is there. It
    /// does not create the thing it is reading.
    pub fn open_existing<P: AsRef<Path>>(db_path: P) -> Result<Option<Self>> {
        let path = db_path.as_ref();
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(Self::open(path)?))
    }

    pub fn open_in_memory() -> Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        Self::configure_connection(&conn)?;
        Self::migrate(&mut conn, ":memory:")?;
        Ok(Self {
            conn: Mutex::new(conn),
            edge_index: Mutex::new(None),
            generation_counts: Mutex::new(None),
            fts_reachable: Mutex::new(None),
            generation_analysis_status: Mutex::new(None),
            source_freshness_cache: Mutex::new(None),
            db_path: None,
            read_only: false,
        })
    }
}
