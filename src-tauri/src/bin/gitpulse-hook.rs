//! GitPulse hook executable.
//!
//! One binary, dispatched by its first argument, reading the host's hook JSON
//! on stdin and writing hook JSON on stdout. Every decision lives in
//! `gitpulse_lib::hooks` so the contract is unit-tested rather than only
//! exercised by spawning this process.
//!
//! Two rules shape this file:
//!
//! * **stdout is the protocol channel.** On a hook invocation nothing but the
//!   hook JSON goes down it. Diagnostics go to stderr, where a hook that exits
//!   0 has them recorded in the host's debug log and shown to nobody else. The
//!   git and harness subprocesses underneath already pipe their own stdout
//!   (`engine::git_cli::git_command`, `harness::sidecar`), so none of them can
//!   leak into ours. The one argument that is not a hook invocation —
//!   `--version`, which no host sends — prints plain text there instead, and
//!   deliberately prints nothing a host could parse as a decision.
//!   The two worktree hooks (`worktree-create`, `worktree-remove`) speak
//!   Claude Code's `WorktreeCreate`/`WorktreeRemove` protocol instead: stdout
//!   carries the created worktree's path and nothing else, and every note goes
//!   to stderr (`hooks::worktree`).
//! * **Exit 0, except for one wake and the worktree hooks.** `deliver`, the
//!   `asyncRewake` `Stop` hook, exits 2 with the message it claimed on stderr:
//!   the documented way to wake the agent
//!   (<https://code.claude.com/docs/en/hooks.md>), on an event with no tool
//!   call to block. The worktree hooks exit with `hooks::worktree`'s codes,
//!   because for those events a non-zero exit is how Claude Code learns that no
//!   worktree was made or removed, and 75 tells the plugin's launcher to fall
//!   back to plain git. Every other path, `deliver`'s included, exits 0.
//!   Exit 2 blocks the tool call whatever the JSON says. A
//!   hook that crashed, timed out or could not read its input must never be the
//!   reason a user's edit is refused, so every failure path here degrades to
//!   "no decision" plus a stderr line. The single exception is a hook the host
//!   *itself* killed: `procguard::exit` reports that signal, which is not exit
//!   2 and so still blocks nothing, and which the host already knows about
//!   because it sent it.

use std::io;

use gitpulse_lib::hooks::{self, IDENTITY_FLAGS, SUBCOMMANDS};

fn main() {
    // A hook runs git for the user's own agent, so it spends the per-user
    // spawn budget the app and the agent servers spend. First, before any
    // spawn builds the gate; this only records the choice and blocks on nothing.
    gitpulse_lib::engine::git_cli::run_process_with_shared_spawn_budget();
    // The one deadline for the whole process starts here, before anything that
    // can block, so every later wait is cut to what the host's timeout leaves.
    hooks::start_deadline();
    let code = run_guarded(decide);
    // The decision is already on stdout. Anything a check abandoned at its
    // deadline is still running: a `git status` or the harness sidecar, in its
    // own process group, able to hold `.git/index.lock` after the hook is gone.
    // `process::exit` runs no destructor, so they are taken down here, as the
    // servers do. Free when nothing is registered; otherwise one `CHILD_GRACE`.
    let sweep = gitpulse_lib::procguard::reap_all(gitpulse_lib::procguard::CHILD_GRACE);
    if !sweep.is_complete() {
        log::warn!(target: "hook", "{}", sweep.describe());
    }
    // Not a bare return: a hook that finished while a caught signal was still
    // being swept would exit out from under it, leaving the git or harness
    // subprocess the sweep was killing.
    gitpulse_lib::procguard::exit(code);
}

/// Runs the hook and returns its exit code, which is 0 when a judging hook
/// panicked. A panic otherwise exits 101: not the blocking 2, but not the
/// promised 0 either, and a host is free to treat any non-zero hook as a
/// failure the user has to look at. The installed panic hook has already logged
/// the fault. A worktree hook that panicked exits non-zero instead
/// ([`hooks::worktree::exit_code_after_panic`]): for those events exit 0 means
/// "made" or "removed", which a panic never did.
fn run_guarded(decide: impl FnOnce() -> i32 + std::panic::UnwindSafe) -> i32 {
    std::panic::catch_unwind(decide).unwrap_or_else(|_| {
        log::error!(target: "hook", "gitpulse-hook panicked; no decision");
        let code = hooks::worktree::exit_code_after_panic();
        if code == 0 && hooks::is_antigravity_env() {
            emit("{}\n".into());
        }
        code
    })
}

/// Runs one hook invocation and returns its exit code, which
/// [`hooks::diagnostic`] fixes at 0 for every outcome it reports.
fn decide() -> i32 {
    gitpulse_lib::logging::init();
    gitpulse_lib::logging::install_panic_hook();
    // A host that decides a hook has taken too long kills it, and the git or
    // harness subprocess a check was in the middle of would otherwise carry
    // on unattached. The cost is one pipe and one thread, well under the
    // startup this file is written to protect. Not logged at startup like the
    // servers do: stderr is the host's debug channel and a line on every
    // matching tool call would drown the ones that matter.
    let _ = gitpulse_lib::procguard::install_signal_handlers();

    let Some(subcommand) = std::env::args().nth(1) else {
        // Exit 2, the host's "block", not 0: a host that does not know the
        // exec form's `args` field (the Windows settings copy uses it) would
        // start this binary with no arguments, and exit 0 there would read as
        // "allowed" for a collision guard or command gate that never ran.
        let reason = format!(
            "gitpulse-hook: no subcommand given; expected one of {}. Blocking: no safety check ran.",
            SUBCOMMANDS.join(", ")
        );
        log::warn!(target: "hook", "{reason}");
        emit_stderr(format!("{reason}\n"));
        return 2;
    };

    // Asked who we are rather than to judge anything. No host sends this — it
    // is what `npm run mcp:doctor` uses to tell an absent or stale hook binary
    // from a current one, which no hook invocation can reveal because silence
    // is a legitimate answer to every one of them. Handled before the
    // subcommand check so it is never reported as an unknown subcommand, and
    // it reads no stdin: there is none to read.
    if IDENTITY_FLAGS.contains(&subcommand.as_str()) {
        emit(hooks::identity());
        return 0;
    }

    if !SUBCOMMANDS.contains(&subcommand.as_str()) {
        log::warn!(target: "hook", "unknown hook subcommand {subcommand:?}; no decision");
        if hooks::is_antigravity_env() {
            emit("{}\n".into());
        }
        return 0;
    }

    // An unparseable payload is reported on stderr and dropped. Guessing at a
    // decision from input we could not read would be the worst of both worlds:
    // neither a check that ran nor an admission that one did not. Exiting 0
    // with no stdout leaves the host's own permission flow exactly as it was
    // (or `"{}"` on Antigravity, which never accepts empty stdout).
    //
    // `notify` takes the matcher's name as a second argument, because a
    // `Notification` payload does not say which matcher routed it. It is taken
    // first because it also decides whether stdin is read at all.
    let argument = std::env::args().nth(2);
    let worktree_hook =
        [hooks::worktree::CREATE, hooks::worktree::REMOVE].contains(&subcommand.as_str());
    if worktree_hook {
        hooks::worktree::begin();
    }
    let mut input = match hooks::read_for(&subcommand, argument.as_deref(), io::stdin()) {
        Ok(input) => input,
        Err(error) => {
            log::warn!(target: "hook", "gitpulse-hook {subcommand}: {error}");
            if !worktree_hook && hooks::is_antigravity_env() {
                emit("{}\n".into());
            }
            // Nothing was done; the launcher can still have plain git do it.
            return if worktree_hook {
                hooks::worktree::EXIT_UNAVAILABLE
            } else {
                0
            };
        }
    };
    let effective_host = hooks::effective_host_kind(input.host, hooks::is_antigravity_env());
    if !input.is_cursor {
        input.host = effective_host;
    }

    if worktree_hook {
        let finished = hooks::worktree::run(&subcommand, &input);
        for note in &finished.notes {
            emit_stderr(format!("gitpulse-hook {subcommand}: {note}\n"));
        }
        if let Some(path) = &finished.path {
            emit(format!("{path}\n"));
        }
        return finished.exit_code;
    }

    match hooks::dispatch(&subcommand, argument.as_deref(), &input) {
        Ok(output) => {
            // GitPulse injects the PreToolUse gates into every session it
            // launches, so a session that also enables the plugin runs them
            // twice. Decisions are left alone; an identical warning is said once.
            let output = if hooks::INJECTED_SUBCOMMANDS.contains(&subcommand.as_str()) {
                hooks::advisory_once::dedupe(&subcommand, &input, output)
            } else {
                output
            };
            if let Some(json) = output.render_for_host_kind(effective_host, &input.hook_event_name)
            {
                emit(format!("{json}\n"));
            }
            0
        }
        // `deliver` claimed a message. The agent reads stderr (the hook is
        // registered `asyncRewake`, and exit 2 is what wakes it), so the text
        // goes there as it is, with no log prefix, and the exit code is the
        // one `diagnostic` fixes for a wake.
        Err(error @ hooks::HookError::Wake(_)) => {
            emit_stderr(format!("{error}"));
            hooks::diagnostic(&error).exit_code
        }
        // One line per invocation, at the level the outcome calls for: a
        // transport this platform does not have is information, not a failure.
        Err(error) => {
            if effective_host == hooks::HostKind::Antigravity {
                emit("{}\n".into());
            }
            let diagnostic = hooks::diagnostic(&error);
            log::log!(target: "hook", diagnostic.level, "{}", diagnostic.line);
            diagnostic.exit_code
        }
    }
}

/// The one place this binary writes to stdout.
///
/// Bounded and deadlined for the same reason every other stdout in this
/// repository is: a host that has gone away leaves a pipe nobody drains, and
/// Rust ignores SIGPIPE, so an unchecked write is a hang rather than an error.
/// A failed write is logged and not retried — there is no second channel to
/// deliver it on, and exiting non-zero would block the user's tool call over a
/// message we could not send.
fn emit(payload: String) {
    let result = gitpulse_lib::output::BoundedOutput::new(
        io::stdout(),
        "gitpulse-hook-stdout",
        hooks::MAX_INPUT_BYTES,
        std::time::Duration::from_secs(1),
    )
    .and_then(|output| output.write(payload.as_bytes()));
    if let Err(error) = result {
        log::warn!(target: "hook", "could not deliver hook stdout: {error}; no retry");
    }
}

/// What an `asyncRewake` hook says to the agent it wakes: stderr, bounded and
/// deadlined like stdout, for the same reason (a host that has gone away leaves
/// a pipe nobody drains). A message that could not be written is logged; there
/// is no second channel, and the exit code stays the wake's, because the claim
/// has already happened and it is the host that decides what an unread
/// reminder is worth.
fn emit_stderr(payload: String) {
    let result = gitpulse_lib::output::BoundedOutput::new(
        io::stderr(),
        "gitpulse-hook-stderr",
        hooks::MAX_INPUT_BYTES,
        std::time::Duration::from_secs(1),
    )
    .and_then(|output| output.write(payload.as_bytes()));
    if let Err(error) = result {
        log::warn!(target: "hook", "could not deliver the wake message: {error}; no retry");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panicking_hook_still_exits_zero_with_no_decision() {
        // Outside a worktree hook, which this test process never starts.
        assert_eq!(run_guarded(|| panic!("hook fault")), 0);
        assert_eq!(run_guarded(|| 0), 0);
    }
}
