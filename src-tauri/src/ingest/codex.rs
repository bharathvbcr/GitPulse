//! Codex rollouts, read as a transcript source.
//!
//! Codex writes one JSONL file per thread under
//! `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<time>-<thread id>.jsonl` (`CODEX_HOME`
//! defaults to `~/.codex`). Every line is an envelope `{"timestamp", "ordinal",
//! "type", "payload"}`, as the Codex CLI `0.149.1` writes it (the rollouts on the
//! build machine); the tool-call payloads are Codex's protocol types. Read:
//!
//! * `session_meta` is the first line. Its payload carries the thread `id`, the
//!   `cwd` and the `cli_version`. Nothing else on a line says which thread or
//!   directory it belongs to, so the reader keeps them, and a read that starts
//!   mid-file (the cursor resumed) takes them by replaying the file's context
//!   lines from its start, under a byte budget (`CodexParser::recover`).
//! * `turn_context` carries the `cwd` of the turn that follows.
//! * A command or a file edit is a `response_item` tool call, in one of the
//!   structured shapes `scan_payload` lists, read from Codex's own protocol types
//!   (`function_call` `exec_command` / `shell`, `local_shell_call`,
//!   `custom_tool_call` `apply_patch`). A patch's edits are its `*** Add File:` /
//!   `*** Update File:` / `*** Delete File:` / `*** Move to:` header lines.
//! * `event_msg`, `compacted`, `world_state`, `inter_agent_communication_metadata`
//!   and every other `response_item` (messages, reasoning, tool outputs, calls to
//!   tools that change nothing) cannot hold a call.
//!
//! # Code mode is parsed, within bounds
//!
//! The Codex SDK's code mode writes a `custom_tool_call` named `exec` whose
//! `input` is a JavaScript program that calls `tools.exec_command(...)` and
//! `tools.apply_patch(...)`. An earlier hand-written reader of that text was wrong
//! both ways (real calls lost to a division it took for a regex, fake ones made of
//! a regex literal), so [`js`] reads the program with a real grammar instead.
//! Straight-line calls with literal arguments become rows exactly as the
//! structured shapes do; every other call, and every program too large, too slow
//! or too deep to read, is counted with a reason and never a row. `js` states the
//! whole contract.
//!
//! Anything else is counted too, never guessed at: a line that is not an envelope
//! of a known `type`, a call to a shell or patch tool in a shape this reader does
//! not know (a `shell_command`, an `exec_command` whose arguments do not parse, a
//! `local_shell_call` of another action). The format moves, and a count of what
//! was not read is what keeps a partial history from reading as a whole one.
//!
//! Like Claude's reader, only the commands and paths an agent executed are
//! extracted; prompts, reasoning and prose are never kept.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::source::{Parsed, RecordParser, SourceRoot, TranscriptSource};
use super::transcript::ToolCall;

mod js;

/// The variable that moves the Codex rollout directory, as
/// `GITPULSE_TRANSCRIPT_ROOT` moves Claude's. It wins over `CODEX_HOME`.
pub(crate) const ROOT_ENV: &str = "GITPULSE_CODEX_TRANSCRIPT_ROOT";

/// The most of one rollout replayed from its start, on a resume, to recover what
/// its `session_meta` and `turn_context` lines said. A rollout resumed past it
/// has its calls counted unread rather than placed by a directory that may be
/// stale; below it, the replay is paid for from the pass's budget
/// ([`super::source::RESUME_PASS_BYTES`]) and deferred, never dropped, when that runs out.
const RESUME_FILE_BYTES: u64 = 128 << 20;

/// The longest single line read on a resume (the `session_meta` line carries the
/// whole system prompt).
const RESUME_LINE_BYTES: u64 = 8 << 20;

pub(super) struct CodexSource;

impl TranscriptSource for CodexSource {
    fn name(&self) -> &'static str {
        "Codex"
    }

    fn actor_id(&self) -> &'static str {
        "codex"
    }

    fn root(&self) -> Option<SourceRoot> {
        if let Some(path) = std::env::var_os(ROOT_ENV).filter(|p| !p.is_empty()) {
            return Some(SourceRoot {
                path: PathBuf::from(path),
                explicit: true,
            });
        }
        // Codex's own home: `CODEX_HOME`, else `~/.codex`; an empty value is
        // unset (`codex-rs/utils/home-dir/src/lib.rs`). Its sessions are below it.
        let home = std::env::var_os("CODEX_HOME")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .or_else(|| super::dirs_home().map(|home| home.join(".codex")))?;
        Some(SourceRoot {
            path: home.join("sessions"),
            explicit: false,
        })
    }

    fn legacy_floor(&self) -> bool {
        false
    }

    fn reports_unwalkable_root(&self) -> bool {
        true
    }

    fn optional(&self) -> bool {
        true
    }

    fn open(&self, path: &Path, start: u64, budget: &mut u64) -> Option<Box<dyn RecordParser>> {
        let mut parser = CodexParser::default();
        if start > RESUME_FILE_BYTES {
            // Never replayable: what follows is counted, not placed.
            parser.lost = true;
        } else if start > 0 {
            if start > *budget {
                return None;
            }
            *budget -= start;
            parser.recover(path, start);
        }
        Some(Box::new(parser))
    }

    fn recognises(&self, path: &Path) -> bool {
        first_line_is_session_meta(path)
    }
}

/// Whether the file's first line is a Codex `session_meta` envelope: the one
/// thing no Claude transcript starts with.
fn first_line_is_session_meta(path: &Path) -> bool {
    use std::io::{BufRead, Read};
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut reader = std::io::BufReader::new(file.take(RESUME_LINE_BYTES));
    let mut line = Vec::new();
    // Blank lines first, as the main reader skips them.
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return false,
            Ok(_) if line.iter().all(u8::is_ascii_whitespace) => continue,
            Ok(_) => break,
        }
    }
    let Ok(text) = std::str::from_utf8(&line) else {
        return false;
    };
    serde_json::from_str::<Head<'_>>(text)
        .is_ok_and(|head| head.kind.as_deref() == Some("session_meta") && head.payload.is_some())
}

/// What a rollout's earlier lines said about the ones after them.
#[derive(Default)]
struct CodexParser {
    thread: String,
    cwd: String,
    version: String,
    /// A resume could not recover the context it needs (the rollout's start was
    /// not read in full), so no call after it can be placed.
    lost: bool,
}

/// The envelope, read without building the payload.
#[derive(Deserialize)]
struct Head<'a> {
    #[serde(borrow, rename = "type")]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    timestamp: Option<Cow<'a, str>>,
    payload: Option<serde::de::IgnoredAny>,
}

/// Envelope `type`s that cannot hold a tool call.
const INERT: [&str; 4] = [
    "event_msg",
    "compacted",
    "world_state",
    "inter_agent_communication_metadata",
];

/// Payload `type`s of a `response_item` that can be a tool call, matched as the
/// quoted JSON string so the common line is not parsed at all.
const CALL_NEEDLES: [&str; 3] = [
    "\"custom_tool_call\"",
    "\"function_call\"",
    "\"local_shell_call\"",
];

impl RecordParser for CodexParser {
    fn parse(&mut self, line: &str) -> Parsed {
        let Ok(head) = serde_json::from_str::<Head<'_>>(line) else {
            return Parsed::skipped();
        };
        let (Some(kind), Some(_)) = (head.kind.as_deref(), head.payload.as_ref()) else {
            return Parsed::skipped();
        };
        match kind {
            "session_meta" | "turn_context" => {
                if let Some(record) = parse_value(line) {
                    self.note_context(kind, &record["payload"]);
                    return Parsed::nothing();
                }
                Parsed::skipped()
            }
            "response_item" => {
                if !CALL_NEEDLES.iter().any(|needle| line.contains(needle)) {
                    return Parsed::nothing();
                }
                let Some(record) = parse_value(line) else {
                    return Parsed::skipped();
                };
                self.calls_in(&record["payload"], head.timestamp.as_deref())
            }
            other if INERT.contains(&other) => Parsed::nothing(),
            _ => Parsed::skipped(),
        }
    }
}

fn parse_value(line: &str) -> Option<serde_json::Value> {
    serde_json::from_str(line).ok()
}

impl CodexParser {
    /// Replays the context lines of the first `start` bytes, which a resumed read
    /// does not see: `session_meta` names the thread, and each `turn_context`
    /// moves the directory, so the one in force at `start` is the last before it.
    ///
    /// A line past [`RESUME_LINE_BYTES`] is skipped as the main reader skips it
    /// (it is a big tool output, counted when first read), unless its head says it
    /// is a context line: then the context is `lost`, as it is when the file
    /// cannot be read up to `start`.
    fn recover(&mut self, path: &Path, start: u64) {
        use std::io::{BufRead, Read};
        let Ok(file) = std::fs::File::open(path) else {
            self.lost = true;
            return;
        };
        let mut reader = std::io::BufReader::new(file.take(start));
        let mut line = Vec::new();
        let mut read = 0u64;
        while read < start {
            line.clear();
            let Ok(n) = reader
                .by_ref()
                .take(RESUME_LINE_BYTES)
                .read_until(b'\n', &mut line)
            else {
                break;
            };
            if n == 0 {
                break;
            }
            read += n as u64;
            if line.last() != Some(&b'\n') && n as u64 >= RESUME_LINE_BYTES {
                let head = &line[..line.len().min(512)];
                let head = String::from_utf8_lossy(head);
                if head.contains("\"session_meta\"") || head.contains("\"turn_context\"") {
                    self.lost = true;
                    return;
                }
                // Skip the rest of the line without keeping it.
                let mut rest = Vec::new();
                loop {
                    rest.clear();
                    let Ok(m) = reader.by_ref().take(1 << 20).read_until(b'\n', &mut rest) else {
                        break;
                    };
                    read += m as u64;
                    if m == 0 || rest.last() == Some(&b'\n') {
                        break;
                    }
                }
                continue;
            }
            let Ok(text) = std::str::from_utf8(&line) else {
                continue;
            };
            if text.contains("\"session_meta\"") || text.contains("\"turn_context\"") {
                self.parse(text.trim_end_matches(['\n', '\r']));
            }
        }
        self.lost = self.lost || read < start;
    }

    fn note_context(&mut self, kind: &str, payload: &serde_json::Value) {
        if kind == "session_meta" {
            let thread = payload["id"]
                .as_str()
                .or_else(|| payload["session_id"].as_str());
            if let Some(thread) = thread {
                self.thread = thread.to_string();
            }
            if let Some(version) = payload["cli_version"].as_str() {
                self.version = version.to_string();
            }
        }
        if let Some(cwd) = payload["cwd"].as_str() {
            self.cwd = cwd.to_string();
        }
    }

    fn calls_in(&self, payload: &serde_json::Value, timestamp: Option<&str>) -> Parsed {
        let Some(found) = scan_payload(payload) else {
            return Parsed::nothing();
        };
        if found.commands.is_empty() && found.patched.is_empty() && found.unread == 0 {
            return Parsed::nothing();
        }
        let mut parsed = Parsed {
            calls: Vec::new(),
            skipped: i64::try_from(found.unread).unwrap_or(i64::MAX),
        };
        let (Some(ts_utc), false, false) = (
            timestamp.filter(|ts| !ts.is_empty()),
            self.thread.is_empty() || self.lost,
            self.cwd.is_empty() || self.lost,
        ) else {
            // Without a time, a thread or a directory a call cannot be placed:
            // counted, as a Claude record without them is.
            parsed.skipped +=
                i64::try_from(found.commands.len() + found.patched.len()).unwrap_or(i64::MAX);
            return parsed;
        };
        let call =
            |tool: &str, cwd: &str, file_path: Option<String>, command: Option<String>| ToolCall {
                session_id: self.thread.clone(),
                ts_utc: ts_utc.to_string(),
                version: self.version.clone(),
                cwd: cwd.to_string(),
                git_branch: None,
                tool: tool.to_string(),
                file_path,
                command,
            };
        for command in found.commands {
            // A relative workdir is relative to the turn's directory.
            let cwd = match command.workdir {
                Some(dir) if dir.starts_with('/') => dir,
                Some(dir) => format!("{}/{dir}", self.cwd.trim_end_matches('/')),
                None => self.cwd.clone(),
            };
            parsed
                .calls
                .push(call("Bash", &cwd, None, Some(command.cmd)));
        }
        for path in found.patched {
            let absolute = if path.starts_with('/') {
                path
            } else {
                format!("{}/{path}", self.cwd.trim_end_matches('/'))
            };
            parsed
                .calls
                .push(call("Edit", &self.cwd, Some(absolute), None));
        }
        parsed
    }
}

/// What a tool-call payload of a `response_item` holds; `None` for one that
/// cannot change anything.
///
/// The shapes are Codex's own (`codex-rs/protocol/src/models.rs`):
/// * `function_call` `exec_command`, `arguments` a JSON string
///   `{cmd, workdir?}` (`core/src/tools/handlers/unified_exec`, `shell_spec.rs`);
/// * `function_call` `shell`, `arguments` `{command: [argv], workdir?}`
///   (`ShellToolCallParams`);
/// * `local_shell_call`, `action` `{type: "exec", command: [argv],
///   working_directory?}` (`LocalShellExecAction`);
/// * `custom_tool_call` `apply_patch`, `input` the raw patch text
///   (`core/src/tools/handlers/apply_patch.rs`);
/// * `custom_tool_call` `exec`, code mode, `input` JavaScript: the calls [`js`]
///   reads, and one counted skip for each it cannot.
///
/// A call to a shell or patch tool in any other shape (or one of these that does
/// not parse) is counted unread. A call to a tool that changes nothing is not.
fn scan_payload(payload: &serde_json::Value) -> Option<Scanned> {
    let kind = payload["type"].as_str().unwrap_or_default();
    let name = payload["name"].as_str().unwrap_or_default();
    let unread = || Scanned {
        unread: 1,
        ..Scanned::default()
    };
    let one = |command: Option<Command>| match command {
        Some(command) => Scanned {
            commands: vec![command],
            ..Scanned::default()
        },
        None => unread(),
    };
    match (kind, name) {
        ("custom_tool_call", "exec") => Some(match payload["input"].as_str() {
            Some(input) => {
                let program = js::scan(input, &js::LIMITS);
                for reason in &program.skips {
                    log::debug!("codex rollout: {reason}");
                }
                Scanned {
                    commands: program.commands,
                    patched: program.patched,
                    unread: program.skips.len(),
                }
            }
            None => unread(),
        }),
        ("custom_tool_call", "apply_patch") => Some(match payload["input"].as_str() {
            Some(input) => {
                let patched = patch_paths(input);
                let unread = usize::from(patched.is_empty());
                Scanned {
                    patched,
                    unread,
                    ..Scanned::default()
                }
            }
            None => unread(),
        }),
        ("function_call", "exec_command") => {
            let args = arguments(payload);
            let cmd = args.as_ref().and_then(|a| a["cmd"].as_str());
            Some(one(cmd.map(|cmd| Command {
                cmd: cmd.to_string(),
                workdir: args.as_ref().and_then(|a| text(&a["workdir"])),
            })))
        }
        ("function_call", "shell") => {
            let args = arguments(payload);
            let argv = args.as_ref().and_then(|a| argv(&a["command"]));
            Some(one(argv.map(|argv| Command {
                cmd: argv,
                workdir: args.as_ref().and_then(|a| text(&a["workdir"])),
            })))
        }
        ("local_shell_call", _) => {
            let action = &payload["action"];
            let exec = action["type"].as_str() == Some("exec");
            let argv = exec.then(|| argv(&action["command"])).flatten();
            Some(one(argv.map(|argv| Command {
                cmd: argv,
                workdir: text(&action["working_directory"]),
            })))
        }
        ("custom_tool_call" | "function_call", _)
            if ["exec_command", "shell", "apply_patch"]
                .iter()
                .any(|tool| name.contains(tool)) =>
        {
            Some(unread())
        }
        _ => None,
    }
}

/// The JSON a `function_call` carries in its `arguments` string.
fn arguments(payload: &serde_json::Value) -> Option<serde_json::Value> {
    serde_json::from_str(payload["arguments"].as_str()?).ok()
}

fn text(value: &serde_json::Value) -> Option<String> {
    value.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

/// An argv as the command line it ran: the script of a `bash -lc <script>`
/// style wrapper, otherwise the words joined by spaces. `None` unless it is a
/// non-empty array of strings.
fn argv(value: &serde_json::Value) -> Option<String> {
    let words: Vec<&str> = value
        .as_array()?
        .iter()
        .map(serde_json::Value::as_str)
        .collect::<Option<_>>()?;
    match words.as_slice() {
        [] => None,
        [_, flag, script] if matches!(*flag, "-c" | "-lc") => Some((*script).to_string()),
        words => Some(words.join(" ")),
    }
}

/// What one tool-call payload holds.
#[derive(Default)]
struct Scanned {
    commands: Vec<Command>,
    /// Paths named by patch headers, as written.
    patched: Vec<String>,
    /// Calls this build did not read, each counted once in `skipped_lines`.
    unread: usize,
}

#[derive(Debug)]
struct Command {
    cmd: String,
    workdir: Option<String>,
}

/// The files a freeform patch names: its `*** Add File:` / `*** Update File:` /
/// `*** Delete File:` / `*** Move to:` header lines.
fn patch_paths(input: &str) -> Vec<String> {
    const HEADERS: [&str; 4] = [
        "*** Add File: ",
        "*** Update File: ",
        "*** Delete File: ",
        "*** Move to: ",
    ];
    input
        .split('\n')
        .filter_map(|line| {
            // Column 0: an indented ` *** Update File:` is a context line of the
            // patch (a line that already held those words), and `+*** …` an
            // added one.
            let rest = HEADERS.iter().find_map(|h| line.strip_prefix(h))?;
            let path = rest.trim();
            (!path.is_empty()).then(|| path.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_headers_are_the_lines_that_start_with_them() {
        let patch = "*** Begin Patch\n*** Add File: a.rs\n+x\n*** Update File: /abs/b.rs\n*** Move to: c.rs\n *** Update File: context.rs\n+*** Add File: text.rs\n*** End Patch";
        assert_eq!(patch_paths(patch), vec!["a.rs", "/abs/b.rs", "c.rs"]);
    }

    fn exec_scanned(input: &str) -> Scanned {
        scan_payload(&serde_json::json!({
            "type": "custom_tool_call", "name": "exec", "input": input
        }))
        .unwrap()
    }

    #[test]
    fn a_code_mode_exec_yields_its_straight_line_literal_calls() {
        let scanned = exec_scanned(
            "await tools.exec_command({cmd:'ls', workdir:'sub'}); await tools.exec_command({cmd:'pwd'})",
        );
        let cmds: Vec<_> = scanned
            .commands
            .iter()
            .map(|c| (c.cmd.as_str(), c.workdir.as_deref()))
            .collect();
        assert_eq!(cmds, vec![("ls", Some("sub")), ("pwd", None)]);
        assert_eq!(scanned.unread, 0);
    }

    #[test]
    fn a_code_mode_exec_that_is_not_text_is_one_counted_skip() {
        let payload = serde_json::json!({"type": "custom_tool_call", "name": "exec", "input": 7});
        assert_eq!(scan_payload(&payload).unwrap().unread, 1);
    }
}
