//! GitPulse as an agent hook: the PreToolUse and SessionStart handlers.
//!
//! One executable (`gitpulse-hook`) is dispatched by its first argument, reads
//! the host's hook JSON on stdin and writes hook JSON on stdout. Everything
//! that *decides* anything lives here as a pure function over already-gathered
//! facts, so the contract is testable without spawning a process or standing up
//! a repository; `src/bin/gitpulse-hook.rs` is only the argv/stdin/stdout shell.
//!
//! Protocol, verified against <https://code.claude.com/docs/en/hooks.md>:
//!
//! * Input is snake_case: `hook_event_name`, `session_id`, `cwd`, `tool_name`,
//!   `tool_input`, plus `source` on SessionStart.
//! * Output is camelCase. `hookSpecificOutput` requires `hookEventName`.
//!   `permissionDecision` is one of `allow`, `deny`, `ask` or `defer` — there is
//!   no `waitForApproval` — and `ask` is the value that escalates to a human.
//!   `permissionDecisionReason` is shown to the user for `ask` and to Claude for
//!   `deny`, which is why the collision reason is written for a person and the
//!   refusal is written for the model.
//! * Exit 0, always. Exit 2 blocks the tool call whatever the JSON says, and a
//!   hook that fell over must never be the reason a user's edit is refused.
//!   Empty stdout on exit 0 is the documented "no decision" answer, which lets
//!   the user's own permission rules run untouched.
//!
//! The invariant this module exists to keep is the repository's own, the one
//! `insights` and `harness::policy` are both built around: a check that could
//! not run must never produce the same output as a check that ran and found
//! nothing. Every path here that fails to check something says so in a
//! `systemMessage`, and no path ever emits `allow` — an approval this hook did
//! not earn would silently override the user's own permission rules.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};

use crate::engine::agent_session_slug;
use crate::engine::git_cli::find_git_root;
use crate::harness::{self, PolicyStatus, ScopedVerdict};
use crate::insights::{self, CollisionRisk, InsightsSnapshot};

/// Wall-clock ceiling for the work behind one hook invocation.
///
/// A PreToolUse hook runs on *every* matching tool call, so the cost of this
/// binary is paid by the user's editor latency. The host's own `timeout` is a
/// weaker backstop than it looks: the docs are explicit that a timed-out
/// PreToolUse command hook has its output discarded and does not block the
/// call, so relying on it would turn a slow repository into a silent skipped
/// check — exactly the failure this module exists to prevent. Giving up here
/// instead lets us give up *loudly*.
pub const BUDGET: Duration = Duration::from_secs(5);

pub const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;

/// Read one complete JSON document, including pretty-printed input. This is
/// for the one-shot hook executable: on timeout it exits and tears down the
/// single reader that may still be waiting for the host to close stdin.
pub fn read_input(reader: impl std::io::Read + Send + 'static) -> Result<HookInput, String> {
    within_budget(BUDGET, move || {
        let mut bytes = Vec::new();
        reader
            .take(MAX_INPUT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("could not read hook stdin: {e}"))?;
        if bytes.len() > MAX_INPUT_BYTES {
            return Err("hook stdin exceeded its 4 MiB limit".into());
        }
        let input =
            std::str::from_utf8(&bytes).map_err(|e| format!("hook stdin is not UTF-8: {e}"))?;
        parse_input(input)
    })
    .ok_or_else(|| {
        "hook stdin deadline exceeded or reader could not run; no decision".to_string()
    })?
}

/// Hard cap on the SessionStart brief, in bytes.
///
/// The host caps hook strings at 10,000 characters and spills the rest to a
/// file, but a brief that needs anywhere near that is not a brief. Past this we
/// cut and mark the cut; a silently shortened brief would be a snapshot that
/// looks complete and is not.
pub const BRIEF_LIMIT: usize = 2000;

/* ── Wire types ───────────────────────────────────────────────────────────── */

/// The subset of the host's hook JSON these three handlers read.
///
/// Parsed field by field out of a `Value` rather than derived, because this
/// struct is filled from a foreign process's stdout: a `tool_input` that is a
/// string instead of an object, or a `cwd` that is a number, must degrade to an
/// empty field and a reported non-check, never to a failed parse of the whole
/// payload or a panic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookInput {
    pub hook_event_name: String,
    pub session_id: String,
    pub cwd: String,
    pub tool_name: String,
    /// `tool_input.file_path` (Edit/Write) or `tool_input.notebook_path`.
    pub file_path: String,
    /// `tool_input.command` (Bash).
    pub command: String,
    /// SessionStart's `source`: startup, resume, clear, compact or fork.
    pub source: String,
    /// Cursor native payloads carry a non-null `cursor_version`. Claude
    /// and Codex do not. The stdout schema splits on this, not on event-name
    /// casing: camelCase `sessionStart` is not a host signal.
    pub is_cursor: bool,
    /// The host's own wording for a `Notification` event (`message`).
    ///
    /// Never required. The reason GitPulse shows comes from the matcher the
    /// host routed through — the argument `notify` is given — and this is only
    /// what the banner adds after it.
    pub message: String,
    /// Which tool call a `PermissionRequest` or `PostToolUse` is about, as a
    /// digest of `tool_name` and `tool_input`. Empty without a tool. The same
    /// call digests the same way in both events, which is how the result of
    /// one call clears the request for that call and no other.
    pub tool_subject: String,
    /// `tool_input`, said in a line: `Bash: cargo test`.
    pub tool_summary: String,
    /// Set inside a subagent (`agent_id`).
    pub agent_id: String,
    /// `Stop`'s `last_assistant_message`: what the agent ended its turn on.
    pub last_assistant_message: String,
    /// `StopFailure`'s `error`: `rate_limit`, `overloaded`, ….
    pub error: String,
}

impl HookInput {
    /// Reads the fields we use out of one already-parsed hook payload.
    pub fn from_value(value: &Value) -> Self {
        let tool_input = value.get("tool_input");
        // `file_path` is the documented field for Write and Edit and is always
        // absolute. `notebook_path` is NotebookEdit's, checked against that
        // tool's own published input schema rather than the hook reference's
        // `tool_input` table, which does not list it. Should either name ever
        // move we derive no path and report a non-check, which is the safe
        // direction to be wrong in.
        let file_path = string_at(tool_input, "file_path");
        let file_path = if file_path.is_empty() {
            string_at(tool_input, "notebook_path")
        } else {
            file_path
        };
        let cwd = string_at(Some(value), "cwd");
        let cwd = if cwd.is_empty() {
            first_workspace_root(value)
        } else {
            cwd
        };
        HookInput {
            hook_event_name: string_at(Some(value), "hook_event_name"),
            session_id: string_at(Some(value), "session_id"),
            cwd,
            tool_name: string_at(Some(value), "tool_name"),
            file_path,
            command: string_at(tool_input, "command"),
            source: string_at(Some(value), "source"),
            is_cursor: is_cursor_payload(value),
            message: string_at(Some(value), "message"),
            tool_subject: tool_subject(value),
            tool_summary: tool_summary(value),
            agent_id: string_at(Some(value), "agent_id"),
            last_assistant_message: string_at(Some(value), "last_assistant_message"),
            error: string_at(Some(value), "error"),
        }
    }
}

/// Writes `value` with every object's keys in sorted order, so one tool call
/// digests the same way whatever order a host serialized its input in.
fn canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                canonical(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// The digest naming one tool call: FNV-1a over the tool's name and its
/// canonical input. Not a security boundary — a collision would clear one
/// waiting request early, and the peer able to cause it is the same user —
/// so the cheapest stable hash that needs no dependency is the right one.
fn tool_subject(value: &Value) -> String {
    let name = string_at(Some(value), "tool_name");
    if name.is_empty() {
        return String::new();
    }
    let mut text = name;
    text.push('\0');
    if let Some(input) = value.get("tool_input") {
        canonical(input, &mut text);
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The longest tool summary a report carries.
const TOOL_SUMMARY_CHARS: usize = 160;

/// `tool_input`, said in a line, for the board: the tool and the one field a
/// person would read to decide — a command, a path, a URL.
fn tool_summary(value: &Value) -> String {
    let name = string_at(Some(value), "tool_name");
    if name.is_empty() {
        return String::new();
    }
    let input = value.get("tool_input");
    let what = [
        "command",
        "file_path",
        "notebook_path",
        "url",
        "pattern",
        "query",
        "description",
        "prompt",
    ]
    .into_iter()
    .map(|key| string_at(input, key))
    .find(|text| !text.trim().is_empty());
    let line = match what {
        Some(text) => format!("{name}: {}", text.lines().next().unwrap_or_default().trim()),
        None => name,
    };
    line.chars().take(TOOL_SUMMARY_CHARS).collect()
}

/// Cursor native payloads name `cursor_version`. Event-name casing is not a
/// host signal: Claude plugins on Cursor still send camelCase `sessionStart`.
fn is_cursor_payload(value: &Value) -> bool {
    value
        .get("cursor_version")
        .is_some_and(|version| !version.is_null())
}

/// How many `workspace_roots` entries we will examine. Cursor's array is
/// unbounded; a pathological payload must not turn SessionStart into a
/// walk of a million empty strings.
const MAX_WORKSPACE_ROOTS: usize = 256;

/// Cursor native sessionStart sends `workspace_roots` (and often no `cwd`).
/// The first non-empty string is the repository we brief; everything else is
/// ignored. Hostile shapes (a string, an object, numbers) yield empty, which
/// is the same answer as "the host told us nothing".
fn first_workspace_root(value: &Value) -> String {
    value
        .get("workspace_roots")
        .or_else(|| value.get("workspaceRoots"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_WORKSPACE_ROOTS)
        .filter_map(Value::as_str)
        .map(str::trim)
        .find(|entry| !entry.is_empty())
        .unwrap_or("")
        .to_string()
}

/// A non-string (or absent) field reads as empty rather than failing the parse.
fn string_at(parent: Option<&Value>, key: &str) -> String {
    parent
        .and_then(|value| value.get(key))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Parses the hook payload the host wrote to our stdin.
///
/// Refuse oversized or invalid documents before interpreting fields; other
/// shape surprises are absorbed by [`HookInput::from_value`].
pub fn parse_input(stdin: &str) -> Result<HookInput, String> {
    if stdin.len() > MAX_INPUT_BYTES {
        return Err("hook stdin exceeded its 4 MiB limit".into());
    }
    let value: Value = serde_json::from_str(stdin.trim())
        .map_err(|e| format!("hook stdin is not valid JSON: {e}"))?;
    if !value.is_object() {
        return Err("hook stdin is not a JSON object".to_string());
    }
    Ok(HookInput::from_value(&value))
}

/// The permission decisions this hook is willing to make.
///
/// The protocol also defines `allow` and `defer`. Neither is modelled: `allow`
/// would override the user's own permission rules with an approval GitPulse has
/// no standing to give, and `defer` parks the tool call for later resumption,
/// which is not something a repository check should be doing to someone's edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionDecision {
    /// Refuse the call. The reason is shown to Claude.
    Deny,
    /// Escalate to the user. The reason is shown to the user, not to Claude.
    Ask,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookSpecificOutput {
    pub hook_event_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_decision: Option<PermissionDecision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_decision_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
}

/// One hook's answer.
///
/// `continue` and `suppressOutput` are deliberately absent: `continue: false`
/// halts the whole turn, which no read-only repository check should be able to
/// do, and the docs say `suppressOutput` has no effect at all.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_specific_output: Option<HookSpecificOutput>,
    /// Shown to the user as a warning. This is the channel every "the check did
    /// not run" notice goes out on, because it reaches a human without
    /// pretending to be a decision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_message: Option<String>,
}

impl HookOutput {
    /// Nothing to say: the host's normal permission flow applies untouched.
    pub fn silent() -> Self {
        HookOutput::default()
    }

    /// A warning for the user with no decision attached.
    pub fn notice(message: impl Into<String>) -> Self {
        HookOutput {
            hook_specific_output: None,
            system_message: Some(message.into()),
        }
    }

    pub fn is_silent(&self) -> bool {
        self.hook_specific_output.is_none() && self.system_message.is_none()
    }

    /// The bytes to put on stdout, or `None` when the answer is "no decision".
    ///
    /// Printing nothing is the documented no-decision answer and is safer than
    /// printing an empty object: stdout is the protocol channel, and the less
    /// that goes down it when we have nothing to say, the fewer ways there are
    /// to be misread.
    pub fn render(&self) -> Option<String> {
        if self.is_silent() {
            return None;
        }
        serde_json::to_string(self).ok()
    }

    /// Host-specific stdout. Claude pastes Claude nested JSON; Cursor native
    /// sessionStart injects only top-level `additional_context`, and Cursor
    /// `preToolUse` is a permission hook whose schema is not Claude's — a
    /// nested `permissionDecision` there blocks the tool.
    pub fn render_for_host(&self, is_cursor: bool) -> Option<String> {
        if is_cursor {
            return self.render_cursor();
        }
        self.render()
    }

    fn render_cursor(&self) -> Option<String> {
        if let Some(specific) = &self.hook_specific_output {
            if specific.permission_decision.is_some() {
                return None;
            }
            if let Some(ctx) = &specific.additional_context {
                return Some(json!({ "additional_context": ctx }).to_string());
            }
        }
        self.render()
    }
}

fn decision(event: &str, decision: PermissionDecision, reason: String) -> HookSpecificOutput {
    HookSpecificOutput {
        hook_event_name: event.to_string(),
        permission_decision: Some(decision),
        permission_decision_reason: Some(reason),
        additional_context: None,
    }
}

/* ── 1. collision-guard: PreToolUse on the file-editing tools ─────────────── */

pub const PRE_TOOL_USE: &str = "PreToolUse";
pub const SESSION_START: &str = "SessionStart";

/// One other worktree holding the same path dirty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollisionOther {
    pub worktree: String,
    pub branch: String,
    /// The agent session slug when the worktree is an agent checkout.
    pub session: String,
    pub agent_kind: String,
}

/// What the collision scan established about one file.
///
/// Shaped after the facets in [`crate::insights`], for the same reason they are
/// shaped that way: `ok`/`error` record whether the check *ran*, and are never
/// inferred from the absence of findings. `partial` is the third state those
/// facets carry as `truncated`/`unscanned_worktrees` — the scan ran, but not
/// over everything, so a negative result is not evidence of absence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CollisionFacts {
    /// True only when the scan ran. Never derived from `others` being empty.
    pub ok: bool,
    /// Empty when `ok`; otherwise why the check could not run.
    pub error: String,
    /// The repo-relative path judged. Empty when none could be derived.
    pub target: String,
    /// Worktrees other than this one with uncommitted changes to `target`.
    pub others: Vec<CollisionOther>,
    /// Empty when the scan covered every worktree and kept every row;
    /// otherwise what it missed.
    pub partial: String,
    /// Symbol classification for `target`, when the collision payload carried
    /// one. Absence means the notice stays at today's file-level wording.
    pub entity: Option<crate::insights::EntityCollisionVerdict>,
}

/// The collision-guard contract, over facts someone else gathered.
///
/// Three outcomes, and the whole point is that they are three and not two:
///
/// 1. a contended file escalates to the user with `ask`;
/// 2. a clean, complete check says nothing at all, so the user's own permission
///    rules decide;
/// 3. a check that could not run, or could only half run, also renders no
///    decision — but says so out loud. Silence here would be indistinguishable
///    from (2), which is the failure this whole module is built to avoid.
pub fn collision_decision(facts: &CollisionFacts) -> HookOutput {
    if !facts.ok {
        return HookOutput::notice(format!(
            "GitPulse collision check did NOT run for this edit: {}. \
             Other worktrees may hold uncommitted changes to this file.",
            clause(&facts.error)
        ));
    }

    if facts.others.is_empty() {
        // A partial scan that found nothing has not established anything. Say
        // which half of the check is missing rather than implying a clean pass.
        if !facts.partial.is_empty() {
            return HookOutput::notice(format!(
                "GitPulse collision check was INCOMPLETE for {}: {}. \
                 Nothing was found, but not everything was looked at.",
                or_unknown(&facts.target),
                clause(&facts.partial)
            ));
        }
        return HookOutput::silent();
    }

    // A positive finding stands on its own evidence, so it is reported even
    // when the sweep was partial — a partial scan can miss a collision, but it
    // cannot invent one.
    let mut reason = format!(
        "GitPulse: {} already has uncommitted changes in {} other worktree{} of this repository.\n",
        or_unknown(&facts.target),
        facts.others.len(),
        if facts.others.len() == 1 { "" } else { "s" }
    );
    for other in &facts.others {
        reason.push_str(&format!("  - {}", other.worktree));
        if !other.branch.is_empty() {
            reason.push_str(&format!(" [branch {}]", other.branch));
        }
        if !other.session.is_empty() {
            reason.push_str(&format!(" [session {}]", other.session));
        } else if !other.agent_kind.is_empty() {
            reason.push_str(&format!(" [agent {}]", other.agent_kind));
        }
        reason.push('\n');
    }
    reason.push_str(&entity_collision_tail(facts));
    if !facts.partial.is_empty() {
        reason.push_str(&format!("\n(Scan was incomplete: {}.)", facts.partial));
    }

    HookOutput {
        hook_specific_output: Some(decision(PRE_TOOL_USE, PermissionDecision::Ask, reason)),
        system_message: None,
    }
}

/// Closing sentence for a positive collision finding.
///
/// Shared-symbol and file-level keep the historical "will conflict" wording.
/// Disjoint symbols are a distinct notice — file overlap is not a merge
/// promise — and must never borrow the collision sentence.
fn entity_collision_tail(facts: &CollisionFacts) -> String {
    match facts.entity.as_ref() {
        Some(entity)
            if matches!(
                entity.kind,
                crate::insights::EntityCollisionKind::DisjointSymbols
            ) =>
        {
            format!(
                "Same file, distinct symbols on the old-side ranges — file overlap, not a merge promise. {}",
                entity.reason
            )
        }
        Some(entity)
            if matches!(entity.kind, crate::insights::EntityCollisionKind::FileLevel)
                && !entity.reason.is_empty() =>
        {
            format!(
                "Editing it here will conflict when these branches meet. (Symbol check stayed at file level: {}.)",
                entity.reason
            )
        }
        Some(entity)
            if matches!(
                entity.kind,
                crate::insights::EntityCollisionKind::SharedSymbol
            ) =>
        {
            format!("{}.", entity.reason.trim_end_matches('.'))
        }
        _ => "Editing it here will conflict when these branches meet.".into(),
    }
}

/// Turns one `CollisionRisk` payload into the facts for one path.
///
/// Split out from the gathering so the mapping — including which worktree
/// counts as "this one" and what makes the sweep partial — is testable without
/// a repository.
pub fn collision_facts(
    risk: &CollisionRisk,
    here: &Path,
    target: &str,
    sessions: &dyn Fn(&str) -> String,
) -> CollisionFacts {
    // Nothing read means nothing established. `CollisionRisk::ok` is a stricter
    // claim than that — it is false as soon as a single worktree failed — so it
    // is the wrong line to draw here: a sweep that read fifteen worktrees and
    // failed on the sixteenth did real work, and any collision it *did* find is
    // still true. `scanned_worktrees` is the field that separates "we looked at
    // nothing" from "we looked at most of it".
    if risk.scanned_worktrees == 0 {
        return CollisionFacts {
            ok: false,
            error: or_unknown(&risk.error),
            target: target.to_string(),
            others: Vec::new(),
            partial: String::new(),
            entity: None,
        };
    }

    let matched = risk.items.iter().find(|item| item.path == target);
    let entity = matched.and_then(|item| item.entity.clone());
    let others = matched
        .map(|item| {
            item.worktrees
                .iter()
                .filter(|party| !same_path(Path::new(&party.path), here))
                .map(|party| CollisionOther {
                    worktree: party.path.clone(),
                    branch: party.branch.clone().unwrap_or_default(),
                    session: sessions(&party.path),
                    agent_kind: party.agent_kind.clone(),
                })
                .collect()
        })
        .unwrap_or_default();

    // Everything that makes a *negative* answer unreliable, each of it already
    // counted by `insights` rather than guessed at here. A worktree that was
    // never read cannot have contributed this path, and a row list cut at its
    // cap may have dropped it — so in either case "no collision" is a statement
    // about what we saw, not about the repository.
    let mut partial = Vec::new();
    if risk.failed_worktrees > 0 {
        // `CollisionRisk::error` is the *first* failure, not a summary of all
        // of them, so above one it has to be labelled as one case and not read
        // as the shared cause. Four worktrees refused for trust and a fifth
        // lost to a bad disk is a different situation from five of either, and
        // the reader cannot tell them apart from a count and one message.
        partial.push(if risk.failed_worktrees > 1 {
            format!(
                "{} worktree(s) could not be read (first of {}: {})",
                risk.failed_worktrees,
                risk.failed_worktrees,
                or_unknown(&risk.error)
            )
        } else {
            format!(
                "1 worktree(s) could not be read ({})",
                or_unknown(&risk.error)
            )
        });
    }
    if risk.unscanned_worktrees > 0 {
        partial.push(format!(
            "{} worktree(s) were not scanned",
            risk.unscanned_worktrees
        ));
    }
    if risk.truncated && risk.failed_worktrees == 0 && risk.unscanned_worktrees == 0 {
        partial.push("the overlap list was truncated".to_string());
    }
    // A facet that says it is not ok while naming no cause still must not read
    // as complete; fall back to its own error rather than inventing coverage.
    if !risk.ok && partial.is_empty() {
        partial.push(format!(
            "the scan reported a failure: {}",
            or_unknown(&risk.error)
        ));
    }

    CollisionFacts {
        ok: true,
        error: String::new(),
        target: target.to_string(),
        others,
        partial: partial.join("; "),
        entity,
    }
}

/// Gathers, then decides. The impure half of collision-guard.
pub fn run_collision_guard(input: &HookInput) -> HookOutput {
    if input.file_path.is_empty() {
        return HookOutput::notice(
            "GitPulse collision check did NOT run: the tool call carried no file path.",
        );
    }
    if input.cwd.is_empty() {
        return unchecked(&input.file_path, "the hook payload carried no cwd");
    }

    let Some(root) = find_git_root(Path::new(&input.cwd)) else {
        return unchecked(
            &input.file_path,
            &format!("{} is not inside a Git repository", input.cwd),
        );
    };

    let file = canonical_enough(Path::new(&input.file_path));
    let Some(target) = repo_relative(&root, &file) else {
        return unchecked(
            &input.file_path,
            &format!("the path is not inside the worktree at {}", root.display()),
        );
    };

    let root_arg = root.to_string_lossy().into_owned();
    let scanned = within_budget(BUDGET, move || insights::collision_risk(&root_arg));
    let Some(risk) = scanned else {
        return unchecked(
            &target,
            &format!("the scan did not finish within {}s", BUDGET.as_secs()),
        );
    };

    let facts = collision_facts(&risk, &root, &target, &|path| {
        agent_session_slug(path).unwrap_or_default()
    });
    log::debug!(
        target: "hooks",
        "collision guard: target={} ok={} others={} partial={} scanned={}",
        facts.target,
        facts.ok,
        facts.others.len(),
        facts.partial,
        risk.scanned_worktrees,
    );
    collision_decision(&facts)
}

/// The "this was not checked" notice, in one place so every caller words it the
/// same way and none of them can accidentally fall silent instead.
fn unchecked(target: &str, why: &str) -> HookOutput {
    collision_decision(&CollisionFacts {
        ok: false,
        error: why.to_string(),
        target: target.to_string(),
        others: Vec::new(),
        partial: String::new(),
        entity: None,
    })
}

/* ── 2. command-gate: PreToolUse on Bash ──────────────────────────────────── */

/// The command-gate contract, over a verdict the harness already returned.
///
/// This mirrors [`crate::harness::guard_command`]'s seam exactly: a blocking
/// verdict refuses, and every way of *not* being judged is reported rather than
/// rendered as an approval. The distinction `PolicyVerdict::gate_failed` draws
/// is kept intact here — "no harness on this machine" is a standing condition
/// the user chose, while "the harness is installed and could not answer" is
/// transient and self-inflicted — because they read very differently to
/// somebody deciding whether to trust the next command.
pub(crate) fn command_gate_decision(judged: &ScopedVerdict) -> HookOutput {
    let verdict = &judged.verdict;
    // Ahead of the refusal: a block that only says "I could not read this line"
    // is not judgement, and in an unbound checkout it is not this hook's to
    // enforce. It is not rendered as an approval either — no decision is taken,
    // so the host's own permission rules run, and the note says the gate did
    // not judge it. `ask` was considered and refused: in a `claude -p` run,
    // which is how GitPulse launches agents, the host turns `ask` into a deny,
    // so it would have softened nothing where it matters most.
    if !judged.bound && verdict.is_unreadable_construct() {
        let note = format!(
            "GitPulse could not judge this command [{}]: {}. It was left to your own \
             permission rules.",
            verdict.rule,
            clause(&verdict.reason)
        );
        return HookOutput {
            hook_specific_output: Some(HookSpecificOutput {
                hook_event_name: PRE_TOOL_USE.to_string(),
                permission_decision: None,
                permission_decision_reason: None,
                // For the model: it is what can choose a form the gate reads
                // (paths from the repository root, no `cd`), so the next
                // command is judged rather than waved through again.
                additional_context: Some(format!(
                    "{note} To have GitPulse judge commands, run them from the repository \
                     root with root-relative paths instead of changing directory, and write \
                     file contents with the editing tools rather than heredocs."
                )),
            }),
            system_message: Some(note),
        };
    }
    if verdict.blocks() {
        return HookOutput {
            hook_specific_output: Some(decision(
                PRE_TOOL_USE,
                PermissionDecision::Deny,
                verdict.refusal(),
            )),
            system_message: None,
        };
    }

    if verdict.gate_failed() {
        return HookOutput::notice(format!(
            "{}\nThis command ran UNGATED.",
            verdict.gate_failure()
        ));
    }

    match verdict.status {
        // Reached only when `gate_failed` said "not_installed": no harness on
        // this machine, which is documented, permanent, and still not a pass.
        PolicyStatus::Unchecked => HookOutput::notice(
            "No MANVI harness is installed, so GitPulse could not judge this command. \
             It ran UNGATED."
                .to_string(),
        ),
        // A rung fired and allowed with a note. The note is the whole value of
        // the rung; swallowing it would make a warned command look clean.
        PolicyStatus::Warned => HookOutput::notice(format!(
            "MANVI harness warning [{}]: {}",
            or_unknown(&verdict.rule),
            or_unknown(&verdict.reason)
        )),
        // Allowed, but some rungs could not run at all — the literal case this
        // repository's invariant is about, so it is surfaced even though the
        // command proceeds.
        PolicyStatus::Degraded => HookOutput::notice(format!(
            "The MANVI harness allowed this command with checks it could not run: {}.",
            verdict.degraded.join(", ")
        )),
        // Demoted, Granted and Widened are allows that something deliberately
        // waived, and they are left silent on purpose.
        //
        // Measured, rather than reasoned about: against `manvi serve` at
        // posture=host, every command outside the global allowlist comes back
        // `Demoted` on `command.not_allowed`, waived by
        // `serve.posture=host: allowlist not enforced (enforce_allowlist=false)`.
        // That is the standing configuration of this posture, not a fact about
        // the command — `git status` allows cleanly, `npm test` and `rm -rf /`
        // both demote — so it fires on nearly every Bash call a session makes.
        // Reporting it each time would be noise that trains the reader to
        // ignore this channel, and this channel is where the *real*
        // non-checks are announced.
        //
        // A checkout bound to a task sends its scope (`run_command_gate`), and
        // a scope violation is not demoted, so it refuses above. What a scope
        // reaches is the command line's *redirection targets*: Manvi does not
        // read a write out of a command's arguments, so `sed -i` on a file
        // outside the plan still comes back here as a demoted allow.
        PolicyStatus::Allowed
        | PolicyStatus::Demoted
        | PolicyStatus::Granted
        | PolicyStatus::Widened => HookOutput::silent(),
        PolicyStatus::Blocked => HookOutput::silent(),
    }
}

/// Gathers, then decides. The impure half of command-gate.
pub fn run_command_gate(input: &HookInput) -> HookOutput {
    command_gate_with(input, |job| within_budget(BUDGET, job))
}

/// One harness judgement, built on the caller's thread and run on whichever
/// thread `run` chooses.
type CommandJob = Box<dyn FnOnce() -> ScopedVerdict + Send>;

/// [`run_command_gate`] with the budgeted runner injected. The tests run the
/// job on their own thread, because the sidecar's test serial guard is
/// reentrant only on the thread that holds it — the budget's worker thread
/// would wait on it until the budget expired.
fn command_gate_with(
    input: &HookInput,
    run: impl FnOnce(CommandJob) -> Option<ScopedVerdict>,
) -> HookOutput {
    if input.command.is_empty() {
        return HookOutput::notice(
            "GitPulse did not gate this call: the tool input carried no command.",
        );
    }
    if input.cwd.is_empty() {
        return HookOutput::notice(
            "GitPulse could not gate this command: the hook payload carried no cwd. \
             It ran UNGATED.",
        );
    }
    let command = input.command.clone();
    // A checkout bound to a task is judged against that task's scope, resolved
    // by the same owner `harness::guard_command` uses — so a redirection into a
    // file outside the plan reaches `scope.unplanned`, which Manvi does not
    // demote once a scope is declared. Resolving it opens the ledger and the
    // task store, so it runs inside the budget the host's timeout allows.
    //
    // Outside a Git repository there is no checkout to be bound, and asking the
    // ledger about a bare directory would fail closed — refusing every command
    // an agent runs anywhere else. That case is judged with no scope, as it
    // always was.
    let job: CommandJob = match find_git_root(Path::new(&input.cwd)) {
        Some(root) => {
            let root = root.to_string_lossy().into_owned();
            Box::new(move || harness::check_command_in_scope(&root, &command))
        }
        None => {
            let root = input.cwd.clone();
            Box::new(move || ScopedVerdict {
                verdict: harness::check_command(&root, &command, None),
                bound: false,
            })
        }
    };
    let judged = run(job);
    if let Some(ScopedVerdict { verdict, bound }) = judged.as_ref() {
        // The verdict is the only record of why this hook stayed silent, and
        // silence is its most common answer. Without this line an operator
        // cannot tell a clean allow from a demoted one, which is the same
        // confusion the rest of this module exists to prevent.
        log::debug!(
            target: "hooks",
            "command gate: status={:?} checked={} rule={} detail_code={} demoted={} degraded={:?} bound={}",
            verdict.status,
            verdict.checked,
            verdict.rule,
            verdict.detail_code,
            verdict.demoted,
            verdict.degraded,
            bound,
        );
    }
    let Some(judged) = judged else {
        return HookOutput::notice(format!(
            "The MANVI harness did not answer within {}s, so GitPulse could not judge \
             this command. It ran UNGATED.",
            BUDGET.as_secs()
        ));
    };
    command_gate_decision(&judged)
}

/* ── 3. session-brief: SessionStart ───────────────────────────────────────── */

/// The SessionStart brief, over a snapshot someone else gathered.
///
/// `here` is the worktree the session actually started in, which is not
/// necessarily the one `snapshot.branch` describes — that field reports the
/// *main* worktree's branch, and an agent session almost always starts in a
/// linked one. Both are labelled rather than conflated.
///
/// A facet that failed is printed as failed. Printing `0 files changed` for a
/// `git status` that never ran would be the same lie this module refuses to
/// tell about collisions, one line further down the page.
pub fn session_brief(snapshot: &InsightsSnapshot, here: &str, source: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "GitPulse brief ({})\nrepo: {}\n",
        if source.is_empty() {
            "session start"
        } else {
            source
        },
        snapshot.repo_path
    ));
    // Printed first because it qualifies everything under it: past the
    // snapshot deadline the expensive stages stop, and the facets they would
    // have filled are partial rather than empty.
    if snapshot.deadline_expired {
        out.push_str("NOTE: the scan hit its deadline; the facets below are partial.\n");
    }

    let mine = snapshot
        .worktrees
        .items
        .iter()
        .find(|w| same_path(Path::new(&w.path), Path::new(here)));
    match mine {
        Some(w) => out.push_str(&format!(
            "branch here: {}{}\n",
            w.branch.clone().unwrap_or_else(|| "(detached)".into()),
            if w.is_main { " (main worktree)" } else { "" }
        )),
        None if snapshot.worktrees.ok => {
            out.push_str("branch here: unknown (worktree not listed)\n")
        }
        None => {}
    }
    if mine.map(|w| !w.is_main).unwrap_or(true) {
        // `branch: None` is two different facts, and the new `branch_ok` is
        // what tells them apart: detached/bare when the lookup ran, and
        // "nobody could tell" when it did not.
        match (&snapshot.branch, snapshot.branch_ok) {
            (Some(branch), _) => out.push_str(&format!("main worktree branch: {branch}\n")),
            (None, true) => out.push_str("main worktree branch: detached or bare\n"),
            (None, false) => out.push_str("main worktree branch: UNKNOWN (not established)\n"),
        }
    }

    if snapshot.changes.ok {
        let c = &snapshot.changes;
        out.push_str(&format!(
            "uncommitted here: {} file(s) ({} staged, {} unstaged, {} untracked, {} conflicted){}\n",
            c.files,
            c.staged,
            c.unstaged,
            c.untracked,
            c.conflicted,
            if c.truncated { " [list truncated]" } else { "" }
        ));
    } else {
        out.push_str(&format!(
            "uncommitted here: UNAVAILABLE ({})\n",
            or_unknown(&snapshot.changes.error)
        ));
    }

    if snapshot.worktrees.ok {
        out.push_str(&format!(
            "worktrees: {} total, {} of {} scanned, {} with uncommitted work{}{}\n",
            snapshot.worktrees.count,
            snapshot.worktrees.scanned,
            snapshot.worktrees.count,
            snapshot.worktrees.dirty,
            // A worktree nobody measured is not a clean worktree. Saying how
            // many went unmeasured is what stops `dirty` reading as a total.
            if snapshot.worktrees.dirty_unknown > 0 {
                format!(", {} unmeasured", snapshot.worktrees.dirty_unknown)
            } else {
                String::new()
            },
            if snapshot.worktrees.truncated {
                " [list truncated]"
            } else {
                ""
            }
        ));
        for w in snapshot
            .worktrees
            .items
            .iter()
            .filter(|w| w.dirty_files.unwrap_or(0) > 0)
            .filter(|w| !same_path(Path::new(&w.path), Path::new(here)))
        {
            out.push_str(&format!(
                "  - {} [{}] {} dirty{}\n",
                w.name,
                w.branch.clone().unwrap_or_else(|| "detached".into()),
                w.dirty_files.unwrap_or(0),
                if w.session_slug.is_empty() {
                    String::new()
                } else {
                    format!(" (session {})", w.session_slug)
                }
            ));
        }
    } else {
        out.push_str(&format!(
            "worktrees: UNAVAILABLE ({})\n",
            or_unknown(&snapshot.worktrees.error)
        ));
    }

    if snapshot.collisions.ok {
        out.push_str(&format!(
            "collisions: {} file(s) contended across {} worktree(s){}\n",
            snapshot.collisions.overlapping_files,
            snapshot.collisions.worktrees_involved,
            if snapshot.collisions.truncated
                || snapshot.collisions.unscanned_worktrees > 0
                || snapshot.collisions.failed_worktrees > 0
            {
                " [partial scan]"
            } else {
                ""
            }
        ));
        for item in snapshot.collisions.items.iter().take(5) {
            out.push_str(&format!("  - {}\n", item.path));
        }
    } else {
        out.push_str(&format!(
            "collisions: UNAVAILABLE ({})\n",
            or_unknown(&snapshot.collisions.error)
        ));
    }
    out.push_str(&live_sessions_lines(&snapshot.agents.live, here));
    for shared in &snapshot.collisions.shared_worktrees {
        out.push_str(&format!(
            "  ! {} live sessions share {}: {}\n",
            shared.sessions.len(),
            shared.path,
            if shared.scanned {
                format!(
                    "{} dirty file(s) open to all of them{}",
                    shared.files.len(),
                    if shared.truncated {
                        " [list truncated]"
                    } else {
                        ""
                    }
                )
            } else {
                "its dirty files were NOT scanned".to_string()
            }
        ));
    }

    out.push_str(&format!(
        "ledger: {}\n",
        if snapshot.ledger.recording {
            let dropped = snapshot.ledger.dropped;
            if dropped > 0 {
                format!("recording ({dropped} append(s) dropped — history incomplete)")
            } else {
                "recording".to_string()
            }
        } else {
            format!("NOT recording ({})", or_unknown(&snapshot.ledger.error))
        }
    ));

    out.push_str(&format!(
        "code graph: {}\n",
        if snapshot.codeintel.available {
            let freshness = match snapshot.codeintel.is_fresh {
                Some(true) => "fresh at status check".to_string(),
                Some(false) => format!(
                    "freshness not established ({})",
                    or_unknown(snapshot.codeintel.freshness_reason.as_deref().unwrap_or(""))
                ),
                None => "freshness UNVERIFIED".to_string(),
            };
            format!(
                "{} symbols across {} files; {freshness}",
                snapshot.codeintel.total_symbols.unwrap_or(0),
                snapshot.codeintel.total_files.unwrap_or(0)
            )
        } else {
            format!(
                "UNAVAILABLE ({})",
                or_unknown(snapshot.codeintel.reason.as_deref().unwrap_or(""))
            )
        }
    ));

    truncate_marked(out, BRIEF_LIMIT)
}

/// Gathers, then renders. The impure half of session-brief.
pub fn run_session_brief(input: &HookInput) -> HookOutput {
    if input.cwd.is_empty() {
        return HookOutput::notice("GitPulse could not brief this session: no cwd was supplied.");
    }
    let Some(root) = find_git_root(Path::new(&input.cwd)) else {
        // Not an error worth warning a user about: plenty of sessions start
        // outside a repository, and GitPulse has nothing to say about those.
        return HookOutput::silent();
    };
    let here = root.to_string_lossy().into_owned();
    let arg = here.clone();
    let Some(snapshot) = within_budget(BUDGET, move || insights::snapshot(&arg)) else {
        return HookOutput::notice(format!(
            "GitPulse could not read this repository within {}s, so this session starts \
             without a repository brief.",
            BUDGET.as_secs()
        ));
    };
    HookOutput {
        hook_specific_output: Some(HookSpecificOutput {
            hook_event_name: SESSION_START.to_string(),
            permission_decision: None,
            permission_decision_reason: None,
            additional_context: Some(session_brief(&snapshot, &here, &input.source)),
        }),
        system_message: None,
    }
}

/* ── Dispatch ─────────────────────────────────────────────────────────────── */

/// Every subcommand `gitpulse-hook` answers to.
pub const SUBCOMMANDS: [&str; 4] = ["collision-guard", "command-gate", "session-brief", "notify"];

/// The arguments that ask this binary who it is instead of running a hook.
pub const IDENTITY_FLAGS: [&str; 2] = ["--version", "-V"];

/// This build's version, the same string `gitpulse-mcp` reports in its
/// handshake.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What `gitpulse-hook --version` prints.
///
/// `gitpulse-mcp` can be asked what it is over its own protocol, which is what
/// lets `check-mcp-install` tell a stale server from a current one. A hook
/// binary had no such channel: a host only ever spawns it with a subcommand, an
/// unknown subcommand is answered with silence by design, and a hook that
/// cannot start at all is reported by the host as a non-blocking error while
/// the tool call proceeds. So an out-of-date — or entirely absent —
/// `gitpulse-hook` looked exactly like one that ran and found nothing, which is
/// the confusion the rest of this module exists to prevent, one layer below
/// where any of its code can see it.
///
/// Two lines, because the doctor has two questions. The version answers "is
/// this the build this tree makes". The subcommand list answers "can the
/// binary on PATH serve the manifest that ships beside it" — the drift a
/// source-only contract test cannot see, because it reads the source rather
/// than whatever executable a host will actually spawn.
pub fn identity() -> String {
    format!(
        "gitpulse-hook {VERSION}\nsubcommands: {}\n",
        SUBCOMMANDS.join(", ")
    )
}

/// Routes one parsed payload to its handler.
///
/// An unknown subcommand is an `Err` for the binary to report on stderr; it is
/// deliberately not a silent no-op, because a plugin whose hook name has
/// drifted would otherwise look like a check that ran.
pub fn dispatch(
    subcommand: &str,
    argument: Option<&str>,
    input: &HookInput,
) -> Result<HookOutput, String> {
    match subcommand {
        "collision-guard" => Ok(run_collision_guard(input)),
        "command-gate" => Ok(run_command_gate(input)),
        "session-brief" => Ok(run_session_brief(input)),
        "notify" => run_notify(argument, input, notify_send),
        other => Err(format!(
            "unknown subcommand '{other}'; expected one of {}",
            SUBCOMMANDS.join(", ")
        )),
    }
}

/* ── notify ───────────────────────────────────────────────────────────────── */

/// The environment variable naming the agent that spawned this hook.
///
/// Set by GitPulse on the sessions it launches, where the launcher is known
/// exactly. An external session has none and is identified from the payload.
pub const AGENT_KIND_ENV: &str = "GITPULSE_AGENT_KIND";

/// Wall-clock ceiling on one notification report.
///
/// Much shorter than [`BUDGET`]: a `Notification` hook runs while the user is
/// already waiting, and there is nothing here worth waiting for. The socket is
/// on the same machine; if it does not answer within this, it is not there.
const NOTIFY_BUDGET: Duration = Duration::from_millis(1500);

/// Tells the running GitPulse that this agent wants the user.
///
/// The argument, not the payload, carries the reason. The plugin registers
/// one entry per `Notification` matcher, and one per lifecycle event, and each
/// passes its own word here; that makes the reason a fact about which hook the
/// host chose to run, rather than a string parsed out of a payload (the host's
/// `notification_type` names the same thing, and is not needed).
///
/// This never produces a `systemMessage`, and that is a deliberate departure
/// from the rest of this module. Everything else here is a *check*, where
/// silence would be mistaken for a pass. This is a side effect: GitPulse being
/// closed is the ordinary case, and a warning in the transcript on every turn
/// would be noise the user cannot act on. Where it went instead is GitPulse's
/// own notification settings, which report whether the socket is listening and
/// how many reports it has accepted — a place the user looks when they wonder
/// why nothing arrives, rather than one that interrupts them when they do not.
fn run_notify(
    argument: Option<&str>,
    input: &HookInput,
    send: impl FnOnce(&std::path::Path, &str) -> Result<(), String> + Send + 'static,
) -> Result<HookOutput, String> {
    let Some(event) = argument.and_then(crate::alerts::bridge::event) else {
        // An Err rather than silence: an unknown reason means the plugin
        // manifest and this binary have drifted, which is exactly the kind of
        // mismatch that otherwise presents as "notifications stopped working".
        return Err(format!(
            "unknown notify event '{}'; expected one of {}",
            argument.unwrap_or_default(),
            crate::alerts::bridge::EVENTS
                .iter()
                .map(|event| event.name)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };
    // A state or a resolution is about a GitPulse terminal, and the socket
    // refuses one that names none. Outside a GitPulse terminal — the common
    // case for a plugin installed once for every session — there is nothing
    // to tell, so this costs a process start and nothing else: no socket, no
    // error. `PostToolUse` runs on every tool call, which is why that matters.
    if event.role != crate::alerts::bridge::Role::Banner && session_env().is_none() {
        return Ok(HookOutput::silent());
    }
    let path = match std::env::var_os(crate::alerts::bridge::SOCKET_ENV) {
        Some(value) if !value.is_empty() => std::path::PathBuf::from(value),
        _ => crate::alerts::bridge::socket_path()
            .ok_or("no GitPulse configuration directory to find a notification socket in")?,
    };
    let payload = notify_payload(event, input);
    // Detached at the deadline. A hook the host is waiting on must not be held
    // by a socket that is not answering, and the report is worth nothing late.
    match within_budget(NOTIFY_BUDGET, move || send(&path, &payload)) {
        Some(Ok(())) => Ok(HookOutput::silent()),
        Some(Err(error)) => Err(format!("notification report not delivered: {error}")),
        None => Err("notification report exceeded its deadline; not retried".into()),
    }
}

/// The GitPulse PTY this hook runs under, when it runs under one.
fn session_env() -> Option<String> {
    std::env::var(crate::alerts::bridge::SESSION_ENV)
        .ok()
        .filter(|session| !session.is_empty())
}

/// The most of an agent's own text one report carries. The socket refuses a
/// report over [`crate::alerts::bridge::MAX_REPORT_BYTES`] whole, and the
/// board shows a line of it, so a long final message must be cut here rather
/// than cost the report.
const NOTIFY_MESSAGE_BYTES: usize = 1024;

fn clip(text: &str, bytes: usize) -> &str {
    if text.len() <= bytes {
        return text;
    }
    let mut end = bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The JSON one report carries, matching `alerts::bridge::parse_report`.
fn notify_payload(event: &crate::alerts::bridge::Event, input: &HookInput) -> String {
    let agent = match std::env::var(AGENT_KIND_ENV) {
        Ok(kind)
            if crate::alerts::bridge::AGENTS
                .iter()
                .any(|(name, _)| *name == kind) =>
        {
            kind
        }
        // Nothing authoritative. Cursor identifies itself in its payload;
        // otherwise this binary is only ever installed as a Claude Code,
        // Codex or Cursor plugin, and Claude Code is the one whose
        // `Notification` event exists.
        _ if input.is_cursor => "cursor".to_string(),
        _ => "claude".to_string(),
    };
    let mut report = serde_json::Map::new();
    report.insert("v".into(), json!(1));
    report.insert("event".into(), json!(event.name));
    report.insert("agent".into(), json!(agent));
    if let Some(session) = session_env() {
        report.insert("session".into(), json!(session));
    }
    if !input.cwd.is_empty() && input.cwd.len() <= 4096 {
        report.insert("cwd".into(), json!(input.cwd));
    }
    // What the agent said with it: the host's notification text, or for an
    // event that has none, the field that says the same thing.
    let message = [
        input.message.as_str(),
        input.tool_summary.as_str(),
        input.last_assistant_message.as_str(),
        input.error.as_str(),
    ]
    .into_iter()
    .find(|text| !text.trim().is_empty())
    .unwrap_or_default();
    // A resolution says nothing the board shows.
    if !message.is_empty() && event.role != crate::alerts::bridge::Role::Resolve {
        report.insert("message".into(), json!(clip(message, NOTIFY_MESSAGE_BYTES)));
    }
    if !input.tool_subject.is_empty() {
        report.insert("subject".into(), json!(input.tool_subject));
    }
    if !input.agent_id.is_empty() {
        report.insert("subagent".into(), json!(true));
    }
    Value::Object(report).to_string()
}

#[cfg(unix)]
fn notify_send(path: &std::path::Path, payload: &str) -> Result<(), String> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    let mut stream = UnixStream::connect(path).map_err(|e| format!("{}: {e}", path.display()))?;
    stream
        .set_write_timeout(Some(NOTIFY_BUDGET))
        .map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(NOTIFY_BUDGET))
        .map_err(|e| e.to_string())?;
    stream
        .write_all(payload.as_bytes())
        .map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())?;
    // Half-close so the reader sees EOF and answers rather than waiting for
    // its own timeout.
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|e| e.to_string())?;
    // Then read the answer. A report GitPulse refused was not delivered, and
    // saying it was is the exact confusion this module exists to prevent: the
    // refusal is the drift between this binary and the app it reports to.
    let mut reply = Vec::with_capacity(16);
    (&mut stream)
        .take(64)
        .read_to_end(&mut reply)
        .map_err(|e| format!("no answer from GitPulse: {e}"))?;
    acknowledged(&reply)
}

/// Whether the socket's one-line answer accepted the report.
fn acknowledged(reply: &[u8]) -> Result<(), String> {
    match serde_json::from_slice::<Value>(reply)
        .ok()
        .and_then(|value| value.get("ok").and_then(Value::as_bool))
    {
        Some(true) => Ok(()),
        Some(false) => Err(
            "GitPulse refused the report; this gitpulse-hook and the running app may be different versions"
                .into(),
        ),
        None => Err(format!(
            "GitPulse answered with something other than an acknowledgement: {:?}",
            String::from_utf8_lossy(reply)
        )),
    }
}

#[cfg(not(unix))]
fn notify_send(_path: &std::path::Path, _payload: &str) -> Result<(), String> {
    Err("agent notification reports need a Unix socket, which this platform does not offer".into())
}

/* ── Shared helpers ───────────────────────────────────────────────────────── */

/// Runs `work` on a worker thread and abandons it at `budget`.
///
/// The budget is a parameter rather than a constant read inside so the
/// give-up path is testable in milliseconds instead of making the suite sit
/// out the production ceiling.
///
/// The scans underneath are synchronous `git` subprocesses with no cancellation
/// token, so the only honest ceiling available is to stop *waiting*. The worker
/// is left detached: the process exits immediately after printing, which tears
/// it down, and a detached thread cannot hold up that exit.
fn within_budget<T: Send + 'static>(
    budget: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    // A send onto a dropped receiver is an error, not a panic, so the worker
    // outliving our patience cannot take the process down with it.
    thread::Builder::new()
        .name("gitpulse-hook-check".into())
        .spawn(move || {
            let _ = tx.send(work());
        })
        .ok()?;
    rx.recv_timeout(budget).ok()
}

/// The canonical form of a path that may not exist yet.
///
/// `Write` creates files, so the target frequently has no inode. Canonicalising
/// the parent and re-joining the name resolves the symlinks that matter (on
/// macOS `/tmp` is `/private/tmp`, and a raw prefix comparison against the
/// worktree root would miss every path under it).
fn canonical_enough(path: &Path) -> PathBuf {
    if let Ok(real) = path.canonicalize() {
        return real;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => match parent.canonicalize() {
            Ok(real) => real.join(name),
            Err(_) => path.to_path_buf(),
        },
        _ => path.to_path_buf(),
    }
}

/// `file` as a `/`-separated path relative to `root`, or `None` if it is
/// outside it.
///
/// Built from components rather than by trimming a string prefix, so a Windows
/// payload — where the docs warn `tool_input.file_path` arrives with
/// backslashes — compares correctly and still comes out in the forward-slash
/// form `git status --porcelain` uses, which is what the collision rows hold.
fn repo_relative(root: &Path, file: &Path) -> Option<String> {
    let rel = file.strip_prefix(root).ok()?;
    let mut out = String::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => {
                if !out.is_empty() {
                    out.push('/');
                }
                out.push_str(&part.to_string_lossy());
            }
            // `..`, a drive prefix or a root inside a "relative" remainder means
            // the strip did not mean what it looks like it meant.
            _ => return None,
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Path equality that survives symlinks, falling back to a literal comparison
/// when either side cannot be canonicalised (a pruned worktree, say).
fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// The brief's live-session line: how many running agents this repository
/// and this worktree hold, per kind, with every kind that could not be
/// observed named as unknown rather than left out of the sum.
fn live_sessions_lines(live: &crate::insights::LiveSessionFacet, here: &str) -> String {
    let kinds: Vec<String> = live
        .kinds
        .iter()
        .map(|k| {
            if !k.ok {
                format!("{} unknown", k.kind)
            } else if k.unverified > 0 || k.truncated {
                format!("{} at least {}", k.kind, k.sessions)
            } else {
                format!("{} {}", k.kind, k.sessions)
            }
        })
        .collect();
    let here_count = live
        .worktrees
        .iter()
        .filter(|w| same_path(Path::new(&w.path), Path::new(here)))
        .map(|w| w.sessions.len())
        .sum::<usize>();
    format!(
        "live agent sessions: {}{} in this repository ({}), {here_count} in this worktree\n",
        if live.ok { "" } else { "at least " },
        live.sessions,
        if kinds.is_empty() {
            "no kind observed".to_string()
        } else {
            kinds.join(", ")
        },
    )
}

/// Never let an empty explanation render as an empty string: "unknown" is a
/// worse answer than a real reason and a much better one than a blank.
fn or_unknown(text: &str) -> String {
    if text.trim().is_empty() {
        "reason unknown".to_string()
    } else {
        text.trim().to_string()
    }
}

/// [`or_unknown`], for a reason being embedded *inside* a longer sentence.
///
/// These reasons are other people's finished sentences — a repository-trust
/// refusal is three of them — so dropping one in front of a `. ` produced
/// "…covers all of them.. Other worktrees may hold…". The terminator belongs
/// to the sentence doing the embedding, not to the fragment being embedded.
///
/// Every trailing terminator, not just one: stripping a single dot off `...`
/// leaves `..`, which is the defect again one character shorter. GitPulse
/// writes truncation as `…`, so an ASCII run of dots here is punctuation, not
/// meaning. A reason that is *nothing but* terminators leaves no clause at
/// all, and an empty one would render as `: . Other worktrees` — so that falls
/// back rather than producing a sentence with a hole in it.
fn clause(text: &str) -> String {
    let reason = or_unknown(text);
    let trimmed = reason.trim_end_matches(['.', '!', '?']).trim_end();
    if trimmed.is_empty() {
        return reason;
    }
    trimmed.to_string()
}

/// Cuts `text` to `limit` bytes and says that it did.
fn truncate_marked(mut text: String, limit: usize) -> String {
    const MARKER: &str = "\n… truncated";
    if text.len() <= limit {
        return text;
    }
    let mut cut = limit.saturating_sub(MARKER.len());
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    text.push_str(MARKER);
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codeintel::CodeintelStatus;
    use crate::harness::PolicyVerdict;
    use crate::insights::{
        AgentSummary, ChangesFacet, CollisionItem, CollisionParty, WorktreeFacet, WorktreeSummary,
    };
    use crate::ledger::LedgerStatus;
    use serde_json::json;

    fn no_sessions(_: &str) -> String {
        String::new()
    }

    fn clean_risk(items: Vec<CollisionItem>) -> CollisionRisk {
        CollisionRisk {
            ok: true,
            error: String::new(),
            overlapping_files: items.len() as u32,
            worktrees_involved: 0,
            scanned_worktrees: 2,
            unscanned_worktrees: 0,
            failed_worktrees: 0,
            truncated: false,
            items,
            shared_worktree_files: 0,
            shared_worktrees: Vec::new(),
            sessions_ok: true,
            sessions_error: String::new(),
        }
    }

    fn party(path: &str, branch: &str) -> CollisionParty {
        CollisionParty {
            path: path.to_string(),
            branch: Some(branch.to_string()),
            agent_kind: "claude".to_string(),
        }
    }

    /// The command-gate decision for a checkout bound to no task — the case
    /// every test below that does not name a binding is about.
    fn decide(verdict: &PolicyVerdict) -> HookOutput {
        command_gate_decision(&ScopedVerdict {
            verdict: verdict.clone(),
            bound: false,
        })
    }

    fn allowed_verdict() -> PolicyVerdict {
        PolicyVerdict {
            status: PolicyStatus::Allowed,
            checked: true,
            target: "git status".to_string(),
            rule: String::new(),
            severity: String::new(),
            reason: String::new(),
            demoted: String::new(),
            grant_id: String::new(),
            granted_by: String::new(),
            widened: String::new(),
            degraded: Vec::new(),
            task_id: String::new(),
            detail: String::new(),
            detail_code: String::new(),
        }
    }

    fn snapshot_fixture() -> InsightsSnapshot {
        InsightsSnapshot {
            repo_path: "/repo".to_string(),
            branch: Some("main".to_string()),
            branch_ok: true,
            worktrees: WorktreeFacet {
                ok: true,
                error: String::new(),
                count: 2,
                dirty: 1,
                scanned: 2,
                dirty_unknown: 0,
                blocked: 0,
                blocked_unknown: 0,
                truncated: false,
                items: vec![
                    WorktreeSummary {
                        path: "/repo".to_string(),
                        name: "repo".to_string(),
                        branch: Some("main".to_string()),
                        is_main: true,
                        is_bare: false,
                        dirty_files: Some(0),
                        agent_kind: String::new(),
                        session_slug: String::new(),
                        operation_kind: String::new(),
                        is_detached: false,
                        operation_ok: true,
                    },
                    WorktreeSummary {
                        path: "/repo/.claude/worktrees/feature".to_string(),
                        name: "feature".to_string(),
                        branch: Some("claude/feature".to_string()),
                        is_main: false,
                        is_bare: false,
                        dirty_files: Some(4),
                        agent_kind: "claude".to_string(),
                        session_slug: "feature-a1b2".to_string(),
                        operation_kind: String::new(),
                        is_detached: false,
                        operation_ok: true,
                    },
                ],
            },
            agents: AgentSummary {
                ok: true,
                sessions: 1,
                kinds: Vec::new(),
                live: crate::insights::LiveSessionFacet {
                    ok: true,
                    sessions: 0,
                    kinds: Vec::new(),
                    worktrees: Vec::new(),
                },
                truncated: false,
            },
            changes: ChangesFacet {
                ok: true,
                error: String::new(),
                files: 3,
                staged: 1,
                unstaged: 2,
                untracked: 0,
                conflicted: 0,
                additions: 10,
                deletions: 2,
                churn_warnings: 0,
                churn_overflowed: false,
                truncated: false,
            },
            collisions: clean_risk(vec![CollisionItem {
                path: "src/lib.rs".to_string(),
                worktrees: vec![
                    party("/repo", "main"),
                    party("/repo/.claude/worktrees/feature", "claude/feature"),
                ],
                entity: None,
            }]),
            ledger: LedgerStatus {
                recording: true,
                path: "/repo/.gitpulse/ledger.sqlite".to_string(),
                dropped: 0,
                error: String::new(),
                error_code: String::new(),
            },
            codeintel: CodeintelStatus {
                available: true,
                is_fresh: Some(true),
                freshness_reason: None,
                source_freshness: Some(true),
                analyzer_freshness: Some(true),
                pending_count: Some(0),
                db_path: "/repo/.devcouncil/codeintel/devmap.sqlite".to_string(),
                generation_id: Some(7),
                total_files: Some(400),
                total_symbols: Some(11294),
                total_edges: Some(29940),
                reason: None,
            },
            deadline_expired: false,
            duration_ms: 12,
        }
    }

    /* ── input parsing ────────────────────────────────────────────────────── */

    #[test]
    fn the_documented_snake_case_input_fields_are_the_ones_we_read() {
        let input = parse_input(
            &json!({
                "session_id": "abc123",
                "transcript_path": "/tmp/t.jsonl",
                "cwd": "/home/user/project",
                "permission_mode": "default",
                "hook_event_name": "PreToolUse",
                "tool_name": "Edit",
                "tool_input": { "file_path": "/home/user/project/src/lib.rs" },
                "tool_use_id": "toolu_01"
            })
            .to_string(),
        )
        .expect("documented payload parses");
        assert_eq!(input.session_id, "abc123");
        assert_eq!(input.cwd, "/home/user/project");
        assert_eq!(input.hook_event_name, "PreToolUse");
        assert_eq!(input.tool_name, "Edit");
        assert_eq!(input.file_path, "/home/user/project/src/lib.rs");
    }

    #[test]
    fn session_start_source_and_bash_command_are_read_from_their_documented_places() {
        let start = parse_input(r#"{"hook_event_name":"SessionStart","source":"resume"}"#)
            .expect("SessionStart payload parses");
        assert_eq!(start.source, "resume");

        let bash = parse_input(
            r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"npm test"}}"#,
        )
        .expect("Bash payload parses");
        assert_eq!(bash.command, "npm test");
    }

    #[test]
    fn a_tool_input_of_an_unexpected_shape_yields_empty_fields_rather_than_a_failed_parse() {
        // The host owns this payload; a shape we did not expect must degrade to
        // "we have no path", which is reported, not to a parse error or a panic.
        let input = parse_input(r#"{"hook_event_name":"PreToolUse","tool_input":"not-an-object"}"#)
            .expect("odd tool_input still parses");
        assert!(input.file_path.is_empty());
        assert!(input.command.is_empty());

        let numeric = parse_input(r#"{"cwd":42,"tool_input":{"file_path":[1,2]}}"#)
            .expect("non-string fields still parse");
        assert!(numeric.cwd.is_empty());
        assert!(numeric.file_path.is_empty());
    }

    #[test]
    fn unparseable_stdin_is_an_error_the_binary_can_report_not_a_panic() {
        assert!(parse_input("{not json").is_err());
        assert!(parse_input("").is_err());
        assert!(
            parse_input("[1,2,3]").is_err(),
            "a JSON array is not a hook payload"
        );
    }

    /// Cursor native sessionStart sends `workspace_roots` and often no `cwd`.
    /// Treating that as "no cwd" made every Cursor session a non-brief.
    #[test]
    fn a_cursor_payload_with_workspace_roots_and_no_cwd_still_has_a_root() {
        let input = parse_input(
            &json!({
                "hook_event_name": "sessionStart",
                "cursor_version": "3.20.7",
                "composer_mode": "agent",
                "workspace_roots": ["/Users/bharath/Code/devtools/GitPulse"],
            })
            .to_string(),
        )
        .expect("Cursor payload parses");
        assert_eq!(input.cwd, "/Users/bharath/Code/devtools/GitPulse");
        assert!(
            input.is_cursor,
            "cursor_version must select the Cursor stdout schema"
        );
    }

    /// `cwd` is the documented Claude field and must still win when both land.
    #[test]
    fn cwd_wins_over_workspace_roots() {
        let input = parse_input(
            &json!({
                "cwd": "/repo/here",
                "workspace_roots": ["/repo/elsewhere", "/repo/third"],
            })
            .to_string(),
        )
        .expect("parses");
        assert_eq!(input.cwd, "/repo/here");
    }

    /// Empty strings, non-strings, and a string-typed field must degrade to
    /// empty rather than panic or pick a number.
    #[test]
    fn hostile_workspace_roots_degrade_to_empty_cwd() {
        for raw in [
            r#"{"workspace_roots":[]}"#,
            r#"{"workspace_roots":["", "  "]}"#,
            r#"{"workspace_roots":[1,2]}"#,
            r#"{"workspace_roots":"/not-an-array"}"#,
            r#"{"workspace_roots":{"0":"/repo"}}"#,
            r#"{"workspaceRoots":["/camel"]}"#,
        ] {
            let input = parse_input(raw).expect("hostile shape still parses");
            if raw.contains("workspaceRoots") {
                assert_eq!(
                    input.cwd, "/camel",
                    "camelCase workspaceRoots is the same field Cursor may emit: {raw}"
                );
            } else {
                assert!(
                    input.cwd.is_empty(),
                    "{raw} must not invent a cwd, got {:?}",
                    input.cwd
                );
            }
        }
    }

    /// Skipping empty entries rather than taking the first slot is what a
    /// multi-root workspace with a blank first folder needs.
    #[test]
    fn the_first_non_empty_workspace_root_is_used() {
        let input = parse_input(
            &json!({
                "workspace_roots": ["", "  ", "/second", "/third"],
            })
            .to_string(),
        )
        .expect("parses");
        assert_eq!(input.cwd, "/second");
    }

    #[test]
    fn workspace_roots_past_the_cap_are_not_walked() {
        let mut roots = vec![""; MAX_WORKSPACE_ROOTS];
        roots.push("/late");
        let input = parse_input(&json!({ "workspace_roots": roots }).to_string()).expect("parses");
        assert!(
            input.cwd.is_empty(),
            "a root past the cap must not be taken: {:?}",
            input.cwd
        );

        let mut under = vec![""; MAX_WORKSPACE_ROOTS - 1];
        under.push("/just-in");
        let input = parse_input(&json!({ "workspace_roots": under }).to_string()).expect("parses");
        assert_eq!(input.cwd, "/just-in");
    }

    #[test]
    fn a_session_with_no_cwd_and_no_workspace_roots_says_so() {
        let output = run_session_brief(&HookInput {
            hook_event_name: SESSION_START.to_string(),
            cwd: String::new(),
            source: "startup".to_string(),
            ..Default::default()
        });
        assert!(!output.is_silent());
        assert!(output.system_message.unwrap_or_default().contains("no cwd"));
    }

    /* ── the honesty invariant ────────────────────────────────────────────── */

    #[test]
    fn a_check_that_could_not_run_is_observably_different_from_a_clean_check() {
        let clean = collision_decision(&CollisionFacts {
            ok: true,
            error: String::new(),
            target: "src/lib.rs".to_string(),
            others: Vec::new(),
            partial: String::new(),
            entity: None,
        });
        let failed = collision_decision(&CollisionFacts {
            ok: false,
            error: "/tmp/x is not inside a Git repository".to_string(),
            target: "src/lib.rs".to_string(),
            others: Vec::new(),
            partial: String::new(),
            entity: None,
        });

        // The whole contract in one assertion: these must not be the same bytes.
        assert_ne!(clean.render(), failed.render());
        assert_eq!(
            clean.render(),
            None,
            "a clean check renders no decision at all"
        );
        let rendered = failed.render().expect("a failed check must say something");
        assert!(rendered.contains("systemMessage"));
        assert!(rendered.contains("did NOT run"));
        assert!(
            !rendered.contains("permissionDecision"),
            "a check that did not run must not render a decision either"
        );
    }

    #[test]
    fn a_partial_scan_that_found_nothing_is_observably_different_from_a_complete_one() {
        let complete = collision_decision(&CollisionFacts {
            ok: true,
            error: String::new(),
            target: "src/lib.rs".to_string(),
            others: Vec::new(),
            partial: String::new(),
            entity: None,
        });
        let partial = collision_decision(&CollisionFacts {
            ok: true,
            error: String::new(),
            target: "src/lib.rs".to_string(),
            others: Vec::new(),
            partial: "2 worktree(s) were not scanned".to_string(),
            entity: None,
        });
        assert_ne!(complete.render(), partial.render());
        assert!(partial
            .render()
            .expect("a partial scan must say so")
            .contains("INCOMPLETE"));
    }

    #[test]
    fn no_collision_guard_outcome_ever_renders_an_allow() {
        // `allow` would override the user's own permission rules with an
        // approval this hook has no standing to give.
        let cases = [
            CollisionFacts::default(),
            CollisionFacts {
                ok: true,
                target: "a".into(),
                ..Default::default()
            },
            CollisionFacts {
                ok: true,
                target: "a".into(),
                partial: "truncated".into(),
                ..Default::default()
            },
            CollisionFacts {
                ok: true,
                target: "a".into(),
                others: vec![CollisionOther {
                    worktree: "/other".into(),
                    branch: "b".into(),
                    session: String::new(),
                    agent_kind: String::new(),
                }],
                ..Default::default()
            },
        ];
        for facts in cases {
            let rendered = collision_decision(&facts).render().unwrap_or_default();
            assert!(
                !rendered.contains("\"allow\""),
                "rendered an allow: {rendered}"
            );
        }
    }

    /* ── collision-guard ──────────────────────────────────────────────────── */

    #[test]
    fn a_contended_file_escalates_to_the_user_and_names_the_other_worktree() {
        let output = collision_decision(&CollisionFacts {
            ok: true,
            error: String::new(),
            target: "src/lib.rs".to_string(),
            others: vec![CollisionOther {
                worktree: "/repo/.claude/worktrees/feature".to_string(),
                branch: "claude/feature".to_string(),
                session: "feature-a1b2".to_string(),
                agent_kind: "claude".to_string(),
            }],
            partial: String::new(),
            entity: None,
        });
        let specific = output
            .hook_specific_output
            .as_ref()
            .expect("a collision renders a decision");
        assert_eq!(specific.hook_event_name, PRE_TOOL_USE);
        assert_eq!(specific.permission_decision, Some(PermissionDecision::Ask));
        let reason = specific
            .permission_decision_reason
            .as_deref()
            .unwrap_or_default();
        assert!(reason.contains("src/lib.rs"));
        assert!(reason.contains("/repo/.claude/worktrees/feature"));
        assert!(reason.contains("claude/feature"));
        assert!(reason.contains("feature-a1b2"));
        assert!(reason.contains("will conflict when these branches meet"));
        // `ask` serialises to the documented wire value, not the Rust name.
        assert!(output.render().expect("renders").contains("\"ask\""));
    }

    #[test]
    fn disjoint_symbol_verdict_does_not_use_the_collision_wording() {
        let output = collision_decision(&CollisionFacts {
            ok: true,
            error: String::new(),
            target: "src/lib.rs".to_string(),
            others: vec![CollisionOther {
                worktree: "/repo/wt/feature".to_string(),
                branch: "feature".to_string(),
                session: String::new(),
                agent_kind: String::new(),
            }],
            partial: String::new(),
            entity: Some(crate::insights::EntityCollisionVerdict {
                path: "src/lib.rs".into(),
                kind: crate::insights::EntityCollisionKind::DisjointSymbols,
                reason: "src/lib.rs is dirty in multiple worktrees, but no shared symbol was seen on the old-side ranges — file overlap, not a merge promise".into(),
                shared_symbols: Vec::new(),
            }),
        });
        let reason = output
            .hook_specific_output
            .as_ref()
            .and_then(|s| s.permission_decision_reason.as_deref())
            .unwrap_or_default();
        assert!(
            !reason.contains("will conflict when these branches meet"),
            "disjoint must not use collision wording: {reason}"
        );
        assert!(reason.contains("not a merge promise"), "{reason}");
    }

    #[test]
    fn head_sha_mismatch_entity_keeps_file_level_collision_wording() {
        let output = collision_decision(&CollisionFacts {
            ok: true,
            error: String::new(),
            target: "src/lib.rs".to_string(),
            others: vec![CollisionOther {
                worktree: "/repo/wt/feature".to_string(),
                branch: "feature".to_string(),
                session: String::new(),
                agent_kind: String::new(),
            }],
            partial: String::new(),
            entity: Some(crate::insights::EntityCollisionVerdict {
                path: "src/lib.rs".into(),
                kind: crate::insights::EntityCollisionKind::FileLevel,
                reason: "indexed generation head_sha abc is not the requested def".into(),
                shared_symbols: Vec::new(),
            }),
        });
        let reason = output
            .hook_specific_output
            .as_ref()
            .and_then(|s| s.permission_decision_reason.as_deref())
            .unwrap_or_default();
        assert!(
            reason.contains("will conflict when these branches meet"),
            "{reason}"
        );
        assert!(
            !reason.contains("not a merge promise"),
            "mismatch must not use disjoint wording: {reason}"
        );
        assert!(reason.contains("head_sha"), "{reason}");
    }

    #[test]
    fn the_worktree_doing_the_editing_is_never_reported_as_its_own_collision_party() {
        let risk = clean_risk(vec![CollisionItem {
            path: "src/lib.rs".to_string(),
            worktrees: vec![party("/repo", "main"), party("/repo/wt/feature", "feature")],
            entity: None,
        }]);
        let facts = collision_facts(&risk, Path::new("/repo"), "src/lib.rs", &no_sessions);
        assert_eq!(facts.others.len(), 1);
        assert_eq!(facts.others[0].worktree, "/repo/wt/feature");
    }

    #[test]
    fn a_path_with_no_overlap_row_produces_a_clean_complete_check() {
        let risk = clean_risk(vec![CollisionItem {
            path: "src/other.rs".to_string(),
            worktrees: vec![party("/repo", "main"), party("/repo/wt/x", "x")],
            entity: None,
        }]);
        let facts = collision_facts(&risk, Path::new("/repo"), "src/lib.rs", &no_sessions);
        assert!(facts.ok);
        assert!(facts.others.is_empty());
        assert!(
            facts.partial.is_empty(),
            "nothing was missed, so nothing is claimed missed"
        );
        assert!(collision_decision(&facts).is_silent());
    }

    #[test]
    fn an_unscanned_worktree_makes_the_absence_of_a_finding_partial_not_clean() {
        let mut risk = clean_risk(Vec::new());
        risk.unscanned_worktrees = 3;
        risk.truncated = true;
        let facts = collision_facts(&risk, Path::new("/repo"), "src/lib.rs", &no_sessions);
        assert!(facts.ok, "the scan did run");
        assert!(facts.partial.contains("3 worktree(s) were not scanned"));
        assert!(!collision_decision(&facts).is_silent());
    }

    #[test]
    fn a_truncated_overlap_list_makes_the_absence_of_a_finding_partial() {
        let mut risk = clean_risk(Vec::new());
        risk.truncated = true;
        let facts = collision_facts(&risk, Path::new("/repo"), "src/lib.rs", &no_sessions);
        assert!(facts.partial.contains("truncated"));
    }

    #[test]
    fn a_worktree_that_failed_to_scan_is_reported_even_though_others_were_read() {
        // A sweep that read two worktrees and failed on a third did real work,
        // so it is not a non-check — but a negative result that ignored the
        // third would be a check claiming more coverage than it had.
        let mut risk = clean_risk(Vec::new());
        risk.ok = false;
        risk.failed_worktrees = 1;
        risk.error = "fatal: not a git repository".to_string();
        let facts = collision_facts(&risk, Path::new("/repo"), "src/lib.rs", &no_sessions);
        assert!(facts.ok, "worktrees were read, so the scan did run");
        assert!(facts.partial.contains("1 worktree(s) could not be read"));
        assert!(facts.partial.contains("fatal: not a git repository"));
        assert!(!collision_decision(&facts).is_silent());
    }

    /// A refusal is a finished sentence — the trust one is three of them — and
    /// the non-check notice embeds it before a `. `. Pasting the two together
    /// shipped "…covers all of them.. Other worktrees may hold…" to every
    /// agent session in an untrusted worktree.
    #[test]
    fn an_embedded_reason_does_not_bring_its_own_full_stop() {
        let facts = CollisionFacts {
            ok: false,
            error: "REPOSITORY_TRUST_REQUIRED: Open /a in GitPulse and trust it. \
                    This is a linked worktree: approving any working tree covers all of them."
                .into(),
            target: "a.txt".into(),
            others: Vec::new(),
            partial: String::new(),
            entity: None,
        };
        let message = collision_decision(&facts)
            .system_message
            .expect("a non-check always says so");
        assert!(
            !message.contains(".."),
            "a reason ending in a full stop must not double it: {message}"
        );
        assert!(
            message.contains("covers all of them. Other worktrees"),
            "the embedding sentence still supplies its own terminator: {message}"
        );

        // A reason that ends mid-clause is left alone; a run of dots goes
        // entirely, because leaving one behind is the same defect shorter.
        assert_eq!(clause("the disk went away"), "the disk went away");
        assert_eq!(clause("it failed..."), "it failed");
        assert_eq!(clause("really?"), "really");
        assert_eq!(clause("   "), "reason unknown");
        // Nothing but terminators would leave a hole in the sentence.
        assert_eq!(clause("..."), "...");
    }

    /// `CollisionRisk::error` holds the first failure only. Rendering it after
    /// a count of five reads as the cause of all five, which is a claim the
    /// scan never made — four refusals and one unreadable disk look identical
    /// to four refusals and one more.
    #[test]
    fn several_failures_do_not_present_one_cause_as_all_of_them() {
        let risk = CollisionRisk {
            ok: false,
            error: "REPOSITORY_TRUST_REQUIRED: Open /a in GitPulse".into(),
            overlapping_files: 0,
            worktrees_involved: 0,
            scanned_worktrees: 1,
            unscanned_worktrees: 0,
            failed_worktrees: 5,
            truncated: true,
            items: Vec::new(),
            shared_worktree_files: 0,
            shared_worktrees: Vec::new(),
            sessions_ok: true,
            sessions_error: String::new(),
        };
        let facts = collision_facts(&risk, Path::new("/repo"), "a.txt", &|_| String::new());
        assert!(facts.ok, "one worktree was read, so the scan ran");
        assert!(
            facts.partial.contains("5 worktree(s) could not be read"),
            "{}",
            facts.partial
        );
        assert!(
            facts.partial.contains("first of 5:"),
            "the one reported cause must not stand for five: {}",
            facts.partial
        );

        // A single failure has nothing to disambiguate, so it stays plain.
        let one = CollisionRisk {
            failed_worktrees: 1,
            ..risk
        };
        let facts = collision_facts(&one, Path::new("/repo"), "a.txt", &|_| String::new());
        assert!(
            facts
                .partial
                .contains("1 worktree(s) could not be read (REPOSITORY"),
            "{}",
            facts.partial
        );
        assert!(!facts.partial.contains("first of"), "{}", facts.partial);
    }

    #[test]
    fn a_sweep_that_read_no_worktree_at_all_is_a_non_check_not_a_partial_one() {
        let mut risk = clean_risk(Vec::new());
        risk.ok = false;
        risk.scanned_worktrees = 0;
        risk.failed_worktrees = 2;
        risk.error = "git worktree list failed".to_string();
        let facts = collision_facts(&risk, Path::new("/repo"), "src/lib.rs", &no_sessions);
        assert!(!facts.ok, "nothing was read, so nothing was established");
        assert!(collision_decision(&facts)
            .system_message
            .unwrap_or_default()
            .contains("did NOT run"));
    }

    #[test]
    fn a_collision_risk_that_did_not_run_is_carried_through_as_a_non_check() {
        let risk = CollisionRisk {
            ok: false,
            error: "git worktree list failed".to_string(),
            overlapping_files: 0,
            worktrees_involved: 0,
            scanned_worktrees: 0,
            unscanned_worktrees: 0,
            failed_worktrees: 1,
            truncated: false,
            items: Vec::new(),
            shared_worktree_files: 0,
            shared_worktrees: Vec::new(),
            sessions_ok: true,
            sessions_error: String::new(),
        };
        let facts = collision_facts(&risk, Path::new("/repo"), "src/lib.rs", &no_sessions);
        assert!(!facts.ok);
        assert!(facts.error.contains("git worktree list failed"));
    }

    #[test]
    fn a_positive_finding_survives_a_partial_scan_because_it_stands_on_its_own_evidence() {
        let output = collision_decision(&CollisionFacts {
            ok: true,
            error: String::new(),
            target: "src/lib.rs".to_string(),
            others: vec![CollisionOther {
                worktree: "/other".to_string(),
                branch: String::new(),
                session: String::new(),
                agent_kind: String::new(),
            }],
            partial: "1 worktree(s) were not scanned".to_string(),
            entity: None,
        });
        let specific = output.hook_specific_output.expect("still decides");
        assert_eq!(specific.permission_decision, Some(PermissionDecision::Ask));
        assert!(specific
            .permission_decision_reason
            .unwrap_or_default()
            .contains("Scan was incomplete"));
    }

    #[test]
    fn a_tool_call_with_no_file_path_reports_a_non_check_rather_than_staying_silent() {
        let output = run_collision_guard(&HookInput {
            hook_event_name: PRE_TOOL_USE.to_string(),
            tool_name: "Edit".to_string(),
            ..Default::default()
        });
        assert!(output
            .system_message
            .unwrap_or_default()
            .contains("no file path"));
    }

    #[test]
    fn a_cwd_outside_any_repository_reports_a_non_check_rather_than_a_clean_pass() {
        let output = run_collision_guard(&HookInput {
            hook_event_name: PRE_TOOL_USE.to_string(),
            tool_name: "Edit".to_string(),
            cwd: "/".to_string(),
            file_path: "/etc/hosts".to_string(),
            ..Default::default()
        });
        assert!(
            !output.is_silent(),
            "a non-check must never look like a clean check"
        );
        assert!(output.hook_specific_output.is_none());
        assert!(output
            .system_message
            .unwrap_or_default()
            .contains("did NOT run"));
    }

    /* ── path handling ────────────────────────────────────────────────────── */

    #[test]
    fn repo_relative_paths_come_out_slash_separated_and_reject_paths_outside_the_worktree() {
        let root = Path::new("/repo");
        assert_eq!(
            repo_relative(root, Path::new("/repo/src/lib.rs")),
            Some("src/lib.rs".to_string())
        );
        assert_eq!(repo_relative(root, Path::new("/elsewhere/lib.rs")), None);
        assert_eq!(repo_relative(root, Path::new("/repo")), None);
    }

    /* ── command-gate ─────────────────────────────────────────────────────── */

    #[test]
    fn a_blocking_verdict_denies_with_the_harnesss_own_refusal_text() {
        let mut verdict = allowed_verdict();
        verdict.status = PolicyStatus::Blocked;
        verdict.rule = "git.force_push".to_string();
        verdict.severity = "hard".to_string();
        verdict.reason = "force-push to a shared branch".to_string();
        verdict.target = "git push --force".to_string();

        let output = decide(&verdict);
        let specific = output.hook_specific_output.expect("a block decides");
        assert_eq!(specific.permission_decision, Some(PermissionDecision::Deny));
        assert_eq!(
            specific.permission_decision_reason,
            Some(verdict.refusal()),
            "the reason must be the canonical refusal, not a second rendering of it"
        );
    }

    /// The shape `manvi serve` really sends for a refused command, measured:
    /// every command decision carries the `host-scope` placeholder task,
    /// whether or not a scope was declared. A fixture with an empty `task_id`
    /// passed these tests while the real binary still got denied — the binding
    /// has to come from GitPulse, never from the verdict.
    fn unreadable(rule: &str) -> PolicyVerdict {
        let mut verdict = allowed_verdict();
        verdict.status = PolicyStatus::Blocked;
        verdict.rule = rule.to_string();
        verdict.severity = "hard".to_string();
        verdict.reason = "the gate could not read this line.".to_string();
        verdict.target = "cd src".to_string();
        verdict.task_id = "host-scope".to_string();
        verdict.degraded = vec![crate::harness::policy::UNREADABLE_ONLY_MARKER.to_string()];
        verdict
    }

    /// A harness that predates the marker cannot promise that nothing else in
    /// the line was refused — measured: it answered `cd src && git push
    /// --force` with `command.directory_change`. Without the marker the hook
    /// refuses as it always did, so a new hook with an old `manvi` is safe.
    #[test]
    fn an_unreadable_construct_without_the_harness_marker_still_refuses() {
        for rule in crate::harness::policy::UNREADABLE_CONSTRUCT_RULES {
            let mut verdict = unreadable(rule);
            verdict.degraded.clear();
            let output = decide_in(&verdict, false);
            assert_eq!(
                output
                    .hook_specific_output
                    .and_then(|s| s.permission_decision),
                Some(PermissionDecision::Deny),
                "{rule}: no marker, no softening"
            );
            verdict.degraded = vec!["repo_map.unavailable".to_string()];
            assert_eq!(
                decide_in(&verdict, false)
                    .hook_specific_output
                    .and_then(|s| s.permission_decision),
                Some(PermissionDecision::Deny),
                "{rule}: only the marker itself counts"
            );
        }
    }

    fn decide_in(verdict: &PolicyVerdict, bound: bool) -> HookOutput {
        command_gate_decision(&ScopedVerdict {
            verdict: verdict.clone(),
            bound,
        })
    }

    /// The measured complaint: `cd src && ls`, a heredoc, and `echo "$(date)"`
    /// were each a hard deny the agent had to write around, while `rm -rf /`
    /// was a silent demoted allow. A block that only says the gate could not
    /// read the line takes no decision in an unbound checkout — and is not
    /// silent either, so it can never pass for a clean check.
    #[test]
    fn an_unreadable_construct_in_an_unbound_checkout_is_announced_not_refused() {
        for rule in crate::harness::policy::UNREADABLE_CONSTRUCT_RULES {
            let output = decide_in(&unreadable(rule), false);
            let specific = output
                .hook_specific_output
                .clone()
                .expect("the model is told why the gate did not judge it");
            assert_eq!(
                specific.permission_decision, None,
                "{rule}: no decision, so the host's own permission rules run"
            );
            assert!(
                specific
                    .additional_context
                    .as_deref()
                    .is_some_and(|c| c.contains(rule)),
                "{rule}: the model must learn which rule it hit: {specific:?}"
            );
            let note = output.system_message.unwrap_or_default();
            assert!(
                note.contains("could not judge") && note.contains(rule),
                "{rule}: the person must be told it was not judged: {note:?}"
            );
            assert!(
                !note.contains(".."),
                "{rule}: the reason's own full stop was doubled: {note:?}"
            );
        }
    }

    /// What does not soften. A declared task scope is a person asking for
    /// enforcement, `eval` hides every clause, git safety is behaviour rather
    /// than reach, and a soft rule is the posture's to demote — each keeps its
    /// existing answer.
    #[test]
    fn only_unbound_unreadable_constructs_soften() {
        let deny = |verdict: &PolicyVerdict, bound: bool| {
            decide_in(verdict, bound)
                .hook_specific_output
                .and_then(|s| s.permission_decision)
        };
        for rule in crate::harness::policy::UNREADABLE_CONSTRUCT_RULES {
            assert_eq!(
                deny(&unreadable(rule), true),
                Some(PermissionDecision::Deny),
                "{rule}: a bound checkout keeps the refusal"
            );
        }
        for rule in [
            "command.reparse",
            "command.force_push",
            "path.outside_root",
            "path.secret",
            "command.too_long",
        ] {
            assert_eq!(
                deny(&unreadable(rule), false),
                Some(PermissionDecision::Deny),
                "{rule} must still refuse"
            );
        }
        let mut soft = unreadable("command.directory_change");
        soft.severity = "soft".to_string();
        assert_eq!(deny(&soft, false), Some(PermissionDecision::Deny));
        // A local block GitPulse made without asking the harness is not the
        // harness failing to read a line.
        let mut local = unreadable("command.directory_change");
        local.checked = false;
        assert_eq!(deny(&local, false), Some(PermissionDecision::Deny));
    }

    #[test]
    fn a_clean_allow_renders_nothing_so_the_users_own_permission_rules_decide() {
        assert!(decide(&allowed_verdict()).is_silent());
    }

    #[test]
    fn a_gate_that_could_not_answer_says_the_command_ran_ungated() {
        let mut verdict = allowed_verdict();
        verdict.status = PolicyStatus::Unchecked;
        verdict.checked = false;
        verdict.detail = "sidecar timed out".to_string();
        verdict.detail_code = "timeout".to_string();
        assert!(verdict.gate_failed());

        let output = decide(&verdict);
        assert!(
            output.hook_specific_output.is_none(),
            "a failed gate never decides"
        );
        let message = output.system_message.expect("a failed gate must speak");
        assert!(message.contains("UNGATED"));
        assert!(message.contains("timeout"));
    }

    #[test]
    fn no_harness_installed_is_reported_as_ungated_but_worded_apart_from_a_gate_failure() {
        let mut verdict = allowed_verdict();
        verdict.status = PolicyStatus::Unchecked;
        verdict.checked = false;
        verdict.detail = "no manvi binary on PATH".to_string();
        verdict.detail_code = "not_installed".to_string();
        assert!(
            !verdict.gate_failed(),
            "a missing harness is not a gate failure"
        );

        let mut failed = verdict.clone();
        failed.detail_code = "timeout".to_string();

        let absent = decide(&verdict);
        let broken = decide(&failed);
        assert!(absent
            .system_message
            .as_deref()
            .unwrap_or_default()
            .contains("UNGATED"));
        assert_ne!(
            absent.system_message, broken.system_message,
            "a standing condition and a transient failure must not read alike"
        );
    }

    #[test]
    fn an_allow_reached_with_rungs_that_could_not_run_is_not_reported_as_a_clean_pass() {
        let mut verdict = allowed_verdict();
        verdict.status = PolicyStatus::Degraded;
        verdict.degraded = vec!["repo_map".to_string()];
        let output = decide(&verdict);
        assert!(output.hook_specific_output.is_none());
        assert!(output
            .system_message
            .unwrap_or_default()
            .contains("repo_map"));
        assert_ne!(
            decide(&verdict).render(),
            decide(&allowed_verdict()).render()
        );
    }

    #[test]
    fn a_warned_verdict_carries_the_rule_that_fired_through_to_the_user() {
        let mut verdict = allowed_verdict();
        verdict.status = PolicyStatus::Warned;
        verdict.rule = "command.slow".to_string();
        verdict.reason = "this rewrites history".to_string();
        let message = decide(&verdict)
            .system_message
            .expect("a warning must reach the user");
        assert!(message.contains("command.slow"));
        assert!(message.contains("this rewrites history"));
    }

    /// A `manvi serve` stand-in that behaves as Manvi does since 31ecc70:
    /// with a declared scope, a redirect outside it is a `scope.unplanned`
    /// denial that posture=host does not demote; without one, the same command
    /// is a demoted allow. Every policy request is recorded.
    #[cfg(unix)]
    const FAKE_SCOPED_MANVI: &str = r#"#!/bin/sh
reply() {
  id=$(printf '%s' "$1" | sed -n 's/^{"id":"\([0-9]*\)".*/\1/p')
  printf '{"id":"%s","ok":true,"result":%s}\n' "$id" "$2"
}
IFS= read -r line || exit 1
reply "$line" '{"protocol":1,"ops":["hello","policy.check.command","policy.check.file"],"posture":"host"}'
while IFS= read -r line; do
  printf '%s\n' "$line" >> "@REQUESTS@"
  case "$line" in
    *'"scope"'*) reply "$line" '{"action":"deny","rule":"scope.unplanned","severity":"soft","reason":"Task TASK-HOOK does not authorize changes to docs/elsewhere.md","target":"docs/elsewhere.md","task_id":"TASK-HOOK","demoted":"","degraded":[]}' ;;
    *) reply "$line" '{"action":"allow","rule":"task.absent","severity":"soft","reason":"No running DevCouncil task authorizes this file write.","target":"docs/elsewhere.md","task_id":"","demoted":"serve.posture=host: no task model in the embedding host","degraded":[]}' ;;
  esac
done
"#;

    #[cfg(unix)]
    fn scoped_hook_fixture(bind: bool) -> (tempfile::TempDir, String, std::path::PathBuf) {
        use crate::test_support::git_in;
        let dir = tempfile::tempdir().expect("repository");
        git_in(dir.path(), &["init", "-b", "main"]);
        std::fs::write(dir.path().join("seed.txt"), "seed").expect("seed");
        git_in(dir.path(), &["add", "seed.txt"]);
        git_in(dir.path(), &["commit", "-m", "seed"]);
        let repo = dir
            .path()
            .canonicalize()
            .expect("canonical repository")
            .to_string_lossy()
            .into_owned();
        if bind {
            let db = crate::tasks::store_path(&repo);
            std::fs::create_dir_all(db.parent().expect("store parent")).expect("store dir");
            let store = dc_store::Store::open(&db).expect("task store");
            store
                .connection()
                .execute(
                    "INSERT INTO tasks (id, title, description, planned_files_json, status)
                     VALUES ('TASK-HOOK', 'Hook scope', '',
                         '[{\"path\":\"src/planned.rs\",\"allowed_change\":\"modify\"}]', 'in_progress')",
                    [],
                )
                .expect("task row");
            crate::ledger::bindings::bind(&repo, &repo, "TASK-HOOK").expect("bind");
        }
        let requests = dir.path().join("requests.ndjson");
        (dir, repo, requests)
    }

    #[cfg(unix)]
    fn bash_call(cwd: &str, command: &str) -> HookInput {
        HookInput {
            hook_event_name: PRE_TOOL_USE.to_string(),
            tool_name: "Bash".to_string(),
            cwd: cwd.to_string(),
            command: command.to_string(),
            ..Default::default()
        }
    }

    /// The production gate, judged on this thread (see [`command_gate_with`]).
    #[cfg(unix)]
    fn gate_here(input: &HookInput) -> HookOutput {
        command_gate_with(input, |job| Some(job()))
    }

    #[cfg(unix)]
    fn install_scoped_manvi(
        dir: &Path,
        requests: &Path,
        serial: &crate::harness::sidecar::SidecarTestGuard,
    ) -> crate::harness::sidecar::TestBinaryBinding {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("scoped-manvi");
        let body = FAKE_SCOPED_MANVI.replace("@REQUESTS@", &requests.display().to_string());
        std::fs::write(&script, body).expect("write fake sidecar");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        crate::harness::sidecar::bind_test_binary(serial, script.to_string_lossy())
    }

    /// The agent's Bash call is judged against the task its checkout is bound
    /// to. Before, the hook sent no scope at all, so a redirect outside the
    /// plan was measured against no task and came back a demoted allow.
    #[cfg(unix)]
    #[test]
    fn a_task_bound_session_is_refused_a_redirect_outside_its_scope() {
        let (dir, repo, requests) = scoped_hook_fixture(true);
        let serial = crate::harness::sidecar::test_serial();
        let _binary = install_scoped_manvi(dir.path(), &requests, &serial);

        // From a subdirectory: the scope belongs to the checkout, not the cwd.
        let sub = Path::new(&repo).join("docs");
        std::fs::create_dir_all(&sub).expect("subdirectory");
        let output = gate_here(&bash_call(
            sub.to_str().expect("utf8"),
            "echo x > ../docs/elsewhere.md",
        ));

        let sent = std::fs::read_to_string(&requests).unwrap_or_default();
        let request: Value = serde_json::from_str(sent.lines().next().expect("a policy request"))
            .expect("the request is JSON");
        assert_eq!(
            request["params"]["scope"]["task_id"], "TASK-HOOK",
            "the bound task's scope never reached the harness: {request}"
        );
        assert_eq!(
            request["params"]["scope"]["planned_files"][0],
            "src/planned.rs"
        );
        assert_eq!(request["params"]["root"], repo.as_str());

        let rendered = output.render().unwrap_or_default();
        let decision = output
            .hook_specific_output
            .unwrap_or_else(|| panic!("an out-of-scope write must be decided: {rendered}"));
        assert!(
            matches!(decision.permission_decision, Some(PermissionDecision::Deny)),
            "{rendered}"
        );
        assert!(rendered.contains("scope.unplanned"), "{rendered}");
    }

    /// An unbound checkout declares nothing, and the same command keeps the
    /// answer it always had.
    #[cfg(unix)]
    #[test]
    fn an_unbound_session_sends_no_scope_and_stays_silent() {
        let (dir, repo, requests) = scoped_hook_fixture(false);
        let serial = crate::harness::sidecar::test_serial();
        let _binary = install_scoped_manvi(dir.path(), &requests, &serial);

        let output = gate_here(&bash_call(&repo, "echo x > docs/elsewhere.md"));
        let sent = std::fs::read_to_string(&requests).unwrap_or_default();
        assert!(!sent.is_empty(), "the command was not judged at all");
        assert!(
            !sent.contains("\"scope\""),
            "an unbound checkout declared a scope: {sent}"
        );
        assert!(output.is_silent(), "{:?}", output.render());
    }

    /// A repository GitPulse is not trusted in has no binding it may read —
    /// resolving one runs Git, which the trust gate refuses. Failing closed
    /// there denied every Bash call in every repository the person never
    /// opened in GitPulse (`hook_protocol_stress` caught it as a deny with no
    /// harness installed). It is judged as it was before scopes: unscoped.
    #[cfg(unix)]
    #[test]
    fn an_untrusted_repository_is_judged_without_scope_not_refused() {
        let (dir, repo, requests) = scoped_hook_fixture(false);
        crate::repository_trust::revoke(&repo).expect("revoke trust");
        let serial = crate::harness::sidecar::test_serial();
        let _binary = install_scoped_manvi(dir.path(), &requests, &serial);

        let output = gate_here(&bash_call(&repo, "echo x > docs/elsewhere.md"));
        assert!(output.is_silent(), "{:?}", output.render());
        let sent = std::fs::read_to_string(&requests).unwrap_or_default();
        assert!(!sent.is_empty(), "the command was not judged at all");
        assert!(!sent.contains("\"scope\""), "{sent}");
    }

    /// Outside any repository there is no binding to look up. Asking the ledger
    /// anyway would fail closed and refuse every command an agent ran there.
    #[cfg(unix)]
    #[test]
    fn a_session_outside_any_repository_is_not_refused_for_having_no_binding() {
        let (dir, _repo, requests) = scoped_hook_fixture(false);
        let serial = crate::harness::sidecar::test_serial();
        let _binary = install_scoped_manvi(dir.path(), &requests, &serial);
        let elsewhere = tempfile::tempdir().expect("non-repository directory");

        let output = gate_here(&bash_call(
            elsewhere.path().to_str().expect("utf8"),
            "echo x > notes.txt",
        ));
        assert!(output.is_silent(), "{:?}", output.render());
        let sent = std::fs::read_to_string(&requests).unwrap_or_default();
        assert!(!sent.contains("\"scope\""), "{sent}");
    }

    #[test]
    fn a_bash_call_with_no_command_reports_a_non_gate_rather_than_staying_silent() {
        let output = run_command_gate(&HookInput {
            hook_event_name: PRE_TOOL_USE.to_string(),
            tool_name: "Bash".to_string(),
            cwd: "/repo".to_string(),
            ..Default::default()
        });
        assert!(!output.is_silent());
        assert!(output.hook_specific_output.is_none());
    }

    /* ── session-brief ────────────────────────────────────────────────────── */

    #[test]
    fn the_brief_reports_branch_dirt_other_worktrees_collisions_and_both_stores() {
        let brief = session_brief(&snapshot_fixture(), "/repo", "startup");
        assert!(brief.contains("startup"));
        assert!(brief.contains("branch here: main"));
        assert!(brief.contains("uncommitted here: 3 file(s)"));
        assert!(brief.contains("worktrees: 2 total, 2 of 2 scanned, 1 with uncommitted work"));
        assert!(brief.contains("feature"));
        assert!(brief.contains("session feature-a1b2"));
        assert!(brief.contains("collisions: 1 file(s)"));
        assert!(brief.contains("ledger: recording"));
        assert!(brief.contains("11294 symbols"));
    }

    /// Two sessions in the worktree the brief is read from, and a kind that
    /// could not be observed: the line names both, and the shared worktree is
    /// called out even though no file is dirty in two worktrees.
    #[test]
    fn the_brief_names_live_sessions_here_and_unknown_kinds() {
        use crate::insights::{LiveKindStatus, LiveSession, LiveWorktree, SharedWorktree};
        let mut snapshot = snapshot_fixture();
        let session = |pid| LiveSession {
            kind: "claude".into(),
            pid,
            entrypoint: "cli".into(),
            status: "busy".into(),
            cwd: "/repo".into(),
        };
        let kind = |kind: &str, ok, sessions| LiveKindStatus {
            kind: kind.into(),
            ok,
            error: if ok {
                String::new()
            } else {
                "no registry".into()
            },
            sessions,
            unverified: 0,
            truncated: false,
        };
        snapshot.agents.live = crate::insights::LiveSessionFacet {
            ok: false,
            sessions: 2,
            kinds: vec![kind("claude", true, 2), kind("codex", false, 0)],
            worktrees: vec![LiveWorktree {
                path: "/repo".into(),
                sessions: vec![session(11), session(12)],
            }],
        };
        snapshot.collisions.shared_worktrees = vec![SharedWorktree {
            path: "/repo".into(),
            branch: Some("main".into()),
            sessions: vec![session(11), session(12)],
            files: vec!["a.rs".into(), "b.rs".into()],
            scanned: true,
            truncated: false,
        }];
        let brief = session_brief(&snapshot, "/repo", "startup");
        assert!(
            brief.contains(
                "live agent sessions: at least 2 in this repository (claude 2, codex unknown), 2 in this worktree"
            ),
            "{brief}"
        );
        assert!(
            brief.contains("! 2 live sessions share /repo: 2 dirty file(s) open to all of them"),
            "{brief}"
        );
    }

    #[test]
    fn the_brief_distinguishes_stale_and_unverified_navigation() {
        let mut snapshot = snapshot_fixture();
        snapshot.codeintel.is_fresh = Some(false);
        snapshot.codeintel.freshness_reason = Some("source tree differs".into());
        let brief = session_brief(&snapshot, "/repo", "startup");
        assert!(
            brief.contains("freshness not established (source tree differs)"),
            "{brief}"
        );
        assert!(
            brief.contains("11294 symbols"),
            "stale navigation is still usable"
        );
        snapshot.codeintel.is_fresh = None;
        snapshot.codeintel.freshness_reason = None;
        let brief = session_brief(&snapshot, "/repo", "startup");
        assert!(brief.contains("freshness UNVERIFIED"), "{brief}");
    }

    #[test]
    fn a_brief_run_from_a_linked_worktree_reports_its_own_branch_not_the_main_ones() {
        let brief = session_brief(
            &snapshot_fixture(),
            "/repo/.claude/worktrees/feature",
            "startup",
        );
        assert!(brief.contains("branch here: claude/feature"));
        assert!(brief.contains("main worktree branch: main"));
    }

    #[test]
    fn a_failed_facet_is_briefed_as_unavailable_and_never_as_zero() {
        let mut snapshot = snapshot_fixture();
        snapshot.changes = ChangesFacet {
            ok: false,
            error: "git status failed".to_string(),
            files: 0,
            staged: 0,
            unstaged: 0,
            untracked: 0,
            conflicted: 0,
            additions: 0,
            deletions: 0,
            churn_warnings: 0,
            churn_overflowed: false,
            truncated: false,
        };
        snapshot.collisions = CollisionRisk {
            ok: false,
            error: "worktree list failed".to_string(),
            overlapping_files: 0,
            worktrees_involved: 0,
            scanned_worktrees: 0,
            unscanned_worktrees: 0,
            failed_worktrees: 0,
            truncated: false,
            items: Vec::new(),
            shared_worktree_files: 0,
            shared_worktrees: Vec::new(),
            sessions_ok: true,
            sessions_error: String::new(),
        };
        snapshot.ledger.recording = false;
        snapshot.ledger.error = "database locked".to_string();
        snapshot.codeintel.available = false;
        snapshot.codeintel.reason = Some("no index".to_string());

        let brief = session_brief(&snapshot, "/repo", "startup");
        assert!(brief.contains("uncommitted here: UNAVAILABLE (git status failed)"));
        assert!(brief.contains("collisions: UNAVAILABLE (worktree list failed)"));
        assert!(brief.contains("ledger: NOT recording (database locked)"));
        assert!(brief.contains("code graph: UNAVAILABLE (no index)"));
        assert!(
            !brief.contains("uncommitted here: 0 file(s)"),
            "a facet that did not run must never be briefed as a zero"
        );
    }

    #[test]
    fn a_snapshot_that_hit_its_deadline_says_so_before_anything_it_reports() {
        let mut snapshot = snapshot_fixture();
        snapshot.deadline_expired = true;
        let brief = session_brief(&snapshot, "/repo", "startup");
        assert!(brief.contains("hit its deadline"));
        let note = brief.find("deadline").expect("the note is present");
        let collisions = brief.find("collisions:").expect("collisions are reported");
        assert!(
            note < collisions,
            "the qualifier must precede what it qualifies"
        );
    }

    #[test]
    fn worktrees_nobody_measured_are_counted_apart_from_worktrees_measured_clean() {
        let mut snapshot = snapshot_fixture();
        snapshot.worktrees.count = 9;
        snapshot.worktrees.scanned = 4;
        snapshot.worktrees.dirty_unknown = 5;
        let brief = session_brief(&snapshot, "/repo", "startup");
        assert!(brief.contains("4 of 9 scanned"));
        assert!(
            brief.contains("5 unmeasured"),
            "an unmeasured worktree must not be folded into the clean ones"
        );
    }

    #[test]
    fn a_branch_that_could_not_be_established_reads_differently_from_a_detached_head() {
        let mut detached = snapshot_fixture();
        detached.branch = None;
        detached.branch_ok = true;
        let mut unknown = snapshot_fixture();
        unknown.branch = None;
        unknown.branch_ok = false;

        let here = "/repo/.claude/worktrees/feature";
        let detached_brief = session_brief(&detached, here, "startup");
        let unknown_brief = session_brief(&unknown, here, "startup");
        assert!(detached_brief.contains("detached or bare"));
        assert!(unknown_brief.contains("UNKNOWN (not established)"));
        assert_ne!(detached_brief, unknown_brief);
    }

    #[test]
    fn the_brief_is_capped_and_says_so_when_it_cuts() {
        let mut snapshot = snapshot_fixture();
        snapshot.worktrees.items = (0..400)
            .map(|i| WorktreeSummary {
                path: format!("/repo/wt/{i}"),
                name: format!("worktree-with-a-long-name-{i}"),
                branch: Some(format!("branch/{i}")),
                is_main: false,
                is_bare: false,
                dirty_files: Some(9),
                agent_kind: "claude".to_string(),
                session_slug: format!("session-{i}"),
                operation_kind: String::new(),
                is_detached: false,
                operation_ok: true,
            })
            .collect();
        let brief = session_brief(&snapshot, "/repo", "startup");
        assert!(brief.len() <= BRIEF_LIMIT);
        assert!(
            brief.ends_with("… truncated"),
            "a cut brief must admit the cut"
        );
    }

    #[test]
    fn the_brief_is_delivered_as_additional_context_under_the_session_start_event_name() {
        let output = HookOutput {
            hook_specific_output: Some(HookSpecificOutput {
                hook_event_name: SESSION_START.to_string(),
                permission_decision: None,
                permission_decision_reason: None,
                additional_context: Some("brief".to_string()),
            }),
            system_message: None,
        };
        let rendered = output.render().expect("renders");
        assert!(rendered.contains("\"hookEventName\":\"SessionStart\""));
        assert!(rendered.contains("\"additionalContext\":\"brief\""));
        assert!(!rendered.contains("permissionDecision"));
    }

    /// Cursor sessionStart injects `additional_context`. Claude nested
    /// `hookSpecificOutput` parses as JSON and is then ignored, so a successful
    /// brief never reached the agent.
    #[test]
    fn a_cursor_session_brief_renders_top_level_additional_context() {
        let output = HookOutput {
            hook_specific_output: Some(HookSpecificOutput {
                hook_event_name: SESSION_START.to_string(),
                permission_decision: None,
                permission_decision_reason: None,
                additional_context: Some("repo brief".to_string()),
            }),
            system_message: None,
        };
        let rendered = output
            .render_for_host(true)
            .expect("Cursor brief must emit");
        let value: Value = serde_json::from_str(&rendered).expect("JSON");
        assert_eq!(value["additional_context"], "repo brief");
        assert!(value.get("hookSpecificOutput").is_none());
        assert!(value.get("permission").is_none());
        assert_eq!(
            output.render_for_host(false).as_deref(),
            output.render().as_deref(),
            "Claude must keep the nested document"
        );
    }

    /// Cursor preToolUse is a permission hook. Claude's nested
    /// `permissionDecision` is a schema mismatch that blocks the tool.
    #[test]
    fn a_cursor_permission_decision_renders_nothing() {
        let output = HookOutput {
            hook_specific_output: Some(decision(
                PRE_TOOL_USE,
                PermissionDecision::Deny,
                "other worktree holds this file".to_string(),
            )),
            system_message: None,
        };
        assert_eq!(
            output.render_for_host(true),
            None,
            "Cursor must fail open rather than block on a Claude permission document"
        );
        assert!(
            output.render_for_host(false).is_some(),
            "Claude must still receive deny/ask"
        );
    }

    /// Notices (`systemMessage`) already parse on Cursor sessionStart. Keep
    /// them: they are warnings, not decisions.
    #[test]
    fn a_cursor_notice_still_renders_system_message() {
        let output =
            HookOutput::notice("GitPulse could not brief this session: no cwd was supplied.");
        let rendered = output.render_for_host(true).expect("notice still emits");
        let value: Value = serde_json::from_str(&rendered).expect("JSON");
        assert_eq!(
            value["systemMessage"],
            "GitPulse could not brief this session: no cwd was supplied."
        );
        assert!(value.get("additional_context").is_none());
    }

    #[test]
    fn a_null_cursor_version_is_not_a_cursor_payload() {
        let input = parse_input(
            &json!({
                "cwd": "/repo",
                "cursor_version": null,
            })
            .to_string(),
        )
        .expect("parses");
        assert!(!input.is_cursor);
        assert_eq!(input.cwd, "/repo");
    }

    #[test]
    fn a_session_started_outside_a_repository_says_nothing_at_all() {
        let output = run_session_brief(&HookInput {
            hook_event_name: SESSION_START.to_string(),
            cwd: "/".to_string(),
            source: "startup".to_string(),
            ..Default::default()
        });
        assert!(output.is_silent());
    }

    /* ── wire shape and dispatch ──────────────────────────────────────────── */

    #[test]
    fn the_rendered_json_uses_the_documented_camel_case_field_names() {
        let output = HookOutput {
            hook_specific_output: Some(decision(
                PRE_TOOL_USE,
                PermissionDecision::Deny,
                "no".to_string(),
            )),
            system_message: Some("note".to_string()),
        };
        let rendered = output.render().expect("renders");
        let value: Value = serde_json::from_str(&rendered).expect("valid JSON");
        assert_eq!(value["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(value["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(
            value["hookSpecificOutput"]["permissionDecisionReason"],
            "no"
        );
        assert_eq!(value["systemMessage"], "note");
        // Anything Claude Code would reject or act on unexpectedly stays absent.
        assert!(value.get("continue").is_none());
        assert!(value.get("decision").is_none());
    }

    #[test]
    fn a_silent_answer_renders_no_bytes_at_all_rather_than_an_empty_object() {
        assert_eq!(HookOutput::silent().render(), None);
    }

    #[test]
    fn an_unknown_subcommand_is_an_error_the_binary_reports_not_a_silent_no_op() {
        let err = dispatch("collision-gaurd", None, &HookInput::default())
            .expect_err("a typo must not pass for a check");
        assert!(err.contains("unknown subcommand"));
        for name in SUBCOMMANDS {
            assert!(err.contains(name), "the error should list {name}");
        }
    }

    /// Subcommands that *report* something rather than *decide* something.
    ///
    /// They may legitimately fail when the thing they report to is absent —
    /// GitPulse closed, no socket — so `dispatch` is only asked to route them.
    /// Their own behaviour is covered by the notify tests below. Listed rather
    /// than inferred so adding one is a deliberate act.
    const REPORTERS: [&str; 1] = ["notify"];

    #[test]
    fn every_advertised_subcommand_dispatches() {
        for name in SUBCOMMANDS {
            let outcome = dispatch(name, None, &HookInput::default());
            if REPORTERS.contains(&name) {
                let error = outcome.expect_err("a reporter with no argument has nothing to send");
                assert!(
                    !error.contains("unknown subcommand"),
                    "{name} is advertised but was not routed: {error}"
                );
                continue;
            }
            assert!(
                outcome.is_ok(),
                "{name} is advertised but does not dispatch"
            );
        }
    }

    #[test]
    fn a_reporter_is_a_subcommand_the_dispatcher_knows() {
        for name in REPORTERS {
            assert!(SUBCOMMANDS.contains(&name), "{name} is not advertised");
        }
    }

    /* ── notify ───────────────────────────────────────────────────────────── */

    /// Serialises the notify tests' environment overrides.
    ///
    /// These two variables are not hypothetical in this process: running the
    /// suite from inside a GitPulse agent tab sets both, so a test that read
    /// the ambient value would pass on a laptop and fail in CI, or the reverse.
    /// Every notify test pins both.
    static NOTIFY_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn notify_serial() -> std::sync::MutexGuard<'static, ()> {
        NOTIFY_ENV
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn notify_input() -> HookInput {
        HookInput {
            hook_event_name: "Notification".into(),
            session_id: "claude-abc".into(),
            cwd: "/Users/me/GitPulse".into(),
            message: "Claude needs your permission to use Bash".into(),
            ..HookInput::default()
        }
    }

    /// Captures what would have gone down the socket.
    fn captured(
        event: &str,
        input: &HookInput,
    ) -> (
        Result<HookOutput, String>,
        std::sync::Arc<std::sync::Mutex<Option<String>>>,
    ) {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let sink = seen.clone();
        let result = run_notify(Some(event), input, move |_, payload| {
            *sink.lock().unwrap() = Some(payload.to_owned());
            Ok(())
        });
        (result, seen)
    }

    /// The report a hook sends and the report the socket accepts are two
    /// halves of one protocol that live in different modules. This asserts
    /// they are the same protocol by feeding one to the other, which a schema
    /// written twice could never do.
    #[test]
    fn what_the_hook_sends_is_what_the_socket_accepts() {
        let serial = notify_serial();
        let _env = crate::test_support::env::bind_env(&serial)
            .set(AGENT_KIND_ENV, "claude")
            .set(crate::alerts::bridge::SESSION_ENV, "term-4-1")
            .set(crate::alerts::bridge::SOCKET_ENV, "/tmp/gitpulse-test.sock");
        let (result, seen) = captured("permission_prompt", &notify_input());
        assert!(result.is_ok(), "{result:?}");
        let payload = seen.lock().unwrap().clone().expect("a report was sent");
        let notice =
            crate::alerts::bridge::parse_report(payload.as_bytes()).expect("the socket accepts it");
        assert_eq!(notice.key, "term-4-1");
        assert_eq!(notice.label, "Claude Code");
        assert_eq!(notice.place.as_deref(), Some("GitPulse"));
        assert_eq!(notice.reason(), Some("needs your permission"));
        assert_eq!(
            notice.detail.as_deref(),
            Some("Claude needs your permission to use Bash")
        );
    }

    #[test]
    fn every_event_the_plugin_can_route_produces_an_acceptable_report() {
        let serial = notify_serial();
        let _env = crate::test_support::env::bind_env(&serial)
            .set(AGENT_KIND_ENV, "codex")
            .set(crate::alerts::bridge::SESSION_ENV, "term-4-1")
            .set(crate::alerts::bridge::SOCKET_ENV, "/tmp/gitpulse-test.sock");
        for event in crate::alerts::bridge::EVENTS {
            let (result, seen) = captured(event.name, &notify_input());
            assert!(result.is_ok(), "{}: {result:?}", event.name);
            let payload = seen.lock().unwrap().clone().expect("a report was sent");
            let notice =
                crate::alerts::bridge::parse_report(payload.as_bytes()).unwrap_or_else(|e| {
                    panic!("{} produced a report the socket refused: {e}", event.name)
                });
            assert_eq!(notice.reason(), Some(event.phrase));
            assert_eq!(notice.label, "Codex");
        }
    }

    #[test]
    fn an_event_the_manifest_and_this_binary_disagree_about_is_an_error() {
        let serial = notify_serial();
        let _env = crate::test_support::env::bind_env(&serial)
            .set(crate::alerts::bridge::SOCKET_ENV, "/tmp/gitpulse-test.sock");
        // The failure this catches is a plugin manifest updated without the
        // binary, which otherwise presents as "notifications stopped working".
        for event in ["", "Notification", "permission-prompt", "agent_completed "] {
            let result = run_notify(Some(event), &notify_input(), |_, _| {
                panic!("an unknown event reached the socket")
            });
            let error = result.expect_err(&format!("{event:?} was accepted"));
            assert!(error.contains("unknown notify event"), "{error}");
        }
        assert!(run_notify(None, &notify_input(), |_, _| Ok(())).is_err());
    }

    #[test]
    fn a_session_outside_gitpulse_still_reports_and_identifies_itself() {
        let serial = notify_serial();
        let _env = crate::test_support::env::bind_env(&serial)
            .remove(AGENT_KIND_ENV)
            .remove(crate::alerts::bridge::SESSION_ENV)
            .set(crate::alerts::bridge::SOCKET_ENV, "/tmp/gitpulse-test.sock");
        let (result, seen) = captured("agent_completed", &notify_input());
        assert!(result.is_ok(), "{result:?}");
        let payload = seen.lock().unwrap().clone().expect("a report was sent");
        assert!(
            !payload.contains("\"session\""),
            "a session key was invented: {payload}"
        );
        let notice = crate::alerts::bridge::parse_report(payload.as_bytes()).unwrap();
        assert_eq!(notice.label, "Claude Code");
        assert!(notice.key.starts_with("hook-claude-"), "{}", notice.key);
    }

    #[test]
    fn an_agent_kind_the_socket_does_not_know_is_not_forwarded_as_one() {
        let serial = notify_serial();
        let _env = crate::test_support::env::bind_env(&serial)
            .set(AGENT_KIND_ENV, "not-an-agent")
            .remove(crate::alerts::bridge::SESSION_ENV)
            .set(crate::alerts::bridge::SOCKET_ENV, "/tmp/gitpulse-test.sock");
        let (_, seen) = captured("error", &notify_input());
        let payload = seen.lock().unwrap().clone().unwrap();
        assert!(crate::alerts::bridge::parse_report(payload.as_bytes()).is_ok());
    }

    #[test]
    fn a_socket_that_never_answers_does_not_hold_the_users_turn() {
        let serial = notify_serial();
        let _env = crate::test_support::env::bind_env(&serial)
            .set(crate::alerts::bridge::SOCKET_ENV, "/tmp/gitpulse-test.sock");
        let started = std::time::Instant::now();
        let error = run_notify(Some("idle_prompt"), &notify_input(), |_, _| {
            thread::sleep(Duration::from_secs(30));
            Ok(())
        })
        .expect_err("a hung socket must not report success");
        assert!(error.contains("deadline"), "{error}");
        assert!(
            started.elapsed() < NOTIFY_BUDGET * 4,
            "waited {:?}",
            started.elapsed()
        );
    }

    /// The shipped plugin manifest and this binary have to agree twice over:
    /// on which subcommands exist, and on which notification events each
    /// `notify` entry passes. A manifest matcher that reached a `notify` word
    /// this binary does not know would fail at the moment the user most needs
    /// it, with nothing in the UI to explain why — so it fails here instead.
    #[test]
    fn the_shipped_manifest_asks_only_for_events_this_binary_serves() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the crate has a parent directory")
            .join("plugins/gitpulse/hooks/hooks.json");
        let parsed: Value = serde_json::from_str(
            &std::fs::read_to_string(&manifest)
                .unwrap_or_else(|e| panic!("{}: {e}", manifest.display())),
        )
        .expect("hooks.json is valid JSON");
        let events = parsed["hooks"]
            .as_object()
            .expect("hooks.json has a hooks object");
        let known: Vec<&str> = crate::alerts::bridge::EVENTS
            .iter()
            .map(|event| event.name)
            .collect();

        let mut notify_entries = 0;
        for (event_name, groups) in events {
            for group in groups.as_array().into_iter().flatten() {
                let matcher = group["matcher"].as_str().unwrap_or_default();
                for handler in group["hooks"].as_array().into_iter().flatten() {
                    let command = handler["command"].as_str().unwrap_or_default();
                    let mut words = command.split_whitespace();
                    assert_eq!(
                        words.next(),
                        Some("gitpulse-hook"),
                        "{event_name} spawns something else: {command}"
                    );
                    let subcommand = words.next().unwrap_or_default();
                    assert!(
                        SUBCOMMANDS.contains(&subcommand),
                        "{event_name} asks for an unknown subcommand: {command}"
                    );
                    if subcommand != "notify" {
                        assert_eq!(words.next(), None, "{command} passes an unread argument");
                        continue;
                    }
                    notify_entries += 1;
                    let argument = words.next().unwrap_or_default();
                    assert!(
                        known.contains(&argument),
                        "{event_name}/{matcher} reports '{argument}', which the socket refuses; known: {known:?}"
                    );
                    assert_eq!(words.next(), None, "{command} passes a third word");
                    // A `Notification` entry's matcher is what decides the
                    // sentence the user reads. If the matcher and the argument
                    // ever drift, the banner names the wrong reason and nothing
                    // else in the system can tell.
                    if event_name == "Notification" {
                        assert_eq!(
                            matcher, argument,
                            "the matcher and the reported reason disagree"
                        );
                    }
                }
            }
        }
        assert!(
            notify_entries >= known.len(),
            "{notify_entries} manifest entries for {} known events: an event the binary \
             serves that nothing routes to it is a sentence no user can ever see",
            known.len()
        );
    }

    /// What the host sends for one tool call, in `PermissionRequest` and
    /// again in `PostToolUse`.
    fn tool_event(event: &str, input: Value) -> HookInput {
        HookInput::from_value(&json!({
            "hook_event_name": event,
            "session_id": "claude-abc",
            "cwd": "/Users/me/GitPulse",
            "tool_name": "Bash",
            "tool_input": input,
        }))
    }

    #[test]
    fn one_tool_call_is_named_the_same_way_when_asked_for_and_when_run() {
        let asked = tool_event(
            "PermissionRequest",
            json!({"command":"cargo test","description":"Run tests","timeout":120000}),
        );
        // The same call, its keys serialized in another order.
        let ran = tool_event(
            "PostToolUse",
            json!({"timeout":120000,"description":"Run tests","command":"cargo test"}),
        );
        let other = tool_event("PostToolUse", json!({"command":"cargo build"}));
        assert_eq!(asked.tool_subject.len(), 16);
        assert_eq!(asked.tool_subject, ran.tool_subject);
        assert_ne!(asked.tool_subject, other.tool_subject);
        assert_eq!(asked.tool_summary, "Bash: cargo test");
        // No tool, no subject: a Notification is about no call in particular.
        assert_eq!(notify_input().tool_subject, "");
    }

    #[test]
    fn a_tool_summary_is_one_bounded_line() {
        let long = tool_event(
            "PermissionRequest",
            json!({"command": format!("echo start\n{}", "x".repeat(4000))}),
        );
        assert_eq!(long.tool_summary, "Bash: echo start");
        let wide = tool_event("PermissionRequest", json!({"command": "y".repeat(4000)}));
        assert_eq!(wide.tool_summary.chars().count(), TOOL_SUMMARY_CHARS);
        let mcp = HookInput::from_value(
            &json!({"tool_name":"mcp__github__create_issue","tool_input":{"title":7}}),
        );
        assert_eq!(mcp.tool_summary, "mcp__github__create_issue");
    }

    #[test]
    fn a_permission_request_and_its_result_reach_the_socket_as_one_subject() {
        let serial = notify_serial();
        let _env = crate::test_support::env::bind_env(&serial)
            .set(AGENT_KIND_ENV, "claude")
            .set(crate::alerts::bridge::SESSION_ENV, "term-4-1")
            .set(crate::alerts::bridge::SOCKET_ENV, "/tmp/gitpulse-test.sock");
        let report = |event: &str, input: &HookInput| {
            let (result, seen) = captured(event, input);
            assert!(result.is_ok(), "{event}: {result:?}");
            let payload = seen.lock().unwrap().clone().expect("a report was sent");
            crate::alerts::bridge::parse_report(payload.as_bytes()).expect("accepted")
        };
        let call = json!({"command":"cargo test"});
        let asked = report(
            "permission_request",
            &tool_event("PermissionRequest", call.clone()),
        );
        let ran = report("tool_finished", &tool_event("PostToolUse", call));
        assert_eq!(asked.detail.as_deref(), Some("Bash: cargo test"));
        assert!(asked.subject.is_some());
        assert_eq!(asked.subject, ran.subject);
        assert!(!asked.subagent);
        // A resolution carries no text for the board to show.
        assert_eq!(ran.detail, None);

        let mut inside = tool_event("PostToolUse", json!({"command":"ls"}));
        inside.agent_id = "agent-7".into();
        assert!(report("tool_finished", &inside).subagent);
    }

    #[test]
    fn a_state_outside_a_gitpulse_terminal_never_touches_the_socket() {
        let serial = notify_serial();
        let _env = crate::test_support::env::bind_env(&serial)
            .remove(crate::alerts::bridge::SESSION_ENV)
            .set(crate::alerts::bridge::SOCKET_ENV, "/tmp/gitpulse-test.sock");
        for event in crate::alerts::bridge::EVENTS
            .iter()
            .filter(|e| e.role != crate::alerts::bridge::Role::Banner)
        {
            let result = run_notify(Some(event.name), &notify_input(), |_, _| {
                panic!("a state with no session reached the socket")
            });
            let output = result.unwrap_or_else(|e| panic!("{}: {e}", event.name));
            assert!(output.is_silent());
        }
    }

    #[test]
    fn a_long_final_message_is_cut_rather_than_costing_the_report() {
        let serial = notify_serial();
        let _env = crate::test_support::env::bind_env(&serial)
            .set(crate::alerts::bridge::SESSION_ENV, "term-4-1")
            .set(crate::alerts::bridge::SOCKET_ENV, "/tmp/gitpulse-test.sock");
        let mut input = notify_input();
        input.message.clear();
        // Multi-byte, so a cut at a byte count lands mid-character.
        input.last_assistant_message = "é".repeat(20_000);
        let (result, seen) = captured("turn_finished", &input);
        assert!(result.is_ok(), "{result:?}");
        let payload = seen.lock().unwrap().clone().unwrap();
        assert!(payload.len() <= crate::alerts::bridge::MAX_REPORT_BYTES);
        let notice = crate::alerts::bridge::parse_report(payload.as_bytes())
            .expect("the socket accepts a clipped report");
        assert!(notice.detail.unwrap().starts_with("éé"));
        // StopFailure has no message; its error type says what happened.
        let mut failed = notify_input();
        failed.message.clear();
        failed.error = "rate_limit".into();
        let (_, seen) = captured("error", &failed);
        let notice =
            crate::alerts::bridge::parse_report(seen.lock().unwrap().clone().unwrap().as_bytes())
                .unwrap();
        assert_eq!(notice.detail.as_deref(), Some("rate_limit"));
    }

    #[test]
    fn a_refused_or_garbled_answer_is_not_reported_as_delivered() {
        assert_eq!(acknowledged(b"{\"ok\":true}\n"), Ok(()));
        let refused = acknowledged(b"{\"ok\":false}\n").unwrap_err();
        assert!(refused.contains("refused"), "{refused}");
        for garbage in [&b""[..], b"ok", b"{\"ok\":1}", b"{\"ok\":tr"] {
            assert!(acknowledged(garbage).is_err(), "{garbage:?}");
        }
    }

    /// The real send against a real listener: the answer decides the result.
    #[cfg(unix)]
    #[test]
    fn notify_send_reports_what_the_socket_answered() {
        use std::io::{Read, Write};
        let base = std::fs::canonicalize("/tmp").unwrap();
        for (answer, ok) in [
            (&b"{\"ok\":true}\n"[..], true),
            (b"{\"ok\":false}\n", false),
        ] {
            let path = base.join(format!("gph-ack-{}-{ok}.sock", std::process::id()));
            let _ = std::fs::remove_file(&path);
            let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut seen = Vec::new();
                stream.read_to_end(&mut seen).unwrap();
                stream.write_all(answer).unwrap();
                seen
            });
            let result = notify_send(&path, r#"{"v":1,"event":"error"}"#);
            assert_eq!(server.join().unwrap(), br#"{"v":1,"event":"error"}"#);
            assert_eq!(result.is_ok(), ok, "{result:?}");
            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    fn a_report_never_produces_a_decision_or_a_transcript_warning() {
        let serial = notify_serial();
        let _env = crate::test_support::env::bind_env(&serial)
            .set(crate::alerts::bridge::SOCKET_ENV, "/tmp/gitpulse-test.sock");
        let (result, _) = captured("agent_completed", &notify_input());
        let output = result.unwrap();
        assert!(output.is_silent());
        assert_eq!(output.render(), None);
    }

    #[test]
    fn identity_reports_this_build_and_every_subcommand_it_serves() {
        let identity = identity();
        let mut lines = identity.lines();
        // The doctor parses these two lines. A reformat that drops either half
        // turns a staleness check into an unresponsive binary.
        assert_eq!(
            lines.next(),
            Some(format!("gitpulse-hook {VERSION}").as_str())
        );
        let listed = lines.next().expect("identity names its subcommands");
        let listed = listed
            .strip_prefix("subcommands: ")
            .expect("the subcommand line keeps its prefix");
        let served: Vec<&str> = listed.split(", ").collect();
        // Derived from SUBCOMMANDS rather than spelled out, so a hook added to
        // the dispatcher is advertised without anyone editing this test.
        assert_eq!(served, SUBCOMMANDS.to_vec());
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn an_identity_flag_is_never_mistaken_for_a_subcommand() {
        // The binary checks IDENTITY_FLAGS before SUBCOMMANDS. If the two sets
        // ever overlapped, asking for a version would run a repository scan and
        // print a hook decision onto a channel nobody is reading as one.
        for flag in IDENTITY_FLAGS {
            assert!(
                !SUBCOMMANDS.contains(&flag),
                "{flag} is both an identity flag and a subcommand"
            );
            assert!(
                dispatch(flag, None, &HookInput::default()).is_err(),
                "{flag} must not dispatch as a hook"
            );
        }
    }

    #[test]
    fn a_truncated_string_is_cut_on_a_character_boundary_and_marked() {
        let text = "é".repeat(100);
        let cut = truncate_marked(text, 40);
        assert!(cut.len() <= 40);
        assert!(cut.ends_with("… truncated"));
    }

    #[test]
    fn the_budget_gives_up_rather_than_waiting_on_work_that_will_not_finish() {
        let slow = within_budget(Duration::from_millis(20), || {
            thread::sleep(Duration::from_secs(30));
            "never"
        });
        assert_eq!(
            slow, None,
            "the hook must stop waiting, not hang the editor"
        );
        assert_eq!(within_budget(Duration::from_secs(5), || 7), Some(7));
    }
}
