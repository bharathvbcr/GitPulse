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

use std::borrow::Cow;
use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// How a call ended, as far as the transcript says.
///
/// A call is written when the model asks for it and its result is written
/// later, on another line. A call whose result this reader never saw (the
/// agent was killed, the result is still to come, the line was unreadable) is
/// [`CallResult::Unobserved`], which is not a success: the ledger's `outcome`
/// column has no "unknown" (it is `ok`, `failed` or `blocked`), so such a row
/// is `ok` there, as every transcript row has always been, and the difference
/// is that it carries no `detail_json.result`. A reader that wants to know
/// whether a call passed asks that key and not the column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CallResult {
    /// No result line was read for this call.
    #[default]
    Unobserved,
    /// The result came back without the error flag.
    Succeeded,
    /// The result came back flagged as an error (`is_error`).
    Errored,
}

impl CallResult {
    /// The word `detail_json.result` carries; an unobserved call carries none.
    pub fn word(self) -> Option<&'static str> {
        match self {
            CallResult::Unobserved => None,
            CallResult::Succeeded => Some("ok"),
            CallResult::Errored => Some("error"),
        }
    }
}

/// The token counts of one model response, and nothing else the response said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

/// Which transcript line a call came from, for the row's identity.
///
/// A call is written when its result is read, which is a later line than the
/// one that holds it, so the identity cannot be taken from the line being read.
/// It is the line that holds the call, as it always was: a row recorded before
/// results were paired keeps the identity it has now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineOrigin {
    pub offset: u64,
    pub ordinal: usize,
    pub digest: u64,
}

/// One mutating tool call, attributed to a repository.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
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
    /// The id the agent gave the call, which its result names.
    #[serde(skip)]
    pub tool_use_id: Option<String>,
    /// What the call's result said; see [`CallResult`].
    #[serde(skip)]
    pub result: CallResult,
    /// Milliseconds from the call's record to its result's, when both were
    /// timed and the result came after. Never a guess: absent is "not known".
    #[serde(skip)]
    pub duration_ms: Option<i64>,
    /// The subagent whose transcript this line is in (`agent-<id>.jsonl`), or
    /// `None` for the session's own transcript. The session id of a subagent's
    /// line is its parent's, so this is what tells the two apart.
    #[serde(skip)]
    pub subagent_id: Option<String>,
    /// Where the call was read when that is not the line being read.
    #[serde(skip)]
    pub origin: Option<LineOrigin>,
    /// The model response a [`USAGE_TOOL`] row counts.
    #[serde(skip)]
    pub message_id: Option<String>,
    /// What a [`USAGE_TOOL`] row counted.
    #[serde(skip)]
    pub usage: Option<TokenUsage>,
}

/// Every ledger action a transcript call can become, and only those.
///
/// `session.spawn` shares the prefix but is GitPulse opening a terminal; a
/// reader asking "what came from transcripts" must match this list, not the
/// prefix.
pub const ACTIONS: [&str; 3] = [EDIT, COMMAND, USAGE];
const EDIT: &str = "session.edit";
const COMMAND: &str = "session.command";
/// One model response's token counts: numbers only, never what was said.
pub const USAGE: &str = "session.usage";

/// The `tool` of a record that is a model response's token counts and not a
/// call. It shares [`ToolCall`] so it is attributed, owned and written by the
/// same machinery as the calls beside it.
pub const USAGE_TOOL: &str = "Usage";

/// A usage record's ordinal in its line's identity, which no call's can reach.
const USAGE_ORDINAL: usize = 1 << 20;

impl ToolCall {
    /// The ledger action this call becomes.
    pub fn action(&self) -> &'static str {
        match self.tool.as_str() {
            "Bash" => COMMAND,
            USAGE_TOOL => USAGE,
            _ => EDIT,
        }
    }

    /// What the call acted on, for the ledger's `object`.
    pub fn object(&self) -> String {
        self.file_path
            .clone()
            .or_else(|| self.command.clone())
            .or_else(|| self.message_id.clone())
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
    match parse_record(line) {
        LineOutcome::Calls(calls) => calls,
        LineOutcome::NoCalls | LineOutcome::Unreadable => Vec::new(),
    }
}

/// What one transcript line turned out to hold.
#[derive(Debug, Clone, PartialEq)]
pub enum LineOutcome {
    /// Readable, and holds no mutating tool call: not an assistant record, or
    /// an assistant record of prose and reads.
    NoCalls,
    /// Tagged as an assistant record but not JSON this build can read — the one
    /// outcome that is a gap worth counting.
    Unreadable,
    /// The mutating tool calls the record holds. Never empty.
    Calls(Vec<ToolCall>),
}

/// The tag the assistant records this parser reads carry, as Claude Code
/// writes it. Used only to decide whether an unreadable line is a gap worth
/// counting; a spaced spelling of it is not.
const ASSISTANT_TAG: &str = "\"type\":\"assistant\"";

#[cfg(test)]
thread_local! {
    /// JSON documents this thread has deserialized through [`parse_record`].
    pub(super) static PARSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// [`parse_line`], saying *why* a line yielded nothing, so no caller has to
/// deserialize the line a second time to find out.
///
/// Most lines of a transcript are tool results and progress records that can
/// never hold a call, and most assistant lines are prose or reads. A line
/// that names no `"assistant"` and no `"tool_use"` cannot hold a call whether
/// or not it is well formed, so it is not built into a document: at most it is
/// validated, without allocating, when it claims to be an assistant record.
pub fn parse_record(line: &str) -> LineOutcome {
    let claims_assistant = line.contains(ASSISTANT_TAG);
    if !(line.contains("\"assistant\"") && line.contains("\"tool_use\"")) {
        #[cfg(test)]
        PARSES.with(|parses| parses.set(parses.get() + usize::from(claims_assistant)));
        return if claims_assistant && serde_json::from_str::<serde::de::IgnoredAny>(line).is_err() {
            LineOutcome::Unreadable
        } else {
            LineOutcome::NoCalls
        };
    }
    #[cfg(test)]
    PARSES.with(|parses| parses.set(parses.get() + 1));
    let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
        return if claims_assistant {
            LineOutcome::Unreadable
        } else {
            LineOutcome::NoCalls
        };
    };
    let calls = calls_in(&record);
    if calls.is_empty() {
        LineOutcome::NoCalls
    } else {
        LineOutcome::Calls(calls)
    }
}

/// The fields every record of a session carries, as a call with nothing else
/// filled in; `None` for a record that cannot be placed in the history.
///
/// Without a session or a time the row could not be placed at all, and without
/// a cwd a Bash call cannot be attributed. Recording it with blanks would put
/// an unattributable row in the ledger, so it is skipped and counted instead.
fn base_call(record: &serde_json::Value) -> Option<ToolCall> {
    let session_id = record["sessionId"].as_str().unwrap_or_default().to_string();
    let ts_utc = record["timestamp"].as_str().unwrap_or_default().to_string();
    if session_id.is_empty() || ts_utc.is_empty() {
        return None;
    }
    Some(ToolCall {
        session_id,
        ts_utc,
        version: record["version"].as_str().unwrap_or_default().to_string(),
        cwd: record["cwd"].as_str().unwrap_or_default().to_string(),
        git_branch: record["gitBranch"].as_str().map(str::to_string),
        ..ToolCall::default()
    })
}

fn calls_in(record: &serde_json::Value) -> Vec<ToolCall> {
    if record.get("type").and_then(|t| t.as_str()) != Some("assistant") {
        return Vec::new();
    }
    let Some(base) = base_call(record) else {
        return Vec::new();
    };

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
            tool: tool.to_string(),
            tool_use_id: block
                .get("id")
                .and_then(|i| i.as_str())
                .filter(|id| !id.is_empty())
                .map(str::to_string),
            file_path: input
                .get("file_path")
                .and_then(|p| p.as_str())
                .map(str::to_string),
            command: input
                .get("command")
                .and_then(|c| c.as_str())
                .map(str::to_string),
            ..base.clone()
        });
    }
    calls
}

// ---------------------------------------------------------------------------
// Reading a whole transcript: results paired with their calls, and token counts.
// ---------------------------------------------------------------------------

/// The tag a tool-result block carries as Claude Code writes it. A result is a
/// block of a `user` record, so it is on the lines [`parse_record`] never reads.
const RESULT_TAG: &str = "\"type\":\"tool_result\"";

/// The most calls waiting for their result at once. A call pushed out by a
/// newer one is written unpaired, which the row says.
const MAX_PENDING: usize = 256;

/// How far (by the transcript's own clock) a newer record may be from a call
/// that has no result before the call is written without one. Above the longest
/// time a command is allowed to run, so a call still running is not given up
/// on; a call the agent never got a result for is not held for ever.
pub const PENDING_MAX_AGE_MS: u64 = 15 * 60 * 1000;

/// Response ids remembered as already counted, per transcript read.
const MAX_COUNTED: usize = 64;

/// A tool-result record, read for the ids and error flags of its blocks and
/// nothing else: `content` is the tool's output and is skipped unread.
#[derive(Deserialize)]
struct ResultRecord<'a> {
    #[serde(borrow, rename = "type")]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    timestamp: Option<Cow<'a, str>>,
    message: Option<ResultMessage<'a>>,
}

#[derive(Deserialize)]
struct ResultMessage<'a> {
    #[serde(borrow)]
    content: Option<ResultBlocks<'a>>,
}

/// A message's `content` when it is a list of blocks; empty for anything else
/// (a prompt typed as plain text), which is skipped unread. A custom reader and
/// not an untagged enum, because that would buffer the whole of every tool
/// output before looking at it.
struct ResultBlocks<'a>(Vec<ResultBlock<'a>>);

impl<'de: 'a, 'a> Deserialize<'de> for ResultBlocks<'a> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Blocks<'a>(std::marker::PhantomData<&'a ()>);
        impl<'de: 'a, 'a> serde::de::Visitor<'de> for Blocks<'a> {
            type Value = ResultBlocks<'a>;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a message's content")
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut blocks = Vec::new();
                while let Some(block) = seq.next_element::<ResultBlock<'a>>()? {
                    blocks.push(block);
                }
                Ok(ResultBlocks(blocks))
            }

            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(ResultBlocks(Vec::new()))
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                while map
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(ResultBlocks(Vec::new()))
            }
        }
        deserializer.deserialize_any(Blocks(std::marker::PhantomData))
    }
}

#[derive(Deserialize)]
struct ResultBlock<'a> {
    #[serde(borrow, rename = "type")]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    tool_use_id: Option<Cow<'a, str>>,
    is_error: Option<bool>,
}

/// An assistant record read only for its response's identity and token counts.
#[derive(Deserialize)]
struct UsageRecord<'a> {
    #[serde(borrow, rename = "type")]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow, rename = "sessionId")]
    session_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    timestamp: Option<Cow<'a, str>>,
    #[serde(borrow)]
    cwd: Option<Cow<'a, str>>,
    #[serde(borrow)]
    version: Option<Cow<'a, str>>,
    #[serde(borrow, rename = "gitBranch")]
    git_branch: Option<Cow<'a, str>>,
    message: Option<UsageMessage<'a>>,
}

#[derive(Deserialize)]
struct UsageMessage<'a> {
    #[serde(borrow)]
    id: Option<Cow<'a, str>>,
    usage: Option<UsageWire>,
}

#[derive(Deserialize)]
struct UsageWire {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
}

impl UsageWire {
    /// `None` when the record counted nothing at all, which is not a response
    /// that used zero tokens.
    fn counts(&self) -> Option<TokenUsage> {
        if self.input_tokens.is_none()
            && self.output_tokens.is_none()
            && self.cache_read_input_tokens.is_none()
            && self.cache_creation_input_tokens.is_none()
        {
            return None;
        }
        Some(TokenUsage {
            input: self.input_tokens.unwrap_or(0),
            output: self.output_tokens.unwrap_or(0),
            cache_read: self.cache_read_input_tokens.unwrap_or(0),
            cache_creation: self.cache_creation_input_tokens.unwrap_or(0),
        })
    }
}

/// A response whose records are still being read: Claude Code writes one record
/// per content block of a response, each repeating the response's usage.
struct OpenUsage {
    call: ToolCall,
}

/// Reads one transcript file's lines in order, pairing each call with its
/// result and counting each response's tokens once.
///
/// The row for a call is written when its result is read, so the reader holds a
/// call back until then. What it holds back must not be lost to a cursor that
/// moved past it, so [`Reader::held_from`] says the earliest line still owed, and
/// the pass leaves its cursor there. The lines between are read again next
/// time; every row's identity is its own line's, so a row already written is
/// refused rather than written twice.
///
/// Only block types, ids, error flags, numbers and the fields of a record's
/// head are read. A result's `content`, the prose of a response and its
/// reasoning are skipped unread.
pub struct Reader {
    subagent_id: Option<String>,
    line: Option<(u64, u64)>,
    pending: VecDeque<ToolCall>,
    open_usage: Option<OpenUsage>,
    counted: VecDeque<String>,
    newest_ms: Option<u64>,
}

impl Reader {
    /// A reader for the transcript at `path`: `agent-<id>.jsonl` is a subagent's.
    pub fn for_path(path: &std::path::Path) -> Self {
        Reader::new(subagent_id_of(path))
    }

    pub fn new(subagent_id: Option<String>) -> Self {
        Reader {
            subagent_id,
            line: None,
            pending: VecDeque::new(),
            open_usage: None,
            counted: VecDeque::new(),
            newest_ms: None,
        }
    }

    /// The byte offset and content digest of the line [`Reader::read`] is about
    /// to be given.
    pub fn locate(&mut self, offset: u64, digest: u64) {
        self.line = Some((offset, digest));
    }

    /// The offset of the earliest line a call or a response still being read
    /// came from: the pass's cursor must not pass it.
    pub fn held_from(&self) -> Option<u64> {
        let calls = self
            .pending
            .iter()
            .filter_map(|c| c.origin.map(|o| o.offset));
        let usage = self
            .open_usage
            .iter()
            .filter_map(|u| u.call.origin.map(|o| o.offset));
        calls.chain(usage).min()
    }

    /// The end of the file was reached. With `give_up`, everything held is
    /// written as it stands (calls without a result, the last response) because
    /// nothing more is coming; without it, nothing is, and it stays held.
    pub fn finish(&mut self, give_up: bool) -> Vec<ToolCall> {
        if !give_up {
            return Vec::new();
        }
        let mut out: Vec<ToolCall> = self.pending.drain(..).collect();
        out.extend(self.close_usage());
        out
    }

    /// Reads one line.
    pub fn read(&mut self, line: &str) -> LineOutcome {
        if line.contains(RESULT_TAG) {
            return self.read_result(line);
        }
        let holds_call = line.contains("\"assistant\"") && line.contains("\"tool_use\"");
        if !holds_call && !line.contains(ASSISTANT_TAG) {
            return LineOutcome::NoCalls;
        }
        #[cfg(test)]
        PARSES.with(|parses| parses.set(parses.get() + 1));
        let mut out = Vec::new();
        if holds_call {
            let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
                return LineOutcome::Unreadable;
            };
            if record.get("type").and_then(|t| t.as_str()) != Some("assistant") {
                return LineOutcome::NoCalls;
            }
            let Some(base) = base_call(&record) else {
                return LineOutcome::NoCalls;
            };
            self.note_time(&base.ts_utc);
            for (ordinal, mut call) in calls_in(&record).into_iter().enumerate() {
                call.subagent_id.clone_from(&self.subagent_id);
                call.origin = self.origin(ordinal);
                if call.tool_use_id.is_some() && call.origin.is_some() {
                    self.pending.push_back(call);
                } else {
                    out.push(call);
                }
            }
            let message = &record["message"];
            let counts = UsageWire {
                input_tokens: message["usage"]["input_tokens"].as_u64(),
                output_tokens: message["usage"]["output_tokens"].as_u64(),
                cache_read_input_tokens: message["usage"]["cache_read_input_tokens"].as_u64(),
                cache_creation_input_tokens: message["usage"]["cache_creation_input_tokens"]
                    .as_u64(),
            }
            .counts();
            out.extend(self.note_usage(base, message["id"].as_str(), counts));
        } else {
            let Ok(record) = serde_json::from_str::<UsageRecord<'_>>(line) else {
                return LineOutcome::Unreadable;
            };
            if let (Some("assistant"), Some(session), Some(ts)) = (
                record.kind.as_deref(),
                record.session_id.as_deref().filter(|s| !s.is_empty()),
                record.timestamp.as_deref().filter(|t| !t.is_empty()),
            ) {
                let base = ToolCall {
                    session_id: session.to_string(),
                    ts_utc: ts.to_string(),
                    version: record.version.as_deref().unwrap_or_default().to_string(),
                    cwd: record.cwd.as_deref().unwrap_or_default().to_string(),
                    git_branch: record.git_branch.as_deref().map(str::to_string),
                    ..ToolCall::default()
                };
                self.note_time(ts);
                let message = record.message.as_ref();
                out.extend(
                    self.note_usage(
                        base,
                        message.and_then(|m| m.id.as_deref()),
                        message
                            .and_then(|m| m.usage.as_ref())
                            .and_then(UsageWire::counts),
                    ),
                );
            }
        }
        out.extend(self.expire());
        Self::outcome(out)
    }

    fn read_result(&mut self, line: &str) -> LineOutcome {
        #[cfg(test)]
        PARSES.with(|parses| parses.set(parses.get() + 1));
        let Ok(record) = serde_json::from_str::<ResultRecord<'_>>(line) else {
            return LineOutcome::NoCalls;
        };
        if record.kind.as_deref() != Some("user") {
            return LineOutcome::NoCalls;
        }
        let Some(ResultBlocks(blocks)) = record.message.and_then(|m| m.content) else {
            return LineOutcome::NoCalls;
        };
        let at_ms = record
            .timestamp
            .as_deref()
            .and_then(crate::ledger::ids::parse_event_time);
        if let Some(at) = at_ms {
            self.newest_ms = Some(self.newest_ms.map_or(at, |newest| newest.max(at)));
        }
        let mut out = Vec::new();
        for block in blocks {
            if block.kind.as_deref() != Some("tool_result") {
                continue;
            }
            let Some(id) = block.tool_use_id.as_deref() else {
                continue;
            };
            let Some(slot) = self
                .pending
                .iter()
                .position(|call| call.tool_use_id.as_deref() == Some(id))
            else {
                continue;
            };
            let Some(mut call) = self.pending.remove(slot) else {
                continue;
            };
            call.result = if block.is_error == Some(true) {
                CallResult::Errored
            } else {
                CallResult::Succeeded
            };
            call.duration_ms = at_ms
                .zip(crate::ledger::ids::parse_event_time(&call.ts_utc))
                .and_then(|(done, began)| {
                    done.checked_sub(began)
                        .and_then(|span| i64::try_from(span).ok())
                });
            out.push(call);
        }
        out.extend(self.expire());
        Self::outcome(out)
    }

    fn outcome(calls: Vec<ToolCall>) -> LineOutcome {
        if calls.is_empty() {
            LineOutcome::NoCalls
        } else {
            LineOutcome::Calls(calls)
        }
    }

    fn origin(&self, ordinal: usize) -> Option<LineOrigin> {
        self.line.map(|(offset, digest)| LineOrigin {
            offset,
            ordinal,
            digest,
        })
    }

    fn note_time(&mut self, ts: &str) {
        if let Some(at) = crate::ledger::ids::parse_event_time(ts) {
            self.newest_ms = Some(self.newest_ms.map_or(at, |newest| newest.max(at)));
        }
    }

    /// The calls whose record is too far behind the newest one to be waiting
    /// for a result any longer, and the oldest past the bound on how many wait.
    fn expire(&mut self) -> Vec<ToolCall> {
        let mut out = Vec::new();
        while self.pending.len() > MAX_PENDING {
            out.extend(self.pending.pop_front());
        }
        if let Some(newest) = self.newest_ms {
            let aged = |call: &ToolCall| {
                crate::ledger::ids::parse_event_time(&call.ts_utc)
                    .is_some_and(|at| newest.saturating_sub(at) > PENDING_MAX_AGE_MS)
            };
            // Arrival order is time order inside one transcript; a call that is
            // out of order is held until its neighbours go, which only delays it.
            while self.pending.front().is_some_and(aged) {
                out.extend(self.pending.pop_front());
            }
        }
        out
    }

    /// Takes in one record's counts. A response is written over several
    /// records that repeat its usage; it is written once, when the next
    /// response starts or the file ends, from the last of them.
    fn note_usage(
        &mut self,
        base: ToolCall,
        message_id: Option<&str>,
        counts: Option<TokenUsage>,
    ) -> Vec<ToolCall> {
        let (Some(id), Some(counts)) = (message_id.filter(|id| !id.is_empty()), counts) else {
            return Vec::new();
        };
        if let Some(open) = &mut self.open_usage {
            if open.call.message_id.as_deref() == Some(id) {
                open.call.usage = Some(counts);
                return Vec::new();
            }
        }
        let closed = self.close_usage();
        if self.counted.iter().any(|seen| seen == id) {
            return closed.into_iter().collect();
        }
        let mut call = ToolCall {
            tool: USAGE_TOOL.to_string(),
            message_id: Some(id.to_string()),
            usage: Some(counts),
            subagent_id: self.subagent_id.clone(),
            ..base
        };
        call.origin = self.origin(USAGE_ORDINAL);
        if call.origin.is_none() {
            return closed.into_iter().chain(Some(call)).collect();
        }
        self.open_usage = Some(OpenUsage { call });
        closed.into_iter().collect()
    }

    fn close_usage(&mut self) -> Option<ToolCall> {
        let open = self.open_usage.take()?;
        if let Some(id) = &open.call.message_id {
            if self.counted.len() >= MAX_COUNTED {
                self.counted.pop_front();
            }
            self.counted.push_back(id.clone());
        }
        Some(open.call)
    }
}

/// The subagent a transcript belongs to: the `<id>` of `agent-<id>.jsonl`.
pub fn subagent_id_of(path: &std::path::Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let id = stem.strip_prefix("agent-")?;
    (!id.is_empty()).then(|| id.to_string())
}

/// What a call is attributed by: a file tool's own absolute path, a command's
/// record `cwd`.
pub fn candidate_path(call: &ToolCall) -> &str {
    call.file_path.as_deref().unwrap_or(&call.cwd)
}

/// The root of `roots` that owns `call`: the deepest one its path lies under,
/// so a worktree nested in the repository takes its own calls and the
/// repository keeps only what is not under any worktree. Roots are ranked by
/// their length without trailing separators, so `/repo/` does not outrank
/// `/repo`; equal depth keeps the first listed.
///
/// Containment is a prefix match with a separator check, so `/repo-other` is
/// never under `/repo`. No path is canonicalized, so `/tmp/x` and
/// `/private/tmp/x` are different roots, and `\` is not a separator; both are
/// the byte-wise comparison of [`under`], the one place to change for either.
pub fn attribution_root<'a>(call: &ToolCall, roots: &[&'a str]) -> Option<&'a str> {
    deepest_root(candidate_path(call), roots)
}

pub fn deepest_root<'a>(path: &str, roots: &[&'a str]) -> Option<&'a str> {
    let depth = |root: &str| root.trim_end_matches('/').len();
    roots
        .iter()
        .copied()
        .filter(|root| under(path, root))
        .reduce(|best, root| {
            if depth(root) > depth(best) {
                root
            } else {
                best
            }
        })
}

/// A call longer than this is never read as a removed worktree's.
const MAX_CWD_BYTES: usize = 4096;
/// Where isolation worktrees live below a checkout.
const WORKTREES_DIR: &str = "/.claude/worktrees/";

/// A worktree that has been removed, found from a call's `cwd`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Departed<'c, 'r> {
    /// The removed worktree: `<checkout>/.claude/worktrees/<name>`, exactly,
    /// borrowed from the call's cwd.
    pub root: &'c str,
    /// The deepest live root containing it, which hosts the row.
    pub host: &'r str,
}

/// The removed worktree `call` ran in, by its path alone.
///
/// GitPulse's isolation worktrees live at `<checkout>/.claude/worktrees/<name>`
/// and nowhere else, so a cwd under a live root that passes through that shape
/// at a name which is not itself a live root ran in a worktree that no longer
/// exists. The deepest such name wins, so `…/a/.claude/worktrees/b` is `b`
/// whether or not `a` is live. Nothing is read from disk: whether a directory
/// exists says nothing about whether it was a worktree (a branch switch deletes
/// plain directories, and a path can fail to stat for reasons other than being
/// gone), so a deleted plain subdirectory stays with the live root around it.
///
/// The call's own path must also lie under the removed worktree. A cwd with an
/// empty, `.` or `..` segment, a NUL, or more than 4096 bytes is never read as
/// departed.
pub fn departed_root<'c, 'r>(call: &'c ToolCall, roots: &[&'r str]) -> Option<Departed<'c, 'r>> {
    let cwd = call.cwd.trim_end_matches('/');
    // Almost every call: nothing below its root has the shape.
    if cwd.len() > MAX_CWD_BYTES || !cwd.contains(WORKTREES_DIR) {
        return None;
    }
    let host = deepest_root(cwd, roots)?;
    let host_len = host.trim_end_matches('/').len();
    let below = &cwd[host_len..];
    let (mut before_last, mut last) = ("", "");
    let mut end = host_len;
    let mut deepest = None;
    for segment in below.split('/').skip(1) {
        if matches!(segment, "" | "." | "..") || segment.contains('\0') {
            return None;
        }
        end += 1 + segment.len();
        if before_last == ".claude" && last == "worktrees" {
            deepest = Some(end);
        }
        (before_last, last) = (last, segment);
    }
    let root = &cwd[..deepest?];
    // A live checkout is the live root's, whatever it is called.
    let live = roots.iter().any(|live| live.trim_end_matches('/') == root);
    (!live && under(candidate_path(call), root)).then_some(Departed { root, host })
}

/// Whether `path` has an empty, `.` or `..` component (a leading or trailing
/// separator does not count), which [`lexical`] resolves.
fn has_dot_components(path: &str) -> bool {
    let body = path.strip_prefix('/').unwrap_or(path);
    let body = body.strip_suffix('/').unwrap_or(body);
    !body.is_empty() && body.split('/').any(|c| matches!(c, "" | "." | ".."))
}

/// `path` with its `.`, `..` and empty components resolved by text alone, as the
/// shell would for a path with no symlinks in it; nothing is read from disk (a
/// symlink is the limitation the module documents). A `..` at the root of an
/// absolute path stays at the root; one before the start of a relative path is
/// kept, so the result is never mistaken for a path under an absolute root.
fn lexical(path: &str) -> std::borrow::Cow<'_, str> {
    if !has_dot_components(path) {
        return std::borrow::Cow::Borrowed(path);
    }
    let absolute = path.starts_with('/');
    let mut kept: Vec<&str> = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => match kept.last() {
                Some(last) if *last != ".." => {
                    kept.pop();
                }
                _ if absolute => {}
                _ => kept.push(".."),
            },
            other => kept.push(other),
        }
    }
    let joined = kept.join("/");
    std::borrow::Cow::Owned(if absolute {
        format!("/{joined}")
    } else {
        joined
    })
}

/// Whether `path` lies under `root`, by whole components, after both are
/// resolved lexically: `/repo-other` and `/repo/../repo-other` are not under
/// `/repo`, and `/repo/../other/x` is not either.
fn under(path: &str, root: &str) -> bool {
    if root.is_empty() || path.is_empty() {
        return false;
    }
    let (path, root) = (lexical(path), lexical(root));
    let (path, root) = (path.as_ref(), root.as_ref());
    // A root is an absolute path. A relative one (or one that resolves to
    // nothing, like `.`) names no directory, so it owns nothing.
    if !root.starts_with('/') {
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

    /// Whether `root` alone would own `call`.
    fn owned_by(call: &ToolCall, root: &str) -> bool {
        attribution_root(call, &[root]).is_some()
    }

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
        assert!(owned_by(&calls[0], "/repo"));
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
        assert!(!owned_by(edit, "/repo"));
        assert!(owned_by(edit, "/other"));

        let cmd = &parse_line(&record("Bash", r#"{"command":"ls"}"#))[0];
        assert!(owned_by(cmd, "/repo"));
        assert!(!owned_by(cmd, "/other"));
    }

    #[test]
    fn dot_components_are_resolved_by_text_before_a_path_is_compared() {
        // Each of these named `/w2/f` or somewhere else outside `/w1`.
        assert!(!under("/w1/../w2/f", "/w1"));
        assert!(under("/w1/../w2/f", "/w2"));
        assert!(under("/w1/./a/../b", "/w1"));
        assert!(!under("/w1/../../etc", "/w1"));
        assert!(!under("/w1/sub/../../w1x/f", "/w1"));
        assert!(under("/w1//a", "/w1"));
        // A `..` at the root stays at the root; a relative one that climbs out
        // of nothing is under no absolute root.
        assert!(under("/../w1/a", "/w1"));
        assert!(!under("../w1/a", "/w1"));
        // A root spelled with dots is the root it resolves to.
        assert!(under("/w2/f", "/w1/../w2"));
    }

    #[test]
    fn a_sibling_directory_is_not_the_repository() {
        // `/repo-other` must never attribute to `/repo`.
        let c = &parse_line(&record("Edit", r#"{"file_path":"/repo-other/a.rs"}"#))[0];
        assert!(!owned_by(c, "/repo"));
        assert!(under("/repo/a.rs", "/repo"));
        assert!(under("/repo/a.rs", "/repo/"));
        assert!(under("/repo", "/repo"));
        assert!(!under("/repo", ""));
        assert!(!under("", "/repo"));
    }

    #[test]
    fn a_worktree_under_the_repo_attributes_to_the_worktree_root() {
        // GitPulse opens worktrees as repositories, so both roots contain the
        // call and the deeper one owns it.
        let c = &parse_line(&record(
            "Edit",
            r#"{"file_path":"/repo/.claude/worktrees/wt/a.rs"}"#,
        ))[0];
        assert!(owned_by(c, "/repo"));
        assert!(owned_by(c, "/repo/.claude/worktrees/wt"));
        assert_eq!(
            attribution_root(c, &["/repo", "/repo/.claude/worktrees/wt"]),
            Some("/repo/.claude/worktrees/wt")
        );
        // Whatever the order roots are listed in.
        assert_eq!(
            attribution_root(c, &["/repo/.claude/worktrees/wt", "/repo"]),
            Some("/repo/.claude/worktrees/wt")
        );
    }

    fn edit_at(path: &str) -> ToolCall {
        parse_line(&record(
            "Edit",
            &serde_json::json!({ "file_path": path }).to_string(),
        ))
        .remove(0)
    }

    #[test]
    fn the_deepest_root_wins_whatever_its_spelling() {
        // A trailing separator must not make a shallower root look deeper.
        let c = edit_at("/repo/.claude/worktrees/wt/a.rs");
        assert_eq!(
            attribution_root(&c, &["/repo/", "/repo/.claude/worktrees/wt"]),
            Some("/repo/.claude/worktrees/wt")
        );
        let c = edit_at("/repo/a.rs");
        assert_eq!(attribution_root(&c, &["/repo/", "/repo"]), Some("/repo/"));
        // `/repo2` is a sibling of `/repo`, not a deeper or shallower root.
        let c = edit_at("/repo2/a.rs");
        assert_eq!(
            attribution_root(&c, &["/repo", "/repo2", "/repo/.claude/worktrees/wt"]),
            Some("/repo2")
        );
        let c = edit_at("/repo/a.rs");
        assert_eq!(
            attribution_root(&c, &["/repo2", "/repo", "/repo/.claude/worktrees/wt"]),
            Some("/repo")
        );
        // Under no root at all.
        assert_eq!(attribution_root(&edit_at("/other/a.rs"), &["/repo"]), None);
        assert_eq!(attribution_root(&edit_at("/repo/a.rs"), &[]), None);
    }

    #[test]
    fn roots_with_spaces_and_unicode_compare_bytewise() {
        let root = "/Users/b h/Café Ünï/repo";
        let wt = "/Users/b h/Café Ünï/repo/.claude/worktrees/ü wt";
        let c = edit_at(&format!("{wt}/a.rs"));
        assert_eq!(attribution_root(&c, &[root, wt]), Some(wt));
        let c = edit_at(&format!("{root}/a.rs"));
        assert_eq!(attribution_root(&c, &[root, wt]), Some(root));
        // A different normalization of the same text is a different root.
        let decomposed = "/Users/b h/Cafe\u{301} Ünï/repo/a.rs";
        assert_eq!(attribution_root(&edit_at(decomposed), &[root]), None);
    }

    fn command_in(cwd: &str, path: Option<&str>) -> ToolCall {
        ToolCall {
            session_id: "S".into(),
            ts_utc: "2026-09-01T12:00:00.000Z".into(),
            version: String::new(),
            cwd: cwd.into(),
            git_branch: None,
            tool: path.map_or("Bash", |_| "Edit").into(),
            file_path: path.map(str::to_string),
            command: path.is_none().then(|| "ls".to_string()),
            ..ToolCall::default()
        }
    }

    const REPO: &str = "/repo";
    const WT: &str = "/repo/.claude/worktrees/wt";

    fn departed<'a>(call: &ToolCall, roots: &[&'a str]) -> Option<(String, &'a str)> {
        departed_root(call, roots).map(|d| (d.root.to_string(), d.host))
    }

    #[test]
    fn a_worktree_that_is_not_live_is_the_one_segment_name_under_worktrees() {
        // The cwd is deep inside the removed worktree; the root is the name.
        let call = command_in(
            &format!("{WT}/src-tauri/src"),
            Some(&format!("{WT}/src-tauri/a.rs")),
        );
        assert_eq!(departed(&call, &[REPO]), Some((WT.into(), REPO)));
        // Trailing separators change nothing.
        let call = command_in(&format!("{WT}/"), None);
        assert_eq!(departed(&call, &[REPO]), Some((WT.into(), REPO)));
        // A live worktree is not departed.
        let call = command_in(&format!("{WT}/src"), None);
        assert_eq!(departed(&call, &[REPO, WT]), None);
        assert_eq!(departed(&call, &[REPO, &format!("{WT}/")]), None);
    }

    #[test]
    fn a_plain_subdirectory_is_never_departed() {
        for cwd in [
            "/repo/src-tauri",
            "/repo/.claude",
            "/repo/.claude/worktrees",
            "/repo/.claude/other/x",
            "/repo/old_dir_removed_by_checkout",
            "/repo/worktrees/x",
        ] {
            assert_eq!(departed(&command_in(cwd, None), &[REPO]), None, "{cwd}");
        }
    }

    #[test]
    fn the_deepest_convention_match_wins() {
        let a = "/repo/.claude/worktrees/a";
        let b = "/repo/.claude/worktrees/a/.claude/worktrees/b";
        let call = command_in(&format!("{b}/src"), Some(&format!("{b}/src/x.rs")));
        // Neither live: b.
        assert_eq!(departed(&call, &[REPO]), Some((b.into(), REPO)));
        // a live, b gone: b, hosted by a.
        assert_eq!(departed(&call, &[REPO, a]), Some((b.into(), a)));
        // b live: nothing departed.
        assert_eq!(departed(&call, &[REPO, a, b]), None);
        // The call's own path must lie under the departed worktree.
        let call = command_in(&format!("{b}/src"), Some("/repo/src/x.rs"));
        assert_eq!(departed(&call, &[REPO]), None);
        // A cwd in a, with a edit in b's tree: a's.
        let call = command_in(&format!("{a}/src"), Some(&format!("{b}/x.rs")));
        assert_eq!(departed(&call, &[REPO]), Some((a.into(), REPO)));
    }

    #[test]
    fn a_cwd_under_no_live_root_is_nobodys() {
        let call = command_in("/repo-old/.claude/worktrees/x", None);
        assert_eq!(departed(&call, &[REPO]), None);
        assert_eq!(departed(&call, &[]), None);
    }

    #[test]
    fn a_hostile_cwd_is_never_departed() {
        let long = format!("/repo/.claude/worktrees/{}", "x".repeat(64 * 1024));
        for cwd in [
            "/repo/.claude/worktrees/../x",
            "/repo/.claude/worktrees/./x",
            "/repo/.claude/worktrees//x",
            "/repo/.claude/worktrees/a\0b",
            "/repo/../.claude/worktrees/x",
            long.as_str(),
        ] {
            assert_eq!(departed(&command_in(cwd, None), &[REPO]), None, "{cwd:.60}");
        }
    }

    #[test]
    fn a_hundred_thousand_distinct_cwds_are_pure_string_work() {
        for i in 0..100_000 {
            let cwd = format!("/repo/.claude/worktrees/gone{i}/src");
            let call = command_in(&cwd, Some(&format!("{cwd}/a.rs")));
            let d = departed_root(&call, &[REPO]).unwrap();
            assert_eq!(
                d.root.len(),
                "/repo/.claude/worktrees/gone".len() + i.to_string().len()
            );
        }
    }

    fn parses_for(line: &str) -> (LineOutcome, usize) {
        let before = PARSES.with(std::cell::Cell::get);
        let outcome = parse_record(line);
        (outcome, PARSES.with(std::cell::Cell::get) - before)
    }

    const PROSE: &str = r#"{"type":"assistant","sessionId":"S1","timestamp":"2026-09-01T12:00:00.000Z","message":{"content":[{"type":"text","text":"done"}]}}"#;
    const TOOL_RESULT: &str =
        r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#;
    const BROKEN: &str = r#"{"type":"assistant", this is not json}"#;

    /// A line is deserialized at most once, whatever it turns out to hold. A
    /// prose-only assistant line used to be parsed by `parse_line` and again by
    /// the caller, which parsed it just to learn it was readable.
    #[test]
    fn no_line_is_deserialized_twice() {
        let edit = record("Edit", r#"{"file_path":"/repo/a.rs"}"#);
        for line in [edit.as_str(), PROSE, TOOL_RESULT, BROKEN, ""] {
            let (_, parses) = parses_for(line);
            assert!(parses <= 1, "{parses} parses of {line}");
        }
    }

    #[test]
    fn a_line_that_cannot_hold_a_call_is_never_built_into_a_document() {
        // A tool result is the bulk of a transcript's bytes. It is neither an
        // assistant record nor a `tool_use`, so nothing is deserialized.
        assert_eq!(parses_for(TOOL_RESULT), (LineOutcome::NoCalls, 0));
    }

    #[test]
    fn failure_is_reported_distinctly_from_having_no_calls() {
        assert_eq!(parse_record(BROKEN), LineOutcome::Unreadable);
        assert_eq!(parse_record(PROSE), LineOutcome::NoCalls);
        // Broken, but not tagged as an assistant record: not a gap worth
        // counting, exactly as before.
        assert_eq!(parse_record("not json at all"), LineOutcome::NoCalls);
        // A call-bearing line that is truncated mid-record.
        let edit = record("Edit", r#"{"file_path":"/repo/a.rs"}"#);
        assert_eq!(
            parse_record(&edit[..edit.len() - 3]),
            LineOutcome::Unreadable
        );
    }

    #[test]
    fn parse_line_and_parse_record_agree_on_calls() {
        let edit = record("Edit", r#"{"file_path":"/repo/a.rs"}"#);
        match parse_record(&edit) {
            LineOutcome::Calls(calls) => assert_eq!(calls, parse_line(&edit)),
            other => panic!("expected calls, got {other:?}"),
        }
        // A read-only tool is a readable record with nothing to record.
        let read = record("Read", r#"{"file_path":"/repo/a.rs"}"#);
        assert_eq!(parse_record(&read), LineOutcome::NoCalls);
    }
}

#[cfg(test)]
mod adv2_under_tests {
    use super::*;

    #[test]
    fn u01_the_fast_path_and_the_normaliser_agree_on_separator_shapes() {
        for (path, root, want) in [
            ("/w1/f", "/w1", true),
            ("/w1x/f", "/w1", false),
            ("/w1", "/w1/", true),
            ("/w1/", "/w1", true),
            ("//w1/f", "/w1", true),
            ("/w1//f", "/w1", true),
            ("/w1/./f", "/w1", true),
            ("/w1/.", "/w1", true),
            ("/w1/f/..", "/w1", true),
            ("/w1/..f", "/w1", true),
            ("/w1/.hidden", "/w1", true),
            ("/w1/...", "/w1", true),
            ("/w1x", "/w1/", false),
            ("/", "/", true),
            ("/a", "/", true),
            ("/a", "/w1/..", true),
            ("/w1/f", "/w1/f/", true),
            ("/w1/f/", "/w1/f", true),
        ] {
            assert_eq!(under(path, root), want, "under({path:?}, {root:?})");
        }
    }

    /// A relative root that normalises to nothing (`.`, `./`, `a/..`) becomes
    /// the empty string and then "owns" every absolute path.
    #[test]
    fn u02_a_relative_root_that_normalises_to_empty_owns_nothing() {
        for root in [".", "./", "a/..", "./."] {
            assert!(
                !under("/etc/passwd", root),
                "under(\"/etc/passwd\", {root:?}) claims an absolute path for a relative root"
            );
        }
    }

    #[test]
    fn u03_backslash_paths_are_never_under_by_the_documented_byte_rule() {
        assert!(!under("C:\\w1\\f", "C:\\w1"));
        assert!(!under("C:\\w1\\..\\w2", "C:\\w1"));
    }
}

#[cfg(test)]
mod reader_tests {
    use super::*;

    /// An assistant record with one content block of the given kind, in
    /// response `msg`, whose usage is `out` output tokens. Synthetic.
    fn assistant(ts: &str, msg: &str, block: &str, out: u64) -> String {
        format!(
            r#"{{"type":"assistant","sessionId":"S1","timestamp":"{ts}","version":"2.1.241","cwd":"/repo","gitBranch":"main","message":{{"id":"{msg}","content":[{block}],"usage":{{"input_tokens":3,"output_tokens":{out},"cache_read_input_tokens":100,"cache_creation_input_tokens":7,"service_tier":"standard"}}}}}}"#
        )
    }

    fn bash(id: &str) -> String {
        format!(
            r#"{{"type":"tool_use","id":"{id}","name":"Bash","input":{{"command":"cargo test"}}}}"#
        )
    }

    fn edit(id: &str) -> String {
        format!(
            r#"{{"type":"tool_use","id":"{id}","name":"Edit","input":{{"file_path":"/repo/a.rs"}}}}"#
        )
    }

    fn thinking() -> String {
        r#"{"type":"thinking","thinking":"SYNTHETIC-REASONING"}"#.to_string()
    }

    /// A user record holding one tool result.
    fn result(ts: &str, id: &str, is_error: Option<bool>) -> String {
        let flag = is_error.map_or(String::new(), |e| format!(r#","is_error":{e}"#));
        format!(
            r#"{{"type":"user","sessionId":"S1","timestamp":"{ts}","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{id}","content":"SYNTHETIC-OUTPUT"{flag}}}]}}}}"#
        )
    }

    fn feed(reader: &mut Reader, offset: u64, line: &str) -> Vec<ToolCall> {
        reader.locate(offset, offset ^ 0xabc);
        match reader.read(line) {
            LineOutcome::Calls(calls) => calls,
            LineOutcome::NoCalls => Vec::new(),
            LineOutcome::Unreadable => panic!("unreadable: {line}"),
        }
    }

    const T0: &str = "2026-09-01T12:00:00.000Z";
    const T0_2S: &str = "2026-09-01T12:00:02.500Z";

    #[test]
    fn a_call_is_written_with_the_error_flag_and_duration_of_its_result() {
        let mut reader = Reader::new(None);
        assert!(feed(&mut reader, 0, &assistant(T0, "m1", &bash("tu1"), 5)).is_empty());
        assert_eq!(reader.held_from(), Some(0), "the call is owed its result");
        let done = feed(&mut reader, 400, &result(T0_2S, "tu1", Some(true)));
        assert_eq!(done.len(), 1);
        let call = &done[0];
        assert_eq!(call.tool, "Bash");
        assert_eq!(call.result, CallResult::Errored);
        assert_eq!(call.duration_ms, Some(2500));
        // The row is the call line's, not the result line's.
        let origin = call.origin.expect("origin");
        assert_eq!((origin.offset, origin.ordinal), (0, 0));
        assert_eq!(origin.digest, 0xabc);
    }

    #[test]
    fn a_result_without_the_error_flag_is_a_success_not_an_unknown() {
        let mut reader = Reader::new(None);
        feed(&mut reader, 0, &assistant(T0, "m1", &edit("tu1"), 5));
        let done = feed(&mut reader, 10, &result(T0_2S, "tu1", None));
        assert_eq!(done[0].result, CallResult::Succeeded);
        let mut reader = Reader::new(None);
        feed(&mut reader, 0, &assistant(T0, "m1", &edit("tu2"), 5));
        let done = feed(&mut reader, 10, &result(T0_2S, "tu2", Some(false)));
        assert_eq!(done[0].result, CallResult::Succeeded);
    }

    #[test]
    fn a_result_older_than_its_call_has_no_duration_rather_than_a_negative_one() {
        let mut reader = Reader::new(None);
        feed(&mut reader, 0, &assistant(T0_2S, "m1", &bash("tu1"), 5));
        let done = feed(&mut reader, 10, &result(T0, "tu1", None));
        assert_eq!(done[0].duration_ms, None);
    }

    #[test]
    fn a_call_with_no_result_stays_held_and_is_then_written_unobserved() {
        let mut reader = Reader::new(None);
        feed(&mut reader, 0, &assistant(T0, "m1", &bash("tu1"), 5));
        // The file is live: nothing is given up on.
        assert!(reader.finish(false).is_empty());
        assert_eq!(reader.held_from(), Some(0));
        // Still: it is written as it stands, and says it has no result.
        let flushed = reader.finish(true);
        assert_eq!(flushed.iter().filter(|c| c.tool == "Bash").count(), 1);
        let call = flushed.iter().find(|c| c.tool == "Bash").unwrap();
        assert_eq!(call.result, CallResult::Unobserved);
        assert_eq!(call.duration_ms, None);
        assert_eq!(reader.held_from(), None);
    }

    #[test]
    fn the_cursor_is_held_at_the_earliest_line_still_owed() {
        let mut reader = Reader::new(None);
        feed(&mut reader, 100, &assistant(T0, "m1", &bash("a"), 5));
        feed(&mut reader, 300, &assistant(T0, "m2", &bash("b"), 5));
        assert_eq!(reader.held_from(), Some(100));
        feed(&mut reader, 500, &result(T0_2S, "a", None));
        assert_eq!(reader.held_from(), Some(300));
    }

    #[test]
    fn a_call_far_behind_the_newest_record_is_given_up_on() {
        let mut reader = Reader::new(None);
        feed(&mut reader, 0, &assistant(T0, "m1", &bash("tu1"), 5));
        // Twenty minutes later by the transcript's own clock: past any command's limit.
        let late = "2026-09-01T12:20:00.000Z";
        let out = feed(&mut reader, 900, &assistant(late, "m2", &thinking(), 9));
        let call = out.iter().find(|c| c.tool == "Bash").expect("given up on");
        assert_eq!(call.result, CallResult::Unobserved);
        // A call 14 minutes behind is not.
        let mut reader = Reader::new(None);
        feed(&mut reader, 0, &assistant(T0, "m1", &bash("tu1"), 5));
        let soon = "2026-09-01T12:14:00.000Z";
        assert!(
            feed(&mut reader, 900, &assistant(soon, "m2", &thinking(), 9))
                .iter()
                .all(|c| c.tool != "Bash")
        );
    }

    #[test]
    fn the_calls_waiting_for_results_are_bounded() {
        let mut reader = Reader::new(None);
        let mut written = 0;
        for i in 0..(MAX_PENDING as u64 + 10) {
            let id = format!("tu{i}");
            written += feed(
                &mut reader,
                i * 10,
                &assistant(T0, &format!("m{i}"), &bash(&id), 1),
            )
            .iter()
            .filter(|c| c.tool == "Bash")
            .count();
        }
        assert_eq!(written, 10, "the oldest ten were pushed out unpaired");
        assert!(reader.pending.len() <= MAX_PENDING);
    }

    #[test]
    fn a_response_is_counted_once_however_many_records_repeat_its_usage() {
        // One response, three records (thinking, then two tool calls), each
        // repeating the response's usage; counting per record would triple it.
        let mut reader = Reader::new(None);
        let mut usage = Vec::new();
        for (offset, block) in [(0, thinking()), (100, bash("a")), (200, edit("b"))] {
            usage.extend(
                feed(&mut reader, offset, &assistant(T0, "m1", &block, 42))
                    .into_iter()
                    .filter(|c| c.tool == USAGE_TOOL),
            );
        }
        assert!(usage.is_empty(), "the response is still being read");
        assert_eq!(reader.held_from(), Some(0));
        // The next response closes it.
        let closed: Vec<ToolCall> = feed(&mut reader, 300, &assistant(T0_2S, "m2", &thinking(), 1))
            .into_iter()
            .filter(|c| c.tool == USAGE_TOOL)
            .collect();
        assert_eq!(closed.len(), 1);
        let counted = closed[0].usage.expect("usage");
        assert_eq!(
            counted,
            TokenUsage {
                input: 3,
                output: 42,
                cache_read: 100,
                cache_creation: 7
            }
        );
        assert_eq!(closed[0].message_id.as_deref(), Some("m1"));
        assert_eq!(closed[0].action(), "session.usage");
        // Its identity is the first record of the response.
        assert_eq!(closed[0].origin.map(|o| o.offset), Some(0));
        // Never counted again, even if its records come round once more (here
        // m2, which was open, is closed by the interruption and counted once).
        let again = feed(&mut reader, 400, &assistant(T0_2S, "m1", &thinking(), 42));
        assert!(again.iter().all(|c| c.message_id.as_deref() != Some("m1")));
        let rest = reader.finish(true);
        assert!(rest.iter().all(|c| c.message_id.as_deref() != Some("m1")));
    }

    #[test]
    fn the_last_response_is_written_when_the_file_has_gone_still() {
        let mut reader = Reader::new(None);
        feed(&mut reader, 0, &assistant(T0, "m1", &thinking(), 42));
        assert!(reader.finish(false).is_empty());
        assert_eq!(reader.held_from(), Some(0));
        let last = reader.finish(true);
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].usage.map(|u| u.output), Some(42));
    }

    #[test]
    fn a_record_without_counts_is_not_a_response_that_used_none() {
        let mut reader = Reader::new(None);
        let bare = r#"{"type":"assistant","sessionId":"S1","timestamp":"2026-09-01T12:00:00.000Z","cwd":"/repo","message":{"id":"m1","content":[]}}"#;
        feed(&mut reader, 0, bare);
        assert_eq!(reader.held_from(), None);
        assert!(reader.finish(true).is_empty());
    }

    #[test]
    fn a_subagent_transcript_names_its_agent_on_every_row() {
        assert_eq!(
            subagent_id_of(std::path::Path::new("/p/s/subagents/agent-a1b2c3.jsonl")).as_deref(),
            Some("a1b2c3")
        );
        assert_eq!(subagent_id_of(std::path::Path::new("/p/s.jsonl")), None);
        assert_eq!(
            subagent_id_of(std::path::Path::new("/p/agent-.jsonl")),
            None
        );
        let mut reader = Reader::for_path(std::path::Path::new("/p/agent-a1b2c3.jsonl"));
        feed(&mut reader, 0, &assistant(T0, "m1", &bash("tu1"), 5));
        let mut rows = feed(&mut reader, 10, &result(T0_2S, "tu1", None));
        rows.extend(reader.finish(true));
        assert_eq!(rows.len(), 2, "the call and its response's usage");
        assert!(rows
            .iter()
            .all(|r| r.subagent_id.as_deref() == Some("a1b2c3")));
        // The session id is still the parent's: that is why the id is needed.
        assert!(rows.iter().all(|r| r.session_id == "S1"));
        let mut own = Reader::for_path(std::path::Path::new("/p/S1.jsonl"));
        feed(&mut own, 0, &assistant(T0, "m1", &bash("tu1"), 5));
        let own_rows = feed(&mut own, 10, &result(T0_2S, "tu1", None));
        assert_eq!(own_rows[0].subagent_id, None);
    }

    #[test]
    fn a_result_line_is_read_once_and_a_prompt_never() {
        let prompt = r#"{"type":"user","sessionId":"S1","timestamp":"2026-09-01T12:00:00.000Z","message":{"role":"user","content":"SYNTHETIC-PROMPT"}}"#;
        let mut reader = Reader::new(None);
        let before = PARSES.with(std::cell::Cell::get);
        reader.read(prompt);
        assert_eq!(PARSES.with(std::cell::Cell::get) - before, 0);
        reader.read(&result(T0_2S, "tuX", None));
        assert_eq!(PARSES.with(std::cell::Cell::get) - before, 1);
    }

    #[test]
    fn a_result_for_a_call_that_was_never_held_writes_nothing() {
        // The call was in an earlier pass's range and is owed to that pass.
        let mut reader = Reader::new(None);
        assert!(feed(&mut reader, 0, &result(T0_2S, "gone", Some(true))).is_empty());
    }

    #[test]
    fn what_a_result_said_is_never_carried() {
        let mut reader = Reader::new(None);
        feed(&mut reader, 0, &assistant(T0, "m1", &bash("tu1"), 5));
        let done = feed(&mut reader, 10, &result(T0_2S, "tu1", Some(true)));
        let shown = format!("{done:?}");
        assert!(!shown.contains("SYNTHETIC-OUTPUT"));
        assert!(!shown.contains("SYNTHETIC-REASONING"));
    }
}
