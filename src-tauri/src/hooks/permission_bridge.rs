//! `gitpulse-hook permission-bridge`: presence-gated universal Claude Code
//! `PermissionRequest` approval bridge (Opus P0-1, P0-2, P0-3, P0-4, P1).
//!
//! Registered as a **separate** synchronous `PermissionRequest` hook entry in
//! `plugins/gitpulse/hooks/hooks.json` alongside the untouched `async: true`
//! `gitpulse-hook notify permission_request` entry.
//!
//! ## Narrow carve-out from `hooks/mod.rs`'s "never emit `allow`" invariant
//!
//! `PreToolUse` safety hooks (`collision-guard`, `command-gate`) NEVER emit
//! `allow`. This subcommand is the **sole** path in `gitpulse-hook` permitted
//! to emit a `PermissionRequest` `{"behavior":"allow"}` verdict, and ONLY when:
//!
//! 1. The local control-plane [`crate::alerts::approval_broker::ApprovalBroker`]
//!    on `approval-broker.sock` resolved the exact `(session_id, input_sha256)`
//!    ticket via an in-process Desktop UI click OR a cryptographically verified
//!    P-256 mobile hardware signature.
//! 2. The emitted decision is strictly one of `allow` (single-use), `deny`, or
//!    `deny` with `interrupt: true` — **NEVER** `updatedInput` or
//!    `updatedPermissions`.
//! 3. Every bypass or failure path (user present at desk, presence unknown,
//!    Free/unpaired mode, non-allowlisted tool, broker absent/unreachable,
//!    user returned mid-hold, timeout) exits `0` with **empty stdout**, letting
//!    Claude Code's native terminal permission dialog run untouched.

use serde::Serialize;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

use crate::alerts::approval_broker::{
    default_socket_path, is_remote_approvable_tool, is_trusted_repo_cwd, ApprovalDecision,
    HookBrokerReply, RegisterApprovalRequest,
};

use super::{HookError, HookInput, HookOutput};

/// Maximum time `permission-bridge` holds for a remote decision when the user
/// is away from the desktop.
///
/// `hooks.json` gives the entry `timeout: 1800` (matching `deliver`); ending
/// 60 s earlier lets the hook exit cleanly with empty stdout before the host
/// kills it. Plain whole seconds so `scripts/plugin-contract.test.ts` parses
/// and enforces the margin.
pub const PERMISSION_BRIDGE_WAIT: Duration = Duration::from_secs(1740);

/// Optional environment variable shortening [`PERMISSION_BRIDGE_WAIT`] in
/// milliseconds for tests. Values larger than [`PERMISSION_BRIDGE_WAIT`] are
/// ignored so nothing can make the hook outlive its host timeout.
pub const WAIT_ENV: &str = "GITPULSE_HOOK_PERMISSION_WAIT_MS";

/// Default message returned to Claude when a human denies a tool call without
/// specifying custom text.
const DEFAULT_DENY_MESSAGE: &str = "Denied by the user in GitPulse.";

/// Strictly minimal Claude `PermissionRequest` decision object (Opus P0-4).
/// `updatedInput` and `updatedPermissions` are deliberately absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "behavior", rename_all = "lowercase")]
enum ClaudePermissionDecision {
    Allow,
    Deny {
        message: String,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        interrupt: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PermissionRequestSpecificOutput {
    hook_event_name: &'static str,
    decision: ClaudePermissionDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PermissionRequestHookOutput {
    hook_specific_output: PermissionRequestSpecificOutput,
}

fn wait_limit(requested: Option<&str>) -> Duration {
    requested
        .and_then(|ms| ms.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
        .map_or(PERMISSION_BRIDGE_WAIT, |asked| {
            asked.min(PERMISSION_BRIDGE_WAIT)
        })
}

/// Renders a broker reply into Claude Code `PermissionRequest` stdout JSON, or
/// `None` (empty stdout, exit `0`) for any bypass.
pub fn render_reply(reply: &HookBrokerReply) -> Option<String> {
    match reply {
        HookBrokerReply::Bypass { .. } => None,
        HookBrokerReply::Decision {
            decision, reason, ..
        } => {
            let wire_decision = match decision {
                ApprovalDecision::AllowOnce => ClaudePermissionDecision::Allow,
                ApprovalDecision::Deny => ClaudePermissionDecision::Deny {
                    message: reason
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .unwrap_or(DEFAULT_DENY_MESSAGE)
                        .to_owned(),
                    interrupt: false,
                },
                ApprovalDecision::DenyAndInterrupt => ClaudePermissionDecision::Deny {
                    message: reason
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .unwrap_or(DEFAULT_DENY_MESSAGE)
                        .to_owned(),
                    interrupt: true,
                },
            };
            let output = PermissionRequestHookOutput {
                hook_specific_output: PermissionRequestSpecificOutput {
                    hook_event_name: "PermissionRequest",
                    decision: wire_decision,
                },
            };
            serde_json::to_string(&output).ok()
        }
    }
}

/// Builds the broker registration payload from parsed Claude hook stdin.
/// Returns `None` when the invocation should bypass immediately without
/// contacting any socket.
pub fn build_request(input: &HookInput, wait: Duration) -> Option<RegisterApprovalRequest> {
    if input.is_antigravity() || input.is_cursor {
        return None;
    }
    if input.session_id.trim().is_empty() || input.tool_name.trim().is_empty() {
        return None;
    }
    if !is_remote_approvable_tool(&input.tool_name) {
        return None;
    }
    let tool_input = input
        .raw_tool_input
        .clone()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    let wait_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX);
    Some(RegisterApprovalRequest {
        session_id: input.session_id.clone(),
        tool_name: input.tool_name.clone(),
        tool_input,
        cwd: input.cwd.clone(),
        tool_summary: input.tool_summary.clone(),
        wait_ms: Some(wait_ms),
    })
}

/// Executes `permission-bridge` against the default `approval-broker.sock`.
/// Returns `Some(json)` only for a verified decision, or `None` for immediate
/// bypass (exit `0`, empty stdout).
///
/// Enforces `find_git_root` + `repository_trust::require` before touching any
/// socket (Opus A1-8 / #39 P1).
pub fn run(input: &HookInput) -> Option<String> {
    if !is_trusted_repo_cwd(&input.cwd) {
        return None;
    }
    let wait = wait_limit(std::env::var(WAIT_ENV).ok().as_deref());
    let socket_path = default_socket_path()?;
    run_against_socket_with_trust_check(input, &socket_path, wait, is_trusted_repo_cwd)
}

/// Executes `permission-bridge` against an explicit socket path, enforcing
/// `find_git_root` + `repository_trust::require` before connecting. Never
/// blocks or denies when the repo is untrusted or the broker is absent.
pub fn run_against_socket(
    input: &HookInput,
    socket_path: &Path,
    wait: Duration,
) -> Option<String> {
    run_against_socket_with_trust_check(input, socket_path, wait, is_trusted_repo_cwd)
}

fn run_against_socket_with_trust_check(
    input: &HookInput,
    socket_path: &Path,
    wait: Duration,
    trust_check: impl Fn(&str) -> bool,
) -> Option<String> {
    if !trust_check(&input.cwd) {
        return None;
    }
    let request = build_request(input, wait)?;
    #[cfg(unix)]
    {
        match crate::alerts::approval_broker::register_permission_via_socket(
            socket_path,
            &request,
            wait,
        ) {
            Ok(reply) => render_reply(&reply),
            Err(err) => {
                log::debug!(
                    target: "hook",
                    "permission-bridge bypassing to native prompt: {err}"
                );
                None
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (socket_path, request);
        None
    }
}

/// Entrypoint for `hooks::dispatch("permission-bridge", ...)` when invoked in
/// unit tests without the binary wrapper. Always returns silent `HookOutput`
/// (which cannot carry `allow`); the binary calls [`run`] directly so the
/// narrow `PermissionRequest` `allow` output stays isolated to this module.
pub(super) fn dispatch_fallback(input: &HookInput) -> Result<HookOutput, HookError> {
    if build_request(input, Duration::from_millis(1)).is_none() {
        return Ok(HookOutput::silent());
    }
    Ok(HookOutput::silent())
}

/// Verifies at compile/test time that no serialized `PermissionRequest` output
/// can ever contain `updatedInput` or `updatedPermissions`.
#[cfg(test)]
pub(crate) fn forbidden_fields_absent(rendered: &str) -> bool {
    let Ok(val) = serde_json::from_str::<Value>(rendered) else {
        return false;
    };
    let decision = val
        .get("hookSpecificOutput")
        .and_then(|o| o.get("decision"))
        .and_then(Value::as_object);
    let Some(obj) = decision else {
        return false;
    };
    !obj.contains_key("updatedInput")
        && !obj.contains_key("updatedPermissions")
        && !obj.contains_key("addPermissionRuleOnAllow")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use crate::alerts::approval_broker::{
        ApprovalBroker, ApprovalBrokerServer, MockPresenceProvider, TrustedApprovalKeyStore,
        UserPresence, SOCKET_NAME,
    };
    use std::sync::Arc;

    fn sample_bash_input() -> HookInput {
        HookInput::from_value(&json!({
            "hook_event_name": "PermissionRequest",
            "session_id": "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c11",
            "cwd": "/repo",
            "tool_name": "Bash",
            "tool_input": { "command": "cargo test --lib" }
        }))
    }

    #[test]
    fn renders_strictly_minimal_decision_surface_with_zero_updated_input_or_permissions() {
        let bypass = render_reply(&HookBrokerReply::Bypass {
            reason: "user_present".into(),
        });
        assert_eq!(bypass, None);

        let allow = render_reply(&HookBrokerReply::Decision {
            ticket_id: "h:t1".into(),
            decision: ApprovalDecision::AllowOnce,
            reason: None,
        })
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&allow).unwrap(),
            json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": { "behavior": "allow" }
                }
            })
        );
        assert!(forbidden_fields_absent(&allow));

        let deny = render_reply(&HookBrokerReply::Decision {
            ticket_id: "h:t1".into(),
            decision: ApprovalDecision::Deny,
            reason: Some("not now".into()),
        })
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&deny).unwrap(),
            json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": { "behavior": "deny", "message": "not now" }
                }
            })
        );
        assert!(forbidden_fields_absent(&deny));

        let deny_int = render_reply(&HookBrokerReply::Decision {
            ticket_id: "h:t1".into(),
            decision: ApprovalDecision::DenyAndInterrupt,
            reason: None,
        })
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&deny_int).unwrap(),
            json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": {
                        "behavior": "deny",
                        "message": DEFAULT_DENY_MESSAGE,
                        "interrupt": true
                    }
                }
            })
        );
        assert!(forbidden_fields_absent(&deny_int));
    }

    #[test]
    fn wait_env_can_only_shorten_wait_never_lengthen() {
        assert_eq!(wait_limit(None), PERMISSION_BRIDGE_WAIT);
        assert_eq!(wait_limit(Some("500")), Duration::from_millis(500));
        assert_eq!(wait_limit(Some("999999999")), PERMISSION_BRIDGE_WAIT);
    }

    #[cfg(unix)]
    #[test]
    fn end_to_end_hook_bypasses_when_present_and_applies_in_process_decision_when_away() {
        let dir = tempfile::Builder::new().prefix("gp-pb-").tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let sock = base.join(SOCKET_NAME);
        let presence = Arc::new(MockPresenceProvider::new(UserPresence::Present));
        let keys = Arc::new(TrustedApprovalKeyStore::new());
        crate::alerts::approval_broker::configure_test_trust_anchor(&keys);
        let broker = Arc::new(
            ApprovalBroker::new(
                "host-pb-1",
                base.join("agent-mailbox"),
                presence.clone(),
                keys,
            )
            .with_repo_trust_checker(Arc::new(
                crate::alerts::approval_broker::MockRepoTrustChecker::new(true),
            ))
            .with_hold_ceiling(Duration::from_secs(30))
            .with_presence_poll_interval(Duration::from_millis(10)),
        );
        broker.set_remote_approval_active(true);
        let _server = ApprovalBrokerServer::bind_and_start(sock.clone(), broker.clone())
            .expect("bind approval broker");

        let input = sample_bash_input();
        // User present -> immediate bypass (empty stdout).
        assert_eq!(
            run_against_socket_with_trust_check(&input, &sock, Duration::from_secs(5), |_| true),
            None
        );

        // User away -> holds until resolved in-process, then emits AllowOnce JSON.
        presence.set(UserPresence::Away);
        let sock_bg = sock.clone();
        let input_bg = input.clone();
        let handle = std::thread::spawn(move || {
            run_against_socket_with_trust_check(&input_bg, &sock_bg, Duration::from_secs(5), |_| {
                true
            })
        });

        assert!(crate::test_support::wait_until(
            Duration::from_secs(2),
            || !broker.pending_tickets().is_empty()
        ));
        let ticket_id = broker.pending_tickets()[0].ticket_id.clone();
        broker.resolve_in_process(&ticket_id, ApprovalDecision::AllowOnce, None);

        let stdout = handle.join().unwrap().expect("expected allow json");
        assert_eq!(
            serde_json::from_str::<Value>(&stdout).unwrap(),
            json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": { "behavior": "allow" }
                }
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn untrusted_repository_bypasses_immediately_without_contacting_broker() {
        let dir = tempfile::Builder::new().prefix("gp-pb-ut-").tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let untrusted_repo = base.join("untrusted-repo");
        std::fs::create_dir_all(untrusted_repo.join(".git")).unwrap();
        let sock = base.join(SOCKET_NAME);

        let presence = Arc::new(MockPresenceProvider::new(UserPresence::Away));
        let keys = Arc::new(TrustedApprovalKeyStore::new());
        crate::alerts::approval_broker::configure_test_trust_anchor(&keys);
        // Even if the broker itself had a permissive checker, `run_against_socket` MUST
        // bypass locally via real `repository_trust::require` before touching the socket.
        let broker = Arc::new(
            ApprovalBroker::new(
                "host-pb-ut",
                base.join("agent-mailbox"),
                presence,
                keys,
            )
            .with_repo_trust_checker(Arc::new(
                crate::alerts::approval_broker::MockRepoTrustChecker::new(true),
            ))
            .with_hold_ceiling(Duration::from_secs(30)),
        );
        broker.set_remote_approval_active(true);
        let _server = ApprovalBrokerServer::bind_and_start(sock.clone(), broker.clone())
            .expect("bind approval broker");

        let mut input = sample_bash_input();
        input.cwd = untrusted_repo.to_string_lossy().into_owned();

        let t0 = std::time::Instant::now();
        assert_eq!(run_against_socket(&input, &sock, Duration::from_secs(5)), None);
        assert!(t0.elapsed() < Duration::from_millis(200));
        assert!(broker.pending_tickets().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn missing_broker_socket_bypasses_cleanly_with_empty_stdout() {
        let dir = tempfile::Builder::new().prefix("gp-pb-none-").tempdir().unwrap();
        let missing = dir.path().join("does-not-exist.sock");
        assert_eq!(
            run_against_socket_with_trust_check(
                &sample_bash_input(),
                &missing,
                Duration::from_secs(1),
                |_| true
            ),
            None
        );
    }
}
