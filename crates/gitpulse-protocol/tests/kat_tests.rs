//! Deterministic Known-Answer Tests (KAT) and adversarial security tests for
//! `gitpulse-protocol`.
//!
//! Guards identical cryptographic behavior across Desktop, `gitpulsed --relay`,
//! `gitpulse-relay` (Go), iOS Swift/UniFFI, and Android Kotlin/UniFFI.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use gitpulse_protocol::canonical::{
    approval_signing_bytes, canonical_json_from_str, canonical_json_string,
    canonical_tool_input_bytes, hex_decode, hex_encode, input_sha256_hex, steering_signing_bytes,
    steering_text_sha256_hex,
};
use gitpulse_protocol::crypto::{
    ed25519_public_from_secret, issue_device_certificate, issue_revocation_list, open_preview,
    relay_signing_message, revocation_signing_bytes, seal_preview_with_ephemeral,
    sign_approval_decision_software, sign_relay_request, sign_steering_confirm_software,
    verify_approval_decision, verify_device_certificate, verify_relay_request,
    verify_revocation_list, verify_steering_confirm, x25519_public_from_secret,
    EpochHandshakeReplayGuard, NoiseHandshake, RevocationPolicy, REPLAY_WINDOW_SIZE,
};
use gitpulse_protocol::uniffi_api;
use gitpulse_protocol::wire::{
    ApprovalChannel, ApprovalDecisionKind, ApprovalOutcome, ApprovalResolvedEvent, ApprovalTicket,
    CapabilitySet, ControlPlaneEvent, DeviceKind, FrameClass, OuterFrame, SessionSnapshot,
    SignedApprovalDecision, SignedRevocationList, SignedSteeringConfirm, SteeringConfirmReq,
    SteeringConfirmResp, SteeringDeliveryMode, SteeringPreviewReq, SteeringPreviewResp,
    SteeringStatus, SubagentSummary, WakePushFrame, WakePushKind, GPTF_VERSION, MAX_CERT_TTL_SECS,
    MAX_PREVIEW_CT_BYTES,
};
use gitpulse_protocol::ProtocolError;
use p256::ecdsa::SigningKey as P256SigningKey;
use serde_json::{json, Value};

const NOW: i64 = 1_800_000_000;

const IDENTITY_SEED: [u8; 32] = [0x11; 32];
const HOST_ED_SEED: [u8; 32] = [0x22; 32];
const HOST_X_SECRET: [u8; 32] = [0x33; 32];
const MOBILE_ED_SEED: [u8; 32] = [0x44; 32];
const MOBILE_X_SECRET: [u8; 32] = [0x55; 32];
const MOBILE_P256_SECRET: [u8; 32] = [0x66; 32];
const EPHEMERAL_X_SECRET: [u8; 32] = [0x77; 32];

fn p256_compressed_pub(secret: &[u8; 32]) -> Vec<u8> {
    let sk = P256SigningKey::from_bytes(secret.into()).expect("valid p256 test scalar");
    sk.verifying_key().to_sec1_point(true).as_bytes().to_vec()
}

fn fixture_certs() -> (
    [u8; 32],
    gitpulse_protocol::wire::DeviceCertificate,
    gitpulse_protocol::wire::DeviceCertificate,
) {
    let identity_pub = ed25519_public_from_secret(&IDENTITY_SEED);
    let host_cert = issue_device_certificate(
        &IDENTITY_SEED,
        "dev_host_01",
        DeviceKind::Desktop,
        "MacBook Pro",
        &ed25519_public_from_secret(&HOST_ED_SEED),
        &x25519_public_from_secret(&HOST_X_SECRET),
        None,
        NOW - 60,
        NOW + 14 * 86_400,
    )
    .expect("host cert");

    let mobile_cert = issue_device_certificate(
        &IDENTITY_SEED,
        "dev_mobile_01",
        DeviceKind::Mobile,
        "iPhone 17 Pro",
        &ed25519_public_from_secret(&MOBILE_ED_SEED),
        &x25519_public_from_secret(&MOBILE_X_SECRET),
        Some(&p256_compressed_pub(&MOBILE_P256_SECRET)),
        NOW - 60,
        NOW + 14 * 86_400,
    )
    .expect("mobile cert");

    (identity_pub, host_cert, mobile_cert)
}

#[test]
fn kat_canonical_json_and_input_sha256_are_deterministic_and_key_order_independent() {
    let input_a = json!({
        "command": "git status --short",
        "timeout": 30000,
        "nested": { "z": 1, "a": [true, null, "x\ny"] }
    });
    let input_b = json!({
        "nested": { "a": [true, null, "x\ny"], "z": 1 },
        "timeout": 30000,
        "command": "git status --short"
    });

    let canon_a = canonical_json_string(&input_a).expect("canon_a");
    let canon_b = canonical_json_string(&input_b).expect("canon_b");
    assert_eq!(
        canon_a,
        r#"{"command":"git status --short","nested":{"a":[true,null,"x\ny"],"z":1},"timeout":30000}"#
    );
    assert_eq!(canon_a, canon_b);

    let full_bytes =
        canonical_tool_input_bytes("Bash", &input_a, "/Users/dev/repo").expect("canonical bytes");
    assert_eq!(
        String::from_utf8(full_bytes).expect("utf8"),
        r#"{"cwd":"/Users/dev/repo","tool_input":{"command":"git status --short","nested":{"a":[true,null,"x\ny"],"z":1},"timeout":30000},"tool_name":"Bash"}"#
    );

    let hash_a = input_sha256_hex("Bash", &input_a, "/Users/dev/repo").expect("hash_a");
    let hash_b = input_sha256_hex("Bash", &input_b, "/Users/dev/repo").expect("hash_b");
    assert_eq!(hash_a, hash_b);
    assert_eq!(hash_a.len(), 64);

    let ffi_hash =
        uniffi_api::hash_canonical("Bash", &input_b.to_string(), "/Users/dev/repo").expect("ffi");
    assert_eq!(hash_a, ffi_hash);

    let diff_cwd = input_sha256_hex("Bash", &input_a, "/Users/dev/other").expect("diff_cwd");
    assert_ne!(hash_a, diff_cwd);
}

#[test]
fn kat_canonical_rejects_non_canonical_numbers_fail_closed() {
    // Integers outside u64/i64 range fail closed with NonCanonicalNumber (approve at desk only).
    for bad in [
        r#"{"n":18446744073709551616}"#,
        r#"{"n":99999999999999999999999999}"#,
        r#"{"n":-9223372036854775809}"#,
        r#"{"n":0.10000000000000000555}"#,
        r#"{"n":1.0}"#,
        r#"{"n":-0}"#,
        r#"{"n":-0.0}"#,
        r#"{"n":1e309}"#,
    ] {
        assert!(
            matches!(
                canonical_json_from_str(bad),
                Err(ProtocolError::NonCanonicalNumber(_))
            ),
            "expected NonCanonicalNumber for {bad}"
        );
        assert!(
            matches!(
                uniffi_api::hash_canonical("Bash", bad, "/repo"),
                Err(ProtocolError::NonCanonicalNumber(_))
            ),
            "expected UniFFI NonCanonicalNumber for {bad}"
        );
    }

    // Valid exact integers (including i64/u64 extremes) and lossless finite floats succeed.
    for good in [
        r#"{"n":0}"#,
        r#"{"n":-9223372036854775808}"#,
        r#"{"n":18446744073709551615}"#,
        r#"{"n":0.5}"#,
        r#"{"n":-12.25}"#,
        // Numbers inside strings are NOT JSON numbers and must remain untouched.
        r#"{"cmd":"echo 99999999999999999999999999 1.0 -0.0"}"#,
    ] {
        assert!(
            canonical_json_from_str(good).is_ok(),
            "unexpected rejection for {good}"
        );
    }
}

#[test]
fn kat_approval_decision_rejects_updated_input_and_updated_permissions() {
    let smuggled_updated_input = json!({
        "host_id": "dev_host_01",
        "ticket_id": "tkt_01",
        "input_sha256": "a".repeat(64),
        "decision": "allow_once",
        "nonce": "nonce_01",
        "issued_at": NOW,
        "expires_at": NOW + 60,
        "device_id": "dev_mobile_01",
        "sig_p256_der_b64": "AAAA",
        "updatedInput": { "command": "rm -rf /" }
    });
    assert!(serde_json::from_value::<SignedApprovalDecision>(smuggled_updated_input).is_err());

    let smuggled_updated_perms = json!({
        "host_id": "dev_host_01",
        "ticket_id": "tkt_01",
        "input_sha256": "a".repeat(64),
        "decision": "allow_once",
        "nonce": "nonce_01",
        "issued_at": NOW,
        "expires_at": NOW + 60,
        "device_id": "dev_mobile_01",
        "sig_p256_der_b64": "AAAA",
        "updatedPermissions": ["allow_all"]
    });
    assert!(serde_json::from_value::<SignedApprovalDecision>(smuggled_updated_perms).is_err());

    let bad_kind = json!("allow_always");
    assert!(serde_json::from_value::<ApprovalDecisionKind>(bad_kind).is_err());
}

#[test]
fn kat_p256_approval_verify_mandatory_revocation_and_tamper_rejection() {
    let (identity_pub, _host_cert, mobile_cert) = fixture_certs();
    let input_hash = input_sha256_hex(
        "Bash",
        &json!({ "command": "cargo test" }),
        "/Users/dev/GitPulse",
    )
    .expect("input hash");

    let signing_bytes = approval_signing_bytes(
        "dev_host_01",
        "tkt_1001",
        &input_hash,
        ApprovalDecisionKind::AllowOnce,
        "nonce_abc",
        NOW - 5,
        NOW + 120,
    )
    .expect("signing bytes");
    assert!(signing_bytes.starts_with(b"gitpulse-approval/v1\n{"));

    let decision = sign_approval_decision_software(
        &MOBILE_P256_SECRET,
        "dev_mobile_01",
        "dev_host_01",
        "tkt_1001",
        &input_hash,
        ApprovalDecisionKind::AllowOnce,
        "nonce_abc",
        NOW - 5,
        NOW + 120,
    )
    .expect("signed decision");

    let rev_list = issue_revocation_list(&IDENTITY_SEED, 1, NOW - 30, NOW + 3600, vec![])
        .expect("revocation list");

    // Valid decision verifies with mandatory RevocationPolicy::Enforced.
    let verdict = verify_approval_decision(
        &decision,
        &mobile_cert,
        &identity_pub,
        "dev_host_01",
        "tkt_1001",
        &input_hash,
        RevocationPolicy::Enforced {
            list: &rev_list,
            min_version: 1,
        },
        NOW,
    )
    .expect("valid approval");
    assert_eq!(verdict, ApprovalDecisionKind::AllowOnce);

    // UniFFI requires revocations_json as non-optional &str; empty/invalid fails closed.
    let decision_json = serde_json::to_string(&decision).unwrap();
    let mobile_cert_json = serde_json::to_string(&mobile_cert).unwrap();
    let rev_json = serde_json::to_string(&rev_list).unwrap();
    let ffi_verdict = uniffi_api::verify_approval_decision(
        &decision_json,
        &mobile_cert_json,
        &identity_pub,
        "dev_host_01",
        "tkt_1001",
        &input_hash,
        &rev_json,
        1,
        NOW,
    )
    .expect("ffi verify");
    assert_eq!(ffi_verdict, ApprovalDecisionKind::AllowOnce);

    assert!(matches!(
        uniffi_api::verify_approval_decision(
            &decision_json,
            &mobile_cert_json,
            &identity_pub,
            "dev_host_01",
            "tkt_1001",
            &input_hash,
            "",
            1,
            NOW,
        ),
        Err(ProtocolError::Json(_))
    ));

    // Deliberate NeverIssued { min_version: 0 } succeeds ONLY when min_version == 0;
    // NeverIssued { min_version: 1 } fails closed with RevocationRollback!
    assert_eq!(
        verify_approval_decision(
            &decision,
            &mobile_cert,
            &identity_pub,
            "dev_host_01",
            "tkt_1001",
            &input_hash,
            RevocationPolicy::NeverIssued { min_version: 0 },
            NOW,
        )
        .unwrap(),
        ApprovalDecisionKind::AllowOnce
    );
    assert_eq!(
        verify_approval_decision(
            &decision,
            &mobile_cert,
            &identity_pub,
            "dev_host_01",
            "tkt_1001",
            &input_hash,
            RevocationPolicy::NeverIssued { min_version: 1 },
            NOW,
        ),
        Err(ProtocolError::RevocationRollback { got: 0, min: 1 })
    );

    // Tamper 1: change decision from AllowOnce to DenyAndInterrupt -> BadSignature.
    let mut tampered_decision = decision.clone();
    tampered_decision.decision = ApprovalDecisionKind::DenyAndInterrupt;
    assert!(matches!(
        verify_approval_decision(
            &tampered_decision,
            &mobile_cert,
            &identity_pub,
            "dev_host_01",
            "tkt_1001",
            &input_hash,
            RevocationPolicy::Enforced {
                list: &rev_list,
                min_version: 1,
            },
            NOW,
        ),
        Err(ProtocolError::BadSignature(_))
    ));

    // Tamper 2: wrong input_sha256 -> BindingMismatch.
    let other_hash = input_sha256_hex("Bash", &json!({ "command": "rm -rf /" }), "/").unwrap();
    assert!(matches!(
        verify_approval_decision(
            &decision,
            &mobile_cert,
            &identity_pub,
            "dev_host_01",
            "tkt_1001",
            &other_hash,
            RevocationPolicy::Enforced {
                list: &rev_list,
                min_version: 1,
            },
            NOW,
        ),
        Err(ProtocolError::BindingMismatch(_))
    ));

    // Tamper 3: expired decision -> Expired.
    assert!(matches!(
        verify_approval_decision(
            &decision,
            &mobile_cert,
            &identity_pub,
            "dev_host_01",
            "tkt_1001",
            &input_hash,
            RevocationPolicy::Enforced {
                list: &rev_list,
                min_version: 1,
            },
            NOW + 120,
        ),
        Err(ProtocolError::Expired(_))
    ));

    // Tamper 4: revoked mobile device -> Revoked.
    let revoked_list = issue_revocation_list(
        &IDENTITY_SEED,
        2,
        NOW - 10,
        NOW + 3600,
        vec!["dev_mobile_01".to_string()],
    )
    .expect("revoked list");
    assert!(matches!(
        verify_approval_decision(
            &decision,
            &mobile_cert,
            &identity_pub,
            "dev_host_01",
            "tkt_1001",
            &input_hash,
            RevocationPolicy::Enforced {
                list: &revoked_list,
                min_version: 2,
            },
            NOW,
        ),
        Err(ProtocolError::Revoked(_))
    ));

    // Tamper 5: untrusted/forged IdentityKey -> BadSignature.
    let stranger_pub = ed25519_public_from_secret(&[0x99; 32]);
    assert!(matches!(
        verify_approval_decision(
            &decision,
            &mobile_cert,
            &stranger_pub,
            "dev_host_01",
            "tkt_1001",
            &input_hash,
            RevocationPolicy::NeverIssued { min_version: 0 },
            NOW,
        ),
        Err(ProtocolError::BadSignature(_))
    ));
}

#[test]
fn kat_device_certificate_enforces_30_day_ttl_and_revocation_list_monotonicity() {
    let (identity_pub, _host_cert, mobile_cert) = fixture_certs();

    let over_ttl = issue_device_certificate(
        &IDENTITY_SEED,
        "dev_over_ttl",
        DeviceKind::Mobile,
        "Too Long",
        &ed25519_public_from_secret(&MOBILE_ED_SEED),
        &x25519_public_from_secret(&MOBILE_X_SECRET),
        None,
        NOW,
        NOW + MAX_CERT_TTL_SECS + 1,
    );
    assert_eq!(over_ttl, Err(ProtocolError::CertTtlExceeded));

    let exact_30d = issue_device_certificate(
        &IDENTITY_SEED,
        "dev_exact_30d",
        DeviceKind::Mobile,
        "Exact 30d",
        &ed25519_public_from_secret(&MOBILE_ED_SEED),
        &x25519_public_from_secret(&MOBILE_X_SECRET),
        None,
        NOW,
        NOW + MAX_CERT_TTL_SECS,
    )
    .expect("30d cert");
    verify_device_certificate(
        &exact_30d,
        &identity_pub,
        RevocationPolicy::NeverIssued { min_version: 0 },
        NOW,
    )
    .expect("valid 30d");

    let list_v2 = issue_revocation_list(&IDENTITY_SEED, 2, NOW - 10, NOW + 600, vec![]).unwrap();
    verify_revocation_list(&list_v2, &identity_pub, 2, NOW).expect("v2 valid");
    assert_eq!(
        verify_revocation_list(&list_v2, &identity_pub, 3, NOW),
        Err(ProtocolError::RevocationRollback { got: 2, min: 3 })
    );

    assert!(matches!(
        verify_revocation_list(&list_v2, &identity_pub, 2, NOW + 600),
        Err(ProtocolError::Expired(_))
    ));

    let mut forged = list_v2.clone();
    forged.revoked_device_ids.push("dev_fake".to_string());
    assert!(matches!(
        verify_revocation_list(&forged, &identity_pub, 2, NOW),
        Err(ProtocolError::BadSignature(_))
    ));

    verify_device_certificate(
        &mobile_cert,
        &identity_pub,
        RevocationPolicy::Enforced {
            list: &list_v2,
            min_version: 2,
        },
        NOW,
    )
    .expect("mobile cert valid");
}

#[test]
fn kat_noise_ik_sliding_replay_window_accepts_in_window_reorder_and_rejects_duplicates_and_below_window(
) {
    let (identity_pub, host_cert, mobile_cert) = fixture_certs();
    let stream_id = "dev_host_01:42";
    let host_epoch = 42u64;

    let mut initiator = NoiseHandshake::new_initiator(
        &MOBILE_X_SECRET,
        &host_cert,
        &identity_pub,
        RevocationPolicy::NeverIssued { min_version: 0 },
        NOW,
        stream_id,
        host_epoch,
    )
    .expect("initiator");
    let mut responder = NoiseHandshake::new_responder(
        &HOST_X_SECRET,
        &mobile_cert,
        &identity_pub,
        RevocationPolicy::NeverIssued { min_version: 0 },
        NOW,
        stream_id,
        host_epoch,
    )
    .expect("responder");

    // Non-empty handshake payload MUST be rejected (Opus review #20 item 5).
    assert!(matches!(
        initiator.write_message(b"secret-in-msg1"),
        Err(ProtocolError::InvalidInput(_))
    ));

    // Empty handshake messages succeed, and EpochHandshakeReplayGuard drops duplicate msg1.
    let mut epoch_guard = EpochHandshakeReplayGuard::new(host_epoch, 64);
    let m1 = initiator.write_message(&[]).expect("m1");
    let p1 = responder
        .read_message_guarded(&m1, &mut epoch_guard)
        .expect("read m1");
    assert!(p1.is_empty());

    // Replaying identical msg1 in the same epoch is cheaply dropped before DH.
    let mut responder_replay = NoiseHandshake::new_responder(
        &HOST_X_SECRET,
        &mobile_cert,
        &identity_pub,
        RevocationPolicy::NeverIssued { min_version: 0 },
        NOW,
        stream_id,
        host_epoch,
    )
    .expect("responder_replay");
    assert!(matches!(
        responder_replay.read_message_guarded(&m1, &mut epoch_guard),
        Err(ProtocolError::ReplayDetected { .. })
    ));

    let m2 = responder.write_message(&[]).expect("m2");
    let p2 = initiator.read_message(&m2).expect("read m2");
    assert!(p2.is_empty());

    let mut client = initiator.into_transport().expect("client transport");
    let mut host = responder.into_transport().expect("host transport");
    assert_eq!(client.handshake_hash(), host.handshake_hash());

    // Generate 1026 frames (seq 1..=1026).
    let mut frames: Vec<Vec<u8>> = Vec::with_capacity(1026);
    for seq in 1..=1026u64 {
        frames.push(client.encrypt(format!("msg-{seq}").as_bytes()).unwrap());
    }

    // Cross-class reorder scenario from review #20: PTY frame seq 11 arrives BEFORE
    // approval frame seq 10 -> BOTH must be accepted once!
    assert_eq!(&*host.decrypt(&frames[10]).expect("seq 11"), b"msg-11");
    assert_eq!(host.recv_high_water(), 11);
    assert_eq!(&*host.decrypt(&frames[9]).expect("seq 10"), b"msg-10");
    assert_eq!(host.recv_high_water(), 11);

    // Replaying either seq 10 or seq 11 MUST be rejected as duplicate!
    assert_eq!(
        host.decrypt(&frames[9]),
        Err(ProtocolError::ReplayDetected {
            got: 10,
            high_water: 11
        })
    );
    assert_eq!(
        host.decrypt(&frames[10]),
        Err(ProtocolError::ReplayDetected {
            got: 11,
            high_water: 11
        })
    );

    // Forged ciphertext claiming high seq 5000 MUST fail AEAD and MUST NOT advance window!
    let mut forged = frames[10].clone();
    forged[..8].copy_from_slice(&5000u64.to_be_bytes());
    assert!(matches!(host.decrypt(&forged), Err(ProtocolError::Aead(_))));
    assert_eq!(host.recv_high_water(), 11);

    // Advance high-water to seq 1024 (index 1023).
    assert_eq!(
        &*host.decrypt(&frames[1023]).expect("seq 1024"),
        b"msg-1024"
    );
    assert_eq!(host.recv_high_water(), 1024);

    // Exact lower edge of 1024-frame window: seq 1 (delta = 1024 - 1 = 1023 < 1024) ACCEPTED!
    assert_eq!(REPLAY_WINDOW_SIZE, 1024);
    assert_eq!(
        &*host.decrypt(&frames[0]).expect("seq 1 in window"),
        b"msg-1"
    );
    // Duplicate of seq 1 at lower edge REJECTED!
    assert_eq!(
        host.decrypt(&frames[0]),
        Err(ProtocolError::ReplayDetected {
            got: 1,
            high_water: 1024
        })
    );

    // Advance high-water to seq 1026 (index 1025). Now seq 2 (delta = 1024 >= 1024) is BELOW window!
    assert_eq!(
        &*host.decrypt(&frames[1025]).expect("seq 1026"),
        b"msg-1026"
    );
    assert_eq!(
        host.decrypt(&frames[1]),
        Err(ProtocolError::ReplayDetected {
            got: 2,
            high_water: 1026
        })
    );
    // While seq 3 (delta = 1023 < 1024) is STILL inside the window and accepted once!
    assert_eq!(
        &*host.decrypt(&frames[2]).expect("seq 3 in window"),
        b"msg-3"
    );
    assert_eq!(
        host.decrypt(&frames[2]),
        Err(ProtocolError::ReplayDetected {
            got: 3,
            high_water: 1026
        })
    );

    // Reverse direction (host -> client) has independent window.
    let reply = host.encrypt(b"inbox-snapshot").expect("reply");
    assert_eq!(
        &*client.decrypt(&reply).expect("dec reply"),
        b"inbox-snapshot"
    );
    assert_eq!(
        client.decrypt(&reply),
        Err(ProtocolError::ReplayDetected {
            got: 1,
            high_water: 1
        })
    );
}

#[test]
fn kat_noise_ik_rejects_mismatched_host_epoch_and_unpinned_peer() {
    let (identity_pub, host_cert, mobile_cert) = fixture_certs();

    let mut init = NoiseHandshake::new_initiator(
        &MOBILE_X_SECRET,
        &host_cert,
        &identity_pub,
        RevocationPolicy::NeverIssued { min_version: 0 },
        NOW,
        "dev_host_01:42",
        42,
    )
    .unwrap();
    let mut resp = NoiseHandshake::new_responder(
        &HOST_X_SECRET,
        &mobile_cert,
        &identity_pub,
        RevocationPolicy::NeverIssued { min_version: 0 },
        NOW,
        "dev_host_01:43",
        43,
    )
    .unwrap();
    let m1 = init.write_message(&[]).unwrap();
    assert!(resp.read_message(&m1).is_err());

    let rogue_x_secret = [0xAA; 32];
    let mut rogue_init = NoiseHandshake::new_initiator(
        &rogue_x_secret,
        &host_cert,
        &identity_pub,
        RevocationPolicy::NeverIssued { min_version: 0 },
        NOW,
        "dev_host_01:42",
        42,
    )
    .unwrap();
    let mut resp_pinned = NoiseHandshake::new_responder(
        &HOST_X_SECRET,
        &mobile_cert,
        &identity_pub,
        RevocationPolicy::NeverIssued { min_version: 0 },
        NOW,
        "dev_host_01:42",
        42,
    )
    .unwrap();
    let rogue_m1 = rogue_init.write_message(&[]).unwrap();
    assert!(matches!(
        resp_pinned.read_message(&rogue_m1),
        Err(ProtocolError::BadSignature(_))
    ));
}

#[test]
fn kat_preview_ct_seal_open_deterministic_and_bounded() {
    let host_ed_pub = ed25519_public_from_secret(&HOST_ED_SEED);
    let mobile_x_pub = x25519_public_from_secret(&MOBILE_X_SECRET);
    let plaintext = br#"{"repo":"GitPulse","tool":"Bash","summary":"Run cargo test"}"#;

    let sealed_a = seal_preview_with_ephemeral(
        "dev_host_01",
        &HOST_ED_SEED,
        &mobile_x_pub,
        &EPHEMERAL_X_SECRET,
        plaintext,
    )
    .expect("sealed_a");
    let sealed_b = seal_preview_with_ephemeral(
        "dev_host_01",
        &HOST_ED_SEED,
        &mobile_x_pub,
        &EPHEMERAL_X_SECRET,
        plaintext,
    )
    .expect("sealed_b");
    assert_eq!(sealed_a, sealed_b);
    assert!(sealed_a.len() <= MAX_PREVIEW_CT_BYTES);
    assert_eq!(&sealed_a[..4], b"GPPV");

    let opened =
        open_preview(&sealed_a, &MOBILE_X_SECRET, &host_ed_pub, "dev_host_01").expect("opened");
    assert_eq!(&*opened, plaintext);

    let ffi_opened =
        uniffi_api::open_preview(&sealed_a, &MOBILE_X_SECRET, &host_ed_pub, "dev_host_01")
            .expect("ffi opened");
    assert_eq!(ffi_opened, plaintext);

    for byte_idx in [0, 4, 6, 40, sealed_a.len() - 70, sealed_a.len() - 1] {
        let mut mutated = sealed_a.clone();
        mutated[byte_idx] ^= 0x01;
        assert!(
            open_preview(&mutated, &MOBILE_X_SECRET, &host_ed_pub, "dev_host_01").is_err(),
            "byte {byte_idx} flip unexpectedly opened"
        );
    }

    assert!(matches!(
        open_preview(&sealed_a, &MOBILE_X_SECRET, &host_ed_pub, "dev_other"),
        Err(ProtocolError::BindingMismatch(_))
    ));

    let oversized = vec![b'A'; MAX_PREVIEW_CT_BYTES];
    assert!(matches!(
        seal_preview_with_ephemeral(
            "dev_host_01",
            &HOST_ED_SEED,
            &mobile_x_pub,
            &EPHEMERAL_X_SECRET,
            &oversized,
        ),
        Err(ProtocolError::TooLarge(_))
    ));
}

#[test]
fn kat_relay_http_ws_request_signing() {
    let host_ed_pub = ed25519_public_from_secret(&HOST_ED_SEED);
    let uri = "/v1/tunnel/ws?role=host&after_epoch=7&after_seq=120";
    let ts_ms = 1_800_000_000_123i64;
    let headers = sign_relay_request("dev_host_01", &HOST_ED_SEED, "GET", uri, ts_ms, b"")
        .expect("sign_relay_request");

    let ffi_headers =
        uniffi_api::sign_relay_request("dev_host_01", &HOST_ED_SEED, "GET", uri, ts_ms, b"")
            .expect("ffi sign");
    assert_eq!(headers, ffi_headers);

    verify_relay_request(&host_ed_pub, "GET", uri, ts_ms, b"", &headers.signature_b64)
        .expect("verify relay request");

    assert!(verify_relay_request(
        &host_ed_pub,
        "GET",
        "/v1/tunnel/ws?role=client&after_epoch=7&after_seq=120",
        ts_ms,
        b"",
        &headers.signature_b64,
    )
    .is_err());

    let frame = OuterFrame {
        v: GPTF_VERSION,
        frame_id: "frm_01".to_string(),
        stream_id: "dev_host_01:7".to_string(),
        seq: 121,
        from_device: "dev_host_01".to_string(),
        to_devices: vec!["dev_mobile_01".to_string()],
        ttl_sec: 60,
        class: FrameClass::Approval,
        ciphertext_b64: STANDARD.encode(b"opaque"),
    };
    let json_val = serde_json::to_value(&frame).expect("frame json");
    assert_eq!(json_val["v"], 1);
    assert_eq!(json_val["class"], "approval");
    assert_eq!(hex_encode(&[0x0a, 0xff]), "0aff");
}

#[test]
fn kat_pinned_golden_digest_and_uniffi_tunnel_roundtrip() {
    let digest = uniffi_api::hash_canonical("Bash", r#"{"cmd":"ls -la"}"#, "/repo").unwrap();
    let rendered =
        uniffi_api::render_canonical_tool_input("Bash", r#"{"cmd":"ls -la"}"#, "/repo").unwrap();
    assert_eq!(
        rendered,
        r#"{"cwd":"/repo","tool_input":{"cmd":"ls -la"},"tool_name":"Bash"}"#
    );
    assert_eq!(
        digest,
        "06c557413628a4c9ec2b0d368df041b2c5264e127cffc82a1d668661c7b59ab4"
    );

    let (identity_pub, host_cert, mobile_cert) = fixture_certs();
    let decision = sign_approval_decision_software(
        &MOBILE_P256_SECRET,
        "dev_host_01",
        "dev_host_01",
        "tkt_2",
        &digest,
        ApprovalDecisionKind::Deny,
        "n2",
        NOW - 1,
        NOW + 60,
    )
    .unwrap();
    assert!(matches!(
        verify_approval_decision(
            &decision,
            &host_cert,
            &identity_pub,
            "dev_host_01",
            "tkt_2",
            &digest,
            RevocationPolicy::NeverIssued { min_version: 0 },
            NOW,
        ),
        Err(ProtocolError::BindingMismatch(_))
    ));

    let host_cert_json = serde_json::to_string(&host_cert).unwrap();
    let mobile_cert_json = serde_json::to_string(&mobile_cert).unwrap();
    let mut init = uniffi_api::tunnel_initiator_new(
        &MOBILE_X_SECRET,
        &host_cert_json,
        &identity_pub,
        None,
        0,
        NOW,
        "dev_host_01:9",
        9,
    )
    .unwrap();
    let mut resp = uniffi_api::tunnel_responder_new(
        &HOST_X_SECRET,
        &mobile_cert_json,
        &identity_pub,
        None,
        0,
        NOW,
        "dev_host_01:9",
        9,
    )
    .unwrap();
    let m1 = init.write_message(&[]).unwrap();
    resp.read_message(&m1).unwrap();
    let m2 = resp.write_message(&[]).unwrap();
    init.read_message(&m2).unwrap();
    let mut client = init.into_transport().unwrap();
    let mut host = resp.into_transport().unwrap();

    let events = vec![
        ControlPlaneEvent::InboxSnapshot {
            host_id: "dev_host_01".to_string(),
            host_epoch: 9,
            seq: 1,
            sessions: vec![SessionSnapshot {
                session_key: "sess_1".to_string(),
                repo_path: "/repo".to_string(),
                repo_label: "repo".to_string(),
                worktree_path: "/repo".to_string(),
                branch: Some("main".to_string()),
                harness: "agy".to_string(),
                title: "Antigravity".to_string(),
                presence: "live".to_string(),
                fully_idle: true,
                provider_session_id: None,
                attention: None,
                attention_detail: None,
                capabilities: CapabilitySet {
                    harness: "agy".to_string(),
                    approval_channel: ApprovalChannel::NativeOnly,
                    supports_remote_approval: false,
                    supports_active_steering: true,
                    supports_idle_wake: false,
                    ungated: true,
                    queued_steering_ttl_sec: Some(7200),
                    pty_input_enabled: false,
                },
                active_subagents: vec![SubagentSummary {
                    agent_id: "sub_1".to_string(),
                    parent_session_id: "sess_1".to_string(),
                    kind: "agy".to_string(),
                    status: "running".to_string(),
                    label: Some("protocol-builder".to_string()),
                    started_at_ms: 1_800_000_000_000,
                    updated_at_ms: 1_800_000_005_000,
                }],
                pending_approval_ids: vec![],
                dirty_files: Some(3),
                updated_at_ms: 1_800_000_005_000,
            }],
            open_approvals: vec![ApprovalTicket {
                ticket_id: "tkt_1".to_string(),
                host_id: "dev_host_01".to_string(),
                session_id: "sess_claude".to_string(),
                dedup_key: format!("sess_claude:{digest}"),
                harness: "claude".to_string(),
                channel: ApprovalChannel::PresenceGatedHook,
                tool_name: "Bash".to_string(),
                tool_input: json!({"cmd": "ls -la"}),
                cwd: "/repo".to_string(),
                input_sha256: digest.clone(),
                summary: Some("Run ls -la".to_string()),
                repo_label: Some("repo".to_string()),
                issued_at: NOW,
                expires_at: NOW + 1800,
            }],
            generated_at_ms: 1_800_000_005_000,
        },
        ControlPlaneEvent::ApprovalResolved {
            resolved: ApprovalResolvedEvent {
                ticket_id: "tkt_1".to_string(),
                host_id: "dev_host_01".to_string(),
                outcome: ApprovalOutcome::NotApplied,
                applied_decision: None,
                resolved_by: Some("desktop".to_string()),
                reason: Some("user_returned_to_desk".to_string()),
                resolved_at: NOW + 10,
            },
        },
        ControlPlaneEvent::SteeringPreviewReq {
            req: SteeringPreviewReq {
                request_id: "req_1".to_string(),
                session_key: "sess_1".to_string(),
                text: "check tests".to_string(),
            },
        },
        ControlPlaneEvent::SteeringPreviewResp {
            resp: SteeringPreviewResp {
                request_id: "req_1".to_string(),
                session_key: "sess_1".to_string(),
                expect_route: "mailbox_queue".to_string(),
                delivery_mode: SteeringDeliveryMode::QueuedNextTurn,
                queued_ttl_sec: Some(7200),
                confirm_token: Some("tok_1".to_string()),
                warning: Some("Queued (next turn)".to_string()),
            },
        },
        ControlPlaneEvent::SteeringConfirmReq {
            req: SteeringConfirmReq {
                request_id: "req_1".to_string(),
                session_key: "sess_1".to_string(),
                expect_route: "mailbox_queue".to_string(),
                text: "check tests".to_string(),
                confirm_token: Some("tok_1".to_string()),
                signed: None,
            },
        },
        ControlPlaneEvent::SteeringConfirmResp {
            resp: SteeringConfirmResp {
                request_id: "req_1".to_string(),
                session_key: "sess_1".to_string(),
                status: SteeringStatus::Queued,
                route_used: Some("mailbox_queue".to_string()),
                expires_at: Some(NOW + 7200),
                reason: None,
            },
        },
        ControlPlaneEvent::CursorGap {
            current_epoch: 9,
            oldest_available_seq: 100,
            latest_seq: 356,
        },
    ];

    for event in events {
        let encoded = serde_json::to_vec(&event).unwrap();
        let ct = uniffi_api::tunnel_encrypt(&mut host, &encoded).unwrap();
        let pt = uniffi_api::tunnel_decrypt(&mut client, &ct).unwrap();
        let decoded: ControlPlaneEvent = serde_json::from_slice(&pt).unwrap();
        assert_eq!(decoded, event);
    }

    let wake = WakePushFrame {
        v: GPTF_VERSION,
        kind: WakePushKind::Approval,
        host_device: "dev_host_01".to_string(),
        stream_seq: 42,
        expires_at: NOW + 1800,
        preview_ct_b64: None,
    };
    let wake_val = serde_json::to_value(&wake).unwrap();
    assert_eq!(wake_val["v"], 1);
    assert_eq!(wake_val["kind"], "approval");
    assert!(wake_val.get("preview_ct_b64").is_none());
}

#[test]
fn kat_cross_language_vector_files_match_byte_for_byte() {
    // 1. SignedRevocationList vector (consumed by Go gitpulse-relay & mobile).
    let rev_raw = include_str!("vectors/revocation_list_v1.json");
    let rev_vec: Value = serde_json::from_str(rev_raw).expect("revocation vector json");
    let id_seed: [u8; 32] = hex_decode(rev_vec["identity_seed_hex"].as_str().unwrap()).unwrap();
    let id_pub: [u8; 32] = hex_decode(rev_vec["identity_pub_hex"].as_str().unwrap()).unwrap();
    assert_eq!(ed25519_public_from_secret(&id_seed), id_pub);

    let list: SignedRevocationList =
        serde_json::from_value(rev_vec["list"].clone()).expect("SignedRevocationList");
    let expected_preimage_hex = rev_vec["preimage_hex"].as_str().unwrap();
    let actual_preimage = revocation_signing_bytes(&list).unwrap();
    assert_eq!(hex_encode(&actual_preimage), expected_preimage_hex);

    let reissued = issue_revocation_list(
        &id_seed,
        list.version,
        list.issued_at,
        list.expires_at,
        list.revoked_device_ids.clone(),
    )
    .unwrap();
    assert_eq!(reissued, list);

    let valid_at = rev_vec["valid_at_sec"].as_i64().unwrap();
    verify_revocation_list(&list, &id_pub, list.version, valid_at).expect("vector rev valid");

    // 2. Relay HTTP/WS Ed25519 request-signing vector (consumed by Go gitpulse-relay & mobile).
    let req_raw = include_str!("vectors/relay_request_signing_v1.json");
    let req_vec: Value = serde_json::from_str(req_raw).expect("relay req vector json");
    let dev_seed: [u8; 32] = hex_decode(req_vec["ed25519_seed_hex"].as_str().unwrap()).unwrap();
    let dev_pub: [u8; 32] = hex_decode(req_vec["ed25519_pub_hex"].as_str().unwrap()).unwrap();
    assert_eq!(ed25519_public_from_secret(&dev_seed), dev_pub);

    let device_id = req_vec["device_id"].as_str().unwrap();
    let method = req_vec["method"].as_str().unwrap();
    let request_uri = req_vec["request_uri"].as_str().unwrap();
    let timestamp_ms = req_vec["timestamp_ms"].as_i64().unwrap();
    let body = req_vec["body_utf8"].as_str().unwrap().as_bytes();

    let preimage = relay_signing_message(method, request_uri, timestamp_ms, body).unwrap();
    assert_eq!(
        String::from_utf8(preimage.clone()).unwrap(),
        req_vec["preimage_utf8"].as_str().unwrap()
    );
    assert_eq!(
        hex_encode(&preimage),
        req_vec["preimage_hex"].as_str().unwrap()
    );

    let signed = sign_relay_request(
        device_id,
        &dev_seed,
        method,
        request_uri,
        timestamp_ms,
        body,
    )
    .unwrap();
    assert_eq!(
        signed.signature_b64,
        req_vec["signature_b64"].as_str().unwrap()
    );
    verify_relay_request(
        &dev_pub,
        method,
        request_uri,
        timestamp_ms,
        body,
        &signed.signature_b64,
    )
    .expect("relay req vector verify");

    // 3. Approval canonicalization & P-256 signing vector (consumed by mobile & daemon).
    let app_raw = include_str!("vectors/approval_canonical_v1.json");
    let app_vec: Value = serde_json::from_str(app_raw).expect("approval vector json");
    let tool_name = app_vec["tool_name"].as_str().unwrap();
    let tool_input = &app_vec["tool_input"];
    let cwd = app_vec["cwd"].as_str().unwrap();
    let canon_bytes = canonical_tool_input_bytes(tool_name, tool_input, cwd).unwrap();
    assert_eq!(
        String::from_utf8(canon_bytes).unwrap(),
        app_vec["canonical_tool_input_utf8"].as_str().unwrap()
    );
    let digest = input_sha256_hex(tool_name, tool_input, cwd).unwrap();
    assert_eq!(digest, app_vec["input_sha256"].as_str().unwrap());

    let decision_kind: ApprovalDecisionKind =
        serde_json::from_value(app_vec["decision"].clone()).unwrap();
    let signing_bytes = approval_signing_bytes(
        app_vec["host_id"].as_str().unwrap(),
        app_vec["ticket_id"].as_str().unwrap(),
        &digest,
        decision_kind,
        app_vec["nonce"].as_str().unwrap(),
        app_vec["issued_at"].as_i64().unwrap(),
        app_vec["expires_at"].as_i64().unwrap(),
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(signing_bytes.clone()).unwrap(),
        app_vec["approval_signing_preimage_utf8"].as_str().unwrap()
    );
    assert_eq!(
        hex_encode(&signing_bytes),
        app_vec["approval_signing_preimage_hex"].as_str().unwrap()
    );

    let p256_secret: [u8; 32] = hex_decode(app_vec["p256_secret_hex"].as_str().unwrap()).unwrap();
    assert_eq!(
        STANDARD.encode(p256_compressed_pub(&p256_secret)),
        app_vec["p256_pub_sec1_b64"].as_str().unwrap()
    );
    let signed_dec = sign_approval_decision_software(
        &p256_secret,
        app_vec["device_id"].as_str().unwrap(),
        app_vec["host_id"].as_str().unwrap(),
        app_vec["ticket_id"].as_str().unwrap(),
        &digest,
        decision_kind,
        app_vec["nonce"].as_str().unwrap(),
        app_vec["issued_at"].as_i64().unwrap(),
        app_vec["expires_at"].as_i64().unwrap(),
    )
    .unwrap();
    // RFC 6979 deterministic ECDSA-P256 matches vector signature byte-for-byte.
    assert_eq!(
        signed_dec.sig_p256_der_b64,
        app_vec["sig_p256_der_b64"].as_str().unwrap()
    );

    // 4. SignedSteeringConfirm vector (Opus review #23).
    let steer_raw = include_str!("vectors/steering_confirm_v1.json");
    let steer_vec: Value = serde_json::from_str(steer_raw).expect("steering vector json");
    let steer_text = steer_vec["text"].as_str().unwrap();
    let steer_text_hash = steering_text_sha256_hex(steer_text);
    assert_eq!(steer_text_hash, steer_vec["text_sha256"].as_str().unwrap());
    assert_eq!(uniffi_api::hash_steering_text(steer_text), steer_text_hash);

    let signed_steer: SignedSteeringConfirm =
        serde_json::from_value(steer_vec["signed"].clone()).expect("SignedSteeringConfirm");
    let steer_preimage = steering_signing_bytes(
        &signed_steer.host_id,
        &signed_steer.session_key,
        &signed_steer.request_id,
        &signed_steer.expect_route,
        &signed_steer.text_sha256,
        &signed_steer.nonce,
        signed_steer.issued_at,
        signed_steer.expires_at,
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(steer_preimage.clone()).unwrap(),
        steer_vec["steering_signing_preimage_utf8"]
            .as_str()
            .unwrap()
    );
    assert_eq!(
        hex_encode(&steer_preimage),
        steer_vec["steering_signing_preimage_hex"].as_str().unwrap()
    );

    let (identity_pub, _host_cert, mobile_cert) = fixture_certs();
    let rev_list = issue_revocation_list(&IDENTITY_SEED, 1, NOW - 30, NOW + 3600, vec![]).unwrap();
    verify_steering_confirm(
        &signed_steer,
        steer_text,
        &mobile_cert,
        &identity_pub,
        &signed_steer.host_id,
        &signed_steer.session_key,
        &signed_steer.request_id,
        &signed_steer.expect_route,
        RevocationPolicy::Enforced {
            list: &rev_list,
            min_version: 1,
        },
        NOW,
    )
    .expect("steering vector verify");

    // UniFFI verify_steering_confirm matches and enforces mandatory revocation list.
    let signed_steer_json = serde_json::to_string(&signed_steer).unwrap();
    let mobile_cert_json = serde_json::to_string(&mobile_cert).unwrap();
    let rev_json = serde_json::to_string(&rev_list).unwrap();
    uniffi_api::verify_steering_confirm(
        &signed_steer_json,
        steer_text,
        &mobile_cert_json,
        &identity_pub,
        &signed_steer.host_id,
        &signed_steer.session_key,
        &signed_steer.request_id,
        &signed_steer.expect_route,
        &rev_json,
        1,
        NOW,
    )
    .expect("ffi steering verify");

    // Text tamper (compromised JS substituting command) fails with BindingMismatch.
    assert!(matches!(
        verify_steering_confirm(
            &signed_steer,
            "rm -rf / && echo pwned",
            &mobile_cert,
            &identity_pub,
            &signed_steer.host_id,
            &signed_steer.session_key,
            &signed_steer.request_id,
            &signed_steer.expect_route,
            RevocationPolicy::Enforced {
                list: &rev_list,
                min_version: 1,
            },
            NOW,
        ),
        Err(ProtocolError::BindingMismatch(_))
    ));

    // Route tamper fails with BindingMismatch.
    assert!(matches!(
        verify_steering_confirm(
            &signed_steer,
            steer_text,
            &mobile_cert,
            &identity_pub,
            &signed_steer.host_id,
            &signed_steer.session_key,
            &signed_steer.request_id,
            "headless_resume",
            RevocationPolicy::Enforced {
                list: &rev_list,
                min_version: 1,
            },
            NOW,
        ),
        Err(ProtocolError::BindingMismatch(_))
    ));

    // Unknown field smuggling fails closed on deserialization.
    let mut smuggled = steer_vec["signed"].clone();
    smuggled
        .as_object_mut()
        .unwrap()
        .insert("extra".to_string(), json!(true));
    assert!(serde_json::from_value::<SignedSteeringConfirm>(smuggled).is_err());

    // Software signer reproduces vector signature byte-for-byte.
    let re_signed = sign_steering_confirm_software(
        &MOBILE_P256_SECRET,
        &signed_steer.device_id,
        &signed_steer.host_id,
        &signed_steer.session_key,
        &signed_steer.request_id,
        &signed_steer.expect_route,
        steer_text,
        &signed_steer.nonce,
        signed_steer.issued_at,
        signed_steer.expires_at,
    )
    .unwrap();
    assert_eq!(re_signed, signed_steer);
}
