//! The AI transport's loopback guarantee.
//!
//! SECURITY.md states that AI requests are restricted to loopback, and the
//! module's own documentation is explicit about why: "the diff of a user's
//! unpublished work is about as sensitive as a payload gets", so "a
//! misconfigured base URL therefore fails as a refusal, never as a silent
//! upload". That is the property under test.

use gitpulse_lib::ai::http::parse_base_url;

#[test]
fn a_hostname_that_merely_looks_like_loopback_is_refused() {
    // The bypass this guards: these are DNS names, resolving wherever their
    // owner points them. A prefix check on "127." accepted every one, which
    // turned the guarantee into a string comparison and allowed unpublished
    // work to be uploaded to a remote host.
    for hostile in [
        "http://127.0.0.1.attacker.com/v1",
        "http://127.evil.example.com/v1",
        "http://127.0.0.1x.example.com/v1",
        "http://localhost.attacker.com/v1",
        "http://127.0.0.1.example.com:1234/v1",
        "http://not-127.0.0.1/v1",
    ] {
        assert!(
            parse_base_url(hostile).is_err(),
            "{hostile} must be refused"
        );
    }
}

#[test]
fn genuine_loopback_addresses_are_accepted() {
    // The whole of 127.0.0.0/8 is loopback, not just 127.0.0.1: a model server
    // bound to 127.0.0.2 is still on this machine.
    for allowed in [
        "http://127.0.0.1:1234/v1",
        "http://127.0.0.2:8080/v1",
        "http://127.255.255.254/v1",
        "http://localhost:11434/v1",
        "http://[::1]:1234/v1",
    ] {
        assert!(
            parse_base_url(allowed).is_ok(),
            "{allowed} must be accepted"
        );
    }
}

#[test]
fn routable_addresses_are_refused() {
    for hostile in [
        "http://192.168.1.5/v1",
        "http://10.0.0.1/v1",
        "http://8.8.8.8/v1",
        "http://0.0.0.0/v1",
        "http://example.com/v1",
        // IPv4-mapped IPv6 is not treated as loopback; refusing it is the
        // conservative reading and matches the behaviour before this change.
        "http://[::ffff:127.0.0.1]/v1",
    ] {
        assert!(
            parse_base_url(hostile).is_err(),
            "{hostile} must be refused"
        );
    }
}

#[test]
fn transport_level_refusals_still_hold() {
    // These were already enforced; asserted here so hardening the host check
    // cannot quietly regress them.
    assert!(
        parse_base_url("https://127.0.0.1/v1").is_err(),
        "https has no local identity"
    );
    assert!(
        parse_base_url("ftp://127.0.0.1/v1").is_err(),
        "unsupported scheme"
    );
    assert!(
        parse_base_url("http://user:pass@127.0.0.1/v1").is_err(),
        "credentials"
    );
    // Header smuggling through the Host: line.
    assert!(
        parse_base_url("http://127.0.0.1\rX:80/v1").is_err(),
        "CR in host"
    );
    assert!(
        parse_base_url("http://127.0.0.1\nX:80/v1").is_err(),
        "LF in host"
    );
    assert!(
        parse_base_url("http://127.0.0.1 X/v1").is_err(),
        "space in host"
    );
}
