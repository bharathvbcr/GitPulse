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
    /// Why, in GitPulse's words: "needs your input", "finished". Empty when
    /// only a bell arrived, because a bell does not say why.
    pub reason: Option<String>,
    /// What the agent itself said, when it said anything.
    pub detail: Option<String>,
    /// Whether this is an agent CLI rather than a plain login shell.
    pub is_agent: bool,
    /// Which convention carried it, for the status readout.
    pub channel: &'static str,
}

impl Notice {
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
        let reason = self.reason.as_deref().unwrap_or_default();
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
        let key: String = self
            .key
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
    /// Replaced by a newer notice from the same session inside its window.
    /// Not a fault — it is the coalescing working — but counted, so that
    /// "offered" and "accounted for" can be reconciled.
    pub coalesced: u64,
    /// Pushed out by more simultaneously signalling sessions than the notifier
    /// tracks. A fault rather than a preference, and counted apart from
    /// dropped_queue because the cause and the remedy are different ones.
    pub displaced: u64,
    /// Held back by the global rate limit.
    pub rate_limited: u64,
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
    displaced: AtomicU64,
    rate_limited: AtomicU64,
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
    let running = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let installed = Hub {
        sender,
        counters: counters.clone(),
        running: running.clone(),
        visible,
        last_error: last_error.clone(),
    };
    if HUB.set(installed).is_err() {
        return;
    }
    match std::thread::Builder::new()
        .name("gitpulse-alerts".into())
        .spawn(move || run(receiver, host, counters, last_error))
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
        displaced: c.displaced.load(Ordering::Relaxed),
        rate_limited: c.rate_limited.load(Ordering::Relaxed),
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

pub(crate) fn count_bridge_rejection() {
    if let Some(hub) = hub() {
        hub.counters.bridge_rejected.fetch_add(1, Ordering::Relaxed);
    }
}

struct Tracked {
    last_sent: Option<Instant>,
    pending: Option<Notice>,
    touched: Instant,
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
}

fn run(
    receiver: mpsc::Receiver<Notice>,
    host: Arc<dyn Host>,
    counters: Arc<Counters>,
    last_error: Arc<Mutex<Option<String>>>,
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
            admit(notice, now, &mut tracked, &counters);
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
    }
}

/// The soonest a pending notice becomes due, or `None` when nothing is held.
fn next_wake(tracked: &HashMap<String, Tracked>) -> Option<Duration> {
    let now = Instant::now();
    tracked
        .values()
        .filter(|entry| entry.pending.is_some())
        .filter_map(|entry| entry.last_sent)
        .map(|sent| (sent + COALESCE).saturating_duration_since(now))
        .min()
}

fn admit(
    notice: Notice,
    now: Instant,
    tracked: &mut HashMap<String, Tracked>,
    counters: &Counters,
) {
    if tracked.len() >= MAX_TRACKED && !tracked.contains_key(&notice.key) {
        // Evict an idle session before a waiting one. An entry with nothing
        // pending costs one extra banner to lose; an entry still holding a
        // notice costs the notice itself, and that one is counted as the fault
        // it is rather than vanishing into the gap between two totals.
        let victim = tracked
            .iter()
            .min_by_key(|(_, entry)| (entry.pending.is_some(), entry.touched))
            .map(|(key, _)| key.clone());
        if let Some(stale) = victim {
            if tracked.remove(&stale).is_some_and(|e| e.pending.is_some()) {
                counters.displaced.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    let entry = tracked.entry(notice.key.clone()).or_insert(Tracked {
        last_sent: None,
        pending: None,
        touched: now,
    });
    entry.touched = now;
    // A notice this one displaces was never shown and never judged. Counting
    // it is what lets the offered total be reconciled against the outcomes,
    // which is the only way to tell coalescing from a leak.
    if entry.pending.replace(notice).is_some() {
        counters.coalesced.fetch_add(1, Ordering::Relaxed);
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
    let due: Vec<String> = tracked
        .iter()
        .filter(|(_, entry)| {
            entry.pending.is_some()
                && (final_pass
                    || entry
                        .last_sent
                        .is_none_or(|sent| now.saturating_duration_since(sent) >= COALESCE))
        })
        .map(|(key, _)| key.clone())
        .collect();
    for key in due {
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
            continue;
        }
        if !bucket.take(now) {
            counters.rate_limited.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        entry.last_sent = Some(now);
        let native = notice.native_id();
        match host.submit(&native, &notice.heading(), &notice.body(), config.sound) {
            Ok(true) => {
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
                counters.failed.fetch_add(1, Ordering::Relaxed);
                if let Ok(mut slot) = last_error.lock() {
                    *slot = Some(error);
                }
            }
        }
    }
}

fn prune(now: Instant, tracked: &mut HashMap<String, Tracked>) {
    tracked.retain(|_, entry| {
        entry.pending.is_some() || now.saturating_duration_since(entry.touched) < TRACK_TTL
    });
}

#[cfg(test)]
mod stress;
#[cfg(test)]
mod tests;
