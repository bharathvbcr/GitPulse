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
use serde::Deserialize;
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
pub(super) enum Transcript {
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
    if run["provider"] != "claude" {
        return refuse("Only a Claude Code conversation can be resumed from GitPulse.");
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
        _ => run["id"].as_str(),
    }
    .filter(|id| is_canonical_uuid(id));
    let Some(session) = session else {
        return refuse("This attempt has no Claude Code session id to resume.");
    };
    if !checkout_exists {
        return refuse("The checkout this conversation ran in no longer exists.");
    }
    match transcript(session) {
        Transcript::Found => json!({
            "ok": true,
            "resumable": true,
            "session_id": session,
            "cwd": run["cwd"],
            "permission_mode": run["permission_mode"],
            "reason": "Claude Code saved this conversation.",
        }),
        Transcript::Absent => refuse("Claude Code has no saved conversation for this attempt."),
        Transcript::Unknown(why) => json!({
            "ok": true,
            "resumable": false,
            "reason": format!("Could not check for a saved conversation: {why}"),
        }),
    }
}

/// Claude Code's configuration directory: `CLAUDE_CONFIG_DIR`, else
/// `~/.claude`.
pub(crate) fn claude_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".claude"))
}

/// Looks for `<home>/projects/<any>/<session>.jsonl`.
///
/// Every project directory is tried rather than the one Claude Code would
/// derive from the checkout path, because that derivation is Claude Code's
/// own and unpublished; a transcribed copy of it is a copy that drifts. The
/// session id is a canonical UUID by the time it gets here, so it cannot
/// name anything outside the directory it is joined to.
pub(super) fn search(home: &Path, session: &str) -> Transcript {
    let projects = home.join("projects");
    let entries = match std::fs::read_dir(&projects) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Transcript::Absent,
        Err(error) => return Transcript::Unknown(error.to_string()),
    };
    let name = format!("{session}.jsonl");
    for (seen, entry) in entries.enumerate() {
        if seen >= MAX_PROJECTS {
            return Transcript::Unknown(format!(
                "more than {MAX_PROJECTS} Claude Code project directories"
            ));
        }
        match entry {
            Ok(entry) if entry.path().join(&name).is_file() => return Transcript::Found,
            Ok(_) => {}
            Err(error) => return Transcript::Unknown(error.to_string()),
        }
    }
    Transcript::Absent
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
    let run = &response["item"];
    let checkout_exists = run["cwd"]
        .as_str()
        .is_some_and(|cwd| Path::new(cwd).is_dir());
    Ok(judge(run, checkout_exists, |session| match claude_home() {
        Some(home) => search(&home, session),
        None => Transcript::Unknown("no home directory to find Claude Code's in".into()),
    }))
}

#[cfg(test)]
mod tests {
    use super::{judge, search, Transcript};
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
}
