//! Host revocation list cache and OS Keychain-backed trust anchors
//! (Opus R3, board #15, and board #31).
//!
//! All certificate, revocation-list, and signature verification delegates
//! strictly to the single audited `waylode-protocol` crate (zero local
//! crypto reimplementation).
//!
//! # Security Invariant (Opus Board #15 & #31)
//!
//! Agents run as the same UID as Waylode, so an agent-writable `0600` file
//! MUST NEVER be trusted as the root of trust for the Account `IdentityKey` or
//! paired mobile P-256 approval keys.
//! - Production (`run_relay_mode`) uses [`PlatformTrustAnchor`] backed by the
//!   macOS Data Protection Keychain Access Group (`SecItemCopyMatching` with
//!   `kSecUseDataProtectionKeychain = kCFBooleanTrue` + `kSecAttrAccessGroup`,
//!   preceded by a non-blocking `<1ms` lock check) or Linux Secret Service.
//! - If no real platform trust anchor is provisioned or the Keychain is locked,
//!   [`PlatformTrustAnchor`] fails closed (`TrustAnchorError::Unavailable` /
//!   `KeychainLocked`) and `wayloded --relay` refuses with exit code `4`.
//! - [`InMemoryTrustAnchor`] is strictly `#[cfg(test)]`-only.

use crate::remote::config::{probe_keychain_status, KeychainStatus};
use std::collections::HashSet;
use std::time::Duration;
pub use waylode_protocol::crypto::{
    issue_device_certificate, issue_revocation_list, verify_device_certificate,
    verify_revocation_list, RevocationPolicy,
};
pub use waylode_protocol::wire::{DeviceCertificate, DeviceKind, SignedRevocationList};
use waylode_protocol::ProtocolError;
use zeroize::Zeroizing;

/// Default staleness ceiling for the host's cached revocation list (15 minutes).
pub const DEFAULT_REVOCATION_STALE_TTL: Duration = Duration::from_secs(15 * 60);

/// Keychain service identifier for Waylode remote-control trust anchors.
pub const TRUST_ANCHOR_SERVICE: &str = "com.gitpulse.desktop.remote-trust";
/// Keychain account name storing the 32-byte pinned Account IdentityKey Ed25519 pubkey.
pub const IDENTITY_PUB_ACCOUNT: &str = "account-identity-ed25519-pub";
/// Keychain account name storing the 32-byte host device Ed25519 signing seed.
pub const HOST_ED25519_SEED_ACCOUNT: &str = "host-device-ed25519-seed";
/// Keychain account name storing the 32-byte host static X25519 Noise IK secret.
pub const HOST_NOISE_X25519_ACCOUNT: &str = "host-noise-static-x25519";

/// Errors produced when loading trust anchors from the platform Keychain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustAnchorError {
    KeychainLocked(String),
    Unavailable(String),
    InvalidKeyMaterial(String),
}

impl std::fmt::Display for TrustAnchorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrustAnchorError::KeychainLocked(msg) => write!(f, "keychain_locked: {msg}"),
            TrustAnchorError::Unavailable(msg) => write!(f, "trust_anchor_unavailable: {msg}"),
            TrustAnchorError::InvalidKeyMaterial(msg) => write!(f, "invalid key material: {msg}"),
        }
    }
}

impl std::error::Error for TrustAnchorError {}

/// Source of pinned trust anchors and host secrets (OS Keychain / Secret
/// Service in production; never an agent-writable `0600` file).
pub trait TrustAnchorProvider: Send + Sync {
    /// Non-blocking Keychain status check.
    fn keychain_status(&self) -> KeychainStatus;
    /// Pinned Account `IdentityKey` Ed25519 public key (32 bytes) to which all
    /// `DeviceCertificate`s and `SignedRevocationList`s chain.
    fn pinned_identity_key_ed25519(&self) -> Result<[u8; 32], TrustAnchorError>;
    /// This host device's Ed25519 signing seed (32 bytes) for relay WS auth.
    fn host_device_ed25519_seed(&self) -> Result<Zeroizing<[u8; 32]>, TrustAnchorError>;
    /// This host device's static X25519 secret key (32 bytes) for Noise `IK`.
    fn host_noise_static_x25519(&self) -> Result<Zeroizing<[u8; 32]>, TrustAnchorError>;

    /// Verifies that every required trust anchor and host secret is present and
    /// readable without prompting. Fails closed on locked/missing anchors.
    fn verify_ready(&self) -> Result<(), TrustAnchorError> {
        match self.keychain_status() {
            KeychainStatus::Unlocked { .. } => {}
            KeychainStatus::KeychainLocked { reason } => {
                return Err(TrustAnchorError::KeychainLocked(reason));
            }
            KeychainStatus::Unavailable { reason } => {
                return Err(TrustAnchorError::Unavailable(reason));
            }
        }
        let _ = self.pinned_identity_key_ed25519()?;
        let _ = self.host_device_ed25519_seed()?;
        let _ = self.host_noise_static_x25519()?;
        Ok(())
    }
}

/// Production OS-backed trust anchor provider.
///
/// - On macOS: queries the Data Protection Keychain (`kSecUseDataProtectionKeychain`)
///   scoped to `access_group` via Security.framework `SecItemCopyMatching`,
///   after a non-blocking `<1ms` lock check (`probe_keychain_status`). Never
///   reads keys from an agent-writable `0600` file and never prompts.
/// - On Linux / other platforms: fails closed with `trust_anchor_unavailable`
///   unless backed by an OS Secret Service integration.
#[derive(Debug, Clone)]
pub struct PlatformTrustAnchor {
    access_group: String,
    /// Read only by the macOS Keychain lookup; elsewhere the anchor fails closed.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    service: String,
}

impl PlatformTrustAnchor {
    pub fn new(access_group: impl Into<String>) -> Self {
        Self {
            access_group: access_group.into(),
            service: TRUST_ANCHOR_SERVICE.to_string(),
        }
    }

    pub fn access_group(&self) -> &str {
        &self.access_group
    }

    fn read_fixed_32(&self, account: &str) -> Result<Zeroizing<[u8; 32]>, TrustAnchorError> {
        match self.keychain_status() {
            KeychainStatus::Unlocked { .. } => {}
            KeychainStatus::KeychainLocked { reason } => {
                return Err(TrustAnchorError::KeychainLocked(reason));
            }
            KeychainStatus::Unavailable { reason } => {
                return Err(TrustAnchorError::Unavailable(reason));
            }
        }
        #[cfg(target_os = "macos")]
        {
            macos_data_protection::read_item_32(&self.access_group, &self.service, account)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = account;
            Err(TrustAnchorError::Unavailable(
                "OS Secret Service trust anchor is not provisioned on this platform".into(),
            ))
        }
    }
}

impl TrustAnchorProvider for PlatformTrustAnchor {
    fn keychain_status(&self) -> KeychainStatus {
        probe_keychain_status(&self.access_group)
    }

    fn pinned_identity_key_ed25519(&self) -> Result<[u8; 32], TrustAnchorError> {
        let bytes = self.read_fixed_32(IDENTITY_PUB_ACCOUNT)?;
        Ok(*bytes)
    }

    fn host_device_ed25519_seed(&self) -> Result<Zeroizing<[u8; 32]>, TrustAnchorError> {
        self.read_fixed_32(HOST_ED25519_SEED_ACCOUNT)
    }

    fn host_noise_static_x25519(&self) -> Result<Zeroizing<[u8; 32]>, TrustAnchorError> {
        self.read_fixed_32(HOST_NOISE_X25519_ACCOUNT)
    }
}

#[cfg(target_os = "macos")]
mod macos_data_protection {
    use super::TrustAnchorError;
    use std::ffi::c_void;
    use zeroize::Zeroizing;

    type CFTypeRef = *const c_void;
    type CFStringRef = *const c_void;
    type CFDataRef = *const c_void;
    type CFDictionaryRef = *const c_void;
    type CFBooleanRef = *const c_void;
    type CFAllocatorRef = *const c_void;
    type CFIndex = i64;
    type OSStatus = i32;

    const ERR_SEC_SUCCESS: OSStatus = 0;
    const ERR_SEC_ITEM_NOT_FOUND: OSStatus = -25300;
    const ERR_SEC_INTERACTION_NOT_ALLOWED: OSStatus = -25308;
    const ERR_SEC_MISSING_ENTITLEMENT: OSStatus = -34018;
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

    #[link(name = "Security", kind = "framework")]
    extern "C" {
        static kSecClass: CFStringRef;
        static kSecClassGenericPassword: CFStringRef;
        static kSecAttrService: CFStringRef;
        static kSecAttrAccount: CFStringRef;
        static kSecAttrAccessGroup: CFStringRef;
        static kSecUseDataProtectionKeychain: CFStringRef;
        static kSecReturnData: CFStringRef;
        static kSecMatchLimit: CFStringRef;
        static kSecMatchLimitOne: CFStringRef;
        fn SecItemCopyMatching(query: CFDictionaryRef, result: *mut CFTypeRef) -> OSStatus;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFBooleanTrue: CFBooleanRef;
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
        fn CFStringCreateWithBytes(
            alloc: CFAllocatorRef,
            bytes: *const u8,
            num_bytes: CFIndex,
            encoding: u32,
            is_external_representation: u8,
        ) -> CFStringRef;
        fn CFDictionaryCreate(
            allocator: CFAllocatorRef,
            keys: *const CFTypeRef,
            values: *const CFTypeRef,
            num_values: CFIndex,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> CFDictionaryRef;
        fn CFDataGetLength(the_data: CFDataRef) -> CFIndex;
        fn CFDataGetBytePtr(the_data: CFDataRef) -> *const u8;
        fn CFRelease(cf: CFTypeRef);
    }

    struct CfGuard(CFTypeRef);
    impl Drop for CfGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: `self.0` is a valid owned CoreFoundation object.
                unsafe { CFRelease(self.0) };
            }
        }
    }

    fn cf_string(s: &str) -> Result<CfGuard, TrustAnchorError> {
        // SAFETY: `s.as_ptr()` is valid UTF-8 for `s.len()` bytes.
        let ptr = unsafe {
            CFStringCreateWithBytes(
                std::ptr::null(),
                s.as_ptr(),
                s.len() as CFIndex,
                K_CF_STRING_ENCODING_UTF8,
                0,
            )
        };
        if ptr.is_null() {
            return Err(TrustAnchorError::Unavailable(
                "CFStringCreateWithBytes returned null".into(),
            ));
        }
        Ok(CfGuard(ptr))
    }

    /// Reads a 32-byte key from the macOS Data Protection Keychain Access Group
    /// via `SecItemCopyMatching` without ever prompting the user.
    pub(super) fn read_item_32(
        access_group: &str,
        service: &str,
        account: &str,
    ) -> Result<Zeroizing<[u8; 32]>, TrustAnchorError> {
        let cf_service = cf_string(service)?;
        let cf_account = cf_string(account)?;
        let cf_group = cf_string(access_group)?;

        // SAFETY: Security.framework and CoreFoundation extern statics are valid
        // constant CFTypeRefs for the lifetime of the process.
        let (keys, values) = unsafe {
            (
                [
                    kSecClass as CFTypeRef,
                    kSecAttrService as CFTypeRef,
                    kSecAttrAccount as CFTypeRef,
                    kSecAttrAccessGroup as CFTypeRef,
                    kSecUseDataProtectionKeychain as CFTypeRef,
                    kSecReturnData as CFTypeRef,
                    kSecMatchLimit as CFTypeRef,
                ],
                [
                    kSecClassGenericPassword as CFTypeRef,
                    cf_service.0,
                    cf_account.0,
                    cf_group.0,
                    kCFBooleanTrue as CFTypeRef,
                    kCFBooleanTrue as CFTypeRef,
                    kSecMatchLimitOne as CFTypeRef,
                ],
            )
        };

        // SAFETY: `keys` and `values` have identical length (7) and remain alive
        // throughout `CFDictionaryCreate`.
        let dict_ptr = unsafe {
            CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                keys.len() as CFIndex,
                &kCFTypeDictionaryKeyCallBacks as *const c_void,
                &kCFTypeDictionaryValueCallBacks as *const c_void,
            )
        };
        if dict_ptr.is_null() {
            return Err(TrustAnchorError::Unavailable(
                "CFDictionaryCreate returned null".into(),
            ));
        }
        let _dict = CfGuard(dict_ptr);

        let mut out: CFTypeRef = std::ptr::null();
        // SAFETY: `dict_ptr` is a valid CFDictionaryRef and `out` is a valid out-pointer.
        let status = unsafe { SecItemCopyMatching(dict_ptr, &mut out) };
        let _out_guard = CfGuard(out);

        match status {
            ERR_SEC_SUCCESS => {
                if out.is_null() {
                    return Err(TrustAnchorError::Unavailable(format!(
                        "Keychain item '{account}' returned empty data"
                    )));
                }
                // SAFETY: `kSecReturnData = true` guarantees `out` is a CFDataRef on success.
                let len = unsafe { CFDataGetLength(out as CFDataRef) };
                if len != 32 {
                    return Err(TrustAnchorError::InvalidKeyMaterial(format!(
                        "Keychain item '{account}' expected 32 bytes, got {len}"
                    )));
                }
                // SAFETY: `len == 32`, non-null CFDataRef kept alive by `_out_guard`.
                let slice = unsafe { std::slice::from_raw_parts(CFDataGetBytePtr(out as CFDataRef), 32) };
                let mut key = Zeroizing::new([0u8; 32]);
                key.copy_from_slice(slice);
                Ok(key)
            }
            ERR_SEC_INTERACTION_NOT_ALLOWED => Err(TrustAnchorError::KeychainLocked(format!(
                "Data Protection Keychain locked for '{account}' (OSStatus {status})"
            ))),
            ERR_SEC_ITEM_NOT_FOUND => Err(TrustAnchorError::Unavailable(format!(
                "Keychain trust anchor '{account}' not provisioned in access group '{access_group}'"
            ))),
            ERR_SEC_MISSING_ENTITLEMENT => Err(TrustAnchorError::Unavailable(format!(
                "Data Protection Keychain entitlement missing for access group '{access_group}' (OSStatus {status})"
            ))),
            other => Err(TrustAnchorError::Unavailable(format!(
                "Data Protection Keychain lookup for '{account}' failed (OSStatus {other})"
            ))),
        }
    }
}

/// Test-only in-memory trust anchor provider.
///
/// Strictly `#[cfg(test)]` so production binaries can NEVER silently run on an
/// in-memory or file-loaded trust anchor (Opus board #15 & #31).
#[cfg(test)]
#[derive(Clone)]
pub struct InMemoryTrustAnchor {
    pub status: KeychainStatus,
    pub identity_pub_ed25519: [u8; 32],
    pub host_ed25519_seed: Zeroizing<[u8; 32]>,
    pub host_noise_static_x25519: Zeroizing<[u8; 32]>,
}

#[cfg(test)]
impl InMemoryTrustAnchor {
    pub fn unlocked(
        identity_pub_ed25519: [u8; 32],
        host_ed25519_seed: [u8; 32],
        host_noise_static_x25519: [u8; 32],
    ) -> Self {
        Self {
            status: KeychainStatus::Unlocked {
                access_group: crate::remote::config::DEFAULT_KEYCHAIN_ACCESS_GROUP.to_string(),
            },
            identity_pub_ed25519,
            host_ed25519_seed: Zeroizing::new(host_ed25519_seed),
            host_noise_static_x25519: Zeroizing::new(host_noise_static_x25519),
        }
    }

    pub fn locked(reason: impl Into<String>) -> Self {
        Self {
            status: KeychainStatus::KeychainLocked {
                reason: reason.into(),
            },
            identity_pub_ed25519: [0u8; 32],
            host_ed25519_seed: Zeroizing::new([0u8; 32]),
            host_noise_static_x25519: Zeroizing::new([0u8; 32]),
        }
    }
}

#[cfg(test)]
impl TrustAnchorProvider for InMemoryTrustAnchor {
    fn keychain_status(&self) -> KeychainStatus {
        self.status.clone()
    }

    fn pinned_identity_key_ed25519(&self) -> Result<[u8; 32], TrustAnchorError> {
        match &self.status {
            KeychainStatus::Unlocked { .. } => Ok(self.identity_pub_ed25519),
            KeychainStatus::KeychainLocked { reason } => {
                Err(TrustAnchorError::KeychainLocked(reason.clone()))
            }
            KeychainStatus::Unavailable { reason } => {
                Err(TrustAnchorError::Unavailable(reason.clone()))
            }
        }
    }

    fn host_device_ed25519_seed(&self) -> Result<Zeroizing<[u8; 32]>, TrustAnchorError> {
        match &self.status {
            KeychainStatus::Unlocked { .. } => Ok(self.host_ed25519_seed.clone()),
            KeychainStatus::KeychainLocked { reason } => {
                Err(TrustAnchorError::KeychainLocked(reason.clone()))
            }
            KeychainStatus::Unavailable { reason } => {
                Err(TrustAnchorError::Unavailable(reason.clone()))
            }
        }
    }

    fn host_noise_static_x25519(&self) -> Result<Zeroizing<[u8; 32]>, TrustAnchorError> {
        match &self.status {
            KeychainStatus::Unlocked { .. } => Ok(self.host_noise_static_x25519.clone()),
            KeychainStatus::KeychainLocked { reason } => {
                Err(TrustAnchorError::KeychainLocked(reason.clone()))
            }
            KeychainStatus::Unavailable { reason } => {
                Err(TrustAnchorError::Unavailable(reason.clone()))
            }
        }
    }
}

/// Host-side cache for the Account IdentityKey-signed [`SignedRevocationList`]
/// (Opus R3). Delegates all cryptographic verification to `waylode-protocol`.
#[derive(Debug, Clone)]
pub struct RevocationCache {
    identity_pub_ed25519: [u8; 32],
    stale_ttl: Duration,
    current_list: Option<SignedRevocationList>,
    current_version: u64,
    last_refreshed_at: Option<i64>,
    revoked_ids: HashSet<String>,
}

impl RevocationCache {
    /// Creates a new fail-closed cache pinned to the IdentityKey loaded from
    /// a [`TrustAnchorProvider`]. Starts empty/stale until a valid signed list
    /// is ingested.
    pub fn from_trust_anchor(
        provider: &dyn TrustAnchorProvider,
        stale_ttl: Duration,
    ) -> Result<Self, TrustAnchorError> {
        let pub_bytes = provider.pinned_identity_key_ed25519()?;
        Ok(Self::new(pub_bytes, stale_ttl))
    }

    pub fn new(identity_pub_ed25519: [u8; 32], stale_ttl: Duration) -> Self {
        Self {
            identity_pub_ed25519,
            stale_ttl,
            current_list: None,
            current_version: 0,
            last_refreshed_at: None,
            revoked_ids: HashSet::new(),
        }
    }

    pub fn identity_pub_ed25519(&self) -> &[u8; 32] {
        &self.identity_pub_ed25519
    }

    pub fn current_version(&self) -> u64 {
        self.current_version
    }

    /// Ingests a freshly fetched [`SignedRevocationList`] via
    /// `waylode_protocol::crypto::verify_revocation_list`, enforcing strict
    /// Ed25519 verification, monotonic version progression, and expiry bounds.
    pub fn update(
        &mut self,
        list: SignedRevocationList,
        now_secs: i64,
    ) -> Result<(), ProtocolError> {
        let min_version = if self.last_refreshed_at.is_some() {
            self.current_version
        } else {
            0
        };
        verify_revocation_list(&list, &self.identity_pub_ed25519, min_version, now_secs)?;

        self.current_version = list.version;
        self.revoked_ids = list.revoked_device_ids.iter().cloned().collect();
        self.current_list = Some(list);
        self.last_refreshed_at = Some(now_secs);
        Ok(())
    }

    /// Returns `true` when no valid list has been loaded, the list is expired,
    /// or `stale_ttl` has elapsed since the last verified refresh.
    pub fn is_stale(&self, now_secs: i64) -> bool {
        let (Some(refreshed), Some(list)) = (self.last_refreshed_at, &self.current_list) else {
            return true;
        };
        if now_secs >= list.expires_at {
            return true;
        }
        let age = now_secs.saturating_sub(refreshed);
        age > i64::try_from(self.stale_ttl.as_secs()).unwrap_or(i64::MAX)
    }

    /// Opus R3: if the host cannot refresh its IdentityKey-signed revocation
    /// list within TTL, it MUST fail closed and bypass remote approval holds.
    pub fn should_bypass_remote_holds(&self, now_secs: i64) -> bool {
        self.is_stale(now_secs)
    }

    pub fn is_revoked(&self, device_id: &str) -> bool {
        self.revoked_ids.contains(device_id)
    }

    /// Produces a mandatory [`RevocationPolicy::Enforced`] for `waylode-protocol`
    /// verifiers. Fails closed with [`ProtocolError::Expired`] whenever the
    /// cache is missing or stale past TTL.
    pub fn enforced_policy(&self, now_secs: i64) -> Result<RevocationPolicy<'_>, ProtocolError> {
        if self.is_stale(now_secs) {
            return Err(ProtocolError::Expired(
                "revocation list missing or stale past TTL; failing closed".into(),
            ));
        }
        let list = self.current_list.as_ref().ok_or_else(|| {
            ProtocolError::Expired("revocation list not loaded; failing closed".into())
        })?;
        Ok(RevocationPolicy::Enforced {
            list,
            min_version: self.current_version,
        })
    }

    /// Verifies a `DeviceCertificate` v2 through `waylode_protocol::crypto::verify_device_certificate`
    /// using our mandatory non-stale [`RevocationPolicy::Enforced`].
    pub fn verify_device_certificate(
        &self,
        cert: &DeviceCertificate,
        now_secs: i64,
    ) -> Result<(), ProtocolError> {
        let policy = self.enforced_policy(now_secs)?;
        verify_device_certificate(cert, &self.identity_pub_ed25519, policy, now_secs)
    }
}
