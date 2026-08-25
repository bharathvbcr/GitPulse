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

/// Runs `action` only if the command gate does not refuse it.
///
/// The verdict travels with the result either way, so a caller can report an
/// action that ran without being checked as exactly that.
pub fn guard_command<T>(
    root: &str,
    argv: &[&str],
    action: impl FnOnce() -> Result<T, String>,
) -> (PolicyVerdict, Result<T, String>) {
    let command = render_command(argv);
    let verdict = check_command(root, &command);
    if verdict.blocks() {
        let refusal = verdict.refusal();
        return (verdict, Err(refusal));
    }
    let outcome = action();
    (verdict, outcome)
}
