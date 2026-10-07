//! One bounded delivery worker for a profile, with no model or repository work.
//! Durable claims precede OS calls; uncertain submissions are never retried.
use super::{query, WorkbenchError, WorkbenchState};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use tauri::{Emitter, Manager};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;
    fn unsupported() -> WorkbenchError {
        WorkbenchError::new(
            "unsupported",
            "Native activity notifications are not supported on this platform yet.",
        )
    }
    pub(super) fn install(
        _sender: mpsc::SyncSender<Event>,
        _status: Arc<Mutex<Status>>,
    ) -> Result<(), WorkbenchError> {
        Err(unsupported())
    }
    pub(super) fn authorization(_request: bool) -> Result<String, WorkbenchError> {
        Err(unsupported())
    }
    pub(super) fn local_minute() -> Result<u16, WorkbenchError> {
        Err(unsupported())
    }
    pub(super) fn submit(
        _native: &str,
        _title: &str,
        _sound: bool,
    ) -> Result<bool, WorkbenchError> {
        Err(unsupported())
    }
    pub(super) fn post(
        _native: &str,
        _heading: &str,
        _body: &str,
        _sound: bool,
        _category: &str,
    ) -> Result<bool, WorkbenchError> {
        Err(unsupported())
    }
    pub(super) fn delivered() -> Result<Vec<String>, WorkbenchError> {
        Err(unsupported())
    }
    pub(super) fn withdraw(_natives: &[String]) {}
    pub(super) const SESSION_CATEGORY: &str = "gitpulse-session";
}

pub(super) struct Coordinator {
    sender: mpsc::SyncSender<Event>,
    status: Arc<Mutex<Status>>,
}
pub(super) enum Event {
    Wake,
    /// macOS notification center delivers this. Other targets never construct
    /// it, but the worker still matches it so the protocol stays one type.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Activate(String),
    /// The banner's own Snooze button. Same delegate and the same identity
    /// checks as [`Event::Activate`], but it never raises the window: Snooze
    /// is the person saying "not now", and stealing focus would contradict it.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Snooze(String),
}

/// How long the banner's Snooze defers a notice. The activity inbox's own
/// Snooze sends the same number from `attentionWrite` in
/// `src/lib/workbench/client.ts`; `scripts/notification-snooze-contract.test.ts`
/// holds the two equal, because a banner and the inbox row it mirrors must not
/// disagree about what Snooze means.
pub(super) const SNOOZE_SECONDS: u32 = 3600;

/// At most this many delivered banners are checked for withdrawal in one pass.
/// Notification Center keeps a history of its own; the cap keeps a pass to a
/// fixed number of store reads whatever it reports.
const WITHDRAW_BATCH: usize = 64;
#[derive(Clone, serde::Serialize)]
struct Status {
    available: bool,
    authorization: String,
    error: Option<String>,
}
impl Default for Status {
    fn default() -> Self {
        Self {
            available: false,
            authorization: "unavailable".into(),
            error: None,
        }
    }
}
#[derive(Deserialize)]
struct Settings {
    enabled: bool,
    sound: bool,
    background: bool,
}
#[derive(Deserialize)]
struct Delivery {
    id: String,
    revision: u64,
    native_id: String,
    state: String,
}
#[derive(Deserialize)]
struct Notice {
    id: String,
    title: String,
}

impl Coordinator {
    pub(super) fn wake(&self) {
        // Full means a wake is already queued. Enabled profiles also reconcile
        // every five seconds, including outcomes committed by the Go host.
        if let Err(mpsc::TrySendError::Disconnected(_)) = self.sender.try_send(Event::Wake) {
            self.error("Native notification worker stopped.");
        }
    }
    fn error(&self, message: &str) {
        if let Ok(mut s) = self.status.lock() {
            s.error = Some(message.into());
        }
    }
}

fn decoded<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T, WorkbenchError> {
    serde_json::from_value(value.clone())
        .map_err(|_| WorkbenchError::new("protocol_error", "Invalid native notification record."))
}
fn call(host: &WorkbenchState, method: &str, input: Value) -> Result<Value, WorkbenchError> {
    host.with_store(|store| query(store, method, &input.to_string()))
}
fn settings(host: &WorkbenchState) -> Result<Settings, WorkbenchError> {
    decoded(&call(host, "notifications.settings.get", json!({"id":"profile"}))?["item"])
}
fn delivery(host: &WorkbenchState, id: &str) -> Result<Delivery, WorkbenchError> {
    decoded(&call(host, "notifications.delivery.get", json!({"id":id}))?["item"])
}
fn notice_id(native: &str) -> Option<&str> {
    let tail = native.strip_prefix("gitpulse.")?;
    let (profile, id) = tail.split_once('.')?;
    if profile.len() != 32 || !profile.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let sequence = id.strip_prefix("event-")?;
    if sequence.is_empty() || sequence.len() > 16 || !sequence.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(id)
}
/// The notice a banner identity names, once its saved delivery confirms the
/// banner is this profile's. A well-formed identity from another profile (or
/// one no claim was ever saved for) is refused, never acted on.
fn owned<'a>(
    host: &WorkbenchState,
    native: &'a str,
) -> Result<(&'a str, Delivery), WorkbenchError> {
    let id = notice_id(native)
        .ok_or_else(|| WorkbenchError::new("invalid_input", "Invalid notification identity."))?;
    let current = delivery(host, id)?;
    if current.native_id != native {
        return Err(WorkbenchError::new(
            "invalid_input",
            "Notification belongs to a different profile.",
        ));
    }
    Ok((id, current))
}
#[derive(Deserialize)]
struct Attention {
    revision: u64,
    read_at: Option<i64>,
    dismissed_at: Option<i64>,
    snoozed_until: Option<i64>,
    target_status: String,
}
fn attention(host: &WorkbenchState, id: &str) -> Result<Attention, WorkbenchError> {
    decoded(&call(host, "attention.get", json!({"id":id}))?["item"])
}
/// The banner's Snooze, written as the same `attention.update` the inbox's
/// Snooze sends, so the durable inbox is the only record of it. No delivery
/// field changes: the claim stays consumed, which is what keeps a snoozed
/// notice from being bannered a second time when it comes back.
fn snooze(host: &WorkbenchState, native: &str) -> Result<(), WorkbenchError> {
    let (id, _) = owned(host, native)?;
    for _ in 0..3 {
        let current = attention(host, id)?;
        let result = call(
            host,
            "attention.update",
            json!({"id":id,"request_id":format!("native-snooze-{id}-{}",current.revision),"expected_revision":current.revision,"action":"snooze","seconds":SNOOZE_SECONDS}),
        );
        match result {
            Ok(_) => return Ok(()),
            Err(e) if e.code == "revision_conflict" => continue,
            Err(e) => return Err(e),
        }
    }
    Err(WorkbenchError::new(
        "busy",
        "Notification changed while snoozing. Snooze it from the activity inbox.",
    ))
}
/// Whether a delivered banner has stopped saying something true. Each arm is a
/// way the notice was resolved somewhere other than the banner: read or
/// dismissed in the inbox, snoozed (from either surface), or its target moved
/// on — a decision answered, expired or withdrawn by the provider, a run that
/// changed, a task edited or deleted. `target_status` is the store's own
/// verdict, the same one eligibility uses, so this cannot drift from it.
fn resolved(notice: &Attention, now: i64) -> bool {
    notice.read_at.is_some()
        || notice.dismissed_at.is_some()
        || notice.snoozed_until.is_some_and(|until| until > now)
        || notice.target_status != "current"
}
/// Which of `delivered` to take back out of Notification Center. Identities in
/// another namespace (session banners), from another profile, or with no saved
/// claim are left alone: this profile can only vouch for its own banners.
fn stale_banners(
    host: &WorkbenchState,
    delivered: &[String],
    now: i64,
) -> Result<Vec<String>, WorkbenchError> {
    let mut stale = Vec::new();
    for native in delivered.iter().take(WITHDRAW_BATCH) {
        let id = match owned(host, native) {
            Ok((id, _)) => id,
            Err(e) if matches!(e.code.as_str(), "invalid_input" | "not_found") => continue,
            Err(e) => return Err(e),
        };
        if resolved(&attention(host, id)?, now) {
            stale.push(native.clone());
        }
    }
    Ok(stale)
}
fn withdraw(
    host: &WorkbenchState,
    delivered: &[String],
    now: i64,
    remove: impl FnOnce(&[String]),
) -> Result<usize, WorkbenchError> {
    let stale = stale_banners(host, delivered, now)?;
    if !stale.is_empty() {
        remove(&stale);
    }
    Ok(stale.len())
}
/// The clock a snooze is compared against. An unreadable clock is an error,
/// not zero: zero would read every snooze as still running and withdraw it.
fn unix_now() -> Result<i64, WorkbenchError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .ok_or_else(|| WorkbenchError::new("clock_error", "System clock is unavailable."))
}
fn activate(host: &WorkbenchState, native: &str) -> Result<(), WorkbenchError> {
    for _ in 0..3 {
        let (id, current) = owned(host, native)?;
        let result = call(
            host,
            "notifications.activate",
            json!({"id":id,"request_id":format!("native-open-{id}-{}",current.revision),"expected_revision":current.revision,"native_id":native}),
        );
        match result {
            Ok(_) => return Ok(()),
            Err(e) if e.code == "revision_conflict" => continue,
            Err(e) => return Err(e),
        }
    }
    Err(WorkbenchError::new(
        "busy",
        "Notification changed while opening. Open the activity inbox.",
    ))
}
fn finish(host: &WorkbenchState, id: &str, submitted: bool) -> Result<(), WorkbenchError> {
    for _ in 0..3 {
        let current = delivery(host, id)?;
        if current.state != "uncertain" {
            return Ok(());
        }
        let result = call(
            host,
            "notifications.finish",
            json!({"id":id,"request_id":format!("native-result-{id}-{}",current.revision),"expected_revision":current.revision,"state":if submitted {"submitted"}else{"failed"}}),
        );
        match result {
            Ok(_) => return Ok(()),
            Err(e) if e.code == "revision_conflict" => continue,
            Err(e) => return Err(e),
        }
    }
    Err(WorkbenchError::new(
        "busy",
        "OS reply received but its receipt could not be saved.",
    ))
}

// A send error is an uncertain result (including callback timeout), so only an
// explicit callback outcome may move the saved claim to submitted or failed.
fn deliver(
    host: &WorkbenchState,
    notice: &Notice,
    minute: u16,
    sound: bool,
    send: impl FnOnce(&str, &str, bool) -> Result<bool, WorkbenchError>,
) -> Result<(), WorkbenchError> {
    let claimed = call(
        host,
        "notifications.claim",
        json!({"id":notice.id,"request_id":format!("native-claim-{}",notice.id),"expected_revision":0,"minute_of_day":minute}),
    )?;
    let record: Delivery = decoded(&claimed["item"])?;
    if record.id != notice.id || record.state != "uncertain" || record.revision != 1 {
        return Err(WorkbenchError::new(
            "protocol_error",
            "Invalid native delivery claim.",
        ));
    }
    host.check_open()?;
    let submitted = send(&record.native_id, &notice.title, sound)?;
    finish(host, &record.id, submitted)
}

pub(crate) fn install(app: &tauri::AppHandle) {
    let host = app.state::<WorkbenchState>().inner().clone();
    let (sender, receiver) = mpsc::sync_channel(32);
    let status = Arc::new(Mutex::new(Status::default()));
    let coordinator = Coordinator {
        sender: sender.clone(),
        status: status.clone(),
    };
    if host.0.notifications.set(coordinator).is_err() {
        return;
    }
    let installed = platform::install(sender, status.clone());
    if let Err(error) = installed {
        if let Ok(mut current) = status.lock() {
            current.error = Some(error.message);
        }
        return;
    }
    if let Ok(mut current) = status.lock() {
        current.available = true;
        current.authorization = "unknown".into();
    }
    let app = app.clone();
    let failure = status.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("workbench-notifications".into())
        .spawn(move || run(host, app, receiver, status))
    {
        if let Ok(mut current) = failure.lock() {
            current.available = false;
            current.error = Some(error.to_string());
        }
    }
}

fn run(
    host: WorkbenchState,
    app: tauri::AppHandle,
    receiver: mpsc::Receiver<Event>,
    status: Arc<Mutex<Status>>,
) {
    let mut enabled = false;
    // Do not create a database merely because the application was launched.
    let mut reconcile = host.profile_path().is_ok_and(|path| path.exists());
    loop {
        if host.check_open().is_err() {
            break;
        }
        if reconcile {
            let result = (|| -> Result<(), WorkbenchError> {
                let preferences = settings(&host)?;
                enabled = preferences.enabled;
                // Withdrawal runs before every gate below. It posts nothing, so
                // neither the background setting nor a hidden window is a reason
                // to leave a resolved banner on screen — and a notice resolved
                // in the open window is the commonest case of all. A disabled
                // profile still takes back what it delivered while enabled; it
                // only reaches here on a wake, never on the periodic timer.
                // A failed pass is reported but does not hold back delivery:
                // the next pass retries it, and a stale banner is a smaller
                // harm than a withheld one.
                let withdrawn = platform::delivered().and_then(|delivered| {
                    withdraw(&host, &delivered, unix_now()?, platform::withdraw)
                });
                if let Err(error) = withdrawn {
                    if let Ok(mut current) = status.lock() {
                        current.error = Some(error.message);
                    }
                }
                if !enabled {
                    return Ok(());
                }
                if !preferences.background
                    && !app.get_webview_window("main").is_some_and(|window| {
                        window.is_visible().unwrap_or(false)
                            && !window.is_minimized().unwrap_or(true)
                    })
                {
                    return Ok(());
                }
                {
                    let authorization = platform::authorization(false)?;
                    if let Ok(mut current) = status.lock() {
                        current.authorization = authorization.clone();
                    }
                    if !matches!(authorization.as_str(), "authorized" | "provisional") {
                        return Ok(());
                    }
                    let minute = platform::local_minute()?;
                    let page = call(
                        &host,
                        "notifications.pending.list",
                        json!({"minute_of_day":minute,"limit":3}),
                    )?;
                    let notices: Vec<Notice> = decoded(&page["items"])?;
                    if notices.len() > 3 {
                        return Err(WorkbenchError::new(
                            "protocol_error",
                            "Native delivery batch exceeded its limit.",
                        ));
                    }
                    for notice in notices {
                        if let Err(error) =
                            deliver(&host, &notice, minute, preferences.sound, platform::submit)
                        {
                            if !matches!(
                                error.code.as_str(),
                                "not_eligible" | "revision_conflict" | "claim_consumed"
                            ) {
                                return Err(error);
                            }
                        }
                    }
                }
                Ok(())
            })();
            if let Err(error) = result {
                if let Ok(mut current) = status.lock() {
                    current.error = Some(error.message);
                }
            }
        }
        // Disabled means no periodic wake, no model worker and no repository scan.
        let event = if enabled {
            receiver.recv_timeout(Duration::from_secs(5))
        } else {
            receiver
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        };
        reconcile = true;
        match event {
            Ok(Event::Snooze(native)) => {
                // The next pass (this loop, straight away) withdraws the banner
                // if macOS has not already taken it down on the button press.
                if let Err(error) = snooze(&host, &native) {
                    if let Ok(mut current) = status.lock() {
                        current.error = Some(error.message);
                    }
                }
            }
            Ok(Event::Activate(native)) => {
                // Two namespaces arrive through one delegate. A session banner
                // has no durable claim behind it — the session either still
                // exists or it does not — so it skips the store entirely and
                // asks the renderer to raise the tab. An activity banner keeps
                // the acknowledged-claim path it always had.
                let session = crate::alerts::native_session_key(&native).map(str::to_owned);
                if session.is_none() {
                    if let Err(error) = activate(&host, &native) {
                        if let Ok(mut current) = status.lock() {
                            current.error = Some(error.message);
                        }
                        continue;
                    }
                }
                let opening = app.clone();
                if let Err(error)=app.run_on_main_thread(move||{
                    if let Some(window)=opening.get_webview_window("main") {
                        for result in [window.show(),window.unminimize(),window.set_focus()] {if let Err(error)=result {log::warn!(target:"workbench","notification window activation: {error}");}}
                    }
                    let emitted = match &session {
                        Some(key) => opening.emit("gitpulse-session-notification-open", key.clone()),
                        None => opening.emit("workbench-notification-open", ()),
                    };
                    if let Err(error)=emitted {log::warn!(target:"workbench","notification activation wake: {error}");}
                }) {if let Ok(mut current)=status.lock(){current.error=Some(error.to_string());}}
            }
            Ok(Event::Wake) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// The [`crate::alerts::Host`] backed by this process's notification centre.
///
/// Session banners share the centre, the delegate and the user's OS-level
/// permission with activity banners — there is only one of each per
/// application — so they are posted through this module rather than through a
/// second bridge that would need its own delegate and its own authorization.
struct SessionHost {
    app: tauri::AppHandle,
}

impl crate::alerts::Host for SessionHost {
    fn submit(&self, native: &str, heading: &str, body: &str, sound: bool) -> Result<bool, String> {
        platform::post(native, heading, body, sound, platform::SESSION_CATEGORY)
            .map_err(|error| error.message)
    }

    /// Both halves are required, and the window half is asked of the window
    /// rather than of the renderer.
    ///
    /// The renderer reports which tab is on screen, which only it knows. It
    /// cannot be trusted to report focus: a webview that has been throttled,
    /// or has stopped running scripts entirely, is exactly the situation in
    /// which a notification matters most, and its last word would have been
    /// "focused". So an unreported tab counts as unattended and the window's
    /// own focus is read natively.
    fn attended(&self, key: &str) -> bool {
        if !crate::alerts::session_is_visible(key) {
            return false;
        }
        self.app
            .get_webview_window("main")
            .is_some_and(|window| window.is_focused().unwrap_or(false))
    }

    /// A failed emit is logged, not retried: the next notice from the session
    /// replaces this one, and the pane's own activity reading still stands.
    fn announce(&self, attention: &crate::alerts::Attention) {
        if let Err(error) = self.app.emit(crate::alerts::ATTENTION_EVENT, attention) {
            log::warn!(target: "alerts", "session attention event: {error}");
        }
    }
}

/// The delivery host for [`crate::alerts::start`].
pub(crate) fn session_host(app: &tauri::AppHandle) -> Arc<dyn crate::alerts::Host> {
    Arc::new(SessionHost { app: app.clone() })
}

pub(super) fn native_request(
    host: &WorkbenchState,
    method: &str,
    input: &str,
) -> Result<Value, WorkbenchError> {
    let args: Value = serde_json::from_str(input).map_err(|_| {
        WorkbenchError::new(
            "invalid_input",
            "Native notification request must be an empty object.",
        )
    })?;
    if !args.as_object().is_some_and(|o| o.is_empty()) {
        return Err(WorkbenchError::new(
            "invalid_input",
            "Native notification request must be an empty object.",
        ));
    }
    if method == "notifications.native.pending" {
        if !host.profile_path()?.exists() {
            return Ok(
                json!({"ok":true,"items":[],"total":0,"shown":0,"has_more":false,"next_cursor":null}),
            );
        }
        return call(host, "notifications.activations.list", json!({"limit":1}));
    }
    if !matches!(
        method,
        "notifications.native.status" | "notifications.native.authorize"
    ) {
        return Err(WorkbenchError::new(
            "unknown_method",
            "Unknown native notification method.",
        ));
    }
    let coordinator = host.0.notifications.get().ok_or_else(|| {
        WorkbenchError::new(
            "unsupported",
            "Native notifications require the desktop application.",
        )
    })?;
    let available = coordinator
        .status
        .lock()
        .map_err(|_| WorkbenchError::new("worker_error", "Notification status lock failed."))?
        .available;
    if available {
        {
            let authorization =
                platform::authorization(method == "notifications.native.authorize")?;
            let mut status = coordinator.status.lock().map_err(|_| {
                WorkbenchError::new("worker_error", "Notification status lock failed.")
            })?;
            status.authorization = authorization;
        }
    }
    let current = coordinator
        .status
        .lock()
        .map_err(|_| WorkbenchError::new("worker_error", "Notification status lock failed."))?
        .clone();
    serde_json::to_value(current).map_err(|e| WorkbenchError::new("protocol_error", e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn activation_is_a_queued_event_distinct_from_wake() {
        assert!(!matches!(Event::Wake, Event::Activate(_)));
        assert!(matches!(
            Event::Activate("gitpulse.0123456789abcdef0123456789abcdef.event-1".into()),
            Event::Activate(native) if notice_id(&native) == Some("event-1")
        ));
    }
    #[test]
    fn native_id_cannot_be_a_path_command_or_another_namespace() {
        assert_eq!(
            notice_id("gitpulse.0123456789abcdef0123456789abcdef.event-42"),
            Some("event-42")
        );
        for id in [
            "event-42",
            "gitpulse.short.event-42",
            "gitpulse.0123456789abcdef0123456789abcdef.event-../repo",
            "gitpulse.0123456789abcdef0123456789abcdef.event-42;sh",
        ] {
            assert_eq!(notice_id(id), None);
        }
    }
    /// The rule that silences a notification overnight is written twice: once
    /// in SQL here, for activity notices, and once in Rust as
    /// `alerts::policy::in_quiet_hours`, for session notices, because a
    /// terminal session exists whether or not a profile database does.
    ///
    /// Two implementations of one rule drift. This drives the real store over
    /// a grid of windows and minutes and fails when they disagree, which a
    /// hand-written table of expectations could never do — it would only prove
    /// each file agreed with itself.
    #[test]
    fn quiet_hours_are_the_same_rule_the_session_notifier_applies() {
        let root = tempfile::tempdir().unwrap();
        let host = WorkbenchState(Arc::new(super::super::Inner {
            path: Some(root.path().join("work.sqlite")),
            ..Default::default()
        }));
        call(&host,"repositories.put",json!({"id":"r","request_id":"r","expected_revision":0,"name":"r","identity_key":"local:r"})).unwrap();
        call(&host,"items.put",json!({"id":"t","request_id":"t","expected_revision":0,"title":"t","repository_ids":["r"],"primary_repository_id":"r"})).unwrap();
        call(
            &host,
            "notifications.settings.put",
            json!({"id":"profile","request_id":"on","expected_revision":1,"enabled":true}),
        )
        .unwrap();
        call(&host,"enhancements.create",json!({"id":"p","request_id":"p","expected_revision":0,"task_id":"t","source_revision":1,"fields":["title"],"provider":"local","model":"configured"})).unwrap();
        call(
            &host,
            "enhancements.complete",
            json!({"id":"p","request_id":"complete","expected_revision":1,"title":"Better"}),
        )
        .unwrap();
        // Outside any quiet window the notice is pending, so an empty page
        // later means "silenced", not "nothing to say".
        assert_eq!(
            call(
                &host,
                "notifications.pending.list",
                json!({"minute_of_day":720,"limit":3})
            )
            .unwrap()["items"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );

        let mut compared = 0;
        let windows: [(u16, u16); 5] = [
            (22 * 60, 7 * 60),
            (9 * 60, 17 * 60),
            (0, 1),
            (1439, 1438),
            (1, 0),
        ];
        // A stepped sweep plus every window's own edges. The sweep alone is
        // what a disagreement usually looks like, but the edges are where it
        // actually lives: a half-open window written closed differs from the
        // store at exactly one minute, and a grid of every 37th minute lands
        // on none of them. Verified by mutation — flipping `<` to `<=` in
        // `in_quiet_hours` left the stepped-only version green.
        let minutes: Vec<u16> = {
            let mut minutes: Vec<u16> = (0..1440u16).step_by(37).collect();
            for (start, end) in windows {
                for edge in [start, end] {
                    minutes.extend([edge.saturating_sub(1), edge, (edge + 1).min(1439)]);
                }
            }
            minutes.sort_unstable();
            minutes.dedup();
            minutes
        };
        // The store's optimistic concurrency means every write needs the
        // revision the previous one produced, which is what the counter is —
        // not an index into `windows`.
        for (revision, (start, end)) in (2u64..).zip(windows) {
            call(&host,"notifications.settings.put",json!({"id":"profile","request_id":format!("q{start}-{end}"),"expected_revision":revision,"enabled":true,"quiet_start":start,"quiet_end":end})).unwrap();
            for &minute in &minutes {
                let page = call(
                    &host,
                    "notifications.pending.list",
                    json!({"minute_of_day": minute, "limit": 3}),
                )
                .unwrap();
                let store_silent = page["items"].as_array().is_some_and(Vec::is_empty);
                assert_eq!(
                    crate::alerts::policy::in_quiet_hours(minute, start, end),
                    store_silent,
                    "window {start}..{end} disagreed at minute {minute}"
                );
                compared += 1;
            }
        }
        assert_eq!(
            compared,
            windows.len() * minutes.len(),
            "the comparison did not cover the grid"
        );
    }

    fn profile_host(root: &std::path::Path) -> WorkbenchState {
        let host = WorkbenchState(Arc::new(super::super::Inner {
            path: Some(root.join("work.sqlite")),
            ..Default::default()
        }));
        call(&host,"repositories.put",json!({"id":"r","request_id":"r","expected_revision":0,"name":"r","identity_key":"local:r"})).unwrap();
        call(
            &host,
            "notifications.settings.put",
            json!({"id":"profile","request_id":"on","expected_revision":1,"enabled":true}),
        )
        .unwrap();
        host
    }

    /// One task with one activity notice that the OS accepted as a banner,
    /// through the real claim/finish path. Returns (notice id, banner id).
    fn bannered(host: &WorkbenchState, task: &str) -> (String, String) {
        call(host,"items.put",json!({"id":task,"request_id":task,"expected_revision":0,"title":task,"repository_ids":["r"],"primary_repository_id":"r"})).unwrap();
        let proposal = format!("{task}-p");
        call(host,"enhancements.create",json!({"id":proposal,"request_id":proposal,"expected_revision":0,"task_id":task,"source_revision":1,"fields":["title"],"provider":"local","model":"configured"})).unwrap();
        let done = call(
            host,
            "enhancements.complete",
            json!({"id":proposal,"request_id":format!("{task}-c"),"expected_revision":1,"title":"Better"}),
        )
        .unwrap();
        let id = format!("event-{}", done["sequence"].as_u64().unwrap());
        let notice = Notice {
            id: id.clone(),
            title: "Task enhancement ready to review".into(),
        };
        let mut native = String::new();
        deliver(host, &notice, 720, false, |n, _, _| {
            native = n.to_owned();
            Ok(true)
        })
        .unwrap();
        assert_eq!(delivery(host, &id).unwrap().state, "submitted");
        (id, native)
    }

    fn act(host: &WorkbenchState, id: &str, action: &str) {
        let revision = attention(host, id).unwrap().revision;
        let mut input = json!({"id":id,"request_id":format!("{action}-{id}"),"expected_revision":revision,"action":action});
        if action == "snooze" {
            input["seconds"] = json!(60);
        }
        call(host, "attention.update", input).unwrap();
    }

    #[test]
    fn the_banner_snooze_is_the_inbox_snooze_and_never_opens_or_rebanners() {
        let root = tempfile::tempdir().unwrap();
        let host = profile_host(root.path());
        let (id, native) = bannered(&host, "t");
        let before = unix_now().unwrap();
        snooze(&host, &native).unwrap();
        let after = unix_now().unwrap();

        let notice = attention(&host, &id).unwrap();
        let until = notice
            .snoozed_until
            .expect("the banner's Snooze reached the inbox row");
        let seconds = i64::from(SNOOZE_SECONDS);
        assert!(
            (before + seconds..=after + seconds).contains(&until),
            "{until}"
        );
        assert!(notice.read_at.is_none() && notice.dismissed_at.is_none());
        // Snooze is not an open: nothing for the review dialog to pick up.
        let activation = call(&host, "notifications.delivery.get", json!({"id":id})).unwrap();
        assert!(activation["item"]["activated_at"].is_null());
        assert_eq!(
            call(&host, "notifications.activations.list", json!({"limit":5})).unwrap()["total"],
            0
        );
        // Snoozed, it is not pending — and once its claim exists it never is
        // again, so the snooze ending brings back the inbox row, not a banner.
        assert_eq!(
            call(
                &host,
                "notifications.pending.list",
                json!({"minute_of_day":720,"limit":3})
            )
            .unwrap()["total"],
            0
        );
        // The banner is then stale, so the withdrawal pass takes it down.
        assert_eq!(
            stale_banners(&host, std::slice::from_ref(&native), after).unwrap(),
            vec![native.clone()]
        );

        // A well-formed banner identity from another profile cannot snooze
        // this profile's notice, and neither can an identity never claimed.
        let foreign = format!("gitpulse.{}.{id}", "f".repeat(32));
        assert_eq!(snooze(&host, &foreign).unwrap_err().code, "invalid_input");
        let unclaimed = native.replace(&id, "event-999999");
        assert_eq!(snooze(&host, &unclaimed).unwrap_err().code, "not_found");
    }

    #[test]
    fn withdrawal_takes_down_exactly_the_banners_resolved_elsewhere() {
        let root = tempfile::tempdir().unwrap();
        let host = profile_host(root.path());
        let read = bannered(&host, "read");
        let dismissed = bannered(&host, "dismissed");
        let snoozed = bannered(&host, "snoozed");
        let changed = bannered(&host, "changed");
        let deleted = bannered(&host, "deleted");
        let current = bannered(&host, "current");

        act(&host, &read.0, "read");
        act(&host, &dismissed.0, "dismiss");
        act(&host, &snoozed.0, "snooze");
        let task = |id: &str| {
            call(&host, "items.get", json!({"id":id})).unwrap()["item"]["revision"]
                .as_u64()
                .unwrap()
        };
        call(&host,"items.put",json!({"id":"changed","request_id":"edit","expected_revision":task("changed"),"title":"edited","repository_ids":["r"],"primary_repository_id":"r"})).unwrap();
        call(
            &host,
            "items.delete",
            json!({"id":"deleted","request_id":"delete","expected_revision":task("deleted")}),
        )
        .unwrap();

        let foreign = format!("gitpulse.{}.{}", "f".repeat(32), current.0);
        let session = "gitpulse-session.terminal-1".to_owned();
        let delivered = vec![
            read.1.clone(),
            dismissed.1.clone(),
            snoozed.1.clone(),
            changed.1.clone(),
            deleted.1.clone(),
            current.1.clone(),
            foreign,
            session,
        ];
        let now = unix_now().unwrap();
        let mut removed = Vec::new();
        let count = withdraw(&host, &delivered, now, |stale| removed = stale.to_vec()).unwrap();
        assert_eq!(count, 5);
        assert_eq!(
            removed,
            vec![
                read.1.clone(),
                dismissed.1.clone(),
                snoozed.1.clone(),
                changed.1.clone(),
                deleted.1.clone()
            ]
        );
        let later = now + 3600;
        assert_eq!(
            stale_banners(&host, &[snoozed.1.clone(), current.1.clone()], later).unwrap(),
            Vec::<String>::new(),
            "an ended snooze is not a reason to withdraw"
        );

        // Nothing resolved → the OS is not called at all.
        let quiet = withdraw(&host, std::slice::from_ref(&current.1), now, |_| {
            panic!("removed a current banner")
        })
        .unwrap();
        assert_eq!(quiet, 0);
    }

    /// Every managed callback the provider adapters capture reaches the banner
    /// queue. The adapters (Manvi `codingagent/codex.go` and `claude.go`) turn
    /// Codex `item/commandExecution/requestApproval`,
    /// `item/fileChange/requestApproval` and Claude `can_use_tool` into a
    /// `permission` request, and Codex `item/tool/requestUserInput` into a
    /// `question`; `serve/managed.go` records each with `decisions.create`.
    /// Those two kinds are the whole vocabulary `decisions.create` accepts, so
    /// both are driven here through the real store against a live run.
    #[test]
    fn every_managed_callback_kind_produces_a_bannerable_notice() {
        let root = tempfile::tempdir().unwrap();
        let host = profile_host(root.path());
        call(&host,"repositories.put",json!({"id":"m","request_id":"m","expected_revision":0,"name":"m","identity_key":"local:/checkout/.git"})).unwrap();
        call(&host,"items.put",json!({"id":"t","request_id":"t","expected_revision":0,"title":"private","repository_ids":["m"],"primary_repository_id":"m"})).unwrap();
        call(&host,"runs.prepare",json!({"id":"run","request_id":"prepare","expected_revision":0,"kind":"managed","task_id":"t","source_revision":1,"repository_id":"m","repository_revision":1,"provider":"codex","permission_mode":"ask","cwd":"/checkout","git_dir":"/checkout/.git","git_common_dir":"/checkout/.git","head_oid":"0123456789abcdef0123456789abcdef01234567","head_ref":"refs/heads/main","max_active_runs":1})).unwrap();
        call(&host,"runs.claim",json!({"id":"run","request_id":"claim","expected_revision":1,"owner_id":"owner","session_id":"session","kind":"managed"})).unwrap();
        let pid = std::process::id();
        let birth = super::super::process_birth::read(pid).unwrap();
        call(&host,"runs.started",json!({"id":"run","request_id":"started","expected_revision":2,"owner_id":"owner","session_id":"session","process_id":pid,"process_start":birth})).unwrap();

        let deadline = unix_now().unwrap() + 300;
        let mut expected = Vec::new();
        for (n, kind, title) in [
            (1, "permission", "Coding agent needs permission"),
            (2, "question", "Coding agent needs input"),
        ] {
            let created = call(&host,"decisions.create",json!({"id":format!("d{n}"),"request_id":format!("d{n}"),"expected_revision":0,"run_id":"run","owner_id":"owner","session_id":"session","provider_thread_id":"thread","provider_turn_id":"turn","protocol_request_id":format!("wire-{n}"),"kind":kind,"payload":"{}","payload_digest":"a".repeat(64),"deadline":deadline})).unwrap();
            expected.push((
                format!("event-{}", created["sequence"].as_u64().unwrap()),
                title,
            ));
        }
        let page = call(
            &host,
            "notifications.pending.list",
            json!({"minute_of_day":720,"limit":3}),
        )
        .unwrap();
        let mut queued: Vec<(String, String)> = page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| {
                (
                    n["id"].as_str().unwrap().to_owned(),
                    n["title"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        queued.sort();
        let mut expected: Vec<(String, String)> = expected
            .into_iter()
            .map(|(id, t)| (id, t.to_owned()))
            .collect();
        let permission = expected[0].0.clone();
        expected.sort();
        assert_eq!(queued, expected);

        // Answering the request in the inspector resolves its banner: the
        // decision's revision moves, so its notice stops being `current` and
        // the withdrawal pass takes the banner down.
        let mut native = String::new();
        let notice = Notice {
            id: permission.clone(),
            title: "Coding agent needs permission".into(),
        };
        deliver(&host, &notice, 720, false, |n, _, _| {
            native = n.to_owned();
            Ok(true)
        })
        .unwrap();
        let now = unix_now().unwrap();
        assert!(stale_banners(&host, std::slice::from_ref(&native), now)
            .unwrap()
            .is_empty());
        call(&host,"decisions.decide",json!({"id":"d1","request_id":"deny","expected_revision":1,"payload_digest":"a".repeat(64),"decision":"deny"})).unwrap();
        assert_eq!(
            stale_banners(&host, std::slice::from_ref(&native), now).unwrap(),
            vec![native]
        );
    }

    #[test]
    fn timeout_retains_claim_and_a_second_attempt_never_calls_the_os() {
        let root = tempfile::tempdir().unwrap();
        let host = WorkbenchState(Arc::new(super::super::Inner {
            path: Some(root.path().join("work.sqlite")),
            ..Default::default()
        }));
        call(&host,"repositories.put",json!({"id":"r","request_id":"r","expected_revision":0,"name":"r","identity_key":"local:r"})).unwrap();
        call(&host,"items.put",json!({"id":"t","request_id":"t","expected_revision":0,"title":"private","repository_ids":["r"],"primary_repository_id":"r"})).unwrap();
        call(
            &host,
            "notifications.settings.put",
            json!({"id":"profile","request_id":"enabled","expected_revision":1,"enabled":true}),
        )
        .unwrap();
        call(&host,"enhancements.create",json!({"id":"p","request_id":"p","expected_revision":0,"task_id":"t","source_revision":1,"fields":["title"],"provider":"local","model":"configured"})).unwrap();
        let result = call(
            &host,
            "enhancements.complete",
            json!({"id":"p","request_id":"complete","expected_revision":1,"title":"Better"}),
        )
        .unwrap();
        let id = format!("event-{}", result["sequence"].as_u64().unwrap());
        let notice = Notice {
            id: id.clone(),
            title: "Task enhancement ready to review".into(),
        };
        assert_eq!(
            deliver(&host, &notice, 720, false, |_, _, _| Err(
                WorkbenchError::new("timeout", "OS callback lost")
            ))
            .unwrap_err()
            .code,
            "timeout"
        );
        assert_eq!(delivery(&host, &id).unwrap().state, "uncertain");
        assert_eq!(
            deliver(&host, &notice, 720, false, |_, _, _| panic!(
                "duplicate OS submission"
            ))
            .unwrap_err()
            .code,
            "claim_consumed"
        );
    }
}
