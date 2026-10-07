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
//! Each agent kind is observed by its own detector, and each detector reports
//! whether it could look. A kind with no detector is reported as unknown,
//! never as zero sessions: "we cannot see Codex sessions" and "no Codex
//! session is running" are different facts.

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

/// Agent kinds GitPulse knows run as sessions but has no way to observe.
///
/// Codex keeps rollout logs under `~/.codex/sessions`, which record that a
/// session happened, not that it is running or where. Listing it here is
/// what makes its absence from the counts an explicit "unknown".
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
    observe_with(
        crate::workbench::conversation::claude_home()
            .map(|home| home.join("sessions"))
            .as_deref(),
        &process_birth::running_since,
    )
}

/// [`observe`] with the registry directory and the liveness probe as
/// arguments, so tests can point it at a fixture registry.
pub(crate) fn observe_with(
    claude_sessions: Option<&Path>,
    alive: &dyn Fn(u32, u128) -> Liveness,
) -> Observation {
    let mut kinds = vec![match claude_sessions {
        Some(dir) => read_claude_registry(dir, alive),
        None => KindObservation {
            kind: "claude".into(),
            error: "no home directory to find Claude Code's session registry in".into(),
            ..KindObservation::default()
        },
    }];
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
    let kinds = std::iter::once("claude")
        .chain(UNOBSERVABLE_KINDS.iter().map(|(kind, _)| *kind))
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
}
