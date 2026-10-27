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
