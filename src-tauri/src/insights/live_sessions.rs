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

use super::{LiveKindStatus, LiveSession, LiveSessionFacet, LiveWorktree};
use crate::workbench::process_birth::{self, Liveness};
use serde::Deserialize;
use std::path::{Path, PathBuf};

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
const UNOBSERVABLE_KINDS: &[(&str, &str)] = &[(
    "codex",
    "Codex keeps no registry of running sessions that GitPulse can read, so its live sessions cannot be counted",
)];

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
}

/// Observes every agent kind on this machine.
pub(crate) fn observe() -> Observation {
    let configs = crate::workbench::conversation::claude_config_dirs();
    let registries: Vec<PathBuf> = configs
        .dirs
        .iter()
        .map(|dir| dir.join("sessions"))
        .collect();
    match crate::sysload::procscan::observe_sessions() {
        Ok(sessions) => from_scan(&sessions, &registries),
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
const SCANNED_KINDS: [crate::sysload::AgentKind; 2] = [
    crate::sysload::AgentKind::Claude,
    crate::sysload::AgentKind::Codex,
];

/// Turns the process scan's sessions into per-kind observations, enriched
/// from Claude Code's registries at `claude_sessions` (one per config
/// directory) where an entry names the same process.
pub(crate) fn from_scan(
    sessions: &[crate::sysload::AgentSession],
    claude_sessions: &[PathBuf],
) -> Observation {
    let mut details = std::collections::HashMap::new();
    for dir in claude_sessions {
        details.extend(registry_details(dir));
    }
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
        let entry = details.get(&pid).filter(|entry| {
            u128::from(session.start_us) * 1_000
                <= u128::from(entry.started_at) * 1_000_000 + BIRTH_CLOCK_SLACK_NANOS
        });
        seen.live.push(LiveSession {
            kind: session.kind.name().into(),
            pid,
            entrypoint: entry.map(|e| e.entrypoint.clone()).unwrap_or_default(),
            status: entry.map(|e| e.status.clone()).unwrap_or_default(),
            cwd: cwd.clone(),
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

/// Claude Code's registry entries by pid, for enrichment only. Unreadable
/// entries are skipped: the session they might describe is already counted
/// from the process table.
fn registry_details(dir: &Path) -> std::collections::HashMap<u32, ClaudeRegistryEntry> {
    let mut details = std::collections::HashMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return details;
    };
    for entry in entries.flatten().take(MAX_REGISTRY_ENTRIES) {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            if let Ok(parsed) = read_entry(&path) {
                details.insert(parsed.pid, parsed);
            }
        }
    }
    details
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
                cwd: entry.cwd,
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

/// Attributes an observation to the worktrees of one repository.
///
/// `worktree_paths` is the repository's worktree listing, spelled as git
/// listed it; the facet reports paths in that spelling. A session whose
/// directory is in none of them belongs to another repository and is not
/// counted here.
pub(crate) fn attribute(observation: &Observation, worktree_paths: &[String]) -> LiveSessionFacet {
    let roots: Vec<(String, PathBuf)> = worktree_paths
        .iter()
        .map(|path| (path.clone(), comparable(path)))
        .collect();
    let mut worktrees: Vec<LiveWorktree> = Vec::new();
    let mut kinds = Vec::new();
    for seen in observation {
        let mut sessions = 0u32;
        for session in &seen.live {
            let Some(owner) = owning_worktree(&comparable(&session.cwd), &roots) else {
                continue;
            };
            sessions += 1;
            match worktrees.iter_mut().find(|w| w.path == owner) {
                Some(row) => row.sessions.push(session.clone()),
                None => worktrees.push(LiveWorktree {
                    path: owner.to_string(),
                    sessions: vec![session.clone()],
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
            footprint: crate::sysload::Reading::Value(1),
            members: 1,
            unreadable_members: 0,
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
        let facet = attribute(
            &from_scan(&sessions, &[reg.path().to_path_buf()]),
            &[repo.path().to_string_lossy().into_owned()],
        );
        assert_eq!(facet.sessions, 8, "{facet:?}");
        assert!(
            facet.ok,
            "both kinds observed, nothing unverified: {facet:?}"
        );
        let claude = facet.kinds.iter().find(|k| k.kind == "claude").unwrap();
        let codex = facet.kinds.iter().find(|k| k.kind == "codex").unwrap();
        assert_eq!((claude.sessions, codex.sessions), (7, 1));
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
}
