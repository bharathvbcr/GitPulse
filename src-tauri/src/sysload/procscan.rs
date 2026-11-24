//! The process table, read once per tick: which agent sessions are running
//! and where, which processes GitPulse launched and what they cost, who else
//! is heavy, whether memguard is watching, and which foreign processes look
//! orphaned.
//!
//! # Sessions by executable, not by registry
//!
//! A Claude Code session is a process whose executable path is Claude's
//! (`…/claude`, the native installer's `…/claude/versions/<version>`, or a
//! Node process running `@anthropic-ai/claude-code`). Its short name is a
//! version string, so a name match would miss every one. The path comes from
//! `KERN_PROCARGS2`, which keeps the spelling the process was exec'd with — a
//! symlink named `claude` is `claude` there, where `proc_pidpath` would have
//! resolved it to its target. Codex is matched the same way (`…/codex`, or
//! Node running `@openai/codex`). A process whose direct parent is the same
//! kind is that parent's wrapper target, not a second session; one started in
//! a shell under another session is a session of its own.
//!
//! # What `observe_sessions` reads
//!
//! `gitpulse-hook` is a new process per call, so it cannot amortise a full
//! table read. `observe_sessions` lists pids and reads each process's short
//! name, parent and start time. It reads the argument block (`KERN_PROCARGS2`,
//! or `/proc/<pid>/cmdline` and `environ`) only when that name could be an
//! agent, or when the name is empty or a version string and the resolved
//! executable could. Footprints and disk writes stay unread: a session from
//! this path reports its footprint as unavailable, which is not zero. A table
//! that could not be listed is an error, never an empty session list. The
//! sampler's [`Scanner::scan`] still reads every identity, footprint and
//! write.
//!
//! # Launched trees
//!
//! A process GitPulse started in a terminal carries `GITPULSE_SESSION_ID`,
//! and a task attempt also `GITPULSE_RUN_ID`; every descendant inherits them.
//! A process belongs to the tree of the nearest ancestor-or-self that carries
//! one, so a child reparented to launchd stays attributed through its own
//! environment. The limit, recorded in `docs/LOAD_GOVERNOR.md`: macOS hides
//! the environment of Apple platform binaries (`/bin/sh`, `/usr/bin/grep`),
//! so one of those that is reparented is lost to its tree.
//!
//! # Never zero for "could not look"
//!
//! A footprint the OS refused is [`Why::Unreadable`] and counted as such;
//! another user's processes are counted as not inspected. Sums say how many
//! members they could not read.

// Where there is no process scan (Windows), the parsing and attribution
// helpers the macOS and Linux readers feed have no caller outside the tests.
#![cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]

use super::sensor::{Reading, Why};
use super::RUN_ID_ENV;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The environment variable every GitPulse PTY child carries.
pub(crate) const SESSION_ENV: &str = crate::alerts::bridge::SESSION_ENV;

/// Upper bound on the processes one scan reads; past it the table is a scan
/// that could not finish, never a prefix judged as the whole.
pub(crate) const MAX_SCANNED_PROCESSES: usize = 65_536;
/// Ancestor hops one walk follows: deeper than any real tree, and a bound on
/// a corrupted or cyclic parent chain.
const MAX_DEPTH: usize = 128;
/// Sessions whose working directory one scan reads.
const MAX_SESSIONS: usize = 256;
/// Longest launch id taken from an environment.
const MAX_ID_CHARS: usize = 128;
/// How many of the heaviest foreign processes Health shows.
pub const FOREIGN_TOP: usize = 5;
/// An orphan must have run this long ...
pub const ORPHAN_MIN_AGE_US: u64 = 30 * 60 * 1_000_000;
/// ... spending at least this share of it on a CPU.
pub const ORPHAN_MIN_CPU_RATIO: f64 = 0.8;
/// Orphans one scan reports.
const MAX_ORPHANS: usize = 16;

/// An agent CLI GitPulse can recognise by its executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Claude,
    Codex,
    Antigravity,
    DevCouncil,
    Manvi,
}

impl AgentKind {
    /// Whether this kind is an IDE's agent, which runs from the application's
    /// own directory (often `/`) and not from the repository it works on. For
    /// the CLI agents the process's cwd is where the agent works.
    pub(crate) fn is_ide_hosted(self) -> bool {
        matches!(self, Self::Antigravity)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Antigravity => "antigravity",
            Self::DevCouncil => "devcouncil",
            Self::Manvi => "manvi",
        }
    }
}

/// Recognises an agent CLI from the path it was exec'd with and its first
/// argument (the script, when the executable is a JavaScript runtime).
pub(crate) fn classify(exec: &str, arg1: Option<&str>) -> Option<AgentKind> {
    let path = Path::new(exec);
    fn name(p: Option<&Path>) -> Option<&str> {
        p.and_then(Path::file_name).and_then(|n| n.to_str())
    }
    let base = name(Some(path))?;
    let parent = name(path.parent());
    let grandparent = name(path.parent().and_then(Path::parent));
    if base == "claude" || (parent == Some("versions") && grandparent == Some("claude")) {
        return Some(AgentKind::Claude);
    }
    if base == "codex" {
        return Some(AgentKind::Codex);
    }
    // The app bundle's binary is `Antigravity` (capitalised); the CLI is `agy`
    // (or `antigravity`). Helpers (`Antigravity Helper`) are neither.
    if base == "agy"
        || base.eq_ignore_ascii_case("antigravity")
        || base
            .get(..12)
            .is_some_and(|head| head.eq_ignore_ascii_case("antigravity-"))
    {
        return Some(AgentKind::Antigravity);
    }
    // Not `dev`: that name is also Jarvis's CLI. DevCouncil's own binary is
    // `devcouncil` (`dev` is only a symlink or npm alias to it).
    if base == "devcouncil" {
        return Some(AgentKind::DevCouncil);
    }
    if base == "manvi" {
        return Some(AgentKind::Manvi);
    }
    let runtime = base == "node" || base == "bun" || base.starts_with("node2");
    if runtime {
        let script = arg1?;
        if script.contains("/@anthropic-ai/claude-code/") {
            return Some(AgentKind::Claude);
        }
        if script.contains("/@openai/codex/") {
            return Some(AgentKind::Codex);
        }
        // DevCouncil's npm shim (`bin/devcouncil.js`) or a global-bin link to
        // it. A path merely under a devcouncil checkout (vite, vitest) is not.
        let script_name = Path::new(script).file_name().and_then(|n| n.to_str());
        if matches!(script_name, Some("devcouncil" | "devcouncil.js")) {
            return Some(AgentKind::DevCouncil);
        }
    }
    None
}

/// Longest argument list searched for an identifying flag.
const MAX_CLASSIFY_ARGS: usize = 256;

/// [`classify`] with the whole argument list, for the binaries that are only
/// identified by a flag: Antigravity's `language_server` runs as
/// `language_server --override_ide_name antigravity`, and the same generic
/// binary name is shipped by other IDEs (`--override_ide_name windsurf`),
/// which are not Antigravity.
pub(crate) fn classify_argv(exec: &str, argv: &[String]) -> Option<AgentKind> {
    if let Some(kind) = classify(exec, argv.get(1).map(String::as_str)) {
        return Some(kind);
    }
    let is_language_server = Path::new(exec)
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "language_server");
    if !is_language_server {
        return None;
    }
    let mut args = argv.iter().take(MAX_CLASSIFY_ARGS).map(String::as_str);
    while let Some(arg) = args.next() {
        let value = if arg == "--override_ide_name" {
            args.next()
        } else {
            arg.strip_prefix("--override_ide_name=")
        };
        if let Some(value) = value {
            return value
                .eq_ignore_ascii_case("antigravity")
                .then_some(AgentKind::Antigravity);
        }
    }
    None
}

/// Whether the short process name is one whose arguments can name an agent.
///
/// Exact for `claude` and `codex`, so `Claude` and `codex-code-mode-host` are
/// not candidates. A prefix for `node`, whose versioned binaries are `node`,
/// `node2` and `nodejs`.
pub(crate) fn name_selects_procargs(name: &str) -> bool {
    name == "claude"
        || name == "codex"
        || name == "agy"
        || name.eq_ignore_ascii_case("antigravity")
        || name == "language_server"
        || name == "devcouncil"
        || name == "manvi"
        || name == "bun"
        || name.starts_with("node")
}

/// Whether a classified process is an IDE-hosted agent rather than a CLI one.
///
/// Antigravity's `Antigravity` app bundle and `language_server` run from `/`
/// and infer their workspace from descendants, whereas the `agy` CLI runs in
/// the developer's working directory like `claude` and `codex`.
pub(crate) fn is_exec_ide_hosted(exec: &str, kind: AgentKind) -> bool {
    if !kind.is_ide_hosted() {
        return false;
    }
    Path::new(exec).file_name().and_then(|n| n.to_str()) != Some("agy")
}

/// A name the kernel reports for a versioned installer binary: empty, or a
/// short string of digits, dots, dashes and underscores that starts with a
/// digit. `1Password` is a product name, not a version.
pub(crate) fn name_wants_resolved_path(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() {
        return true;
    }
    if bytes.len() > 32 || !bytes[0].is_ascii_digit() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_digit() || matches!(*b, b'.' | b'-' | b'_'))
}

/// Whether a resolved executable is worth an argument read: it is an agent on
/// its own, or a JavaScript runtime whose script might be one.
pub(crate) fn path_selects_procargs(path: &str) -> bool {
    if classify(path, None).is_some() {
        return true;
    }
    let base = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    base == "bun" || base == "language_server" || base.starts_with("node")
}

/// What the sessions scan still has to read for one process.
pub(crate) enum SessionProbe {
    /// The short name is a candidate. Arguments decide, and the resolved path
    /// is the fallback when they cannot be read.
    Args,
    /// The resolved path is a candidate. The string is that path.
    ArgsOf(String),
}

/// `None` when this process cannot be an agent, so its arguments stay unread.
pub(crate) fn session_probe(
    name: &str,
    resolve: impl FnOnce() -> Option<String>,
) -> Option<SessionProbe> {
    if name_selects_procargs(name) {
        return Some(SessionProbe::Args);
    }
    if !name_wants_resolved_path(name) {
        return None;
    }
    let path = resolve()?;
    path_selects_procargs(&path).then_some(SessionProbe::ArgsOf(path))
}

/// One process the sessions-only scan has already classified.
pub(crate) struct SessionRow {
    pub pid: i32,
    pub ppid: i32,
    pub start_us: u64,
    pub kind: AgentKind,
    pub ide_hosted: bool,
    pub launch: Option<LaunchKey>,
}

/// A process's parent and start, for every process the sessions-only scan
/// listed, not only the candidates. A session's ancestor is found by walking
/// these through the shells and runtimes between it and the agent above it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcLink {
    pub ppid: i32,
    /// Microseconds since the Unix epoch; 0 when the OS would not say.
    pub start_us: u64,
}

/// What the sessions-only scan read: the classified rows, and the parent link
/// of every process it listed.
pub(crate) struct SessionTable {
    pub rows: Vec<SessionRow>,
    pub links: HashMap<i32, ProcLink>,
    /// The working directory already read for a row, so it is read once: the
    /// OS's, or for an IDE-hosted session outside every repository the one
    /// repository its work descendants all name. A row not in the map has its
    /// directory read when the sessions are assembled.
    pub cwds: HashMap<i32, Option<PathBuf>>,
}

impl From<Vec<SessionRow>> for SessionTable {
    /// A table whose only known processes are its rows: an ancestor that is
    /// not itself a candidate is then not walked through.
    fn from(rows: Vec<SessionRow>) -> Self {
        let links = rows
            .iter()
            .map(|row| {
                (
                    row.pid,
                    ProcLink {
                        ppid: row.ppid,
                        start_us: row.start_us,
                    },
                )
            })
            .collect();
        Self {
            rows,
            links,
            cwds: HashMap::new(),
        }
    }
}

/// Descendant depth, descendant count, arguments per descendant and bytes of
/// one argument that a repository hint reads. An argument over the byte bound
/// is read as an empty one, so the arguments around it keep their positions.
const MAX_HINT_DEPTH: usize = 6;
const MAX_HINT_DESCENDANTS: usize = 64;
const MAX_HINT_ARGS: usize = 64;
const MAX_HINT_ARG_BYTES: usize = 4096;
/// Flags whose value is a path inside the repository the process works on.
const HINT_FLAGS: [&str; 7] = [
    "--manifest-path",
    "--root",
    "--repo-path",
    "--repo_path",
    "--repo",
    "--workspace",
    "--cwd",
];

/// The checkout containing `path`, canonicalised: the nearest ancestor with a
/// `.git` entry (a directory, or the file a linked worktree has). `/` is never
/// one. A path that cannot be canonicalised (missing, a symlink loop) is no
/// evidence, so two spellings of one checkout are one root.
fn repo_root_of(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let real = std::fs::canonicalize(path).ok()?;
    real.ancestors()
        .take(MAX_DEPTH)
        .filter(|dir| dir.parent().is_some())
        .find(|dir| std::fs::symlink_metadata(dir.join(".git")).is_ok())
        .map(Path::to_path_buf)
}

/// The values of `argv` that follow one of [`HINT_FLAGS`], as `--flag value`
/// or `--flag=value`, paired by position over the first [`MAX_HINT_ARGS`]
/// arguments; a value over [`MAX_HINT_ARG_BYTES`] or empty is then dropped.
fn hint_paths(argv: &[String]) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut args = argv.iter().take(MAX_HINT_ARGS).map(String::as_str);
    while let Some(arg) = args.next() {
        let value = match arg.split_once('=') {
            Some((flag, value)) if HINT_FLAGS.contains(&flag) => Some(value),
            _ if HINT_FLAGS.contains(&arg) => args.next(),
            _ => None,
        };
        if let Some(value) = value.filter(|v| !v.is_empty() && v.len() <= MAX_HINT_ARG_BYTES) {
            paths.push(PathBuf::from(value));
        }
    }
    paths
}

/// Whether a command line is a tool server and not work: its executable or
/// any argument that is not a [`HINT_FLAGS`] value has `mcp` as a word
/// (`devmap ... mcp`, `devcouncil mcp-server`, `gitpulse-mcp`,
/// `node mcp/dist/index.js`, `npx chrome-devtools-mcp@latest`). A server is
/// ambient: it runs in whatever repository its host was opened on, so where it
/// points says nothing about where the agent works.
fn is_tool_server(argv: &[String]) -> bool {
    let word = |text: &str| {
        text.split(|c: char| !c.is_ascii_alphanumeric())
            .any(|w| w.eq_ignore_ascii_case("mcp"))
    };
    let mut args = argv.iter().take(MAX_HINT_ARGS).map(String::as_str);
    let program = args.next().unwrap_or("");
    if word(
        Path::new(program)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(program),
    ) {
        return true;
    }
    while let Some(arg) = args.next() {
        if arg.len() > MAX_HINT_ARG_BYTES {
            continue;
        }
        if HINT_FLAGS.contains(&arg) {
            args.next();
        } else if !arg
            .split_once('=')
            .is_some_and(|(flag, _)| HINT_FLAGS.contains(&flag))
            && word(arg)
        {
            return true;
        }
    }
    false
}

/// Descendants of `root` in breadth-first order, to [`MAX_HINT_DEPTH`] levels
/// and [`MAX_HINT_DESCENDANTS`] processes.
fn descendants_of(root: i32, children: &HashMap<i32, Vec<i32>>) -> Vec<i32> {
    let mut found: Vec<i32> = Vec::new();
    let mut seen = std::collections::HashSet::from([root]);
    let mut level = vec![root];
    for _ in 0..MAX_HINT_DEPTH {
        let mut next = Vec::new();
        for pid in &level {
            for child in children.get(pid).into_iter().flatten() {
                if found.len() >= MAX_HINT_DESCENDANTS {
                    return found;
                }
                if seen.insert(*child) {
                    found.push(*child);
                    next.push(*child);
                }
            }
        }
        level = next;
    }
    found
}

/// The one repository the work descendants of `pid` all name, or `None`.
///
/// A descendant is evidence only if its arguments were read and it is not a
/// tool server ([`is_tool_server`]); it names a repository by a flag from
/// [`HINT_FLAGS`] or by its own working directory. Any disagreement, and no
/// evidence, is `None`: a wrong repository is worse than an unknown one.
fn work_repo_of(
    pid: i32,
    children: &HashMap<i32, Vec<i32>>,
    cwd_of: &dyn Fn(i32) -> Option<PathBuf>,
    argv_of: &mut dyn FnMut(i32) -> Option<Vec<String>>,
) -> Option<PathBuf> {
    let mut roots: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
    for child in descendants_of(pid, children) {
        let Some(argv) = argv_of(child) else {
            continue;
        };
        if is_tool_server(&argv) {
            continue;
        }
        roots.extend(
            hint_paths(&argv)
                .into_iter()
                .chain(cwd_of(child))
                .filter_map(|path| repo_root_of(&path)),
        );
    }
    if roots.len() == 1 {
        roots.pop_first()
    } else {
        None
    }
}

/// The working directory of each row, read once. It is the OS's, except for an
/// IDE-hosted session whose own directory is in no repository, which takes
/// [`work_repo_of`] when there is one. Reads arguments only, spawns nothing.
fn session_cwds(
    rows: &[SessionRow],
    links: &HashMap<i32, ProcLink>,
    cwd_of: &dyn Fn(i32) -> Option<PathBuf>,
    argv_of: &mut dyn FnMut(i32) -> Option<Vec<String>>,
) -> HashMap<i32, Option<PathBuf>> {
    let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
    let mut cwds = HashMap::new();
    for row in rows.iter().take(MAX_SESSIONS) {
        let own = cwd_of(row.pid);
        let outside = own.as_deref().and_then(repo_root_of).is_none();
        let hinted = if row.ide_hosted && outside {
            if children.is_empty() {
                for (pid, link) in links {
                    if *pid != link.ppid {
                        children.entry(link.ppid).or_default().push(*pid);
                    }
                }
            }
            work_repo_of(row.pid, &children, cwd_of, argv_of)
        } else {
            None
        };
        cwds.insert(row.pid, hinted.or(own));
    }
    cwds
}

/// The nearest agent session above the process `pid` (parent `ppid`, started
/// `start_us`), found by walking parents through whatever stands between, or
/// `None` when no session is above it or the chain cannot be trusted.
///
/// The walk stops, with no ancestor, at a parent that is missing from the
/// table, at a cycle or a chain longer than [`MAX_DEPTH`], and at a parent that
/// started *after* its child: a pid the kernel handed to a newer process since
/// the child's real parent exited. `link` reads the table; `is_session` says
/// whether a pid is one of the sessions being reported, so a wrapper target
/// (which is not) is walked through to the session it belongs to.
fn nearest_agent_ancestor(
    pid: i32,
    ppid: i32,
    start_us: u64,
    link: impl Fn(i32) -> Option<ProcLink>,
    is_session: impl Fn(i32) -> bool,
) -> Option<i32> {
    let (mut child_start, mut at) = (start_us, ppid);
    for _ in 0..MAX_DEPTH {
        if at <= 0 || at == pid {
            return None;
        }
        let parent = link(at)?;
        if parent.start_us != 0 && child_start != 0 && parent.start_us > child_start {
            return None;
        }
        if is_session(at) {
            return Some(at);
        }
        child_start = parent.start_us;
        at = parent.ppid;
    }
    None
}

/// Arguments that were read, or `None` when the read failed. A readable
/// argument list that is not an agent is not rescued by the resolved path.
pub(crate) fn session_row(
    pid: i32,
    ppid: i32,
    start_us: u64,
    probe: SessionProbe,
    args: Option<(String, Vec<String>, Option<LaunchKey>)>,
    resolve: impl FnOnce() -> Option<String>,
) -> Option<SessionRow> {
    if let Some((exec, argv, launch)) = args {
        let kind = classify_argv(&exec, &argv)?;
        let ide_hosted = is_exec_ide_hosted(&exec, kind);
        return Some(SessionRow {
            pid,
            ppid,
            start_us,
            kind,
            ide_hosted,
            launch,
        });
    }
    let path = match probe {
        SessionProbe::ArgsOf(path) => path,
        SessionProbe::Args => resolve()?,
    };
    let kind = classify(&path, None)?;
    let ide_hosted = is_exec_ide_hosted(&path, kind);
    Some(SessionRow {
        pid,
        ppid,
        start_us,
        kind,
        ide_hosted,
        launch: None,
    })
}

/// Which GitPulse launch a process belongs to, from its own environment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub(crate) struct LaunchKey {
    pub run_id: Option<String>,
    pub session_id: Option<String>,
}

impl LaunchKey {
    /// The tree this key names: the task run when there is one, else the
    /// terminal session.
    fn tree(&self) -> Option<String> {
        self.run_id
            .as_ref()
            .map(|id| format!("run:{id}"))
            .or_else(|| self.session_id.as_ref().map(|id| format!("session:{id}")))
    }

    /// Picks `GITPULSE_RUN_ID` and `GITPULSE_SESSION_ID` out of `NAME=value`
    /// strings. An empty, oversized or non-printable value is ignored.
    pub(crate) fn from_env<'a>(vars: impl IntoIterator<Item = &'a str>) -> Option<Self> {
        let clean = |v: &str| {
            (!v.is_empty()
                && v.chars().count() <= MAX_ID_CHARS
                && v.chars().all(|c| c.is_ascii_graphic()))
            .then(|| v.to_string())
        };
        let mut key = Self::default();
        for var in vars {
            if let Some(v) = var
                .strip_prefix(RUN_ID_ENV)
                .and_then(|r| r.strip_prefix('='))
            {
                key.run_id = clean(v);
            } else if let Some(v) = var
                .strip_prefix(SESSION_ENV)
                .and_then(|r| r.strip_prefix('='))
            {
                key.session_id = clean(v);
            }
        }
        (key.run_id.is_some() || key.session_id.is_some()).then_some(key)
    }
}

/// What does not change for the life of one process, read once per
/// (pid, start) and shared across ticks.
#[derive(Debug, Default)]
pub(crate) struct Identity {
    pub name: String,
    /// The path it was exec'd with; `None` when the OS would not say.
    pub exec: Option<String>,
    /// Every argument, kept only for memguard, whose flags Health shows.
    pub argv: Vec<String>,
    pub kind: Option<AgentKind>,
    pub ide_hosted: bool,
    pub launch: Option<LaunchKey>,
    pub memguard: bool,
}

impl Identity {
    pub(crate) fn new(
        name: String,
        exec: Option<String>,
        argv: Vec<String>,
        launch: Option<LaunchKey>,
    ) -> Self {
        let kind = exec.as_deref().and_then(|e| classify_argv(e, &argv));
        let ide_hosted = match (exec.as_deref(), kind) {
            (Some(e), Some(k)) => is_exec_ide_hosted(e, k),
            (_, Some(k)) => k.is_ide_hosted(),
            _ => false,
        };
        let memguard = name == "memguard"
            || exec
                .as_deref()
                .and_then(|e| Path::new(e).file_name())
                .is_some_and(|n| n == "memguard");
        Self {
            name,
            exec,
            argv: if memguard { argv } else { Vec::new() },
            kind,
            ide_hosted,
            launch,
            memguard,
        }
    }
}

/// One process as one tick saw it.
#[derive(Debug, Clone)]
pub(crate) struct ProcFacts {
    pub pid: i32,
    pub ppid: i32,
    pub uid: u32,
    /// Microseconds since the Unix epoch.
    pub start_us: u64,
    pub ident: Arc<Identity>,
    pub footprint: Reading<u64>,
    /// Cumulative bytes written to storage.
    pub written: Reading<u64>,
    /// Cumulative CPU time.
    pub cpu_ns: Reading<u64>,
}

/// Where process facts come from: the OS, or a fixture.
pub(crate) trait ProcSource: Send {
    /// Fills `out` with every process. `Err` when the table itself could not
    /// be read — never "no processes".
    fn read(&mut self, out: &mut Vec<ProcFacts>) -> Result<(), String>;
    fn cwd(&self, pid: i32) -> Option<PathBuf>;
    /// The path behind descriptor `fd` of `pid`.
    fn fd_path(&self, pid: i32, fd: i32) -> Reading<String>;
    /// The user this process runs as.
    fn me_uid(&self) -> u32;
    /// A process's real, effective and saved uids, for the signal table. The
    /// default is "not reported": the scan's one uid stands for all three.
    fn creds(&self, _pid: i32) -> Option<Creds> {
        None
    }
    /// The executable the kernel runs a process from, for the signal table.
    /// Unlike the exec spelling a scan carries (on Linux `argv[0]` when it is
    /// a path), a process cannot choose it. The default is "not reported": the
    /// OS's exec path is already the kernel's.
    fn real_exe(&self, _pid: i32) -> Option<String> {
        None
    }
    /// Whether [`ProcSource::real_exe`] is how this OS says what a process
    /// runs, so that a process it cannot read is one whose identity is not
    /// known. False where the exec path a scan carries is already the kernel's.
    fn reports_real_exe(&self) -> bool {
        false
    }
    /// Whether the sender can signal every process, and so whether the signal
    /// table must read the arguments of every process, not only its own.
    fn is_root(&self) -> bool {
        self.me_uid() == 0
    }
}

/// Launched and observed sessions of one kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCount {
    /// Started by GitPulse (the process carries a GitPulse launch id).
    pub launched: u32,
    /// Running without one: started in another terminal or host.
    pub observed: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentSession {
    pub kind: AgentKind,
    pub pid: i32,
    pub start_us: u64,
    pub cwd: Option<String>,
    /// The launched tree this session is in, when GitPulse started it.
    pub tree: Option<String>,
    pub run_id: Option<String>,
    /// The pid of the nearest agent session above this one, found by walking
    /// parents through any shells and runtimes between them. `None` is "no
    /// session above it that the scan could see", not "launched by a person":
    /// a parent that exited, a reused pid and a chain past the walk bound all
    /// read the same. A direct same-kind parent is a wrapper (this process
    /// would not be a session), so it is never reported here.
    pub parent_pid: Option<i32>,
    /// CPU time the session and its descendants used per second since the
    /// previous tick, in nanoseconds (a full core is 1,000,000,000). Not a
    /// rate on the first tick, and unreadable when any member's CPU time was.
    pub cpu_rate: Reading<u64>,
    /// The session's own process plus every descendant still attached.
    pub footprint: Reading<u64>,
    /// Bytes per second the session's own process and its attached
    /// descendants wrote since the previous tick: the same members as
    /// `footprint`, each process counted in exactly one session. A floor,
    /// never the whole: a process that started and exited between two ticks is
    /// never read, a process that exited since the last tick takes its latest
    /// bytes with it, and a process whose counter cannot be read adds nothing.
    /// It says which processes wrote, not to which folder.
    pub write_rate: Reading<u64>,
    pub members: u32,
    pub unreadable_members: u32,
    #[serde(skip)]
    pub(crate) ide_hosted: bool,
    #[serde(skip)]
    pub(crate) terminal_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LaunchedTree {
    pub tree: String,
    pub run_id: Option<String>,
    pub session_id: Option<String>,
    pub members: u32,
    pub unreadable_members: u32,
    pub footprint: Reading<u64>,
    /// Bytes per second written since the previous tick.
    pub write_rate: Reading<u64>,
    /// CPU nanoseconds per second used since the previous tick (see
    /// [`AgentSession::cpu_rate`]).
    pub cpu_rate: Reading<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ForeignProcess {
    pub pid: i32,
    pub name: String,
    pub footprint: u64,
    /// When this is an agent session GitPulse did not launch.
    pub session: Option<AgentKind>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Orphan {
    pub pid: i32,
    pub start_us: u64,
    pub name: String,
    pub exec: Option<String>,
    pub cwd: String,
    /// The git checkout its directory is in.
    pub checkout: String,
    pub age_s: u64,
    pub cpu_ratio: f64,
    pub footprint: Reading<u64>,
}

/// memguard as the process table shows it. GitPulse reports on it and never
/// starts, restarts or signals it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MemguardHealth {
    pub present: bool,
    pub pid: Option<i32>,
    pub start_us: Option<u64>,
    pub exec: Option<String>,
    pub flags: Vec<String>,
    /// Where its stdout goes, which is where it logs.
    pub log_path: Reading<String>,
    /// Seconds since the log was last written.
    pub heartbeat_age_s: Reading<u64>,
    /// Its `-heartbeat` interval, when it was given one.
    pub heartbeat_interval_s: Option<u64>,
    pub warn: bool,
    pub warnings: Vec<String>,
}

impl MemguardHealth {
    fn absent(governed: bool) -> Self {
        Self {
            present: false,
            pid: None,
            start_us: None,
            exec: None,
            flags: Vec::new(),
            log_path: Reading::Unavailable(Why::Unsupported),
            heartbeat_age_s: Reading::Unavailable(Why::Unsupported),
            heartbeat_interval_s: None,
            warn: governed,
            warnings: if governed {
                vec!["memguard is not running: no process over 32 GiB will be stopped".into()]
            } else {
                Vec::new()
            },
        }
    }
}

/// One scan's answer.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProcView {
    /// Whether the process table could be read at all.
    pub ok: bool,
    pub error: Option<String>,
    pub processes: u32,
    /// Other users' processes: counted, not inspected.
    pub other_users: u32,
    /// This user's processes whose footprint the OS refused.
    pub unreadable: u32,
    pub claude: SessionCount,
    pub codex: SessionCount,
    /// Sessions of every other kind (Antigravity, DevCouncil, Manvi), so
    /// the per-kind counts add up to the sessions the scan found.
    pub other: SessionCount,
    pub sessions: Vec<AgentSession>,
    /// More sessions ran than one scan describes.
    pub sessions_truncated: bool,
    pub trees: Vec<LaunchedTree>,
    pub launched_footprint: Reading<u64>,
    pub observed_footprint: Reading<u64>,
    pub foreign_top: Vec<ForeignProcess>,
    pub largest_foreign: Reading<u64>,
    pub launched_write_rate: Reading<u64>,
    pub max_tree_write_rate: Reading<u64>,
    pub memguard: MemguardHealth,
    pub orphans: Vec<Orphan>,
}

impl ProcView {
    fn failed(error: String, governed: bool) -> Self {
        let why = Reading::Unavailable(Why::Failed);
        Self {
            ok: false,
            error: Some(error),
            processes: 0,
            other_users: 0,
            unreadable: 0,
            claude: SessionCount::default(),
            codex: SessionCount::default(),
            other: SessionCount::default(),
            sessions: Vec::new(),
            sessions_truncated: false,
            trees: Vec::new(),
            launched_footprint: why,
            observed_footprint: why,
            foreign_top: Vec::new(),
            largest_foreign: why,
            launched_write_rate: why,
            max_tree_write_rate: why,
            memguard: MemguardHealth {
                warnings: vec!["the process table could not be read".into()],
                warn: governed,
                ..MemguardHealth::absent(false)
            },
            orphans: Vec::new(),
        }
    }
}

/// What a scan needs to know about the process doing it.
#[derive(Debug, Clone)]
pub struct ScanContext {
    pub me_pid: i32,
    pub now_us: u64,
    /// `$HOME`, for the memguard log-location check.
    pub home: Option<PathBuf>,
    /// Whether this platform is governed (memguard warnings apply).
    pub governed: bool,
}

impl ScanContext {
    /// The context of the calling process, now.
    pub fn here() -> Self {
        Self {
            me_pid: i32::try_from(std::process::id()).unwrap_or(-1),
            now_us: now_us(),
            home: std::env::var_os("HOME").map(PathBuf::from),
            governed: super::Platform::current() == super::Platform::Governed,
        }
    }
}

pub(crate) fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// A scan that remembers what it saw, for disk-write rates and to read each
/// process's identity once.
pub struct Scanner {
    source: Box<dyn ProcSource>,
    facts: Vec<ProcFacts>,
    index: HashMap<i32, usize>,
    tree_of: Vec<Option<u32>>,
    session_of: Vec<Option<u32>>,
    tree_names: Vec<String>,
    written_before: HashMap<(i32, u64), u64>,
    written_now: HashMap<(i32, u64), u64>,
    cpu_before: HashMap<(i32, u64), u64>,
    cpu_now: HashMap<(i32, u64), u64>,
    previous_us: Option<u64>,
}

const UNSEEN: Option<u32> = None;

impl Scanner {
    pub(crate) fn new(source: Box<dyn ProcSource>) -> Self {
        Self {
            source,
            facts: Vec::with_capacity(4096),
            index: HashMap::with_capacity(4096),
            tree_of: Vec::with_capacity(4096),
            session_of: Vec::with_capacity(4096),
            tree_names: Vec::new(),
            written_before: HashMap::with_capacity(4096),
            written_now: HashMap::with_capacity(4096),
            cpu_before: HashMap::with_capacity(4096),
            cpu_now: HashMap::with_capacity(4096),
            previous_us: None,
        }
    }

    /// The scanner over this operating system's process table.
    pub fn os() -> Self {
        Self::new(Box::new(os::OsSource::new()))
    }

    /// Reads the whole table once and attributes it.
    pub fn scan(&mut self, ctx: &ScanContext) -> ProcView {
        if let Err(error) = self.source.read(&mut self.facts) {
            self.previous_us = None;
            return ProcView::failed(error, ctx.governed);
        }
        let me_uid = self.source.me_uid();
        self.index.clear();
        for (i, f) in self.facts.iter().enumerate() {
            self.index.insert(f.pid, i);
        }
        self.assign_trees();
        let sessions_at = self.assign_sessions(me_uid);
        let view = self.summarise(ctx, me_uid, &sessions_at);
        self.previous_us = Some(ctx.now_us);
        std::mem::swap(&mut self.written_before, &mut self.written_now);
        std::mem::swap(&mut self.cpu_before, &mut self.cpu_now);
        view
    }

    fn parent_of(&self, i: usize) -> Option<usize> {
        let f = &self.facts[i];
        if f.ppid <= 0 || f.ppid == f.pid {
            return None;
        }
        self.index.get(&f.ppid).copied()
    }

    /// Each process's launched tree: the nearest ancestor-or-self carrying a
    /// launch key. Memoised, so the whole table is one pass.
    fn assign_trees(&mut self) {
        self.tree_names.clear();
        let mut names: HashMap<String, u32> = HashMap::new();
        self.tree_of.clear();
        self.tree_of.resize(self.facts.len(), UNSEEN);
        let mut resolved = vec![false; self.facts.len()];
        let mut path = Vec::with_capacity(MAX_DEPTH);
        for start in 0..self.facts.len() {
            path.clear();
            let mut at = Some(start);
            let mut found = None;
            while let Some(i) = at {
                if resolved[i] {
                    found = self.tree_of[i];
                    break;
                }
                if let Some(tree) = self.facts[i]
                    .ident
                    .launch
                    .as_ref()
                    .and_then(LaunchKey::tree)
                {
                    let next = names.len() as u32;
                    let id = *names.entry(tree.clone()).or_insert_with(|| {
                        self.tree_names.push(tree);
                        next
                    });
                    self.tree_of[i] = Some(id);
                    resolved[i] = true;
                    found = Some(id);
                    break;
                }
                path.push(i);
                if path.len() >= MAX_DEPTH {
                    // A walk cut off by the bound proved nothing about the
                    // nodes it passed; leave them for a walk that ends.
                    path.clear();
                    break;
                }
                at = self.parent_of(i);
            }
            for &i in &path {
                self.tree_of[i] = found;
                resolved[i] = true;
            }
        }
    }

    /// The topmost agent sessions, and for each process the session whose
    /// subtree it is in.
    fn assign_sessions(&mut self, me_uid: u32) -> Vec<usize> {
        let mut sessions = Vec::new();
        for i in 0..self.facts.len() {
            let f = &self.facts[i];
            let Some(kind) = f.ident.kind else { continue };
            if f.uid != me_uid {
                continue;
            }
            // A wrapper whose own child is the real binary (Codex's npm shim
            // starting the native `codex`) is one session. A Claude started in
            // a shell somewhere under another Claude — an agent's terminal, a
            // GitPulse dev build run from one — is a session of its own.
            let wrapped = self
                .parent_of(i)
                .is_some_and(|p| self.facts[p].ident.kind == Some(kind));
            if !wrapped {
                sessions.push(i);
            }
        }
        self.session_of.clear();
        self.session_of.resize(self.facts.len(), UNSEEN);
        let mut is_session = vec![None; self.facts.len()];
        for (n, &i) in sessions.iter().enumerate() {
            is_session[i] = Some(n as u32);
        }
        let mut resolved = vec![false; self.facts.len()];
        let mut path = Vec::with_capacity(MAX_DEPTH);
        for start in 0..self.facts.len() {
            path.clear();
            let mut at = Some(start);
            let mut found = None;
            while let Some(i) = at {
                if resolved[i] {
                    found = self.session_of[i];
                    break;
                }
                if let Some(n) = is_session[i] {
                    self.session_of[i] = Some(n);
                    resolved[i] = true;
                    found = Some(n);
                    break;
                }
                path.push(i);
                if path.len() >= MAX_DEPTH {
                    path.clear();
                    break;
                }
                at = self.parent_of(i);
            }
            for &i in &path {
                self.session_of[i] = found;
                resolved[i] = true;
            }
        }
        sessions
    }

    /// Whether `i` is this process or one of its descendants.
    fn is_mine(&self, i: usize, me_pid: i32) -> bool {
        let mut at = Some(i);
        for _ in 0..MAX_DEPTH {
            let Some(p) = at else { return false };
            if self.facts[p].pid == me_pid {
                return true;
            }
            at = self.parent_of(p);
        }
        false
    }

    fn summarise(&mut self, ctx: &ScanContext, me_uid: u32, sessions_at: &[usize]) -> ProcView {
        #[derive(Default, Clone, Copy)]
        struct Sum {
            bytes: u64,
            readable: u32,
            unreadable: u32,
            written: u64,
            /// CPU nanoseconds since the previous tick.
            cpu: u64,
            /// A member's CPU time could not be read, so the sum is not one.
            cpu_unread: bool,
        }
        impl Sum {
            fn add(&mut self, footprint: Reading<u64>) {
                match footprint {
                    Reading::Value(v) => {
                        self.bytes = self.bytes.saturating_add(v);
                        self.readable += 1;
                    }
                    Reading::Unavailable(_) => self.unreadable += 1,
                }
            }
            fn reading(&self) -> Reading<u64> {
                if self.readable == 0 && self.unreadable > 0 {
                    Reading::Unavailable(Why::Unreadable)
                } else {
                    Reading::Value(self.bytes)
                }
            }
        }

        let dt_us = self
            .previous_us
            .and_then(|before| ctx.now_us.checked_sub(before))
            .filter(|dt| *dt > 0);
        let mut trees = vec![Sum::default(); self.tree_names.len()];
        let mut per_session = vec![Sum::default(); sessions_at.len()];
        let (mut launched, mut observed) = (Sum::default(), Sum::default());
        let (mut other_users, mut unreadable) = (0u32, 0u32);
        let mut foreign: Vec<ForeignProcess> = Vec::new();
        let mut orphan_candidates = Vec::new();
        let mut memguard: Option<usize> = None;
        self.written_now.clear();
        self.cpu_now.clear();

        for i in 0..self.facts.len() {
            let f = &self.facts[i];
            if f.uid != me_uid {
                other_users += 1;
            } else if matches!(f.footprint, Reading::Unavailable(Why::Unreadable)) {
                unreadable += 1;
            }
            // Bytes this process wrote since the previous tick: its delta when
            // it was seen then, everything when it started since, nothing when
            // it predates the baseline.
            let mut written_delta = 0u64;
            if let Reading::Value(now) = f.written {
                let id = (f.pid, f.start_us);
                self.written_now.insert(id, now);
                written_delta = match (self.written_before.get(&id), self.previous_us) {
                    (Some(before), _) => now.saturating_sub(*before),
                    (None, Some(prev)) if f.start_us > prev => now,
                    _ => 0,
                };
            }
            // The same for CPU time, with one difference: a reading that could
            // not be taken makes the sum of the tree it is in unreadable
            // instead of counting as zero.
            let (mut cpu_delta, mut cpu_unread) = (0u64, false);
            match f.cpu_ns {
                Reading::Value(now) => {
                    let id = (f.pid, f.start_us);
                    self.cpu_now.insert(id, now);
                    cpu_delta = match (self.cpu_before.get(&id), self.previous_us) {
                        (Some(before), _) => now.saturating_sub(*before),
                        (None, Some(prev)) if f.start_us > prev => now,
                        _ => 0,
                    };
                }
                Reading::Unavailable(_) => cpu_unread = true,
            }
            match self.tree_of[i] {
                Some(t) => {
                    let sum = &mut trees[t as usize];
                    sum.add(f.footprint);
                    sum.written = sum.written.saturating_add(written_delta);
                    sum.cpu = sum.cpu.saturating_add(cpu_delta);
                    sum.cpu_unread |= cpu_unread;
                    launched.add(f.footprint);
                }
                None if self.session_of[i].is_some() => observed.add(f.footprint),
                None => {}
            }
            if let Some(s) = self.session_of[i] {
                let sum = &mut per_session[s as usize];
                sum.add(f.footprint);
                sum.written = sum.written.saturating_add(written_delta);
                sum.cpu = sum.cpu.saturating_add(cpu_delta);
                sum.cpu_unread |= cpu_unread;
            }
            if f.ident.memguard
                && memguard.is_none_or(|m| {
                    let current = &self.facts[m];
                    (current.uid != me_uid && f.uid == me_uid) || f.start_us < current.start_us
                })
            {
                memguard = Some(i);
            }
            let foreign_candidate = f.pid != ctx.me_pid && self.tree_of[i].is_none();
            if foreign_candidate {
                if let Reading::Value(bytes) = f.footprint {
                    let heavy_enough =
                        foreign.len() < FOREIGN_TOP || foreign.iter().any(|p| p.footprint < bytes);
                    if heavy_enough && !self.is_mine(i, ctx.me_pid) {
                        let f = &self.facts[i];
                        foreign.push(ForeignProcess {
                            pid: f.pid,
                            name: f.ident.name.clone(),
                            footprint: bytes,
                            session: self.session_of[i]
                                .filter(|s| sessions_at[*s as usize] == i)
                                .and(f.ident.kind),
                        });
                        foreign.sort_by_key(|p| std::cmp::Reverse(p.footprint));
                        foreign.truncate(FOREIGN_TOP);
                    }
                }
            }
            let f = &self.facts[i];
            if f.ppid == 1
                && f.uid == me_uid
                && f.pid != ctx.me_pid
                && f.ident.kind.is_none()
                && !f.ident.memguard
                && self.tree_of[i].is_none()
                && self.session_of[i].is_none()
            {
                orphan_candidates.push(i);
            }
        }

        let rate = |written: u64| match dt_us {
            Some(dt) => Reading::Value(
                u64::try_from(u128::from(written) * 1_000_000 / u128::from(dt)).unwrap_or(u64::MAX),
            ),
            None => Reading::Unavailable(Why::NotYet),
        };
        // CPU nanoseconds per second: the delta over the tick's microseconds.
        let cpu_rate = |sum: &Sum| match dt_us {
            _ if sum.cpu_unread => Reading::Unavailable(Why::Unreadable),
            Some(dt) => Reading::Value(
                u64::try_from(u128::from(sum.cpu) * 1_000_000 / u128::from(dt)).unwrap_or(u64::MAX),
            ),
            None => Reading::Unavailable(Why::NotYet),
        };
        let mut launched_written = 0u64;
        let mut max_tree: Reading<u64> = if dt_us.is_some() {
            Reading::Value(0)
        } else {
            Reading::Unavailable(Why::NotYet)
        };
        let mut tree_rows = Vec::with_capacity(trees.len());
        for (t, sum) in trees.iter().enumerate() {
            launched_written = launched_written.saturating_add(sum.written);
            let write_rate = rate(sum.written);
            if let (Reading::Value(r), Reading::Value(m)) = (write_rate, max_tree) {
                max_tree = Reading::Value(r.max(m));
            }
            let name = &self.tree_names[t];
            let (run_id, session_id) = match name.split_once(':') {
                Some(("run", id)) => (Some(id.to_string()), None),
                Some(("session", id)) => (None, Some(id.to_string())),
                _ => (None, None),
            };
            tree_rows.push(LaunchedTree {
                tree: name.clone(),
                run_id,
                session_id,
                members: sum.readable + sum.unreadable,
                unreadable_members: sum.unreadable,
                footprint: sum.reading(),
                write_rate,
                cpu_rate: cpu_rate(sum),
            });
        }
        tree_rows.sort_by(|a, b| a.tree.cmp(&b.tree));

        let (mut claude, mut codex, mut other) = (
            SessionCount::default(),
            SessionCount::default(),
            SessionCount::default(),
        );
        let mut sessions = Vec::with_capacity(sessions_at.len().min(MAX_SESSIONS));
        let session_pids: std::collections::HashSet<i32> =
            sessions_at.iter().map(|&i| self.facts[i].pid).collect();
        for (n, &i) in sessions_at.iter().enumerate() {
            let f = &self.facts[i];
            let kind = f.ident.kind.unwrap_or(AgentKind::Claude);
            let tree = self.tree_of[i].map(|t| self.tree_names[t as usize].clone());
            let count: &mut SessionCount = match kind {
                AgentKind::Claude => &mut claude,
                AgentKind::Codex => &mut codex,
                AgentKind::Antigravity | AgentKind::DevCouncil | AgentKind::Manvi => &mut other,
            };
            if tree.is_some() {
                count.launched += 1;
            } else {
                count.observed += 1;
            }
            if sessions.len() >= MAX_SESSIONS {
                continue;
            }
            let sum = per_session[n];
            sessions.push(AgentSession {
                kind,
                pid: f.pid,
                start_us: f.start_us,
                cwd: self
                    .source
                    .cwd(f.pid)
                    .map(|p| p.to_string_lossy().into_owned()),
                run_id: f.ident.launch.as_ref().and_then(|k| k.run_id.clone()),
                tree,
                parent_pid: nearest_agent_ancestor(
                    f.pid,
                    f.ppid,
                    f.start_us,
                    |pid| {
                        self.index.get(&pid).map(|&at| ProcLink {
                            ppid: self.facts[at].ppid,
                            start_us: self.facts[at].start_us,
                        })
                    },
                    |pid| session_pids.contains(&pid),
                ),
                cpu_rate: cpu_rate(&sum),
                footprint: sum.reading(),
                write_rate: rate(sum.written),
                members: sum.readable + sum.unreadable,
                unreadable_members: sum.unreadable,
                ide_hosted: f.ident.ide_hosted,
                terminal_session_id: f.ident.launch.as_ref().and_then(|k| k.session_id.clone()),
            });
        }
        sessions.sort_by_key(|s| s.pid);

        let mut orphans = Vec::new();
        for i in orphan_candidates {
            if orphans.len() >= MAX_ORPHANS {
                break;
            }
            let source = &*self.source;
            if let Some(orphan) = orphan_of(&self.facts[i], ctx.now_us, &|pid| source.cwd(pid)) {
                orphans.push(orphan);
            }
        }

        ProcView {
            ok: true,
            error: None,
            processes: u32::try_from(self.facts.len()).unwrap_or(u32::MAX),
            other_users,
            unreadable,
            claude,
            codex,
            other,
            sessions_truncated: sessions_at.len() > MAX_SESSIONS,
            sessions,
            trees: tree_rows,
            launched_footprint: launched.reading(),
            observed_footprint: observed.reading(),
            largest_foreign: foreign
                .first()
                .map_or(Reading::Value(0), |p| Reading::Value(p.footprint)),
            foreign_top: foreign,
            launched_write_rate: rate(launched_written),
            max_tree_write_rate: max_tree,
            memguard: match memguard {
                Some(i) => memguard_of(&self.facts[i], ctx, &*self.source),
                None => MemguardHealth::absent(ctx.governed),
            },
            orphans,
        }
    }
}

/// Whether `f` is a foreign orphan worth showing: started by nobody still
/// alive (its parent is launchd), working in a git checkout, and spinning —
/// at least [`ORPHAN_MIN_CPU_RATIO`] of its life on a CPU, for longer than
/// [`ORPHAN_MIN_AGE_US`]. The 19-hour `/usr/bin/grep` at 21 GiB was this.
fn orphan_of(f: &ProcFacts, now_us: u64, cwd: &dyn Fn(i32) -> Option<PathBuf>) -> Option<Orphan> {
    let age_us = now_us.checked_sub(f.start_us)?;
    if age_us < ORPHAN_MIN_AGE_US {
        return None;
    }
    let cpu_ratio = f.cpu_ns.value()? as f64 / (age_us as f64 * 1000.0);
    if cpu_ratio < ORPHAN_MIN_CPU_RATIO {
        return None;
    }
    let cwd = cwd(f.pid)?;
    let checkout = git_checkout_of(&cwd)?;
    Some(Orphan {
        pid: f.pid,
        start_us: f.start_us,
        name: f.ident.name.clone(),
        exec: f.ident.exec.clone(),
        cwd: cwd.to_string_lossy().into_owned(),
        checkout: checkout.to_string_lossy().into_owned(),
        age_s: age_us / 1_000_000,
        cpu_ratio,
        footprint: f.footprint,
    })
}

/// The git checkout `dir` is in, by the nearest `.git` above it.
fn git_checkout_of(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .take(MAX_DEPTH)
        .find(|a| a.join(".git").exists())
        .map(Path::to_path_buf)
}

/// Parses a Go duration flag value such as `1h`, `90s`, `2m30s`.
fn go_duration_s(text: &str) -> Option<u64> {
    let mut total = 0u64;
    let mut digits = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let n: u64 = digits.parse().ok()?;
        digits.clear();
        total += n * match c {
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => return None,
        };
    }
    digits.is_empty().then_some(total).filter(|t| *t > 0)
}

fn memguard_of(f: &ProcFacts, ctx: &ScanContext, source: &dyn ProcSource) -> MemguardHealth {
    let flags: Vec<String> = f.ident.argv.iter().skip(1).cloned().collect();
    let heartbeat_interval_s = flags
        .iter()
        .position(|a| a == "-heartbeat" || a == "--heartbeat")
        .and_then(|at| flags.get(at + 1))
        .and_then(|v| go_duration_s(v))
        .or_else(|| {
            flags.iter().find_map(|a| {
                a.strip_prefix("-heartbeat=")
                    .or_else(|| a.strip_prefix("--heartbeat="))
                    .and_then(go_duration_s)
            })
        });
    let log_path = match source.fd_path(f.pid, 1) {
        Reading::Value(p) => Reading::Value(p),
        Reading::Unavailable(_) => source.fd_path(f.pid, 2),
    };
    let heartbeat_age_s = match &log_path {
        Reading::Value(path) => match std::fs::metadata(path).and_then(|m| m.modified()) {
            Ok(modified) => {
                let modified_us = modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
                    .unwrap_or(0);
                Reading::Value(ctx.now_us.saturating_sub(modified_us) / 1_000_000)
            }
            Err(e) => Reading::Unavailable(Why::of_errno(e.raw_os_error())),
        },
        Reading::Unavailable(why) => Reading::Unavailable(*why),
    };
    let mut warnings = Vec::new();
    if ctx.governed {
        let logs = ctx.home.as_ref().map(|h| h.join("Library").join("Logs"));
        match (&log_path, &logs) {
            (Reading::Value(path), Some(logs)) if !Path::new(path).starts_with(logs) => warnings
                .push(format!(
                    "memguard logs to {path}, outside ~/Library/Logs: its record is easy to lose"
                )),
            (Reading::Unavailable(_), _) => {
                warnings.push("memguard's log path could not be read".into())
            }
            _ => {}
        }
        if let (Reading::Value(age), Some(interval)) = (heartbeat_age_s, heartbeat_interval_s) {
            if age > interval.saturating_mul(2) {
                warnings.push(format!(
                    "memguard has not logged for {age}s against a {interval}s heartbeat"
                ));
            }
        }
    }
    MemguardHealth {
        present: true,
        pid: Some(f.pid),
        start_us: Some(f.start_us),
        exec: f.ident.exec.clone(),
        flags,
        log_path,
        heartbeat_age_s,
        heartbeat_interval_s,
        warn: !warnings.is_empty(),
        warnings,
    }
}

/// Each process's identity, read once per (pid, start time) and reused while
/// that same process lives.
type IdentityCache = HashMap<i32, (u64, Arc<Identity>)>;

/// The cached identity of `pid` when the cache holds this very process, or a
/// fresh one from `read`. A pid now held by a process that started at a
/// different time is a reuse, and inherits nothing — a recycled pid must never
/// keep a dead Claude's "session" label or a dead run's launch id. A start
/// time of 0 (another user's process, whose start the OS would not give) can
/// tell no two processes apart, so it is never reused.
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
fn cached_identity(
    cache: &mut IdentityCache,
    pid: i32,
    start_us: u64,
    read: impl FnOnce() -> Identity,
) -> Arc<Identity> {
    if let Some((start, ident)) = cache.get(&pid) {
        if *start == start_us && start_us != 0 {
            return ident.clone();
        }
    }
    let ident = Arc::new(read());
    cache.insert(pid, (start_us, ident.clone()));
    ident
}

/// One process's live facts, for a decision that must re-check them
/// (the orphan Terminate).
pub(crate) fn probe_one(pid: i32) -> Option<ProcFacts> {
    os::probe_one(pid)
}

/// Sends SIGTERM to a foreign orphan Health showed, after a person clicked
/// Terminate on it.
///
/// Everything that made it an orphan is checked again now, on the live
/// process, because the row the person clicked is up to a tick old: the pid
/// must still be the same process (pid and start time), this user's, parented
/// by launchd, no agent session, in no GitPulse launch (those are the
/// supervisor's, card 4), not memguard, and still spinning in a checkout.
/// Anything else refuses and signals nothing. SIGTERM, never SIGKILL: the
/// process may still clean up.
///
/// Security: this signals only a process the current user already owns and
/// could `kill` from a shell. The Terminate button is DOM, so any script that
/// reaches the IPC bridge could call the command behind it; `confirm` is the
/// native dialog ([`crate::desktop::confirm::native`]) that page script cannot
/// answer, so nothing is signalled without the person's own Yes. The process
/// is re-checked after the dialog closes, since it can take any amount of
/// time to answer.
pub fn terminate_orphan(
    pid: i32,
    start_us: u64,
    confirm: &dyn Fn(&str, &str) -> bool,
) -> Result<String, String> {
    terminate_confirmed(
        || still_orphaned(pid, start_us),
        confirm,
        || signal_term(pid, start_us),
    )
}

/// The decision behind [`terminate_orphan`], separated from the OS so it is
/// tested with closures: check, ask, check again, then signal — and on any
/// refusal or decline, signal nothing.
fn terminate_confirmed(
    check: impl Fn() -> Result<Orphan, String>,
    confirm: &dyn Fn(&str, &str) -> bool,
    signal: impl FnOnce() -> Result<(), String>,
) -> Result<String, String> {
    let seen = check()?;
    let body = format!(
        "Send SIGTERM to {} (pid {})?\n\nIt has run {} min with no parent in {}.",
        seen.name,
        seen.pid,
        seen.age_s / 60,
        seen.checkout
    );
    if !confirm("Terminate orphaned process", &body) {
        return Err("termination was not confirmed; nothing was signalled".into());
    }
    let orphan = check()?;
    signal()?;
    Ok(format!(
        "Sent SIGTERM to {} ({}), which had run {} min in {}",
        orphan.name,
        orphan.pid,
        orphan.age_s / 60,
        orphan.checkout
    ))
}

/// Everything that made `pid` an orphan, checked again on the live process.
fn still_orphaned(pid: i32, start_us: u64) -> Result<Orphan, String> {
    if pid <= 1 || start_us == 0 || i32::try_from(std::process::id()).ok() == Some(pid) {
        return Err("that process is not one GitPulse will terminate".into());
    }
    let f = probe_one(pid).ok_or("the process is no longer running; nothing was signalled")?;
    if f.start_us != start_us {
        return Err("that pid now belongs to a different process; nothing was signalled".into());
    }
    #[cfg(unix)]
    // SAFETY: getuid has no preconditions and cannot fail.
    let me = unsafe { libc::getuid() };
    #[cfg(not(unix))]
    let me = u32::MAX;
    if f.uid != me {
        return Err("the process belongs to another user; nothing was signalled".into());
    }
    if f.ident.memguard {
        return Err("GitPulse never terminates memguard".into());
    }
    if f.ident.launch.is_some() {
        return Err("the process belongs to a GitPulse launch; the supervisor owns it".into());
    }
    if f.ident.kind.is_some() {
        return Err("the process is an agent session; nothing was signalled".into());
    }
    if f.ppid != 1 {
        return Err("the process has a parent again; it is no longer orphaned".into());
    }
    orphan_of(&f, now_us(), &|pid| os::cwd(pid))
        .ok_or_else(|| "the process no longer looks orphaned; nothing was signalled".into())
}

/// The SIGTERM itself, through the one guarded primitive: it refuses an id that
/// is not an ordinary pid and re-reads the process's start time at the moment of
/// the signal, so a pid recycled since [`still_orphaned`] ran is not signalled.
#[cfg(unix)]
fn signal_term(pid: i32, start_us: u64) -> Result<(), String> {
    crate::procguard::signal::signal_process(
        pid,
        start_us,
        &|pid| probe_one(pid).map(|facts| facts.start_us),
        libc::SIGTERM,
        "Health orphan terminate",
    )
    .map_err(|error| format!("could not signal {pid}: {error}"))
}

#[cfg(not(unix))]
fn signal_term(_pid: i32, _start_us: u64) -> Result<(), String> {
    Err("terminating a process is not supported on this platform".into())
}

/// One live process inside an agent session tree eligible for background
/// scheduler/I/O throttling while the host is at critical pressure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentThrottleCandidate {
    pub pid: i32,
    pub start_us: u64,
    pub name: String,
    pub session_pid: i32,
    pub session_kind: AgentKind,
    pub session_cwd: Option<String>,
    pub is_session_root: bool,
    /// Working directories of this process's ancestors, nearest first, up to
    /// and including the session root (`/` and unreadable ones left out). A
    /// build script or a dependency's `rustc` runs in the package's own
    /// directory under the Cargo registry, outside every repository; the
    /// `cargo` that started it is where the work belongs.
    pub lineage_cwds: Vec<String>,
    /// Process ids of this process's ancestors, nearest first, up to and
    /// including the session root (at most `MAX_ANCESTOR_PIDS`). The steward
    /// uses them to tell a child that inherited background priority from a
    /// process it throttled apart from one someone else backgrounded.
    pub ancestor_pids: Vec<i32>,
}

/// Most ancestor working directories one candidate carries.
const MAX_LINEAGE_CWDS: usize = 8;

/// Most ancestor process ids one candidate carries.
const MAX_ANCESTOR_PIDS: usize = 8;

/// Returns live current-user worker processes belonging to active agent session
/// trees (`Claude`, `Codex`, `Antigravity`, `DevCouncil`), excluding GitPulse
/// itself, `memguard`, `Manvi` workers, helper binaries (`devmap`, `ptyd`,
/// `gitpulse-*`), and session root processes (interactive CLI TUIs and IDE
/// hosts, whose responsiveness and `fork()` priority inheritance must stay clean).
pub(crate) fn agent_throttle_candidates() -> Result<Vec<AgentThrottleCandidate>, String> {
    if !os::SUPPORTED {
        return Err("process scanning is not supported on this platform".into());
    }
    let mut scanner = Scanner::os();
    scanner.collect_throttle_candidates(i32::try_from(std::process::id()).unwrap_or(-1))
}

impl Scanner {
    pub(crate) fn collect_throttle_candidates(
        &mut self,
        me_pid: i32,
    ) -> Result<Vec<AgentThrottleCandidate>, String> {
        self.source.read(&mut self.facts)?;
        let me_uid = self.source.me_uid();
        self.index.clear();
        for (i, f) in self.facts.iter().enumerate() {
            self.index.insert(f.pid, i);
        }
        let sessions_at = self.assign_sessions(me_uid);
        let mut session_cwds: Vec<Option<String>> = Vec::with_capacity(sessions_at.len());
        for &si in &sessions_at {
            let pid = self.facts[si].pid;
            session_cwds.push(
                self.source
                    .cwd(pid)
                    .map(|p| p.to_string_lossy().into_owned()),
            );
        }
        let mut out = Vec::new();
        let mut ancestor_cwds: HashMap<i32, Option<String>> = HashMap::new();
        for (i, f) in self.facts.iter().enumerate() {
            let Some(s_idx) = self.session_of[i].map(|n| n as usize) else {
                continue;
            };
            let Some(&root_i) = sessions_at.get(s_idx) else {
                continue;
            };
            let root = &self.facts[root_i];
            let Some(session_kind) = root.ident.kind else {
                continue;
            };
            // Never throttle GitPulse's own Manvi worker trees.
            if session_kind == AgentKind::Manvi {
                continue;
            }
            if f.pid <= 1 || f.start_us == 0 || f.pid == me_pid || f.uid != me_uid {
                continue;
            }
            if f.ident.memguard {
                continue;
            }
            // Never throttle the session root itself (interactive CLI TUI or IDE
            // host): keeping the root at normal priority preserves Ctrl+C / hook
            // responsiveness and prevents XNU fork() inheritance of PRIO_DARWIN_BG
            // onto newly spawned long-lived child helpers.
            if f.pid == root.pid {
                continue;
            }
            // An IDE-hosted root is the whole editor: beside its agent server
            // it parents the integrated-terminal shells and everything the
            // person types there. Only the agent server's own descendants are
            // the agent's work.
            if root.ident.ide_hosted
                && !is_ide_agent_server(root)
                && !self.descends_from_agent_server(i, root_i)
            {
                continue;
            }
            let lower_name = f.ident.name.to_ascii_lowercase();
            if matches!(
                lower_name.as_str(),
                "gitpulse"
                    | "gitpulsed"
                    | "gitpulse-mcp"
                    | "gitpulse-hook"
                    | "manvi"
                    | "ptyd"
                    | "devmap"
            ) {
                continue;
            }
            let own_cwd = self
                .source
                .cwd(f.pid)
                .filter(|p| p != std::path::Path::new("/"))
                .map(|p| p.to_string_lossy().into_owned());
            let effective_cwd = own_cwd.or_else(|| session_cwds.get(s_idx).cloned().flatten());
            let lineage_cwds = self.lineage_cwds(i, root_i, &mut ancestor_cwds);
            let ancestor_pids = self.ancestor_pids(i, root_i);
            out.push(AgentThrottleCandidate {
                pid: f.pid,
                start_us: f.start_us,
                name: f.ident.name.clone(),
                session_pid: root.pid,
                session_kind,
                session_cwd: effective_cwd,
                is_session_root: false,
                lineage_cwds,
                ancestor_pids,
            });
        }
        Ok(out)
    }

    /// Working directories of `i`'s ancestors, nearest first, through the
    /// session root `root_i`, each read once per scan through `cache`.
    fn lineage_cwds(
        &self,
        i: usize,
        root_i: usize,
        cache: &mut HashMap<i32, Option<String>>,
    ) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut at = self.parent_of(i);
        for _ in 0..MAX_DEPTH {
            let Some(p) = at else { break };
            let pid = self.facts[p].pid;
            let cwd = cache
                .entry(pid)
                .or_insert_with(|| {
                    self.source
                        .cwd(pid)
                        .filter(|c| c != Path::new("/"))
                        .map(|c| c.to_string_lossy().into_owned())
                })
                .clone();
            if let Some(cwd) = cwd {
                if out.last() != Some(&cwd) {
                    out.push(cwd);
                    if out.len() >= MAX_LINEAGE_CWDS {
                        break;
                    }
                }
            }
            if p == root_i {
                break;
            }
            at = self.parent_of(p);
        }
        out
    }

    /// Process ids of `i`'s ancestors, nearest first, through the session
    /// root `root_i`, bounded by `MAX_ANCESTOR_PIDS`.
    fn ancestor_pids(&self, i: usize, root_i: usize) -> Vec<i32> {
        let mut out = Vec::new();
        let mut at = self.parent_of(i);
        for _ in 0..MAX_DEPTH.min(MAX_ANCESTOR_PIDS) {
            let Some(p) = at else { break };
            out.push(self.facts[p].pid);
            if p == root_i {
                break;
            }
            at = self.parent_of(p);
        }
        out
    }

    /// Whether an IDE's agent server lies strictly between process `i` and
    /// its session root `root_i`.
    fn descends_from_agent_server(&self, i: usize, root_i: usize) -> bool {
        let mut at = self.parent_of(i);
        for _ in 0..MAX_DEPTH {
            let Some(p) = at else { return false };
            if p == root_i {
                return false;
            }
            if is_ide_agent_server(&self.facts[p]) {
                return true;
            }
            at = self.parent_of(p);
        }
        false
    }
}

/// Antigravity's `language_server`: the process inside the IDE that runs the
/// agent, as opposed to the editor windows, helpers and terminal host.
fn is_ide_agent_server(f: &ProcFacts) -> bool {
    f.ident.kind == Some(AgentKind::Antigravity) && f.ident.name == "language_server"
}

#[cfg(target_os = "macos")]
const PRIO_DARWIN_PROCESS_WHICH: libc::c_int = 4;
#[cfg(target_os = "macos")]
const PRIO_DARWIN_BG_FLAG: libc::c_int = 0x1000;
/// Darwin XNU `MAXPRI_THROTTLE`: task priority assigned when a process is
/// placed in `PRIO_DARWIN_BG` (`BASEPRI_DEFAULT` is `31`).
#[cfg(target_os = "macos")]
const DARWIN_MAXPRI_THROTTLE: i32 = 4;

/// Reads XNU `proc_taskinfo::pti_priority` for `pid` on macOS, since
/// `getpriority(PRIO_DARWIN_PROCESS, pid)` only queries the calling task (`who == 0`)
/// whereas `setpriority(PRIO_DARWIN_PROCESS, pid, ...)` updates any same-UID `pid`.
#[cfg(target_os = "macos")]
pub(crate) fn darwin_task_priority(pid: i32) -> Result<i32, String> {
    let size = std::mem::size_of::<libc::proc_taskinfo>();
    let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
    // SAFETY: `info` matches `proc_taskinfo` layout and is read only after the
    // kernel reports writing all `size` bytes.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr().cast(),
            size as i32,
        )
    };
    if usize::try_from(written).ok() != Some(size) {
        return Err(format!(
            "proc_pidinfo(PROC_PIDTASKINFO) failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: fully initialized by `proc_pidinfo`.
    Ok(unsafe { info.assume_init() }.pti_priority)
}

/// The nice value a slowed Linux process is given.
#[cfg(any(target_os = "linux", test))]
const LINUX_SLOW_NICE: i32 = 10;

/// Whether a Linux process at nice `current` may be slowed. Raising nice is
/// always allowed, but lowering it again needs the target's `RLIMIT_NICE` to
/// cover the old value (`20 - nice <= limit`) or a privileged caller, and the
/// usual limit is 0. A process that could not be put back is not slowed.
#[cfg(any(target_os = "linux", test))]
fn linux_may_slow(current: i32, rlimit_nice: u64, privileged: bool) -> Result<bool, String> {
    if current >= LINUX_SLOW_NICE {
        return Ok(false);
    }
    let needed = u64::try_from(20 - current).unwrap_or(u64::MAX);
    if privileged || needed <= rlimit_nice {
        return Ok(true);
    }
    Err(format!(
        "Linux would not let GitPulse put its priority back (its RLIMIT_NICE is {rlimit_nice}, nice {current} needs {needed}), so it was left at normal priority"
    ))
}

/// The nice value to restore a slowed Linux process to: the one it had before,
/// when this process slowed it. After a restart that record is gone, and 0, the
/// default, is used. A process whose nice someone else changed since is left.
#[cfg(any(target_os = "linux", test))]
fn linux_restore_nice(recorded: Option<i32>, current: i32) -> Option<i32> {
    (current == LINUX_SLOW_NICE).then(|| recorded.unwrap_or(0))
}

/// Nice values of the Linux processes this process slowed, by `(pid,
/// start_us)`, so each is restored to what it had. Bounded: past the cap no
/// further process is slowed rather than one that cannot be put back exactly.
#[cfg(target_os = "linux")]
static LINUX_SLOWED_FROM: std::sync::Mutex<std::collections::BTreeMap<(i32, u64), i32>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());
#[cfg(target_os = "linux")]
const MAX_LINUX_SLOWED: usize = 4096;

/// Places (`enable = true`) or restores (`enable = false`) a live current-user
/// process in OS background scheduler / I/O throttle after re-verifying both
/// `pid` and `start_us` against the live process table.
///
/// Returns `Ok(true)` when the process's scheduling state was changed by this
/// call, `Ok(false)` when it was already in the target state, or `Err` when
/// refused or unsupported.
pub(crate) fn set_background_throttle(
    pid: i32,
    start_us: u64,
    enable: bool,
) -> Result<bool, String> {
    if pid <= 1 || start_us == 0 || i32::try_from(std::process::id()).ok() == Some(pid) {
        return Err("refused to change priority of reserved or current process".into());
    }
    let Some(facts) = probe_one(pid) else {
        return Err("process is no longer running".into());
    };
    if facts.start_us != start_us {
        return Err("pid was recycled by a newer process".into());
    }
    #[cfg(unix)]
    // SAFETY: getuid has no preconditions and cannot fail.
    let me = unsafe { libc::getuid() };
    #[cfg(not(unix))]
    let me = u32::MAX;
    if facts.uid != me {
        return Err("process belongs to another user".into());
    }
    if facts.ident.memguard {
        return Err("GitPulse never modifies memguard scheduling".into());
    }

    #[cfg(target_os = "macos")]
    {
        let pri = darwin_task_priority(pid)?;
        let already_throttled = pri <= DARWIN_MAXPRI_THROTTLE;
        let target = pid as libc::id_t;
        // SAFETY: `setpriority` takes plain integer identifiers with no pointers.
        unsafe {
            if enable {
                if already_throttled {
                    return Ok(false);
                }
                if libc::setpriority(PRIO_DARWIN_PROCESS_WHICH, target, PRIO_DARWIN_BG_FLAG) != 0 {
                    return Err(format!(
                        "setpriority(PRIO_DARWIN_BG) failed: {}",
                        std::io::Error::last_os_error()
                    ));
                }
                Ok(true)
            } else {
                if !already_throttled {
                    return Ok(false);
                }
                if libc::setpriority(PRIO_DARWIN_PROCESS_WHICH, target, 0) != 0 {
                    return Err(format!(
                        "setpriority(restore) failed: {}",
                        std::io::Error::last_os_error()
                    ));
                }
                Ok(true)
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        let target = pid as libc::id_t;
        // SAFETY: `__errno_location` is thread-local; `getpriority` takes plain integers.
        let current = unsafe {
            *libc::__errno_location() = 0;
            let current = libc::getpriority(libc::PRIO_PROCESS, target);
            let err = *libc::__errno_location();
            if current == -1 && err != 0 {
                return Err(format!(
                    "getpriority failed: {}",
                    std::io::Error::from_raw_os_error(err)
                ));
            }
            current
        };
        let set_nice = |nice: i32| -> Result<(), String> {
            // SAFETY: `setpriority` takes plain integers.
            if unsafe { libc::setpriority(libc::PRIO_PROCESS, target, nice) } != 0 {
                return Err(format!(
                    "setpriority(nice {nice}) failed: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(())
        };
        if enable {
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: reads `pid`'s limit into a local; a null new limit sets nothing.
            if unsafe { libc::prlimit(pid, libc::RLIMIT_NICE, std::ptr::null(), &mut limit) } != 0 {
                return Err(format!(
                    "could not read its RLIMIT_NICE: {}",
                    std::io::Error::last_os_error()
                ));
            }
            // SAFETY: `geteuid` has no preconditions.
            let privileged = unsafe { libc::geteuid() } == 0;
            if !linux_may_slow(current, limit.rlim_cur, privileged)? {
                return Ok(false);
            }
            let mut slowed = crate::supervisor::lock_or_recover(&LINUX_SLOWED_FROM);
            if slowed.len() >= MAX_LINUX_SLOWED {
                // Processes that exited while slowed need no restore.
                slowed.retain(|&(p, s), _| probe_one(p).is_some_and(|f| f.start_us == s));
            }
            if slowed.len() >= MAX_LINUX_SLOWED && !slowed.contains_key(&(pid, start_us)) {
                return Err(
                    "GitPulse already holds as many slowed processes as it can put back".into(),
                );
            }
            set_nice(LINUX_SLOW_NICE)?;
            slowed.insert((pid, start_us), current);
            Ok(true)
        } else {
            let mut slowed = crate::supervisor::lock_or_recover(&LINUX_SLOWED_FROM);
            let recorded = slowed.get(&(pid, start_us)).copied();
            let Some(to) = linux_restore_nice(recorded, current) else {
                slowed.remove(&(pid, start_us));
                return Ok(false);
            };
            set_nice(to)?;
            slowed.remove(&(pid, start_us));
            Ok(true)
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = enable;
        Err("process background throttling is not supported on this platform".into())
    }
}

/// Whether this platform has a process scan at all.
pub(crate) const SCAN_SUPPORTED: bool = os::SUPPORTED;

/// Identity of the agent session that spawned the current server process,
/// resolved by walking parents using both pid and start timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallerIdentity {
    pub pid: i32,
    pub start_us: u64,
}

/// One sessions-only process scan paired with the caller's own owning
/// session (when an agent session sits above this process in the table).
#[derive(Debug, Clone, Default)]
pub struct SessionScan {
    pub sessions: Vec<AgentSession>,
    pub caller: Option<CallerIdentity>,
}

/// Start time in microseconds since the Unix epoch of a live process,
/// or `None` when the process is gone or this platform cannot report it.
pub fn process_start_us(pid: u32) -> Option<u64> {
    if !os::SUPPORTED {
        return None;
    }
    let pid = i32::try_from(pid).ok().filter(|&p| p > 0)?;
    let (_, start_us) = os::pid_ppid_start(pid)?;
    (start_us > 0).then_some(start_us)
}

/// Turns classified rows into sessions. A row whose parent row is the same
/// kind is that parent's wrapper target, not a second session. A parent that
/// is not itself a candidate is not a wrapper. More than [`MAX_SESSIONS`] is
/// a scan that could not finish. Working directories are read only for the
/// sessions that remain.
fn assemble_scan(
    table: SessionTable,
    mut cwd_of: impl FnMut(i32) -> Option<PathBuf>,
    caller: Option<(i32, Option<(i32, u64)>)>,
) -> Result<SessionScan, String> {
    let SessionTable {
        rows,
        links,
        mut cwds,
    } = table;
    let by_pid: HashMap<i32, (AgentKind, bool)> = rows
        .iter()
        .map(|row| (row.pid, (row.kind, row.ide_hosted)))
        .collect();
    let kept: Vec<&SessionRow> = rows
        .iter()
        .filter(|row| by_pid.get(&row.ppid).copied() != Some((row.kind, row.ide_hosted)))
        .collect();
    if kept.len() > MAX_SESSIONS {
        return Err(format!(
            "more than {MAX_SESSIONS} agent sessions are running; the scan does not describe them all"
        ));
    }
    let start_by_pid: HashMap<i32, u64> = kept.iter().map(|row| (row.pid, row.start_us)).collect();
    let sessions = kept
        .into_iter()
        .map(|row| AgentSession {
            kind: row.kind,
            pid: row.pid,
            start_us: row.start_us,
            cwd: cwds
                .remove(&row.pid)
                .unwrap_or_else(|| cwd_of(row.pid))
                .map(|path| path.to_string_lossy().into_owned()),
            tree: row.launch.as_ref().and_then(LaunchKey::tree),
            run_id: row.launch.as_ref().and_then(|key| key.run_id.clone()),
            parent_pid: nearest_agent_ancestor(
                row.pid,
                row.ppid,
                row.start_us,
                |pid| links.get(&pid).copied(),
                |pid| start_by_pid.contains_key(&pid),
            ),
            // This path reads neither footprints nor CPU time. Unavailable is
            // the not-measured signal; zero would say the session uses no
            // memory and is not running.
            cpu_rate: Reading::Unavailable(Why::Unsupported),
            footprint: Reading::Unavailable(Why::Unsupported),
            write_rate: Reading::Unavailable(Why::Unsupported),
            members: 1,
            unreadable_members: 0,
            ide_hosted: row.ide_hosted,
            terminal_session_id: row.launch.as_ref().and_then(|key| key.session_id.clone()),
        })
        .collect();
    let resolved_caller = caller.and_then(|(caller_pid, fallback_link)| {
        let link = links
            .get(&caller_pid)
            .copied()
            .or_else(|| fallback_link.map(|(ppid, start_us)| ProcLink { ppid, start_us }))?;
        let agent_pid = nearest_agent_ancestor(
            caller_pid,
            link.ppid,
            link.start_us,
            |pid| links.get(&pid).copied(),
            |pid| start_by_pid.contains_key(&pid),
        )?;
        let start_us = start_by_pid.get(&agent_pid).copied()?;
        Some(CallerIdentity {
            pid: agent_pid,
            start_us,
        })
    });
    Ok(SessionScan {
        sessions,
        caller: resolved_caller,
    })
}

/// A failed table read stays an error. An empty `Ok` would mean no sessions.
pub(crate) fn sessions_from_read(
    read: Result<SessionTable, String>,
    cwd_of: impl FnMut(i32) -> Option<PathBuf>,
) -> Result<Vec<AgentSession>, String> {
    Ok(assemble_scan(read?, cwd_of, None)?.sessions)
}

#[cfg(test)]
pub(crate) fn scan_from_read(
    read: Result<SessionTable, String>,
    cwd_of: impl FnMut(i32) -> Option<PathBuf>,
    caller_pid: i32,
    caller_link: Option<(i32, u64)>,
) -> Result<SessionScan, String> {
    assemble_scan(read?, cwd_of, Some((caller_pid, caller_link)))
}

/// Every agent session running now, for callers outside the sampler (the
/// insights facet and `gitpulse-hook`).
///
/// A fresh process pays this once. It lists the table and reads arguments
/// only for processes that could be agents, and it does not read footprints
/// or disk writes. `Err` when the table could not be read or the session
/// list is longer than [`MAX_SESSIONS`] — never an empty `Ok` for a scan
/// that did not run.
pub fn observe_sessions() -> Result<Vec<AgentSession>, String> {
    if !os::SUPPORTED {
        return Err("this platform has no process scan".into());
    }
    sessions_from_read(os::read_session_rows(), os::cwd)
}

/// [`observe_sessions`] plus the nearest owning agent session above the
/// current process, verified by `(pid, start_us)`.
pub fn observe_sessions_with_caller() -> Result<SessionScan, String> {
    if !os::SUPPORTED {
        return Err("this platform has no process scan".into());
    }
    let table = os::read_session_rows()?;
    let self_pid = i32::try_from(std::process::id()).unwrap_or(-1);
    let fallback = if self_pid > 0 && !table.links.contains_key(&self_pid) {
        os::pid_ppid_start(self_pid)
    } else {
        None
    };
    assemble_scan(
        table,
        os::cwd,
        (self_pid > 0).then_some((self_pid, fallback)),
    )
}

/// The processes whose working directory is `dir` or inside it, other than
/// this one. `Err` when the table could not be read.
///
/// This is the cwd-only question. A process that builds into `dir` from
/// somewhere else (`cargo --target-dir dir`, a test executable run from
/// `dir`) is invisible to it, so a caller that decides to delete `dir` asks
/// [`processes_using`] instead; this one remains for callers that want to
/// find processes to signal by where they are standing.
pub(crate) fn working_in(dir: &Path) -> Result<Vec<i32>, String> {
    let me = i32::try_from(std::process::id()).unwrap_or(-1);
    let mut pids = Vec::new();
    os::list_pids(&mut pids)?;
    Ok(pids
        .into_iter()
        .filter(|pid| *pid > 0 && *pid != me)
        .filter(|pid| os::cwd(*pid).is_some_and(|cwd| cwd.starts_with(dir)))
        .collect())
}

/// How a process was found to be using a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) enum UseKind {
    /// Its current directory is the directory or inside it.
    WorkingDirectory,
    /// It is running an executable that lives inside the directory.
    Executable,
    /// An argument names the directory (`--target-dir dir`, `--manifest-path
    /// dir/Cargo.toml`, a bare path), as a path or inside a longer argument.
    Argument,
    /// A variable in its environment names the directory (`CARGO_TARGET_DIR`).
    Environment,
}

impl UseKind {
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Self::WorkingDirectory => "its working directory is inside",
            Self::Executable => "it runs an executable from inside",
            Self::Argument => "an argument names",
            Self::Environment => "its environment names",
        }
    }
}

/// What the OS would say about one process, as far as the user is allowed to
/// read it. `None` is "could not be read", never "empty".
#[derive(Debug, Clone, Default)]
pub(crate) struct ProcessUse {
    pub pid: i32,
    /// The process belongs to the user running this program. Another user's
    /// process cannot be inspected without privilege and is only counted.
    pub mine: bool,
    pub cwd: Option<PathBuf>,
    pub exe: Option<String>,
    pub argv: Option<Vec<String>>,
    pub env: Option<Vec<String>>,
}

impl ProcessUse {
    /// One of the user's own processes with a working directory, argument list
    /// or environment the OS would not show: it cannot be shown not to use a
    /// directory. Another user's process is never unproven, only counted.
    pub(crate) fn unproven(&self) -> bool {
        self.mine && (self.argv.is_none() || self.env.is_none() || self.cwd.is_none())
    }
}

/// Who is using a directory, and what could not be checked.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub(crate) struct DirUse {
    pub users: Vec<(i32, UseKind)>,
    /// The user's own processes whose arguments or environment could not be
    /// read, so they cannot be shown not to use the directory.
    pub unreadable: Vec<i32>,
    /// Other users' processes, which are not inspected.
    pub other_users: usize,
}

impl DirUse {
    pub(crate) fn pids(&self) -> Vec<i32> {
        let mut pids: Vec<i32> = self.users.iter().map(|(pid, _)| *pid).collect();
        pids.dedup();
        pids
    }

    /// The directory is shown unused only when nobody is using it and every
    /// process of the user's could be read.
    pub(crate) fn provably_unused(&self) -> bool {
        self.users.is_empty() && self.unreadable.is_empty()
    }

    /// One sentence for a candidate that is kept because of this reading.
    pub(crate) fn why_kept(&self) -> Option<String> {
        if let Some((pid, kind)) = self.users.first() {
            return Some(format!(
                "process {pid}: {} it ({} process(es) in all)",
                kind.describe(),
                self.pids().len()
            ));
        }
        (!self.unreadable.is_empty()).then(|| {
            format!(
                "{} of your processes could not be read, so none of them can be shown not to use it",
                self.unreadable.len()
            )
        })
    }
}

/// A `KERN_PROCARGS2` block: argc, the exec path, padding, `argc` arguments,
/// then the environment. `None` for a block too short to hold argc and a path.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn parse_procargs2(data: &[u8]) -> Option<(String, Vec<String>, Vec<String>)> {
    let argc = i32::from_ne_bytes(data.get(..4)?.try_into().ok()?);
    let argc = usize::try_from(argc).ok()?.min(4096);
    let rest = &data[4..];
    let exec_end = rest.iter().position(|b| *b == 0)?;
    let exec = String::from_utf8_lossy(&rest[..exec_end]).into_owned();
    let mut items = rest[exec_end..]
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned());
    let argv: Vec<String> = items.by_ref().take(argc).collect();
    Some((exec, argv, items.collect()))
}

/// Whether `pid` is still a process. `EPERM` means it is, and is not ours.
#[cfg(target_os = "macos")]
fn process_exists(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists and may be signalled.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// The spellings of `dir` a process may use: as given, resolved, and with the
/// macOS `/private` alias added or removed (`/tmp` is `/private/tmp`).
fn dir_spellings(dir: &Path) -> Vec<PathBuf> {
    let mut all = vec![dir.to_path_buf()];
    if let Ok(real) = dir.canonicalize() {
        all.push(real);
    }
    let extra: Vec<PathBuf> = all
        .iter()
        .filter_map(|path| {
            if let Ok(rest) = path.strip_prefix("/private") {
                Some(Path::new("/").join(rest))
            } else if path.starts_with("/tmp")
                || path.starts_with("/var")
                || path.starts_with("/etc")
            {
                Some(Path::new("/private").join(path.strip_prefix("/").ok()?))
            } else {
                None
            }
        })
        .collect();
    all.extend(extra);
    all.sort();
    all.dedup();
    all
}

fn path_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '@' | '+' | '~')
}

/// `text` contains `needle` as a whole path: not the tail of a longer path
/// (`/other/dir`) and not the head of a longer name (`dir-two`).
fn mentions(text: &str, needle: &str) -> bool {
    text.match_indices(needle).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + needle.len()..].chars().next();
        before.is_none_or(|c| !path_char(c) && c != '/')
            && after.is_none_or(|c| c == '/' || !path_char(c))
    })
}

/// `cwd` joined with `token`, with `.` and `..` resolved as text.
fn lexical_join(cwd: &Path, token: &str) -> PathBuf {
    let mut out = PathBuf::new();
    for part in cwd.join(token).components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// How `process` uses a directory spelled `spellings`, if it does.
fn classify_use(spellings: &[PathBuf], process: &ProcessUse) -> Option<UseKind> {
    let text: Vec<&str> = spellings.iter().filter_map(|p| p.to_str()).collect();
    let inside = |path: &Path| spellings.iter().any(|dir| path.starts_with(dir));
    let names = |value: &str| text.iter().any(|dir| mentions(value, dir));
    if process.cwd.as_deref().is_some_and(inside) {
        return Some(UseKind::WorkingDirectory);
    }
    if process
        .exe
        .as_deref()
        .is_some_and(|exe| inside(Path::new(exe)))
    {
        return Some(UseKind::Executable);
    }
    if let Some(argv) = &process.argv {
        for (index, arg) in argv.iter().enumerate() {
            if names(arg) {
                return Some(UseKind::Argument);
            }
            // The program's own name is never a path argument, and a relative
            // argument means nothing without the directory it is relative to.
            // `--target-dir=rel` carries its path after the `=`.
            let relative = if arg.starts_with("--") {
                arg.split_once('=').map(|(_, value)| value)
            } else if arg.starts_with('-') {
                None
            } else {
                Some(arg.as_str())
            };
            if let (true, Some(relative), Some(cwd)) = (index > 0, relative, &process.cwd) {
                if !relative.is_empty()
                    && !relative.starts_with('/')
                    && inside(&lexical_join(cwd, relative))
                {
                    return Some(UseKind::Argument);
                }
            }
        }
    }
    if let Some(env) = &process.env {
        for entry in env {
            let Some((name, value)) = entry.split_once('=') else {
                continue;
            };
            if names(value) {
                return Some(UseKind::Environment);
            }
            // A relative build directory means the process's own working
            // directory, which may be somewhere else entirely.
            if matches!(name, "CARGO_TARGET_DIR" | "CARGO_BUILD_TARGET_DIR")
                && !value.is_empty()
                && !value.starts_with('/')
            {
                if let Some(cwd) = &process.cwd {
                    if inside(&lexical_join(cwd, value)) {
                        return Some(UseKind::Environment);
                    }
                }
            }
        }
    }
    None
}

/// The id a Claude CLI was started with, from `--session-id <id>` or
/// `--session-id=<id>`. Only that flag: `--resume <id>` may continue under a new
/// id, so it would name a session that is not the one running. The value must be
/// shaped like an id (letters, digits, `-` and `_`, 8 to 96 long).
pub(crate) fn claude_session_id_in(argv: &[String]) -> Option<String> {
    // The flag's spelling belongs to the file that builds it.
    let flag = crate::workbench::terminal_command::CLAUDE_SESSION_ID_FLAG;
    let mut iter = argv.iter().skip(1);
    while let Some(arg) = iter.next() {
        let value = if arg == flag {
            iter.next()?.as_str()
        } else if let Some(value) = arg
            .strip_prefix(flag)
            .and_then(|rest| rest.strip_prefix('='))
        {
            value
        } else {
            continue;
        };
        let plain = (8..=96).contains(&value.len())
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        return plain.then(|| value.to_string());
    }
    None
}

/// Whether a process the scan files under the Claude executable is one of its
/// helper modes, not a session: the browser bridge (`--chrome-native-host`) and
/// the bundled ripgrep, which the binary runs as when `argv[0]` is `rg`.
pub(crate) fn is_claude_helper(argv: &[String]) -> bool {
    argv.first()
        .and_then(|a| Path::new(a).file_name())
        .is_some_and(|name| name == "rg")
        || argv.iter().skip(1).any(|a| a == "--chrome-native-host")
}

/// [`DirUse`] over an already-read process table, `me` excluded.
pub(crate) fn dir_use_in(dir: &Path, table: &[ProcessUse], me: i32) -> DirUse {
    let spellings = dir_spellings(dir);
    let mut out = DirUse::default();
    for process in table.iter().filter(|p| p.pid > 0 && p.pid != me) {
        if let Some(kind) = classify_use(&spellings, process) {
            out.users.push((process.pid, kind));
        } else if !process.mine {
            out.other_users += 1;
        } else if process.unproven() {
            // A working directory the OS would not show is as unproven as an
            // argument list it would not show, whatever the reason: no path is
            // not "inside nothing". (macOS keeps answering with the old path
            // for a directory removed under a process, so that case reads as a
            // user of that path; a failed lookup is what lands here.)
            out.unreadable.push(process.pid);
        }
    }
    out
}

/// How many times [`settle_unproven`] re-reads the rows still unproven.
const USE_SETTLE_ROUNDS: usize = 3;

/// The pause before each settle round. Worst case [`USE_SETTLE_ROUNDS`] x this
/// is 60 ms, and only when some row of the user's is unproven after the first
/// read; a table with no unproven row pays nothing.
const USE_SETTLE_PAUSE: std::time::Duration = std::time::Duration::from_millis(20);

/// Give a process caught mid-exit time to finish exiting. Each round, when any
/// row is unproven, `pause` runs once and every unproven pid is read again:
/// `None` (gone, or a zombie) removes the row, `Some` replaces it. After the
/// last round whatever is still unproven stays in the table, so a process that
/// is alive and really unreadable is still refused on, never dropped.
fn settle_unproven(
    mut rows: Vec<ProcessUse>,
    mut reread: impl FnMut(i32) -> Option<ProcessUse>,
    mut pause: impl FnMut(),
) -> Vec<ProcessUse> {
    for _ in 0..USE_SETTLE_ROUNDS {
        if !rows.iter().any(ProcessUse::unproven) {
            break;
        }
        pause();
        rows = rows
            .into_iter()
            .filter_map(|row| {
                if row.unproven() {
                    reread(row.pid)
                } else {
                    Some(row)
                }
            })
            .collect();
    }
    rows
}

/// Every process the OS lists, read for [`dir_use_in`]. A process that exits
/// while the table is read is dropped; a table that cannot be listed is an
/// `Err`. A row of the user's own still unreadable after the first pass is
/// read again, up to [`USE_SETTLE_ROUNDS`] times with [`USE_SETTLE_PAUSE`]
/// between, so a process in the middle of exiting is dropped rather than
/// reported as unreadable; one that stays unreadable is kept.
pub(crate) fn read_use_table() -> Result<Vec<ProcessUse>, String> {
    let mut pids = Vec::new();
    os::list_pids(&mut pids)?;
    let mut buf = Vec::new();
    let rows = pids
        .into_iter()
        .filter(|pid| *pid > 0)
        .filter_map(|pid| os::read_use(pid, &mut buf))
        .collect();
    Ok(settle_unproven(
        rows,
        |pid| os::read_use(pid, &mut buf),
        || std::thread::sleep(USE_SETTLE_PAUSE),
    ))
}

/// Who is using `dir` right now, read from the live process table.
pub(crate) fn dir_use(dir: &Path) -> Result<DirUse, String> {
    let me = i32::try_from(std::process::id()).unwrap_or(-1);
    Ok(dir_use_in(dir, &read_use_table()?, me))
}

/// The pids using `dir`, for a caller that is about to delete it or something
/// inside it: a working directory inside, an executable inside, an argument
/// or an environment value naming it. `Err` when the table could not be read,
/// and `Err` when nothing was found but some process of the user's could not
/// be read: unreadable is not unused.
pub(crate) fn processes_using(dir: &Path) -> Result<Vec<i32>, String> {
    let reading = dir_use(dir)?;
    if reading.users.is_empty() && !reading.unreadable.is_empty() {
        return Err(reading.why_kept().unwrap_or_default());
    }
    Ok(reading.pids())
}

/// Every pid in the process table, bounded by [`MAX_SCANNED_PROCESSES`].
/// `Err` when the table could not be read, or is larger than that bound.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn all_pids() -> Result<Vec<i32>, String> {
    let mut pids = Vec::new();
    os::list_pids(&mut pids)?;
    Ok(pids)
}

/// A process's executable path, when the OS will say. macOS only.
#[cfg(target_os = "macos")]
pub(crate) fn exec_path_of(pid: i32) -> Option<String> {
    os::pidpath(pid)
}

/// A process's phys footprint (bytes) and CPU time (ns). macOS only.
#[cfg(target_os = "macos")]
pub(crate) fn footprint_cpu_of(pid: i32) -> Option<(u64, u64)> {
    os::footprint_cpu(pid)
}

/// A process's working directory, when the OS will say.
pub(crate) fn cwd_of(pid: i32) -> Option<PathBuf> {
    os::cwd(pid)
}

/// One process as something about to signal it would see it: what `pkill`,
/// `pgrep` and `killall` match against, and whether GitPulse would call it
/// its own.
#[derive(Debug, Clone)]
pub struct TableRow {
    pub pid: i32,
    pub ppid: i32,
    /// The effective uid (what `pgrep -u` matches). Where the OS reports only
    /// one id, that one, and [`TableRow::ruid`] / [`TableRow::suid`] are `None`.
    pub uid: u32,
    /// The real uid, when the OS reported it (`Uid:` in `/proc/<pid>/status`).
    pub ruid: Option<u32>,
    /// The saved set-user-id, when the OS reported it.
    pub suid: Option<u32>,
    /// Microseconds since the Unix epoch; 0 when the OS would not say.
    pub start_us: u64,
    /// The process name the OS reports.
    pub name: String,
    /// The path it was exec'd with, when readable.
    pub exec: Option<String>,
    /// The executable the kernel runs it from (`/proc/<pid>/exe` on Linux),
    /// which `argv[0]` cannot change. `None` when the OS would not say, and on
    /// macOS, where [`TableRow::exec`] already is the kernel's.
    pub real_exe: Option<String>,
    /// The OS reports the executable the kernel runs (Linux) and this process's
    /// could not be read, or is the dynamic loader a program was run through
    /// (`ld.so PROGRAM`): what the process is cannot be told from its own
    /// spelling (`argv[0]`), which it chooses. Never set where the OS does not
    /// report it.
    pub exe_unknown: bool,
    /// Every argument, `argv[0]` first. `None` when the OS refused it: another
    /// user's process when the sender is not root, or one that exited while
    /// the table was being read. On Linux an empty argument between two others
    /// is kept (the tools show it as a double space) and a process that
    /// blanked its argument area has none.
    pub argv: Option<Vec<String>>,
    /// The agent CLI this is, by executable.
    pub agent: Option<AgentKind>,
    /// Inside a tree GitPulse launched: it, or an ancestor, carries a
    /// GitPulse launch id.
    pub hosted: bool,
}

impl TableRow {
    /// The real uid; the effective one when the OS did not report it.
    pub fn real_uid(&self) -> u32 {
        self.ruid.unwrap_or(self.uid)
    }

    /// The saved set-user-id; the effective uid when the OS did not report it.
    pub fn saved_uid(&self) -> u32 {
        self.suid.unwrap_or(self.uid)
    }
}

/// The three ids `kill(2)` and `pgrep` tell apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Creds {
    pub real: u32,
    pub effective: u32,
    pub saved: u32,
}

impl Creds {
    fn any_is(&self, uid: u32) -> bool {
        self.real == uid || self.effective == uid || self.saved == uid
    }
}

/// The `Uid:` line of `/proc/<pid>/status` (`real effective saved filesystem`),
/// which the kernel fills from the process's credentials. Unlike the owner of
/// `/proc/<pid>`, which the kernel makes root's for a process that is not
/// dumpable (`ssh-agent`, a set-uid program, `prctl(PR_SET_DUMPABLE, 0)`), it
/// is the user the process runs as. `None` when the line is absent or short.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_status_uids(status: &str) -> Option<Creds> {
    let line = status.lines().find_map(|l| l.strip_prefix("Uid:"))?;
    let mut ids = line.split_whitespace().map(|v| v.parse::<u32>().ok());
    Some(Creds {
        real: ids.next()??,
        effective: ids.next()??,
        saved: ids.next()??,
    })
}

/// Whether `path` is a dynamic loader (`ld-linux-aarch64.so.1`, `ld.so`,
/// `ld-2.31.so`): a program run as `ld.so PROGRAM` has the loader as its
/// executable and its own path only as an argument.
pub(crate) fn is_dynamic_loader(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.starts_with("ld-linux")
        || name.starts_with("ld.so")
        || (name.starts_with("ld-") && name.contains(".so"))
}

/// The arguments in a `/proc/<pid>/cmdline`: NUL-terminated, an empty one
/// between two others kept, trailing empty ones dropped. The procps tools join
/// them with a space, so `a`, ``, `b` is `a  b` to a `-f` pattern, and a
/// process that zeroed its argument area (all NULs) has none.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn split_cmdline(raw: &[u8]) -> Vec<String> {
    split_cmdline_within(raw, None)
}

/// [`split_cmdline`] with `limits` as (arguments, bytes per argument) when only
/// a bounded look is wanted: arguments past the count are not read, and one
/// over the byte bound is read as an empty one so the others keep their place.
pub(crate) fn split_cmdline_within(raw: &[u8], limits: Option<(usize, usize)>) -> Vec<String> {
    let mut items: Vec<&[u8]> = raw.split(|b| *b == 0).collect();
    while items.last().is_some_and(|s| s.is_empty()) {
        items.pop();
    }
    let (max_args, max_bytes) = limits.unwrap_or((usize::MAX, usize::MAX));
    items
        .into_iter()
        .take(max_args)
        .map(|s| {
            if s.len() > max_bytes {
                String::new()
            } else {
                String::from_utf8_lossy(s).into_owned()
            }
        })
        .collect()
}

/// The whole table, with the user it was read as.
#[derive(Debug, Clone)]
pub struct Table {
    pub rows: Vec<TableRow>,
    pub me_uid: u32,
}

/// Argument bytes the signal table reads from processes that are not the
/// sender's, which only a root sender does. Past it the rest stay unread, and
/// the guard asks about them: a bounded read that fails closed, not a sample
/// reported as the whole table.
const FOREIGN_ARGV_BUDGET: usize = 64 << 20;

impl Scanner {
    /// Reads the whole table once as [`TableRow`]s. The arguments are not part
    /// of a scan's identities (only memguard's are kept), so `argv_of` supplies
    /// them, for this user's processes only: the processes the sender can
    /// signal are the ones whose command line a `-f` pattern is tested
    /// against, which for a root sender is every process. Where the OS reports
    /// a process's real, effective and saved uids they replace the single uid
    /// a scan carries, which on Linux is the owner of `/proc/<pid>`.
    pub(crate) fn table(
        &mut self,
        argv_of: impl FnMut(i32) -> Option<Vec<String>>,
    ) -> Result<Table, String> {
        self.table_within(argv_of, FOREIGN_ARGV_BUDGET)
    }

    /// [`Scanner::table`] with the bytes of other processes' arguments it may
    /// read named.
    fn table_within(
        &mut self,
        mut argv_of: impl FnMut(i32) -> Option<Vec<String>>,
        foreign_budget: usize,
    ) -> Result<Table, String> {
        self.source.read(&mut self.facts)?;
        let me_uid = self.source.me_uid();
        let root = self.source.is_root();
        self.index.clear();
        for (i, f) in self.facts.iter().enumerate() {
            self.index.insert(f.pid, i);
        }
        self.assign_trees();
        let mut foreign_bytes = 0usize;
        let rows = self
            .facts
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let creds = self.source.creds(f.pid);
                let mine = f.uid == me_uid || creds.is_some_and(|c| c.any_is(me_uid));
                let readable = mine || (root && foreign_bytes < foreign_budget);
                let argv = if readable { argv_of(f.pid) } else { None };
                if !mine {
                    foreign_bytes += argv
                        .as_ref()
                        .map_or(0, |a| a.iter().map(|s| s.len() + 1).sum());
                }
                // A binary replaced since the process started reads
                // `/path/claude (deleted)`: it is still that program.
                let real_exe = if readable {
                    self.source.real_exe(f.pid).map(|exe| {
                        exe.strip_suffix(" (deleted)")
                            .map_or_else(|| exe.clone(), str::to_string)
                    })
                } else {
                    None
                };
                let exe_unknown = readable
                    && self.source.reports_real_exe()
                    && real_exe.as_deref().is_none_or(is_dynamic_loader);
                // A process that is run from a GitPulse or agent executable
                // under another `argv[0]` is still that: the kernel's path is
                // the one it cannot change.
                let agent = f.ident.kind.or_else(|| {
                    real_exe
                        .as_deref()
                        .and_then(|exe| classify_argv(exe, argv.as_deref().unwrap_or(&[])))
                });
                TableRow {
                    pid: f.pid,
                    ppid: f.ppid,
                    uid: creds.map_or(f.uid, |c| c.effective),
                    ruid: creds.map(|c| c.real),
                    suid: creds.map(|c| c.saved),
                    start_us: f.start_us,
                    name: f.ident.name.clone(),
                    exec: f.ident.exec.clone(),
                    real_exe,
                    exe_unknown,
                    argv,
                    agent,
                    hosted: self.tree_of.get(i).copied().flatten().is_some(),
                }
            })
            .collect();
        Ok(Table { rows, me_uid })
    }
}

/// The process table with every argument list, for the signal guard. `Err`
/// when the table could not be read.
pub(crate) fn signal_table() -> Result<Table, String> {
    if !os::SUPPORTED {
        return Err("this platform has no process scan".into());
    }
    let mut buf = Vec::new();
    Scanner::os().table(|pid| os::argv_of(pid, &mut buf))
}

#[cfg(target_os = "macos")]
mod os {
    use super::{Identity, LaunchKey, ProcFacts, ProcSource, MAX_SCANNED_PROCESSES};
    use crate::sysload::sensor::{Reading, Why};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    pub(super) const SUPPORTED: bool = true;

    fn errno() -> Option<i32> {
        std::io::Error::last_os_error().raw_os_error()
    }

    pub(crate) fn list_pids(pids: &mut Vec<i32>) -> Result<(), String> {
        // Asked twice: once for the count, once with room to spare, because
        // the table can grow between the calls.
        // SAFETY: a null buffer asks only for the number of pids.
        let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        let count = usize::try_from(count)
            .map_err(|_| "the process table could not be read".to_string())?;
        if count > MAX_SCANNED_PROCESSES {
            return Err(format!(
                "{count} processes are running, more than the {MAX_SCANNED_PROCESSES} this scan reads"
            ));
        }
        pids.clear();
        pids.resize(count + 64, 0);
        let bytes = i32::try_from(pids.len() * std::mem::size_of::<i32>())
            .map_err(|_| "the process table is too large to read".to_string())?;
        // SAFETY: `bytes` is the buffer's own size in bytes.
        let written = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        let written = usize::try_from(written)
            .map_err(|_| "the process table could not be read".to_string())?;
        if written >= pids.len() {
            return Err("the process table grew while it was being read".into());
        }
        pids.truncate(written);
        Ok(())
    }

    pub(crate) fn cwd(pid: i32) -> Option<PathBuf> {
        use std::ffi::CStr;
        use std::os::unix::ffi::OsStrExt;
        let size = std::mem::size_of::<libc::proc_vnodepathinfo>();
        let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::uninit();
        // SAFETY: the buffer is exactly one `proc_vnodepathinfo`, and it is
        // read only after the kernel reports that it filled every byte of it.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                info.as_mut_ptr().cast(),
                i32::try_from(size).ok()?,
            )
        };
        if usize::try_from(written).ok() != Some(size) {
            return None;
        }
        // SAFETY: fully initialised, established above.
        let info = unsafe { info.assume_init() };
        // `vip_path` is MAXPATHLEN bytes split into rows for old rustc; it is
        // one contiguous NUL-terminated C string.
        let raw: &[libc::c_char] = info.pvi_cdir.vip_path.as_flattened();
        // SAFETY: c_char and u8 share size and alignment.
        let bytes: &[u8] = unsafe { std::slice::from_raw_parts(raw.as_ptr().cast(), raw.len()) };
        let path = CStr::from_bytes_until_nul(bytes).ok()?.to_bytes();
        if path.first() != Some(&b'/') {
            return None;
        }
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(path)))
    }

    /// `struct proc_fileinfo` from <sys/proc_info.h>.
    #[repr(C)]
    struct ProcFileInfo {
        fi_openflags: u32,
        fi_status: u32,
        fi_offset: i64,
        fi_type: i32,
        fi_guardflags: u32,
    }

    /// `struct vnode_fdinfowithpath`. Its size is checked against what the
    /// kernel writes, which is what guards this hand-written layout.
    #[repr(C)]
    struct VnodeFdInfoWithPath {
        pfi: ProcFileInfo,
        pvip: libc::vnode_info_path,
    }

    /// `PROC_PIDFDVNODEPATHINFO`.
    const FD_VNODE_PATH: i32 = 2;

    pub(crate) fn fd_path(pid: i32, fd: i32) -> Reading<String> {
        use std::ffi::CStr;
        let size = std::mem::size_of::<VnodeFdInfoWithPath>();
        let mut info = std::mem::MaybeUninit::<VnodeFdInfoWithPath>::zeroed();
        let Ok(len) = i32::try_from(size) else {
            return Reading::Unavailable(Why::Failed);
        };
        // SAFETY: the buffer is one VnodeFdInfoWithPath of `len` bytes.
        let written =
            unsafe { libc::proc_pidfdinfo(pid, fd, FD_VNODE_PATH, info.as_mut_ptr().cast(), len) };
        if written <= 0 {
            return Reading::Unavailable(Why::of_errno(errno()));
        }
        if usize::try_from(written).ok() != Some(size) {
            return Reading::Unavailable(Why::Failed);
        }
        // SAFETY: zeroed, then filled entirely by the kernel.
        let info = unsafe { info.assume_init() };
        let raw: &[libc::c_char] = info.pvip.vip_path.as_flattened();
        // SAFETY: c_char and u8 share size and alignment.
        let bytes: &[u8] = unsafe { std::slice::from_raw_parts(raw.as_ptr().cast(), raw.len()) };
        match CStr::from_bytes_until_nul(bytes) {
            Ok(path) if path.to_bytes().first() == Some(&b'/') => {
                Reading::Value(path.to_string_lossy().into_owned())
            }
            _ => Reading::Unavailable(Why::Failed),
        }
    }

    fn bsdinfo(pid: i32) -> Result<libc::proc_bsdinfo, Option<i32>> {
        let size = std::mem::size_of::<libc::proc_bsdinfo>();
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
        // SAFETY: the buffer matches the requested BSDINFO layout and is read
        // only after the kernel reports filling all of it.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                size as i32,
            )
        };
        if usize::try_from(written).ok() != Some(size) {
            return Err(errno());
        }
        // SAFETY: fully initialised, established above.
        Ok(unsafe { info.assume_init() })
    }

    fn c_name(raw: &[libc::c_char]) -> String {
        let bytes: Vec<u8> = raw
            .iter()
            .take_while(|c| **c != 0)
            .map(|c| *c as u8)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    struct Usage {
        footprint: Reading<u64>,
        written: Reading<u64>,
        cpu_ns: Reading<u64>,
    }

    fn usage(pid: i32, timebase: (u32, u32)) -> Usage {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
        // SAFETY: the buffer is one rusage_info_v4, as RUSAGE_INFO_V4 asks.
        let rc =
            unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V4, info.as_mut_ptr().cast()) };
        if rc != 0 {
            let why = Reading::Unavailable(Why::of_errno(errno()));
            return Usage {
                footprint: why,
                written: why,
                cpu_ns: why,
            };
        }
        // SAFETY: zeroed, then filled by the kernel.
        let info = unsafe { info.assume_init() };
        // CPU times are in Mach absolute-time units (ticks of 125/3 ns on
        // Apple silicon, 1 ns on Intel).
        let ticks = u128::from(info.ri_user_time) + u128::from(info.ri_system_time);
        let ns = ticks * u128::from(timebase.0) / u128::from(timebase.1.max(1));
        Usage {
            footprint: Reading::Value(info.ri_phys_footprint),
            written: Reading::Value(info.ri_diskio_byteswritten),
            cpu_ns: Reading::Value(u64::try_from(ns).unwrap_or(u64::MAX)),
        }
    }

    // Deprecated in `libc` in favour of the `mach2` crate, which this build
    // does not depend on; the call and its layout are stable.
    #[allow(deprecated)]
    fn timebase() -> (u32, u32) {
        let mut tb = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: a live mach_timebase_info.
        if unsafe { libc::mach_timebase_info(&mut tb) } != 0 || tb.denom == 0 {
            return (1, 1);
        }
        (tb.numer, tb.denom)
    }

    /// The path, arguments and launch environment of `pid`, from
    /// `KERN_PROCARGS2`: argc, the exec path, padding, argv, then the
    /// environment. Apple platform binaries show no environment.
    fn procargs(pid: i32, buf: &mut Vec<u8>) -> Option<(String, Vec<String>, Option<LaunchKey>)> {
        procargs_within(pid, buf, None)
    }

    /// [`procargs`], with `limits` as (arguments, bytes per argument) when
    /// only a bounded look is wanted: an argument over the byte bound is read
    /// as an empty one, and the environment is not read.
    fn procargs_within(
        pid: i32,
        buf: &mut Vec<u8>,
        limits: Option<(usize, usize)>,
    ) -> Option<(String, Vec<String>, Option<LaunchKey>)> {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut size = buf.len();
        // SAFETY: `buf` is a live buffer of `size` bytes.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buf.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || size < 4 || size > buf.len() {
            return None;
        }
        let data = &buf[..size];
        let argc = i32::from_ne_bytes([data[0], data[1], data[2], data[3]]);
        let argc = usize::try_from(argc).ok()?.min(4096);
        let rest = &data[4..];
        let exec_end = rest.iter().position(|b| *b == 0)?;
        let exec = String::from_utf8_lossy(&rest[..exec_end]).into_owned();
        let mut items = rest[exec_end..]
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty());
        let (max_args, max_bytes) = limits.unwrap_or((usize::MAX, usize::MAX));
        let argv: Vec<String> = items
            .by_ref()
            .take(argc.min(max_args))
            .map(|s| {
                if s.len() > max_bytes {
                    String::new()
                } else {
                    String::from_utf8_lossy(s).into_owned()
                }
            })
            .collect();
        let launch = if limits.is_some() {
            None
        } else {
            LaunchKey::from_env(items.filter_map(|s| std::str::from_utf8(s).ok()))
        };
        Some((exec, argv, launch))
    }

    fn argmax() -> usize {
        let mut value: libc::c_int = 0;
        let mut size = std::mem::size_of::<libc::c_int>();
        let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
        // SAFETY: one int, sized in `size`.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                2,
                (&mut value as *mut libc::c_int).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc == 0 {
            usize::try_from(value)
                .unwrap_or(1 << 20)
                .clamp(4096, 4 << 20)
        } else {
            1 << 20
        }
    }

    fn uid() -> u32 {
        // SAFETY: getuid has no preconditions and cannot fail.
        unsafe { libc::getuid() }
    }

    /// What every process has, whoever owns it.
    struct Basic {
        ppid: i32,
        uid: u32,
        /// Microseconds since the epoch; 0 when the OS would not say.
        start_us: u64,
        zombie: bool,
        name: String,
    }

    /// The owner a process is filed under when the OS will not even say who
    /// owns it. It is never dropped. The process listing counts it with
    /// other users' processes (it is not one this user was shown to own), but
    /// the directory-use check files it as `mine` with no arguments or
    /// environment read, because it may be this user's: so it is unproven
    /// (unreadable), never taken for a process that uses nothing.
    pub(super) const UNKNOWN_OWNER: u32 = u32::MAX;

    /// `PROC_PIDTBSDINFO` describes only this user's processes; for anyone
    /// else's it is `EPERM` (232 of 943 on the host this was written on). The
    /// short form still gives the parent, owner and name, which is all a
    /// process this user cannot measure needs to be counted. `None` only when
    /// the process is gone.
    fn basic(pid: i32) -> Option<Basic> {
        match bsdinfo(pid) {
            Ok(info) => {
                let mut name = c_name(&info.pbi_name);
                if name.is_empty() {
                    name = c_name(&info.pbi_comm);
                }
                return Some(Basic {
                    ppid: i32::try_from(info.pbi_ppid).unwrap_or(0),
                    uid: info.pbi_uid,
                    start_us: info
                        .pbi_start_tvsec
                        .saturating_mul(1_000_000)
                        .saturating_add(info.pbi_start_tvusec),
                    zombie: info.pbi_status == libc::SZOMB,
                    name,
                });
            }
            Err(Some(libc::ESRCH)) => return None,
            Err(_) => {}
        }
        let size = std::mem::size_of::<libc::proc_bsdshortinfo>();
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdshortinfo>::uninit();
        // SAFETY: the buffer matches the requested SHORTBSDINFO layout and is
        // read only after the kernel reports filling all of it.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDT_SHORTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                size as i32,
            )
        };
        if usize::try_from(written).ok() == Some(size) {
            // SAFETY: fully initialised, established above.
            let info = unsafe { info.assume_init() };
            return Some(Basic {
                ppid: i32::try_from(info.pbsi_ppid).unwrap_or(0),
                uid: info.pbsi_uid,
                start_us: 0,
                zombie: info.pbsi_status == libc::SZOMB,
                name: c_name(&info.pbsi_comm),
            });
        }
        if errno() == Some(libc::ESRCH) {
            return None;
        }
        Some(Basic {
            ppid: 0,
            uid: UNKNOWN_OWNER,
            start_us: 0,
            zombie: false,
            name: String::new(),
        })
    }

    fn facts(
        pid: i32,
        me: u32,
        timebase: (u32, u32),
        ident: impl FnOnce(&Basic) -> Arc<Identity>,
    ) -> Option<ProcFacts> {
        let basic = basic(pid)?;
        // A zombie holds no memory and does no work; it is not a process
        // anyone should be told about.
        if basic.zombie {
            return None;
        }
        let (footprint, written, cpu_ns) = if basic.uid == me {
            let u = usage(pid, timebase);
            (u.footprint, u.written, u.cpu_ns)
        } else {
            let not_ours = Reading::Unavailable(Why::Unreadable);
            (not_ours, not_ours, not_ours)
        };
        Some(ProcFacts {
            pid,
            ppid: basic.ppid,
            uid: basic.uid,
            start_us: basic.start_us,
            ident: ident(&basic),
            footprint,
            written,
            cpu_ns,
        })
    }

    fn read_identity(pid: i32, basic: &Basic, me: u32, buf: &mut Vec<u8>) -> Identity {
        if basic.uid != me {
            return Identity::new(basic.name.clone(), None, Vec::new(), None);
        }
        match procargs(pid, buf) {
            Some((exec, argv, launch)) => {
                Identity::new(basic.name.clone(), Some(exec), argv, launch)
            }
            None => Identity::new(basic.name.clone(), None, Vec::new(), None),
        }
    }

    /// Every argument of `pid`, from `KERN_PROCARGS2`. `buf` is scratch space
    /// the caller reuses across calls.
    pub(crate) fn argv_of(pid: i32, buf: &mut Vec<u8>) -> Option<Vec<String>> {
        if buf.is_empty() {
            buf.resize(argmax(), 0);
        }
        procargs(pid, buf).map(|(_, argv, _)| argv)
    }

    /// [`argv_of`], bounded for a repository hint: at most
    /// `MAX_HINT_ARGS` arguments, each read as empty when over
    /// `MAX_HINT_ARG_BYTES`.
    fn hint_argv_of(pid: i32, buf: &mut Vec<u8>) -> Option<Vec<String>> {
        if buf.is_empty() {
            buf.resize(argmax(), 0);
        }
        procargs_within(
            pid,
            buf,
            Some((super::MAX_HINT_ARGS, super::MAX_HINT_ARG_BYTES)),
        )
        .map(|(_, argv, _)| argv)
    }

    /// from `KERN_PROCARGS2`; `None` when the kernel refuses it or the block
    /// is larger than `buf`.
    fn full_procargs(pid: i32, buf: &mut [u8]) -> Option<(String, Vec<String>, Vec<String>)> {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut size = buf.len();
        // SAFETY: `buf` is a live buffer of `size` bytes.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buf.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || size > buf.len() {
            return None;
        }
        super::parse_procargs2(&buf[..size])
    }

    /// The directory-use row for a process whose arguments are not read: one
    /// of another user's (`mine: false`, nothing of it is inspected) or one
    /// whose owner the OS would not name ([`UNKNOWN_OWNER`]), which is filed as
    /// `mine` with no arguments or environment so it reads as unproven and is
    /// never taken for a process that uses nothing. `None` for this user's own
    /// process, whose arguments the caller reads.
    pub(super) fn use_without_arguments(
        pid: i32,
        owner: u32,
        me: u32,
        cwd: Option<std::path::PathBuf>,
    ) -> Option<super::ProcessUse> {
        let mine = if owner == UNKNOWN_OWNER {
            true
        } else if owner != me {
            false
        } else {
            return None;
        };
        Some(super::ProcessUse {
            pid,
            mine,
            cwd,
            ..Default::default()
        })
    }

    /// What `pid` shows of itself to this user, for the directory-use check.
    /// `None` when it is gone. A process of the user's whose argument block
    /// cannot be read is returned with `argv` and `env` empty of knowledge
    /// (`None`), so the caller cannot take it for one that uses nothing.
    pub(crate) fn read_use(pid: i32, buf: &mut Vec<u8>) -> Option<super::ProcessUse> {
        let first = basic(pid)?;
        if first.zombie {
            return None;
        }
        let cwd = cwd(pid);
        // The OS would not even say who owns it (a failed lookup that was not
        // "no such process"): it may be the user's, so it is unproven, never
        // counted as another user's and never dropped.
        if let Some(row) = use_without_arguments(pid, first.uid, uid(), cwd.clone()) {
            return Some(row);
        }
        if buf.is_empty() {
            buf.resize(argmax(), 0);
        }
        match full_procargs(pid, buf) {
            Some((exe, argv, env)) => Some(super::ProcessUse {
                pid,
                mine: true,
                cwd,
                exe: Some(exe),
                argv: Some(argv),
                env: Some(env),
            }),
            // The kernel refused the argument block. A process that is
            // exiting refuses it too, so ask once more and look at the
            // process again: one that has gone or is a zombie holds no
            // arguments and needs none read; one that is alive and still
            // refuses is unreadable, and stays so in the table.
            None => match full_procargs(pid, buf) {
                Some((exe, argv, env)) => Some(super::ProcessUse {
                    pid,
                    mine: true,
                    cwd,
                    exe: Some(exe),
                    argv: Some(argv),
                    env: Some(env),
                }),
                None => basic(pid)
                    .is_some_and(|now| !now.zombie && super::process_exists(pid))
                    .then_some(super::ProcessUse {
                        pid,
                        mine: true,
                        cwd,
                        ..Default::default()
                    }),
            },
        }
    }

    pub(crate) fn probe_one(pid: i32) -> Option<ProcFacts> {
        let me = uid();
        let mut buf = vec![0u8; argmax()];
        facts(pid, me, timebase(), |basic| {
            Arc::new(read_identity(pid, basic, me, &mut buf))
        })
    }

    pub(super) fn pid_ppid_start(pid: i32) -> Option<(i32, u64)> {
        basic(pid).map(|info| (info.ppid, info.start_us))
    }

    pub(super) struct OsSource {
        pids: Vec<i32>,
        args: Vec<u8>,
        timebase: (u32, u32),
        me: u32,
        cache: super::IdentityCache,
        seen: Vec<i32>,
    }

    impl OsSource {
        pub(super) fn new() -> Self {
            Self {
                pids: Vec::with_capacity(4096),
                args: vec![0u8; argmax()],
                timebase: timebase(),
                me: uid(),
                cache: HashMap::with_capacity(4096),
                seen: Vec::with_capacity(4096),
            }
        }
    }

    impl ProcSource for OsSource {
        fn read(&mut self, out: &mut Vec<ProcFacts>) -> Result<(), String> {
            list_pids(&mut self.pids)?;
            out.clear();
            self.seen.clear();
            let (me, timebase) = (self.me, self.timebase);
            for &pid in &self.pids {
                if pid <= 0 {
                    continue;
                }
                let cache = &mut self.cache;
                let args = &mut self.args;
                let fact = facts(pid, me, timebase, |basic| {
                    super::cached_identity(cache, pid, basic.start_us, || {
                        read_identity(pid, basic, me, args)
                    })
                });
                if let Some(fact) = fact {
                    self.seen.push(pid);
                    out.push(fact);
                }
            }
            // Forget processes that are gone, so the cache is bounded by the
            // table rather than by everything that ever ran.
            if self.cache.len() > self.seen.len() * 2 + 64 {
                let live: std::collections::HashSet<i32> = self.seen.iter().copied().collect();
                self.cache.retain(|pid, _| live.contains(pid));
            }
            Ok(())
        }

        fn cwd(&self, pid: i32) -> Option<PathBuf> {
            cwd(pid)
        }

        fn fd_path(&self, pid: i32, fd: i32) -> Reading<String> {
            fd_path(pid, fd)
        }

        fn me_uid(&self) -> u32 {
            self.me
        }

        fn is_root(&self) -> bool {
            // SAFETY: `geteuid` has no preconditions and cannot fail.
            self.me == 0 || unsafe { libc::geteuid() } == 0
        }
    }

    /// Phys footprint (bytes) and CPU time (ns) of `pid` from
    /// `proc_pid_rusage`; `None` when the process is gone or unreadable.
    pub(crate) fn footprint_cpu(pid: i32) -> Option<(u64, u64)> {
        let usage = usage(pid, timebase());
        match (usage.footprint, usage.cpu_ns) {
            (Reading::Value(footprint), Reading::Value(cpu_ns)) => Some((footprint, cpu_ns)),
            _ => None,
        }
    }

    pub(crate) fn pidpath(pid: i32) -> Option<String> {
        let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: `buf` is PROC_PIDPATHINFO_MAXSIZE bytes, which is what the
        // call writes into.
        let written = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
        if written <= 0 {
            return None;
        }
        let written = usize::try_from(written).ok()?.min(buf.len());
        let end = buf[..written]
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(written);
        let path = String::from_utf8_lossy(&buf[..end]);
        path.starts_with('/').then(|| path.into_owned())
    }

    /// Classified agent processes only. `Err` when the pid list itself could
    /// not be read — never an empty list standing in for that failure.
    pub(super) fn read_session_rows() -> Result<super::SessionTable, String> {
        let mut pids = Vec::new();
        list_pids(&mut pids)?;
        let me = uid();
        let mut args_buf = vec![0u8; argmax()];
        let mut rows = Vec::new();
        let mut links = std::collections::HashMap::with_capacity(pids.len());
        for pid in pids {
            if pid <= 0 {
                continue;
            }
            let Some(info) = basic(pid) else {
                continue;
            };
            // Every process, whoever owns it: a session's ancestor chain runs
            // through shells and runtimes that are not candidates.
            links.insert(
                pid,
                super::ProcLink {
                    ppid: info.ppid,
                    start_us: info.start_us,
                },
            );
            if info.zombie || info.uid != me {
                continue;
            }
            let Some(probe) = super::session_probe(&info.name, || pidpath(pid)) else {
                continue;
            };
            let args = procargs(pid, &mut args_buf);
            if let Some(row) =
                super::session_row(pid, info.ppid, info.start_us, probe, args, || pidpath(pid))
            {
                rows.push(row);
            }
        }
        let cwds = super::session_cwds(&rows, &links, &cwd, &mut |pid| {
            hint_argv_of(pid, &mut args_buf)
        });
        Ok(super::SessionTable { rows, links, cwds })
    }
}

#[cfg(any(target_os = "linux", all(unix, test)))]
fn parse_proc_cwd_link(path: PathBuf) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    if path.exists() {
        return Some(path);
    }
    let clean = path
        .to_str()
        .and_then(|raw| raw.strip_suffix(" (deleted)"))
        .map(PathBuf::from)
        .unwrap_or(path);
    clean.is_absolute().then_some(clean)
}

#[cfg(target_os = "linux")]
mod os {
    use super::{Identity, LaunchKey, ProcFacts, ProcSource, MAX_SCANNED_PROCESSES};
    use crate::sysload::sensor::{Reading, Why};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    pub(super) const SUPPORTED: bool = true;

    fn read_bounded(path: &str, limit: u64) -> std::io::Result<Vec<u8>> {
        use std::io::Read;
        let mut out = Vec::new();
        std::fs::File::open(path)?
            .take(limit)
            .read_to_end(&mut out)?;
        Ok(out)
    }

    pub(crate) fn list_pids(pids: &mut Vec<i32>) -> Result<(), String> {
        pids.clear();
        let entries = std::fs::read_dir("/proc")
            .map_err(|e| format!("the process table could not be read: {e}"))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("the process table could not be read: {e}"))?;
            if let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<i32>().ok())
            {
                pids.push(pid);
                if pids.len() > MAX_SCANNED_PROCESSES {
                    return Err(format!(
                        "more than {MAX_SCANNED_PROCESSES} processes are running"
                    ));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn cwd(pid: i32) -> Option<PathBuf> {
        let path = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
        // A deleted directory reads back as "/old/path (deleted)", which is still
        // proof of this process's working directory rather than an unreadable process.
        super::parse_proc_cwd_link(path)
    }

    pub(crate) fn fd_path(pid: i32, fd: i32) -> Reading<String> {
        match std::fs::read_link(format!("/proc/{pid}/fd/{fd}")) {
            Ok(path) if path.is_absolute() => Reading::Value(path.to_string_lossy().into_owned()),
            Ok(_) => Reading::Unavailable(Why::Failed),
            Err(e) => Reading::Unavailable(Why::of_errno(e.raw_os_error())),
        }
    }

    fn uid() -> u32 {
        // SAFETY: getuid has no preconditions and cannot fail.
        unsafe { libc::getuid() }
    }

    fn clock_ticks() -> u64 {
        // SAFETY: sysconf has no memory preconditions.
        u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) })
            .ok()
            .filter(|t| *t > 0)
            .unwrap_or(100)
    }

    fn page_size() -> u64 {
        // SAFETY: sysconf has no memory preconditions.
        u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap_or(4096)
    }

    fn boot_us() -> u64 {
        static BOOT: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        *BOOT.get_or_init(|| {
            read_bounded("/proc/stat", 1 << 20)
                .ok()
                .and_then(|b| {
                    String::from_utf8_lossy(&b)
                        .lines()
                        .find_map(|l| l.strip_prefix("btime ")?.trim().parse::<u64>().ok())
                })
                .map_or(0, |s| s.saturating_mul(1_000_000))
        })
    }

    struct Stat {
        name: String,
        ppid: i32,
        cpu_ticks: u64,
        start_ticks: u64,
        zombie: bool,
    }

    fn stat(pid: i32) -> Option<Stat> {
        let raw = read_bounded(&format!("/proc/{pid}/stat"), 8192).ok()?;
        let text = String::from_utf8_lossy(&raw);
        let open = text.find('(')?;
        let close = text.rfind(')')?;
        let name = text.get(open + 1..close)?.to_string();
        let fields: Vec<&str> = text.get(close + 1..)?.split_whitespace().collect();
        let field = |i: usize| fields.get(i).and_then(|v| v.parse::<u64>().ok());
        Some(Stat {
            name,
            zombie: fields.first() == Some(&"Z"),
            ppid: field(1).and_then(|v| i32::try_from(v).ok()).unwrap_or(0),
            cpu_ticks: field(11)?.saturating_add(field(12)?),
            start_ticks: field(19)?,
        })
    }

    fn written(pid: i32) -> Reading<u64> {
        match read_bounded(&format!("/proc/{pid}/io"), 4096) {
            Ok(raw) => String::from_utf8_lossy(&raw)
                .lines()
                .find_map(|l| l.strip_prefix("write_bytes:")?.trim().parse().ok())
                .map_or(Reading::Unavailable(Why::Failed), Reading::Value),
            Err(e) => Reading::Unavailable(Why::of_errno(e.raw_os_error())),
        }
    }

    fn resident(pid: i32, page: u64) -> Reading<u64> {
        match read_bounded(&format!("/proc/{pid}/statm"), 512) {
            Ok(raw) => String::from_utf8_lossy(&raw)
                .split_whitespace()
                .nth(1)
                .and_then(|v| v.parse::<u64>().ok())
                .map_or(Reading::Unavailable(Why::Failed), |pages| {
                    Reading::Value(pages.saturating_mul(page))
                }),
            Err(e) => Reading::Unavailable(Why::of_errno(e.raw_os_error())),
        }
    }

    fn owner(pid: i32) -> Option<u32> {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(format!("/proc/{pid}"))
            .ok()
            .map(|m| m.uid())
    }

    fn read_identity(pid: i32, name: String, ours: bool) -> Identity {
        if !ours {
            return Identity::new(name, None, Vec::new(), None);
        }
        let exec = std::fs::read_link(format!("/proc/{pid}/exe"))
            .ok()
            .map(|p| p.to_string_lossy().into_owned());
        let argv: Vec<String> = read_bounded(&format!("/proc/{pid}/cmdline"), 1 << 20)
            .map(|raw| {
                raw.split(|b| *b == 0)
                    .filter(|s| !s.is_empty())
                    .map(|s| String::from_utf8_lossy(s).into_owned())
                    .collect()
            })
            .unwrap_or_default();
        let launch = read_bounded(&format!("/proc/{pid}/environ"), 1 << 20)
            .ok()
            .and_then(|raw| {
                LaunchKey::from_env(
                    raw.split(|b| *b == 0)
                        .filter_map(|s| std::str::from_utf8(s).ok())
                        .collect::<Vec<_>>(),
                )
            });
        // `/proc/<pid>/exe` is the resolved binary; the exec spelling a
        // symlink would keep is argv[0] when it is a path.
        let exec = match (&exec, argv.first()) {
            (_, Some(first)) if first.starts_with('/') => Some(first.clone()),
            _ => exec,
        };
        Identity::new(name, exec, argv, launch)
    }

    fn facts(
        pid: i32,
        me: u32,
        ticks: u64,
        page: u64,
        ident: impl FnOnce(String, bool, u64) -> Arc<Identity>,
    ) -> Option<ProcFacts> {
        let st = stat(pid)?;
        if st.zombie {
            return None;
        }
        let uid = owner(pid)?;
        let start_us = boot_us().saturating_add(st.start_ticks.saturating_mul(1_000_000) / ticks);
        let ours = uid == me;
        let (footprint, written, cpu_ns) = if ours {
            (
                resident(pid, page),
                written(pid),
                Reading::Value(st.cpu_ticks.saturating_mul(1_000_000_000) / ticks),
            )
        } else {
            let not_ours = Reading::Unavailable(Why::Unreadable);
            (not_ours, not_ours, not_ours)
        };
        Some(ProcFacts {
            pid,
            ppid: st.ppid,
            uid,
            start_us,
            ident: ident(st.name, ours, start_us),
            footprint,
            written,
            cpu_ns,
        })
    }

    /// Every argument of `pid`, from `/proc/<pid>/cmdline`, the way the
    /// procps tools show them: an empty argument between two others stays.
    /// A process that is gone by the time it is read has no arguments left to
    /// match and is not a read that failed.
    /// [`argv_of`], bounded for a repository hint: at most
    /// `MAX_HINT_ARGS` arguments, each read as empty when over
    /// `MAX_HINT_ARG_BYTES`. Unlike [`argv_of`] a process that is gone is not
    /// an empty list: it is no evidence.
    fn hint_argv_of(pid: i32) -> Option<Vec<String>> {
        let raw = read_bounded(
            &format!("/proc/{pid}/cmdline"),
            (super::MAX_HINT_ARGS * super::MAX_HINT_ARG_BYTES) as u64,
        )
        .ok()?;
        Some(super::split_cmdline_within(
            &raw,
            Some((super::MAX_HINT_ARGS, super::MAX_HINT_ARG_BYTES)),
        ))
    }

    pub(crate) fn argv_of(pid: i32, _buf: &mut Vec<u8>) -> Option<Vec<String>> {
        match read_bounded(&format!("/proc/{pid}/cmdline"), 1 << 20) {
            Ok(raw) => Some(super::split_cmdline(&raw)),
            // Gone means the directory is gone too; a read that fails with the
            // process still listed is one that could not be made.
            Err(_) if !std::path::Path::new(&format!("/proc/{pid}")).exists() => Some(Vec::new()),
            Err(_) => None,
        }
    }

    pub(crate) fn probe_one(pid: i32) -> Option<ProcFacts> {
        facts(pid, uid(), clock_ticks(), page_size(), |name, ours, _| {
            Arc::new(read_identity(pid, name, ours))
        })
    }

    pub(super) fn pid_ppid_start(pid: i32) -> Option<(i32, u64)> {
        let st = stat(pid)?;
        let ticks = clock_ticks().max(1);
        let boot = boot_us();
        let start_us = boot.saturating_add(st.start_ticks.saturating_mul(1_000_000) / ticks);
        Some((st.ppid, start_us))
    }

    pub(super) struct OsSource {
        pids: Vec<i32>,
        me: u32,
        ticks: u64,
        page: u64,
        cache: super::IdentityCache,
    }

    impl OsSource {
        pub(super) fn new() -> Self {
            Self {
                pids: Vec::with_capacity(4096),
                me: uid(),
                ticks: clock_ticks(),
                page: page_size(),
                cache: HashMap::with_capacity(4096),
            }
        }
    }

    impl ProcSource for OsSource {
        fn read(&mut self, out: &mut Vec<ProcFacts>) -> Result<(), String> {
            list_pids(&mut self.pids)?;
            out.clear();
            let (me, ticks, page) = (self.me, self.ticks, self.page);
            for &pid in &self.pids {
                let cache = &mut self.cache;
                let fact = facts(pid, me, ticks, page, |name, ours, start_us| {
                    super::cached_identity(cache, pid, start_us, || read_identity(pid, name, ours))
                });
                if let Some(fact) = fact {
                    out.push(fact);
                }
            }
            if self.cache.len() > out.len() * 2 + 64 {
                let live: std::collections::HashSet<i32> = out.iter().map(|f| f.pid).collect();
                self.cache.retain(|pid, _| live.contains(pid));
            }
            Ok(())
        }

        fn cwd(&self, pid: i32) -> Option<PathBuf> {
            cwd(pid)
        }

        fn fd_path(&self, pid: i32, fd: i32) -> Reading<String> {
            fd_path(pid, fd)
        }

        fn me_uid(&self) -> u32 {
            self.me
        }

        fn creds(&self, pid: i32) -> Option<super::Creds> {
            let raw = read_bounded(&format!("/proc/{pid}/status"), 8192).ok()?;
            super::parse_status_uids(&String::from_utf8_lossy(&raw))
        }

        fn real_exe(&self, pid: i32) -> Option<String> {
            exe_path(pid)
        }

        fn reports_real_exe(&self) -> bool {
            true
        }

        fn is_root(&self) -> bool {
            // SAFETY: `geteuid` has no preconditions and cannot fail.
            self.me == 0 || unsafe { libc::geteuid() } == 0
        }
    }

    fn exe_path(pid: i32) -> Option<String> {
        let path = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
        let text = path.to_string_lossy();
        text.starts_with('/').then(|| text.into_owned())
    }

    /// What `pid` shows of itself to this user, for the directory-use check.
    /// `None` when it is gone. Anything of the user's that cannot be read is
    /// `None` in the row, never an empty list.
    pub(crate) fn read_use(pid: i32, _buf: &mut Vec<u8>) -> Option<super::ProcessUse> {
        let info = stat(pid)?;
        if info.zombie {
            return None;
        }
        let cwd = cwd(pid);
        if owner(pid)? != uid() {
            return Some(super::ProcessUse {
                pid,
                mine: false,
                cwd,
                ..Default::default()
            });
        }
        let split = |raw: Vec<u8>| -> Vec<String> {
            raw.split(|byte| *byte == 0)
                .filter(|item| !item.is_empty())
                .map(|item| String::from_utf8_lossy(item).into_owned())
                .collect()
        };
        let argv = read_bounded(&format!("/proc/{pid}/cmdline"), 1 << 20)
            .ok()
            .map(split);
        let env = read_bounded(&format!("/proc/{pid}/environ"), 1 << 20)
            .ok()
            .map(split);
        // A process that exited while it was read holds nothing to read.
        if (argv.is_none() || env.is_none()) && stat(pid).is_none_or(|now| now.zombie) {
            return None;
        }
        Some(super::ProcessUse {
            pid,
            mine: true,
            cwd,
            exe: exe_path(pid),
            argv,
            env,
        })
    }

    /// The exec spelling, arguments and launch environment. `argv[0]` wins
    /// when it is a path, because `/proc/<pid>/exe` has already resolved a
    /// symlink. `None` when the argument list itself could not be read.
    fn procargs(pid: i32) -> Option<(String, Vec<String>, Option<LaunchKey>)> {
        let raw = read_bounded(&format!("/proc/{pid}/cmdline"), 1 << 20).ok()?;
        let argv: Vec<String> = raw
            .split(|byte| *byte == 0)
            .filter(|item| !item.is_empty())
            .map(|item| String::from_utf8_lossy(item).into_owned())
            .collect();
        let resolved = exe_path(pid);
        let exec = match argv.first() {
            Some(first) if first.starts_with('/') => first.clone(),
            _ => resolved?,
        };
        let launch = read_bounded(&format!("/proc/{pid}/environ"), 1 << 20)
            .ok()
            .and_then(|raw| {
                LaunchKey::from_env(
                    raw.split(|byte| *byte == 0)
                        .filter_map(|item| std::str::from_utf8(item).ok()),
                )
            });
        Some((exec, argv, launch))
    }

    /// Classified agent processes only. `Err` when the pid list itself could
    /// not be read — never an empty list standing in for that failure.
    pub(super) fn read_session_rows() -> Result<super::SessionTable, String> {
        let mut pids = Vec::new();
        list_pids(&mut pids)?;
        let me = uid();
        let ticks = clock_ticks();
        let boot = boot_us();
        let mut rows = Vec::new();
        let mut links = std::collections::HashMap::with_capacity(pids.len());
        for pid in pids {
            if pid <= 0 {
                continue;
            }
            let Some(info) = stat(pid) else {
                continue;
            };
            let start_us = boot.saturating_add(info.start_ticks.saturating_mul(1_000_000) / ticks);
            // Every process, whoever owns it: a session's ancestor chain runs
            // through shells and runtimes that are not candidates.
            links.insert(
                pid,
                super::ProcLink {
                    ppid: info.ppid,
                    start_us,
                },
            );
            if info.zombie {
                continue;
            }
            let Some(owner_uid) = owner(pid) else {
                continue;
            };
            if owner_uid != me {
                continue;
            }
            let Some(probe) = super::session_probe(&info.name, || exe_path(pid)) else {
                continue;
            };
            let args = procargs(pid);
            if let Some(row) =
                super::session_row(pid, info.ppid, start_us, probe, args, || exe_path(pid))
            {
                rows.push(row);
            }
        }
        let cwds = super::session_cwds(&rows, &links, &cwd, &mut |pid| hint_argv_of(pid));
        Ok(super::SessionTable { rows, links, cwds })
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod os {
    use super::{ProcFacts, ProcSource};
    use crate::sysload::sensor::{Reading, Why};
    use std::path::PathBuf;

    pub(super) const SUPPORTED: bool = false;

    pub(crate) fn list_pids(_pids: &mut Vec<i32>) -> Result<(), String> {
        Err("listing processes is not supported on this platform".into())
    }

    pub(crate) fn cwd(_pid: i32) -> Option<PathBuf> {
        None
    }

    pub(crate) fn argv_of(_pid: i32, _buf: &mut Vec<u8>) -> Option<Vec<String>> {
        None
    }

    pub(crate) fn probe_one(_pid: i32) -> Option<ProcFacts> {
        None
    }

    pub(super) fn pid_ppid_start(_pid: i32) -> Option<(i32, u64)> {
        None
    }

    pub(crate) fn read_use(_pid: i32, _buf: &mut Vec<u8>) -> Option<super::ProcessUse> {
        None
    }

    pub(super) struct OsSource;

    impl OsSource {
        pub(super) fn new() -> Self {
            Self
        }
    }

    impl ProcSource for OsSource {
        fn read(&mut self, _out: &mut Vec<ProcFacts>) -> Result<(), String> {
            Err("this platform has no process scan".into())
        }

        fn cwd(&self, _pid: i32) -> Option<PathBuf> {
            None
        }

        fn fd_path(&self, _pid: i32, _fd: i32) -> Reading<String> {
            Reading::Unavailable(Why::Unsupported)
        }

        fn me_uid(&self) -> u32 {
            0
        }
    }

    pub(super) fn read_session_rows() -> Result<super::SessionTable, String> {
        Err("this platform has no process scan".into())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;

    pub(crate) const ME: u32 = 501;

    fn spinning_orphan() -> Orphan {
        Orphan {
            pid: 4242,
            start_us: 7,
            name: "node".into(),
            exec: None,
            cwd: "/r/app".into(),
            checkout: "/r/app".into(),
            age_s: 1_800,
            cpu_ratio: 0.9,
            footprint: Reading::default(),
        }
    }

    /// The Terminate button is DOM; the native Yes is the boundary. A
    /// declined dialog signals nothing, and the dialog names the process.
    #[test]
    fn a_declined_confirmation_signals_nothing() {
        let shown = std::cell::RefCell::new(String::new());
        let signalled = std::cell::Cell::new(false);
        let refused = terminate_confirmed(
            || Ok(spinning_orphan()),
            &|_, body| {
                *shown.borrow_mut() = body.to_owned();
                false
            },
            || {
                signalled.set(true);
                Ok(())
            },
        )
        .expect_err("declined");
        assert!(refused.contains("not confirmed"), "{refused}");
        assert!(!signalled.get(), "a declined dialog sent a signal");
        assert!(
            shown.borrow().contains("4242") && shown.borrow().contains("node"),
            "{}",
            shown.borrow()
        );
    }

    /// The dialog can stay open for minutes; a process that stopped being an
    /// orphan meanwhile is refused after the Yes, not signalled on it.
    #[test]
    fn a_process_that_changed_while_the_dialog_was_open_is_not_signalled() {
        let checks = std::cell::Cell::new(0u32);
        let signalled = std::cell::Cell::new(false);
        let refused = terminate_confirmed(
            || {
                checks.set(checks.get() + 1);
                if checks.get() == 1 {
                    Ok(spinning_orphan())
                } else {
                    Err("the process has a parent again; it is no longer orphaned".into())
                }
            },
            &|_, _| true,
            || {
                signalled.set(true);
                Ok(())
            },
        )
        .expect_err("changed");
        assert!(refused.contains("parent again"), "{refused}");
        assert_eq!(checks.get(), 2, "re-checked after the dialog");
        assert!(!signalled.get());
    }

    #[test]
    fn a_confirmed_orphan_is_signalled_once() {
        let signals = std::cell::Cell::new(0u32);
        let sent = terminate_confirmed(
            || Ok(spinning_orphan()),
            &|_, _| true,
            || {
                signals.set(signals.get() + 1);
                Ok(())
            },
        )
        .expect("confirmed");
        assert_eq!(signals.get(), 1);
        assert!(sent.contains("4242") && sent.contains("30 min"), "{sent}");
    }

    /// The signal re-reads the start time itself: a pid that now belongs to a
    /// different process than the one the person was shown is never signalled,
    /// even when the checks before it passed. The stand-in for the reused pid is
    /// a live process asked about under a start time it does not have.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_signal_itself_refuses_a_pid_whose_start_time_moved() {
        use crate::procguard::LockedSpawn;
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .spawn_locked()
            .expect("spawn");
        let pid = i32::try_from(child.id()).unwrap();
        let start = (0..100)
            .find_map(|_| {
                std::thread::sleep(std::time::Duration::from_millis(10));
                probe_one(pid).map(|f| f.start_us)
            })
            .expect("the fixture is visible to the scan");
        let refused = signal_term(pid, start + 1).expect_err("a moved start time");
        assert!(refused.contains("different process"), "{refused}");
        assert!(
            child.try_wait().unwrap().is_none(),
            "a pid whose start time moved was signalled"
        );
        for id in [0, 1, -1, i32::MIN] {
            assert!(signal_term(id, start).is_err(), "pid {id}");
        }
        signal_term(pid, start).expect("the process the start time names");
        let status = child.wait().unwrap();
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&status),
            Some(libc::SIGTERM)
        );
    }

    fn table_row(pid: i32, cwd: &str, exe: &str, argv: &[&str], env: &[&str]) -> ProcessUse {
        ProcessUse {
            pid,
            mine: true,
            cwd: Some(PathBuf::from(cwd)),
            exe: Some(exe.into()),
            argv: Some(argv.iter().map(|a| (*a).into()).collect()),
            env: Some(env.iter().map(|e| (*e).into()).collect()),
        }
    }

    /// The cwd-only reading, over the same rows, for a before/after pair.
    fn cwd_only(dir: &Path, table: &[ProcessUse]) -> Vec<i32> {
        table
            .iter()
            .filter(|p| p.cwd.as_deref().is_some_and(|cwd| cwd.starts_with(dir)))
            .map(|p| p.pid)
            .collect()
    }

    /// Today's shape: the building process stands in another directory and the
    /// scratch directory appears only as `--target-dir` and in the paths of
    /// running test executables.
    #[test]
    fn a_directory_named_only_by_arguments_and_executables_is_in_use() {
        let dir = Path::new("/private/tmp/claude-501/proj/sess/scratchpad/cargo-target");
        let table = vec![
            table_row(
                10,
                "/Users/u/repo",
                "/Users/u/.cargo/bin/cargo",
                &[
                    "cargo",
                    "test",
                    "--target-dir",
                    "/private/tmp/claude-501/proj/sess/scratchpad/cargo-target",
                ],
                &["HOME=/Users/u"],
            ),
            table_row(
                11,
                "/Users/u/repo/src-tauri",
                "/private/tmp/claude-501/proj/sess/scratchpad/cargo-target/debug/deps/app-1a2b",
                &["app-1a2b"],
                &[],
            ),
            table_row(12, "/Users/u", "/bin/zsh", &["-zsh"], &[]),
        ];
        assert!(
            cwd_only(dir, &table).is_empty(),
            "the cwd-only reading calls today's directory unused"
        );
        let reading = dir_use_in(dir, &table, 1);
        assert_eq!(
            reading.users,
            vec![(10, UseKind::Argument), (11, UseKind::Executable)]
        );
        assert!(!reading.provably_unused());
    }

    #[test]
    fn every_way_of_naming_a_directory_is_found() {
        let dir = Path::new("/work/x-target");
        let by = |argv: &[&str], env: &[&str], cwd: &str| {
            classify_use(
                &dir_spellings(dir),
                &table_row(1, cwd, "/bin/tool", argv, env),
            )
        };
        assert_eq!(
            by(&["t"], &[], "/work/x-target/deep"),
            Some(UseKind::WorkingDirectory)
        );
        assert_eq!(
            by(&["t", "--target-dir=/work/x-target"], &[], "/"),
            Some(UseKind::Argument)
        );
        assert_eq!(
            by(
                &["rustc", "-Ldependency=/work/x-target/debug/deps"],
                &[],
                "/"
            ),
            Some(UseKind::Argument)
        );
        assert_eq!(
            by(
                &["cargo", "--manifest-path", "/work/x-target/Cargo.toml"],
                &[],
                "/"
            ),
            Some(UseKind::Argument)
        );
        // A relative spelling resolved against the process's own directory.
        assert_eq!(
            by(
                &["cargo", "build", "--target-dir", "x-target"],
                &[],
                "/work"
            ),
            Some(UseKind::Argument)
        );
        assert_eq!(
            by(
                &["cargo", "--target-dir", "../x-target"],
                &[],
                "/work/other"
            ),
            Some(UseKind::Argument)
        );
        assert_eq!(
            by(&["cargo"], &["CARGO_TARGET_DIR=/work/x-target"], "/"),
            Some(UseKind::Environment)
        );
    }

    /// Names that only look like the directory are not it, so a different
    /// session's longer id (or another prefix) never keeps this one alive.
    #[test]
    fn a_longer_name_or_a_longer_path_is_not_the_directory() {
        let dir = Path::new("/work/sess-1");
        let by = |argv: &[&str], env: &[&str]| {
            classify_use(
                &dir_spellings(dir),
                &table_row(1, "/", "/bin/tool", argv, env),
            )
        };
        assert_eq!(by(&["t", "/work/sess-10/target"], &[]), None);
        assert_eq!(by(&["t", "/work/sess-1-copy"], &[]), None);
        assert_eq!(by(&["t", "/elsewhere/work/sess-1/target"], &[]), None);
        assert_eq!(by(&["t"], &["X=/work/sess-1x"]), None);
        assert_eq!(by(&["t", "/work"], &[]), None);
        assert_eq!(
            by(&["t", "/work/sess-1/target"], &[]),
            Some(UseKind::Argument)
        );
    }

    #[test]
    fn the_macos_private_alias_is_the_same_directory() {
        let table = vec![table_row(
            1,
            "/",
            "/bin/tool",
            &["t", "--target-dir", "/tmp/a/b"],
            &[],
        )];
        assert_eq!(
            dir_use_in(Path::new("/private/tmp/a/b"), &table, 99).users,
            vec![(1, UseKind::Argument)]
        );
        let table = vec![table_row(1, "/private/tmp/a/b", "/bin/tool", &["t"], &[])];
        assert_eq!(
            dir_use_in(Path::new("/tmp/a/b"), &table, 99).users,
            vec![(1, UseKind::WorkingDirectory)]
        );
    }

    /// A process of the user's that cannot be read cannot be shown not to use
    /// the directory; another user's is counted and does not block.
    #[test]
    fn an_unreadable_process_is_unproven_not_unused() {
        let dir = Path::new("/work/x");
        let unreadable = ProcessUse {
            pid: 7,
            mine: true,
            ..Default::default()
        };
        let foreign = ProcessUse {
            pid: 8,
            mine: false,
            ..Default::default()
        };
        let reading = dir_use_in(dir, &[unreadable.clone(), foreign.clone()], 1);
        assert!(reading.users.is_empty());
        assert_eq!(reading.unreadable, vec![7]);
        assert_eq!(reading.other_users, 1);
        assert!(!reading.provably_unused());
        assert!(reading
            .why_kept()
            .is_some_and(|why| why.contains("could not be read")));
        let clear = dir_use_in(dir, &[foreign], 1);
        assert!(clear.provably_unused());
        // Only a read failure of the argument block or environment counts.
        let half = ProcessUse {
            argv: Some(vec!["x".into()]),
            ..unreadable
        };
        assert_eq!(dir_use_in(dir, &[half], 1).unreadable, vec![7]);
    }

    fn unreadable_row(pid: i32) -> ProcessUse {
        ProcessUse {
            pid,
            mine: true,
            ..Default::default()
        }
    }

    fn complete_row(pid: i32, cwd: &str) -> ProcessUse {
        ProcessUse {
            pid,
            mine: true,
            cwd: Some(PathBuf::from(cwd)),
            exe: Some("/bin/tool".into()),
            argv: Some(vec!["tool".into()]),
            env: Some(Vec::new()),
        }
    }

    /// A process caught mid-exit is unreadable on the first read and gone on
    /// the next: it is dropped, and the directory is then provably unused.
    #[test]
    fn a_row_that_is_gone_on_the_reread_is_dropped() {
        let dir = Path::new("/work/x");
        let (mut rereads, mut pauses) = (Vec::new(), 0usize);
        let table = settle_unproven(
            vec![unreadable_row(7), complete_row(9, "/elsewhere")],
            |pid| {
                rereads.push(pid);
                None
            },
            || pauses += 1,
        );
        assert_eq!(
            table.iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![9],
            "the exiting process must be dropped and the complete one kept"
        );
        assert_eq!(rereads, vec![7]);
        assert_eq!(pauses, 1);
        assert!(dir_use_in(dir, &table, 1).provably_unused());
    }

    /// The re-read row replaces the unproven one: its content is used, not
    /// only its absence.
    #[test]
    fn a_row_that_is_complete_on_the_reread_replaces_the_unproven_one() {
        let dir = Path::new("/work/x");
        let table = settle_unproven(
            vec![unreadable_row(7)],
            |pid| Some(complete_row(pid, "/work/x/sub")),
            || {},
        );
        let reading = dir_use_in(dir, &table, 1);
        assert!(reading.unreadable.is_empty(), "{reading:?}");
        assert_eq!(reading.users, vec![(7, UseKind::WorkingDirectory)]);
    }

    /// A process that is alive and really unreadable is still unproven after
    /// every round, and the settle pass is bounded.
    #[test]
    fn a_row_still_unproven_after_every_round_stays_and_the_pass_is_bounded() {
        let dir = Path::new("/work/x");
        let (mut rereads, mut pauses) = (0usize, 0usize);
        let table = settle_unproven(
            vec![unreadable_row(7)],
            |pid| {
                rereads += 1;
                Some(unreadable_row(pid))
            },
            || pauses += 1,
        );
        assert_eq!(dir_use_in(dir, &table, 1).unreadable, vec![7]);
        // Three rounds, stated outright: comparing with the constant alone
        // would pass a pass that does nothing at zero rounds.
        assert_eq!(rereads, 3, "one re-read per round, no more");
        assert_eq!(pauses, 3, "one pause per round, no more");
        assert_eq!(USE_SETTLE_ROUNDS, 3);
    }

    /// A table with nothing unproven costs nothing: no re-read, no pause.
    #[test]
    fn a_table_with_no_unproven_row_is_not_reread_or_paused() {
        let (mut rereads, mut pauses) = (0usize, 0usize);
        let table = settle_unproven(
            vec![complete_row(7, "/a"), complete_row(8, "/b")],
            |_| {
                rereads += 1;
                None
            },
            || pauses += 1,
        );
        assert_eq!(table.len(), 2);
        assert_eq!((rereads, pauses), (0, 0));
    }

    /// Another user's process cannot be inspected, so it is never re-read.
    #[test]
    fn another_users_row_is_never_reread() {
        let foreign = ProcessUse {
            pid: 8,
            mine: false,
            ..Default::default()
        };
        assert!(!foreign.unproven());
        let (mut rereads, mut pauses) = (0usize, 0usize);
        let table = settle_unproven(
            vec![foreign],
            |_| {
                rereads += 1;
                None
            },
            || pauses += 1,
        );
        assert_eq!(table.len(), 1);
        assert_eq!((rereads, pauses), (0, 0));
    }

    /// A relative build directory means the process's own working directory:
    /// `--target-dir rel`,`--target-dir=rel` and `CARGO_TARGET_DIR=rel` each
    /// name the directory from another cwd, and none of them from a cwd that
    /// resolves elsewhere or a value that names a longer sibling.
    #[test]
    fn a_relative_build_directory_is_resolved_against_the_processs_cwd() {
        let dir = Path::new("/work/repo/.gitpulse/scratch/target-a");
        let table = vec![
            table_row(
                1,
                "/work/repo",
                "/bin/tool",
                &[
                    "cargo",
                    "build",
                    "--target-dir",
                    ".gitpulse/scratch/target-a",
                ],
                &[],
            ),
            table_row(
                2,
                "/work/repo",
                "/bin/tool",
                &["cargo", "build", "--target-dir=.gitpulse/scratch/target-a"],
                &[],
            ),
            table_row(
                3,
                "/work/repo/src",
                "/bin/tool",
                &["cargo", "build"],
                &["CARGO_TARGET_DIR=../.gitpulse/scratch/target-a"],
            ),
            // The same words from a cwd that resolves somewhere else.
            table_row(
                4,
                "/elsewhere",
                "/bin/tool",
                &["cargo", "build", "--target-dir=.gitpulse/scratch/target-a"],
                &["CARGO_TARGET_DIR=.gitpulse/scratch/target-a"],
            ),
            // A longer sibling name is not the directory.
            table_row(
                5,
                "/work/repo",
                "/bin/tool",
                &["cargo", "build", "--target-dir=.gitpulse/scratch/target-ab"],
                &[],
            ),
        ];
        let reading = dir_use_in(dir, &table, 99);
        assert_eq!(
            reading.users,
            vec![
                (1, UseKind::Argument),
                (2, UseKind::Argument),
                (3, UseKind::Environment)
            ]
        );
    }

    /// A session started with an explicit id names it in its own arguments; a
    /// resume does not (it may continue under a new id), and a helper mode of the
    /// same executable is not a session at all.
    #[test]
    fn a_claude_cli_session_id_comes_from_session_id_only() {
        let args = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let id = "0123abcd-4567-89ef-0123-456789abcdef";
        assert_eq!(
            claude_session_id_in(&args(&["claude", "--model", "m", "--session-id", id])),
            Some(id.to_string())
        );
        assert_eq!(
            claude_session_id_in(&args(&["claude", &format!("--session-id={id}")])),
            Some(id.to_string())
        );
        assert_eq!(
            claude_session_id_in(&args(&["claude", "--resume", id])),
            None
        );
        assert_eq!(
            claude_session_id_in(&args(&["claude", "--session-id", "../../etc"])),
            None,
            "an id is a plain name, never a path"
        );
        assert_eq!(
            claude_session_id_in(&args(&["claude", "--session-id"])),
            None
        );
        assert!(is_claude_helper(&args(&["rg", "-G", "--hidden"])));
        assert!(is_claude_helper(&args(&[
            "/x/claude",
            "--chrome-native-host"
        ])));
        assert!(!is_claude_helper(&args(&["claude", "--session-id", id])));
    }

    /// A process of the user's whose working directory the OS would not show
    /// is unproven, like one whose arguments it would not show.
    #[test]
    fn an_unreadable_working_directory_is_unproven() {
        let dir = Path::new("/work/x");
        let hidden = ProcessUse {
            pid: 7,
            mine: true,
            cwd: None,
            exe: Some("/bin/tool".into()),
            argv: Some(vec!["t".into()]),
            env: Some(vec![]),
        };
        let reading = dir_use_in(dir, std::slice::from_ref(&hidden), 1);
        assert_eq!(reading.unreadable, vec![7]);
        assert!(!reading.provably_unused());
        let seen = ProcessUse {
            cwd: Some("/elsewhere".into()),
            ..hidden
        };
        assert!(dir_use_in(dir, &[seen], 1).provably_unused());
    }

    /// What the kernel says about a process whose working directory was removed
    /// under it, read from a child this test owns: the old path, so the
    /// process still counts as a user of it. A failed lookup reads as no path,
    /// which is unproven.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_removed_working_directory_still_reports_its_old_path_and_a_failed_lookup_is_unproven() {
        use crate::procguard::LockedSpawn;
        let temp = tempfile::tempdir().unwrap();
        let base = crate::engine::process::canonicalize_plain(temp.path()).unwrap();
        let dir = base.join("gone");
        std::fs::create_dir(&dir).unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .current_dir(&dir)
            .stdin(std::process::Stdio::null())
            .spawn_locked()
            .expect("spawn");
        let pid = i32::try_from(child.id()).unwrap();
        let inside =
            crate::test_support::env::wait_until(std::time::Duration::from_secs(2), || {
                os::cwd(pid).as_deref() == Some(dir.as_path())
            });
        std::fs::remove_dir(&dir).unwrap();
        let removed = os::cwd(pid);
        let mut buf = Vec::new();
        let row = os::read_use(pid, &mut buf);
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(inside, "the child never showed its directory");
        // Measured here (macOS, `proc_pidinfo(PROC_PIDVNODEPATHINFO)`): the
        // kernel keeps answering with the removed directory's old path, so the
        // process is still a user of that path.
        assert_eq!(removed.as_deref(), Some(dir.as_path()), "{removed:?}");
        let row = row.expect("alive when read");
        assert_eq!(row.cwd.as_deref(), Some(dir.as_path()), "{row:?}");
        let reading = dir_use_in(&base, std::slice::from_ref(&row), 1);
        assert!(reading.unreadable.is_empty(), "{reading:?}");
        assert_eq!(reading.users.len(), 1, "{reading:?}");
        // A lookup that failed (no such process) reads as no path at all.
        assert_eq!(os::cwd(i32::MAX - 7), None);
    }

    /// A process whose owner the OS would not name is filed as this user's
    /// with nothing read, so it is unproven in the directory-use check; one
    /// that is plainly another user's is not inspected and not unproven; this
    /// user's own is left for the caller to read.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_unnamed_owner_is_unproven_and_another_users_process_is_not() {
        let dir = Path::new("/work/x");
        let me = 501;
        let unknown = os::use_without_arguments(9, os::UNKNOWN_OWNER, me, None)
            .expect("an unnamed owner has a row");
        assert!(unknown.mine && unknown.argv.is_none() && unknown.env.is_none());
        let reading = dir_use_in(dir, std::slice::from_ref(&unknown), 1);
        assert_eq!(reading.unreadable, vec![9], "{reading:?}");
        let other =
            os::use_without_arguments(10, 0, me, None).expect("another user's process has a row");
        assert!(!other.mine);
        assert!(dir_use_in(dir, std::slice::from_ref(&other), 1).provably_unused());
        assert!(os::use_without_arguments(11, me, me, None).is_none());
    }

    #[test]
    fn this_process_is_never_its_own_user() {
        let dir = Path::new("/work/x");
        let table = vec![table_row(5, "/work/x", "/bin/tool", &["t"], &[])];
        assert!(dir_use_in(dir, &table, 5).users.is_empty());
    }

    #[test]
    fn a_procargs2_block_yields_path_arguments_and_environment() {
        let mut block = 2i32.to_ne_bytes().to_vec();
        block.extend_from_slice(
            b"/bin/tool\0\0\0tool\0--target-dir\0/w/x\0HOME=/h\0CARGO_TARGET_DIR=/w/x\0",
        );
        let (exec, argv, env) = parse_procargs2(&block).expect("block");
        assert_eq!(exec, "/bin/tool");
        assert_eq!(argv, vec!["tool", "--target-dir"]);
        assert_eq!(env, vec!["/w/x", "HOME=/h", "CARGO_TARGET_DIR=/w/x"]);
        assert!(parse_procargs2(&[1, 0]).is_none());
    }

    /// The child the real-process test runs: this very test binary, asked to
    /// run only this test, which holds still until its stdin closes. A
    /// platform binary such as `/bin/sh` shows no environment to a scan and a
    /// copy of one is killed by the OS, so the test binary is the one program
    /// that is both ours to spawn and readable.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn directory_use_child_holds_still_until_stdin_closes() {
        if std::env::var_os("GITPULSE_DIR_USE_CHILD").is_none() {
            return;
        }
        let mut sink = Vec::new();
        let _ = std::io::Read::read_to_end(&mut std::io::stdin(), &mut sink);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub(crate) fn directory_use_child(
        scratch: &Path,
        elsewhere: &Path,
        name_in_argument: bool,
    ) -> std::process::Child {
        use crate::procguard::LockedSpawn;
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "sysload::procscan::tests::directory_use_child_holds_still_until_stdin_closes",
                "--nocapture",
            ])
            .env("GITPULSE_DIR_USE_CHILD", "1")
            .current_dir(elsewhere)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if name_in_argument {
            // Extra test filters are accepted and match nothing: the path
            // reaches the process's argument list and does nothing else.
            command.arg(scratch);
        } else {
            command.env("CARGO_TARGET_DIR", scratch);
        }
        command.spawn_locked().expect("spawn")
    }

    /// The same shape against real processes: neither child stands in the
    /// directory, so the cwd-only check finds nothing and the new one finds
    /// the child whose argument names it and the one whose environment does.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn real_processes_naming_a_directory_are_found_where_cwd_is_blind() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let scratch = root.join("scratch-target");
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(scratch.join("debug/deps")).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        let mut by_argument = directory_use_child(&scratch, &elsewhere, true);
        let mut by_environment = directory_use_child(&scratch, &elsewhere, false);
        let (argument_pid, environment_pid) = (
            i32::try_from(by_argument.id()).unwrap(),
            i32::try_from(by_environment.id()).unwrap(),
        );
        let mut found = Vec::new();
        let seen = crate::test_support::env::wait_until(std::time::Duration::from_secs(20), || {
            found = processes_using(&scratch).unwrap_or_default();
            found.contains(&argument_pid) && found.contains(&environment_pid)
        });
        let blind = working_in(&scratch).expect("scan");
        let detail = dir_use(&scratch).expect("scan").users;
        for child in [&mut by_argument, &mut by_environment] {
            drop(child.stdin.take());
            let _ = child.kill();
            let _ = child.wait();
        }
        assert!(
            blind.is_empty(),
            "the cwd-only check sees no process: {blind:?}"
        );
        assert!(
            seen,
            "{argument_pid} (argument) and {environment_pid} (environment) not both in {found:?}"
        );
        assert!(
            detail.contains(&(argument_pid, UseKind::Argument)),
            "{detail:?}"
        );
        assert!(
            detail.contains(&(environment_pid, UseKind::Environment)),
            "{detail:?}"
        );
    }

    /// A refusal before the dialog never shows it.
    #[test]
    fn a_process_that_is_not_an_orphan_never_reaches_the_dialog() {
        let asked = std::cell::Cell::new(false);
        let refused = terminate_orphan(1, 7, &|_, _| {
            asked.set(true);
            true
        })
        .expect_err("pid 1");
        assert!(
            refused.contains("not one GitPulse will terminate"),
            "{refused}"
        );
        assert!(!asked.get());
    }

    /// Set on a re-executed test binary to make it a fixture process instead
    /// of a test run: `sleep=<s>[,alloc_mib=<n>][,pidfile=<path>]`.
    pub(crate) const FIXTURE_ENV: &str = "GITPULSE_SYSLOAD_FIXTURE";
    const FIXTURE_TEST: &str = "sysload::procscan::tests::fixture_process";
    /// The ceiling the card sets for any real allocation in this series.
    const FIXTURE_MAX_ALLOC_MIB: u64 = 512;

    /// The fixture's own body. Under a normal test run (no [`FIXTURE_ENV`])
    /// it returns at once.
    #[test]
    fn fixture_process() {
        let Ok(spec) = std::env::var(FIXTURE_ENV) else {
            return;
        };
        let field = |name: &str| {
            spec.split(',')
                .find_map(|kv| kv.strip_prefix(name)?.strip_prefix('='))
                .map(str::to_owned)
        };
        let sleep_s: u64 = field("sleep")
            .and_then(|v| v.parse().ok())
            .unwrap_or(30)
            .min(120);
        let alloc_mib: u64 = field("alloc_mib")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
            .min(FIXTURE_MAX_ALLOC_MIB);
        // Written, not just reserved: the footprint counts dirty pages.
        let held = vec![1u8; usize::try_from(alloc_mib << 20).unwrap_or(0)];
        if let Some(pidfile) = field("pidfile") {
            let tmp = format!("{pidfile}.tmp");
            std::fs::write(&tmp, std::process::id().to_string()).unwrap();
            std::fs::rename(&tmp, &pidfile).unwrap();
        }
        std::thread::sleep(std::time::Duration::from_secs(sleep_s));
        std::hint::black_box(&held);
    }

    /// The test binary and the arguments that make it run only
    /// [`fixture_process`].
    pub(crate) fn fixture_command() -> (PathBuf, Vec<String>) {
        (
            std::env::current_exe().unwrap(),
            [
                "--exact",
                FIXTURE_TEST,
                "--nocapture",
                "--test-threads=1",
                "-q",
            ]
            .map(String::from)
            .to_vec(),
        )
    }

    /// Waits for a fixture's pidfile and returns the pid in it.
    pub(crate) fn fixture_pid(pidfile: &Path) -> i32 {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            if let Some(pid) = std::fs::read_to_string(pidfile)
                .ok()
                .and_then(|t| t.trim().parse().ok())
            {
                return pid;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("the fixture never wrote {}", pidfile.display());
    }

    #[cfg(unix)]
    pub(crate) fn kill_fixture(pid: i32) {
        // 0, 1 and negatives address more than one process.
        assert!(pid > 1, "refusing to signal pid {pid}");
        // SAFETY: a pid this test started and recorded itself.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }

    /// A process table written out by hand.
    pub(crate) struct Fixture {
        pub facts: Arc<Mutex<Vec<ProcFacts>>>,
        pub cwds: HashMap<i32, PathBuf>,
        pub fds: HashMap<(i32, i32), String>,
        pub fail: Option<String>,
    }

    impl ProcSource for Fixture {
        fn read(&mut self, out: &mut Vec<ProcFacts>) -> Result<(), String> {
            if let Some(why) = &self.fail {
                return Err(why.clone());
            }
            out.clear();
            out.extend(self.facts.lock().unwrap().iter().cloned());
            Ok(())
        }
        fn cwd(&self, pid: i32) -> Option<PathBuf> {
            self.cwds.get(&pid).cloned()
        }
        fn fd_path(&self, pid: i32, fd: i32) -> Reading<String> {
            self.fds
                .get(&(pid, fd))
                .cloned()
                .map_or(Reading::Unavailable(Why::Unreadable), Reading::Value)
        }
        fn me_uid(&self) -> u32 {
            ME
        }
    }

    pub(crate) struct P {
        pub pid: i32,
        pub ppid: i32,
        pub uid: u32,
        pub exec: &'static str,
        pub env: &'static [&'static str],
        pub footprint: Reading<u64>,
        pub written: u64,
        pub start_us: u64,
        pub cpu_ns: u64,
    }

    pub(crate) fn p(pid: i32, ppid: i32, exec: &'static str) -> P {
        P {
            pid,
            ppid,
            uid: ME,
            exec,
            env: &[],
            footprint: Reading::Value(100),
            written: 0,
            start_us: 1_000,
            cpu_ns: 0,
        }
    }

    pub(crate) fn facts_of(rows: Vec<P>) -> Vec<ProcFacts> {
        rows.into_iter()
            .map(|r| {
                let name = Path::new(r.exec)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let readable = r.uid == ME;
                ProcFacts {
                    pid: r.pid,
                    ppid: r.ppid,
                    uid: r.uid,
                    start_us: r.start_us,
                    ident: Arc::new(Identity::new(
                        name,
                        readable.then(|| r.exec.to_string()),
                        vec![r.exec.to_string()],
                        LaunchKey::from_env(r.env.iter().copied()),
                    )),
                    footprint: r.footprint,
                    written: Reading::Value(r.written),
                    cpu_ns: Reading::Value(r.cpu_ns),
                }
            })
            .collect()
    }

    pub(crate) fn scanner(rows: Vec<P>) -> (Scanner, Arc<Mutex<Vec<ProcFacts>>>) {
        let facts = Arc::new(Mutex::new(facts_of(rows)));
        (
            Scanner::new(Box::new(Fixture {
                facts: facts.clone(),
                cwds: HashMap::new(),
                fds: HashMap::new(),
                fail: None,
            })),
            facts,
        )
    }

    pub(crate) fn ctx(now_us: u64) -> ScanContext {
        ScanContext {
            me_pid: 10,
            now_us,
            home: Some(PathBuf::from("/Users/me")),
            governed: true,
        }
    }

    const CLAUDE: &str = "/Users/me/.local/share/claude/versions/2.1.296";

    #[test]
    fn agent_executables_are_recognised_by_path_and_runtime_script() {
        for (exec, arg1, want) in [
            (CLAUDE, None, Some(AgentKind::Claude)),
            ("/Users/me/.local/bin/claude", None, Some(AgentKind::Claude)),
            (
                "/Users/me/Library/Application Support/Claude/claude-code/2.1.293/x/claude.app/Contents/MacOS/claude",
                None,
                Some(AgentKind::Claude),
            ),
            (
                "/opt/homebrew/bin/node",
                Some("/opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/cli.js"),
                Some(AgentKind::Claude),
            ),
            (
                "/Users/me/.codex/packages/standalone/current/bin/codex",
                None,
                Some(AgentKind::Codex),
            ),
            (
                "/usr/local/bin/node",
                Some("/usr/local/lib/node_modules/@openai/codex/bin/codex.js"),
                Some(AgentKind::Codex),
            ),
            ("/usr/local/bin/antigravity", None, Some(AgentKind::Antigravity)),
            ("/usr/local/bin/devcouncil", None, Some(AgentKind::DevCouncil)),
            // `dev` is a generic name (Jarvis ships a `dev` CLI too); only
            // DevCouncil's own binary name or its npm shim script identify it.
            ("/usr/local/bin/dev", None, None),
            ("/Users/me/Code/Jarvis/build/dev", None, None),
            (
                "/opt/homebrew/bin/node",
                Some("/opt/homebrew/bin/devcouncil"),
                Some(AgentKind::DevCouncil),
            ),
            // Tooling run from inside a devcouncil checkout is not the agent.
            (
                "/opt/homebrew/bin/node",
                Some("/Users/me/Code/devcouncil/node_modules/vite/bin/vite.js"),
                None,
            ),
            (
                "/Users/me/.bun/bin/bun",
                Some("/Users/me/Code/DevCouncil/node_modules/.bin/vitest"),
                None,
            ),
            ("/usr/local/bin/manvi", None, Some(AgentKind::Manvi)),
            (
                "/opt/homebrew/bin/node",
                Some("/Users/me/Code/DevCouncil/bin/devcouncil.js"),
                Some(AgentKind::DevCouncil),
            ),
            // Not agents: the desktop app, a helper, a version directory that
            // is not Claude's, a node running something else.
            ("/Applications/Claude.app/Contents/MacOS/Claude", None, None),
            ("/Users/me/.codex/packages/standalone/current/bin/codex-code-mode-host", None, None),
            ("/opt/tool/versions/2.1.0", None, None),
            ("/opt/homebrew/bin/node", Some("/srv/app/server.js"), None),
            ("/bin/sleep", None, None),
        ] {
            assert_eq!(classify(exec, arg1), want, "{exec} {arg1:?}");
        }
    }

    const AG_APP: &str = "/Applications/Antigravity.app/Contents/MacOS/Antigravity";
    const AG_LS: &str = "/Applications/Antigravity.app/Contents/Resources/bin/language_server";

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| (*a).to_string()).collect()
    }

    /// Rows read from a live Antigravity (`ps -axo pid,ppid,comm,args`,
    /// 2026-11-06; the csrf token is dropped).
    fn ag_language_server_argv() -> Vec<String> {
        args(&[
            AG_LS,
            "--standalone",
            "--override_ide_name",
            "antigravity",
            "--subclient_type",
            "hub",
            "--override_ide_version",
            "2.29.0",
            "--override_user_agent_name",
            "antigravity",
            "--https_server_port",
            "0",
            "--app_data_dir",
            "antigravity",
            "--api_server_url",
            "https://generativelanguage.googleapis.com",
        ])
    }

    #[test]
    fn antigravity_app_and_its_language_server_are_recognised_without_false_positives() {
        let cases: Vec<(&str, Vec<String>, Option<AgentKind>)> = vec![
            (AG_APP, args(&[AG_APP]), Some(AgentKind::Antigravity)),
            (AG_LS, ag_language_server_argv(), Some(AgentKind::Antigravity)),
            (
                AG_LS,
                args(&[AG_LS, "--override_ide_name=Antigravity"]),
                Some(AgentKind::Antigravity),
            ),
            // Another IDE's language server, or one that does not say.
            (
                "/Applications/Windsurf.app/Contents/Resources/bin/language_server",
                args(&["language_server", "--override_ide_name", "windsurf"]),
                None,
            ),
            (AG_LS, args(&[AG_LS, "--standalone"]), None),
            (AG_LS, args(&[AG_LS, "--override_ide_name"]), None),
            (AG_LS, args(&[AG_LS, "--override_ide_name", "antigravity-not"]), None),
            // The Electron helper is not a session.
            (
                "/Applications/Antigravity.app/Contents/Frameworks/Antigravity Helper.app/Contents/MacOS/Antigravity Helper",
                args(&["Antigravity Helper", "--import=file:///x/register_loader.js"]),
                None,
            ),
        ];
        for (exec, argv, want) in cases {
            assert_eq!(classify_argv(exec, &argv), want, "{exec} {argv:?}");
        }
        assert!(name_selects_procargs("Antigravity"));
        assert!(name_selects_procargs("agy"));
        assert!(name_selects_procargs("language_server"));
        assert!(!name_selects_procargs("Antigravity Helper"));
        assert!(path_selects_procargs(AG_LS));
        assert!(path_selects_procargs("/Users/me/.local/bin/agy"));
        assert_eq!(
            classify("/Users/me/.local/bin/agy", None),
            Some(AgentKind::Antigravity)
        );
        assert!(is_exec_ide_hosted(AG_APP, AgentKind::Antigravity));
        assert!(is_exec_ide_hosted(AG_LS, AgentKind::Antigravity));
        assert!(!is_exec_ide_hosted(
            "/Users/me/.local/bin/agy",
            AgentKind::Antigravity
        ));
        // Without readable arguments the flag cannot be checked.
        let row = session_row(7, 1, 4, SessionProbe::ArgsOf(AG_LS.into()), None, || None);
        assert!(row.is_none());
    }

    #[test]
    fn agy_cli_session_keeps_its_own_working_directory_without_ide_descendant_reattribution() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("Code");
        std::fs::create_dir_all(&home).unwrap();
        let repo = checkout(tmp.path(), "OtherRepo");
        let agy_row = session_row(
            42,
            1,
            10,
            SessionProbe::Args,
            Some((
                "/Users/me/.local/bin/agy".into(),
                args(&["agy", "--resume", "conv-1"]),
                LaunchKey::from_env(["GITPULSE_SESSION_ID=term-agy"]),
            )),
            || None,
        )
        .expect("agy CLI classified");
        assert_eq!(agy_row.kind, AgentKind::Antigravity);
        assert!(!agy_row.ide_hosted);
        let mut table = SessionTable::from(vec![agy_row]);
        let mut fx = ProcFixture::default();
        fx.cwds.insert(42, home.clone());
        fx.add(
            &mut table,
            43,
            42,
            repo.to_str().unwrap(),
            &["git", "status"],
        );
        let sessions = sessions_for(table, &fx);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].kind, AgentKind::Antigravity);
        assert_eq!(sessions[0].kind.name(), "antigravity");
        assert_eq!(sessions[0].cwd.as_deref(), home.to_str());
        assert_eq!(sessions[0].tree.as_deref(), Some("session:term-agy"));
    }

    /// A checkout fixture: `<tmp>/<name>/.git`.
    fn checkout(parent: &Path, name: &str) -> PathBuf {
        let root = parent.join(name);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("src-tauri")).unwrap();
        root
    }

    /// A process table fixture: the rows' working directories and arguments.
    #[derive(Default)]
    struct ProcFixture {
        cwds: HashMap<i32, PathBuf>,
        argvs: HashMap<i32, Vec<String>>,
        cwd_reads: std::cell::RefCell<HashMap<i32, u32>>,
    }

    impl ProcFixture {
        fn add(&mut self, table: &mut SessionTable, pid: i32, ppid: i32, cwd: &str, argv: &[&str]) {
            table.links.insert(
                pid,
                ProcLink {
                    ppid,
                    start_us: u64::try_from(pid).unwrap(),
                },
            );
            self.cwds.insert(pid, PathBuf::from(cwd));
            self.argvs.insert(pid, args(argv));
        }

        fn cwd(&self, pid: i32) -> Option<PathBuf> {
            *self.cwd_reads.borrow_mut().entry(pid).or_default() += 1;
            self.cwds.get(&pid).cloned()
        }
    }

    /// The sessions the scan reports for `table`, with each working directory
    /// read through `fixture`.
    fn sessions_for(mut table: SessionTable, fixture: &ProcFixture) -> Vec<AgentSession> {
        table.cwds = session_cwds(
            &table.rows,
            &table.links,
            &|pid| fixture.cwd(pid),
            &mut |pid| fixture.argvs.get(&pid).cloned(),
        );
        sessions_from_read(Ok(table), |pid| fixture.cwd(pid)).unwrap()
    }

    fn canon(path: &Path) -> String {
        path.canonicalize().unwrap().to_string_lossy().into_owned()
    }

    /// The live Antigravity's table (`rows.txt`): the app and its language
    /// server run from `/`, over MCP servers and Electron helpers. `devmap` is
    /// pointed at DevCouncil, `devcouncil mcp-server` runs in DevCouncil, and
    /// `gitpulse-mcp` has no arguments.
    fn real_antigravity_table(tmp: &Path) -> (SessionTable, ProcFixture, PathBuf, PathBuf) {
        let repo = checkout(tmp, "GitPulse");
        let other = checkout(tmp, "DevCouncil");
        let other_root = other.to_string_lossy().into_owned();
        let mut table = SessionTable::from(vec![
            agent_row(60496, 1, 100, AgentKind::Antigravity),
            agent_row(60567, 60496, 101, AgentKind::Antigravity),
        ]);
        let mut fx = ProcFixture::default();
        fx.cwds.insert(60496, PathBuf::from("/"));
        fx.cwds.insert(60567, PathBuf::from("/"));
        let ide = "/Applications/Antigravity.app/Contents/Frameworks/Antigravity Helper.app/Contents/MacOS/Antigravity Helper";
        fx.add(
            &mut table,
            30881,
            60567,
            "/",
            &["devmap", "--root", &other_root, "mcp"],
        );
        fx.add(
            &mut table,
            30882,
            60567,
            &other_root,
            &["devcouncil", "mcp-server"],
        );
        fx.add(
            &mut table,
            60582,
            60567,
            "/",
            &["/Users/bharath/.cargo/bin/gitpulse-mcp"],
        );
        fx.add(
            &mut table,
            60584,
            60567,
            "/",
            &["node", "mcp/dist/index.js", "--mode=notebook"],
        );
        fx.add(
            &mut table,
            60586,
            60567,
            "/",
            &["npm exec firebase-tools@latest mcp"],
        );
        fx.add(
            &mut table,
            60587,
            60567,
            "/",
            &["npm exec chrome-devtools-mcp@latest"],
        );
        fx.add(
            &mut table,
            60641,
            60567,
            "/",
            &[ide, "../launch.mjs", "compose-preview"],
        );
        (table, fx, repo, other)
    }

    #[test]
    fn ambient_tool_servers_do_not_attribute_a_session_to_their_repository() {
        let tmp = tempfile::tempdir().unwrap();
        let (table, fx, _repo, _other) = real_antigravity_table(tmp.path());
        let sessions = sessions_for(table, &fx);
        assert_eq!(sessions.len(), 1, "{sessions:?}");
        assert_eq!(sessions[0].cwd.as_deref(), Some("/"));
        // Still counted by the facet, with an unknown worktree.
        let kinds = crate::insights::live_sessions::from_scan(&sessions, &[]);
        let antigravity = kinds.iter().find(|k| k.kind == "antigravity").unwrap();
        assert_eq!(antigravity.live.len(), 1);
    }

    #[test]
    fn a_work_descendant_names_the_repository_among_ambient_tool_servers() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut table, mut fx, repo, _other) = real_antigravity_table(tmp.path());
        let manifest = repo.join("src-tauri/Cargo.toml");
        std::fs::write(&manifest, "").unwrap();
        let manifest = manifest.to_string_lossy().into_owned();
        fx.add(
            &mut table,
            61000,
            60567,
            "/",
            &["cargo", "build", "--release", "--manifest-path", &manifest],
        );
        let sessions = sessions_for(table, &fx);
        assert_eq!(sessions.len(), 1, "{sessions:?}");
        assert_eq!(sessions[0].pid, 60496);
        assert_eq!(sessions[0].cwd.as_deref(), Some(canon(&repo).as_str()));
        let kinds = crate::insights::live_sessions::from_scan(&sessions, &[]);
        let antigravity = kinds.iter().find(|k| k.kind == "antigravity").unwrap();
        assert_eq!(antigravity.live.len(), 1);
    }

    #[test]
    fn work_descendants_in_two_repositories_leave_the_worktree_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut table, mut fx, repo, other) = real_antigravity_table(tmp.path());
        let manifest = repo.join("src-tauri/Cargo.toml");
        std::fs::write(&manifest, "").unwrap();
        let manifest = manifest.to_string_lossy().into_owned();
        fx.add(
            &mut table,
            61000,
            60567,
            "/",
            &["cargo", "build", "--manifest-path", &manifest],
        );
        fx.add(
            &mut table,
            61001,
            60567,
            other.to_str().unwrap(),
            &["git", "status"],
        );
        let sessions = sessions_for(table, &fx);
        assert_eq!(sessions[0].cwd.as_deref(), Some("/"));
    }

    #[test]
    fn a_descendant_whose_arguments_could_not_be_read_is_no_evidence() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut table, mut fx, repo, _other) = real_antigravity_table(tmp.path());
        fx.add(&mut table, 61000, 60567, repo.to_str().unwrap(), &["git"]);
        fx.argvs.remove(&61000);
        assert_eq!(sessions_for(table, &fx)[0].cwd.as_deref(), Some("/"));
    }

    #[test]
    fn only_ide_hosted_kinds_are_attributed_from_their_descendants() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("Code");
        std::fs::create_dir_all(&home).unwrap();
        let repo = checkout(tmp.path(), "X");
        for kind in [
            AgentKind::Claude,
            AgentKind::Codex,
            AgentKind::DevCouncil,
            AgentKind::Manvi,
        ] {
            let mut table = SessionTable::from(vec![agent_row(10, 1, 1, kind)]);
            let mut fx = ProcFixture::default();
            fx.cwds.insert(10, home.clone());
            fx.add(
                &mut table,
                11,
                10,
                repo.to_str().unwrap(),
                &["git", "status"],
            );
            let sessions = sessions_for(table, &fx);
            assert_eq!(sessions[0].cwd.as_deref(), home.to_str(), "{kind:?}");
        }
    }

    // Creating the link needs a privilege on Windows.
    #[cfg(unix)]
    #[test]
    fn repository_roots_are_canonical_so_spellings_of_one_checkout_agree() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = checkout(tmp.path(), "R1");
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&repo, &link).unwrap();
        let mut table = SessionTable::from(vec![agent_row(5, 1, 1, AgentKind::Antigravity)]);
        let mut fx = ProcFixture::default();
        fx.cwds.insert(5, PathBuf::from("/"));
        let direct = repo.join("src-tauri/Cargo.toml");
        std::fs::write(&direct, "").unwrap();
        let via_link = link
            .join("src-tauri/Cargo.toml")
            .to_string_lossy()
            .into_owned();
        let dotdot = repo
            .join("src-tauri/../src-tauri/Cargo.toml")
            .to_string_lossy()
            .into_owned();
        fx.add(
            &mut table,
            6,
            5,
            "/",
            &["cargo", "build", "--manifest-path", &via_link],
        );
        fx.add(
            &mut table,
            7,
            5,
            "/",
            &["cargo", "check", "--manifest-path", &dotdot],
        );
        fx.add(
            &mut table,
            8,
            5,
            "/",
            &["cargo", "test", "--manifest-path", direct.to_str().unwrap()],
        );
        let sessions = sessions_for(table, &fx);
        assert_eq!(sessions[0].cwd.as_deref(), Some(canon(&repo).as_str()));
        // A path that does not exist cannot be canonicalised: no evidence.
        assert_eq!(repo_root_of(&repo.join("missing/file")), None);
    }

    #[test]
    fn a_session_with_no_repository_evidence_keeps_its_own_directory_and_is_never_dropped() {
        let table = SessionTable::from(vec![agent_row(5, 1, 1, AgentKind::Antigravity)]);
        let mut fx = ProcFixture::default();
        fx.cwds.insert(5, PathBuf::from("/"));
        let sessions = sessions_for(table, &fx);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].cwd.as_deref(), Some("/"));
    }

    #[test]
    fn a_session_already_in_a_repository_is_not_reattributed() {
        let tmp = tempfile::tempdir().unwrap();
        let own = checkout(tmp.path(), "own");
        let named = checkout(tmp.path(), "named");
        let mut table = SessionTable::from(vec![agent_row(5, 1, 1, AgentKind::Antigravity)]);
        let mut fx = ProcFixture::default();
        fx.cwds.insert(5, own.clone());
        fx.add(
            &mut table,
            6,
            5,
            named.to_str().unwrap(),
            &["git", "status"],
        );
        let sessions = sessions_for(table, &fx);
        assert_eq!(sessions[0].cwd.as_deref(), own.to_str());
    }

    #[test]
    fn each_session_directory_is_read_once() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut table, mut fx, repo, _other) = real_antigravity_table(tmp.path());
        fx.add(
            &mut table,
            61000,
            60567,
            repo.to_str().unwrap(),
            &["git", "status"],
        );
        sessions_for(table, &fx);
        let reads = fx.cwd_reads.borrow();
        for pid in [60496, 60567] {
            assert_eq!(reads.get(&pid), Some(&1), "pid {pid}");
        }
    }

    #[test]
    fn repository_hints_are_bounded() {
        let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
        // A chain deeper than the bound, and a fan-out wider than it.
        for pid in 1..40 {
            children.entry(pid).or_default().push(pid + 1);
        }
        assert_eq!(descendants_of(1, &children).len(), MAX_HINT_DEPTH);
        children.insert(1000, (2000..2500).collect());
        assert_eq!(descendants_of(1000, &children).len(), MAX_HINT_DESCENDANTS);
        // A cycle terminates.
        children.insert(7000, vec![7001]);
        children.insert(7001, vec![7000, 7001]);
        assert_eq!(descendants_of(7000, &children), [7001]);
        // Past the argument cap nothing is read.
        let mut many = vec!["x"; MAX_HINT_ARGS];
        many.extend(["--root", "/late"]);
        assert!(hint_paths(&args(&many)).is_empty());
        assert_eq!(
            hint_paths(&args(&[
                "--root=/a",
                "--manifest-path",
                "/b/Cargo.toml",
                "--root"
            ])),
            [PathBuf::from("/a"), PathBuf::from("/b/Cargo.toml")]
        );
    }

    #[test]
    fn an_oversized_argument_is_dropped_after_pairing_not_before() {
        let huge = "a".repeat(MAX_HINT_ARG_BYTES + 1);
        assert_eq!(
            hint_paths(&args(&["--root", &huge, "--repo", "/q"])),
            [PathBuf::from("/q")]
        );
        assert!(hint_paths(&args(&["--root", &huge])).is_empty());
    }

    #[test]
    fn argument_reads_are_capped_while_they_are_materialised() {
        let mut raw = Vec::new();
        for i in 0..(MAX_HINT_ARGS + 10) {
            raw.extend(format!("a{i}").as_bytes());
            raw.push(0);
        }
        raw.extend(vec![b'z'; MAX_HINT_ARG_BYTES + 1]);
        raw.push(0);
        let argv = split_cmdline_within(&raw, Some((MAX_HINT_ARGS, MAX_HINT_ARG_BYTES)));
        assert_eq!(argv.len(), MAX_HINT_ARGS);
        let big = [
            b"--root\0".as_slice(),
            &vec![b'z'; MAX_HINT_ARG_BYTES + 1],
            b"\0--repo\0/q\0",
        ]
        .concat();
        let argv = split_cmdline_within(&big, Some((MAX_HINT_ARGS, MAX_HINT_ARG_BYTES)));
        assert_eq!(argv, ["--root", "", "--repo", "/q"]);
        assert_eq!(split_cmdline_within(&big, None).len(), 4);
    }

    #[test]
    fn tool_servers_are_told_by_an_mcp_word_not_by_a_name_list() {
        let server = |a: &[&str]| is_tool_server(&args(a));
        assert!(server(&["devmap", "--root", "/r", "mcp"]));
        assert!(server(&["devcouncil", "mcp-server"]));
        assert!(server(&["/Users/x/.cargo/bin/gitpulse-mcp"]));
        assert!(server(&["node", "mcp/dist/index.js", "--mode=notebook"]));
        assert!(server(&["node", "/x/scholarlm-mcp.mjs"]));
        assert!(server(&["npm exec chrome-devtools-mcp@latest"]));
        assert!(!server(&[
            "cargo",
            "build",
            "--manifest-path",
            "/Users/x/mcp/Cargo.toml"
        ]));
        assert!(!server(&["git", "status"]));
        assert!(!server(&["/bin/zsh", "-l"]));
        assert!(!server(&["rustc", "--crate-name", "mcpish"]));
    }

    #[test]
    fn argument_reads_are_limited_to_names_and_paths_that_could_be_agents() {
        assert!(name_selects_procargs("claude"));
        assert!(name_selects_procargs("codex"));
        assert!(name_selects_procargs("node"));
        assert!(name_selects_procargs("node20"));
        assert!(name_selects_procargs("bun"));
        assert!(!name_selects_procargs("Claude"));
        assert!(!name_selects_procargs("codex-code-mode-host"));
        assert!(!name_selects_procargs("git"));
        assert!(name_wants_resolved_path(""));
        assert!(name_wants_resolved_path("2.1.296"));
        assert!(name_wants_resolved_path("2.1.296-1"));
        assert!(!name_wants_resolved_path("1Password"));
        assert!(!name_wants_resolved_path("claude"));
        assert!(!name_wants_resolved_path(&"1".repeat(33)));
        assert!(path_selects_procargs(CLAUDE));
        assert!(path_selects_procargs("/opt/homebrew/bin/node"));
        assert!(path_selects_procargs("/usr/local/bin/bun"));
        assert!(!path_selects_procargs("/usr/bin/git"));
        assert!(!path_selects_procargs(
            "/Applications/Claude.app/Contents/MacOS/Claude"
        ));
        assert!(!path_selects_procargs(
            "/Users/me/.codex/packages/standalone/current/bin/codex-code-mode-host"
        ));
        assert!(matches!(
            session_probe("node", || None),
            Some(SessionProbe::Args)
        ));
        assert!(matches!(
            session_probe("2.1.296", || Some(CLAUDE.into())),
            Some(SessionProbe::ArgsOf(_))
        ));
        assert!(session_probe("git", || Some("/usr/bin/git".into())).is_none());
        assert!(session_probe("Claude", || {
            Some("/Applications/Claude.app/Contents/MacOS/Claude".into())
        })
        .is_none());
        assert!(session_probe("2.1.0", || Some("/opt/tool/versions/2.1.0".into())).is_none());
    }

    /// A scan that could not read the table is an error, never "no sessions".
    #[test]
    fn a_session_scan_that_could_not_read_the_table_is_an_error() {
        let err = sessions_from_read(Err("denied".into()), |_| None).unwrap_err();
        assert!(err.contains("denied"), "{err}");
    }

    #[test]
    fn a_wrapper_target_is_not_a_second_session_and_the_launch_id_names_the_tree() {
        let rows = vec![
            SessionRow {
                pid: 10,
                ppid: 1,
                start_us: 5,
                kind: AgentKind::Claude,
                ide_hosted: false,
                launch: LaunchKey::from_env(["GITPULSE_SESSION_ID=term-9"]),
            },
            // The native binary a shim exec'd: same kind, direct child.
            SessionRow {
                pid: 11,
                ppid: 10,
                start_us: 6,
                kind: AgentKind::Claude,
                ide_hosted: false,
                launch: None,
            },
            // A shell is not a candidate, so this Claude is its own session.
            SessionRow {
                pid: 21,
                ppid: 20,
                start_us: 8,
                kind: AgentKind::Claude,
                ide_hosted: false,
                launch: None,
            },
            SessionRow {
                pid: 30,
                ppid: 1,
                start_us: 9,
                kind: AgentKind::Codex,
                ide_hosted: false,
                launch: LaunchKey::from_env(["GITPULSE_RUN_ID=run-3"]),
            },
        ];
        let sessions = sessions_from_read(Ok(rows.into()), |pid| {
            (pid == 10).then(|| std::path::PathBuf::from("/repo"))
        })
        .unwrap();
        let pids: Vec<i32> = sessions.iter().map(|session| session.pid).collect();
        assert_eq!(pids, [10, 21, 30]);
        let launched = sessions.iter().find(|session| session.pid == 10).unwrap();
        assert_eq!(launched.tree.as_deref(), Some("session:term-9"));
        assert_eq!(launched.cwd.as_deref(), Some("/repo"));
        assert_eq!(launched.members, 1);
        assert!(matches!(
            launched.footprint,
            Reading::Unavailable(Why::Unsupported)
        ));
        assert_eq!(
            sessions
                .iter()
                .find(|session| session.pid == 30)
                .unwrap()
                .tree
                .as_deref(),
            Some("run:run-3")
        );
        assert!(sessions
            .iter()
            .find(|session| session.pid == 21)
            .unwrap()
            .cwd
            .is_none());
    }

    #[test]
    fn more_sessions_than_the_scan_will_describe_is_an_error() {
        // Parents are not candidates, so none of these is a wrapper target.
        let rows: Vec<SessionRow> = (1..=i32::try_from(MAX_SESSIONS).unwrap() + 1)
            .map(|pid| SessionRow {
                pid,
                ppid: 0,
                start_us: 1,
                kind: AgentKind::Claude,
                ide_hosted: false,
                launch: None,
            })
            .collect();
        let err = sessions_from_read(Ok(rows.into()), |_| None).unwrap_err();
        assert!(err.contains("more than"), "{err}");
    }

    fn agent_row(pid: i32, ppid: i32, start_us: u64, kind: AgentKind) -> SessionRow {
        SessionRow {
            pid,
            ppid,
            start_us,
            kind,
            ide_hosted: kind.is_ide_hosted(),
            launch: None,
        }
    }

    /// A table with the rows and the non-candidate processes between them,
    /// as `(pid, ppid, start_us)`.
    fn table_with(rows: Vec<SessionRow>, others: &[(i32, i32, u64)]) -> SessionTable {
        let mut table = SessionTable::from(rows);
        for &(pid, ppid, start_us) in others {
            table.links.insert(pid, ProcLink { ppid, start_us });
        }
        table
    }

    fn parents_of(table: SessionTable) -> Vec<(i32, Option<i32>)> {
        sessions_from_read(Ok(table), |_| None)
            .unwrap()
            .iter()
            .map(|session| (session.pid, session.parent_pid))
            .collect()
    }

    /// The lineage a flat session list throws away: a Claude started by a tool
    /// call of another Claude sits under a shell, and the shell is not an
    /// agent, so the parent is found by walking through it.
    #[test]
    fn a_session_started_under_a_shell_names_the_session_above_it() {
        let table = table_with(
            vec![
                agent_row(80, 1, 100, AgentKind::Claude),
                agent_row(100, 90, 300, AgentKind::Claude),
            ],
            &[(1, 0, 1), (90, 85, 200), (85, 80, 150)],
        );
        assert_eq!(parents_of(table), [(80, None), (100, Some(80))]);
    }

    #[test]
    fn caller_identity_picks_nearest_agent_ancestor_and_rejects_pid_reuse() {
        // Coordinator (80) -> wrapper shell (85) -> subagent (100) -> shell (110) -> server (120)
        // Sibling (200) sits under init (1).
        let table = table_with(
            vec![
                agent_row(80, 1, 100, AgentKind::Claude),
                agent_row(100, 85, 300, AgentKind::Claude),
                agent_row(200, 1, 150, AgentKind::Codex),
            ],
            &[(1, 0, 1), (85, 80, 200), (110, 100, 400), (120, 110, 500)],
        );
        let scan = scan_from_read(Ok(table), |_| None, 120, None).unwrap();
        assert_eq!(
            scan.caller,
            Some(CallerIdentity {
                pid: 100,
                start_us: 300,
            })
        );

        // PID reuse: agent 80 started *after* shell 90 (start_us 900 > 200).
        let reused = table_with(
            vec![agent_row(80, 1, 900, AgentKind::Claude)],
            &[(1, 0, 1), (90, 80, 200), (120, 90, 500)],
        );
        let scan = scan_from_read(Ok(reused), |_| None, 120, None).unwrap();
        assert_eq!(scan.caller, None);

        // Direct agent parent via fallback link (caller not in table.links) and reparented-to-init.
        let direct = table_with(vec![agent_row(80, 1, 100, AgentKind::Claude)], &[(1, 0, 1)]);
        let scan = scan_from_read(Ok(direct), |_| None, 999, Some((80, 200))).unwrap();
        assert_eq!(
            scan.caller,
            Some(CallerIdentity {
                pid: 80,
                start_us: 100,
            })
        );

        let orphan = table_with(vec![agent_row(80, 1, 100, AgentKind::Claude)], &[(1, 0, 1)]);
        let scan = scan_from_read(Ok(orphan), |_| None, 999, Some((1, 200))).unwrap();
        assert_eq!(scan.caller, None);
    }

    /// A direct same-kind child is a wrapper target and still not a second
    /// session; sessions below it belong to the session it wraps.
    #[test]
    fn a_wrapper_target_is_walked_through_and_never_reported_as_a_parent() {
        let table = table_with(
            vec![
                agent_row(10, 1, 100, AgentKind::Codex),
                agent_row(11, 10, 110, AgentKind::Codex),
                agent_row(13, 12, 130, AgentKind::Claude),
            ],
            &[(12, 11, 120)],
        );
        assert_eq!(parents_of(table), [(10, None), (13, Some(10))]);
    }

    /// A different kind directly under an agent is a session of its own, and
    /// its parent is that agent.
    #[test]
    fn a_different_kind_directly_under_an_agent_is_a_child_session() {
        let table = table_with(
            vec![
                agent_row(10, 1, 100, AgentKind::Claude),
                agent_row(11, 10, 110, AgentKind::Codex),
            ],
            &[],
        );
        assert_eq!(parents_of(table), [(10, None), (11, Some(10))]);
    }

    /// A pid the kernel gave to a newer process is not the child's parent: the
    /// real one exited and the child was reparented. Reading the stranger as
    /// the parent would attach this session to someone else's tree.
    #[test]
    fn a_parent_that_started_after_its_child_is_a_reused_pid() {
        let table = table_with(
            vec![
                agent_row(80, 1, 900, AgentKind::Claude),
                agent_row(100, 90, 300, AgentKind::Claude),
            ],
            &[(90, 80, 200)],
        );
        // 90 is older than 100 and its own parent 80 started at 900, after 90.
        assert_eq!(parents_of(table), [(80, None), (100, None)]);
        let direct = table_with(
            vec![
                agent_row(80, 1, 900, AgentKind::Codex),
                agent_row(100, 80, 300, AgentKind::Claude),
            ],
            &[],
        );
        assert_eq!(parents_of(direct), [(80, None), (100, None)]);
    }

    /// An unread start time proves nothing either way, so the link stands.
    #[test]
    fn an_unread_start_time_does_not_cut_the_chain() {
        let table = table_with(
            vec![
                agent_row(80, 1, 0, AgentKind::Claude),
                agent_row(100, 90, 300, AgentKind::Claude),
            ],
            &[(90, 80, 0)],
        );
        assert_eq!(parents_of(table), [(80, None), (100, Some(80))]);
    }

    /// Missing parents, launchd and a parent cycle that never reaches a
    /// session all end the walk with no ancestor, in bounded time.
    #[test]
    fn a_missing_parent_or_a_cycle_ends_the_walk_with_no_ancestor() {
        let table = table_with(
            vec![
                agent_row(100, 999, 300, AgentKind::Claude),
                agent_row(101, 1, 300, AgentKind::Claude),
                agent_row(102, 50, 300, AgentKind::Claude),
                agent_row(103, 52, 300, AgentKind::Claude),
            ],
            &[(1, 0, 1), (50, 51, 100), (51, 50, 100), (52, 52, 100)],
        );
        assert_eq!(
            parents_of(table),
            [(100, None), (101, None), (102, None), (103, None)]
        );
    }

    /// The sampler's scan reads the same lineage from its own table.
    #[test]
    fn the_sampler_scan_names_a_sessions_agent_ancestor_too() {
        let (mut scan, _) = scanner(vec![
            P {
                start_us: 100,
                ..p(20, 1, CLAUDE)
            },
            P {
                start_us: 200,
                ..p(21, 20, "/bin/zsh")
            },
            P {
                start_us: 300,
                ..p(22, 21, CLAUDE)
            },
            // The wrapper target of 22 is not a session, so not a parent.
            P {
                start_us: 310,
                ..p(23, 22, CLAUDE)
            },
            P {
                start_us: 400,
                ..p(24, 23, "/bin/sh")
            },
            P {
                start_us: 500,
                ..p(25, 24, CLAUDE)
            },
        ]);
        let view = scan.scan(&ctx(5_000));
        let parents: Vec<(i32, Option<i32>)> = view
            .sessions
            .iter()
            .map(|session| (session.pid, session.parent_pid))
            .collect();
        assert_eq!(
            parents,
            [(20, None), (22, Some(20)), (25, Some(22))],
            "{view:?}"
        );
    }

    #[test]
    fn unreadable_arguments_still_count_a_resolved_agent_without_a_tree() {
        let row = session_row(7, 1, 4, SessionProbe::ArgsOf(CLAUDE.into()), None, || None).unwrap();
        let sessions = sessions_from_read(Ok(vec![row].into()), |_| {
            Some(std::path::PathBuf::from("/r"))
        })
        .unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].tree.is_none());
        assert_eq!(sessions[0].kind, AgentKind::Claude);
        assert!(matches!(
            sessions[0].footprint,
            Reading::Unavailable(Why::Unsupported)
        ));
    }

    #[test]
    fn readable_arguments_that_are_not_an_agent_are_not_rescued_by_the_path() {
        let row = session_row(
            7,
            1,
            4,
            SessionProbe::ArgsOf("/opt/homebrew/bin/node".into()),
            Some((
                "/opt/homebrew/bin/node".into(),
                vec!["/opt/homebrew/bin/node".into(), "/srv/app/server.js".into()],
                None,
            )),
            || Some(CLAUDE.into()),
        );
        assert!(row.is_none());
    }

    #[test]
    fn launch_ids_come_from_the_environment_and_bad_ones_are_ignored() {
        let key = LaunchKey::from_env([
            "PATH=/bin",
            "GITPULSE_RUN_ID=run-7",
            "GITPULSE_SESSION_ID=term-1-2",
        ])
        .unwrap();
        assert_eq!(key.tree().as_deref(), Some("run:run-7"));
        let session = LaunchKey::from_env(["GITPULSE_SESSION_ID=term-1-2"]).unwrap();
        assert_eq!(session.tree().as_deref(), Some("session:term-1-2"));
        let long = format!("GITPULSE_RUN_ID={}", "x".repeat(MAX_ID_CHARS + 1));
        for bad in [
            "GITPULSE_RUN_ID=",
            "GITPULSE_RUN_ID=a b",
            long.as_str(),
            "GITPULSE_RUN_IDX=1",
        ] {
            assert_eq!(LaunchKey::from_env([bad]), None, "{bad}");
        }
    }

    /// The defect this card fixes: sessions counted by executable, launched
    /// and observed apart, one session per topmost process.
    #[test]
    fn sessions_are_counted_by_executable_launched_and_observed_apart() {
        let (mut scan, _) = scanner(vec![
            p(10, 1, "/Applications/GitPulse.app/Contents/MacOS/gitpulse"),
            // A GitPulse terminal running claude, with its MCP child.
            P {
                env: &["GITPULSE_SESSION_ID=term-10-1"],
                ..p(20, 10, "/bin/zsh")
            },
            P {
                env: &["GITPULSE_SESSION_ID=term-10-1"],
                ..p(21, 20, CLAUDE)
            },
            P {
                env: &["GITPULSE_SESSION_ID=term-10-1"],
                ..p(22, 21, "/Users/me/.cargo/bin/gitpulse-mcp")
            },
            // A task attempt.
            P {
                env: &["GITPULSE_SESSION_ID=term-10-2", "GITPULSE_RUN_ID=run-9"],
                ..p(30, 10, CLAUDE)
            },
            // A claude in some other terminal, and the binary its shim
            // exec'd as a child: one session.
            p(40, 1, "/Applications/iTerm.app/Contents/MacOS/iTerm2"),
            p(41, 40, CLAUDE),
            p(42, 41, "/Users/me/.local/bin/claude"),
            // A claude run from a shell inside that session: its own session.
            p(43, 41, "/bin/zsh"),
            p(44, 43, CLAUDE),
            // Codex outside GitPulse.
            p(
                50,
                1,
                "/Users/me/.codex/packages/standalone/current/bin/codex",
            ),
            // Another user's claude is not this user's session.
            P {
                uid: 0,
                ..p(60, 1, CLAUDE)
            },
        ]);
        let view = scan.scan(&ctx(5_000));
        assert!(view.ok);
        assert_eq!(
            view.claude,
            SessionCount {
                launched: 2,
                observed: 2
            }
        );
        assert_eq!(
            view.codex,
            SessionCount {
                launched: 0,
                observed: 1
            }
        );
        let pids: Vec<i32> = view.sessions.iter().map(|s| s.pid).collect();
        assert_eq!(pids, [21, 30, 41, 44, 50]);
        let task = view.sessions.iter().find(|s| s.pid == 30).unwrap();
        assert_eq!(task.run_id.as_deref(), Some("run-9"));
        assert_eq!(task.tree.as_deref(), Some("run:run-9"));
        // 41's subtree is itself, 42 and the shell 43; 44 is its own.
        let observed = view.sessions.iter().find(|s| s.pid == 41).unwrap();
        assert_eq!(observed.members, 3);
        assert_eq!(observed.footprint, Reading::Value(300));
    }

    /// Agents that have no per-kind field still count somewhere, so the
    /// counts add up to every session the scan found.
    #[test]
    fn sessions_of_other_kinds_are_counted_in_other() {
        let (mut scan, _) = scanner(vec![
            p(10, 1, CLAUDE),
            p(20, 1, "/usr/local/bin/manvi"),
            p(30, 1, "/usr/local/bin/antigravity"),
            p(40, 1, "/usr/local/bin/devcouncil"),
        ]);
        let view = scan.scan(&ctx(5_000));
        let total = |c: SessionCount| c.launched + c.observed;
        assert_eq!(
            total(view.claude) + total(view.codex) + total(view.other),
            u32::try_from(view.sessions.len()).unwrap()
        );
        assert_eq!(
            view.other,
            SessionCount {
                launched: 0,
                observed: 3
            }
        );
    }

    /// A launched tree keeps a child that was reparented to launchd, through
    /// the launch id in that child's own environment.
    #[test]
    fn a_reparented_child_stays_in_its_launched_tree() {
        let (mut scan, _) = scanner(vec![
            p(10, 1, "/Applications/GitPulse.app/Contents/MacOS/gitpulse"),
            P {
                env: &["GITPULSE_RUN_ID=run-1"],
                footprint: Reading::Value(1_000),
                ..p(20, 10, CLAUDE)
            },
            // Was 20's grandchild; its parent exited and launchd adopted it.
            P {
                env: &["GITPULSE_RUN_ID=run-1"],
                footprint: Reading::Value(70_000),
                ..p(25, 1, "/opt/homebrew/bin/go")
            },
            // An attached platform binary with no visible environment: the
            // ppid walk still finds the tree.
            P {
                footprint: Reading::Value(5),
                ..p(21, 20, "/bin/sh")
            },
            // A detached one is lost to it, as documented.
            P {
                footprint: Reading::Value(7),
                ..p(26, 1, "/usr/bin/grep")
            },
        ]);
        let view = scan.scan(&ctx(5_000));
        let tree = view.trees.iter().find(|t| t.tree == "run:run-1").unwrap();
        assert_eq!(tree.members, 3, "{tree:?}");
        assert_eq!(tree.footprint, Reading::Value(71_005));
        assert_eq!(view.launched_footprint, Reading::Value(71_005));
    }

    #[test]
    fn an_unreadable_footprint_is_counted_unreadable_never_zero() {
        let (mut scan, _) = scanner(vec![
            P {
                env: &["GITPULSE_RUN_ID=r"],
                footprint: Reading::Unavailable(Why::Unreadable),
                ..p(20, 1, CLAUDE)
            },
            P {
                env: &["GITPULSE_RUN_ID=s"],
                footprint: Reading::Unavailable(Why::Unreadable),
                ..p(30, 1, CLAUDE)
            },
            P {
                env: &["GITPULSE_RUN_ID=s"],
                footprint: Reading::Value(9),
                ..p(31, 30, "/bin/x")
            },
        ]);
        let view = scan.scan(&ctx(5_000));
        assert_eq!(view.unreadable, 2);
        let r = view.trees.iter().find(|t| t.tree == "run:r").unwrap();
        assert_eq!(r.footprint, Reading::Unavailable(Why::Unreadable));
        let s = view.trees.iter().find(|t| t.tree == "run:s").unwrap();
        assert_eq!((s.footprint, s.unreadable_members), (Reading::Value(9), 1));
    }

    #[test]
    fn disk_write_rates_need_a_baseline_and_count_only_new_bytes() {
        let (mut scan, facts) = scanner(vec![P {
            env: &["GITPULSE_RUN_ID=w"],
            written: 1_000_000,
            ..p(20, 1, CLAUDE)
        }]);
        let first = scan.scan(&ctx(10_000_000));
        // Bytes written before the sampler started are not a rate.
        assert_eq!(first.launched_write_rate, Reading::Unavailable(Why::NotYet));
        {
            let mut f = facts.lock().unwrap();
            f[0].written = Reading::Value(1_000_000 + 100_000_000);
            // A child born after the first tick: all its bytes are new.
            f.extend(facts_of(vec![P {
                env: &["GITPULSE_RUN_ID=w"],
                written: 20_000_000,
                start_us: 11_000_000,
                ..p(21, 20, "/bin/dd")
            }]));
        }
        let second = scan.scan(&ctx(12_000_000));
        // 120 MB over 2 s.
        assert_eq!(second.launched_write_rate, Reading::Value(60_000_000));
        assert_eq!(second.max_tree_write_rate, Reading::Value(60_000_000));
        // A process gone between ticks takes its bytes with it; a counter
        // that went down never produces a negative rate.
        facts.lock().unwrap().truncate(1);
        facts.lock().unwrap()[0].written = Reading::Value(5);
        let third = scan.scan(&ctx(14_000_000));
        assert_eq!(third.launched_write_rate, Reading::Value(0));
    }

    /// The session's write rate is the same sum its footprint is: its own
    /// process and what is attached below it, each process in one session.
    #[test]
    fn a_sessions_write_rate_counts_its_descendants_once_where_its_footprint_lands() {
        let (mut scan, facts) = scanner(vec![
            // An observed session with a shell and a build under it.
            p(20, 1, CLAUDE),
            p(21, 20, "/bin/sh"),
            p(22, 21, "/usr/bin/rustc"),
            // A Claude started in that shell is a session of its own.
            p(30, 21, CLAUDE),
            p(31, 30, "/bin/dd"),
            // A launched session: its tree and its session are the same set.
            P {
                env: &["GITPULSE_RUN_ID=w"],
                ..p(40, 1, CLAUDE)
            },
            P {
                env: &["GITPULSE_RUN_ID=w"],
                ..p(41, 40, "/bin/dd")
            },
        ]);
        let rate_of = |view: &ProcView, pid: i32| {
            view.sessions
                .iter()
                .find(|s| s.pid == pid)
                .unwrap_or_else(|| panic!("no session {pid}: {:?}", view.sessions))
                .write_rate
        };
        let first = scan.scan(&ctx(10_000_000));
        for pid in [20, 30, 40] {
            assert_eq!(rate_of(&first, pid), Reading::Unavailable(Why::NotYet));
        }
        {
            let mut f = facts.lock().unwrap();
            for (pid, bytes) in [
                (20, 10_000_000),
                (21, 20_000_000),
                (22, 30_000_000),
                (30, 40_000_000),
                (31, 50_000_000),
                (40, 8_000_000),
                (41, 12_000_000),
            ] {
                f.iter_mut().find(|r| r.pid == pid).unwrap().written = Reading::Value(bytes);
            }
            // Born after the first tick: all its bytes are new, and they are
            // the outer session's.
            f.extend(facts_of(vec![P {
                written: 4_000_000,
                start_us: 11_000_000,
                ..p(23, 20, "/usr/bin/cc")
            }]));
        }
        let second = scan.scan(&ctx(12_000_000));
        // 10 + 20 + 30 + 4 MB over 2 s; the nested session's bytes are its own.
        assert_eq!(rate_of(&second, 20), Reading::Value(32_000_000));
        assert_eq!(rate_of(&second, 30), Reading::Value(45_000_000));
        let launched = rate_of(&second, 40);
        assert_eq!(launched, Reading::Value(10_000_000));
        let tree = second.trees.iter().find(|t| t.tree == "run:w").unwrap();
        assert_eq!(tree.write_rate, launched);
        // Every written byte is in exactly one session.
        let total: u64 = second
            .sessions
            .iter()
            .map(|s| s.write_rate.value().unwrap())
            .sum();
        assert_eq!(total, 87_000_000);
        // A member gone between ticks takes its bytes with it.
        facts.lock().unwrap().retain(|r| r.pid != 22);
        let third = scan.scan(&ctx(14_000_000));
        assert_eq!(rate_of(&third, 20), Reading::Value(0));
    }

    #[test]
    fn tree_cpu_rate_is_a_delta_and_unreadable_on_first_tick() {
        let (mut scan, facts) = scanner(vec![P {
            env: &["GITPULSE_RUN_ID=c"],
            cpu_ns: 5_000_000_000,
            ..p(20, 1, CLAUDE)
        }]);
        let first = scan.scan(&ctx(10_000_000));
        // CPU used before the sampler started is not a rate.
        let tree = |view: &ProcView| {
            view.trees
                .iter()
                .find(|t| t.tree == "run:c")
                .unwrap()
                .cpu_rate
        };
        assert_eq!(tree(&first), Reading::Unavailable(Why::NotYet));
        assert_eq!(
            first.sessions[0].cpu_rate,
            Reading::Unavailable(Why::NotYet)
        );
        {
            let mut f = facts.lock().unwrap();
            f[0].cpu_ns = Reading::Value(6_000_000_000);
            // A child born after the first tick: all its CPU is new.
            f.extend(facts_of(vec![P {
                env: &["GITPULSE_RUN_ID=c"],
                cpu_ns: 500_000_000,
                start_us: 11_000_000,
                ..p(21, 20, "/usr/bin/cc")
            }]));
        }
        let second = scan.scan(&ctx(12_000_000));
        // 1.5 s of CPU over 2 s: three quarters of a core.
        assert_eq!(tree(&second), Reading::Value(750_000_000));
        assert_eq!(second.sessions[0].cpu_rate, Reading::Value(750_000_000));
        // A counter that went down is never a negative rate, and a member
        // whose CPU time cannot be read makes the sum unreadable, not zero.
        facts.lock().unwrap()[0].cpu_ns = Reading::Value(1);
        let third = scan.scan(&ctx(14_000_000));
        assert_eq!(tree(&third), Reading::Value(0));
        facts.lock().unwrap()[1].cpu_ns = Reading::Unavailable(Why::Unreadable);
        let fourth = scan.scan(&ctx(16_000_000));
        assert_eq!(tree(&fourth), Reading::Unavailable(Why::Unreadable));
        assert_eq!(
            fourth.sessions[0].cpu_rate,
            Reading::Unavailable(Why::Unreadable)
        );
    }

    #[test]
    fn foreign_top_excludes_gitpulse_and_its_trees_and_keeps_the_heaviest_five() {
        let mut rows = vec![
            P {
                footprint: Reading::Value(1 << 40),
                ..p(10, 1, "/Applications/GitPulse.app/Contents/MacOS/gitpulse")
            },
            P {
                footprint: Reading::Value(1 << 39),
                ..p(11, 10, "/usr/bin/git")
            },
            P {
                env: &["GITPULSE_RUN_ID=r"],
                footprint: Reading::Value(1 << 38),
                ..p(20, 1, CLAUDE)
            },
            P {
                footprint: Reading::Value(18 << 30),
                ..p(41, 1, CLAUDE)
            },
            P {
                uid: 0,
                footprint: Reading::Unavailable(Why::Unreadable),
                ..p(5, 1, "/kernel")
            },
        ];
        for (n, gib) in [
            (100, 1u64),
            (101, 12),
            (102, 6),
            (103, 3),
            (104, 9),
            (105, 2),
        ] {
            rows.push(P {
                footprint: Reading::Value(gib << 30),
                ..p(n, 1, "/Applications/Chrome")
            });
        }
        let (mut scan, _) = scanner(rows);
        let view = scan.scan(&ctx(5_000));
        let top: Vec<(i32, u64)> = view
            .foreign_top
            .iter()
            .map(|f| (f.pid, f.footprint >> 30))
            .collect();
        assert_eq!(top, [(41, 18), (101, 12), (104, 9), (102, 6), (103, 3)]);
        assert_eq!(view.foreign_top[0].session, Some(AgentKind::Claude));
        assert_eq!(view.largest_foreign, Reading::Value(18 << 30));
        assert_eq!(view.other_users, 1);
    }

    #[test]
    fn an_orphan_is_launchd_parented_spinning_long_in_a_checkout_and_nobodys() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        let inside = repo.path().join("src");
        std::fs::create_dir(&inside).unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let hour = 3_600 * 1_000_000u64;
        let now = 100 * hour;
        let spinning = |pid| P {
            start_us: now - hour,
            cpu_ns: hour * 1000 * 9 / 10,
            ..p(pid, 1, "/usr/bin/grep")
        };
        let facts = Arc::new(Mutex::new(facts_of(vec![
            spinning(70),
            // Too young.
            P {
                start_us: now - hour / 4,
                ..spinning(71)
            },
            // Idle.
            P {
                cpu_ns: hour * 1000 / 10,
                ..spinning(72)
            },
            // Not in a checkout.
            spinning(73),
            // Still has a parent.
            P {
                ppid: 40,
                ..spinning(74)
            },
            // In a launched tree: card 4's, not this one's.
            P {
                env: &["GITPULSE_RUN_ID=r"],
                ..spinning(75)
            },
            // memguard is never an orphan to terminate.
            P {
                exec: "/Users/me/.local/bin/memguard",
                ..spinning(76)
            },
        ])));
        let mut cwds = HashMap::new();
        for pid in [70, 71, 72, 74, 75, 76] {
            cwds.insert(pid, inside.clone());
        }
        cwds.insert(73, elsewhere.path().to_path_buf());
        let mut scan = Scanner::new(Box::new(Fixture {
            facts,
            cwds,
            fds: HashMap::new(),
            fail: None,
        }));
        let view = scan.scan(&ctx(now));
        let pids: Vec<i32> = view.orphans.iter().map(|o| o.pid).collect();
        assert_eq!(pids, [70], "{:?}", view.orphans);
        let o = &view.orphans[0];
        assert_eq!(o.age_s, 3_600);
        assert!((o.cpu_ratio - 0.9).abs() < 1e-9);
        assert_eq!(Path::new(&o.checkout), repo.path());
    }

    #[test]
    fn memguard_health_warns_on_a_log_outside_library_logs_or_its_absence() {
        let log_dir = tempfile::tempdir().unwrap();
        let log = log_dir.path().join("memguard.log");
        std::fs::write(&log, "{}\n").unwrap();
        let facts = Arc::new(Mutex::new(vec![ProcFacts {
            pid: 68214,
            ppid: 1,
            uid: ME,
            start_us: 1,
            ident: Arc::new(Identity::new(
                "memguard".into(),
                Some("/Users/me/.local/bin/memguard".into()),
                [
                    "/Users/me/.local/bin/memguard",
                    "-cap-gib",
                    "32",
                    "-heartbeat",
                    "1h",
                ]
                .map(String::from)
                .to_vec(),
                None,
            )),
            footprint: Reading::Value(1),
            written: Reading::Value(0),
            cpu_ns: Reading::Value(0),
        }]));
        let mut fds = HashMap::new();
        fds.insert((68214, 1), log.to_string_lossy().into_owned());
        let mut scan = Scanner::new(Box::new(Fixture {
            facts: facts.clone(),
            cwds: HashMap::new(),
            fds,
            fail: None,
        }));
        let now = now_us();
        let health = scan.scan(&ctx(now)).memguard;
        assert!(health.present && health.warn, "{health:?}");
        assert_eq!(health.flags, ["-cap-gib", "32", "-heartbeat", "1h"]);
        assert_eq!(health.heartbeat_interval_s, Some(3_600));
        assert!(matches!(health.heartbeat_age_s, Reading::Value(age) if age < 60));
        assert!(
            health.warnings[0].contains("outside ~/Library/Logs"),
            "{health:?}"
        );

        // Its log under ~/Library/Logs: green.
        let home = log_dir.path().to_path_buf();
        std::fs::create_dir_all(home.join("Library/Logs/memguard")).unwrap();
        let good = home.join("Library/Logs/memguard/memguard.log");
        std::fs::write(&good, "{}\n").unwrap();
        let mut fds = HashMap::new();
        fds.insert((68214, 1), good.to_string_lossy().into_owned());
        let mut scan = Scanner::new(Box::new(Fixture {
            facts: facts.clone(),
            cwds: HashMap::new(),
            fds,
            fail: None,
        }));
        let health = scan
            .scan(&ScanContext {
                home: Some(home),
                ..ctx(now)
            })
            .memguard;
        assert!(!health.warn, "{health:?}");

        // Gone: a warning, on a governed platform only.
        facts.lock().unwrap().clear();
        let mut gone = Scanner::new(Box::new(Fixture {
            facts,
            cwds: HashMap::new(),
            fds: HashMap::new(),
            fail: None,
        }));
        let absent = gone.scan(&ctx(now)).memguard;
        assert!(!absent.present && absent.warn);
        assert!(absent.warnings[0].contains("not running"));
        let ungoverned = gone
            .scan(&ScanContext {
                governed: false,
                ..ctx(now)
            })
            .memguard;
        assert!(!ungoverned.warn);
    }

    #[test]
    fn an_unreadable_table_is_a_failed_scan_not_an_empty_one() {
        let mut scan = Scanner::new(Box::new(Fixture {
            facts: Arc::new(Mutex::new(Vec::new())),
            cwds: HashMap::new(),
            fds: HashMap::new(),
            fail: Some("proc_listallpids: Operation not permitted".into()),
        }));
        let view = scan.scan(&ctx(1));
        assert!(!view.ok);
        assert_eq!(view.launched_footprint, Reading::Unavailable(Why::Failed));
        assert!(view.error.unwrap().contains("not permitted"));
    }

    #[test]
    fn a_parent_cycle_or_a_deep_chain_terminates() {
        let mut rows = vec![p(2, 3, "/bin/a"), p(3, 2, "/bin/b")];
        for pid in 100..400 {
            rows.push(p(pid, pid - 1, "/bin/c"));
        }
        rows.push(P {
            env: &["GITPULSE_RUN_ID=deep"],
            ..p(99, 1, "/bin/root")
        });
        let (mut scan, _) = scanner(rows);
        let view = scan.scan(&ctx(1));
        assert!(view.ok);
        let deep = view.trees.iter().find(|t| t.tree == "run:deep").unwrap();
        // Within MAX_DEPTH hops of the root, the memo carries the tree down.
        assert!(deep.members as usize > MAX_DEPTH, "{deep:?}");
    }

    /// Real: a fixture process carrying a run id, allocating 128 MiB (under
    /// the 512 MiB ceiling), started by a shell that exits at once — so
    /// launchd adopts it. The scan still attributes it, and its footprint, to
    /// its run's tree through its own environment.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn real_reparented_fixture_is_attributed_to_its_run_with_its_footprint() {
        use crate::procguard::LockedSpawn;
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("fixture.pid");
        let run_id = format!("run-fixture-{}", std::process::id());
        let (exe, args) = fixture_command();
        let status = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("\"$0\" \"$@\" >/dev/null 2>&1 & exit 0")
            .arg(&exe)
            .args(&args)
            .env(
                FIXTURE_ENV,
                format!("sleep=30,alloc_mib=128,pidfile={}", pidfile.display()),
            )
            .env(RUN_ID_ENV, &run_id)
            .stdin(std::process::Stdio::null())
            .status_locked()
            .unwrap();
        assert!(status.success());
        let pid = fixture_pid(&pidfile);
        let me = i32::try_from(std::process::id()).unwrap();
        // The shell that started it has exited, so the fixture was adopted:
        // by launchd on macOS, and on Linux by pid 1 or whichever subreaper
        // (`systemd --user`, a container's init) the host has. Either way it
        // is out of this test's tree, so its own environment is the only way
        // back to its run.
        let adopted = (0..100).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            probe_one(pid).is_some_and(|f| {
                if cfg!(target_os = "macos") {
                    f.ppid == 1
                } else {
                    f.ppid > 0 && f.ppid != me
                }
            })
        });
        let mut scanner = Scanner::os();
        let view = scanner.scan(&ScanContext::here());
        kill_fixture(pid);
        assert!(adopted, "the fixture's starting shell never let go of it");
        let tree = view
            .trees
            .iter()
            .find(|t| t.run_id.as_deref() == Some(run_id.as_str()))
            .unwrap_or_else(|| panic!("no tree for {run_id}: {:?}", view.trees));
        assert_eq!(tree.members, 1, "{tree:?}");
        assert!(
            matches!(tree.footprint, Reading::Value(bytes) if bytes >= 128 << 20),
            "{tree:?}"
        );
    }

    /// Real: the scan describes every process the table lists, other users'
    /// included. `PROC_PIDTBSDINFO` refuses those with `EPERM`; before the
    /// short-info fallback a quarter of this host's table (232 of 943) was
    /// dropped and "other users" read 0, an undercount reported as complete.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn real_scan_counts_other_users_processes_instead_of_dropping_them() {
        let mut listed = Vec::new();
        os::list_pids(&mut listed).unwrap();
        let view = Scanner::os().scan(&ScanContext::here());
        assert!(view.ok, "{:?}", view.error);
        // launchd / init, pid 1, is root's on every host this runs on.
        assert!(view.other_users >= 1, "{view:?}");
        // Processes come and go between the two reads; a whole class of them
        // missing does not.
        let floor = listed.len() * 9 / 10;
        assert!(
            view.processes as usize >= floor,
            "described {} of {} listed",
            view.processes,
            listed.len()
        );
    }

    /// A recycled pid: the same number, a later start. Its identity is read
    /// again, never inherited — so a pid a dead Claude held does not keep
    /// counting as a session once `sleep` (or anything else) is given it.
    #[test]
    fn a_recycled_pid_never_inherits_the_cached_identity() {
        let mut cache = IdentityCache::new();
        let reads = std::cell::Cell::new(0);
        let read = |exec: &str| {
            reads.set(reads.get() + 1);
            Identity::new("x".into(), Some(exec.into()), Vec::new(), None)
        };
        let first = cached_identity(&mut cache, 77, 1_000, || read(CLAUDE));
        assert_eq!(first.kind, Some(AgentKind::Claude));
        // The same process, next tick: read once, reused.
        let again = cached_identity(&mut cache, 77, 1_000, || read("/bin/sleep"));
        assert_eq!(again.kind, Some(AgentKind::Claude));
        assert_eq!(reads.get(), 1);
        // The pid recycled to a process that started later.
        let reused = cached_identity(&mut cache, 77, 2_000, || read("/bin/sleep"));
        assert_eq!(reused.kind, None);
        assert_eq!(reads.get(), 2);
        // No start time to compare: never trusted from the cache.
        cached_identity(&mut cache, 88, 0, || read(CLAUDE));
        cached_identity(&mut cache, 88, 0, || read(CLAUDE));
        assert_eq!(reads.get(), 4);
    }

    #[test]
    fn go_durations_parse() {
        assert_eq!(go_duration_s("1h"), Some(3_600));
        assert_eq!(go_duration_s("2m30s"), Some(150));
        assert_eq!(go_duration_s("10s"), Some(10));
        assert_eq!(go_duration_s("1.5h"), None);
        assert_eq!(go_duration_s("0s"), None);
        assert_eq!(go_duration_s("h"), None);
        assert_eq!(go_duration_s("12"), None);
    }

    /// The signal guard reads the table through the same attribution the
    /// sampler uses: a hosted agent is hosted, a foreign one is not, a hosted
    /// child under it is in the tree, and arguments are read only for this
    /// user's processes.
    #[test]
    fn the_signal_table_marks_hosted_agents_and_reads_arguments_for_this_user_only() {
        let (mut scan, _) = scanner(vec![
            P {
                env: &["GITPULSE_SESSION_ID=s-1"],
                ..p(20, 1, CLAUDE)
            },
            p(21, 20, "/bin/sleep"),
            p(30, 1, CLAUDE),
            P {
                uid: 0,
                ..p(40, 1, "/usr/libexec/logd")
            },
        ]);
        let table = scan
            .table(|pid| Some(vec![format!("argv-of-{pid}")]))
            .expect("the fixture table reads");
        let row = |pid: i32| table.rows.iter().find(|r| r.pid == pid).unwrap();
        assert_eq!(table.me_uid, ME);
        assert!(row(20).hosted && row(20).agent == Some(AgentKind::Claude));
        assert!(
            row(21).hosted && row(21).agent.is_none(),
            "a child in the tree"
        );
        assert!(!row(30).hosted && row(30).agent == Some(AgentKind::Claude));
        assert_eq!(
            row(20).argv.as_deref(),
            Some(&["argv-of-20".to_string()][..])
        );
        assert!(
            row(40).argv.is_none(),
            "another user's arguments are not read, and so not claimed"
        );
        assert_eq!(row(40).uid, 0);
    }

    #[test]
    fn a_table_that_could_not_be_read_is_an_error_never_an_empty_table() {
        let mut scan = Scanner::new(Box::new(Fixture {
            facts: Arc::new(Mutex::new(Vec::new())),
            cwds: HashMap::new(),
            fds: HashMap::new(),
            fail: Some("the kernel said no".into()),
        }));
        let error = scan.table(|_| None).expect_err("a refused read");
        assert!(error.contains("the kernel said no"), "{error}");
    }

    #[test]
    fn the_uid_line_of_proc_status_gives_real_effective_and_saved() {
        let status = "Name:\tssh-agent\nUmask:\t0077\nState:\tS (sleeping)\n\
                      Uid:\t1001\t1002\t1003\t1002\nGid:\t1001\t1001\t1001\t1001\n";
        assert_eq!(
            parse_status_uids(status),
            Some(Creds {
                real: 1001,
                effective: 1002,
                saved: 1003
            })
        );
        for bad in [
            "",
            "Name:\tx\n",
            "Uid:\n",
            "Uid:\t1\t2\n",
            "Uid:\t1\tx\t3\t4\n",
            "Uid:\t-1\t2\t3\t4\n",
        ] {
            assert_eq!(parse_status_uids(bad), None, "{bad:?}");
        }
    }

    /// A source that reports credentials, and the executable the kernel runs,
    /// for some pids, and that can say its sender is root.
    struct WithCreds {
        inner: Fixture,
        creds: HashMap<i32, Creds>,
        exes: HashMap<i32, String>,
        root: bool,
        /// Reports the executable the kernel runs (Linux), so an unreadable
        /// one is an unknown identity.
        reports_exe: bool,
    }

    impl ProcSource for WithCreds {
        fn read(&mut self, out: &mut Vec<ProcFacts>) -> Result<(), String> {
            self.inner.read(out)
        }
        fn cwd(&self, pid: i32) -> Option<PathBuf> {
            self.inner.cwd(pid)
        }
        fn fd_path(&self, pid: i32, fd: i32) -> Reading<String> {
            self.inner.fd_path(pid, fd)
        }
        fn me_uid(&self) -> u32 {
            self.inner.me_uid()
        }
        fn creds(&self, pid: i32) -> Option<Creds> {
            self.creds.get(&pid).copied()
        }
        fn real_exe(&self, pid: i32) -> Option<String> {
            self.exes.get(&pid).cloned()
        }
        fn reports_real_exe(&self) -> bool {
            self.reports_exe
        }
        fn is_root(&self) -> bool {
            self.root
        }
    }

    /// The kernel makes `/proc/<pid>` root's for a process that is not
    /// dumpable, so a scan sees this user's own process as root's. The signal
    /// table takes the ids from the credentials instead, and reads the
    /// arguments of a process that is this user's by them.
    #[test]
    fn the_signal_table_takes_uids_from_credentials_not_from_the_proc_directory_owner() {
        let facts = Arc::new(Mutex::new(facts_of(vec![
            P {
                uid: 0,
                ..p(50, 1, "/usr/bin/ssh-agent")
            },
            P {
                uid: 0,
                ..p(51, 1, "/usr/libexec/logd")
            },
            p(52, 1, "/bin/sleep"),
        ])));
        let mut scan = Scanner::new(Box::new(WithCreds {
            inner: Fixture {
                facts,
                cwds: HashMap::new(),
                fds: HashMap::new(),
                fail: None,
            },
            creds: HashMap::from([
                (
                    50,
                    Creds {
                        real: ME,
                        effective: ME,
                        saved: ME,
                    },
                ),
                (
                    51,
                    Creds {
                        real: 0,
                        effective: 0,
                        saved: 0,
                    },
                ),
                (
                    52,
                    Creds {
                        real: ME,
                        effective: 7,
                        saved: 9,
                    },
                ),
            ]),
            exes: HashMap::new(),
            root: false,
            reports_exe: false,
        }));
        let table = scan
            .table(|pid| Some(vec![format!("argv-of-{pid}")]))
            .expect("the fixture table reads");
        let row = |pid: i32| table.rows.iter().find(|r| r.pid == pid).unwrap();
        assert_eq!(row(50).uid, ME, "the owner of /proc/50 was root's");
        assert_eq!((row(50).ruid, row(50).suid), (Some(ME), Some(ME)));
        assert!(
            row(50).argv.is_some(),
            "this user's, so its arguments are read"
        );
        assert_eq!(row(51).uid, 0);
        assert!(row(51).argv.is_none(), "root's stay unread");
        assert_eq!(
            (row(52).uid, row(52).ruid, row(52).suid),
            (7, Some(ME), Some(9)),
            "uid is the effective id; the real and saved ids are kept apart"
        );
        assert!(
            row(52).argv.is_some(),
            "a real uid of this user is this user's"
        );
    }

    #[test]
    fn a_source_that_reports_no_credentials_leaves_the_one_uid_it_has() {
        let (mut scan, _) = scanner(vec![p(60, 1, "/bin/sleep")]);
        let table = scan.table(|_| None).expect("reads");
        assert_eq!(table.rows[0].uid, ME);
        assert_eq!((table.rows[0].ruid, table.rows[0].suid), (None, None));
    }

    fn source_with(
        rows: Vec<P>,
        root: bool,
        exes: HashMap<i32, String>,
        reports_exe: bool,
    ) -> Scanner {
        Scanner::new(Box::new(WithCreds {
            inner: Fixture {
                facts: Arc::new(Mutex::new(facts_of(rows))),
                cwds: HashMap::new(),
                fds: HashMap::new(),
                fail: None,
            },
            creds: HashMap::new(),
            exes,
            root,
            reports_exe,
        }))
    }

    fn root_source(rows: Vec<P>, root: bool, exes: HashMap<i32, String>) -> Scanner {
        source_with(rows, root, exes, false)
    }

    /// Class 4 of ft-1d9f44d5. A sender that is root can signal every process,
    /// so the arguments of every process are what `-f` is tested against.
    #[test]
    fn a_root_sender_reads_the_arguments_of_every_process_and_another_sender_only_its_own() {
        let rows = || {
            vec![
                p(70, 1, "/bin/sleep"),
                P {
                    uid: 1001,
                    ..p(71, 1, "/usr/bin/worker")
                },
            ]
        };
        let argv = |pid: i32| Some(vec![format!("argv-of-{pid}")]);
        let table = root_source(rows(), true, HashMap::new())
            .table(argv)
            .expect("reads");
        assert!(table.rows.iter().all(|r| r.argv.is_some()));
        let table = root_source(rows(), false, HashMap::new())
            .table(argv)
            .expect("reads");
        let of = |pid: i32| table.rows.iter().find(|r| r.pid == pid).unwrap();
        assert!(of(70).argv.is_some(), "its own");
        assert!(of(71).argv.is_none(), "another user's stays unread");
    }

    /// The read of other users' arguments is bounded, and what the bound leaves
    /// out is unread, never a shorter table passed off as the whole one.
    #[test]
    fn a_root_sender_stops_reading_other_users_arguments_at_the_budget() {
        let rows = (71..76)
            .map(|pid| P {
                uid: 1001,
                ..p(pid, 1, "/usr/bin/worker")
            })
            .collect();
        let mut scan = root_source(rows, true, HashMap::new());
        let table = scan
            .table_within(|_| Some(vec!["0123456789".to_string()]), 25)
            .expect("reads");
        let read = table.rows.iter().filter(|r| r.argv.is_some()).count();
        // 11 bytes each: 0, 11, 22 are under 25, 33 is not.
        assert_eq!(
            read,
            3,
            "{:?}",
            table
                .rows
                .iter()
                .map(|r| r.argv.is_some())
                .collect::<Vec<_>>()
        );
        assert_eq!(table.rows.len(), 5, "no row is dropped, only left unread");
    }

    /// Class 5. A process run from a GitPulse or agent executable under another
    /// `argv[0]` is that executable's: the table carries the kernel's path.
    #[test]
    fn the_signal_table_carries_the_executable_the_kernel_runs_and_names_an_agent_by_it() {
        let rows = vec![
            // exec'd with a path that is not the agent's
            p(80, 1, "/opt/other/otherprog"),
            p(81, 1, "/opt/other/otherprog"),
            p(82, 1, "/bin/sleep"),
        ];
        let exes = HashMap::from([
            (80, CLAUDE.to_string()),
            (81, "/tmp/lab/gitpulse-aliased".to_string()),
            (82, "/bin/sleep".to_string()),
        ]);
        let table = root_source(rows, false, exes)
            .table(|pid| Some(vec![format!("argv-of-{pid}")]))
            .expect("reads");
        let row = |pid: i32| table.rows.iter().find(|r| r.pid == pid).unwrap();
        assert_eq!(row(80).agent, Some(AgentKind::Claude));
        assert_eq!(row(80).exec.as_deref(), Some("/opt/other/otherprog"));
        assert_eq!(row(80).real_exe.as_deref(), Some(CLAUDE));
        assert_eq!(row(81).agent, None);
        assert_eq!(
            row(81).real_exe.as_deref(),
            Some("/tmp/lab/gitpulse-aliased")
        );
        assert_eq!(row(82).agent, None);
    }

    #[test]
    fn a_cmdline_keeps_an_empty_argument_between_two_and_drops_trailing_ones() {
        let split = split_cmdline;
        assert_eq!(split(b"a\0b\0"), vec!["a", "b"]);
        assert_eq!(split(b"a\0\0b\0"), vec!["a", "", "b"]);
        assert_eq!(split(b"\0x\0"), vec!["", "x"]);
        assert_eq!(split(b"x\0\0"), vec!["x"]);
        assert_eq!(split(b"x\0\0\0"), vec!["x"]);
        assert_eq!(split(b"a b\0c\xffd\0"), vec!["a b", "c\u{fffd}d"]);
        // A blanked argument area is no arguments, not seventeen empty ones.
        assert!(split(&[0u8; 17]).is_empty());
        assert!(split(b"").is_empty());
    }

    /// A binary replaced since the process started reads `... (deleted)`: it is
    /// still that program.
    #[test]
    fn a_deleted_executable_is_still_the_program_it_names() {
        let rows = vec![p(80, 1, "/opt/other/otherprog")];
        let exes = HashMap::from([(80, format!("{CLAUDE} (deleted)"))]);
        let table = source_with(rows, false, exes, true)
            .table(|pid| Some(vec![format!("argv-of-{pid}")]))
            .expect("reads");
        let row = &table.rows[0];
        assert_eq!(row.real_exe.as_deref(), Some(CLAUDE));
        assert_eq!(row.agent, Some(AgentKind::Claude));
        assert!(!row.exe_unknown);
    }

    /// Where the OS reports the executable, a process whose own cannot be read,
    /// or is the dynamic loader, is of unknown identity; where it does not
    /// (macOS), none is.
    #[test]
    fn an_unreadable_executable_or_the_loader_is_an_unknown_identity_only_where_the_os_reports_it()
    {
        let rows = || {
            vec![
                p(81, 1, "/opt/other/unreadable"),
                p(82, 1, "/lib/ld-linux-aarch64.so.1"),
                p(83, 1, "/bin/sleep"),
                P {
                    uid: 1001,
                    ..p(84, 1, "/usr/bin/foreign")
                },
            ]
        };
        let exes = HashMap::from([
            (82, "/lib/ld-linux-aarch64.so.1".to_string()),
            (83, "/bin/sleep".to_string()),
        ]);
        let argv = |pid: i32| Some(vec![format!("argv-of-{pid}")]);
        let table = source_with(rows(), false, exes.clone(), true)
            .table(argv)
            .expect("reads");
        let unknown: Vec<i32> = table
            .rows
            .iter()
            .filter(|r| r.exe_unknown)
            .map(|r| r.pid)
            .collect();
        assert_eq!(
            unknown,
            vec![81, 82],
            "unreadable and the loader; a process the sender cannot read at all is not one"
        );
        let table = source_with(rows(), false, exes, false)
            .table(argv)
            .expect("reads");
        assert!(table.rows.iter().all(|r| !r.exe_unknown));
    }

    #[test]
    fn the_dynamic_loader_is_told_by_its_name() {
        for loader in [
            "/lib/ld-linux-aarch64.so.1",
            "/lib64/ld-linux-x86-64.so.2",
            "/usr/lib/ld.so",
            "/lib/ld-2.31.so",
            "ld-linux.so.3",
        ] {
            assert!(is_dynamic_loader(loader), "{loader}");
        }
        for other in [
            "/bin/sleep",
            "/usr/bin/ldd",
            "/usr/bin/ld",
            "/opt/old-linux",
        ] {
            assert!(!is_dynamic_loader(other), "{other}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_linux_proc_cwd_with_a_deleted_suffix_keeps_its_path_unless_literally_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let gone = dir.path().join("gone-worktree");
        let marked = PathBuf::from(format!("{} (deleted)", gone.display()));
        assert_eq!(super::parse_proc_cwd_link(marked), Some(gone));

        let literal_deleted = dir.path().join("real (deleted)");
        std::fs::create_dir(&literal_deleted).expect("create dir with deleted suffix");
        assert_eq!(
            super::parse_proc_cwd_link(literal_deleted.clone()),
            Some(literal_deleted)
        );

        assert_eq!(
            super::parse_proc_cwd_link(PathBuf::from("relative (deleted)")),
            None
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_cwd_returns_the_path_even_after_the_directory_is_deleted() {
        use crate::procguard::LockedSpawn;
        let dir = tempfile::tempdir().expect("tempdir");
        let work = dir.path().join("vanished");
        std::fs::create_dir(&work).expect("create vanished dir");
        let pidfile = dir.path().join("child.pid");
        let (exe, args) = fixture_command();
        let mut child = std::process::Command::new(&exe)
            .args(&args)
            .env(
                FIXTURE_ENV,
                format!("sleep=30,pidfile={}", pidfile.display()),
            )
            .current_dir(&work)
            .stdout(std::process::Stdio::null())
            .spawn_locked()
            .expect("spawn child in vanished dir");
        let pid = fixture_pid(&pidfile);
        std::fs::remove_dir(&work).expect("remove child cwd while alive");
        let resolved = super::os::cwd(pid);
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(resolved, Some(work));
    }

    #[test]
    fn collect_throttle_candidates_skips_session_roots_and_helpers_and_resolves_worker_cwd() {
        let facts = Arc::new(Mutex::new(antigravity_facts(vec![
            p(100, 1, "/usr/local/bin/claude"),
            p(101, 100, "/Users/dev/.cargo/bin/cargo"),
            p(102, 100, "/Users/dev/.local/bin/devmap"),
            p(103, 100, "/Users/dev/.local/bin/gitpulse-mcp"),
            p(
                200,
                1,
                "/Applications/Antigravity.app/Contents/MacOS/Antigravity",
            ),
            p(202, 200, AG_LANGUAGE_SERVER),
            p(201, 202, "/Users/dev/.rustup/toolchains/stable/bin/rustc"),
        ])));
        let mut scanner = Scanner::new(Box::new(Fixture {
            facts,
            cwds: HashMap::from([
                (100, PathBuf::from("/Users/dev/repo-a")),
                (200, PathBuf::from("/")),
                (201, PathBuf::from("/Users/dev/opted-out-repo")),
            ]),
            fds: HashMap::new(),
            fail: None,
        }));
        let out = scanner.collect_throttle_candidates(999).expect("scan");
        let pids: Vec<i32> = out.iter().map(|c| c.pid).collect();
        assert_eq!(
            pids,
            vec![101, 201],
            "interactive session roots (100, 200) and helper binaries (102, 103) must be excluded"
        );
        assert_eq!(out[0].session_cwd.as_deref(), Some("/Users/dev/repo-a"));
        assert_eq!(
            out[1].session_cwd.as_deref(),
            Some("/Users/dev/opted-out-repo"),
            "worker cwd must override IDE root cwd == /"
        );
    }

    #[test]
    fn a_worker_outside_every_repository_carries_its_ancestors_working_directories() {
        let facts = Arc::new(Mutex::new(facts_of(vec![
            p(100, 1, "/usr/local/bin/claude"),
            p(101, 100, "/Users/dev/.cargo/bin/cargo"),
            p(
                102,
                101,
                "/Users/dev/repo/target/debug/build/serde-1/build-script-build",
            ),
            p(103, 102, "/usr/bin/cc"),
        ])));
        let registry = "/Users/dev/.cargo/registry/src/index/serde-1.0.0";
        let mut scanner = Scanner::new(Box::new(Fixture {
            facts,
            cwds: HashMap::from([
                (100, PathBuf::from("/Users/dev/repo")),
                (101, PathBuf::from("/Users/dev/repo")),
                (102, PathBuf::from(registry)),
                (103, PathBuf::from(registry)),
            ]),
            fds: HashMap::new(),
            fail: None,
        }));
        let out = scanner.collect_throttle_candidates(999).expect("scan");
        let lineage = |pid: i32| {
            out.iter()
                .find(|c| c.pid == pid)
                .map(|c| c.lineage_cwds.clone())
                .unwrap_or_else(|| panic!("{pid} not a candidate: {out:?}"))
        };
        assert_eq!(lineage(102), vec!["/Users/dev/repo".to_string()]);
        assert_eq!(
            lineage(103),
            vec![registry.to_string(), "/Users/dev/repo".to_string()],
            "nearest first, each directory once"
        );
    }

    #[test]
    fn linux_slows_only_what_it_can_put_back_and_restores_the_value_it_found() {
        // The usual limit (0) cannot cover any return trip: not slowed, and said why.
        let refused = linux_may_slow(0, 0, false).unwrap_err();
        assert!(refused.contains("RLIMIT_NICE is 0"), "{refused}");
        // A limit that covers the old value (20 - 5 = 15), or a privileged caller.
        assert_eq!(linux_may_slow(5, 15, false), Ok(true));
        assert!(linux_may_slow(5, 14, false).is_err());
        assert_eq!(linux_may_slow(0, 0, true), Ok(true));
        // Already at or past the slowed value: nothing to do, nothing to undo.
        assert_eq!(linux_may_slow(LINUX_SLOW_NICE, 0, false), Ok(false));
        assert_eq!(linux_may_slow(15, 0, false), Ok(false));

        // Restored to what it had, or to the default when that record is gone.
        assert_eq!(linux_restore_nice(Some(5), LINUX_SLOW_NICE), Some(5));
        assert_eq!(linux_restore_nice(None, LINUX_SLOW_NICE), Some(0));
        // Someone changed it since: theirs to keep.
        assert_eq!(linux_restore_nice(Some(5), 12), None);
    }

    const AG_LANGUAGE_SERVER: &str =
        "/Applications/Antigravity.app/Contents/Resources/bin/language_server";

    /// [`facts_of`], with Antigravity's `language_server` given the flag that
    /// identifies it (`facts_of` passes the executable as the only argument).
    fn antigravity_facts(rows: Vec<P>) -> Vec<ProcFacts> {
        let mut facts = facts_of(rows);
        for f in &mut facts {
            if f.ident.name == "language_server" {
                f.ident = Arc::new(Identity::new(
                    "language_server".into(),
                    Some(AG_LANGUAGE_SERVER.into()),
                    vec![
                        AG_LANGUAGE_SERVER.into(),
                        "--override_ide_name".into(),
                        "antigravity".into(),
                    ],
                    LaunchKey::from_env(std::iter::empty::<&str>()),
                ));
            }
        }
        facts
    }

    #[test]
    fn an_ide_s_terminal_shells_are_never_slowed_only_its_agent_s_work() {
        let facts = Arc::new(Mutex::new(antigravity_facts(vec![
            p(
                200,
                1,
                "/Applications/Antigravity.app/Contents/MacOS/Antigravity",
            ),
            // The agent server and a build it started.
            p(202, 200, AG_LANGUAGE_SERVER),
            p(
                203,
                202,
                "/Users/dev/.rustup/toolchains/stable/bin/rustc",
            ),
            // The editor's terminal host, the person's shell, and their build.
            p(
                210,
                200,
                "/Applications/Antigravity.app/Contents/Frameworks/Antigravity Helper.app/Contents/MacOS/Antigravity Helper",
            ),
            p(211, 210, "/bin/zsh"),
            p(212, 211, "/Users/dev/.cargo/bin/cargo"),
        ])));
        let mut scanner = Scanner::new(Box::new(Fixture {
            facts,
            cwds: HashMap::from([
                (200, PathBuf::from("/")),
                (203, PathBuf::from("/Users/dev/repo")),
                (211, PathBuf::from("/Users/dev/repo")),
                (212, PathBuf::from("/Users/dev/repo")),
            ]),
            fds: HashMap::new(),
            fail: None,
        }));
        let out = scanner.collect_throttle_candidates(999).expect("scan");
        let pids: Vec<i32> = out.iter().map(|c| c.pid).collect();
        assert_eq!(
            pids,
            vec![203],
            "only the agent server's build; not the server itself, the terminal host, the shell or what was typed in it"
        );
    }
}
