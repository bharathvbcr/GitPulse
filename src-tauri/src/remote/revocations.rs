//! IdentityKey-signed revocation list cache, short-lived `DeviceCertificate` v2
//! verification, and Keychain-backed trust anchors (Opus R3, board #15, and
//! protocol contract #18).
//!
//! # Security Invariant (Opus Board #15)
//!
//! Because agents run as the same UID as GitPulse, an agent-writable `0600`
//! file MUST NEVER be trusted as the root of trust for the Account
//! `IdentityKey` or paired mobile P-256 approval keys. All trust anchors are
//! loaded exclusively via [`TrustAnchorProvider`] (Data Protection Keychain
//! Access Group on macOS, Secret Service on Linux, or in-memory test doubles).
//!
//! # Fail-Closed Staleness (Opus R3)
//!
//! If the host has no verified [`SignedRevocationList`], or if the cached list
//! is expired or older than `stale_ttl`, [`RevocationCache::should_bypass_remote_holds`]
//! returns `true` and [`RevocationCache::verify_device_certificate`] fails closed.

use crate::remote::config::KeychainStatus;
use base64::Engine as _;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use p256::ecdsa::VerifyingKey as P256VerifyingKey;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::Duration;
use zeroize::Zeroizing;

/// Maximum lifetime of a `DeviceCertificate` (30 days, Opus R3 / P0-6).
pub const MAX_DEVICE_CERT_TTL_SECS: i64 = 30 * 24 * 3600;

/// Default staleness ceiling for the host's cached revocation list (15 minutes).
pub const DEFAULT_REVOCATION_STALE_TTL: Duration = Duration::from_secs(15 * 60);

const CLOCK_SKEW_SECS: i64 = 300;
/// Exact domain prefix from protocol contract #18.
const REVOCATION_DOMAIN: &[u8] = b"gitpulse-protocol/v1/revocation-list\0";
/// Exact domain prefix from protocol contract #18.
const DEVICE_CERT_V2_DOMAIN: &[u8] = b"gitpulse-protocol/v2/device-cert\0";

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
            TrustAnchorError::Unavailable(msg) => write!(f, "trust anchor unavailable: {msg}"),
            TrustAnchorError::InvalidKeyMaterial(msg) => write!(f, "invalid key material: {msg}"),
        }
    }
}

impl std::error::Error for TrustAnchorError {}

/// Source of pinned trust anchors and host secrets (Keychain-backed in
/// production; never an agent-writable `0600` file).
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
}

/// In-memory trust anchor provider (for unit/integration tests and in-memory
/// key handoff over authenticated control plane).
#[derive(Clone)]
pub struct InMemoryTrustAnchor {
    pub status: KeychainStatus,
    pub identity_pub_ed25519: [u8; 32],
    pub host_ed25519_seed: Zeroizing<[u8; 32]>,
    pub host_noise_static_x25519: Zeroizing<[u8; 32]>,
}

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

/// Device kind matching protocol contract #18 (`desktop` = 1, `mobile` = 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    Desktop,
    Mobile,
}

impl DeviceKind {
    fn wire_tag(self) -> u8 {
        match self {
            DeviceKind::Desktop => 1,
            DeviceKind::Mobile => 2,
        }
    }
}

/// Short-lived IdentityKey-signed `DeviceCertificate` v2 matching protocol
/// contract #18 byte-for-byte.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCertificate {
    pub version: u8,
    pub device_id: String,
    pub kind: DeviceKind,
    #[serde(default)]
    pub name: String,
    pub ed25519_pub_b64: String,
    pub x25519_pub_b64: String,
    /// Optional SEC1-encoded P-256 public key (33B compressed or 65B uncompressed)
    /// held in Secure Enclave / StrongBox; required for mobile approval signing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_pub_p256_b64: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
    pub identity_sig_b64: String,
}

impl DeviceCertificate {
    /// Exact binary preimage from protocol contract #18:
    /// `b"gitpulse-protocol/v2/device-cert\0" || version(u8=2) || u16BE+device_id ||
    /// kind(1=desktop,2=mobile) || u16BE+name || ed25519_pub(32B) || x25519_pub(32B) ||
    /// (0u8 | 1u8 || u16BE+sec1_p256_pub) || created_at(i64 BE) || expires_at(i64 BE)`
    pub fn signing_bytes(&self) -> Result<Vec<u8>, RevocationError> {
        let ed25519_pub = decode_fixed_32(&self.ed25519_pub_b64, "ed25519_pub_b64")?;
        let x25519_pub = decode_fixed_32(&self.x25519_pub_b64, "x25519_pub_b64")?;
        let p256_opt = match &self.approval_pub_p256_b64 {
            Some(b64) => Some(
                base64::engine::general_purpose::STANDARD
                    .decode(b64.trim())
                    .map_err(|e| {
                        RevocationError::InvalidCertificateField(format!(
                            "approval_pub_p256_b64: {e}"
                        ))
                    })?,
            ),
            None => None,
        };

        let mut out = Vec::with_capacity(256);
        out.extend_from_slice(DEVICE_CERT_V2_DOMAIN);
        out.push(self.version);
        put_u16_str(&mut out, &self.device_id, "device_id")?;
        out.push(self.kind.wire_tag());
        put_u16_str(&mut out, &self.name, "name")?;
        out.extend_from_slice(&ed25519_pub);
        out.extend_from_slice(&x25519_pub);
        match p256_opt {
            None => out.push(0u8),
            Some(bytes) => {
                let len = u16::try_from(bytes.len()).map_err(|_| {
                    RevocationError::InvalidCertificateField("approval_pub_p256 too long".into())
                })?;
                out.push(1u8);
                out.extend_from_slice(&len.to_be_bytes());
                out.extend_from_slice(&bytes);
            }
        }
        out.extend_from_slice(&self.created_at.to_be_bytes());
        out.extend_from_slice(&self.expires_at.to_be_bytes());
        Ok(out)
    }

    /// Issues and signs a `DeviceCertificate` v2 under the Account `IdentityKey`.
    #[allow(clippy::too_many_arguments)]
    pub fn issue(
        identity_signing_key: &SigningKey,
        device_id: impl Into<String>,
        kind: DeviceKind,
        ed25519_pub: &[u8; 32],
        x25519_pub: &[u8; 32],
        approval_pub_p256_sec1: Option<&[u8]>,
        name: impl Into<String>,
        created_at: i64,
        expires_at: i64,
    ) -> Self {
        let mut cert = Self {
            version: 2,
            device_id: device_id.into(),
            kind,
            name: name.into(),
            ed25519_pub_b64: base64::engine::general_purpose::STANDARD.encode(ed25519_pub),
            x25519_pub_b64: base64::engine::general_purpose::STANDARD.encode(x25519_pub),
            approval_pub_p256_b64: approval_pub_p256_sec1
                .map(|b| base64::engine::general_purpose::STANDARD.encode(b)),
            created_at,
            expires_at,
            identity_sig_b64: String::new(),
        };
        let preimage = cert.signing_bytes().expect("valid issued certificate fields");
        let sig = identity_signing_key.sign(&preimage);
        cert.identity_sig_b64 = base64::engine::general_purpose::STANDARD.encode(sig.to_bytes());
        cert
    }
}

/// Verified key material extracted from a non-revoked, non-expired
/// `DeviceCertificate` v2.
#[derive(Debug, Clone)]
pub struct VerifiedDevice {
    pub device_id: String,
    pub kind: DeviceKind,
    pub ed25519_pub: [u8; 32],
    pub x25519_pub: [u8; 32],
    pub approval_verifying_key: Option<P256VerifyingKey>,
    pub expires_at: i64,
}

impl VerifiedDevice {
    /// Returns the mobile hardware P-256 approval verifying key, failing closed
    /// if the certificate did not enroll one.
    pub fn require_approval_key(&self) -> Result<&P256VerifyingKey, RevocationError> {
        self.approval_verifying_key.as_ref().ok_or_else(|| {
            RevocationError::InvalidCertificateField(
                "certificate has no enrolled hardware P-256 approval key".into(),
            )
        })
    }
}

/// IdentityKey-signed, monotonically versioned revocation list matching
/// protocol contract #18 byte-for-byte.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedRevocationList {
    pub version: u64,
    pub issued_at: i64,
    pub expires_at: i64,
    pub revoked_device_ids: Vec<String>,
    pub identity_sig_b64: String,
}

impl SignedRevocationList {
    /// Exact binary preimage from protocol contract #18:
    /// `b"gitpulse-protocol/v1/revocation-list\0" || version(u64 BE) ||
    /// issued_at(i64 BE) || expires_at(i64 BE) || count(u32 BE) ||
    /// for each id in revoked_device_ids (sorted unique): len(u16 BE) || utf8(id)`
    pub fn signing_bytes(&self) -> Result<Vec<u8>, RevocationError> {
        let mut sorted = self.revoked_device_ids.clone();
        sorted.sort();
        sorted.dedup();
        let count = u32::try_from(sorted.len()).map_err(|_| {
            RevocationError::InvalidCertificateField("revoked_device_ids count overflow".into())
        })?;
        let mut out = Vec::with_capacity(REVOCATION_DOMAIN.len() + 32 + sorted.len() * 24);
        out.extend_from_slice(REVOCATION_DOMAIN);
        out.extend_from_slice(&self.version.to_be_bytes());
        out.extend_from_slice(&self.issued_at.to_be_bytes());
        out.extend_from_slice(&self.expires_at.to_be_bytes());
        out.extend_from_slice(&count.to_be_bytes());
        for id in &sorted {
            put_u16_str(&mut out, id, "revoked_device_id")?;
        }
        Ok(out)
    }

    pub fn sign(
        identity_signing_key: &SigningKey,
        version: u64,
        issued_at: i64,
        expires_at: i64,
        mut revoked_device_ids: Vec<String>,
    ) -> Self {
        revoked_device_ids.sort();
        revoked_device_ids.dedup();
        let mut list = Self {
            version,
            issued_at,
            expires_at,
            revoked_device_ids,
            identity_sig_b64: String::new(),
        };
        let preimage = list.signing_bytes().expect("valid revocation list fields");
        let sig = identity_signing_key.sign(&preimage);
        list.identity_sig_b64 = base64::engine::general_purpose::STANDARD.encode(sig.to_bytes());
        list
    }
}

/// Failure modes for revocation list updates and certificate verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevocationError {
    InvalidIdentityKey(String),
    InvalidSignature(String),
    RollbackRejected { current: u64, incoming: u64 },
    RevocationListExpired,
    RevocationListFutureDated,
    RevocationListStale,
    DeviceRevoked(String),
    CertificateExpired,
    CertificateLifetimeExceeded,
    CertificateFutureDated,
    InvalidCertificateField(String),
}

impl std::fmt::Display for RevocationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RevocationError::InvalidIdentityKey(m) => write!(f, "invalid identity key: {m}"),
            RevocationError::InvalidSignature(m) => write!(f, "invalid signature: {m}"),
            RevocationError::RollbackRejected { current, incoming } => write!(
                f,
                "revocation list rollback rejected (current version {current} > incoming {incoming})"
            ),
            RevocationError::RevocationListExpired => write!(f, "revocation list is expired"),
            RevocationError::RevocationListFutureDated => {
                write!(f, "revocation list issued_at is in the future")
            }
            RevocationError::RevocationListStale => {
                write!(f, "revocation list is missing or stale past TTL; failing closed")
            }
            RevocationError::DeviceRevoked(id) => write!(f, "device '{id}' is revoked"),
            RevocationError::CertificateExpired => write!(f, "device certificate is expired"),
            RevocationError::CertificateLifetimeExceeded => {
                write!(f, "device certificate exceeds 30-day maximum lifetime")
            }
            RevocationError::CertificateFutureDated => {
                write!(f, "device certificate created_at is in the future")
            }
            RevocationError::InvalidCertificateField(m) => {
                write!(f, "invalid device certificate field: {m}")
            }
        }
    }
}

impl std::error::Error for RevocationError {}

/// Host-side cache for the IdentityKey-signed revocation list (Opus R3).
#[derive(Debug, Clone)]
pub struct RevocationCache {
    identity_key: VerifyingKey,
    stale_ttl: Duration,
    current_version: u64,
    list_expires_at: Option<i64>,
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
        Self::new(pub_bytes, stale_ttl)
            .map_err(|e| TrustAnchorError::InvalidKeyMaterial(e.to_string()))
    }

    pub fn new(
        identity_pub_ed25519: [u8; 32],
        stale_ttl: Duration,
    ) -> Result<Self, RevocationError> {
        let identity_key = VerifyingKey::from_bytes(&identity_pub_ed25519)
            .map_err(|e| RevocationError::InvalidIdentityKey(e.to_string()))?;
        Ok(Self {
            identity_key,
            stale_ttl,
            current_version: 0,
            list_expires_at: None,
            last_refreshed_at: None,
            revoked_ids: HashSet::new(),
        })
    }

    pub fn current_version(&self) -> u64 {
        self.current_version
    }

    /// Ingests a freshly fetched [`SignedRevocationList`], enforcing strict
    /// Ed25519 verification (`verify_strict`), monotonic version progression,
    /// and validity bounds.
    pub fn update(
        &mut self,
        list: &SignedRevocationList,
        now_secs: i64,
    ) -> Result<(), RevocationError> {
        if self.last_refreshed_at.is_some() && list.version < self.current_version {
            return Err(RevocationError::RollbackRejected {
                current: self.current_version,
                incoming: list.version,
            });
        }
        if list.issued_at > now_secs + CLOCK_SKEW_SECS {
            return Err(RevocationError::RevocationListFutureDated);
        }
        if list.expires_at <= list.issued_at || now_secs >= list.expires_at {
            return Err(RevocationError::RevocationListExpired);
        }
        let preimage = list.signing_bytes()?;
        verify_ed25519_strict_b64(&self.identity_key, &preimage, &list.identity_sig_b64)?;

        self.current_version = list.version;
        self.list_expires_at = Some(list.expires_at);
        self.last_refreshed_at = Some(now_secs);
        self.revoked_ids = list.revoked_device_ids.iter().cloned().collect();
        Ok(())
    }

    /// Returns `true` when no valid list has been loaded, the list is expired,
    /// or `stale_ttl` has elapsed since the last verified refresh.
    pub fn is_stale(&self, now_secs: i64) -> bool {
        let (Some(refreshed), Some(expires_at)) = (self.last_refreshed_at, self.list_expires_at)
        else {
            return true;
        };
        if now_secs >= expires_at {
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

    /// Verifies a `DeviceCertificate` v2 against the pinned Account `IdentityKey`,
    /// the 30-day max TTL, current clock, and the non-stale revocation cache.
    pub fn verify_device_certificate(
        &self,
        cert: &DeviceCertificate,
        now_secs: i64,
    ) -> Result<VerifiedDevice, RevocationError> {
        if self.is_stale(now_secs) {
            return Err(RevocationError::RevocationListStale);
        }
        if cert.version != 2 {
            return Err(RevocationError::InvalidCertificateField(format!(
                "unsupported certificate version {}",
                cert.version
            )));
        }
        if cert.device_id.trim().is_empty() || cert.device_id.len() > 64 {
            return Err(RevocationError::InvalidCertificateField("device_id".into()));
        }
        if self.is_revoked(&cert.device_id) {
            return Err(RevocationError::DeviceRevoked(cert.device_id.clone()));
        }
        if cert.created_at > now_secs + CLOCK_SKEW_SECS {
            return Err(RevocationError::CertificateFutureDated);
        }
        if cert.expires_at <= cert.created_at || now_secs >= cert.expires_at {
            return Err(RevocationError::CertificateExpired);
        }
        if cert.expires_at - cert.created_at > MAX_DEVICE_CERT_TTL_SECS {
            return Err(RevocationError::CertificateLifetimeExceeded);
        }

        let preimage = cert.signing_bytes()?;
        verify_ed25519_strict_b64(&self.identity_key, &preimage, &cert.identity_sig_b64)?;

        let ed25519_pub = decode_fixed_32(&cert.ed25519_pub_b64, "ed25519_pub_b64")?;
        VerifyingKey::from_bytes(&ed25519_pub)
            .map_err(|e| RevocationError::InvalidCertificateField(format!("ed25519_pub: {e}")))?;

        let x25519_pub = decode_fixed_32(&cert.x25519_pub_b64, "x25519_pub_b64")?;

        let approval_verifying_key = match &cert.approval_pub_p256_b64 {
            Some(b64) => {
                let p256_bytes = base64::engine::general_purpose::STANDARD
                    .decode(b64.trim())
                    .map_err(|e| {
                        RevocationError::InvalidCertificateField(format!(
                            "approval_pub_p256_b64: {e}"
                        ))
                    })?;
                Some(P256VerifyingKey::from_sec1_bytes(&p256_bytes).map_err(|e| {
                    RevocationError::InvalidCertificateField(format!("approval_pub_p256: {e}"))
                })?)
            }
            None => None,
        };

        Ok(VerifiedDevice {
            device_id: cert.device_id.clone(),
            kind: cert.kind,
            ed25519_pub,
            x25519_pub,
            approval_verifying_key,
            expires_at: cert.expires_at,
        })
    }
}

fn verify_ed25519_strict_b64(
    key: &VerifyingKey,
    payload: &[u8],
    sig_b64: &str,
) -> Result<(), RevocationError> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(sig_b64.trim())
        .map_err(|e| RevocationError::InvalidSignature(format!("base64 decode: {e}")))?;
    let arr: [u8; 64] = raw
        .try_into()
        .map_err(|_| RevocationError::InvalidSignature("signature must be 64 bytes".into()))?;
    let sig = Signature::from_bytes(&arr);
    key.verify_strict(payload, &sig)
        .map_err(|_| RevocationError::InvalidSignature("Ed25519 strict verification failed".into()))
}

fn decode_fixed_32(b64: &str, field: &str) -> Result<[u8; 32], RevocationError> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| RevocationError::InvalidCertificateField(format!("{field}: {e}")))?;
    raw.try_into()
        .map_err(|_| RevocationError::InvalidCertificateField(format!("{field} must be 32 bytes")))
}

fn put_u16_str(out: &mut Vec<u8>, text: &str, field: &str) -> Result<(), RevocationError> {
    let len = u16::try_from(text.len())
        .map_err(|_| RevocationError::InvalidCertificateField(format!("{field} too long")))?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(text.as_bytes());
    Ok(())
}
