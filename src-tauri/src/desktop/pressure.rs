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
    self, FrozenTree, ReliefActionKind, ReliefOutcome, ReliefRung, ReliefStatus,
};
use crate::sysload::{Binding, Clock, Reading, Tier};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;
use tauri::AppHandle;

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
    /// What that verdict is bound by, when it names one. Severe is not always
    /// memory: a disk about to fill or a runaway load binds it too.
    pub cause: Option<Binding>,
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

    pub fn of(outcome: &ReliefOutcome, severe: bool, cause: Option<Binding>) -> Self {
        let (paused, unverified) = match outcome {
            ReliefOutcome::Absent => (0, false),
            ReliefOutcome::Fresh { status } => (status.paused_count(), false),
            ReliefOutcome::Stale { status, .. } => (status.paused_count(), true),
            ReliefOutcome::Unreadable { .. } => (0, true),
        };
        Self {
            severe,
            cause: if severe { cause } else { None },
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
            lines.push(match self.cause {
                Some(cause) => format!("Host load is severe: {}", cause.cause_phrase()),
                None => "Host load is severe".to_string(),
            });
        }
        lines
    }
}

/// Which banner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeKind {
    Paused,
    CapResumed,
    /// A tree rung B may not pause by itself, offered to the person.
    Offer,
    /// A heavy tree in an untrusted repository: advice, no action.
    Advice,
}

/// One banner to raise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub kind: NoticeKind,
    pub heading: &'static str,
    pub body: String,
    /// The tree (pid, start time and the moment it was paused or offered) the
    /// banner is about, which its "End it" or "Pause it" button acts on. Travels
    /// in the banner's identifier, so a banner left on screen cannot reach a
    /// later pause of the same process.
    pub target: Option<relief_status::TreeId>,
}

impl Notice {
    /// The identifier the banner is posted under. A banner about a tree
    /// carries the tree in it (a repeat for the same tree replaces the banner
    /// on screen instead of stacking, and a button press knows which tree it is
    /// for); the cap banner has one fixed identifier.
    pub fn native_id(&self) -> String {
        use crate::workbench::notifications as n;
        let about = |prefix: &str| match self.target {
            Some(id) => relief_status::banner_id(prefix, id),
            None => prefix.to_string(),
        };
        match self.kind {
            NoticeKind::Paused => about(n::RELIEF_PAUSED_PREFIX),
            NoticeKind::CapResumed => "gitpulse.relief.cap-resumed".to_string(),
            NoticeKind::Offer => about(n::RELIEF_OFFER_PREFIX),
            NoticeKind::Advice => about(n::RELIEF_ADVICE_PREFIX),
        }
    }

    /// Which set of buttons the banner carries.
    pub(crate) fn banner(&self) -> crate::workbench::notifications::ReliefBanner {
        use crate::workbench::notifications::ReliefBanner;
        match self.kind {
            NoticeKind::Paused | NoticeKind::CapResumed => ReliefBanner::Paused,
            NoticeKind::Offer => ReliefBanner::Offer,
            NoticeKind::Advice => ReliefBanner::Advice,
        }
    }
}

/// The least time between two pause banners in one episode.
const BANNER_GAP_MS: u64 = 60_000;

/// What the banner rule remembers between reads.
#[derive(Debug, Default)]
pub struct Episode {
    /// The trees (`pid`, `start_us`) a banner has announced in this episode.
    announced: BTreeSet<(u32, u64)>,
    /// When the last pause banner of this episode was raised.
    last_banner_ms: Option<u64>,
    /// The newest cap resume that has been announced.
    announced_cap_at_ms: u64,
    /// The offered trees (`pid`, `start_us`) a banner has been raised for.
    /// Reset when a read finds no offers: an offer has its own life, apart
    /// from the pause episode (a tree that is only offered is paused by nobody).
    offered: BTreeSet<(u32, u64)>,
    /// When the last offer banner was raised.
    last_offer_ms: Option<u64>,
    /// The person opted in to ending a tree at the 10-minute cap, so the pause
    /// banner says that instead of "resumes".
    pub terminate_at_cap: bool,
}

impl Episode {
    /// The banner this read calls for, if any.
    ///
    /// A pause banner when a tree not yet announced in this episode is paused,
    /// at most one every [`BANNER_GAP_MS`]: a second tree paused later in the
    /// same episode is news, a flapping one is not. A tree held back by the
    /// gap is announced on the first read after it. One banner when the
    /// ten-minute cap resumed a pause, once per cap. An episode is from the
    /// first pause until a read that finds the supervisor at rest. A stale or
    /// unreadable status neither starts nor ends one: a status that flaps
    /// between readable and not must not announce the same pause twice, and
    /// "could not read" is not "resumed".
    pub fn observe(&mut self, outcome: &ReliefOutcome, now_ms: u64) -> Option<Notice> {
        let ReliefOutcome::Fresh { status } = outcome else {
            return None;
        };
        let paused: Vec<&FrozenTree> = status.paused().collect();
        if status.rung == ReliefRung::None && paused.is_empty() {
            self.announced.clear();
            self.last_banner_ms = None;
        }
        if let Some(action) = &status.last_action {
            if action.kind == ReliefActionKind::CapResume
                && action.at_ms > self.announced_cap_at_ms
                && now_ms.saturating_sub(action.at_ms) <= CAP_NEWS_MS
            {
                self.announced_cap_at_ms = action.at_ms;
                return Some(cap_notice(action.detail.as_str()));
            }
        }
        if let Some(fresh) = paused
            .iter()
            .find(|tree| !self.announced.contains(&(tree.pid, tree.start_us)))
        {
            let due = self
                .last_banner_ms
                .is_none_or(|last| now_ms.saturating_sub(last) >= BANNER_GAP_MS);
            if !due {
                return None;
            }
            self.announced
                .extend(paused.iter().map(|tree| (tree.pid, tree.start_us)));
            self.last_banner_ms = Some(now_ms);
            return Some(if self.terminate_at_cap {
                pause_notice_terminating(status, fresh)
            } else {
                pause_notice(status, fresh)
            });
        }
        self.observe_offers(status, now_ms)
    }

    /// The banner a standing offer calls for: one per offered tree, at most one
    /// every [`BANNER_GAP_MS`]. An observed session's tree gets "Pause it"; an
    /// untrusted repository's gets advice only.
    fn observe_offers(&mut self, status: &ReliefStatus, now_ms: u64) -> Option<Notice> {
        if status.offers.is_empty() {
            self.offered.clear();
            self.last_offer_ms = None;
            return None;
        }
        let fresh = status
            .offers
            .iter()
            .find(|tree| !self.offered.contains(&(tree.pid, tree.start_us)))?;
        let due = self
            .last_offer_ms
            .is_none_or(|last| now_ms.saturating_sub(last) >= BANNER_GAP_MS);
        if !due {
            return None;
        }
        self.offered.insert((fresh.pid, fresh.start_us));
        self.last_offer_ms = Some(now_ms);
        Some(offer_notice(fresh))
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
    let why = status.binding.map_or(
        "memory is nearly exhausted",
        relief_status::ReliefBinding::phrase,
    );
    let more = status.paused_count().saturating_sub(1);
    let also = if more == 0 {
        String::new()
    } else {
        format!(" and {more} more")
    };
    Notice {
        kind: NoticeKind::Paused,
        heading: "Waylode paused a build",
        body: format!(
            "`{}`{also} ({}) under the agent in {} was paused because {why}. \
             It resumes automatically when pressure eases, or after 10 minutes. \
             \"End it\" ends it instead.",
            clean(&tree.command, MAX_COMMAND),
            gib(tree.footprint_bytes),
            repo_name(&tree.repo_path),
        ),
        target: Some(relief_status::TreeId::of(tree)),
    }
}

/// The pause banner when the person chose to have a paused build ended at the
/// 10-minute cap: it must not promise a resume that will not come.
fn pause_notice_terminating(status: &ReliefStatus, tree: &FrozenTree) -> Notice {
    let why = status.binding.map_or(
        "memory is nearly exhausted",
        relief_status::ReliefBinding::phrase,
    );
    Notice {
        kind: NoticeKind::Paused,
        heading: "Waylode paused a build",
        body: format!(
            "`{}` ({}) under the agent in {} was paused because {why}. \
             It resumes when pressure eases; if it is still paused after 10 minutes it is \
             ended, as you chose in Settings. \"End it\" ends it now.",
            clean(&tree.command, MAX_COMMAND),
            gib(tree.footprint_bytes),
            repo_name(&tree.repo_path),
        ),
        target: Some(relief_status::TreeId::of(tree)),
    }
}

/// The banner for an offered tree.
fn offer_notice(tree: &FrozenTree) -> Notice {
    let command = clean(&tree.command, MAX_COMMAND);
    let repo = repo_name(&tree.repo_path);
    match tree.why {
        Some(relief_status::OfferWhy::Untrusted) => Notice {
            kind: NoticeKind::Advice,
            heading: "Memory is running out",
            body: format!(
                "`{command}` ({}) under the agent in {repo} is using a lot of it. Waylode \
                 does not trust that repository, so it will not pause the build.",
                gib(tree.footprint_bytes),
            ),
            target: Some(relief_status::TreeId::of(tree)),
        },
        _ => Notice {
            kind: NoticeKind::Offer,
            heading: "Memory is running out",
            body: format!(
                "Memory is running out. Pause `{command}` ({}) under the agent in {repo}? \
                 Waylode did not start that agent, so it asks first.",
                gib(tree.footprint_bytes),
            ),
            target: Some(relief_status::TreeId::of(tree)),
        },
    }
}

fn cap_notice(detail: &str) -> Notice {
    let detail = clean(detail, 120);
    Notice {
        kind: NoticeKind::CapResumed,
        heading: "Waylode resumed a paused build",
        body: if detail.is_empty() {
            "The 10-minute limit on a pause was reached, so it was resumed even though memory is still tight.".to_string()
        } else {
            format!(
                "The 10-minute limit on a pause was reached, so it was resumed even though memory is still tight: {detail}"
            )
        },
        target: None,
    }
}

/// The governor's held verdict is Severe, from the published snapshot. A
/// snapshot that cannot be acted on says nothing here: the item then simply
/// does not claim Severe. The second value is what the verdict is bound by.
fn severity_now() -> (bool, Option<Binding>) {
    let Some(dir) = crate::tool_config::default_config_dir() else {
        return (false, None);
    };
    let (report, snapshot) =
        crate::sysload::report(&dir.join(crate::sysload::SNAPSHOT_FILE_NAME), Clock::now());
    if !report.state.governs() {
        return (false, None);
    }
    match snapshot {
        Some(s) if s.enabled => match s.verdict {
            Reading::Value(v) if v.tier == Tier::Severe => (true, v.binding),
            _ => (false, None),
        },
        _ => (false, None),
    }
}

/// What one poll keeps between polls, so a restart after a panic carries on
/// where it was instead of announcing everything again.
#[derive(Debug, Default)]
struct Poller {
    episode: Episode,
    /// What the status item last showed, set only once it was drawn.
    shown: Option<HostPressure>,
}

impl Poller {
    /// One read: redraw the status item if what it shows changed, and raise
    /// the banner it calls for. `apply` returns `Err` when the item could not
    /// be drawn, which leaves `shown` as it was so the next poll tries again.
    /// `post` is told of a banner; one the notification centre does not take
    /// is logged, not retried every poll.
    fn step(
        &mut self,
        outcome: &ReliefOutcome,
        severity: (bool, Option<Binding>),
        now_ms: u64,
        apply: &mut dyn FnMut(HostPressure) -> Result<(), String>,
        post: &mut dyn FnMut(&Notice),
    ) {
        let pressure = HostPressure::of(outcome, severity.0, severity.1);
        if self.shown != Some(pressure) {
            match apply(pressure) {
                Ok(()) => self.shown = Some(pressure),
                Err(error) => {
                    log::warn!(target: "desktop", "status item pressure not applied, will retry: {error}")
                }
            }
        }
        if let Some(notice) = self.episode.observe(outcome, now_ms) {
            post(&notice);
        }
    }
}

/// Stops the poller [`start`] began.
#[derive(Clone, Debug)]
pub struct PressureHandle(Arc<AtomicBool>);

impl PressureHandle {
    pub fn stop(&self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Draws `pressure` on the status item on the main thread and waits, bounded,
/// for the answer: a draw that was queued is not a draw that happened.
fn apply_on_main(app: &AppHandle, pressure: HostPressure) -> Result<(), String> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let _ = sender.send(super::set_host_pressure(&handle, pressure));
    })
    .map_err(|error| error.to_string())?;
    receiver
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| "the main thread did not answer in time".to_string())?
}

/// Starts the service that keeps the status item and the banner current. It is
/// restarted with backoff if it panics (so the status item is not left at its
/// last state), carries its memory across the restart, and ends when the
/// returned handle is stopped. It only reads.
pub fn start(app: &AppHandle) -> Option<PressureHandle> {
    let stop = Arc::new(AtomicBool::new(false));
    let handle = PressureHandle(stop.clone());
    let app = app.clone();
    let mut poller = Poller::default();
    let spawned = crate::supervisor::spawn_service("relief-surfaces", move || {
        while !stop.load(Ordering::Acquire) {
            // Slept in slices so a stop is heard within a second.
            let mut waited = Duration::ZERO;
            while waited < POLL && !stop.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_secs(1));
                waited += Duration::from_secs(1);
            }
            if stop.load(Ordering::Acquire) {
                return;
            }
            let outcome = match relief_status::default_dir() {
                Some(dir) => relief_status::read_status(&dir),
                None => ReliefOutcome::Unreadable {
                    reason: "there is no per-user config directory (HOME unset)".into(),
                },
            };
            poller.episode.terminate_at_cap = crate::tool_config::host_load_supervisor_terminate();
            poller.step(
                &outcome,
                severity_now(),
                relief_status::now_ms(),
                &mut |pressure| apply_on_main(&app, pressure),
                &mut |notice| {
                    match crate::workbench::notifications::post_relief(
                        &app,
                        &notice.native_id(),
                        notice.heading,
                        &notice.body,
                        notice.banner(),
                    ) {
                        Ok(true) => {}
                        Ok(false) => {
                            log::info!(target: "desktop", "pressure relief banner not shown: notifications are off for this profile")
                        }
                        Err(error) => {
                            log::warn!(target: "desktop", "pressure relief banner not posted: {error}")
                        }
                    }
                },
            );
        }
    });
    match spawned {
        Ok(_) => Some(handle),
        Err(error) => {
            log::warn!(target: "desktop", "pressure relief surfaces did not start: {error}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sysload::relief_status::{ReliefAction, ReliefMode};

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
            why: None,
        }
    }

    fn fresh(
        rung: ReliefRung,
        trees: Vec<FrozenTree>,
        last: Option<ReliefAction>,
    ) -> ReliefOutcome {
        ReliefOutcome::Fresh {
            status: ReliefStatus {
                rung,
                frozen: trees,
                last_action: last,
                ..ReliefStatus::at_rest(ReliefMode::Act, 1)
            },
        }
    }

    fn rest() -> ReliefOutcome {
        fresh(ReliefRung::None, vec![], None)
    }

    fn cap_at(at_ms: u64) -> Option<ReliefAction> {
        Some(ReliefAction {
            at_ms,
            kind: ReliefActionKind::CapResume,
            detail: "cargo test".into(),
        })
    }

    #[test]
    fn pressure_reflects_a_pause_the_tier_and_what_could_not_be_checked() {
        let calm = HostPressure::of(&rest(), false, None);
        assert!(!calm.needs_glance());
        assert!(calm.tooltip_lines().is_empty());
        assert!(HostPressure::of(&ReliefOutcome::Absent, false, None) == calm);

        let severe = HostPressure::of(&ReliefOutcome::Absent, true, Some(Binding::Swap));
        assert!(severe.needs_glance());
        assert_eq!(
            severe.tooltip_lines(),
            ["Host load is severe: swap in use is above Waylode's limit for it"]
        );

        let paused = HostPressure::of(
            &fresh(ReliefRung::Freeze, vec![tree(false)], None),
            true,
            Some(Binding::CompressorSegments),
        );
        assert_eq!(paused.paused, 1);
        assert!(paused.needs_glance());
        assert_eq!(
            paused.tooltip_lines(),
            [
                "1 build or test paused by memory protection",
                "Host load is severe: the memory compressor is nearly out of segments"
            ]
        );

        // Severe is not always memory: a disk bound verdict says disk, and
        // never "memory".
        let disk = HostPressure::of(&ReliefOutcome::Absent, true, Some(Binding::LowDisk));
        let lines = disk.tooltip_lines();
        assert_eq!(
            lines,
            ["Host load is severe: free disk space is below Waylode's floor"]
        );
        assert!(!lines[0].to_lowercase().contains("memory"));
        // A severe verdict naming nothing still says only what is known.
        let unnamed = HostPressure::of(&ReliefOutcome::Absent, true, None);
        assert_eq!(unnamed.tooltip_lines(), ["Host load is severe"]);
        // A cause with no severe verdict is dropped.
        assert_eq!(
            HostPressure::of(&ReliefOutcome::Absent, false, Some(Binding::Swap)),
            calm
        );

        // Advise mode records what it would pause; nothing is stopped.
        let advised = HostPressure::of(
            &fresh(ReliefRung::Freeze, vec![tree(true)], None),
            false,
            None,
        );
        assert_eq!(advised.paused, 0);
        assert!(!advised.needs_glance());
    }

    #[test]
    fn stale_and_unreadable_are_unverified_never_clear() {
        let stale = ReliefOutcome::Stale {
            status: ReliefStatus {
                rung: ReliefRung::Freeze,
                frozen: vec![tree(false)],
                ..ReliefStatus::at_rest(ReliefMode::Act, 1)
            },
            age_ms: 90_000,
            cause: relief_status::ReliefStaleCause::Age,
        };
        let pressure = HostPressure::of(&stale, false, None);
        assert!(pressure.unverified && pressure.paused == 1 && pressure.needs_glance());
        assert_eq!(
            pressure.tooltip_lines(),
            ["1 build or test paused by memory protection (unverified)"]
        );
        let unreadable = HostPressure::of(
            &ReliefOutcome::Unreadable {
                reason: "garbled".into(),
            },
            false,
            None,
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
        let paused = fresh(ReliefRung::Freeze, vec![tree(false)], None);
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
        assert!(episode.observe(&paused, 100 + BANNER_GAP_MS * 5).is_none());
    }

    fn other_tree(pid: u32) -> FrozenTree {
        FrozenTree {
            pid,
            start_us: 99,
            command: "go test ./...".into(),
            ..tree(false)
        }
    }

    #[test]
    fn a_new_freeze_in_the_same_episode_gets_a_banner_rate_limited_to_the_gap() {
        let mut episode = Episode::default();
        let first = fresh(ReliefRung::Freeze, vec![tree(false)], None);
        assert!(episode.observe(&first, 1_000).is_some());
        // A different tree (pid and start) joins within the gap: held back,
        // and not forgotten.
        let two = fresh(ReliefRung::Freeze, vec![tree(false), other_tree(8)], None);
        assert!(episode.observe(&two, 1_000 + 5_000).is_none());
        let notice = episode
            .observe(&two, 1_000 + BANNER_GAP_MS)
            .expect("the held-back freeze is announced once the gap has passed");
        assert!(notice.body.contains("go test ./..."), "{}", notice.body);
        // And then it is announced: nothing more for the same set.
        assert!(episode.observe(&two, 1_000 + 3 * BANNER_GAP_MS).is_none());
        // The same pid with a different start is a different tree.
        let reused = fresh(
            ReliefRung::Freeze,
            vec![FrozenTree {
                start_us: 12345,
                ..other_tree(8)
            }],
            None,
        );
        assert!(episode
            .observe(&reused, 1_000 + 5 * BANNER_GAP_MS)
            .is_some());
    }

    /// The step the poller takes: a draw that failed is not remembered as
    /// shown, so the next poll tries it again.
    #[test]
    fn a_failed_draw_is_retried_and_only_a_successful_one_is_remembered() {
        let mut poller = Poller::default();
        let severe = (true, Some(Binding::Swap));
        let mut attempts = 0;
        let mut banners = 0;
        let paused = fresh(ReliefRung::Freeze, vec![tree(false)], None);
        let mut run = |poller: &mut Poller, ok: bool, now: u64| {
            poller.step(
                &paused,
                severe,
                now,
                &mut |_| {
                    attempts += 1;
                    if ok {
                        Ok(())
                    } else {
                        Err("the main thread is busy".into())
                    }
                },
                &mut |_| banners += 1,
            );
        };
        run(&mut poller, false, 1);
        assert_eq!(poller.shown, None);
        run(&mut poller, false, 2);
        run(&mut poller, true, 3);
        assert!(poller.shown.is_some());
        run(&mut poller, true, 4);
        // Drawn once more than it failed, then left alone while unchanged.
        assert_eq!(attempts, 3);
        assert_eq!(banners, 1, "the banner is independent of the draw");
    }

    #[test]
    fn a_flapping_status_does_not_announce_the_same_pause_twice() {
        let mut episode = Episode::default();
        let paused = fresh(ReliefRung::Freeze, vec![tree(false)], None);
        assert!(episode.observe(&paused, 1).is_some());
        let unreadable = ReliefOutcome::Unreadable {
            reason: "torn".into(),
        };
        assert!(episode.observe(&unreadable, 2).is_none());
        assert!(episode.observe(&ReliefOutcome::Absent, 3).is_none());
        assert!(episode.observe(&paused, 4).is_none());
    }

    #[test]
    fn a_new_episode_after_rest_is_announced_again() {
        let mut episode = Episode::default();
        let paused = fresh(ReliefRung::Freeze, vec![tree(false)], None);
        assert!(episode.observe(&paused, 1).is_some());
        assert!(episode.observe(&rest(), 2).is_none());
        assert!(episode.observe(&paused, 3).is_some());
    }

    #[test]
    fn an_advised_only_entry_is_not_a_pause_to_announce() {
        let mut episode = Episode::default();
        assert!(episode
            .observe(&fresh(ReliefRung::Freeze, vec![tree(true)], None), 1)
            .is_none());
    }

    #[test]
    fn the_cap_resume_is_announced_once_and_old_ones_are_history() {
        let mut episode = Episode::default();
        let capped = fresh(ReliefRung::Slow, vec![], cap_at(1_000_000));
        let notice = episode.observe(&capped, 1_030_000).expect("cap news");
        assert_eq!(notice.kind, NoticeKind::CapResumed);
        assert!(notice.body.contains("10-minute"));
        assert!(episode.observe(&capped, 1_035_000).is_none());
        // The same cap seen by an app that started long after it.
        let mut late = Episode::default();
        assert!(late.observe(&capped, 1_000_000 + CAP_NEWS_MS + 1).is_none());
        // A later cap is news again.
        let again = fresh(ReliefRung::Slow, vec![], cap_at(1_100_000));
        assert!(episode.observe(&again, 1_101_000).is_some());
    }

    #[test]
    fn banner_text_is_cleaned_and_bounded() {
        let mut t = tree(false);
        t.command = format!("cargo test \u{202e}{}", "x".repeat(500));
        t.repo_path = format!("/work/{}", "r".repeat(500));
        let notice = pause_notice(
            &ReliefStatus {
                rung: ReliefRung::Freeze,
                frozen: vec![t.clone()],
                ..ReliefStatus::at_rest(ReliefMode::Act, 1)
            },
            &t,
        );
        assert!(!notice.body.contains('\u{202e}'));
        assert!(notice.body.chars().count() < 400, "{}", notice.body);
    }

    fn offer(pid: u32, why: relief_status::OfferWhy) -> FrozenTree {
        FrozenTree {
            pid,
            start_us: 500 + u64::from(pid),
            advised_only: true,
            why: Some(why),
            ..tree(true)
        }
    }

    fn with_offers(offers: Vec<FrozenTree>) -> ReliefOutcome {
        ReliefOutcome::Fresh {
            status: ReliefStatus {
                rung: ReliefRung::Slow,
                offers,
                ..ReliefStatus::at_rest(ReliefMode::Act, 1)
            },
        }
    }

    #[test]
    fn an_offer_is_announced_once_per_tree_rate_limited_and_asks_first() {
        let mut episode = Episode::default();
        let observed = relief_status::OfferWhy::Observed;
        let one = with_offers(vec![offer(7, observed)]);
        let notice = episode.observe(&one, 1_000).expect("an offer is news");
        assert_eq!(notice.kind, NoticeKind::Offer);
        assert_eq!(
            notice.target,
            Some(relief_status::TreeId::of(&offer(7, observed)))
        );
        for needle in [
            "Memory is running out. Pause",
            "cargo test --workspace",
            "gitpulse",
        ] {
            assert!(notice.body.contains(needle), "{needle}: {}", notice.body);
        }
        assert_eq!(notice.native_id(), "gitpulse.relief.offer:7:507:1");
        // Once per tree: the same offer standing is not announced again.
        assert!(episode.observe(&one, 1_000 + 10 * BANNER_GAP_MS).is_none());
        // A second tree inside the gap is held back, not forgotten.
        let two = with_offers(vec![offer(7, observed), offer(8, observed)]);
        assert!(episode.observe(&two, 1_000 + 5_000).is_none());
        let second = episode
            .observe(&two, 1_000 + BANNER_GAP_MS)
            .expect("announced once the gap has passed");
        assert_eq!(second.target.map(|id| id.pid), Some(8));
        // The offers going away is its own reset: a later offer is news again.
        assert!(episode
            .observe(&rest(), 1_000 + 3 * BANNER_GAP_MS)
            .is_none());
        assert!(episode.observe(&one, 1_000 + 4 * BANNER_GAP_MS).is_some());
    }

    #[test]
    fn an_untrusted_offer_is_advice_with_no_pause_and_a_paused_banner_says_what_the_opt_in_does() {
        let mut episode = Episode::default();
        let advice = episode
            .observe(
                &with_offers(vec![offer(9, relief_status::OfferWhy::Untrusted)]),
                1,
            )
            .unwrap();
        assert_eq!(advice.kind, NoticeKind::Advice);
        assert_eq!(
            advice.banner(),
            crate::workbench::notifications::ReliefBanner::Advice
        );
        assert!(advice.body.contains("will not pause"), "{}", advice.body);
        let paused = fresh(ReliefRung::Freeze, vec![tree(false)], None);
        let mut off = Episode::default();
        let plain = off.observe(&paused, 1).unwrap();
        assert!(
            plain.body.contains("resumes automatically"),
            "{}",
            plain.body
        );
        assert!(plain.body.contains("End it"));
        let mut on = Episode {
            terminate_at_cap: true,
            ..Episode::default()
        };
        let ending = on.observe(&paused, 1).unwrap();
        assert!(
            ending.body.contains("ended") && !ending.body.contains("resumes automatically"),
            "{}",
            ending.body
        );
        assert_eq!(
            ending.banner(),
            crate::workbench::notifications::ReliefBanner::Paused
        );
        assert_eq!(ending.native_id(), "gitpulse.relief.paused:7:1:1");
    }

    #[test]
    fn each_kind_has_its_own_banner_identifier() {
        let a = pause_notice(&ReliefStatus::at_rest(ReliefMode::Act, 1), &tree(false));
        let b = cap_notice("");
        assert_ne!(a.native_id(), b.native_id());
        assert!(a.native_id().starts_with("gitpulse.relief."));
    }
}
