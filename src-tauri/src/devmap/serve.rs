//! Talk to `devmap serve`, the kernel's own index daemon.
//!
//! ## Why this exists
//!
//! GitPulse's live gate is a poll loop: a watcher event debounces, then
//! `devmap status --json` is spawned to ask whether the store is stale, then
//! `devmap build` is spawned if it is. Three processes per burst of edits, and
//! the index is only ever as fresh as the last time something asked.
//!
//! `devmap serve` does that job properly. It watches the tree itself, enqueues
//! the paths that changed, and a drain loop inside the daemon rebuilds and
//! persists a new generation. Where a daemon is serving, the poll loop is not
//! merely redundant — it is a *second writer*. Both take the same
//! `<db>.writer.lock`, so a GitPulse build launched while the daemon is
//! draining waits on the lock rather than doing anything useful.
//!
//! ## What this module does and does not replace
//!
//! It stands the gate down from *incremental* rebuilds while a daemon is
//! confirmed live. It deliberately does not stand down the rest, because the
//! daemon's `status` reply is not the CLI's:
//!
//! * it carries `is_fresh`, `degraded_reason` and generation counts, but **not**
//!   `schema_outdated`, `rebuild_required` or `schema_relation` — so schema
//!   migration decisions must still come from `devmap status --json`, or the
//!   gate would be blind to exactly the state it was recently taught to see;
//! * it persists *generations to the store*, not the consumer artifacts. There
//!   is a separate `devmap manifest` for `repo_map.json` and `code_graph.json`,
//!   so a missing artifact still needs `build --manifest` from here.
//!
//! ## Protocol
//!
//! One request per connection, JSON-lines: write one object and a newline,
//! read one object and a newline, server closes. No handshake — `version` is a
//! field on every request, and a mismatch comes back as an error envelope
//! rather than a distinct code. The endpoint is a unix socket whose path the
//! binary itself computes; this module asks for it with
//! `serve --print-socket-path` rather than re-deriving the hash, because the
//! kernel warns that two implementations of that formula which disagree each
//! start a daemon against the same store, and neither side can see it.

use super::cli::resolve_binary;
use crate::engine::git_cli::validate_repo;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The protocol version every request carries. The daemon rejects anything
/// else outright, so a bump here is a wire break, not a negotiation.
const PROTOCOL_VERSION: u32 = 1;

/// The smallest request that proves a daemon is answering.
///
/// This and the two budgets below are read only by the `#[cfg(unix)]`
/// `probe_socket`; the `not(unix)` arm reports that the daemon speaks a named
/// pipe here and connects to nothing. Ungated, each one is dead code on
/// Windows, which `-D warnings` promotes to an error.
#[cfg(unix)]
const STATUS_FRAME: &str = "{\"version\":1,\"cmd\":\"status\"}\n";

/// How long to wait for that answer. The daemon's own per-read budget is 5s;
/// a kernel that cannot answer `status` inside this is not usefully serving
/// this repository right now, and is reported as unconfirmed rather than dead.
#[cfg(unix)]
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// A status envelope is a few hundred bytes. This is a ceiling on a reply from
/// a process we do not control, not a size we expect to approach.
#[cfg(unix)]
const MAX_REPLY_BYTES: usize = 256 * 1024;

/// Resolving the socket path opens nothing and creates nothing, so it gets a
/// far tighter budget than a command that does real work.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(10);

/// Whether a daemon is serving one repository, and what it said.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonState {
    /// A daemon answered a `status` request on this repository's endpoint.
    pub serving: bool,
    /// The endpoint, when it could be resolved — useful in a bug report even
    /// when nothing is listening on it.
    pub socket: Option<String>,
    /// Why `serving` is false. Always set when it is false, never when true:
    /// "no daemon" and "could not ask" must not render alike.
    pub reason: Option<String>,
    /// Paths the daemon has queued and not yet folded into a generation. A
    /// non-zero count means the index is behind the tree *and* something is
    /// already working on it.
    pub pending: Option<u64>,
}

impl DaemonState {
    fn absent(socket: Option<String>, reason: impl Into<String>) -> Self {
        Self {
            serving: false,
            socket,
            reason: Some(reason.into()),
            pending: None,
        }
    }
}

/// Endpoints already resolved, by canonical repository root.
///
/// The path is a pure function of that root and the temp directory, so it
/// cannot change under a running process. Caching it is not a micro-optimism:
/// the whole point of the probe is to replace a process spawn with a socket
/// round trip, and paying a spawn to *find* the socket would give most of that
/// back. Measured at ~14ms per resolve against this host's binary.
static SOCKETS: std::sync::LazyLock<std::sync::Mutex<HashMap<PathBuf, String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// Test seam: an endpoint answered without asking the binary, and a count of how
/// many times the uncached resolution actually ran.
///
/// [`socket_path`]'s contract is that it asks *once* per repository. Proving that
/// by counting a stub's invocations also made the assertion depend on that stub
/// reaching `main` inside [`RESOLVE_TIMEOUT`]: when it did not, the first resolve
/// failed, `socket_path` returned an error, and the test failed having learned
/// nothing about the cache it exists to check. The question worth asking is "how
/// many times did the uncached path run", so it is counted here and the process
/// is dropped. Installed only through [`bind_test_endpoint`], which owns the
/// serial that makes the override exclusive.
// The only caller is a `#[cfg(unix)]` test. Leaving these as `#[cfg(test)]`
// makes them dead on Windows, and clippy `-D warnings` fails that CI leg.
#[cfg(all(test, unix))]
static TEST_ENDPOINT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

#[cfg(all(test, unix))]
static TEST_ENDPOINT_RESOLVES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// How many times the uncached resolution has run since the binding was taken.
#[cfg(all(test, unix))]
pub(crate) fn test_endpoint_resolves() -> usize {
    TEST_ENDPOINT_RESOLVES.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(all(test, unix))]
pub(crate) struct TestEndpointBinding(
    /// Held, never read: the serial's whole job is to be released on drop.
    #[allow(dead_code)]
    std::sync::MutexGuard<'static, ()>,
);

#[cfg(all(test, unix))]
impl Drop for TestEndpointBinding {
    fn drop(&mut self) {
        *TEST_ENDPOINT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        TEST_ENDPOINT_RESOLVES.store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Answer every uncached endpoint resolution with `endpoint`, counting each one,
/// until the returned binding is dropped.
#[cfg(all(test, unix))]
pub(crate) fn bind_test_endpoint(endpoint: impl Into<String>) -> TestEndpointBinding {
    let serial = super::cli::test_serial();
    TEST_ENDPOINT_RESOLVES.store(0, std::sync::atomic::Ordering::Relaxed);
    *TEST_ENDPOINT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(endpoint.into());
    TestEndpointBinding(serial)
}

/// Ask the binary where this repository's endpoint is.
///
/// `--print-socket-path` is documented to return before opening a store or
/// creating any file, which is what makes it safe to call on a path that has
/// never been indexed.
pub fn socket_path(repo: &Path) -> Result<String, String> {
    if let Some(hit) = SOCKETS
        .lock()
        .ok()
        .and_then(|cache| cache.get(repo).cloned())
    {
        return Ok(hit);
    }
    let resolved = resolve_socket_path_uncached(repo)?;
    if let Ok(mut cache) = SOCKETS.lock() {
        cache.insert(repo.to_path_buf(), resolved.clone());
    }
    Ok(resolved)
}

/// Forget every cached endpoint. Only the tests need this — a running app
/// cannot change either input. Its one caller resolves a unix socket path and
/// is `#[cfg(unix)]`, so off unix this is dead and `-D warnings` says so.
#[cfg(all(test, unix))]
pub(crate) fn clear_socket_cache() {
    if let Ok(mut cache) = SOCKETS.lock() {
        cache.clear();
    }
}

fn resolve_socket_path_uncached(repo: &Path) -> Result<String, String> {
    #[cfg(all(test, unix))]
    {
        if let Some(canned) = TEST_ENDPOINT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            TEST_ENDPOINT_RESOLVES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(canned);
        }
    }
    let binary = resolve_binary()?;
    let run = super::cli::run_devmap_public(
        &binary,
        repo,
        &[
            "serve",
            "--print-socket-path",
            "--root",
            &repo.to_string_lossy(),
        ],
        RESOLVE_TIMEOUT,
    )?;
    let path = String::from_utf8_lossy(&run.stdout).trim().to_string();
    if path.is_empty() {
        return Err(format!(
            "devmap serve --print-socket-path printed nothing (exit {:?})",
            run.status_code
        ));
    }
    Ok(path)
}

/// Connect, send one `status`, and read the envelope back.
///
/// Unix only, and honestly so: the daemon speaks a named pipe on Windows, and
/// a pipe client written here could be neither compiled nor exercised from this
/// host. Reporting "not supported on this platform" is the truthful answer; a
/// silent `serving: false` would make Windows look like a machine that simply
/// never has a daemon, which is a different claim.
#[cfg(unix)]
fn probe_socket(socket: &str) -> DaemonState {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    let owned = Some(socket.to_string());
    if !Path::new(socket).exists() {
        return DaemonState::absent(
            owned,
            "no daemon is listening on this repository's endpoint",
        );
    }
    let mut stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        // A socket file with nothing behind it is the normal residue of a
        // daemon that exited; it is an absence, not a fault.
        Err(e) => return DaemonState::absent(owned, format!("cannot connect: {e}")),
    };
    if let Err(e) = stream
        .set_read_timeout(Some(PROBE_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(PROBE_TIMEOUT)))
    {
        return DaemonState::absent(owned, format!("cannot bound the probe: {e}"));
    }
    if let Err(e) = stream.write_all(STATUS_FRAME.as_bytes()) {
        return DaemonState::absent(owned, format!("cannot send a status request: {e}"));
    }
    // The server answers one frame and shuts its write side, so a bounded
    // read-to-end terminates on its own.
    let mut reply = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                reply.extend_from_slice(&buf[..n]);
                if reply.len() > MAX_REPLY_BYTES {
                    return DaemonState::absent(owned, "the daemon's reply exceeded its cap");
                }
                if reply.contains(&b'\n') {
                    break;
                }
            }
            Err(e) => return DaemonState::absent(owned, format!("cannot read the reply: {e}")),
        }
    }
    interpret_reply(&reply, owned)
}

#[cfg(not(unix))]
fn probe_socket(socket: &str) -> DaemonState {
    DaemonState::absent(
        Some(socket.to_string()),
        "querying the devmap daemon is not supported on this platform yet; \
         it speaks a named pipe here and GitPulse has no client for it",
    )
}

/// Read one envelope.
///
/// Separated from the socket so the wire contract is testable without a
/// daemon: every shape below is one the kernel can actually produce.
pub fn interpret_reply(reply: &[u8], socket: Option<String>) -> DaemonState {
    let line = match std::str::from_utf8(reply) {
        Ok(text) => text.trim(),
        Err(_) => return DaemonState::absent(socket, "the daemon replied with invalid UTF-8"),
    };
    if line.is_empty() {
        return DaemonState::absent(socket, "the daemon closed without replying");
    }
    let value: serde_json::Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(e) => return DaemonState::absent(socket, format!("unreadable reply: {e}")),
    };
    // A version the daemon refuses comes back as an ordinary error envelope,
    // so the mismatch has to be recognized by reading `protocol_version` — not
    // by waiting for a code that does not exist.
    if let Some(version) = value
        .get("protocol_version")
        .and_then(serde_json::Value::as_u64)
    {
        if version != u64::from(PROTOCOL_VERSION) {
            return DaemonState::absent(
                socket,
                format!(
                    "the daemon speaks protocol {version}; this build speaks {PROTOCOL_VERSION}"
                ),
            );
        }
    }
    if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        let message = value
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("the daemon refused the request");
        return DaemonState::absent(socket, message.to_string());
    }
    DaemonState {
        serving: true,
        socket,
        reason: None,
        pending: value
            .get("result")
            .and_then(|result| result.get("pending_count"))
            .and_then(serde_json::Value::as_u64),
    }
}

/// Is a daemon maintaining this repository's index right now?
///
/// Trust-gated like every other path that runs a tool against a repository.
pub fn probe(repo_path: &str) -> Result<DaemonState, String> {
    let repo = validate_repo(repo_path)?;
    match socket_path(&repo) {
        Ok(socket) => Ok(probe_socket(&socket)),
        Err(reason) => Ok(DaemonState::absent(None, reason)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(reply: &str) -> DaemonState {
        interpret_reply(reply.as_bytes(), Some("/tmp/x/ipc.sock".into()))
    }

    #[test]
    fn a_status_envelope_means_the_daemon_is_serving() {
        let serving = state(
            r#"{"ok":true,"protocol_version":1,"result":{"pending_count":3,"is_fresh":false}}"#,
        );
        assert!(serving.serving);
        assert_eq!(serving.pending, Some(3));
        assert_eq!(serving.reason, None);
    }

    #[test]
    fn a_serving_daemon_never_carries_a_reason() {
        // The invariant the renderer leans on: `reason` is the explanation for
        // an absence, so a present daemon with an explanation would be a
        // contradiction the UI has no branch for.
        let serving = state(r#"{"ok":true,"protocol_version":1,"result":{"pending_count":0}}"#);
        assert!(serving.serving && serving.reason.is_none());
    }

    #[test]
    fn an_error_envelope_carries_the_daemon_s_own_message() {
        let refused = state(
            r#"{"ok":false,"protocol_version":1,"error":{"code":"request_failed","message":"unsupported protocol version 9"}}"#,
        );
        assert!(!refused.serving);
        assert_eq!(
            refused.reason.as_deref(),
            Some("unsupported protocol version 9")
        );
    }

    #[test]
    fn a_future_protocol_is_named_rather_than_read_as_absent() {
        // A daemon from a newer kernel is running and healthy; calling it "no
        // daemon" would send the gate off to spawn builds that contend with it.
        let newer = state(r#"{"ok":true,"protocol_version":2,"result":{}}"#);
        assert!(!newer.serving);
        let reason = newer.reason.unwrap_or_default();
        assert!(reason.contains("protocol 2"), "{reason}");
        assert!(reason.contains("speaks 1"), "{reason}");
    }

    #[test]
    fn garbage_is_not_a_daemon() {
        for reply in ["", "   ", "not json", "{\"ok\":true", "[]"] {
            let parsed = state(reply);
            assert!(!parsed.serving, "{reply:?} was read as a serving daemon");
            assert!(parsed.reason.is_some(), "{reply:?} gave no reason");
        }
    }

    #[test]
    fn an_ok_envelope_without_a_pending_count_still_serves() {
        // `pending_count` is a detail; its absence must not demote the fact
        // that a daemon answered.
        let serving = state(r#"{"ok":true,"protocol_version":1,"result":{}}"#);
        assert!(serving.serving);
        assert_eq!(serving.pending, None);
    }

    #[test]
    fn an_envelope_missing_ok_is_refused_rather_than_assumed() {
        let parsed = state(r#"{"protocol_version":1,"result":{"pending_count":0}}"#);
        assert!(!parsed.serving);
    }

    #[test]
    fn invalid_utf8_is_reported_not_lossily_accepted() {
        let parsed = interpret_reply(&[0xff, 0xfe, b'\n'], None);
        assert!(!parsed.serving);
        assert!(parsed.reason.unwrap_or_default().contains("UTF-8"));
    }

    /// The endpoint is asked for once per repository, not once per probe.
    ///
    /// Without this the probe costs a process spawn, which is the very thing it
    /// exists to avoid — a liveness check as expensive as the build it is
    /// deciding against would be no saving at all.
    #[test]
    #[cfg(unix)]
    fn the_endpoint_is_resolved_once_per_repository() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let endpoint = dir.path().join("ipc.sock").to_string_lossy().into_owned();

        clear_socket_cache();
        let _bound = bind_test_endpoint(endpoint.clone());
        struct ResetCache;
        impl Drop for ResetCache {
            fn drop(&mut self) {
                clear_socket_cache();
            }
        }
        let _reset = ResetCache;

        let first = socket_path(dir.path()).expect("first");
        let second = socket_path(dir.path()).expect("second");
        assert_eq!(first, endpoint);
        assert_eq!(first, second);
        assert_eq!(
            test_endpoint_resolves(),
            1,
            "the binary was asked more than once for a path it cannot change"
        );
    }

    // The frame exists only where the unix probe that sends it does.
    #[cfg(unix)]
    #[test]
    fn the_status_frame_is_one_line_and_carries_the_version() {
        // The daemon reads until the first newline and rejects a frame with no
        // `version`, so both properties are wire requirements, not style.
        assert!(STATUS_FRAME.ends_with('\n'));
        assert_eq!(STATUS_FRAME.matches('\n').count(), 1);
        let parsed: serde_json::Value = serde_json::from_str(STATUS_FRAME.trim()).expect("json");
        assert_eq!(
            parsed.get("version").and_then(serde_json::Value::as_u64),
            Some(1)
        );
        assert_eq!(
            parsed.get("cmd").and_then(serde_json::Value::as_str),
            Some("status")
        );
    }
}
