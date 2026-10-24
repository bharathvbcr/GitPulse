//! Why the socket did not take a report: the closed set the reply carries.
//!
//! One enum owns every refusal, whether it comes from parsing (a report that is
//! not one), from the connection (too large, unreadable, too many open) or from
//! the hub (stale, queue full, not running). The wire form is a short token in
//! the reply's `reason` field; the hook looks the token up here, so a peer that
//! sends a token this build does not know is shown it verbatim and never
//! mistaken for one it does.
//!
//! The set is closed on purpose. A reason is GitPulse's sentence about GitPulse's
//! own decision, never a copy of anything the peer sent, so a hostile report
//! cannot choose the text the hook prints.

/// A refusal, with a stable wire token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// More than `MAX_REPORT_BYTES`, read or parsed.
    Oversized,
    /// The read failed for a reason other than the deadline.
    Unreadable,
    /// The connection cap was reached; nothing was read.
    AtCapacity,
    NotJson,
    NotAnObject,
    TooManyFields,
    /// `v` is not a version this bridge speaks. The reply also carries `max_v`.
    UnsupportedVersion,
    NoEvent,
    UnknownEvent,
    MalformedSubject,
    MalformedSubagent,
    /// `seq` is present and not a positive integer.
    MalformedSeq,
    UnknownAgent,
    /// `session`, `cwd` or `message` is present and not a string.
    MalformedField,
    StateOutsideSession,
    /// Older than a report already applied for its session.
    Stale,
    /// The notification worker's queue had no room for it.
    QueueFull,
    /// There is no notification worker to take it.
    NotRunning,
    /// `session` names a key the live terminal registry does not hold, so no
    /// GitPulse session exists for the report to be about.
    UnknownSession,
    /// The session (or, for a report from outside any terminal, the class of
    /// them) is sending faster than a real agent does.
    RateLimited,
}

impl Reason {
    /// Every reason, so a test can prove the tokens are distinct and a caller
    /// can look one up without a second table.
    pub const ALL: [Reason; 20] = [
        Reason::Oversized,
        Reason::Unreadable,
        Reason::AtCapacity,
        Reason::NotJson,
        Reason::NotAnObject,
        Reason::TooManyFields,
        Reason::UnsupportedVersion,
        Reason::NoEvent,
        Reason::UnknownEvent,
        Reason::MalformedSubject,
        Reason::MalformedSubagent,
        Reason::MalformedSeq,
        Reason::UnknownAgent,
        Reason::MalformedField,
        Reason::StateOutsideSession,
        Reason::Stale,
        Reason::QueueFull,
        Reason::NotRunning,
        Reason::UnknownSession,
        Reason::RateLimited,
    ];

    /// The wire token: lowercase, `_`-separated, never changed once shipped.
    pub const fn token(self) -> &'static str {
        match self {
            Reason::Oversized => "oversized",
            Reason::Unreadable => "unreadable",
            Reason::AtCapacity => "at_capacity",
            Reason::NotJson => "not_json",
            Reason::NotAnObject => "not_an_object",
            Reason::TooManyFields => "too_many_fields",
            Reason::UnsupportedVersion => "unsupported_version",
            Reason::NoEvent => "no_event",
            Reason::UnknownEvent => "unknown_event",
            Reason::MalformedSubject => "malformed_subject",
            Reason::MalformedSubagent => "malformed_subagent",
            Reason::MalformedSeq => "malformed_seq",
            Reason::UnknownAgent => "unknown_agent",
            Reason::MalformedField => "malformed_field",
            Reason::StateOutsideSession => "state_outside_session",
            Reason::Stale => "stale",
            Reason::QueueFull => "queue_full",
            Reason::NotRunning => "not_running",
            Reason::UnknownSession => "unknown_session",
            Reason::RateLimited => "rate_limited",
        }
    }

    /// The reason a wire token names, if this build knows it.
    pub fn from_token(token: &str) -> Option<Reason> {
        Self::ALL.into_iter().find(|reason| reason.token() == token)
    }

    /// Plain words for a log line or a hook's stderr. For the parse refusals
    /// these are the same strings `parse_report` has always returned.
    pub const fn message(self) -> &'static str {
        match self {
            Reason::Oversized => "oversized",
            Reason::Unreadable => "unreadable",
            Reason::AtCapacity => "too many connections open",
            Reason::NotJson => "not json",
            Reason::NotAnObject => "not an object",
            Reason::TooManyFields => "too many fields",
            Reason::UnsupportedVersion => "unsupported protocol version",
            Reason::NoEvent => "no event",
            Reason::UnknownEvent => "unknown event",
            Reason::MalformedSubject => "malformed subject",
            Reason::MalformedSubagent => "malformed subagent",
            Reason::MalformedSeq => "malformed seq",
            Reason::UnknownAgent => "unknown agent",
            Reason::MalformedField => "malformed field",
            Reason::StateOutsideSession => "state report outside a session",
            Reason::Stale => "older than a report already applied for the session",
            Reason::QueueFull => "the notification queue is full",
            Reason::NotRunning => "the notification worker is not running",
            Reason::UnknownSession => "no terminal session has that id",
            Reason::RateLimited => "reporting faster than a session can",
        }
    }
}
