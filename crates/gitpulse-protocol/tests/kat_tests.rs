//! Deterministic Known-Answer Tests (KAT) and adversarial security tests for
//! `gitpulse-protocol`.
//!
//! Guards identical cryptographic behavior across Desktop, `gitpulsed --relay`,
//! `gitpulse-relay` (Go), iOS Swift/UniFFI, and Android Kotlin/UniFFI.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use gitpulse_protocol::canonical::{
    approval_signing_bytes, canonical_json_string, canonical_tool_input_bytes, hex_encode,
    input_sha256_hex,
};
use gitpulse_protocol::crypto::{
    ed25519_public_from_secret, issue_device_certificate, issue_revocation_list, open_preview,
    seal_preview_with_ephemeral, sign_approval_decision_software, sign_relay_request,
    verify_approval_decision, verify_device_certificate, verify_relay_request,
    verify_revocation_list, x25519_public_from_secret, NoiseHandshake,
};
use gitpulse_protocol::uniffi_api;
use gitpulse_protocol::wire::{
    ApprovalDecisionKind, DeviceKind, FrameClass, OuterFrame, SignedApprovalDecision, GPTF_VERSION,
    MAX_CERT_TTL_SECS, MAX_PREVIEW_CT_BYTES,
};
use gitpulse_protocol::ProtocolError;
use p256::ecdsa::SigningKey as P256SigningKey;
use serde_json::json;

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

    // UniFFI surface produces identical hash.
    let ffi_hash =
        uniffi_api::hash_canonical("Bash", &input_b.to_string(), "/Users/dev/repo").expect("ffi");
    assert_eq!(hash_a, ffi_hash);

    // Any change to cwd or tool_name alters the digest.
    let diff_cwd = input_sha256_hex("Bash", &input_a, "/Users/dev/other").expect("diff_cwd");
    assert_ne!(hash_a, diff_cwd);
}

#[test]
fn kat_approval_decision_rejects_updated_input_and_updated_permissions() {
    // Opus P0-4: updatedInput / updatedPermissions MUST fail closed at deserialization.
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

    // Invalid decision enum value (e.g. allow_always) is rejected.
    let bad_kind = json!("allow_always");
    assert!(serde_json::from_value::<ApprovalDecisionKind>(bad_kind).is_err());
}

#[test]
fn kat_p256_approval_verify_and_tamper_rejection() {
    let (identity_pub, _host_cert, mobile_cert) = fixture_certs();
    let input_hash = input_sha256_hex(
        "Bash",
        &json!({ "command": "cargo test" }),
        "/Users/dev/GitPulse",
    )
    .expect("input hash");

    // Verify exact signing preimage format.
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

    // Valid decision verifies through both Rust and UniFFI APIs.
    let verdict = verify_approval_decision(
        &decision,
        &mobile_cert,
        &identity_pub,
        "dev_host_01",
        "tkt_1001",
        &input_hash,
        Some(&rev_list),
        1,
        NOW,
    )
    .expect("valid approval");
    assert_eq!(verdict, ApprovalDecisionKind::AllowOnce);

    let ffi_verdict = uniffi_api::verify_approval_decision(
        &serde_json::to_string(&decision).expect("decision json"),
        &serde_json::to_string(&mobile_cert).expect("cert json"),
        &identity_pub,
        "dev_host_01",
        "tkt_1001",
        &input_hash,
        Some(&serde_json::to_string(&rev_list).expect("rev json")),
        1,
        NOW,
    )
    .expect("ffi verify");
    assert_eq!(ffi_verdict, ApprovalDecisionKind::AllowOnce);

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
            Some(&rev_list),
            1,
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
            Some(&rev_list),
            1,
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
            Some(&rev_list),
            1,
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
            Some(&revoked_list),
            2,
            NOW,
        ),
        Err(ProtocolError::Revoked(_))
    ));

    // Tamper 5: untrusted/forged IdentityKey -> BadSignature on DeviceCertificate.
    let stranger_pub = ed25519_public_from_secret(&[0x99; 32]);
    assert!(matches!(
        verify_approval_decision(
            &decision,
            &mobile_cert,
            &stranger_pub,
            "dev_host_01",
            "tkt_1001",
            &input_hash,
            None,
            0,
            NOW,
        ),
        Err(ProtocolError::BadSignature(_))
    ));
}

#[test]
fn kat_device_certificate_enforces_30_day_ttl_and_revocation_list_monotonicity() {
    let (identity_pub, _host_cert, mobile_cert) = fixture_certs();

    // Cert TTL > 30 days refused at issue.
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

    // Exactly 30 days succeeds.
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
    verify_device_certificate(&exact_30d, &identity_pub, None, 0, NOW).expect("valid 30d");

    // Revocation list rollback rejected (Opus R3).
    let list_v2 = issue_revocation_list(&IDENTITY_SEED, 2, NOW - 10, NOW + 600, vec![]).unwrap();
    verify_revocation_list(&list_v2, &identity_pub, 2, NOW).expect("v2 valid");
    assert_eq!(
        verify_revocation_list(&list_v2, &identity_pub, 3, NOW),
        Err(ProtocolError::RevocationRollback { got: 2, min: 3 })
    );

    // Expired revocation list rejected so host fails closed.
    assert!(matches!(
        verify_revocation_list(&list_v2, &identity_pub, 2, NOW + 600),
        Err(ProtocolError::Expired(_))
    ));

    // Tampered revocation list rejected.
    let mut forged = list_v2.clone();
    forged.revoked_device_ids.push("dev_fake".to_string());
    assert!(matches!(
        verify_revocation_list(&forged, &identity_pub, 2, NOW),
        Err(ProtocolError::BadSignature(_))
    ));

    // Valid mobile cert passes when revocation list is fresh and does not name it.
    verify_device_certificate(&mobile_cert, &identity_pub, Some(&list_v2), 2, NOW)
        .expect("mobile cert valid");
}

#[test]
fn kat_noise_ik_handshake_transport_and_replay_reorder_rejection() {
    let (identity_pub, host_cert, mobile_cert) = fixture_certs();
    let stream_id = "dev_host_01:42";
    let host_epoch = 42u64;

    let mut initiator = NoiseHandshake::new_initiator(
        &MOBILE_X_SECRET,
        &host_cert,
        &identity_pub,
        None,
        0,
        NOW,
        stream_id,
        host_epoch,
    )
    .expect("initiator");
    let mut responder = NoiseHandshake::new_responder(
        &HOST_X_SECRET,
        &mobile_cert,
        &identity_pub,
        None,
        0,
        NOW,
        stream_id,
        host_epoch,
    )
    .expect("responder");

    // Message 1: Initiator -> Responder (-> e, es, s, ss)
    let m1 = initiator.write_message(b"hello-host").expect("m1");
    let p1 = responder.read_message(&m1).expect("read m1");
    assert_eq!(p1, b"hello-host");

    // Message 2: Responder -> Initiator (<- e, ee, se)
    let m2 = responder.write_message(b"hello-mobile").expect("m2");
    let p2 = initiator.read_message(&m2).expect("read m2");
    assert_eq!(p2, b"hello-mobile");

    assert!(initiator.is_handshake_finished());
    assert!(responder.is_handshake_finished());

    let mut client = initiator.into_transport().expect("client transport");
    let mut host = responder.into_transport().expect("host transport");
    assert_eq!(client.handshake_hash(), host.handshake_hash());

    // Transport round-trip both directions.
    let ct1 = client.encrypt(b"approval-req-1").expect("ct1");
    let ct2 = client.encrypt(b"approval-req-2").expect("ct2");
    let ct3 = client.encrypt(b"approval-req-3").expect("ct3");

    // Deliver frame 1 then frame 3 (gap allowed, advances high-water to 3).
    assert_eq!(&*host.decrypt(&ct1).expect("dec1"), b"approval-req-1");
    assert_eq!(host.recv_high_water(), 1);
    assert_eq!(&*host.decrypt(&ct3).expect("dec3"), b"approval-req-3");
    assert_eq!(host.recv_high_water(), 3);

    // Reordered frame 2 (seq 2 <= high-water 3) MUST be rejected!
    assert_eq!(
        host.decrypt(&ct2),
        Err(ProtocolError::ReplayDetected {
            got: 2,
            high_water: 3
        })
    );

    // Replayed frame 3 MUST be rejected!
    assert_eq!(
        host.decrypt(&ct3),
        Err(ProtocolError::ReplayDetected {
            got: 3,
            high_water: 3
        })
    );

    // Tampered seq prefix with higher seq fails AEAD and MUST NOT advance high-water!
    let mut forged = ct3.clone();
    forged[..8].copy_from_slice(&99u64.to_be_bytes());
    assert!(matches!(host.decrypt(&forged), Err(ProtocolError::Aead(_))));
    assert_eq!(host.recv_high_water(), 3);

    // Host -> Client direction has independent monotonic counter.
    let reply = host.encrypt(b"inbox-snapshot").expect("reply");
    assert_eq!(
        &*client.decrypt(&reply).expect("dec reply"),
        b"inbox-snapshot"
    );
    assert_eq!(client.recv_high_water(), 1);
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

    // Epoch mismatch (42 vs 43) fails Noise handshake via prologue binding.
    let mut init = NoiseHandshake::new_initiator(
        &MOBILE_X_SECRET,
        &host_cert,
        &identity_pub,
        None,
        0,
        NOW,
        "dev_host_01:42",
        42,
    )
    .unwrap();
    let mut resp = NoiseHandshake::new_responder(
        &HOST_X_SECRET,
        &mobile_cert,
        &identity_pub,
        None,
        0,
        NOW,
        "dev_host_01:43",
        43,
    )
    .unwrap();
    let m1 = init.write_message(b"").unwrap();
    assert!(resp.read_message(&m1).is_err());

    // Rogue initiator using unpinned X25519 key is rejected by responder.
    let rogue_x_secret = [0xAA; 32];
    let mut rogue_init = NoiseHandshake::new_initiator(
        &rogue_x_secret,
        &host_cert,
        &identity_pub,
        None,
        0,
        NOW,
        "dev_host_01:42",
        42,
    )
    .unwrap();
    let mut resp_pinned = NoiseHandshake::new_responder(
        &HOST_X_SECRET,
        &mobile_cert,
        &identity_pub,
        None,
        0,
        NOW,
        "dev_host_01:42",
        42,
    )
    .unwrap();
    let rogue_m1 = rogue_init.write_message(b"").unwrap();
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

    // Opens cleanly via Rust and UniFFI API.
    let opened =
        open_preview(&sealed_a, &MOBILE_X_SECRET, &host_ed_pub, "dev_host_01").expect("opened");
    assert_eq!(&*opened, plaintext);

    let ffi_opened =
        uniffi_api::open_preview(&sealed_a, &MOBILE_X_SECRET, &host_ed_pub, "dev_host_01")
            .expect("ffi opened");
    assert_eq!(ffi_opened, plaintext);

    // Every single bit flip is rejected.
    for byte_idx in [0, 4, 6, 40, sealed_a.len() - 70, sealed_a.len() - 1] {
        let mut mutated = sealed_a.clone();
        mutated[byte_idx] ^= 0x01;
        assert!(
            open_preview(&mutated, &MOBILE_X_SECRET, &host_ed_pub, "dev_host_01").is_err(),
            "byte {byte_idx} flip unexpectedly opened"
        );
    }

    // Wrong expected sender device id rejected.
    assert!(matches!(
        open_preview(&sealed_a, &MOBILE_X_SECRET, &host_ed_pub, "dev_other"),
        Err(ProtocolError::BindingMismatch(_))
    ));

    // Oversized preview (> 2 KiB total) rejected.
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

    // Deterministic signature matches UniFFI wrapper.
    let ffi_headers =
        uniffi_api::sign_relay_request("dev_host_01", &HOST_ED_SEED, "GET", uri, ts_ms, b"")
            .expect("ffi sign");
    assert_eq!(headers, ffi_headers);

    verify_relay_request(&host_ed_pub, "GET", uri, ts_ms, b"", &headers.signature_b64)
        .expect("verify relay request");

    // URI query tamper fails verification.
    assert!(verify_relay_request(
        &host_ed_pub,
        "GET",
        "/v1/tunnel/ws?role=client&after_epoch=7&after_seq=120",
        ts_ms,
        b"",
        &headers.signature_b64,
    )
    .is_err());

    // GPTF OuterFrame round-trip JSON contract check.
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
    use gitpulse_protocol::wire::{
        ApprovalChannel, ApprovalOutcome, ApprovalResolvedEvent, ApprovalTicket, CapabilitySet,
        ControlPlaneEvent, SessionSnapshot, SteeringConfirmReq, SteeringConfirmResp,
        SteeringDeliveryMode, SteeringPreviewReq, SteeringPreviewResp, SteeringStatus,
        SubagentSummary, WakePushFrame, WakePushKind,
    };

    // Frozen golden SHA-256 of canonical {"cwd":"/repo","tool_input":{"cmd":"ls -la"},"tool_name":"Bash"}.
    // Computed independently with SHA-256 to guard against cross-platform drift.
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

    // Cert without P-256 key (desktop cert) MUST NOT be allowed to verify an approval decision.
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
            None,
            0,
            NOW,
        ),
        Err(ProtocolError::BindingMismatch(_))
    ));

    // Full UniFFI Noise IK handshake + tunnel_encrypt / tunnel_decrypt.
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
    let m1 = init.write_message(b"").unwrap();
    resp.read_message(&m1).unwrap();
    let m2 = resp.write_message(b"").unwrap();
    init.read_message(&m2).unwrap();
    let mut client = init.into_transport().unwrap();
    let mut host = resp.into_transport().unwrap();

    // Serialize every key ControlPlaneEvent variant through Noise IK transport.
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

    // WakePushFrame wire shape check.
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
