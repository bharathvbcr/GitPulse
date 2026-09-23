//! Portless routes detection and deterministic port hashing for worktrees.
//!
//! Replaces random port conflicts with stable, named local URLs or deterministic ports.
//! Reads Vercel Labs `portless` active state from `~/.portless/routes.json` (or `$PORTLESS_STATE_DIR`),
//! matching subdomains (e.g. `https://<branch>.<app>.localhost`) to active worktrees.
//! Falls back to deterministic port hashing when `portless` is not running.

use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

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

/// A parsed entry from `~/.portless/routes.json`.
#[derive(Debug, Clone, Deserialize)]
struct PortlessRouteEntry {
    #[serde(default)]
    hostname: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    pid: Option<u32>,
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
    let Some(state_dir) = portless_state_dir() else {
        return Vec::new();
    };
    let routes_file = state_dir.join("routes.json");
    if !routes_file.exists() {
        return Vec::new();
    }

    let raw = match fs::read_to_string(&routes_file) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    parse_portless_routes_json(&raw)
}

/// Parses `routes.json` content, supporting both list format and key-value dictionary.
pub fn parse_portless_routes_json(raw: &str) -> Vec<WorktreeRouteInfo> {
    let mut out = Vec::new();

    // Try array format first: [{"hostname": "...", "port": 4123, "pid": 123}]
    if let Ok(list) = serde_json::from_str::<Vec<PortlessRouteEntry>>(raw) {
        for entry in list {
            if let (Some(hostname), Some(port)) = (entry.hostname, entry.port) {
                let listening = is_port_listening(port);
                let url = if hostname.ends_with(".localhost") || hostname.ends_with(".test") {
                    format!("https://{hostname}")
                } else {
                    format!("http://localhost:{port}")
                };
                out.push(WorktreeRouteInfo {
                    hostname,
                    port,
                    url,
                    is_portless: true,
                    is_listening: listening,
                    pid: entry.pid,
                });
            }
        }
        return out;
    }

    // Try map format: {"hostname.localhost": {"port": 4123, "pid": 123}}
    if let Ok(map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(raw) {
        for (key, val) in map {
            let port = val.get("port").and_then(|p| p.as_u64()).map(|p| p as u16);
            let pid = val.get("pid").and_then(|p| p.as_u64()).map(|p| p as u32);
            let hostname = val
                .get("hostname")
                .and_then(|h| h.as_str())
                .map(|s| s.to_string())
                .unwrap_or(key);

            if let Some(p) = port {
                let listening = is_port_listening(p);
                let url = format!("https://{hostname}");
                out.push(WorktreeRouteInfo {
                    hostname,
                    port: p,
                    url,
                    is_portless: true,
                    is_listening: listening,
                    pid,
                });
            }
        }
    }

    out
}

/// Checks whether a TCP port is currently listening on localhost.
pub fn is_port_listening(port: u16) -> bool {
    let addr = format!("127.0.0.1:{port}");
    TcpStream::connect_timeout(
        &addr
            .parse()
            .unwrap_or_else(|_| "127.0.0.1:0".parse().unwrap()),
        Duration::from_millis(50),
    )
    .is_ok()
}

/// Deterministically computes an ephemeral port (3100..3999) based on branch/worktree name.
pub fn hash_port(name: &str) -> u16 {
    let mut hasher = DefaultHasher::new();
    name.hash(&mut hasher);
    let hash = hasher.finish();
    3100 + (hash % 900) as u16
}

/// Detects all server routes matching a worktree, prioritizing portless subdomains
/// and including assigned fallback port if a server is listening there.
pub fn detect_worktree_routes(worktree_path: &str, branch: Option<&str>) -> Vec<WorktreeRouteInfo> {
    let mut matching = Vec::new();
    let portless_routes = read_portless_routes();

    let path_name = Path::new(worktree_path)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();

    let branch_clean = branch.map(|b| b.trim().to_lowercase().replace('/', "-").replace('_', "-"));

    for route in portless_routes {
        let host_lower = route.hostname.to_lowercase();
        let is_match = if let Some(ref b) = branch_clean {
            host_lower.starts_with(b)
                || host_lower.contains(&format!("{b}."))
                || host_lower.contains(&format!(".{b}."))
        } else {
            false
        } || host_lower.contains(&path_name.to_lowercase());

        if is_match {
            matching.push(route);
        }
    }

    // If no portless route matched, check if a dev server is listening on the deterministic hash_port
    let candidate_name = branch.unwrap_or(&path_name);
    let assigned_port = hash_port(candidate_name);
    let listening = is_port_listening(assigned_port);

    if listening && !matching.iter().any(|r| r.port == assigned_port) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_port_bounds_and_determinism() {
        let p1 = hash_port("feat-auth");
        let p2 = hash_port("feat-auth");
        assert_eq!(p1, p2, "hash_port must be deterministic");
        assert!(p1 >= 3100 && p1 < 4000);

        let p3 = hash_port("bugfix-123");
        assert!(p3 >= 3100 && p3 < 4000);
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
}
