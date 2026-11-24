//! Remote control plane host modules for `gitpulsed --relay`.
//!
//! - [`config`]: Tier gate (`free` / opt-out refuses with exit `4` before any
//!   socket or network call), Enterprise BYOC offline org-signed policy bundles,
//!   single-owner `gitpulsed-relay.lock`, Keychain non-blocking probe, and
//!   dynamic trusted-workspace discovery (`workspace_registry` + `repository_trust`).
//! - [`revocations`]: IdentityKey-signed monotonic revocation list cache
//!   (fails closed / releases remote holds when stale past TTL) and
//!   `DeviceCertificate` v2 verification anchored in OS Keychain / Secret Service.
//! - [`tunnel`]: Outbound-only WSS client (`tokio-tungstenite`), Noise `IK`
//!   E2EE per paired mobile device, `inbox_snapshot` re-announcement on connect,
//!   immediate `remote-approval.state` release on WS `4003`, and jittered
//!   `250ms..15s` reconnect backoff.
//! - [`ptyd_handoff`]: Opus R5 `TakeoverPtyd` → daemon close → `HANDOFF_GRACE`
//!   (`250ms`) post-FIN drain → bounded retry `Hello` sequence.
//! - [`pty_gate`]: Opus P0-7 read-only `<=16 KiB` lossy PTY streaming by default
//!   plus Desktop-consented, P-256 step-up-signed, audited `pty.input` RCE gate.
//! - [`codex_app_server`]: Codex Tier 1 `codex app-server` JSON-RPC stdio
//!   hosting with item-ID-bound approvals, true idle wake via `turn/start`, and
//!   strict refusal of remote `approval_policy` / `sandbox_mode` relaxation.
//! - [`manvi_tickets`]: Single-use, hash-bound, TTL'd Manvi policy override
//!   grants (never cached from mobile, hard-denies non-overridable, Enterprise
//!   `remote_approval` policy enforced).

pub mod codex_app_server;
pub mod config;
pub mod manvi_tickets;
pub mod pty_gate;
pub mod ptyd_handoff;
pub mod revocations;
pub mod tunnel;

#[cfg(test)]
mod tests;

use config::{
    acquire_relay_lock, discover_trusted_repositories, evaluate_relay_gate, probe_keychain_status,
    EffectiveRelayConfig, KeychainStatus, RemoteControlConfig,
};
use serde::Serialize;
use std::path::Path;
use tunnel::{now_unix_ms, now_unix_secs, write_remote_approval_state, RemoteApprovalState};

/// Structured NDJSON status report emitted by `gitpulsed --relay` on startup
/// or `--once` health probe.
#[derive(Debug, Clone, Serialize)]
pub struct RelayStartupReport {
    pub mode: &'static str,
    pub tier: &'static str,
    pub relay_url: String,
    pub org_id: Option<String>,
    pub trusted_repos: usize,
    pub skipped_untrusted_repos: usize,
    pub keychain_status: KeychainStatus,
    pub pty_input_entitled: bool,
    pub status: String,
}

/// Executes the `gitpulsed --relay` preflight, lock acquisition, and loop.
///
/// # Hard Free/Local Invariant
///
/// Checks [`evaluate_relay_gate`] FIRST — before acquiring any lock, binding
/// any Unix socket, or opening any network socket. On `free` tier or when
/// `remote_control_enabled == false`, immediately returns exit code `4`.
pub fn run_relay_mode(
    config_dir: &Path,
    once: bool,
    pinned_org_key: Option<&ed25519_dalek::VerifyingKey>,
) -> i32 {
    let cfg = match RemoteControlConfig::load_from_dir(config_dir) {
        Ok(cfg) => cfg,
        Err(err) => {
            log::error!(target: "gitpulsed", "failed to load remote-control config: {err}");
            return 1;
        }
    };

    let effective: EffectiveRelayConfig = match evaluate_relay_gate(&cfg, pinned_org_key, now_unix_secs()) {
        Ok(effective) => effective,
        Err(refusal) => {
            log::error!(target: "gitpulsed", "{refusal}");
            return refusal.exit_code();
        }
    };

    let lock_dir = config_dir.join("gitpulsed");
    let _relay_lock = match acquire_relay_lock(&lock_dir, env!("CARGO_PKG_VERSION")) {
        crate::daemon_lock::Acquire::Held(lock) => lock,
        crate::daemon_lock::Acquire::Occupied(owner) => {
            log::error!(
                target: "gitpulsed",
                "another gitpulsed --relay already runs for this profile (pid {}, version {}); not starting a second",
                owner.pid.map_or("unknown".into(), |pid| pid.to_string()),
                owner.version.as_deref().unwrap_or("unknown"),
            );
            return 3;
        }
        crate::daemon_lock::Acquire::Unavailable(reason) => {
            log::error!(target: "gitpulsed", "the relay daemon lock could not be taken: {reason}");
            return 1;
        }
    };

    let keychain_status = probe_keychain_status(&effective.keychain_access_group);
    if !keychain_status.is_unlocked() {
        let _ = write_remote_approval_state(
            config_dir,
            &RemoteApprovalState::ReleasedToNative {
                reason: "keychain_locked".into(),
                updated_at_ms: now_unix_ms(),
            },
        );
    }

    let discovery = discover_trusted_repositories(&effective.workspace_roots);
    let report = RelayStartupReport {
        mode: "relay",
        tier: effective.tier.as_str(),
        relay_url: effective.relay_url.clone(),
        org_id: effective.org_id.clone(),
        trusted_repos: discovery.trusted.len(),
        skipped_untrusted_repos: discovery.skipped_untrusted.len(),
        keychain_status: keychain_status.clone(),
        pty_input_entitled: effective.pty_input_entitled,
        status: if keychain_status.is_unlocked() {
            "ready".into()
        } else {
            "keychain_locked".into()
        },
    };

    if let Ok(line) = serde_json::to_string(&report) {
        println!("{line}");
    }

    if once {
        return 0;
    }

    // Long-lived relay loop: wait cleanly for shutdown signal while keeping the
    // single-owner lock held and re-checking keychain/config state cleanly.
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            log::error!(target: "gitpulsed", "failed to start relay async runtime: {err}");
            return 1;
        }
    };

    rt.block_on(async move {
        let _ = tokio::signal::ctrl_c().await;
    });
    let _ = write_remote_approval_state(
        config_dir,
        &RemoteApprovalState::ReleasedToNative {
            reason: "daemon_stopped".into(),
            updated_at_ms: now_unix_ms(),
        },
    );
    0
}
