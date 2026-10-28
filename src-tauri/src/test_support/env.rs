//! Process-global environment overrides that cannot outlive the test.
//!
//! # Why this exists
//!
//! An environment variable is one slot shared by every thread in the test
//! binary. Eighteen call sites used to install an override and undo it on the
//! last line of the test body:
//!
//! ```ignore
//! std::env::set_var("GITPULSE_MANVI_BIN", fake.display().to_string());
//! let msg = resolve_binary_absence();
//! std::env::remove_var("GITPULSE_MANVI_BIN");
//! assert!(msg.contains("not a file"));
//! ```
//!
//! which works exactly until the body fails before reaching the last line —
//! and the body is a test, so failing is the one thing it is built to do. The
//! panic skips the restore, unwinding drops whatever `TempDir` the override
//! pointed into, and the value stays installed for every later test in the
//! process. That is not a hypothetical: the sidecar's fake-binary override
//! leaked exactly this way, and one slow test became nineteen failures across
//! terminal, `tool_install`, workbench and firebase, each reading
//! `could not start …/profile-sidecar: No such file or directory` and each
//! passing when run on its own, which is what makes the class so expensive to
//! diagnose — the damage never surfaces where it is caused.
//!
//! [`bind_env`] is the fix in the shape this crate already uses for the two
//! binary overrides (`harness::sidecar::bind_test_binary` and
//! `devmap::cli::bind_test_binary`): undoing the override is a `Drop`,
//! so it runs on the failing path and the successful one alike, and there is no
//! last line to forget.
//!
//! # What it restores
//!
//! The *previous value*, not "nothing". The hand-rolled sites mostly ended in
//! `remove_var`, which is only accidentally correct: it is right when the
//! variable started out unset and silently destroys a real one otherwise. A
//! developer with `GITPULSE_DEVMAP_BIN` exported lost it for the rest of the
//! run. `Drop` here puts back exactly what was there, including absence.
//!
//! # Why a serial is required to get one (and why there is only one)
//!
//! Restoring reliably is only half the contract. Two tests that set the same
//! variable in parallel interleave regardless of how carefully each one cleans
//! up, and the loser reads the winner's value — or, worse, reads the
//! developer's real environment and asserts against whatever happens to be on
//! the machine. So the only way to obtain a binding is to hand it a serial
//! guard, which makes "I forgot to serialize" a compile error rather than a
//! race that shows up once a fortnight in CI. The binding borrows that guard,
//! so the guard cannot be released while the override is still installed.
//!
//! # Why this file is included twice
//!
//! `gitpulsed` is a separate bin crate and cannot see `gitpulse_lib`'s
//! `#[cfg(test)]` items, but it overrides `GITPULSE_TRANSCRIPT_ROOT` the same
//! way and leaks it the same way. It includes this file with `#[path]` — the
//! idiom `tests/common/process_trust.rs` already uses across two integration
//! tests — so the guarantee is one implementation rather than two that drift.

// Each including crate uses a subset of the API: `gitpulsed` only ever sets a
// variable, so `remove` and `invalidating` are dead code there. The alternative
// is a second, smaller copy of this file, which is the duplication this module
// exists to remove.
#![allow(dead_code)]

use std::ffi::{OsStr, OsString};

/// Proof that the holder owns the process's one environment lock.
///
/// Obtained only from [`env_serial`], and [`bind_env`] takes exactly this type,
/// so "I forgot to serialize" and "I serialized against a lock nothing else
/// takes" are both compile errors. This used to be a trait implemented for any
/// `MutexGuard<()>`, which let six unrelated locks (the sidecar serial, the
/// tool-config lock, lappi's, the hooks notify lock, ingest's and `gitpulsed`'s)
/// all count as "the" serial while serializing nothing against each other: a
/// reader holding one saw another test's `GITPULSE_TOOL_CONFIG` mid-flight.
///
/// Reentrant on the owning thread: a test that holds the lock across a call
/// that takes it again (every sidecar call does) must not deadlock against
/// itself. `std::sync::ReentrantLock` is unstable, so the depth count is the
/// reentrancy. Not `Send`: ownership is the thread's.
pub(crate) struct EnvGuard {
    /// `None` for a re-entry: the outermost guard on this thread owns the
    /// mutex and is what releases it.
    outer: Option<std::sync::MutexGuard<'static, ()>>,
    _not_send: std::marker::PhantomData<*const ()>,
}

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static ENV_OWNER: std::sync::Mutex<Option<(std::thread::ThreadId, usize)>> =
    std::sync::Mutex::new(None);

fn env_owner() -> std::sync::MutexGuard<'static, Option<(std::thread::ThreadId, usize)>> {
    ENV_OWNER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The one lock every test that reads or writes a process-global environment
/// variable takes. Poison-tolerant: a panicking test must not wedge every later
/// one, and the data behind the lock is the emptiness of `()`.
pub(crate) fn env_serial() -> EnvGuard {
    let me = std::thread::current().id();
    {
        let mut owner = env_owner();
        if let Some((thread, depth)) = owner.as_mut() {
            if *thread == me {
                *depth += 1;
                return EnvGuard {
                    outer: None,
                    _not_send: std::marker::PhantomData,
                };
            }
        }
    }
    // Not the owner: block for the real lock, then claim it. Ownership is
    // recorded only after acquisition, so no other thread can mistake itself
    // for the owner in the window between the two.
    let guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *env_owner() = Some((me, 1));
    EnvGuard {
        outer: Some(guard),
        _not_send: std::marker::PhantomData,
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        let mut owner = env_owner();
        if let Some((_, depth)) = owner.as_mut() {
            *depth -= 1;
            if *depth == 0 {
                *owner = None;
            }
        }
        // Ownership is cleared while the mutex is still held, so the next
        // thread in cannot observe a stale owner: `self.outer` releases the
        // mutex after this scope ends.
        drop(owner);
    }
}

/// Environment overrides installed for the whole process, undone on drop.
///
/// Obtained only from [`bind_env`]. The lifetime is the serial's: the borrow
/// keeps the guard alive for at least as long as the override it authorises.
pub(crate) struct EnvBinding<'serial> {
    /// Key and the value it had before this binding touched it, in the order
    /// they were touched. Restored in reverse, so a key overridden twice ends
    /// up back at the value it had before the *first* override.
    saved: Vec<(OsString, Option<OsString>)>,
    /// Caches to drop once the variables are back. Run on the way in as well,
    /// for the reason given on [`EnvBinding::invalidating`].
    invalidations: Vec<Box<dyn Fn()>>,
    _serial: std::marker::PhantomData<&'serial ()>,
    /// The guard, when the binding owns it ([`bind_env_owned`]). A field, so it
    /// is released after `Drop::drop` has restored the variables.
    _owned: Option<EnvGuard>,
}

/// Begin overriding the environment, serialized behind `serial`.
///
/// The returned binding starts out having changed nothing; [`EnvBinding::set`]
/// and [`EnvBinding::remove`] install the overrides.
pub(crate) fn bind_env(_serial: &EnvGuard) -> EnvBinding<'_> {
    EnvBinding {
        saved: Vec::new(),
        invalidations: Vec::new(),
        _serial: std::marker::PhantomData,
        _owned: None,
    }
}

/// [`bind_env`] for a fixture that outlives the function that built it: the
/// binding takes the guard, so it can sit in a struct beside the temp directory
/// it points into without borrowing from a local.
pub(crate) fn bind_env_owned(serial: EnvGuard) -> EnvBinding<'static> {
    EnvBinding {
        saved: Vec::new(),
        invalidations: Vec::new(),
        _serial: std::marker::PhantomData,
        _owned: Some(serial),
    }
}

impl EnvBinding<'_> {
    /// Set `key` to `value` until this binding is dropped.
    pub(crate) fn set(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        let key = key.as_ref();
        self.save(key);
        // SAFETY: serialized behind the guard `bind_env` required, and undone
        // by this binding's `Drop` on the unwinding path as well as the normal
        // one.
        unsafe { std::env::set_var(key, value) };
        self
    }

    /// Unset `key` until this binding is dropped.
    ///
    /// The mirror of [`set`](Self::set), and needed just as often: several
    /// tests here assert what happens with *no* override installed, which is a
    /// claim about the developer's environment unless the variable is cleared
    /// for the duration — and clearing it is the same process-global mutation,
    /// with the same obligation to put it back.
    pub(crate) fn remove(mut self, key: impl AsRef<OsStr>) -> Self {
        let key = key.as_ref();
        self.save(key);
        // SAFETY: as in `set`.
        unsafe { std::env::remove_var(key) };
        self
    }

    /// Run `invalidate` now and again once the variables are restored.
    ///
    /// Both ends, because a cached read is as process-global as the variable it
    /// came from: a value cached before the override answers from the old
    /// environment, and one cached during it answers the *next* test from an
    /// environment that no longer exists. `bind_test_binary` drops the sidecar
    /// slot on the way in for precisely this reason; this is the same rule for
    /// the caches that sit in front of these variables.
    ///
    /// Call it after the [`set`](Self::set) calls it belongs to — the builder
    /// runs in the order it is written.
    pub(crate) fn invalidating(mut self, invalidate: impl Fn() + 'static) -> Self {
        invalidate();
        self.invalidations.push(Box::new(invalidate));
        self
    }

    fn save(&mut self, key: &OsStr) {
        self.saved.push((key.to_os_string(), std::env::var_os(key)));
    }
}

impl Drop for EnvBinding<'_> {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..).rev() {
            match value {
                // SAFETY: still serialized — the binding borrows the guard, so
                // the guard outlives this drop.
                Some(value) => unsafe { std::env::set_var(&key, value) },
                None => unsafe { std::env::remove_var(&key) },
            }
        }
        // After the restore, never before: an invalidation that ran first would
        // repopulate the cache from the environment this binding is about to
        // take away.
        for invalidate in &self.invalidations {
            invalidate();
        }
    }
}

/// Git settings every test-owned git config carries.
///
/// Git 2.54's `commit` runs `maintenance run --auto`, whose reflog-expire task
/// deletes reflog entries older than the expiry once 100 have aged out.
/// Fixtures date their commits at fixed epochs that age past it, so the same
/// test lost 200 of its 251 entries on the day the epoch turned 30 days old. A
/// test that points `GIT_CONFIG_GLOBAL` at a config of its own (an empty file,
/// `/dev/null`, a mailmap) drops the hermetic config and with it this pin, so
/// every such config is written through [`write_git_config`], or points at
/// [`hermetic::pinned_git_config`].
pub(crate) const GIT_MAINTENANCE_PIN: &str = "[maintenance]\n\tauto = false\n[gc]\n\tauto = 0\n\treflogExpire = never\n\treflogExpireUnreachable = never\n";

/// Writes `path` as a git config holding [`GIT_MAINTENANCE_PIN`] followed by
/// `extra` (more git config text, or empty).
pub(crate) fn write_git_config(path: &std::path::Path, extra: &str) -> std::io::Result<()> {
    std::fs::write(path, format!("{GIT_MAINTENANCE_PIN}{extra}"))
}

/// Names the per-process root. Its presence means this process is already
/// hermetic: either a second `include!` of this file in the same crate ran the
/// constructor first, or this process was spawned by a hermetic test process
/// and inherited its environment, including any `HOME` the parent deliberately
/// pointed it at.
const ROOT_MARKER: &str = "GITPULSE_TEST_HERMETIC_ROOT";
/// The developer's real `HOME`, kept so a canary can prove nothing resolves
/// under it. Informational only; nothing reads state from it.
const REAL_HOME_MARKER: &str = "GITPULSE_TEST_REAL_HOME";
/// The pid whose constructor has already started the disk-write guard.
const GUARDED_MARKER: &str = "GITPULSE_TEST_GUARDED_PID";
/// The process that created the root, and so the only one that removes it.
const OWNER_MARKER: &str = "GITPULSE_TEST_HERMETIC_OWNER";

/// One hermetic environment for every test process.
///
/// A test suite that reads the developer's machine passes or fails on facts
/// about that machine: a `core.hooksPath` or `init.defaultBranch` in their
/// `~/.gitconfig` applies inside every fixture repository, `~/.claude/sessions`
/// and the live process table decide what `insights` reports, the real
/// Application Support trust store answers `repository_trust`, and
/// `tools.json` decides which `devmap` and `manvi` get resolved. None of that
/// fails loudly. It makes the suite pass on one laptop and fail on another, or
/// — worse — grants a fixture repository a persistent trust record in the
/// developer's real store.
///
/// The environment is rewritten once, by a constructor that runs before
/// `main`, so it is single-threaded and so cannot race a reader: no test has to
/// remember to call anything, and a new suite that includes this file is
/// hermetic from its first test. Doing it per test would be a `set_var` on a
/// live multi-threaded process, which is the hazard [`bind_env`] exists to
/// contain.
///
/// Scoped overrides ([`bind_env`]) still work and restore to the hermetic
/// value, not to the developer's.
pub(crate) mod hermetic {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    /// The per-process root every resolver below is pointed under.
    pub(crate) fn root() -> PathBuf {
        PathBuf::from(
            std::env::var_os(super::ROOT_MARKER)
                .expect("the hermetic constructor did not run in this test process"),
        )
    }

    /// The pinned global git config the constructor wrote, by path under the
    /// root rather than by the live `GIT_CONFIG_GLOBAL`, which a scoped
    /// `bind_env` may be pointing elsewhere at that moment. A test that wants an
    /// identity-free, maintenance-free git config of its own passes this to
    /// `GIT_CONFIG_GLOBAL` instead of `/dev/null` or an empty file.
    pub(crate) fn pinned_git_config() -> PathBuf {
        root().join("gitconfig")
    }

    /// The hermetic `HOME`, by path under the root rather than by the live
    /// `HOME`, which a scoped `bind_env` may be pointing at a directory it is
    /// about to delete.
    pub(crate) fn home() -> PathBuf {
        root().join("home")
    }

    /// Points a fixture's `git` child at the pinned config and the hermetic
    /// home, named outright.
    ///
    /// A fixture command that inherits the process environment reads whatever
    /// `HOME`, `XDG_CONFIG_HOME` and `GIT_CONFIG_GLOBAL` hold at that instant.
    /// Those are process-wide, and a neighbouring test that rebinds them (a
    /// user-level mailmap, a fake home) holds the environment lock, which a
    /// fixture helper does not take. The helper then reads the neighbour's
    /// config, or a config file the neighbour has just deleted, and git dies
    /// with "unknown error occurred while reading the configuration files".
    /// Only the product call under test should see a rebound environment.
    pub(crate) fn pin_git(command: &mut std::process::Command) -> &mut std::process::Command {
        let home = home();
        command
            .env("GIT_CONFIG_GLOBAL", pinned_git_config())
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("USERPROFILE", &home)
            .env("HOME", home)
    }

    /// The `HOME` the developer's shell had before the constructor replaced it.
    pub(crate) fn real_home() -> Option<PathBuf> {
        std::env::var_os(super::REAL_HOME_MARKER)
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
    }

    /// Variables that, set in the developer's shell or leaked by a git hook that
    /// launched the test run, redirect git or point GitPulse at their state.
    const SCRUBBED: &[&str] = &[
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_SYSTEM",
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_PREFIX",
        "GIT_NAMESPACE",
        "GITPULSE_TOOL_CONFIG",
        "GITPULSE_TRANSCRIPT_ROOT",
        "GITPULSE_CODEX_TRANSCRIPT_ROOT",
        "CODEX_HOME",
        "GITPULSE_BACKUPS_DIR",
    ];

    /// Toolchain homes that default to a directory under `HOME`. The tests
    /// spawn `cargo`, `rustc` and `go` through rustup shims that find their
    /// toolchain there, so moving `HOME` must not move them.
    fn keep_toolchain_homes(real_home: &Path) {
        for (key, relative) in [("CARGO_HOME", ".cargo"), ("RUSTUP_HOME", ".rustup")] {
            if std::env::var_os(key).is_none() {
                // SAFETY: runs in the pre-`main` constructor, single-threaded.
                unsafe { std::env::set_var(key, real_home.join(relative)) };
            }
        }
    }

    pub(super) extern "C" fn install() {
        // Not `static`: a second include of this file in the same crate has its
        // own copy of every static, and the environment is the one thing both
        // copies share.
        let pid = std::process::id().to_string();
        if std::env::var_os(super::GUARDED_MARKER).is_none_or(|guarded| guarded != *pid) {
            // SAFETY: single-threaded pre-`main` constructor.
            unsafe { std::env::set_var(super::GUARDED_MARKER, &pid) };
            super::diskio::start_watchdog();
            // SAFETY: registers an `extern "C"` function with no captured state.
            unsafe { atexit(super::diskio::report_at_exit) };
        }
        if std::env::var_os(super::ROOT_MARKER).is_some() {
            return;
        }
        // Canonical, so no path under it crosses a symlink: macOS's temp dir is
        // `/var/folders/..`, and `/var` is a link to `/private/var`, which the
        // storage cleaner refuses as a root ("Symlink paths are not eligible").
        let temp = std::env::temp_dir();
        let temp = std::fs::canonicalize(&temp).unwrap_or(temp);
        let root = temp.join(format!(
            "gitpulse-hermetic-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        let home = root.join("home");
        let dirs = [
            home.clone(),
            home.join(".config"),
            home.join(".cache"),
            home.join(".local").join("share"),
            home.join(".claude"),
            home.join("AppData").join("Roaming"),
            home.join("AppData").join("Local"),
        ];
        for dir in &dirs {
            // A root that cannot be created means the test process has no
            // hermetic home to run in. Running on regardless would read the
            // developer's, which is the thing this exists to prevent.
            std::fs::create_dir_all(dir)
                .unwrap_or_else(|error| panic!("hermetic test root {}: {error}", dir.display()));
        }
        let gitconfig = root.join("gitconfig");
        super::write_git_config(&gitconfig, "")
            .unwrap_or_else(|error| panic!("hermetic gitconfig {}: {error}", gitconfig.display()));
        // The application's own git runs with `GIT_CONFIG_GLOBAL` stripped and
        // reads `$HOME/.gitconfig` instead, so the pin has to be there too.
        let home_config = home.join(".gitconfig");
        super::write_git_config(&home_config, "").unwrap_or_else(|error| {
            panic!("hermetic gitconfig {}: {error}", home_config.display())
        });

        let real_home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from);
        if let Some(real_home) = &real_home {
            keep_toolchain_homes(real_home);
            // SAFETY: single-threaded pre-`main` constructor, as above.
            unsafe { std::env::set_var(super::REAL_HOME_MARKER, real_home) };
        }

        let set = |key: &str, value: &Path| unsafe { std::env::set_var(key, value) };
        set("HOME", &home);
        set("USERPROFILE", &home);
        set("APPDATA", &home.join("AppData").join("Roaming"));
        set("LOCALAPPDATA", &home.join("AppData").join("Local"));
        set("XDG_CONFIG_HOME", &home.join(".config"));
        set("XDG_CACHE_HOME", &home.join(".cache"));
        set("XDG_DATA_HOME", &home.join(".local").join("share"));
        set("CLAUDE_CONFIG_DIR", &home.join(".claude"));
        set("GIT_CONFIG_GLOBAL", &gitconfig);
        // SAFETY: as above.
        unsafe {
            std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
            for key in SCRUBBED {
                std::env::remove_var(key);
            }
            std::env::set_var(super::OWNER_MARKER, std::process::id().to_string());
            std::env::set_var(super::ROOT_MARKER, &root);
            atexit(remove_root);
        }
    }

    extern "C" {
        fn atexit(callback: extern "C" fn()) -> i32;
    }

    /// Removes the root at process exit, from the process that made it only: a
    /// child that inherited the marker must not delete its parent's home.
    extern "C" fn remove_root() {
        let ours = std::env::var_os(super::OWNER_MARKER)
            .is_some_and(|owner| owner == OsString::from(std::process::id().to_string()));
        if let (true, Some(root)) = (ours, std::env::var_os(super::ROOT_MARKER)) {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

/// Wall-clock limits in tests go through this, and only this.
///
/// Two things make a limit that holds on a quiet laptop fail without the code
/// changing, and both scale it here. Coverage instrumentation makes sub-second
/// process and thread deadlines flake, so under `cargo llvm-cov` a limit is
/// multiplied (and floored at five seconds). A machine busier than it has cores
/// (a parallel suite on a small runner, a build beside the tests) stretches
/// every process start and fs call, so a limit is multiplied by how
/// oversubscribed the host is, up to [`MAX_LOAD_FACTOR`]. Production timeouts
/// stay unchanged; only tests call this. A raw `Duration::from_millis(400)` in
/// an assertion is a limit nobody scaled, which is how a suite that is green on
/// a quiet laptop goes red on an instrumented or loaded runner;
/// `tests/hermetic_contract.rs` refuses new ones.
pub(crate) fn coverage_relaxed(duration: std::time::Duration) -> std::time::Duration {
    relaxed_for(duration, instrumented(), host_load())
}

/// The most a limit is stretched for host load. Bounded so a limit that exists
/// to prove something did *not* wait (a deadline far shorter than the timeout
/// it is racing) still discriminates on a busy machine.
pub(crate) const MAX_LOAD_FACTOR: f64 = 4.0;

fn instrumented() -> bool {
    std::env::var_os("CARGO_LLVM_COV").is_some() || std::env::var_os("LLVM_PROFILE_FILE").is_some()
}

/// [`coverage_relaxed`] with its two inputs supplied, so the rule is testable
/// without instrumenting or loading the machine running the test.
pub(crate) fn relaxed_for(
    duration: std::time::Duration,
    instrumented: bool,
    load: Option<(f64, usize)>,
) -> std::time::Duration {
    let mut limit = duration;
    if instrumented {
        limit = limit
            .saturating_mul(8)
            .max(std::time::Duration::from_secs(5));
    }
    if let Some((one_minute, cpus)) = load {
        let factor = (one_minute / cpus.max(1) as f64).clamp(1.0, MAX_LOAD_FACTOR);
        if factor.is_finite() {
            limit = limit.mul_f64(factor);
        }
    }
    limit
}

/// The one-minute load average and the number of cores, where the OS reports
/// them. `None` is "unknown", which scales nothing.
pub(crate) fn host_load() -> Option<(f64, usize)> {
    #[cfg(unix)]
    {
        extern "C" {
            fn getloadavg(average: *mut f64, count: i32) -> i32;
        }
        let mut average = [0.0_f64; 1];
        // SAFETY: `average` has room for the one sample requested.
        let read = unsafe { getloadavg(average.as_mut_ptr(), 1) };
        let cpus = std::thread::available_parallelism().ok()?.get();
        (read == 1).then_some((average[0], cpus))
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(test)]
mod relaxed_tests {
    #[test]
    fn a_quiet_plain_run_is_unscaled() {
        let limit = std::time::Duration::from_millis(400);
        assert_eq!(super::relaxed_for(limit, false, None), limit);
        assert_eq!(super::relaxed_for(limit, false, Some((3.0, 8))), limit);
        // Fewer runnable tasks than cores is not "load".
        assert_eq!(super::relaxed_for(limit, false, Some((0.0, 8))), limit);
    }

    #[test]
    fn an_oversubscribed_host_stretches_the_limit_by_its_ratio_up_to_the_cap() {
        let limit = std::time::Duration::from_secs(2);
        assert_eq!(
            super::relaxed_for(limit, false, Some((16.0, 8))),
            std::time::Duration::from_secs(4)
        );
        assert_eq!(
            super::relaxed_for(limit, false, Some((800.0, 8))),
            limit.mul_f64(super::MAX_LOAD_FACTOR)
        );
        // Nonsense from the OS scales nothing.
        assert_eq!(super::relaxed_for(limit, false, Some((f64::NAN, 8))), limit);
        assert_eq!(
            super::relaxed_for(limit, false, Some((16.0, 0))),
            limit.mul_f64(super::MAX_LOAD_FACTOR)
        );
    }

    #[test]
    fn instrumentation_and_load_compose() {
        let limit = std::time::Duration::from_millis(100);
        assert_eq!(
            super::relaxed_for(limit, true, Some((16.0, 8))),
            std::time::Duration::from_secs(10)
        );
    }

    #[test]
    fn the_host_reports_a_load_the_scaling_can_use() {
        if let Some((load, cpus)) = super::host_load() {
            assert!(load >= 0.0 && cpus >= 1, "{load} on {cpus}");
        }
    }
}

/// What a test process has written to storage, from the operating system's own
/// counters, and the guard built on it.
///
/// A suite that writes 549.8 GiB in 90 minutes (a `.diag` disk-writes report for
/// a `gitpulse_lib` test binary, 101 MB/s) is burning SSD endurance the machine
/// does not get back, and nothing in a passing run says so. Wall-clock limits
/// do not catch it: the process is busy, not hung. So the process is held to a
/// byte budget instead:
///
/// * [`bytes_written`] reads the kernel's per-process counter
///   (`ri_diskio_byteswritten` from `proc_pid_rusage` on macOS, `ru_oublock`
///   from `getrusage(RUSAGE_SELF)` on Linux). It counts what reached the block
///   layer from this process and not its children, so a loop rewriting one
///   file counts every rewrite and a spawned `git` counts against its own
///   process, which runs no watchdog of its own; a re-executed test binary does.
/// * A watchdog thread samples it and, when the process averages more than
///   [`MAX_WRITE_RATE`] over [`RATE_WINDOW`] or has written more than
///   [`MAX_TOTAL_WRITES`] in all, prints the numbers and aborts the process:
///   the test run fails loudly and quickly instead of finishing green an hour
///   later. Both limits are far above any measured test (see
///   `scripts/test-disk-writes.mjs`, which attributes bytes to single tests).
/// * `GITPULSE_TEST_DISKIO_REPORT=<file>` appends one line per process at exit
///   (`<bytes>\t<argv>`), which is how that script measures one test per
///   process.
pub(crate) mod diskio {
    use std::time::{Duration, Instant};

    /// Sustained write rate that fails the process. The whole suite at eight
    /// test threads averaged up to 41 MiB/s over 30 s on fec1e008, so a 40
    /// MiB/s ceiling aborted healthy runs. It has since grown: the 8-thread
    /// Ubuntu lib suite averaged 84 MiB/s and the instrumented coverage suite
    /// 89 MiB/s (runs 40346884169 and 40346884031). Those are the suite, not
    /// one runaway. 160 MiB/s stays above that peak and still aborts a process
    /// writing about twice what the suite measured.
    pub(crate) const MAX_WRITE_RATE: u64 = 160 * 1024 * 1024;
    /// How long the rate is averaged over, so a legitimate burst (a fixture
    /// repository, a build of a cache) is not mistaken for a runaway.
    pub(crate) const RATE_WINDOW: Duration = Duration::from_secs(30);
    /// Bytes one test process may write in total.
    pub(crate) const MAX_TOTAL_WRITES: u64 = 8 * 1024 * 1024 * 1024;
    const SAMPLE_EVERY: Duration = Duration::from_millis(500);
    /// `0` disables the watchdog, for a run that is meant to write. Any other
    /// number replaces both limits with that many MiB (total) and MiB/s (rate),
    /// which is how the guard's own test gives a child a budget it can break in
    /// a second.
    pub(crate) const BUDGET_ENV: &str = "GITPULSE_TEST_WRITE_BUDGET";

    pub(crate) const REPORT_ENV: &str = "GITPULSE_TEST_DISKIO_REPORT";

    /// The limits in force, or `None` when the watchdog is off.
    #[derive(Debug, PartialEq, Eq)]
    pub(crate) struct Limits {
        /// Bytes per second, averaged over `window`.
        pub(crate) rate: u64,
        pub(crate) window: Duration,
        /// Bytes in all.
        pub(crate) total: u64,
    }

    pub(crate) fn limits(setting: Option<&str>) -> Option<Limits> {
        let default = Limits {
            rate: MAX_WRITE_RATE,
            window: RATE_WINDOW,
            total: MAX_TOTAL_WRITES,
        };
        match setting.map(str::trim) {
            None | Some("") => Some(default),
            Some(text) => match text.parse::<u64>() {
                Ok(0) => None,
                Ok(mib) => Some(Limits {
                    rate: mib << 20,
                    window: Duration::from_secs(1),
                    total: mib << 20,
                }),
                // An unparseable budget must not silently switch the guard off.
                Err(_) => Some(default),
            },
        }
    }

    /// Bytes this process has written to storage, or `None` where the platform
    /// offers no counter (the caller must treat that as "unmeasured", never as
    /// zero).
    #[cfg(target_os = "macos")]
    pub(crate) fn bytes_written() -> Option<u64> {
        // `struct rusage_info_v2` from `<sys/resource.h>`: a uuid, sixteen
        // 64-bit counters, then the two disk counters.
        #[repr(C)]
        struct RusageInfoV2 {
            uuid: [u8; 16],
            counters: [u64; 16],
            diskio_bytesread: u64,
            diskio_byteswritten: u64,
        }
        extern "C" {
            fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut RusageInfoV2) -> i32;
        }
        const RUSAGE_INFO_V2: i32 = 2;
        let mut info = RusageInfoV2 {
            uuid: [0; 16],
            counters: [0; 16],
            diskio_bytesread: 0,
            diskio_byteswritten: 0,
        };
        // SAFETY: `info` is a correctly laid-out, writable `rusage_info_v2`
        // that outlives the call; the kernel fills it for our own pid.
        let status =
            unsafe { proc_pid_rusage(std::process::id() as i32, RUSAGE_INFO_V2, &mut info) };
        (status == 0).then_some(info.diskio_byteswritten)
    }

    /// `ru_oublock` is the kernel's `write_bytes` in 512-byte units, for this
    /// process alone: reaped children go to `RUSAGE_CHILDREN`. `/proc/self/io`
    /// adds them in, which charged a test for every `git` it ran.
    #[cfg(target_os = "linux")]
    pub(crate) fn bytes_written() -> Option<u64> {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: `usage` is a writable `rusage`; the kernel fills it for
        // this process and returns 0, or leaves it and returns -1.
        let status = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
        if status != 0 {
            return None;
        }
        // SAFETY: zeroed and then filled by a successful `getrusage`.
        let blocks = unsafe { usage.assume_init() }.ru_oublock;
        u64::try_from(blocks).ok().map(|blocks| blocks * 512)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub(crate) fn bytes_written() -> Option<u64> {
        None
    }

    /// Whether a sample history breaks the budget, and why. Pure, so the
    /// policy is tested without writing a byte.
    pub(crate) fn verdict(samples: &[(Duration, u64)], limits: &Limits) -> Option<String> {
        let (max_rate, window, max_total) = (limits.rate, limits.window, limits.total);
        let (now, written) = *samples.last()?;
        if written > max_total {
            return Some(format!(
                "this test process has written {} MiB, over its {} MiB budget",
                written >> 20,
                max_total >> 20
            ));
        }
        // The oldest sample at least `window` old; none yet means the process
        // is too young to have a rate worth judging.
        let (then, before) = samples
            .iter()
            .rev()
            .find(|(at, _)| now.saturating_sub(*at) >= window)
            .copied()?;
        let seconds = now.saturating_sub(then).as_secs_f64();
        let rate = (written.saturating_sub(before)) as f64 / seconds;
        (rate > max_rate as f64).then(|| {
            format!(
                "this test process averaged {:.0} MiB/s of disk writes over {seconds:.0} s, over \
                 its {} MiB/s budget",
                rate / (1024.0 * 1024.0),
                max_rate >> 20
            )
        })
    }

    fn append_report() {
        let (Some(path), Some(bytes)) = (std::env::var_os(REPORT_ENV), bytes_written()) else {
            return;
        };
        let argv: Vec<String> = std::env::args().skip(1).collect();
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            use std::io::Write;
            // One `write` call: `writeln!` on an unbuffered file issues one per
            // piece, and concurrent test processes interleave those pieces.
            let line = format!("{bytes}\t{}\n", argv.join(" "));
            let _ = file.write_all(line.as_bytes());
        }
    }

    pub(super) extern "C" fn report_at_exit() {
        append_report();
    }

    /// Starts the watchdog. A thread that cannot be started leaves the process
    /// unguarded, which is said out loud rather than assumed fine.
    pub(super) fn start_watchdog() {
        let Some(limits) = limits(std::env::var(BUDGET_ENV).ok().as_deref()) else {
            return;
        };
        if bytes_written().is_none() {
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("gitpulse-write-watchdog".into())
            .spawn(move || {
                let started = Instant::now();
                let mut samples: Vec<(Duration, u64)> = Vec::new();
                loop {
                    std::thread::sleep(SAMPLE_EVERY);
                    let Some(written) = bytes_written() else { return };
                    samples.push((started.elapsed(), written));
                    // Older than twice the window is never consulted.
                    let keep_from = samples
                        .iter()
                        .position(|(at, _)| {
                            started.elapsed().saturating_sub(*at) <= limits.window * 2
                        })
                        .unwrap_or(0);
                    samples.drain(..keep_from);
                    if let Some(why) = verdict(&samples, &limits) {
                        eprintln!(
                            "\nDISK WRITE BUDGET EXCEEDED: {why}. A test is writing far more than any \
                             measured test does; aborting the run. Find it with \
                             `node scripts/test-disk-writes.mjs`; set {BUDGET_ENV}=0 to disable."
                        );
                        std::process::abort();
                    }
                }
            });
        if let Err(error) = spawned {
            eprintln!("warning: disk-write watchdog not started: {error}");
        }
    }
}

/// Runs [`hermetic::install`] before `main`, in the test binary that includes
/// this file.
#[used]
#[cfg_attr(
    any(target_os = "linux", target_os = "android", target_os = "freebsd"),
    link_section = ".init_array"
)]
#[cfg_attr(target_vendor = "apple", link_section = "__DATA,__mod_init_func")]
#[cfg_attr(windows, link_section = ".CRT$XCU")]
static HERMETIC_CONSTRUCTOR: extern "C" fn() = hermetic::install;

#[cfg(test)]
mod tests {
    // A crate with `harness = false` compiles this module without its `#[test]`
    // functions, which would leave these imports unused there.
    #![allow(unused_imports)]
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    const PROBE: &str = "GITPULSE_TEST_ENV_BINDING_PROBE";

    /// Regression (a panic leaks a process-global override): the whole point of
    /// the type. A test that fails while holding an override must leave the
    /// environment exactly as it found it, because the next test to read that
    /// variable has no way to tell an override from the truth — and will fail
    /// somewhere else entirely, looking like an unrelated flake.
    ///
    /// Asserted for both starting states, because "restore" means two different
    /// operations: putting a value back, and taking one away.
    #[test]
    fn a_failing_test_cannot_leave_its_env_override_installed() {
        let serial = env_serial();
        // SAFETY: serialized behind `serial`; both arms below restore it.
        unsafe { std::env::remove_var(PROBE) };

        let invalidated = std::sync::Arc::new(AtomicUsize::new(0));

        // Starting absent: the override must be gone afterwards, not empty.
        let counter = std::sync::Arc::clone(&invalidated);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _env = bind_env(&serial)
                .set(PROBE, "installed")
                .invalidating(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                });
            assert_eq!(
                std::env::var(PROBE).as_deref(),
                Ok("installed"),
                "the fixture must install an override before it fails"
            );
            panic!("simulated failure while holding the environment override");
        }));
        assert!(outcome.is_err(), "the fixture must actually have failed");
        assert_eq!(
            std::env::var_os(PROBE),
            None,
            "the override outlived the test that installed it; every later test \
             reading this variable would answer from a value nobody meant it to see"
        );
        assert_eq!(
            invalidated.load(Ordering::SeqCst),
            2,
            "the cache must be dropped on the way in and again on the way out, \
             including when the way out is a panic"
        );

        // Starting present: restoring is putting the old value back, and a
        // binding that only knew how to `remove_var` would pass the case above
        // while destroying a real setting here.
        // SAFETY: serialized behind `serial`; removed at the end of the test.
        unsafe { std::env::set_var(PROBE, "pre-existing") };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _env = bind_env(&serial).set(PROBE, "installed").remove(PROBE);
            assert_eq!(std::env::var_os(PROBE), None);
            panic!("simulated failure while holding the environment override");
        }));
        assert!(outcome.is_err(), "the fixture must actually have failed");
        assert_eq!(
            std::env::var(PROBE).as_deref(),
            Ok("pre-existing"),
            "restoring must put the previous value back, not unset the variable"
        );

        // SAFETY: serialized behind `serial`, which is still held here.
        unsafe { std::env::remove_var(PROBE) };
    }
}
