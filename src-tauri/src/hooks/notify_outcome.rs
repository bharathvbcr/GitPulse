//! What became of one `notify` report, told in terms the binary can act on.
//!
//! A report that did not arrive is one of several different facts, and the old
//! `Result<(), String>` could only say the last:
//!
//! * the transport does not exist on this platform ([`SendError::Unsupported`]):
//!   nothing was attempted, and nothing is wrong. Windows has no Unix socket,
//!   so the bridge is not offered there (`docs/QUALIFICATION.md`, "Known absent
//!   off macOS");
//! * the transport exists and no reply came within its bound
//!   ([`SendError::NoReply`]): the one fact the drop count is about, however
//!   the wait ended (see below);
//! * the transport exists and the report did not get through
//!   ([`SendError::Failed`]): the socket was refused, or the app answered
//!   something that is not an acknowledgement;
//! * success, which is the `Ok` that none of the above may ever collapse
//!   into. A check that could not run must not report like one that ran.
//!
//! "No reply within the bound" has two clocks: the hook's outer budget
//! (`within_budget`) and the socket's own deadline. They are the same length
//! for every event but `SessionEnd`, so which one fires first is a coin toss,
//! and an outcome decided by a coin toss is not an outcome. Both clocks now
//! produce [`SendError::NoReply`] (the outer one through the budget's own
//! miss), and the caller counts that once. The two durations are deliberately
//! not nudged apart: that would hide the race, not remove it.
//!
//! This file depends on `std` and `log` only, so the `cfg(not(unix))` path can
//! be compiled for Windows from a scratch crate without the rest of the hook.

use std::fmt;
use std::path::PathBuf;

/// Whether this build has a transport for the notification bridge at all. The
/// Unix domain socket is the only one there is.
pub const TRANSPORT_AVAILABLE: bool = cfg!(unix);

/// Why the transport did not carry a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    /// This platform has no transport for the bridge. Nothing was attempted.
    Unsupported,
    /// The transport exists and no reply came: the bound passed, or the peer
    /// closed without one. Counted as a missed reply by the caller; not
    /// confirmed delivered, not retried.
    NoReply,
    /// The transport exists and the report was not delivered.
    Failed(String),
}

impl SendError {
    /// Classifies an I/O error from the exchange. A timeout is a missed reply
    /// whichever way the platform spells it (`EAGAIN` as `WouldBlock` on
    /// macOS, `TimedOut` on Linux); anything else is a failure, said as
    /// `context: error`.
    pub fn from_io(context: &str, error: &std::io::Error) -> Self {
        match error.kind() {
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => SendError::NoReply,
            _ => SendError::Failed(format!("{context}: {error}")),
        }
    }
}

/// What a finished exchange with the bridge means, as a pure function of what
/// was read and how the read and the send ended. No I/O, so every host tests
/// the same table; `acknowledge` is the parser of a complete reply.
///
/// * A reply that was read decides the outcome, however the read then ended
///   and whether or not the send succeeded. A bridge at capacity writes its
///   refusal and closes the socket without reading the report; Linux then
///   reports `ECONNRESET` on the next read after the reply (macOS reports EOF),
///   and the refusal must be reported as itself on both. A truncated reply
///   still fails `acknowledge`, so nothing is hidden.
/// * No reply and a peer that is gone (end of stream, or `BrokenPipe`,
///   `ConnectionReset`, `NotConnected`) is [`SendError::NoReply`]: not
///   confirmed delivered.
/// * A timeout, with or without a partial reply, is [`SendError::NoReply`]
///   (through [`SendError::from_io`]), counted once like the outer budget.
/// * Any other error is a failure, said as `context: error`.
pub fn classify_exchange(
    reply: &[u8],
    read: Result<(), &std::io::Error>,
    sent: Result<(), &std::io::Error>,
    acknowledge: impl FnOnce(&[u8]) -> Result<(), String>,
) -> Result<(), SendError> {
    let peer_gone = |error: &std::io::Error| {
        matches!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::NotConnected
        )
    };
    let decided_by_reply = || acknowledge(reply).map_err(SendError::Failed);
    match (read, sent) {
        (Ok(()), Ok(())) if reply.is_empty() => Err(SendError::NoReply),
        (Ok(()), Ok(())) => decided_by_reply(),
        (Ok(()), Err(error)) if reply.is_empty() => Err(if peer_gone(error) {
            SendError::NoReply
        } else {
            SendError::from_io("could not send the report", error)
        }),
        (Ok(()), Err(_)) => decided_by_reply(),
        (Err(error), _) if peer_gone(error) => {
            if reply.is_empty() {
                Err(SendError::NoReply)
            } else {
                decided_by_reply()
            }
        }
        (Err(error), Ok(())) => Err(SendError::from_io("no answer from GitPulse", error)),
        (Err(_), Err(error)) => Err(SendError::from_io("could not send the report", error)),
    }
}

/// The socket a report goes to, or why there is none. The transport is checked
/// first: on a platform with none the answer is `Unsupported` whatever the path
/// would have been, and `resolve` is not called. A missing config directory
/// (`APPDATA` unset on Windows) is a fact about where a socket would live, which
/// is not a question on a platform that has no socket.
pub(super) fn socket_for_send(
    transport_available: bool,
    resolve: impl FnOnce() -> Option<PathBuf>,
) -> Result<PathBuf, HookError> {
    if !transport_available {
        return Err(SendError::Unsupported.into());
    }
    resolve().ok_or_else(|| {
        HookError::from("no GitPulse configuration directory to find a notification socket in")
    })
}

/// Why a hook subcommand produced no output, as the binary reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookError {
    /// The hook could not do what it was asked: an unknown subcommand, an
    /// unreadable payload, a report that was refused or never answered.
    Failed(String),
    /// The hook has nothing to do on this platform. Not a failure.
    Unsupported(String),
}

impl fmt::Display for HookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HookError::Failed(message) | HookError::Unsupported(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for HookError {}

impl From<String> for HookError {
    fn from(message: String) -> Self {
        HookError::Failed(message)
    }
}

impl From<&str> for HookError {
    fn from(message: &str) -> Self {
        HookError::Failed(message.to_owned())
    }
}

impl From<SendError> for HookError {
    fn from(error: SendError) -> Self {
        match error {
            SendError::Unsupported => HookError::Unsupported(
                "agent notification reports need a Unix socket, which this platform does not \
                 offer; nothing was sent"
                    .to_owned(),
            ),
            // The text only. Counting a missed reply is the caller's job
            // (`run_notify_within`), which handles `NoReply` before converting.
            SendError::NoReply => HookError::Failed(
                "notification report got no reply within its bound; not retried".to_owned(),
            ),
            SendError::Failed(reason) => {
                HookError::Failed(format!("notification report not delivered: {reason}"))
            }
        }
    }
}

/// The one line the binary writes for a [`HookError`], and how it exits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub level: log::Level,
    pub line: String,
    /// Always 0. Exit 2 blocks the user's tool call whatever the JSON says, and
    /// a hook that could not report must never be the reason an edit is refused.
    pub exit_code: i32,
}

/// Maps an outcome to its log line and exit code. Independent of the host
/// platform, so every arm is tested on every host.
///
/// A failure is a warning: something that should have worked did not.
/// `Unsupported` is information: the platform has nothing to report to, and a
/// warning on every tool call of a Windows session would be noise nobody can
/// act on.
pub fn diagnostic(error: &HookError) -> Diagnostic {
    let level = match error {
        HookError::Failed(_) => log::Level::Warn,
        HookError::Unsupported(_) => log::Level::Info,
    };
    Diagnostic {
        level,
        line: format!("gitpulse-hook: {error}"),
        exit_code: 0,
    }
}

/// The `notify` transport on a platform that has none: says so, sends nothing.
#[cfg(not(unix))]
pub(super) fn send_unsupported(_path: &std::path::Path, _payload: &str) -> Result<(), SendError> {
    Err(SendError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unsupported_transport_is_info_and_exits_zero() {
        let diagnostic = diagnostic(&HookError::from(SendError::Unsupported));
        assert_eq!(diagnostic.level, log::Level::Info);
        assert_eq!(diagnostic.exit_code, 0);
        assert!(
            diagnostic.line.contains("does not offer"),
            "{}",
            diagnostic.line
        );
        assert!(diagnostic.line.contains("nothing was sent"));
    }

    #[test]
    fn a_real_send_failure_is_still_a_warning() {
        let diagnostic = diagnostic(&HookError::from(SendError::Failed("no listener".into())));
        assert_eq!(diagnostic.level, log::Level::Warn);
        assert_eq!(diagnostic.exit_code, 0);
        assert!(
            diagnostic
                .line
                .contains("notification report not delivered: no listener"),
            "{}",
            diagnostic.line
        );
    }

    #[test]
    fn every_other_failure_is_a_warning() {
        for error in [
            HookError::from("unknown subcommand"),
            HookError::from("x".to_owned()),
        ] {
            let diagnostic = diagnostic(&error);
            assert_eq!(diagnostic.level, log::Level::Warn, "{error}");
            assert_eq!(diagnostic.exit_code, 0);
        }
    }

    /// The point of the type: the three outcomes must stay three. Unsupported
    /// is neither a success nor the failure it used to be reported as.
    #[test]
    fn unsupported_is_distinguishable_from_failure_and_from_success() {
        let unsupported = HookError::from(SendError::Unsupported);
        let failed = HookError::from(SendError::Failed("refused".into()));
        assert!(matches!(unsupported, HookError::Unsupported(_)));
        assert!(matches!(failed, HookError::Failed(_)));
        assert_ne!(unsupported, failed);
        let delivered: Result<(), SendError> = Ok(());
        assert_ne!(delivered, Err(SendError::Unsupported));
        let refused: Result<(), SendError> = Err(SendError::Failed(String::new()));
        assert_ne!(refused, Err(SendError::Unsupported));
    }

    #[test]
    fn a_failure_message_survives_the_string_conversions() {
        assert_eq!(HookError::from("a").to_string(), "a");
        assert_eq!(HookError::from(String::from("b")).to_string(), "b");
    }

    /// macOS spells an expired socket timeout `EAGAIN` (`WouldBlock`), Linux
    /// `TimedOut`. Both are the same missed reply; neither is a failure.
    #[test]
    fn either_spelling_of_a_socket_timeout_is_a_missed_reply() {
        use std::io::{Error, ErrorKind};
        for kind in [ErrorKind::WouldBlock, ErrorKind::TimedOut] {
            assert_eq!(
                SendError::from_io("read", &Error::from(kind)),
                SendError::NoReply,
                "{kind:?}"
            );
        }
        let reset = Error::from(ErrorKind::ConnectionReset);
        assert!(
            matches!(SendError::from_io("read", &reset), SendError::Failed(text) if text.starts_with("read: ")),
        );
    }

    /// The exchange table, on every host. The fake acknowledgement accepts only
    /// `{"ok":true}` and refuses anything else with the bytes it saw.
    fn acknowledge(bytes: &[u8]) -> Result<(), String> {
        if bytes == b"{\"ok\":true}\n" {
            Ok(())
        } else {
            Err(format!(
                "refused: {}",
                String::from_utf8_lossy(bytes).trim()
            ))
        }
    }

    const AT_CAPACITY: &[u8] = b"{\"ok\":false,\"reason\":\"at_capacity\"}\n";

    fn error(kind: std::io::ErrorKind) -> std::io::Error {
        std::io::Error::from(kind)
    }

    /// Linux: the bridge answered at capacity and closed unread, so the read
    /// that returned the reply is followed by `ECONNRESET`. The reply decides.
    #[test]
    fn a_reply_read_before_a_reset_is_the_outcome() {
        use std::io::ErrorKind::{BrokenPipe, ConnectionReset, NotConnected};
        for kind in [ConnectionReset, BrokenPipe, NotConnected] {
            let reset = error(kind);
            let outcome = classify_exchange(AT_CAPACITY, Err(&reset), Ok(()), acknowledge);
            assert!(
                matches!(&outcome, Err(SendError::Failed(text)) if text.contains("at_capacity")),
                "{kind:?}: {outcome:?}"
            );
            // The same refusal with a failed send as well.
            let outcome =
                classify_exchange(AT_CAPACITY, Err(&reset), Err(&error(kind)), acknowledge);
            assert!(
                matches!(&outcome, Err(SendError::Failed(text)) if text.contains("at_capacity")),
                "{kind:?} with a failed send: {outcome:?}"
            );
        }
        // macOS: the same reply then end of stream.
        let outcome = classify_exchange(AT_CAPACITY, Ok(()), Ok(()), acknowledge);
        assert!(matches!(&outcome, Err(SendError::Failed(text)) if text.contains("at_capacity")));
        // An acknowledgement before a reset is still an acknowledgement.
        let reset = error(std::io::ErrorKind::ConnectionReset);
        assert_eq!(
            classify_exchange(b"{\"ok\":true}\n", Err(&reset), Ok(()), acknowledge),
            Ok(())
        );
        // A truncated reply after a reset is not hidden.
        let outcome = classify_exchange(b"{\"ok\":fal", Err(&reset), Ok(()), acknowledge);
        assert!(matches!(&outcome, Err(SendError::Failed(text)) if text.contains("refused")));
    }

    /// A timeout, with nothing or with part of a reply, is a missed reply and
    /// is not swept into the acknowledgement parser (which would make it an
    /// uncounted failure).
    #[test]
    fn a_timeout_is_a_missed_reply_with_or_without_part_of_a_reply() {
        for kind in [std::io::ErrorKind::TimedOut, std::io::ErrorKind::WouldBlock] {
            let timed_out = error(kind);
            for partial in [&b""[..], b"{\"ok\":fal"] {
                assert_eq!(
                    classify_exchange(partial, Err(&timed_out), Ok(()), acknowledge),
                    Err(SendError::NoReply),
                    "{kind:?} {partial:?}"
                );
            }
        }
    }

    /// No reply from a peer that is gone is a missed reply; any other error
    /// stays a failure that says what it was.
    #[test]
    fn a_peer_that_is_gone_without_a_reply_is_missed_and_other_errors_fail() {
        use std::io::ErrorKind::{BrokenPipe, ConnectionReset, PermissionDenied};
        assert_eq!(
            classify_exchange(b"", Ok(()), Ok(()), acknowledge),
            Err(SendError::NoReply)
        );
        let reset = error(ConnectionReset);
        assert_eq!(
            classify_exchange(b"", Err(&reset), Ok(()), acknowledge),
            Err(SendError::NoReply)
        );
        assert_eq!(
            classify_exchange(b"", Ok(()), Err(&error(BrokenPipe)), acknowledge),
            Err(SendError::NoReply)
        );
        let denied = error(PermissionDenied);
        assert!(matches!(
            classify_exchange(b"", Err(&denied), Ok(()), acknowledge),
            Err(SendError::Failed(text)) if text.starts_with("no answer from GitPulse: ")
        ));
        assert!(matches!(
            classify_exchange(b"", Ok(()), Err(&denied), acknowledge),
            Err(SendError::Failed(text)) if text.starts_with("could not send the report: ")
        ));
        // A plain acknowledgement is delivered.
        assert_eq!(
            classify_exchange(b"{\"ok\":true}\n", Ok(()), Ok(()), acknowledge),
            Ok(())
        );
    }

    /// An unavailable transport answers before the path is asked for: a missing
    /// config directory (`APPDATA` unset) must not turn "this platform has no
    /// socket" into a failure.
    #[test]
    fn an_unavailable_transport_never_resolves_a_path() {
        let error = socket_for_send(false, || panic!("the path was resolved")).unwrap_err();
        assert!(matches!(error, HookError::Unsupported(_)), "{error:?}");
        assert_eq!(diagnostic(&error).level, log::Level::Info);
    }

    #[test]
    fn an_available_transport_without_a_path_is_a_failure() {
        let error = socket_for_send(true, || None).unwrap_err();
        assert!(
            matches!(&error, HookError::Failed(text) if text.contains("configuration directory"))
        );
        let path = socket_for_send(true, || Some(PathBuf::from("/s"))).unwrap();
        assert_eq!(path, PathBuf::from("/s"));
    }
}
