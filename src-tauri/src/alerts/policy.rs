//! Whether a session's request for attention becomes an OS banner.
//!
//! Every rule that can *silence* a notification lives here, together, for one
//! reason: silence is the failure mode nobody notices. A rule buried at its
//! call site looks like an absence of notifications, which is exactly what the
//! feature looks like when it is broken.
//!
//! Each refusal is a named [`Verdict`], counted by the hub and reported in
//! status, so "GitPulse chose not to" is always distinguishable from "GitPulse
//! never saw it".

use crate::tool_config::SessionAlertSettings;

/// Why a signal did or did not become a banner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Deliver,
    /// The feature is off.
    Disabled,
    /// This launcher's bell is not treated as attention.
    NotAnAgent,
    /// The user is looking at this exact session right now.
    Attended,
    /// Inside the configured quiet hours.
    Quiet,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Self::Deliver => "deliver",
            Self::Disabled => "disabled",
            Self::NotAnAgent => "not_an_agent",
            Self::Attended => "attended",
            Self::Quiet => "quiet",
        }
    }
}

/// What the hub knows about the session a signal came from.
#[derive(Debug, Clone, Copy)]
pub struct Context {
    /// False for a plain login shell, true for an agent CLI launcher.
    pub is_agent: bool,
    /// The user is focused on the GitPulse window *and* this session is the
    /// visible one. Both halves are required; a focused window showing another
    /// tab has not shown the user anything.
    pub attended: bool,
    /// Local minute of the day, or `None` when the clock could not be read.
    pub minute: Option<u16>,
}

pub fn decide(config: &SessionAlertSettings, context: Context) -> Verdict {
    if !config.enabled {
        return Verdict::Disabled;
    }
    if !context.is_agent && !config.shell_bell {
        return Verdict::NotAnAgent;
    }
    if context.attended {
        return Verdict::Attended;
    }
    if let (Some(minute), Some(start), Some(end)) =
        (context.minute, config.quiet_start, config.quiet_end)
    {
        if in_quiet_hours(minute, start, end) {
            return Verdict::Quiet;
        }
    }
    // A clock we could not read does not silence anything. Erring toward the
    // banner is the honest direction: a notification during quiet hours is a
    // nuisance, a missed permission prompt is a stalled agent.
    Verdict::Deliver
}

/// Whether a local minute falls inside a quiet window.
///
/// The one implementation of a rule the workbench store also applies, in SQL,
/// to activity notices. A window that wraps midnight (`22:00`–`07:00`) is the
/// normal case, not the exception, which is why the comparison flips rather
/// than the window being split. `end` is exclusive at both ends of the flip so
/// a minute belongs to exactly one side.
///
/// `alerts::tests::quiet_hours_match_the_store` drives the real store over a
/// grid of windows and minutes and fails if these two ever disagree.
pub fn in_quiet_hours(minute: u16, start: u16, end: u16) -> bool {
    if start < end {
        minute >= start && minute < end
    } else {
        minute >= start || minute < end
    }
}

/// The local minute of the day, on any platform GitPulse builds for.
///
/// `localtime_r` is POSIX and `localtime_s` is its Windows spelling; both apply
/// the machine's timezone and DST, which is the whole point — quiet hours are a
/// promise about the user's evening, not about UTC.
pub fn local_minute() -> Option<u16> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let seconds = libc::time_t::try_from(seconds).ok()?;
    let mut out = std::mem::MaybeUninit::<libc::tm>::uninit();
    #[cfg(unix)]
    // SAFETY: both pointers are valid for the call, and the output is read
    // only after a non-null return proves it was initialised.
    let ok = unsafe { !libc::localtime_r(&seconds, out.as_mut_ptr()).is_null() };
    #[cfg(windows)]
    // SAFETY: as above. `localtime_s` reports success as 0 rather than by
    // pointer, and takes its arguments in the opposite order.
    let ok = unsafe { libc::localtime_s(out.as_mut_ptr(), &seconds) == 0 };
    if !ok {
        return None;
    }
    // SAFETY: guarded by `ok`.
    let time = unsafe { out.assume_init() };
    if !(0..24).contains(&time.tm_hour) || !(0..60).contains(&time.tm_min) {
        return None;
    }
    u16::try_from(time.tm_hour * 60 + time.tm_min).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> SessionAlertSettings {
        SessionAlertSettings::default()
    }

    fn context() -> Context {
        Context {
            is_agent: true,
            attended: false,
            minute: Some(12 * 60),
        }
    }

    #[test]
    fn an_unattended_agent_session_is_delivered() {
        assert_eq!(decide(&config(), context()), Verdict::Deliver);
    }

    #[test]
    fn each_refusal_is_its_own_named_verdict() {
        let mut off = config();
        off.enabled = false;
        assert_eq!(decide(&off, context()), Verdict::Disabled);

        assert_eq!(
            decide(
                &config(),
                Context {
                    is_agent: false,
                    ..context()
                }
            ),
            Verdict::NotAnAgent
        );
        assert_eq!(
            decide(
                &config(),
                Context {
                    attended: true,
                    ..context()
                }
            ),
            Verdict::Attended
        );

        let mut quiet = config();
        quiet.quiet_start = Some(22 * 60);
        quiet.quiet_end = Some(7 * 60);
        assert_eq!(
            decide(
                &quiet,
                Context {
                    minute: Some(23 * 60),
                    ..context()
                }
            ),
            Verdict::Quiet
        );
        assert_eq!(decide(&quiet, context()), Verdict::Deliver);
    }

    #[test]
    fn a_shell_bell_is_delivered_only_when_the_user_asked_for_it() {
        let mut on = config();
        on.shell_bell = true;
        assert_eq!(
            decide(
                &on,
                Context {
                    is_agent: false,
                    ..context()
                }
            ),
            Verdict::Deliver
        );
    }

    #[test]
    fn an_unreadable_clock_does_not_silence_a_waiting_agent() {
        let mut quiet = config();
        quiet.quiet_start = Some(0);
        quiet.quiet_end = Some(1439);
        assert_eq!(
            decide(
                &quiet,
                Context {
                    minute: None,
                    ..context()
                }
            ),
            Verdict::Deliver
        );
    }

    #[test]
    fn a_wrapping_window_covers_the_night_and_nothing_else() {
        let (start, end) = (22 * 60, 7 * 60);
        assert!(in_quiet_hours(22 * 60, start, end));
        assert!(in_quiet_hours(23 * 59, start, end));
        assert!(in_quiet_hours(0, start, end));
        assert!(in_quiet_hours(7 * 60 - 1, start, end));
        assert!(!in_quiet_hours(7 * 60, start, end));
        assert!(!in_quiet_hours(12 * 60, start, end));
        assert!(!in_quiet_hours(22 * 60 - 1, start, end));
    }

    #[test]
    fn a_non_wrapping_window_is_half_open_the_same_way() {
        let (start, end) = (9 * 60, 17 * 60);
        assert!(!in_quiet_hours(9 * 60 - 1, start, end));
        assert!(in_quiet_hours(9 * 60, start, end));
        assert!(in_quiet_hours(17 * 60 - 1, start, end));
        assert!(!in_quiet_hours(17 * 60, start, end));
    }

    #[test]
    fn every_minute_belongs_to_exactly_one_side_of_any_window() {
        for start in (0..1440).step_by(97) {
            for end in (0..1440).step_by(89) {
                if start == end {
                    continue;
                }
                let inside = (0..1440).filter(|m| in_quiet_hours(*m, start, end)).count();
                let span = if start < end {
                    usize::from(end - start)
                } else {
                    usize::from(1440 - start + end)
                };
                assert_eq!(inside, span, "window {start}..{end}");
            }
        }
    }

    #[test]
    fn the_local_clock_is_readable_and_in_range() {
        let minute = local_minute().expect("local minute");
        assert!(minute <= 1439);
    }
}
