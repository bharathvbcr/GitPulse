//! Remote control plane host modules for `wayloded --relay`.
//!
//! All Noise `IK` E2EE, 1024-bit sliding replay windows, Ed25519 WS signing,
//! `DeviceCertificate` v2, `SignedRevocationList`, hardware P-256 approval &
//! steering verification, and canonical JSON hashing delegate strictly to the
//! single audited `waylode-protocol` crate (Opus R1/M4/#31).

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
    acquire_relay_lock, discover_trusted_repositories, evaluate_relay_gate, EffectiveRelayConfig,
    KeychainStatus, RemoteControlConfig, EXIT_CODE_FREE_OR_DISABLED,
};
use revocations::{PlatformTrustAnchor, TrustAnchorError, TrustAnchorProvider};
use serde::Serialize;
use std::path::Path;
use tunnel::{now_unix_ms, now_unix_secs, write_remote_approval_state, RemoteApprovalState};

/// Structured NDJSON status report emitted by `wayloded --relay` on startup
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

/// Production entrypoint for `wayloded --relay`.
///
/// Always enforces:
/// 1. **Hard Free/Local Invariant:** checks [`evaluate_relay_gate`] BEFORE
///    acquiring any lock or opening any Unix/network socket; exits **`4`** on
///    `free` tier or when `remote_control_enabled == false`.
/// 2. **Production Trust Anchor Invariant (Opus #15 & #31):** loads the
///    OS-backed [`PlatformTrustAnchor`] (macOS Data Protection Keychain Access
///    Group / Linux Secret Service) and NEVER falls back to an in-memory or
///    agent-writable file anchor. If no real platform trust anchor is
///    provisioned or the Keychain is locked, refuses with exit **`4`** and
///    emits `trust_anchor_unavailable` / `keychain_locked`.
pub fn run_relay_mode(
    config_dir: &Path,
    once: bool,
    pinned_org_key: Option<&ed25519_dalek::VerifyingKey>,
) -> i32 {
    let cfg = match RemoteControlConfig::load_from_dir(config_dir) {
        Ok(cfg) => cfg,
        Err(err) => {
            log::error!(target: "wayloded", "failed to load remote-control config: {err}");
            return 1;
        }
    };

    let effective: EffectiveRelayConfig =
        match evaluate_relay_gate(&cfg, pinned_org_key, now_unix_secs()) {
            Ok(effective) => effective,
            Err(refusal) => {
                log::error!(target: "wayloded", "{refusal}");
                return refusal.exit_code();
            }
        };

    // Production trust anchor ONLY — never falls back to an in-memory anchor!
    let platform_trust = PlatformTrustAnchor::new(&effective.keychain_access_group);
    run_relay_with_trust_anchor(config_dir, once, &effective, &platform_trust)
}

/// Core relay lifecycle parameterized over [`TrustAnchorProvider`].
///
/// Production [`run_relay_mode`] always supplies [`PlatformTrustAnchor`]; tests
/// may call this directly with a mock anchor to exercise the post-anchor path
/// without weakening production's fail-closed `PlatformTrustAnchor` requirement.
pub fn run_relay_with_trust_anchor(
    config_dir: &Path,
    once: bool,
    effective: &EffectiveRelayConfig,
    trust: &dyn TrustAnchorProvider,
) -> i32 {
    if let Err(err) = trust.verify_ready() {
        let code_reason = match &err {
            TrustAnchorError::KeychainLocked(reason) => format!("keychain_locked: {reason}"),
            TrustAnchorError::Unavailable(reason) => format!("trust_anchor_unavailable: {reason}"),
            TrustAnchorError::InvalidKeyMaterial(reason) => {
                format!("trust_anchor_unavailable: {reason}")
            }
        };
        log::error!(target: "wayloded", "refusing --relay startup: {code_reason}");
        let _ = write_remote_approval_state(
            config_dir,
            &RemoteApprovalState::ReleasedToNative {
                reason: code_reason,
                updated_at_ms: now_unix_ms(),
            },
        );
        return EXIT_CODE_FREE_OR_DISABLED;
    }

    let lock_dir = config_dir.join("wayloded");
    let _relay_lock = match acquire_relay_lock(&lock_dir, env!("CARGO_PKG_VERSION")) {
        crate::daemon_lock::Acquire::Held(lock) => lock,
        crate::daemon_lock::Acquire::Occupied(owner) => {
            log::error!(
                target: "wayloded",
                "another wayloded --relay already runs for this profile (pid {}, version {}); not starting a second",
                owner.pid.map_or("unknown".into(), |pid| pid.to_string()),
                owner.version.as_deref().unwrap_or("unknown"),
            );
            return 3;
        }
        crate::daemon_lock::Acquire::Unavailable(reason) => {
            log::error!(target: "wayloded", "the relay daemon lock could not be taken: {reason}");
            return 1;
        }
    };

    let keychain_status = trust.keychain_status();
    let discovery = discover_trusted_repositories(&effective.workspace_roots);
    let report = RelayStartupReport {
        mode: "relay",
        tier: effective.tier.as_str(),
        relay_url: effective.relay_url.clone(),
        org_id: effective.org_id.clone(),
        trusted_repos: discovery.trusted.len(),
        skipped_untrusted_repos: discovery.skipped_untrusted.len(),
        keychain_status,
        pty_input_entitled: effective.pty_input_entitled,
        status: "ready".into(),
    };

    if let Ok(line) = serde_json::to_string(&report) {
        println!("{line}");
    }

    if once {
        return 0;
    }

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            log::error!(target: "wayloded", "failed to start relay async runtime: {err}");
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
