//! Gate verdict machinery shared by every mutating command, plus harness and policy commands.

use super::*;

// ---------------------------------------------------------------------------
// MANVI harness: policy-gated Git actions, and the local AI features.
// ---------------------------------------------------------------------------

/// A Git action's result together with the policy decision it ran under.
///
/// The verdict travels with every guarded action so the UI can distinguish an
/// action the gate approved from one that ran with no gate available. They are
/// not the same event and are never rendered the same way.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Guarded<T> {
    pub policy: crate::harness::PolicyVerdict,
    pub output: T,
}

/// Evaluates one command line, refusing the action when a rule fires.
///
/// Thin delegate to the harness's canonical owner (`crate::harness::
/// guard_command`), so this file keeps no second copy of render→check→refuse.
pub(super) fn guard(
    repo_path: &str,
    argv: &[&str],
) -> Result<crate::harness::PolicyVerdict, String> {
    // Every mutation passes here, and only mutations: the user clicked
    // something, so the spawn gate must not defer it behind refresh traffic.
    crate::engine::git_cli::mark_user_action(repo_path);
    crate::harness::guard_command(repo_path, argv)
}

/// [`guard`] for a file write the user clicked: a save, a discard, a conflict
/// resolution, a move or a cleanup. The harness's file check resolves the
/// worktree's task binding with git, so the promotion comes first: as a
/// deferrable read that lookup is shed under memory pressure
/// (`sysload::governor`), and the click it guards would be refused with it.
pub(super) fn guard_file(
    repo_path: &str,
    file_path: &str,
    op: &str,
) -> Result<crate::harness::PolicyVerdict, String> {
    crate::engine::git_cli::mark_user_action(repo_path);
    crate::harness::guard_file(repo_path, file_path, op)
}

/// [`guard_file`] for a click that writes many files: one verdict per file, in
/// order, with the scope resolved once. The promotion is made here, on the
/// calling thread, before the harness resolves that scope.
pub(super) fn guard_files(
    repo_path: &str,
    file_paths: &[&str],
    op: &str,
) -> Result<Vec<crate::harness::PolicyVerdict>, String> {
    crate::engine::git_cli::mark_user_action(repo_path);
    crate::harness::guard_files(repo_path, file_paths, op)
}

/// [`guard`] for a mutation nobody clicked (the auto-fetch timer): the same
/// write gate, without promoting the command to a user action, so the spawn
/// gate can still defer or shed it behind the user's own work.
pub(super) fn guard_background(
    repo_path: &str,
    argv: &[&str],
) -> Result<crate::harness::PolicyVerdict, String> {
    crate::harness::guard_command(repo_path, argv)
}

/// A guarded action that selected no git command. The gate did not run
/// because there was nothing to judge; `checked` stays false so that is
/// not reported as an allow.
pub(super) fn no_git_verdict() -> crate::harness::PolicyVerdict {
    crate::harness::PolicyVerdict {
        status: crate::harness::PolicyStatus::Unchecked,
        checked: false,
        target: "no git command".to_string(),
        rule: String::new(),
        severity: String::new(),
        reason: String::new(),
        demoted: String::new(),
        grant_id: String::new(),
        granted_by: String::new(),
        widened: String::new(),
        degraded: Vec::new(),
        task_id: String::new(),
        detail: "no git command was required".to_string(),
        detail_code: "no_mutation".to_string(),
    }
}

/// Picks the verdict a multi-command action should report.
///
/// Ranking, strictest first:
/// Blocked > Warned > Widened > Granted > Degraded > Demoted > Unchecked > Allowed.
///
/// Blocked never actually reaches here as a survivor — `guard` refuses on it —
/// but the ranking still documents intent. A warning is an explicit rule
/// firing, a demotion is posture noise, an unchecked gate is an absence of a
/// check, and a clean allow carries nothing worth surfacing over its peers.
///
/// The three middle rungs are all "a rule fired and something waived it", and
/// they are ordered by who did the waiving and how long it lasts. `Widened`
/// outranks `Granted` because a grant expires and appended scope does not: the
/// widening is still there in the next run with no grant beside it, so it is
/// the one worth surfacing when a multi-command action produced both.
/// `Degraded` sits below them because no rule fired at all — some simply could
/// not be asked — but above `Demoted`, which is a posture the user chose.
pub(super) fn strictest_verdict(
    a: crate::harness::PolicyVerdict,
    b: crate::harness::PolicyVerdict,
) -> crate::harness::PolicyVerdict {
    use crate::harness::PolicyStatus::*;
    let rank = |v: &crate::harness::PolicyVerdict| match v.status {
        Blocked => 8,
        Warned => 7,
        Widened => 6,
        Granted => 5,
        Degraded => 4,
        Demoted => 3,
        Unchecked => 2,
        Allowed => 1,
    };
    if rank(&a) >= rank(&b) {
        a
    } else {
        b
    }
}

/// Accumulates the verdicts of every command and file check one handler judges
/// and yields the strictest, so a warning on an early line is not overwritten
/// by a later clean pass. Interior mutability because the gate closures the
/// engines take are `Fn`, and two closures (command and file) feed one fold.
#[derive(Default)]
pub(super) struct VerdictFold(std::cell::RefCell<Option<crate::harness::PolicyVerdict>>);

impl VerdictFold {
    pub(super) fn record(&self, verdict: crate::harness::PolicyVerdict) {
        let mut slot = self.0.borrow_mut();
        *slot = Some(match slot.take() {
            Some(previous) => strictest_verdict(previous, verdict),
            None => verdict,
        });
    }

    /// Judges nothing yet: `None`, never a fabricated allow.
    pub(super) fn finish(self) -> Option<crate::harness::PolicyVerdict> {
        self.0.into_inner()
    }
}

/// Degraded status used when the probe task itself failed to run (pool join
/// error). Mirrors the shape `HarnessStatus::probe` produces for a dead
/// sidecar, so the UI cannot tell the two apart by structure.
pub(super) fn harness_join_failure(error: String) -> crate::harness::HarnessStatus {
    crate::harness::HarnessStatus {
        available: false,
        binary: String::new(),
        protocol: 0,
        posture: String::new(),
        ops: Vec::new(),
        error,
        error_code: "unavailable".into(),
    }
}

#[tauri::command(async)]
pub async fn cmd_harness_status() -> crate::harness::HarnessStatus {
    // Probing can spawn the sidecar and wait out a 20s handshake; keep that
    // off the async workers.
    off_thread(|| Ok::<_, String>(crate::harness::HarnessStatus::probe()))
        .await
        .unwrap_or_else(|e| harness_join_failure(e.to_string()))
}

/// Drops the current sidecar and clears its backoff, so the next call starts a
/// fresh one. The UI's "reconnect" affordance after installing MANVI.
#[tauri::command(async)]
pub async fn cmd_harness_reconnect() -> crate::harness::HarnessStatus {
    off_thread(|| {
        crate::harness::sidecar::reset();
        Ok::<_, String>(crate::harness::HarnessStatus::probe())
    })
    .await
    .unwrap_or_else(|e| harness_join_failure(e.to_string()))
}

/// Judges `command` for `repo_path` on the calling thread.
///
/// Reserved endpoint, but still an entry point: `root` crosses a process
/// boundary into the harness sidecar, so it is resolved as a repository here
/// instead of being forwarded as an arbitrary string. Filesystem-only on
/// purpose — judging a command starts nothing in the checkout, so this asks
/// for identity, not execution authority.
///
/// A path that is not a repository comes back unchecked, carrying the
/// resolution failure as its detail, rather than as a verdict for a root the
/// gate was never given.
pub(super) fn policy_verdict_for(repo_path: &str, command: &str) -> crate::harness::PolicyVerdict {
    match crate::engine::git_cli::validate_repo_path(repo_path) {
        Ok(repo) => crate::harness::check_command(&repo.to_string_lossy(), command, None),
        Err(reason) => crate::harness::PolicyVerdict::unchecked(
            command,
            &crate::harness::HarnessError::Unavailable(reason),
        ),
    }
}

#[tauri::command(async)]
pub async fn cmd_policy_check_command(
    repo_path: String,
    command: String,
) -> crate::harness::PolicyVerdict {
    let command_for_fallback = command.clone();
    off_thread(move || Ok::<_, String>(policy_verdict_for(&repo_path, &command)))
        .await
        .unwrap_or_else(|e| {
            crate::harness::PolicyVerdict::unchecked(
                &command_for_fallback,
                &crate::harness::HarnessError::Unavailable(e.to_string()),
            )
        })
}
