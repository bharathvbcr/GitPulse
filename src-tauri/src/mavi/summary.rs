//! End-of-session summarization for Mavi threads.
//!
//! Builds a bounded, secret-redacted digest from a live session's replay ring
//! (or the page's fallback timeline transcript) and summarizes it using either
//! `claude -p --model haiku` or GitPulse's configured AI route (`Manvi` / local
//! model server).

use super::{command_for, resolve, session, MaviState};
use crate::ai::prompt::{
    budget_text, session_summary_system, session_summary_user, strip_think_tags,
};
use crate::ipc_error::CommandError;
use crate::workbench::terminal_command::validate_model_id;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::io::{Read, Write};
use std::process::Stdio;
use std::time::{Duration, Instant};

pub(crate) const MAX_DIGEST_BYTES: usize = 24_000;
const MAX_TOUCHED_FILES: usize = 40;
const MAX_TEXT_FIELD_CHARS: usize = 2_000;
const MAX_TOOL_ARG_CHARS: usize = 240;
const CLAUDE_SUMMARY_TIMEOUT: Duration = Duration::from_secs(45);
const CLAUDE_MAX_OUTPUT_BYTES: usize = 64 * 1024;
const CLAUDE_MAX_STDERR_BYTES: usize = 16 * 1024;
const DEFAULT_CLAUDE_MODEL: &str = "haiku";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SummarizeRequest {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub transcript: Option<String>,
    #[serde(default)]
    pub touched: Vec<String>,
    #[serde(default)]
    pub repo_path: Option<String>,
    #[serde(default)]
    pub engine: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MaviWorkSummary {
    pub text: String,
    pub model: String,
    pub engine: String,
    pub elapsed_ms: u64,
    pub truncated: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionDigest {
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SummaryEngine {
    Claude { model: String },
    Ai,
}

pub(crate) fn parse_engine(
    engine: Option<&str>,
    model: Option<&str>,
) -> Result<SummaryEngine, CommandError> {
    let kind = engine
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("claude");
    match kind {
        "claude" => {
            let model = model
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(DEFAULT_CLAUDE_MODEL);
            validate_model_id("model", model).map_err(CommandError::plain)?;
            Ok(SummaryEngine::Claude {
                model: model.to_owned(),
            })
        }
        "ai" => Ok(SummaryEngine::Ai),
        other => Err(CommandError::plain(format!(
            "Unsupported summary engine `{other}`. Expected `claude` or `ai`."
        ))),
    }
}

pub(crate) fn claude_summary_args(
    model: &str,
    system_prompt: &str,
) -> Result<Vec<String>, CommandError> {
    validate_model_id("model", model).map_err(CommandError::plain)?;
    Ok(vec![
        "-p".into(),
        "--model".into(),
        model.to_owned(),
        "--no-session-persistence".into(),
        "--output-format".into(),
        "text".into(),
        "--tools".into(),
        String::new(),
        "--system-prompt".into(),
        system_prompt.to_owned(),
    ])
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn single_line_preview(text: &str, max_chars: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&collapsed, max_chars)
}

fn tool_primary_arg(input: Option<&Value>) -> Option<String> {
    let obj = input?.as_object()?;
    for key in [
        "file_path",
        "notebook_path",
        "path",
        "command",
        "pattern",
        "description",
        "url",
        "query",
    ] {
        if let Some(val) = obj.get(key).and_then(Value::as_str) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(single_line_preview(trimmed, MAX_TOOL_ARG_CHARS));
            }
        }
    }
    None
}

fn extract_activity_lines(entries: &[session::MaviEntry]) -> Vec<String> {
    let mut lines = Vec::new();
    for entry in entries {
        let Ok(value) = serde_json::from_str::<Value>(&entry.line) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some("mavi_user") => {
                if value.get("automatic").and_then(Value::as_bool) == Some(true) {
                    continue;
                }
                if let Some(text) = value
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    lines.push(format!(
                        "User: {}",
                        truncate_chars(text, MAX_TEXT_FIELD_CHARS)
                    ));
                }
            }
            Some("assistant") => {
                let is_subagent = value
                    .get("parent_tool_use_id")
                    .and_then(Value::as_str)
                    .is_some();
                let blocks = value.pointer("/message/content").and_then(Value::as_array);
                for block in blocks.into_iter().flatten() {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") if !is_subagent => {
                            if let Some(text) = block
                                .get("text")
                                .and_then(Value::as_str)
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                            {
                                lines.push(format!(
                                    "Assistant: {}",
                                    truncate_chars(text, MAX_TEXT_FIELD_CHARS)
                                ));
                            }
                        }
                        Some("tool_use") => {
                            let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                            match tool_primary_arg(block.get("input")) {
                                Some(arg) => lines.push(format!("Tool {name}: {arg}")),
                                None => lines.push(format!("Tool {name}")),
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("system")
                if value.get("subtype").and_then(Value::as_str) == Some("post_turn_summary") =>
            {
                let category = value
                    .get("status_category")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .unwrap_or("");
                let detail = value
                    .get("status_detail")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .unwrap_or("");
                let needs_action = value
                    .get("needs_action")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .unwrap_or("");
                if !category.is_empty() || !detail.is_empty() || !needs_action.is_empty() {
                    let mut summary = String::from("Turn summary");
                    if !category.is_empty() {
                        summary.push_str(&format!(" [{category}]"));
                    }
                    if !detail.is_empty() {
                        summary.push_str(&format!(
                            ": {}",
                            truncate_chars(detail, MAX_TEXT_FIELD_CHARS)
                        ));
                    }
                    if !needs_action.is_empty() {
                        summary.push_str(&format!(
                            " (needs action: {})",
                            truncate_chars(needs_action, 500)
                        ));
                    }
                    lines.push(summary);
                }
            }
            Some("result") => {
                let subtype = value
                    .get("subtype")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let is_error = value.get("is_error").and_then(Value::as_bool) == Some(true);
                if is_error || subtype != "success" {
                    lines.push(format!("Turn result: {subtype}"));
                }
            }
            Some("mavi_exit") => {
                let exit = value.get("exit").and_then(Value::as_str).unwrap_or("ended");
                let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
                let mut msg = format!("Session exited ({exit}, ok={ok})");
                if !ok {
                    if let Some(err_line) =
                        value
                            .get("stderr")
                            .and_then(Value::as_array)
                            .and_then(|arr| {
                                arr.iter()
                                    .filter_map(Value::as_str)
                                    .find(|s| !s.trim().is_empty())
                            })
                    {
                        msg.push_str(&format!(": {}", single_line_preview(err_line, 240)));
                    }
                }
                lines.push(msg);
            }
            // Deliberately skip `user` (tool_result blobs), `stream_event`, and noisy markers.
            _ => {}
        }
    }
    lines
}

fn dedupe_touched(session_touched: &[String], request_touched: &[String]) -> (Vec<String>, usize) {
    let mut seen = HashSet::new();
    let mut kept = Vec::new();
    let mut total = 0usize;
    for path in session_touched.iter().chain(request_touched.iter()) {
        let trimmed = path.trim();
        if trimmed.is_empty() || !seen.insert(trimmed.to_owned()) {
            continue;
        }
        total += 1;
        if kept.len() < MAX_TOUCHED_FILES {
            kept.push(trimmed.to_owned());
        }
    }
    let omitted = total.saturating_sub(kept.len());
    (kept, omitted)
}

pub(crate) fn build_session_digest(
    title: Option<&str>,
    session_entries: Option<&[session::MaviEntry]>,
    session_touched: &[String],
    request_touched: &[String],
    fallback_transcript: Option<&str>,
) -> Result<SessionDigest, CommandError> {
    let (touched, omitted_touched) = dedupe_touched(session_touched, request_touched);
    let activity = session_entries
        .map(extract_activity_lines)
        .unwrap_or_default();
    let fallback = fallback_transcript.map(str::trim).filter(|s| !s.is_empty());

    if activity.is_empty() && fallback.is_none() && touched.is_empty() {
        return Err(CommandError::plain(
            "There is no session activity to summarize yet.",
        ));
    }

    let mut sections = Vec::new();
    if let Some(title) = title.map(str::trim).filter(|s| !s.is_empty()) {
        sections.push(format!("Thread title: {}", single_line_preview(title, 200)));
    }
    if !touched.is_empty() {
        let mut block = String::from("Touched files:\n");
        for path in &touched {
            block.push_str("- ");
            block.push_str(path);
            block.push('\n');
        }
        if omitted_touched > 0 {
            block.push_str(&format!("- ({omitted_touched} more file(s) omitted)\n"));
        }
        sections.push(block.trim_end().to_owned());
    }
    if !activity.is_empty() {
        sections.push(format!("Session activity:\n{}", activity.join("\n")));
    } else if let Some(transcript) = fallback {
        sections.push(format!("Session transcript:\n{transcript}"));
    }

    let raw = sections.join("\n\n");
    let redacted = dc_redact::redact_secrets(&raw);
    if redacted.len() <= MAX_DIGEST_BYTES {
        return Ok(SessionDigest {
            text: redacted,
            truncated: false,
        });
    }
    // Leave room for `budget_text`'s truncation banner so the final payload
    // never exceeds `MAX_DIGEST_BYTES`.
    let budgeted = budget_text(&redacted, MAX_DIGEST_BYTES.saturating_sub(96));
    Ok(SessionDigest {
        text: budgeted.text,
        truncated: true,
    })
}

fn read_capped(mut reader: impl Read, cap: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::with_capacity(cap.min(8192));
    let mut buf = [0u8; 4096];
    let mut truncated = false;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let remaining = cap.saturating_sub(out.len());
                if remaining > 0 {
                    let take = n.min(remaining);
                    out.extend_from_slice(&buf[..take]);
                    if n > take {
                        truncated = true;
                        break;
                    }
                } else {
                    truncated = true;
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    (out, truncated)
}

fn run_claude_summary(
    model: &str,
    digest: &SessionDigest,
) -> Result<MaviWorkSummary, CommandError> {
    let started = Instant::now();
    let args = claude_summary_args(model, &session_summary_system())?;
    let (program, path) = resolve("claude").map_err(CommandError::plain)?;
    let mut cmd = command_for(&program, path);
    cmd.args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let (mut child, registration) = crate::procguard::spawn(&mut cmd, "mavi summary")
        .map_err(|e| CommandError::plain(format!("Could not start Claude to summarize: {e}")))?;

    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let user_prompt = session_summary_user(&digest.text);
    let (status, stdout_bytes, stdout_truncated, stderr_bytes) = std::thread::scope(|scope| {
        let writer = scope.spawn(move || {
            if let Some(mut stdin) = stdin {
                let _ = stdin.write_all(user_prompt.as_bytes());
                let _ = stdin.flush();
                // Dropped here, closing stdin so `claude -p` sees EOF.
            }
        });
        let out_reader = scope.spawn(move || {
            stdout
                .map(|r| read_capped(r, CLAUDE_MAX_OUTPUT_BYTES))
                .unwrap_or_default()
        });
        let err_reader = scope.spawn(move || {
            stderr
                .map(|r| read_capped(r, CLAUDE_MAX_STDERR_BYTES))
                .unwrap_or_default()
        });

        let deadline = Instant::now() + CLAUDE_SUMMARY_TIMEOUT;
        let status = loop {
            match registration.poll(|| child.try_wait()) {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        registration.kill_tree(&mut child);
                        let _ = registration.reap(|| child.wait());
                        let _ = writer.join();
                        let _ = out_reader.join();
                        let _ = err_reader.join();
                        return Err(CommandError::plain(
                            "Claude took too long to summarize this session (timed out after 45s).",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(e) => {
                    registration.kill_tree(&mut child);
                    let _ = registration.reap(|| child.wait());
                    let _ = writer.join();
                    let _ = out_reader.join();
                    let _ = err_reader.join();
                    return Err(CommandError::plain(format!(
                        "Failed waiting for Claude summary process: {e}"
                    )));
                }
            }
        };

        let _ = writer.join();
        let (stdout_bytes, stdout_truncated) = out_reader.join().unwrap_or_default();
        let (stderr_bytes, _) = err_reader.join().unwrap_or_default();
        Ok((status, stdout_bytes, stdout_truncated, stderr_bytes))
    })?;

    if !status.success() {
        let stderr_text = dc_redact::redact_secrets(&String::from_utf8_lossy(&stderr_bytes));
        let detail = single_line_preview(stderr_text.trim(), 300);
        return Err(CommandError::plain(if detail.is_empty() {
            format!("Claude failed to summarize this session ({status}).")
        } else {
            format!("Claude failed to summarize this session: {detail}")
        }));
    }

    let raw_text = String::from_utf8_lossy(&stdout_bytes);
    let text = strip_think_tags(&raw_text).trim().to_string();
    if text.is_empty() {
        return Err(CommandError::plain(
            "Claude returned an empty session summary.",
        ));
    }

    let mut warnings = Vec::new();
    if digest.truncated {
        warnings.push(
            "Session transcript exceeded 24,000 bytes and was truncated before summarizing.".into(),
        );
    }
    if stdout_truncated {
        warnings.push("Claude summary output was truncated at 64 KiB.".into());
    }

    Ok(MaviWorkSummary {
        text,
        model: model.to_owned(),
        engine: "claude".into(),
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        truncated: digest.truncated || stdout_truncated,
        warnings,
    })
}

pub(crate) fn summarize(
    state: &MaviState,
    workbench: &crate::workbench::WorkbenchState,
    request: SummarizeRequest,
) -> Result<MaviWorkSummary, CommandError> {
    let engine = parse_engine(request.engine.as_deref(), request.model.as_deref())?;

    let session = request
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|id| state.get(id).ok());

    let (session_entries, session_touched, session_repo_key) = match &session {
        Some(sess) => {
            let (entries, _) = sess.replay(0);
            let summary = sess.summary();
            let repo_key = match &summary.target {
                session::MaviTarget::Local { root } => Some(root.clone()),
                session::MaviTarget::Ssh { host, dir } => Some(format!("{host}:{dir}")),
            };
            (Some(entries), summary.touched, repo_key)
        }
        None => (None, Vec::new(), None),
    };

    let digest = build_session_digest(
        request.title.as_deref(),
        session_entries.as_deref(),
        &session_touched,
        &request.touched,
        request.transcript.as_deref(),
    )?;

    match engine {
        SummaryEngine::Claude { model } => run_claude_summary(&model, &digest),
        SummaryEngine::Ai => {
            let repo_key = request
                .repo_path
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .or(session_repo_key)
                .unwrap_or_else(|| "mavi".to_owned());
            let (route, warning) =
                crate::commands::assist::route_for(request.base_url, request.model, workbench)?;
            let generation = crate::commands::assist::with_route_warning(
                crate::ai::summarize_session(&repo_key, &digest.text, route),
                warning,
            )?;
            Ok(MaviWorkSummary {
                text: generation.text,
                model: generation.model,
                engine: "ai".into(),
                elapsed_ms: generation.elapsed_ms,
                truncated: generation.truncated || generation.diff_truncated || digest.truncated,
                warnings: generation.warnings,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(seq: u64, value: Value) -> session::MaviEntry {
        session::MaviEntry {
            seq,
            line: value.to_string(),
        }
    }

    #[test]
    fn extracts_replay_lines_and_skips_bulky_tool_results() {
        let entries = vec![
            entry(
                1,
                json!({"type": "mavi_user", "text": "Fix the login bug", "turn": 1}),
            ),
            entry(
                2,
                json!({
                    "type": "assistant",
                    "message": {
                        "content": [
                            {"type": "text", "text": "Checking auth.rs now."},
                            {"type": "tool_use", "id": "t1", "name": "Edit", "input": {"file_path": "src/auth.rs", "old_string": "a", "new_string": "b"}},
                            {"type": "tool_use", "id": "t2", "name": "Bash", "input": {"command": "cargo test auth"}}
                        ]
                    }
                }),
            ),
            entry(
                3,
                json!({
                    "type": "user",
                    "message": {
                        "role": "user",
                        "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "HUGE_TOOL_RESULT_BLOB_THAT_MUST_BE_SKIPPED"}]
                    }
                }),
            ),
            entry(
                4,
                json!({
                    "type": "system",
                    "subtype": "post_turn_summary",
                    "status_category": "completed",
                    "status_detail": "Fixed token refresh and verified tests."
                }),
            ),
            entry(
                5,
                json!({"type": "mavi_exit", "exit": "exited with code 0", "ok": true}),
            ),
        ];

        let digest = build_session_digest(
            Some("Login fix"),
            Some(&entries),
            &["src/auth.rs".into(), "src/lib.rs".into()],
            &["src/lib.rs".into(), "src/auth_test.rs".into()],
            None,
        )
        .unwrap();

        assert!(!digest.truncated);
        assert!(digest.text.contains("Thread title: Login fix"));
        assert!(digest.text.contains("User: Fix the login bug"));
        assert!(digest.text.contains("Assistant: Checking auth.rs now."));
        assert!(digest.text.contains("Tool Edit: src/auth.rs"));
        assert!(digest.text.contains("Tool Bash: cargo test auth"));
        assert!(digest
            .text
            .contains("Turn summary [completed]: Fixed token refresh and verified tests."));
        assert!(digest
            .text
            .contains("Session exited (exited with code 0, ok=true)"));
        assert!(!digest
            .text
            .contains("HUGE_TOOL_RESULT_BLOB_THAT_MUST_BE_SKIPPED"));
        // Touched files are deduped in order.
        let lib_positions: Vec<_> = digest.text.match_indices("- src/lib.rs").collect();
        assert_eq!(lib_positions.len(), 1);
    }

    #[test]
    fn redacts_secrets_from_digest() {
        let token = format!("ghp_{}", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8");
        let transcript = format!("User: Please rotate {token}");
        let digest = build_session_digest(None, None, &[], &[], Some(&transcript)).unwrap();
        assert!(!digest.text.contains(&token));
    }

    #[test]
    fn truncates_oversized_digest_and_marks_truncated_flag() {
        let huge_line = "Edited src/foo.rs and ran cargo test.\n";
        let transcript = huge_line.repeat(1_200);
        assert!(transcript.len() > MAX_DIGEST_BYTES);
        let digest =
            build_session_digest(Some("Big session"), None, &[], &[], Some(&transcript)).unwrap();
        assert!(digest.truncated);
        assert!(digest.text.len() <= MAX_DIGEST_BYTES);
        assert!(digest.text.contains("[context truncated:"));
    }

    #[test]
    fn refuses_empty_session_even_with_title() {
        let err =
            build_session_digest(Some("Only a title"), None, &[], &[], Some("   \n")).unwrap_err();
        assert_eq!(
            err.message(),
            "There is no session activity to summarize yet."
        );
    }

    #[test]
    fn rejects_invalid_engine_and_invalid_model() {
        assert!(
            parse_engine(None, None)
                == Ok(SummaryEngine::Claude {
                    model: "haiku".into()
                })
        );
        assert!(parse_engine(Some("ai"), None) == Ok(SummaryEngine::Ai));
        assert!(parse_engine(Some("unknown"), None).is_err());
        assert!(parse_engine(Some("claude"), Some("--dangerously-skip-permissions")).is_err());
        assert!(parse_engine(Some("claude"), Some("haiku;rm")).is_err());
        assert!(parse_engine(Some("claude"), Some("claude-haiku-4-5-20251001")).is_ok());
    }

    #[test]
    fn builds_expected_claude_cli_args() {
        let args = claude_summary_args("haiku", "SYS PROMPT").unwrap();
        assert_eq!(
            args,
            vec![
                "-p",
                "--model",
                "haiku",
                "--no-session-persistence",
                "--output-format",
                "text",
                "--tools",
                "",
                "--system-prompt",
                "SYS PROMPT",
            ]
        );
        assert!(claude_summary_args("--bad", "SYS").is_err());
    }
}
