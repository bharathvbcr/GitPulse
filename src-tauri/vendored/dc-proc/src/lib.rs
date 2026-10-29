//! One bounded subprocess, for every place the kernel shells out.
//!
//! The kernel asks `git` three questions — `HEAD` for the store's freshness
//! sentinel, `HEAD` and `ls-files` for the artifact digests, `log` for churn —
//! and until this module existed each asker ran its own child process its own
//! way. `devmap-store` drained both pipes on threads and killed at five
//! seconds; `devmap-query`'s digests called `Command::output()` with no bound
//! at all, on the path every artifact write takes — the hang the store's
//! deadline exists to prevent, reintroduced one crate up; and the churn reader
//! had its own drain thread and its own cap, closing the pipe at the cap and
//! letting `SIGPIPE` end git. Correct, and a third copy of the discipline.
//!
//! Three runners is how three disciplines drift. This is the one, and the
//! properties every caller gets by going through it:
//!
//! * **A wall-clock deadline.** The child is killed and reaped when it expires,
//!   whether it is still writing, has closed its pipes and hung, or is waiting
//!   on a terminal that is not there.
//! * **An output cap that keeps the child honest.** Bytes past the cap are
//!   read and discarded rather than the pipe being closed under the child, so
//!   it finishes on its own terms and its exit status still means what it
//!   says; the caller learns that the answer is incomplete — never a truncated
//!   answer presented as whole. A pipe that stops answering before EOF is
//!   reported the same way, because the caller is holding the same thing: a
//!   prefix.
//! * **Both pipes drained concurrently**, because reading them after exit
//!   deadlocks the moment either buffer fills.
//! * **No terminal, no stdin.** `stdin` is `/dev/null`; the `git` constructor
//!   below also refuses interactive prompts and optional locks.
//!
//! The deadline reaches the child's *descendants*, not only the child. On unix
//! the child leads its own process group and the expiry signals the group, so a
//! hook, a credential helper, a pager or an `sh -c` fan-out dies with the
//! process that started it. Killing one pid instead used to leave two problems
//! behind, and they are the same problem: the descendant ran on past a deadline
//! the caller had been told was enforced, and — still holding the inherited
//! write ends — it left both drain threads blocked in `read` on a process the
//! runner never started. The runner returned `Deadline` without them: two
//! threads and two descriptors per expiry, freed only when the descendant chose
//! to exit, in a daemon that runs `git` on a schedule. After the group kill the
//! threads are given a bounded chance to see EOF and come home, so the common
//! case hands nothing back. A descendant that called `setsid` has left the group
//! and can still hold a pipe past that window; that is the residual, and the
//! runner still returns on time when it happens.
//!
//! **The trade-off, stated plainly.** A child in its own process group no longer
//! receives the terminal's `SIGINT`: interrupting a foreground `devmap` leaves an
//! in-flight child to finish on its own, or — in the hung case — to linger with
//! nobody left to enforce the deadline, because the enforcer was the process the
//! user just interrupted. It is bought deliberately. Every caller today is a
//! `git` read with no stdin and `GIT_TERMINAL_PROMPT=0`, which finishes in
//! milliseconds or is the pathological case this bound exists for; `devmap serve`
//! handles `SIGTERM`/`SIGINT` itself and lets in-flight runs complete. And an
//! interrupted parent takes the read ends of both pipes with it, so a child that
//! is still *writing* dies of `SIGPIPE` on its next write — leaving only the
//! child that has gone quiet, which is the one the deadline could not have
//! helped either. The alternative — a signal handler — is process-global state,
//! and a library that installs one takes that decision away from every binary
//! that links it.
//!
//! What it deliberately does not decide is what a failure *means*: the store
//! treats an unavailable `HEAD` as "unavailable", the digests fall back to the
//! two fingerprints, churn reports `computed: false` with the reason. Each
//! caller keeps its contract and gains the bounds.

pub mod diskio;

use std::ffi::OsStr;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Wall clock allowed for `git rev-parse HEAD`, wherever the kernel asks it.
///
/// `git` can stall on pathological repositories, network mounts or hook
/// misconfigurations; unbounded, it hung every drain batch and CLI status
/// behind it. On expiry the child is killed and the caller gets an error —
/// every asker already treats an unavailable head as "unavailable", so a
/// stalled git degrades honestly instead of wedging the daemon. One constant
/// for the store's sentinel and the artifacts' digest, so the two cannot
/// disagree about how long a head is worth waiting for.
pub const GIT_HEAD_DEADLINE: Duration = Duration::from_secs(5);

/// How long the runner waits, after killing the child's process group, for the
/// two drain threads to see EOF and finish.
///
/// Bounded on purpose and spent only on an expiry — the path that has already
/// cost a full deadline — so it buys the common case back its threads and its
/// descriptors without letting a descendant that escaped the group (`setsid`)
/// hold the runner past its promise. Exceeding it is the old behaviour, not a
/// new failure: the threads are handed back to nobody, exactly as before.
const DRAIN_HANDBACK: Duration = Duration::from_millis(500);

/// How much of a child the caller is prepared to wait for and to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    /// Wall clock from spawn to exit. On expiry the child is killed.
    pub deadline: Duration,
    /// Bytes of stdout kept. Anything past it is drained and dropped, and
    /// [`Captured::stdout_truncated`] says so.
    pub stdout_cap: usize,
    /// Bytes of stderr kept, the same way.
    pub stderr_cap: usize,
}

/// What a child that exited within its bounds left behind.
#[derive(Debug)]
pub struct Captured {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    /// The bytes here are a *prefix* of what the child had to say: it wrote
    /// more than the cap, or the pipe stopped answering before EOF. One flag
    /// for both, because every caller asks the same question of it — "are these
    /// bytes the whole answer" — and there is no reading of that question where
    /// a pipe that failed mid-stream counts as yes.
    pub stdout_truncated: bool,
    pub stderr: Vec<u8>,
    pub stderr_truncated: bool,
    /// Spawn to reap.
    pub elapsed: Duration,
}

impl Captured {
    /// stdout as text, lossily. Truncation is still the caller's to check.
    pub fn stdout_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// stderr as trimmed text, for error messages.
    pub fn stderr_trimmed(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_string()
    }
}

/// Why no [`Captured`] came back. A non-zero exit is *not* a failure here —
/// the child ran and answered — so it arrives as a [`Captured`] with its
/// status, and each caller decides what a refusal means for it.
#[derive(Debug)]
pub enum Failure {
    /// The program could not be started at all.
    Spawn {
        program: String,
        error: std::io::Error,
    },
    /// The child was still running at the deadline and was killed.
    Deadline { program: String, deadline: Duration },
    /// The child could not be waited on.
    Wait {
        program: String,
        error: std::io::Error,
    },
    /// The read was refused before anything ran, for the reason given — a
    /// git that does not honour `GIT_NO_LAZY_FETCH` asked to read a partial
    /// clone, see [`git_with_program`].
    Refused { program: String, reason: String },
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Spawn { program, error } => write!(f, "cannot spawn {program}: {error}"),
            Failure::Deadline { program, deadline } => {
                write!(f, "{program} exceeded {deadline:?} and was killed")
            }
            Failure::Wait { program, error } => write!(f, "cannot wait for {program}: {error}"),
            Failure::Refused { program, reason } => write!(f, "refused to run {program}: {reason}"),
        }
    }
}

impl std::error::Error for Failure {}

/// A `git` invocation against `root`, with the hygiene every kernel call
/// wants: `-C root` rather than `current_dir` so a missing root is git's error
/// and not a spawn failure, no terminal prompt, and no optional index locks
/// (a read must never leave `index.lock` behind in someone's checkout).
///
/// The `git` it runs is found by [`resolve_outside`], and the arguments carry
/// [`guarded_args`]: neither the repository's files nor its `.git/config`
/// choose a program this read runs.
pub fn git<I, S>(root: &Path, args: I) -> Result<Command, Failure>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    git_with_program(OsStr::new("git"), root, args)
}

/// [`git`] with the program named, so a test can stand a script in for it.
pub fn git_with_program<I, S>(program: &OsStr, root: &Path, args: I) -> Result<Command, Failure>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let resolved = resolve_outside(program, std::env::var_os("PATH").as_deref(), root)?;
    let args: Vec<std::ffi::OsString> = args
        .into_iter()
        .map(|a| a.as_ref().to_os_string())
        .collect();
    let subcommand = args.get(subcommand_index(&args)).and_then(|s| s.to_str());
    let filter_free = subcommand.is_some_and(|s| FILTER_FREE.contains(&s));
    // Decision D9 in ft-ff2153d9: a git that does not honour
    // GIT_NO_LAZY_FETCH may read an ordinary clone, and refuses a partial
    // clone's read. The version is asked only when it decides something: a
    // filter-free read skips the config listing on a git that honours the
    // variable, and any other read lists the config anyway and asks the
    // version only when the listing shows a promisor.
    let mut filters = Vec::new();
    let honours = if filter_free {
        git_version(&resolved)?.is_some_and(|v| v.honors_no_lazy_fetch())
    } else {
        false
    };
    if !honours {
        let config = repository_config(&resolved, root)?;
        if config.partial_clone {
            let version = git_version(&resolved)?;
            if !version
                .as_ref()
                .is_some_and(GitVersion::honors_no_lazy_fetch)
            {
                let version = version
                    .map_or_else(|| "of unrecognised version".to_string(), |v| v.to_string());
                return Err(Failure::Refused {
                    program: resolved.display().to_string(),
                    reason: format!(
                        "{} is a partial clone, and git {version} does not honour \
                         GIT_NO_LAZY_FETCH, so a read could fetch missing objects from its \
                         promisor remote and run the programs that fetch runs; upgrade git to \
                         2.45.0 or later, or to a maintenance release carrying the CVE-2024-32465 \
                         fix (2.39.4, 2.40.2, 2.41.1, 2.42.2, 2.43.4, 2.44.1)",
                        root.display()
                    ),
                });
            }
        }
        if !filter_free {
            filters = config.filters;
        }
    }
    let mut command = Command::new(resolved);
    command
        .arg("-C")
        .arg(root)
        .args(filters)
        .args(guarded_args(args));
    git_env(&mut command);
    Ok(command)
}

/// The environment every git the kernel runs gets: no terminal prompt, no
/// optional index lock (a read must never leave `index.lock` behind in
/// someone's checkout), and no lazy fetch from a promisor remote, so a missing
/// object fails the read loudly instead of being fetched — the fetch runs
/// what the repository's config names (`core.sshCommand`, credential helpers,
/// remote helpers). Every pathspec the kernel passes is a repository file name,
/// so `GIT_LITERAL_PATHSPECS` stops a file called `:(glob)x`, `:!x` or `*.rs`
/// from being read as pathspec magic; `--` alone only ends option parsing. The
/// Go host's `proc.gitEnv` keeps its own list because its callers pass
/// `:(top,exclude)` on purpose. The glob, noglob and icase switches are global
/// pathspec settings too, and git refuses a literal setting alongside any of
/// them, so a user's inherited copies are removed.
fn git_env(command: &mut Command) {
    command
        .env_remove("GIT_GLOB_PATHSPECS")
        .env_remove("GIT_NOGLOB_PATHSPECS")
        .env_remove("GIT_ICASE_PATHSPECS");
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_LITERAL_PATHSPECS", "1");
}

/// The version a git binary reports, reduced to the numbers that decide what
/// it honours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    /// The whole `git version` line.
    pub raw: String,
}

impl fmt::Display for GitVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl GitVersion {
    /// Reads `git version 2.54.0 (Apple Git-157)`, `git version
    /// 2.39.5.windows.1` and the like. Only the leading numbers count; a
    /// missing patch level is zero.
    pub fn parse(line: &str) -> Option<GitVersion> {
        let line = line.trim();
        let rest = line.strip_prefix("git version ")?;
        let number = rest.split_whitespace().next()?;
        let mut parts = number.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        Some(GitVersion {
            major,
            minor,
            patch,
            raw: line.to_string(),
        })
    }

    /// Whether this git refuses to fetch a missing object from a promisor
    /// remote when `GIT_NO_LAZY_FETCH=1`.
    ///
    /// Checked against git's source rather than recalled (ft-ff2153d9): the
    /// variable is read in `environment.c` from v2.45.0, and in
    /// `promisor-remote.c` in the CVE-2024-32465 maintenance releases
    /// v2.39.4, v2.40.2, v2.41.1, v2.42.2, v2.43.4 and v2.44.1. v2.44.0 does
    /// not read it. The Go host's `proc.GitVersion.HonorsNoLazyFetch` is the
    /// same rule.
    pub fn honors_no_lazy_fetch(&self) -> bool {
        if (self.major, self.minor) >= (2, 45) {
            return true;
        }
        let backport = match (self.major, self.minor) {
            (2, 39) => 4,
            (2, 40) => 2,
            (2, 41) => 1,
            (2, 42) => 2,
            (2, 43) => 4,
            (2, 44) => 1,
            _ => return false,
        };
        self.patch >= backport
    }
}

/// Wall clock for `git version`. It answers in milliseconds; the bound is
/// short because the probe runs before the read a caller bounded, and must
/// not outlast the shortest of those deadlines ([`GIT_HEAD_DEADLINE`]). A
/// git that stalls here is reported as killed, not waited for.
pub const GIT_VERSION_DEADLINE: Duration = Duration::from_secs(2);

/// The version of the git at `git` (an absolute path, as [`resolve_outside`]
/// returns it), asked once per binary per process. A git replaced in place —
/// a different size or modification time — is asked again. `git version`
/// reads no repository, so it runs nothing a repository names.
///
/// `None` is a git that ran and gave no version this can read; callers treat
/// it as one that does not honour `GIT_NO_LAZY_FETCH`, the cautious reading.
/// A git that could not be run at all is a [`Failure`].
pub fn git_version(git: &Path) -> Result<Option<GitVersion>, Failure> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    type Key = (PathBuf, u64, Option<std::time::SystemTime>);
    static VERSIONS: OnceLock<Mutex<HashMap<Key, Option<GitVersion>>>> = OnceLock::new();
    let meta = std::fs::metadata(git).map_err(|error| Failure::Spawn {
        program: git.display().to_string(),
        error,
    })?;
    let key: Key = (git.to_path_buf(), meta.len(), meta.modified().ok());
    let versions = VERSIONS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(version) = versions
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key)
    {
        return Ok(version.clone());
    }
    let mut command = Command::new(git);
    command.arg("version").current_dir(std::env::temp_dir());
    git_env(&mut command);
    let captured = run_bounded(
        &mut command,
        Bounds {
            deadline: GIT_VERSION_DEADLINE,
            stdout_cap: 4096,
            stderr_cap: 4096,
        },
    )?;
    let version = if captured.status.success() && !captured.stdout_truncated {
        GitVersion::parse(&captured.stdout_lossy())
    } else {
        None
    };
    versions
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key, version.clone());
    Ok(version)
}

/// `args` with the guards every kernel git read carries.
///
/// A repository's own `.git/config` can name programs git runs during an
/// ordinary read, and a `.git/config` can arrive with the repository.
/// Verified against git 2.54: `core.fsmonitor` runs on `diff` and
/// `ls-files`, `diff.external` on `diff`, a diff driver's `textconv` on
/// `blame`, and `gpg.program` on `log` through `log.showSignature` when a
/// commit carries a signature header. A planted fsmonitor that answers
/// "nothing changed" also hides changes from the read. The guards switch
/// each of those off from the command line, which outranks every config file:
///
/// * before the subcommand: `core.fsmonitor=false`, `core.hooksPath` at the
///   null device (no hook can be found there), `log.showSignature=false`,
///   `core.useReplaceRefs=false` (a `git replace` ref rewrites history at read
///   time: a commit that adds a credential, replaced by a clean twin, is read
///   as the twin by every diff, log and show);
/// * after `diff`, `log` or `show`: `--no-ext-diff --no-textconv --no-color`;
/// * after `blame`: `--no-textconv`;
/// * after `diff` and `status`: `--ignore-submodules=dirty`. Asked whether a
///   submodule's working tree is dirty, git runs status inside it, and that
///   status runs the submodule's own filter drivers and fsmonitor, which the
///   superproject's config listing never sees. A submodule whose checked-out
///   commit moved is still reported. No kernel read compares a working tree
///   today; the Go host, which does, reports dirty submodules with a guarded
///   read of each (`proc.GitSession.DirtySubmodules`).
///
/// `--no-color` runs no program but closes the same kind of hole: `color.ui`
/// or `color.diff` set to `always` colors output even into a pipe, and a
/// reader recognizing `diff --git` or `@@` lines then recognizes none.
///
/// Global options the caller puts before its subcommand (`-c k=v`, `-C dir`)
/// stay in place; the subcommand is the first argument that is not one. The
/// Go host's `proc.GitArgs` applies the same rule.
///
/// Filter drivers need the repository's config read first, because no flag
/// disables them; [`git_with_program`] does that with
/// `repository_config`.
pub fn guarded_args<I, S>(args: I) -> Vec<std::ffi::OsString>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    const NULL_DEVICE: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let args: Vec<std::ffi::OsString> = args
        .into_iter()
        .map(|a| a.as_ref().to_os_string())
        .collect();
    let mut out: Vec<std::ffi::OsString> = [
        "-c".to_string(),
        "core.fsmonitor=false".to_string(),
        "-c".to_string(),
        format!("core.hooksPath={NULL_DEVICE}"),
        "-c".to_string(),
        "log.showSignature=false".to_string(),
        "-c".to_string(),
        "core.useReplaceRefs=false".to_string(),
    ]
    .into_iter()
    .map(std::ffi::OsString::from)
    .collect();
    let i = subcommand_index(&args);
    out.extend_from_slice(&args[..i]);
    if let Some(sub) = args.get(i) {
        out.push(sub.clone());
        let guards: &[&str] = match sub.to_str() {
            Some("diff") => &[
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--ignore-submodules=dirty",
            ],
            Some("log" | "show") => &["--no-ext-diff", "--no-textconv", "--no-color"],
            Some("blame") => &["--no-textconv"],
            Some("status") => &["--ignore-submodules=dirty"],
            _ => &[],
        };
        out.extend(guards.iter().map(std::ffi::OsString::from));
        out.extend_from_slice(&args[i + 1..]);
    }
    out
}

/// The index of the subcommand in `args`: the first argument that is not a
/// global option (`-c k=v` and `-C dir` take a value). `args.len()` when there
/// is none.
fn subcommand_index(args: &[std::ffi::OsString]) -> usize {
    let mut i = 0;
    while i < args.len() && args[i].as_encoded_bytes().starts_with(b"-") {
        if args[i] == "-c" || args[i] == "-C" {
            i += 1;
        }
        i += 1;
    }
    i.min(args.len())
}

/// Read subcommands that never pass a working-tree file through a filter
/// driver, so [`git_with_program`] skips reading the filter config for them —
/// which keeps `rev-parse HEAD` on every hook and churn's `log` at one spawn.
/// Listed this way round on purpose: a subcommand added later gets the
/// filter read until someone shows it cannot need one. The Go host's
/// `proc.filterFree` is the same list.
const FILTER_FREE: &[&str] = &["rev-parse", "rev-list", "log"];

/// The config listing read before a git read is bounded; one that does not
/// fit is refused, because a truncated listing could omit a driver.
const FILTER_CONFIG_LIMIT: usize = 1 << 20;

/// What one `git config` listing must carry: every filter driver setting, and
/// the settings that make the repository a partial clone, so detecting one
/// costs no extra spawn. Keys are matched in git's canonical form, section
/// and variable lowercased. The Go host's `proc.repositoryConfigPattern` is
/// the same pattern.
const REPOSITORY_CONFIG_PATTERN: &str =
    "^(filter\\.|extensions\\.partialclone$|remote\\..*\\.(promisor|partialclonefilter)$)";

/// What the guards need from the repository's config.
#[derive(Clone)]
struct RepositoryConfig {
    /// `-c` arguments neutralizing the repository's filter drivers.
    filters: Vec<String>,
    /// A promisor remote may supply missing objects on demand.
    partial_clone: bool,
}

/// The repository's filter and partial-clone config, listed once with
/// scopes. The Go host's `proc.readRepositoryConfig` applies the same rule;
/// see [`filter_overrides`] for it.
///
/// `filter.<name>.clean` runs on `diff`, and no flag turns filters off, so the
/// repository's config is read first (reading config runs nothing). A root
/// git cannot enter has no repository config to neutralize; the read that
/// follows reports the root itself.
fn probe_repository_config(git: &Path, root: &Path) -> Result<RepositoryConfig, Failure> {
    let program = git.display().to_string();
    let mut command = Command::new(git);
    command.arg("-C").arg(root).args([
        "config",
        "--show-scope",
        "-z",
        "--get-regexp",
        REPOSITORY_CONFIG_PATTERN,
    ]);
    git_env(&mut command);
    let captured = run_bounded(
        &mut command,
        Bounds {
            deadline: Duration::from_secs(10),
            stdout_cap: FILTER_CONFIG_LIMIT,
            stderr_cap: 4096,
        },
    )?;
    let refuse = |message: String| Failure::Spawn {
        program: program.clone(),
        error: std::io::Error::new(std::io::ErrorKind::InvalidData, message),
    };
    match captured.status.code() {
        Some(0) => {}
        // 1: nothing matched anywhere. 128: git cannot enter root, so there
        // is no repository config.
        Some(1 | 128) => {
            return Ok(RepositoryConfig {
                filters: Vec::new(),
                partial_clone: false,
            });
        }
        _ => {
            return Err(refuse(format!(
                "reading the repository's config exited {:?}: {}",
                captured.status.code(),
                captured.stderr_trimmed()
            )));
        }
    }
    if captured.stdout_truncated {
        return Err(refuse(format!(
            "the repository's filter config exceeds {FILTER_CONFIG_LIMIT} bytes; refusing to read \
             with filters it could not check"
        )));
    }
    Ok(RepositoryConfig {
        filters: filter_overrides(&captured.stdout).map_err(refuse)?,
        partial_clone: is_partial_clone(&captured.stdout),
    })
}

/// What one operation remembers about the repositories it reads: their
/// config listings, keyed by the git that listed them and the root it listed.
///
/// An operation — one `blast`, one `suspects`, one verify — runs dozens of git
/// reads against one repository, and each non-trivial read began by spawning
/// `git config` for the same answer. Opening a [`config_scope`] makes the
/// listing once per root for as long as the scope lives, on the thread that
/// opened it. Nothing outlives the operation: a config edited between two
/// operations is read afresh by the second, and inside one the entry is also
/// dropped when the repository's own `config` file changes size, modification
/// time or inode.
#[derive(Default)]
struct ConfigScope {
    entries: std::collections::HashMap<(PathBuf, PathBuf), (ConfigStamp, RepositoryConfig)>,
}

/// Entries one scope keeps; an operation reading more roots than this reads
/// the rest uncached.
const CONFIG_SCOPE_ENTRIES: usize = 32;

/// What a config file looked like when it was listed.
type ConfigStamp = (u64, Option<std::time::SystemTime>, u64);

thread_local! {
    static CONFIG_SCOPE: std::cell::RefCell<Option<ConfigScope>> =
        const { std::cell::RefCell::new(None) };
}

/// Guard returned by [`config_scope`]; dropping it ends the scope.
#[must_use = "the scope ends when this is dropped"]
pub struct ConfigScopeGuard {
    /// False for a scope opened inside another, which leaves the outer one's
    /// entries and lifetime alone.
    owner: bool,
}

impl Drop for ConfigScopeGuard {
    fn drop(&mut self) {
        if self.owner {
            CONFIG_SCOPE.with(|scope| *scope.borrow_mut() = None);
        }
    }
}

/// Opens an operation scope on this thread: until the guard drops, every
/// [`git_with_program`] lists a repository's filter and promisor config once.
/// Nested scopes share the outermost one.
pub fn config_scope() -> ConfigScopeGuard {
    let owner = CONFIG_SCOPE.with(|scope| {
        let mut scope = scope.borrow_mut();
        if scope.is_some() {
            false
        } else {
            *scope = Some(ConfigScope::default());
            true
        }
    });
    ConfigScopeGuard { owner }
}

/// The stamp of `root`'s repository config, or `None` when it cannot be read
/// as one file (a worktree's `.git` is a file pointing elsewhere); such a
/// repository is listed every time rather than cached on a guess.
fn config_stamp(root: &Path) -> Option<(PathBuf, ConfigStamp)> {
    let top = repository_top(root).ok()?;
    let meta = std::fs::metadata(top.join(".git").join("config")).ok()?;
    #[cfg(unix)]
    let inode = std::os::unix::fs::MetadataExt::ino(&meta);
    #[cfg(not(unix))]
    let inode = 0;
    Some((top, (meta.len(), meta.modified().ok(), inode)))
}

/// The repository's filter and partial-clone config, from the open
/// [`config_scope`] when there is one and its entry is current.
fn repository_config(git: &Path, root: &Path) -> Result<RepositoryConfig, Failure> {
    let scoped = CONFIG_SCOPE.with(|scope| scope.borrow().is_some());
    let stamp = if scoped { config_stamp(root) } else { None };
    let key = stamp
        .as_ref()
        .map(|(top, _)| (git.to_path_buf(), top.clone()));
    if let (Some(key), Some((_, stamp))) = (&key, &stamp) {
        let hit = CONFIG_SCOPE.with(|scope| {
            scope
                .borrow()
                .as_ref()
                .and_then(|scope| scope.entries.get(key))
                .filter(|(held, _)| held == stamp)
                .map(|(_, config)| config.clone())
        });
        if let Some(config) = hit {
            return Ok(config);
        }
    }
    let fresh = probe_repository_config(git, root)?;
    if let (Some(key), Some((_, stamp))) = (key, stamp) {
        CONFIG_SCOPE.with(|scope| {
            if let Some(scope) = scope.borrow_mut().as_mut()
                && (scope.entries.len() < CONFIG_SCOPE_ENTRIES || scope.entries.contains_key(&key))
            {
                scope.entries.insert(key, (stamp, fresh.clone()));
            }
        });
    }
    Ok(fresh)
}

/// Whether a `--show-scope -z` listing names a promisor:
/// `extensions.partialclone`, a remote's `promisor` flag, or a remote's
/// `partialclonefilter`. Any scope counts; a promisor configured globally
/// fetches just the same. A flag git would not read as false counts as true:
/// an unreadable promisor flag is not evidence of no promisor.
pub fn is_partial_clone(listing: &[u8]) -> bool {
    let fields: Vec<&[u8]> = listing.split(|b| *b == 0).collect();
    fields.chunks(2).any(|pair| {
        let [_, entry] = pair else { return false };
        let entry = String::from_utf8_lossy(entry);
        let (key, value) = match entry.split_once('\n') {
            Some((key, value)) => (key, Some(value.trim())),
            None => (entry.as_ref(), None),
        };
        let remote = key.starts_with("remote.");
        if key == "extensions.partialclone" || (remote && key.ends_with(".partialclonefilter")) {
            value.is_some_and(|v| !v.is_empty())
        } else if remote && key.ends_with(".promisor") {
            !matches!(
                value.map(str::to_ascii_lowercase).as_deref(),
                Some("false" | "no" | "off" | "0" | "")
            )
        } else {
            false
        }
    })
}

/// Turns `git config --show-scope -z --get-regexp ^filter\.` output
/// (`scope NUL key [LF value] NUL`, repeated) into `-c` arguments.
///
/// A driver any repository-scoped entry touches — scope `local` or
/// `worktree`, which includes values pulled in through `include.path` from a
/// tracked file — has each setting the repository gave it set again: to the
/// operator's own value when `system`, `global` or `command` scope defines one
/// (a repository that redefines git-lfs's filter gets git-lfs's back), else to
/// empty. Only those settings: git treats an empty `process` as configured,
/// so blanking one the repository never set would stop the operator's `clean`
/// from running. Its `required` is always set, to the operator's value or
/// `false`, so a blanked filter is skipped rather than failing the read.
/// Drivers only the operator defines, such as a global git-lfs install, are
/// left alone. A driver name a `-c` argument cannot spell (one containing
/// `=`) is refused rather than left running. This is the owner's decision
/// recorded in ft-ff2153d9.
pub fn filter_overrides(listing: &[u8]) -> Result<Vec<String>, String> {
    use std::collections::{BTreeSet, HashMap, HashSet};
    let mut operator: HashMap<String, Option<String>> = HashMap::new();
    let mut repository: BTreeSet<String> = BTreeSet::new();
    let mut repository_keys: HashSet<String> = HashSet::new();
    let fields: Vec<&[u8]> = listing.split(|b| *b == 0).collect();
    for pair in fields.chunks(2) {
        let [scope, entry] = pair else { continue };
        let entry = String::from_utf8_lossy(entry);
        let (key, value) = match entry.split_once('\n') {
            Some((key, value)) => (key.to_string(), Some(value.to_string())),
            None => (entry.to_string(), None),
        };
        let Some(rest) = key.strip_prefix("filter.") else {
            continue;
        };
        let Some(dot) = rest.rfind('.') else { continue };
        if dot == 0 {
            continue;
        }
        let name = &rest[..dot];
        match *scope {
            b"local" | b"worktree" => {
                repository.insert(name.to_string());
                repository_keys.insert(key.clone());
            }
            // system, global and command: the operator's own settings, listed
            // in precedence order, so the last one wins.
            _ => {
                operator.insert(key.clone(), value);
            }
        }
    }
    let mut args = Vec::new();
    for name in &repository {
        if name.contains('=') {
            return Err(format!(
                "the repository defines filter {name:?}, whose name a -c argument cannot spell; \
                 refusing to read with it"
            ));
        }
        for var in ["clean", "smudge", "process", "required"] {
            let key = format!("filter.{name}.{var}");
            if var != "required" && !repository_keys.contains(&key) {
                continue;
            }
            args.push("-c".to_string());
            args.push(match operator.get(&key) {
                Some(Some(value)) => format!("{key}={value}"),
                Some(None) => key,
                None if var == "required" => format!("{key}=false"),
                None => format!("{key}="),
            });
        }
    }
    Ok(args)
}

/// The program a command against `root` runs, as an absolute path.
///
/// A `program` with a directory in it is the caller's explicit choice and is
/// used as named, made absolute so the file is the same one wherever the
/// child runs. A bare name is searched for along `path_var` (the `PATH`
/// value), in order, and the first entry holding an executable of that name
/// decides:
///
/// * an entry that is empty or relative is refused. It names whatever
///   directory this process happens to run in, which is usually the
///   repository. Go's `os/exec` refuses the same thing (`exec.ErrDot`); Rust's
///   standard library does not, so it is refused here.
/// * a candidate whose canonical location is inside the repository `root` is
///   in is refused: the repository's own files are input to analysis, not
///   programs to trust. The boundary is the repository's top, not `root`
///   itself ([`repository_top`]): run against `repo/backend`, a program at
///   `repo/tools/bin` is as much repository content.
///   Containment is decided by identity — device and inode of each directory
///   above the candidate against `root`'s — not by comparing names, because a
///   filesystem that folds case or Unicode normalization (APFS, NTFS) gives
///   one directory many spellings. A `root` that does not exist contains
///   nothing, so git still reports the missing root itself. A directory that
///   cannot be examined is an error, never an answer of "outside".
///
/// A refusal or a program found nowhere is a [`Failure::Spawn`]: nothing ran,
/// and every caller already reports that as "git could not run".
pub fn resolve_outside(
    program: &OsStr,
    path_var: Option<&OsStr>,
    root: &Path,
) -> Result<PathBuf, Failure> {
    let name = program.to_string_lossy().into_owned();
    let refuse = |kind: std::io::ErrorKind, message: String| Failure::Spawn {
        program: name.clone(),
        error: std::io::Error::new(kind, message),
    };
    let named = Path::new(program);
    if named
        .parent()
        .is_some_and(|dir| !dir.as_os_str().is_empty())
    {
        return std::path::absolute(named)
            .map_err(|error| refuse(error.kind(), format!("{name}: {error}")));
    }
    let boundary = repository_top(root).map_err(|error| {
        refuse(
            error.kind(),
            format!("finding the repository around {}: {error}", root.display()),
        )
    })?;
    let root = boundary.as_path();
    let entries = path_var.map(std::env::split_paths).into_iter().flatten();
    for entry in entries {
        for candidate in executable_names(&entry, program) {
            if !is_executable(&candidate) {
                continue;
            }
            if entry.as_os_str().is_empty() || entry.is_relative() {
                return Err(refuse(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "{name} resolves to {} through the relative PATH entry {:?}, which names \
                         whatever directory this process runs in",
                        candidate.display(),
                        entry
                    ),
                ));
            }
            return match follow_outside(&candidate, root) {
                Ok(Ok(resolved)) => Ok(resolved),
                Ok(Err(bound)) => Err(refuse(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "{name} resolves through {}, which is inside the repository under analysis",
                        bound.display()
                    ),
                )),
                Err(error) => Err(refuse(
                    error.kind(),
                    format!(
                        "{name} resolves through {}, whose location could not be checked: {error}",
                        candidate.display()
                    ),
                )),
            };
        }
    }
    Err(refuse(
        std::io::ErrorKind::NotFound,
        format!("{name} is not on PATH"),
    ))
}

/// The top of the repository `root` is in, found from the filesystem alone —
/// no git runs to answer it, because the git that would answer is one of the
/// programs being resolved. The Go host's `proc.repositoryTop` is the same
/// rule.
///
/// The walk goes up from `root` and stops at the nearest directory holding a
/// `.git` (a directory, or the file a worktree or submodule has); that is the
/// top. With no `.git` above, the nearest directory holding a `.devcouncil`
/// directory is the top, and with neither, `root` itself. The walk stops at
/// the nearest `.git` on purpose: a home directory that is itself a
/// repository must not make every repository beneath it swallow
/// `~/.local/bin`. A marker that cannot be examined is an error, never
/// "absent", which would shrink the boundary. A `root` that does not exist is
/// returned as given: it contains nothing.
pub fn repository_top(root: &Path) -> std::io::Result<PathBuf> {
    let start = match std::fs::canonicalize(root) {
        Ok(start) => start,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(root.to_path_buf()),
        Err(error) => return Err(error),
    };
    let mut devcouncil = None;
    let mut current = Some(start.as_path());
    while let Some(dir) = current {
        match std::fs::symlink_metadata(dir.join(".git")) {
            Ok(_) => return Ok(dir.to_path_buf()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if devcouncil.is_none() {
            match std::fs::metadata(dir.join(".devcouncil")) {
                Ok(meta) if meta.is_dir() => devcouncil = Some(dir.to_path_buf()),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        current = dir.parent();
    }
    Ok(devcouncil.unwrap_or(start))
}

/// The file names `program` may have in `dir`: itself, and on Windows each
/// `PATHEXT` extension when it has none of its own.
fn executable_names(dir: &Path, program: &OsStr) -> Vec<PathBuf> {
    let mut names = vec![dir.join(program)];
    if cfg!(windows) && Path::new(program).extension().is_none() {
        let pathext = std::env::var_os("PATHEXT")
            .unwrap_or_else(|| std::ffi::OsString::from(".COM;.EXE;.BAT;.CMD"));
        for ext in pathext
            .to_string_lossy()
            .split(';')
            .filter(|e| !e.is_empty())
        {
            let mut file = program.to_os_string();
            file.push(ext);
            names.push(dir.join(file));
        }
    }
    names
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The symlink chain [`follow_outside`] follows, bounded as POSIX path
/// resolution is.
const MAX_LINK_HOPS: usize = 40;

/// Follows `candidate`'s symlink chain to its canonical end, or names the
/// first link on the way that is bound inside `root` (the inner `Err`).
///
/// Every link is checked, not only where the chain ends. A repository need
/// not ship a program to choose one: a link it carries, named `git` and
/// pointing at `/usr/bin/true`, is a git that exits 0 with no output, which
/// reads as "nothing changed". The binding is repository content even when
/// the bytes it points at are not.
fn follow_outside(candidate: &Path, root: &Path) -> std::io::Result<Result<PathBuf, PathBuf>> {
    let mut current = candidate.to_path_buf();
    for _ in 0..=MAX_LINK_HOPS {
        let parent = current.parent().unwrap_or_else(|| Path::new("/"));
        // The directory that holds a name decides what the name means.
        let holder = std::fs::canonicalize(parent)?;
        if contained_in(&holder, root)? {
            return Ok(Err(holder.join(current.file_name().unwrap_or_default())));
        }
        if !std::fs::symlink_metadata(&current)?
            .file_type()
            .is_symlink()
        {
            return std::fs::canonicalize(&current).map(Ok);
        }
        let target = std::fs::read_link(&current)?;
        current = if target.is_absolute() {
            target
        } else {
            holder.join(target)
        };
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!(
            "more than {MAX_LINK_HOPS} links from {}",
            candidate.display()
        ),
    ))
}

/// Whether `path` (canonical) is `root` or lies beneath it, by identity.
fn contained_in(path: &Path, root: &Path) -> std::io::Result<bool> {
    let root_meta = match std::fs::metadata(root) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let root_id = (root_meta.dev(), root_meta.ino());
        let mut current = Some(path);
        while let Some(dir) = current {
            let meta = std::fs::metadata(dir)?;
            if (meta.dev(), meta.ino()) == root_id {
                return Ok(true);
            }
            current = dir.parent();
        }
        Ok(false)
    }
    #[cfg(not(unix))]
    {
        // `canonicalize` on Windows asks the handle for its final path, which
        // carries the on-disk spelling, so two canonical paths compare by
        // identity component by component.
        let _ = root_meta;
        let root = std::fs::canonicalize(root)?;
        Ok(path.starts_with(&root))
    }
}

/// Linux can briefly retain a writer inherited by another concurrent fork
/// until that process execs. Retry only this pre-exec refusal: no child has
/// started, and a successful spawn must never be repeated. Persistent writers
/// stop after eight attempts; the original request deadline also bounds retries.
fn spawn_with_retry(
    mut spawn: impl FnMut() -> std::io::Result<std::process::Child>,
    program: &str,
    started: Instant,
    deadline: Duration,
) -> Result<std::process::Child, Failure> {
    const MAX_ATTEMPTS: usize = 8;
    const RETRY_DELAY: Duration = Duration::from_millis(10);
    for attempt in 1..=MAX_ATTEMPTS {
        if started.elapsed() >= deadline {
            return Err(Failure::Deadline {
                program: program.to_owned(),
                deadline,
            });
        }
        match spawn() {
            Ok(child) => return Ok(child),
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && attempt < MAX_ATTEMPTS =>
            {
                thread::sleep(RETRY_DELAY.min(deadline.saturating_sub(started.elapsed())));
            }
            Err(error) => {
                return Err(Failure::Spawn {
                    program: program.to_owned(),
                    error,
                });
            }
        }
    }
    unreachable!("the final attempt always returns")
}

/// Run `command` to completion within `bounds`.
///
/// `stdin`, `stdout` and `stderr` are set here; anything the caller configured
/// for them is replaced, because a child holding a terminal or an undrained
/// pipe is exactly what the bounds exist to rule out.
pub fn run_bounded(command: &mut Command, bounds: Bounds) -> Result<Captured, Failure> {
    let program = command.get_program().to_string_lossy().into_owned();
    let started = Instant::now();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The child leads a process group of its own, so the expiry below can name
    // the whole tree it started rather than the one pid `Child` knows about.
    // `process_group` is `std`'s own `setpgid` in the child, between fork and
    // exec — no `pre_exec`, nothing unsafe here.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = spawn_with_retry(|| command.spawn(), &program, started, bounds.deadline)?;

    // Both pipes are taken before anything waits: a reader that starts after
    // the child has filled its buffer starts too late.
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let (stdout_done, stdout_rx) = mpsc::channel();
    let (stderr_done, stderr_rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = stdout_done.send(drain(stdout_pipe, bounds.stdout_cap));
    });
    thread::spawn(move || {
        let _ = stderr_done.send(drain(stderr_pipe, bounds.stderr_cap));
    });

    let deadline = started + bounds.deadline;
    // EOF on both pipes is the cheap signal that the child is finishing; the
    // reap below is what confirms it. Waiting on the readers first means the
    // common case costs no polling at all.
    let stdout = match stdout_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(drained) => drained,
        Err(_) => {
            kill_and_reap(&mut child, &stdout_rx, &stderr_rx);
            return Err(Failure::Deadline {
                program,
                deadline: bounds.deadline,
            });
        }
    };
    let stderr = match stderr_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(drained) => drained,
        Err(_) => {
            kill_and_reap(&mut child, &stdout_rx, &stderr_rx);
            return Err(Failure::Deadline {
                program,
                deadline: bounds.deadline,
            });
        }
    };

    // A child can close its pipes and then hang — a hook that daemonised, a
    // process waiting on something that is not coming. The reap is bounded by
    // the same deadline for that reason.
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                kill_and_reap(&mut child, &stdout_rx, &stderr_rx);
                return Err(Failure::Deadline {
                    program,
                    deadline: bounds.deadline,
                });
            }
            Ok(None) => thread::sleep(Duration::from_millis(1)),
            Err(error) => {
                // The same clean-up: a child we can no longer wait on is a child
                // still running, and its descendants are still holding the pipes.
                kill_and_reap(&mut child, &stdout_rx, &stderr_rx);
                return Err(Failure::Wait { program, error });
            }
        }
    };

    Ok(Captured {
        status,
        stdout: stdout.bytes,
        stdout_truncated: stdout.truncated,
        stderr: stderr.bytes,
        stderr_truncated: stderr.truncated,
        elapsed: started.elapsed(),
    })
}

/// A long-lived child the caller converses with over its pipes: a language
/// server, anything that answers requests rather than running to exit.
///
/// [`run_bounded`] cannot hold one — it owns the pipes and waits for EOF — so
/// before this, such a child was spawned with a bare `Command` and inherited
/// none of the runner's discipline. A `Session` keeps the part that does not
/// depend on the protocol: the child leads its own process group, [`kill`]
/// signals the whole group and reaps it, and dropping the session does the
/// same, so no path out of a caller leaves the child or its descendants
/// running.
///
/// The deadline stays the caller's, because only the caller knows what one
/// request is. What it must not do is read the pipe on the thread that
/// enforces the deadline: a blocking read on a child that has gone quiet never
/// returns, and the deadline is never checked. Read on a thread, wait on a
/// channel with `recv_timeout`, and [`kill`] on expiry — the kill closes the
/// pipe and ends the reader.
///
/// [`kill`]: Session::kill
pub struct Session {
    child: std::process::Child,
    program: String,
    killed: bool,
    /// [`Session::try_status`] saw the child exit, which reaps it.
    reaped: bool,
    /// [`Session::kill`] sent the group a signal. Read by the tests that pin
    /// when it must not.
    group_signalled: bool,
}

impl Session {
    /// The child's stdin, once; `None` after the first call.
    pub fn take_stdin(&mut self) -> Option<std::process::ChildStdin> {
        self.child.stdin.take()
    }

    /// The child's stdout, once; `None` after the first call.
    pub fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.child.stdout.take()
    }

    /// The exit status if the child has already exited, without waiting.
    pub fn try_status(&mut self) -> Option<ExitStatus> {
        let status = self.child.try_wait().ok().flatten();
        self.reaped |= status.is_some();
        status
    }

    /// The program this session runs, for messages.
    pub fn program(&self) -> &str {
        &self.program
    }

    /// Signal the child's process group and reap the child. Idempotent.
    ///
    /// The group is signalled even when the child has already exited and been
    /// reaped by [`try_status`](Self::try_status), but only while a member is
    /// left in it: a descendant the child left behind is still in the group,
    /// and the group id cannot be reused while any member lives. With no member
    /// left the id is free, and the kernel may already have handed it to an
    /// unrelated process group that the `SIGKILL` would then take.
    pub fn kill(&mut self) {
        if self.killed {
            return;
        }
        self.killed = true;
        if !self.reaped || group_has_members(self.child.id()) {
            self.group_signalled = true;
            kill_descendants(&mut self.child);
        }
        let _ = self.child.wait();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.kill();
    }
}

/// A command for [`spawn_session`], so a session's program is named through
/// this module like every other child the kernel starts.
pub fn session_command(program: impl AsRef<OsStr>) -> Command {
    Command::new(program)
}

/// Start `command` as a [`Session`]: stdin and stdout piped, stderr discarded
/// (a conversational child that fills an undrained stderr pipe blocks), and the
/// child leading its own process group. Spawning retries the same pre-exec
/// refusal [`run_bounded`] does, within `spawn_deadline`.
pub fn spawn_session(command: &mut Command, spawn_deadline: Duration) -> Result<Session, Failure> {
    let program = command.get_program().to_string_lossy().into_owned();
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = spawn_with_retry(|| command.spawn(), &program, Instant::now(), spawn_deadline)?;
    Ok(Session {
        child,
        program,
        killed: false,
        reaped: false,
        group_signalled: false,
    })
}

/// End the child and everything it started, then let the drain threads come
/// home.
///
/// The order is the whole point. The kill goes out **before** the reap, because
/// an unreaped child keeps its process group alive even when it has already
/// exited — which is exactly the daemonising-hook shape, the parent gone and
/// the descendants still holding the pipes. Reaping first would dissolve the
/// group and leave nothing to signal.
///
/// The wait on the readers afterwards is bounded by [`DRAIN_HANDBACK`] and is
/// not load-bearing: the pipes reach EOF once the last holder is dead, and the
/// only reason to wait at all is so this function does not return two threads
/// and two descriptors to nobody. A descendant that escaped the group can still
/// outlast the window, and then the runner does what it always did — returns on
/// time and leaves the readers to end when the pipe does.
fn kill_and_reap(
    child: &mut std::process::Child,
    stdout_rx: &mpsc::Receiver<Drained>,
    stderr_rx: &mpsc::Receiver<Drained>,
) {
    kill_descendants(child);
    let _ = child.wait();
    // One budget across both, not one each: the bound the caller was promised is
    // a wall clock, and two waits in series would spend twice what it says.
    let handback = Instant::now() + DRAIN_HANDBACK;
    let _ = stdout_rx.recv_timeout(handback.saturating_duration_since(Instant::now()));
    let _ = stderr_rx.recv_timeout(handback.saturating_duration_since(Instant::now()));
}

/// Whether the group `leader` led still has a member this process may signal:
/// a signal-0 probe, which delivers nothing. `ESRCH` is an empty group, and
/// `EPERM` is a group that is not ours (an id reused by someone else), which is
/// not a member to kill either.
#[cfg(unix)]
fn group_has_members(leader: u32) -> bool {
    signal_group(leader, Signal::Probe).is_ok()
}

/// Elsewhere there is no group to probe, and a reaped child has nothing left.
#[cfg(not(unix))]
fn group_has_members(_leader: u32) -> bool {
    false
}

/// `SIGKILL` to the child's process group — the child and every descendant that
/// stayed in it.
///
/// The child was spawned with `process_group(0)`, so it leads the group and its
/// pid is the group id; a negated pid names the group to `kill(2)`. If the
/// group cannot be signalled at all the direct child is killed the old way,
/// because "the group call failed" must never come out as "nothing was killed".
#[cfg(unix)]
fn kill_descendants(child: &mut std::process::Child) {
    if signal_group(child.id(), Signal::Kill).is_err() {
        let _ = child.kill();
    }
}

/// Elsewhere the deadline reaches the direct child only.
///
/// Windows would need a Job Object — a different lifetime model, with its own
/// handle to own and inherit — and this runner does not have one. Naming the
/// gap here rather than leaving `process_group` silently absent: on these
/// targets a descendant that inherited the pipes survives the deadline, exactly
/// as it did everywhere before.
#[cfg(not(unix))]
fn kill_descendants(child: &mut std::process::Child) {
    let _ = child.kill();
}

/// A signal this module will deliver for a caller that owns the target.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// `SIGTERM`: ask the target to shut down.
    Terminate,
    /// `SIGKILL`: end it.
    Kill,
    /// Signal 0: deliver nothing, only ask whether the target exists and may
    /// be signalled. `Ok` means it does; `ESRCH` means it does not.
    Probe,
}

/// Deliver `signal` to one process.
///
/// For a process the caller started, or one it can name with certainty — a
/// harness that owns a daemon's process group, not a pid read from a file.
#[cfg(unix)]
pub fn signal_process(pid: u32, signal: Signal) -> std::io::Result<()> {
    let pid =
        i32::try_from(pid).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    if pid <= 0 {
        // 0 and negatives name groups, not one process.
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    raw_kill(pid, signal)
}

/// Deliver `signal` to the process group `leader` leads — every member,
/// including descendants that outlived the leader.
///
/// For a group the caller created, as [`spawn_session`] and
/// `Command::process_group(0)` do: the leader's pid is the group id, and the
/// id cannot be reused while any member lives.
#[cfg(unix)]
pub fn signal_group(leader: u32, signal: Signal) -> std::io::Result<()> {
    let leader = i32::try_from(leader)
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    if leader <= 1 {
        // 0 is the caller's own group and 1 would name init's.
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    raw_kill(-leader, signal)
}

/// Whether a signal failed because its target no longer exists (`ESRCH`) —
/// the answer a [`Signal::Probe`] gives for a process or group that is gone.
#[cfg(unix)]
pub fn target_is_gone(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::ESRCH)
}

/// The one `kill(2)` in the workspace. Every signal goes through here so the
/// audited surface stays one call.
#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "one audited libc::kill, reached only through signal_process and signal_group"
)]
fn raw_kill(target: i32, signal: Signal) -> std::io::Result<()> {
    let signal = match signal {
        Signal::Terminate => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
        Signal::Probe => 0,
    };
    // SAFETY: `kill` delivers a signal to the named process or group; it reads
    // and writes none of our memory. `target` was range-checked by the caller:
    // a positive pid, or a negated group leader above 1.
    if unsafe { libc::kill(target, signal) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

struct Drained {
    bytes: Vec<u8>,
    /// The bytes kept are a prefix of what the child had to say — because the
    /// cap cut it, or because the pipe stopped answering before EOF.
    truncated: bool,
}

/// Read a pipe to EOF, keeping at most `cap` bytes.
///
/// Reading continues past the cap on purpose. A reader that stops leaves the
/// child blocked on a full pipe, and then only the deadline ends it — which
/// turns "this answer was long" into "this answer took ten seconds", and
/// charges every caller the full deadline for the privilege of a partial
/// result.
///
/// Only `Ok(0)` is the end. `read` on a pipe may return `Interrupted` before it
/// has transferred anything, and `std` retries that only inside `read_to_end` /
/// `read_exact` — never on a bare `read` — so this loop has to. Any other error
/// leaves a *prefix*, and a prefix is reported the same way the cap's is: the
/// one property every caller of this module reads is "are the bytes I have the
/// whole answer", and there is no version of that question where a pipe that
/// stopped answering counts as yes.
fn drain(pipe: Option<impl Read>, cap: usize) -> Drained {
    let mut drained = Drained {
        bytes: Vec::new(),
        truncated: false,
    };
    let Some(mut pipe) = pipe else {
        return drained;
    };
    let mut chunk = [0u8; 64 * 1024];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) => break,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => {
                drained.truncated = true;
                break;
            }
            Ok(read) => {
                let room = cap.saturating_sub(drained.bytes.len());
                if read > room {
                    drained.bytes.extend_from_slice(&chunk[..room]);
                    drained.truncated = true;
                } else {
                    drained.bytes.extend_from_slice(&chunk[..read]);
                }
            }
        }
    }
    drained
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    fn session_of(script: &str) -> Session {
        let mut command = session_command("sh");
        command.arg("-c").arg(script);
        spawn_session(&mut command, Duration::from_secs(5)).expect("sh starts")
    }

    #[cfg(unix)]
    fn wait_until_reaped(session: &mut Session) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while session.try_status().is_none() {
            assert!(Instant::now() < deadline, "the child never exited");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// `try_status` reaps the child; the group id then names nothing of ours,
    /// and a pid the kernel has since handed to another group would take the
    /// SIGKILL. With no member left, `kill` must not signal at all.
    #[cfg(unix)]
    #[test]
    fn killing_a_reaped_session_with_no_members_left_signals_no_group() {
        let mut session = session_of("exit 0");
        wait_until_reaped(&mut session);
        session.kill();
        assert!(
            !session.group_signalled,
            "SIGKILL went to the group of a child that was already reaped"
        );
    }

    /// The case the signal exists for: the leader is gone, a descendant it left
    /// behind still holds the group, and that one is still ended.
    #[cfg(unix)]
    #[test]
    fn killing_a_reaped_session_still_ends_a_descendant_that_outlived_it() {
        let mut session = session_of("sleep 60 & exit 0");
        wait_until_reaped(&mut session);
        let leader = session.child.id();
        session.kill();
        assert!(
            session.group_signalled,
            "a surviving member was not signalled"
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while signal_group(leader, Signal::Probe).is_ok() {
            assert!(Instant::now() < deadline, "the descendant outlived kill");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// An unreaped child is always signalled: its group id is still ours.
    #[cfg(unix)]
    #[test]
    fn killing_a_live_session_signals_its_group() {
        let mut session = session_of("sleep 60");
        session.kill();
        assert!(session.group_signalled);
        assert!(session.try_status().is_none() || session.reaped);
    }

    use super::*;
    use std::io::ErrorKind;

    #[test]
    fn guarded_args_places_each_guard() {
        let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
        let global = vec![
            "-c".to_string(),
            "core.fsmonitor=false".to_string(),
            "-c".to_string(),
            format!("core.hooksPath={null}"),
            "-c".to_string(),
            "log.showSignature=false".to_string(),
            "-c".to_string(),
            "core.useReplaceRefs=false".to_string(),
        ];
        let cases: &[(&[&str], &[&str])] = &[
            (
                &["diff", "HEAD"],
                &[
                    "diff",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--no-color",
                    "--ignore-submodules=dirty",
                    "HEAD",
                ],
            ),
            (
                &["status", "--porcelain"],
                &["status", "--ignore-submodules=dirty", "--porcelain"],
            ),
            (
                &["blame", "-w", "HEAD"],
                &["blame", "--no-textconv", "-w", "HEAD"],
            ),
            (
                &["show", "x"],
                &["show", "--no-ext-diff", "--no-textconv", "--no-color", "x"],
            ),
            (
                &["log", "-z"],
                &["log", "--no-ext-diff", "--no-textconv", "--no-color", "-z"],
            ),
            (
                &[
                    "-c",
                    "core.quotePath=false",
                    "-C",
                    "sub",
                    "--no-pager",
                    "diff",
                    "-U0",
                ],
                &[
                    "-c",
                    "core.quotePath=false",
                    "-C",
                    "sub",
                    "--no-pager",
                    "diff",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--no-color",
                    "--ignore-submodules=dirty",
                    "-U0",
                ],
            ),
            (&["ls-files", "-z"], &["ls-files", "-z"]),
            (&["rev-parse", "diff"], &["rev-parse", "diff"]),
            (&["-c"], &["-c"]),
            (&["--version"], &["--version"]),
            (&[], &[]),
        ];
        for (input, want) in cases {
            let got: Vec<String> = guarded_args(input.iter())
                .into_iter()
                .map(|a| a.into_string().unwrap())
                .collect();
            let mut expected = global.clone();
            expected.extend(want.iter().map(|s| s.to_string()));
            assert_eq!(got, expected, "input {input:?}");
        }
    }

    #[test]
    fn filter_overrides_parses_listings() {
        fn entry(scope: &str, kv: &str) -> String {
            format!("{scope}\0{kv}\0")
        }
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        type Case = (&'static str, String, Result<Vec<String>, ()>);
        let cases: Vec<Case> = vec![
            ("empty", String::new(), Ok(vec![])),
            (
                "operator only is left alone",
                entry("global", "filter.lfs.clean\ngit-lfs clean -- %f")
                    + &entry("global", "filter.lfs.required\ntrue"),
                Ok(vec![]),
            ),
            (
                "repository driver blanked, required false",
                entry("local", "filter.x.clean\n/evil"),
                Ok(s(&[
                    "-c",
                    "filter.x.clean=",
                    "-c",
                    "filter.x.required=false",
                ])),
            ),
            (
                "worktree scope",
                entry("worktree", "filter.x.process\n/evil"),
                Ok(s(&[
                    "-c",
                    "filter.x.process=",
                    "-c",
                    "filter.x.required=false",
                ])),
            ),
            (
                "operator value restored, last wins",
                entry("system", "filter.lfs.clean\nold")
                    + &entry("global", "filter.lfs.clean\ngit-lfs clean -- %f")
                    + &entry("global", "filter.lfs.required\ntrue")
                    + &entry("local", "filter.lfs.clean\n/evil"),
                Ok(s(&[
                    "-c",
                    "filter.lfs.clean=git-lfs clean -- %f",
                    "-c",
                    "filter.lfs.required=true",
                ])),
            ),
            (
                "command scope is the operator's",
                entry("local", "filter.x.smudge\n/evil")
                    + &entry("command", "filter.x.smudge\ncat"),
                Ok(s(&[
                    "-c",
                    "filter.x.smudge=cat",
                    "-c",
                    "filter.x.required=false",
                ])),
            ),
            (
                "valueless operator key stays valueless",
                entry("global", "filter.x.required") + &entry("local", "filter.x.clean\n/evil"),
                Ok(s(&["-c", "filter.x.clean=", "-c", "filter.x.required"])),
            ),
            (
                "dotted name",
                entry("local", "filter.a.b.clean\n/evil"),
                Ok(s(&[
                    "-c",
                    "filter.a.b.clean=",
                    "-c",
                    "filter.a.b.required=false",
                ])),
            ),
            (
                "sorted",
                entry("local", "filter.z.clean\n1") + &entry("local", "filter.a.clean\n2"),
                Ok(s(&[
                    "-c",
                    "filter.a.clean=",
                    "-c",
                    "filter.a.required=false",
                    "-c",
                    "filter.z.clean=",
                    "-c",
                    "filter.z.required=false",
                ])),
            ),
            (
                "unspellable name refused",
                entry("local", "filter.we=ird.clean\n/evil"),
                Err(()),
            ),
            (
                "truncated trailing record ignored",
                entry("local", "filter.x.clean\n/evil") + "local",
                Ok(s(&[
                    "-c",
                    "filter.x.clean=",
                    "-c",
                    "filter.x.required=false",
                ])),
            ),
            (
                "non-driver keys ignored",
                entry("local", "filter.\n/evil") + &entry("local", "filters.x.clean\n1"),
                Ok(vec![]),
            ),
        ];
        for (name, listing, want) in cases {
            let got = filter_overrides(listing.as_bytes()).map_err(|_| ());
            assert_eq!(got, want, "{name}");
        }
    }

    /// A working-tree diff through the seam does not run a filter the
    /// repository's own config defines, even marked required, and the diff
    /// is still the true one. Kernel reads today never diff the working tree,
    /// so this is the path a future one would take.
    #[cfg(unix)]
    #[test]
    fn a_working_tree_diff_does_not_run_the_repository_s_filter() {
        let base = Scratch::new("filter");
        let root = base.0.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let marker = base.0.join("ran");
        let script = plant(&base.0.join("bin"), "filter.sh", 0o755);
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho ran >> '{}'\ncat\n", marker.display()),
        )
        .unwrap();
        let setup = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(["-c", "user.email=a@example.test", "-c", "user.name=a"])
                .args(args)
                .stdin(Stdio::null())
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        setup(&["init", "-q"]);
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        setup(&["add", "a.txt"]);
        setup(&["commit", "-qm", "base"]);
        std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
        setup(&["config", "filter.planted.clean", script.to_str().unwrap()]);
        setup(&["config", "filter.planted.required", "true"]);
        std::fs::write(root.join(".git/info/attributes"), "* filter=planted\n").unwrap();

        let mut command = git_with_program(OsStr::new("git"), &root, ["diff", "HEAD"]).unwrap();
        let captured = run_bounded(
            &mut command,
            Bounds {
                deadline: Duration::from_secs(10),
                stdout_cap: 1 << 20,
                stderr_cap: 4096,
            },
        )
        .unwrap();
        assert!(!marker.exists(), "the repository's filter ran");
        assert!(captured.status.success(), "{}", captured.stderr_trimmed());
        assert!(
            captured.stdout_lossy().contains("+two"),
            "{}",
            captured.stdout_lossy()
        );
    }

    /// A `git replace` ref rewrites history at read time. A read through the
    /// seam sees the commit that was made, not the clean twin an agent put in
    /// its place, while the same read without the guard sees the twin.
    #[cfg(unix)]
    #[test]
    fn a_replace_ref_does_not_rewrite_what_a_read_through_the_seam_sees() {
        let base = Scratch::new("replace");
        let root = base.0.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let git = |args: &[&str]| -> String {
            let out = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(["-c", "user.email=a@example.test", "-c", "user.name=a"])
                .args(args)
                .stdin(Stdio::null())
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-qm", "base"]);
        std::fs::write(root.join("keys.txt"), "ORIGINAL-ADDED-LINE\n").unwrap();
        git(&["add", "keys.txt"]);
        git(&["commit", "-qm", "adds the secret"]);
        let bad = git(&["rev-parse", "HEAD"]);
        let tree = git(&["rev-parse", &format!("{bad}^^{{tree}}")]);
        let parent = git(&["rev-parse", &format!("{bad}^")]);
        let clean = git(&["commit-tree", &tree, "-p", &parent, "-m", "clean twin"]);
        git(&["replace", &bad, &clean]);

        let subjects = |guarded: bool| -> String {
            let mut command = if guarded {
                git_with_program(OsStr::new("git"), &root, ["log", "--format=%s", "-1", &bad])
                    .unwrap()
            } else {
                let mut c = Command::new("git");
                c.arg("-C")
                    .arg(&root)
                    .args(["log", "--format=%s", "-1", &bad]);
                c
            };
            let captured = run_bounded(
                &mut command,
                Bounds {
                    deadline: Duration::from_secs(10),
                    stdout_cap: 1 << 20,
                    stderr_cap: 4096,
                },
            )
            .unwrap();
            assert!(captured.status.success(), "{}", captured.stderr_trimmed());
            captured.stdout_lossy().trim().to_string()
        };
        assert_eq!(
            subjects(false),
            "clean twin",
            "the control read does not honour the replace ref, so this test proves nothing"
        );
        assert_eq!(subjects(true), "adds the secret");
    }

    /// A filter listing too large to read whole is refused: a truncated one
    /// could omit the driver that matters.
    #[cfg(unix)]
    #[test]
    fn a_filter_listing_past_the_cap_is_refused() {
        let base = Scratch::new("filter-flood");
        let flood = FILTER_CONFIG_LIMIT + 1024;
        let git = plant(&base.0.join("bin"), "git", 0o755);
        // A modern version, so a filter-free read needs no config listing.
        std::fs::write(
            &git,
            format!(
                "#!/bin/sh\n[ \"$1\" = version ] && {{ echo 'git version 2.54.0'; exit 0; }}\n\
                 head -c {flood} /dev/zero | tr '\\0' a\n"
            ),
        )
        .unwrap();

        let (kind, message) = match git_with_program(git.as_os_str(), &base.0, ["ls-files"]) {
            Err(Failure::Spawn { error, .. }) => (error.kind(), error.to_string()),
            other => panic!(
                "a flooded filter listing was accepted: {:?}",
                other.map(|_| ())
            ),
        };
        assert_eq!(kind, ErrorKind::InvalidData);
        assert!(message.contains("exceeds"), "{message}");
        // A filter-free subcommand on a git that honours GIT_NO_LAZY_FETCH
        // never asks, so the same git still runs.
        assert!(git_with_program(git.as_os_str(), &base.0, ["rev-parse", "HEAD"]).is_ok());
    }

    /// A fresh directory under the system temp dir, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("dc-proc-{label}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(std::fs::canonicalize(&dir).unwrap())
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    fn plant(dir: &Path, name: &str, mode: u32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    fn refusal(result: Result<PathBuf, Failure>) -> (ErrorKind, String) {
        match result {
            Err(Failure::Spawn { error, .. }) => (error.kind(), error.to_string()),
            other => panic!("expected a spawn refusal, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_installed_git_outside_the_repository_is_used_by_its_canonical_path() {
        let base = Scratch::new("outside");
        let root = base.0.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let installed = plant(&base.0.join("usr").join("bin"), "git", 0o755);
        let path = base.0.join("usr").join("bin");

        let got = resolve_outside(OsStr::new("git"), Some(path.as_os_str()), &root).unwrap();
        assert_eq!(got, std::fs::canonicalize(installed).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn a_git_inside_the_repository_is_refused_not_run() {
        let base = Scratch::new("inside");
        let root = base.0.join("repo");
        plant(&root.join("bin"), "git", 0o755);
        let path = std::env::join_paths([root.join("bin"), PathBuf::from("/usr/bin")]).unwrap();

        let (kind, message) = refusal(resolve_outside(OsStr::new("git"), Some(&path), &root));
        assert_eq!(kind, ErrorKind::PermissionDenied);
        assert!(message.contains("inside the repository"), "{message}");
    }

    /// The boundary is the repository's top, not the root a read was handed:
    /// run against `repo/backend`, a git planted at `repo/tools/bin` is still
    /// the repository's. The top is the nearest `.git` (directory or file),
    /// else the nearest `.devcouncil` directory.
    #[cfg(unix)]
    #[test]
    fn a_git_elsewhere_in_the_repository_is_refused_from_a_subdirectory() {
        for marker in [".git/", ".git", ".devcouncil/"] {
            let base = Scratch::new("subdir");
            let repo = base.0.join("repo");
            let sub = repo.join("backend");
            std::fs::create_dir_all(&sub).unwrap();
            match marker.strip_suffix('/') {
                Some(dir) => std::fs::create_dir_all(repo.join(dir)).unwrap(),
                None => std::fs::write(repo.join(marker), "gitdir: elsewhere\n").unwrap(),
            }
            plant(&repo.join("tools/bin"), "git", 0o755);
            let path =
                std::env::join_paths([repo.join("tools/bin"), PathBuf::from("/usr/bin")]).unwrap();

            let (kind, message) = refusal(resolve_outside(OsStr::new("git"), Some(&path), &sub));
            assert_eq!(kind, ErrorKind::PermissionDenied, "{marker}: {message}");
            assert!(
                message.contains("inside the repository"),
                "{marker}: {message}"
            );
            assert_eq!(repository_top(&sub).unwrap(), repo, "{marker}");
        }
    }

    /// With no marker above it, a root is its own boundary, and a program
    /// beside it — outside it — still resolves.
    #[cfg(unix)]
    #[test]
    fn a_root_with_no_repository_above_it_is_its_own_boundary() {
        let base = Scratch::new("no-marker");
        let root = base.0.join("plain");
        std::fs::create_dir_all(&root).unwrap();
        let outside = plant(&base.0.join("bin"), "git", 0o755);
        let path = std::env::join_paths([base.0.join("bin")]).unwrap();
        assert_eq!(repository_top(&root).unwrap(), root);
        assert_eq!(
            resolve_outside(OsStr::new("git"), Some(&path), &root).unwrap(),
            outside
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_into_the_repository_is_still_the_repository() {
        let base = Scratch::new("symlink");
        let root = base.0.join("repo");
        let planted = plant(&root.join("target"), "git", 0o755);
        let bin = base.0.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(&planted, bin.join("git")).unwrap();

        let (_, message) = refusal(resolve_outside(
            OsStr::new("git"),
            Some(bin.as_os_str()),
            &root,
        ));
        assert!(message.contains("inside the repository"), "{message}");
    }

    /// A relative entry names whatever directory the process runs in. It is
    /// refused when it holds the program, whichever directory that is.
    #[cfg(unix)]
    #[test]
    fn a_relative_path_entry_is_refused() {
        let base = Scratch::new("relative");
        let elsewhere = base.0.join("elsewhere");
        plant(&elsewhere.join("bin"), "git", 0o755);
        let root = base.0.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        // A relative spelling of `elsewhere/bin`, from this process's cwd.
        let cwd = std::env::current_dir().unwrap();
        let mut relative = PathBuf::new();
        for _ in cwd.components().skip(1) {
            relative.push("..");
        }
        relative.push(elsewhere.join("bin").strip_prefix("/").unwrap());

        let (kind, message) = refusal(resolve_outside(
            OsStr::new("git"),
            Some(relative.as_os_str()),
            &root,
        ));
        assert_eq!(kind, ErrorKind::PermissionDenied);
        assert!(message.contains("relative PATH entry"), "{message}");
    }

    /// Identity, not spelling: on a filesystem that folds case, `REPO/bin`
    /// is `Repo/bin`. Skipped where the filesystem keeps them apart.
    #[cfg(unix)]
    #[test]
    fn a_case_variant_spelling_of_the_repository_is_the_repository() {
        let base = Scratch::new("case");
        let root = base.0.join("Repo");
        plant(&root.join("bin"), "git", 0o755);
        let alias = base.0.join("REPO");
        let same = std::fs::metadata(&alias).is_ok_and(|m| {
            use std::os::unix::fs::MetadataExt;
            let r = std::fs::metadata(&root).unwrap();
            (m.dev(), m.ino()) == (r.dev(), r.ino())
        });
        if !same {
            return;
        }
        let entry = alias.join("bin");
        let (_, message) = refusal(resolve_outside(
            OsStr::new("git"),
            Some(entry.as_os_str()),
            &root,
        ));
        assert!(message.contains("inside the repository"), "{message}");
    }

    /// The `bin/git -> /usr/bin/true` attack: the target is outside, the
    /// choice is the repository's.
    #[cfg(unix)]
    #[test]
    fn a_repository_link_to_an_outside_program_is_refused() {
        let base = Scratch::new("true-link");
        let root = base.0.join("repo");
        let outside = plant(&base.0.join("usr").join("bin"), "true", 0o755);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("bin").join("git")).unwrap();
        let chain = base.0.join("bin");
        std::fs::create_dir_all(&chain).unwrap();
        std::os::unix::fs::symlink(root.join("bin").join("git"), chain.join("git")).unwrap();

        for entry in [root.join("bin"), chain] {
            let (_, message) = refusal(resolve_outside(
                OsStr::new("git"),
                Some(entry.as_os_str()),
                &root,
            ));
            assert!(message.contains("inside the repository"), "{message}");
        }
    }

    /// Homebrew's shape: `bin/git -> ../Cellar/git/x/bin/git`, all outside.
    #[cfg(unix)]
    #[test]
    fn a_chain_that_stays_outside_resolves_to_its_end() {
        let base = Scratch::new("cellar");
        let root = base.0.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let installed = plant(
            &base.0.join("Cellar").join("git").join("2.0").join("bin"),
            "git",
            0o755,
        );
        std::fs::create_dir_all(base.0.join("bin")).unwrap();
        std::os::unix::fs::symlink("../Cellar/git/2.0/bin/git", base.0.join("bin").join("git"))
            .unwrap();

        let got = resolve_outside(
            OsStr::new("git"),
            Some(base.0.join("bin").as_os_str()),
            &root,
        )
        .unwrap();
        assert_eq!(got, std::fs::canonicalize(installed).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_loop_is_an_error_not_a_hang() {
        let base = Scratch::new("loop");
        let root = base.0.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(base.0.join("bin")).unwrap();
        let a = base.0.join("bin").join("git");
        let b = base.0.join("b");
        std::os::unix::fs::symlink(&b, &a).unwrap();
        std::os::unix::fs::symlink(&a, &b).unwrap();
        // A looping link is not an executable to `metadata`, so it is passed
        // over and the search ends in "not found" rather than spinning.
        let (kind, _) = refusal(resolve_outside(
            OsStr::new("git"),
            Some(base.0.join("bin").as_os_str()),
            &root,
        ));
        assert_eq!(kind, ErrorKind::NotFound);
        // And the chain walk itself is bounded when called on one directly.
        assert!(follow_outside(&a, &root).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_file_without_the_execute_bit_is_passed_over() {
        let base = Scratch::new("noexec");
        let root = base.0.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        plant(&base.0.join("first"), "git", 0o644);
        let installed = plant(&base.0.join("second"), "git", 0o755);
        let path = std::env::join_paths([base.0.join("first"), base.0.join("second")]).unwrap();

        let got = resolve_outside(OsStr::new("git"), Some(&path), &root).unwrap();
        assert_eq!(got, std::fs::canonicalize(installed).unwrap());
    }

    #[test]
    fn a_git_found_nowhere_is_not_found() {
        let base = Scratch::new("nowhere");
        let (kind, _) = refusal(resolve_outside(
            OsStr::new("git"),
            Some(base.0.as_os_str()),
            &base.0,
        ));
        assert_eq!(kind, ErrorKind::NotFound);
        let (kind, _) = refusal(resolve_outside(OsStr::new("git"), None, &base.0));
        assert_eq!(kind, ErrorKind::NotFound);
    }

    /// A root that does not exist contains nothing, so git is still found and
    /// reports the missing root itself, as `git`'s `-C root` contract says.
    #[cfg(unix)]
    #[test]
    fn a_missing_root_leaves_the_error_to_git() {
        let base = Scratch::new("missing-root");
        let installed = plant(&base.0.join("bin"), "git", 0o755);
        let missing = base.0.join("no-such-repo");

        let got = resolve_outside(
            OsStr::new("git"),
            Some(base.0.join("bin").as_os_str()),
            &missing,
        )
        .unwrap();
        assert_eq!(got, std::fs::canonicalize(installed).unwrap());
    }

    /// An explicit program — a path, not a name — is the caller's choice, used
    /// as named even inside the repository, and made absolute.
    #[cfg(unix)]
    #[test]
    fn an_explicit_program_is_used_as_named() {
        let base = Scratch::new("explicit");
        let root = base.0.join("repo");
        let stand_in = plant(&root.join("bin"), "git", 0o755);

        let got = resolve_outside(stand_in.as_os_str(), Some(OsStr::new("")), &root).unwrap();
        assert_eq!(got, stand_in);
        let got = resolve_outside(OsStr::new("some/dir/git"), None, &root).unwrap();
        assert!(
            got.is_absolute() && got.ends_with("some/dir/git"),
            "{}",
            got.display()
        );
    }

    #[test]
    fn a_transient_executable_writer_retries_without_duplicating_a_child() {
        let mut attempts = 0;
        let mut child = spawn_with_retry(
            || {
                attempts += 1;
                if attempts < 3 {
                    Err(std::io::Error::from(ErrorKind::ExecutableFileBusy))
                } else {
                    Command::new(std::env::current_exe().unwrap())
                        .arg("--list")
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                }
            },
            "fixture",
            Instant::now(),
            Duration::from_secs(5),
        )
        .expect("transient writer must clear");
        assert!(child.wait().unwrap().success());
        assert_eq!(attempts, 3, "success must end retries immediately");
    }

    #[test]
    fn a_persistent_executable_writer_has_a_finite_retry_budget() {
        let mut attempts = 0;
        let failure = spawn_with_retry(
            || {
                attempts += 1;
                Err(std::io::Error::from(ErrorKind::ExecutableFileBusy))
            },
            "fixture",
            Instant::now(),
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(matches!(failure, Failure::Spawn { .. }));
        assert_eq!(attempts, 8);
    }

    #[test]
    fn executable_retries_do_not_reset_the_overall_deadline() {
        let started = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        let mut attempts = 0;
        let failure = spawn_with_retry(
            || {
                attempts += 1;
                Err(std::io::Error::from(ErrorKind::ExecutableFileBusy))
            },
            "fixture",
            started,
            Duration::from_millis(20),
        )
        .unwrap_err();
        assert!(matches!(failure, Failure::Deadline { .. }));
        assert_eq!(attempts, 0, "an expired request must not execute");
    }

    #[test]
    fn an_ordinary_spawn_error_is_never_retried() {
        let mut attempts = 0;
        let failure = spawn_with_retry(
            || {
                attempts += 1;
                Err(std::io::Error::from(ErrorKind::PermissionDenied))
            },
            "fixture",
            Instant::now(),
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(
            matches!(failure, Failure::Spawn { error, .. } if error.kind() == ErrorKind::PermissionDenied)
        );
        assert_eq!(attempts, 1);
    }

    /// A reader that returns a scripted sequence, so `drain`'s error handling
    /// can be exercised without arranging a signal.
    struct Scripted(std::collections::VecDeque<std::io::Result<&'static [u8]>>);

    impl Read for Scripted {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.pop_front() {
                None => Ok(0),
                Some(Err(error)) => Err(error),
                Some(Ok(bytes)) => {
                    buf[..bytes.len()].copy_from_slice(bytes);
                    Ok(bytes.len())
                }
            }
        }
    }

    fn scripted(steps: Vec<std::io::Result<&'static [u8]>>) -> Option<Scripted> {
        Some(Scripted(steps.into_iter().collect()))
    }

    /// `read` is allowed to return `Interrupted` before it has transferred
    /// anything, and `std` retries that only inside `read_to_end` /
    /// `read_exact` — never on a bare `read`. Reading it as EOF drops the rest
    /// of the child's answer.
    #[test]
    fn an_interrupted_read_is_resumed_not_read_as_the_end() {
        let drained = drain(
            scripted(vec![
                Ok(b"first "),
                Err(std::io::Error::from(ErrorKind::Interrupted)),
                Ok(b"second"),
            ]),
            1 << 20,
        );
        assert_eq!(drained.bytes, b"first second");
        assert!(
            !drained.truncated,
            "nothing was lost, so nothing may be reported as lost"
        );
    }

    /// A pipe that stops answering before EOF leaves a *prefix*. The whole
    /// point of the cap is that a prefix is never presented as the whole
    /// answer, and a read error produces exactly the same prefix.
    #[test]
    fn a_pipe_that_fails_before_eof_leaves_a_prefix_that_says_so() {
        let drained = drain(
            scripted(vec![
                Ok(b"as far as this"),
                Err(std::io::Error::from(ErrorKind::BrokenPipe)),
                Ok(b"never seen"),
            ]),
            1 << 20,
        );
        assert_eq!(drained.bytes, b"as far as this");
        assert!(
            drained.truncated,
            "the read stopped short of EOF; the caller is holding a prefix and \
             has just been told it is the whole answer"
        );
    }

    /// The cap's own case, unchanged: bytes past it are dropped and reported.
    #[test]
    fn the_cap_still_reports_its_own_truncation() {
        let drained = drain(scripted(vec![Ok(b"0123456789")]), 4);
        assert_eq!(drained.bytes, b"0123");
        assert!(drained.truncated);
    }

    /// …and a clean read to EOF reports nothing.
    #[test]
    fn a_clean_read_reports_no_truncation() {
        let drained = drain(scripted(vec![Ok(b"whole")]), 1 << 20);
        assert_eq!(drained.bytes, b"whole");
        assert!(!drained.truncated);
    }

    /// The signal API reaches a whole group the caller created, including a
    /// descendant, and reports the group gone once it is.
    #[cfg(unix)]
    #[test]
    fn a_group_is_probed_terminated_and_then_reported_gone() {
        use std::os::unix::process::CommandExt;
        let mut child = Command::new("sh")
            .args(["-c", "sleep 30 & wait"])
            .process_group(0)
            .spawn()
            .expect("sh spawns");
        let leader = child.id();
        signal_group(leader, Signal::Probe).expect("a live group answers the probe");
        signal_process(leader, Signal::Probe).expect("its leader answers too");
        signal_group(leader, Signal::Terminate).expect("the group takes TERM");
        let _ = child.wait();
        // The backgrounded sleep was in the group and got TERM as well; give it
        // a moment to be reaped by init.
        let deadline = Instant::now() + Duration::from_secs(5);
        // Poll for ESRCH itself: while the backgrounded sleep is a zombie
        // awaiting init, macOS answers the group probe with EPERM, which is
        // neither "alive" nor "gone".
        loop {
            match signal_group(leader, Signal::Probe) {
                Err(gone) if target_is_gone(&gone) => break,
                answer => assert!(
                    Instant::now() < deadline,
                    "a TERMed group still answers {answer:?} after 5 s"
                ),
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// 0, 1 and negatives name groups the caller does not own — its own, or
    /// init's — and are refused before any signal is sent.
    #[cfg(unix)]
    #[test]
    fn signals_refuse_targets_that_name_someone_elses_group() {
        for pid in [0, 1] {
            assert_eq!(
                signal_group(pid, Signal::Probe).unwrap_err().kind(),
                ErrorKind::InvalidInput,
                "group {pid}"
            );
        }
        assert_eq!(
            signal_process(0, Signal::Probe).unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
        assert_eq!(
            signal_process(u32::MAX, Signal::Probe).unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
    }
}
