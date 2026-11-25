//! The native surfaces of the pressure-relief supervisor: what the menu-bar
//! status item shows and the one banner raised when a build is paused.
//!
//! Both are driven from here, from the supervisor's published status
//! (`sysload::relief_status`) and the governor's held verdict, on a thread of
//! the app process. Neither goes through the web view: under the memory
//! exhaustion this exists for, the web view is the part most likely to be
//! gone, and the menu bar and Notification Center are the parts still on
//! screen.
//!
//! A status that is stale or could not be read is shown as unverified, never
//! as nothing paused.

use crate::sysload::relief_status::{
    self, ActionKind, FrozenTree, ReadOutcome, ReliefStatus, Rung,
};
use crate::sysload::{Clock, Reading, Tier};
use std::time::Duration;
use tauri::{AppHandle, Runtime};

/// How often the status file and the verdict are read.
const POLL: Duration = Duration::from_secs(5);

/// A cap resume older than this is history, not news: an app that starts
/// after one has nothing to announce.
const CAP_NEWS_MS: u64 = 2 * 60_000;

/// The longest command echoed into a banner.
const MAX_COMMAND: usize = 80;

/// What the menu-bar item needs to know, from the host and the supervisor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostPressure {
    /// The governor's held verdict is Severe.
    pub severe: bool,
    /// Process trees the supervisor has stopped. Advised-only entries do not
    /// count: nothing is stopped for them.
    pub paused: u32,
    /// The supervisor's status is stale or could not be read, so `paused` is
    /// the last thing known, not the present.
    pub unverified: bool,
}

impl HostPressure {
    /// Whether the status item should ask for a glance.
    pub fn needs_glance(&self) -> bool {
        self.severe || self.paused > 0 || self.unverified
    }

    pub fn of(outcome: &ReadOutcome, severe: bool) -> Self {
        let (paused, unverified) = match outcome {
            ReadOutcome::Absent => (0, false),
            ReadOutcome::Fresh { status } => (status.paused_count(), false),
            ReadOutcome::Stale { status, .. } => (status.paused_count(), true),
            ReadOutcome::Unreadable { .. } => (0, true),
        };
        Self {
            severe,
            paused: u32::try_from(paused).unwrap_or(u32::MAX),
            unverified,
        }
    }

    /// The lines the status item's tooltip adds, none when there is nothing.
    pub fn tooltip_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if self.paused > 0 {
            lines.push(format!(
                "{} build or test paused by memory protection{}",
                self.paused,
                if self.unverified { " (unverified)" } else { "" }
            ));
        } else if self.unverified {
            lines.push("Memory protection status could not be verified".to_string());
        }
        if self.severe {
            lines.push("Memory is critically low".to_string());
        }
        lines
    }
}

/// Which banner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeKind {
    Paused,
    CapResumed,
}

/// One banner to raise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub kind: NoticeKind,
    pub heading: &'static str,
    pub body: String,
}

impl Notice {
    /// The identifier the banner is posted under: one per kind, so a repeat
    /// replaces the banner on screen instead of stacking.
    pub fn native_id(&self) -> &'static str {
        match self.kind {
            NoticeKind::Paused => "gitpulse.relief.paused",
            NoticeKind::CapResumed => "gitpulse.relief.cap-resumed",
        }
    }
}

/// What the banner rule remembers between reads.
#[derive(Debug, Default)]
pub struct Episode {
    /// A pause in the current episode has been announced.
    announced_pause: bool,
    /// The newest cap resume that has been announced.
    announced_cap_at_ms: u64,
}

impl Episode {
    /// The banner this read calls for, if any.
    ///
    /// One banner when a pause starts and one when the ten-minute cap resumed
    /// it, each once per episode. An episode is from the first pause until a
    /// read that finds the supervisor at rest. A stale or unreadable status
    /// neither starts nor ends one: a status that flaps between readable and
    /// not must not announce the same pause twice, and "could not read" is not
    /// "resumed".
    pub fn observe(&mut self, outcome: &ReadOutcome, now_ms: u64) -> Option<Notice> {
        let ReadOutcome::Fresh { status } = outcome else {
            return None;
        };
        if status.rung == Rung::None && status.paused_count() == 0 {
            self.announced_pause = false;
        }
        if let Some(action) = &status.last_action {
            if action.kind == ActionKind::CapResume
                && action.at_ms > self.announced_cap_at_ms
                && now_ms.saturating_sub(action.at_ms) <= CAP_NEWS_MS
            {
                self.announced_cap_at_ms = action.at_ms;
                return Some(cap_notice(action.detail.as_str()));
            }
        }
        if !self.announced_pause {
            if let Some(tree) = status.paused().next() {
                self.announced_pause = true;
                return Some(pause_notice(status, tree));
            }
        }
        None
    }
}

fn clean(text: &str, max: usize) -> String {
    let text = crate::alerts::text::sanitize(text);
    if text.chars().count() <= max {
        return text;
    }
    let mut cut: String = text.chars().take(max).collect();
    cut.push('…');
    cut
}

fn repo_name(path: &str) -> String {
    clean(
        std::path::Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(path),
        40,
    )
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1u64 << 30) as f64)
}

fn pause_notice(status: &ReliefStatus, tree: &FrozenTree) -> Notice {
    let why = status
        .binding
        .map_or("memory is nearly exhausted", relief_status::Binding::phrase);
    let more = status.paused_count().saturating_sub(1);
    let also = if more == 0 {
        String::new()
    } else {
        format!(" and {more} more")
    };
    Notice {
        kind: NoticeKind::Paused,
        heading: "GitPulse paused a build",
        body: format!(
            "`{}`{also} ({}) under the agent in {} was paused because {why}. \
             It resumes automatically when pressure eases, or after 10 minutes.",
            clean(&tree.command, MAX_COMMAND),
            gib(tree.footprint_bytes),
            repo_name(&tree.repo_path),
        ),
    }
}

fn cap_notice(detail: &str) -> Notice {
    let detail = clean(detail, 120);
    Notice {
        kind: NoticeKind::CapResumed,
        heading: "GitPulse resumed a paused build",
        body: if detail.is_empty() {
            "The 10-minute limit on a pause was reached, so it was resumed even though memory is still tight.".to_string()
        } else {
            format!(
                "The 10-minute limit on a pause was reached, so it was resumed even though memory is still tight: {detail}"
            )
        },
    }
}

/// The governor's held verdict is Severe, from the published snapshot. A
/// snapshot that cannot be acted on says nothing here: the item then simply
/// does not claim Severe.
fn severe_now() -> bool {
    let Some(dir) = crate::tool_config::default_config_dir() else {
        return false;
    };
    let (report, snapshot) =
        crate::sysload::report(&dir.join(crate::sysload::SNAPSHOT_FILE_NAME), Clock::now());
    report.state.governs()
        && snapshot.is_some_and(|s| {
            s.enabled && matches!(s.verdict, Reading::Value(v) if v.tier == Tier::Severe)
        })
}

/// Starts the thread that keeps the status item and the banner current. It
/// runs for the life of the app process and only reads.
pub fn start<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    let spawned = crate::supervisor::spawn_once(
        "relief-surfaces",
        "pressure relief surfaces",
        move || {
            let mut episode = Episode::default();
            let mut shown: Option<HostPressure> = None;
            loop {
                std::thread::sleep(POLL);
                let outcome = match relief_status::default_dir() {
                    Some(dir) => relief_status::read_status(&dir),
                    None => ReadOutcome::Unreadable {
                        reason: "there is no per-user config directory (HOME unset)".into(),
                    },
                };
                let pressure = HostPressure::of(&outcome, severe_now());
                if shown != Some(pressure) {
                    let handle = app.clone();
                    let applied = app.run_on_main_thread(move || {
                    if let Err(error) = super::set_host_pressure(&handle, pressure) {
                        log::warn!(target: "desktop", "status item pressure not applied: {error}");
                    }
                });
                    match applied {
                        Ok(()) => shown = Some(pressure),
                        Err(error) => {
                            log::warn!(target: "desktop", "status item pressure not queued: {error}")
                        }
                    }
                }
                if let Some(notice) = episode.observe(&outcome, relief_status::now_ms()) {
                    if let Err(error) = crate::workbench::notifications::post_relief(
                        notice.native_id(),
                        notice.heading,
                        &notice.body,
                    ) {
                        log::warn!(target: "desktop", "pressure relief banner not posted: {error}");
                    }
                }
            }
        },
    );
    if let Err(error) = spawned {
        log::warn!(target: "desktop", "pressure relief surfaces did not start: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sysload::relief_status::{Mode, ReliefAction};

    fn tree(advised_only: bool) -> FrozenTree {
        FrozenTree {
            pid: 7,
            start_us: 1,
            agent_root_pid: 3,
            terminal_session_id: Some("term-3-aa".into()),
            repo_path: "/work/gitpulse".into(),
            command: "cargo test --workspace".into(),
            footprint_bytes: 9 << 30,
            since_ms: 1,
            advised_only,
        }
    }

    fn fresh(rung: Rung, trees: Vec<FrozenTree>, last: Option<ReliefAction>) -> ReadOutcome {
        ReadOutcome::Fresh {
            status: ReliefStatus {
                rung,
                frozen: trees,
                last_action: last,
                ..ReliefStatus::at_rest(Mode::Act, 1)
            },
        }
    }

    fn rest() -> ReadOutcome {
        fresh(Rung::None, vec![], None)
    }

    fn cap_at(at_ms: u64) -> Option<ReliefAction> {
        Some(ReliefAction {
            at_ms,
            kind: ActionKind::CapResume,
            detail: "cargo test".into(),
        })
    }

    #[test]
    fn pressure_reflects_a_pause_the_tier_and_what_could_not_be_checked() {
        let calm = HostPressure::of(&rest(), false);
        assert!(!calm.needs_glance());
        assert!(calm.tooltip_lines().is_empty());
        assert!(HostPressure::of(&ReadOutcome::Absent, false) == calm);

        let severe = HostPressure::of(&ReadOutcome::Absent, true);
        assert!(severe.needs_glance());
        assert_eq!(severe.tooltip_lines(), ["Memory is critically low"]);

        let paused = HostPressure::of(&fresh(Rung::Freeze, vec![tree(false)], None), true);
        assert_eq!(paused.paused, 1);
        assert!(paused.needs_glance());
        assert_eq!(
            paused.tooltip_lines(),
            [
                "1 build or test paused by memory protection",
                "Memory is critically low"
            ]
        );

        // Advise mode records what it would pause; nothing is stopped.
        let advised = HostPressure::of(&fresh(Rung::Freeze, vec![tree(true)], None), false);
        assert_eq!(advised.paused, 0);
        assert!(!advised.needs_glance());
    }

    #[test]
    fn stale_and_unreadable_are_unverified_never_clear() {
        let stale = ReadOutcome::Stale {
            status: ReliefStatus {
                rung: Rung::Freeze,
                frozen: vec![tree(false)],
                ..ReliefStatus::at_rest(Mode::Act, 1)
            },
            age_ms: 90_000,
        };
        let pressure = HostPressure::of(&stale, false);
        assert!(pressure.unverified && pressure.paused == 1 && pressure.needs_glance());
        assert_eq!(
            pressure.tooltip_lines(),
            ["1 build or test paused by memory protection (unverified)"]
        );
        let unreadable = HostPressure::of(
            &ReadOutcome::Unreadable {
                reason: "garbled".into(),
            },
            false,
        );
        assert!(unreadable.needs_glance());
        assert_eq!(
            unreadable.tooltip_lines(),
            ["Memory protection status could not be verified"]
        );
    }

    #[test]
    fn a_pause_is_announced_once_per_episode() {
        let mut episode = Episode::default();
        let paused = fresh(Rung::Freeze, vec![tree(false)], None);
        let notice = episode.observe(&paused, 100).expect("the pause is news");
        assert_eq!(notice.kind, NoticeKind::Paused);
        for needle in [
            "cargo test --workspace",
            "9.0 GiB",
            "gitpulse",
            "resumes automatically",
        ] {
            assert!(notice.body.contains(needle), "{needle}: {}", notice.body);
        }
        assert!(episode.observe(&paused, 200).is_none());
        // A second tree in the same episode is not a second banner.
        let two = fresh(Rung::Freeze, vec![tree(false), tree(false)], None);
        assert!(episode.observe(&two, 300).is_none());
    }

    #[test]
    fn a_flapping_status_does_not_announce_the_same_pause_twice() {
        let mut episode = Episode::default();
        let paused = fresh(Rung::Freeze, vec![tree(false)], None);
        assert!(episode.observe(&paused, 1).is_some());
        let unreadable = ReadOutcome::Unreadable {
            reason: "torn".into(),
        };
        assert!(episode.observe(&unreadable, 2).is_none());
        assert!(episode.observe(&ReadOutcome::Absent, 3).is_none());
        assert!(episode.observe(&paused, 4).is_none());
    }

    #[test]
    fn a_new_episode_after_rest_is_announced_again() {
        let mut episode = Episode::default();
        let paused = fresh(Rung::Freeze, vec![tree(false)], None);
        assert!(episode.observe(&paused, 1).is_some());
        assert!(episode.observe(&rest(), 2).is_none());
        assert!(episode.observe(&paused, 3).is_some());
    }

    #[test]
    fn an_advised_only_entry_is_not_a_pause_to_announce() {
        let mut episode = Episode::default();
        assert!(episode
            .observe(&fresh(Rung::Freeze, vec![tree(true)], None), 1)
            .is_none());
    }

    #[test]
    fn the_cap_resume_is_announced_once_and_old_ones_are_history() {
        let mut episode = Episode::default();
        let capped = fresh(Rung::Slow, vec![], cap_at(1_000_000));
        let notice = episode.observe(&capped, 1_030_000).expect("cap news");
        assert_eq!(notice.kind, NoticeKind::CapResumed);
        assert!(notice.body.contains("10-minute"));
        assert!(episode.observe(&capped, 1_035_000).is_none());
        // The same cap seen by an app that started long after it.
        let mut late = Episode::default();
        assert!(late.observe(&capped, 1_000_000 + CAP_NEWS_MS + 1).is_none());
        // A later cap is news again.
        let again = fresh(Rung::Slow, vec![], cap_at(1_100_000));
        assert!(episode.observe(&again, 1_101_000).is_some());
    }

    #[test]
    fn banner_text_is_cleaned_and_bounded() {
        let mut t = tree(false);
        t.command = format!("cargo test \u{202e}{}", "x".repeat(500));
        t.repo_path = format!("/work/{}", "r".repeat(500));
        let notice = pause_notice(
            &ReliefStatus {
                rung: Rung::Freeze,
                frozen: vec![t.clone()],
                ..ReliefStatus::at_rest(Mode::Act, 1)
            },
            &t,
        );
        assert!(!notice.body.contains('\u{202e}'));
        assert!(notice.body.chars().count() < 400, "{}", notice.body);
    }

    #[test]
    fn each_kind_has_its_own_banner_identifier() {
        let a = pause_notice(&ReliefStatus::at_rest(Mode::Act, 1), &tree(false));
        let b = cap_notice("");
        assert_ne!(a.native_id(), b.native_id());
        assert!(a.native_id().starts_with("gitpulse.relief."));
    }
}
