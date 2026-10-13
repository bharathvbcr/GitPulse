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
}

impl AgentKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
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
    let runtime = base == "node" || base == "bun" || base.starts_with("node2");
    if runtime {
        let script = arg1?;
        if script.contains("/@anthropic-ai/claude-code/") {
            return Some(AgentKind::Claude);
        }
        if script.contains("/@openai/codex/") {
            return Some(AgentKind::Codex);
        }
    }
    None
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
        let kind = exec
            .as_deref()
            .and_then(|e| classify(e, argv.get(1).map(String::as_str)));
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
    /// The session's own process plus every descendant still attached.
    pub footprint: Reading<u64>,
    pub members: u32,
    pub unreadable_members: u32,
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
            match self.tree_of[i] {
                Some(t) => {
                    let sum = &mut trees[t as usize];
                    sum.add(f.footprint);
                    sum.written = sum.written.saturating_add(written_delta);
                    launched.add(f.footprint);
                }
                None if self.session_of[i].is_some() => observed.add(f.footprint),
                None => {}
            }
            if let Some(s) = self.session_of[i] {
                per_session[s as usize].add(f.footprint);
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
            });
        }
        tree_rows.sort_by(|a, b| a.tree.cmp(&b.tree));

        let (mut claude, mut codex) = (SessionCount::default(), SessionCount::default());
        let mut sessions = Vec::with_capacity(sessions_at.len().min(MAX_SESSIONS));
        for (n, &i) in sessions_at.iter().enumerate() {
            let f = &self.facts[i];
            let kind = f.ident.kind.unwrap_or(AgentKind::Claude);
            let tree = self.tree_of[i].map(|t| self.tree_names[t as usize].clone());
            let count = match kind {
                AgentKind::Claude => &mut claude,
                AgentKind::Codex => &mut codex,
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
                footprint: sum.reading(),
                members: sum.readable + sum.unreadable,
                unreadable_members: sum.unreadable,
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
        || signal_term(pid),
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

#[cfg(unix)]
fn signal_term(pid: i32) -> Result<(), String> {
    // SAFETY: a plain signal to a pid re-validated just above.
    if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
        Ok(())
    } else {
        Err(format!(
            "could not signal {pid}: {}",
            std::io::Error::last_os_error()
        ))
    }
}

#[cfg(not(unix))]
fn signal_term(_pid: i32) -> Result<(), String> {
    Err("terminating a process is not supported on this platform".into())
}

/// Whether this platform has a process scan at all.
pub(crate) const SCAN_SUPPORTED: bool = os::SUPPORTED;

/// Every agent session running now, for callers outside the sampler (the
/// insights facet). Keeps its own identity cache across calls, so a
/// repeated call reads only processes it has not seen.
pub(crate) fn observe_sessions() -> Result<Vec<AgentSession>, String> {
    static SCANNER: std::sync::OnceLock<std::sync::Mutex<Scanner>> = std::sync::OnceLock::new();
    if !os::SUPPORTED {
        return Err("this platform has no process scan".into());
    }
    let scanner = SCANNER.get_or_init(|| std::sync::Mutex::new(Scanner::os()));
    let mut scanner = scanner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let view = scanner.scan(&ScanContext::here());
    if !view.ok {
        return Err(view
            .error
            .unwrap_or_else(|| "the process table could not be read".into()));
    }
    if view.sessions_truncated {
        return Err(format!(
            "more than {MAX_SESSIONS} agent sessions are running; the scan does not describe them all"
        ));
    }
    Ok(view.sessions)
}

/// The processes whose working directory is `dir` or inside it, other than
/// this one. `Err` when the table could not be read.
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

/// Every pid in the process table, bounded by [`MAX_SCANNED_PROCESSES`].
/// `Err` when the table could not be read, or is larger than that bound.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn all_pids() -> Result<Vec<i32>, String> {
    let mut pids = Vec::new();
    os::list_pids(&mut pids)?;
    Ok(pids)
}

/// A process's working directory, when the OS will say.
pub(crate) fn cwd_of(pid: i32) -> Option<PathBuf> {
    os::cwd(pid)
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
        let argv: Vec<String> = items
            .by_ref()
            .take(argc)
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        let launch = LaunchKey::from_env(items.filter_map(|s| std::str::from_utf8(s).ok()));
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
    /// owns it: counted as another user's, not inspected — never dropped.
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

    pub(crate) fn probe_one(pid: i32) -> Option<ProcFacts> {
        let me = uid();
        let mut buf = vec![0u8; argmax()];
        facts(pid, me, timebase(), |basic| {
            Arc::new(read_identity(pid, basic, me, &mut buf))
        })
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
    }
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
        // A deleted directory reads back as "/old/path (deleted)".
        path.is_absolute().then_some(path).filter(|p| p.is_dir())
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

    pub(crate) fn probe_one(pid: i32) -> Option<ProcFacts> {
        facts(pid, uid(), clock_ticks(), page_size(), |name, ours, _| {
            Arc::new(read_identity(pid, name, ours))
        })
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

    pub(crate) fn probe_one(_pid: i32) -> Option<ProcFacts> {
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
}
