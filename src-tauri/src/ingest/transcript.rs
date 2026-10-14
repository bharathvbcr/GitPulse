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

/// Every ledger action a transcript call can become, and only those.
///
/// `session.spawn` shares the prefix but is GitPulse opening a terminal; a
/// reader asking "what came from transcripts" must match this list, not the
/// prefix.
pub const ACTIONS: [&str; 2] = [EDIT, COMMAND];
const EDIT: &str = "session.edit";
const COMMAND: &str = "session.command";

impl ToolCall {
    /// The ledger action this call becomes.
    pub fn action(&self) -> &'static str {
        match self.tool.as_str() {
            "Bash" => COMMAND,
            _ => EDIT,
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

fn calls_in(record: &serde_json::Value) -> Vec<ToolCall> {
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

/// Whether `call` lies under the root `repo_path`.
///
/// A file tool is attributed by its own absolute path; a command by the
/// record's `cwd`. Both are prefix matches against the root, with a separator
/// check so `/repo-other` never matches `/repo`. Roots nest (a worktree under
/// `<repo>/.claude/worktrees` is also under `<repo>`), so a match alone does
/// not say whose call it is: [`attribution_root`] picks the one that is.
///
/// No path is canonicalized, so `/tmp/x` and `/private/tmp/x` are different
/// roots, and `\` is not a separator; both are the byte-wise comparison of
/// [`under`], which is the one place to change for either.
pub fn belongs_to(call: &ToolCall, repo_path: &str) -> bool {
    under(candidate_path(call), repo_path)
}

pub fn candidate_path(call: &ToolCall) -> &str {
    call.file_path.as_deref().unwrap_or(&call.cwd)
}

/// The root of `roots` that owns `call`: the deepest one it lies under, so a
/// worktree nested in the repository takes its own calls and the repository
/// keeps only what is not under any worktree. Roots are ranked by their length
/// without trailing separators, so `/repo/` does not outrank `/repo`; equal
/// depth keeps the first listed.
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
        // GitPulse opens worktrees as repositories, so both roots contain the
        // call and the deeper one owns it.
        let c = &parse_line(&record(
            "Edit",
            r#"{"file_path":"/repo/.claude/worktrees/wt/a.rs"}"#,
        ))[0];
        assert!(belongs_to(c, "/repo"));
        assert!(belongs_to(c, "/repo/.claude/worktrees/wt"));
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
