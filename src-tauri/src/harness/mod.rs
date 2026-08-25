//! Integration with the MANVI coding-agent harness.
//!
//! GitPulse embeds MANVI the way its host plane is meant to be embedded: as a
//! `manvi serve` child speaking NDJSON over stdio. Three of its planes are
//! used — the command gate and the write gate guard every mutating Git action,
//! and the local-LLM plane (capability probing, token budgeting, completion
//! parsing) backs the AI features in `crate::ai`.
//!
//! Everything here degrades: with no harness installed GitPulse still commits,
//! pushes and rebases, and every unguarded action is reported as unguarded.

pub mod policy;
pub mod protocol;
pub mod sidecar;

use serde::{Deserialize, Serialize};

pub use policy::{check_command, check_file, render_command, PolicyStatus, PolicyVerdict};
pub use protocol::{HelloResult, PrepareResult, ProbeResult, SettleResult};
pub use sidecar::{HarnessError, DEFAULT_CALL_TIMEOUT};

/// What the UI shows about the harness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessStatus {
    pub available: bool,
    /// Absolute path of the binary in use, empty when none was found.
    pub binary: String,
    pub protocol: i64,
    pub posture: String,
    pub ops: Vec<String>,
    /// Empty when available; otherwise why not.
    pub error: String,
    pub error_code: String,
}

impl HarnessStatus {
    pub fn probe() -> Self {
        match sidecar::handshake() {
            Ok((binary, hello)) => HarnessStatus {
                available: true,
                binary,
                protocol: hello.protocol,
                posture: hello.posture,
                ops: hello.ops,
                error: String::new(),
                error_code: String::new(),
            },
            Err(e) => HarnessStatus {
                available: false,
                binary: sidecar::resolve_binary().unwrap_or_default(),
                protocol: 0,
                posture: String::new(),
                ops: Vec::new(),
                error: e.message(),
                error_code: e.code().to_string(),
            },
        }
    }
}

/// The single gate seam for guarded Git actions.
///
/// Every mutating command goes through here and nowhere else: `argv` is
/// rendered the way a shell would show it ([`render_command`]) so the
/// command gate judges the command the client actually runs, a blocking
/// verdict becomes the rendered refusal (`Err`), and otherwise the
/// [`PolicyVerdict`] is handed back so the caller can report an action that
/// ran unchecked when no harness is installed — which is never rendered as
/// an approval.
///
/// This used to exist twice (a generic closure-taking variant here, and a
/// private reimplementation in `commands`); this signature is the canonical
/// one both now share.
pub(crate) fn guard_command(repo_path: &str, argv: &[&str]) -> Result<PolicyVerdict, String> {
    let command = render_command(argv);
    gated(check_command(repo_path, &command))
}

/// Evaluates one file write, on the same terms as [`guard_command`].
pub(crate) fn guard_file(
    repo_path: &str,
    file_path: &str,
    op: &str,
) -> Result<PolicyVerdict, String> {
    gated(check_file(repo_path, file_path, op))
}

/// The single place a verdict becomes permission to act.
///
/// Two things refuse here, and the second is the one that is easy to miss. A
/// rule firing is obvious. A gate that *could not run* is not: an unchecked
/// verdict does not block, so before this seam refused it, a busy or wedged
/// sidecar let `git push --force` straight through while the UI recorded it as
/// merely "unguarded" — the check that could not run reporting exactly what a
/// check that ran and passed reports. See [`PolicyVerdict::gate_failed`] for
/// why a missing harness is the only unchecked verdict that may proceed.
fn gated(verdict: PolicyVerdict) -> Result<PolicyVerdict, String> {
    if verdict.blocks() {
        return Err(verdict.refusal());
    }
    if verdict.gate_failed() {
        return Err(verdict.gate_failure());
    }
    Ok(verdict)
}
