//! Minimal, stable FFI / UniFFI-ready surface (Opus M4).
//!
//! Deliberately flat primitive/string/byte signatures so this module can be
//! exposed directly via `uniffi-bindgen-react-native`, a Swift `XCFramework`
//! (for the iOS Notification Service Extension), Kotlin/JNI, or a hand-written
//! C ABI with zero complex generics or lifetimes crossing the boundary.
//!
//! Security boundaries (Opus M2, M3, #15, #20):
//! - Raw key material is passed in by the native module (from Keychain / Keystore)
//!   for the duration of the call and never returned to JS.
//! - Hardware P-256 approval keys never enter Rust in private form: native code
//!   calls [`build_approval_signing_bytes`] and signs the returned bytes inside
//!   iOS Secure Enclave or Android StrongBox.
//! - [`verify_approval_decision`] REQUIRES a non-empty, fresh [`SignedRevocationList`]
//!   JSON (`revocations_json: &str`); missing or stale lists fail closed rather than
//!   silently bypassing revocation checks.

use crate::canonical::{
    approval_signing_bytes, canonical_tool_input_bytes, input_sha256_hex,
    parse_canonical_json_value,
};
use crate::crypto::{self, NoiseHandshake, RelayAuthHeaders, RevocationPolicy, TunnelSession};
use crate::error::{ProtocolError, Result};
use crate::wire::{
    ApprovalDecisionKind, DeviceCertificate, SignedApprovalDecision, SignedRevocationList,
};

/// Computes lowercase hex `input_sha256` from `tool_name`, JSON-encoded `tool_input_json`,
/// and `cwd`. Fails closed with [`ProtocolError::NonCanonicalNumber`] if `tool_input_json`
/// contains a number that cannot be represented losslessly.
pub fn hash_canonical(tool_name: &str, tool_input_json: &str, cwd: &str) -> Result<String> {
    let tool_input = parse_canonical_json_value(tool_input_json)?;
    input_sha256_hex(tool_name, &tool_input, cwd)
}

/// Returns the exact canonical UTF-8 JSON bytes rendered by the native mobile approval
/// confirmation sheet before computing [`hash_canonical`] (Opus P0-5/M1/M3).
pub fn render_canonical_tool_input(
    tool_name: &str,
    tool_input_json: &str,
    cwd: &str,
) -> Result<String> {
    let tool_input = parse_canonical_json_value(tool_input_json)?;
    let bytes = canonical_tool_input_bytes(tool_name, &tool_input, cwd)?;
    String::from_utf8(bytes).map_err(|e| ProtocolError::Json(e.to_string()))
}

/// Builds the exact domain-separated bytes signed by the mobile hardware P-256 key
/// (Secure Enclave / StrongBox) for a [`SignedApprovalDecision`].
#[allow(clippy::too_many_arguments)]
pub fn build_approval_signing_bytes(
    host_id: &str,
    ticket_id: &str,
    input_sha256: &str,
    decision: ApprovalDecisionKind,
    nonce: &str,
    issued_at: i64,
    expires_at: i64,
) -> Result<Vec<u8>> {
    approval_signing_bytes(
        host_id,
        ticket_id,
        input_sha256,
        decision,
        nonce,
        issued_at,
        expires_at,
    )
}

/// Verifies a JSON-encoded [`SignedApprovalDecision`] against a JSON-encoded
/// [`DeviceCertificate`], a Keychain-pinned 32-byte Account `IdentityKey` Ed25519
/// public key, expected ticket binding tuple, a **mandatory** JSON-encoded
/// [`SignedRevocationList`], minimum revocation list version, and `now_sec`.
///
/// Fails closed if `revocations_json` is missing, malformed, expired, forged, or
/// rolled back (Opus review #20).
#[allow(clippy::too_many_arguments)]
pub fn verify_approval_decision(
    decision_json: &str,
    cert_json: &str,
    identity_pub_ed25519: &[u8],
    expected_host_id: &str,
    expected_ticket_id: &str,
    expected_input_sha256: &str,
    revocations_json: &str,
    min_revocation_version: u64,
    now_sec: i64,
) -> Result<ApprovalDecisionKind> {
    let decision: SignedApprovalDecision = serde_json::from_str(decision_json)
        .map_err(|e| ProtocolError::Json(format!("decision_json: {e}")))?;
    let cert: DeviceCertificate = serde_json::from_str(cert_json)
        .map_err(|e| ProtocolError::Json(format!("cert_json: {e}")))?;
    let revocations: SignedRevocationList = serde_json::from_str(revocations_json)
        .map_err(|e| ProtocolError::Json(format!("revocations_json: {e}")))?;
    let identity_pub = slice_to_array32(identity_pub_ed25519, "identity_pub_ed25519")?;
    crypto::verify_approval_decision(
        &decision,
        &cert,
        &identity_pub,
        expected_host_id,
        expected_ticket_id,
        expected_input_sha256,
        RevocationPolicy::Enforced {
            list: &revocations,
            min_version: min_revocation_version,
        },
        now_sec,
    )
}

/// Seals a `<= 2 KiB` NSE/FCM wake push `preview_ct` blob.
pub fn seal_preview(
    sender_device_id: &str,
    sender_ed25519_seed: &[u8],
    recipient_x25519_pub: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let seed = slice_to_array32(sender_ed25519_seed, "sender_ed25519_seed")?;
    let pubkey = slice_to_array32(recipient_x25519_pub, "recipient_x25519_pub")?;
    crypto::seal_preview(sender_device_id, &seed, &pubkey, plaintext)
}

/// Opens and authenticates a `<= 2 KiB` NSE/FCM wake push `preview_ct` blob.
pub fn open_preview(
    sealed_bytes: &[u8],
    recipient_x25519_secret: &[u8],
    sender_ed25519_pub: &[u8],
    expected_sender_device_id: &str,
) -> Result<Vec<u8>> {
    let secret = slice_to_array32(recipient_x25519_secret, "recipient_x25519_secret")?;
    let sender_pub = slice_to_array32(sender_ed25519_pub, "sender_ed25519_pub")?;
    let opened = crypto::open_preview(
        sealed_bytes,
        &secret,
        &sender_pub,
        expected_sender_device_id,
    )?;
    Ok(opened.to_vec())
}

/// Signs an HTTP or WebSocket upgrade request for `gitpulse-relay`.
pub fn sign_relay_request(
    device_id: &str,
    ed25519_seed: &[u8],
    method: &str,
    request_uri: &str,
    timestamp_ms: i64,
    body: &[u8],
) -> Result<RelayAuthHeaders> {
    let seed = slice_to_array32(ed25519_seed, "ed25519_seed")?;
    crypto::sign_relay_request(device_id, &seed, method, request_uri, timestamp_ms, body)
}

/// Starts a Noise `IK` initiator handshake pinned to the verified responder `DeviceCertificate`
/// and a mandatory [`SignedRevocationList`] (pass `revocations_json = None` only when
/// `min_revocation_version == 0` during initial account bootstrap).
#[allow(clippy::too_many_arguments)]
pub fn tunnel_initiator_new(
    local_x25519_secret: &[u8],
    remote_cert_json: &str,
    identity_pub_ed25519: &[u8],
    revocations_json: Option<&str>,
    min_revocation_version: u64,
    now_sec: i64,
    stream_id: &str,
    host_epoch: u64,
) -> Result<NoiseHandshake> {
    let secret = slice_to_array32(local_x25519_secret, "local_x25519_secret")?;
    let identity_pub = slice_to_array32(identity_pub_ed25519, "identity_pub_ed25519")?;
    let cert: DeviceCertificate = serde_json::from_str(remote_cert_json)
        .map_err(|e| ProtocolError::Json(format!("remote_cert_json: {e}")))?;
    let revocations: Option<SignedRevocationList> = match revocations_json {
        Some(raw) => Some(
            serde_json::from_str(raw)
                .map_err(|e| ProtocolError::Json(format!("revocations_json: {e}")))?,
        ),
        None => None,
    };
    let policy = match revocations.as_ref() {
        Some(list) => RevocationPolicy::Enforced {
            list,
            min_version: min_revocation_version,
        },
        None => RevocationPolicy::NeverIssued {
            min_version: min_revocation_version,
        },
    };
    NoiseHandshake::new_initiator(
        &secret,
        &cert,
        &identity_pub,
        policy,
        now_sec,
        stream_id,
        host_epoch,
    )
}

/// Starts a Noise `IK` responder handshake pinned to the verified initiator `DeviceCertificate`.
#[allow(clippy::too_many_arguments)]
pub fn tunnel_responder_new(
    local_x25519_secret: &[u8],
    expected_initiator_cert_json: &str,
    identity_pub_ed25519: &[u8],
    revocations_json: Option<&str>,
    min_revocation_version: u64,
    now_sec: i64,
    stream_id: &str,
    host_epoch: u64,
) -> Result<NoiseHandshake> {
    let secret = slice_to_array32(local_x25519_secret, "local_x25519_secret")?;
    let identity_pub = slice_to_array32(identity_pub_ed25519, "identity_pub_ed25519")?;
    let cert: DeviceCertificate = serde_json::from_str(expected_initiator_cert_json)
        .map_err(|e| ProtocolError::Json(format!("expected_initiator_cert_json: {e}")))?;
    let revocations: Option<SignedRevocationList> = match revocations_json {
        Some(raw) => Some(
            serde_json::from_str(raw)
                .map_err(|e| ProtocolError::Json(format!("revocations_json: {e}")))?,
        ),
        None => None,
    };
    let policy = match revocations.as_ref() {
        Some(list) => RevocationPolicy::Enforced {
            list,
            min_version: min_revocation_version,
        },
        None => RevocationPolicy::NeverIssued {
            min_version: min_revocation_version,
        },
    };
    NoiseHandshake::new_responder(
        &secret,
        &cert,
        &identity_pub,
        policy,
        now_sec,
        stream_id,
        host_epoch,
    )
}

/// Encrypts a plaintext payload on an established Noise `IK` [`TunnelSession`].
pub fn tunnel_encrypt(session: &mut TunnelSession, plaintext: &[u8]) -> Result<Vec<u8>> {
    session.encrypt(plaintext)
}

/// Decrypts a framed ciphertext payload on an established Noise `IK` [`TunnelSession`],
/// enforcing the 1024-bit sliding replay window.
pub fn tunnel_decrypt(session: &mut TunnelSession, ciphertext: &[u8]) -> Result<Vec<u8>> {
    let plain = session.decrypt(ciphertext)?;
    Ok(plain.to_vec())
}

fn slice_to_array32(slice: &[u8], field: &str) -> Result<[u8; 32]> {
    slice
        .try_into()
        .map_err(|_| ProtocolError::InvalidInput(format!("{field} must be exactly 32 bytes")))
}
