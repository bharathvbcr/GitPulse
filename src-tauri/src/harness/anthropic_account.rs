//! The machine's Anthropic account registry, as Manvi's `anthropic-account`
//! launcher keeps it.
//!
//! `~/.anthropic-accounts` lists each Anthropic Console organisation under a
//! name, with that organisation's workspace id, and the login Keychain holds
//! each one's key. A key and a workspace id are only valid together: a
//! workspace id from one organisation sent with another organisation's key is
//! refused, or bills the wrong one. `anthropic-account exec` is what pairs
//! them, so when the registry exists GitPulse launches `manvi` through it
//! instead of handing Manvi the key GitPulse saved itself.
//!
//! GitPulse reads neither key. `anthropic-account which` reports the chosen
//! account and workspace without touching the Keychain, and `exec` replaces
//! itself with `manvi` (`execve`), so the process GitPulse spawned and
//! supervises is `manvi` under the same pid.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::engine::process::{find_external_tool_with, run_bounded_capped};

/// The launcher Manvi installs beside `manvi`.
pub(crate) const PROGRAM: &str = "anthropic-account";
/// The registry, in the home directory.
pub(crate) const REGISTRY: &str = ".anthropic-accounts";
/// The variable that names an account for one launch. A person's shell may
/// set it; a GUI-launched GitPulse does not see their shell, so it is cleared
/// for both `which` and `exec`, which keeps the two answers the same.
const ACCOUNT_ENV: &str = "ANTHROPIC_ACCOUNT";
const WHICH_TIMEOUT: Duration = Duration::from_secs(10);
const OUTPUT_CAP: usize = 16 * 1024;

/// The account a Claude launch bills. None of these fields is a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Account {
    /// The registry's name for the organisation (`main`, `alt`, ...).
    pub name: String,
    /// That organisation's workspace id, when the registry records one.
    pub workspace: Option<String>,
    /// Why this account was chosen, as `anthropic-account` reports it
    /// (`default` for the registry's first line).
    pub source: String,
}

/// What the home directory holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Detection {
    /// No registry: GitPulse's own saved key applies.
    Absent,
    /// The registry's chosen account, and the launcher that applies it.
    Found {
        program: PathBuf,
        home: PathBuf,
        account: Account,
    },
    /// A registry exists but could not be read through the launcher. Never
    /// treated as [`Detection::Absent`]: falling back to GitPulse's own key
    /// could bill a different organisation from the one the person set up.
    Broken(String),
}

#[derive(Deserialize)]
struct WhichReport {
    account: String,
    source: String,
    #[serde(default)]
    workspace: Option<String>,
}

/// Detects the registry for this process's user.
pub(crate) fn detect() -> Detection {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let path = std::env::var_os("PATH");
    detect_with(home.as_deref(), path.as_deref())
}

/// [`detect`] preserving typed spawn-gate deferrals (`DeferredUnderLoad`) so
/// background callers such as `harness::steward` do not mistake a load-deferred
/// launcher check for a broken `~/.anthropic-accounts` registry.
pub(crate) fn try_detect() -> Result<Detection, crate::ipc_error::CommandError> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let path = std::env::var_os("PATH");
    try_detect_with(home.as_deref(), path.as_deref())
}

pub(crate) fn detect_with(home: Option<&Path>, path_var: Option<&OsStr>) -> Detection {
    match try_detect_with(home, path_var) {
        Ok(detection) => detection,
        Err(error) => Detection::Broken(error.into_message()),
    }
}

pub(crate) fn try_detect_with(
    home: Option<&Path>,
    path_var: Option<&OsStr>,
) -> Result<Detection, crate::ipc_error::CommandError> {
    let Some(home) = home else {
        return Ok(Detection::Absent);
    };
    let registry = home.join(REGISTRY);
    match std::fs::symlink_metadata(&registry) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Detection::Absent),
        Err(error) => {
            return Ok(Detection::Broken(format!(
                "could not read {}: {error}",
                registry.display()
            )))
        }
    }
    let Some(program) = find_external_tool_with(PROGRAM, path_var, Some(home.as_os_str())) else {
        return Ok(Detection::Broken(format!(
            "{} exists but `{PROGRAM}` is not on PATH or in ~/.local/bin, so GitPulse cannot \
             pair its key with its workspace. Reinstall Manvi's tools.",
            registry.display()
        )));
    };
    let program = PathBuf::from(program);
    let mut command = Command::new(&program);
    command
        .arg("which")
        .arg("--json")
        .args(selector_args(home))
        .env_clear();
    for name in ["HOME", "USER", "LOGNAME", "TMPDIR"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    // `which` resolves the registry from HOME; pin it to the one checked above.
    command.env("HOME", home).current_dir(home);
    let run = match run_bounded_capped(
        command,
        "anthropic-account which",
        WHICH_TIMEOUT,
        None,
        OUTPUT_CAP,
    )
    .and_then(|run| run.require_complete("anthropic-account which"))
    {
        Ok(run) => run,
        Err(error) if error.origin() == Some(crate::ipc_error::IpcErrorKind::DeferredUnderLoad) => {
            return Err(crate::ipc_error::CommandError::typed(
                crate::ipc_error::IpcErrorKind::DeferredUnderLoad,
                format!(
                    "could not ask `{}` which account to use: {}",
                    program.display(),
                    error.message()
                ),
            ));
        }
        Err(error) => {
            return Ok(Detection::Broken(format!(
                "could not ask `{}` which account to use: {}",
                program.display(),
                error.message()
            )))
        }
    };
    if !run.success {
        let stderr = String::from_utf8_lossy(&run.stderr).trim().to_string();
        return Ok(Detection::Broken(format!(
            "`{} which` failed: {}",
            program.display(),
            if stderr.is_empty() {
                "no message"
            } else {
                &stderr
            }
        )));
    }
    match parse_which(&run.stdout) {
        Ok(account) => Ok(Detection::Found {
            program,
            home: home.to_path_buf(),
            account,
        }),
        Err(why) => Ok(Detection::Broken(format!(
            "`{} which --json`: {why}",
            program.display()
        ))),
    }
}

fn parse_which(stdout: &[u8]) -> Result<Account, String> {
    let report: WhichReport =
        serde_json::from_slice(stdout).map_err(|e| format!("unexpected output: {e}"))?;
    if report.account.trim().is_empty() {
        return Err("it named no account".into());
    }
    Ok(Account {
        name: report.account,
        workspace: report.workspace.filter(|w| !w.trim().is_empty()),
        source: report.source,
    })
}

/// Which credential a Claude launch would use, as the hosted server's record
/// keeps it: the account and its workspace, or GitPulse's own key. No secret,
/// but enough that a server started on one credential is not reused once the
/// machine names another.
#[cfg(unix)]
#[cfg(any(unix, test))]
pub(crate) fn credential_label(detection: &Detection) -> String {
    match detection {
        Detection::Absent => "key".into(),
        Detection::Found { account, .. } => format!(
            "account:{}:{}",
            account.name,
            account.workspace.as_deref().unwrap_or("-")
        ),
        Detection::Broken(_) => "unreadable".into(),
    }
}

/// The account selector both `which` and `exec` get. The sidecar is shared by
/// every repository GitPulse has open, so a per-repository marker cannot
/// apply; `--repo <home>` makes `anthropic-account` skip the marker walk and
/// take the registry's default.
fn selector_args(home: &Path) -> [OsString; 2] {
    [OsString::from("--repo"), home.as_os_str().to_os_string()]
}

/// Turns `command` (a fresh `Command::new(program)`) into
/// `anthropic-account exec --no-fallback --repo <home> -- <binary>`.
///
/// `--no-fallback`: the child gets exactly one organisation's key and its
/// workspace, not a second organisation's key for when the first runs out of
/// credit. That keeps the one-credential promise the sidecar makes.
pub(crate) fn exec_prefix(command: &mut Command, home: &Path, binary: &str) {
    command
        .args(["exec", "--no-fallback"])
        .args(selector_args(home))
        .arg("--")
        .arg(binary)
        .env_remove(ACCOUNT_ENV);
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fake_launcher(bin: &Path, body: &str) {
        std::fs::create_dir_all(bin).unwrap();
        let path = bin.join(PROGRAM);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn no_registry_is_absent_even_with_the_launcher_installed() {
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join(".local/bin");
        fake_launcher(&bin, "exit 99");
        let detection = detect_with(Some(home.path()), Some(bin.as_os_str()));
        assert_eq!(detection, Detection::Absent);
    }

    #[test]
    fn a_registry_without_the_launcher_is_broken_not_absent() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(REGISTRY), "main wrkspc_x\n").unwrap();
        let empty = tempfile::tempdir().unwrap();
        match detect_with(Some(home.path()), Some(empty.path().as_os_str())) {
            Detection::Broken(why) => assert!(why.contains(PROGRAM), "{why}"),
            other => panic!("expected Broken, got {other:?}"),
        }
    }

    #[test]
    fn found_reports_the_account_and_workspace_and_asks_from_home() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(REGISTRY), "main wrkspc_x\n").unwrap();
        let bin = home.path().join(".local/bin");
        // Echo the arguments and environment it was given into the report, so
        // the test sees what `which` was asked.
        fake_launcher(
            &bin,
            r#"printf '{"account":"main","source":"default","registry":"r","fallback":null,"workspace":"wrkspc_01Test"}' ; printf '%s|' "$@" > "$HOME/args"; printf '%s' "${ANTHROPIC_ACCOUNT:-unset}" > "$HOME/env""#,
        );
        let detection = detect_with(Some(home.path()), Some(bin.as_os_str()));
        let Detection::Found {
            account,
            home: found_home,
            program,
        } = detection
        else {
            panic!("expected Found, got {detection:?}");
        };
        assert_eq!(
            account,
            Account {
                name: "main".into(),
                workspace: Some("wrkspc_01Test".into()),
                source: "default".into(),
            }
        );
        assert_eq!(found_home, home.path());
        assert_eq!(program, bin.join(PROGRAM));
        let args = std::fs::read_to_string(home.path().join("args")).unwrap();
        assert_eq!(
            args,
            format!("which|--json|--repo|{}|", home.path().display())
        );
        let env = std::fs::read_to_string(home.path().join("env")).unwrap();
        assert_eq!(env, "unset", "ANTHROPIC_ACCOUNT must not reach `which`");
    }

    #[test]
    fn a_failing_or_garbled_launcher_is_broken() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(REGISTRY), "main\n").unwrap();
        let bin = home.path().join(".local/bin");
        fake_launcher(&bin, "echo 'registry is malformed' >&2; exit 2");
        match detect_with(Some(home.path()), Some(bin.as_os_str())) {
            Detection::Broken(why) => assert!(why.contains("registry is malformed"), "{why}"),
            other => panic!("expected Broken, got {other:?}"),
        }
        fake_launcher(&bin, "echo not-json");
        assert!(matches!(
            detect_with(Some(home.path()), Some(bin.as_os_str())),
            Detection::Broken(_)
        ));
    }

    #[test]
    fn an_empty_workspace_is_none() {
        let account = parse_which(br#"{"account":"lappi","source":"default"}"#).unwrap();
        assert_eq!(account.workspace, None);
        let account =
            parse_which(br#"{"account":"lappi","source":"default","workspace":""}"#).unwrap();
        assert_eq!(account.workspace, None);
    }

    #[test]
    fn credential_label_tells_key_accounts_and_workspaces_apart() {
        let found = |name: &str, workspace: Option<&str>| Detection::Found {
            program: PathBuf::from("/p"),
            home: PathBuf::from("/h"),
            account: Account {
                name: name.into(),
                workspace: workspace.map(Into::into),
                source: "default".into(),
            },
        };
        assert_eq!(credential_label(&Detection::Absent), "key");
        assert_eq!(
            credential_label(&found("main", Some("wrkspc_a"))),
            "account:main:wrkspc_a"
        );
        assert_ne!(
            credential_label(&found("main", Some("wrkspc_a"))),
            credential_label(&found("alt", Some("wrkspc_b")))
        );
        assert_eq!(credential_label(&found("lappi", None)), "account:lappi:-");
        assert_eq!(
            credential_label(&Detection::Broken("x".into())),
            "unreadable"
        );
    }

    #[test]
    fn exec_prefix_hands_over_one_organisation_and_the_binary() {
        let mut command = Command::new("/opt/anthropic-account");
        command.env(ACCOUNT_ENV, "alt");
        exec_prefix(&mut command, Path::new("/Users/u"), "/opt/manvi");
        command.args(["serve", "--posture", "host"]);
        let args: Vec<_> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "exec",
                "--no-fallback",
                "--repo",
                "/Users/u",
                "--",
                "/opt/manvi",
                "serve",
                "--posture",
                "host"
            ]
        );
        assert!(command
            .get_envs()
            .any(|(name, value)| name == ACCOUNT_ENV && value.is_none()));
    }
}
