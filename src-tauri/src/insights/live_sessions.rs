//! Live agent sessions, and the worktree each one is working in.
//!
//! Worktree layout (`.claude/worktrees/<slug>`) says where an agent *was
//! given* a checkout; it says nothing about how many agents are running in
//! one. Two sessions in the same checkout share one layout label — or none,
//! in the main checkout — so a collision scan that only compares worktrees
//! reads them as clean. This module answers the other half: which agent
//! processes are alive right now, and which worktree each one's directory
//! sits in.
//!
//! Sessions are found by the host-load process scan
//! (`sysload::procscan`): a Claude Code or Codex process is recognised by its
//! executable, wherever it was started and whichever `CLAUDE_CONFIG_DIR` it
//! registered under. Claude Code's own registry (`<config>/sessions/*.json`)
//! only adds what the process table cannot say — the entrypoint and the
//! agent's last-reported status. Counting from that registry undercounted:
//! a second account's config directory, or a session that never wrote an
//! entry, was invisible (5 of 7 live sessions counted, 2026-10-09).
//!
//! Each kind reports whether it could look. Where there is no process scan
//! (Windows) the registry is the detector again, and a kind with no detector
//! is reported as unknown, never as zero sessions: "we cannot see Codex
//! sessions" and "no Codex session is running" are different facts.

use super::{LiveKindStatus, LiveSession, LiveSessionFacet, LiveSessionNode, LiveWorktree};
use crate::alerts::{HubSessionState, HubStateRead, HubStateSnapshot};
use crate::workbench::process_birth::{self, Liveness};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Registry entries one probe will read. Claude Code prunes its registry as
/// sessions exit, so a real one holds tens of entries; past this the kind is
/// reported `truncated` rather than silently sampled.
const MAX_REGISTRY_ENTRIES: usize = 1024;
/// Largest registry entry read. A real one is well under 1 KiB.
const MAX_ENTRY_BYTES: u64 = 64 * 1024;
/// How far a process's kernel-recorded birth may trail the wall-clock
/// `startedAt` its session wrote and still be the same process.
///
/// The two come from different clocks — the kernel derives start time from
/// boot time, the agent reads the wall clock — and they disagree by
/// milliseconds. Without slack a session that registered the instant it
/// started reads as a pid reuse, which turns a live session into "gone": the
/// unsafe direction. A pid reused within these seconds is still missed, but
/// only a session that died and had its pid recycled that fast.
const BIRTH_CLOCK_SLACK_NANOS: u128 = 5_000_000_000;

/// Agent kinds the registry fallback (a platform with no process scan) has no
/// way to observe.
///
/// Codex keeps rollout logs under `~/.codex/sessions`, which record that a
/// session happened, not that it is running or where. Listing it here is
/// what makes its absence from the fallback's counts an explicit "unknown".
const UNOBSERVABLE_KINDS: &[(&str, &str)] = &[
    (
        "codex",
        "Codex keeps no registry of running sessions that GitPulse can read, so its live sessions cannot be counted",
    ),
    (
        "antigravity",
        "Antigravity keeps no registry of running sessions that GitPulse can read, so its live sessions cannot be counted",
    ),
    (
        "devcouncil",
        "DevCouncil keeps no registry of running sessions that GitPulse can read, so its live sessions cannot be counted",
    ),
    (
        "manvi",
        "Manvi keeps no registry of running sessions that GitPulse can read, so its live sessions cannot be counted",
    ),
];

/// What one detector saw, before attribution to any repository.
#[derive(Debug, Clone, Default)]
pub(crate) struct KindObservation {
    pub kind: String,
    /// Whether the detector could look at all.
    pub ok: bool,
    pub error: String,
    /// Sessions established to be running.
    pub live: Vec<LiveSession>,
    /// Entries that could not be read, or whose process could not be judged
    /// alive or gone. The cwd is kept when it was readable, so attribution
    /// can still say "possibly in this worktree"; `None` could be anywhere.
    pub unverified: Vec<Option<String>>,
    /// True when the registry held more entries than one probe reads.
    pub truncated: bool,
}

/// Every detector's observation. Repository-independent, so one probe can
/// serve the agents facet and the collision scan of the same snapshot.
pub(crate) type Observation = Vec<KindObservation>;

/// One entry of Claude Code's session registry, `<config>/sessions/<pid>.json`.
///
/// Only the fields read here are declared. The registry belongs to Claude
/// Code, so an entry missing `pid` or `cwd` is unreadable — counted as
/// unverified — rather than guessed at.
#[derive(Deserialize)]
struct ClaudeRegistryEntry {
    pid: u32,
    cwd: String,
    /// Milliseconds since the Unix epoch when the session registered. A
    /// process holding `pid` that started after this is a reuse of the pid.
    #[serde(rename = "startedAt")]
    started_at: u64,
    #[serde(default)]
    entrypoint: String,
    #[serde(default)]
    status: String,
    #[serde(default, rename = "sessionId")]
    session_id: String,
    #[serde(default, rename = "procStart")]
    proc_start: String,
}

/// Observes every agent kind on this machine.
pub(crate) fn observe() -> Observation {
    let configs = crate::workbench::conversation::claude_config_dirs();
    let registries: Vec<PathBuf> = configs
        .dirs
        .iter()
        .map(|dir| dir.join("sessions"))
        .collect();
    match crate::sysload::procscan::observe_sessions_with_caller() {
        Ok(scan) => from_scan_with(&scan.sessions, scan.caller, &registries),
        // A scan that exists and failed is a failed detector; falling back to
        // the registry would report its undercount as if it were the answer.
        Err(error) if crate::sysload::procscan::SCAN_SUPPORTED => failed_scan(&error),
        Err(_) => observe_registries(
            &registries,
            configs.error.as_deref(),
            &process_birth::running_since,
        ),
    }
}

/// The kinds the process scan observes.
const SCANNED_KINDS: [crate::sysload::AgentKind; 5] = [
    crate::sysload::AgentKind::Claude,
    crate::sysload::AgentKind::Codex,
    crate::sysload::AgentKind::Antigravity,
    crate::sysload::AgentKind::DevCouncil,
    crate::sysload::AgentKind::Manvi,
];

/// Turns the process scan's sessions into per-kind observations, enriched
/// from Claude Code's registries at `claude_sessions` (one per config
/// directory) where an entry names the same process.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn from_scan(
    sessions: &[crate::sysload::AgentSession],
    claude_sessions: &[PathBuf],
) -> Observation {
    from_scan_with(sessions, None, claude_sessions)
}

pub(crate) fn from_scan_with(
    sessions: &[crate::sysload::AgentSession],
    caller: Option<crate::sysload::CallerIdentity>,
    claude_sessions: &[PathBuf],
) -> Observation {
    let reg = read_claude_registry_snapshot(claude_sessions, None);
    from_scan_snapshot(sessions, caller, &reg)
}

fn from_scan_snapshot(
    sessions: &[crate::sysload::AgentSession],
    caller: Option<crate::sysload::CallerIdentity>,
    reg: &ClaudeRegistrySnapshot,
) -> Observation {
    let mut kinds: Vec<KindObservation> = SCANNED_KINDS
        .iter()
        .map(|kind| KindObservation {
            kind: kind.name().into(),
            ok: true,
            ..KindObservation::default()
        })
        .collect();
    for session in sessions {
        let Some(seen) = kinds.iter_mut().find(|k| k.kind == session.kind.name()) else {
            continue;
        };
        let (Ok(pid), Some(cwd)) = (u32::try_from(session.pid), session.cwd.as_ref()) else {
            // Running, but where is unknown: possibly in any repository.
            seen.unverified.push(None);
            continue;
        };
        // The entry describes this process only if the process was already
        // running when the entry was written; a later process on the same pid
        // is a reuse and inherits nothing from it.
        let entry = reg.matching_entry(pid, session.start_us).map(|r| &r.entry);
        let is_caller =
            caller.is_some_and(|c| c.pid == session.pid && c.start_us == session.start_us);
        seen.live.push(LiveSession {
            kind: session.kind.name().into(),
            pid,
            entrypoint: entry.map(|e| e.entrypoint.clone()).unwrap_or_default(),
            status: entry.map(|e| e.status.clone()).unwrap_or_default(),
            cwd: cwd.clone(),
            tree: session.tree.clone(),
            run_id: session.run_id.clone(),
            parent_pid: session.parent_pid.and_then(|pid| u32::try_from(pid).ok()),
            session_id: entry.and_then(|e| nonempty(&e.session_id)),
            proc_start: entry.and_then(|e| nonempty(&e.proc_start)),
            registry_cwd: entry.map(|e| e.cwd.clone()),
            start_us: (session.start_us > 0).then_some(session.start_us),
            is_caller,
            ide_hosted: session.ide_hosted,
            terminal_session_id: session.terminal_session_id.clone(),
        });
    }
    kinds
}

fn failed_scan(error: &str) -> Observation {
    SCANNED_KINDS
        .iter()
        .map(|kind| KindObservation {
            kind: kind.name().into(),
            error: format!("the process table could not be scanned: {error}"),
            ..KindObservation::default()
        })
        .collect()
}

struct ClaudeRegistryRecord {
    entry: ClaudeRegistryEntry,
    mtime_ms: Option<u64>,
}

#[derive(Default)]
struct ClaudeRegistrySnapshot {
    entries: HashMap<u32, ClaudeRegistryRecord>,
    corrupt_pids: HashSet<u32>,
    has_unkeyed_corrupt: bool,
    errors: Vec<String>,
}

impl ClaudeRegistrySnapshot {
    fn matching_entry(&self, pid: u32, start_us: u64) -> Option<&ClaudeRegistryRecord> {
        self.entries.get(&pid).filter(|rec| {
            u128::from(start_us) * 1_000
                <= u128::from(rec.entry.started_at) * 1_000_000 + BIRTH_CLOCK_SLACK_NANOS
        })
    }

    fn has_corruption_for(&self, pid: u32) -> bool {
        self.has_unkeyed_corrupt || self.corrupt_pids.contains(&pid) || !self.errors.is_empty()
    }

    fn degraded_error(&self) -> Option<String> {
        if !self.errors.is_empty() {
            return Some(self.errors.join("; "));
        }
        if !self.corrupt_pids.is_empty() || self.has_unkeyed_corrupt {
            return Some(
                "one or more Claude session registry entries are unreadable or corrupt".into(),
            );
        }
        None
    }
}

fn system_time_ms(ts: SystemTime) -> Option<u64> {
    ts.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
}

fn read_claude_registry_snapshot(
    dirs: &[PathBuf],
    discovery_error: Option<&str>,
) -> ClaudeRegistrySnapshot {
    let mut out = ClaudeRegistrySnapshot::default();
    if let Some(error) = discovery_error {
        out.errors.push(error.to_owned());
    }
    for dir in dirs {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                out.errors
                    .push(format!("cannot read {}: {error}", dir.display()));
                continue;
            }
        };
        let mut paths = Vec::new();
        for item in entries {
            match item {
                Ok(entry) => {
                    let path = entry.path();
                    if path.extension().is_some_and(|ext| ext == "json") {
                        paths.push(path);
                    }
                }
                Err(error) => {
                    out.errors
                        .push(format!("cannot list {}: {error}", dir.display()));
                    break;
                }
            }
        }
        paths.sort();
        if paths.len() > MAX_REGISTRY_ENTRIES {
            out.errors.push(format!(
                "{} holds more than {MAX_REGISTRY_ENTRIES} entries; truncated",
                dir.display()
            ));
            paths.truncate(MAX_REGISTRY_ENTRIES);
        }
        for path in paths {
            match read_entry(&path) {
                Ok(parsed) => {
                    let mtime_ms = std::fs::metadata(&path)
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(system_time_ms);
                    let replace = out
                        .entries
                        .get(&parsed.pid)
                        .is_none_or(|prev| parsed.started_at >= prev.entry.started_at);
                    if replace {
                        out.entries.insert(
                            parsed.pid,
                            ClaudeRegistryRecord {
                                entry: parsed,
                                mtime_ms,
                            },
                        );
                    }
                }
                Err(_) => {
                    let parsed_pid = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .and_then(|s| s.parse::<u32>().ok());
                    if let Some(pid) = parsed_pid {
                        out.corrupt_pids.insert(pid);
                    } else {
                        out.has_unkeyed_corrupt = true;
                    }
                }
            }
        }
    }
    out
}

/// [`observe`]'s registry fallback, for a platform with no process scan, with
/// one registry directory and the liveness probe as arguments, so tests can
/// point it at a fixture registry.
#[cfg(test)]
pub(crate) fn observe_with(
    claude_sessions: Option<&Path>,
    alive: &dyn Fn(u32, u128) -> Liveness,
) -> Observation {
    let dirs: Vec<PathBuf> = claude_sessions.map(Path::to_path_buf).into_iter().collect();
    observe_registries(&dirs, None, alive)
}

/// Observes every agent kind, reading Claude Code's registry from each of
/// `claude_sessions` and merging them into one `claude` observation.
///
/// The merge keeps the strictest reading: the kind is `ok` only when every
/// directory was read, entries count as truncated if any directory was, and
/// `discovery_error` (the config-directory list itself could not be completed)
/// fails the kind so a missed directory never reads as a clean zero.
pub(crate) fn observe_registries(
    claude_sessions: &[PathBuf],
    discovery_error: Option<&str>,
    alive: &dyn Fn(u32, u128) -> Liveness,
) -> Observation {
    let mut claude = KindObservation {
        kind: "claude".into(),
        ok: true,
        ..KindObservation::default()
    };
    if claude_sessions.is_empty() {
        claude.ok = false;
        claude.error = "no home directory to find Claude Code's session registry in".into();
    }
    let mut errors: Vec<String> = discovery_error.map(str::to_string).into_iter().collect();
    for dir in claude_sessions {
        let one = read_claude_registry(dir, alive);
        claude.ok &= one.ok;
        if !one.error.is_empty() {
            errors.push(one.error);
        }
        claude.truncated |= one.truncated;
        for session in one.live {
            // One session registers in one directory; a repeat is the same
            // directory reached through two spellings.
            if !claude
                .live
                .iter()
                .any(|s| s.pid == session.pid && s.cwd == session.cwd)
            {
                claude.live.push(session);
            }
        }
        claude.unverified.extend(one.unverified);
    }
    if discovery_error.is_some() {
        claude.ok = false;
    }
    if !errors.is_empty() {
        claude.error = errors.join("; ");
    }
    let mut kinds = vec![claude];
    for (kind, why) in UNOBSERVABLE_KINDS {
        kinds.push(KindObservation {
            kind: (*kind).into(),
            error: (*why).into(),
            ..KindObservation::default()
        });
    }
    kinds
}

fn read_claude_registry(dir: &Path, alive: &dyn Fn(u32, u128) -> Liveness) -> KindObservation {
    let mut seen = KindObservation {
        kind: "claude".into(),
        ..KindObservation::default()
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        // No registry is what a machine that never ran Claude Code has: the
        // detector looked, and there is nothing running.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            seen.ok = true;
            return seen;
        }
        Err(error) => {
            seen.error = format!("cannot read {}: {error}", dir.display());
            return seen;
        }
    };
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "json") {
                    paths.push(path);
                }
            }
            Err(error) => {
                seen.error = format!("cannot list {}: {error}", dir.display());
                return seen;
            }
        }
    }
    paths.sort();
    if paths.len() > MAX_REGISTRY_ENTRIES {
        seen.truncated = true;
        paths.truncate(MAX_REGISTRY_ENTRIES);
    }
    for path in paths {
        let entry = match read_entry(&path) {
            Ok(entry) => entry,
            Err(_) => {
                seen.unverified.push(None);
                continue;
            }
        };
        let instant = u128::from(entry.started_at) * 1_000_000 + BIRTH_CLOCK_SLACK_NANOS;
        match alive(entry.pid, instant) {
            Liveness::Alive => seen.live.push(LiveSession {
                kind: "claude".into(),
                pid: entry.pid,
                entrypoint: entry.entrypoint,
                status: entry.status,
                cwd: entry.cwd.clone(),
                // A registry entry says nothing about launch or lineage.
                tree: None,
                run_id: None,
                parent_pid: None,
                session_id: nonempty(&entry.session_id),
                proc_start: nonempty(&entry.proc_start),
                registry_cwd: Some(entry.cwd),
                start_us: None,
                is_caller: false,
                ide_hosted: false,
                terminal_session_id: None,
            }),
            // Claude Code removes an entry when its session exits cleanly; one
            // left behind by a crash names a process that is gone.
            Liveness::Gone => {}
            Liveness::Unknown(_) => seen.unverified.push(Some(entry.cwd)),
        }
    }
    seen.ok = true;
    seen
}

fn read_entry(path: &Path) -> Result<ClaudeRegistryEntry, String> {
    use std::io::Read;
    let mut text = String::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(MAX_ENTRY_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() as u64 > MAX_ENTRY_BYTES {
        return Err("registry entry exceeds its size limit".into());
    }
    let entry: ClaudeRegistryEntry = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if entry.pid == 0 || !Path::new(&entry.cwd).is_absolute() {
        return Err("registry entry has no usable pid or cwd".into());
    }
    Ok(entry)
}

fn nonempty(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// The canonical spelling of a path when it exists, else the path as given.
///
/// Both sides of a containment test go through this, so `/tmp` and
/// `/private/tmp` meet; a directory that no longer exists keeps its raw
/// spelling and can still match a worktree listed the same way.
fn comparable(path: &str) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path))
}

/// The worktree among `worktrees` that contains `cwd`, choosing the deepest
/// when they nest — a linked worktree under `.claude/worktrees/` lives inside
/// the main checkout's directory, and belongs to the linked one.
fn owning_worktree<'a>(cwd: &Path, worktrees: &'a [(String, PathBuf)]) -> Option<&'a str> {
    worktrees
        .iter()
        .filter(|(_, root)| cwd.starts_with(root))
        .max_by_key(|(_, root)| root.components().count())
        .map(|(path, _)| path.as_str())
}

/// A worktree lock whose pid and start identify the session git assigned
/// the checkout to. A pid with no start is not a match: a reused pid must
/// not inherit a lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorktreeLock {
    pub worktree: String,
    pub pid: u32,
    pub start_text: Option<String>,
}

/// Attributes an observation to the worktrees of one repository.
///
/// `worktree_paths` is the repository's worktree listing, spelled as git
/// listed it; the facet reports paths in that spelling. A session whose
/// directory is in none of them belongs to another repository and is not
/// counted here.
///
/// Ownership, first match wins: the worktree lock's pid and start, then the
/// cwd the edit hook last claimed for the session, then the registry cwd,
/// then the process cwd. Isolation worktrees keep the process inside the
/// parent checkout, so the process cwd alone files that session on main.
pub(crate) fn attribute(observation: &Observation, worktree_paths: &[String]) -> LiveSessionFacet {
    let locks = read_worktree_locks(worktree_paths);
    let (claims, problems) = load_claims(worktree_paths);
    let mut facet = attribute_with(observation, worktree_paths, &locks, &claims);
    augment_ide_peers(worktree_paths, &mut facet);
    if !problems.is_empty() {
        // Attribution fell back to the registry or process cwd for whatever
        // the claims would have said, so the facet is not "observed
        // completely and verifiably".
        facet.ok = false;
        facet.attribution_error = format!("hook claims not read in full: {}", problems.join("; "));
    }
    facet
}

const PEERS_MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Deserialize)]
struct IdePeersFile {
    #[serde(default)]
    peers: Vec<IdePeerRecord>,
}

#[derive(Deserialize)]
struct IdePeerRecord {
    #[serde(default)]
    provider: String,
    #[serde(default)]
    surface: String,
    #[serde(default)]
    provider_session_id: Option<String>,
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    worktree_path: String,
    #[serde(default)]
    turn_state: String,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    proc_start_ns: Option<u64>,
}

/// Supplements `facet` with live Antigravity IDE sessions recorded in
/// `<git-common-dir>/gitpulse/lanes/peers.json` whose presence lock is actively
/// held (or whose `(pid, proc_start_ns)` is alive and `turn_state != "exited"`).
/// Strictly read-only: never takes `.bus.lock` or writes any files.
pub(crate) fn augment_ide_peers(worktree_paths: &[String], facet: &mut LiveSessionFacet) {
    let agy_homes = crate::workbench::conversation::antigravity_homes();
    augment_ide_peers_in(
        worktree_paths,
        &agy_homes,
        &process_birth::running_since,
        facet,
    );
}

pub(crate) fn augment_ide_peers_in(
    worktree_paths: &[String],
    agy_homes: &[PathBuf],
    alive_probe: &dyn Fn(u32, u128) -> Liveness,
    facet: &mut LiveSessionFacet,
) {
    use std::io::Read;

    if worktree_paths.is_empty() {
        return;
    }
    let roots: Vec<(String, PathBuf)> = worktree_paths
        .iter()
        .map(|p| (p.clone(), comparable(p)))
        .collect();

    let mut seen_common = HashSet::new();
    let mut existing_sids: HashSet<String> = HashSet::new();
    let mut existing_pids: HashSet<u32> = HashSet::new();
    for wt in &facet.worktrees {
        for s in &wt.sessions {
            if s.pid > 0 {
                existing_pids.insert(s.pid);
            }
            if let Some(ref sid) = s.session_id {
                existing_sids.insert(sid.to_ascii_lowercase());
            }
        }
    }

    let mut added = 0u32;
    let mut touched_worktrees: HashSet<String> = HashSet::new();

    for path in worktree_paths {
        let Ok(common) = crate::engine::git_cli::resolve_git_common_dir(Path::new(path)) else {
            continue;
        };
        if !seen_common.insert(common.clone()) {
            continue;
        }
        if !matches!(super::claims::admitted(Path::new(path)), Ok(true)) {
            continue;
        }
        let peers_path = common.join("gitpulse").join("lanes").join("peers.json");
        let Ok(file) = std::fs::File::open(&peers_path) else {
            continue;
        };
        let mut buf = Vec::new();
        if file
            .take(PEERS_MAX_BYTES + 1)
            .read_to_end(&mut buf)
            .is_err()
        {
            continue;
        }
        if buf.len() as u64 > PEERS_MAX_BYTES || buf.is_empty() {
            continue;
        }
        let Ok(parsed) = serde_json::from_slice::<IdePeersFile>(&buf) else {
            continue;
        };
        for peer in parsed.peers {
            let provider = crate::engine::agent_bus::canonical_provider(&peer.provider);
            let surface = crate::engine::agent_bus::canonical_surface(&peer.surface);
            if provider != "agy" || surface != "ide" {
                continue;
            }
            if peer.worktree_path.trim().is_empty() {
                continue;
            }
            let peer_wt_comp = comparable(&peer.worktree_path);
            let Some(owner_wt) = owning_worktree(&peer_wt_comp, &roots) else {
                continue;
            };
            let sid_lower = peer
                .provider_session_id
                .as_deref()
                .map(str::trim)
                .filter(|s| crate::workbench::terminal_command::is_canonical_uuid(s))
                .map(|s| s.to_ascii_lowercase());
            let presence_locked = sid_lower.as_deref().is_some_and(|sid| {
                crate::engine::agent_bus::is_antigravity_presence_locked(agy_homes, sid)
            });
            let pid_alive = peer.turn_state != "exited"
                && match (peer.pid, peer.proc_start_ns) {
                    (Some(pid), Some(start_ns)) => {
                        matches!(alive_probe(pid, u128::from(start_ns)), Liveness::Alive)
                    }
                    _ => false,
                };
            if !presence_locked && !pid_alive {
                continue;
            }
            if let Some(ref sid) = sid_lower {
                if existing_sids.contains(sid) {
                    continue;
                }
                existing_sids.insert(sid.clone());
            } else if let Some(pid) = peer.pid.filter(|&p| p > 0) {
                if existing_pids.contains(&pid) {
                    continue;
                }
                existing_pids.insert(pid);
            }

            let owner_wt_owned = owner_wt.to_string();
            let session = LiveSession {
                kind: "antigravity".to_string(),
                pid: peer.pid.unwrap_or(0),
                entrypoint: "ide".to_string(),
                status: if peer.turn_state.trim().is_empty() {
                    "idle".to_string()
                } else {
                    peer.turn_state.clone()
                },
                cwd: owner_wt_owned.clone(),
                tree: None,
                run_id: peer.run_id.clone(),
                parent_pid: None,
                session_id: sid_lower,
                proc_start: None,
                registry_cwd: Some(peer.worktree_path.clone()),
                start_us: None,
                is_caller: false,
                ide_hosted: true,
                terminal_session_id: None,
            };
            touched_worktrees.insert(owner_wt_owned.clone());
            match facet
                .worktrees
                .iter_mut()
                .find(|w| w.path == owner_wt_owned)
            {
                Some(row) => row.sessions.push(session),
                None => facet.worktrees.push(LiveWorktree {
                    path: owner_wt_owned,
                    sessions: vec![session],
                    children: Vec::new(),
                    children_truncated: false,
                }),
            }
            added += 1;
        }
    }

    if added > 0 {
        for row in &mut facet.worktrees {
            if touched_worktrees.contains(&row.path) {
                row.sessions.sort_by_key(|s| s.pid);
                (row.children, row.children_truncated) = nest(&row.sessions);
            }
        }
        facet.worktrees.sort_by(|a, b| a.path.cmp(&b.path));
        if let Some(kind) = facet.kinds.iter_mut().find(|k| k.kind == "antigravity") {
            kind.sessions += added;
        } else {
            facet.kinds.push(LiveKindStatus {
                kind: "antigravity".to_string(),
                ok: true,
                error: String::new(),
                sessions: added,
                unverified: 0,
                truncated: false,
            });
        }
        facet.sessions = facet.kinds.iter().map(|k| k.sessions).sum();
    }
}

pub(crate) fn attribute_with(
    observation: &Observation,
    worktree_paths: &[String],
    locks: &[WorktreeLock],
    claims: &[super::claims::FileClaim],
) -> LiveSessionFacet {
    let roots: Vec<(String, PathBuf)> = worktree_paths
        .iter()
        .map(|path| (path.clone(), comparable(path)))
        .collect();
    let mut worktrees: Vec<LiveWorktree> = Vec::new();
    let mut kinds = Vec::new();
    for seen in observation {
        let mut sessions = 0u32;
        for session in &seen.live {
            let Some(owner) = owner_of(session, &roots, locks, claims) else {
                continue;
            };
            sessions += 1;
            match worktrees.iter_mut().find(|w| w.path == owner) {
                Some(row) => row.sessions.push(session.clone()),
                None => worktrees.push(LiveWorktree {
                    path: owner.to_string(),
                    sessions: vec![session.clone()],
                    children: Vec::new(),
                    children_truncated: false,
                }),
            }
        }
        // An entry with no readable cwd could be in any repository, so it is
        // unverified for every one of them.
        let unverified = seen
            .unverified
            .iter()
            .filter(|cwd| match cwd {
                Some(cwd) => owning_worktree(&comparable(cwd), &roots).is_some(),
                None => true,
            })
            .count() as u32;
        kinds.push(LiveKindStatus {
            kind: seen.kind.clone(),
            ok: seen.ok,
            error: seen.error.clone(),
            sessions,
            unverified,
            truncated: seen.truncated,
        });
    }
    for row in &mut worktrees {
        row.sessions.sort_by_key(|s| s.pid);
        (row.children, row.children_truncated) = nest(&row.sessions);
    }
    worktrees.sort_by(|a, b| a.path.cmp(&b.path));
    let sessions = kinds.iter().map(|k| k.sessions).sum();
    LiveSessionFacet {
        ok: kinds
            .iter()
            .all(|k| k.ok && k.unverified == 0 && !k.truncated),
        sessions,
        kinds,
        worktrees,
        attribution_error: String::new(),
    }
}

fn owner_of<'a>(
    session: &LiveSession,
    roots: &'a [(String, PathBuf)],
    locks: &[WorktreeLock],
    claims: &[super::claims::FileClaim],
) -> Option<&'a str> {
    // One parent CLI that started several isolation agents has one pid and one
    // start across all their locks, so "the lock names this session" is true of
    // every lane at once. That is no evidence for any one of them: the session
    // is not moved, and the rest of the order decides.
    let mut matching = locks.iter().filter(|lock| lock_matches(session, lock));
    if let (Some(locked), None) = (matching.next(), matching.next()) {
        if let Some((path, _)) = roots.iter().find(|(path, _)| path == &locked.worktree) {
            return Some(path.as_str());
        }
    }
    if let Some(cwd) = session
        .session_id
        .as_deref()
        .and_then(|id| super::claims::last_claimed_cwd(claims, id))
    {
        if let Some(owner) = owning_worktree(&comparable(cwd), roots) {
            return Some(owner);
        }
    }
    if let Some(cwd) = session.registry_cwd.as_deref() {
        if Path::new(cwd).is_absolute() {
            if let Some(owner) = owning_worktree(&comparable(cwd), roots) {
                return Some(owner);
            }
        }
    }
    owning_worktree(&comparable(&session.cwd), roots)
}

fn lock_matches(session: &LiveSession, lock: &WorktreeLock) -> bool {
    if lock.pid != session.pid {
        return false;
    }
    match (&lock.start_text, &session.proc_start) {
        (Some(lock_start), Some(session_start)) => lock_start == session_start,
        _ => false,
    }
}

/// Lock files of the listed worktrees. A linked worktree's `.git` is a
/// `gitdir:` pointer; the lock is `<admin>/locked`. A missing file is an
/// unlocked checkout, not an error. At most one lock is read per worktree.
pub(crate) fn read_worktree_locks(worktree_paths: &[String]) -> Vec<WorktreeLock> {
    let mut locks = Vec::new();
    for path in worktree_paths.iter().take(64) {
        let Some(admin) = gitdir_admin(Path::new(path)) else {
            continue;
        };
        let Ok(text) = read_small(&admin.join("locked"), 1024) else {
            continue;
        };
        if let Some((pid, start_text)) = parse_lock_reason(&text) {
            if start_text.is_none() {
                continue;
            }
            locks.push(WorktreeLock {
                worktree: path.clone(),
                pid,
                start_text,
            });
        }
    }
    locks
}

fn gitdir_admin(worktree: &Path) -> Option<PathBuf> {
    let git = worktree.join(".git");
    let meta = fs_meta(&git)?;
    if !meta.is_file() {
        return None;
    }
    let text = read_small(&git, 4096).ok()?;
    let line = text.lines().next()?.trim();
    let rest = line.strip_prefix("gitdir:")?.trim();
    if rest.is_empty() || rest.contains('\0') {
        return None;
    }
    let pointed = PathBuf::from(rest);
    if pointed.is_absolute() {
        Some(pointed)
    } else {
        Some(worktree.join(pointed))
    }
}

fn fs_meta(path: &Path) -> Option<std::fs::Metadata> {
    std::fs::symlink_metadata(path).ok()
}

fn read_small(path: &Path, cap: u64) -> Result<String, ()> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|_| ())?;
    let mut buf = String::new();
    file.take(cap + 1)
        .read_to_string(&mut buf)
        .map_err(|_| ())?;
    if buf.len() as u64 > cap {
        return Err(());
    }
    Ok(buf)
}

/// Claude Code writes `claude agent|session <id> (pid N start <procStart>)`.
/// A pid with no start is parsed and then ignored: it cannot be checked
/// against pid reuse. The structured `pid=<n> start_us=<n>` form is accepted
/// so a test can name a start without inventing a date string; it is stored
/// as text and matched against `proc_start` the same way.
pub(crate) fn parse_lock_reason(text: &str) -> Option<(u32, Option<String>)> {
    let line = text.lines().next()?.trim();
    if let Some(rest) = line.strip_prefix("pid=") {
        let (pid, start) = rest.split_once(" start_us=")?;
        let pid: u32 = pid.parse().ok()?;
        if start.is_empty() || !start.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        return Some((pid, Some(format!("start_us:{start}"))));
    }
    let rest = line
        .strip_prefix("claude agent ")
        .or_else(|| line.strip_prefix("claude session "))?;
    let marker = " (pid ";
    let at = rest.rfind(marker)?;
    if at == 0 || at > 255 {
        return None;
    }
    let after = rest[at + marker.len()..].strip_suffix(')')?;
    let (pid_text, start) = match after.split_once(" start ") {
        Some((pid, start)) if !start.is_empty() && start.len() <= 255 => {
            (pid, Some(start.to_string()))
        }
        Some(_) => return None,
        None => (after, None),
    };
    if pid_text.is_empty() || pid_text.len() > 10 || !pid_text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((pid_text.parse().ok()?, start))
}

/// The hook claims of every repository in `worktree_paths`, and why any of them
/// could not be read in full. A repository GitPulse does not trust has no
/// claims to read; a store that failed, or was read with unreadable lines
/// skipped, is a problem the caller reports, never an empty list.
fn load_claims(worktree_paths: &[String]) -> (Vec<super::claims::FileClaim>, Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    let mut problems = Vec::new();
    let now = super::claims::now_ms();
    for path in worktree_paths.iter().take(16) {
        let Ok(common) = crate::engine::git_cli::resolve_git_common_dir(Path::new(path)) else {
            // A listed worktree whose git dir cannot be resolved (pruned, or
            // gone from disk) has no store of its own to read.
            continue;
        };
        if !seen.insert(common.clone()) {
            continue;
        }
        match super::claims::admitted(Path::new(path)) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => {
                problems.push(format!("trust for {path} could not be read ({error})"));
                continue;
            }
        }
        match super::claims::read_store(
            &super::claims::store_path(&common),
            now,
            super::claims::DEFAULT_TTL_MS,
            super::claims::DEFAULT_CAP,
        ) {
            Ok(read) => {
                if let Some(degraded) = read.degraded() {
                    problems.push(format!("{}: {degraded}", common.display()));
                }
                rows.extend(read.rows);
            }
            Err(error) => problems.push(error),
        }
    }
    (rows, problems)
}

/// How deep [`nest`] descends. Real agent trees are a few levels (Manvi bounds
/// its own spawn depth); the bound keeps the output's recursion shallow for the
/// readers that parse it, and a tree past it says so.
pub(crate) const MAX_NEST_DEPTH: usize = 16;

/// The sessions of one worktree that sit under another session of the same
/// worktree, nested under it, and whether the depth bound cut any.
///
/// A root is a session with at least one child here. A parent that is not in
/// `sessions` (another worktree, another repository, a pid the scan did not
/// keep) leaves its child a root of its own rather than a node of anything,
/// and a parent chain that loops back on itself is entered once, at the
/// lowest pid, and cut where it would repeat — so every session appears at
/// most once and the walk ends.
fn nest(sessions: &[LiveSession]) -> (Vec<LiveSessionNode>, bool) {
    let index: std::collections::HashMap<u32, usize> = sessions
        .iter()
        .enumerate()
        .map(|(at, session)| (session.pid, at))
        .collect();
    let mut kids: Vec<Vec<usize>> = vec![Vec::new(); sessions.len()];
    let mut has_parent = vec![false; sessions.len()];
    for (at, session) in sessions.iter().enumerate() {
        let Some(&parent) = session.parent_pid.and_then(|pid| index.get(&pid)) else {
            continue;
        };
        if parent == at {
            continue;
        }
        kids[parent].push(at);
        has_parent[at] = true;
    }
    let mut placed = vec![false; sessions.len()];
    let mut truncated = false;
    let mut forest = Vec::new();
    let order = (0..sessions.len())
        .filter(|at| !has_parent[*at])
        .chain((0..sessions.len()).filter(|at| has_parent[*at]));
    for root in order {
        if placed[root] || kids[root].is_empty() {
            continue;
        }
        forest.push(node(root, 1, sessions, &kids, &mut placed, &mut truncated));
    }
    (forest, truncated)
}

fn node(
    at: usize,
    depth: usize,
    sessions: &[LiveSession],
    kids: &[Vec<usize>],
    placed: &mut [bool],
    truncated: &mut bool,
) -> LiveSessionNode {
    placed[at] = true;
    let mut children = Vec::new();
    for &child in &kids[at] {
        if placed[child] {
            continue;
        }
        if depth >= MAX_NEST_DEPTH {
            // Cut here, and keep the rest of the subtree from resurfacing as
            // a root of its own further on.
            *truncated = true;
            hide(child, kids, placed);
            continue;
        }
        children.push(node(child, depth + 1, sessions, kids, placed, truncated));
    }
    LiveSessionNode {
        pid: sessions[at].pid,
        kind: sessions[at].kind.clone(),
        children,
    }
}

/// Marks `at` and everything under it placed without emitting it.
fn hide(at: usize, kids: &[Vec<usize>], placed: &mut [bool]) {
    let mut stack = vec![at];
    while let Some(next) = stack.pop() {
        if std::mem::replace(&mut placed[next], true) {
            continue;
        }
        stack.extend(kids[next].iter().copied());
    }
}

/// The facet for a repository whose worktree listing failed: nothing can be
/// attributed, so every kind is reported as not looked at.
pub(crate) fn unattributed(error: &str) -> LiveSessionFacet {
    let kinds = SCANNED_KINDS
        .iter()
        .map(|kind| kind.name())
        .map(|kind| LiveKindStatus {
            kind: kind.into(),
            ok: false,
            error: error.to_string(),
            sessions: 0,
            unverified: 0,
            truncated: false,
        })
        .collect();
    LiveSessionFacet {
        ok: false,
        sessions: 0,
        kinds,
        worktrees: Vec::new(),
        attribution_error: String::new(),
    }
}

/// Default number of rows returned by [`sessions`] when a caller omits `limit`.
pub const DEFAULT_SESSIONS_CAP: u32 = 64;
const MAX_SESSIONS_CAP: u32 = 256;

/// Status of the desktop app's live hook-state snapshot facet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppStateFacet {
    pub ok: bool,
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_ms: Option<u64>,
}

/// One attributed agent session with caller flag, launch metadata, task
/// binding, and sourced execution state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRow {
    pub kind: String,
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_us: Option<u64>,
    pub is_caller: bool,
    pub worktree: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    pub state: String,
    pub state_source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_age_s: Option<u64>,
}

/// Bounded read-only session snapshot for `gitpulse_sessions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionsReport {
    pub repo_path: String,
    pub ok: bool,
    pub error: String,
    pub app_state: AppStateFacet,
    pub kinds: Vec<LiveKindStatus>,
    pub cap: u32,
    pub total: u32,
    pub returned: u32,
    pub truncated: bool,
    pub sessions: Vec<SessionRow>,
}

/// Read-only `gitpulse_sessions` query: process scan + Claude registry +
/// CARD 11 worktree attribution + desktop hook-state snapshot, with zero `git`
/// subprocesses.
pub fn sessions(
    repo_path: &str,
    cap: Option<u32>,
    run_task_lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<SessionsReport, String> {
    let pair = crate::engine::worktree::admit_worktree_pair(repo_path, repo_path)
        .map_err(|e| e.into_message())?;
    let (checkouts, checkouts_error) = match pair.registered_checkouts() {
        Some(paths) => (paths, None),
        None => (
            vec![pair.repo_path().to_path_buf()],
            Some(
                "worktree registry metadata is incomplete; only the requested checkout was checked"
                    .to_string(),
            ),
        ),
    };
    let worktree_paths: Vec<String> = checkouts
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let primary_repo = worktree_paths
        .first()
        .cloned()
        .unwrap_or_else(|| pair.repo_path().to_string_lossy().into_owned());
    let now_ms = super::claims::now_ms();
    let mut problems = Vec::new();
    if let Some(err) = checkouts_error {
        problems.push(err);
    }
    let claim_rows = match super::claims::read_store(
        &super::claims::store_path(pair.common_dir()),
        now_ms,
        super::claims::DEFAULT_TTL_MS,
        super::claims::DEFAULT_CAP,
    ) {
        Ok(read) => {
            if let Some(degraded) = read.degraded() {
                problems.push(format!("{}: {degraded}", pair.common_dir().display()));
            }
            read.rows
        }
        Err(error) => {
            problems.push(error);
            Vec::new()
        }
    };
    let locks = read_worktree_locks(&worktree_paths);
    let configs = crate::workbench::conversation::claude_config_dirs();
    let registries: Vec<PathBuf> = configs
        .dirs
        .iter()
        .map(|dir| dir.join("sessions"))
        .collect();
    let scan = crate::sysload::procscan::observe_sessions_with_caller();
    let reg = read_claude_registry_snapshot(&registries, configs.error.as_deref());
    let hub_read = crate::alerts::read_state_snapshot();
    let ledger_task = |wt: &str| {
        if crate::ledger::is_initialised(&primary_repo) {
            if let Ok(Some((_, Some(tid)))) =
                crate::ledger::newest_binding_readonly(&primary_repo, wt, wt)
            {
                return Some(tid);
            }
        }
        if wt != primary_repo && crate::ledger::is_initialised(wt) {
            if let Ok(Some((_, Some(tid)))) = crate::ledger::newest_binding_readonly(wt, wt, wt) {
                return Some(tid);
            }
        }
        None
    };

    Ok(assemble_sessions_report(
        pair.repo_path().to_string_lossy().into_owned(),
        &worktree_paths,
        &locks,
        &claim_rows,
        problems,
        scan,
        &reg,
        hub_read,
        cap,
        now_ms,
        run_task_lookup,
        &ledger_task,
    ))
}

#[allow(clippy::too_many_arguments)]
fn assemble_sessions_report(
    repo_path: String,
    worktree_paths: &[String],
    locks: &[WorktreeLock],
    claim_rows: &[super::claims::FileClaim],
    mut problems: Vec<String>,
    scan: Result<crate::sysload::SessionScan, String>,
    reg: &ClaudeRegistrySnapshot,
    hub_read: HubStateRead,
    cap: Option<u32>,
    now_ms: u64,
    run_task_lookup: &dyn Fn(&str) -> Option<String>,
    ledger_task_lookup: &dyn Fn(&str) -> Option<String>,
) -> SessionsReport {
    let resolved_cap = cap
        .unwrap_or(DEFAULT_SESSIONS_CAP)
        .clamp(1, MAX_SESSIONS_CAP);
    let (app_state, hub_snap) = match hub_read {
        HubStateRead::Ok(snap) => (
            AppStateFacet {
                ok: true,
                error: String::new(),
                updated_ms: Some(snap.updated_ms),
            },
            Some(snap),
        ),
        HubStateRead::Unavailable(reason) => (
            AppStateFacet {
                ok: false,
                error: reason,
                updated_ms: None,
            },
            None,
        ),
    };

    let obs = match scan {
        Ok(ref scanned) => from_scan_snapshot(&scanned.sessions, scanned.caller, reg),
        Err(ref error) => failed_scan(error),
    };
    let worktrees: Vec<(String, PathBuf)> = worktree_paths
        .iter()
        .map(|p| (p.clone(), comparable(p)))
        .collect();

    let mut kinds = Vec::with_capacity(obs.len());
    let mut attributed: Vec<(String, LiveSession)> = Vec::new();
    for item in obs {
        let mut count: u32 = 0;
        let mut unverified_here: u32 = 0;
        for session in item.live {
            if let Some(wt) = owner_of(&session, &worktrees, locks, claim_rows) {
                count = count.saturating_add(1);
                attributed.push((wt.to_string(), session));
            }
        }
        for cwd in &item.unverified {
            let possible = match cwd {
                Some(dir) => owning_worktree(&comparable(dir), &worktrees).is_some(),
                None => true,
            };
            if possible {
                unverified_here = unverified_here.saturating_add(1);
            }
        }
        let mut kind_ok = item.ok && unverified_here == 0 && !item.truncated;
        let mut kind_error = item.error;
        if item.kind == "claude" {
            if let Some(reg_err) = reg.degraded_error() {
                kind_ok = false;
                if kind_error.is_empty() {
                    kind_error = reg_err;
                } else {
                    kind_error = format!("{kind_error}; {reg_err}");
                }
            }
        }
        if !kind_ok && !kind_error.is_empty() {
            problems.push(format!("{}: {kind_error}", item.kind));
        }
        kinds.push(LiveKindStatus {
            kind: item.kind,
            ok: kind_ok,
            error: kind_error,
            sessions: count,
            unverified: unverified_here,
            truncated: item.truncated,
        });
    }

    let hub_index = hub_snap
        .as_ref()
        .map(|snap| HubIndex::build(snap, &attributed));
    let mut run_task_cache: HashMap<String, Option<String>> = HashMap::new();
    let mut wt_task_cache: HashMap<String, Option<String>> = HashMap::new();

    let mut rows: Vec<SessionRow> = attributed
        .into_iter()
        .map(|(worktree, live)| {
            let (state, state_source, state_age_s) =
                resolve_session_state(&live, reg, hub_index.as_ref(), now_ms);
            let task_id = live
                .run_id
                .as_deref()
                .and_then(|rid| {
                    run_task_cache
                        .entry(rid.to_string())
                        .or_insert_with(|| run_task_lookup(rid))
                        .clone()
                })
                .or_else(|| {
                    wt_task_cache
                        .entry(worktree.clone())
                        .or_insert_with(|| ledger_task_lookup(&worktree))
                        .clone()
                });
            let entrypoint = (!live.entrypoint.is_empty()).then_some(live.entrypoint);
            SessionRow {
                kind: live.kind,
                pid: live.pid,
                start_us: live.start_us,
                is_caller: live.is_caller,
                worktree,
                cwd: live.cwd,
                tree: live.tree,
                run_id: live.run_id,
                task_id,
                parent_pid: live.parent_pid,
                session_id: live.session_id,
                entrypoint,
                state,
                state_source,
                state_age_s,
            }
        })
        .collect();

    rows.sort_by(|a, b| {
        (!a.is_caller)
            .cmp(&(!b.is_caller))
            .then_with(|| a.worktree.cmp(&b.worktree))
            .then_with(|| a.kind.cmp(&b.kind))
            .then_with(|| a.pid.cmp(&b.pid))
    });
    let total = u32::try_from(rows.len()).unwrap_or(u32::MAX);
    rows.truncate(resolved_cap as usize);
    let returned = u32::try_from(rows.len()).unwrap_or(u32::MAX);
    let truncated = total > returned;
    let ok = problems.is_empty() && kinds.iter().all(|k| k.ok);
    let error = problems.join("; ");

    SessionsReport {
        repo_path,
        ok,
        error,
        app_state,
        kinds,
        cap: resolved_cap,
        total,
        returned,
        truncated,
        sessions: rows,
    }
}

struct HubIndex<'a> {
    by_key: HashMap<&'a str, &'a HubSessionState>,
    by_provider_sid: HashMap<&'a str, &'a HubSessionState>,
    by_place: HashMap<PathBuf, Vec<&'a HubSessionState>>,
    live_kind_cwd_counts: HashMap<(String, PathBuf), usize>,
}

impl<'a> HubIndex<'a> {
    fn build(snap: &'a HubStateSnapshot, attributed: &[(String, LiveSession)]) -> Self {
        let mut by_key = HashMap::new();
        let mut by_provider_sid = HashMap::new();
        let mut by_place: HashMap<PathBuf, Vec<&HubSessionState>> = HashMap::new();
        for entry in &snap.sessions {
            by_key.insert(entry.key.as_str(), entry);
            if let Some(sid) = entry
                .provider_session_id
                .as_deref()
                .filter(|s| !s.is_empty())
            {
                by_provider_sid.insert(sid, entry);
            }
            if entry.key.starts_with("hook-") {
                if let Some(place) = entry.place.as_deref().filter(|s| !s.is_empty()) {
                    by_place.entry(comparable(place)).or_default().push(entry);
                }
            }
        }
        let mut live_kind_cwd_counts = HashMap::new();
        for (_, live) in attributed {
            let key = (live.kind.clone(), comparable(&live.cwd));
            *live_kind_cwd_counts.entry(key).or_insert(0) += 1;
        }
        Self {
            by_key,
            by_provider_sid,
            by_place,
            live_kind_cwd_counts,
        }
    }

    fn find(&self, live: &LiveSession) -> Option<&'a HubSessionState> {
        if let Some(term_key) = live.terminal_session_id.as_deref() {
            if let Some(entry) = self.by_key.get(term_key).copied() {
                return Some(entry);
            }
        }
        if let Some(sid) = live.session_id.as_deref() {
            if let Some(entry) = self.by_provider_sid.get(sid).copied() {
                return Some(entry);
            }
        }
        let cwd_canon = comparable(&live.cwd);
        if self
            .live_kind_cwd_counts
            .get(&(live.kind.clone(), cwd_canon.clone()))
            .copied()
            .unwrap_or(0)
            != 1
        {
            return None;
        }
        let candidates = self.by_place.get(&cwd_canon)?;
        let matching: Vec<&HubSessionState> = candidates
            .iter()
            .copied()
            .filter(|entry| {
                hook_key_matches_kind(&entry.key, &live.kind)
                    && match (&entry.provider_session_id, &live.session_id) {
                        (Some(a), Some(b)) => a == b,
                        _ => true,
                    }
            })
            .collect();
        (matching.len() == 1).then(|| matching[0])
    }
}

fn hook_key_matches_kind(key: &str, kind: &str) -> bool {
    let Some(rest) = key.strip_prefix("hook-") else {
        return false;
    };
    if rest.starts_with(&format!("{kind}-")) {
        return true;
    }
    !SCANNED_KINDS
        .iter()
        .any(|k| rest.starts_with(&format!("{}-", k.name())))
}

fn resolve_session_state(
    live: &LiveSession,
    reg: &ClaudeRegistrySnapshot,
    hub: Option<&HubIndex<'_>>,
    now_ms: u64,
) -> (String, String, Option<u64>) {
    if let Some(entry) = hub.and_then(|idx| idx.find(live)) {
        return (
            entry.state.clone(),
            "hub".into(),
            Some(now_ms.saturating_sub(entry.changed_ms) / 1000),
        );
    }
    if live.kind == "claude" {
        if let Some(rec) = reg.matching_entry(live.pid, live.start_us.unwrap_or(0)) {
            let state = normalize_registry_status(&rec.entry.status).to_owned();
            let age = rec.mtime_ms.map(|m| now_ms.saturating_sub(m) / 1000);
            return (state, "registry".into(), age);
        }
        if reg.has_corruption_for(live.pid) {
            return ("unknown".into(), "unknown".into(), None);
        }
    }
    ("unknown".into(), "none".into(), None)
}

fn normalize_registry_status(raw: &str) -> &'static str {
    match raw.trim().to_ascii_lowercase().as_str() {
        "busy" | "working" | "running" => "working",
        "idle" => "idle",
        "blocked" | "waiting" | "attention" | "permission" | "needs_input" | "needs_you" => {
            "blocked"
        }
        "done" | "finished" | "completed" | "stopped" => "done",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn registry(entries: &[(&str, String)]) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        for (name, body) in entries {
            fs::write(dir.path().join(name), body).unwrap();
        }
        dir
    }

    fn entry(pid: u32, cwd: &Path, started_at_ms: u64) -> String {
        serde_json::json!({
            "pid": pid,
            "cwd": cwd,
            "startedAt": started_at_ms,
            "entrypoint": "cli",
            "status": "busy",
            "kind": "interactive",
        })
        .to_string()
    }

    fn always(liveness: Liveness) -> impl Fn(u32, u128) -> Liveness {
        move |_, _| liveness.clone()
    }

    #[test]
    fn a_kind_with_no_detector_is_unknown_not_zero() {
        let reg = registry(&[]);
        let seen = observe_with(Some(reg.path()), &always(Liveness::Alive));
        let facet = attribute(&seen, &["/repo".into()]);
        let codex = facet.kinds.iter().find(|k| k.kind == "codex").unwrap();
        assert!(!codex.ok, "{facet:?}");
        assert!(!codex.error.is_empty());
        let claude = facet.kinds.iter().find(|k| k.kind == "claude").unwrap();
        assert!(claude.ok && claude.sessions == 0, "{facet:?}");
        // One kind unseen makes the facet as a whole not-ok, so `sessions: 0`
        // can never be read as "nobody is running".
        assert!(!facet.ok);
        assert_eq!(facet.sessions, 0);
    }

    #[test]
    fn sessions_are_attributed_to_the_deepest_containing_worktree() {
        let repo = tempfile::TempDir::new().unwrap();
        let main = repo.path().to_path_buf();
        let linked = main.join(".claude/worktrees/lane");
        let nested_cwd = linked.join("src/deep");
        fs::create_dir_all(&nested_cwd).unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        let reg = registry(&[
            ("1.json", entry(101, &main, 1)),
            ("2.json", entry(102, &nested_cwd, 1)),
            ("3.json", entry(103, outside.path(), 1)),
        ]);
        let seen = observe_with(Some(reg.path()), &always(Liveness::Alive));
        let listing = vec![
            main.to_string_lossy().into_owned(),
            linked.to_string_lossy().into_owned(),
        ];
        let facet = attribute(&seen, &listing);
        assert_eq!(facet.sessions, 2, "{facet:?}");
        let row = |p: &Path| {
            facet
                .worktrees
                .iter()
                .find(|w| Path::new(&w.path) == p)
                .unwrap_or_else(|| panic!("no row for {}: {facet:?}", p.display()))
        };
        assert_eq!(
            row(&main)
                .sessions
                .iter()
                .map(|s| s.pid)
                .collect::<Vec<_>>(),
            [101]
        );
        assert_eq!(
            row(&linked)
                .sessions
                .iter()
                .map(|s| s.pid)
                .collect::<Vec<_>>(),
            [102]
        );
    }

    fn session_at(pid: u32, cwd: &Path) -> LiveSession {
        LiveSession {
            kind: "claude".into(),
            pid,
            entrypoint: "cli".into(),
            status: "busy".into(),
            cwd: cwd.to_string_lossy().into_owned(),
            tree: None,
            run_id: None,
            parent_pid: None,
            session_id: None,
            proc_start: None,
            registry_cwd: None,
            start_us: None,
            is_caller: false,
            ide_hosted: false,
            terminal_session_id: None,
        }
    }

    fn observation_of(session: LiveSession) -> Observation {
        vec![KindObservation {
            kind: "claude".into(),
            ok: true,
            live: vec![session],
            ..KindObservation::default()
        }]
    }

    fn owner_of_pid(facet: &LiveSessionFacet, pid: u32) -> Option<&str> {
        facet
            .worktrees
            .iter()
            .find(|row| row.sessions.iter().any(|session| session.pid == pid))
            .map(|row| row.path.as_str())
    }

    /// The process sits in the main checkout. The lane's lock names that pid
    /// and the same start string, so the session belongs to the lane.
    #[test]
    fn a_locked_lane_owns_the_session_whose_process_cwd_is_the_main_checkout() {
        let root = tempfile::tempdir().unwrap();
        let main = root.path().join("main");
        let lane = root.path().join("lane");
        let admin = root.path().join("admin");
        fs::create_dir_all(&main).unwrap();
        fs::create_dir_all(&lane).unwrap();
        fs::create_dir_all(&admin).unwrap();
        fs::write(lane.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        let start = "Thu Nov  5 17:58:03 2026";
        fs::write(
            admin.join("locked"),
            format!("claude agent lane-1 (pid 42 start {start})\n"),
        )
        .unwrap();
        let mut session = session_at(42, &main);
        session.proc_start = Some(start.into());
        session.session_id = Some("lane-1".into());
        session.registry_cwd = Some(main.to_string_lossy().into_owned());
        let listing = vec![
            main.to_string_lossy().into_owned(),
            lane.to_string_lossy().into_owned(),
        ];
        // A last-claimed cwd of main disagrees with the lock. The lock wins.
        let claim = super::super::claims::FileClaim {
            source: super::super::claims::ClaimSource::Observed,
            session_id: Some("lane-1".into()),
            run_id: None,
            path: "a.rs".into(),
            worktree: main.to_string_lossy().into_owned(),
            ts_ms: 1,
            cwd: Some(main.to_string_lossy().into_owned()),
        };
        let facet = attribute_with(&observation_of(session.clone()), &listing, &[], &[claim]);
        assert_eq!(
            owner_of_pid(&facet, 42).map(Path::new),
            Some(main.as_path()),
            "without the lock the claim cwd is main: {facet:?}"
        );
        let facet = attribute(&observation_of(session), &listing);
        assert_eq!(
            owner_of_pid(&facet, 42).map(Path::new),
            Some(lane.as_path()),
            "the lock file names the lane: {facet:?}"
        );
    }

    #[test]
    fn a_lock_without_a_start_does_not_steal_a_session() {
        let parsed = parse_lock_reason("claude session abc (pid 42)").unwrap();
        assert_eq!(parsed, (42, None));
        let root = tempfile::tempdir().unwrap();
        let main = root.path().join("main");
        let lane = root.path().join("lane");
        let admin = root.path().join("admin");
        fs::create_dir_all(&main).unwrap();
        fs::create_dir_all(&lane).unwrap();
        fs::create_dir_all(&admin).unwrap();
        fs::write(lane.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        fs::write(admin.join("locked"), "claude agent lane-1 (pid 42)\n").unwrap();
        let mut session = session_at(42, &main);
        session.proc_start = Some("Thu Nov  5 17:58:03 2026".into());
        let listing = vec![
            main.to_string_lossy().into_owned(),
            lane.to_string_lossy().into_owned(),
        ];
        assert!(read_worktree_locks(&listing).is_empty());
        let facet = attribute(&observation_of(session), &listing);
        assert_eq!(
            owner_of_pid(&facet, 42).map(Path::new),
            Some(main.as_path()),
            "{facet:?}"
        );
    }

    /// Process cwd and registry cwd name two different checkouts. The hook's
    /// last-claimed cwd is the worktree the session is attributed to.
    #[test]
    fn a_hook_reported_worktree_wins_when_registry_cwd_differs_from_process_cwd() {
        let repo = crate::test_support::git_repo();
        let main = repo.path().to_path_buf();
        let lane = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        super::super::claims::record_observed(
            &main,
            "enter-worktree",
            "a.rs",
            lane.path().to_str().unwrap(),
            super::super::claims::now_ms(),
        )
        .unwrap();
        let mut session = session_at(7, &main);
        session.session_id = Some("enter-worktree".into());
        session.registry_cwd = Some(elsewhere.path().to_string_lossy().into_owned());
        let listing = vec![
            main.to_string_lossy().into_owned(),
            lane.path().to_string_lossy().into_owned(),
            elsewhere.path().to_string_lossy().into_owned(),
        ];
        let facet = attribute(&observation_of(session), &listing);
        assert_eq!(
            owner_of_pid(&facet, 7).map(Path::new),
            Some(lane.path()),
            "{facet:?}"
        );
    }

    /// One parent CLI started two isolation agents: both locks name its pid and
    /// start. That is no evidence for either lane, so the session stays where
    /// its cwd puts it. Before, the first lock listed took it.
    #[test]
    fn two_locks_naming_one_session_move_it_to_neither_lane() {
        let root = tempfile::tempdir().unwrap();
        let main = root.path().join("main");
        fs::create_dir_all(&main).unwrap();
        let start = "Thu Nov  5 17:58:03 2026";
        let mut listing = vec![main.to_string_lossy().into_owned()];
        for lane in ["lane-a", "lane-b"] {
            let checkout = root.path().join(lane);
            let admin = root.path().join(format!("admin-{lane}"));
            fs::create_dir_all(&checkout).unwrap();
            fs::create_dir_all(&admin).unwrap();
            fs::write(
                checkout.join(".git"),
                format!("gitdir: {}\n", admin.display()),
            )
            .unwrap();
            fs::write(
                admin.join("locked"),
                format!("claude agent {lane} (pid 42 start {start})\n"),
            )
            .unwrap();
            listing.push(checkout.to_string_lossy().into_owned());
        }
        assert_eq!(read_worktree_locks(&listing).len(), 2);
        let mut session = session_at(42, &main);
        session.proc_start = Some(start.into());
        let facet = attribute(&observation_of(session), &listing);
        assert_eq!(
            owner_of_pid(&facet, 42).map(Path::new),
            Some(main.as_path()),
            "{facet:?}"
        );
    }

    /// A claim store the attribution could not read in full is said on the
    /// facet, and `ok` drops: before, `if let Ok` made it an empty claim list.
    #[test]
    fn an_unreadable_claim_store_marks_attribution_degraded() {
        let repo = crate::test_support::git_repo();
        let main = repo.path().to_path_buf();
        let common = crate::engine::git_cli::resolve_git_common_dir(&main).unwrap();
        let store = super::super::claims::store_path(&common);
        fs::create_dir_all(store.parent().unwrap()).unwrap();
        fs::write(&store, b"{\"source\":\"obs").unwrap();
        let listing = vec![main.to_string_lossy().into_owned()];
        let facet = attribute(&observation_of(session_at(7, &main)), &listing);
        assert!(!facet.ok, "{facet:?}");
        assert!(
            facet.attribution_error.contains("1 unreadable claim line"),
            "{facet:?}"
        );
        // The session is still attributed, by what could be read.
        assert_eq!(owner_of_pid(&facet, 7).map(Path::new), Some(main.as_path()));

        let clean = crate::test_support::git_repo();
        let clean_listing = vec![clean.path().to_string_lossy().into_owned()];
        let facet = attribute(&observation_of(session_at(8, clean.path())), &clean_listing);
        assert!(facet.attribution_error.is_empty(), "{facet:?}");
    }

    #[test]
    fn a_gone_process_is_dropped_and_an_unjudged_one_is_unverified() {
        let repo = tempfile::TempDir::new().unwrap();
        let reg = registry(&[
            ("1.json", entry(201, repo.path(), 1)),
            ("2.json", entry(202, repo.path(), 1)),
            ("3.json", "{not json".into()),
        ]);
        let probe = |pid: u32, _: u128| match pid {
            201 => Liveness::Gone,
            _ => Liveness::Unknown("permission denied".into()),
        };
        let seen = observe_with(Some(reg.path()), &probe);
        let facet = attribute(&seen, &[repo.path().to_string_lossy().into_owned()]);
        let claude = facet.kinds.iter().find(|k| k.kind == "claude").unwrap();
        assert_eq!(claude.sessions, 0, "{facet:?}");
        // 202 could not be judged and sits in this repo; the unparseable entry
        // could be anywhere. Both are reported, neither is counted as live.
        assert_eq!(claude.unverified, 2, "{facet:?}");
        assert!(facet.worktrees.is_empty());
    }

    #[test]
    fn a_reused_pid_reads_as_gone_through_the_real_birth_probe() {
        // This process started before now, so a registry entry claiming it
        // registered at the Unix epoch names a process that started *after*
        // the session — a pid reuse.
        let repo = tempfile::TempDir::new().unwrap();
        let me = std::process::id();
        let reg = registry(&[("1.json", entry(me, repo.path(), 0))]);
        let seen = observe_with(Some(reg.path()), &process_birth::running_since);
        let facet = attribute(&seen, &[repo.path().to_string_lossy().into_owned()]);
        assert_eq!(facet.sessions, 0, "{facet:?}");
    }

    #[test]
    fn an_unreadable_registry_is_a_failed_detector() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let seen = observe_with(Some(file.path()), &always(Liveness::Alive));
        let claude = seen.iter().find(|k| k.kind == "claude").unwrap();
        assert!(!claude.ok);
        assert!(!claude.error.is_empty());
        let missing = file.path().with_extension("absent");
        let seen = observe_with(Some(&missing), &always(Liveness::Alive));
        assert!(seen.iter().find(|k| k.kind == "claude").unwrap().ok);
    }

    /// Characterization (host-load governor, Part 0): seven live sessions,
    /// five counted. Claude Code writes its registry under the session's own
    /// `CLAUDE_CONFIG_DIR`, so sessions run under a second account's config
    /// directory register where the registry detector never looks. Measured
    /// on the host on 2026-10-09 as 5 counted of 7 live. The registry is now
    /// only the fallback where there is no process scan; the scan's answer to
    /// the same seven is the next test.
    #[test]
    fn characterization_sessions_registered_under_another_config_dir_go_uncounted() {
        let repo = tempfile::TempDir::new().unwrap();
        let mine: Vec<(String, String)> = (0..5)
            .map(|i| (format!("{i}.json"), entry(300 + i, repo.path(), 1)))
            .collect();
        let theirs: Vec<(String, String)> = (5..7)
            .map(|i| (format!("{i}.json"), entry(300 + i, repo.path(), 1)))
            .collect();
        fn as_refs(rows: &[(String, String)]) -> Vec<(&str, String)> {
            rows.iter().map(|(n, b)| (n.as_str(), b.clone())).collect()
        }
        let read_here = registry(&as_refs(&mine));
        let _other_account = registry(&as_refs(&theirs));
        let seen = observe_with(Some(read_here.path()), &always(Liveness::Alive));
        let facet = attribute(&seen, &[repo.path().to_string_lossy().into_owned()]);
        assert_eq!(facet.sessions, 5, "seven are live; {facet:?}");
    }

    fn scanned(
        kind: crate::sysload::AgentKind,
        pid: i32,
        cwd: Option<&Path>,
    ) -> crate::sysload::AgentSession {
        crate::sysload::AgentSession {
            kind,
            pid,
            start_us: 1_000_000,
            cwd: cwd.map(|c| c.to_string_lossy().into_owned()),
            tree: None,
            run_id: None,
            parent_pid: None,
            cpu_rate: crate::sysload::Reading::Value(0),
            footprint: crate::sysload::Reading::Value(1),
            write_rate: crate::sysload::Reading::Unavailable(crate::sysload::Why::Unsupported),
            members: 1,
            unreadable_members: 0,
            ide_hosted: false,
            terminal_session_id: None,
        }
    }

    /// The fix for the characterization above: all seven are counted from
    /// the process table, five enriched from the registry that has them, and
    /// Codex is observed rather than unknown.
    #[test]
    fn the_process_scan_counts_every_session_whichever_registry_it_used() {
        use crate::sysload::AgentKind;
        let repo = tempfile::TempDir::new().unwrap();
        let rows: Vec<(String, String)> = (0..5)
            .map(|i| (format!("{i}.json"), entry(300 + i, repo.path(), 2_000)))
            .collect();
        let refs: Vec<(&str, String)> = rows.iter().map(|(n, b)| (n.as_str(), b.clone())).collect();
        let reg = registry(&refs);
        let mut sessions: Vec<_> = (0..7)
            .map(|i| scanned(AgentKind::Claude, 300 + i, Some(repo.path())))
            .collect();
        sessions.push(scanned(AgentKind::Codex, 400, Some(repo.path())));
        sessions.push(scanned(AgentKind::Antigravity, 401, Some(repo.path())));
        sessions.push(scanned(AgentKind::DevCouncil, 402, Some(repo.path())));
        sessions.push(scanned(AgentKind::Manvi, 403, Some(repo.path())));
        let facet = attribute(
            &from_scan(&sessions, &[reg.path().to_path_buf()]),
            &[repo.path().to_string_lossy().into_owned()],
        );
        assert_eq!(facet.sessions, 11, "{facet:?}");
        assert!(
            facet.ok,
            "all kinds observed, nothing unverified: {facet:?}"
        );
        let claude = facet.kinds.iter().find(|k| k.kind == "claude").unwrap();
        let codex = facet.kinds.iter().find(|k| k.kind == "codex").unwrap();
        let antigravity = facet
            .kinds
            .iter()
            .find(|k| k.kind == "antigravity")
            .unwrap();
        let devcouncil = facet.kinds.iter().find(|k| k.kind == "devcouncil").unwrap();
        let manvi = facet.kinds.iter().find(|k| k.kind == "manvi").unwrap();
        assert_eq!((claude.sessions, codex.sessions), (7, 1));
        assert_eq!(
            (antigravity.sessions, devcouncil.sessions, manvi.sessions),
            (1, 1, 1)
        );
        let rows = &facet.worktrees[0].sessions;
        let busy = rows.iter().filter(|s| s.status == "busy").count();
        assert_eq!(busy, 5, "the five with registry entries are enriched");
    }

    /// Enrichment reads every config directory's registry, not only the
    /// default one: a session registered under a second account's directory
    /// is enriched as well as counted.
    #[test]
    fn the_process_scan_is_enriched_from_every_config_dirs_registry() {
        use crate::sysload::AgentKind;
        let repo = tempfile::TempDir::new().unwrap();
        let default = registry(&[("1.json", entry(501, repo.path(), 2_000))]);
        let work = registry(&[("2.json", entry(502, repo.path(), 2_000))]);
        let sessions = [
            scanned(AgentKind::Claude, 501, Some(repo.path())),
            scanned(AgentKind::Claude, 502, Some(repo.path())),
        ];
        let seen = from_scan(
            &sessions,
            &[default.path().to_path_buf(), work.path().to_path_buf()],
        );
        let claude = seen.iter().find(|k| k.kind == "claude").unwrap();
        assert_eq!(claude.live.len(), 2);
        assert!(
            claude.live.iter().all(|s| s.status == "busy"),
            "{:?}",
            claude.live
        );
    }

    #[test]
    fn a_registry_entry_never_enriches_a_later_process_on_the_same_pid() {
        use crate::sysload::AgentKind;
        let repo = tempfile::TempDir::new().unwrap();
        // The entry was written at 1 s; this process started at 1000 s.
        let reg = registry(&[("1.json", entry(77, repo.path(), 1_000))]);
        let mut later = scanned(AgentKind::Claude, 77, Some(repo.path()));
        later.start_us = 1_000_000_000;
        let seen = from_scan(&[later], &[reg.path().to_path_buf()]);
        let claude = seen.iter().find(|k| k.kind == "claude").unwrap();
        assert_eq!(claude.live.len(), 1);
        assert_eq!(
            claude.live[0].status, "",
            "a reused pid inherited a stale status"
        );
    }

    #[test]
    fn a_scanned_session_with_no_readable_directory_is_unverified() {
        use crate::sysload::AgentKind;
        let repo = tempfile::TempDir::new().unwrap();
        let seen = from_scan(&[scanned(AgentKind::Codex, 5, None)], &[]);
        let facet = attribute(&seen, &[repo.path().to_string_lossy().into_owned()]);
        let codex = facet.kinds.iter().find(|k| k.kind == "codex").unwrap();
        assert_eq!((codex.sessions, codex.unverified), (0, 1));
        assert!(!facet.ok);
        let failed = attribute(&failed_scan("denied"), &[]);
        assert!(failed
            .kinds
            .iter()
            .all(|k| !k.ok && k.error.contains("denied")));
    }

    #[test]
    fn a_registry_past_its_cap_is_truncated_not_sampled() {
        let repo = tempfile::TempDir::new().unwrap();
        let reg = tempfile::TempDir::new().unwrap();
        for i in 0..=MAX_REGISTRY_ENTRIES {
            fs::write(
                reg.path().join(format!("{i}.json")),
                entry(1000 + i as u32, repo.path(), 1),
            )
            .unwrap();
        }
        let seen = observe_with(Some(reg.path()), &always(Liveness::Alive));
        let facet = attribute(&seen, &[repo.path().to_string_lossy().into_owned()]);
        let claude = facet.kinds.iter().find(|k| k.kind == "claude").unwrap();
        assert!(claude.truncated);
        assert_eq!(claude.sessions as usize, MAX_REGISTRY_ENTRIES);
        assert!(!facet.ok);
    }

    /// A second account's config directory (`~/.claude-work`) holds live
    /// sessions the default directory never sees; reading one directory
    /// counted 5 of 15 here while reporting ok.
    #[test]
    fn sessions_under_every_config_dir_are_counted() {
        let repo = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        for (dir, pid) in [
            (".claude", 301),
            (".claude-work", 302),
            (".claude-lab", 303),
        ] {
            let sessions = home.path().join(dir).join("sessions");
            fs::create_dir_all(&sessions).unwrap();
            fs::write(
                sessions.join(format!("{pid}.json")),
                entry(pid, repo.path(), 1),
            )
            .unwrap();
        }
        // Not a config dir: no `.claude-` prefix, so it is never read.
        fs::create_dir_all(home.path().join("claude-notes/sessions")).unwrap();
        let configs =
            crate::workbench::conversation::config_dirs_in(None, None, None, Some(home.path()));
        assert!(configs.error.is_none(), "{configs:?}");
        let registries: Vec<PathBuf> = configs.dirs.iter().map(|d| d.join("sessions")).collect();
        let seen = observe_registries(&registries, None, &always(Liveness::Alive));
        let facet = attribute(&seen, &[repo.path().to_string_lossy().into_owned()]);
        let claude = facet.kinds.iter().find(|k| k.kind == "claude").unwrap();
        assert_eq!(claude.sessions, 3, "{facet:?}");
        assert!(claude.ok, "{facet:?}");
    }

    fn child_of(
        mut session: crate::sysload::AgentSession,
        parent: i32,
    ) -> crate::sysload::AgentSession {
        session.parent_pid = Some(parent);
        session
    }

    fn pids(nodes: &[LiveSessionNode]) -> Vec<(u32, Vec<u32>)> {
        nodes
            .iter()
            .map(|n| (n.pid, n.children.iter().map(|c| c.pid).collect()))
            .collect()
    }

    /// The process scan already knew each session's launch and ancestor; the
    /// facet used to drop both on the way in.
    #[test]
    fn the_scan_s_launch_and_ancestor_reach_the_live_session() {
        use crate::sysload::AgentKind;
        let repo = tempfile::TempDir::new().unwrap();
        let mut launched = scanned(AgentKind::Claude, 700, Some(repo.path()));
        launched.tree = Some("run:run-3".into());
        launched.run_id = Some("run-3".into());
        let child = child_of(scanned(AgentKind::Codex, 701, Some(repo.path())), 700);
        let seen = from_scan(&[launched, child], &[]);
        let claude = seen.iter().find(|k| k.kind == "claude").unwrap();
        assert_eq!(claude.live[0].tree.as_deref(), Some("run:run-3"));
        assert_eq!(claude.live[0].run_id.as_deref(), Some("run-3"));
        assert_eq!(claude.live[0].parent_pid, None);
        let codex = seen.iter().find(|k| k.kind == "codex").unwrap();
        assert_eq!(codex.live[0].parent_pid, Some(700));
        assert_eq!(codex.live[0].tree, None);
    }

    /// Nesting is a second view of the same sessions: the flat list and every
    /// count are what they were without it.
    #[test]
    fn a_child_nests_under_its_parent_and_no_count_changes() {
        use crate::sysload::AgentKind;
        let repo = tempfile::TempDir::new().unwrap();
        let root = repo.path();
        let sessions = [
            scanned(AgentKind::Claude, 800, Some(root)),
            child_of(scanned(AgentKind::Claude, 801, Some(root)), 800),
            child_of(scanned(AgentKind::Codex, 802, Some(root)), 801),
            child_of(scanned(AgentKind::Claude, 803, Some(root)), 800),
            scanned(AgentKind::Claude, 804, Some(root)),
        ];
        let seen = from_scan(&sessions, &[]);
        let facet = attribute(&seen, &[root.to_string_lossy().into_owned()]);
        assert_eq!(facet.sessions, 5, "{facet:?}");
        let row = &facet.worktrees[0];
        assert_eq!(
            row.sessions.iter().map(|s| s.pid).collect::<Vec<_>>(),
            [800, 801, 802, 803, 804]
        );
        assert_eq!(pids(&row.children), [(800, vec![801, 803])]);
        assert_eq!(pids(&row.children[0].children[0].children), [(802, vec![])]);
        assert_eq!(row.children[0].children[0].children[0].kind, "codex");
        assert!(!row.children_truncated);
        let claude = facet.kinds.iter().find(|k| k.kind == "claude").unwrap();
        let codex = facet.kinds.iter().find(|k| k.kind == "codex").unwrap();
        assert_eq!((claude.sessions, codex.sessions), (4, 1));
        assert!(facet.ok);
    }

    /// A parent in another worktree is not a node here, and a child whose
    /// parent was never seen stays a plain session with its `parent_pid`.
    #[test]
    fn a_parent_outside_the_worktree_is_not_nested_here() {
        use crate::sysload::AgentKind;
        let repo = tempfile::TempDir::new().unwrap();
        let main = repo.path().to_path_buf();
        let linked = main.join(".claude/worktrees/lane");
        fs::create_dir_all(&linked).unwrap();
        let sessions = [
            scanned(AgentKind::Claude, 810, Some(&main)),
            child_of(scanned(AgentKind::Claude, 811, Some(&linked)), 810),
            child_of(scanned(AgentKind::Claude, 812, Some(&linked)), 9_999),
        ];
        let seen = from_scan(&sessions, &[]);
        let facet = attribute(
            &seen,
            &[
                main.to_string_lossy().into_owned(),
                linked.to_string_lossy().into_owned(),
            ],
        );
        assert_eq!(facet.sessions, 3);
        for row in &facet.worktrees {
            assert!(row.children.is_empty(), "{row:?}");
        }
        let lane = facet
            .worktrees
            .iter()
            .find(|w| Path::new(&w.path) == linked)
            .unwrap();
        let by_pid = |pid| lane.sessions.iter().find(|s| s.pid == pid).unwrap();
        assert_eq!(by_pid(811).parent_pid, Some(810));
        assert_eq!(by_pid(812).parent_pid, Some(9_999));
    }

    /// Lineage that loops, or points at itself, still ends: each session is
    /// placed at most once and nothing recurses without bound.
    #[test]
    fn a_looping_or_self_parent_chain_terminates_with_each_session_once() {
        use crate::sysload::AgentKind;
        let repo = tempfile::TempDir::new().unwrap();
        let root = repo.path();
        let sessions = [
            child_of(scanned(AgentKind::Claude, 820, Some(root)), 821),
            child_of(scanned(AgentKind::Claude, 821, Some(root)), 820),
            child_of(scanned(AgentKind::Claude, 822, Some(root)), 822),
        ];
        let seen = from_scan(&sessions, &[]);
        let facet = attribute(&seen, &[root.to_string_lossy().into_owned()]);
        let row = &facet.worktrees[0];
        assert_eq!(facet.sessions, 3);
        assert_eq!(pids(&row.children), [(820, vec![821])]);
    }

    #[test]
    fn nesting_stops_at_its_depth_bound_and_says_so() {
        use crate::sysload::AgentKind;
        let repo = tempfile::TempDir::new().unwrap();
        let root = repo.path();
        let depth = MAX_NEST_DEPTH as i32 + 4;
        let sessions: Vec<_> = (0..depth)
            .map(|n| {
                let session = scanned(AgentKind::Claude, 900 + n, Some(root));
                if n == 0 {
                    session
                } else {
                    child_of(session, 900 + n - 1)
                }
            })
            .collect();
        let seen = from_scan(&sessions, &[]);
        let facet = attribute(&seen, &[root.to_string_lossy().into_owned()]);
        let row = &facet.worktrees[0];
        assert!(row.children_truncated, "{row:?}");
        assert_eq!(row.sessions.len() as i32, depth, "the flat list is whole");
        let mut levels = 0;
        let mut at = &row.children[0];
        loop {
            levels += 1;
            match at.children.first() {
                Some(next) => at = next,
                None => break,
            }
        }
        assert_eq!(levels, MAX_NEST_DEPTH);
    }

    /// A registry knows nothing about lineage, and says nothing rather than
    /// "no parent".
    #[test]
    fn a_registry_session_has_no_lineage() {
        let repo = tempfile::TempDir::new().unwrap();
        let reg = registry(&[("1.json", entry(910, repo.path(), 1))]);
        let seen = observe_with(Some(reg.path()), &always(Liveness::Alive));
        let facet = attribute(&seen, &[repo.path().to_string_lossy().into_owned()]);
        let session = &facet.worktrees[0].sessions[0];
        assert_eq!(
            (&session.tree, &session.run_id, session.parent_pid),
            (&None, &None, None)
        );
        assert!(facet.worktrees[0].children.is_empty());
    }

    /// A config-directory list that could not be completed fails the kind: a
    /// directory that was never looked at must not read as zero sessions.
    #[test]
    fn an_incomplete_config_dir_discovery_fails_the_kind() {
        let repo = tempfile::TempDir::new().unwrap();
        let reg = registry(&[("1.json", entry(401, repo.path(), 1))]);
        let seen = observe_registries(
            &[reg.path().to_path_buf()],
            Some("cannot list /home/x for `.claude-*` config directories"),
            &always(Liveness::Alive),
        );
        let claude = seen.iter().find(|k| k.kind == "claude").unwrap();
        assert!(!claude.ok);
        assert!(
            claude.error.contains("config directories"),
            "{}",
            claude.error
        );
        // What was found is still reported, so a partial answer stays useful.
        assert_eq!(claude.live.len(), 1);
    }

    #[test]
    fn sessions_report_states_sources_caller_task_and_bounding() {
        let repo = tempfile::TempDir::new().unwrap();
        let wt = repo.path().join("wt-main");
        fs::create_dir_all(&wt).unwrap();
        let wt_str = wt.to_string_lossy().into_owned();

        let claude_dir = tempfile::TempDir::new().unwrap();
        let reg_path = claude_dir.path();
        fs::write(
            reg_path.join("102.json"),
            serde_json::json!({
                "pid": 102,
                "cwd": &wt_str,
                "startedAt": 1000,
                "entrypoint": "cli",
                "status": "busy",
                "sessionId": "sid-102"
            })
            .to_string(),
        )
        .unwrap();
        // Corrupt entry for PID 103
        fs::write(reg_path.join("103.json"), "{not-valid-json").unwrap();

        let scan_sessions = vec![
            crate::sysload::AgentSession {
                kind: crate::sysload::AgentKind::Claude,
                pid: 101,
                start_us: 100_000,
                cwd: Some(wt_str.clone()),
                tree: Some("run:run-caller".into()),
                run_id: Some("run-caller".into()),
                parent_pid: None,
                footprint: crate::sysload::Reading::Unavailable(crate::sysload::Why::Unsupported),
                cpu_rate: crate::sysload::Reading::Unavailable(crate::sysload::Why::Unsupported),
                write_rate: crate::sysload::Reading::Unavailable(crate::sysload::Why::Unsupported),
                members: 1,
                unreadable_members: 0,
                ide_hosted: false,
                terminal_session_id: Some("term-101".into()),
            },
            crate::sysload::AgentSession {
                kind: crate::sysload::AgentKind::Claude,
                pid: 102,
                start_us: 200_000,
                cwd: Some(wt_str.clone()),
                tree: None,
                run_id: None,
                parent_pid: None,
                footprint: crate::sysload::Reading::Unavailable(crate::sysload::Why::Unsupported),
                cpu_rate: crate::sysload::Reading::Unavailable(crate::sysload::Why::Unsupported),
                write_rate: crate::sysload::Reading::Unavailable(crate::sysload::Why::Unsupported),
                members: 1,
                unreadable_members: 0,
                ide_hosted: false,
                terminal_session_id: None,
            },
            crate::sysload::AgentSession {
                kind: crate::sysload::AgentKind::Claude,
                pid: 103,
                start_us: 300_000,
                cwd: Some(wt_str.clone()),
                tree: None,
                run_id: None,
                parent_pid: None,
                footprint: crate::sysload::Reading::Unavailable(crate::sysload::Why::Unsupported),
                cpu_rate: crate::sysload::Reading::Unavailable(crate::sysload::Why::Unsupported),
                write_rate: crate::sysload::Reading::Unavailable(crate::sysload::Why::Unsupported),
                members: 1,
                unreadable_members: 0,
                ide_hosted: false,
                terminal_session_id: None,
            },
            crate::sysload::AgentSession {
                kind: crate::sysload::AgentKind::Codex,
                pid: 104,
                start_us: 400_000,
                cwd: Some(wt_str.clone()),
                tree: None,
                run_id: None,
                parent_pid: None,
                footprint: crate::sysload::Reading::Unavailable(crate::sysload::Why::Unsupported),
                cpu_rate: crate::sysload::Reading::Unavailable(crate::sysload::Why::Unsupported),
                write_rate: crate::sysload::Reading::Unavailable(crate::sysload::Why::Unsupported),
                members: 1,
                unreadable_members: 0,
                ide_hosted: false,
                terminal_session_id: None,
            },
        ];
        let reg_snap = read_claude_registry_snapshot(&[reg_path.to_path_buf()], None);
        let now_ms = 50_000;

        // Case 1: App not running (Unavailable) -> app_state.ok == false, registry answers 102, 101 has state_source="none", 103 corrupt="unknown", 104 codex="unknown"/"none"
        let offline = assemble_sessions_report(
            wt_str.clone(),
            std::slice::from_ref(&wt_str),
            &[],
            &[],
            Vec::new(),
            Ok(crate::sysload::SessionScan {
                sessions: scan_sessions.clone(),
                caller: Some(crate::sysload::CallerIdentity {
                    pid: 102,
                    start_us: 200_000,
                }),
            }),
            &reg_snap,
            HubStateRead::Unavailable("app not running".into()),
            None,
            now_ms,
            &|rid| (rid == "run-caller").then(|| "task-1".into()),
            &|_| None,
        );
        assert!(!offline.app_state.ok);
        assert!(offline.app_state.error.contains("app not running"));
        assert!(!offline.ok); // 103.json corrupt sets claude ok=false
        assert_eq!(offline.total, 4);
        assert_eq!(offline.returned, 4);
        assert!(!offline.truncated);
        // Caller 102 is sorted first!
        assert_eq!(offline.sessions[0].pid, 102);
        assert!(offline.sessions[0].is_caller);
        assert_eq!(offline.sessions[0].state, "working");
        assert_eq!(offline.sessions[0].state_source, "registry");
        assert!(offline.sessions[0].state_age_s.is_some());

        let r101 = offline.sessions.iter().find(|s| s.pid == 101).unwrap();
        assert!(!r101.is_caller);
        assert_eq!(r101.run_id.as_deref(), Some("run-caller"));
        assert_eq!(r101.task_id.as_deref(), Some("task-1"));
        assert_eq!(r101.state, "unknown");
        assert_eq!(r101.state_source, "none");

        let r103 = offline.sessions.iter().find(|s| s.pid == 103).unwrap();
        assert_eq!(r103.state, "unknown");
        assert_eq!(r103.state_source, "unknown");

        let r104 = offline.sessions.iter().find(|s| s.pid == 104).unwrap();
        assert_eq!(r104.kind, "codex");
        assert_eq!(r104.state, "unknown");
        assert_eq!(r104.state_source, "none");

        // Case 2: App running -> hub state overrides 101 by terminal_session_id, 102 by session_id, and cap=2 bounds result while keeping caller first.
        let hub = HubStateSnapshot {
            version: 1,
            writer_pid: std::process::id(),
            writer_start_us: 1,
            updated_ms: now_ms,
            sessions: vec![
                HubSessionState {
                    key: "term-101".into(),
                    provider_session_id: None,
                    place: Some(wt_str.clone()),
                    state: "blocked".into(),
                    event: Some("permission_request".into()),
                    changed_ms: now_ms - 5_000,
                },
                HubSessionState {
                    key: "hook-claude-abc".into(),
                    provider_session_id: Some("sid-102".into()),
                    place: Some(wt_str.clone()),
                    state: "done".into(),
                    event: Some("turn_finished".into()),
                    changed_ms: now_ms - 12_000,
                },
            ],
        };
        let online = assemble_sessions_report(
            wt_str.clone(),
            &[wt_str],
            &[],
            &[],
            Vec::new(),
            Ok(crate::sysload::SessionScan {
                sessions: scan_sessions,
                caller: Some(crate::sysload::CallerIdentity {
                    pid: 102,
                    start_us: 200_000,
                }),
            }),
            &reg_snap,
            HubStateRead::Ok(hub),
            Some(2),
            now_ms,
            &|_| None,
            &|_| None,
        );
        assert!(online.app_state.ok);
        assert_eq!(online.cap, 2);
        assert_eq!(online.total, 4);
        assert_eq!(online.returned, 2);
        assert!(online.truncated);
        assert_eq!(online.sessions[0].pid, 102);
        assert_eq!(online.sessions[0].state, "done");
        assert_eq!(online.sessions[0].state_source, "hub");
        assert_eq!(online.sessions[0].state_age_s, Some(12));
        assert_eq!(online.sessions[1].pid, 101);
        assert_eq!(online.sessions[1].state, "blocked");
        assert_eq!(online.sessions[1].state_source, "hub");
        assert_eq!(online.sessions[1].state_age_s, Some(5));
    }

    #[test]
    fn augment_ide_peers_surfaces_antigravity_from_peers_json_and_presence_lock() {
        let repo = crate::test_support::git_repo();
        let repo_str = repo.path().to_string_lossy().into_owned();
        let common = crate::engine::git_cli::resolve_git_common_dir(repo.path()).unwrap();
        let lanes_dir = common.join("gitpulse").join("lanes");
        std::fs::create_dir_all(&lanes_dir).unwrap();

        let agy_home = tempfile::tempdir().unwrap();
        let presence_dir = agy_home.path().join("presence");
        std::fs::create_dir_all(&presence_dir).unwrap();

        let conv_locked = "aaaaaaaa-1111-4222-8333-444444444444";
        let conv_locked_2 = "cccccccc-1111-4222-8333-444444444444";
        let conv_stale = "bbbbbbbb-1111-4222-8333-444444444444";
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(presence_dir.join(format!("{conv_locked}.lock")))
            .unwrap();
        let lock_file_2 = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(presence_dir.join(format!("{conv_locked_2}.lock")))
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            assert_eq!(
                unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
                0
            );
            assert_eq!(
                unsafe { libc::flock(lock_file_2.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
                0
            );
        }
        std::fs::write(presence_dir.join(format!("{conv_stale}.lock")), b"").unwrap();

        let peers_json = serde_json::json!({
            "peers": [
                {
                    "peer_id": format!("agy:{conv_locked}"),
                    "provider": "antigravity",
                    "surface": "ide",
                    "provider_session_id": conv_locked,
                    "pid": 40908,
                    "worktree_path": repo_str,
                    "turn_state": "idle",
                    "updated_at_ms": 1000
                },
                {
                    "peer_id": format!("agy:{conv_locked_2}"),
                    "provider": "agy",
                    "surface": "ide",
                    "provider_session_id": conv_locked_2,
                    "pid": 40908,
                    "worktree_path": repo_str,
                    "turn_state": "running",
                    "updated_at_ms": 1001
                },
                {
                    "peer_id": format!("agy:{conv_stale}"),
                    "provider": "agy",
                    "surface": "ide",
                    "provider_session_id": conv_stale,
                    "worktree_path": repo_str,
                    "turn_state": "exited",
                    "updated_at_ms": 1000
                }
            ]
        });
        std::fs::write(
            lanes_dir.join("peers.json"),
            serde_json::to_vec_pretty(&peers_json).unwrap(),
        )
        .unwrap();

        let obs = from_scan(&[], &[]);
        let mut facet = attribute_with(&obs, std::slice::from_ref(&repo_str), &[], &[]);
        let homes = vec![agy_home.path().to_path_buf()];
        augment_ide_peers_in(
            std::slice::from_ref(&repo_str),
            &homes,
            &|_, _| Liveness::Gone,
            &mut facet,
        );

        let agy = facet
            .kinds
            .iter()
            .find(|k| k.kind == "antigravity")
            .expect("antigravity kind status");
        if cfg!(unix) {
            assert_eq!(
                agy.sessions, 2,
                "both presence-locked agy IDE sessions sharing pid must be surfaced: {facet:?}"
            );
            assert_eq!(facet.sessions, 2, "{facet:?}");
            assert_eq!(facet.worktrees[0].sessions.len(), 2);
            let sids: Vec<Option<&str>> = facet.worktrees[0]
                .sessions
                .iter()
                .map(|s| s.session_id.as_deref())
                .collect();
            assert!(sids.contains(&Some(conv_locked)), "{sids:?}");
            assert!(sids.contains(&Some(conv_locked_2)), "{sids:?}");
            assert!(facet.worktrees[0].sessions.iter().all(|s| s.ide_hosted));

            // Second augmentation pass must deduplicate without double-counting.
            augment_ide_peers_in(
                std::slice::from_ref(&repo_str),
                &homes,
                &|_, _| Liveness::Gone,
                &mut facet,
            );
            assert_eq!(
                facet
                    .kinds
                    .iter()
                    .find(|k| k.kind == "antigravity")
                    .unwrap()
                    .sessions,
                2
            );
        }
        drop(lock_file);
        drop(lock_file_2);
    }
}
