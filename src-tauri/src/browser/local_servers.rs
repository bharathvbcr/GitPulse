//! Discovery of loopback local dev servers and `gitpulse-run.sh` previews.

use std::collections::HashSet;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::engine::portless::read_portless_routes;

const MAX_RUN_SCRIPT_BYTES: u64 = 64 * 1024;
const MAX_DISCOVERED_SERVERS: usize = 24;
const MAX_LSOF_OUTPUT_BYTES: u64 = 256 * 1024;
const LSOF_TIMEOUT: Duration = Duration::from_millis(1_500);

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
        let normalized = if cleaned.contains("://") {
            cleaned.to_string()
        } else {
            format!("http://{cleaned}")
        };
        if super::policy::check_loopback(&normalized).is_ok() {
            return Some(normalized);
        }
    }
    None
}

/// Locates `<repo>/gitpulse-run.sh` when symlink-contained inside `repo_path` and `<= 64 KiB`.
pub fn discover_run_script(repo_path: &Path) -> Option<RunScriptInfo> {
    let canon_repo = std::fs::canonicalize(repo_path).ok()?;
    let candidate = canon_repo.join("gitpulse-run.sh");
    let canon_file = std::fs::canonicalize(&candidate).ok()?;
    if !canon_file.starts_with(&canon_repo) {
        return None;
    }
    let meta = std::fs::metadata(&canon_file).ok()?;
    if !meta.is_file() {
        return None;
    }
    if meta.len() > MAX_RUN_SCRIPT_BYTES {
        return Some(RunScriptInfo {
            path: "gitpulse-run.sh".to_string(),
            preview_url: None,
        });
    }
    let file = std::fs::File::open(&canon_file).ok()?;
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
    })
}

fn is_loopback_or_wildcard_host(host: &str) -> bool {
    matches!(
        host.trim(),
        "*" | "127.0.0.1" | "localhost" | "[::1]" | "::1" | "[::]" | "::" | "0.0.0.0"
    )
}

/// Parses `lsof -nP -iTCP -sTCP:LISTEN -F pcn` into unique `(pid, process, port)` loopback listeners.
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
            if listeners.len() >= MAX_DISCOVERED_SERVERS {
                break;
            }
        }
    }
    listeners.sort_by_key(|(_, _, port)| *port);
    listeners
}

#[cfg(unix)]
fn run_lsof_bounded() -> Option<String> {
    let mut child = Command::new("lsof")
        .args(["-nP", "-iTCP", "-sTCP:LISTEN", "-F", "pcn"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.by_ref().take(MAX_LSOF_OUTPUT_BYTES).read_to_end(&mut buf);
        buf
    });

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => {
                let bytes = reader.join().ok()?;
                return Some(String::from_utf8_lossy(&bytes).into_owned());
            }
            Ok(None) => {
                if start.elapsed() >= LSOF_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = reader.join();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
        }
    }
}

#[cfg(not(unix))]
fn run_lsof_bounded() -> Option<String> {
    None
}

fn port_from_url(url: &str) -> Option<u16> {
    let without_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = without_scheme.split('/').next()?;
    let (_, port_part) = authority.rsplit_once(':')?;
    port_part.parse::<u16>().ok()
}

/// Collects listening Portless routes (normalized to browser-policy loopback URLs),
/// bounded `lsof` TCP listeners, and optional repository `gitpulse-run.sh` metadata.
pub fn discover_local_servers(repo_path: Option<&Path>) -> LocalServerDiscovery {
    let run_script = repo_path.and_then(discover_run_script);
    let mut servers = Vec::new();
    let mut seen_ports = HashSet::new();

    for route in read_portless_routes() {
        if !route.is_listening || route.port == 0 {
            continue;
        }
        // `PortlessSnapshot` constructs `<branch>.<app>.localhost`, which `browser::policy`
        // refuses because only exact `localhost` / `127.0.0.0/8` / `::1` are allowed.
        let safe_url = if super::policy::check_loopback(&route.url).is_ok() {
            route.url.clone()
        } else {
            let fallback = format!("http://localhost:{}", route.port);
            if super::policy::check_loopback(&fallback).is_err() {
                continue;
            }
            fallback
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
            return LocalServerDiscovery {
                servers,
                run_script,
            };
        }
    }

    if let Some(stdout) = run_lsof_bounded() {
        for (pid, proc_name, port) in parse_lsof_listeners(&stdout) {
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

    if let Some(script) = &run_script {
        if let Some(preview_url) = &script.preview_url {
            if let Some(port) = port_from_url(preview_url) {
                if seen_ports.insert(port) && servers.len() < MAX_DISCOVERED_SERVERS {
                    servers.push(LocalServerInfo {
                        url: preview_url.clone(),
                        port,
                        label: format!("gitpulse-run.sh :{port}"),
                        source: "run_script".to_string(),
                        pid: None,
                        process: None,
                        hostname: None,
                    });
                }
            }
        }
    }

    LocalServerDiscovery {
        servers,
        run_script,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn parse_run_script_preview_accepts_loopback_and_rejects_remote_or_subdomain() {
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

        let external = "# gitpulse-preview: https://example.com\n";
        assert_eq!(parse_run_script_preview(external), None);

        let subdomain = "# gitpulse-preview: http://evil.localhost:3000\n";
        assert_eq!(parse_run_script_preview(subdomain), None);
    }

    #[test]
    fn parse_lsof_listeners_extracts_loopback_and_skips_lan_ips() {
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
    }

    #[test]
    fn discover_run_script_ignores_escaping_symlinks_and_oversized_files() {
        let repo = TempDir::new().expect("repo");
        let outside = TempDir::new().expect("outside");
        let outside_file = outside.path().join("outside.sh");
        fs::write(&outside_file, "#!/bin/sh\n# gitpulse-preview: http://localhost:3000\n").unwrap();

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
