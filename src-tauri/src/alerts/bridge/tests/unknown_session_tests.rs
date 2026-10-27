//! A report for a session the terminal registry does not hold is refused at
//! the socket, with its own reason, and counted.
use super::*;
use std::io::{Read, Write};
use std::sync::Arc;

/// A host whose registry holds nothing: every key is unknown.
struct EmptyRegistry;

impl crate::alerts::Host for EmptyRegistry {
    fn submit(&self, _: &str, _: &str, _: &str, _: bool) -> Result<bool, String> {
        Ok(true)
    }
    fn attended(&self, _: &str) -> bool {
        false
    }
    fn knows(&self, _: &str) -> bool {
        false
    }
}

fn through_the_socket(report: &[u8]) -> (Outcome, String) {
    let counters = Arc::new(crate::alerts::Counters::default());
    let (intake, _receiver) = crate::alerts::Intake::channel(counters);
    let intake = intake.guarded_by(Arc::new(EmptyRegistry));
    let (mut client, server) = std::os::unix::net::UnixStream::pair().unwrap();
    client.write_all(report).unwrap();
    client.shutdown(std::net::Shutdown::Write).unwrap();
    let outcome = handle_into(
        server,
        std::time::Duration::from_secs(5),
        &mut |notice, seq| intake.offer_stamped(notice, order_for(seq, order_stamp_ns())),
    );
    let mut reply = String::new();
    client.read_to_string(&mut reply).unwrap();
    (outcome, reply)
}

#[test]
fn prompt_submitted_for_an_unknown_session_is_refused_and_counted() {
    // The counter lives on the global hub; start it if nothing else has. The
    // refusal below comes from the intake this test builds around its own
    // `EmptyRegistry`, not from the hub's host, and a hub started with that
    // host would refuse every other test's session-keyed report.
    start_test_hub();
    let before = crate::alerts::status().bridge_rejected;
    let (outcome, reply) = through_the_socket(
        br#"{"v":1,"event":"prompt_submitted","agent":"claude","session":"term-404-dead"}"#,
    );
    assert_eq!(outcome, Outcome::Refused(Reason::UnknownSession));
    assert_eq!(reply.trim(), r#"{"ok":false,"reason":"unknown_session"}"#);
    assert!(
        crate::alerts::status().bridge_rejected > before,
        "the refusal was not counted as bridge_rejected"
    );
}

/// A report from outside any terminal names no session, so it is not asked of
/// the registry: the reply is the queue's, not `unknown_session`.
#[test]
fn a_report_from_outside_a_terminal_is_not_asked_of_the_registry() {
    let (outcome, _) = through_the_socket(
        br#"{"v":1,"event":"permission_prompt","agent":"claude","cwd":"/work/proj"}"#,
    );
    assert_ne!(outcome, Outcome::Refused(Reason::UnknownSession));
}
