//! The NDJSON wire Mavi speaks with Claude Code's `--input-format stream-json
//! --output-format stream-json --permission-prompt-tool stdio` mode.
//!
//! Everything here is pure: what a line from the agent means to the host, and
//! what the host writes back. The renderer never builds a control message
//! itself. A permission answer names a request id and a decision, and this
//! module fills in the tool input and the suggested rules from the request
//! the agent actually sent. A page therefore cannot answer "allow" with input
//! the agent never asked to run.

use serde_json::{json, Value};
use std::collections::HashMap;

/// The longest line passed on to the page. A tool result that reads a large
/// file can be megabytes. Past this the line is replaced by a marker that
/// names its size, so the timeline shows that something was cut.
pub(crate) const MAX_LINE_BYTES: usize = 1024 * 1024;

/// The most permission requests one session may have open at once. The agent
/// asks one at a time, so this cap only matters for an agent that misbehaves.
pub(crate) const MAX_PENDING: usize = 64;

/// The longest deny message or user turn the host forwards.
pub(crate) const MAX_TEXT_BYTES: usize = 256 * 1024;

/// What the host has to act on in one line from the agent.
#[derive(Debug, PartialEq)]
pub(crate) enum Signal {
    /// A turn ended (`type: result`).
    TurnEnded,
    /// The agent asks whether a tool may run.
    PermissionRequest { request_id: String },
    /// The agent's `system/init`, carrying its conversation id.
    Init { session_id: String },
    /// Nothing for the host to do.
    None,
}

/// Classifies one complete line. A line that is not JSON is `None`. The page
/// still gets it and shows it as unparsed output.
pub(crate) fn classify(line: &str) -> (Signal, Option<Value>) {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return (Signal::None, None);
    };
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let signal = match kind {
        "result" => Signal::TurnEnded,
        "control_request"
            if value.pointer("/request/subtype").and_then(Value::as_str)
                == Some("can_use_tool") =>
        {
            match value.get("request_id").and_then(Value::as_str) {
                Some(id) if !id.is_empty() && id.len() <= 200 => Signal::PermissionRequest {
                    request_id: id.to_owned(),
                },
                _ => Signal::None,
            }
        }
        "system" if value.get("subtype").and_then(Value::as_str) == Some("init") => {
            match value.get("session_id").and_then(Value::as_str) {
                Some(id) if super::ssh::is_uuid(id) => Signal::Init {
                    session_id: id.to_ascii_lowercase(),
                },
                _ => Signal::None,
            }
        }
        _ => Signal::None,
    };
    (signal, Some(value))
}

/// A line no longer than [`MAX_LINE_BYTES`], or a marker in its place.
pub(crate) fn capped(line: String) -> String {
    if line.len() <= MAX_LINE_BYTES {
        return line;
    }
    // Keep the type so the page can tell a cut tool result from a cut
    // assistant message.
    let head = &line.as_bytes()[..line.len().min(4096)];
    let kind = std::str::from_utf8(head)
        .ok()
        .and_then(|text| {
            let at = text.find("\"type\":\"")? + 8;
            let rest = &text[at..];
            Some(rest[..rest.find('"')?].to_owned())
        })
        .unwrap_or_default();
    synthetic(
        "mavi_truncated",
        json!({"bytes": line.len(), "original_type": kind}),
    )
}

/// A line the host adds to the stream. Its `type` starts with `mavi_`, so the
/// page can never confuse it with something the agent said.
pub(crate) fn synthetic(kind: &str, fields: Value) -> String {
    let mut value = fields;
    if !value.is_object() {
        value = json!({});
    }
    value["type"] = Value::String(kind.to_owned());
    value.to_string()
}

/// A user turn.
pub(crate) fn user_message(text: &str) -> Result<String, String> {
    let text = text.trim_end();
    if text.trim().is_empty() {
        return Err("Write a message first.".into());
    }
    if text.len() > MAX_TEXT_BYTES {
        return Err("The message is longer than 256 KiB.".into());
    }
    Ok(json!({"type":"user","message":{"role":"user","content":text}}).to_string())
}

/// An interrupt of the running turn.
pub(crate) fn interrupt(request_id: &str) -> String {
    json!({"type":"control_request","request_id":request_id,"request":{"subtype":"interrupt"}})
        .to_string()
}

/// How the person answered a permission request.
///
/// There is no "allow for the rest of the session": the rules Claude Code
/// suggests with a request can be a `setMode` (for an Edit, `acceptEdits`),
/// which would raise the session's permission mode behind the mode chip and
/// around the policy table (`apply_permission_mode`). A wider mode is chosen
/// when the thread starts, where the person can see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    Allow,
    Deny,
}

impl Decision {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "allow" => Some(Self::Allow),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }
}

/// The permission requests a session has open, keyed by request id. The input
/// is what the agent sent, held so the answer echoes it.
#[derive(Default)]
pub(crate) struct Pending {
    /// Request id to the tool the agent asked about and the input it sent.
    open: HashMap<String, (String, Value)>,
}

impl Pending {
    /// Records a `can_use_tool` request. A session past [`MAX_PENDING`] open
    /// requests refuses new ones. The agent then waits, and the page still
    /// shows the request with no way to answer it, which is honest.
    pub(crate) fn record(&mut self, request_id: &str, line: &Value) -> bool {
        if self.open.len() >= MAX_PENDING && !self.open.contains_key(request_id) {
            return false;
        }
        let input = line
            .pointer("/request/input")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let tool = line
            .pointer("/request/tool_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        self.open.insert(request_id.to_owned(), (tool, input));
        true
    }

    pub(crate) fn len(&self) -> usize {
        self.open.len()
    }

    /// Every open request is void once the process is gone or the turn ended.
    pub(crate) fn clear(&mut self) {
        self.open.clear();
    }

    /// The `control_response` line for an answer. The request is then closed.
    /// An unknown id is an error, never a guess.
    #[cfg(test)]
    pub(crate) fn answer(
        &mut self,
        request_id: &str,
        decision: Decision,
        message: Option<&str>,
    ) -> Result<String, String> {
        let Some((tool, input)) = self.peek(request_id) else {
            return Err("That request is no longer waiting for an answer.".into());
        };
        // An answer the host refuses leaves the request open.
        let line = Self::respond(request_id, &tool, &input, decision, message)?;
        self.take(request_id);
        Ok(line)
    }

    /// The tool and input of an open request, without closing it.
    pub(crate) fn peek(&self, request_id: &str) -> Option<(String, Value)> {
        self.open.get(request_id).cloned()
    }

    /// Closes a request, returning the tool and input the agent sent with it.
    pub(crate) fn take(&mut self, request_id: &str) -> Option<(String, Value)> {
        self.open.remove(request_id)
    }

    /// Reopens a request whose answer could not be delivered.
    pub(crate) fn restore(&mut self, request_id: &str, tool: String, input: Value) {
        self.open.insert(request_id.to_owned(), (tool, input));
    }

    /// The line answering `request_id` for `tool`. A question is answered by
    /// [`question_response`]; every other tool echoes its own input. This is
    /// the one place that picks, so no caller can answer a question the
    /// generic way (an Allow with no answers, which Claude Code reports to the
    /// model as "The user did not answer the questions").
    pub(crate) fn respond(
        request_id: &str,
        tool: &str,
        input: &Value,
        decision: Decision,
        message: Option<&str>,
    ) -> Result<String, String> {
        if tool == ASK_TOOL {
            return question_response(request_id, input, decision, message);
        }
        Ok(Self::response(request_id, input, decision, message))
    }

    /// The `control_response` line answering `request_id`.
    pub(crate) fn response(
        request_id: &str,
        input: &Value,
        decision: Decision,
        message: Option<&str>,
    ) -> String {
        let response = match decision {
            Decision::Allow => json!({"behavior":"allow","updatedInput":input}),
            Decision::Deny => {
                let text = message
                    .map(str::trim)
                    .filter(|m| !m.is_empty())
                    .unwrap_or("The person declined this tool call.");
                let text: String = text.chars().take(4000).collect();
                json!({"behavior":"deny","message":text})
            }
        };
        json!({
            "type":"control_response",
            "response":{"subtype":"success","request_id":request_id,"response":response}
        })
        .to_string()
    }
}

/// The tool whose permission request is a question put to the person.
pub(crate) const ASK_TOOL: &str = "AskUserQuestion";

/// The longest answer to one question the host forwards.
pub(crate) const MAX_ANSWER_BYTES: usize = 4000;

/// What the agent is told when the person declines to answer. It must not read
/// as consent or as a choice of any option: an agent that took its own
/// recommended option after a bare refusal acted without the person.
pub(crate) const QUESTION_DECLINED: &str = "The user declined to answer these questions. This is not consent and not a choice of any option: do not pick one yourself or carry on as if they agreed. Ask again in plain text, or stop and wait for them.";

/// Guidance appended to the agent's system prompt (`--append-system-prompt`).
/// A question left unanswered or declined, and a terminal tab that is waiting
/// on the person, are the two ways an agent has acted as if it had consent.
// Passed by `claude_args` in mod.rs, which another change owns; until that
// line lands only the tests read it.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const AGENT_GUIDANCE: &str = "A question you ask with AskUserQuestion that the person has not answered, or has declined, is not consent: do not choose an option for them and do not continue as if they agreed. Ask again in plain text, or stop and wait. After you start an agent or a long-running process in a terminal tab with gitpulse_terminal_spawn, call gitpulse_terminal_wait on it. If it reports needs_input, tell the person which tab needs them and what it is asking; do not approve its prompt on their behalf, and do not report that tab as working or finished while it waits.";

/// The `control_response` line for a request that asks the person a question.
///
/// Allow carries the answers, keyed by the question's text, merged into the
/// input the agent sent (never an input the page supplied). Without answers
/// the request cannot be allowed: Claude Code would tell the model nobody
/// answered. Deny is a refusal to answer and says it is not consent.
pub(crate) fn question_response(
    request_id: &str,
    input: &Value,
    decision: Decision,
    message: Option<&str>,
) -> Result<String, String> {
    let response = match decision {
        Decision::Allow => {
            json!({"behavior":"allow","updatedInput":merge_answers(input, message)?})
        }
        Decision::Deny => {
            let note: String = message
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .unwrap_or_default()
                .chars()
                .take(1000)
                .collect();
            let text = if note.is_empty() {
                QUESTION_DECLINED.to_owned()
            } else {
                format!("{QUESTION_DECLINED} The person added: {note}")
            };
            json!({"behavior":"deny","message":text})
        }
    };
    Ok(json!({
        "type":"control_response",
        "response":{"subtype":"success","request_id":request_id,"response":response}
    })
    .to_string())
}

/// The agent's own input with the person's answers added.
///
/// `message` is `{"answers": {"<question text>": "<answer>"}}`. Every key must
/// be the text of one of the agent's questions and every question must be
/// answered, with a non-empty, bounded string: an option label, labels joined
/// with ", " for a multi-select question, or the person's own words. Anything
/// else is refused and the request stays open.
fn merge_answers(input: &Value, message: Option<&str>) -> Result<Value, String> {
    let refuse = "Answer every question, or decline.";
    let Some(message) = message else {
        return Err(refuse.into());
    };
    let parsed: Value = serde_json::from_str(message).map_err(|_| refuse.to_string())?;
    let Some(given) = parsed.get("answers").and_then(Value::as_object) else {
        return Err(refuse.into());
    };
    let questions: Vec<&str> = input
        .get("questions")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|q| q.get("question").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    if questions.is_empty() {
        return Err("This request has no questions to answer.".into());
    }
    if let Some(unknown) = given.keys().find(|k| !questions.contains(&k.as_str())) {
        let shown: String = unknown.chars().take(80).collect();
        return Err(format!("“{shown}” is not one of the agent's questions."));
    }
    let mut answers = serde_json::Map::new();
    for question in &questions {
        let answer = given
            .get(*question)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .ok_or_else(|| refuse.to_string())?;
        if answer.len() > MAX_ANSWER_BYTES || answer.chars().any(|c| c.is_control() && c != '\n') {
            return Err("An answer is too long or has control characters.".into());
        }
        answers.insert((*question).to_owned(), Value::String(answer.to_owned()));
    }
    let mut merged = input.clone();
    if let Some(object) = merged.as_object_mut() {
        object.insert("answers".into(), Value::Object(answers));
    }
    Ok(merged)
}

/// The longest text a single tool result keeps. A `Read` of a large file or a
/// noisy command fills the timeline with output no one scrolls; the cut says
/// how much was left out.
pub(crate) const MAX_RESULT_TEXT: usize = 32 * 1024;

/// The most hunks kept from one edit's `structuredPatch`.
const MAX_PATCH_HUNKS: usize = 64;

fn cut_text(text: &str, max: usize) -> Option<String> {
    if text.len() <= max {
        return None;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Some(format!(
        "{}\n… [{} more bytes not shown]",
        &text[..end],
        text.len() - end
    ))
}

/// Removes what the page never reads and what would make one line enormous,
/// in place. The agent's own record of a turn is in its transcript; this is
/// the timeline's copy.
///
/// - `system/init` repeats every turn with the whole tool, skill and plugin
///   catalogue (~35 KB). The page needs the model, mode, cwd, version and
///   conversation id.
/// - An edit's `tool_use_result` carries `originalFile` (the whole file before
///   the edit); the exact hunks are in `structuredPatch`, which is kept.
/// - A `Read` result carries the file's content twice (in `tool_result` and in
///   `tool_use_result.file.content`); the second copy goes, the first is cut.
pub(crate) fn slim(value: &mut Value) {
    match value.get("type").and_then(Value::as_str) {
        Some("system") if value.get("subtype").and_then(Value::as_str) == Some("init") => {
            let keep = [
                "subtype",
                "session_id",
                "model",
                "permissionMode",
                "cwd",
                "claude_code_version",
                "uuid",
            ];
            if let Some(object) = value.as_object_mut() {
                let tools = object.get("tools").and_then(Value::as_array).map(Vec::len);
                object.retain(|key, _| key == "type" || keep.contains(&key.as_str()));
                if let Some(count) = tools {
                    object.insert("tool_count".into(), json!(count));
                }
            }
        }
        Some("user") => {
            if let Some(result) = value
                .get_mut("tool_use_result")
                .and_then(Value::as_object_mut)
            {
                result.remove("originalFile");
                if let Some(file) = result.get_mut("file").and_then(Value::as_object_mut) {
                    file.remove("content");
                }
                if let Some(Value::String(diff)) = result.get_mut("gitDiff") {
                    if let Some(cut) = cut_text(diff, MAX_RESULT_TEXT) {
                        *diff = cut;
                    }
                }
                if let Some(Value::Array(hunks)) = result.get_mut("structuredPatch") {
                    hunks.truncate(MAX_PATCH_HUNKS);
                }
                for key in ["stdout", "stderr", "content"] {
                    if let Some(Value::String(text)) = result.get_mut(key) {
                        if let Some(cut) = cut_text(text, MAX_RESULT_TEXT) {
                            *text = cut;
                        }
                    }
                }
            }
            if let Some(Value::Array(blocks)) = value.pointer_mut("/message/content") {
                for block in blocks {
                    match block.get_mut("content") {
                        Some(Value::String(text)) => {
                            if let Some(cut) = cut_text(text, MAX_RESULT_TEXT) {
                                *text = cut;
                            }
                        }
                        Some(Value::Array(parts)) => {
                            for part in parts {
                                if let Some(Value::String(text)) = part.get_mut("text") {
                                    if let Some(cut) = cut_text(text, MAX_RESULT_TEXT) {
                                        *text = cut;
                                    }
                                }
                                // An image a tool returned is base64; the timeline
                                // names it rather than carrying it.
                                if part.get("type").and_then(Value::as_str) == Some("image") {
                                    *part = json!({"type":"text","text":"[image not shown]"});
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }
}

/// One line from the agent, made ready for the page.
pub(crate) struct Prepared {
    pub signal: Signal,
    /// The text sent to the page.
    pub text: String,
    /// Whether the replay ring keeps it. Streaming deltas are live-only: the
    /// complete `assistant` line that follows each block carries the same text,
    /// so a page that replays sees every finished block once.
    pub keep: bool,
    pub value: Option<Value>,
    /// For a permission request only: the line as the agent sent it, before
    /// slimming and redaction. Its input is what an Allow echoes back, so the
    /// tool runs exactly what the agent asked for, never a page copy with a
    /// `[REDACTED]` marker or a trimmed body in it.
    pub raw: Option<Value>,
}

/// The page copy of a permission request whose input is too large or could
/// not be redacted cleanly: everything but the input, which is described.
/// The request stays answerable; the host keeps the real input.
fn request_without_input(value: &Value, why: &str, bytes: usize) -> String {
    let mut copy = value.clone();
    if let Some(request) = copy.get_mut("request").and_then(Value::as_object_mut) {
        request.insert(
            "input".into(),
            json!({"mavi_input_hidden": why, "bytes": bytes}),
        );
    }
    dc_redact::redact_secrets(&copy.to_string())
}

/// Classifies, slims and redacts one line. Credential shapes are redacted with
/// the same detector the commit-path leak scan uses (`dc_redact`), so a token
/// an agent read from a file is not shown in the timeline or kept in memory
/// in the clear. Deltas are not redacted line by line (a secret split across
/// two deltas would pass anyway); the block they build is, when it lands.
pub(crate) fn prepare(line: String) -> Prepared {
    let (signal, value) = classify(&line);
    let Some(mut value) = value else {
        // Plain text (a banner, a hook's output) is redacted like the rest.
        return Prepared {
            signal,
            text: capped(dc_redact::redact_secrets(&line)),
            keep: true,
            value: None,
            raw: None,
        };
    };
    if value.get("type").and_then(Value::as_str) == Some("stream_event") {
        return Prepared {
            signal,
            text: capped(line),
            keep: false,
            value: Some(value),
            raw: None,
        };
    }
    let raw = matches!(signal, Signal::PermissionRequest { .. }).then(|| value.clone());
    slim(&mut value);
    let text = value.to_string();
    if raw.is_some() && text.len() > MAX_LINE_BYTES {
        let page = request_without_input(&value, "too_large", text.len());
        let value = serde_json::from_str(&page).ok();
        return Prepared {
            signal,
            text: page,
            keep: true,
            value,
            raw,
        };
    }
    let redacted = dc_redact::redact_secrets(&text);
    let (text, value) = if redacted == text {
        (text, value)
    } else {
        match serde_json::from_str::<Value>(&redacted) {
            Ok(clean) => (redacted, clean),
            // A redaction that broke the JSON is never passed on raw. A
            // permission request keeps its id, so it can still be answered.
            Err(_) if raw.is_some() => {
                let page = request_without_input(&value, "redacted", text.len());
                let value = serde_json::from_str(&page).ok();
                return Prepared {
                    signal,
                    text: page,
                    keep: true,
                    value,
                    raw,
                };
            }
            Err(_) => {
                let marker =
                    synthetic("mavi_redacted", json!({"original_type": value.get("type")}));
                let parsed = serde_json::from_str(&marker).ok();
                return Prepared {
                    signal,
                    text: marker,
                    keep: true,
                    value: parsed,
                    raw: None,
                };
            }
        }
    };
    Prepared {
        signal,
        text: capped(text),
        keep: true,
        value: Some(value),
        raw,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQ: &str = r#"{"type":"control_request","request_id":"r-1","request":{"subtype":"can_use_tool","tool_name":"Edit","input":{"file_path":"/x/a.txt","old_string":"a","new_string":"b"},"tool_use_id":"toolu_1","permission_suggestions":[{"type":"addRules","rules":[{"toolName":"Edit"}],"behavior":"allow","destination":"session"}]}}"#;

    #[test]
    fn classifies_the_lines_the_host_acts_on() {
        assert_eq!(
            classify(r#"{"type":"result","subtype":"success"}"#).0,
            Signal::TurnEnded
        );
        assert_eq!(
            classify(REQ).0,
            Signal::PermissionRequest {
                request_id: "r-1".into()
            }
        );
        assert_eq!(
            classify(r#"{"type":"system","subtype":"init","session_id":"0D235A3D-03CF-44A9-AD04-EFFB59321456"}"#).0,
            Signal::Init { session_id: "0d235a3d-03cf-44a9-ad04-effb59321456".into() }
        );
        assert_eq!(
            classify(r#"{"type":"system","subtype":"init","session_id":"../x"}"#).0,
            Signal::None
        );
        assert_eq!(classify("not json").0, Signal::None);
        assert!(classify("not json").1.is_none());
        // An interrupt request from the agent side is not a permission request.
        assert_eq!(
            classify(
                r#"{"type":"control_request","request_id":"x","request":{"subtype":"interrupt"}}"#
            )
            .0,
            Signal::None
        );
    }

    #[test]
    fn an_over_long_line_becomes_a_marker_that_keeps_its_type() {
        let big = format!(
            r#"{{"type":"user","pad":"{}"}}"#,
            "x".repeat(MAX_LINE_BYTES)
        );
        let out: Value = serde_json::from_str(&capped(big.clone())).unwrap();
        assert_eq!(out["type"], "mavi_truncated");
        assert_eq!(out["original_type"], "user");
        assert_eq!(out["bytes"], big.len());
        assert_eq!(capped("{}".into()), "{}");
    }

    #[test]
    fn answers_echo_the_agents_own_input_and_close_the_request() {
        let mut pending = Pending::default();
        let (_, value) = classify(REQ);
        assert!(pending.record("r-1", &value.unwrap()));
        let allow: Value =
            serde_json::from_str(&pending.answer("r-1", Decision::Allow, None).unwrap()).unwrap();
        assert_eq!(allow["type"], "control_response");
        assert_eq!(allow["response"]["request_id"], "r-1");
        assert_eq!(allow["response"]["response"]["behavior"], "allow");
        assert_eq!(
            allow["response"]["response"]["updatedInput"]["new_string"],
            "b"
        );
        assert!(allow["response"]["response"]
            .get("updatedPermissions")
            .is_none());
        assert!(
            pending.answer("r-1", Decision::Allow, None).is_err(),
            "answered twice"
        );
    }

    #[test]
    fn deny_carries_a_message_and_no_session_rules_are_ever_forwarded() {
        let mut pending = Pending::default();
        let value = classify(REQ).1.unwrap();
        assert_eq!(Decision::parse("allow_session"), None);
        pending.record("r-1", &value);
        let allow: Value =
            serde_json::from_str(&pending.answer("r-1", Decision::Allow, None).unwrap()).unwrap();
        assert!(allow["response"]["response"]
            .get("updatedPermissions")
            .is_none());
        pending.record("r-1", &value);
        let deny: Value = serde_json::from_str(
            &pending
                .answer("r-1", Decision::Deny, Some("  not that file "))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(deny["response"]["response"]["behavior"], "deny");
        assert_eq!(deny["response"]["response"]["message"], "not that file");
        pending.record("r-1", &value);
        let bare: Value =
            serde_json::from_str(&pending.answer("r-1", Decision::Deny, None).unwrap()).unwrap();
        assert!(bare["response"]["response"]["message"]
            .as_str()
            .unwrap()
            .contains("declined"));
    }

    #[test]
    fn only_a_question_needs_answers_and_the_guidance_names_the_two_rules() {
        let mut pending = Pending::default();
        let ask = json!({"type":"control_request","request_id":"q","request":{
            "subtype":"can_use_tool","tool_name":"AskUserQuestion",
            "input":{"questions":[{"question":"Scope?","options":[{"label":"A"},{"label":"B"}]}]}}});
        pending.record("q", &ask);
        assert!(pending.answer("q", Decision::Allow, None).is_err());
        assert_eq!(pending.len(), 1, "a refused answer leaves the request open");
        let ok: Value = serde_json::from_str(
            &pending
                .answer("q", Decision::Allow, Some(r#"{"answers":{"Scope?":"B"}}"#))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            ok["response"]["response"]["updatedInput"]["answers"]["Scope?"],
            "B"
        );
        // Another tool's Allow still needs no message.
        pending.record("r-1", &classify(REQ).1.unwrap());
        assert!(pending.answer("r-1", Decision::Allow, None).is_ok());
        assert!(AGENT_GUIDANCE.contains("not consent"));
        assert!(AGENT_GUIDANCE.contains("gitpulse_terminal_wait"));
        assert!(AGENT_GUIDANCE.contains("needs_input"));
    }

    #[test]
    fn pending_requests_are_bounded() {
        let mut pending = Pending::default();
        let value = classify(REQ).1.unwrap();
        for i in 0..MAX_PENDING {
            assert!(pending.record(&format!("r-{i}"), &value));
        }
        assert!(!pending.record("one-more", &value));
        assert!(
            pending.record("r-0", &value),
            "re-recording an open id is not growth"
        );
        pending.clear();
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn user_messages_are_trimmed_bounded_and_never_empty() {
        let line: Value = serde_json::from_str(&user_message("fix it\n\n").unwrap()).unwrap();
        assert_eq!(line["message"]["content"], "fix it");
        assert!(user_message("   \n").is_err());
        assert!(user_message(&"x".repeat(MAX_TEXT_BYTES + 1)).is_err());
        let int: Value = serde_json::from_str(&interrupt("i-1")).unwrap();
        assert_eq!(int["request"]["subtype"], "interrupt");
        let syn: Value = serde_json::from_str(&synthetic("mavi_exit", json!({"code": 1}))).unwrap();
        assert_eq!(syn["type"], "mavi_exit");
        assert_eq!(syn["code"], 1);
    }

    #[test]
    fn slim_keeps_what_the_page_reads_and_drops_the_bulk() {
        let mut init = json!({"type":"system","subtype":"init","session_id":"s","model":"m","tools":["a","b"],"skills":["x"],"cwd":"/r"});
        slim(&mut init);
        assert_eq!(
            init,
            json!({"type":"system","subtype":"init","session_id":"s","model":"m","cwd":"/r","tool_count":2})
        );

        let big = "x".repeat(MAX_RESULT_TEXT * 2);
        let mut edit = json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":big}]},
            "tool_use_result":{"filePath":"/r/a","originalFile":"whole file","structuredPatch":[{"oldStart":1,"lines":["-a","+b"]}],"file":{"content":"dup"}}});
        slim(&mut edit);
        let result = &edit["tool_use_result"];
        assert!(result.get("originalFile").is_none());
        assert!(result["file"].get("content").is_none());
        assert_eq!(result["structuredPatch"][0]["lines"][1], "+b");
        let text = edit["message"]["content"][0]["content"].as_str().unwrap();
        assert!(text.len() < MAX_RESULT_TEXT + 100 && text.ends_with("more bytes not shown]"));
    }

    #[test]
    fn prepare_keeps_finished_lines_and_streams_deltas_live_only() {
        let delta =
            prepare(r#"{"type":"stream_event","event":{"type":"content_block_delta"}}"#.into());
        assert!(!delta.keep);
        let done = prepare(r#"{"type":"result","subtype":"success"}"#.into());
        assert!(done.keep);
        assert_eq!(done.signal, Signal::TurnEnded);
        let junk = prepare("warning: not json".into());
        assert!(junk.keep && junk.value.is_none());
    }

    #[test]
    fn prepare_redacts_credential_shapes() {
        let token = format!("ghp_{}", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8");
        let line = json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":format!("TOKEN={token}")}]}}).to_string();
        let out = prepare(line);
        assert!(!out.text.contains(&token), "{}", out.text);
        assert!(out.value.is_some());
    }

    #[test]
    fn an_oversized_request_stays_answerable_and_allow_echoes_the_real_input() {
        let body = "x".repeat(MAX_LINE_BYTES + 10);
        let line = json!({"type":"control_request","request_id":"r-big","request":{"subtype":"can_use_tool","tool_name":"Write","input":{"file_path":"a","content":body}}}).to_string();
        let out = prepare(line);
        let page: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(page["request_id"], "r-big");
        assert_eq!(page["request"]["input"]["mavi_input_hidden"], "too_large");
        let raw = out.raw.expect("raw request kept for the answer");
        let mut pending = Pending::default();
        assert!(pending.record("r-big", &raw));
        let answer: Value =
            serde_json::from_str(&pending.answer("r-big", Decision::Allow, None).unwrap()).unwrap();
        assert_eq!(
            answer["response"]["response"]["updatedInput"]["content"]
                .as_str()
                .map(str::len),
            Some(MAX_LINE_BYTES + 10)
        );
    }
}
