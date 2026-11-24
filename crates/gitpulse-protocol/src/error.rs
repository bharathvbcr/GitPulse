//! Error types for `gitpulse-protocol`.

use thiserror::Error;

/// Canonical error type returned by every fallible protocol and crypto operation.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// Input failed boundary, syntax, or invariant validation.
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// Wire payload or frame exceeded its maximum allowed size.
    #[error("payload too large: {0}")]
    TooLarge(String),

    /// Wire payload could not be parsed or contained unexpected trailing bytes.
    #[error("malformed wire data: {0}")]
    Malformed(String),

    /// Cryptographic signature verification failed.
    #[error("invalid signature: {0}")]
    BadSignature(String),

    /// Certificate or revocation list has expired.
    #[error("expired: {0}")]
    Expired(String),

    /// Certificate or decision is not yet valid (beyond clock skew tolerance).
    #[error("not yet valid: {0}")]
    NotYetValid(String),

    /// Device has been revoked by an IdentityKey-signed revocation list.
    #[error("device revoked: {0}")]
    Revoked(String),

    /// Certificate TTL exceeds the 30-day maximum (Opus R3/M2).
    #[error("certificate TTL exceeds 30-day maximum")]
    CertTtlExceeded,

    /// Revocation list version regressed (non-monotonic replay attempt).
    #[error("revocation list version rollback: got {got}, minimum {min}")]
    RevocationRollback {
        /// Version supplied in the candidate list.
        got: u64,
        /// Minimum version already observed.
        min: u64,
    },

    /// Noise handshake or transport operation failed.
    #[error("noise error: {0}")]
    Noise(String),

    /// Replay, duplicate, or out-of-order frame detected on a tunnel stream.
    #[error("replay or reorder detected: seq {got} <= high-water {high_water}")]
    ReplayDetected {
        /// Incoming frame sequence number.
        got: u64,
        /// Highest already-authenticated sequence number.
        high_water: u64,
    },

    /// Tunnel session reached its frame limit and must re-handshake.
    #[error("tunnel rekey required after {limit} frames")]
    RekeyRequired {
        /// Maximum frames allowed under one session key.
        limit: u64,
    },

    /// Authenticated encryption or decryption failed.
    #[error("aead authentication failed: {0}")]
    Aead(String),

    /// OS randomness source failed.
    #[error("randomness unavailable: {0}")]
    Rng(String),

    /// Approval decision binding mismatch (host, ticket, or canonical input hash).
    #[error("approval binding mismatch: {0}")]
    BindingMismatch(String),

    /// JSON serialization or deserialization failed.
    #[error("json error: {0}")]
    Json(String),
}

/// Convenience result alias for `gitpulse-protocol`.
pub type Result<T> = std::result::Result<T, ProtocolError>;
