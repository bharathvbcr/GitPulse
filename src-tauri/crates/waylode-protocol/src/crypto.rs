//! Cryptographic primitives for `waylode-protocol`:
//! - **Noise `IK`** (`Noise_IK_25519_ChaChaPoly_SHA256`) authenticated E2EE tunnel
//!   with `DeviceCertificate` static X25519 key pinning, `host_epoch` prologue binding,
//!   empty handshake payloads (msg1 has weak forward secrecy and is replayable),
//!   per-epoch msg1 replay guard, WireGuard/IPsec-style 1024-bit sliding replay window
//!   (tolerates cross-class reorder between `pty` and `control`/`approval` while rejecting
//!   duplicates and below-window replays), and `zeroize` on drop (Opus R1 + review #20).
//! - **DeviceCertificate** (≤30-day TTL, binding Ed25519, X25519, and optional hardware P-256
//!   approval public key) and **SignedRevocationList** (monotonic version, IdentityKey Ed25519
//!   signature, Opus M2 & R3).
//! - **Mandatory [`RevocationPolicy`]** on approval verification so a missing or stale
//!   revocation list fails closed rather than silently skipping revocation checks (review #20).
//! - **P-256 ECDSA** approval signature verification (DER-encoded, hardware Secure Enclave /
//!   StrongBox compatible, Opus P0-5/R2/M2).
//!   **Caller trust boundary (Opus #15):** `identity_pub_ed25519` MUST be loaded from a
//!   tamper-resistant OS Keychain / Secret Service (never an agent-writable file).
//! - **Ed25519 relay HTTP/WS request signing & verification** matching `gitpulse-relay`
//!   `internal/server/deviceauth.go` (`METHOD\nRequestURI\nts\nhex(sha256(body))`).
//! - **`<= 2 KiB` `preview_ct` sealing/opening** (ephemeral X25519 + HKDF-SHA256 +
//!   ChaCha20-Poly1305 + host Ed25519 signature) for metadata-free APNs/FCM NSE wake pushes.

use crate::canonical::{
    approval_signing_bytes, hex_encode, steering_signing_bytes, steering_text_sha256_hex,
    validate_non_empty_id, validate_sha256_hex,
};
use crate::error::{ProtocolError, Result};
use crate::wire::{
    ApprovalDecisionKind, DeviceCertificate, DeviceKind, SignedApprovalDecision,
    SignedRevocationList, SignedSteeringConfirm, CLOCK_SKEW_SECS, MAX_CERT_TTL_SECS,
    MAX_PREVIEW_CT_BYTES,
};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use chacha20poly1305::{AeadInOut, ChaCha20Poly1305, Key, KeyInit, Nonce};
use ed25519_dalek::{
    Signature as EdSignature, Signer as EdSigner, SigningKey as EdSigningKey, Verifier,
    VerifyingKey as EdVerifyingKey,
};
use hkdf::Hkdf;
use p256::ecdsa::{DerSignature, VerifyingKey as P256VerifyingKey};
use sha2::{Digest, Sha256};
use snow::{Builder, HandshakeState, StatelessTransportState};
use std::collections::{HashSet, VecDeque};
use x25519_dalek::{x25519, X25519_BASEPOINT_BYTES};
use zeroize::Zeroizing;

/// Noise protocol suite used for all host↔mobile E2EE tunnels (Opus R1).
pub const NOISE_PARAMS: &str = "Noise_IK_25519_ChaChaPoly_SHA256";

/// Domain prefix for Noise `IK` handshake prologue binding `(stream_id, host_epoch)`.
const DOMAIN_TUNNEL_PROLOGUE: &[u8] = b"gitpulse-tunnel/v1\0";

/// Domain prefix for `DeviceCertificate` v2 IdentityKey signatures.
const DOMAIN_DEVICE_CERT_V2: &[u8] = b"gitpulse-protocol/v2/device-cert\0";

/// Domain prefix for `SignedRevocationList` IdentityKey signatures.
pub const DOMAIN_REVOCATION_LIST: &[u8] = b"gitpulse-protocol/v1/revocation-list\0";

/// Domain prefix + magic for `<= 2 KiB` NSE wake `preview_ct` blobs.
const PREVIEW_MAGIC: &[u8; 4] = b"GPPV";
const PREVIEW_VERSION: u8 = 1;
const INFO_PREVIEW_KEY: &[u8] = b"gitpulse-preview/v1/chacha20poly1305";
const DOMAIN_PREVIEW_SIG: &[u8] = b"gitpulse-preview/v1/sig\0";
const DOMAIN_PREVIEW_AAD: &[u8] = b"gitpulse-preview/v1/aad\0";

/// Current `DeviceCertificate` schema version (supports optional hardware P-256 key).
pub const DEVICE_CERT_VERSION: u8 = 2;

/// Maximum Noise transport plaintext per frame (fits inside Noise 65535B limit minus 16B tag).
pub const MAX_TUNNEL_PLAINTEXT_BYTES: usize = 65_500;

/// Maximum frames encrypted per direction under a single Noise handshake before rekey.
pub const MAX_FRAMES_PER_SESSION: u64 = 100_000;

/// WireGuard/IPsec-style sliding replay window width in frames (`16 * 64 = 1024`).
pub const REPLAY_WINDOW_SIZE: u64 = 1024;
const REPLAY_WINDOW_WORDS: usize = 16;

/// Relay HTTP/WS authentication header names (matching `gitpulse-relay`).
pub const DEVICE_HEADER: &str = "X-GitPulse-Device";
/// Relay request timestamp header (unix milliseconds).
pub const TIMESTAMP_HEADER: &str = "X-GitPulse-Timestamp";
/// Relay request Ed25519 signature header (standard base64).
pub const SIGNATURE_HEADER: &str = "X-GitPulse-Signature";

/// Explicit revocation-check policy required by certificate and approval verifiers.
///
/// Prevents callers from accidentally passing `None` and silently skipping
/// revocation checks on paths that authorize `allow_once` (Opus review #20).
#[derive(Debug, Clone, Copy)]
pub enum RevocationPolicy<'a> {
    /// Enforce a fresh Account IdentityKey-signed [`SignedRevocationList`] with
    /// strictly monotonic `version >= min_version`.
    Enforced {
        /// Signed revocation list fetched from relay / cache.
        list: &'a SignedRevocationList,
        /// Highest previously observed revocation list version.
        min_version: u64,
    },
    /// Deliberate opt-in for an account that has never yet published a revocation list.
    /// Fails closed with [`ProtocolError::RevocationRollback`] if `min_version != 0`.
    NeverIssued {
        /// Must be `0`; any non-zero value means a list was previously seen and cannot be skipped.
        min_version: u64,
    },
}

/// Relay HTTP/WS authentication headers produced by [`sign_relay_request`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayAuthHeaders {
    /// Device identifier (`X-GitPulse-Device`).
    pub device_id: String,
    /// Unix milliseconds (`X-GitPulse-Timestamp`).
    pub timestamp_ms: i64,
    /// Standard base64-encoded 64-byte Ed25519 signature (`X-GitPulse-Signature`).
    pub signature_b64: String,
}

impl RelayAuthHeaders {
    /// Returns the three HTTP header `(name, value)` pairs ready for HTTP/WS requests.
    pub fn pairs(&self) -> [(&'static str, String); 3] {
        [
            (DEVICE_HEADER, self.device_id.clone()),
            (TIMESTAMP_HEADER, self.timestamp_ms.to_string()),
            (SIGNATURE_HEADER, self.signature_b64.clone()),
        ]
    }
}

/// Computes the exact byte payload signed by Ed25519 for relay HTTP and WebSocket upgrades:
/// `METHOD\nRequestURI\nts\nhex(sha256(body))`.
pub fn relay_signing_message(
    method: &str,
    request_uri: &str,
    timestamp_ms: i64,
    body: &[u8],
) -> Result<Vec<u8>> {
    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(ProtocolError::InvalidInput(
            "HTTP method must be non-empty ASCII uppercase".to_string(),
        ));
    }
    if !request_uri.starts_with('/') || request_uri.bytes().any(|b| b <= b' ' || b == 0x7f) {
        return Err(ProtocolError::InvalidInput(
            "request_uri must start with '/' and contain no whitespace/control bytes".to_string(),
        ));
    }
    if timestamp_ms < 0 {
        return Err(ProtocolError::InvalidInput(
            "timestamp_ms must be non-negative".to_string(),
        ));
    }
    let body_hash = hex_encode(&Sha256::digest(body));
    Ok(format!("{method}\n{request_uri}\n{timestamp_ms}\n{body_hash}").into_bytes())
}

/// Signs a relay HTTP or WebSocket upgrade request with a device's 32-byte Ed25519 seed.
pub fn sign_relay_request(
    device_id: &str,
    ed25519_seed: &[u8; 32],
    method: &str,
    request_uri: &str,
    timestamp_ms: i64,
    body: &[u8],
) -> Result<RelayAuthHeaders> {
    validate_non_empty_id(device_id, "device_id", 64)?;
    let msg = relay_signing_message(method, request_uri, timestamp_ms, body)?;
    let signing = EdSigningKey::from_bytes(ed25519_seed);
    let sig = signing.sign(&msg);
    Ok(RelayAuthHeaders {
        device_id: device_id.to_string(),
        timestamp_ms,
        signature_b64: STANDARD.encode(sig.to_bytes()),
    })
}

/// Verifies a relay HTTP or WebSocket upgrade request signature against a device's Ed25519 public key.
pub fn verify_relay_request(
    device_ed25519_pub: &[u8; 32],
    method: &str,
    request_uri: &str,
    timestamp_ms: i64,
    body: &[u8],
    signature_b64: &str,
) -> Result<()> {
    let raw = STANDARD
        .decode(signature_b64)
        .map_err(|_| ProtocolError::Malformed("relay request signature base64".to_string()))?;
    let sig_bytes: [u8; 64] = raw
        .try_into()
        .map_err(|_| ProtocolError::Malformed("relay request signature length".to_string()))?;
    let vk = EdVerifyingKey::from_bytes(device_ed25519_pub)
        .map_err(|_| ProtocolError::InvalidInput("invalid Ed25519 public key".to_string()))?;
    let msg = relay_signing_message(method, request_uri, timestamp_ms, body)?;
    vk.verify_strict(&msg, &EdSignature::from_bytes(&sig_bytes))
        .map_err(|_| ProtocolError::BadSignature("relay request".to_string()))
}

// ---------------------------------------------------------------------------
// DeviceCertificate & SignedRevocationList (Opus M2, R3)
// ---------------------------------------------------------------------------

fn put_u16_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
}

fn cert_signed_bytes(cert: &DeviceCertificate) -> Result<Vec<u8>> {
    let ed_pub = decode_fixed_b64::<32>(&cert.ed25519_pub_b64, "ed25519_pub_b64")?;
    let x_pub = decode_fixed_b64::<32>(&cert.x25519_pub_b64, "x25519_pub_b64")?;
    let p256_pub = match &cert.approval_pub_p256_b64 {
        Some(b64) => {
            let bytes = STANDARD
                .decode(b64)
                .map_err(|_| ProtocolError::Malformed("approval_pub_p256_b64".to_string()))?;
            if bytes.len() != 33 && bytes.len() != 65 {
                return Err(ProtocolError::Malformed(
                    "approval_pub_p256 must be 33-byte compressed or 65-byte uncompressed SEC1"
                        .to_string(),
                ));
            }
            Some(bytes)
        }
        None => None,
    };

    let mut out = Vec::with_capacity(256);
    out.extend_from_slice(DOMAIN_DEVICE_CERT_V2);
    out.push(cert.version);
    put_u16_bytes(&mut out, cert.device_id.as_bytes());
    out.push(match cert.kind {
        DeviceKind::Desktop => 1,
        DeviceKind::Mobile => 2,
    });
    put_u16_bytes(&mut out, cert.name.as_bytes());
    out.extend_from_slice(&ed_pub);
    out.extend_from_slice(&x_pub);
    match p256_pub {
        Some(bytes) => {
            out.push(1);
            put_u16_bytes(&mut out, &bytes);
        }
        None => out.push(0),
    }
    out.extend_from_slice(&cert.created_at.to_be_bytes());
    out.extend_from_slice(&cert.expires_at.to_be_bytes());
    Ok(out)
}

/// Issues a short-lived (`<= 30 day`) [`DeviceCertificate`] signed by the Account `IdentityKey`.
#[allow(clippy::too_many_arguments)]
pub fn issue_device_certificate(
    identity_ed25519_seed: &[u8; 32],
    device_id: &str,
    kind: DeviceKind,
    name: &str,
    ed25519_pub: &[u8; 32],
    x25519_pub: &[u8; 32],
    approval_pub_p256_sec1: Option<&[u8]>,
    created_at: i64,
    expires_at: i64,
) -> Result<DeviceCertificate> {
    validate_non_empty_id(device_id, "device_id", 64)?;
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err(ProtocolError::InvalidInput(
            "device name must be 1..=128 printable characters".to_string(),
        ));
    }
    if expires_at <= created_at {
        return Err(ProtocolError::InvalidInput(
            "certificate expires_at must be > created_at".to_string(),
        ));
    }
    if expires_at.saturating_sub(created_at) > MAX_CERT_TTL_SECS {
        return Err(ProtocolError::CertTtlExceeded);
    }
    EdVerifyingKey::from_bytes(ed25519_pub).map_err(|_| {
        ProtocolError::InvalidInput("invalid device Ed25519 public key".to_string())
    })?;
    if x25519_pub.iter().all(|&b| b == 0) {
        return Err(ProtocolError::InvalidInput(
            "all-zero X25519 public key".to_string(),
        ));
    }
    let approval_pub_p256_b64 = match approval_pub_p256_sec1 {
        Some(sec1) => {
            P256VerifyingKey::from_sec1_bytes(sec1).map_err(|_| {
                ProtocolError::InvalidInput("invalid SEC1 P-256 public key".to_string())
            })?;
            Some(STANDARD.encode(sec1))
        }
        None => None,
    };

    let mut cert = DeviceCertificate {
        version: DEVICE_CERT_VERSION,
        device_id: device_id.to_string(),
        kind,
        name: name.to_string(),
        ed25519_pub_b64: STANDARD.encode(ed25519_pub),
        x25519_pub_b64: STANDARD.encode(x25519_pub),
        approval_pub_p256_b64,
        created_at,
        expires_at,
        identity_sig_b64: String::new(),
    };
    let signing = EdSigningKey::from_bytes(identity_ed25519_seed);
    let msg = cert_signed_bytes(&cert)?;
    cert.identity_sig_b64 = STANDARD.encode(signing.sign(&msg).to_bytes());
    Ok(cert)
}

/// Verifies a [`DeviceCertificate`] against a Keychain-pinned Account `IdentityKey` Ed25519
/// public key, current unix timestamp `now_sec`, and an explicit [`RevocationPolicy`].
///
/// Checks signature first (so forged certificates always report `BadSignature`), then
/// enforces `<= 30 day` TTL, revocation policy, expiry, and clock skew.
pub fn verify_device_certificate(
    cert: &DeviceCertificate,
    identity_pub_ed25519: &[u8; 32],
    revocation_policy: RevocationPolicy<'_>,
    now_sec: i64,
) -> Result<()> {
    if cert.version != DEVICE_CERT_VERSION {
        return Err(ProtocolError::Malformed(format!(
            "unsupported DeviceCertificate version {}",
            cert.version
        )));
    }
    validate_non_empty_id(&cert.device_id, "device_id", 64)?;
    let identity_vk = EdVerifyingKey::from_bytes(identity_pub_ed25519)
        .map_err(|_| ProtocolError::InvalidInput("invalid identity Ed25519 key".to_string()))?;
    let sig_bytes = decode_fixed_b64::<64>(&cert.identity_sig_b64, "identity_sig_b64")?;
    let msg = cert_signed_bytes(cert)?;
    identity_vk
        .verify_strict(&msg, &EdSignature::from_bytes(&sig_bytes))
        .map_err(|_| ProtocolError::BadSignature("device certificate".to_string()))?;

    if cert.expires_at <= cert.created_at
        || cert.expires_at.saturating_sub(cert.created_at) > MAX_CERT_TTL_SECS
    {
        return Err(ProtocolError::CertTtlExceeded);
    }

    match revocation_policy {
        RevocationPolicy::Enforced { list, min_version } => {
            verify_revocation_list(list, identity_pub_ed25519, min_version, now_sec)?;
            if list
                .revoked_device_ids
                .iter()
                .any(|id| id == &cert.device_id)
            {
                return Err(ProtocolError::Revoked(cert.device_id.clone()));
            }
        }
        RevocationPolicy::NeverIssued { min_version } => {
            if min_version != 0 {
                return Err(ProtocolError::RevocationRollback {
                    got: 0,
                    min: min_version,
                });
            }
        }
    }

    if now_sec >= cert.expires_at {
        return Err(ProtocolError::Expired("device certificate".to_string()));
    }
    if cert.created_at > now_sec.saturating_add(CLOCK_SKEW_SECS) {
        return Err(ProtocolError::NotYetValid("device certificate".to_string()));
    }
    Ok(())
}

/// Computes the exact byte preimage signed by the Account `IdentityKey` Ed25519 key
/// for a [`SignedRevocationList`].
///
/// Format:
/// `b"gitpulse-protocol/v1/revocation-list\0" || version(u64 BE) || issued_at(i64 BE) || expires_at(i64 BE) || count(u32 BE) || (len(u16 BE) || utf8(device_id))*`
pub fn revocation_signing_bytes(list: &SignedRevocationList) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(128 + list.revoked_device_ids.len() * 32);
    out.extend_from_slice(DOMAIN_REVOCATION_LIST);
    out.extend_from_slice(&list.version.to_be_bytes());
    out.extend_from_slice(&list.issued_at.to_be_bytes());
    out.extend_from_slice(&list.expires_at.to_be_bytes());
    let count = u32::try_from(list.revoked_device_ids.len())
        .map_err(|_| ProtocolError::TooLarge("revocation list count".to_string()))?;
    out.extend_from_slice(&count.to_be_bytes());
    for id in &list.revoked_device_ids {
        validate_non_empty_id(id, "revoked_device_id", 64)?;
        put_u16_bytes(&mut out, id.as_bytes());
    }
    Ok(out)
}

/// Issues a monotonic Account IdentityKey-signed [`SignedRevocationList`] (Opus R3).
pub fn issue_revocation_list(
    identity_ed25519_seed: &[u8; 32],
    version: u64,
    issued_at: i64,
    expires_at: i64,
    mut revoked_device_ids: Vec<String>,
) -> Result<SignedRevocationList> {
    if expires_at <= issued_at {
        return Err(ProtocolError::InvalidInput(
            "revocation list expires_at must be > issued_at".to_string(),
        ));
    }
    revoked_device_ids.sort();
    revoked_device_ids.dedup();
    for id in &revoked_device_ids {
        validate_non_empty_id(id, "revoked_device_id", 64)?;
    }

    let mut list = SignedRevocationList {
        version,
        issued_at,
        expires_at,
        revoked_device_ids,
        identity_sig_b64: String::new(),
    };
    let msg = revocation_signing_bytes(&list)?;
    let signing = EdSigningKey::from_bytes(identity_ed25519_seed);
    list.identity_sig_b64 = STANDARD.encode(signing.sign(&msg).to_bytes());
    Ok(list)
}

/// Verifies a [`SignedRevocationList`] against the Keychain-pinned Account `IdentityKey`,
/// enforcing monotonic `version >= min_version` and non-expiry at `now_sec` (Opus R3).
pub fn verify_revocation_list(
    list: &SignedRevocationList,
    identity_pub_ed25519: &[u8; 32],
    min_version: u64,
    now_sec: i64,
) -> Result<()> {
    let identity_vk = EdVerifyingKey::from_bytes(identity_pub_ed25519)
        .map_err(|_| ProtocolError::InvalidInput("invalid identity Ed25519 key".to_string()))?;
    let sig_bytes = decode_fixed_b64::<64>(&list.identity_sig_b64, "identity_sig_b64")?;
    let msg = revocation_signing_bytes(list)?;
    identity_vk
        .verify_strict(&msg, &EdSignature::from_bytes(&sig_bytes))
        .map_err(|_| ProtocolError::BadSignature("revocation list".to_string()))?;

    if list.version < min_version {
        return Err(ProtocolError::RevocationRollback {
            got: list.version,
            min: min_version,
        });
    }
    if list.expires_at <= list.issued_at || now_sec >= list.expires_at {
        return Err(ProtocolError::Expired("revocation list".to_string()));
    }
    if list.issued_at > now_sec.saturating_add(CLOCK_SKEW_SECS) {
        return Err(ProtocolError::NotYetValid("revocation list".to_string()));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// P-256 Hardware Approval Decision Verification (Opus P0-4, P0-5, R2, M2, #15)
// ---------------------------------------------------------------------------

/// Verifies a [`SignedApprovalDecision`] end-to-end:
/// 1. Verifies `signer_cert` chains to the Keychain-pinned `identity_pub_ed25519`, is
///    unexpired, within `<= 30 day` TTL, and satisfies `revocation_policy` (mandatory —
///    no silent `Option::None` bypass).
/// 2. Checks `signer_cert.device_id == decision.device_id` and extracts `approval_pub_p256`.
/// 3. Verifies exact binding to `(expected_host_id, expected_ticket_id, expected_input_sha256)`.
/// 4. Enforces `decision` time validity (`issued_at <= now + skew`, `now < expires_at`).
/// 5. Verifies the ASN.1 DER ECDSA-P256-SHA256 signature over [`approval_signing_bytes`].
#[allow(clippy::too_many_arguments)]
pub fn verify_approval_decision(
    decision: &SignedApprovalDecision,
    signer_cert: &DeviceCertificate,
    identity_pub_ed25519: &[u8; 32],
    expected_host_id: &str,
    expected_ticket_id: &str,
    expected_input_sha256: &str,
    revocation_policy: RevocationPolicy<'_>,
    now_sec: i64,
) -> Result<ApprovalDecisionKind> {
    verify_device_certificate(
        signer_cert,
        identity_pub_ed25519,
        revocation_policy,
        now_sec,
    )?;

    if signer_cert.device_id != decision.device_id {
        return Err(ProtocolError::BindingMismatch(
            "decision.device_id does not match DeviceCertificate.device_id".to_string(),
        ));
    }
    if decision.host_id != expected_host_id {
        return Err(ProtocolError::BindingMismatch(
            "decision.host_id mismatch".to_string(),
        ));
    }
    if decision.ticket_id != expected_ticket_id {
        return Err(ProtocolError::BindingMismatch(
            "decision.ticket_id mismatch".to_string(),
        ));
    }
    validate_sha256_hex(expected_input_sha256)?;
    if decision.input_sha256 != expected_input_sha256 {
        return Err(ProtocolError::BindingMismatch(
            "decision.input_sha256 mismatch".to_string(),
        ));
    }
    if decision.expires_at <= decision.issued_at || now_sec >= decision.expires_at {
        return Err(ProtocolError::Expired("approval decision".to_string()));
    }
    if decision.issued_at > now_sec.saturating_add(CLOCK_SKEW_SECS) {
        return Err(ProtocolError::NotYetValid("approval decision".to_string()));
    }

    let p256_b64 = signer_cert
        .approval_pub_p256_b64
        .as_deref()
        .ok_or_else(|| {
            ProtocolError::BindingMismatch(
                "signer DeviceCertificate has no enrolled P-256 approval key".to_string(),
            )
        })?;
    let p256_sec1 = STANDARD
        .decode(p256_b64)
        .map_err(|_| ProtocolError::Malformed("approval_pub_p256_b64".to_string()))?;
    let p256_vk = P256VerifyingKey::from_sec1_bytes(&p256_sec1)
        .map_err(|_| ProtocolError::Malformed("invalid SEC1 P-256 public key".to_string()))?;

    let sig_der_bytes = STANDARD
        .decode(&decision.sig_p256_der_b64)
        .map_err(|_| ProtocolError::Malformed("sig_p256_der_b64".to_string()))?;
    let der_sig = DerSignature::from_bytes(&sig_der_bytes)
        .map_err(|_| ProtocolError::Malformed("invalid ASN.1 DER P-256 signature".to_string()))?;

    let signing_bytes = approval_signing_bytes(
        &decision.host_id,
        &decision.ticket_id,
        &decision.input_sha256,
        decision.decision,
        &decision.nonce,
        decision.issued_at,
        decision.expires_at,
    )?;

    p256_vk
        .verify(&signing_bytes, &der_sig)
        .map_err(|_| ProtocolError::BadSignature("P-256 approval decision".to_string()))?;

    Ok(decision.decision)
}

/// Software P-256 signing helper for deterministic tests and non-hardware test harnesses.
///
/// Production mobile apps MUST sign inside Secure Enclave / StrongBox using
/// [`approval_signing_bytes`] and never export raw P-256 private keys (Opus M2).
#[allow(clippy::too_many_arguments)]
pub fn sign_approval_decision_software(
    p256_secret_scalar: &[u8; 32],
    device_id: &str,
    host_id: &str,
    ticket_id: &str,
    input_sha256: &str,
    decision: ApprovalDecisionKind,
    nonce: &str,
    issued_at: i64,
    expires_at: i64,
) -> Result<SignedApprovalDecision> {
    use p256::ecdsa::signature::Signer as P256Signer;
    use p256::ecdsa::SigningKey as P256SigningKey;

    validate_non_empty_id(device_id, "device_id", 64)?;
    let signing_bytes = approval_signing_bytes(
        host_id,
        ticket_id,
        input_sha256,
        decision,
        nonce,
        issued_at,
        expires_at,
    )?;
    let sk = P256SigningKey::from_bytes(p256_secret_scalar.into())
        .map_err(|_| ProtocolError::InvalidInput("invalid P-256 secret scalar".to_string()))?;
    let sig: DerSignature = sk.sign(&signing_bytes);
    Ok(SignedApprovalDecision {
        host_id: host_id.to_string(),
        ticket_id: ticket_id.to_string(),
        input_sha256: input_sha256.to_string(),
        decision,
        nonce: nonce.to_string(),
        issued_at,
        expires_at,
        device_id: device_id.to_string(),
        sig_p256_der_b64: STANDARD.encode(sig.as_bytes()),
    })
}

// ---------------------------------------------------------------------------
// P-256 Hardware Steering Confirm Verification (Opus review #23)
// ---------------------------------------------------------------------------

/// Verifies a [`SignedSteeringConfirm`] end-to-end before a host deposits remote
/// steering text with `HumanConfirmed` provenance:
/// 1. Verifies `signer_cert` chains to the Keychain-pinned `identity_pub_ed25519`,
///    is unexpired, within `<= 30 day` TTL, and satisfies `revocation_policy` (mandatory).
/// 2. Verifies `signer_cert.device_id == signed.device_id` and extracts `approval_pub_p256`.
/// 3. Verifies exact binding to `(expected_host_id, expected_session_key,
///    expected_request_id, expected_route)` and `signed.text_sha256 == hex(sha256(text))`.
/// 4. Enforces `signed` time validity (`issued_at <= now + skew`, `now < expires_at`).
/// 5. Verifies the ASN.1 DER ECDSA-P256-SHA256 signature over [`steering_signing_bytes`].
#[allow(clippy::too_many_arguments)]
pub fn verify_steering_confirm(
    signed: &SignedSteeringConfirm,
    text: &str,
    signer_cert: &DeviceCertificate,
    identity_pub_ed25519: &[u8; 32],
    expected_host_id: &str,
    expected_session_key: &str,
    expected_request_id: &str,
    expected_route: &str,
    revocation_policy: RevocationPolicy<'_>,
    now_sec: i64,
) -> Result<()> {
    verify_device_certificate(
        signer_cert,
        identity_pub_ed25519,
        revocation_policy,
        now_sec,
    )?;

    if signer_cert.device_id != signed.device_id {
        return Err(ProtocolError::BindingMismatch(
            "steering.device_id does not match DeviceCertificate.device_id".to_string(),
        ));
    }
    if signed.host_id != expected_host_id {
        return Err(ProtocolError::BindingMismatch(
            "steering.host_id mismatch".to_string(),
        ));
    }
    if signed.session_key != expected_session_key {
        return Err(ProtocolError::BindingMismatch(
            "steering.session_key mismatch".to_string(),
        ));
    }
    if signed.request_id != expected_request_id {
        return Err(ProtocolError::BindingMismatch(
            "steering.request_id mismatch".to_string(),
        ));
    }
    if signed.expect_route != expected_route {
        return Err(ProtocolError::BindingMismatch(
            "steering.expect_route mismatch".to_string(),
        ));
    }

    let expected_text_sha256 = steering_text_sha256_hex(text);
    if signed.text_sha256 != expected_text_sha256 {
        return Err(ProtocolError::BindingMismatch(
            "steering.text_sha256 does not match text".to_string(),
        ));
    }
    if signed.expires_at <= signed.issued_at || now_sec >= signed.expires_at {
        return Err(ProtocolError::Expired("steering confirm".to_string()));
    }
    if signed.issued_at > now_sec.saturating_add(CLOCK_SKEW_SECS) {
        return Err(ProtocolError::NotYetValid("steering confirm".to_string()));
    }

    let p256_b64 = signer_cert
        .approval_pub_p256_b64
        .as_deref()
        .ok_or_else(|| {
            ProtocolError::BindingMismatch(
                "signer DeviceCertificate has no enrolled P-256 approval key".to_string(),
            )
        })?;
    let p256_sec1 = STANDARD
        .decode(p256_b64)
        .map_err(|_| ProtocolError::Malformed("approval_pub_p256_b64".to_string()))?;
    let p256_vk = P256VerifyingKey::from_sec1_bytes(&p256_sec1)
        .map_err(|_| ProtocolError::Malformed("invalid SEC1 P-256 public key".to_string()))?;

    let sig_der_bytes = STANDARD
        .decode(&signed.sig_p256_der_b64)
        .map_err(|_| ProtocolError::Malformed("sig_p256_der_b64".to_string()))?;
    let der_sig = DerSignature::from_bytes(&sig_der_bytes)
        .map_err(|_| ProtocolError::Malformed("invalid ASN.1 DER P-256 signature".to_string()))?;

    let signing_bytes = steering_signing_bytes(
        &signed.host_id,
        &signed.session_key,
        &signed.request_id,
        &signed.expect_route,
        &signed.text_sha256,
        &signed.nonce,
        signed.issued_at,
        signed.expires_at,
    )?;

    p256_vk
        .verify(&signing_bytes, &der_sig)
        .map_err(|_| ProtocolError::BadSignature("P-256 steering confirm".to_string()))?;

    Ok(())
}

/// Software P-256 signing helper for deterministic steering KAT tests.
///
/// Production mobile apps MUST sign inside Secure Enclave / StrongBox from a native
/// sheet that renders the exact UTF-8 steering text (Opus review #23).
#[allow(clippy::too_many_arguments)]
pub fn sign_steering_confirm_software(
    p256_secret_scalar: &[u8; 32],
    device_id: &str,
    host_id: &str,
    session_key: &str,
    request_id: &str,
    expect_route: &str,
    text: &str,
    nonce: &str,
    issued_at: i64,
    expires_at: i64,
) -> Result<SignedSteeringConfirm> {
    use p256::ecdsa::signature::Signer as P256Signer;
    use p256::ecdsa::SigningKey as P256SigningKey;

    validate_non_empty_id(device_id, "device_id", 64)?;
    let text_sha256 = steering_text_sha256_hex(text);
    let signing_bytes = steering_signing_bytes(
        host_id,
        session_key,
        request_id,
        expect_route,
        &text_sha256,
        nonce,
        issued_at,
        expires_at,
    )?;
    let sk = P256SigningKey::from_bytes(p256_secret_scalar.into())
        .map_err(|_| ProtocolError::InvalidInput("invalid P-256 secret scalar".to_string()))?;
    let sig: DerSignature = sk.sign(&signing_bytes);
    Ok(SignedSteeringConfirm {
        host_id: host_id.to_string(),
        session_key: session_key.to_string(),
        request_id: request_id.to_string(),
        expect_route: expect_route.to_string(),
        text_sha256,
        issued_at,
        expires_at,
        nonce: nonce.to_string(),
        device_id: device_id.to_string(),
        sig_p256_der_b64: STANDARD.encode(sig.as_bytes()),
    })
}

// ---------------------------------------------------------------------------
// <= 2 KiB NSE Wake Push `preview_ct` Sealing & Opening (Opus R8)
// ---------------------------------------------------------------------------

/// Wire format for `preview_ct` (total size strictly `<= 2048` bytes):
/// ```text
/// "GPPV" (4B) | version=1 (1B) | sender_id_len (1B) | sender_device_id (<=64B)
/// | ephemeral_x25519_pub (32B) | ct_len (u16 BE) | ciphertext_with_poly1305_tag
/// | ed25519_sig (64B over DOMAIN_PREVIEW_SIG || header_and_ciphertext)
/// ```
///
/// Security note (Opus R8): Sealed to the mobile device's static X25519 key (no forward
/// secrecy for lock-screen preview snippets). Callers MUST never include full commands
/// and MUST rotate X25519 keys with each `<= 30 day` `DeviceCertificate` renewal.
pub fn seal_preview(
    sender_device_id: &str,
    sender_ed25519_seed: &[u8; 32],
    recipient_x25519_pub: &[u8; 32],
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let mut ephemeral_secret = Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut ephemeral_secret[..]).map_err(|e| ProtocolError::Rng(e.to_string()))?;
    seal_preview_with_ephemeral(
        sender_device_id,
        sender_ed25519_seed,
        recipient_x25519_pub,
        &ephemeral_secret,
        plaintext,
    )
}

/// Deterministic variant of [`seal_preview`] accepting an explicit ephemeral X25519 secret
/// for Known-Answer Tests.
pub fn seal_preview_with_ephemeral(
    sender_device_id: &str,
    sender_ed25519_seed: &[u8; 32],
    recipient_x25519_pub: &[u8; 32],
    ephemeral_x25519_secret: &[u8; 32],
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    validate_non_empty_id(sender_device_id, "sender_device_id", 64)?;
    let header_and_overhead = 4 + 1 + 1 + sender_device_id.len() + 32 + 2 + 16 + 64;
    if plaintext.len() + header_and_overhead > MAX_PREVIEW_CT_BYTES {
        return Err(ProtocolError::TooLarge(format!(
            "preview_ct would exceed {MAX_PREVIEW_CT_BYTES} bytes"
        )));
    }

    let ephemeral_pub = x25519(*ephemeral_x25519_secret, X25519_BASEPOINT_BYTES);
    let shared = Zeroizing::new(x25519(*ephemeral_x25519_secret, *recipient_x25519_pub));
    if shared.iter().all(|&b| b == 0) {
        return Err(ProtocolError::InvalidInput(
            "degenerate X25519 shared secret".to_string(),
        ));
    }

    let (key, nonce) = derive_preview_key_nonce(&shared, &ephemeral_pub, recipient_x25519_pub)?;
    let aad = preview_aad(sender_device_id, &ephemeral_pub, recipient_x25519_pub);

    let mut ct = Zeroizing::new(Vec::with_capacity(plaintext.len() + 16));
    ct.extend_from_slice(plaintext);
    let cipher = ChaCha20Poly1305::new(&Key::from(*key));
    cipher
        .encrypt_in_place(&Nonce::from(nonce), &aad, &mut *ct)
        .map_err(|_| ProtocolError::Aead("preview encryption failed".to_string()))?;

    let ct_len = u16::try_from(ct.len())
        .map_err(|_| ProtocolError::TooLarge("preview ciphertext length".to_string()))?;

    let mut out = Vec::with_capacity(header_and_overhead + plaintext.len());
    out.extend_from_slice(PREVIEW_MAGIC);
    out.push(PREVIEW_VERSION);
    out.push(u8::try_from(sender_device_id.len()).unwrap_or(64));
    out.extend_from_slice(sender_device_id.as_bytes());
    out.extend_from_slice(&ephemeral_pub);
    out.extend_from_slice(&ct_len.to_be_bytes());
    out.extend_from_slice(&ct);

    let mut sig_input = Vec::with_capacity(DOMAIN_PREVIEW_SIG.len() + out.len());
    sig_input.extend_from_slice(DOMAIN_PREVIEW_SIG);
    sig_input.extend_from_slice(&out);
    let signing = EdSigningKey::from_bytes(sender_ed25519_seed);
    let sig = signing.sign(&sig_input);
    out.extend_from_slice(&sig.to_bytes());

    if out.len() > MAX_PREVIEW_CT_BYTES {
        return Err(ProtocolError::TooLarge(format!(
            "preview_ct exceeded {MAX_PREVIEW_CT_BYTES} bytes"
        )));
    }
    Ok(out)
}

/// Opens and authenticates a `<= 2 KiB` `preview_ct` blob inside the mobile NSE / FCM service.
pub fn open_preview(
    sealed_bytes: &[u8],
    recipient_x25519_secret: &[u8; 32],
    sender_ed25519_pub: &[u8; 32],
    expected_sender_device_id: &str,
) -> Result<Zeroizing<Vec<u8>>> {
    if sealed_bytes.len() > MAX_PREVIEW_CT_BYTES {
        return Err(ProtocolError::TooLarge(format!(
            "preview_ct exceeds {MAX_PREVIEW_CT_BYTES} bytes"
        )));
    }
    if sealed_bytes.len() < 4 + 1 + 1 + 1 + 32 + 2 + 16 + 64 {
        return Err(ProtocolError::Malformed("preview_ct truncated".to_string()));
    }
    if &sealed_bytes[0..4] != PREVIEW_MAGIC {
        return Err(ProtocolError::Malformed("preview_ct magic".to_string()));
    }
    if sealed_bytes[4] != PREVIEW_VERSION {
        return Err(ProtocolError::Malformed("preview_ct version".to_string()));
    }

    let signed_portion_len = sealed_bytes.len() - 64;
    let sig_bytes: [u8; 64] = sealed_bytes[signed_portion_len..]
        .try_into()
        .map_err(|_| ProtocolError::Malformed("preview_ct signature".to_string()))?;

    let sender_vk = EdVerifyingKey::from_bytes(sender_ed25519_pub)
        .map_err(|_| ProtocolError::InvalidInput("invalid sender Ed25519 key".to_string()))?;
    let mut sig_input = Vec::with_capacity(DOMAIN_PREVIEW_SIG.len() + signed_portion_len);
    sig_input.extend_from_slice(DOMAIN_PREVIEW_SIG);
    sig_input.extend_from_slice(&sealed_bytes[..signed_portion_len]);
    sender_vk
        .verify_strict(&sig_input, &EdSignature::from_bytes(&sig_bytes))
        .map_err(|_| ProtocolError::BadSignature("preview_ct".to_string()))?;

    let id_len = usize::from(sealed_bytes[5]);
    if id_len == 0 || id_len > 64 || 6 + id_len + 32 + 2 > signed_portion_len {
        return Err(ProtocolError::Malformed(
            "preview_ct sender_device_id length".to_string(),
        ));
    }
    let sender_id = std::str::from_utf8(&sealed_bytes[6..6 + id_len])
        .map_err(|_| ProtocolError::Malformed("preview_ct sender_device_id utf8".to_string()))?;
    if sender_id != expected_sender_device_id {
        return Err(ProtocolError::BindingMismatch(
            "preview_ct sender_device_id mismatch".to_string(),
        ));
    }

    let eph_start = 6 + id_len;
    let ephemeral_pub: [u8; 32] = sealed_bytes[eph_start..eph_start + 32]
        .try_into()
        .map_err(|_| ProtocolError::Malformed("preview_ct ephemeral key".to_string()))?;
    let ct_len_start = eph_start + 32;
    let ct_len = usize::from(u16::from_be_bytes([
        sealed_bytes[ct_len_start],
        sealed_bytes[ct_len_start + 1],
    ]));
    let ct_start = ct_len_start + 2;
    if ct_len < 16 || ct_start + ct_len != signed_portion_len {
        return Err(ProtocolError::Malformed(
            "preview_ct ciphertext length".to_string(),
        ));
    }

    let recipient_pub = x25519(*recipient_x25519_secret, X25519_BASEPOINT_BYTES);
    let shared = Zeroizing::new(x25519(*recipient_x25519_secret, ephemeral_pub));
    if shared.iter().all(|&b| b == 0) {
        return Err(ProtocolError::Aead(
            "degenerate X25519 shared secret".to_string(),
        ));
    }

    let (key, nonce) = derive_preview_key_nonce(&shared, &ephemeral_pub, &recipient_pub)?;
    let aad = preview_aad(sender_id, &ephemeral_pub, &recipient_pub);
    let mut buf = Zeroizing::new(sealed_bytes[ct_start..ct_start + ct_len].to_vec());
    let cipher = ChaCha20Poly1305::new(&Key::from(*key));
    cipher
        .decrypt_in_place(&Nonce::from(nonce), &aad, &mut *buf)
        .map_err(|_| ProtocolError::Aead("preview decryption failed".to_string()))?;
    Ok(buf)
}

fn derive_preview_key_nonce(
    shared: &[u8; 32],
    ephemeral_pub: &[u8; 32],
    recipient_pub: &[u8; 32],
) -> Result<(Zeroizing<[u8; 32]>, [u8; 12])> {
    let mut salt = [0u8; 64];
    salt[..32].copy_from_slice(ephemeral_pub);
    salt[32..].copy_from_slice(recipient_pub);
    let hk = Hkdf::<Sha256>::new(Some(&salt), shared);
    let mut okm = Zeroizing::new([0u8; 44]);
    hk.expand(INFO_PREVIEW_KEY, &mut okm[..])
        .map_err(|_| ProtocolError::InvalidInput("hkdf expand length".to_string()))?;
    let mut key = Zeroizing::new([0u8; 32]);
    key.copy_from_slice(&okm[..32]);
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&okm[32..]);
    Ok((key, nonce))
}

fn preview_aad(
    sender_device_id: &str,
    ephemeral_pub: &[u8; 32],
    recipient_pub: &[u8; 32],
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(DOMAIN_PREVIEW_AAD.len() + 1 + sender_device_id.len() + 64);
    aad.extend_from_slice(DOMAIN_PREVIEW_AAD);
    aad.push(u8::try_from(sender_device_id.len()).unwrap_or(64));
    aad.extend_from_slice(sender_device_id.as_bytes());
    aad.extend_from_slice(ephemeral_pub);
    aad.extend_from_slice(recipient_pub);
    aad
}

// ---------------------------------------------------------------------------
// Noise `IK` Authenticated E2EE Tunnel + Sliding Replay Window (Opus R1, #20)
// ---------------------------------------------------------------------------

/// Cheap per-epoch responder replay guard for Noise `IK` message 1.
///
/// Noise `IK` msg1 has weak forward secrecy and can be replayed by a malicious relay
/// within the same `host_epoch`. Responders check incoming msg1 hashes through this
/// bounded guard before spending X25519 DH / responder handshake work.
pub struct EpochHandshakeReplayGuard {
    host_epoch: u64,
    capacity: usize,
    seen: HashSet<[u8; 32]>,
    order: VecDeque<[u8; 32]>,
}

impl EpochHandshakeReplayGuard {
    /// Creates a bounded msg1 replay guard for `host_epoch` (default capacity: `1024`).
    pub fn new(host_epoch: u64, capacity: usize) -> Self {
        let cap = capacity.max(1);
        Self {
            host_epoch,
            capacity: cap,
            seen: HashSet::with_capacity(cap.min(1024)),
            order: VecDeque::with_capacity(cap.min(1024)),
        }
    }

    /// Epoch bound to this guard.
    pub fn host_epoch(&self) -> u64 {
        self.host_epoch
    }

    /// Rejects duplicate msg1 bytes within the same `host_epoch` with
    /// [`ProtocolError::ReplayDetected`] before running DH; records fresh hashes
    /// with bounded FIFO eviction.
    pub fn check_and_record(&mut self, host_epoch: u64, msg1: &[u8]) -> Result<()> {
        if host_epoch != self.host_epoch {
            return Err(ProtocolError::InvalidInput(format!(
                "handshake replay guard epoch mismatch: expected {}, got {host_epoch}",
                self.host_epoch
            )));
        }
        let digest: [u8; 32] = Sha256::digest(msg1).into();
        if self.seen.contains(&digest) {
            return Err(ProtocolError::ReplayDetected {
                got: 0,
                high_water: 0,
            });
        }
        if self.order.len() >= self.capacity {
            if let Some(evicted) = self.order.pop_front() {
                self.seen.remove(&evicted);
            }
        }
        self.seen.insert(digest);
        self.order.push_back(digest);
        Ok(())
    }
}

/// WireGuard / RFC 6479-style 1024-bit sliding replay window.
///
/// Bit `delta` (`0 <= delta < 1024`) records whether `high_water - delta` has already
/// been authenticated. Advances ONLY via [`Self::mark_accepted`] after AEAD passes.
#[derive(Debug, Clone)]
struct ReplayWindow {
    high_water: u64,
    bitmap: [u64; REPLAY_WINDOW_WORDS],
}

impl ReplayWindow {
    const fn new() -> Self {
        Self {
            high_water: 0,
            bitmap: [0u64; REPLAY_WINDOW_WORDS],
        }
    }

    fn check(&self, seq: u64) -> Result<()> {
        if seq == 0 {
            return Err(ProtocolError::ReplayDetected {
                got: 0,
                high_water: self.high_water,
            });
        }
        if seq > self.high_water {
            return Ok(());
        }
        let delta = self.high_water - seq;
        if delta >= REPLAY_WINDOW_SIZE {
            return Err(ProtocolError::ReplayDetected {
                got: seq,
                high_water: self.high_water,
            });
        }
        let word = (delta / 64) as usize;
        let bit = delta % 64;
        if (self.bitmap[word] & (1u64 << bit)) != 0 {
            return Err(ProtocolError::ReplayDetected {
                got: seq,
                high_water: self.high_water,
            });
        }
        Ok(())
    }

    fn mark_accepted(&mut self, seq: u64) {
        if seq > self.high_water {
            let shift = seq - self.high_water;
            if shift >= REPLAY_WINDOW_SIZE {
                self.bitmap = [0u64; REPLAY_WINDOW_WORDS];
            } else {
                let word_shift = (shift / 64) as usize;
                let bit_shift = (shift % 64) as u32;
                let mut next = [0u64; REPLAY_WINDOW_WORDS];
                for i in (word_shift..REPLAY_WINDOW_WORDS).rev() {
                    let src = i - word_shift;
                    let mut val = self.bitmap[src] << bit_shift;
                    if bit_shift > 0 && src > 0 {
                        val |= self.bitmap[src - 1] >> (64 - bit_shift);
                    }
                    next[i] = val;
                }
                self.bitmap = next;
            }
            self.bitmap[0] |= 1u64;
            self.high_water = seq;
        } else {
            let delta = self.high_water - seq;
            if delta < REPLAY_WINDOW_SIZE {
                let word = (delta / 64) as usize;
                let bit = delta % 64;
                self.bitmap[word] |= 1u64 << bit;
            }
        }
    }
}

fn tunnel_prologue(stream_id: &str, host_epoch: u64) -> Result<Vec<u8>> {
    validate_non_empty_id(stream_id, "stream_id", 128)?;
    let mut out = Vec::with_capacity(DOMAIN_TUNNEL_PROLOGUE.len() + stream_id.len() + 1 + 8);
    out.extend_from_slice(DOMAIN_TUNNEL_PROLOGUE);
    out.extend_from_slice(stream_id.as_bytes());
    out.push(0);
    out.extend_from_slice(&host_epoch.to_be_bytes());
    Ok(out)
}

/// Noise `IK` handshake state for either initiator (mobile client) or responder (host daemon).
///
/// Security invariants:
/// - Pins the peer's static X25519 key from a verified [`DeviceCertificate`].
/// - Binds `(stream_id, host_epoch)` in the Noise prologue so each `host_epoch` requires a
///   fresh authenticated handshake (Opus R1).
/// - Enforces **empty handshake payloads** (`write_message(&[])`) because Noise `IK` msg1
///   provides weak forward secrecy and is replayable; application data/secrets travel ONLY
///   after [`Self::into_transport`] (Opus review #20).
pub struct NoiseHandshake {
    state: HandshakeState,
    expected_remote_static: Zeroizing<[u8; 32]>,
    stream_id: String,
    host_epoch: u64,
    is_initiator: bool,
}

impl NoiseHandshake {
    /// Creates a Noise `IK` initiator pinned to the verified responder `DeviceCertificate`.
    #[allow(clippy::too_many_arguments)]
    pub fn new_initiator(
        local_x25519_secret: &[u8; 32],
        remote_cert: &DeviceCertificate,
        identity_pub_ed25519: &[u8; 32],
        revocation_policy: RevocationPolicy<'_>,
        now_sec: i64,
        stream_id: &str,
        host_epoch: u64,
    ) -> Result<Self> {
        verify_device_certificate(
            remote_cert,
            identity_pub_ed25519,
            revocation_policy,
            now_sec,
        )?;
        let remote_x25519 = decode_fixed_b64::<32>(&remote_cert.x25519_pub_b64, "x25519_pub_b64")?;
        Self::new_initiator_pinned(local_x25519_secret, &remote_x25519, stream_id, host_epoch)
    }

    /// Creates a Noise `IK` initiator with an already-verified remote static X25519 key.
    pub fn new_initiator_pinned(
        local_x25519_secret: &[u8; 32],
        remote_x25519_pub: &[u8; 32],
        stream_id: &str,
        host_epoch: u64,
    ) -> Result<Self> {
        let prologue = tunnel_prologue(stream_id, host_epoch)?;
        let params: snow::params::NoiseParams = NOISE_PARAMS
            .parse()
            .map_err(|e: snow::Error| ProtocolError::Noise(e.to_string()))?;
        let state = Builder::new(params)
            .local_private_key(local_x25519_secret)
            .map_err(|e| ProtocolError::Noise(e.to_string()))?
            .remote_public_key(remote_x25519_pub)
            .map_err(|e| ProtocolError::Noise(e.to_string()))?
            .prologue(&prologue)
            .map_err(|e| ProtocolError::Noise(e.to_string()))?
            .build_initiator()
            .map_err(|e| ProtocolError::Noise(e.to_string()))?;
        Ok(Self {
            state,
            expected_remote_static: Zeroizing::new(*remote_x25519_pub),
            stream_id: stream_id.to_string(),
            host_epoch,
            is_initiator: true,
        })
    }

    /// Creates a Noise `IK` responder pinned to the verified initiator `DeviceCertificate`.
    #[allow(clippy::too_many_arguments)]
    pub fn new_responder(
        local_x25519_secret: &[u8; 32],
        expected_initiator_cert: &DeviceCertificate,
        identity_pub_ed25519: &[u8; 32],
        revocation_policy: RevocationPolicy<'_>,
        now_sec: i64,
        stream_id: &str,
        host_epoch: u64,
    ) -> Result<Self> {
        verify_device_certificate(
            expected_initiator_cert,
            identity_pub_ed25519,
            revocation_policy,
            now_sec,
        )?;
        let remote_x25519 =
            decode_fixed_b64::<32>(&expected_initiator_cert.x25519_pub_b64, "x25519_pub_b64")?;
        Self::new_responder_pinned(local_x25519_secret, &remote_x25519, stream_id, host_epoch)
    }

    /// Creates a Noise `IK` responder expecting a specific verified initiator static X25519 key.
    pub fn new_responder_pinned(
        local_x25519_secret: &[u8; 32],
        expected_initiator_x25519_pub: &[u8; 32],
        stream_id: &str,
        host_epoch: u64,
    ) -> Result<Self> {
        let prologue = tunnel_prologue(stream_id, host_epoch)?;
        let params: snow::params::NoiseParams = NOISE_PARAMS
            .parse()
            .map_err(|e: snow::Error| ProtocolError::Noise(e.to_string()))?;
        let state = Builder::new(params)
            .local_private_key(local_x25519_secret)
            .map_err(|e| ProtocolError::Noise(e.to_string()))?
            .prologue(&prologue)
            .map_err(|e| ProtocolError::Noise(e.to_string()))?
            .build_responder()
            .map_err(|e| ProtocolError::Noise(e.to_string()))?;
        Ok(Self {
            state,
            expected_remote_static: Zeroizing::new(*expected_initiator_x25519_pub),
            stream_id: stream_id.to_string(),
            host_epoch,
            is_initiator: false,
        })
    }

    /// Writes the next Noise `IK` handshake message (`-> e, es, s, ss` for initiator;
    /// `<- e, ee, se` for responder).
    ///
    /// Enforces `payload.is_empty()`: Noise `IK` msg1 has weak forward secrecy and is
    /// replayable, so zero application data or secrets may be placed in handshake messages.
    pub fn write_message(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        if !payload.is_empty() {
            return Err(ProtocolError::InvalidInput(
                "Noise IK handshake payloads must be empty (send application data only after into_transport)"
                    .to_string(),
            ));
        }
        let mut out = vec![0u8; 256];
        let n = self
            .state
            .write_message(&[], &mut out)
            .map_err(|e| ProtocolError::Noise(e.to_string()))?;
        out.truncate(n);
        Ok(out)
    }

    /// Reads an incoming Noise `IK` handshake message, enforces an empty handshake payload,
    /// and verifies the remote static key pin as soon as it becomes available.
    pub fn read_message(&mut self, message: &[u8]) -> Result<Vec<u8>> {
        if message.len() > 256 {
            return Err(ProtocolError::TooLarge(
                "Noise IK empty-payload handshake message exceeds 256 bytes".to_string(),
            ));
        }
        let mut out = vec![0u8; message.len()];
        let n = self
            .state
            .read_message(message, &mut out)
            .map_err(|e| ProtocolError::Noise(e.to_string()))?;
        out.truncate(n);

        if !out.is_empty() {
            return Err(ProtocolError::Malformed(
                "unexpected non-empty Noise IK handshake payload".to_string(),
            ));
        }

        if let Some(remote_static) = self.state.get_remote_static() {
            if remote_static != *self.expected_remote_static {
                return Err(ProtocolError::BadSignature(
                    "Noise remote static X25519 key does not match pinned DeviceCertificate"
                        .to_string(),
                ));
            }
        }
        Ok(out)
    }

    /// Responder convenience helper that checks [`EpochHandshakeReplayGuard`] before
    /// spending DH work on [`Self::read_message`].
    pub fn read_message_guarded(
        &mut self,
        message: &[u8],
        guard: &mut EpochHandshakeReplayGuard,
    ) -> Result<Vec<u8>> {
        guard.check_and_record(self.host_epoch, message)?;
        self.read_message(message)
    }

    /// True once both Noise `IK` messages have been exchanged.
    pub fn is_handshake_finished(&self) -> bool {
        self.state.is_handshake_finished()
    }

    /// Transitions a completed handshake into an authenticated [`TunnelSession`] with
    /// per-direction monotonic send counters and a 1024-bit sliding replay window.
    pub fn into_transport(self) -> Result<TunnelSession> {
        if !self.state.is_handshake_finished() {
            return Err(ProtocolError::Noise(
                "cannot enter transport mode before handshake finishes".to_string(),
            ));
        }
        let remote_static = self.state.get_remote_static().ok_or_else(|| {
            ProtocolError::Noise("missing remote static key after Noise IK handshake".to_string())
        })?;
        if remote_static != *self.expected_remote_static {
            return Err(ProtocolError::BadSignature(
                "Noise remote static X25519 key mismatch".to_string(),
            ));
        }
        let handshake_hash = Zeroizing::new(self.state.get_handshake_hash().to_vec());
        let transport = self
            .state
            .into_stateless_transport_mode()
            .map_err(|e| ProtocolError::Noise(e.to_string()))?;
        Ok(TunnelSession {
            transport,
            stream_id: self.stream_id,
            host_epoch: self.host_epoch,
            is_initiator: self.is_initiator,
            send_seq: 0,
            recv_window: ReplayWindow::new(),
            handshake_hash,
        })
    }
}

/// Authenticated Noise `IK` transport session with strict monotonic send counters
/// and a WireGuard/IPsec-style 1024-bit sliding replay window (Opus R1, review #20).
///
/// Wire ciphertext layout: `seq (8-byte BE u64, starting at 1) || Noise AEAD ciphertext`.
/// - Out-of-order frames within `[high_water - 1023, high_water]` (e.g. an `approval`
///   frame replayed after a live `pty` frame) are accepted once and recorded in the bitmap.
/// - Duplicate frames and frames older than `REPLAY_WINDOW_SIZE` (`1024`) are rejected
///   with [`ProtocolError::ReplayDetected`] before AEAD work.
/// - Tampering with the 8-byte `seq` prefix fails Poly1305 verification without
///   advancing or polluting the replay window.
pub struct TunnelSession {
    transport: StatelessTransportState,
    stream_id: String,
    host_epoch: u64,
    is_initiator: bool,
    send_seq: u64,
    recv_window: ReplayWindow,
    handshake_hash: Zeroizing<Vec<u8>>,
}

impl TunnelSession {
    /// Stream identifier bound in the Noise prologue.
    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    /// Host epoch bound in the Noise prologue.
    pub fn host_epoch(&self) -> u64 {
        self.host_epoch
    }

    /// True if this side acted as the Noise `IK` initiator.
    pub fn is_initiator(&self) -> bool {
        self.is_initiator
    }

    /// Highest sequence number transmitted so far (`0` before first frame).
    pub fn send_seq(&self) -> u64 {
        self.send_seq
    }

    /// Highest authenticated sequence number received so far (`0` before first frame).
    pub fn recv_high_water(&self) -> u64 {
        self.recv_window.high_water
    }

    /// Noise handshake hash `h` (32 bytes for SHA-256), usable as a channel binding token.
    pub fn handshake_hash(&self) -> &[u8] {
        &self.handshake_hash
    }

    /// Encrypts `plaintext` with the next monotonic sequence number (`send_seq + 1`).
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        if plaintext.len() > MAX_TUNNEL_PLAINTEXT_BYTES {
            return Err(ProtocolError::TooLarge(format!(
                "tunnel plaintext exceeds {MAX_TUNNEL_PLAINTEXT_BYTES} bytes"
            )));
        }
        let next_seq = self
            .send_seq
            .checked_add(1)
            .ok_or(ProtocolError::RekeyRequired {
                limit: MAX_FRAMES_PER_SESSION,
            })?;
        if next_seq > MAX_FRAMES_PER_SESSION {
            return Err(ProtocolError::RekeyRequired {
                limit: MAX_FRAMES_PER_SESSION,
            });
        }

        let mut out = vec![0u8; 8 + plaintext.len() + 16];
        out[..8].copy_from_slice(&next_seq.to_be_bytes());
        let written = self
            .transport
            .write_message(next_seq, plaintext, &mut out[8..])
            .map_err(|e| ProtocolError::Noise(e.to_string()))?;
        out.truncate(8 + written);
        self.send_seq = next_seq;
        Ok(out)
    }

    /// Decrypts a framed ciphertext using the 1024-bit sliding replay window.
    ///
    /// Rejects duplicates and below-window frames BEFORE AEAD decryption, and
    /// marks `seq` accepted in the bitmap ONLY after Poly1305 authentication succeeds.
    pub fn decrypt(&mut self, framed_ciphertext: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        if framed_ciphertext.len() < 8 + 16 {
            return Err(ProtocolError::Malformed(
                "tunnel ciphertext shorter than header + tag".to_string(),
            ));
        }
        if framed_ciphertext.len() > 8 + 65_535 {
            return Err(ProtocolError::TooLarge(
                "tunnel ciphertext exceeds Noise 65535-byte message limit".to_string(),
            ));
        }
        let seq = u64::from_be_bytes(
            framed_ciphertext[..8]
                .try_into()
                .map_err(|_| ProtocolError::Malformed("tunnel seq prefix".to_string()))?,
        );
        self.recv_window.check(seq)?;
        if seq > MAX_FRAMES_PER_SESSION {
            return Err(ProtocolError::RekeyRequired {
                limit: MAX_FRAMES_PER_SESSION,
            });
        }

        let mut plain = Zeroizing::new(vec![0u8; framed_ciphertext.len() - 8]);
        let n = self
            .transport
            .read_message(seq, &framed_ciphertext[8..], &mut plain)
            .map_err(|_| ProtocolError::Aead("tunnel frame authentication failed".to_string()))?;
        plain.truncate(n);
        self.recv_window.mark_accepted(seq);
        Ok(plain)
    }
}

/// Helper to derive an X25519 public key from a 32-byte secret seed.
pub fn x25519_public_from_secret(secret: &[u8; 32]) -> [u8; 32] {
    x25519(*secret, X25519_BASEPOINT_BYTES)
}

/// Helper to derive an Ed25519 public key from a 32-byte secret seed.
pub fn ed25519_public_from_secret(secret: &[u8; 32]) -> [u8; 32] {
    EdSigningKey::from_bytes(secret).verifying_key().to_bytes()
}

pub(crate) fn decode_fixed_b64<const N: usize>(b64: &str, field: &str) -> Result<[u8; N]> {
    let raw = STANDARD
        .decode(b64)
        .map_err(|_| ProtocolError::Malformed(format!("{field}: invalid base64")))?;
    raw.try_into()
        .map_err(|_| ProtocolError::Malformed(format!("{field}: expected {N} bytes")))
}
