//! Configuration, tier gating, Enterprise BYOC policy bundles, single-owner
//! relay lock (`gitpulsed-relay.lock`), Data Protection Keychain probe, and
//! dynamic trusted-workspace discovery for `gitpulsed --relay`.
//!
//! # Hard Free/Local Invariant
//!
//! On the `free` tier, or whenever `remote_control_enabled` is `false`,
//! `gitpulsed --relay` MUST refuse and exit with [`EXIT_CODE_FREE_OR_DISABLED`]
//! (`4`) **before** opening any Unix socket or network connection.

use crate::daemon_lock::{self, Acquire, Owner};
use crate::repository_trust;
use crate::workspace_registry;
use base64::Engine as _;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Exit code emitted by `gitpulsed --relay` when tier is `free` or remote
/// control opt-in is disabled. Guaranteed before opening any socket/network.
pub const EXIT_CODE_FREE_OR_DISABLED: i32 = 4;

/// Dedicated single-owner lock for `gitpulsed --relay`, distinct from
/// `gitpulsed.lock` so attribution catch-up and relay hosting never collide.
pub const RELAY_LOCK_FILE: &str = "gitpulsed-relay.lock";

/// Default cloud relay WebSocket URL for `pro` and `team` tiers.
pub const DEFAULT_CLOUD_RELAY_URL: &str = "wss://relay.gitpulse.dev";

/// Default macOS Data Protection Keychain Access Group shared between
/// GitPulse Desktop and `gitpulsed --relay` in notarized builds.
pub const DEFAULT_KEYCHAIN_ACCESS_GROUP: &str = "com.gitpulse.desktop.shared";

/// Persisted config file name inside the GitPulse config directory.
pub const REMOTE_CONFIG_FILE: &str = "remote-control.json";

/// Persisted state file observed by hooks/Desktop so active holds release to
/// native prompts immediately on downgrade, WS close `4003`, revocation
/// staleness, or keychain lock.
pub const REMOTE_APPROVAL_STATE_FILE: &str = "remote-approval.state";

const ORG_BUNDLE_DOMAIN: &[u8] = b"gitpulse-enterprise-policy/v1\0";
const CLOCK_SKEW_SECS: i64 = 300;
const MAX_ORG_BUNDLE_TTL_SECS: i64 = 365 * 24 * 3600;

/// Wire tier identifiers matching `gitpulse-relay`'s `internal/entitlements`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    #[default]
    Free,
    Pro,
    Team,
    Enterprise,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Free => "free",
            Tier::Pro => "pro",
            Tier::Team => "team",
            Tier::Enterprise => "enterprise",
        }
    }

    pub fn allows_remote_control(self) -> bool {
        matches!(self, Tier::Pro | Tier::Team | Tier::Enterprise)
    }
}

/// Enterprise policy governing remote approvals (Opus R9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RemoteApprovalPolicy {
    /// Standard hardware P-256 approval signature required (Opus R2).
    Allow,
    /// Requires fresh step-up biometric hardware P-256 signature on every action.
    #[default]
    StepUp,
    /// All remote approvals are refused; host stays observe-only for approvals.
    Deny,
}

/// Offline org-signed Enterprise license and policy bundle (Opus R9).
///
/// Verified against a pinned org Ed25519 verifying key from a non-agent-writable
/// trust root (MDM / Keychain / embedded enterprise key), never from an
/// agent-writable user file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgPolicyBundle {
    pub org_id: String,
    pub tier: Tier,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_url: Option<String>,
    pub remote_approval: RemoteApprovalPolicy,
    #[serde(default)]
    pub allow_pty_input: bool,
    /// Unix seconds.
    pub issued_at: i64,
    /// Unix seconds.
    pub expires_at: i64,
    pub key_id: String,
    /// Base64-encoded 64-byte Ed25519 signature over canonical bundle payload.
    pub sig_ed25519_b64: String,
}

impl OrgPolicyBundle {
    /// Canonical domain-separated bytes signed by the organization's Ed25519 key.
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hasher.update(ORG_BUNDLE_DOMAIN);
        let relay = self.relay_url.as_deref().unwrap_or("");
        let policy = match self.remote_approval {
            RemoteApprovalPolicy::Allow => "allow",
            RemoteApprovalPolicy::StepUp => "step_up",
            RemoteApprovalPolicy::Deny => "deny",
        };
        let canonical = format!(
            "org_id={}\ntier={}\nrelay_url={}\nremote_approval={}\nallow_pty_input={}\nissued_at={}\nexpires_at={}\nkey_id={}\n",
            self.org_id,
            self.tier.as_str(),
            relay,
            policy,
            self.allow_pty_input,
            self.issued_at,
            self.expires_at,
            self.key_id,
        );
        hasher.update(canonical.as_bytes());
        let mut out = Vec::with_capacity(ORG_BUNDLE_DOMAIN.len() + 32);
        out.extend_from_slice(ORG_BUNDLE_DOMAIN);
        out.extend_from_slice(&hasher.finalize());
        out
    }

    /// Creates and signs an offline Enterprise policy bundle (primarily for
    /// tests and enterprise provisioning tooling).
    #[allow(clippy::too_many_arguments)]
    pub fn sign(
        signing_key: &SigningKey,
        org_id: impl Into<String>,
        relay_url: Option<String>,
        remote_approval: RemoteApprovalPolicy,
        allow_pty_input: bool,
        issued_at: i64,
        expires_at: i64,
        key_id: impl Into<String>,
    ) -> Self {
        let mut bundle = Self {
            org_id: org_id.into(),
            tier: Tier::Enterprise,
            relay_url,
            remote_approval,
            allow_pty_input,
            issued_at,
            expires_at,
            key_id: key_id.into(),
            sig_ed25519_b64: String::new(),
        };
        let sig = signing_key.sign(&bundle.signing_bytes());
        bundle.sig_ed25519_b64 = base64::engine::general_purpose::STANDARD.encode(sig.to_bytes());
        bundle
    }

    /// Verifies signature, tier, time window, and field sanity against a pinned
    /// organization Ed25519 verifying key.
    pub fn verify(&self, pinned_org_key: &VerifyingKey, now_secs: i64) -> Result<(), String> {
        if self.org_id.trim().is_empty() || self.org_id.len() > 128 {
            return Err("enterprise policy bundle has invalid org_id".into());
        }
        if self.tier != Tier::Enterprise {
            return Err(format!(
                "enterprise policy bundle tier must be 'enterprise', got '{}'",
                self.tier.as_str()
            ));
        }
        if self.expires_at <= self.issued_at {
            return Err("enterprise policy bundle expires_at must be after issued_at".into());
        }
        if self.expires_at - self.issued_at > MAX_ORG_BUNDLE_TTL_SECS {
            return Err("enterprise policy bundle lifetime exceeds maximum 365-day ceiling".into());
        }
        if self.issued_at > now_secs + CLOCK_SKEW_SECS {
            return Err("enterprise policy bundle issued_at is in the future".into());
        }
        if now_secs >= self.expires_at {
            return Err("enterprise policy bundle is expired".into());
        }
        let sig_bytes = base64::engine::general_purpose::STANDARD
            .decode(self.sig_ed25519_b64.trim())
            .map_err(|e| format!("enterprise policy bundle signature is not valid base64: {e}"))?;
        let sig_arr: [u8; 64] = sig_bytes.try_into().map_err(|_| {
            "enterprise policy bundle signature must be exactly 64 bytes".to_string()
        })?;
        let signature = Signature::from_bytes(&sig_arr);
        pinned_org_key
            .verify(&self.signing_bytes(), &signature)
            .map_err(|_| "enterprise policy bundle signature verification failed".to_string())?;
        Ok(())
    }
}

/// Persisted remote-control configuration loaded by `gitpulsed --relay`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteControlConfig {
    #[serde(default)]
    pub tier: Tier,
    #[serde(default)]
    pub remote_control_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_url: Option<String>,
    /// Separate entitlement for `pty.input` RCE surface (Opus P0-7); off by default.
    #[serde(default)]
    pub pty_input_entitled: bool,
    #[serde(default = "default_keychain_access_group")]
    pub keychain_access_group: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_policy_bundle: Option<OrgPolicyBundle>,
    /// Optional workspace roots whose `.devmap/workspace.json` files are read
    /// dynamically by `gitpulsed --relay`.
    #[serde(default)]
    pub workspace_roots: Vec<PathBuf>,
    /// Loopback `ws://` override strictly for local/unit tests; never enabled
    /// for non-loopback URLs.
    #[serde(default)]
    pub allow_insecure_loopback: bool,
}

fn default_keychain_access_group() -> String {
    DEFAULT_KEYCHAIN_ACCESS_GROUP.to_string()
}

impl Default for RemoteControlConfig {
    fn default() -> Self {
        Self {
            tier: Tier::Free,
            remote_control_enabled: false,
            relay_url: None,
            pty_input_entitled: false,
            keychain_access_group: default_keychain_access_group(),
            org_policy_bundle: None,
            workspace_roots: Vec::new(),
            allow_insecure_loopback: false,
        }
    }
}

impl RemoteControlConfig {
    /// Loads `remote-control.json` from `config_dir`. When absent, returns the
    /// safe Free/disabled default so `gitpulsed --relay` refuses with exit 4.
    pub fn load_from_dir(config_dir: &Path) -> Result<Self, String> {
        let path = config_dir.join(REMOTE_CONFIG_FILE);
        match std::fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw)
                .map_err(|e| format!("failed to parse {}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("failed to read {}: {e}", path.display())),
        }
    }
}

/// Validated, ready-to-run relay settings produced only after all tier, opt-in,
/// URL, and Enterprise bundle checks succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveRelayConfig {
    pub tier: Tier,
    pub relay_url: String,
    pub remote_approval_policy: RemoteApprovalPolicy,
    pub pty_input_entitled: bool,
    pub keychain_access_group: String,
    pub org_id: Option<String>,
    pub workspace_roots: Vec<PathBuf>,
}

/// Why `gitpulsed --relay` refused to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayGateError {
    /// Hard Free/Local invariant: exit code `4`, zero sockets/network opened.
    FreeOrDisabled(String),
    /// Enterprise policy bundle missing signature / expired / invalid.
    InvalidEnterpriseBundle(String),
    /// Invalid or insecure relay URL.
    InvalidRelayUrl(String),
}

impl RelayGateError {
    pub fn exit_code(&self) -> i32 {
        match self {
            RelayGateError::FreeOrDisabled(_) => EXIT_CODE_FREE_OR_DISABLED,
            RelayGateError::InvalidEnterpriseBundle(_) | RelayGateError::InvalidRelayUrl(_) => 1,
        }
    }
}

impl std::fmt::Display for RelayGateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelayGateError::FreeOrDisabled(msg)
            | RelayGateError::InvalidEnterpriseBundle(msg)
            | RelayGateError::InvalidRelayUrl(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for RelayGateError {}

/// Validates whether `gitpulsed --relay` is permitted to start.
///
/// Must be called **before** acquiring locks, binding sockets, or opening any
/// network connection. Returns `Err(RelayGateError::FreeOrDisabled(_))`
/// (exit code `4`) whenever `tier == Free` or `!remote_control_enabled`.
pub fn evaluate_relay_gate(
    cfg: &RemoteControlConfig,
    pinned_org_key: Option<&VerifyingKey>,
    now_secs: i64,
) -> Result<EffectiveRelayConfig, RelayGateError> {
    if !cfg.tier.allows_remote_control() {
        return Err(RelayGateError::FreeOrDisabled(
            "remote control relay is disabled on the free tier; refusing to open any socket or network connection"
                .into(),
        ));
    }
    if !cfg.remote_control_enabled {
        return Err(RelayGateError::FreeOrDisabled(
            "remote_control opt-in is disabled; refusing to open any socket or network connection"
                .into(),
        ));
    }

    let mut remote_approval_policy = RemoteApprovalPolicy::Allow;
    let mut pty_input_entitled = cfg.pty_input_entitled;
    let mut org_id = None;
    let mut relay_url = cfg
        .relay_url
        .clone()
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_CLOUD_RELAY_URL.to_string());

    if cfg.tier == Tier::Enterprise {
        if let Some(bundle) = &cfg.org_policy_bundle {
            let org_key = pinned_org_key.ok_or_else(|| {
                RelayGateError::InvalidEnterpriseBundle(
                    "enterprise policy bundle present but no pinned org verifying key was supplied"
                        .into(),
                )
            })?;
            bundle
                .verify(org_key, now_secs)
                .map_err(RelayGateError::InvalidEnterpriseBundle)?;
            remote_approval_policy = bundle.remote_approval;
            // Org bundle can only narrow, never unilaterally enable pty_input
            // without explicit local entitlement too.
            pty_input_entitled = cfg.pty_input_entitled && bundle.allow_pty_input;
            org_id = Some(bundle.org_id.clone());
            if let Some(bundle_url) = &bundle.relay_url {
                if !bundle_url.trim().is_empty() {
                    relay_url = bundle_url.clone();
                }
            }
        } else if cfg
            .relay_url
            .as_deref()
            .is_some_and(|u| u.trim() != DEFAULT_CLOUD_RELAY_URL)
        {
            return Err(RelayGateError::InvalidEnterpriseBundle(
                "enterprise BYOC relay_url requires a valid offline org-signed policy bundle"
                    .into(),
            ));
        }
    } else if cfg
        .relay_url
        .as_deref()
        .is_some_and(|u| !u.trim().is_empty() && u.trim() != DEFAULT_CLOUD_RELAY_URL)
        && !cfg.allow_insecure_loopback
    {
        return Err(RelayGateError::InvalidRelayUrl(format!(
            "custom BYOC relay_url is only permitted on enterprise tier (got tier '{}')",
            cfg.tier.as_str()
        )));
    }

    validate_relay_url(&relay_url, cfg.allow_insecure_loopback)?;

    Ok(EffectiveRelayConfig {
        tier: cfg.tier,
        relay_url,
        remote_approval_policy,
        pty_input_entitled,
        keychain_access_group: cfg.keychain_access_group.clone(),
        org_id,
        workspace_roots: cfg.workspace_roots.clone(),
    })
}

fn validate_relay_url(raw: &str, allow_insecure_loopback: bool) -> Result<(), RelayGateError> {
    let parsed = url::Url::parse(raw)
        .map_err(|e| RelayGateError::InvalidRelayUrl(format!("invalid relay_url {raw:?}: {e}")))?;
    match parsed.scheme() {
        "wss" => {
            if parsed.host_str().is_none() {
                return Err(RelayGateError::InvalidRelayUrl(
                    "relay_url must include a host".into(),
                ));
            }
            Ok(())
        }
        "ws" if allow_insecure_loopback => {
            let is_loopback = matches!(
                parsed.host_str(),
                Some("127.0.0.1") | Some("localhost") | Some("[::1]") | Some("::1")
            );
            if !is_loopback {
                return Err(RelayGateError::InvalidRelayUrl(
                    "plaintext ws:// is refused for non-loopback hosts".into(),
                ));
            }
            Ok(())
        }
        other => Err(RelayGateError::InvalidRelayUrl(format!(
            "relay_url scheme must be wss:// (got {other}://)"
        ))),
    }
}

/// Acquires the dedicated `gitpulsed-relay.lock` single-owner lock.
pub fn acquire_relay_lock(dir: &Path, version: &str) -> Acquire {
    daemon_lock::acquire_named(dir, RELAY_LOCK_FILE, version)
}

/// Probes whether another `gitpulsed --relay` holds `gitpulsed-relay.lock`.
pub fn relay_running(dir: &Path) -> Result<Option<Owner>, String> {
    daemon_lock::running_named(dir, RELAY_LOCK_FILE)
}

/// One dynamically discovered, trust-verified repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedWorkspaceRepo {
    pub name: String,
    pub root: PathBuf,
    pub db_path: PathBuf,
}

/// Result of dynamically reading `workspace_registry` + `repository_trust`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceDiscovery {
    pub trusted: Vec<TrustedWorkspaceRepo>,
    pub skipped_untrusted: Vec<String>,
    pub errors: Vec<String>,
}

/// Reads registered workspaces dynamically via `workspace_registry::list` and
/// filters them through `repository_trust::require` so `gitpulsed --relay`
/// never touches untrusted repositories and needs no `<REPO>...` CLI arguments.
pub fn discover_trusted_repositories(workspace_roots: &[PathBuf]) -> WorkspaceDiscovery {
    let mut discovery = WorkspaceDiscovery::default();
    let mut seen = std::collections::HashSet::new();

    for root in workspace_roots {
        let root_str = root.to_string_lossy();
        match workspace_registry::list(&root_str) {
            Ok(snapshot) => {
                for entry in snapshot.repos {
                    if !seen.insert(entry.root.clone()) {
                        continue;
                    }
                    let repo_path = Path::new(&entry.root);
                    match repository_trust::require(repo_path) {
                        Ok(()) => discovery.trusted.push(TrustedWorkspaceRepo {
                            name: entry.name,
                            root: PathBuf::from(entry.root),
                            db_path: PathBuf::from(entry.db_path),
                        }),
                        Err(_) => discovery.skipped_untrusted.push(entry.root),
                    }
                }
            }
            Err(err) => {
                discovery
                    .errors
                    .push(format!("{}: {}", root.display(), err.message()));
            }
        }
    }

    discovery
}

/// Status of the host Keychain / Data Protection Access Group.
///
/// Reported explicitly so `gitpulsed --relay` never hangs waiting on a locked
/// macOS login Keychain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum KeychainStatus {
    Unlocked { access_group: String },
    KeychainLocked { reason: String },
    Unavailable { reason: String },
}

impl KeychainStatus {
    pub fn is_unlocked(&self) -> bool {
        matches!(self, KeychainStatus::Unlocked { .. })
    }

    pub fn is_locked(&self) -> bool {
        matches!(self, KeychainStatus::KeychainLocked { .. })
    }
}

/// Pluggable non-blocking Keychain status probe for tests and platform drivers.
pub trait KeychainProbe: Send + Sync {
    fn probe(&self, access_group: &str) -> KeychainStatus;
}

/// Platform Keychain probe. On macOS checks `SecKeychainGetStatus` in `<1ms`
/// without triggering any GUI unlock prompt.
#[derive(Debug, Default, Clone)]
pub struct PlatformKeychainProbe;

impl KeychainProbe for PlatformKeychainProbe {
    fn probe(&self, access_group: &str) -> KeychainStatus {
        probe_keychain_status(access_group)
    }
}

#[cfg(target_os = "macos")]
mod macos_keychain_probe {
    use std::ffi::c_void;

    type SecKeychainRef = *mut c_void;
    type CFTypeRef = *const c_void;
    type OSStatus = i32;

    const K_SEC_UNLOCK_STATE_STATUS: u32 = 1;
    const K_SEC_READ_PERM_STATUS: u32 = 2;

    #[link(name = "Security", kind = "framework")]
    extern "C" {
        fn SecKeychainCopyDefault(keychain: *mut SecKeychainRef) -> OSStatus;
        fn SecKeychainGetStatus(keychain: SecKeychainRef, keychain_status: *mut u32) -> OSStatus;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(cf: CFTypeRef);
    }

    pub(super) fn probe(access_group: &str) -> super::KeychainStatus {
        let mut kc: SecKeychainRef = std::ptr::null_mut();
        // SAFETY: `kc` is a valid out-pointer; retained by Security.framework on rc == 0.
        let copy_rc = unsafe { SecKeychainCopyDefault(&mut kc) };
        if copy_rc != 0 || kc.is_null() {
            return super::KeychainStatus::Unavailable {
                reason: format!("no default macOS Keychain available (OSStatus {copy_rc})"),
            };
        }
        let mut status: u32 = 0;
        // SAFETY: `kc` is a valid retained SecKeychainRef and `status` is an initialized out-pointer.
        let status_rc = unsafe { SecKeychainGetStatus(kc, &mut status) };
        // SAFETY: `kc` was retained by SecKeychainCopyDefault and is non-null.
        unsafe { CFRelease(kc as CFTypeRef) };

        if status_rc != 0 {
            return super::KeychainStatus::Unavailable {
                reason: format!("macOS Keychain status unreadable (OSStatus {status_rc})"),
            };
        }
        if (status & K_SEC_UNLOCK_STATE_STATUS) == 0 {
            return super::KeychainStatus::KeychainLocked {
                reason: "macOS Keychain is locked; waiting for unlock or Data Protection handoff"
                    .to_string(),
            };
        }
        if (status & K_SEC_READ_PERM_STATUS) == 0 {
            return super::KeychainStatus::KeychainLocked {
                reason: "macOS Keychain is not readable without user unlock".to_string(),
            };
        }
        super::KeychainStatus::Unlocked {
            access_group: access_group.to_string(),
        }
    }
}

/// Non-blocking (<1ms) Keychain probe. Never triggers a GUI prompt or hangs.
pub fn probe_keychain_status(access_group: &str) -> KeychainStatus {
    if access_group.trim().is_empty() {
        return KeychainStatus::Unavailable {
            reason: "keychain_access_group is empty".into(),
        };
    }
    #[cfg(target_os = "macos")]
    {
        macos_keychain_probe::probe(access_group)
    }
    #[cfg(not(target_os = "macos"))]
    {
        KeychainStatus::Unlocked {
            access_group: access_group.to_string(),
        }
    }
}
