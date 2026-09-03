//! Process resource limits, raised once at startup.
//!
//! A GUI launch on macOS inherits launchd's soft `RLIMIT_NOFILE` of 256
//! (`launchctl limit maxfiles` reports `256 unlimited`), which a desktop app
//! spends before it does any work of its own: the WebKit process group, the
//! WAL SQLite ledger, an FSEvents watch per open repository, PTY sessions, and
//! then two pipe descriptors for every `git` child. Past that ceiling *every*
//! spawn fails with EMFILE, and because the UI retries, the failure never
//! clears -- it presents as a permanent "Failed to spawn git ...: Too many
//! open files (os error 24)" storm across every open repository.
//!
//! The hard limit is unbounded, so the soft limit is ours to raise; the kernel
//! still caps it at `kern.maxfilesperproc`, which is why the ladder below
//! probes downward instead of asking for `RLIM_INFINITY` (which macOS rejects
//! outright).

/// What [`raise_open_file_limit`] actually achieved.
///
/// A failed raise is never reported as a successful one: the app has to be
/// able to say "we are still on 256" rather than log a reassuring line that
/// only means the attempt ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LimitOutcome {
    /// The soft limit was raised from `from` to `to`.
    Raised { from: u64, to: u64 },
    /// The soft limit already met [`TARGET_OPEN_FILES`]; nothing to do.
    AlreadySufficient(u64),
    /// Every candidate was refused. `current` is the limit still in force.
    Failed { current: u64, error: String },
    /// No supported way to query or set limits on this target.
    Unsupported,
}

impl LimitOutcome {
    /// One line for the startup log, phrased so a degraded outcome reads as
    /// degraded.
    pub fn describe(&self) -> String {
        match self {
            Self::Raised { from, to } => {
                format!("open file limit raised from {from} to {to}")
            }
            Self::AlreadySufficient(current) => {
                format!("open file limit already {current}; left as is")
            }
            Self::Failed { current, error } => format!(
                "open file limit could NOT be raised (still {current}): {error}; \
                 git operations may fail with \"Too many open files\""
            ),
            Self::Unsupported => {
                "open file limit not adjustable on this platform; left as is".to_string()
            }
        }
    }

    /// True only when the process is known to have descriptor headroom.
    pub fn is_sufficient(&self) -> bool {
        match self {
            Self::Raised { to, .. } => *to >= MIN_USABLE_OPEN_FILES,
            Self::AlreadySufficient(current) => *current >= MIN_USABLE_OPEN_FILES,
            Self::Failed { .. } | Self::Unsupported => false,
        }
    }
}

/// What we ask for first. Comfortably above anything the app can reach: the
/// spawn gate in `engine::git_cli` caps concurrent children, so this only has
/// to cover the steady-state cost of the webview, watchers, PTYs and SQLite.
pub const TARGET_OPEN_FILES: u64 = 16_384;

/// The floor below which the descriptor table is too tight to call healthy --
/// roughly the webview's own baseline plus a fully saturated spawn gate plus
/// a watch per repository in a large workspace.
pub const MIN_USABLE_OPEN_FILES: u64 = 2_048;

/// Descending candidates. macOS refuses `RLIM_INFINITY` for `RLIMIT_NOFILE`
/// and silently caps at `kern.maxfilesperproc`, so asking for one large value
/// and giving up would leave 256 in force on any host that says no.
const CANDIDATES: [u64; 5] = [TARGET_OPEN_FILES, 8_192, 4_096, 2_048, 1_024];

/// Raises this process's soft open-file limit toward [`TARGET_OPEN_FILES`],
/// never above the inherited hard limit.
pub fn raise_open_file_limit() -> LimitOutcome {
    imp::raise(&CANDIDATES, TARGET_OPEN_FILES)
}

#[cfg(all(unix, target_pointer_width = "64"))]
mod imp {
    use super::LimitOutcome;

    // `struct rlimit { rlim_t rlim_cur; rlim_t rlim_max; }`. `rlim_t` is
    // `__uint64_t` on Darwin and `unsigned long` on glibc/musl -- identical on
    // the 64-bit targets this module is compiled for, which is exactly what
    // the `target_pointer_width` gate above pins.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RLimit {
        rlim_cur: u64,
        rlim_max: u64,
    }

    // From <sys/resource.h>: Darwin numbers RLIMIT_NOFILE 8 (7 is RLIMIT_NPROC
    // there), Linux and the BSDs number it 7.
    #[cfg(target_os = "macos")]
    const RLIMIT_NOFILE: i32 = 8;
    #[cfg(not(target_os = "macos"))]
    const RLIMIT_NOFILE: i32 = 7;

    extern "C" {
        fn getrlimit(resource: i32, rlp: *mut RLimit) -> i32;
        fn setrlimit(resource: i32, rlp: *const RLimit) -> i32;
    }

    fn get() -> Result<RLimit, String> {
        let mut limit = RLimit::default();
        // SAFETY: `limit` is a live, correctly sized `struct rlimit` and the
        // resource id is a compile-time constant from <sys/resource.h>.
        let rc = unsafe { getrlimit(RLIMIT_NOFILE, &mut limit) };
        if rc == 0 {
            Ok(limit)
        } else {
            Err(last_os_error())
        }
    }

    fn set(limit: RLimit) -> Result<(), String> {
        // SAFETY: as above; `setrlimit` only reads through the pointer.
        let rc = unsafe { setrlimit(RLIMIT_NOFILE, &limit) };
        if rc == 0 {
            Ok(())
        } else {
            Err(last_os_error())
        }
    }

    fn last_os_error() -> String {
        std::io::Error::last_os_error().to_string()
    }

    pub(super) fn raise(candidates: &[u64], target: u64) -> LimitOutcome {
        let current = match get() {
            Ok(limit) => limit,
            Err(error) => {
                return LimitOutcome::Failed {
                    current: 0,
                    error: format!("getrlimit failed: {error}"),
                }
            }
        };
        if current.rlim_cur >= target {
            return LimitOutcome::AlreadySufficient(current.rlim_cur);
        }

        let mut last_error = String::from("no candidate exceeded the current limit");
        for candidate in candidates {
            // Never ask past the hard limit: an unprivileged process cannot
            // raise it, and the whole call would fail rather than clamp.
            let wanted = (*candidate).min(current.rlim_max);
            if wanted <= current.rlim_cur {
                continue;
            }
            let attempt = RLimit {
                rlim_cur: wanted,
                // Left exactly as inherited. Lowering it here would be a
                // one-way door: only root can raise it back.
                rlim_max: current.rlim_max,
            };
            match set(attempt) {
                Ok(()) => {
                    return LimitOutcome::Raised {
                        from: current.rlim_cur,
                        to: wanted,
                    }
                }
                Err(error) => last_error = format!("setrlimit({wanted}) failed: {error}"),
            }
        }
        LimitOutcome::Failed {
            current: current.rlim_cur,
            error: last_error,
        }
    }
}

#[cfg(not(all(unix, target_pointer_width = "64")))]
mod imp {
    use super::LimitOutcome;

    pub(super) fn raise(_candidates: &[u64], _target: u64) -> LimitOutcome {
        LimitOutcome::Unsupported
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_descend_and_start_at_the_target() {
        assert_eq!(CANDIDATES[0], TARGET_OPEN_FILES);
        for pair in CANDIDATES.windows(2) {
            assert!(
                pair[0] > pair[1],
                "candidates must descend so a refused ask falls back: {pair:?}"
            );
        }
        assert!(
            *CANDIDATES.last().expect("non-empty") > 256,
            "the lowest candidate must still beat launchd's 256 default"
        );
    }

    /// The regression this module exists for: a GUI launch starts at a soft
    /// limit of 256, far under what the app needs, and every git spawn past
    /// the ceiling fails with EMFILE.
    ///
    /// A host whose *hard* limit is itself below the floor cannot be raised by
    /// any unprivileged process, so this asserts the honest outcome for the
    /// environment it actually ran in rather than pretending the check passed
    /// -- and says which case that was.
    #[test]
    fn raising_leaves_the_process_with_usable_headroom() {
        let outcome = raise_open_file_limit();
        // Idempotent: startup runs it once, and the test binary may already
        // have. The second call must agree with the first.
        let again = raise_open_file_limit();

        match &outcome {
            LimitOutcome::Unsupported => {
                assert_eq!(again, LimitOutcome::Unsupported);
            }
            LimitOutcome::Failed { current, .. } => {
                assert!(
                    *current < MIN_USABLE_OPEN_FILES,
                    "a raise may only fail when the hard limit forbids it; \
                     soft was already {current}: {}",
                    outcome.describe()
                );
                eprintln!(
                    "note: this host caps the hard limit below {MIN_USABLE_OPEN_FILES}, \
                     so only the refusal path was exercised: {}",
                    outcome.describe()
                );
            }
            _ => {
                assert!(
                    outcome.is_sufficient(),
                    "expected descriptor headroom after the raise, got: {}",
                    outcome.describe()
                );
                assert!(
                    again.is_sufficient(),
                    "the raise must be idempotent, got: {}",
                    again.describe()
                );
            }
        }
    }

    #[test]
    fn a_failed_raise_never_describes_itself_as_success() {
        let failed = LimitOutcome::Failed {
            current: 256,
            error: "setrlimit(1024) failed: Invalid argument".into(),
        };
        assert!(!failed.is_sufficient());
        let text = failed.describe();
        assert!(text.contains("could NOT be raised"), "{text}");
        assert!(text.contains("256"), "{text}");

        assert!(!LimitOutcome::Unsupported.is_sufficient());
        assert!(!LimitOutcome::AlreadySufficient(256).is_sufficient());
        assert!(!LimitOutcome::Raised { from: 256, to: 512 }.is_sufficient());
        assert!(LimitOutcome::Raised {
            from: 256,
            to: MIN_USABLE_OPEN_FILES,
        }
        .is_sufficient());
    }
}
