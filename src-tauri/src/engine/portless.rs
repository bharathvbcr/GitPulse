//! Portless routes detection and deterministic port hashing for worktrees.
//!
//! Replaces random port conflicts with stable, named local URLs or deterministic ports.
//! Reads Vercel Labs `portless` active state from `~/.portless/routes.json` (or `$PORTLESS_STATE_DIR`),
//! matching subdomains (e.g. `https://<branch>.<app>.localhost`) to active worktrees.
//! Falls back to deterministic port hashing when `portless` is not running.
//!
//! `routes.json` belongs to the machine, not to a worktree: a [`PortlessSnapshot`]
//! reads, parses and probes it once, and a listing asks the snapshot about each
//! worktree instead of repeating that work per worktree.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Probes run on this many threads at most. 256 ports at two 50 ms connects is
/// 8 rounds of 100 ms, under [`PROBE_BUDGET`].
const PROBE_FAN_OUT: usize = 32;

/// The longest a snapshot starts new probes for. A probe already running
/// finishes, so a snapshot takes at most this plus one probe's two connects.
const PROBE_BUDGET: Duration = Duration::from_secs(1);

/// How long one connect to one loopback address may take. Loopback answers or
/// refuses at once; the bound is for a filtered port.
const PROBE_TIMEOUT: Duration = Duration::from_millis(50);

/// The largest `routes.json` read. Portless writes one short object per
/// running server, so a file past this is not one, and is treated as unreadable.
const MAX_ROUTES_FILE_BYTES: u64 = 1 << 20;

/// The most routes taken from one `routes.json`, which bounds the probes a
/// listing makes (two connects per distinct port).
const MAX_ROUTES: usize = 256;

/// The final label of a hostname portless serves: `https://<name>.localhost`,
/// or under `.test`. A route under any other suffix is not a portless
/// subdomain of a worktree.
const PORTLESS_TLDS: [&str; 2] = ["localhost", "test"];

/// The deterministic fallback ports are `FALLBACK_PORT_BASE..+FALLBACK_PORT_SPAN`.
const FALLBACK_PORT_BASE: u16 = 3100;
const FALLBACK_PORT_SPAN: u64 = 900;

/// Information about a detected server route for a worktree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeRouteInfo {
    pub hostname: String,
    pub port: u16,
    pub url: String,
    pub is_portless: bool,
    pub is_listening: bool,
    pub pid: Option<u32>,
}

/// Resolves the portless state directory path.
pub fn portless_state_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("PORTLESS_STATE_DIR") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    // Default: ~/.portless
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".portless"))
}

/// Reads active routes from `routes.json` in the portless state directory.
pub fn read_portless_routes() -> Vec<WorktreeRouteInfo> {
    PortlessSnapshot::load().routes
}

/// The text of `routes.json`, or `None` when it is absent, unreadable, not
/// UTF-8, or larger than [`MAX_ROUTES_FILE_BYTES`]. One open and one bounded
/// read, so a concurrent rewrite cannot make the answer a mixture of two files.
fn read_routes_text() -> Option<String> {
    let routes_file = portless_state_dir()?.join("routes.json");
    #[cfg(test)]
    read_log::record(&routes_file);
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    // A FIFO at this path would park a blocking open until something wrote to
    // it. A symlink is followed on purpose: a user may keep the file elsewhere.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = match options.open(&routes_file) {
        Ok(file) => file,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                log::warn!(
                    "portless routes unreadable at {}: {e}",
                    routes_file.display()
                );
            }
            return None;
        }
    };
    if !file.metadata().is_ok_and(|meta| meta.is_file()) {
        log::warn!(
            "portless routes path is not a regular file: {}",
            routes_file.display()
        );
        return None;
    }
    let mut raw = String::new();
    file.take(MAX_ROUTES_FILE_BYTES + 1)
        .read_to_string(&mut raw)
        .ok()?;
    if u64::try_from(raw.len()).is_ok_and(|len| len <= MAX_ROUTES_FILE_BYTES) {
        Some(raw)
    } else {
        log::warn!(
            "portless routes larger than {MAX_ROUTES_FILE_BYTES} bytes ignored: {}",
            routes_file.display()
        );
        None
    }
}

/// Whether `part` is one DNS label: 1..=63 ASCII letters, digits or hyphens,
/// not starting or ending with a hyphen. Nothing that is URL syntax (`/`, `@`,
/// `#`, `?`, space, `\`) is one.
fn is_dns_label(part: &str) -> bool {
    (1..=63).contains(&part.len())
        && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !part.starts_with('-')
        && !part.ends_with('-')
}

/// The labels of a portless hostname, lowercased as ASCII only (a Kelvin sign
/// is not a `k`): every label a DNS label, at most 253 bytes, the last one
/// `localhost` or `test`, and at least one label before it. One trailing dot is
/// allowed. This is the only test of what a portless hostname is.
fn portless_labels(hostname: &str) -> Option<Vec<String>> {
    let lowered = hostname.to_ascii_lowercase();
    let host = lowered.strip_suffix('.').unwrap_or(&lowered);
    if host.len() > 253 {
        return None;
    }
    let mut parts: Vec<String> = host.split('.').map(str::to_string).collect();
    let tld = parts.pop()?;
    (PORTLESS_TLDS.contains(&tld.as_str())
        && !parts.is_empty()
        && parts.iter().chain([&tld]).all(|p| is_dns_label(p)))
    .then_some(parts)
}

/// The link for a route: `https://<hostname>` for a portless hostname, else the
/// plain loopback port.
fn route_url(hostname: &str, port: u16) -> String {
    if portless_labels(hostname).is_some() {
        format!("https://{hostname}")
    } else {
        format!("http://localhost:{port}")
    }
}

/// A port from a `routes.json` number: 1..=65535, never wrapped into range.
fn checked_port(value: Option<&Value>) -> Option<u16> {
    let port = u16::try_from(value?.as_u64()?).ok()?;
    (port != 0).then_some(port)
}

/// A pid from a `routes.json` entry. `Some(None)` is no pid (absent or null);
/// `None` is a pid that is not a `u32`, which makes the entry unusable rather
/// than naming another process.
fn checked_pid(value: Option<&Value>) -> Option<Option<u32>> {
    match value {
        None | Some(Value::Null) => Some(None),
        Some(other) => u32::try_from(other.as_u64()?).ok().map(Some),
    }
}

/// The validated `(port, pid)` of one entry, or `None` when either is out of
/// range.
fn entry_ports(entry: &Value) -> Option<(u16, Option<u32>)> {
    Some((
        checked_port(entry.get("port"))?,
        checked_pid(entry.get("pid"))?,
    ))
}

/// Parses `routes.json` content, supporting both list format and key-value
/// dictionary. An entry whose port or pid does not fit its type is dropped on
/// its own; the entries around it are kept. `is_listening` is `false`: probing
/// is the snapshot's job, once per port.
fn parse_portless_routes_json(raw: &str) -> Vec<WorktreeRouteInfo> {
    let Ok(parsed) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    let route = |hostname: String, port: u16, pid: Option<u32>, url: String| WorktreeRouteInfo {
        hostname,
        port,
        url,
        is_portless: true,
        is_listening: false,
        pid,
    };
    match parsed {
        // [{"hostname": "...", "port": 4123, "pid": 123}]
        Value::Array(list) => list
            .iter()
            .filter_map(|entry| {
                let hostname = entry.get("hostname")?.as_str()?.to_string();
                let (port, pid) = entry_ports(entry)?;
                let url = route_url(&hostname, port);
                (!hostname.is_empty()).then(|| route(hostname, port, pid, url))
            })
            .collect(),
        // {"hostname.localhost": {"port": 4123, "pid": 123}}
        Value::Object(map) => map
            .iter()
            .filter_map(|(key, entry)| {
                let hostname = entry
                    .get("hostname")
                    .and_then(Value::as_str)
                    .unwrap_or(key)
                    .to_string();
                let (port, pid) = entry_ports(entry)?;
                let url = route_url(&hostname, port);
                (!hostname.is_empty()).then(|| route(hostname, port, pid, url))
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Checks whether a TCP port is currently listening on localhost: on IPv4
/// loopback or on IPv6 loopback, since a Node >= 17 server bound to
/// `localhost` listens on `::1` alone.
pub fn is_port_listening(port: u16) -> bool {
    [
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(Ipv6Addr::LOCALHOST),
    ]
    .into_iter()
    .any(|ip| TcpStream::connect_timeout(&SocketAddr::new(ip, port), PROBE_TIMEOUT).is_ok())
}

/// 64-bit FNV-1a, defined here so a port derived from a name does not depend on
/// the standard library's hasher, whose algorithm is unspecified and changes
/// between Rust releases.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes.iter().fold(OFFSET_BASIS, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(PRIME)
    })
}

/// Deterministically computes an ephemeral port (3100..3999) based on
/// branch/worktree name: FNV-1a 64 of the name's UTF-8 bytes, modulo 900.
pub fn hash_port(name: &str) -> u16 {
    let offset = fnv1a_64(name.as_bytes()) % FALLBACK_PORT_SPAN;
    // The offset is below 900, so it always fits; the fallback satisfies the type.
    FALLBACK_PORT_BASE + u16::try_from(offset).unwrap_or(0)
}

/// A worktree's name as one DNS label: lowercase, anything but `a-z0-9-`
/// (path separators, `_`, dots, non-ASCII) as `-`, no leading or trailing `-`.
/// `None` when nothing is left or the result cannot be a label (63 bytes is the
/// most one holds), so a nameless worktree has no name to claim a route with.
fn dns_label(name: &str) -> Option<String> {
    let label: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let label = label.trim_matches('-');
    (!label.is_empty() && label.len() <= 63).then(|| label.to_string())
}

/// Whether a route's hostname names one of the worktree's `labels`: it is a
/// portless hostname ([`portless_labels`]) and one of the labels before the
/// suffix equals a worktree label exactly. A substring, a prefix or the suffix
/// itself never matches.
fn hostname_belongs_to(hostname: &str, labels: &[String]) -> bool {
    portless_labels(hostname).is_some_and(|names| names.iter().any(|part| labels.contains(part)))
}

/// The portless routes of this machine, read, parsed and probed once.
///
/// A listing makes one and asks it about every worktree, so `routes.json` is
/// read once and each route's port is probed once however many worktrees there
/// are. The answer is a moment's: a route that starts or stops afterwards is
/// seen by the next snapshot.
#[derive(Debug, Clone, Default)]
pub struct PortlessSnapshot {
    routes: Vec<WorktreeRouteInfo>,
    omitted_routes: usize,
    unprobed_ports: usize,
}

/// Probes `ports` on at most [`PROBE_FAN_OUT`] threads and stops starting new
/// probes once `budget` has passed, so a listing waits at most the budget plus
/// one probe however many ports there are. Returns each probed port's answer
/// and how many ports were never probed.
fn probe_ports(
    ports: &[u16],
    budget: Duration,
    probe: impl Fn(u16) -> bool + Sync,
) -> (HashMap<u16, bool>, usize) {
    let deadline = Instant::now() + budget;
    let next = AtomicUsize::new(0);
    let answers = Mutex::new(HashMap::new());
    std::thread::scope(|scope| {
        for _ in 0..PROBE_FAN_OUT.min(ports.len()) {
            scope.spawn(|| {
                while Instant::now() < deadline {
                    let Some(&port) = ports.get(next.fetch_add(1, Ordering::Relaxed)) else {
                        break;
                    };
                    let listening = probe(port);
                    answers
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(port, listening);
                }
            });
        }
    });
    let answers = answers.into_inner().unwrap_or_else(|e| e.into_inner());
    let unprobed = ports.len() - answers.len();
    (answers, unprobed)
}

impl PortlessSnapshot {
    /// Reads `routes.json` and probes each distinct route port. An absent,
    /// empty, malformed or oversize file is a snapshot with no routes.
    pub fn load() -> Self {
        Self::from_text(read_routes_text().as_deref())
    }

    /// Valid routes past [`MAX_ROUTES`] that the snapshot does not hold. Zero
    /// means the file's routes are all here.
    pub fn omitted_routes(&self) -> usize {
        self.omitted_routes
    }

    /// Distinct route ports the probe budget ran out before reaching; their
    /// routes read `is_listening: false` without having been asked.
    pub fn unprobed_ports(&self) -> usize {
        self.unprobed_ports
    }

    fn from_text(raw: Option<&str>) -> Self {
        Self::from_text_probed(raw, PROBE_BUDGET, is_port_listening)
    }

    fn from_text_probed(
        raw: Option<&str>,
        budget: Duration,
        probe: impl Fn(u16) -> bool + Sync,
    ) -> Self {
        let mut routes = raw.map(parse_portless_routes_json).unwrap_or_default();
        let omitted_routes = routes.len().saturating_sub(MAX_ROUTES);
        routes.truncate(MAX_ROUTES);
        let mut ports: Vec<u16> = routes.iter().map(|r| r.port).collect();
        ports.sort_unstable();
        ports.dedup();
        let (answers, unprobed_ports) = probe_ports(&ports, budget, probe);
        for route in &mut routes {
            route.is_listening = answers.get(&route.port).copied().unwrap_or(false);
        }
        if omitted_routes > 0 || unprobed_ports > 0 {
            log::warn!(
                "portless snapshot is partial: {omitted_routes} routes past the {MAX_ROUTES} \
                 limit left out, {unprobed_ports} ports not probed within {budget:?}"
            );
        }
        Self {
            routes,
            omitted_routes,
            unprobed_ports,
        }
    }

    /// Detects all server routes matching a worktree, prioritizing portless subdomains
    /// and including assigned fallback port if a server is listening there.
    pub fn detect(&self, worktree_path: &str, branch: Option<&str>) -> Vec<WorktreeRouteInfo> {
        let path_name = Path::new(worktree_path)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let labels: Vec<String> = [branch.unwrap_or_default(), path_name.as_str()]
            .into_iter()
            .filter_map(dns_label)
            .collect();

        let mut matching: Vec<WorktreeRouteInfo> = self
            .routes
            .iter()
            .filter(|route| hostname_belongs_to(&route.hostname, &labels))
            .cloned()
            .collect();

        // If no portless route matched, check if a dev server is listening on the deterministic hash_port
        let candidate_name = branch
            .map(str::trim)
            .filter(|b| !b.is_empty())
            .unwrap_or(&path_name);
        if candidate_name.is_empty() {
            return matching;
        }
        let assigned_port = hash_port(candidate_name);
        if matching.iter().any(|r| r.port == assigned_port) {
            return matching;
        }
        let listening = self
            .routes
            .iter()
            .find(|r| r.port == assigned_port)
            .map_or_else(|| is_port_listening(assigned_port), |r| r.is_listening);
        if listening {
            matching.push(WorktreeRouteInfo {
                hostname: format!("localhost:{assigned_port}"),
                port: assigned_port,
                url: format!("http://localhost:{assigned_port}"),
                is_portless: false,
                is_listening: true,
                pid: None,
            });
        }
        matching
    }
}

/// Detects all server routes matching one worktree. Reads `routes.json` for
/// this call alone; a caller with several worktrees makes one
/// [`PortlessSnapshot`] and asks it about each.
pub fn detect_worktree_routes(worktree_path: &str, branch: Option<&str>) -> Vec<WorktreeRouteInfo> {
    PortlessSnapshot::load().detect(worktree_path, branch)
}

/// Test-only count of attempts to read `routes.json`, so a test can prove how
/// many reads a listing made without timing anything.
#[cfg(test)]
pub(crate) mod read_log {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    static READS: Mutex<Option<HashMap<PathBuf, usize>>> = Mutex::new(None);

    /// Counts an attempt to read `routes_file`, keyed by file so a test reads
    /// back only its own state directory's count.
    pub(super) fn record(routes_file: &Path) {
        let mut reads = READS.lock().unwrap_or_else(|e| e.into_inner());
        *reads
            .get_or_insert_with(HashMap::new)
            .entry(routes_file.to_path_buf())
            .or_insert(0) += 1;
    }

    /// How many times `routes.json` under `state_dir` has been read.
    pub(crate) fn routes_reads(state_dir: &Path) -> usize {
        let reads = READS.lock().unwrap_or_else(|e| e.into_inner());
        reads
            .as_ref()
            .and_then(|map| map.get(&state_dir.join("routes.json")))
            .copied()
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::read_log::routes_reads;
    use super::*;
    use crate::test_support::env::{bind_env_owned, env_serial, EnvBinding};
    use std::net::TcpListener;

    /// A state directory holding `routes.json` with `json` (none when `None`),
    /// bound as `PORTLESS_STATE_DIR` until the returned pair is dropped.
    fn state_with(json: Option<&str>) -> (EnvBinding<'static>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        if let Some(json) = json {
            std::fs::write(dir.path().join("routes.json"), json).unwrap();
        }
        let binding = bind_env_owned(env_serial()).set("PORTLESS_STATE_DIR", dir.path());
        (binding, dir)
    }

    /// Whether a portless route named `host` is attributed to a worktree
    /// checked out at `/work/<dir>` on `branch`.
    fn attributed(branch: Option<&str>, dir: &str, host: &str) -> bool {
        let mut map = serde_json::Map::new();
        map.insert(host.to_string(), serde_json::json!({ "port": 4999 }));
        let json = serde_json::Value::Object(map).to_string();
        let (_env, _state) = state_with(Some(&json));
        detect_worktree_routes(&format!("/work/{dir}"), branch)
            .iter()
            .any(|route| route.is_portless)
    }

    /// A port nothing listens on: bound, then released.
    fn closed_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[test]
    fn a_route_belongs_to_a_worktree_by_whole_label_only() {
        // (branch, directory, hostname, attributed)
        let table: &[(Option<&str>, &str, &str, bool)] = &[
            (Some("fix"), "zz", "fixtures.localhost", false),
            (None, "app", "webapp.localhost", false),
            (Some("feat-x"), "zz", "feat-x.localhost", true),
            (Some("feat/x"), "zz", "feat-x.myapp.localhost", true),
            (Some("Feat_X"), "zz", "FEAT-X.Localhost.", true),
            (Some("foo"), "zz", "xfoo.localhost", false),
            (Some("foo"), "zz", "foox.localhost", false),
            (Some("foo"), "zz", "foo.localhost.evil", false),
            (Some("foo"), "zz", "foo.evil", false),
            (Some("foo"), "zz", "foo..localhost", false),
            (Some("foo"), "zz", "foo.localhost:4000", false),
            (Some("foo"), "zz", "[::1]", false),
            (Some("foo"), "zz", "::1", false),
            (Some("foo"), "zz", "[foo].localhost", false),
            (Some("xn--caf-dma"), "zz", "xn--caf-dma.localhost", true),
            (Some("caf"), "zz", "xn--caf-dma.localhost", false),
            (Some("localhost"), "zz", "a.localhost", false),
            (Some("localhost"), "zz", "localhost", false),
            (Some("test"), "zz", "a.test", false),
            (Some("  "), "zz", "a.localhost", false),
            (Some("///"), "zz", ".localhost", false),
            (Some("\u{65e5}\u{672c}"), "zz", "x.localhost", false),
            (None, "app", "app.localhost", true),
            (None, "myapp", "feat.myapp.localhost", true),
            (None, "MyApp", "feat.myapp.localhost", true),
            (None, "app", "feat.myapp.localhost", false),
        ];
        for (branch, dir, host, expected) in table {
            assert_eq!(
                attributed(*branch, dir, host),
                *expected,
                "branch {branch:?}, directory {dir:?}, hostname {host:?}"
            );
        }
    }

    #[test]
    fn a_worktree_with_no_name_claims_no_route() {
        let json = r#"[
            {"hostname": "a.localhost", "port": 4001},
            {"hostname": ".localhost", "port": 4002},
            {"hostname": "localhost", "port": 4003},
            {"hostname": "a.b.localhost", "port": 4004}
        ]"#;
        let (_env, _state) = state_with(Some(json));
        for path in ["/", "", "/work/..", "/work/."] {
            for branch in [None, Some(""), Some("  ")] {
                let found = detect_worktree_routes(path, branch);
                assert!(
                    found.iter().all(|route| !route.is_portless),
                    "path {path:?} branch {branch:?} claimed {found:?}"
                );
            }
        }
    }

    #[test]
    fn a_server_bound_only_to_ipv6_loopback_is_listening() {
        let listener = TcpListener::bind("[::1]:0").expect("this host has no IPv6 loopback");
        let port = listener.local_addr().unwrap().port();
        assert!(is_port_listening(port));
        let json = format!(r#"{{"v6.localhost": {{"port": {port}}}}}"#);
        let (_env, _state) = state_with(Some(&json));
        let found = detect_worktree_routes("/work/zz", Some("v6"));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].is_listening, "{found:?}");
    }

    #[test]
    fn a_server_bound_only_to_ipv4_loopback_is_still_listening() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        assert!(is_port_listening(listener.local_addr().unwrap().port()));
        assert!(!is_port_listening(closed_port()));
    }

    #[test]
    fn a_route_oversize_in_port_or_pid_is_dropped_not_wrapped() {
        let json = r#"{
            "port-big.localhost": {"port": 70000, "pid": 1},
            "port-edge.localhost": {"port": 65536, "pid": 1},
            "port-neg.localhost": {"port": -1, "pid": 1},
            "port-zero.localhost": {"port": 0, "pid": 1},
            "port-frac.localhost": {"port": 80.5, "pid": 1},
            "port-huge.localhost": {"port": 18446744073709551615, "pid": 1},
            "pid-big.localhost": {"port": 4100, "pid": 4294967296},
            "pid-wrap.localhost": {"port": 4101, "pid": 4294967297},
            "pid-neg.localhost": {"port": 4102, "pid": -1},
            "ok-max.localhost": {"port": 65535, "pid": 4294967295},
            "ok-nopid.localhost": {"port": 4103}
        }"#;
        let mut hosts: Vec<(String, u16, Option<u32>)> = parse_portless_routes_json(json)
            .into_iter()
            .map(|r| (r.hostname, r.port, r.pid))
            .collect();
        hosts.sort();
        assert_eq!(
            hosts,
            vec![
                ("ok-max.localhost".to_string(), 65535, Some(u32::MAX)),
                ("ok-nopid.localhost".to_string(), 4103, None),
            ]
        );
    }

    #[test]
    fn one_oversize_entry_does_not_discard_the_rest_of_a_list() {
        let json = r#"[
            {"hostname": "a.localhost", "port": 4001, "pid": 7},
            {"hostname": "big.localhost", "port": 70000},
            {"hostname": "pid.localhost", "port": 4002, "pid": 4294967296},
            {"hostname": "b.localhost", "port": 4003},
            {"port": 4004},
            {"hostname": "noport.localhost"},
            "junk",
            null
        ]"#;
        let hosts: Vec<(String, u16)> = parse_portless_routes_json(json)
            .into_iter()
            .map(|r| (r.hostname, r.port))
            .collect();
        assert_eq!(
            hosts,
            vec![
                ("a.localhost".to_string(), 4001),
                ("b.localhost".to_string(), 4003)
            ]
        );
    }

    #[test]
    fn hash_port_is_pinned_to_fnv1a_whatever_the_toolchain() {
        // FNV-1a 64 of the UTF-8 bytes, 3100 + hash % 900. Computed outside
        // Rust; a change here moves every worktree's assigned port.
        let golden = [
            ("", 3537),
            ("a", 3296),
            ("foobar", 3268),
            ("feat-auth", 3558),
            ("bugfix-123", 3909),
            ("main", 3636),
            ("agent/00", 3479),
        ];
        for (name, port) in golden {
            assert_eq!(hash_port(name), port, "hash_port({name:?})");
        }
    }

    #[test]
    fn an_unreadable_routes_file_yields_no_routes_in_one_read() {
        let huge = format!(
            "[{}]",
            r#"{"hostname": "a.localhost", "port": 4001},"#.repeat(60_000)
                + r#"{"hostname": "z.localhost", "port": 4002}"#
        );
        assert!(huge.len() > 2 * 1024 * 1024);
        let cases: Vec<(&str, Option<&str>)> = vec![
            ("missing", None),
            ("empty", Some("")),
            ("whitespace", Some("  \n")),
            (
                "truncated",
                Some(r#"[{"hostname": "a.localhost", "port": 40"#),
            ),
            ("null", Some("null")),
            ("scalar", Some("42")),
            ("string", Some("\"x\"")),
            ("oversize", Some(&huge)),
        ];
        for (label, json) in cases {
            let (_env, state) = state_with(json);
            assert!(read_portless_routes().is_empty(), "{label}");
            assert_eq!(routes_reads(state.path()), 1, "{label}");
        }
    }

    #[test]
    fn fnv1a_matches_the_published_test_vectors() {
        assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn a_snapshot_answers_every_worktree_from_the_one_read() {
        let (_env, state) = state_with(Some(
            r#"{"a.localhost": {"port": 4001}, "b.localhost": {"port": 4002}}"#,
        ));
        let snapshot = PortlessSnapshot::load();
        // The file changes under the listing: a rewrite, then a torn write.
        std::fs::write(
            state.path().join("routes.json"),
            r#"{"c.localhost": {"port": 4003}}"#,
        )
        .unwrap();
        let a = snapshot.detect("/work/zz", Some("a"));
        std::fs::write(state.path().join("routes.json"), r#"{"a.local"#).unwrap();
        let b = snapshot.detect("/work/zz", Some("b"));
        let c = snapshot.detect("/work/zz", Some("c"));
        assert_eq!(a.iter().map(|r| r.port).collect::<Vec<_>>(), vec![4001]);
        assert_eq!(b.iter().map(|r| r.port).collect::<Vec<_>>(), vec![4002]);
        assert!(c.is_empty(), "{c:?}");
        assert_eq!(routes_reads(state.path()), 1);
    }

    #[test]
    fn detecting_one_worktree_reads_the_routes_file_once() {
        let (_env, state) = state_with(Some(r#"[{"hostname": "a.localhost", "port": 4001}]"#));
        let _ = detect_worktree_routes("/work/a", Some("a"));
        assert_eq!(routes_reads(state.path()), 1);
    }

    #[test]
    fn test_hash_port_bounds_and_determinism() {
        let p1 = hash_port("feat-auth");
        let p2 = hash_port("feat-auth");
        assert_eq!(p1, p2, "hash_port must be deterministic");
        assert!((3100..4000).contains(&p1));

        let p3 = hash_port("bugfix-123");
        assert!((3100..4000).contains(&p3));
    }

    #[test]
    fn test_parse_portless_routes_array() {
        let json = r#"[
            {"hostname": "feat-auth.myapp.localhost", "port": 4123, "pid": 99999},
            {"hostname": "api.myapp.localhost", "port": 4567, "pid": 99998}
        ]"#;
        let routes = parse_portless_routes_json(json);
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].hostname, "feat-auth.myapp.localhost");
        assert_eq!(routes[0].port, 4123);
        assert_eq!(routes[0].url, "https://feat-auth.myapp.localhost");
        assert!(routes[0].is_portless);
    }

    #[test]
    fn test_parse_portless_routes_map() {
        let json = r#"{
            "docs.myapp.localhost": {"port": 4200, "pid": 8888}
        }"#;
        let routes = parse_portless_routes_json(json);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].hostname, "docs.myapp.localhost");
        assert_eq!(routes[0].port, 4200);
        assert_eq!(routes[0].url, "https://docs.myapp.localhost");
    }

    // ---- adversarial ----

    #[cfg(unix)]
    #[test]
    fn a_fifo_at_the_routes_path_does_not_block_the_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("routes.json");
        let name = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: a valid NUL-terminated path and a plain mode.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let path = dir.path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _env = bind_env_owned(env_serial()).set("PORTLESS_STATE_DIR", &path);
            let _ = tx.send(PortlessSnapshot::load().routes.len());
        });
        let got = rx.recv_timeout(crate::test_support::coverage_relaxed(Duration::from_secs(
            2,
        )));
        // Release a blocked opener so the test thread can end. Non-blocking, so
        // with no reader parked it fails at once instead of parking us.
        {
            use std::os::unix::fs::OpenOptionsExt;
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&fifo);
        }
        assert_eq!(got, Ok(0), "a FIFO parked the snapshot");
    }

    #[test]
    fn the_routes_path_being_a_directory_a_symlink_or_unreadable_is_handled() {
        // A directory is no routes.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("routes.json")).unwrap();
        let _env = bind_env_owned(env_serial()).set("PORTLESS_STATE_DIR", dir.path());
        assert!(PortlessSnapshot::load().routes.is_empty());
        drop(_env);

        // A symlink to a real file is followed: a user may keep it elsewhere.
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.json");
        std::fs::write(&real, r#"{"a.localhost": {"port": 4001}}"#).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&real, dir.path().join("routes.json")).unwrap();
            let _env = bind_env_owned(env_serial()).set("PORTLESS_STATE_DIR", dir.path());
            assert_eq!(PortlessSnapshot::load().routes.len(), 1);
        }

        // Mode 000 is no routes, unless the tests run as root.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("routes.json");
            std::fs::write(&file, r#"{"a.localhost": {"port": 4001}}"#).unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0)).unwrap();
            let _env = bind_env_owned(env_serial()).set("PORTLESS_STATE_DIR", dir.path());
            let readable = std::fs::File::open(&file).is_ok();
            assert_eq!(PortlessSnapshot::load().routes.len(), usize::from(readable));
        }
    }

    #[test]
    fn the_routes_file_cap_is_exact() {
        let pad = |n: usize| {
            let head = r#"[{"hostname": "a.localhost", "port": 4001}"#;
            format!("{head}{}]", " ".repeat(n - head.len() - 1))
        };
        let at = pad(1 << 20);
        let over = pad((1 << 20) + 1);
        assert_eq!(at.len(), 1 << 20);
        let (_env, _s) = state_with(Some(&at));
        assert_eq!(PortlessSnapshot::load().routes.len(), 1, "exactly 1 MiB");
        drop(_env);
        let (_env, _s) = state_with(Some(&over));
        assert!(PortlessSnapshot::load().routes.is_empty(), "1 MiB + 1");
    }

    #[test]
    fn json_number_shapes_for_port_and_pid() {
        // (json for the entry body, kept as (port, pid))
        let table: &[(&str, Option<(u16, Option<u32>)>)] = &[
            (r#""port": 65535.0"#, None),
            (r#""port": 1e3"#, None),
            (r#""port": -0"#, None),
            (r#""port": 4123"#, Some((4123, None))),
            (r#""port": "4123""#, None),
            (r#""port": true"#, None),
            (r#""port": null"#, None),
            (r#""port": 1e400"#, None),
            (r#""port": 99999999999999999999999"#, None),
            (r#""port": 4123, "pid": "7""#, None),
            (r#""port": 4123, "pid": true"#, None),
            (r#""port": 4123, "pid": 7.0"#, None),
            (r#""port": 4123, "pid": -0"#, None),
            (r#""port": 4123, "pid": null"#, Some((4123, None))),
            (r#""port": 4123, "pid": 0"#, Some((4123, Some(0)))),
        ];
        for (body, expected) in table {
            let json = format!(r#"{{"a.localhost": {{{body}}}}}"#);
            let got = parse_portless_routes_json(&json)
                .first()
                .map(|r| (r.port, r.pid));
            assert_eq!(got, *expected, "{body}");
        }
    }

    #[test]
    fn a_hostname_with_url_syntax_in_a_label_is_never_attributed_or_linked() {
        for host in [
            "foo.evil.com/x.localhost",
            "foo.a@b.localhost",
            "foo.x#y.localhost",
            "foo.x?y.localhost",
            "foo.x y.localhost",
            "foo.x\\y.localhost",
            "foo.x_y.localhost",
            "foo.-x.localhost",
            "foo.x-.localhost",
            "foo.\u{212a}.localhost",
        ] {
            assert!(
                !attributed(Some("foo"), "zz", host),
                "{host:?} was attributed"
            );
        }
        // A Kelvin sign lowercases to an ASCII k; a hostname is not folded that way.
        assert!(!attributed(Some("k"), "zz", "\u{212a}.localhost"));
        // And none of them becomes a link, whoever lists them.
        let json = r#"[{"hostname": "foo.evil.com/x.localhost", "port": 4001}]"#;
        let object = r#"{"foo.evil.com/x.localhost": {"port": 4001}}"#;
        for text in [json, object] {
            for route in parse_portless_routes_json(text) {
                assert!(
                    !route.url.contains("evil.com"),
                    "a link to another host: {}",
                    route.url
                );
            }
        }
    }

    fn routes_json(count: usize) -> String {
        let entries: Vec<String> = (0..count)
            .map(|i| format!(r#""r{i}.localhost": {{"port": {}}}"#, 10_000 + i))
            .collect();
        format!("{{{}}}", entries.join(","))
    }

    #[test]
    fn routes_past_the_cap_are_counted_not_silently_dropped() {
        for (count, omitted) in [(255, 0), (256, 0), (257, 1), (300, 44)] {
            let snapshot = PortlessSnapshot::from_text_probed(
                Some(&routes_json(count)),
                Duration::from_secs(5),
                |_| false,
            );
            assert_eq!(snapshot.routes.len(), count.min(256), "{count} routes");
            assert_eq!(snapshot.omitted_routes(), omitted, "{count} routes");
            assert_eq!(snapshot.unprobed_ports(), 0);
        }
    }

    #[test]
    fn many_slow_probes_run_in_parallel_not_in_series() {
        let slow = |_port: u16| {
            std::thread::sleep(Duration::from_millis(100));
            true
        };
        let started = Instant::now();
        let snapshot = PortlessSnapshot::from_text_probed(
            Some(&routes_json(64)),
            Duration::from_secs(5),
            slow,
        );
        let took = started.elapsed();
        assert!(snapshot.routes.iter().all(|r| r.is_listening));
        // Serial is 64 x 100 ms = 6.4 s; 32 at a time is two rounds.
        let limit = crate::test_support::coverage_relaxed(Duration::from_millis(2500));
        assert!(took < limit, "64 probes took {took:?} (limit {limit:?})");
    }

    #[test]
    fn a_probe_budget_that_runs_out_is_reported_and_bounds_the_wait() {
        let slow = |_port: u16| {
            std::thread::sleep(Duration::from_millis(100));
            true
        };
        let started = Instant::now();
        let snapshot = PortlessSnapshot::from_text_probed(
            Some(&routes_json(256)),
            Duration::from_millis(150),
            slow,
        );
        let took = started.elapsed();
        let probed = snapshot.routes.iter().filter(|r| r.is_listening).count();
        assert!(snapshot.unprobed_ports() > 0, "nothing was cut off");
        assert!(probed > 0);
        assert_eq!(probed + snapshot.unprobed_ports(), 256);
        let limit = crate::test_support::coverage_relaxed(Duration::from_millis(3000));
        assert!(took < limit, "256 probes took {took:?} (limit {limit:?})");
    }

    #[test]
    fn labels_over_63_bytes_and_slug_collisions_are_deterministic() {
        let long = "a".repeat(64);
        assert!(!attributed(Some(&long), "zz", &format!("{long}.localhost")));
        let ok = "a".repeat(63);
        assert!(attributed(Some(&ok), "zz", &format!("{ok}.localhost")));
        // Two branches that slug alike both claim the route, every time.
        for _ in 0..3 {
            assert!(attributed(Some("feat/x"), "zz", "feat-x.localhost"));
            assert!(attributed(Some("feat-x"), "zz", "feat-x.localhost"));
        }
    }
}
