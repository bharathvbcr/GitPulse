//! The local socket an agent hook reports through.
//!
//! Reading the PTY tells GitPulse *that* a session wants attention. It cannot
//! tell it *why*, because the conventions that carry a bell carry nothing else.
//! Claude Code, by contrast, fires a `Notification` hook whose matcher already
//! names the reason — `permission_prompt`, `idle_prompt`, `agent_completed` —
//! and a hook is a separate process with no way to write to the terminal it
//! was spawned under. So it needs a door.
//!
//! ## What this widens, precisely
//!
//! A `SOCK_STREAM` endpoint in GitPulse's own config directory, present only
//! while the app is running and only while the user has the bridge enabled.
//! Anything a peer sends can cause exactly one thing: a notification banner
//! whose heading GitPulse writes, whose body is stripped of control characters
//! and truncated, and whose activation focuses a GitPulse tab. It cannot open
//! a file, run a command, or read anything back.
//!
//! Every defence is here because the peer is not authenticated by anything
//! stronger than being the same user:
//!
//! * The directory is created `0700` and refused if any ancestor is a symlink;
//!   the socket itself is made `0600` inside it, without touching the
//!   process-wide umask (`crate::local_socket`).
//! * Every connection's peer credentials are read and refused unless the uid
//!   matches this process's. On a Unix where neither `getpeereid` nor
//!   `SO_PEERCRED` is available the socket is **not bound at all** — a check
//!   that cannot run must not be reported as a check that passed.
//! * Reads are bounded in bytes and in wall-clock time, one connection at a
//!   time, and every field is validated against a closed vocabulary before it
//!   reaches the hub, where the global rate limit applies to it as well.
//!
//! On Windows the socket is not offered. The hook path there is a documented
//! gap rather than a silently missing feature.

use super::{Notice, Origin};
#[cfg(unix)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(unix)]
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};

/// The file name inside GitPulse's config directory.
pub const SOCKET_NAME: &str = "agent-notify.sock";

/// The environment variable a launched agent's hooks read to find the socket.
pub const SOCKET_ENV: &str = "GITPULSE_NOTIFY_SOCKET";

/// The environment variable that tells a hook which GitPulse PTY it is under.
pub const SESSION_ENV: &str = "GITPULSE_SESSION_ID";

/// Largest report accepted from one connection.
pub const MAX_REPORT_BYTES: usize = 8 * 1024;

/// Wall-clock ceiling on one connection.
///
/// Only the Unix accept loop reads this. On Windows the socket is not offered,
/// and a constant with no caller fails `clippy -D warnings` there.
#[cfg(unix)]
const CONNECTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug, Clone, Default)]
pub struct BridgeStatus {
    pub listening: bool,
    pub path: Option<String>,
    pub error: Option<String>,
}

struct State {
    listening: AtomicBool,
    /// Bumped to retire a running accept loop. A loop whose generation is no
    /// longer current unlinks its socket and exits.
    generation: AtomicU64,
    path: Mutex<Option<String>>,
    error: Mutex<Option<String>>,
    /// Held for the whole of `start` and of `stop`. Without it a `stop` that
    /// lands between `start`'s generation bump and its `listening` store leaves
    /// the flag set and a socket file behind with no loop to serve it, and a
    /// retired loop no longer repairs that (it clears `listening` only while
    /// current).
    control: Mutex<()>,
    /// The current accept loop's wake pipe. Written by whoever retires the
    /// loop, so a loop that waits with no timeout still looks at `generation`.
    #[cfg(unix)]
    wake: Mutex<Option<Arc<crate::local_socket::Wake>>>,
    /// `(generation, times that generation's accept loop came out of its
    /// wait)`, for whatever reason. A loop adds to it only while its own
    /// generation is the one recorded, so a retired predecessor still draining
    /// its exit wake cannot count against its successor. An idle bridge should
    /// not move this. Test-only: the loop pays nothing for it in production.
    #[cfg(all(unix, test))]
    accept_wakes: Mutex<(u64, u64)>,
    /// Accept loops that have started and not yet finished their exit work.
    #[cfg(unix)]
    live_loops: AtomicUsize,
}

fn state() -> &'static State {
    static STATE: OnceLock<State> = OnceLock::new();
    STATE.get_or_init(|| State {
        listening: AtomicBool::new(false),
        generation: AtomicU64::new(0),
        path: Mutex::new(None),
        error: Mutex::new(None),
        control: Mutex::new(()),
        #[cfg(unix)]
        wake: Mutex::new(None),
        #[cfg(all(unix, test))]
        accept_wakes: Mutex::new((0, 0)),
        #[cfg(unix)]
        live_loops: AtomicUsize::new(0),
    })
}

pub fn status() -> BridgeStatus {
    let state = state();
    BridgeStatus {
        listening: state.listening.load(Ordering::SeqCst),
        path: state.path.lock().ok().and_then(|p| p.clone()),
        error: state.error.lock().ok().and_then(|e| e.clone()),
    }
}

fn set_error(message: Option<String>) {
    if let Ok(mut slot) = state().error.lock() {
        *slot = message;
    }
}

/// The socket path this build would use, whether or not it is bound.
pub fn socket_path() -> Option<std::path::PathBuf> {
    Some(crate::tool_config::default_config_dir()?.join(SOCKET_NAME))
}

/// Brings the listener into line with the user's setting.
///
/// Called at startup and again whenever the preference changes, so turning the
/// bridge off actually removes the endpoint rather than leaving it bound and
/// ignoring what arrives.
pub fn apply(enabled: bool) {
    apply_at(enabled, socket_path());
}

/// [`apply`] against a named path, so the accept loop can be driven end to end
/// in a test without a real configuration directory.
pub(crate) fn apply_at(enabled: bool, path: Option<std::path::PathBuf>) {
    if !enabled {
        stop();
        return;
    }
    start(path);
}

fn stop() {
    let state = state();
    let _control = state
        .control
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    stop_locked(state);
}

/// `stop` for a caller already holding `State::control`.
fn stop_locked(state: &State) {
    state.generation.fetch_add(1, Ordering::SeqCst);
    #[cfg(unix)]
    wake_retired_loop(state);
    state.listening.store(false, Ordering::SeqCst);
    // Remove the path we actually bound, which is not always the one
    // `socket_path` names today: a test binds elsewhere, and a configuration
    // directory can move under a running app.
    let bound = state.path.lock().ok().and_then(|mut slot| slot.take());
    set_error(None);
    #[cfg(unix)]
    if let Some(path) = bound {
        let _ = std::fs::remove_file(path);
    }
    #[cfg(not(unix))]
    let _ = bound;
}

/// Tells the loop a generation bump just retired to look at it. Called after
/// the bump, so the loop that wakes sees a generation that is no longer its
/// own; a byte written before it starts waiting is still there when it does.
#[cfg(unix)]
fn wake_retired_loop(state: &State) {
    let wake = state.wake.lock().ok().and_then(|mut slot| slot.take());
    if let Some(wake) = wake {
        wake.notify();
    }
}

#[cfg(not(unix))]
fn start(_path: Option<std::path::PathBuf>) {
    state().listening.store(false, Ordering::SeqCst);
    set_error(Some(
        "Agent hook reports need a Unix socket, which this platform does not offer.".into(),
    ));
}

#[cfg(unix)]
fn start(path: Option<std::path::PathBuf>) {
    let state = state();
    let _control = state
        .control
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if state.listening.load(Ordering::SeqCst) {
        return;
    }
    let generation = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
    #[cfg(test)]
    {
        *state
            .accept_wakes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = (generation, 0);
    }
    wake_retired_loop(state);
    // Before the socket, so a listener is never bound that cannot be told to
    // stop. There is no polling fallback: the cost of one would be paid by
    // every idle second of the app, so a failure is shown instead.
    let wake = match crate::local_socket::Wake::new() {
        Ok(wake) => Arc::new(wake),
        Err(error) => {
            state.listening.store(false, Ordering::SeqCst);
            set_error(Some(format!(
                "Hook bridge could not create its wake pipe: {error}"
            )));
            return;
        }
    };
    let listener = match bind(path) {
        Ok((listener, path)) => {
            if let Ok(mut slot) = state.path.lock() {
                *slot = Some(path.display().to_string());
            }
            set_error(None);
            listener
        }
        Err(error) => {
            state.listening.store(false, Ordering::SeqCst);
            set_error(Some(error));
            return;
        }
    };
    state.listening.store(true, Ordering::SeqCst);
    if let Ok(mut slot) = state.wake.lock() {
        *slot = Some(wake.clone());
    }
    let spawned =
        crate::supervisor::spawn_once("gitpulse-alert-bridge", "alert bridge", move || {
            accept_loop(listener, generation, &wake)
        });
    if let Err(error) = spawned {
        state.listening.store(false, Ordering::SeqCst);
        stop_locked(state);
        set_error(Some(format!("Hook bridge could not start: {error}")));
    }
}

/// The one hardened bind every GitPulse socket shares; see `local_socket`.
#[cfg(unix)]
fn bind(
    path: Option<std::path::PathBuf>,
) -> Result<(std::os::unix::net::UnixListener, std::path::PathBuf), String> {
    let path = path.ok_or("Application config directory unavailable")?;
    let listener = crate::local_socket::bind_private(&path, "notification socket")?;
    Ok((listener, path))
}

/// Counts one return from the wait against `generation`, if it is still the
/// generation being counted.
#[cfg(all(unix, test))]
fn count_wake(state: &State, generation: u64) {
    tests::hold_after_wait();
    let mut counted = state
        .accept_wakes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if counted.0 == generation {
        counted.1 += 1;
    }
}

#[cfg(unix)]
fn accept_loop(
    listener: std::os::unix::net::UnixListener,
    generation: u64,
    wake: &crate::local_socket::Wake,
) {
    use std::os::unix::io::AsRawFd;
    /// Counts the loop out only after its exit work, the unlink included.
    struct Running(&'static State);
    impl Drop for Running {
        fn drop(&mut self) {
            self.0.live_loops.fetch_sub(1, Ordering::SeqCst);
        }
    }
    let state = state();
    state.live_loops.fetch_add(1, Ordering::SeqCst);
    let _running = Running(state);
    // SAFETY: `getuid` reads this process's real user id and cannot fail.
    let own = unsafe { libc::getuid() };
    loop {
        if state.generation.load(Ordering::SeqCst) != generation {
            break;
        }
        // No timeout: nothing here needs to run unless a connection arrives or
        // `stop`/`start` retires this generation and writes to the wake pipe.
        let ready =
            crate::local_socket::wait_readable_or_woken(listener.as_raw_fd(), wake.reader_fd());
        #[cfg(test)]
        count_wake(state, generation);
        if ready.woken {
            wake.drain();
        }
        if !ready.listener {
            continue;
        }
        // The accepted descriptor is made close-on-exec only after `accept`
        // returns; the inheritance lock keeps a concurrent spawn from copying
        // it in between. Non-blocking, so the lock covers one syscall.
        match crate::procguard::with_inheritance_lock(|| listener.accept()) {
            Ok((stream, _)) => {
                if crate::local_socket::peer_uid(&stream) != Some(own) {
                    super::count_bridge_rejection();
                    continue;
                }
                handle(stream);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(error) => {
                if state.generation.load(Ordering::SeqCst) == generation {
                    set_error(Some(format!("Hook bridge stopped accepting: {error}")));
                }
                break;
            }
        }
    }
    // Only if this loop is still the current one. `listening` and the error
    // now belong to the successor of a loop a restart retired, and a loop that
    // exits promptly would otherwise switch the new listener off.
    let current = state.generation.load(Ordering::SeqCst) == generation;
    if current {
        state
            .listening
            .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
            .ok();
    }
    let bound = listener
        .local_addr()
        .ok()
        .and_then(|addr| addr.as_pathname().map(std::path::Path::to_path_buf));
    drop(listener);
    // A loop retired by a restart must not delete the socket its successor has
    // already bound.
    if state.generation.load(Ordering::SeqCst) == generation {
        if let Some(path) = bound {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(unix)]
fn handle(stream: std::os::unix::net::UnixStream) {
    use crate::deadline::DeadlineStream;
    use std::io::{Read, Write};
    let _ = stream.set_nonblocking(false);
    // One deadline for the whole exchange, not a timeout per read: a peer that
    // keeps a byte in flight inside each read timeout would otherwise hold
    // this loop, and every other agent's report, for as long as it likes.
    let mut stream = DeadlineStream::new(stream, std::time::Instant::now() + CONNECTION_TIMEOUT);
    let mut bytes = Vec::new();
    let read = (&mut stream)
        .take(MAX_REPORT_BYTES as u64 + 1)
        .read_to_end(&mut bytes);
    let accepted = match read {
        Ok(_) if bytes.len() <= MAX_REPORT_BYTES => match parse_report(&bytes) {
            Ok(notice) => {
                super::offer(notice);
                true
            }
            Err(_) => false,
        },
        _ => false,
    };
    if !accepted {
        super::count_bridge_rejection();
    }
    let reply = if accepted {
        b"{\"ok\":true}\n".as_slice()
    } else {
        b"{\"ok\":false}\n".as_slice()
    };
    let _ = stream.write_all(reply);
    let _ = stream.flush();
}

/// What an event asks of the user. The renderer's `ATTENTION_EVENTS` maps each
/// one to its own kind, and `sessionActivity.test.ts` reads this table to fail
/// if the two ever disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// The agent is blocked on the user: a permission, a question, input.
    Ask,
    /// The agent stopped because something failed.
    Error,
    /// The agent finished and is waiting to be read.
    Finished,
    /// Whatever the session was asking for has been answered.
    Clear,
}

/// What GitPulse does with an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The host chose to notify: a banner, subject to policy, and a state.
    Banner,
    /// A state the board shows at once, never a banner by itself. The host
    /// decides when a moment is worth interrupting someone for, and these
    /// events fire before (or instead of) that decision.
    State,
    /// Ends whatever stands, if it is what this event answers.
    Resolve,
}

/// One event an agent hook may report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    /// Spelled as the plugin manifest passes it.
    pub name: &'static str,
    /// How GitPulse words it.
    pub phrase: &'static str,
    pub need: Need,
    pub role: Role,
}

/// Events an agent hook may report, and how GitPulse words each one.
///
/// A closed vocabulary rather than free text: the reason is GitPulse's
/// sentence, and a peer that could choose it could write a banner that looks
/// like it came from somewhere else.
///
/// The banner events are Claude Code's `Notification` matchers, spelled exactly
/// as the host spells them, because the plugin manifest routes one hook entry
/// per matcher and `hooks::tests` fails if the two lists stop matching. `error`
/// is `StopFailure`, which has its own matchers for *why* the API call failed
/// and is registered without one: a turn that ended on an error is worth the
/// same single sentence whichever error it was.
///
/// The rest come from lifecycle hooks and exist because a `Notification` says
/// when to interrupt, not what the session is doing. `permission_prompt` fires
/// about six seconds after the dialog appears; `PermissionRequest` fires as it
/// appears and names the tool. And nothing in `Notification` ever says the
/// question was answered — without `UserPromptSubmit`, `PostToolUse` and
/// `SessionEnd`, an agent approved from anywhere but its own GitPulse tab kept
/// "needs your permission" until someone typed into that tab.
pub const EVENTS: &[Event] = &[
    Event {
        name: "permission_prompt",
        phrase: "needs your permission",
        need: Need::Ask,
        role: Role::Banner,
    },
    Event {
        name: "idle_prompt",
        phrase: "is waiting for you",
        need: Need::Ask,
        role: Role::Banner,
    },
    Event {
        name: "agent_needs_input",
        phrase: "needs your input",
        need: Need::Ask,
        role: Role::Banner,
    },
    Event {
        name: "agent_completed",
        phrase: "finished its work",
        need: Need::Finished,
        role: Role::Banner,
    },
    Event {
        name: "elicitation_dialog",
        phrase: "is asking a question",
        need: Need::Ask,
        role: Role::Banner,
    },
    Event {
        name: "elicitation_url_dialog",
        phrase: "is asking you to open a link",
        need: Need::Ask,
        role: Role::Banner,
    },
    Event {
        name: "error",
        phrase: "stopped on an error",
        need: Need::Error,
        role: Role::Banner,
    },
    // `PermissionRequest`. Also fires when another hook is about to approve
    // the call itself, which is why it is never a banner: the `Notification`
    // that follows six seconds later, only if the dialog is still up, is.
    Event {
        name: "permission_request",
        phrase: "needs your permission",
        need: Need::Ask,
        role: Role::State,
    },
    // `Stop`: the turn ended. `idle_prompt` follows a minute later if nobody
    // answers, and that one may interrupt.
    Event {
        name: "turn_finished",
        phrase: "finished its turn",
        need: Need::Finished,
        role: Role::State,
    },
    // `UserPromptSubmit`: the user answered, from wherever they answered.
    Event {
        name: "prompt_submitted",
        phrase: "is working again",
        need: Need::Clear,
        role: Role::Resolve,
    },
    // `PostToolUse` / `PostToolUseFailure`: a tool ran, so the call it names
    // is no longer waiting for approval.
    Event {
        name: "tool_finished",
        phrase: "is working again",
        need: Need::Clear,
        role: Role::Resolve,
    },
    // `SessionEnd`, or the PTY's process exiting: nobody can answer a
    // question from a session that is gone.
    Event {
        name: "session_ended",
        phrase: "ended its session",
        need: Need::Clear,
        role: Role::Resolve,
    },
];

/// The event named `name`, if it is one.
pub fn event(name: &str) -> Option<&'static Event> {
    EVENTS.iter().find(|event| event.name == name)
}

/// The longest tool-call fingerprint a report may carry.
pub const MAX_SUBJECT_CHARS: usize = 32;

fn valid_subject(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SUBJECT_CHARS
        && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Agent identities a report may claim, and their display names.
pub const AGENTS: &[(&str, &str)] = &[
    ("claude", "Claude Code"),
    ("codex", "Codex"),
    ("manvi", "Manvi"),
    ("grok", "Grok"),
    ("agy", "Antigravity"),
    ("cursor", "Cursor"),
];

pub(crate) fn valid_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Turns one report into a notice, or refuses it.
///
/// Every refusal is a plain `Err` because the peer learns nothing from the
/// distinction and the counter is what a person reads.
pub fn parse_report(bytes: &[u8]) -> Result<Notice, &'static str> {
    if bytes.len() > MAX_REPORT_BYTES {
        return Err("oversized");
    }
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| "not json")?;
    let object = value.as_object().ok_or("not an object")?;
    if object.len() > 12 {
        return Err("too many fields");
    }
    if object.get("v").and_then(serde_json::Value::as_u64) != Some(1) {
        return Err("unsupported protocol version");
    }
    let text = |key: &str| -> Option<&str> { object.get(key).and_then(serde_json::Value::as_str) };

    let event = event(text("event").ok_or("no event")?).ok_or("unknown event")?;
    // Which tool call a permission or a tool result is about, as a digest the
    // hook computed. Optional; a malformed one is refused rather than dropped,
    // because a report that cannot be matched to its request would clear the
    // wrong one or none.
    let subject = match object.get("subject") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(
            value
                .as_str()
                .filter(|s| valid_subject(s))
                .ok_or("malformed subject")?
                .to_owned(),
        ),
    };
    let subagent = match object.get("subagent") {
        None | Some(serde_json::Value::Null) => false,
        Some(value) => value.as_bool().ok_or("malformed subagent")?,
    };
    let agent = text("agent").unwrap_or("claude");
    let label = AGENTS
        .iter()
        .find(|(name, _)| *name == agent)
        .map(|(_, display)| *display)
        .ok_or("unknown agent")?;
    // A state or a resolution is about a session GitPulse can show. With no
    // GitPulse session there is nothing for it to change, and accepting it
    // would only spend the rate limit.
    let session_named = text("session").is_some_and(valid_key);
    if event.role != Role::Banner && !session_named {
        return Err("state report outside a session");
    }

    // The GitPulse PTY this hook is running under, when it is running under
    // one. Keying to it is what lets "you are looking at this tab" suppress
    // the banner, and what makes a later report replace the earlier one.
    let session = text("session").filter(|s| valid_key(s));
    let place = text("cwd")
        .filter(|cwd| cwd.len() <= 4096)
        .and_then(|cwd| {
            std::path::Path::new(cwd)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .map(|name| super::scan::sanitize(&name))
        .filter(|name| !name.is_empty());
    let detail = text("message")
        .map(super::scan::sanitize)
        .filter(|m| !m.is_empty());

    let key = match session {
        Some(session) => session.to_owned(),
        None => {
            // No GitPulse session: key by agent and place so an external
            // Claude Code in a given checkout replaces its own banner rather
            // than stacking one per turn.
            let place = place.as_deref().unwrap_or("session");
            format!(
                "hook-{agent}-{}",
                place
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric())
                    .take(48)
                    .collect::<String>()
            )
        }
    };

    Ok(Notice {
        key,
        origin: Origin::Hook,
        label: label.to_owned(),
        place,
        event: Some(event.name),
        detail,
        // A hook only exists because an agent installed it.
        is_agent: true,
        channel: "hook",
        subject,
        subagent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(json: &str) -> Result<Notice, &'static str> {
        parse_report(json.as_bytes())
    }

    #[test]
    fn a_well_formed_claude_report_becomes_a_notice() {
        let notice =
            report(r#"{"v":1,"event":"permission_prompt","agent":"claude","session":"term-9-1a","cwd":"/Users/me/GitPulse","message":"Bash(rm -rf)"}"#)
                .unwrap();
        assert_eq!(notice.key, "term-9-1a");
        assert_eq!(notice.label, "Claude Code");
        assert_eq!(notice.place.as_deref(), Some("GitPulse"));
        assert_eq!(notice.reason(), Some("needs your permission"));
        assert_eq!(notice.detail.as_deref(), Some("Bash(rm -rf)"));
        assert!(notice.is_agent);
    }

    #[test]
    fn the_reason_is_gitpulses_sentence_not_the_peers() {
        // A peer supplying its own wording for `event` is refused outright,
        // and `message` — which it does control — never becomes the heading.
        assert_eq!(
            report(r#"{"v":1,"event":"Your bank needs you","agent":"claude"}"#),
            Err("unknown event")
        );
        let notice = report(r#"{"v":1,"event":"error","message":"Sign in at evil.example"}"#)
            .expect("valid report");
        assert_eq!(notice.label, "Claude Code");
        assert_eq!(notice.reason(), Some("stopped on an error"));
    }

    #[test]
    fn every_malformed_shape_is_refused_rather_than_guessed() {
        for json in [
            "",
            "[]",
            "null",
            r#""text""#,
            "{}",
            r#"{"event":"error"}"#,
            r#"{"v":2,"event":"error"}"#,
            r#"{"v":1}"#,
            r#"{"v":1,"event":"error","agent":"unknown-agent"}"#,
            r#"{"v":1,"event":""}"#,
        ] {
            assert!(report(json).is_err(), "accepted {json}");
        }
    }

    #[test]
    fn an_oversized_report_is_refused_before_it_is_parsed() {
        let padding = "x".repeat(MAX_REPORT_BYTES);
        let json = format!(r#"{{"v":1,"event":"error","message":"{padding}"}}"#);
        assert_eq!(parse_report(json.as_bytes()), Err("oversized"));
    }

    #[test]
    fn a_field_storm_is_refused() {
        let mut json = String::from(r#"{"v":1,"event":"error""#);
        for i in 0..40 {
            json.push_str(&format!(r#","f{i}":1"#));
        }
        json.push('}');
        assert_eq!(parse_report(json.as_bytes()), Err("too many fields"));
    }

    #[test]
    fn a_session_id_that_is_not_one_is_ignored_rather_than_trusted() {
        for bad in ["../../etc", "term 1", "", &"t".repeat(200)] {
            let json = format!(
                r#"{{"v":1,"event":"error","agent":"codex","session":"{}","cwd":"/tmp/work"}}"#,
                bad.replace('\\', "")
            );
            let notice = parse_report(json.as_bytes()).expect("valid report");
            assert!(
                notice.key.starts_with("hook-codex-"),
                "trusted {bad}: {}",
                notice.key
            );
            assert!(super::super::native_session_key(&format!(
                "{}{}",
                super::super::NATIVE_PREFIX,
                notice.key
            ))
            .is_some());
        }
    }

    #[test]
    fn control_characters_never_survive_into_a_banner() {
        let notice =
            report("{\"v\":1,\"event\":\"error\",\"message\":\"line\\u0000one\\u001b[31m\\ntwo\"}")
                .unwrap();
        let detail = notice.detail.unwrap();
        assert!(!detail.contains('\u{1b}') && !detail.contains('\0') && !detail.contains('\n'));
    }

    /* ── the live socket ──────────────────────────────────────────────────── */

    /// The listener is process-global, so only one test may own it at a time.
    #[cfg(unix)]
    static LISTENER: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(unix)]
    fn own_listener() -> std::sync::MutexGuard<'static, ()> {
        LISTENER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A temporary directory short enough to hold a socket.
    ///
    /// `sun_path` is 104 bytes on macOS and the system temp directory alone
    /// spends half of that, so the usual `tempdir()` under a nested path is not
    /// available here. This is the same limit production hits with a long home
    /// directory, which `bind` reports rather than letting it surface as
    /// `ENAMETOOLONG`.
    #[cfg(unix)]
    struct ShortDir(std::path::PathBuf);

    #[cfg(unix)]
    impl ShortDir {
        fn new(tag: &str) -> Self {
            // Canonicalised: `/tmp` is a symlink to `/private/tmp` on macOS,
            // and the bridge refuses a symlinked ancestor — correctly, which is
            // why the test has to resolve it rather than the bridge relax.
            let base = std::fs::canonicalize("/tmp").expect("a temporary directory");
            let path = base.join(format!("gpa-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("scratch directory");
            Self(path)
        }
        fn socket(&self) -> std::path::PathBuf {
            self.0.join(SOCKET_NAME)
        }
    }

    #[cfg(unix)]
    impl Drop for ShortDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    fn send(path: &std::path::Path, payload: &[u8]) -> String {
        use std::io::{Read, Write};
        let mut stream = std::os::unix::net::UnixStream::connect(path).expect("connect");
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        // Tolerated, not asserted: for a report past the cap the server stops
        // reading, answers and closes, so our write can legitimately meet a
        // peer that has already gone. That is the bound working.
        let _ = stream.write_all(payload);
        let _ = stream.shutdown(std::net::Shutdown::Write);
        let mut reply = String::new();
        let _ = stream.read_to_string(&mut reply);
        reply
    }

    #[cfg(unix)]
    fn wait_until(mut ready: impl FnMut() -> bool) -> bool {
        for _ in 0..100 {
            if ready() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    }

    #[cfg(unix)]
    #[test]
    fn a_live_socket_accepts_a_real_report_and_refuses_everything_else() {
        use std::os::unix::fs::PermissionsExt;
        let _own = own_listener();
        let dir = ShortDir::new("live");
        apply_at(true, Some(dir.socket()));
        assert!(status().listening, "{:?}", status().error);
        assert_eq!(
            status().path.as_deref(),
            Some(dir.socket().to_str().unwrap())
        );

        // The socket is readable and writable by this user and nobody else,
        // and so is the directory holding it.
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir.socket()) & 0o077, 0, "the socket is not private");
        assert_eq!(mode(&dir.0), 0o700, "the directory is not private");

        let report = br#"{"v":1,"event":"permission_prompt","agent":"claude","session":"term-1","cwd":"/w/GitPulse"}"#;
        assert_eq!(send(&dir.socket(), report).trim(), r#"{"ok":true}"#);

        for bad in [
            &b"not json"[..],
            &b"{}"[..],
            br#"{"v":1,"event":"launch a missile"}"#,
            // One byte past the cap: large enough to be refused, small enough
            // that the whole thing still fits in a socket buffer, so what is
            // being tested is the limit rather than a write race.
            &vec![b'x'; MAX_REPORT_BYTES + 1][..],
        ] {
            assert_eq!(
                send(&dir.socket(), bad).trim(),
                r#"{"ok":false}"#,
                "accepted {} bytes of nonsense",
                bad.len()
            );
        }

        // Turning it off removes the endpoint rather than leaving it bound.
        apply_at(false, None);
        assert!(!status().listening);
        assert!(
            wait_until(|| !dir.socket().exists()),
            "the socket outlived the setting that turned it off"
        );
        assert!(std::os::unix::net::UnixStream::connect(dir.socket()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_stale_socket_is_replaced_but_a_regular_file_is_never_deleted() {
        let _own = own_listener();
        let dir = ShortDir::new("stale");

        // A socket left by a killed process: replaced, so the bridge comes back.
        let orphan = crate::procguard::with_inheritance_lock(|| {
            std::os::unix::net::UnixListener::bind(dir.socket())
        })
        .unwrap();
        drop(orphan);
        assert!(dir.socket().exists());
        apply_at(true, Some(dir.socket()));
        assert!(status().listening, "{:?}", status().error);
        apply_at(false, None);
        assert!(wait_until(|| !dir.socket().exists()));

        // Something that is not a socket: refused, and left alone.
        std::fs::write(dir.socket(), b"someone else's file").unwrap();
        apply_at(true, Some(dir.socket()));
        assert!(!status().listening);
        assert!(status().error.unwrap_or_default().contains("not a socket"));
        assert_eq!(
            std::fs::read(dir.socket()).unwrap(),
            b"someone else's file",
            "the bridge deleted a file it did not create"
        );
        apply_at(false, None);
    }

    #[cfg(unix)]
    #[test]
    fn a_path_too_long_for_the_platform_is_reported_as_that() {
        let _own = own_listener();
        let long = std::path::PathBuf::from(format!("/tmp/{}/{SOCKET_NAME}", "n".repeat(200)));
        apply_at(true, Some(long));
        assert!(!status().listening);
        let error = status().error.unwrap_or_default();
        assert!(error.contains("bytes and this system allows"), "{error}");
        apply_at(false, None);
    }

    #[cfg(unix)]
    #[test]
    fn a_flood_of_connections_neither_blocks_nor_grows_without_bound() {
        let _own = own_listener();
        let dir = ShortDir::new("flood");
        apply_at(true, Some(dir.socket()));
        assert!(status().listening, "{:?}", status().error);
        let started = std::time::Instant::now();
        for index in 0..200 {
            let report = format!(
                r#"{{"v":1,"event":"idle_prompt","agent":"claude","session":"term-{index}"}}"#
            );
            assert_eq!(
                send(&dir.socket(), report.as_bytes()).trim(),
                r#"{"ok":true}"#
            );
        }
        // Serialised on purpose — one connection at a time is the bound — but
        // each must be answered as it arrives. The budget is ~25 ms per report
        // against a measured ~0.1 ms: loose enough to survive a loaded machine,
        // tight enough to fail the regression it was written for, which was a
        // sleep between accepts that cost every report 200 ms and made this
        // loop take 40 seconds.
        assert!(
            started.elapsed()
                < crate::test_support::coverage_relaxed(std::time::Duration::from_secs(5)),
            "200 reports took {:?}",
            started.elapsed()
        );
        // A peer that connects and says nothing is bounded by the read timeout
        // and does not wedge the loop: the next report still gets through.
        let quiet = std::os::unix::net::UnixStream::connect(dir.socket()).unwrap();
        drop(quiet);
        assert_eq!(
            send(&dir.socket(), br#"{"v":1,"event":"error","agent":"codex"}"#).trim(),
            r#"{"ok":true}"#
        );
        apply_at(false, None);
    }

    /// Polls `ready` every few milliseconds until it holds or `within` passes.
    #[cfg(unix)]
    fn within(within: std::time::Duration, mut ready: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + within;
        while std::time::Instant::now() < deadline {
            if ready() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        ready()
    }

    #[cfg(unix)]
    fn loop_wakes() -> u64 {
        state()
            .accept_wakes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .1
    }

    #[cfg(unix)]
    fn live_loops() -> usize {
        state().live_loops.load(Ordering::SeqCst)
    }

    /// How long a loop that has come out of its wait stalls before counting
    /// it: a stand-in for a thread the scheduler has not run yet.
    #[cfg(unix)]
    static HOLD_AFTER_WAIT_MS: AtomicU64 = AtomicU64::new(0);

    #[cfg(unix)]
    pub(super) fn hold_after_wait() {
        let held = HOLD_AFTER_WAIT_MS.load(Ordering::SeqCst);
        if held > 0 {
            std::thread::sleep(std::time::Duration::from_millis(held));
        }
    }

    /// Two loops a test retired without waiting for them are still on their way
    /// out when the next test starts its own. Their exit wakes are not the new
    /// loop's, and an idle loop must not be charged for them.
    #[cfg(unix)]
    #[test]
    fn a_retiring_predecessor_does_not_count_against_an_idle_loop() {
        let _own = own_listener();
        let dir = ShortDir::new("pred");
        for _ in 0..2 {
            apply_at(true, Some(dir.socket()));
            assert!(within(std::time::Duration::from_secs(5), || live_loops() >= 1));
            // Let it reach its wait, so retiring it is a wake rather than a
            // generation check.
            std::thread::sleep(std::time::Duration::from_millis(100));
            HOLD_AFTER_WAIT_MS.store(600, Ordering::SeqCst);
            apply_at(false, None);
        }
        apply_at(true, Some(dir.socket()));
        assert!(status().listening, "{:?}", status().error);
        let before = loop_wakes();
        std::thread::sleep(std::time::Duration::from_millis(1000));
        let woke = loop_wakes() - before;
        apply_at(false, None);
        assert!(within(std::time::Duration::from_secs(10), || live_loops() == 0));
        HOLD_AFTER_WAIT_MS.store(0, Ordering::SeqCst);
        assert_eq!(
            woke, 0,
            "retiring predecessors woke the idle loop {woke} times"
        );
    }

    /// The accept loop used to wake five times a second for as long as the
    /// bridge was on, only to find out whether it had been retired.
    #[cfg(unix)]
    #[test]
    fn an_idle_accept_loop_does_not_wake() {
        let _own = own_listener();
        let dir = ShortDir::new("idle");
        apply_at(true, Some(dir.socket()));
        assert!(status().listening, "{:?}", status().error);
        assert!(within(std::time::Duration::from_secs(5), || live_loops() == 1));
        // One for an interrupted wait is tolerated; the old loop took ten.
        std::thread::sleep(std::time::Duration::from_millis(100));
        let before = loop_wakes();
        std::thread::sleep(std::time::Duration::from_secs(2));
        let woke = loop_wakes() - before;
        assert!(woke <= 1, "the idle accept loop woke {woke} times in 2 s");
        // And it is still listening: not waking is not the same as not working.
        assert_eq!(
            send(&dir.socket(), br#"{"v":1,"event":"error","agent":"codex"}"#).trim(),
            r#"{"ok":true}"#
        );
        apply_at(false, None);
        assert!(within(std::time::Duration::from_secs(5), || live_loops() == 0));
    }

    /// The wait has no timeout, so retiring the loop has to wake it.
    #[cfg(unix)]
    #[test]
    fn stop_ends_a_blocked_accept_loop_promptly() {
        let _own = own_listener();
        let dir = ShortDir::new("stop");
        apply_at(true, Some(dir.socket()));
        assert!(status().listening, "{:?}", status().error);
        assert!(within(std::time::Duration::from_secs(5), || live_loops() == 1));
        // Let it reach its wait.
        std::thread::sleep(std::time::Duration::from_millis(100));
        let stopped = std::time::Instant::now();
        apply_at(false, None);
        assert!(
            within(
                crate::test_support::coverage_relaxed(std::time::Duration::from_millis(100)),
                || live_loops() == 0
            ),
            "the accept loop was still running {:?} after stop",
            stopped.elapsed()
        );
        assert!(!dir.socket().exists());
    }

    /// A loop retired by a restart finishes after its successor has bound the
    /// same path. It must exit, and must not unlink what is no longer its own.
    #[cfg(unix)]
    #[test]
    fn a_restart_retires_the_old_loop_without_unlinking_the_new_socket() {
        let _own = own_listener();
        let dir = ShortDir::new("race");
        for _ in 0..20 {
            apply_at(true, Some(dir.socket()));
            apply_at(false, None);
            apply_at(true, Some(dir.socket()));
            assert!(status().listening, "{:?}", status().error);
            // The old loop is gone and only the successor remains.
            assert!(
                within(std::time::Duration::from_secs(5), || live_loops() == 1),
                "{} accept loops running",
                live_loops()
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
            assert!(dir.socket().exists(), "the successor's socket was unlinked");
            assert_eq!(
                send(&dir.socket(), br#"{"v":1,"event":"error","agent":"codex"}"#).trim(),
                r#"{"ok":true}"#
            );
            apply_at(false, None);
            assert!(within(std::time::Duration::from_secs(5), || live_loops() == 0));
        }
    }

    /// Open descriptors of this process, by listing the kernel's table.
    #[cfg(unix)]
    fn open_fds() -> usize {
        std::fs::read_dir("/dev/fd").map(|d| d.count()).unwrap_or(0)
    }

    /// Two threads flip the bridge at once. Whatever order they land in, once
    /// both are done the published state must describe one real thing: either
    /// not listening with no socket file and no loop, or listening with exactly
    /// one loop that answers on the socket.
    #[cfg(unix)]
    #[test]
    fn concurrent_apply_leaves_listening_and_the_socket_in_agreement() {
        let _own = own_listener();
        let dir = ShortDir::new("conc");
        let mut disagreements = Vec::new();
        for round in 0..300 {
            let path = dir.socket();
            let a = {
                let path = path.clone();
                std::thread::spawn(move || apply_at(true, Some(path)))
            };
            let b = std::thread::spawn(|| apply_at(false, None));
            a.join().unwrap();
            b.join().unwrap();
            // Let any retired loop finish its exit work.
            let listening = status().listening;
            // A loop counts itself in only once its thread is running.
            let settled = within(std::time::Duration::from_secs(5), || {
                if listening {
                    live_loops() == 1
                } else {
                    live_loops() == 0
                }
            });
            let exists = path.exists();
            let loops = live_loops();
            let answers = listening
                && exists
                && std::os::unix::net::UnixStream::connect(&path).is_ok_and(|mut stream| {
                    use std::io::{Read, Write};
                    let _ = stream.write_all(br#"{"v":1,"event":"error","agent":"codex"}"#);
                    let _ = stream.shutdown(std::net::Shutdown::Write);
                    let mut reply = String::new();
                    let _ = stream.read_to_string(&mut reply);
                    reply.trim() == r#"{"ok":true}"#
                });
            let consistent = settled
                && if listening {
                    answers
                } else {
                    // A retired loop's thread may count itself in after the
                    // check above, so only the file is judged here; the end of
                    // the round waits for the count to reach zero.
                    !exists
                };
            if !consistent {
                disagreements.push(format!(
                    "round {round}: listening={listening} socket_exists={exists} loops={loops} answers={answers}"
                ));
            }
            apply_at(false, None);
            assert!(within(std::time::Duration::from_secs(5), || live_loops() == 0));
            let _ = std::fs::remove_file(&path);
        }
        assert!(
            disagreements.is_empty(),
            "{} of 300 rounds disagreed, first: {:?}",
            disagreements.len(),
            disagreements.first()
        );
    }

    /// 1000 start/stop cycles must not leak descriptors (the wake pipe, the
    /// listener) and must leave no loop behind.
    #[cfg(unix)]
    #[test]
    fn a_thousand_start_stop_cycles_leak_no_descriptors_and_no_loops() {
        let _own = own_listener();
        let dir = ShortDir::new("leak");
        apply_at(true, Some(dir.socket()));
        apply_at(false, None);
        assert!(within(std::time::Duration::from_secs(5), || live_loops() == 0));
        let before = open_fds();
        for _ in 0..1000 {
            apply_at(true, Some(dir.socket()));
            apply_at(false, None);
        }
        assert!(within(std::time::Duration::from_secs(10), || live_loops() == 0));
        let after = open_fds();
        assert!(
            // A leak would be three descriptors per cycle, 3000 in all; slack
            // covers descriptors parallel tests open and close meanwhile.
            after <= before + 200,
            "descriptors grew from {before} to {after} over 1000 cycles"
        );
    }

    /// A loop retired while it is inside `handle` (a slow client) must still
    /// exit once the connection is done, even though the wake byte was written
    /// while it was not waiting.
    #[cfg(unix)]
    #[test]
    fn a_loop_retired_inside_a_slow_connection_exits_when_it_returns() {
        use std::io::Write;
        let _own = own_listener();
        let dir = ShortDir::new("slow");
        apply_at(true, Some(dir.socket()));
        assert!(within(std::time::Duration::from_secs(5), || live_loops() == 1));
        let mut client = std::os::unix::net::UnixStream::connect(dir.socket()).unwrap();
        client.write_all(b"{").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        apply_at(false, None);
        // The loop is stuck in `handle` until the ceiling; it must then leave.
        assert!(
            within(
                CONNECTION_TIMEOUT + std::time::Duration::from_secs(5),
                || { live_loops() == 0 }
            ),
            "the retired loop never exited after its connection ended"
        );
        drop(client);
    }

    /// Unlinking the socket file behind a running loop must not make it spin.
    #[cfg(unix)]
    #[test]
    fn an_externally_unlinked_socket_does_not_make_the_loop_spin() {
        let _own = own_listener();
        let dir = ShortDir::new("unlink");
        apply_at(true, Some(dir.socket()));
        assert!(within(std::time::Duration::from_secs(5), || live_loops() == 1));
        std::thread::sleep(std::time::Duration::from_millis(100));
        std::fs::remove_file(dir.socket()).unwrap();
        let before = loop_wakes();
        std::thread::sleep(std::time::Duration::from_secs(1));
        let woke = loop_wakes() - before;
        assert!(
            woke <= 1,
            "the loop woke {woke} times in 1 s after an unlink"
        );
        apply_at(false, None);
        assert!(within(std::time::Duration::from_secs(5), || live_loops() == 0));
    }

    /// `CONNECTION_TIMEOUT` is documented as a wall-clock ceiling on one
    /// connection, and the accept loop serves connections one at a time. A
    /// peer that keeps a byte in flight inside every read timeout never trips
    /// a per-read timeout, so it held the loop — and every other agent's
    /// notification behind it — for up to `MAX_REPORT_BYTES` timeouts.
    #[cfg(unix)]
    #[test]
    fn a_peer_dripping_bytes_cannot_hold_the_connection_past_its_ceiling() {
        use std::io::Write;
        let (mut client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        let drip = std::thread::spawn(move || {
            // Well inside the read timeout each time; stops once the server
            // closes its end, or after far longer than the ceiling.
            for _ in 0..40 {
                if client.write_all(b" ").is_err() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        });
        let started = std::time::Instant::now();
        handle(server);
        let held = started.elapsed();
        drip.join().unwrap();
        assert!(
            held < CONNECTION_TIMEOUT + std::time::Duration::from_secs(1),
            "one connection held the accept loop for {held:?}"
        );
    }

    #[test]
    fn the_event_and_agent_vocabularies_have_no_duplicates() {
        for (mut names, what) in [
            (EVENTS.iter().map(|e| e.name).collect::<Vec<_>>(), "event"),
            (AGENTS.iter().map(|(name, _)| *name).collect(), "agent"),
        ] {
            names.sort_unstable();
            let before = names.len();
            names.dedup();
            assert_eq!(names.len(), before, "duplicate {what}");
        }
    }

    #[test]
    fn every_event_is_a_clear_exactly_when_it_resolves() {
        // A resolving event that asked for something, or a banner that said
        // "answered", would contradict itself on the board.
        for event in EVENTS {
            assert_eq!(
                event.need == Need::Clear,
                event.role == Role::Resolve,
                "{}",
                event.name
            );
        }
    }

    #[test]
    fn a_report_names_the_tool_call_it_is_about() {
        let notice = report(
            r#"{"v":1,"event":"permission_request","session":"term-1","message":"Bash: cargo test","subject":"00ff00ff00ff00ff","subagent":true}"#,
        )
        .unwrap();
        assert_eq!(notice.event, Some("permission_request"));
        assert_eq!(notice.subject.as_deref(), Some("00ff00ff00ff00ff"));
        assert!(notice.subagent);
        let plain = report(r#"{"v":1,"event":"tool_finished","session":"term-1"}"#).unwrap();
        assert_eq!(plain.subject, None);
        assert!(!plain.subagent);
    }

    #[test]
    fn a_subject_or_scope_that_is_not_one_is_refused() {
        for json in [
            r#"{"v":1,"event":"tool_finished","session":"term-1","subject":"../etc"}"#,
            r#"{"v":1,"event":"tool_finished","session":"term-1","subject":""}"#,
            r#"{"v":1,"event":"tool_finished","session":"term-1","subject":7}"#,
            r#"{"v":1,"event":"tool_finished","session":"term-1","subject":"0123456789abcdef0123456789abcdef0"}"#,
            r#"{"v":1,"event":"tool_finished","session":"term-1","subagent":"yes"}"#,
        ] {
            assert!(report(json).is_err(), "accepted {json}");
        }
    }

    #[test]
    fn a_state_or_resolution_outside_a_session_is_refused() {
        // A banner from a Claude Code outside GitPulse is still worth showing;
        // a state about no session would only spend the rate limit.
        for event in EVENTS.iter().filter(|e| e.role != Role::Banner) {
            for session in ["", r#","session":"../x""#] {
                let json = format!(r#"{{"v":1,"event":"{}"{session}}}"#, event.name);
                assert_eq!(
                    report(&json),
                    Err("state report outside a session"),
                    "{json}"
                );
            }
        }
        for event in EVENTS.iter().filter(|e| e.role == Role::Banner) {
            assert!(report(&format!(r#"{{"v":1,"event":"{}"}}"#, event.name)).is_ok());
        }
    }
}
