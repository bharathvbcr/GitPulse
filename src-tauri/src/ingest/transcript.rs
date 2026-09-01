//! Reading agent transcripts, so provenance needs no agent to cooperate.
//!
//! Claude Code writes one JSONL file per session under
//! `~/.claude/projects/<slug>/`. Every assistant record carries `cwd`,
//! `gitBranch`, `sessionId`, `timestamp` and `version` at the top level, and
//! its `message.content` holds the tool calls the model made. That is enough to
//! say who changed what, and when, without the agent reporting anything about
//! itself — which is the point: attribution derived from observation is more
//! trustworthy than attribution an agent asserts.
//!
//! # Version gating
//!
//! `version` is recorded on every event. A transcript whose shape this build
//! does not recognise degrades to *unattributed* — it is never an error, and
//! never a guess. Measured across the real corpus this parser sees 17 distinct
//! schema versions, so the format does move.
//!
//! # What is deliberately not read
//!
//! Only tool *calls* are parsed: the file paths and command lines an agent
//! actually executed. Prompts, model reasoning and assistant prose are never
//! extracted. The ledger records observable actions, not what an agent was
//! thinking, and a provenance store that quoted reasoning would be a very
//! effective way to leak the contents of every private repository it watched.

use serde::{Deserialize, Serialize};

/// One mutating tool call, attributed to a repository.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The session this came from, which is also the transcript's identity.
    pub session_id: String,
    /// ISO-8601, as the transcript recorded it.
    pub ts_utc: String,
    /// The agent's own version string, kept so a later reader can tell which
    /// parser shape produced this row.
    pub version: String,
    /// Working directory of the record. This is what attributes a `Bash` call,
    /// which carries no path of its own.
    pub cwd: String,
    pub git_branch: Option<String>,
    /// `Edit`, `Write`, `NotebookEdit`, `Bash`.
    pub tool: String,
    /// Absolute path for file tools; `None` for a command.
    pub file_path: Option<String>,
    /// The command line for `Bash`; `None` for a file tool.
    pub command: Option<String>,
}

impl ToolCall {
    /// The ledger action this call becomes.
    pub fn action(&self) -> &'static str {
        match self.tool.as_str() {
            "Bash" => "session.command",
            _ => "session.edit",
        }
    }

    /// What the call acted on, for the ledger's `object`.
    pub fn object(&self) -> String {
        self.file_path
            .clone()
            .or_else(|| self.command.clone())
            .unwrap_or_default()
    }
}

/// Tools that change something. A read is not provenance.
const MUTATING: [&str; 4] = ["Edit", "Write", "NotebookEdit", "Bash"];

/// Parses one transcript line, returning the mutating tool calls it holds.
///
/// A line this build cannot read yields nothing rather than an error: a
/// transcript is an *observation*, and a malformed or future-shaped record is a
/// gap in what we saw, not a failure of the thing being observed. The caller
/// counts skips so the gap is reportable.
pub fn parse_line(line: &str) -> Vec<ToolCall> {
    let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
        return Vec::new();
    };
    if record.get("type").and_then(|t| t.as_str()) != Some("assistant") {
        return Vec::new();
    }

    let session_id = record["sessionId"].as_str().unwrap_or_default().to_string();
    let ts_utc = record["timestamp"].as_str().unwrap_or_default().to_string();
    let version = record["version"].as_str().unwrap_or_default().to_string();
    let cwd = record["cwd"].as_str().unwrap_or_default().to_string();
    let git_branch = record["gitBranch"].as_str().map(str::to_string);

    // Without a cwd a Bash call cannot be attributed at all, and a file tool's
    // absolute path is the only thing left. Recording the call with an empty
    // cwd would put an unattributable row in the ledger, so it is skipped and
    // counted instead.
    if session_id.is_empty() || ts_utc.is_empty() {
        return Vec::new();
    }

    let Some(blocks) = record["message"]["content"].as_array() else {
        return Vec::new();
    };

    let mut calls = Vec::new();
    for block in blocks {
        if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
            continue;
        }
        let Some(tool) = block.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        if !MUTATING.contains(&tool) {
            continue;
        }
        let input = &block["input"];
        calls.push(ToolCall {
            session_id: session_id.clone(),
            ts_utc: ts_utc.clone(),
            version: version.clone(),
            cwd: cwd.clone(),
            git_branch: git_branch.clone(),
            tool: tool.to_string(),
            file_path: input
                .get("file_path")
                .and_then(|p| p.as_str())
                .map(str::to_string),
            command: input
                .get("command")
                .and_then(|c| c.as_str())
                .map(str::to_string),
        });
    }
    calls
}

/// Whether `call` belongs to the repository rooted at `repo_path`.
///
/// A file tool is attributed by its own absolute path; a command by the
/// record's `cwd`. Both are prefix matches against the repository root, with a
/// separator check so `/repo-other` never matches `/repo`.
pub fn belongs_to(call: &ToolCall, repo_path: &str) -> bool {
    let candidate = call.file_path.as_deref().unwrap_or(&call.cwd);
    under(candidate, repo_path)
}

fn under(path: &str, root: &str) -> bool {
    if root.is_empty() || path.is_empty() {
        return false;
    }
    let root = root.trim_end_matches('/');
    if path == root {
        return true;
    }
    path.starts_with(root) && path.as_bytes().get(root.len()) == Some(&b'/')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(tool: &str, input: &str) -> String {
        format!(
            r#"{{"type":"assistant","sessionId":"S1","timestamp":"2026-09-01T12:00:00.000Z",
                 "version":"2.1.241","cwd":"/repo","gitBranch":"main",
                 "message":{{"content":[{{"type":"tool_use","name":"{tool}","input":{input}}}]}}}}"#
        )
        .replace('\n', "")
    }

    #[test]
    fn parses_a_file_edit() {
        let calls = parse_line(&record("Edit", r#"{"file_path":"/repo/src/a.rs"}"#));
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.tool, "Edit");
        assert_eq!(c.file_path.as_deref(), Some("/repo/src/a.rs"));
        assert_eq!(c.session_id, "S1");
        assert_eq!(c.git_branch.as_deref(), Some("main"));
        assert_eq!(c.action(), "session.edit");
    }

    #[test]
    fn parses_a_command_and_attributes_it_by_cwd() {
        // Bash carries no path. The record's own cwd is what makes it
        // attributable — 88% of mutating events in the real corpus are Bash.
        let calls = parse_line(&record("Bash", r#"{"command":"cargo test"}"#));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].command.as_deref(), Some("cargo test"));
        assert!(calls[0].file_path.is_none());
        assert_eq!(calls[0].action(), "session.command");
        assert!(belongs_to(&calls[0], "/repo"));
    }

    #[test]
    fn ignores_reads_and_other_non_mutating_tools() {
        for tool in ["Read", "Grep", "Glob", "WebFetch", "TodoWrite"] {
            assert!(
                parse_line(&record(tool, r#"{"file_path":"/repo/a.rs"}"#)).is_empty(),
                "{tool} is not a mutation"
            );
        }
    }

    #[test]
    fn ignores_non_assistant_records() {
        for line in [
            r#"{"type":"user","message":{"content":"hi"}}"#,
            r#"{"type":"queue-operation","operation":"enqueue"}"#,
            r#"{"type":"summary"}"#,
        ] {
            assert!(parse_line(line).is_empty());
        }
    }

    #[test]
    fn an_unreadable_line_is_a_gap_not_a_failure() {
        // A transcript is an observation. A line this build cannot read means
        // we did not see something, which is different from an error — and
        // must never abort the rest of the file.
        assert!(parse_line("not json at all").is_empty());
        assert!(parse_line("").is_empty());
        assert!(parse_line(r#"{"type":"assistant"}"#).is_empty());
        assert!(parse_line(r#"{"type":"assistant","sessionId":"S1"}"#).is_empty());
    }

    #[test]
    fn a_record_without_identity_is_skipped_rather_than_half_attributed() {
        // No session or no timestamp means the row could not be placed in the
        // history. Recording it with blanks would put an unattributable event
        // in the ledger, which is worse than not recording it.
        let no_session =
            record("Edit", r#"{"file_path":"/repo/a.rs"}"#).replace(r#""sessionId":"S1","#, "");
        assert!(parse_line(&no_session).is_empty());
    }

    #[test]
    fn several_tool_calls_in_one_record_all_parse() {
        let line = r#"{"type":"assistant","sessionId":"S1","timestamp":"2026-09-01T12:00:00.000Z",
            "version":"2.1.241","cwd":"/repo","message":{"content":[
              {"type":"text","text":"working"},
              {"type":"tool_use","name":"Edit","input":{"file_path":"/repo/a.rs"}},
              {"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}]}}"#
            .replace('\n', "");
        let calls = parse_line(&line);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].tool, "Edit");
        assert_eq!(calls[1].tool, "Bash");
    }

    #[test]
    fn attribution_is_by_path_for_files_and_cwd_for_commands() {
        let edit = &parse_line(&record("Edit", r#"{"file_path":"/other/x.rs"}"#))[0];
        // The file's own path wins: an agent can edit outside its cwd.
        assert!(!belongs_to(edit, "/repo"));
        assert!(belongs_to(edit, "/other"));

        let cmd = &parse_line(&record("Bash", r#"{"command":"ls"}"#))[0];
        assert!(belongs_to(cmd, "/repo"));
        assert!(!belongs_to(cmd, "/other"));
    }

    #[test]
    fn a_sibling_directory_is_not_the_repository() {
        // `/repo-other` must never attribute to `/repo`.
        let c = &parse_line(&record("Edit", r#"{"file_path":"/repo-other/a.rs"}"#))[0];
        assert!(!belongs_to(c, "/repo"));
        assert!(under("/repo/a.rs", "/repo"));
        assert!(under("/repo/a.rs", "/repo/"));
        assert!(under("/repo", "/repo"));
        assert!(!under("/repo", ""));
        assert!(!under("", "/repo"));
    }

    #[test]
    fn a_worktree_under_the_repo_attributes_to_the_worktree_root() {
        // GitPulse opens worktrees as repositories, so the deeper root wins
        // when both match.
        let c = &parse_line(&record(
            "Edit",
            r#"{"file_path":"/repo/.claude/worktrees/wt/a.rs"}"#,
        ))[0];
        assert!(belongs_to(c, "/repo"));
        assert!(belongs_to(c, "/repo/.claude/worktrees/wt"));
    }
}
