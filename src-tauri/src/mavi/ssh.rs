//! Running a Mavi agent on another machine through the system `ssh`.
//!
//! The agent's NDJSON channel is plain stdio, so SSH needs no daemon, port or
//! token: `ssh HOST -- 'cd DIR && exec claude …'` *is* the transport. The
//! system binary is used on purpose. It reads the person's `~/.ssh/config`
//! (aliases, users, ports, jump hosts, `ControlMaster`), their agent and their
//! `known_hosts`, and GitPulse does not keep a second copy of any of them.
//!
//! **Safety.**
//! - `BatchMode=yes`: ssh never prompts. A password or passphrase prompt would
//!   hang on a pipe nobody reads, so a host that needs one is reported as an
//!   authentication failure with a way forward.
//! - `StrictHostKeyChecking=yes`: an unknown or changed host key is refused,
//!   never accepted on the person's behalf. They confirm a new host once from
//!   a terminal.
//! - The destination is validated ([`valid_host`]) and placed after `--`, so a
//!   value such as `-oProxyCommand=…` can never be read as an option.
//! - The remote directory and every argument are single-quoted for the remote
//!   shell ([`quote`]). Nothing the page sends is pasted into a command line
//!   unquoted.

use crate::procguard::LockedSpawn;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The most hosts read from `~/.ssh/config`.
const MAX_HOSTS: usize = 200;
const MAX_CONFIG_BYTES: u64 = 512 * 1024;
const PROBE_DEADLINE: Duration = Duration::from_secs(20);
const MAX_PROBE_OUTPUT: usize = 64 * 1024;

/// Options every Mavi ssh call carries.
pub(crate) const SSH_OPTIONS: [&str; 13] = [
    "-T",
    // A host whose config forwards ports would fail to bind them on a second
    // connection; Mavi never needs a forward.
    "-o",
    "ClearAllForwardings=yes",
    "-o",
    "BatchMode=yes",
    "-o",
    "StrictHostKeyChecking=yes",
    "-o",
    "ServerAliveInterval=15",
    "-o",
    "ServerAliveCountMax=4",
    "-o",
    "ConnectTimeout=10",
];

/// A non-interactive ssh command does not read the person's login profile,
/// so `claude` installed under `~/.local/bin`, nvm or npm-global is often not
/// on its PATH. The script reads `~/.profile` quietly, then adds the usual
/// user-local install directories.
const REMOTE_PATH_PREFIX: &str = r#"[ -r "$HOME/.profile" ] && . "$HOME/.profile" >/dev/null 2>&1; PATH="$HOME/.local/bin:$HOME/.claude/local:$HOME/.npm-global/bin:$HOME/.bun/bin:/usr/local/bin:/opt/homebrew/bin:$PATH"; export PATH"#;

/// Whether `id` is a canonical UUID (any case).
pub(crate) fn is_uuid(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => *b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// Whether `host` may be passed to ssh as a destination: an alias or
/// `user@host` made of letters, digits, `.`, `_`, `-` and one `@`, not starting
/// with `-`, at most 255 bytes. Ports and options belong in `~/.ssh/config`.
pub(crate) fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 255
        && !host.starts_with('-')
        && !host.starts_with('@')
        && !host.ends_with('@')
        && host.matches('@').count() <= 1
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'@'))
}

/// Whether `dir` is a remote directory Mavi will `cd` into: absolute or
/// `~`-relative, no NUL or newline, at most 4096 bytes.
pub(crate) fn valid_remote_dir(dir: &str) -> bool {
    !dir.is_empty()
        && dir.len() <= 4096
        && (dir.starts_with('/') || dir == "~" || dir.starts_with("~/"))
        && !dir.bytes().any(|b| b == 0 || b == b'\n' || b == b'\r')
}

/// One argument, single-quoted for a POSIX shell.
pub(crate) fn quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', r"'\''"))
}

/// The `cd` target: a leading `~/` stays outside the quotes so the remote
/// shell expands it.
fn quoted_dir(dir: &str) -> String {
    if dir == "~" {
        "~".to_owned()
    } else if let Some(rest) = dir.strip_prefix("~/") {
        format!("~/{}", quote(rest))
    } else {
        quote(dir)
    }
}

/// Wraps a POSIX script so the remote login shell (which may be fish or
/// csh, whose quoting differs) only ever sees `exec sh -c '<script>'`.
fn in_sh(script: &str) -> String {
    format!("exec sh -c {}", quote(script))
}

/// The POSIX script that runs `program args…` in `dir`.
pub(crate) fn remote_script(dir: &str, program: &str, args: &[String]) -> String {
    let mut script = format!(
        "cd {} || exit 97; {REMOTE_PATH_PREFIX}; exec {}",
        quoted_dir(dir),
        quote(program)
    );
    for arg in args {
        script.push(' ');
        script.push_str(&quote(arg));
    }
    script
}

/// The remote command (for ssh) that runs `program args…` in `dir`.
pub(crate) fn remote_command(dir: &str, program: &str, args: &[String]) -> String {
    in_sh(&remote_script(dir, program, args))
}

/// The remote command that prints one path's patch against `HEAD` in `dir`
/// (or the whole content of an untracked file as added lines).
pub(crate) fn remote_diff_command(dir: &str, path: &str) -> String {
    let p = quote(path);
    let flags = "--no-ext-diff --no-textconv --no-color --no-renames";
    in_sh(&format!(
        r#"cd {} || exit 97; out=$(git --no-optional-locks diff HEAD {flags} -- {p}); if [ -z "$out" ] && [ -f {p} ] && ! git ls-files --error-unmatch -- {p} >/dev/null 2>&1; then git --no-optional-locks diff --no-index {flags} -- /dev/null {p}; else printf '%s\n' "$out"; fi"#,
        quoted_dir(dir)
    ))
}

/// The full ssh argument list for running `remote` on `host`.
pub(crate) fn ssh_args(host: &str, remote: &str) -> Vec<String> {
    let mut args: Vec<String> = SSH_OPTIONS.iter().map(|s| (*s).to_owned()).collect();
    args.push("--".into());
    args.push(host.to_owned());
    args.push(remote.to_owned());
    args
}

/// The concrete `Host` aliases in an ssh config text. Patterns (`*`, `?`,
/// `!`) are skipped because they name no machine. `Include` is not followed.
pub(crate) fn config_hosts(text: &str) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let mut words = line
            .split(|c: char| c.is_whitespace() || c == '=')
            .filter(|w| !w.is_empty());
        let Some(key) = words.next() else { continue };
        if !key.eq_ignore_ascii_case("host") {
            continue;
        }
        for alias in words {
            if alias.starts_with('#') {
                break;
            }
            if alias.contains(['*', '?', '!']) || !valid_host(alias) {
                continue;
            }
            if !hosts.iter().any(|h| h == alias) {
                hosts.push(alias.to_owned());
            }
            if hosts.len() >= MAX_HOSTS {
                return hosts;
            }
        }
    }
    hosts
}

/// The hosts in the person's `~/.ssh/config`, or none when it is absent.
pub(crate) fn list_hosts() -> Vec<String> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let path = Path::new(&home).join(".ssh").join("config");
    let Ok(file) = std::fs::File::open(&path) else {
        return Vec::new();
    };
    let mut text = String::new();
    if file
        .take(MAX_CONFIG_BYTES)
        .read_to_string(&mut text)
        .is_err()
    {
        return Vec::new();
    }
    config_hosts(&text)
}

/// What a probe of a host and directory found.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MaviProbeKind {
    Ok,
    AuthFailed,
    HostKey,
    Unreachable,
    NoSuchDir,
    AgentMissing,
    NotARepo,
    Timeout,
    SshMissing,
    Failed,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MaviProbe {
    pub kind: MaviProbeKind,
    /// One sentence the page shows, saying what to do.
    pub message: String,
    /// The repository root on the host, when the directory is in one.
    pub root: Option<String>,
    pub millis: u64,
}

const PROBE_SCRIPT_MARK_DIR: &str = "MAVI_NO_DIR";
const PROBE_SCRIPT_MARK_AGENT: &str = "MAVI_AGENT_OK";
const PROBE_SCRIPT_MARK_REPO: &str = "MAVI_NOT_REPO";

fn probe_script(dir: &str) -> String {
    in_sh(&format!(
        "cd {} 2>/dev/null || {{ echo {PROBE_SCRIPT_MARK_DIR}; exit 0; }}; {REMOTE_PATH_PREFIX}; \
         command -v claude >/dev/null 2>&1 && echo {PROBE_SCRIPT_MARK_AGENT}; \
         git rev-parse --show-toplevel 2>/dev/null || echo {PROBE_SCRIPT_MARK_REPO}",
        quoted_dir(dir)
    ))
}

/// Turns what ssh printed into a typed answer. Pure, so every branch is tested.
pub(crate) fn classify_probe(
    exit: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> (MaviProbeKind, String, Option<String>) {
    let err = stderr.to_ascii_lowercase();
    if exit == Some(255) || exit.is_none() && !err.is_empty() {
        if err.contains("host key verification failed")
            || err.contains("remote host identification has changed")
        {
            return (
                MaviProbeKind::HostKey,
                "The host key is unknown or has changed. Connect once from a terminal to confirm it.".into(),
                None,
            );
        }
        if err.contains("permission denied") || err.contains("too many authentication failures") {
            return (
                MaviProbeKind::AuthFailed,
                "ssh could not sign in without a prompt. Add your key to ssh-agent or ~/.ssh/config.".into(),
                None,
            );
        }
        if [
            "could not resolve",
            "connection refused",
            "timed out",
            "no route to host",
            "network is unreachable",
            "connection closed",
        ]
        .iter()
        .any(|needle| err.contains(needle))
        {
            return (
                MaviProbeKind::Unreachable,
                "The host did not answer. Check the name and your network.".into(),
                None,
            );
        }
        let first = stderr
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("ssh failed.");
        return (
            MaviProbeKind::Failed,
            first.chars().take(300).collect(),
            None,
        );
    }
    if stdout.contains(PROBE_SCRIPT_MARK_DIR) {
        return (
            MaviProbeKind::NoSuchDir,
            "That directory does not exist on the host.".into(),
            None,
        );
    }
    if !stdout.contains(PROBE_SCRIPT_MARK_AGENT) {
        return (
            MaviProbeKind::AgentMissing,
            "Claude Code is not installed on the host, or not on its PATH.".into(),
            None,
        );
    }
    if stdout.contains(PROBE_SCRIPT_MARK_REPO) {
        return (
            MaviProbeKind::NotARepo,
            "The directory is not in a Git repository. Mavi can still run there, without change tracking.".into(),
            None,
        );
    }
    let root = stdout
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with('/'))
        .map(str::to_owned);
    (MaviProbeKind::Ok, "Ready.".into(), root)
}

/// Asks the host whether Mavi can run in `dir`, within [`PROBE_DEADLINE`].
pub(crate) fn probe(host: &str, dir: &str) -> MaviProbe {
    let started = Instant::now();
    let done = |kind, message: String, root| MaviProbe {
        kind,
        message,
        root,
        millis: started.elapsed().as_millis() as u64,
    };
    if !valid_host(host) {
        return done(
            MaviProbeKind::Failed,
            "That is not a host name ssh accepts here.".into(),
            None,
        );
    }
    if !valid_remote_dir(dir) {
        return done(
            MaviProbeKind::Failed,
            "Give the directory as an absolute or ~/ path.".into(),
            None,
        );
    }
    let mut child = match Command::new("ssh")
        .args(ssh_args(host, &probe_script(dir)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn_locked()
    {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return done(
                MaviProbeKind::SshMissing,
                "ssh is not installed on this computer.".into(),
                None,
            )
        }
        Err(e) => {
            return done(
                MaviProbeKind::Failed,
                format!("Could not start ssh: {e}"),
                None,
            )
        }
    };
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(pipe) = pipe {
                let _ = pipe.take(MAX_PROBE_OUTPUT as u64).read_to_end(&mut buf);
            }
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let out = read(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = read(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() > PROBE_DEADLINE => {
                let _ = child.kill();
                let _ = child.wait();
                return done(
                    MaviProbeKind::Timeout,
                    "The host took too long to answer.".into(),
                    None,
                );
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break None,
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    let (kind, message, root) = classify_probe(status.and_then(|s| s.code()), &stdout, &stderr);
    done(kind, message, root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_names_cannot_smuggle_options() {
        for ok in ["devbox", "me@devbox.lan", "build-01", "10.0.0.4", "a_b"] {
            assert!(valid_host(ok), "{ok}");
        }
        for bad in [
            "",
            "-oProxyCommand=touch /tmp/x",
            "a b",
            "a;b",
            "a@b@c",
            "@a",
            "a@",
            "$(x)",
            "a/b",
            "host:22",
        ] {
            assert!(!valid_host(bad), "{bad}");
        }
    }

    #[test]
    fn remote_directories_are_absolute_or_home_relative() {
        assert!(valid_remote_dir("/srv/app"));
        assert!(valid_remote_dir("~"));
        assert!(valid_remote_dir("~/code/app"));
        assert!(!valid_remote_dir("code/app"));
        assert!(!valid_remote_dir("/a\nrm -rf ~"));
        assert!(!valid_remote_dir("~user/x"));
    }

    #[test]
    fn the_remote_command_quotes_every_argument() {
        let script = remote_script("~/my 'app'", "claude", &["-p".into(), "it's".into()]);
        assert!(
            script.starts_with(r"cd ~/'my '\''app'\''' || exit 97;"),
            "{script}"
        );
        assert!(
            script.ends_with(r"exec 'claude' '-p' 'it'\''s'"),
            "{script}"
        );
        assert!(remote_script("/srv/$(x)", "claude", &[]).starts_with("cd '/srv/$(x)' ||"));
        // What the login shell sees is one quoted argument to `sh -c`.
        let cmd = remote_command("/srv/app", "claude", &[]);
        assert!(
            cmd.starts_with("exec sh -c '") && cmd.ends_with('\''),
            "{cmd}"
        );
        // And `sh` itself round-trips it: the script runs, not its quotes.
        let out = Command::new("sh")
            .args(["-c", &in_sh("printf '%s' \"it's ok\"")])
            .output_locked()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "it's ok");
    }

    /// What ssh hands the remote login shell, run by a local shell: the
    /// directory with spaces and quotes is entered, and every argument
    /// reaches the program intact, one per argv slot.
    #[cfg(unix)]
    #[test]
    fn the_remote_command_runs_in_a_shell_with_every_argument_intact() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("it's a $(dir)");
        std::fs::create_dir(&dir).unwrap();
        let program = base.path().join("fake-agent");
        std::fs::write(
            &program,
            "#!/bin/sh\npwd\nfor a in \"$@\"; do printf '[%s]\\n' \"$a\"; done\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let args = vec![
            "--flag".to_string(),
            "a b".to_string(),
            "it's".to_string(),
            "$HOME".to_string(),
            "".to_string(),
        ];
        let command = remote_command(dir.to_str().unwrap(), program.to_str().unwrap(), &args);
        let out = Command::new("sh")
            .args(["-c", &command])
            .env("HOME", base.path())
            .output_locked()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout);
        let mut lines = text.lines();
        assert_eq!(
            std::fs::canonicalize(lines.next().unwrap()).unwrap(),
            std::fs::canonicalize(&dir).unwrap()
        );
        assert_eq!(
            lines.collect::<Vec<_>>(),
            vec!["[--flag]", "[a b]", "[it's]", "[$HOME]", "[]"]
        );
        // A directory that is not there stops before the program runs.
        let missing = remote_command("/nonexistent/mavi", program.to_str().unwrap(), &[]);
        let out = Command::new("sh")
            .args(["-c", &missing])
            .output_locked()
            .unwrap();
        assert_eq!(out.status.code(), Some(97));
        assert!(out.stdout.is_empty());
    }

    #[test]
    fn the_destination_follows_a_double_dash() {
        let args = ssh_args("devbox", "true");
        let at = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(args[at + 1], "devbox");
        assert!(args.contains(&"BatchMode=yes".to_string()));
        assert!(args.contains(&"StrictHostKeyChecking=yes".to_string()));
    }

    #[test]
    fn config_hosts_lists_concrete_aliases_once() {
        let text = "Host *\n  User me\nHost devbox gpu-1 *.lan !bad\nhost=devbox\n  HostName 1.2.3.4\nHost build # comment\nMatch host x\nHost -evil\n";
        assert_eq!(config_hosts(text), vec!["devbox", "gpu-1", "build"]);
    }

    #[test]
    fn probe_answers_are_typed() {
        let k = |exit, out: &str, err: &str| classify_probe(exit, out, err).0;
        assert_eq!(
            k(Some(255), "", "Host key verification failed."),
            MaviProbeKind::HostKey
        );
        assert_eq!(
            k(Some(255), "", "me@x: Permission denied (publickey)."),
            MaviProbeKind::AuthFailed
        );
        assert_eq!(
            k(Some(255), "", "ssh: Could not resolve hostname nope"),
            MaviProbeKind::Unreachable
        );
        assert_eq!(k(Some(255), "", "weird"), MaviProbeKind::Failed);
        assert_eq!(k(Some(0), "MAVI_NO_DIR\n", ""), MaviProbeKind::NoSuchDir);
        assert_eq!(k(Some(0), "/srv/app\n", ""), MaviProbeKind::AgentMissing);
        assert_eq!(
            k(Some(0), "MAVI_AGENT_OK\nMAVI_NOT_REPO\n", ""),
            MaviProbeKind::NotARepo
        );
        let (kind, _, root) = classify_probe(Some(0), "MAVI_AGENT_OK\n/srv/app\n", "");
        assert_eq!(kind, MaviProbeKind::Ok);
        assert_eq!(root.as_deref(), Some("/srv/app"));
    }

    #[test]
    fn uuids_are_checked_by_shape() {
        assert!(is_uuid("0d235a3d-03cf-44a9-ad04-effb59321456"));
        assert!(!is_uuid("0d235a3d-03cf-44a9-ad04-effb5932145"));
        assert!(!is_uuid("0d235a3d_03cf-44a9-ad04-effb59321456"));
    }
}
