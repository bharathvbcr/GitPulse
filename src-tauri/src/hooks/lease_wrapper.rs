//! The program a heavy `cargo` or `go` build goes through: it takes a build
//! slot (`sysload::lease`), then becomes the build.
//!
//! # Why a wrapper on PATH and not the hook
//!
//! The `command-gate` hook runs under a 10 s timeout and can only answer
//! allow, ask or deny: it cannot wait for a slot and it cannot rewrite the
//! command. A wrapper can wait as long as it takes, and when it `exec`s the
//! build it becomes the build, so the slot's lock belongs to the build's own
//! process and is dropped by the kernel when that process ends.
//!
//! It runs as `gitpulse-hook lease -- <program> <args>`, or as `cargo` / `go`
//! when the app's `lease-bin` folder (symlinks to this binary) is on PATH.
//! `gitpulse-hook`'s `main` hands control here before it starts the hook
//! deadline or installs a signal handler: neither belongs to a process that
//! may wait for hours.
//!
//! # What it never does
//!
//! * It never refuses a build on a guess. A lease directory it cannot use, a
//!   verdict it cannot read, a program it cannot classify: the build runs, and
//!   a line on stderr says it was **not** leased and why (and a marker counts
//!   it for Health). A check that could not run is never silent.
//! * It never leases a nested call (`GITPULSE_LEASE_HELD`, or the slot-gate
//!   shim's `CARGO_SLOT_HELD`), a build the heavy-lane holder already booked
//!   (`MAC_HEAVY_LABEL`), or a verb that builds nothing or never exits
//!   (`cargo run`, `cargo clean`, `go env`; see `heavy_commands::leased`).
//! * It never waits forever. After `wait_max_s` a build starts without a slot
//!   and says so loudly, unless the governor refuses new work; then it keeps
//!   waiting and gives up (exit 75, nothing built) at `wait_hard_s`.

use std::ffi::{CString, OsStr, OsString};
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::advisory_once::{marker_dir, private_dir, prune};
use super::heavy_commands::leased;
use super::load_gate::{availability_live, Availability};
use crate::sysload::lease::{
    self, capacity, may_take, order, session_of, Dir, Grant, LeaseCapacity, LeaseRecord,
    LeaseWaiter, Load, Own,
};
use crate::sysload::Clock;
use crate::tool_config::LeaseMode;

/// The subcommand: `waylode-hook lease -- cargo build`.
pub const SUBCOMMAND: &str = "lease";

pub use super::lease_env::{BIN_ENV, HELD_ENV, LEGACY_BIN_ENV, LEGACY_HELD_ENV, SHIM_HELD_ENV};

/// Test seams: the polling and announcing intervals, in milliseconds (clamped
/// to a sane range). Real runs set neither. The directory and the program are
/// found from `HOME` and `PATH`, which a test controls too, so they have no
/// seam of their own.
pub const POLL_ENV: &str = "WAYLODE_LEASE_POLL_MS";
pub const LEGACY_POLL_ENV: &str = "GITPULSE_LEASE_POLL_MS";
pub const ANNOUNCE_ENV: &str = "WAYLODE_LEASE_ANNOUNCE_MS";
pub const LEGACY_ANNOUNCE_ENV: &str = "GITPULSE_LEASE_ANNOUNCE_MS";

/// Programs the wrapper is installed in front of.
pub const PROGRAMS: [&str; 2] = ["cargo", "go"];

/// The exit status of a build that waited the whole of `wait_hard_s`.
pub const EXIT_GAVE_UP: i32 = 75;

/// Everything a build waits by.
#[derive(Debug, Clone)]
pub struct Policy {
    pub slots: u32,
    pub wait_max: Duration,
    pub wait_hard: Duration,
    pub poll: Duration,
    pub announce: Duration,
}

/// What the wrapper reads from outside itself while it waits.
pub trait Ports {
    fn load(&mut self) -> Load;
    /// Active Manvi system steward critical slowdown reason for this build's
    /// working directory, when in force. Holds capacity at 0 refusing slots
    /// even if `host_load.govern_launches == false` or `LeaseMode::Off`.
    fn steward_refusal(&mut self) -> Option<String> {
        None
    }
    fn now_ns(&self) -> u64;
    fn sleep(&mut self, how_long: Duration);
    fn say(&mut self, line: &str);
}

pub(crate) fn steward_refusal_note(
    record: &crate::harness::steward::ActiveThrottleRecord,
) -> String {
    format!(
        "Manvi system steward critical slowdown is active ({} load: {})",
        record.tier, record.reason
    )
}

/// Whether an agent session launched by GitPulse should have `lease-bin`
/// prepended to its `PATH`. True when build leases are `On`, and also when
/// `LeaseMode::Off` while Manvi steward critical slowdown is enabled so
/// non-hook agents (such as Codex) pass straight through while calm (`0` slots
/// overhead) and still obey `steward/active.json` if critical pressure hits.
#[must_use]
pub fn should_inject_lease_bin(mode: LeaseMode, steward_any_slow: bool) -> bool {
    mode == LeaseMode::On || steward_any_slow
}

/// Decides whether `run` enters the build-lease queue rather than `exec`-ing
/// straight through. Even when `LeaseMode::Off`, an active Manvi steward
/// critical slowdown lease gates heavy `cargo`/`go` builds for non-hook agents
/// (such as Codex or direct CLI subshells).
#[must_use]
pub fn should_enter_wrapper(
    nested: bool,
    booked: bool,
    is_leased_cmd: bool,
    mode: LeaseMode,
    steward_active: bool,
) -> bool {
    !nested && !booked && is_leased_cmd && (mode == LeaseMode::On || steward_active)
}

/// How a wait ended.
#[derive(Debug)]
pub enum Acquired {
    Granted {
        grant: Grant,
        waited: Duration,
        capacity: LeaseCapacity,
    },
    /// Waited past `wait_max` and started without a slot (or without a slot
    /// record: every slot file was in use, `grant` is `None`).
    Overflow {
        grant: Option<Grant>,
        waited: Duration,
    },
    /// The governor refused new work for all of `wait_hard`.
    GaveUp { waited: Duration },
}

/// The nth, spelled as a person says it.
fn ordinal(n: usize) -> String {
    let suffix = match (n % 100, n % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

fn age(ns: u64) -> String {
    let s = ns / 1_000_000_000;
    match s {
        0..=89 => format!("{s}s"),
        90..=5_399 => format!("{}m", s / 60),
        _ => format!("{}h", s / 3_600),
    }
}

/// The line said while a build waits: where it is in the queue, who holds the
/// slots, and why the count is what it is.
pub fn waiting_line(
    rank: usize,
    queued: usize,
    holders: &[lease::LeaseHolder],
    capacity: &LeaseCapacity,
    now_ns: u64,
) -> String {
    let who = holders
        .iter()
        .take(4)
        .map(|h| match &h.record {
            Some(r) => format!(
                "{} in {} (pid {}, {})",
                r.label,
                r.cwd.rsplit('/').next().unwrap_or(&r.cwd),
                h.pid,
                age(now_ns.saturating_sub(r.granted_mono_ns.unwrap_or(r.enqueued_mono_ns)))
            ),
            None => format!("pid {}, unknown", h.pid),
        })
        .collect::<Vec<_>>()
        .join("; ");
    let held = format!(
        "{} of {} slots held{}",
        holders.len(),
        capacity.slots,
        if who.is_empty() {
            String::new()
        } else {
            format!(": {who}")
        }
    );
    let note = capacity
        .note
        .as_deref()
        .map(|n| format!(" ({n})"))
        .unwrap_or_default();
    format!(
        "gitpulse: waiting for a build slot, {} of {queued} in line; {held}{note}.",
        ordinal(rank + 1)
    )
}

/// Takes the lowest slot that can be had. A slot whose file cannot be used is
/// passed over for the next one, so one bad file never aborts the wait: `Err`
/// only when every free slot failed that way. `Ok(None)` is a lost race (or no
/// free slot), which the caller answers by reading the board again.
fn take_any(
    dir: &Dir,
    board: &lease::Board,
    own: &lease::Own,
    record: &LeaseRecord,
) -> Result<Option<Grant>, String> {
    let mut failed = None;
    for slot in Dir::free_slots(board, own) {
        match dir.take(slot, record) {
            Ok(Some(grant)) => return Ok(Some(grant)),
            Ok(None) => return Ok(None),
            Err(why) => failed = Some(why),
        }
    }
    match failed {
        Some(why) => Err(why),
        None => Ok(None),
    }
}

/// Waits for a slot. `Err` only when the queue cannot be used at all, which
/// the caller reports as "not leased" and runs the build anyway.
pub fn acquire(
    dir: &Dir,
    template: &LeaseRecord,
    policy: &Policy,
    ports: &mut dyn Ports,
) -> Result<Acquired, String> {
    let started = ports.now_ns();
    let mut ticket = dir.enqueue(template)?;
    say_record_error(ports, &ticket);
    let mut last_said: Option<u64> = None;
    let mut rounds: u64 = 0;
    loop {
        rounds += 1;
        let now = ports.now_ns();
        let waited = Duration::from_nanos(now.saturating_sub(started));
        if !dir.still_queued(&ticket) {
            // Deleted under us by a reader that raced the lock: queue again,
            // keeping the original time, so the wait is not lost.
            ticket = dir.enqueue(template)?;
            say_record_error(ports, &ticket);
        }
        let load = ports.load();
        let mut cap = capacity(policy.slots, &load);
        if let Some(note) = ports.steward_refusal() {
            cap = LeaseCapacity {
                configured: cap.configured,
                slots: 0,
                refuses: true,
                tier: Some(crate::sysload::Tier::Severe),
                note: Some(note),
            };
        }
        let own = ticket.own();
        let mut board = dir.board(&own);
        board.waiters.push(LeaseWaiter {
            name: ticket.name.clone(),
            record: template.clone(),
        });
        order(&mut board.waiters, &board.holders);
        let rank = board
            .waiters
            .iter()
            .position(|w| w.name == ticket.name)
            .unwrap_or(0);
        // A slot file nobody can read may be held: it counts as held.
        let held = board.occupied();
        let granted_at = |overflow: bool| LeaseRecord {
            granted_mono_ns: Some(now),
            tier_at_grant: cap.tier,
            verdict_unavailable_reason: match &load {
                Load::Unknown(why) => Some(why.clone()),
                Load::Verdict(_) => None,
            },
            overflow,
            ..template.clone()
        };

        if may_take(rank, held, &cap) {
            if let Some(grant) = take_any(dir, &board, &own, &granted_at(false))? {
                // Two waiters that read the same free slot at once cannot
                // both keep it: the one that finds more held than the
                // count allows lets go and asks again.
                let after = dir.board(&Own {
                    slot: Some(grant.slot),
                    ticket: Some(ticket.name.clone()),
                });
                if after.occupied() < cap.slots as usize {
                    ticket.withdraw();
                    return Ok(Acquired::Granted {
                        grant,
                        waited,
                        capacity: cap,
                    });
                }
                drop(grant);
            }
        }

        if cap.refuses {
            if waited >= policy.wait_hard {
                ticket.withdraw();
                return Ok(Acquired::GaveUp { waited });
            }
        } else if waited >= policy.wait_max {
            // An overflow runs without a slot when none can be had; it does not
            // fail the build over a slot file it cannot use.
            let grant = take_any(dir, &board, &own, &granted_at(true)).unwrap_or(None);
            ticket.withdraw();
            return Ok(Acquired::Overflow { grant, waited });
        }

        let due = match last_said {
            None => true,
            Some(at) => now.saturating_sub(at) >= policy.announce.as_nanos() as u64,
        };
        if due {
            ports.say(&waiting_line(
                rank,
                board.waiters.len(),
                &board.holders,
                &cap,
                now,
            ));
            last_said = Some(now);
        }
        // A little per-process spread, so two waiters that woke together do
        // not read the same board again together.
        let spread =
            Duration::from_millis((u64::try_from(template.pid).unwrap_or(0) * 37 + rounds) % 120);
        ports.sleep(policy.poll + spread);
    }
}

/// Says that a queue entry holds its place without a record (a full disk):
/// the build still waits its turn, but other builds cannot see it waiting.
fn say_record_error(ports: &mut dyn Ports, ticket: &lease::Ticket) {
    if let Some(why) = &ticket.record_error {
        ports.say(&format!(
            "gitpulse: waiting for a build slot, but this build's queue entry could not be written ({why}): it keeps its place, and other builds cannot see it waiting."
        ));
    }
}

/// `CARGO_BUILD_JOBS`: the CPUs divided by the configured slots, at least 2,
/// so N leased builds do not each assume the whole machine.
pub fn jobs_for(cpus: usize, slots: u32) -> usize {
    (cpus / slots.max(1) as usize).max(2)
}

/// The environment the build starts with: `base` with `set` overriding and
/// `defaults` filling what is absent.
pub fn exec_env(
    base: Vec<(OsString, OsString)>,
    set: &[(&str, String)],
    defaults: &[(&str, String)],
) -> Vec<(OsString, OsString)> {
    let mut env = base;
    for (name, value) in set {
        env.retain(|(k, _)| k != OsStr::new(name));
        env.push((OsString::from(name), OsString::from(value)));
    }
    for (name, value) in defaults {
        if !env.iter().any(|(k, _)| k == OsStr::new(name)) {
            env.push((OsString::from(name), OsString::from(value)));
        }
    }
    env
}

/// The first executable called `name` on `path`, skipping `skip` (the
/// wrapper's own folders) and anything that is this wrapper: a program found
/// there would run the wrapper again, forever.
pub fn resolve_program(
    name: &str,
    path: &OsStr,
    skip: &[PathBuf],
    own_exe: &Path,
) -> Option<PathBuf> {
    let own = std::fs::canonicalize(own_exe).ok();
    for dir in std::env::split_paths(path) {
        if dir.as_os_str().is_empty() || skip.iter().any(|s| s == &dir) {
            continue;
        }
        let candidate = dir.join(name);
        let Ok(meta) = std::fs::metadata(&candidate) else {
            continue;
        };
        if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
            continue;
        }
        let canonical = std::fs::canonicalize(&candidate).unwrap_or_else(|_| candidate.clone());
        let is_wrapper = Some(&canonical) == own.as_ref()
            || canonical.file_name().is_some_and(|f| {
                let s = f.to_string_lossy();
                s.starts_with("waylode-hook") || s.starts_with("gitpulse-hook")
            });
        if is_wrapper {
            continue;
        }
        return Some(candidate);
    }
    None
}

/// Replaces this process with `program`. Returns only if that failed.
fn exec(
    program: &Path,
    argv0: &str,
    args: &[OsString],
    env: Vec<(OsString, OsString)>,
) -> std::io::Error {
    let cstr = |bytes: &[u8]| CString::new(bytes.to_vec());
    let Ok(path) = cstr(program.as_os_str().as_bytes()) else {
        return std::io::Error::from(std::io::ErrorKind::InvalidInput);
    };
    let mut argv: Vec<CString> = Vec::with_capacity(args.len() + 1);
    for word in std::iter::once(OsStr::new(argv0)).chain(args.iter().map(OsString::as_os_str)) {
        match cstr(word.as_bytes()) {
            Ok(word) => argv.push(word),
            Err(_) => return std::io::Error::from(std::io::ErrorKind::InvalidInput),
        }
    }
    let mut envp: Vec<CString> = Vec::with_capacity(env.len());
    for (name, value) in env {
        let mut pair = name.into_vec();
        pair.push(b'=');
        pair.extend(value.into_vec());
        if let Ok(pair) = CString::new(pair) {
            envp.push(pair);
        }
    }
    let argv_ptrs: Vec<*const libc::c_char> = argv
        .iter()
        .map(|c| c.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();
    let env_ptrs: Vec<*const libc::c_char> = envp
        .iter()
        .map(|c| c.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();
    // SAFETY: `path`, every `argv` and `envp` entry are NUL-terminated and
    // outlive the call, and both pointer arrays end in a null pointer. On
    // success this never returns; on failure nothing was changed. Rust's
    // runtime ignores SIGPIPE at start and an ignored signal survives `exec`,
    // so the build would never die on a closed pipe the way a shell's child
    // does (`cargo build | head`): put the default back first.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        libc::execve(path.as_ptr(), argv_ptrs.as_ptr(), env_ptrs.as_ptr());
    }
    std::io::Error::last_os_error()
}

/// `(program, args)` when this process was started as a lease wrapper:
/// as `cargo` / `go` (a link in `lease-bin`), or as
/// `gitpulse-hook lease [--] <program> <args>`.
pub fn invocation(argv: &[OsString]) -> Option<(String, Vec<OsString>)> {
    let name = Path::new(argv.first()?)
        .file_name()?
        .to_string_lossy()
        .into_owned();
    if PROGRAMS.contains(&name.as_str()) {
        return Some((name, argv[1..].to_vec()));
    }
    if argv.get(1).map(OsString::as_os_str) != Some(OsStr::new(SUBCOMMAND)) {
        return None;
    }
    let rest = &argv[2..];
    let rest = match rest.first() {
        Some(first) if first == "--" => &rest[1..],
        _ => rest,
    };
    let program = rest.first()?.to_string_lossy().into_owned();
    let program = Path::new(&program)
        .file_name()?
        .to_string_lossy()
        .into_owned();
    Some((program, rest[1..].to_vec()))
}

fn say_stderr(line: &str) {
    let _ = writeln!(std::io::stderr(), "{line}");
}

struct LivePorts {
    cwd: String,
}

impl Ports for LivePorts {
    fn load(&mut self) -> Load {
        match availability_live() {
            Availability::Verdict { verdict, .. } => Load::Verdict(verdict),
            Availability::Unavailable(why) => Load::Unknown(if why.is_empty() {
                "the host-load verdict could not be read".into()
            } else {
                why
            }),
        }
    }

    fn steward_refusal(&mut self) -> Option<String> {
        crate::harness::steward::active_throttle_for_cwd(&self.cwd)
            .as_ref()
            .map(steward_refusal_note)
    }

    fn now_ns(&self) -> u64 {
        Clock::now().mono_ns
    }

    fn sleep(&mut self, how_long: Duration) {
        std::thread::sleep(how_long);
    }

    fn say(&mut self, line: &str) {
        say_stderr(line);
    }
}

fn env_ms(primary: &str, legacy: &str, default: Duration, min: u64, max: u64) -> Duration {
    crate::env_compat::var_compat(primary, legacy)
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(default, |ms| Duration::from_millis(ms.clamp(min, max)))
}

/// The record a waiter and its slot carry, from this process and its
/// environment.
fn template(label: String) -> LeaseRecord {
    let me = i32::try_from(std::process::id()).unwrap_or(0);
    let facts = crate::sysload::procscan::probe_one(me);
    let start_us = facts.as_ref().map_or(0, |f| f.start_us);
    let parent = facts
        .as_ref()
        .map(|f| f.ppid)
        .and_then(|ppid| crate::sysload::procscan::probe_one(ppid).map(|p| (p.pid, p.start_us)))
        .unwrap_or((0, 0));
    let terminal = crate::env_compat::var_compat(
        crate::terminal::TERMINAL_SESSION_ENV,
        crate::terminal::LEGACY_TERMINAL_SESSION_ENV,
    );
    let run = crate::env_compat::var_compat(
        crate::sysload::RUN_ID_ENV,
        crate::sysload::LEGACY_RUN_ID_ENV,
    );
    let ancestor = (terminal.is_none() && run.is_none())
        .then(|| agent_ancestor(me))
        .flatten();
    let (session_key, priority) = session_of(terminal.as_deref(), run.as_deref(), ancestor, parent);
    LeaseRecord {
        pid: me,
        start_us,
        session_key,
        priority,
        terminal_session: terminal,
        run_id: run,
        cwd: std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        label,
        enqueued_mono_ns: Clock::now().mono_ns,
        granted_mono_ns: None,
        tier_at_grant: None,
        verdict_unavailable_reason: None,
        overflow: false,
    }
    .bounded()
}

/// The nearest agent session above `pid`, by walking parents (bounded) and
/// looking each up among the sessions the scan sees.
fn agent_ancestor(pid: i32) -> Option<(i32, u64)> {
    let sessions = crate::sysload::observe_sessions().ok()?;
    let mut at = crate::sysload::procscan::probe_one(pid)?.ppid;
    for _ in 0..32 {
        if at <= 1 {
            return None;
        }
        if let Some(found) = sessions.iter().find(|s| s.pid == at) {
            return Some((found.pid, found.start_us));
        }
        at = crate::sysload::procscan::probe_one(at)?.ppid;
    }
    None
}

/// What a person reads as the label of a build: the program and its verb.
fn label_of(program: &str, args: &[OsString]) -> String {
    let verb = args
        .iter()
        .map(|a| a.to_string_lossy())
        .find(|a| !a.starts_with('-') && !a.starts_with('+'));
    let label = match verb {
        Some(verb) => format!("{program} {verb}"),
        None => program.to_string(),
    };
    label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || " -_.:/+".contains(c) {
                c
            } else {
                '?'
            }
        })
        .take(48)
        .collect()
}

/// Runs the wrapper if this process is one. `None` when it is not (the hook
/// binary goes on to be a hook). `Some(code)` is an exit status: the build
/// itself never comes back here, it replaces this process.
pub fn maybe_run() -> Option<i32> {
    let argv: Vec<OsString> = std::env::args_os().collect();
    let (program, args) = invocation(&argv)?;
    Some(run(&program, &args))
}

fn run(program: &str, args: &[OsString]) -> i32 {
    let config = crate::tool_config::default_config_dir();
    let own_exe = std::env::current_exe().unwrap_or_default();
    let mut skip: Vec<PathBuf> = Vec::new();
    if let Some(config) = &config {
        skip.push(lease::bin_dir(config));
    }
    if let Some(bin) = crate::env_compat::var_os_compat(BIN_ENV, LEGACY_BIN_ENV) {
        skip.push(PathBuf::from(bin));
    }
    let real =
        std::env::var_os("PATH").and_then(|path| resolve_program(program, &path, &skip, &own_exe));
    let Some(real) = real else {
        say_stderr(&format!(
            "waylode-hook lease: no {program} found on PATH (past Waylode's own link); nothing was run"
        ));
        return 127;
    };
    let base_env = || std::env::vars_os().collect::<Vec<_>>();
    let words: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let nested = crate::env_compat::var_os_compat(HELD_ENV, LEGACY_HELD_ENV).is_some()
        || std::env::var_os(SHIM_HELD_ENV).is_some();
    let booked = std::env::var_os("MAC_HEAVY_LABEL").is_some_and(|v| !v.is_empty());
    let settings = crate::tool_config::host_load_leases();
    let is_leased = leased(program, &words);
    let steward_active = is_leased
        && !nested
        && !booked
        && crate::harness::steward::active_throttle_for_cwd(&cwd).is_some();
    if !should_enter_wrapper(nested, booked, is_leased, settings.mode, steward_active) {
        return finish(exec(&real, program, args, base_env()), &real);
    }

    let label = label_of(program, args);
    let tpl = template(label);
    let opened = match config.as_ref().map(|c| lease::lease_dir(c)) {
        Some(path) => Dir::open(&path),
        None => Err("there is no per-user config directory (HOME unset)".to_string()),
    };
    let dir = match opened {
        Ok(dir) => dir,
        Err(why) => return unleased(&real, program, args, &why),
    };
    let policy = Policy {
        slots: settings.slots,
        wait_max: Duration::from_secs(settings.wait_max_s),
        wait_hard: Duration::from_secs(settings.wait_hard_s),
        poll: env_ms(POLL_ENV, LEGACY_POLL_ENV, Duration::from_secs(2), 5, 10_000),
        announce: env_ms(
            ANNOUNCE_ENV,
            LEGACY_ANNOUNCE_ENV,
            Duration::from_secs(30),
            5,
            3_600_000,
        ),
    };
    let mut ports = LivePorts {
        cwd: tpl.cwd.clone(),
    };
    let acquired = match acquire(&dir, &tpl, &policy, &mut ports) {
        Ok(acquired) => acquired,
        Err(why) => return unleased(&real, program, args, &why),
    };
    let cpus = std::thread::available_parallelism().map_or(2, usize::from);
    let mut set: Vec<(&str, String)> = vec![(SHIM_HELD_ENV, "gitpulse-lease".into())];
    let mut defaults: Vec<(&str, String)> = Vec::new();
    if program == "cargo" {
        defaults.push((
            "CARGO_BUILD_JOBS",
            jobs_for(cpus, settings.slots).to_string(),
        ));
        if settings.incremental_off {
            set.push(("CARGO_INCREMENTAL", "0".into()));
        }
    } else if program == "go" && steward_active {
        defaults.push(("GOMAXPROCS", jobs_for(cpus, settings.slots).to_string()));
    }
    let grant = match acquired {
        Acquired::Granted {
            grant,
            waited,
            capacity,
        } => {
            if waited >= Duration::from_secs(1) {
                say_stderr(&format!(
                    "gitpulse: build slot {} granted after {}s{}.",
                    grant.slot,
                    waited.as_secs(),
                    capacity
                        .note
                        .as_deref()
                        .map(|n| format!(" ({n})"))
                        .unwrap_or_default()
                ));
            }
            Some(grant)
        }
        Acquired::Overflow { grant, waited } => {
            say_stderr(&format!(
                "gitpulse: WARNING: no build slot freed in {}s, so this build is starting WITHOUT one{}.",
                waited.as_secs(),
                if grant.is_some() {
                    " (recorded as an overflow)"
                } else {
                    " and every slot file is in use, so it is not recorded either"
                }
            ));
            grant
        }
        Acquired::GaveUp { waited } => {
            say_stderr(&format!(
                "gitpulse: gave up after {}s: the host load stayed too high to start a build, and nothing was built. Try again when the load clears, or change Settings > Agents > Host load.",
                waited.as_secs()
            ));
            return EXIT_GAVE_UP;
        }
    };
    if let Some((slot, why)) = grant
        .as_ref()
        .and_then(|g| g.record_error.as_ref().map(|why| (g.slot, why)))
    {
        say_stderr(&format!(
            "gitpulse: holding build slot {slot}, but its record could not be written ({why}): Health shows the slot held by an unnamed process."
        ));
    }
    let slot = grant
        .as_ref()
        .map_or_else(|| "overflow".to_string(), |g| g.slot.to_string());
    set.push((HELD_ENV, slot.clone()));
    set.push((LEGACY_HELD_ENV, slot));
    if let Some(grant) = &grant {
        if let Err(error) = grant.keep_for_exec() {
            // The descriptor would close at exec and take the lock with it:
            // the build would run holding nothing. Say so.
            say_stderr(&format!(
                "waylode: did NOT keep the build slot across exec ({error}); this build runs without holding one"
            ));
        }
    }
    let env = exec_env(base_env(), &set, &defaults);
    let error = exec(&real, program, args, env);
    finish(error, &real)
}

fn finish(error: std::io::Error, real: &Path) -> i32 {
    say_stderr(&format!(
        "waylode-hook lease: could not run {}: {error}",
        real.display()
    ));
    if error.kind() == std::io::ErrorKind::NotFound {
        127
    } else {
        126
    }
}

/// The build runs, unleased, and the person and Health are told.
fn unleased(real: &Path, program: &str, args: &[OsString], why: &str) -> i32 {
    say_stderr(&format!("gitpulse: did NOT lease this build: {why}"));
    if let Err(error) = record_miss(why) {
        say_stderr(&format!(
            "gitpulse: and this unleased build could NOT be counted for Health ({error}), so Health will not show it."
        ));
    }
    finish(
        exec(real, program, args, std::env::vars_os().collect()),
        real,
    )
}

const MISS_PREFIX: &str = "leasemiss-";

/// One marker per distinct reason a build ran without a lease, in the same
/// private directory as the hook's other markers (15 minute prune). Health
/// counts them: a lease that quietly never happens is the failure this exists
/// to make visible. `Err` when the marker could not be made, which the caller
/// says: a miss Health cannot count must not look like no miss at all.
fn record_miss(why: &str) -> Result<(), String> {
    let dir = marker_dir();
    private_dir(&dir).map_err(|e| format!("cannot use {}: {e}", dir.display()))?;
    let name = format!(
        "{MISS_PREFIX}{:016x}-{}",
        super::fnv1a(why),
        std::process::id()
    );
    let made = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(&name))
        .map(drop)
        .map_err(|e| format!("cannot create {}: {e}", dir.join(&name).display()));
    prune(&dir, std::time::SystemTime::now());
    made
}

/// How many builds ran unleased in the last 15 minutes, counted from the
/// markers, and whether the directory held more entries than one read looks at
/// (then the count is a floor). `Err` when they cannot be read, never
/// `Ok((0, false))` for that.
pub fn misses() -> Result<(usize, bool), String> {
    misses_in(&marker_dir(), super::advisory_once::PRUNE_SCAN)
}

/// [`misses`] over `dir`, reading at most `cap` entries.
pub fn misses_in(dir: &Path, cap: usize) -> Result<(usize, bool), String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((0, false)),
        Err(error) => return Err(format!("could not read {}: {error}", dir.display())),
    };
    let now = std::time::SystemTime::now();
    let read: Vec<_> = entries.take(cap.saturating_add(1)).collect();
    let truncated = read.len() > cap;
    let count = read
        .into_iter()
        .take(cap)
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(MISS_PREFIX))
        .filter(|e| {
            e.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age <= super::advisory_once::MARKER_TTL)
        })
        .count();
    Ok((count, truncated))
}

/* ── How a session that GitPulse did not launch gets the wrapper ─────────── */

/// How long a session's "its PATH is led by the lease folder" record is kept.
const PATH_RECORD_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

fn path_record_dir(config: &Path) -> PathBuf {
    lease::lease_dir(config).join("paths")
}

fn plain_session(session: &str) -> bool {
    !session.is_empty()
        && session.len() <= 128
        && session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Whether `session` was recorded as having the lease folder on its PATH.
pub fn session_has_path(config: &Path, session: &str) -> bool {
    plain_session(session)
        && std::fs::metadata(path_record_dir(config).join(format!("leasepath-{session}")))
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| std::time::SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age <= PATH_RECORD_TTL)
}

fn record_session_path(config: &Path, session: &str) -> Result<(), String> {
    let dir = path_record_dir(config);
    if let Some(parent) = dir.parent() {
        private_dir(parent)?;
    }
    private_dir(&dir)?;
    std::fs::write(dir.join(format!("leasepath-{session}")), b"")
        .map_err(|e| format!("could not record the session's PATH: {e}"))?;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let now = std::time::SystemTime::now();
        for entry in entries.take(super::advisory_once::PRUNE_SCAN).flatten() {
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age > PATH_RECORD_TTL);
            if old {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Ok(())
}

/// What the session-start hook did about the wrapper for one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// GitPulse launched the session with the folder on its PATH, and the line
    /// that keeps it first was also written to `CLAUDE_ENV_FILE`.
    Launched,
    /// The folder was added through `CLAUDE_ENV_FILE`.
    EnvFile,
    /// Neither, and why.
    Skipped(String),
}

impl Route {
    /// The clause the session brief carries.
    pub fn describe(&self) -> String {
        match self {
            Self::Launched => {
                "build leases: on (GitPulse launched this session with its lease folder on PATH, and the lease folder is put first again before each Bash command)"
                    .into()
            }
            Self::EnvFile => {
                "build leases: on (the lease folder is put first on PATH before each Bash command)"
                    .into()
            }
            Self::Skipped(why) => {
                format!("build leases: NOT gating this session's builds: {why}")
            }
        }
    }
}

/// Puts the lease folder first on this session's PATH, through the file Claude
/// Code runs as shell code before each Bash command (`CLAUDE_ENV_FILE`, which
/// a `SessionStart` hook may write: https://code.claude.com/docs/en/hooks,
/// fetched 2026-11-15). The file is appended to, never replaced, as the docs
/// ask, and written once per session. `launched` is whether this process
/// already has `GITPULSE_LEASE_BIN` (a session GitPulse started).
pub fn route_session(
    session: &str,
    env_file: Option<&OsStr>,
    config: Option<&Path>,
    launched: bool,
) -> Route {
    let enabled = crate::tool_config::host_load_leases().mode == LeaseMode::On;
    if !enabled {
        return Route::Skipped("build leases are switched off in Settings".into());
    }
    let Some(config) = config else {
        return Route::Skipped("there is no per-user config directory (HOME unset)".into());
    };
    let bin = lease::bin_dir(config);
    if let Some(problem) = lease::links_problem(&bin, None) {
        return Route::Skipped(format!(
            "the cargo and go links in {} do not work ({problem}): Waylode has not made them, or they are stale (is the app running, and is waylode-hook beside it?)",
            bin.display()
        ));
    }
    let Some(env_file) = env_file else {
        return Route::Skipped(if launched {
            "GitPulse started this session with its lease folder first on PATH, but this host gave the hook no CLAUDE_ENV_FILE to keep it there, and a shell startup file (such as ~/.zshenv) can put another cargo ahead of it, so the gate cannot be verified".into()
        } else {
            "this host gave the hook no CLAUDE_ENV_FILE, and the session was not launched from GitPulse"
                .into()
        });
    };
    // Validated before anything is written or recorded.
    if !plain_session(session) {
        return Route::Skipped("the session has no usable id to record".into());
    }
    let bin_text = bin.to_string_lossy();
    if bin_text.contains(['"', '$', '`', '\\', '\n']) {
        return Route::Skipped(format!(
            "{} contains a character that cannot be quoted safely for a shell",
            bin.display()
        ));
    }
    let line = format!("export PATH=\"{bin_text}:$PATH\"");
    if let Err(error) = append_line_once(env_file, &line) {
        return Route::Skipped(format!("could not write CLAUDE_ENV_FILE: {error}"));
    }
    match record_session_path(config, session) {
        Ok(()) if launched => Route::Launched,
        Ok(()) => Route::EnvFile,
        Err(why) => Route::Skipped(why),
    }
}

/// Appends `line` to `path` unless a line equal to it is already there, so a
/// session-start hook that runs more than once does not stack copies. The file
/// is appended to, never replaced. `Ok(true)` when it wrote.
fn append_line_once(path: &OsStr, line: &str) -> std::io::Result<bool> {
    use std::io::{Read as _, Write as _};
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(path)?;
    let mut text = Vec::new();
    (&file).take(1 << 20).read_to_end(&mut text)?;
    if text.split(|b| *b == b'\n').any(|l| l == line.as_bytes()) {
        return Ok(false);
    }
    // A file that does not end in a newline must not have this glued to its
    // last line.
    let mut out = Vec::with_capacity(line.len() + 2);
    if text.last().is_some_and(|b| *b != b'\n') {
        out.push(b'\n');
    }
    out.extend_from_slice(line.as_bytes());
    out.push(b'\n');
    file.write_all(&out)?;
    Ok(true)
}

#[cfg(test)]
#[path = "lease_wrapper_tests.rs"]
mod tests;
