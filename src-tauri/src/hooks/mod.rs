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
use serde_json::Value;

use crate::engine::agent_session_slug;
use crate::engine::git_cli::find_git_root;
use crate::harness::{self, PolicyStatus, PolicyVerdict};
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
}

impl HookInput {
    /// Reads the fields we use out of one already-parsed hook payload.
    pub fn from_value(value: &Value) -> Self {
        let tool_input = value.get("tool_input");
        // `file_path` is the documented field for Write and Edit and is always
        // absolute. NotebookEdit is not in the documented `tool_input` table, so
        // `notebook_path` is a defensive fallback rather than a verified name:
        // if it is wrong we derive no path and report a non-check, which is the
        // safe direction to be wrong in.
        let file_path = string_at(tool_input, "file_path");
        let file_path = if file_path.is_empty() {
            string_at(tool_input, "notebook_path")
        } else {
            file_path
        };
        HookInput {
            hook_event_name: string_at(Some(value), "hook_event_name"),
            session_id: string_at(Some(value), "session_id"),
            cwd: string_at(Some(value), "cwd"),
            tool_name: string_at(Some(value), "tool_name"),
            file_path,
            command: string_at(tool_input, "command"),
            source: string_at(Some(value), "source"),
        }
    }
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
            or_unknown(&facts.error)
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
                facts.partial
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
    reason.push_str("Editing it here will conflict when these branches meet.");
    if !facts.partial.is_empty() {
        reason.push_str(&format!("\n(Scan was incomplete: {}.)", facts.partial));
    }

    HookOutput {
        hook_specific_output: Some(decision(PRE_TOOL_USE, PermissionDecision::Ask, reason)),
        system_message: None,
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
        };
    }

    let others = risk
        .items
        .iter()
        .find(|item| item.path == target)
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
        partial.push(format!(
            "{} worktree(s) could not be read ({})",
            risk.failed_worktrees,
            or_unknown(&risk.error)
        ));
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
pub fn command_gate_decision(verdict: &PolicyVerdict) -> HookOutput {
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
        // waived. They are left silent on purpose: a hook cannot pass the task
        // scope that `harness::guard_command` passes (`scope_for` is private to
        // that module), so an unbound `task.absent` demotion is the *expected*
        // answer for every command here. Reporting it on every Bash call would
        // be noise that trains the reader to ignore this channel, which would
        // cost more honesty than it buys.
        PolicyStatus::Allowed
        | PolicyStatus::Demoted
        | PolicyStatus::Granted
        | PolicyStatus::Widened => HookOutput::silent(),
        PolicyStatus::Blocked => HookOutput::silent(),
    }
}

/// Gathers, then decides. The impure half of command-gate.
pub fn run_command_gate(input: &HookInput) -> HookOutput {
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
    let root = find_git_root(Path::new(&input.cwd))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| input.cwd.clone());

    let command = input.command.clone();
    // No `HostScope`: this process has no bound task, and `harness::scope_for`
    // is private to that module. Without it the ladder stops at `task.absent`,
    // which the host posture demotes to an allow — so this gate reliably
    // catches the hard rungs (force-push, destructive commands) and cannot
    // reach the scope rungs. That is a smaller gate than the desktop app's, and
    // it is a real one.
    let judged = within_budget(BUDGET, move || {
        harness::check_command(&root, &command, None)
    });
    if let Some(verdict) = judged.as_ref() {
        // The verdict is the only record of why this hook stayed silent, and
        // silence is its most common answer. Without this line an operator
        // cannot tell a clean allow from a demoted one, which is the same
        // confusion the rest of this module exists to prevent.
        log::debug!(
            target: "hooks",
            "command gate: status={:?} checked={} rule={} detail_code={} demoted={} degraded={:?}",
            verdict.status,
            verdict.checked,
            verdict.rule,
            verdict.detail_code,
            verdict.demoted,
            verdict.degraded,
        );
    }
    let Some(verdict) = judged else {
        return HookOutput::notice(format!(
            "The MANVI harness did not answer within {}s, so GitPulse could not judge \
             this command. It ran UNGATED.",
            BUDGET.as_secs()
        ));
    };
    command_gate_decision(&verdict)
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
pub const SUBCOMMANDS: [&str; 3] = ["collision-guard", "command-gate", "session-brief"];

/// Routes one parsed payload to its handler.
///
/// An unknown subcommand is an `Err` for the binary to report on stderr; it is
/// deliberately not a silent no-op, because a plugin whose hook name has
/// drifted would otherwise look like a check that ran.
pub fn dispatch(subcommand: &str, input: &HookInput) -> Result<HookOutput, String> {
    match subcommand {
        "collision-guard" => Ok(run_collision_guard(input)),
        "command-gate" => Ok(run_command_gate(input)),
        "session-brief" => Ok(run_session_brief(input)),
        other => Err(format!(
            "unknown subcommand '{other}'; expected one of {}",
            SUBCOMMANDS.join(", ")
        )),
    }
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

/// Never let an empty explanation render as an empty string: "unknown" is a
/// worse answer than a real reason and a much better one than a blank.
fn or_unknown(text: &str) -> String {
    if text.trim().is_empty() {
        "reason unknown".to_string()
    } else {
        text.trim().to_string()
    }
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
        }
    }

    fn party(path: &str, branch: &str) -> CollisionParty {
        CollisionParty {
            path: path.to_string(),
            branch: Some(branch.to_string()),
            agent_kind: "claude".to_string(),
        }
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

    /* ── the honesty invariant ────────────────────────────────────────────── */

    #[test]
    fn a_check_that_could_not_run_is_observably_different_from_a_clean_check() {
        let clean = collision_decision(&CollisionFacts {
            ok: true,
            error: String::new(),
            target: "src/lib.rs".to_string(),
            others: Vec::new(),
            partial: String::new(),
        });
        let failed = collision_decision(&CollisionFacts {
            ok: false,
            error: "/tmp/x is not inside a Git repository".to_string(),
            target: "src/lib.rs".to_string(),
            others: Vec::new(),
            partial: String::new(),
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
        });
        let partial = collision_decision(&CollisionFacts {
            ok: true,
            error: String::new(),
            target: "src/lib.rs".to_string(),
            others: Vec::new(),
            partial: "2 worktree(s) were not scanned".to_string(),
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
        // `ask` serialises to the documented wire value, not the Rust name.
        assert!(output.render().expect("renders").contains("\"ask\""));
    }

    #[test]
    fn the_worktree_doing_the_editing_is_never_reported_as_its_own_collision_party() {
        let risk = clean_risk(vec![CollisionItem {
            path: "src/lib.rs".to_string(),
            worktrees: vec![party("/repo", "main"), party("/repo/wt/feature", "feature")],
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

        let output = command_gate_decision(&verdict);
        let specific = output.hook_specific_output.expect("a block decides");
        assert_eq!(specific.permission_decision, Some(PermissionDecision::Deny));
        assert_eq!(
            specific.permission_decision_reason,
            Some(verdict.refusal()),
            "the reason must be the canonical refusal, not a second rendering of it"
        );
    }

    #[test]
    fn a_clean_allow_renders_nothing_so_the_users_own_permission_rules_decide() {
        assert!(command_gate_decision(&allowed_verdict()).is_silent());
    }

    #[test]
    fn a_gate_that_could_not_answer_says_the_command_ran_ungated() {
        let mut verdict = allowed_verdict();
        verdict.status = PolicyStatus::Unchecked;
        verdict.checked = false;
        verdict.detail = "sidecar timed out".to_string();
        verdict.detail_code = "timeout".to_string();
        assert!(verdict.gate_failed());

        let output = command_gate_decision(&verdict);
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

        let absent = command_gate_decision(&verdict);
        let broken = command_gate_decision(&failed);
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
        let output = command_gate_decision(&verdict);
        assert!(output.hook_specific_output.is_none());
        assert!(output
            .system_message
            .unwrap_or_default()
            .contains("repo_map"));
        assert_ne!(
            command_gate_decision(&verdict).render(),
            command_gate_decision(&allowed_verdict()).render()
        );
    }

    #[test]
    fn a_warned_verdict_carries_the_rule_that_fired_through_to_the_user() {
        let mut verdict = allowed_verdict();
        verdict.status = PolicyStatus::Warned;
        verdict.rule = "command.slow".to_string();
        verdict.reason = "this rewrites history".to_string();
        let message = command_gate_decision(&verdict)
            .system_message
            .expect("a warning must reach the user");
        assert!(message.contains("command.slow"));
        assert!(message.contains("this rewrites history"));
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
        let err = dispatch("collision-gaurd", &HookInput::default())
            .expect_err("a typo must not pass for a check");
        assert!(err.contains("unknown subcommand"));
        for name in SUBCOMMANDS {
            assert!(err.contains(name), "the error should list {name}");
        }
    }

    #[test]
    fn every_advertised_subcommand_dispatches() {
        for name in SUBCOMMANDS {
            assert!(
                dispatch(name, &HookInput::default()).is_ok(),
                "{name} is advertised but does not dispatch"
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
