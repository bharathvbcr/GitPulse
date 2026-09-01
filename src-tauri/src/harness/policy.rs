//! Policy verdicts: the harness's decision, rendered for a Git client.
//!
//! The one rule this module exists to keep is that a check which could not run
//! never looks like a check that ran and passed. It has a mirror that matters
//! just as much: a check that ran and *failed* must not look like one that
//! passed either. `PolicyStatus` therefore has eight values, not two.
//!
//! Four of them are kinds of "the operation may proceed" that are emphatically
//! not clean passes: `Demoted` (a posture waived it), `Granted` (a human
//! waived it), `Widened` (the executor waived it for itself), and `Degraded`
//! (some rungs could not run at all). The harness reports `action: allow` for
//! every one of them, which is why the extra fields on the decision — not the
//! action — are what this module classifies on.
//!
//! The classification and its evaluation order are the shared contract in
//! `contracts/verdict.schema.json` ($defs.classification), and
//! `contracts/verdict.cases.json` holds the 65 generated cases all three
//! products are held to.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::protocol::{RawDecision, OP_POLICY_CHECK_COMMAND, OP_POLICY_CHECK_FILE};
use super::sidecar::{self, HarnessError, DEFAULT_CALL_TIMEOUT};

const POLICY_TIMEOUT: Duration = DEFAULT_CALL_TIMEOUT;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PolicyStatus {
    /// The ladder ran, every rung ran, and nothing fired.
    Allowed,
    /// A soft denial the host posture demoted. Allowed, but not clean.
    Demoted,
    /// A soft denial an override grant cleared. A human waived a rule that
    /// fired; that is not the same as no rule firing.
    Granted,
    /// Authorised by scope the executor appended to its own task rather than
    /// by the plan. The narrowest kind of allow, and the easiest to launder.
    Widened,
    /// Allowed, but reached without some rungs — the repo map was missing, a
    /// check was disabled. Not a refusal, and not a clean pass.
    Degraded,
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
    /// Non-empty when an override grant cleared a soft block.
    pub grant_id: String,
    /// Who issued the grant named by `grant_id`.
    pub granted_by: String,
    /// Non-empty when executor-appended scope, not the plan, authorised this.
    pub widened: String,
    /// Checks that could not run. Empty means every rung ran.
    pub degraded: Vec<String>,
    /// The task this decision was measured against, empty when none was
    /// declared.
    ///
    /// The harness has always sent it; this end used to drop it. Without it a
    /// verdict cannot be attributed to the work it belongs to, and the
    /// scope rules that fire against a task have nothing to point at.
    pub task_id: String,
    /// Why the gate did not run, when it did not.
    pub detail: String,
    /// A machine-readable reason for `detail` ("not_installed", "timeout", …).
    pub detail_code: String,
}

impl PolicyVerdict {
    fn from_decision(target: &str, d: RawDecision) -> Self {
        // The shared classification, in the contract's evaluation order. A
        // decision satisfies several of these at once — a granted decision is
        // also an allow — and the earlier branch is the more honest reading.
        //
        // Order and semantics: contracts/verdict.schema.json
        // ($defs.classification). Held to contracts/verdict.cases.json by
        // scripts/verdict-contract.test.ts and the Rust test below.
        let action = d.action.to_ascii_lowercase();
        let status = match action.as_str() {
            "deny" | "block" => PolicyStatus::Blocked,
            "allow" | "warn" if !d.grant_id.is_empty() => PolicyStatus::Granted,
            "allow" | "warn" if !d.demoted.is_empty() => PolicyStatus::Demoted,
            "allow" | "warn" if !d.widened.is_empty() => PolicyStatus::Widened,
            "allow" | "warn" if !d.degraded.is_empty() => PolicyStatus::Degraded,
            "warn" => PolicyStatus::Warned,
            "allow" => PolicyStatus::Allowed,
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
            task_id: d.task_id,
            grant_id: d.grant_id,
            granted_by: d.granted_by,
            widened: d.widened,
            degraded: d.degraded,
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
            task_id: String::new(),
            grant_id: String::new(),
            granted_by: String::new(),
            widened: String::new(),
            degraded: Vec::new(),
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
///
/// Verdicts go through [`sidecar::call_policy`]: they are milliseconds of
/// harness work, so one transport fault is retried once on a fresh connection
/// before this reports unchecked. Without that retry, a single slow verdict
/// would start the respawn backoff and leave every mutation unchecked for
/// thirty seconds.
/// The task scope a check is measured against.
///
/// Mirrors `serve.HostScope` in Manvi, which owns the shape. Sending one is
/// what makes the scope rungs — `scope.unplanned`, `scope.read_only`,
/// `scope.operation`, `task.forbidden_change` — reachable at all: without it
/// the ladder stops at `task.absent` and the host posture demotes that to an
/// allow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HostScope {
    pub task_id: String,
    /// Repo-relative paths the plan authorises.
    ///
    /// An empty list is a real value: the task authorises nothing, and every
    /// write is unplanned. It is never sent to mean "we did not look".
    pub planned_files: Vec<String>,
    pub forbidden_changes: Vec<String>,
    pub worktree: String,
    /// Commands the task authorises, in fnmatch form.
    ///
    /// Read from the bound task rather than invented here: a task that declares
    /// what it may run should have that honoured, and dropping it made every
    /// command in a bound worktree fall to `command.not_allowed`.
    pub allowed_commands: Vec<String>,
}

pub fn check_command(root: &str, command: &str, scope: Option<&HostScope>) -> PolicyVerdict {
    check_command_allowing(root, command, scope, &[])
}

/// Evaluates a command while declaring an allowlist for it.
///
/// The allowlist is how a caller says "these are the commands I intend to run",
/// which is what lets the harness answer with a clean allow rather than a
/// demoted `command.not_allowed`. It never widens anything the hard rules
/// refuse: a declared command that force-pushes is still refused, because those
/// rungs run before the allowlist and carry Severity Hard.
///
/// `enforce_allowlist` is deliberately left false. Turning it on would make
/// "not in the list" a refusal, and this host's list is a statement of intent
/// rather than a complete inventory of what a repository may legitimately run.
pub fn check_command_allowing(
    root: &str,
    command: &str,
    scope: Option<&HostScope>,
    allowed: &[String],
) -> PolicyVerdict {
    let mut params = serde_json::json!({ "command": command, "root": root });
    if let Some(scope) = scope {
        params["scope"] = serde_json::to_value(scope).unwrap_or(serde_json::Value::Null);
    }
    if !allowed.is_empty() {
        params["allowed_commands"] =
            serde_json::to_value(allowed).unwrap_or(serde_json::Value::Null);
    }
    match sidecar::call_policy::<RawDecision>(OP_POLICY_CHECK_COMMAND, params, POLICY_TIMEOUT) {
        Ok(d) => PolicyVerdict::from_decision(command, d),
        Err(e) => PolicyVerdict::unchecked(command, &e),
    }
}

/// Evaluates one file write against the write gate.
///
/// `op` is create, modify, delete, or write; an unknown value is refused by the
/// harness rather than guessed at here. Like [`check_command`], the verdict is
/// retried once on a fresh connection before being reported unchecked.
pub fn check_file(root: &str, path: &str, op: &str, scope: Option<&HostScope>) -> PolicyVerdict {
    let mut params = serde_json::json!({ "root": root, "path": path, "op": op });
    if let Some(scope) = scope {
        params["scope"] = serde_json::to_value(scope).unwrap_or(serde_json::Value::Null);
    }
    match sidecar::call_policy::<RawDecision>(OP_POLICY_CHECK_FILE, params, POLICY_TIMEOUT) {
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
            ..Default::default()
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

#[cfg(test)]
mod contract_tests {
    use super::*;
    use crate::harness::protocol::RawDecision;

    /// The generated parity fixture every product is held to.
    ///
    /// GitPulse is a *consumer* of the verdict contract, so its job here is to
    /// agree with Manvi and DevCouncil about what a decision means — not to
    /// have its own opinion. The cases enumerate the cross-product of every
    /// classification-relevant field, so a combination cannot go uncovered.
    #[derive(serde::Deserialize)]
    struct CasesFile {
        version: u32,
        cases: Vec<Case>,
    }

    #[derive(serde::Deserialize)]
    struct Case {
        name: String,
        decision: Option<RawDecision>,
        expect: String,
    }

    fn status_name(s: PolicyStatus) -> &'static str {
        match s {
            PolicyStatus::Allowed => "clean",
            PolicyStatus::Demoted => "demoted",
            PolicyStatus::Granted => "granted",
            PolicyStatus::Widened => "widened",
            PolicyStatus::Degraded => "degraded",
            PolicyStatus::Warned => "warned",
            PolicyStatus::Blocked => "denied",
            PolicyStatus::Unchecked => "unchecked",
        }
    }

    fn load() -> CasesFile {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../contracts/verdict.cases.json"
        );
        let raw = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {path}: {e}"));
        serde_json::from_str(&raw).expect("parsing verdict.cases.json")
    }

    #[test]
    fn classification_matches_the_shared_contract() {
        let file = load();
        assert_eq!(file.version, 1, "cases file version this test understands");
        assert!(
            file.cases.len() >= 8,
            "only {} cases; the fixture is not covering the decision space",
            file.cases.len()
        );

        let mut seen = std::collections::BTreeSet::new();
        for case in &file.cases {
            let got = match &case.decision {
                // No decision at all is not a verdict of allow. GitPulse
                // reaches this state through `unchecked`, which is
                // constructed from a transport error rather than a decision.
                None => "unchecked",
                Some(d) => status_name(PolicyVerdict::from_decision(&d.target, d.clone()).status),
            };
            seen.insert(case.expect.clone());
            assert_eq!(
                got, case.expect,
                "case {:?}: classified {:?}, contract says {:?}",
                case.name, got, case.expect
            );
        }

        for state in [
            "unchecked",
            "denied",
            "granted",
            "demoted",
            "widened",
            "degraded",
            "warned",
            "clean",
        ] {
            assert!(
                seen.contains(state),
                "no case in the fixture reaches {state:?}"
            );
        }
    }

    /// The regression this whole extension exists to prevent.
    ///
    /// Before it, `RawDecision` dropped `grant_id`, `widened` and `degraded`
    /// on the floor, so a grant-cleared write, a self-widened write, and a
    /// write judged without the repo map all rendered as `Allowed` — the exact
    /// same pixels as a decision where every rung ran and none fired.
    #[test]
    fn the_four_kinds_of_unclean_allow_are_distinguishable() {
        let base = RawDecision {
            action: "allow".into(),
            rule: "scope.unplanned".into(),
            severity: "soft".into(),
            reason: "outside the plan".into(),
            target: "src/lib/x.ts".into(),
            task_id: "TASK-1".into(),
            demoted: String::new(),
            grant_id: String::new(),
            granted_by: String::new(),
            widened: String::new(),
            degraded: Vec::new(),
        };

        let clean = PolicyVerdict::from_decision(
            "t",
            RawDecision {
                rule: String::new(),
                severity: "none".into(),
                ..base.clone()
            },
        );
        let granted = PolicyVerdict::from_decision(
            "t",
            RawDecision {
                grant_id: "G-1".into(),
                granted_by: "bharath".into(),
                ..base.clone()
            },
        );
        let demoted = PolicyVerdict::from_decision(
            "t",
            RawDecision {
                demoted: "policy.file.mode=advisory".into(),
                ..base.clone()
            },
        );
        let widened = PolicyVerdict::from_decision(
            "t",
            RawDecision {
                widened: "src/lib/**".into(),
                ..base.clone()
            },
        );
        let degraded = PolicyVerdict::from_decision(
            "t",
            RawDecision {
                degraded: vec!["repo_map.unavailable".into()],
                ..base.clone()
            },
        );

        assert_eq!(clean.status, PolicyStatus::Allowed);
        assert_eq!(granted.status, PolicyStatus::Granted);
        assert_eq!(demoted.status, PolicyStatus::Demoted);
        assert_eq!(widened.status, PolicyStatus::Widened);
        assert_eq!(degraded.status, PolicyStatus::Degraded);

        // All five permit the action. That is precisely why the status has to
        // carry the difference: `blocks()` cannot.
        for v in [&clean, &granted, &demoted, &widened, &degraded] {
            assert!(!v.blocks(), "none of these refuse the action");
            assert!(v.checked, "the gate ran for all of them");
        }

        // And none of the four may collapse onto the clean one.
        for v in [&granted, &demoted, &widened, &degraded] {
            assert_ne!(
                v.status, clean.status,
                "an unclean allow must not render as a clean pass"
            );
        }
    }

    /// A grant outranks a demotion outranks a widening outranks a degradation.
    /// The order is the contract's, and it is not arbitrary: it reports the
    /// most specific thing that actually happened to this decision.
    #[test]
    fn classification_order_is_stable_when_several_apply() {
        let all = RawDecision {
            action: "allow".into(),
            rule: "scope.unplanned".into(),
            severity: "soft".into(),
            reason: String::new(),
            target: "t".into(),
            task_id: String::new(),
            demoted: "mode=advisory".into(),
            grant_id: "G-1".into(),
            granted_by: "bharath".into(),
            widened: "src/**".into(),
            degraded: vec!["repo_map.unavailable".into()],
        };
        assert_eq!(
            PolicyVerdict::from_decision("t", all.clone()).status,
            PolicyStatus::Granted
        );

        let no_grant = RawDecision {
            grant_id: String::new(),
            granted_by: String::new(),
            ..all.clone()
        };
        assert_eq!(
            PolicyVerdict::from_decision("t", no_grant.clone()).status,
            PolicyStatus::Demoted
        );

        let no_demote = RawDecision {
            demoted: String::new(),
            ..no_grant.clone()
        };
        assert_eq!(
            PolicyVerdict::from_decision("t", no_demote.clone()).status,
            PolicyStatus::Widened
        );

        let no_widen = RawDecision {
            widened: String::new(),
            ..no_demote.clone()
        };
        assert_eq!(
            PolicyVerdict::from_decision("t", no_widen).status,
            PolicyStatus::Degraded
        );

        // A denial outranks every one of them: it is the only one that stops
        // the operation, so nothing may reclassify it into an allow.
        let denied = RawDecision {
            action: "deny".into(),
            ..all
        };
        assert_eq!(
            PolicyVerdict::from_decision("t", denied).status,
            PolicyStatus::Blocked
        );
    }
}
