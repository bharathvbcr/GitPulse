//! Discovery of loopback local dev servers and `gitpulse-run.sh` previews.

use std::collections::HashSet;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::engine::portless::{read_portless_routes, WorktreeRouteInfo};

const MAX_RUN_SCRIPT_BYTES: u64 = 64 * 1024;
const MAX_DISCOVERED_SERVERS: usize = 24;
const MAX_SCAN_LSOF_LISTENERS: usize = 256;
const MAX_LSOF_OUTPUT_BYTES: u64 = 256 * 1024;
const LSOF_TIMEOUT: Duration = Duration::from_millis(1_500);

const DEV_SERVER_PROCESSES: &[&str] = &[
    "node", "bun", "deno", "vite", "next", "python", "python3", "ruby", "cargo", "go", "java",
    "php", "caddy", "nginx", "uvicorn", "rails", "trunk", "esbuild", "webpack", "astro", "nuxt",
    "svelte", "tsx", "hugo",
];

/// A loopback server candidate that can open directly in the embedded browser.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalServerInfo {
    pub url: String,
    pub port: u16,
    pub label: String,
    pub source: String,
    pub pid: Option<u32>,
    pub process: Option<String>,
    pub hostname: Option<String>,
}

/// Repository-local `gitpulse-run.sh` metadata, with optional loopback preview URL.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunScriptInfo {
    pub path: String,
    pub preview_url: Option<String>,
    pub preview_listening: bool,
}

/// Combined discovery payload for the browser pane empty state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalServerDiscovery {
    pub servers: Vec<LocalServerInfo>,
    pub run_script: Option<RunScriptInfo>,
}

/// Extracts and validates `# gitpulse-preview: <loopback-url>` from `gitpulse-run.sh`.
pub fn parse_run_script_preview(contents: &str) -> Option<String> {
    for line in contents.lines().take(64) {
        let trimmed = line.trim();
        let Some(comment_body) = trimmed.strip_prefix('#') else {
            continue;
        };
        let Some(raw_url) = comment_body.trim_start().strip_prefix("gitpulse-preview:") else {
            continue;
        };
        let Some(token) = raw_url.split_whitespace().next() else {
            continue;
        };
        let cleaned = token.trim_matches(|c| c == '"' || c == '\'');
        if cleaned.is_empty() {
            continue;
        }
        let normalized = if cleaned.contains("://") || cleaned.starts_with("about:") {
            cleaned.to_string()
        } else {
            format!("http://{cleaned}")
        };
        if let Ok(parsed) = super::policy::check_loopback(&normalized) {
            if matches!(parsed.scheme(), "http" | "https") {
                return Some(normalized);
            }
        }
    }
    None
}

/// Normalizes a Portless route URL so subdomain URLs like `<branch>.<app>.localhost`
/// fall back to exact `http://localhost:<port>` allowed by `browser::policy`.
pub fn normalize_portless_url(url: &str, port: u16) -> Option<String> {
    if super::policy::check_loopback(url).is_ok() {
        return Some(url.to_string());
    }
    if port == 0 {
        return None;
    }
    let fallback = format!("http://localhost:{port}");
    super::policy::check_loopback(&fallback)
        .ok()
        .map(|_| fallback)
}

/// Locates `<repo>/gitpulse-run.sh` when symlink-contained inside `repo_path` and `<= 64 KiB`.
pub fn discover_run_script(repo_path: &Path) -> Option<RunScriptInfo> {
    let canon_repo = std::fs::canonicalize(repo_path).ok()?;
    let candidate = canon_repo.join("gitpulse-run.sh");
    let canon_file = std::fs::canonicalize(&candidate).ok()?;
    if !canon_file.starts_with(&canon_repo) {
        return None;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(&canon_file).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    if meta.len() > MAX_RUN_SCRIPT_BYTES {
        return Some(RunScriptInfo {
            path: "gitpulse-run.sh".to_string(),
            preview_url: None,
            preview_listening: false,
        });
    }
    let mut buf = Vec::new();
    file.take(MAX_RUN_SCRIPT_BYTES + 1)
        .read_to_end(&mut buf)
        .ok()?;
    let preview_url = if buf.len() as u64 <= MAX_RUN_SCRIPT_BYTES {
        parse_run_script_preview(&String::from_utf8_lossy(&buf))
    } else {
        None
    };
    Some(RunScriptInfo {
        path: "gitpulse-run.sh".to_string(),
        preview_url,
        preview_listening: false,
    })
}

fn is_loopback_or_wildcard_host(host: &str) -> bool {
    matches!(
        host.trim(),
        "*" | "127.0.0.1" | "localhost" | "[::1]" | "::1" | "[::]" | "::" | "0.0.0.0"
    )
}

fn listener_rank(proc_name: &str, port: u16) -> u8 {
    let lower = proc_name.to_ascii_lowercase();
    if DEV_SERVER_PROCESSES
        .iter()
        .any(|prefix| lower == *prefix || lower.starts_with(prefix))
    {
        return 0;
    }
    if port == 1420 || (3000..=9999).contains(&port) {
        return 1;
    }
    2
}

/// Parses `lsof -nP -iTCP -sTCP:LISTEN -F pcn` into ranked, bounded `(pid, process, port)` loopback listeners.
pub fn parse_lsof_listeners(stdout: &str) -> Vec<(u32, String, u16)> {
    let mut current_pid: Option<u32> = None;
    let mut current_cmd: String = String::new();
    let mut seen_ports = HashSet::new();
    let mut listeners = Vec::new();

    for line in stdout.lines() {
        if let Some(pid_str) = line.strip_prefix('p') {
            current_pid = pid_str.trim().parse::<u32>().ok();
            current_cmd.clear();
        } else if let Some(cmd_str) = line.strip_prefix('c') {
            current_cmd = cmd_str.trim().to_string();
        } else if let Some(addr) = line.strip_prefix('n') {
            let Some((host, port_str)) = addr.trim().rsplit_once(':') else {
                continue;
            };
            if !is_loopback_or_wildcard_host(host) {
                continue;
            }
            let Ok(port) = port_str.parse::<u16>() else {
                continue;
            };
            if port == 0 || !seen_ports.insert(port) {
                continue;
            }
            let Some(pid) = current_pid else {
                continue;
            };
            let proc_name = if current_cmd.is_empty() {
                "server".to_string()
            } else {
                current_cmd.clone()
            };
            listeners.push((pid, proc_name, port));
            if listeners.len() >= MAX_SCAN_LSOF_LISTENERS {
                break;
            }
        }
    }
    listeners.sort_by_key(|(_, proc_name, port)| (listener_rank(proc_name, *port), *port));
    listeners.truncate(MAX_DISCOVERED_SERVERS);
    listeners
}

/// The listener table from `lsof`, run through the engine's gated spawn seam
/// (inheritance lock, spawn gate, deadline and output cap) rather than a
/// hand-rolled child and reader thread.
///
/// `lsof` exits non-zero when some sockets cannot be listed, so the status is
/// not consulted: whatever it printed is the table. A run that could not
/// start, was deferred, or hit [`LSOF_TIMEOUT`] yields `None`. Output past
/// [`MAX_LSOF_OUTPUT_BYTES`] keeps its prefix, as before, cut back to the last
/// whole line so a record split at the cap cannot be read as a shorter port.
#[cfg(unix)]
fn run_lsof_bounded() -> Option<String> {
    use crate::engine::process::{build_capture_command, run_bounded_capped, Incomplete};

    let path_var = std::env::var_os("PATH");
    let home = std::env::var_os("HOME");
    let cmd = build_capture_command(
        "lsof",
        &["-nP", "-iTCP", "-sTCP:LISTEN", "-F", "pcn"],
        None,
        &[],
        path_var.as_deref(),
        home.as_deref(),
    );
    let run = run_bounded_capped(
        cmd,
        "lsof",
        LSOF_TIMEOUT,
        None,
        MAX_LSOF_OUTPUT_BYTES as usize,
    )
    .ok()?;
    let stdout = match run.incomplete {
        None => run.stdout,
        Some(Incomplete::OverCap(_)) => {
            let mut prefix = run.stdout;
            let whole = prefix
                .iter()
                .rposition(|b| *b == b'\n')
                .map_or(0, |at| at + 1);
            prefix.truncate(whole);
            prefix
        }
        Some(_) => return None,
    };
    Some(String::from_utf8_lossy(&stdout).into_owned())
}

#[cfg(not(unix))]
fn run_lsof_bounded() -> Option<String> {
    None
}

fn port_from_url(url: &str) -> Option<u16> {
    super::policy::check_loopback(url)
        .ok()?
        .port_or_known_default()
}

fn assemble_discovery(
    routes: Vec<WorktreeRouteInfo>,
    lsof_stdout: Option<&str>,
    mut run_script: Option<RunScriptInfo>,
) -> LocalServerDiscovery {
    let mut servers = Vec::new();
    let mut seen_ports = HashSet::new();

    for route in routes {
        if !route.is_listening || route.port == 0 {
            continue;
        }
        let Some(safe_url) = normalize_portless_url(&route.url, route.port) else {
            continue;
        };
        if !seen_ports.insert(route.port) {
            continue;
        }
        servers.push(LocalServerInfo {
            url: safe_url,
            port: route.port,
            label: route.hostname.clone(),
            source: "portless".to_string(),
            pid: route.pid,
            process: None,
            hostname: Some(route.hostname),
        });
        if servers.len() >= MAX_DISCOVERED_SERVERS {
            break;
        }
    }

    if servers.len() < MAX_DISCOVERED_SERVERS {
        if let Some(stdout) = lsof_stdout {
            for (pid, proc_name, port) in parse_lsof_listeners(stdout) {
                if servers.len() >= MAX_DISCOVERED_SERVERS {
                    break;
                }
                if !seen_ports.insert(port) {
                    continue;
                }
                servers.push(LocalServerInfo {
                    url: format!("http://localhost:{port}"),
                    port,
                    label: format!("{proc_name} :{port}"),
                    source: "lsof".to_string(),
                    pid: Some(pid),
                    process: Some(proc_name),
                    hostname: None,
                });
            }
        }
    }

    if let Some(script) = &mut run_script {
        script.preview_listening = script
            .preview_url
            .as_deref()
            .and_then(port_from_url)
            .is_some_and(|port| seen_ports.contains(&port));
    }

    LocalServerDiscovery {
        servers,
        run_script,
    }
}

/// Collects listening Portless routes (normalized to browser-policy loopback URLs),
/// ranked bounded `lsof` TCP listeners, and optional repository `gitpulse-run.sh` metadata.
pub fn discover_local_servers(repo_path: Option<&Path>) -> LocalServerDiscovery {
    let run_script = repo_path.and_then(discover_run_script);
    let routes = read_portless_routes();
    let lsof_stdout = run_lsof_bounded();
    assemble_discovery(routes, lsof_stdout.as_deref(), run_script)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn parse_run_script_preview_accepts_loopback_and_rejects_remote_or_about_or_subdomain() {
        let valid = "#!/usr/bin/env bash\n# gitpulse-preview: http://localhost:5173\nnpm run dev\n";
        assert_eq!(
            parse_run_script_preview(valid).as_deref(),
            Some("http://localhost:5173")
        );

        let bare_port = "# gitpulse-preview: localhost:4321\n";
        assert_eq!(
            parse_run_script_preview(bare_port).as_deref(),
            Some("http://localhost:4321")
        );

        let ipv6 = "# gitpulse-preview: http://[::1]:5174/app?q=1\n";
        assert_eq!(
            parse_run_script_preview(ipv6).as_deref(),
            Some("http://[::1]:5174/app?q=1")
        );

        assert_eq!(
            parse_run_script_preview("# gitpulse-preview: about:blank\n"),
            None
        );
        assert_eq!(
            parse_run_script_preview("# gitpulse-preview: https://example.com\n"),
            None
        );
        assert_eq!(
            parse_run_script_preview("# gitpulse-preview: http://evil.localhost:3000\n"),
            None
        );
    }

    #[test]
    fn port_from_url_and_portless_normalization_handle_ipv6_defaults_and_subdomains() {
        assert_eq!(port_from_url("http://[::1]:5173/app?a=1"), Some(5173));
        assert_eq!(port_from_url("http://localhost"), Some(80));
        assert_eq!(port_from_url("https://localhost"), Some(443));

        assert_eq!(
            normalize_portless_url("https://feat.app.localhost:5173", 5173).as_deref(),
            Some("http://localhost:5173")
        );
        assert_eq!(
            normalize_portless_url("http://127.0.0.1:4173", 4173).as_deref(),
            Some("http://127.0.0.1:4173")
        );
    }

    #[test]
    fn parse_lsof_listeners_extracts_loopback_skips_lan_and_prioritizes_dev_servers() {
        let sample = "\
p1234
cnode
n127.0.0.1:3000
n192.168.1.42:8080
p5678
cvite
n[::1]:5173
p9999
cpython
n*:8000
";
        assert_eq!(
            parse_lsof_listeners(sample),
            vec![
                (1234, "node".to_string(), 3000),
                (5678, "vite".to_string(), 5173),
                (9999, "python".to_string(), 8000),
            ]
        );

        let mut noisy = String::new();
        for idx in 0..30u16 {
            let port = 50_000 + idx;
            noisy.push_str(&format!(
                "p{}\ncdaemon{idx}\nn127.0.0.1:{port}\n",
                100 + idx
            ));
        }
        noisy.push_str("p8888\nccargo\nn127.0.0.1:1420\n");
        let parsed = parse_lsof_listeners(&noisy);
        assert_eq!(parsed.len(), MAX_DISCOVERED_SERVERS);
        assert_eq!(parsed[0], (8888, "cargo".to_string(), 1420));
    }

    #[test]
    fn assemble_discovery_sets_preview_listening_without_duplicating_run_script_server() {
        let lsof = "p111\nCVite\nn127.0.0.1:5173\n".replace("CVite", "cvite");
        let running_script = RunScriptInfo {
            path: "gitpulse-run.sh".to_string(),
            preview_url: Some("http://localhost:5173".to_string()),
            preview_listening: false,
        };
        let active = assemble_discovery(vec![], Some(&lsof), Some(running_script));
        assert_eq!(active.servers.len(), 1);
        assert!(active.run_script.unwrap().preview_listening);

        let idle_script = RunScriptInfo {
            path: "gitpulse-run.sh".to_string(),
            preview_url: Some("http://localhost:9001".to_string()),
            preview_listening: false,
        };
        let inactive = assemble_discovery(vec![], Some(&lsof), Some(idle_script));
        assert_eq!(inactive.servers.len(), 1);
        assert!(!inactive.run_script.unwrap().preview_listening);
    }

    #[test]
    fn discover_run_script_ignores_escaping_symlinks_and_oversized_files() {
        let repo = TempDir::new().expect("repo");
        let outside = TempDir::new().expect("outside");
        let outside_file = outside.path().join("outside.sh");
        fs::write(
            &outside_file,
            "#!/bin/sh\n# gitpulse-preview: http://localhost:3000\n",
        )
        .unwrap();

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside_file, repo.path().join("gitpulse-run.sh"))
                .expect("symlink");
            assert_eq!(discover_run_script(repo.path()), None);
            fs::remove_file(repo.path().join("gitpulse-run.sh")).unwrap();
        }

        let big = "#".repeat((MAX_RUN_SCRIPT_BYTES as usize) + 128);
        fs::write(repo.path().join("gitpulse-run.sh"), big).unwrap();
        let oversized = discover_run_script(repo.path()).expect("file exists");
        assert_eq!(oversized.preview_url, None);

        fs::write(
            repo.path().join("gitpulse-run.sh"),
            "#!/bin/sh\n# gitpulse-preview: http://127.0.0.1:4173\n",
        )
        .unwrap();
        let ok = discover_run_script(repo.path()).expect("valid script");
        assert_eq!(ok.preview_url.as_deref(), Some("http://127.0.0.1:4173"));
    }
}
