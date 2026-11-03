//! `gitpulse-hook deliver`: the `Stop` hook that lets GitPulse speak to an agent
//! that is waiting for its next instruction.
//!
//! The plugin manifest and the settings GitPulse injects into every Claude Code
//! session it launches both carry it as a second `Stop` entry marked
//! `"asyncRewake": true` with a long `timeout`. The Claude Code hooks reference
//! (<https://code.claude.com/docs/en/hooks.md>) says of that field: it "runs in
//! the background and wakes Claude on exit code 2", and "the hook's stderr, or
//! stdout if stderr is empty, is shown to Claude as a system reminder". It also
//! says a hook marked `async` cannot block, which is why this is not that. So:
//! the hook waits in the mailbox (`crate::agent_mailbox`) for the session it
//! runs under; when a person has sent something, it claims it, prints it on
//! stderr, and exits 2. Anything else — no message in time, a session it does
//! not know, a cap reached, a file system it cannot trust — exits 0 having said
//! nothing to the agent.
//!
//! What this is not allowed to do, each pinned by a test:
//!
//! * **Never block a normal stop.** It is registered `asyncRewake`, so Claude
//!   Code does not wait for it; and it exits 0, not 2, in every case but a
//!   claimed message.
//! * **Never wait on its own account.** The wait is [`DELIVER_WAIT`], chosen to
//!   end before the entry's `timeout` in `hooks.json` with a stated margin, and
//!   deliberately *not* clamped to [`super::TOTAL_BUDGET`]: that budget is for
//!   hooks the agent is waiting on, and a waiter cut to it would give up on a
//!   session that is merely idle — the one case it exists for. The environment
//!   can only shorten it ([`WAIT_ENV`]), never lengthen it.
//! * **Deliver once, however many copies run.** The plugin's copy and the
//!   `--settings` copy both run for the same stop (Claude Code does not
//!   deduplicate a plugin's handler against a settings one), and so does every
//!   earlier stop's waiter that has not finished. A message is a rename away
//!   from claimed, and exactly one process wins it.
//! * **Bounded in number.** At most `MAX_WAITERS_PER_SESSION` hooks wait on a
//!   session and `MAX_WAITERS_TOTAL` in all; one past a cap exits 0 at once.
//! * **Only GitPulse's own sessions.** Without `GITPULSE_SESSION_ID` (set on
//!   every PTY GitPulse spawns) the session is not one GitPulse launched and
//!   the hook leaves at once.
//!
//! `stop_hook_active` is `true` when Claude is already continuing because of a
//! stop hook, and a `Stop` can then fire several times in a row. The reference
//! gives it no meaning for an `asyncRewake` hook, so it is used for the one
//! thing it can mean here: those repeats do not each start another waiter. They
//! look once, take a message that is already there, and leave; the waiter from
//! the first stop is still waiting.

use std::time::Duration;

use super::{HookError, HookInput, HookOutput};
use crate::agent_mailbox::{Mailbox, MailboxError, Wait};

/// How long one waiter waits for a message.
///
/// `hooks.json` gives the entry a `timeout` of 1800 s, the longest the hook
/// reference's tooling here allows for it. Claude Code enforces that timeout on
/// an `asyncRewake` hook (default 600 s when none is given), killing the
/// process, so the wait ends 60 s sooner and the process leaves on its own.
/// A plain whole number of seconds so `scripts/plugin-contract.test.ts` can
/// read it and hold the manifest's `timeout` above it.
pub const DELIVER_WAIT: Duration = Duration::from_secs(1740);

/// An environment variable that shortens [`DELIVER_WAIT`], in milliseconds. A
/// larger value is ignored, so it cannot be used to make a waiter outlive the
/// host's timeout. It exists so the wait path can be run to its end in a test.
pub const WAIT_ENV: &str = "GITPULSE_HOOK_DELIVER_WAIT_MS";

/// What the agent reads. It says where the text came from and that a person
/// approved it, because stderr from a hook looks like tool output unless told
/// otherwise.
fn reminder(text: &str) -> String {
    format!(
        "A message from the person using GitPulse, sent from the GitPulse app. They previewed \
         and approved this exact text before it was sent; it is not tool output.\n\n{text}\n"
    )
}

/// The wait for this run: [`DELIVER_WAIT`], or less if [`WAIT_ENV`] says so.
fn wait_limit(requested: Option<&str>) -> Duration {
    requested
        .and_then(|ms| ms.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
        .map_or(DELIVER_WAIT, |asked| asked.min(DELIVER_WAIT))
}

/// Whether this process runs under a terminal GitPulse spawned.
fn recognized() -> bool {
    super::session_env().is_some()
}

/// The handler. See the module documentation.
pub(super) fn run(input: &HookInput) -> Result<HookOutput, HookError> {
    if !recognized() {
        return Ok(HookOutput::silent());
    }
    let mailbox = match Mailbox::at_default() {
        Ok(mailbox) => mailbox,
        Err(error) => return Err(HookError::Failed(format!("no mailbox to wait on: {error}"))),
    };
    let wait = wait_limit(std::env::var(WAIT_ENV).ok().as_deref());
    run_in(&mailbox, input, wait)
}

/// [`run`] against a given mailbox and wait, with the session already
/// recognized. Split out so every outcome can be driven without a real
/// configuration directory or a real half hour.
pub(super) fn run_in(
    mailbox: &Mailbox,
    input: &HookInput,
    wait: Duration,
) -> Result<HookOutput, HookError> {
    // A payload with no usable session id is a session this hook does not
    // know: leave, quietly. Nothing was checked, and nothing is claimed to be.
    if crate::agent_mailbox::session_key(&input.session_id).is_err() {
        return Ok(HookOutput::silent());
    }
    let waited = if input.stop_hook_active {
        mailbox
            .claim_next(&input.session_id, std::time::SystemTime::now())
            .map(|claim| claim.map_or(Wait::TimedOut, Wait::Message))
    } else {
        mailbox.wait(&input.session_id, wait)
    };
    match waited {
        Ok(Wait::Message(claim)) => Err(HookError::Wake(reminder(&claim.text))),
        Ok(Wait::TimedOut) => Ok(HookOutput::silent()),
        Ok(Wait::Declined(why)) => {
            log::debug!(target: "hook", "deliver did not wait: {why}");
            Ok(HookOutput::silent())
        }
        Err(error @ (MailboxError::Insecure(_) | MailboxError::Io(_))) => Err(HookError::Failed(
            format!("deliver could not use the mailbox: {error}"),
        )),
        Err(error) => Err(HookError::Failed(format!("deliver: {error}"))),
    }
}

#[cfg(test)]
mod tests;
