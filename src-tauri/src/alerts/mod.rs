//! Native notifications for agent sessions.
//!
//! An agent CLI running in a GitPulse terminal tab used to be silent. It would
//! stop on a permission prompt, or finish a twenty-minute task, and the only
//! evidence was a dot on a tab the user had to be looking at to see. Claude
//! Code sends a desktop notification only in Ghostty, kitty and iTerm2, and
//! Codex's OSC 9 probe recognises a similar short list, so neither of them
//! considered this terminal worth speaking to.
//!
//! Three pieces close that:
//!
//! * [`scan`] reads the PTY byte stream for the four conventions a CLI can use
//!   to say "tell the user" — no cooperation from the CLI beyond emitting one
//!   of them.
//! * [`crate::terminal`] asks each CLI it launches to emit one, using that
//!   CLI's own documented, session-scoped flag. No file of the user's is
//!   written and no key they set elsewhere is otherwise disturbed.
//! * [`bridge`] listens on a local socket so an agent *hook* — which knows the
//!   difference between "needs permission" and "finished" — can say so
//!   precisely, including from a session that is not in a GitPulse tab at all.
//!
//! Everything funnels into one worker so that coalescing, rate limiting and
//! the record of what was suppressed have a single owner. The counters are not
//! decoration: this subsystem's failure mode is silence, and a count of
//! "suppressed because you were looking at it" is the only thing that
//! distinguishes working-as-intended from broken.

pub mod bridge;
pub mod policy;
pub mod scan;

use policy::Verdict;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long one session must wait between banners.
///
/// An agent that rings three times while printing a permission prompt should
/// produce one notification, not three. Within the window the newest notice
/// replaces the pending one rather than queueing behind it, so the banner that
/// eventually appears describes the latest state, not the first.
const COALESCE: Duration = Duration::from_secs(5);

/// Token-bucket ceiling and refill for delivery across all sessions.
///
/// A runaway program can write `BEL` as fast as the PTY will carry it. The
/// per-session window above already absorbs that; this is the backstop for
/// *many* sessions doing it at once, and for the socket, where the peer is not
/// necessarily a session at all.
const RATE_CAPACITY: u32 = 10;
const RATE_REFILL: Duration = Duration::from_secs(6);

/// Queue depth between the producers and the delivery worker.
const QUEUE: usize = 64;

/// How many sessions' coalescing state is retained.
///
/// Twice the PTY ceiling, so every terminal GitPulse can open has room and the
/// rest is headroom for hook reports keyed to sessions outside it. Derived
/// from that ceiling rather than chosen, because the two move together: a
/// terminal that can exist and cannot be tracked would have its notices
/// displaced by other sessions.
const MAX_TRACKED: usize = crate::terminal::MAX_PTY_SESSIONS * 2;

/// How long a session's coalescing state outlives its last signal.
const TRACK_TTL: Duration = Duration::from_secs(600);

/// Where a notice came from. Only the origin knows whether the thing asking
/// for attention is an agent, so the decision is not re-derived downstream.
/// Not a wire type: no payload carrying it crosses IPC, and the frontend has
/// no union to keep in step with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Read out of a PTY's own output stream.
    Terminal,
    /// Reported by an agent hook over the local socket.
    Hook,
    /// The renderer: the user typed into the session, which answers whatever
    /// it asked.
    Reader,
}

/// One request for the user's attention, before any policy has been applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// Stable identity for coalescing and for replacing an existing banner.
    /// A GitPulse PTY session id where there is one; the agent's own session
    /// id where the report came from outside.
    pub key: String,
    pub origin: Origin,
    /// What to call the thing that wants attention: "Claude Code", "Codex".
    pub label: String,
    /// Where it is working — a repository or worktree name, when known.
    pub place: Option<String>,
    /// Why, as one of [`bridge::EVENTS`]' names. Absent when only a bell
    /// arrived, because a bell does not say why.
    pub event: Option<&'static str>,
    /// What the agent itself said, when it said anything.
    pub detail: Option<String>,
    /// Whether this is an agent CLI rather than a plain login shell.
    pub is_agent: bool,
    /// Which convention carried it, for the status readout.
    pub channel: &'static str,
    /// Which tool call a permission request or a tool result is about, as a
    /// digest. What lets one finished tool clear the request it answered and
    /// not a different one still waiting beside it.
    pub subject: Option<String>,
    /// Reported from inside a subagent rather than the session's main agent.
    pub subagent: bool,
}

impl Notice {
    /// Why, in GitPulse's words: "needs your input", "finished".
    pub fn reason(&self) -> Option<&'static str> {
        self.event.and_then(bridge::event).map(|event| event.phrase)
    }

    fn role(&self) -> bridge::Role {
        self.event
            .and_then(bridge::event)
            .map_or(bridge::Role::Banner, |event| event.role)
    }

    /// What the user is asked for, `None` for a bare signal.
    fn need(&self) -> Option<bridge::Need> {
        self.event.and_then(bridge::event).map(|event| event.need)
    }

    /// A terminal convention that says only that something was signalled.
    fn is_bare_signal(&self) -> bool {
        self.origin == Origin::Terminal && self.event.is_none()
    }

    /// Lower first: a stalled agent outranks a finished one when the rate
    /// limit decides which banner goes out now.
    fn urgency(&self) -> u8 {
        match self.need() {
            Some(bridge::Need::Ask) => 0,
            Some(bridge::Need::Error) => 1,
            Some(bridge::Need::Finished) => 2,
            None | Some(bridge::Need::Clear) => 3,
        }
    }

    /// The banner heading. Always names GitPulse's view of the session rather
    /// than echoing agent-supplied text, so a program cannot dress its
    /// notification up as something else.
    fn heading(&self) -> String {
        match &self.place {
            Some(place) if !place.is_empty() => format!("{} · {place}", self.label),
            _ => self.label.clone(),
        }
    }

    /// The banner body, which is where agent-supplied text is allowed to go.
    fn body(&self) -> String {
        let reason = self.reason().unwrap_or_default();
        let detail = self.detail.as_deref().unwrap_or_default();
        let text = match (reason.is_empty(), detail.is_empty()) {
            (false, false) if reason != detail => format!("{reason} — {detail}"),
            (false, _) => reason.to_owned(),
            (_, false) => detail.to_owned(),
            _ => "This session is asking for your attention.".to_owned(),
        };
        scan::sanitize(&text)
    }

    /// The identifier macOS files the banner under.
    ///
    /// Namespaced away from the workbench's `gitpulse.<profile>.event-N` so
    /// activation can tell a session banner from an activity one without
    /// consulting a database, and restricted to characters that cannot be
    /// mistaken for a path or another namespace's separator.
    fn native_id(&self) -> String {
        native_id(&self.key)
    }
}

/// The banner identifier for session `key`.
fn native_id(key: &str) -> String {
    let key: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(96)
        .collect();
    format!("{NATIVE_PREFIX}{key}")
}

/// The renderer event an [`Attention`] is emitted as.
pub const ATTENTION_EVENT: &str = "gitpulse-session-attention";

/// The shortest gap between two announcements for one session.
///
/// Announcements are state, not banners: they are not held for [`COALESCE`]
/// (a permission prompt should show in the task's Agents pane at once), but a
/// program ringing the bell in a loop must not become an event per ring, so
/// within this gap the newest replaces the one still waiting.
const ANNOUNCE_GAP: Duration = Duration::from_secs(1);

/// The longest agent-supplied text an announcement carries.
const ANNOUNCE_DETAIL_CHARS: usize = 240;

/// What an agent session last asked for, as the renderer is told it.
///
/// A banner is one way to hear that an agent needs you; the task's Agents
/// pane is another, and it needs the fact whether or not a banner was shown —
/// a session the user was looking at, a notice inside quiet hours, banners
/// turned off: each suppresses the banner and none of them changes what the
/// agent is waiting for. So every agent notice is announced, ahead of policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Attention {
    /// The GitPulse PTY session id the notice is keyed to.
    pub session: String,
    /// "hook" (the agent said why) or a terminal convention ("bell", "osc9"…),
    /// which says only that something was signalled.
    pub channel: &'static str,
    /// Why, as the hook event that said so (one of [`bridge::EVENTS`]' names,
    /// such as `permission_prompt`); absent for a terminal signal, which does
    /// not say why. A name rather than the banner's phrase, so the renderer
    /// decides on a closed vocabulary instead of matching display text. An
    /// event whose need is [`bridge::Need::Clear`] tells the renderer that
    /// nothing stands any more.
    pub event: Option<&'static str>,
    /// What the agent itself said, sanitized and bounded.
    pub detail: Option<String>,
}

impl Attention {
    /// None for a notice the renderer can do nothing with: not an agent, or
    /// a hook report from outside any GitPulse terminal (keyed `hook-…`, which
    /// names no session a task attempt could own).
    fn of(notice: &Notice) -> Option<Self> {
        if !shows(notice) {
            return None;
        }
        let bound = |text: &str| -> Option<String> {
            let clean = scan::sanitize(text);
            let clipped: String = clean.chars().take(ANNOUNCE_DETAIL_CHARS).collect();
            (!clipped.trim().is_empty()).then_some(clipped)
        };
        Some(Self {
            session: notice.key.clone(),
            channel: notice.channel,
            event: notice.event,
            detail: notice.detail.as_deref().and_then(bound),
        })
    }

    /// Tells the renderer that nothing stands for `session` any more.
    fn cleared(session: &str, by: &Notice) -> Self {
        Self {
            session: session.to_owned(),
            channel: by.channel,
            event: by.event,
            detail: None,
        }
    }
}

/// What a session stands asking for, as a page that was not listening reads
/// it: the announcement it missed, field for field, and how long ago it was
/// made. Spelled out rather than flattened so `check:types` can hold the
/// renderer's `StandingAttention` to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StandingAttention {
    pub session: String,
    pub channel: String,
    pub event: Option<String>,
    pub detail: Option<String>,
    pub age_ms: u64,
}

impl StandingAttention {
    fn new(attention: &Attention, age: Duration) -> Self {
        Self {
            session: attention.session.clone(),
            channel: attention.channel.to_owned(),
            event: attention.event.map(str::to_owned),
            detail: attention.detail.clone(),
            age_ms: u64::try_from(age.as_millis()).unwrap_or(u64::MAX),
        }
    }
}

/// Whether the renderer can show anything about this notice's session: an
/// agent, inside a GitPulse terminal (a report from outside one is keyed
/// `hook-…`, which names no session a task attempt could own).
fn shows(notice: &Notice) -> bool {
    notice.is_agent && !notice.key.starts_with("hook-")
}

/// The namespace every session banner's identifier begins with.
pub const NATIVE_PREFIX: &str = "gitpulse.session.";

/// The session key inside a session banner's identifier, if it is one.
pub fn native_session_key(native: &str) -> Option<&str> {
    let key = native.strip_prefix(NATIVE_PREFIX)?;
    let ok = !key.is_empty()
        && key.len() <= 96
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    ok.then_some(key)
}

/// What this subsystem did, and did not do, since the app started.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SessionAlertStatus {
    /// The worker is running and able to receive notices.
    pub running: bool,
    pub delivered: u64,
    /// Refused by policy, one counter per named reason. A large `attended`
    /// count is the system working; a large `disabled` count with the setting
    /// on would be a bug.
    pub suppressed_disabled: u64,
    pub suppressed_not_agent: u64,
    pub suppressed_attended: u64,
    pub suppressed_quiet: u64,
    /// Replaced by a newer notice from the same session inside its window,
    /// or a bell that only repeated what that session's hook had just said.
    /// Not a fault — it is the coalescing working — but counted, so that
    /// "offered" and "accounted for" can be reconciled.
    pub coalesced: u64,
    /// Answered before its banner was due, so never shown. The user had
    /// already done what it would have asked.
    pub resolved: u64,
    /// Pushed out by more simultaneously signalling sessions than the notifier
    /// tracks. A fault rather than a preference, and counted apart from
    /// dropped_queue because the cause and the remedy are different ones.
    pub displaced: u64,
    /// Never shown because the global rate limit still held it when the
    /// worker stopped. Rare: a held banner is delayed, not discarded.
    pub rate_limited: u64,
    /// Banners the rate limit delayed, counted once each. Not an outcome —
    /// each of them also ends up in exactly one of the other counters — but
    /// the number that says the limit is doing something.
    pub deferred: u64,
    /// Never reached the worker because its queue was full.
    pub dropped_queue: u64,
    /// Produced by a scanner but not returned, because one read carried more
    /// signals than a read is allowed to return.
    pub dropped_scan: u64,
    /// Reached the OS and was refused there.
    pub failed: u64,
    /// Rejected at the socket: malformed, oversized, or from another user.
    pub bridge_rejected: u64,
    pub bridge_listening: bool,
    pub bridge_path: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Default)]
struct Counters {
    delivered: AtomicU64,
    suppressed_disabled: AtomicU64,
    suppressed_not_agent: AtomicU64,
    suppressed_attended: AtomicU64,
    suppressed_quiet: AtomicU64,
    coalesced: AtomicU64,
    resolved: AtomicU64,
    displaced: AtomicU64,
    rate_limited: AtomicU64,
    deferred: AtomicU64,
    dropped_queue: AtomicU64,
    dropped_scan: AtomicU64,
    failed: AtomicU64,
    bridge_rejected: AtomicU64,
}

impl Counters {
    fn record(&self, verdict: Verdict) {
        let slot = match verdict {
            Verdict::Deliver => return,
            Verdict::Disabled => &self.suppressed_disabled,
            Verdict::NotAnAgent => &self.suppressed_not_agent,
            Verdict::Attended => &self.suppressed_attended,
            Verdict::Quiet => &self.suppressed_quiet,
        };
        slot.fetch_add(1, Ordering::Relaxed);
    }
}

/// Everything outside the worker that the worker's decisions depend on.
///
/// A trait rather than direct calls so the whole worker — coalescing, rate
/// limiting, policy, counters — is testable without a bundled application, a
/// notification centre, a granted permission, a real clock, or a `tools.json`
/// on disk. Each of those has silenced a notification in some build of this
/// feature, and none of them is reachable from a unit test by accident.
pub trait Host: Send + Sync + 'static {
    /// Returns whether the OS accepted the submission. An error is an
    /// uncertain outcome, not a refusal.
    fn submit(&self, native: &str, heading: &str, body: &str, sound: bool) -> Result<bool, String>;
    /// Whether the user is looking at this exact session right now.
    fn attended(&self, key: &str) -> bool;
    /// The user's current preferences. Re-read per delivery so a setting
    /// change takes effect without a restart.
    fn config(&self) -> crate::tool_config::SessionAlertSettings {
        crate::tool_config::session_alerts()
    }
    /// The local minute of the day, or `None` when the clock is unreadable.
    fn minute(&self) -> Option<u16> {
        policy::local_minute()
    }
    /// Tells the renderer what an agent session asked for. Must not block:
    /// it runs on the worker thread between deliveries.
    fn announce(&self, _attention: &Attention) {}
    /// Takes down a banner whose question has been answered, so the
    /// notification centre does not keep asking it. Must not block.
    fn withdraw(&self, _native: &str) {}
}

struct Hub {
    sender: mpsc::SyncSender<Notice>,
    counters: Arc<Counters>,
    running: Arc<std::sync::atomic::AtomicBool>,
    /// The sessions the renderer last reported as on screen — plural, because
    /// a split terminal shows two at once and silencing only one of them would
    /// be arbitrary. An empty list means "nothing reported", which is treated
    /// as unattended: failing toward a banner is the safe direction when the
    /// renderer is the thing that has gone quiet.
    visible: Arc<Mutex<Vec<String>>>,
    last_error: Arc<Mutex<Option<String>>>,
    /// What every session stands asking for, as the worker last decided it.
    /// Announcements are events, and an event emitted while no page was
    /// listening — at start-up, across a reload — is gone. This is what a
    /// page that has just started listening reads to catch up.
    standing: Arc<Mutex<Vec<StandingAttention>>>,
}

static HUB: OnceLock<Hub> = OnceLock::new();

fn hub() -> Option<&'static Hub> {
    HUB.get()
}

/// Starts the delivery worker. Idempotent; a second call is a no-op.
pub fn start(host: Arc<dyn Host>) {
    let (sender, receiver) = mpsc::sync_channel(QUEUE);
    let counters = Arc::new(Counters::default());
    let visible = Arc::new(Mutex::new(Vec::new()));
    let last_error = Arc::new(Mutex::new(None));
    let standing = Arc::new(Mutex::new(Vec::new()));
    let running = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let installed = Hub {
        sender,
        counters: counters.clone(),
        running: running.clone(),
        visible,
        last_error: last_error.clone(),
        standing: standing.clone(),
    };
    if HUB.set(installed).is_err() {
        return;
    }
    match std::thread::Builder::new()
        .name("gitpulse-alerts".into())
        .spawn(move || run(receiver, host, counters, last_error, standing))
    {
        Ok(_) => running.store(true, Ordering::SeqCst),
        Err(error) => {
            // Left false on purpose. A queue that quietly fills and drops would
            // look exactly like an agent that never asked for anything.
            let message = format!("Notification worker could not start: {error}");
            log::error!(target: "alerts", "{message}");
            if let Some(hub) = hub() {
                if let Ok(mut slot) = hub.last_error.lock() {
                    *slot = Some(message);
                }
            }
        }
    }
}

/// Offers a notice to the worker without ever blocking the caller.
///
/// The callers are a PTY reader thread and a socket accept loop. Neither may
/// wait on a notification: a reader that blocks stops draining the terminal,
/// which is a visible hang for a feature nobody asked to be interrupted by.
/// A full queue is counted and dropped.
pub fn offer(notice: Notice) {
    let Some(hub) = hub() else { return };
    if hub.sender.try_send(notice).is_err() {
        hub.counters.dropped_queue.fetch_add(1, Ordering::Relaxed);
    }
}

/// The user typed into `session`, which answers whatever it was asking.
///
/// The renderer clears its own pane at once; this tells the worker, so the
/// question is not replayed to the next page that asks what stands, a banner
/// still waiting out its window is not shown, and one already shown is taken
/// down. Returns false for a key that cannot be a session.
pub fn answered(session: &str) -> bool {
    if !bridge::valid_key(session) {
        return false;
    }
    offer(Notice {
        key: session.to_owned(),
        origin: Origin::Reader,
        label: String::new(),
        place: None,
        event: Some("prompt_submitted"),
        detail: None,
        is_agent: true,
        channel: "reader",
        subject: None,
        subagent: false,
    });
    true
}

/// What every agent session in a GitPulse terminal stands asking for now.
pub fn standing() -> Vec<StandingAttention> {
    hub()
        .and_then(|hub| hub.standing.lock().ok().map(|list| list.clone()))
        .unwrap_or_default()
}

/// Records signals a scanner produced but could not return.
pub fn record_scan_drops(count: u64) {
    if count == 0 {
        return;
    }
    if let Some(hub) = hub() {
        hub.counters
            .dropped_scan
            .fetch_add(count, Ordering::Relaxed);
    }
}

/// How many on-screen sessions are tracked. A split terminal shows two; the
/// cap is a bound on an IPC-supplied list, not a product limit.
const MAX_VISIBLE: usize = 8;

/// Reports which sessions the user can actually see.
pub fn set_visible_sessions(keys: Vec<String>) {
    let Some(hub) = hub() else { return };
    if let Ok(mut slot) = hub.visible.lock() {
        *slot = keys
            .into_iter()
            .filter(|key| !key.is_empty() && key.len() <= 128)
            .take(MAX_VISIBLE)
            .collect();
    }
}

/// Whether the renderer says this session is on screen.
pub fn session_is_visible(key: &str) -> bool {
    hub()
        .and_then(|hub| hub.visible.lock().ok())
        .is_some_and(|visible| visible.iter().any(|seen| seen == key))
}

pub fn status() -> SessionAlertStatus {
    let Some(hub) = hub() else {
        return SessionAlertStatus::default();
    };
    let c = &hub.counters;
    let bridge = bridge::status();
    SessionAlertStatus {
        running: hub.running.load(Ordering::SeqCst),
        delivered: c.delivered.load(Ordering::Relaxed),
        suppressed_disabled: c.suppressed_disabled.load(Ordering::Relaxed),
        suppressed_not_agent: c.suppressed_not_agent.load(Ordering::Relaxed),
        suppressed_attended: c.suppressed_attended.load(Ordering::Relaxed),
        suppressed_quiet: c.suppressed_quiet.load(Ordering::Relaxed),
        coalesced: c.coalesced.load(Ordering::Relaxed),
        resolved: c.resolved.load(Ordering::Relaxed),
        displaced: c.displaced.load(Ordering::Relaxed),
        rate_limited: c.rate_limited.load(Ordering::Relaxed),
        deferred: c.deferred.load(Ordering::Relaxed),
        dropped_queue: c.dropped_queue.load(Ordering::Relaxed),
        dropped_scan: c.dropped_scan.load(Ordering::Relaxed),
        failed: c.failed.load(Ordering::Relaxed),
        bridge_rejected: c.bridge_rejected.load(Ordering::Relaxed),
        bridge_listening: bridge.listening,
        bridge_path: bridge.path,
        last_error: hub
            .last_error
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
            .or(bridge.error),
    }
}

/// The socket that increments this is Unix-only. A callerless function on
/// Windows is dead code under `-D warnings`.
#[cfg(unix)]
pub(crate) fn count_bridge_rejection() {
    if let Some(hub) = hub() {
        hub.counters.bridge_rejected.fetch_add(1, Ordering::Relaxed);
    }
}

/// How long a bare bell from a session whose hooks report waits for the
/// report that explains it.
///
/// Claude Code rings the bell and runs its `Notification` hook for the same
/// moment, and the hook — a process spawn and a socket round trip — usually
/// lands second. Delivering the bell at once cost the user a reasonless
/// banner and then, five seconds later, a second one saying why. Equal to the
/// hook's own deadline: a report later than this is not coming.
const HOOK_GRACE: Duration = Duration::from_millis(1500);

/// How long after a hook report a bare bell from that session is taken to be
/// the same request, rung on the other channel.
const HOOK_SHADOW: Duration = Duration::from_secs(10);

/// How long a session that has shown its hooks report is still treated as
/// one whose bells wait for a report.
const HOOKED_TTL: Duration = Duration::from_secs(6 * 60 * 60);

/// What a session stands asking for, as the worker decided it.
struct Standing {
    attention: Attention,
    subject: Option<String>,
    /// Said by a hook, with a reason, rather than inferred from a bell.
    from_hook: bool,
    /// When it was said.
    since: Instant,
}

impl Standing {
    fn need(&self) -> Option<bridge::Need> {
        self.attention
            .event
            .and_then(bridge::event)
            .map(|event| event.need)
    }

    fn is_permission(&self) -> bool {
        matches!(
            self.attention.event,
            Some("permission_request" | "permission_prompt")
        )
    }
}

struct Tracked {
    last_sent: Option<Instant>,
    pending: Option<Notice>,
    /// When the held banner was first offered, for [`HOOK_GRACE`].
    pending_since: Instant,
    /// When the rate limit will next have a token for the held banner.
    retry_at: Option<Instant>,
    touched: Instant,
    /// When this session was last announced, for [`ANNOUNCE_GAP`].
    last_announced: Option<Instant>,
    /// The newest announcement not yet made.
    unannounced: Option<Attention>,
    /// When this session's hook last reported.
    hooked_at: Option<Instant>,
    standing: Option<Standing>,
    /// A banner for this session may still be in the notification centre.
    shown: bool,
    /// That banner answers a question nobody is asking any more.
    withdraw: bool,
}

impl Tracked {
    fn new(now: Instant) -> Self {
        Self {
            last_sent: None,
            pending: None,
            pending_since: now,
            retry_at: None,
            touched: now,
            last_announced: None,
            unannounced: None,
            hooked_at: None,
            standing: None,
            shown: false,
            withdraw: false,
        }
    }

    fn holds_work(&self) -> bool {
        self.pending.is_some() || self.unannounced.is_some() || self.withdraw
    }

    fn hooked_within(&self, now: Instant, window: Duration) -> bool {
        self.hooked_at
            .is_some_and(|at| now.saturating_duration_since(at) < window)
    }

    /// When the held banner may go out, or `None` with nothing held. The one
    /// place every reason to wait is applied, so the worker's wake-up and its
    /// delivery cannot disagree about when that is.
    fn due_at(&self) -> Option<Instant> {
        let notice = self.pending.as_ref()?;
        let mut at = self.pending_since;
        if let Some(sent) = self.last_sent {
            at = at.max(sent + COALESCE);
        }
        // By origin, not by whether it has borrowed a reason: a bell that
        // restates a standing question can still be ahead of the report that
        // asks a new one.
        if notice.origin == Origin::Terminal && self.hooked_within(self.pending_since, HOOKED_TTL) {
            at = at.max(self.pending_since + HOOK_GRACE);
        }
        if let Some(retry) = self.retry_at {
            at = at.max(retry);
        }
        Some(at)
    }
}

/// A token bucket with whole-token refill.
struct Bucket {
    tokens: u32,
    last: Instant,
}

impl Bucket {
    fn new(now: Instant) -> Self {
        Self {
            tokens: RATE_CAPACITY,
            last: now,
        }
    }
    fn take(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last);
        let earned = u32::try_from(elapsed.as_secs() / RATE_REFILL.as_secs()).unwrap_or(u32::MAX);
        if earned > 0 {
            self.tokens = self.tokens.saturating_add(earned).min(RATE_CAPACITY);
            self.last += RATE_REFILL * earned;
        }
        if self.tokens == 0 {
            return false;
        }
        self.tokens -= 1;
        true
    }
    /// When the next token is earned.
    fn next_token(&self) -> Instant {
        self.last + RATE_REFILL
    }
}

fn run(
    receiver: mpsc::Receiver<Notice>,
    host: Arc<dyn Host>,
    counters: Arc<Counters>,
    last_error: Arc<Mutex<Option<String>>>,
    standing: Arc<Mutex<Vec<StandingAttention>>>,
) {
    let mut tracked: HashMap<String, Tracked> = HashMap::new();
    let mut bucket = Bucket::new(Instant::now());
    loop {
        let wait = next_wake(&tracked);
        let received = match wait {
            Some(wait) => receiver.recv_timeout(wait),
            None => receiver
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };
        let closed = matches!(received, Err(mpsc::RecvTimeoutError::Disconnected));
        let now = Instant::now();
        if let Ok(notice) = received {
            if let Some(last) = admit(notice, now, &mut tracked, &counters) {
                host.announce(&last);
            }
        }
        // A closing channel is the last chance to act on what is held. The
        // coalescing window exists to batch notices that are still arriving;
        // once nothing more can arrive, waiting it out would discard a real
        // request for attention with nothing recording that it happened.
        flush(
            now,
            closed,
            &mut tracked,
            &mut bucket,
            &host,
            &counters,
            &last_error,
        );
        if closed {
            break;
        }
        prune(now, &mut tracked);
        if let Ok(mut slot) = standing.lock() {
            *slot = standing_of(&tracked, now);
        }
    }
}

/// What every session stands asking for, for a page that was not listening
/// when it was announced.
fn standing_of(tracked: &HashMap<String, Tracked>, now: Instant) -> Vec<StandingAttention> {
    let mut list: Vec<StandingAttention> = tracked
        .values()
        .filter_map(|entry| entry.standing.as_ref())
        .map(|standing| {
            StandingAttention::new(
                &standing.attention,
                now.saturating_duration_since(standing.since),
            )
        })
        .collect();
    list.sort_by(|a, b| a.session.cmp(&b.session));
    list
}

/// The soonest a held notice or announcement becomes due, or `None` when
/// nothing is held.
fn next_wake(tracked: &HashMap<String, Tracked>) -> Option<Duration> {
    let now = Instant::now();
    let banners = tracked
        .values()
        .filter_map(Tracked::due_at)
        .map(|at| at.saturating_duration_since(now));
    let announcements = tracked
        .values()
        .filter(|entry| entry.unannounced.is_some())
        .filter_map(|entry| entry.last_announced)
        .map(|said| (said + ANNOUNCE_GAP).saturating_duration_since(now));
    let withdrawals = tracked
        .values()
        .filter(|entry| entry.withdraw)
        .map(|_| Duration::ZERO);
    banners.chain(announcements).chain(withdrawals).min()
}

/// Takes one notice into the worker's state.
///
/// Returns the last word of a session evicted to make room, when it had one
/// it had not yet said: an announcement still waiting out its gap. The caller
/// says it at once. Dropped with the entry, it left the pane showing what the
/// worker had already moved past — a request it had decided was answered.
#[must_use = "an evicted session's unsaid announcement must be announced"]
fn admit(
    mut notice: Notice,
    now: Instant,
    tracked: &mut HashMap<String, Tracked>,
    counters: &Counters,
) -> Option<Attention> {
    let mut farewell = None;
    if tracked.len() >= MAX_TRACKED && !tracked.contains_key(&notice.key) {
        // Evict an idle session before a waiting one, and a session asking
        // for nothing before one that still is. An entry with nothing pending
        // costs one extra banner to lose; an entry still holding a notice
        // costs the notice itself, and that one is counted as the fault it is
        // rather than vanishing into the gap between two totals.
        let victim = tracked
            .iter()
            .min_by_key(|(_, entry)| (entry.holds_work(), entry.standing.is_some(), entry.touched))
            .map(|(key, _)| key.clone());
        if let Some(stale) = victim {
            if let Some(evicted) = tracked.remove(&stale) {
                if evicted.pending.is_some() {
                    counters.displaced.fetch_add(1, Ordering::Relaxed);
                }
                farewell = evicted.unannounced;
            }
        }
    }
    let known = tracked.contains_key(&notice.key);
    let entry = tracked
        .entry(notice.key.clone())
        .or_insert_with(|| Tracked::new(now));
    entry.touched = now;
    if notice.origin == Origin::Hook {
        entry.hooked_at = Some(now);
    }
    match notice.role() {
        bridge::Role::Resolve => {
            resolve(entry, &notice, known, counters);
            return farewell;
        }
        bridge::Role::State => {
            stand(entry, &notice);
            return farewell;
        }
        bridge::Role::Banner => {}
    }

    // One request, two channels. A bell right behind a hook report from the
    // same session is that report rung again, and has nothing to add.
    if notice.is_bare_signal() && entry.hooked_within(now, HOOK_SHADOW) {
        counters.coalesced.fetch_add(1, Ordering::Relaxed);
        return farewell;
    }
    // A bell later than that, while a hook's question still stands, is a
    // reminder of that question — not a new, reasonless one replacing it.
    // Likewise the `Notification` that follows `PermissionRequest`: same
    // dialog, said less precisely. Either borrows what stands and leaves it.
    let restates = match &entry.standing {
        Some(standing) if standing.from_hook && notice.is_bare_signal() => {
            notice.event = standing.attention.event;
            true
        }
        Some(standing)
            if notice.event == Some("permission_prompt")
                && standing.attention.event == Some("permission_request") =>
        {
            true
        }
        _ => false,
    };
    if restates {
        if let Some(standing) = &entry.standing {
            if standing.attention.detail.is_some() {
                notice.detail.clone_from(&standing.attention.detail);
            }
        }
    } else {
        stand(entry, &notice);
    }
    // A notice this one displaces was never shown and never judged. Counting
    // it is what lets the offered total be reconciled against the outcomes,
    // which is the only way to tell coalescing from a leak.
    match entry.pending.replace(notice) {
        Some(_) => {
            counters.coalesced.fetch_add(1, Ordering::Relaxed);
        }
        None => entry.pending_since = now,
    }
    farewell
}

/// Records what a session now stands asking for and queues it to be
/// announced. Newest wins: within the gap, what the agent asks for last is
/// what it is waiting for.
fn stand(entry: &mut Tracked, notice: &Notice) {
    let Some(attention) = Attention::of(notice) else {
        return;
    };
    entry.standing = Some(Standing {
        attention: attention.clone(),
        subject: notice.subject.clone(),
        from_hook: notice.origin == Origin::Hook,
        since: entry.touched,
    });
    entry.unannounced = Some(attention);
}

/// Ends what `entry` stands asking for, when `notice` is what answers it.
///
/// `known` is false when the worker has no memory of the session — evicted
/// past `MAX_TRACKED`, or asked before this process started. The pane may
/// still be showing what it was told then, and an answer from the user ends
/// that whatever it was, so it is announced anyway: forgetting a session must
/// not strand its pane on a question that has been answered.
fn resolve(entry: &mut Tracked, notice: &Notice, known: bool, counters: &Counters) {
    let Some(standing) = &entry.standing else {
        if !known && notice.event == Some("prompt_submitted") && shows(notice) {
            entry.unannounced = Some(Attention::cleared(&notice.key, notice));
        }
        return;
    };
    let answers = match notice.event {
        // Whoever typed, and wherever: the agent has its answer.
        Some("prompt_submitted") => true,
        // A finished or failed session has said its last word, and it stays
        // said; a question it asked can no longer be answered.
        Some("session_ended") => !matches!(
            standing.need(),
            Some(bridge::Need::Finished | bridge::Need::Error)
        ),
        // The tool a permission was asked for ran, so it was approved. Any
        // other tool running proves only that *something* is working: while
        // one call waits for approval another in the same batch can finish,
        // and a subagent works on regardless of what its parent is blocked
        // on. So a permission ends only on its own call, and anything else
        // only on the main agent's own work.
        Some("tool_finished") => match (&standing.subject, &notice.subject) {
            (Some(asked), Some(ran)) => asked == ran || (!standing.is_permission() && !notice.subagent),
            _ => !standing.is_permission() && !notice.subagent,
        },
        _ => false,
    };
    if !answers {
        return;
    }
    entry.standing = None;
    entry.unannounced = Some(Attention::cleared(&notice.key, notice));
    // A banner still waiting out its window would now ask a question that
    // has been answered.
    if entry.pending.take().is_some() {
        entry.retry_at = None;
        counters.resolved.fetch_add(1, Ordering::Relaxed);
    }
    if entry.shown {
        entry.withdraw = true;
    }
}

fn flush(
    now: Instant,
    // Deliver everything held, whatever its window. Set only when the worker
    // is shutting down and nothing more can arrive.
    final_pass: bool,
    tracked: &mut HashMap<String, Tracked>,
    bucket: &mut Bucket,
    host: &Arc<dyn Host>,
    counters: &Counters,
    last_error: &Mutex<Option<String>>,
) {
    announce(now, final_pass, tracked, host);
    for (key, entry) in tracked.iter_mut() {
        if std::mem::take(&mut entry.withdraw) {
            host.withdraw(&native_id(key));
            entry.shown = false;
        }
    }
    // The most urgent first, then the longest waiting: when the rate limit
    // cannot let everything through, a stalled agent goes before a finished
    // one, whatever order a hash map happens to iterate in.
    let mut due: Vec<(u8, Instant, String)> = tracked
        .iter()
        .filter_map(|(key, entry)| {
            let notice = entry.pending.as_ref()?;
            let ready = final_pass || entry.due_at().is_some_and(|at| at <= now);
            ready.then(|| (notice.urgency(), entry.pending_since, key.clone()))
        })
        .collect();
    due.sort();
    for (_, _, key) in due {
        let Some(entry) = tracked.get_mut(&key) else {
            continue;
        };
        let Some(notice) = entry.pending.take() else {
            continue;
        };
        let config = host.config();
        let verdict = policy::decide(
            &config,
            policy::Context {
                is_agent: notice.is_agent,
                attended: host.attended(&notice.key),
                minute: host.minute(),
            },
        );
        counters.record(verdict);
        if verdict != Verdict::Deliver {
            // Not sent, so `last_sent` is untouched: a session the user stops
            // watching must be able to notify immediately rather than serve
            // out a window it never used.
            entry.retry_at = None;
            continue;
        }
        if !bucket.take(now) {
            // Held, not dropped: a permission prompt that loses a race with
            // nine other sessions is still a permission prompt. Only on the
            // final pass, when there is no later, is it lost to the limit.
            if final_pass {
                counters.rate_limited.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            // Once per banner, so the counter is banners delayed rather than
            // retries made.
            if entry.retry_at.is_none() {
                counters.deferred.fetch_add(1, Ordering::Relaxed);
            }
            entry.retry_at = Some(bucket.next_token());
            entry.pending = Some(notice);
            continue;
        }
        entry.retry_at = None;
        entry.last_sent = Some(now);
        let native = notice.native_id();
        match host.submit(&native, &notice.heading(), &notice.body(), config.sound) {
            Ok(true) => {
                entry.shown = true;
                counters.delivered.fetch_add(1, Ordering::Relaxed);
                if let Ok(mut slot) = last_error.lock() {
                    *slot = None;
                }
            }
            Ok(false) => {
                counters.failed.fetch_add(1, Ordering::Relaxed);
                if let Ok(mut slot) = last_error.lock() {
                    *slot = Some("The operating system refused a session notification.".into());
                }
            }
            Err(error) => {
                // Uncertain, so it may be on screen.
                entry.shown = true;
                counters.failed.fetch_add(1, Ordering::Relaxed);
                if let Ok(mut slot) = last_error.lock() {
                    *slot = Some(error);
                }
            }
        }
    }
}

/// Makes every announcement whose gap has passed, ahead of banner policy.
fn announce(
    now: Instant,
    final_pass: bool,
    tracked: &mut HashMap<String, Tracked>,
    host: &Arc<dyn Host>,
) {
    for entry in tracked.values_mut() {
        let due = final_pass
            || entry
                .last_announced
                .is_none_or(|said| now.saturating_duration_since(said) >= ANNOUNCE_GAP);
        if !due {
            continue;
        }
        if let Some(attention) = entry.unannounced.take() {
            host.announce(&attention);
            entry.last_announced = Some(now);
        }
    }
}

fn prune(now: Instant, tracked: &mut HashMap<String, Tracked>) {
    // A session whose hook said it is asking for something is kept however
    // long it waits — an agent can sit on a permission prompt overnight — and
    // so is one whose hooks have reported, so its next bell still waits for
    // its report. Both stay bounded by `MAX_TRACKED`. A bare bell is weaker
    // evidence and is released with the rest of the session's state.
    tracked.retain(|_, entry| {
        entry.holds_work()
            || entry.standing.as_ref().is_some_and(|standing| standing.from_hook)
            || entry.hooked_within(now, HOOKED_TTL)
            || now.saturating_duration_since(entry.touched) < TRACK_TTL
    });
}

#[cfg(test)]
mod stress;
#[cfg(test)]
mod tests;
