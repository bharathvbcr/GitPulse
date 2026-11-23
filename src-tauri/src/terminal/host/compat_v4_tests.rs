//! The v4 shapes pinned twice: against literal JSON that never changes, and
//! against `proto.rs` while it still speaks protocol 4.
//!
//! The literals are the permanent proof. When `PROTOCOL` moves to 5, `proto.rs`
//! is free to change every frame and the parity test below then fails on its
//! first assertion, on purpose: delete it in the bump, and these literals keep
//! `compat_v4` honest about what a v4 host sends.

use super::*;
use crate::terminal::host::proto;

fn bytes_of(header: &impl Serialize, payload: &[u8]) -> Vec<u8> {
    let mut wire = Vec::new();
    write_frame(&mut wire, header, payload).unwrap();
    wire
}

fn proto_bytes_of(header: &impl Serialize, payload: &[u8]) -> Vec<u8> {
    let mut wire = Vec::new();
    proto::write_frame(&mut wire, header, payload).unwrap();
    wire
}

fn header_of(wire: &[u8]) -> Vec<u8> {
    read_frame(&mut &wire[..]).unwrap().unwrap().0
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

/// Every shape in the subset, serialized by `proto.rs` and by this copy: the
/// same bytes. While `PROTOCOL` is 4 this is the proof that the copy is the
/// protocol; when it moves, this test is retired (see the module comment).
#[test]
fn compat_v4_and_proto_agree_on_every_frame_while_proto_is_v4() {
    assert_eq!(
        proto::PROTOCOL,
        PROTOCOL,
        "proto.rs moved past protocol 4: v4 parity against it no longer applies. Delete this \
         test; the literal tests above keep compat_v4 pinned to what a v4 host sends"
    );

    // Requests, byte for byte, frame and all.
    let requests = [
        (
            proto::Request::Hello {
                protocol: 4,
                build: "b".into(),
            },
            Request::Hello {
                protocol: 4,
                build: "b".into(),
            },
        ),
        (proto::Request::List, Request::List),
        (
            proto::Request::Kill {
                session: "s".into(),
            },
            Request::Kill {
                session: "s".into(),
            },
        ),
        (
            proto::Request::Transcript {
                session: "s".into(),
                max_bytes: 9,
                redact: true,
                run_id: Some("r".into()),
            },
            Request::Transcript {
                session: "s".into(),
                max_bytes: 9,
                redact: true,
                run_id: Some("r".into()),
            },
        ),
        (
            proto::Request::Transcript {
                session: "s".into(),
                max_bytes: 9,
                redact: false,
                run_id: None,
            },
            Request::Transcript {
                session: "s".into(),
                max_bytes: 9,
                redact: false,
                run_id: None,
            },
        ),
    ];
    for (id, (now, then)) in requests.into_iter().enumerate() {
        let id = id as u64 + 1;
        let from_proto = proto_bytes_of(&proto::Envelope { id, request: now }, b"");
        let from_compat = bytes_of(&Envelope { id, request: then }, b"");
        assert_eq!(from_proto, from_compat);
        // And each reads the other's.
        let header = header_of(&from_proto);
        assert!(decode::<Envelope>(&header).is_ok());
        assert!(proto::decode::<proto::Envelope>(&header_of(&from_compat)).is_ok());
    }

    // The listing: serialized by the live type, read by the copy, and back.
    let live = crate::terminal::TerminalListing {
        id: "term-1-a".into(),
        shell: "claude".into(),
        cwd: "/w".into(),
        repo: "/r".into(),
        launcher: Some("claude".into()),
        run_id: Some("run-1".into()),
        detached: true,
        ending: true,
        parent_session_id: Some("term-0-a".into()),
        title: Some("t".into()),
        quiet_ms: Some(7),
        protected: false,
    };
    let live_json = serde_json::to_vec(&live).unwrap();
    let frozen: Listing = decode(&live_json).unwrap();
    assert_eq!(serde_json::to_vec(&frozen).unwrap(), live_json);
    let bare = crate::terminal::TerminalListing {
        parent_session_id: None,
        title: None,
        quiet_ms: None,
        protected: false,
        ..live.clone()
    };
    let bare_json = serde_json::to_vec(&bare).unwrap();
    assert_eq!(
        serde_json::to_vec(&decode::<Listing>(&bare_json).unwrap()).unwrap(),
        bare_json
    );

    // Replies carrying it, in both directions.
    let reply = proto::HostFrame::Reply {
        id: 2,
        ok: Some(serde_json::to_value(vec![&live]).unwrap()),
        error: None,
    };
    let reply_bytes = proto_bytes_of(&reply, b"");
    let HostFrame::Reply { id, ok, error } = decode(&header_of(&reply_bytes)).unwrap() else {
        panic!("a v4 reply was not read as one");
    };
    assert_eq!((id, error), (2, None));
    assert_eq!(
        bytes_of(
            &HostFrame::Reply {
                id: 2,
                ok,
                error: None
            },
            b""
        ),
        reply_bytes
    );
    let refusal = HostFrame::Reply {
        id: 1,
        ok: None,
        error: Some("no".into()),
    };
    assert_eq!(
        proto::decode::<proto::HostFrame>(&header_of(&bytes_of(&refusal, b""))).unwrap(),
        proto::HostFrame::Reply {
            id: 1,
            ok: None,
            error: Some("no".into())
        }
    );

    // Hello's answer.
    let hello = proto::Hello {
        protocol: 4,
        build: "b".into(),
        pid: 9,
        sessions: 2,
    };
    let hello_json = serde_json::to_vec(&hello).unwrap();
    assert_eq!(
        serde_json::to_vec(&decode::<Hello>(&hello_json).unwrap()).unwrap(),
        hello_json
    );

    // Every transcript answer, both ways.
    use crate::terminal::{TranscriptRead as Live, TranscriptTail as LiveTail};
    let found = Live::Found(LiveTail {
        data_b64: "aGk=".into(),
        bytes: 2,
        retained: 2,
        total_bytes: 2,
        truncated: true,
        rotated: true,
        clipped: true,
        gap_markers: 1,
        unrecorded_bytes: 3,
        redacted: true,
        live: true,
        settled: false,
        recording_error: Some("e".into()),
        run_id: Some("r".into()),
    });
    for answer in [
        found,
        Live::Missing,
        Live::Unreadable { reason: "u".into() },
        Live::Reused { reason: "r".into() },
        Live::Unavailable { reason: "x".into() },
    ] {
        let live_json = serde_json::to_vec(&answer).unwrap();
        let frozen: TranscriptRead = decode(&live_json).unwrap();
        assert_eq!(serde_json::to_vec(&frozen).unwrap(), live_json);
        assert_eq!(
            serde_json::from_slice::<Live>(&serde_json::to_vec(&frozen).unwrap()).unwrap(),
            answer
        );
    }

    // Every other frame a v4 host sends is skipped, not refused.
    let others = [
        proto::HostFrame::Output {
            session: "s".into(),
        },
        proto::HostFrame::End {
            session: "s".into(),
        },
        proto::HostFrame::Loss {
            loss: crate::terminal::TerminalLossPayload {
                id: "s".into(),
                message: "m".into(),
            },
        },
        proto::HostFrame::ScanDrops { dropped: 1 },
        proto::HostFrame::Exit {
            ended: proto::WireEnded {
                payload: crate::terminal::TerminalExitPayload {
                    id: "s".into(),
                    exit_code: Some(0),
                    signal: String::new(),
                    error: None,
                    reaped: true,
                    ended_by_gitpulse: false,
                },
                ended_by_gitpulse: false,
                program: "sh".into(),
                cwd: "/".into(),
                run_id: None,
            },
        },
    ];
    for frame in others {
        assert_eq!(
            decode::<HostFrame>(&header_of(&proto_bytes_of(&frame, b"xy"))).unwrap(),
            HostFrame::Other
        );
    }

    // The bounds are the same ones.
    assert_eq!(MAX_HEADER_BYTES, proto::MAX_HEADER_BYTES);
    assert_eq!(MAX_PAYLOAD_BYTES, proto::MAX_PAYLOAD_BYTES);
    assert_eq!(
        MAX_TRANSCRIPT_TAIL_BYTES,
        crate::terminal::MAX_TRANSCRIPT_TAIL_BYTES
    );
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
