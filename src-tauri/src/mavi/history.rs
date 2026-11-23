//! A conversation's earlier turns, read from Claude Code's own transcript.
//!
//! When Mavi resumes a conversation (after a quit, after "Bring back to Mavi",
//! or taking one over from a terminal tab) the agent process remembers
//! everything and the page remembers nothing. The transcript Claude Code keeps
//! at `<config>/projects/<encoded cwd>/<session>.jsonl` is the record; this
//! module turns it into the same lines the timeline is built from, so earlier
//! turns render exactly as live ones do (tool rows, edits with their hunks,
//! thinking, interruptions), and seeds them into the session's replay ring.
//!
//! The transcript is a superset of the stream-json wire. Kept: the person's
//! prompts (`user` with text content, as `mavi_user`), assistant blocks
//! (`assistant`, as is), tool results (`user` with `tool_result` blocks; the
//! `toolUseResult` field becomes the wire's `tool_use_result`), interruption
//! markers, and compaction boundaries. Dropped: subagent side-chains,
//! attachments, queue and cost bookkeeping, meta and local-command entries.
//! Turn ends are not recorded in a transcript; a synthetic `result` closes
//! each turn before the next prompt (marked `mavi_history`, with no cost or
//! duration, which the transcript does not say per turn).
//!
//! Every produced line goes through [`super::wire::prepare`], so a history
//! line is slimmed and redacted exactly as a live one is. Bounded: the last
//! [`MAX_READ_BYTES`] of the file and the last [`MAX_LINES`] produced lines;
//! a cut is announced in the opening marker.

use super::wire;
use crate::workbench::conversation::{
    claude_config_dirs, locate_claude_transcript_in, ClaudeTranscriptLocation,
};
use serde_json::{json, Value};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// The most of a transcript read: its tail, where the recent turns are.
pub(crate) const MAX_READ_BYTES: u64 = 16 * 1024 * 1024;
/// The most timeline lines a history seeds.
pub(crate) const MAX_LINES: usize = 4_000;

/// Earlier turns, ready for the ring.
#[derive(Debug, Default)]
pub(crate) struct History {
    pub lines: Vec<String>,
    /// Prompts seen, so the next turn continues the numbering.
    pub turns: u32,
}

/// Where a conversation's transcript is, relative to a checkout.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MaviTranscriptState {
    /// Saved for this checkout: resuming here finds it.
    Found,
    /// Saved for another directory: Claude Code resumes it only there.
    Elsewhere,
    Absent,
    Unknown,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MaviConversationCheck {
    pub state: MaviTranscriptState,
    pub reason: Option<String>,
    /// The worktree root the conversation would resume in, canonical, so the
    /// page can tell whether a Mavi thread already works there.
    pub root: Option<String>,
}

/// Where `conversation`'s transcript is, for a resume in `root`.
pub(crate) fn check(root: &Path, conversation: &str) -> MaviConversationCheck {
    match locate_claude_transcript_in(&claude_config_dirs(), conversation, Some(root)) {
        ClaudeTranscriptLocation::Found { in_checkout_project: true, .. } => MaviConversationCheck {
            state: MaviTranscriptState::Found,
            reason: None,
            root: Some(root.to_string_lossy().into_owned()),
        },
        ClaudeTranscriptLocation::Found { .. } => MaviConversationCheck {
            state: MaviTranscriptState::Elsewhere,
            reason: Some(
                "Claude Code saved this conversation for another directory; it can only continue there.".into(),
            ),
            root: None,
        },
        ClaudeTranscriptLocation::Absent => MaviConversationCheck {
            state: MaviTranscriptState::Absent,
            reason: Some("Claude Code saved no conversation with this id.".into()),
            root: None,
        },
        ClaudeTranscriptLocation::Unknown(why) => MaviConversationCheck {
            state: MaviTranscriptState::Unknown,
            reason: Some(format!("Couldn't look for the conversation: {why}")),
            root: None,
        },
    }
}

/// Reads and converts the transcript of `conversation` saved for `root`.
pub(crate) fn load(root: &Path, conversation: &str) -> Result<History, String> {
    let path = match locate_claude_transcript_in(&claude_config_dirs(), conversation, Some(root)) {
        ClaudeTranscriptLocation::Found {
            path,
            in_checkout_project: true,
        } => path,
        ClaudeTranscriptLocation::Found { .. } => {
            return Err("Claude Code saved this conversation for another directory.".into())
        }
        ClaudeTranscriptLocation::Absent => {
            return Err("Claude Code saved no conversation with this id.".into())
        }
        ClaudeTranscriptLocation::Unknown(why) => {
            return Err(format!("Couldn't look for the transcript: {why}"))
        }
    };
    let (text, cut) = read_tail(&path, MAX_READ_BYTES)
        .map_err(|e| format!("Couldn't read the transcript: {e}"))?;
    Ok(convert(&text, cut))
}

/// The last `max` bytes of `path`, starting at a line boundary when cut.
fn read_tail(path: &Path, max: u64) -> std::io::Result<(String, bool)> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let cut = len > max;
    if cut {
        file.seek(SeekFrom::Start(len - max))?;
    }
    let mut bytes = Vec::new();
    file.take(max).read_to_end(&mut bytes)?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if cut {
        // The first line is a fragment of a longer one.
        text = text
            .split_once('\n')
            .map(|(_, rest)| rest.to_owned())
            .unwrap_or_default();
    }
    Ok((text, cut))
}

/// The prompt text of a `user` entry, or None when it is not a prompt.
fn prompt_text(entry: &Value) -> Option<String> {
    if entry.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let content = entry.pointer("/message/content")?;
    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => {
            if blocks
                .iter()
                .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            {
                return None;
            }
            blocks
                .iter()
                .filter_map(|b| match b.get("type").and_then(Value::as_str) {
                    Some("text") => b.get("text").and_then(Value::as_str).map(str::to_owned),
                    Some("image") => Some("[image]".to_owned()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        _ => return None,
    };
    let trimmed = text.trim();
    // Slash-command plumbing and caveats Claude Code records, not words the
    // person typed to the agent.
    if trimmed.is_empty()
        || trimmed.starts_with("<command-")
        || trimmed.starts_with("<local-command")
        || trimmed.starts_with("Caveat:")
        || trimmed.starts_with("[Request interrupted")
    {
        return None;
    }
    Some(text)
}

fn is_interruption(entry: &Value) -> bool {
    let text = match entry.pointer("/message/content") {
        Some(Value::String(text)) => text.as_str(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .find_map(|b| b.get("text").and_then(Value::as_str))
            .unwrap_or_default(),
        _ => "",
    };
    text.trim_start().starts_with("[Request interrupted")
}

fn timestamp_ms(entry: &Value) -> Option<u64> {
    crate::ledger::ids::parse_event_time(entry.get("timestamp")?.as_str()?)
}

/// Converts transcript text into timeline lines. Pure, so it is tested
/// against real transcript shapes.
pub(crate) fn convert(text: &str, cut: bool) -> History {
    let mut out: Vec<String> = Vec::new();
    let mut turns: u32 = 0;
    let mut open = false;
    let mut interrupted = false;
    let close = |out: &mut Vec<String>, interrupted: bool| {
        out.push(
            json!({
                "type": "result",
                "subtype": if interrupted { "error_during_execution" } else { "success" },
                "mavi_history": true,
            })
            .to_string(),
        );
    };
    for raw in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        if entry.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        match entry.get("type").and_then(Value::as_str) {
            Some("user") => {
                if let Some(prompt) = prompt_text(&entry) {
                    if open {
                        close(&mut out, interrupted);
                    }
                    turns += 1;
                    open = true;
                    interrupted = false;
                    // A turn Claude Code started itself (a background task
                    // reporting back) is recorded as a prompt with a system
                    // source; it is not the person's words.
                    let automatic = entry.get("promptSource").and_then(Value::as_str)
                        == Some("system")
                        || entry.get("origin").is_some_and(Value::is_object);
                    let mut line = if automatic {
                        json!({"type": "mavi_user", "text": "", "turn": turns, "history": true, "automatic": true})
                    } else {
                        json!({"type": "mavi_user", "text": prompt.trim_end(), "turn": turns, "history": true})
                    };
                    if let Some(at) = timestamp_ms(&entry) {
                        line["at"] = json!(at);
                    }
                    out.push(line.to_string());
                } else if is_interruption(&entry) {
                    interrupted = true;
                    out.push(json!({"type": "user", "message": entry.get("message").cloned().unwrap_or(Value::Null)}).to_string());
                } else if entry
                    .pointer("/message/content")
                    .is_some_and(Value::is_array)
                {
                    let mut line = json!({"type": "user", "message": entry.get("message").cloned().unwrap_or(Value::Null)});
                    if let Some(result) = entry.get("toolUseResult").filter(|r| r.is_object()) {
                        line["tool_use_result"] = result.clone();
                    }
                    out.push(line.to_string());
                }
            }
            Some("assistant") => {
                if let Some(message) = entry.get("message").filter(|m| m.is_object()) {
                    out.push(json!({"type": "assistant", "message": message}).to_string());
                }
            }
            Some("system")
                if entry.get("subtype").and_then(Value::as_str) == Some("compact_boundary") =>
            {
                out.push(json!({"type": "system", "subtype": "compact_boundary"}).to_string());
            }
            _ => {}
        }
    }
    if open {
        close(&mut out, interrupted);
    }
    let mut truncated = cut;
    if out.len() > MAX_LINES {
        out.drain(..out.len() - MAX_LINES);
        truncated = true;
    }
    let mut lines = Vec::with_capacity(out.len() + 2);
    lines.push(wire::synthetic(
        "mavi_history",
        json!({"available": true, "turns": turns, "truncated": truncated}),
    ));
    lines.extend(out.into_iter().map(|line| wire::prepare(line).text));
    lines.push(wire::synthetic("mavi_history_end", json!({"turns": turns})));
    History { lines, turns }
}

/// The marker a resume seeds when no history could be read.
pub(crate) fn unavailable(reason: &str) -> History {
    History {
        lines: vec![wire::synthetic(
            "mavi_history",
            json!({"available": false, "reason": reason}),
        )],
        turns: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Entry shapes from a real Claude Code 2.1.330 transcript: a prompt, the
    /// bookkeeping around it, an edit with its result, a second prompt that
    /// was interrupted mid-answer.
    fn transcript() -> String {
        [
            json!({"type":"queue-operation","operation":"enqueue","content":"x"}),
            json!({"type":"user","isSidechain":false,"timestamp":"2026-11-23T06:33:40.000Z","message":{"role":"user","content":"Append world to a.txt"}}),
            json!({"type":"attachment","isSidechain":false,"attachment":{"type":"hook"}}),
            json!({"type":"assistant","isSidechain":false,"message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Edit","input":{"file_path":"/r/a.txt","old_string":"hello\n","new_string":"hello\nworld\n"}}]}}),
            json!({"type":"user","isSidechain":false,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]},"toolUseResult":{"filePath":"/r/a.txt","originalFile":"hello\n","structuredPatch":[{"oldStart":1,"oldLines":1,"newStart":1,"newLines":2,"lines":[" hello","+world"]}]}}),
            json!({"type":"assistant","isSidechain":true,"message":{"id":"side","content":[{"type":"text","text":"subagent"}]}}),
            json!({"type":"assistant","isSidechain":false,"message":{"id":"m2","content":[{"type":"text","text":"Done."}]}}),
            json!({"type":"user","isMeta":true,"message":{"role":"user","content":"<command-name>/clear</command-name>"}}),
            json!({"type":"user","isSidechain":false,"message":{"role":"user","content":"Write a long poem"}}),
            json!({"type":"assistant","isSidechain":false,"isAbortedMidStream":true,"message":{"id":"m3","content":[{"type":"text","text":"1. In the beginning"}]}}),
            json!({"type":"user","isSidechain":false,"message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]}}),
            json!({"type":"user","isSidechain":false,"promptSource":"system","origin":{"kind":"task-notification"},"message":{"role":"user","content":"<task-notification>done</task-notification>"}}),
            json!({"type":"cost-state","totalCostUSD":0.2}),
        ]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
    }

    fn parsed(history: &History) -> Vec<Value> {
        history
            .lines
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn a_transcript_becomes_the_lines_a_live_turn_would_have_made() {
        let history = convert(&transcript(), false);
        assert_eq!(history.turns, 3);
        let lines = parsed(&history);
        let kinds: Vec<&str> = lines.iter().map(|l| l["type"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            vec![
                "mavi_history",
                "mavi_user",
                "assistant",
                "user",
                "assistant",
                "result",
                "mavi_user",
                "assistant",
                "user",
                "result",
                "mavi_user",
                "result",
                "mavi_history_end",
            ]
        );
        assert_eq!(lines[0]["turns"], 3);
        assert_eq!(
            lines[10]["automatic"], true,
            "a background task's report is not the person's words"
        );
        assert_eq!(lines[10]["text"], "");
        assert_eq!(lines[1]["text"], "Append world to a.txt");
        assert_eq!(lines[1]["turn"], 1);
        assert!(lines[1]["at"].as_i64().is_some());
        // The edit keeps its exact hunks under the wire's name, and loses the
        // whole-file copy, as a live line does.
        assert_eq!(
            lines[3]["tool_use_result"]["structuredPatch"][0]["lines"][1],
            "+world"
        );
        assert!(lines[3]["tool_use_result"].get("originalFile").is_none());
        assert_eq!(lines[5]["subtype"], "success");
        assert_eq!(lines[6]["turn"], 2);
        assert_eq!(
            lines[9]["subtype"], "error_during_execution",
            "an interrupted turn ends as one"
        );
        assert!(
            !history.lines.iter().any(|l| l.contains("subagent")),
            "side-chains are dropped"
        );
        assert!(
            !history.lines.iter().any(|l| l.contains("/clear")),
            "command plumbing is dropped"
        );
    }

    #[test]
    fn history_is_bounded_and_says_so() {
        let mut text = String::new();
        for i in 0..(MAX_LINES + 10) {
            text.push_str(
                &json!({"type":"user","message":{"role":"user","content":format!("prompt {i}")}})
                    .to_string(),
            );
            text.push('\n');
        }
        let history = convert(&text, false);
        let lines = parsed(&history);
        assert_eq!(lines[0]["truncated"], true);
        assert!(history.lines.len() <= MAX_LINES + 2);
        assert_eq!(history.turns as usize, MAX_LINES + 10);
    }

    #[test]
    fn a_cut_read_drops_the_partial_first_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, "aaaaaaaaaa\nbb\ncc\n").unwrap();
        let (text, cut) = read_tail(&path, 8).unwrap();
        assert!(cut);
        assert_eq!(text, "bb\ncc\n");
        let (whole, cut) = read_tail(&path, 1024).unwrap();
        assert!(!cut);
        assert!(whole.starts_with("aaaa"));
    }

    #[test]
    fn credential_shapes_in_history_are_redacted() {
        let token = format!("ghp_{}", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8");
        let text =
            json!({"type":"user","message":{"role":"user","content":format!("use {token}")}})
                .to_string();
        let history = convert(&text, false);
        assert!(!history.lines.iter().any(|l| l.contains(&token)));
    }
}

#[cfg(test)]
mod real_transcript {
    /// Converts a real transcript into the frontend's history fixture.
    /// `MAVI_TRANSCRIPT=<file> MAVI_HISTORY_OUT=<file> cargo test --lib -- --ignored mavi_history_dump`
    #[test]
    #[ignore]
    fn mavi_history_dump() {
        let (Ok(input), Ok(output)) = (
            std::env::var("MAVI_TRANSCRIPT"),
            std::env::var("MAVI_HISTORY_OUT"),
        ) else {
            return;
        };
        let text = std::fs::read_to_string(input).unwrap();
        let history = super::convert(&text, false);
        std::fs::write(output, history.lines.join("\n") + "\n").unwrap();
    }
}
