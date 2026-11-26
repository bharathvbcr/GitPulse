//! The v4 shapes pinned against literal JSON that never changes: what a v4 host
//! sends. `PROTOCOL` moved to 5 (the `screen` notice channel), so the parity
//! test against `proto.rs` that held while it spoke protocol 4 was retired in
//! that bump, as it said it would be; these literals keep `compat_v4` honest.

use super::*;

fn bytes_of(header: &impl Serialize, payload: &[u8]) -> Vec<u8> {
    let mut wire = Vec::new();
    write_frame(&mut wire, header, payload).unwrap();
    wire
}

#[test]
fn the_requests_are_the_bytes_a_v4_host_was_built_to_read() {
    let cases: [(Request, u64, &str); 5] = [
        (
            Request::Hello {
                protocol: 4,
                build: "1.2.3".into(),
            },
            1,
            r#"{"id":1,"op":"hello","protocol":4,"build":"1.2.3"}"#,
        ),
        (Request::List, 2, r#"{"id":2,"op":"list"}"#),
        (
            Request::Kill {
                session: "term-1-a".into(),
            },
            3,
            r#"{"id":3,"op":"kill","session":"term-1-a"}"#,
        ),
        (
            Request::Transcript {
                session: "term-1-a".into(),
                max_bytes: 4096,
                redact: true,
                run_id: None,
            },
            4,
            r#"{"id":4,"op":"transcript","session":"term-1-a","max_bytes":4096,"redact":true}"#,
        ),
        (
            Request::Transcript {
                session: "term-1-a".into(),
                max_bytes: 10,
                redact: false,
                run_id: Some("run-7".into()),
            },
            5,
            r#"{"id":5,"op":"transcript","session":"term-1-a","max_bytes":10,"redact":false,"run_id":"run-7"}"#,
        ),
    ];
    for (request, id, literal) in cases {
        let envelope = Envelope { id, request };
        assert_eq!(serde_json::to_string(&envelope).unwrap(), literal);
        assert_eq!(decode::<Envelope>(literal.as_bytes()).unwrap(), envelope);
    }
}

#[test]
fn a_frame_is_a_length_a_header_a_length_and_a_payload() {
    let wire = bytes_of(
        &Envelope {
            id: 2,
            request: Request::List,
        },
        b"ab",
    );
    let header = br#"{"id":2,"op":"list"}"#;
    let mut expected = (header.len() as u32).to_be_bytes().to_vec();
    expected.extend_from_slice(header);
    expected.extend_from_slice(&2u32.to_be_bytes());
    expected.extend_from_slice(b"ab");
    assert_eq!(wire, expected);
}

#[test]
fn the_replies_a_v4_host_sends_decode_and_everything_else_is_skipped() {
    let ok = decode::<HostFrame>(br#"{"kind":"reply","id":2,"ok":[]}"#).unwrap();
    assert_eq!(
        ok,
        HostFrame::Reply {
            id: 2,
            ok: Some(serde_json::json!([])),
            error: None
        }
    );
    let refused = decode::<HostFrame>(
        br#"{"kind":"reply","id":1,"error":"Another GitPulse window is connected to this terminal host."}"#,
    )
    .unwrap();
    assert_eq!(
        refused,
        HostFrame::Reply {
            id: 1,
            ok: None,
            error: Some("Another GitPulse window is connected to this terminal host.".into())
        }
    );
    for literal in [
        r#"{"kind":"output","session":"term-1-a"}"#,
        r#"{"kind":"end","session":"term-1-a"}"#,
        r#"{"kind":"loss","loss":{"id":"term-1-a","message":"m"}}"#,
        r#"{"kind":"scan_drops","dropped":3}"#,
        r#"{"kind":"exit","ended":{"payload":{"id":"term-1-a","exit_code":0,"signal":"","error":null,"reaped":true,"ended_by_gitpulse":false},"ended_by_gitpulse":false,"program":"sh","cwd":"/","run_id":null}}"#,
        r#"{"kind":"a_kind_a_later_host_invents"}"#,
    ] {
        assert_eq!(
            decode::<HostFrame>(literal.as_bytes()).unwrap(),
            HostFrame::Other,
            "{literal}"
        );
    }
    assert!(decode::<HostFrame>(b"not json").is_err());
    assert!(
        decode::<HostFrame>(br#"{"id":1}"#).is_err(),
        "no kind is not a frame"
    );
}

#[test]
fn a_listing_and_a_transcript_decode_from_what_a_v4_host_wrote() {
    let listing = br#"{"id":"term-1-a","shell":"claude","cwd":"/w","repo":"/r","launcher":"claude","run_id":null,"detached":true,"ending":false,"quiet_ms":5}"#;
    assert_eq!(
        decode::<Listing>(listing).unwrap(),
        Listing {
            id: "term-1-a".into(),
            shell: "claude".into(),
            cwd: "/w".into(),
            repo: "/r".into(),
            launcher: Some("claude".into()),
            run_id: None,
            detached: true,
            ending: false,
            parent_session_id: None,
            title: None,
            quiet_ms: Some(5),
        }
    );
    // A v4 host older than its own `ending` and `quiet_ms` leaves them out.
    let bare = br#"{"id":"t","shell":"sh","cwd":"/","repo":"/","launcher":null,"run_id":null,"detached":false}"#;
    let bare = decode::<Listing>(bare).unwrap();
    assert!(!bare.ending);
    assert_eq!(bare.quiet_ms, None);

    let found = br#"{"state":"found","data_b64":"aGk=","bytes":2,"retained":2,"total_bytes":2,"truncated":false,"rotated":false,"clipped":false,"gap_markers":0,"unrecorded_bytes":0,"redacted":false,"live":false,"settled":true,"recording_error":null,"run_id":"run-7"}"#;
    let TranscriptRead::Found(tail) = decode(found).unwrap() else {
        panic!("not a found answer");
    };
    assert_eq!(
        (tail.data_b64.as_str(), tail.bytes, tail.total_bytes),
        ("aGk=", 2, 2)
    );
    assert_eq!(tail.run_id.as_deref(), Some("run-7"));
    for (literal, expected) in [
        (r#"{"state":"missing"}"#, TranscriptRead::Missing),
        (
            r#"{"state":"unreadable","reason":"denied"}"#,
            TranscriptRead::Unreadable {
                reason: "denied".into(),
            },
        ),
        (
            r#"{"state":"reused","reason":"r"}"#,
            TranscriptRead::Reused { reason: "r".into() },
        ),
        (
            r#"{"state":"unavailable","reason":"u"}"#,
            TranscriptRead::Unavailable { reason: "u".into() },
        ),
    ] {
        assert_eq!(
            decode::<TranscriptRead>(literal.as_bytes()).unwrap(),
            expected
        );
        assert_eq!(serde_json::to_string(&expected).unwrap(), literal);
    }
}

#[test]
fn a_cut_or_oversized_frame_is_an_error_not_a_frame() {
    let wire = bytes_of(
        &Envelope {
            id: 1,
            request: Request::List,
        },
        b"xyz",
    );
    for cut in 1..wire.len() {
        assert!(read_frame(&mut &wire[..cut]).is_err(), "cut at {cut}");
    }
    let mut huge = ((MAX_HEADER_BYTES + 1) as u32).to_be_bytes().to_vec();
    huge.extend_from_slice(&[0; 8]);
    assert!(read_frame(&mut &huge[..]).is_err());
    assert!(read_frame(&mut &0u32.to_be_bytes()[..]).is_err());
    let mut fat = 2u32.to_be_bytes().to_vec();
    fat.extend_from_slice(b"{}");
    fat.extend_from_slice(&((MAX_PAYLOAD_BYTES + 1) as u32).to_be_bytes());
    assert!(read_frame(&mut &fat[..]).is_err());
    assert!(write_frame(&mut Vec::new(), &1, &vec![0; MAX_PAYLOAD_BYTES + 1]).is_err());
    assert!(read_frame(&mut &[][..]).unwrap().is_none(), "a clean end");
}
