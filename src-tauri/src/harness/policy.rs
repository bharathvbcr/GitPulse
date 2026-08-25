//! Policy verdicts: the harness's decision, rendered for a Git client.
//!
//! The one rule this module exists to keep is that a check which could not run
//! never looks like a check that ran and passed. `PolicyStatus` therefore has
//! five values, not two: an allow, a demoted allow, a warning, a refusal, and
//! "nobody checked". The last is not a failure of the user's action — GitPulse
//! works with no harness installed — but it is never rendered as approval.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::protocol::{RawDecision, OP_POLICY_CHECK_COMMAND, OP_POLICY_CHECK_FILE};
use super::sidecar::{self, HarnessError, DEFAULT_CALL_TIMEOUT};

const POLICY_TIMEOUT: Duration = DEFAULT_CALL_TIMEOUT;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PolicyStatus {
    /// The ladder ran and nothing fired.
    Allowed,
    /// A soft denial the host posture demoted. Allowed, but not clean.
    Demoted,
    /// A rule fired that allows with a note.
    Warned,
    /// A rule refused it. GitPulse does not perform the action.
    Blocked,
    /// The gate did not run. The action proceeds and says so.
    Unchecked,
}

impl PolicyStatus {
    pub fn blocks(self) -> bool {
        matches!(self, PolicyStatus::Blocked)
    }
}

/// One decision, with everything a user needs to understand it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyVerdict {
    pub status: PolicyStatus,
    /// True only when the gate actually ran. Never inferred from `status`.
    pub checked: bool,
    /// What was judged: the command line, or the path.
    pub target: String,
    /// The rung that fired, empty on a clean allow.
    pub rule: String,
    pub severity: String,
    pub reason: String,
    /// Non-empty when a soft denial was demoted by the host posture.
    pub demoted: String,
    /// Why the gate did not run, when it did not.
    pub detail: String,
    /// A machine-readable reason for `detail` ("not_installed", "timeout", …).
    pub detail_code: String,
}

impl PolicyVerdict {
    fn from_decision(target: &str, d: RawDecision) -> Self {
        let status = match d.action.to_ascii_lowercase().as_str() {
            "allow" if !d.demoted.is_empty() => PolicyStatus::Demoted,
            "allow" => PolicyStatus::Allowed,
            "warn" => PolicyStatus::Warned,
            "deny" | "block" => PolicyStatus::Blocked,
            // An action this build does not recognise is treated as a refusal.
            // Failing closed on an unknown decision is the only safe reading:
            // a future rung that means "stop" must not be executed as an allow.
            _ => PolicyStatus::Blocked,
        };
        let reason = if status == PolicyStatus::Blocked && d.reason.is_empty() {
            format!("harness returned an unrecognised decision '{}'", d.action)
        } else {
            d.reason
        };
        PolicyVerdict {
            status,
            checked: true,
            target: if d.target.is_empty() {
                target.to_string()
            } else {
                d.target
            },
            rule: d.rule,
            severity: d.severity,
            reason,
            demoted: d.demoted,
            detail: String::new(),
            detail_code: String::new(),
        }
    }

    pub(crate) fn unchecked(target: &str, err: &HarnessError) -> Self {
        PolicyVerdict {
            status: PolicyStatus::Unchecked,
            checked: false,
            target: target.to_string(),
            rule: String::new(),
            severity: String::new(),
            reason: String::new(),
            demoted: String::new(),
            detail: err.message(),
            detail_code: err.code().to_string(),
        }
    }

    pub fn blocks(&self) -> bool {
        self.status.blocks()
    }

    /// True when the gate did not run for a reason that is *not* "this machine
    /// has no harness installed".
    ///
    /// The two are not the same event and must not degrade the same way.
    /// Running unguarded with no harness installed is GitPulse's documented
    /// behaviour: it is a standing condition the user chose and the UI shows
    /// permanently. Running unguarded because our own sidecar was busy, wedged,
    /// timed out, or spoke a protocol we could not read is none of those — it
    /// is transient, self-inflicted, and invisible in the moment it matters.
    /// Treating it as a pass is how a force-push gets through the check that
    /// exists to stop it, so a mutating action refuses instead.
    ///
    /// Note this is deliberately *not* folded into [`PolicyVerdict::blocks`]:
    /// no rule fired, so nothing was refused *by policy*, and the UI must keep
    /// telling those apart. This is the action seam's question, not a verdict.
    pub fn gate_failed(&self) -> bool {
        self.status == PolicyStatus::Unchecked && self.detail_code != "not_installed"
    }

    /// The gate failure, rendered for an error dialog.
    pub fn gate_failure(&self) -> String {
        format!(
            "The MANVI harness is installed but could not judge this action, so \
             GitPulse did not run it [{}]: {}\n  target: {}",
            if self.detail_code.is_empty() {
                "unknown"
            } else {
                &self.detail_code
            },
            self.detail,
            self.target
        )
    }

    /// The refusal, rendered for an error dialog.
    pub fn refusal(&self) -> String {
        let rule = if self.rule.is_empty() {
            "policy"
        } else {
            &self.rule
        };
        format!(
            "Blocked by the MANVI harness [{}{}]: {}\n  target: {}",
            rule,
            if self.severity.is_empty() {
                String::new()
            } else {
                format!("/{}", self.severity)
            },
            self.reason,
            self.target
        )
    }
}

/// Evaluates one command line against the command gate.
pub fn check_command(root: &str, command: &str) -> PolicyVerdict {
    let params = serde_json::json!({ "command": command, "root": root });
    match sidecar::call_typed::<RawDecision>(OP_POLICY_CHECK_COMMAND, params, POLICY_TIMEOUT) {
        Ok(d) => PolicyVerdict::from_decision(command, d),
        Err(e) => PolicyVerdict::unchecked(command, &e),
    }
}

/// Evaluates one file write against the write gate.
///
/// `op` is create, modify, delete, or write; an unknown value is refused by the
/// harness rather than guessed at here.
pub fn check_file(root: &str, path: &str, op: &str) -> PolicyVerdict {
    let params = serde_json::json!({ "root": root, "path": path, "op": op });
    match sidecar::call_typed::<RawDecision>(OP_POLICY_CHECK_FILE, params, POLICY_TIMEOUT) {
        Ok(d) => PolicyVerdict::from_decision(path, d),
        Err(e) => PolicyVerdict::unchecked(path, &e),
    }
}

/// Renders an argv the way a shell would show it, for the command gate.
///
/// The gate parses a command line, so it has to receive one. Quoting matters:
/// an unquoted commit message containing `;` or `&&` would be read as extra
/// commands and judged as commands the client never runs.
pub fn render_command(argv: &[&str]) -> String {
    argv.iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(arg: &str) -> String {
    let safe = !arg.is_empty()
        && arg.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '-' | '_' | '.' | '/' | '=' | ':' | '@' | '+' | ',' | '~')
        });
    if safe {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(action: &str, demoted: &str) -> RawDecision {
        RawDecision {
            action: action.into(),
            rule: "command.force_push".into(),
            severity: "hard".into(),
            reason: "Force pushes are not allowed.".into(),
            target: "git push --force".into(),
            task_id: "host-scope".into(),
            demoted: demoted.into(),
        }
    }

    #[test]
    fn deny_blocks_and_allow_does_not() {
        assert!(PolicyVerdict::from_decision("x", decision("deny", "")).blocks());
        assert!(!PolicyVerdict::from_decision("x", decision("allow", "")).blocks());
    }

    #[test]
    fn demoted_allow_is_not_a_clean_allow() {
        let clean = PolicyVerdict::from_decision("x", decision("allow", ""));
        let demoted = PolicyVerdict::from_decision("x", decision("allow", "posture=host"));
        assert_eq!(clean.status, PolicyStatus::Allowed);
        assert_eq!(demoted.status, PolicyStatus::Demoted);
        assert_ne!(clean.status, demoted.status);
    }

    #[test]
    fn unknown_action_fails_closed() {
        let v = PolicyVerdict::from_decision("x", decision("quarantine", ""));
        assert_eq!(v.status, PolicyStatus::Blocked);
        assert!(v.checked);
    }

    #[test]
    fn unchecked_is_distinguishable_from_allowed() {
        let v = PolicyVerdict::unchecked(
            "git commit -m x",
            &HarnessError::NotInstalled("no manvi binary".into()),
        );
        assert_eq!(v.status, PolicyStatus::Unchecked);
        assert!(!v.checked);
        assert!(!v.blocks());
        assert_eq!(v.detail_code, "not_installed");
        assert_ne!(v.status, PolicyStatus::Allowed);
    }

    /// Regression: contention, a timeout, a dead child and a protocol breach
    /// all produce an *unchecked* verdict, and an unchecked verdict does not
    /// block. Before `gate_failed`, that meant a momentarily busy sidecar let
    /// `git push --force` through the gate that exists to refuse it, recorded
    /// only as "unguarded". Only a machine with no harness installed may
    /// degrade that way.
    #[test]
    fn a_gate_that_failed_while_installed_is_not_permission_to_act() {
        for err in [
            HarnessError::Busy("another gated action is in progress".into()),
            HarnessError::Timeout("no answer in 15s".into()),
            HarnessError::Unavailable("sidecar exited".into()),
            HarnessError::Protocol("undecodable response".into()),
            HarnessError::Refused(crate::harness::protocol::WireError {
                code: "E_INTERNAL".into(),
                message: "boom".into(),
                retryable: false,
            }),
        ] {
            let v = PolicyVerdict::unchecked("git push --force origin main", &err);
            assert!(!v.checked, "{err:?} must not read as a check that ran");
            assert!(
                v.gate_failed(),
                "{err:?} must not be treated as permission to act"
            );
            // The refusal has to name the cause, or the user cannot act on it.
            assert!(v.gate_failure().contains(&err.message()));
            // Still not a policy refusal: no rule fired, and the UI must be
            // able to tell an unavailable gate from a gate that said no.
            assert!(!v.blocks(), "{err:?} is not a rule firing");
        }
    }

    #[test]
    fn no_harness_installed_is_the_one_unchecked_verdict_that_may_proceed() {
        let v = PolicyVerdict::unchecked(
            "git push --force origin main",
            &HarnessError::NotInstalled("no manvi binary".into()),
        );
        assert!(!v.gate_failed());
        assert!(!v.blocks());
    }

    #[test]
    fn a_verdict_that_ran_never_reads_as_a_failed_gate() {
        for action in ["allow", "warn", "deny", "something-new"] {
            let v = PolicyVerdict::from_decision("x", decision(action, ""));
            assert!(v.checked);
            assert!(!v.gate_failed(), "{action} ran; the gate did not fail");
        }
        assert!(!PolicyVerdict::from_decision("x", decision("allow", "host-scope")).gate_failed());
    }

    #[test]
    fn command_rendering_quotes_shell_metacharacters() {
        let rendered = render_command(&["git", "commit", "-m", "feat: x; rm -rf /"]);
        assert_eq!(rendered, "git commit -m 'feat: x; rm -rf /'");
        assert_eq!(
            render_command(&["git", "push", "origin", "main"]),
            "git push origin main"
        );
        assert_eq!(
            render_command(&["git", "commit", "-m", "it's"]),
            "git commit -m 'it'\\''s'"
        );
    }
}
