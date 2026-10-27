//! What became of one `notify` report, told in terms the binary can act on.
//!
//! A report that did not arrive is one of three different facts, and the old
//! `Result<(), String>` could only say the third:
//!
//! * the transport does not exist on this platform ([`SendError::Unsupported`]):
//!   nothing was attempted, and nothing is wrong. Windows has no Unix socket,
//!   so the bridge is not offered there (`docs/QUALIFICATION.md`, "Known absent
//!   off macOS");
//! * the transport exists and the report did not get through
//!   ([`SendError::Failed`]): the socket was refused, the app answered
//!   something else, or the reply never came;
//! * success, which is the `Ok` that neither of the above may ever collapse
//!   into. A check that could not run must not report like one that ran.
//!
//! This file depends on `std` and `log` only, so the `cfg(not(unix))` path can
//! be compiled for Windows from a scratch crate without the rest of the hook.

use std::fmt;

/// Why the transport did not carry a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    /// This platform has no transport for the bridge. Nothing was attempted.
    Unsupported,
    /// The transport exists and the report was not delivered.
    Failed(String),
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
}
