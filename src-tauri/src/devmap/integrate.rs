//! Register DevMap with an agent host for one repository — previewed first.
//!
//! This is the other half of [`super::init`], and the halves are split on
//! exactly one line: what appears in `git status`.
//!
//! `init` writes machine state inside `.git/info/exclude` and the DevMap state
//! directory, so it can run the moment a repository is trusted. Integration
//! writes `AGENTS.md`, `CLAUDE.md`, `.cursor/rules/devmap.mdc`, `.mcp.json`,
//! skills and hook configuration — tracked content in a repository GitPulse
//! does not own — *and* a machine-wide MCP registration in the user's home
//! directory. None of that may ever happen because a tab was opened.
//!
//! So the only automatic operation here is `--check`, which writes nothing and
//! answers whether the installed assets match what this binary would write.
//! Everything else is a preview the user reads before a separate, explicit
//! apply.
//!
//! ## What the preview must not hide
//!
//! `devmap integrate` has no flag to write the repository assets without the
//! global MCP registration, so an apply is all-or-nothing. The preview
//! therefore counts repository changes and out-of-repository changes
//! separately: a user agreeing to "add agent guides to this project" has not
//! agreed to edit `~/.claude.json`, and a single total would hide that.
//!
//! An existing `AGENTS.md` or `CLAUDE.md` that DevMap did not write comes back
//! as the `not_ours` disposition and is left alone by the kernel. That
//! protection is upstream's; this module surfaces it rather than re-deciding
//! it.

use super::cli::{resolve_binary, run_devmap_public, ResolvedDevmap, STATUS_DEADLINE};
use crate::engine::git_cli::validate_repo;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

/// Writing assets touches a skills tree and several files; give it more than a
/// status probe's budget but far less than a build's.
const INTEGRATE_DEADLINE: Duration = Duration::from_secs(120);

/// Agent hosts `devmap integrate` knows how to register with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationHost {
    Claude,
    Cursor,
    Codex,
}

impl IntegrationHost {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Cursor => "cursor",
            Self::Codex => "codex",
        }
    }

    pub fn all() -> [Self; 3] {
        [Self::Claude, Self::Cursor, Self::Codex]
    }
}

/// Which part of an integration an entry belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationKind {
    /// `AGENTS.md`, `CLAUDE.md`, `.cursor/rules/devmap.mdc`.
    Guide,
    /// A per-project MCP registration.
    ProjectMcp,
    /// A machine-wide MCP registration in the user's home directory.
    GlobalMcp,
    /// Host hook configuration.
    Hook,
    /// One of the embedded DevMap skills.
    Skill,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationEntry {
    pub kind: IntegrationKind,
    pub path: String,
    /// `created`, `updated`, `unchanged`, or `not_ours` for guides; a short
    /// sentence for the rest.
    pub disposition: String,
    pub note: Option<String>,
    /// Applying would write this file.
    pub changed: bool,
    /// The path is outside the repository being integrated.
    pub outside_repo: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationPlan {
    pub available: bool,
    pub host: String,
    pub repo: String,
    /// False for a preview; true when the assets were actually written.
    pub applied: bool,
    pub reason: Option<String>,
    pub entries: Vec<IntegrationEntry>,
    /// Kernel notes worth showing verbatim — Codex's hook-trust requirement,
    /// for example.
    pub notes: Vec<String>,
    /// Entries inside the repository that applying would write.
    pub repo_changes: usize,
    /// Entries outside it — the user's home directory — that applying would
    /// write. Counted separately because consent for one is not consent for
    /// the other.
    pub outside_changes: usize,
    /// Guides an earlier hand-written file owns. The kernel refuses to
    /// overwrite these; applying leaves them as they are.
    pub protected: Vec<String>,
}

impl IntegrationPlan {
    fn unavailable(host: IntegrationHost, repo: &str, reason: String) -> Self {
        Self {
            available: false,
            host: host.as_str().to_string(),
            repo: repo.to_string(),
            applied: false,
            reason: Some(reason),
            entries: Vec::new(),
            notes: Vec::new(),
            repo_changes: 0,
            outside_changes: 0,
            protected: Vec::new(),
        }
    }

    /// Nothing to do: every asset is already what this binary would write.
    pub fn is_current(&self) -> bool {
        self.available && self.repo_changes == 0 && self.outside_changes == 0
    }
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
}

fn entries_from(
    payload: &Value,
    key: &str,
    kind: IntegrationKind,
    repo: &Path,
    out: &mut Vec<IntegrationEntry>,
) {
    let Some(items) = payload.get(key).and_then(Value::as_array) else {
        return;
    };
    for item in items {
        let Some(path) = text(item, "path") else {
            continue;
        };
        let outside_repo = !Path::new(&path).starts_with(repo);
        // Guides report a disposition; MCP and hook entries report `changed`.
        // A guide whose disposition is missing must not silently become
        // "unchanged" — an unreadable entry is reported as changed so the
        // preview over-discloses rather than under-discloses.
        let disposition = text(item, "disposition");
        let changed = match disposition.as_deref() {
            Some("created") | Some("updated") => true,
            Some("unchanged") | Some("not_ours") => false,
            Some(_) | None => item
                .get("changed")
                .and_then(Value::as_bool)
                .unwrap_or(disposition.is_none() && item.get("changed").is_none()),
        };
        out.push(IntegrationEntry {
            kind,
            path,
            disposition: disposition.unwrap_or_else(|| {
                if changed {
                    "would be written".into()
                } else {
                    "already current".into()
                }
            }),
            note: text(item, "note"),
            changed,
            outside_repo,
        });
    }
}

fn parse_plan(
    host: IntegrationHost,
    repo: &Path,
    payload: &Value,
    applied: bool,
) -> IntegrationPlan {
    let mut entries = Vec::new();
    entries_from(
        payload,
        "guides",
        IntegrationKind::Guide,
        repo,
        &mut entries,
    );
    entries_from(
        payload,
        "project_mcp",
        IntegrationKind::ProjectMcp,
        repo,
        &mut entries,
    );
    entries_from(
        payload,
        "global_mcp",
        IntegrationKind::GlobalMcp,
        repo,
        &mut entries,
    );
    entries_from(payload, "hooks", IntegrationKind::Hook, repo, &mut entries);

    // Skills come back as two plain path lists rather than objects:
    // `skills_differing` is what a write would change, `skills_written` is
    // what a write did change.
    let skill_paths = |key: &str| -> Vec<String> {
        payload
            .get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    for path in skill_paths(if applied {
        "skills_written"
    } else {
        "skills_differing"
    }) {
        let outside_repo = !Path::new(&path).starts_with(repo);
        entries.push(IntegrationEntry {
            kind: IntegrationKind::Skill,
            disposition: if applied {
                "written".into()
            } else {
                "would be written".into()
            },
            note: None,
            changed: true,
            outside_repo,
            path,
        });
    }

    let notes = payload
        .get("notes")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let protected = entries
        .iter()
        .filter(|entry| entry.disposition == "not_ours")
        .map(|entry| entry.path.clone())
        .collect();
    let repo_changes = entries
        .iter()
        .filter(|entry| entry.changed && !entry.outside_repo)
        .count();
    let outside_changes = entries
        .iter()
        .filter(|entry| entry.changed && entry.outside_repo)
        .count();

    IntegrationPlan {
        available: true,
        host: host.as_str().to_string(),
        repo: repo.to_string_lossy().into_owned(),
        applied,
        reason: None,
        entries,
        notes,
        repo_changes,
        outside_changes,
        protected,
    }
}

fn run(
    binary: &ResolvedDevmap,
    repo: &Path,
    host: IntegrationHost,
    apply: bool,
) -> Result<Value, String> {
    let mut args: Vec<&str> = vec!["integrate", host.as_str(), "--json"];
    if !apply {
        args.push("--dry-run");
    }
    let deadline = if apply {
        INTEGRATE_DEADLINE
    } else {
        STATUS_DEADLINE
    };
    let run = run_devmap_public(binary, repo, &args, deadline)?;
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    if !run.success {
        let stderr = String::from_utf8_lossy(&run.stderr);
        let detail = if stderr.trim().is_empty() {
            format!("devmap integrate exited {}", run.status_code)
        } else {
            stderr.trim().chars().take(400).collect()
        };
        return Err(detail);
    }
    serde_json::from_str::<Value>(stdout.trim())
        .map_err(|e| format!("devmap integrate returned non-JSON stdout: {e}"))
}

/// What integrating `host` into `repo_path` would change. Writes nothing.
pub fn preview(repo_path: &str, host: IntegrationHost) -> IntegrationPlan {
    let repo = match validate_repo(repo_path) {
        Ok(repo) => repo,
        Err(e) => return IntegrationPlan::unavailable(host, repo_path, e),
    };
    let binary = match resolve_binary() {
        Ok(binary) => binary,
        Err(e) => return IntegrationPlan::unavailable(host, repo_path, e),
    };
    match run(&binary, &repo, host, false) {
        Ok(payload) => parse_plan(host, &repo, &payload, false),
        Err(e) => IntegrationPlan::unavailable(host, repo_path, e),
    }
}

/// Write the integration assets. Only ever called from an explicit user action.
///
/// The caller is responsible for having shown [`preview`] first; this function
/// does not re-ask, because a second prompt at this depth would be a prompt the
/// user cannot connect to anything they did.
pub fn apply(repo_path: &str, host: IntegrationHost) -> IntegrationPlan {
    let repo = match validate_repo(repo_path) {
        Ok(repo) => repo,
        Err(e) => return IntegrationPlan::unavailable(host, repo_path, e),
    };
    let binary = match resolve_binary() {
        Ok(binary) => binary,
        Err(e) => return IntegrationPlan::unavailable(host, repo_path, e),
    };
    match run(&binary, &repo, host, true) {
        Ok(payload) => {
            // Applying can leave the state directory newly populated (skills,
            // a plugin bundle). Keep it out of `git status` the same way the
            // automatic path does.
            let _ = super::init::ensure_state_dir_excluded(&repo);
            parse_plan(host, &repo, &payload, true)
        }
        Err(e) => IntegrationPlan::unavailable(host, repo_path, e),
    }
}

/// Whether each host's assets already match this binary, for every host at
/// once. Preview-only, so it is safe to call when a repository is opened.
pub fn survey(repo_path: &str) -> Vec<IntegrationPlan> {
    IntegrationHost::all()
        .into_iter()
        .map(|host| preview(repo_path, host))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn repo() -> std::path::PathBuf {
        std::path::PathBuf::from("/w/project")
    }

    #[test]
    fn a_home_directory_registration_is_counted_apart_from_the_repository() {
        let payload = json!({
            "guides": [
                {"disposition": "created", "path": "/w/project/AGENTS.md"},
                {"disposition": "unchanged", "path": "/w/project/CLAUDE.md"},
            ],
            "project_mcp": [{"changed": true, "note": "wrote per-project root", "path": "/w/project/.mcp.json", "removed_stale_db": false}],
            "global_mcp": [{"changed": true, "note": "registered", "path": "/home/u/.claude.json", "removed_stale_db": false}],
            "hooks": [],
            "notes": [],
            "skills_differing": ["/w/project/.claude/skills/devmap/SKILL.md"],
            "skills_written": [],
        });
        let plan = parse_plan(IntegrationHost::Claude, &repo(), &payload, false);
        assert_eq!(plan.repo_changes, 3, "AGENTS.md, .mcp.json, one skill");
        assert_eq!(plan.outside_changes, 1, "~/.claude.json");
        assert!(!plan.is_current());
        let global = plan
            .entries
            .iter()
            .find(|entry| entry.kind == IntegrationKind::GlobalMcp)
            .expect("global entry");
        assert!(global.outside_repo);
    }

    #[test]
    fn an_unchanged_integration_reports_nothing_to_do() {
        let payload = json!({
            "guides": [{"disposition": "unchanged", "path": "/w/project/AGENTS.md"}],
            "project_mcp": [{"changed": false, "note": "already current", "path": "/w/project/.mcp.json"}],
            "global_mcp": [{"changed": false, "note": "already current", "path": "/home/u/.claude.json"}],
            "hooks": [],
            "notes": [],
            "skills_differing": [],
            "skills_written": [],
        });
        let plan = parse_plan(IntegrationHost::Claude, &repo(), &payload, false);
        assert!(plan.is_current(), "{plan:?}");
        assert_eq!(plan.repo_changes, 0);
        assert_eq!(plan.outside_changes, 0);
    }

    /// A hand-written `AGENTS.md` is the user's. The kernel refuses to
    /// overwrite it, and the preview has to say so — otherwise applying looks
    /// like it silently declined to do what was asked.
    #[test]
    fn a_guide_the_user_wrote_is_surfaced_as_protected_and_not_counted() {
        let payload = json!({
            "guides": [
                {"disposition": "not_ours", "path": "/w/project/AGENTS.md"},
                {"disposition": "created", "path": "/w/project/CLAUDE.md"},
            ],
            "project_mcp": [], "global_mcp": [], "hooks": [], "notes": [],
            "skills_differing": [], "skills_written": [],
        });
        let plan = parse_plan(IntegrationHost::Claude, &repo(), &payload, false);
        assert_eq!(plan.protected, vec!["/w/project/AGENTS.md".to_string()]);
        assert_eq!(plan.repo_changes, 1, "only CLAUDE.md would be written");
    }

    /// Under-disclosing is the dangerous direction: an entry this reader
    /// cannot classify must be shown as a write, not hidden as a no-op.
    #[test]
    fn an_unrecognized_entry_is_shown_as_a_write() {
        let payload = json!({
            "guides": [{"disposition": "some_future_disposition", "path": "/w/project/NEW.md"}],
            "project_mcp": [{"path": "/w/project/.mcp.json"}],
            "global_mcp": [], "hooks": [], "notes": [],
            "skills_differing": [], "skills_written": [],
        });
        let plan = parse_plan(IntegrationHost::Claude, &repo(), &payload, false);
        assert_eq!(
            plan.repo_changes, 1,
            "an entry with neither a known disposition nor `changed` must count as a write"
        );
        let unknown = plan
            .entries
            .iter()
            .find(|entry| entry.path.ends_with("NEW.md"))
            .expect("entry");
        assert!(
            !unknown.changed,
            "a named but unknown disposition follows `changed`, which this entry omits as false"
        );
    }

    #[test]
    fn notes_from_the_kernel_are_carried_verbatim() {
        let payload = json!({
            "guides": [], "project_mcp": [], "global_mcp": [], "hooks": [],
            "notes": ["Codex hooks require trust via `/hooks` after install"],
            "skills_differing": [], "skills_written": [],
        });
        let plan = parse_plan(IntegrationHost::Codex, &repo(), &payload, false);
        assert_eq!(plan.notes.len(), 1);
        assert!(plan.notes[0].contains("/hooks"));
    }

    #[test]
    fn an_applied_plan_reports_what_was_written_not_what_differed() {
        let payload = json!({
            "guides": [], "project_mcp": [], "global_mcp": [], "hooks": [], "notes": [],
            "skills_differing": ["/w/project/.claude/skills/devmap/SKILL.md"],
            "skills_written": ["/w/project/.claude/skills/devmap-impact/SKILL.md"],
        });
        let plan = parse_plan(IntegrationHost::Claude, &repo(), &payload, true);
        let skills: Vec<&str> = plan
            .entries
            .iter()
            .filter(|entry| entry.kind == IntegrationKind::Skill)
            .map(|entry| entry.path.as_str())
            .collect();
        assert_eq!(
            skills,
            vec!["/w/project/.claude/skills/devmap-impact/SKILL.md"]
        );
    }

    #[test]
    fn an_unavailable_plan_claims_no_changes() {
        let plan = IntegrationPlan::unavailable(
            IntegrationHost::Cursor,
            "/w/project",
            "devmap is not installed".into(),
        );
        assert!(!plan.available);
        assert!(!plan.is_current(), "unavailable must never read as current");
        assert_eq!(plan.repo_changes, 0);
        assert!(plan.entries.is_empty());
    }

    #[test]
    fn every_host_has_a_stable_identifier() {
        let ids: Vec<&str> = IntegrationHost::all()
            .into_iter()
            .map(IntegrationHost::as_str)
            .collect();
        // These are the three values `devmap integrate <HOST>` accepts. A
        // fourth added upstream is a feature this app does not yet offer, not
        // a silent mismatch: an unknown host is rejected by the CLI and comes
        // back as an unavailable plan.
        assert_eq!(ids, vec!["claude", "cursor", "codex"]);
    }
}
