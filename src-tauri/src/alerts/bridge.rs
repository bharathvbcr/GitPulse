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
//!   the socket itself is bound under a `0177`-clearing umask so it is `0600`.
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// How long the accept loop waits for a connection before re-checking whether
/// it has been retired.
///
/// This is a *ceiling on shutdown*, not a polling interval: the loop waits in
/// `poll`, so a connection is accepted the moment it arrives. An earlier
/// version slept for this long between `accept` attempts, which cost every
/// single hook report up to 200 ms and made a burst of them take minutes.
#[cfg(unix)]
const ACCEPT_WAIT: std::time::Duration = std::time::Duration::from_millis(200);

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
}

fn state() -> &'static State {
    static STATE: OnceLock<State> = OnceLock::new();
    STATE.get_or_init(|| State {
        listening: AtomicBool::new(false),
        generation: AtomicU64::new(0),
        path: Mutex::new(None),
        error: Mutex::new(None),
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
    state.generation.fetch_add(1, Ordering::SeqCst);
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
    if state.listening.load(Ordering::SeqCst) {
        return;
    }
    let generation = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
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
    let spawned = std::thread::Builder::new()
        .name("gitpulse-alert-bridge".into())
        .spawn(move || accept_loop(listener, generation));
    if let Err(error) = spawned {
        state.listening.store(false, Ordering::SeqCst);
        set_error(Some(format!("Hook bridge could not start: {error}")));
        stop();
    }
}

/// The longest socket path this platform can bind.
///
/// `sun_path` is a fixed array — 104 bytes on macOS, 108 on Linux — and
/// `bind` reports a path one byte too long as `ENAMETOOLONG`, which reads as a
/// filesystem problem rather than as "your home directory's name is long".
/// Checking it here is what lets the settings panel say the real reason.
#[cfg(unix)]
fn max_socket_path_bytes() -> usize {
    // SAFETY: reading the length of a field of a zeroed `sockaddr_un` is a
    // compile-time constant expressed through the libc type; nothing is
    // dereferenced.
    let probe = unsafe { std::mem::zeroed::<libc::sockaddr_un>() };
    probe.sun_path.len().saturating_sub(1)
}

#[cfg(unix)]
fn bind(
    path: Option<std::path::PathBuf>,
) -> Result<(std::os::unix::net::UnixListener, std::path::PathBuf), String> {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    if !peer_identity_is_checkable() {
        return Err(
            "This platform cannot report a socket peer's user, so the hook bridge stays closed."
                .into(),
        );
    }
    let path = path.ok_or("Application config directory unavailable")?;
    let limit = max_socket_path_bytes();
    if path.as_os_str().len() > limit {
        return Err(format!(
            "The notification socket path is {} bytes and this system allows {limit}: {}",
            path.as_os_str().len(),
            path.display()
        ));
    }
    let dir = path
        .parent()
        .ok_or("Application config directory unavailable")?
        .to_path_buf();
    for ancestor in dir.ancestors() {
        if ancestor.try_exists().map_err(|e| e.to_string())? {
            crate::storage::hygiene::tree::no_symlinks(ancestor)?;
            break;
        }
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    crate::storage::hygiene::tree::no_symlinks(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("{}: {e}", dir.display()))?;

    // A stale socket from a killed process would make `bind` fail with
    // EADDRINUSE forever. Remove only a socket, never a regular file someone
    // else put at this path.
    match std::fs::symlink_metadata(&path) {
        Ok(meta) => {
            use std::os::unix::fs::FileTypeExt;
            if !meta.file_type().is_socket() {
                return Err(format!(
                    "{} exists and is not a socket; the hook bridge stays closed.",
                    path.display()
                ));
            }
            std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", path.display())),
    }

    // Bind under a umask that clears group and other bits. The socket's mode
    // is applied at creation; chmod after the fact leaves a window in which it
    // is world-writable.
    // SAFETY: `umask` only reads and replaces this process's file-mode mask.
    // It is restored before returning, and binding is serialised by `start`.
    let previous = unsafe { libc::umask(0o177) };
    let listener = UnixListener::bind(&path);
    // SAFETY: as above; restores the value read a moment ago.
    unsafe { libc::umask(previous) };
    let listener = listener.map_err(|e| format!("{}: {e}", path.display()))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((listener, path))
}

#[cfg(unix)]
fn peer_identity_is_checkable() -> bool {
    cfg!(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly",
        target_os = "linux",
        target_os = "android"
    ))
}

/// The uid on the other end of a connected socket, or `None` when it cannot be
/// established. `None` is always a refusal.
#[cfg(unix)]
fn peer_uid(stream: &std::os::unix::net::UnixStream) -> Option<u32> {
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    {
        let mut uid: libc::uid_t = 0;
        let mut gid: libc::gid_t = 0;
        // SAFETY: `fd` is a live connected socket owned by `stream`, and both
        // out-pointers address initialised locals for the call's duration.
        let ok = unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } == 0;
        return ok.then_some(uid);
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let mut cred = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: `fd` is a live connected socket; the buffer and its length
        // describe one `ucred` and are valid for the call.
        let ok = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                std::ptr::addr_of_mut!(cred).cast(),
                &mut len,
            )
        } == 0;
        return (ok && len as usize == std::mem::size_of::<libc::ucred>()).then_some(cred.uid);
    }
    #[allow(unreachable_code)]
    {
        let _ = fd;
        None
    }
}

/// Waits for a listener to have a connection ready, or for the timeout.
///
/// `poll` rather than a sleep between non-blocking `accept` calls: the
/// difference is the latency of every notification, which is the whole point
/// of the socket.
#[cfg(unix)]
fn wait_readable(fd: std::os::unix::io::RawFd, timeout: std::time::Duration) -> bool {
    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: one initialised `pollfd` describing a descriptor this thread
    // owns for the duration of the call.
    unsafe { libc::poll(&mut descriptor, 1, millis) > 0 }
}

#[cfg(unix)]
fn accept_loop(listener: std::os::unix::net::UnixListener, generation: u64) {
    use std::os::unix::io::AsRawFd;
    let state = state();
    // SAFETY: `getuid` reads this process's real user id and cannot fail.
    let own = unsafe { libc::getuid() };
    loop {
        if state.generation.load(Ordering::SeqCst) != generation {
            break;
        }
        if !wait_readable(listener.as_raw_fd(), ACCEPT_WAIT) {
            continue;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if peer_uid(&stream) != Some(own) {
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
                set_error(Some(format!("Hook bridge stopped accepting: {error}")));
                break;
            }
        }
    }
    state
        .listening
        .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
        .ok();
    let bound = listener
        .local_addr()
        .ok()
        .and_then(|addr| addr.as_pathname().map(std::path::Path::to_path_buf));
    drop(listener);
    // Only if this loop is still the current one: a loop retired by a restart
    // must not delete the socket its successor has already bound.
    if state.generation.load(Ordering::SeqCst) == generation {
        if let Some(path) = bound {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(unix)]
fn handle(mut stream: std::os::unix::net::UnixStream) {
    use std::io::{Read, Write};
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(CONNECTION_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CONNECTION_TIMEOUT));
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

/// Events an agent hook may report, and how GitPulse words each one.
///
/// A closed vocabulary rather than free text: the reason is GitPulse's
/// sentence, and a peer that could choose it could write a banner that looks
/// like it came from somewhere else.
/// The first five are Claude Code's `Notification` matchers, spelled exactly as
/// the host spells them, because the plugin manifest routes one hook entry per
/// matcher and `hooks::tests` fails if the two lists stop matching. `error` is
/// `StopFailure`, which has its own matchers for *why* the API call failed and
/// is registered without one: a turn that ended on an error is worth the same
/// single sentence whichever error it was.
pub const EVENTS: &[(&str, &str)] = &[
    ("permission_prompt", "needs your permission"),
    ("idle_prompt", "is waiting for you"),
    ("agent_needs_input", "needs your input"),
    ("agent_completed", "finished its work"),
    ("elicitation_dialog", "is asking a question"),
    ("error", "stopped on an error"),
];

/// Agent identities a report may claim, and their display names.
pub const AGENTS: &[(&str, &str)] = &[
    ("claude", "Claude Code"),
    ("codex", "Codex"),
    ("manvi", "Manvi"),
    ("grok", "Grok"),
    ("agy", "Antigravity"),
    ("cursor", "Cursor"),
];

fn valid_key(value: &str) -> bool {
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

    let event = text("event").ok_or("no event")?;
    let reason = EVENTS
        .iter()
        .find(|(name, _)| *name == event)
        .map(|(_, phrase)| *phrase)
        .ok_or("unknown event")?;
    let agent = text("agent").unwrap_or("claude");
    let label = AGENTS
        .iter()
        .find(|(name, _)| *name == agent)
        .map(|(_, display)| *display)
        .ok_or("unknown agent")?;

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
        reason: Some(reason.to_owned()),
        detail,
        // A hook only exists because an agent installed it.
        is_agent: true,
        channel: "hook",
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
        assert_eq!(notice.reason.as_deref(), Some("needs your permission"));
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
        assert_eq!(notice.reason.as_deref(), Some("stopped on an error"));
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
        let orphan = std::os::unix::net::UnixListener::bind(dir.socket()).unwrap();
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
            started.elapsed() < std::time::Duration::from_secs(5),
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

    #[test]
    fn the_event_and_agent_vocabularies_have_no_duplicates() {
        for (list, what) in [(EVENTS, "event"), (AGENTS, "agent")] {
            let mut names: Vec<&str> = list.iter().map(|(name, _)| *name).collect();
            names.sort_unstable();
            let before = names.len();
            names.dedup();
            assert_eq!(names.len(), before, "duplicate {what}");
        }
    }
}
