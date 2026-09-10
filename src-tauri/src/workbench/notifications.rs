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
}
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
fn activate(host: &WorkbenchState, native: &str) -> Result<(), WorkbenchError> {
    let id = notice_id(native)
        .ok_or_else(|| WorkbenchError::new("invalid_input", "Invalid notification identity."))?;
    for _ in 0..3 {
        let current = delivery(host, id)?;
        if current.native_id != native {
            return Err(WorkbenchError::new(
                "invalid_input",
                "Notification belongs to a different profile.",
            ));
        }
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
            Ok(Event::Activate(native)) => {
                if let Err(error) = activate(&host, &native) {
                    if let Ok(mut current) = status.lock() {
                        current.error = Some(error.message);
                    }
                    continue;
                }
                let opening = app.clone();
                if let Err(error)=app.run_on_main_thread(move||{
                    if let Some(window)=opening.get_webview_window("main") {
                        for result in [window.show(),window.unminimize(),window.set_focus()] {if let Err(error)=result {log::warn!(target:"workbench","notification window activation: {error}");}}
                    }
                    if let Err(error)=opening.emit("workbench-notification-open",()) {log::warn!(target:"workbench","notification activation wake: {error}");}
                }) {if let Ok(mut current)=status.lock(){current.error=Some(error.to_string());}}
            }
            Ok(Event::Wake) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
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
