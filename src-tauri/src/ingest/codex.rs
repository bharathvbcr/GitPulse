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
//!   mid-file (the cursor resumed) takes them from the file's first line.
//! * `turn_context` carries the `cwd` of the turn that follows. A resume that
//!   starts after it falls back to the `session_meta` one.
//! * A command or a file edit is a `response_item` tool call, in one of the
//!   shapes `scan_payload` lists, read from Codex's own protocol types
//!   (`function_call` `exec_command` / `shell`, `local_shell_call`,
//!   `custom_tool_call` `apply_patch`). The fifth is code mode, which the Codex
//!   SDK writes (and the only one rollouts on the build machine held): a
//!   `custom_tool_call` named `exec` whose `input` is JavaScript, read for the
//!   string literal handed to `tools.exec_command({cmd: ..., workdir: ...})` and
//!   the headers of a patch handed to `tools.apply_patch`. Code mode is a real
//!   Codex feature (`CodeModeExecuteHandler` in `core/src/tools/handlers`), so
//!   its scanner stays; it is the narrow part, and what it cannot read is counted.
//!   A patch's edits are its `*** Add File:` / `*** Update File:` /
//!   `*** Delete File:` / `*** Move to:` headers.
//! * `event_msg`, `compacted`, `world_state`, `inter_agent_communication_metadata`
//!   and every other `response_item` (messages, reasoning, tool outputs, calls to
//!   tools that change nothing) cannot hold a call.
//!
//! Anything else is counted, never guessed at: a line that is not an envelope of
//! a known `type`, a call to a shell or patch tool in a shape this reader does
//! not know (a `shell_command`, an `exec_command` whose arguments do not parse,
//! a `local_shell_call` of another action), and a code-mode command whose string
//! is not a literal (a variable, a concatenation, a template with `${}`). The format moves, and a count of what was not read is
//! what keeps a partial history from reading as a whole one.
//!
//! Like Claude's reader, only the commands and paths an agent executed are
//! extracted; prompts, reasoning and prose are never kept.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::source::{Parsed, RecordParser, SourceRoot, TranscriptSource};
use super::transcript::ToolCall;

/// The variable that moves the Codex rollout directory, as
/// `GITPULSE_TRANSCRIPT_ROOT` moves Claude's. It wins over `CODEX_HOME`.
pub(crate) const ROOT_ENV: &str = "GITPULSE_CODEX_TRANSCRIPT_ROOT";

/// How much of a rollout's first line is read to find its `session_meta` when a
/// pass resumes mid-file. The line carries the whole system prompt.
const HEAD_BYTES: u64 = 1 << 20;

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

    fn open(&self, path: &Path, start: u64) -> Box<dyn RecordParser> {
        let mut parser = CodexParser::default();
        if start > 0 {
            if let Some(first) = first_line(path) {
                parser.parse(&first);
            }
        }
        Box::new(parser)
    }
}

/// The first line of the file at `path`, if it ends within [`HEAD_BYTES`].
fn first_line(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut head = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(HEAD_BYTES)
        .read_to_end(&mut head)
        .ok()?;
    let end = head.iter().position(|b| *b == b'\n')?;
    String::from_utf8(head[..end].to_vec()).ok()
}

/// What a rollout's earlier lines said about the ones after them.
#[derive(Default)]
struct CodexParser {
    thread: String,
    cwd: String,
    version: String,
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
            self.thread.is_empty(),
            self.cwd.is_empty(),
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
/// * `custom_tool_call` `exec`, code mode, `input` JavaScript: what the Codex
///   SDK writes, and the only shape the rollouts on the build machine held.
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
        ("custom_tool_call", "exec") => {
            Some(payload["input"].as_str().map_or_else(unread, scan_input))
        }
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

/// What the JavaScript of one `exec` call holds.
#[derive(Default)]
struct Scanned {
    commands: Vec<Command>,
    /// Paths named by patch headers, as written.
    patched: Vec<String>,
    /// `tools.exec_command` / `tools.apply_patch` calls whose command or paths
    /// could not be recovered.
    unread: usize,
}

struct Command {
    cmd: String,
    workdir: Option<String>,
}

const EXEC: &str = "tools.exec_command(";
const PATCH: &str = "tools.apply_patch(";

/// The most calls one `exec` is read for. A script with more is a program, not a
/// call; the rest are counted unread.
const MAX_CALLS_PER_EXEC: usize = 256;

fn scan_input(input: &str) -> Scanned {
    let mut scanned = Scanned::default();
    let mut from = 0;
    while let Some(at) = input[from..].find(EXEC) {
        let start = from + at + EXEC.len();
        from = start;
        if scanned.commands.len() + scanned.unread >= MAX_CALLS_PER_EXEC {
            scanned.unread += 1;
            continue;
        }
        match command_at(input, start) {
            Some(command) => scanned.commands.push(command),
            None => scanned.unread += 1,
        }
    }
    if input.contains(PATCH) {
        scanned.patched = patch_paths(input);
        if scanned.patched.is_empty() {
            scanned.unread += 1;
        }
    }
    scanned
}

/// The `{cmd: ..., workdir: ...}` object literal that starts at `start`.
fn command_at(input: &str, start: usize) -> Option<Command> {
    let mut cursor = Cursor {
        text: input,
        at: start,
    };
    cursor.skip_space();
    if !cursor.eat('{') {
        return None;
    }
    let (mut cmd, mut workdir) = (None, None);
    loop {
        cursor.skip_space();
        while cursor.eat(',') {
            cursor.skip_space();
        }
        if cursor.eat('}') {
            break;
        }
        let key = cursor.key()?;
        cursor.skip_space();
        if !cursor.eat(':') {
            return None;
        }
        cursor.skip_space();
        let value = cursor.value()?;
        match (key.as_str(), value) {
            ("cmd", Some(text)) => cmd = Some(text),
            ("workdir", Some(text)) => workdir = Some(text),
            // A `cmd` that is not a literal cannot be read.
            ("cmd", None) => return None,
            _ => {}
        }
    }
    Some(Command { cmd: cmd?, workdir })
}

struct Cursor<'a> {
    text: &'a str,
    at: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<char> {
        self.text[self.at..].chars().next()
    }

    fn eat(&mut self, wanted: char) -> bool {
        if self.peek() == Some(wanted) {
            self.at += wanted.len_utf8();
            true
        } else {
            false
        }
    }

    fn skip_space(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.at += self.peek().map_or(0, char::len_utf8);
        }
    }

    /// A property name: an identifier, or a string literal.
    fn key(&mut self) -> Option<String> {
        match self.peek()? {
            '"' | '\'' => self.literal(),
            c if c.is_alphanumeric() || c == '_' || c == '$' => {
                let start = self.at;
                while self
                    .peek()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$')
                {
                    self.at += self.peek().map_or(0, char::len_utf8);
                }
                Some(self.text[start..self.at].to_string())
            }
            _ => None,
        }
    }

    /// A property value: `Some(text)` for a string literal, `None` for any other
    /// expression, which is stepped over. `None` from the outer `Option` is
    /// text that does not end.
    fn value(&mut self) -> Option<Option<String>> {
        if matches!(self.peek()?, '"' | '\'' | '`') {
            let literal = self.literal();
            self.skip_space();
            // `'a' + b` is not the string `a`.
            return match self.peek() {
                Some(',' | '}') => Some(literal),
                Some(_) => {
                    self.skip_expression()?;
                    Some(None)
                }
                None => None,
            };
        }
        self.skip_expression()?;
        Some(None)
    }

    /// Steps over an expression up to the `,` or `}` that ends it.
    fn skip_expression(&mut self) -> Option<()> {
        let mut depth = 0usize;
        loop {
            match self.peek()? {
                '"' | '\'' | '`' => {
                    self.literal_end()?;
                    continue;
                }
                '(' | '[' | '{' => depth += 1,
                ')' | ']' => depth = depth.checked_sub(1)?,
                '}' if depth == 0 => return Some(()),
                '}' => depth -= 1,
                ',' if depth == 0 => return Some(()),
                _ => {}
            }
            self.at += self.peek().map_or(0, char::len_utf8);
        }
    }

    /// Steps over the string literal at the cursor.
    fn literal_end(&mut self) -> Option<()> {
        let quote = self.peek()?;
        self.at += quote.len_utf8();
        loop {
            let c = self.peek()?;
            self.at += c.len_utf8();
            if c == '\\' {
                self.at += self.peek()?.len_utf8();
            } else if c == quote {
                return Some(());
            }
        }
    }

    /// The string literal at the cursor, unescaped. `None` for one that does
    /// not end, uses an escape this does not know, or is a template literal with
    /// a `${}` in it.
    fn literal(&mut self) -> Option<String> {
        let quote = self.peek()?;
        let literal_start = self.at;
        self.literal_end()?;
        let body = &self.text[literal_start + 1..self.at - 1];
        unescape(body, quote)
    }
}

fn unescape(body: &str, quote: char) -> Option<String> {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if quote == '`' && c == '$' && chars.peek() == Some(&'{') {
            return None;
        }
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            '0' if !chars.peek().is_some_and(char::is_ascii_digit) => out.push('\0'),
            '\n' => {}
            c @ ('\\' | '"' | '\'' | '`' | '$' | '/') => out.push(c),
            'x' => {
                let hex: String = (0..2).filter_map(|_| chars.next()).collect();
                out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
            }
            'u' => {
                let code = if chars.peek() == Some(&'{') {
                    chars.next();
                    let hex: String = chars.by_ref().take_while(|c| *c != '}').collect();
                    u32::from_str_radix(&hex, 16).ok()?
                } else {
                    let hex: String = (0..4).filter_map(|_| chars.next()).collect();
                    u32::from_str_radix(&hex, 16).ok()?
                };
                out.push(char::from_u32(code)?);
            }
            _ => return None,
        }
    }
    Some(out)
}

/// The files a patch names. The patch is text inside the script, so its lines
/// are separated by real newlines (a template literal) or by the two characters
/// `\n` (a string literal).
fn patch_paths(input: &str) -> Vec<String> {
    const HEADERS: [&str; 4] = [
        "*** Add File: ",
        "*** Update File: ",
        "*** Delete File: ",
        "*** Move to: ",
    ];
    input
        .split('\n')
        .flat_map(|line| line.split("\\n"))
        .filter_map(|line| {
            let line = line.trim_start();
            let rest = HEADERS.iter().find_map(|h| line.strip_prefix(h))?;
            let path = rest
                .trim()
                .trim_end_matches(['"', '\'', '`', ',', ';', ')']);
            (!path.is_empty()).then(|| path.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands(input: &str) -> Vec<(String, Option<String>)> {
        scan_input(input)
            .commands
            .into_iter()
            .map(|c| (c.cmd, c.workdir))
            .collect()
    }

    #[test]
    fn a_command_is_the_string_literal_in_any_of_the_three_quotes() {
        for (src, want) in [
            (
                r#"await tools.exec_command({cmd:"cargo test",workdir:"/r"})"#,
                "cargo test",
            ),
            (
                "await tools.exec_command({cmd:'cargo test',workdir:\"/r\"})",
                "cargo test",
            ),
            (
                "await tools.exec_command({cmd:`cargo test`,workdir:\"/r\"})",
                "cargo test",
            ),
        ] {
            assert_eq!(
                commands(src),
                vec![(want.to_string(), Some("/r".to_string()))],
                "{src}"
            );
        }
    }

    #[test]
    fn escapes_are_the_javascript_ones() {
        let got = commands(r#"tools.exec_command({cmd:"echo \"a\"\nls",yield_time_ms:1000})"#);
        assert_eq!(got, vec![("echo \"a\"\nls".to_string(), None)]);
        let got = commands(r#"tools.exec_command({cmd:'it\'s A \x42'})"#);
        assert_eq!(got, vec![("it's A B".to_string(), None)]);
    }

    #[test]
    fn a_command_that_is_not_a_literal_is_unread_not_guessed() {
        for src in [
            "tools.exec_command({cmd: command})",
            "tools.exec_command({cmd: 'a' + suffix})",
            "tools.exec_command({cmd: `echo ${x}`})",
            "tools.exec_command(args)",
            "tools.exec_command({workdir: '/r'})",
            "tools.exec_command({cmd: 'unterminated})",
        ] {
            let scanned = scan_input(src);
            assert!(scanned.commands.is_empty(), "{src}");
            assert_eq!(scanned.unread, 1, "{src}");
        }
    }

    #[test]
    fn a_non_literal_value_beside_the_command_is_stepped_over() {
        let got = commands(
            "tools.exec_command({cmd:'ls', env:{A:'}', B:[1,2]}, workdir:\"/w\", n: f(1, 2)})",
        );
        assert_eq!(got, vec![("ls".to_string(), Some("/w".to_string()))]);
    }

    #[test]
    fn patch_headers_are_found_in_both_spellings_of_a_newline() {
        let real = "tools.apply_patch(`*** Begin Patch\n*** Add File: a.rs\n+x\n*** Update File: /abs/b.rs\n*** Move to: c.rs\n*** End Patch`)";
        assert_eq!(patch_paths(real), vec!["a.rs", "/abs/b.rs", "c.rs"]);
        let escaped =
            r#"tools.apply_patch("*** Begin Patch\n*** Delete File: d.rs\n*** End Patch")"#;
        assert_eq!(patch_paths(escaped), vec!["d.rs"]);
    }

    #[test]
    fn a_script_with_too_many_calls_is_read_to_a_bound_and_the_rest_counted() {
        let src = "tools.exec_command({cmd:'ls'});".repeat(MAX_CALLS_PER_EXEC + 10);
        let scanned = scan_input(&src);
        assert_eq!(scanned.commands.len(), MAX_CALLS_PER_EXEC);
        assert_eq!(scanned.unread, 10);
    }

    #[test]
    fn hostile_input_never_panics() {
        for src in [
            "tools.exec_command(",
            "tools.exec_command({",
            "tools.exec_command({cmd",
            "tools.exec_command({cmd:",
            "tools.exec_command({cmd:'\\",
            "tools.exec_command({cmd:'\\u{zz}'})",
            "tools.exec_command({cmd:'\\u{110000}'})",
            "tools.exec_command({cmd:'é', 'é':'é'})",
            "tools.exec_command({]})",
            "tools.exec_command({cmd:'a'\u{0}",
            "tools.apply_patch(",
        ] {
            let _ = scan_input(src);
        }
    }
}
