//! Regression guards from the adversarial pass on ft-e7c644de (lane L4,
//! `l4-load-flakes`). Each holds a claim the lane's own comments make to the
//! facts, and passes now that the claim is true.
use super::*;
use std::time::Duration;

/// `own_listener()` empties the shared outside bucket so that "a probe that names
/// no session is refused as `rate_limited` here every time, not once in a few
/// hundred runs under load" (the comment on `drain_outside_bucket`). The bucket
/// regains 4 tokens a second (`limits::OUTSIDE_PER_SEC`), so before the drain
/// held the refill off the claim held only for a probe sent within a quarter of
/// a second of it, and a leftover session-less probe in a test that waits first
/// (`an_idle_accept_loop_does_not_wake` sends its probe after 2 s) or retries
/// (`a_flood_of_connections_...`'s `within(..)`) was admitted. This probe is
/// sent 600 ms after the drain and must still be refused.
#[test]
fn adv_l4_a_session_less_probe_a_moment_after_the_drain_is_still_refused() {
    let _own = own_listener();
    let dir = ShortDir::new("l4drain");
    apply_at(true, Some(dir.socket()));
    assert!(status().listening, "{:?}", status().error);
    std::thread::sleep(Duration::from_millis(600));
    let reply = send(&dir.socket(), br#"{"v":1,"event":"error","agent":"codex"}"#);
    apply_at(false, None);
    assert_eq!(
        reply.trim(),
        r#"{"ok":false,"reason":"rate_limited"}"#,
        "the drain emptied the bucket and 600 ms later a session-less probe was taken"
    );
}
