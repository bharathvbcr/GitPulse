//! `runs.conversation`: whether an ended attempt's Claude Code conversation can
//! be picked up again, and with what.
//!
//! A terminal attempt passes its run id to Claude Code as `--session-id`
//! (`terminal_command::arguments`); a managed attempt records the id Claude
//! reported as `provider_thread_id`. Either way GitPulse holds the id, but
//! holding an id is not evidence of a conversation: attempts launched before
//! the id was passed, or ones Claude never got as far as saving, have none.
//! So the answer is decided by looking for the transcript Claude Code writes,
//! and an attempt is offered for resumption only when that file is there.
//!
//! Read-only. Nothing here starts a process; the renderer opens an ordinary
//! Claude Code tab with `--resume <id>` in the attempt's checkout.

use super::{query, terminal_command::is_canonical_uuid, WorkbenchError, WorkbenchState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// How many of Claude Code's per-project directories one answer will look in.
/// One per checkout it has ever run in; a host past this is answered "could
/// not check", never "no conversation".
const MAX_PROJECTS: usize = 4096;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: String,
}

/// What the transcript search found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Transcript {
    Found,
    Absent,
    Unknown(String),
}

/// The decision, with the filesystem injected so every branch is drivable.
pub(super) fn judge(
    run: &Value,
    checkout_exists: bool,
    transcript: impl FnOnce(&str) -> Transcript,
) -> Value {
    let refuse = |reason: &str| json!({"ok":true,"resumable":false,"reason":reason});
    let provider = run["provider"].as_str().unwrap_or_default();
    if provider != "claude" && provider != "agy" {
        return refuse(
            "Only a Claude Code or Antigravity conversation can be resumed from GitPulse.",
        );
    }
    let state = run["state"].as_str().unwrap_or_default();
    if matches!(state, "prepared" | "starting" | "running" | "unresolved") {
        return refuse("The attempt has not ended; open its terminal instead.");
    }
    // A managed attempt's start is the thread id below; a terminal attempt's
    // is the process GitPulse recorded.
    if run["kind"] != "managed" && run["started_at"].is_null() && run["process_id"].is_null() {
        return refuse("The agent never started, so there is no conversation.");
    }
    let session = match run["kind"].as_str() {
        Some("managed") => run["provider_thread_id"].as_str(),
        _ => run["provider_thread_id"]
            .as_str()
            .filter(|id| is_canonical_uuid(id))
            .or_else(|| run["id"].as_str()),
    }
    .filter(|id| is_canonical_uuid(id));
    let (found_reason, absent_reason, session_missing_reason) = if provider == "agy" {
        (
            "Antigravity saved this conversation.",
            "Antigravity has no saved conversation for this attempt.",
            "This attempt has no Antigravity session id to resume.",
        )
    } else {
        (
            "Claude Code saved this conversation.",
            "Claude Code has no saved conversation for this attempt.",
            "This attempt has no Claude Code session id to resume.",
        )
    };
    let Some(session) = session else {
        return refuse(session_missing_reason);
    };
    if !checkout_exists {
        return refuse("The checkout this conversation ran in no longer exists.");
    }
    match transcript(session) {
        Transcript::Found => json!({
            "ok": true,
            "resumable": true,
            "session_id": session,
            "provider": provider,
            "cwd": run["cwd"],
            "permission_mode": run["permission_mode"],
            "reason": found_reason,
        }),
        Transcript::Absent => refuse(absent_reason),
        Transcript::Unknown(why) => json!({
            "ok": true,
            "resumable": false,
            "reason": format!("Could not check for a saved conversation: {why}"),
        }),
    }
}

/// Claude Code's default configuration directory (`CLAUDE_CONFIG_DIR`, else
/// `~/.claude`). Production resume checks must use [`claude_config_dirs`] +
/// [`search_claude_configs`] so `.claude-accounts/<seat>` transcripts are found.
#[cfg(test)]
pub(crate) fn claude_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".claude"))
}

/// Antigravity configuration directories: `ANTIGRAVITY_HOME`, plus both the
/// IDE (`~/.gemini/antigravity`) and standalone CLI (`~/.gemini/antigravity-cli`)
/// homes under `HOME` / `USERPROFILE`.
pub(crate) fn antigravity_homes() -> Vec<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    let home = var("HOME")
        .or_else(|| var("USERPROFILE"))
        .map(PathBuf::from);
    antigravity_homes_in(var("ANTIGRAVITY_HOME").map(PathBuf::from), home.as_deref())
}

pub(crate) fn antigravity_homes_in(agy_home: Option<PathBuf>, home: Option<&Path>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(explicit) = agy_home {
        candidates.push(explicit);
    }
    if let Some(home) = home {
        let gemini = home.join(".gemini");
        candidates.push(gemini.join("antigravity"));
        candidates.push(gemini.join("antigravity-cli"));
        candidates.push(gemini.join("antigravity-ide"));
    }
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for dir in candidates {
        let key = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if !seen.contains(&key) {
            seen.push(key);
            out.push(dir);
        }
    }
    out
}

/// Every Claude Code configuration directory this account may be running
/// sessions under, and whether the list is known to be complete.
///
/// `CLAUDE_CONFIG_DIR` points one launch at one directory, and people with
/// several accounts keep one per account (`~/.claude-work`, `~/.claude-lab`,
/// or seat mappings in `~/.claude-accounts`). A reader that looked only at the
/// current process's directory counted 5 of 15 live sessions on a real machine
/// while reporting itself healthy, so the candidates are: `CLAUDE_CONFIG_DIR`,
/// `GITPULSE_CLAUDE_CONFIG_DIRS` (a PATH-style list for directories outside the
/// conventions), `~/.claude`, the XDG `claude` directory, every `~/.claude-*`
/// directory, and every seat target declared in `~/.claude-accounts`.
#[derive(Debug, Clone, Default)]
pub(crate) struct ClaudeConfigDirs {
    pub dirs: Vec<PathBuf>,
    /// Set when the home directory could not be listed for `~/.claude-*`:
    /// directories may exist that are not in `dirs`, so the list is not
    /// evidence of absence.
    pub error: Option<String>,
}

pub(crate) fn claude_config_dirs() -> ClaudeConfigDirs {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    let home = var("HOME")
        .or_else(|| var("USERPROFILE"))
        .map(PathBuf::from);
    config_dirs_in(
        var("CLAUDE_CONFIG_DIR").map(PathBuf::from),
        var("GITPULSE_CLAUDE_CONFIG_DIRS"),
        var("XDG_CONFIG_HOME").map(PathBuf::from),
        home.as_deref(),
    )
}

/// [`claude_config_dirs`] with the environment as arguments.
pub(crate) fn config_dirs_in(
    config_dir: Option<PathBuf>,
    extra: Option<std::ffi::OsString>,
    xdg_config_home: Option<PathBuf>,
    home: Option<&Path>,
) -> ClaudeConfigDirs {
    /// More sibling directories than any person keeps; past it the list is
    /// reported incomplete rather than read without bound.
    const MAX_HOME_ENTRIES: usize = 4096;
    let mut out = ClaudeConfigDirs::default();
    let mut candidates: Vec<PathBuf> = Vec::new();
    candidates.extend(config_dir);
    if let Some(extra) = extra {
        candidates.extend(std::env::split_paths(&extra).filter(|p| !p.as_os_str().is_empty()));
    }
    if let Some(home) = home {
        candidates.push(home.join(".claude"));
        candidates.push(
            xdg_config_home
                .clone()
                .unwrap_or_else(|| home.join(".config"))
                .join("claude"),
        );
        match std::fs::read_dir(home) {
            Ok(entries) => {
                let mut siblings: Vec<PathBuf> = Vec::new();
                for (seen, entry) in entries.enumerate() {
                    if seen >= MAX_HOME_ENTRIES {
                        out.error = Some(format!(
                            "{} holds more than {MAX_HOME_ENTRIES} entries; some `.claude-*` directories may not be listed",
                            home.display()
                        ));
                        break;
                    }
                    let Ok(entry) = entry else { continue };
                    let name = entry.file_name();
                    if name.to_string_lossy().starts_with(".claude-")
                        && entry.file_type().is_ok_and(|t| t.is_dir())
                    {
                        siblings.push(entry.path());
                    }
                }
                siblings.sort();
                candidates.extend(siblings);
            }
            Err(error) => {
                out.error = Some(format!(
                    "cannot list {} for `.claude-*` config directories: {error}",
                    home.display()
                ));
            }
        }
        let accounts_path = home.join(".claude-accounts");
        if let Ok(contents) = std::fs::read_to_string(&accounts_path) {
            for raw_line in contents.lines().take(256) {
                let line = raw_line.split('#').next().unwrap_or("").trim();
                if line.is_empty() {
                    continue;
                }
                let mut parts = line.split_whitespace();
                let Some(_seat) = parts.next() else { continue };
                let rest = parts.collect::<Vec<_>>().join(" ");
                let raw_dir = rest.trim().trim_matches('"').trim_matches('\'');
                if raw_dir.is_empty() {
                    continue;
                }
                let expanded = if let Some(stripped) = raw_dir.strip_prefix("~/") {
                    home.join(stripped)
                } else {
                    PathBuf::from(raw_dir)
                };
                if expanded.is_absolute() {
                    candidates.push(expanded);
                }
            }
        }
    } else if let Some(xdg) = xdg_config_home {
        candidates.push(xdg.join("claude"));
    }
    let mut seen: Vec<PathBuf> = Vec::new();
    for dir in candidates {
        let key = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if !seen.contains(&key) {
            seen.push(key);
            out.dirs.push(dir);
        }
    }
    out
}

/// Outcome of locating a Claude Code conversation transcript (`<uuid>.jsonl`)
/// across [`ClaudeConfigDirs`], distinguishing whether it resides in `checkout`'s
/// own project directory or another checkout's project directory (relevant for
/// cross-worktree `--fork-session`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClaudeTranscriptLocation {
    Found {
        path: PathBuf,
        in_checkout_project: bool,
    },
    Absent,
    Unknown(String),
}

fn checkout_project_names(checkout: Option<&Path>) -> Vec<String> {
    let Some(co) = checkout else {
        return Vec::new();
    };
    let mut names = Vec::new();
    let canon = co.canonicalize().unwrap_or_else(|_| co.to_path_buf());
    for candidate in [&canon, &co.to_path_buf()] {
        let encoded = claude_project_dir_name(candidate);
        if !encoded.is_empty() && !names.contains(&encoded) {
            names.push(encoded);
        }
    }
    names
}

/// Locates `<config_dir>/projects/<project>/<session>.jsonl` across `configs`.
/// When `checkout` is `Some`, prefers a transcript inside that checkout's
/// encoded project directory (`in_checkout_project: true`), falling back to
/// any sibling checkout's project directory (`in_checkout_project: false`) and
/// preserving `Unknown` if any store could not be read and no match was found.
pub(crate) fn locate_claude_transcript_in(
    configs: &ClaudeConfigDirs,
    session: &str,
    checkout: Option<&Path>,
) -> ClaudeTranscriptLocation {
    let want_projects = checkout_project_names(checkout);
    let name = format!("{session}.jsonl");
    let mut first_cross_checkout: Option<PathBuf> = None;
    let mut first_unknown: Option<String> = None;

    for dir in &configs.dirs {
        let projects = dir.join("projects");
        let entries = match std::fs::read_dir(&projects) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                if first_unknown.is_none() {
                    first_unknown = Some(error.to_string());
                }
                continue;
            }
        };
        for (seen, entry) in entries.enumerate() {
            if seen >= MAX_PROJECTS {
                if first_unknown.is_none() {
                    first_unknown = Some(format!(
                        "more than {MAX_PROJECTS} Claude Code project directories"
                    ));
                }
                break;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    if first_unknown.is_none() {
                        first_unknown = Some(error.to_string());
                    }
                    break;
                }
            };
            let candidate = entry.path().join(&name);
            if candidate.is_file() {
                let in_checkout = checkout.is_none()
                    || entry
                        .file_name()
                        .to_str()
                        .is_some_and(|dir_name| want_projects.iter().any(|w| w == dir_name));
                if in_checkout {
                    return ClaudeTranscriptLocation::Found {
                        path: candidate,
                        in_checkout_project: true,
                    };
                }
                if first_cross_checkout.is_none() {
                    first_cross_checkout = Some(candidate);
                }
            }
        }
    }

    if let Some(path) = first_cross_checkout {
        return ClaudeTranscriptLocation::Found {
            path,
            in_checkout_project: false,
        };
    }
    if let Some(why) = first_unknown {
        return ClaudeTranscriptLocation::Unknown(why);
    }
    if let Some(err) = &configs.error {
        return ClaudeTranscriptLocation::Unknown(err.clone());
    }
    if configs.dirs.is_empty() {
        return ClaudeTranscriptLocation::Unknown(
            "no home directory to find Claude Code's in".into(),
        );
    }
    ClaudeTranscriptLocation::Absent
}

/// Looks for `<home>/projects/<any>/<session>.jsonl`.
#[cfg(test)]
pub(super) fn search(home: &Path, session: &str) -> Transcript {
    let single = ClaudeConfigDirs {
        dirs: vec![home.to_path_buf()],
        error: None,
    };
    match locate_claude_transcript_in(&single, session, None) {
        ClaudeTranscriptLocation::Found { .. } => Transcript::Found,
        ClaudeTranscriptLocation::Absent => Transcript::Absent,
        ClaudeTranscriptLocation::Unknown(why) => Transcript::Unknown(why),
    }
}

/// Looks for `<home>/conversations/<session>.db` or `<session>.pb` (standalone CLI).
pub(super) fn search_antigravity(home: &Path, session: &str) -> Transcript {
    let conv_dir = home.join("conversations");
    let mut first_unknown: Option<String> = None;
    for ext in ["db", "pb"] {
        let candidate = conv_dir.join(format!("{session}.{ext}"));
        match std::fs::metadata(&candidate) {
            Ok(meta) if meta.is_file() => {
                if meta.len() == 0 {
                    continue;
                }
                match std::fs::File::open(&candidate) {
                    Ok(_) => return Transcript::Found,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => {
                        if first_unknown.is_none() {
                            first_unknown = Some(err.to_string());
                        }
                    }
                }
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                if first_unknown.is_none() {
                    first_unknown = Some(err.to_string());
                }
            }
        }
    }
    if let Some(why) = first_unknown {
        Transcript::Unknown(why)
    } else {
        Transcript::Absent
    }
}

pub(crate) fn search_antigravity_homes(homes: &[PathBuf], session: &str) -> Transcript {
    if homes.is_empty() {
        return Transcript::Unknown("no home directory to find Antigravity's in".into());
    }
    let mut first_unknown: Option<String> = None;
    for home in homes {
        match search_antigravity(home, session) {
            Transcript::Found => return Transcript::Found,
            Transcript::Absent => {}
            Transcript::Unknown(why) => {
                if first_unknown.is_none() {
                    first_unknown = Some(why);
                }
            }
        }
    }
    if let Some(why) = first_unknown {
        Transcript::Unknown(why)
    } else {
        Transcript::Absent
    }
}

/// Most ids one [`transcripts`] call answers: an incident's whole list
/// (`MAX_INCIDENT_SESSIONS` in the renderer's `incident.ts`).
pub(crate) const MAX_TRANSCRIPT_QUERIES: usize = 512;

/// Whether Claude Code saved one hosted conversation, as
/// `cmd_terminal_conversations` answers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConversationTranscript {
    pub session_id: String,
    /// `found`, `absent` or `unknown`: a plain word rather than a serialized
    /// enum, because the renderer branches on it and holds it to its own union.
    pub transcript: String,
    /// Why it could not be checked, for `unknown`; `None` otherwise.
    pub reason: Option<String>,
}

/// Whether Claude Code saved each of these conversations: the same evidence
/// an attempt's "Resume conversation" is decided on (`search` in
/// `claude_home`), for the plain Claude tabs GitPulse hosts. Holding an id is
/// not evidence of a conversation: a session killed before its first message
/// has none. Read-only.
///
/// Every id must be a canonical session id; anything else is refused whole,
/// because it can only come from a caller that is not the renderer's own.
/// Searches every Claude Code configuration directory in `configs` (`~/.claude`,
/// `~/.config/claude`, `~/.claude-*`, `~/.claude-accounts`, `CLAUDE_CONFIG_DIR`,
/// `GITPULSE_CLAUDE_CONFIG_DIRS`), preserving `Transcript::Unknown` whenever a
/// directory could not be read or the config-directory list itself is incomplete or empty.
pub(crate) fn search_claude_configs(configs: &ClaudeConfigDirs, session: &str) -> Transcript {
    match locate_claude_transcript_in(configs, session, None) {
        ClaudeTranscriptLocation::Found { .. } => Transcript::Found,
        ClaudeTranscriptLocation::Absent => Transcript::Absent,
        ClaudeTranscriptLocation::Unknown(why) => Transcript::Unknown(why),
    }
}

pub(crate) fn transcripts(session_ids: &[String]) -> Result<Vec<ConversationTranscript>, String> {
    if session_ids.len() > MAX_TRANSCRIPT_QUERIES {
        return Err(format!(
            "At most {MAX_TRANSCRIPT_QUERIES} conversations can be checked at once."
        ));
    }
    if let Some(bad) = session_ids.iter().find(|id| !is_canonical_uuid(id)) {
        let shown: String = bad.chars().take(64).collect();
        return Err(format!("{shown:?} is not a valid session id."));
    }
    let configs = claude_config_dirs();
    let agy_homes = antigravity_homes();
    Ok(session_ids
        .iter()
        .map(|session| {
            let claude_res = search_claude_configs(&configs, session);
            let found = match claude_res {
                Transcript::Found => Transcript::Found,
                _ if agy_homes.is_empty() => claude_res,
                _ => match search_antigravity_homes(&agy_homes, session) {
                    Transcript::Found => Transcript::Found,
                    Transcript::Absent => claude_res,
                    agy_unknown @ Transcript::Unknown(_) => {
                        if matches!(claude_res, Transcript::Absent) {
                            agy_unknown
                        } else {
                            claude_res
                        }
                    }
                },
            };
            answer(session, found)
        })
        .collect())
}

fn answer(session: &str, found: Transcript) -> ConversationTranscript {
    let (transcript, reason) = match found {
        Transcript::Found => ("found", None),
        Transcript::Absent => ("absent", None),
        Transcript::Unknown(why) => ("unknown", Some(why)),
    };
    ConversationTranscript {
        session_id: session.to_owned(),
        transcript: transcript.into(),
        reason,
    }
}

/// Checks whether `provider` exposes an on-disk transcript locator that can
/// prove a conversation exists before offering or running `--resume`.
pub fn has_transcript_locator(provider: &str) -> bool {
    matches!(provider.trim(), "claude" | "agy" | "antigravity")
}

/// Result of verifying whether a recorded session can be resumed right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderResumability {
    pub resumable: bool,
    pub reason: Option<String>,
}

/// Checks whether a session for `provider` (`claude`, `agy`, `codex`, `grok`,
/// `manvi`, `shell`, ...) with optional `session_id` and working directory
/// `cwd` can be resumed, using `probe` to inspect transcripts on disk.
pub(crate) fn check_session_resumability_with(
    provider: &str,
    session_id: Option<&str>,
    cwd: &Path,
    probe: impl FnOnce(&str, &str) -> Transcript,
) -> ProviderResumability {
    let provider = provider.trim();
    if provider.is_empty() || provider == "shell" {
        return ProviderResumability {
            resumable: false,
            reason: Some("not restorable".into()),
        };
    }
    if !has_transcript_locator(provider) {
        return ProviderResumability {
            resumable: false,
            reason: Some(format!(
                "Provider {provider} has no transcript locator; sessions cannot be resumed."
            )),
        };
    }
    let sid = session_id.map(str::trim).filter(|s| !s.is_empty());
    let Some(sid) = sid else {
        return ProviderResumability {
            resumable: false,
            reason: Some(format!(
                "No {provider} session id was recorded for this tab."
            )),
        };
    };
    if provider == "agy" && sid == super::terminal_command::AGY_CONTINUE_SENTINEL {
        if !cwd.is_dir() {
            return ProviderResumability {
                resumable: false,
                reason: Some(format!(
                    "Working directory no longer exists: {}",
                    cwd.display()
                )),
            };
        }
        return ProviderResumability {
            resumable: true,
            reason: None,
        };
    }
    if !is_canonical_uuid(sid) {
        return ProviderResumability {
            resumable: false,
            reason: Some(format!(
                "Recorded {provider} session id is not a canonical UUID."
            )),
        };
    }
    if !cwd.is_dir() {
        return ProviderResumability {
            resumable: false,
            reason: Some(format!(
                "Working directory no longer exists: {}",
                cwd.display()
            )),
        };
    }
    match probe(provider, sid) {
        Transcript::Found => ProviderResumability {
            resumable: true,
            reason: None,
        },
        Transcript::Absent => ProviderResumability {
            resumable: false,
            reason: Some(format!(
                "No saved {provider} transcript was found for session {sid}."
            )),
        },
        Transcript::Unknown(why) => ProviderResumability {
            resumable: false,
            reason: Some(format!("Could not verify saved conversation: {why}")),
        },
    }
}

/// Checks whether a recorded session is resumable using live transcript
/// discovery for `provider` (`claude` or `agy`).
pub fn check_session_resumability(
    provider: &str,
    session_id: Option<&str>,
    cwd: &Path,
) -> ProviderResumability {
    check_session_resumability_with(provider, session_id, cwd, |prov, sid| {
        if prov == "agy" || prov == "antigravity" {
            let homes = antigravity_homes();
            search_antigravity_homes(&homes, sid)
        } else {
            let configs = claude_config_dirs();
            search_claude_configs(&configs, sid)
        }
    })
}

/// Checks whether a saved transcript exists for `provider` and canonical UUID `session`.
pub(crate) fn has_saved_transcript(provider: &str, session: &str) -> bool {
    if !is_canonical_uuid(session) {
        return false;
    }
    if provider == "agy" || provider == "antigravity" {
        let homes = antigravity_homes();
        matches!(search_antigravity_homes(&homes, session), Transcript::Found)
    } else if provider == "claude" {
        let configs = claude_config_dirs();
        matches!(search_claude_configs(&configs, session), Transcript::Found)
    } else {
        false
    }
}

/// Claude Code's project directory name for `checkout`: every non-alphanumeric
/// character becomes `-` (so `/Users/a/repo_1` maps to `-Users-a-repo-1`).
pub fn claude_project_dir_name(checkout: &Path) -> String {
    checkout
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Finds the newest non-empty Claude Code transcript (`<uuid>.jsonl`) stored
/// for `checkout` across `dirs`. Returns `None` when none exists or is readable.
pub(crate) fn latest_claude_session_in(dirs: &ClaudeConfigDirs, checkout: &Path) -> Option<String> {
    let mut project_names = Vec::new();
    let canon = checkout
        .canonicalize()
        .unwrap_or_else(|_| checkout.to_path_buf());
    for path in [&canon, &checkout.to_path_buf()] {
        let encoded = claude_project_dir_name(path);
        if !encoded.is_empty() && !project_names.contains(&encoded) {
            project_names.push(encoded);
        }
    }

    let mut newest: Option<(std::time::SystemTime, String)> = None;
    for config_dir in &dirs.dirs {
        let projects_root = config_dir.join("projects");
        for name in &project_names {
            let dir = projects_root.join(name);
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for (seen, entry) in entries.enumerate() {
                if seen >= MAX_PROJECTS {
                    break;
                }
                let Ok(entry) = entry else { continue };
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if !is_canonical_uuid(stem) {
                    continue;
                }
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                if !meta.is_file() || meta.len() == 0 {
                    continue;
                }
                let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                let sid = stem.to_ascii_lowercase();
                let replace = match &newest {
                    None => true,
                    Some((prev_time, prev_sid)) => {
                        modified > *prev_time || (modified == *prev_time && sid > *prev_sid)
                    }
                };
                if replace {
                    newest = Some((modified, sid));
                }
            }
        }
    }
    newest.map(|(_, sid)| sid)
}

/// Finds the newest saved Claude Code session id for `checkout` in the default
/// Claude config directories (`CLAUDE_CONFIG_DIR` / `~/.claude`).
pub fn latest_claude_session_for_checkout(checkout: &Path) -> Option<String> {
    let dirs = claude_config_dirs();
    latest_claude_session_in(&dirs, checkout)
}

/// Verifies `session_id` does not already have a saved Claude Code transcript
/// in `dirs`. Forking into an existing `--session-id` fails in Claude Code 2.1.325
/// with `"Session ID ... is already in use."`, so callers fail closed here.
pub(crate) fn verify_unused_claude_session_in(
    dirs: &ClaudeConfigDirs,
    session_id: &str,
) -> Result<(), String> {
    if !is_canonical_uuid(session_id) {
        return Err(format!(
            "Claude Code session_id must be a canonical UUID, got {session_id:?}."
        ));
    }
    match search_claude_configs(dirs, session_id) {
        Transcript::Absent => Ok(()),
        Transcript::Found => Err(format!(
            "Session ID {session_id} already has a saved Claude Code transcript and cannot be used for a forked session."
        )),
        Transcript::Unknown(why) => Err(format!(
            "Could not verify that session ID {session_id} is unused: {why}"
        )),
    }
}

/// Live-config wrapper around [`verify_unused_claude_session_in`].
pub fn verify_unused_claude_session(session_id: &str) -> Result<(), String> {
    let dirs = claude_config_dirs();
    verify_unused_claude_session_in(&dirs, session_id)
}

pub(super) fn find(state: &WorkbenchState, input: &str) -> Result<Value, WorkbenchError> {
    if input.len() > 1024 {
        return Err(WorkbenchError::new(
            "invalid_input",
            "A conversation request exceeds 1 KiB.",
        ));
    }
    let request: Request = serde_json::from_str(input)
        .map_err(|e| WorkbenchError::new("invalid_input", e.to_string()))?;
    if request.id.is_empty() || request.id.len() > 128 {
        return Err(WorkbenchError::new(
            "invalid_input",
            "Select a saved attempt.",
        ));
    }
    let response = state
        .with_store(|store| query(store, "runs.get", &json!({"id": request.id}).to_string()))?;
    let mut run = response["item"].clone();
    let checkout_exists = run["cwd"]
        .as_str()
        .is_some_and(|cwd| Path::new(cwd).is_dir());
    let provider = run["provider"].as_str().unwrap_or("claude").to_owned();
    if run["provider_thread_id"].as_str().is_none() && checkout_exists {
        if let Some(cwd) = run["cwd"].as_str() {
            if let Some(peer_sid) =
                crate::engine::agent_bus::peer_session_id_for_run(cwd, &request.id)
            {
                run["provider_thread_id"] = json!(peer_sid);
            }
        }
    }
    Ok(judge(&run, checkout_exists, |session| {
        if provider == "agy" {
            let homes = antigravity_homes();
            search_antigravity_homes(&homes, session)
        } else {
            let configs = claude_config_dirs();
            search_claude_configs(&configs, session)
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::{answer, judge, search, transcripts, Transcript, MAX_TRANSCRIPT_QUERIES};
    use serde_json::json;

    const ID: &str = "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c11";
    const THREAD: &str = "0b7d4e2a-91c3-4f6e-8a2d-5c4b3a291e70";

    fn ended() -> serde_json::Value {
        json!({
            "id": ID, "kind": "external_terminal", "provider": "claude",
            "state": "exited", "cwd": "/checkout", "permission_mode": "inspect",
            "started_at": 1, "process_id": 42,
        })
    }

    #[test]
    fn an_ended_claude_attempt_with_a_saved_transcript_resumes_in_its_own_mode() {
        let mut asked = None;
        let answer = judge(&ended(), true, |session| {
            asked = Some(session.to_owned());
            Transcript::Found
        });
        assert_eq!(asked.as_deref(), Some(ID));
        assert_eq!(answer["resumable"], true);
        assert_eq!(answer["session_id"], ID);
        assert_eq!(answer["cwd"], "/checkout");
        // An inspect-only attempt must not come back with more authority.
        assert_eq!(answer["permission_mode"], "inspect");
    }

    #[test]
    fn a_managed_attempt_resumes_by_the_thread_claude_reported_not_the_run_id() {
        let mut run = ended();
        run["kind"] = json!("managed");
        run["provider_thread_id"] = json!(THREAD);
        let answer = judge(&run, true, |session| {
            assert_eq!(session, THREAD);
            Transcript::Found
        });
        assert_eq!(answer["session_id"], THREAD);
    }

    #[test]
    fn nothing_is_offered_without_evidence_of_a_conversation() {
        let never = |_: &str| -> Transcript { panic!("must not look") };
        let cases: Vec<(serde_json::Value, bool)> = vec![
            (json!({"provider": "codex"}), true),
            (json!({"state": "running"}), true),
            (json!({"state": "unresolved"}), true),
            (json!({"started_at": null, "process_id": null}), true),
            (json!({"id": "run-1"}), true),
            (json!({}), false),
        ];
        for (patch, exists) in cases {
            let mut run = ended();
            for (key, value) in patch.as_object().unwrap() {
                run[key] = value.clone();
            }
            let answer = judge(&run, exists, never);
            assert_eq!(answer["resumable"], false, "{patch}");
            assert!(answer["reason"].as_str().unwrap().len() > 10, "{patch}");
            assert!(answer.get("session_id").is_none(), "{patch}");
        }
        // A managed attempt with no thread id is not resumed by its run id.
        let mut managed = ended();
        managed["kind"] = json!("managed");
        assert_eq!(judge(&managed, true, never)["resumable"], false);

        assert_eq!(
            judge(&ended(), true, |_| Transcript::Absent)["resumable"],
            false
        );
        // A search that could not run says so; it is not "no conversation".
        let unknown = judge(&ended(), true, |_| Transcript::Unknown("denied".into()));
        assert_eq!(unknown["resumable"], false);
        assert!(unknown["reason"]
            .as_str()
            .unwrap()
            .starts_with("Could not check"));
    }

    #[test]
    fn the_transcript_is_found_in_whichever_project_directory_holds_it() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(search(home.path(), ID), Transcript::Absent);
        let projects = home.path().join("projects");
        for project in ["-Users-a-one", "-Users-a-two"] {
            std::fs::create_dir_all(projects.join(project)).unwrap();
        }
        std::fs::write(
            projects
                .join("-Users-a-one")
                .join(format!("{THREAD}.jsonl")),
            "{}",
        )
        .unwrap();
        assert_eq!(search(home.path(), ID), Transcript::Absent);
        std::fs::write(
            projects.join("-Users-a-two").join(format!("{ID}.jsonl")),
            "{}",
        )
        .unwrap();
        assert_eq!(search(home.path(), ID), Transcript::Found);
        // A directory of that name is not a transcript.
        std::fs::create_dir_all(
            projects
                .join("-Users-a-one")
                .join(format!("{THREAD}x.jsonl")),
        )
        .unwrap();
        assert_eq!(
            search(home.path(), &format!("{THREAD}x")),
            Transcript::Absent
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_store_is_unknown_not_absent() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let projects = home.path().join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::set_permissions(&projects, std::fs::Permissions::from_mode(0o000)).unwrap();
        let found = search(home.path(), ID);
        std::fs::set_permissions(&projects, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(found, Transcript::Unknown(_)), "{found:?}");
    }

    /// The hosted-tab answer is the attempt's evidence in three words, and
    /// only a canonical id, in a bounded list, is ever looked for.
    #[test]
    fn a_hosted_conversation_is_answered_found_absent_or_unknown_and_bad_ids_are_refused() {
        assert_eq!(
            answer(ID, Transcript::Found),
            super::ConversationTranscript {
                session_id: ID.into(),
                transcript: "found".into(),
                reason: None
            }
        );
        assert_eq!(answer(ID, Transcript::Absent).transcript, "absent");
        let unknown = answer(ID, Transcript::Unknown("denied".into()));
        assert_eq!(unknown.transcript, "unknown");
        assert_eq!(unknown.reason.as_deref(), Some("denied"));

        for bad in [
            "",
            "../../etc/passwd",
            "6F1C2A7E-0D4B-4C1E-9A55-3B0E8F2D9C11x",
            "not-a-uuid",
        ] {
            assert!(transcripts(&[ID.into(), bad.into()]).is_err(), "{bad:?}");
        }
        assert!(transcripts(&vec![ID.to_string(); MAX_TRANSCRIPT_QUERIES + 1]).is_err());
        assert!(transcripts(&[]).unwrap().is_empty());
    }

    #[test]
    fn an_ended_agy_attempt_with_a_saved_transcript_resumes_in_its_own_mode() {
        let mut run = ended();
        run["provider"] = json!("agy");
        let mut asked = None;
        let answer = judge(&run, true, |session| {
            asked = Some(session.to_owned());
            Transcript::Found
        });
        assert_eq!(asked.as_deref(), Some(ID));
        assert_eq!(answer["resumable"], true);
        assert_eq!(answer["session_id"], ID);
        assert_eq!(answer["provider"], "agy");
        assert_eq!(answer["cwd"], "/checkout");
        assert_eq!(answer["permission_mode"], "inspect");
        assert_eq!(answer["reason"], "Antigravity saved this conversation.");
    }

    #[test]
    fn antigravity_transcript_is_found_when_db_file_exists() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            super::search_antigravity(home.path(), ID),
            Transcript::Absent
        );
        let conv_dir = home.path().join("conversations");
        std::fs::create_dir_all(&conv_dir).unwrap();
        assert_eq!(
            super::search_antigravity(home.path(), ID),
            Transcript::Absent
        );

        // 0-byte file must be treated as Absent
        let db_file = conv_dir.join(format!("{ID}.db"));
        std::fs::write(&db_file, b"").unwrap();
        assert_eq!(
            super::search_antigravity(home.path(), ID),
            Transcript::Absent
        );

        // Valid non-empty file is Found
        std::fs::write(&db_file, b"SQLite format 3\0").unwrap();
        assert_eq!(
            super::search_antigravity(home.path(), ID),
            Transcript::Found
        );

        // Unreadable file reports Unknown error
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&db_file, std::fs::Permissions::from_mode(0o000)).unwrap();
            assert!(matches!(
                super::search_antigravity(home.path(), ID),
                Transcript::Unknown(_)
            ));
            std::fs::set_permissions(&db_file, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
    }

    #[test]
    fn multi_account_claude_config_dirs_finds_transcripts_in_sibling_profiles_and_preserves_unknown(
    ) {
        let home = tempfile::tempdir().unwrap();
        let sibling = home.path().join(".claude-work");
        let proj = sibling.join("projects").join("-Users-a-repo");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join(format!("{ID}.jsonl")), b"{}\n").unwrap();

        let configs = super::config_dirs_in(None, None, None, Some(home.path()));
        assert_eq!(
            super::search_claude_configs(&configs, ID),
            Transcript::Found
        );
        assert_eq!(
            super::search_claude_configs(&configs, THREAD),
            Transcript::Absent
        );

        // Empty config directory set (no HOME/USERPROFILE) reports Unknown, never Absent.
        let empty = super::ClaudeConfigDirs::default();
        assert!(matches!(
            super::search_claude_configs(&empty, ID),
            Transcript::Unknown(_)
        ));

        // Partial home listing error reports Unknown when absent from listed dirs,
        // while still returning Found when a listed dir holds the transcript.
        let mut partial = configs.clone();
        partial.error = Some("home truncated".into());
        assert_eq!(
            super::search_claude_configs(&partial, ID),
            Transcript::Found
        );
        assert_eq!(
            super::search_claude_configs(&partial, THREAD),
            Transcript::Unknown("home truncated".into())
        );
    }

    #[test]
    fn claude_accounts_file_and_standalone_agy_cli_pb_transcripts_are_discovered() {
        let sandbox = tempfile::tempdir().unwrap();
        let home = sandbox.path().join("home");
        let external_seat = sandbox.path().join("seats").join("enterprise-seat");
        let proj = external_seat.join("projects").join("-Users-a-repo");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(proj.join(format!("{ID}.jsonl")), b"{}\n").unwrap();
        std::fs::write(
            home.join(".claude-accounts"),
            format!(
                "# seat mapping\nenterprise {}\nrelative ~/custom-seat\n",
                external_seat.display()
            ),
        )
        .unwrap();

        let configs = super::config_dirs_in(None, None, None, Some(&home));
        assert_eq!(
            super::search_claude_configs(&configs, ID),
            Transcript::Found
        );
        assert!(configs.dirs.contains(&home.join("custom-seat")));

        // Standalone Antigravity CLI stores `.pb` transcripts under `~/.gemini/antigravity-cli/conversations/`.
        let agy_cli_conv = home
            .join(".gemini")
            .join("antigravity-cli")
            .join("conversations");
        std::fs::create_dir_all(&agy_cli_conv).unwrap();
        let pb_file = agy_cli_conv.join(format!("{THREAD}.pb"));
        std::fs::write(&pb_file, b"").unwrap();
        let agy_homes = super::antigravity_homes_in(None, Some(&home));
        assert_eq!(
            super::search_antigravity_homes(&agy_homes, THREAD),
            Transcript::Absent
        );

        std::fs::write(&pb_file, b"\x0a\x04turn").unwrap();
        assert_eq!(
            super::search_antigravity_homes(&agy_homes, THREAD),
            Transcript::Found
        );
        assert!(agy_homes.contains(&home.join(".gemini").join("antigravity-ide")));

        // An external_terminal `agy` attempt bound via `runs.bind_thread` resumes by
        // its bound `provider_thread_id` rather than its run id.
        let mut agy_term = ended();
        agy_term["id"] = json!("run-agy-ext-1");
        agy_term["provider"] = json!("agy");
        agy_term["provider_thread_id"] = json!(THREAD);
        let resumed = judge(&agy_term, true, |session| {
            assert_eq!(session, THREAD);
            Transcript::Found
        });
        assert_eq!(resumed["resumable"], true);
        assert_eq!(resumed["session_id"], THREAD);
        assert_eq!(resumed["provider"], "agy");
    }

    #[test]
    fn provider_without_transcript_locator_and_shells_are_not_resumable_with_reasons() {
        let cwd = tempfile::tempdir().unwrap();
        for provider in ["codex", "grok", "manvi"] {
            let status =
                super::check_session_resumability_with(provider, Some(ID), cwd.path(), |_, _| {
                    unreachable!("locator should not be called")
                });
            assert!(!status.resumable, "{provider} must not be resumable");
            assert!(
                status
                    .reason
                    .as_deref()
                    .is_some_and(|r| r.contains("no transcript locator")),
                "{provider}: {:?}",
                status.reason
            );
        }

        let shell = super::check_session_resumability_with("shell", None, cwd.path(), |_, _| {
            unreachable!("shell should not query transcript")
        });
        assert!(!shell.resumable);
        assert_eq!(shell.reason.as_deref(), Some("not restorable"));

        let missing_dir = cwd.path().join("gone");
        let gone =
            super::check_session_resumability_with("claude", Some(ID), &missing_dir, |_, _| {
                Transcript::Found
            });
        assert!(!gone.resumable);
        assert!(gone
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("no longer exists")));

        let found =
            super::check_session_resumability_with("claude", Some(ID), cwd.path(), |prov, sid| {
                assert_eq!(prov, "claude");
                assert_eq!(sid, ID);
                Transcript::Found
            });
        assert!(found.resumable);
        assert_eq!(found.reason, None);
    }

    #[test]
    fn latest_claude_session_and_unused_session_check_work_against_project_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path().join("claude-cfg");
        let checkout = tmp.path().join("repo_wt");
        std::fs::create_dir_all(&checkout).unwrap();
        let canon = checkout.canonicalize().unwrap();
        let proj = config_dir
            .join("projects")
            .join(super::claude_project_dir_name(&canon));
        std::fs::create_dir_all(&proj).unwrap();

        let older = "11111111-1111-4111-8111-111111111111";
        let newer = "22222222-2222-4222-8222-222222222222";
        let empty = "33333333-3333-4333-8333-333333333333";
        let unused = "44444444-4444-4444-8444-444444444444";

        let older_path = proj.join(format!("{older}.jsonl"));
        let newer_path = proj.join(format!("{newer}.jsonl"));
        std::fs::write(&older_path, "{\"type\":\"user\"}\n").unwrap();
        std::fs::write(&newer_path, "{\"type\":\"user\"}\n").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&older_path)
            .unwrap()
            .set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000))
            .unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&newer_path)
            .unwrap()
            .set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(2_000))
            .unwrap();
        std::fs::write(proj.join(format!("{empty}.jsonl")), "").unwrap();
        std::fs::write(proj.join("not-a-uuid.jsonl"), "{}").unwrap();

        let dirs = super::ClaudeConfigDirs {
            dirs: vec![config_dir],
            error: None,
        };
        assert_eq!(
            super::latest_claude_session_in(&dirs, &checkout).as_deref(),
            Some(newer)
        );
        assert!(super::verify_unused_claude_session_in(&dirs, unused).is_ok());
        assert!(super::verify_unused_claude_session_in(&dirs, older).is_err());

        let unreadable = super::ClaudeConfigDirs {
            dirs: vec![],
            error: Some("HOME missing".to_string()),
        };
        assert!(super::verify_unused_claude_session_in(&unreadable, unused).is_err());
    }

    #[test]
    fn locate_claude_transcript_distinguishes_same_checkout_from_cross_checkout_and_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let dir_first = tmp.path().join("cfg-first");
        let dir_second = tmp.path().join("cfg-second");
        let src_wt = tmp.path().join("wt-src");
        let fork_wt = tmp.path().join("wt-fork");
        std::fs::create_dir_all(&src_wt).unwrap();
        std::fs::create_dir_all(&fork_wt).unwrap();
        let src_canon = src_wt.canonicalize().unwrap();
        let fork_canon = fork_wt.canonicalize().unwrap();

        let src_proj = dir_first
            .join("projects")
            .join(super::claude_project_dir_name(&src_canon));
        let fork_proj = dir_second
            .join("projects")
            .join(super::claude_project_dir_name(&fork_canon));
        std::fs::create_dir_all(&src_proj).unwrap();
        std::fs::create_dir_all(&fork_proj).unwrap();

        let src_transcript = src_proj.join(format!("{ID}.jsonl"));
        std::fs::write(&src_transcript, "{\"type\":\"user\"}\n").unwrap();

        let dirs = super::ClaudeConfigDirs {
            dirs: vec![dir_first, dir_second],
            error: None,
        };
        assert_eq!(
            super::locate_claude_transcript_in(&dirs, ID, Some(&src_wt)),
            super::ClaudeTranscriptLocation::Found {
                path: src_transcript.clone(),
                in_checkout_project: true,
            }
        );
        assert_eq!(
            super::locate_claude_transcript_in(&dirs, ID, Some(&fork_wt)),
            super::ClaudeTranscriptLocation::Found {
                path: src_transcript,
                in_checkout_project: false,
            }
        );
        assert_eq!(
            super::locate_claude_transcript_in(&dirs, THREAD, Some(&fork_wt)),
            super::ClaudeTranscriptLocation::Absent
        );

        // If a transcript also exists under fork_wt's own project dir later in `dirs`,
        // same-checkout wins over an earlier cross-checkout match.
        let fork_transcript = fork_proj.join(format!("{ID}.jsonl"));
        std::fs::write(&fork_transcript, "{\"type\":\"user\"}\n").unwrap();
        assert_eq!(
            super::locate_claude_transcript_in(&dirs, ID, Some(&fork_wt)),
            super::ClaudeTranscriptLocation::Found {
                path: fork_transcript,
                in_checkout_project: true,
            }
        );

        let empty = super::ClaudeConfigDirs::default();
        assert!(matches!(
            super::locate_claude_transcript_in(&empty, ID, Some(&fork_wt)),
            super::ClaudeTranscriptLocation::Unknown(_)
        ));
    }
}
