//! Provider arguments and one temporary source file for an explicitly requested
//! terminal attempt. User task text never becomes shell syntax or an argv blob.

use super::WorkbenchError;
use crate::engine::git_cli::{capture_command, extended_child_path, resolve_spawn_program_with};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static FILE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub(super) struct BriefFile {
    pub path: PathBuf,
    _file: File,
}

impl BriefFile {
    pub fn create(markdown: &str) -> Result<Self, WorkbenchError> {
        Self::under(&std::env::temp_dir(), markdown)
    }
    fn under(root: &Path, markdown: &str) -> Result<Self, WorkbenchError> {
        if markdown.is_empty() || markdown.len() > 2 * 1024 * 1024 {
            return Err(error(
                "invalid_input",
                "The task brief is empty or exceeds 2 MiB.",
            ));
        }
        let tick = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| error("file_error", e.to_string()))?
            .as_nanos();
        let path = root.join(format!(
            "gitpulse-task-{}-{tick}-{}.md",
            std::process::id(),
            FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&path)
            .map_err(|e| error("file_error", e.to_string()))?;
        let mut brief = Self { path, _file: file };
        brief
            ._file
            .write_all(markdown.as_bytes())
            .and_then(|()| brief._file.sync_all())
            .map_err(|e| error("file_error", e.to_string()))?;
        Ok(brief)
    }
}
impl Drop for BriefFile {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!(target: "workbench", "could not remove the temporary task brief: {error}");
            }
        }
    }
}

fn error(code: &str, message: impl Into<String>) -> WorkbenchError {
    WorkbenchError::new(code, message)
}

pub(super) fn program(provider: &str) -> Result<String, WorkbenchError> {
    if !["codex", "claude"].contains(&provider) {
        return Err(error("invalid_input", "Unsupported terminal provider."));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    let path = extended_child_path(std::env::var_os("PATH").as_deref(), home.as_deref());
    let resolved = resolve_spawn_program_with(provider, path.as_deref(), home.as_deref());
    if !Path::new(&resolved).is_absolute() || !Path::new(&resolved).is_file() {
        return Err(error(
            "not_installed",
            format!("{provider} is not available on the terminal PATH."),
        ));
    }
    Ok(resolved)
}

pub(super) fn check(
    program: &str,
    cwd: &str,
    provider: &str,
    mode: &str,
) -> Result<(), WorkbenchError> {
    let version = capture_command(
        program,
        &["--version"],
        Some(Path::new(cwd)),
        Duration::from_secs(8),
        &[],
    )
    .map_err(|e| error("capability_error", e))?;
    let help = capture_command(
        program,
        &["--help"],
        Some(Path::new(cwd)),
        Duration::from_secs(8),
        &[],
    )
    .map_err(|e| error("capability_error", e))?;
    if !version.success
        || version.stdout.len() > 1024
        || !help.success
        || help.stdout.len() > 128 * 1024
    {
        return Err(error(
            "capability_error",
            "The provider did not return bounded version and help responses.",
        ));
    }
    let version = std::str::from_utf8(&version.stdout)
        .map_err(|_| error("capability_error", "Invalid provider version response."))?;
    let help = std::str::from_utf8(&help.stdout)
        .map_err(|_| error("capability_error", "Invalid provider help response."))?;
    let identity = match provider {
        "codex" => version.starts_with("codex-cli "),
        "claude" => version.contains("Claude Code"),
        _ => false,
    };
    let (flags, _) = policy(provider, mode)?;
    let supported = flags
        .iter()
        .enumerate()
        .filter(|(_, flag)| flag.starts_with("--"))
        .all(|(index, flag)| {
            let value = flags
                .get(index + 1)
                .copied()
                .filter(|next| !next.starts_with("--"));
            advertised_option(help, flag, value)
        });
    if !identity || !supported || (provider == "codex" && !advertised_option(help, "--cd", None)) {
        return Err(error(
            "unsupported_capability",
            "This provider build does not advertise the requested launch controls.",
        ));
    }
    Ok(())
}

/// Verify the exact option and value within its own help block. A similarly
/// named flag or a value mentioned by a different option is not capability proof.
fn advertised_option(help: &str, flag: &str, value: Option<&str>) -> bool {
    let mut found = false;
    let mut description = String::new();
    for line in help.lines() {
        let trimmed = line.trim_start();
        let is_option = trimmed.starts_with("--")
            || (trimmed.starts_with('-') && trimmed.as_bytes().get(2) == Some(&b','));
        if is_option {
            if found {
                break;
            }
            found = trimmed
                .split(|c: char| c.is_whitespace() || c == ',' || c == '=')
                .any(|word| word == flag);
        }
        if found {
            description.push_str(line);
            description.push('\n');
        }
    }
    found
        && value.is_none_or(|value| {
            description
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
                .any(|word| word == value)
        })
}

fn policy(provider: &str, mode: &str) -> Result<(Vec<&'static str>, bool), WorkbenchError> {
    let flags = match (provider, mode) {
        ("codex", "inspect") => vec!["--sandbox", "read-only", "--ask-for-approval", "never"],
        ("codex", "ask") => vec!["--sandbox", "read-only", "--ask-for-approval", "on-request"],
        ("codex", "edit") => vec![
            "--sandbox",
            "workspace-write",
            "--ask-for-approval",
            "on-request",
        ],
        ("codex", "preapproved") => vec![
            "--sandbox",
            "workspace-write",
            "--ask-for-approval",
            "never",
        ],
        ("codex", "auto_review") => vec!["--approve-for-me"],
        ("codex", "bypass") => vec!["--dangerously-bypass-approvals-and-sandbox"],
        ("claude", "inspect") => vec!["--permission-mode", "plan"],
        ("claude", "ask") => vec!["--permission-mode", "manual"],
        ("claude", "edit") => vec!["--permission-mode", "acceptEdits"],
        ("claude", "auto_review") => vec!["--permission-mode", "auto"],
        ("claude", "preapproved") => vec!["--permission-mode", "dontAsk"],
        ("claude", "bypass") => vec!["--permission-mode", "bypassPermissions"],
        _ => {
            return Err(error(
                "unsupported_capability",
                "Unsupported provider permission mode.",
            ))
        }
    };
    Ok((flags, mode == "inspect"))
}

pub(super) fn arguments(
    provider: &str,
    mode: &str,
    acknowledged: bool,
    cwd: &str,
    brief: &Path,
) -> Result<Vec<String>, WorkbenchError> {
    if (mode == "bypass") != acknowledged {
        return Err(error(
            "invalid_input",
            "Bypass requires acknowledgment for this attempt.",
        ));
    }
    let (flags, inspect) = policy(provider, mode)?;
    let path = brief
        .to_str()
        .ok_or_else(|| error("file_error", "Task brief path is not Unicode."))?;
    let quoted = serde_json::to_string(path).map_err(|e| error("file_error", e.to_string()))?;
    let mut args: Vec<String> = flags.into_iter().map(String::from).collect();
    if provider == "codex" {
        args.extend(["--cd".into(), cwd.into()]);
    }
    let scope = if inspect {
        "Inspect and propose a plan; do not modify files."
    } else {
        "Carry out the saved task within the selected permission mode. Report verification and anything requiring human review."
    };
    args.push(format!("Read the UTF-8 task brief at {quoted}. It contains the user's saved task and repository references. {scope} Do not mark the task accepted or publish changes on the user's behalf."));
    if args.iter().any(|a| a.len() > 16 * 1024 || a.contains('\0')) {
        return Err(error(
            "invalid_input",
            "The launch arguments exceed terminal limits.",
        ));
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::{arguments, BriefFile};
    #[test]
    #[ignore = "requires explicitly installed Claude Code and Codex binaries; probes help only"]
    fn installed_provider_help_supports_requested_modes() {
        let root = tempfile::tempdir().unwrap();
        for provider in ["claude", "codex"] {
            let program = super::program(provider).unwrap();
            for mode in [
                "inspect",
                "ask",
                "edit",
                "auto_review",
                "preapproved",
                "bypass",
            ] {
                super::check(&program, root.path().to_str().unwrap(), provider, mode)
                    .unwrap_or_else(|error| panic!("{provider}/{mode}: {}", error.message));
            }
        }
    }
    #[test]
    fn capability_values_belong_to_the_exact_advertised_option() {
        let help = "  -s, --sandbox <MODE>\n    [possible values: read-only, workspace-write]\n  -a, --ask-for-approval <POLICY>\n    - on-request: ask\n    - never: deny\n  -C, --cd <DIR>\n    Set root\n";
        assert!(super::advertised_option(
            help,
            "--sandbox",
            Some("workspace-write")
        ));
        assert!(!super::advertised_option(help, "--sandbox", Some("never")));
        assert!(super::advertised_option(
            help,
            "--ask-for-approval",
            Some("never")
        ));
        assert!(!super::advertised_option(help, "--ask", None));
        assert!(!super::advertised_option(
            "  --permission-mode-legacy <MODE>\n    auto\n",
            "--permission-mode",
            Some("auto")
        ));
        assert!(!super::advertised_option(
            "  --permission-mode <MODE>\n    automatic\n",
            "--permission-mode",
            Some("auto")
        ));
        assert!(!super::advertised_option("", "--cd", None));
    }
    #[cfg(unix)]
    #[test]
    fn probe_refuses_a_build_that_advertises_the_flag_without_the_requested_value() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("scripted-cli");
        std::fs::write(&path, "#!/bin/sh\ncase \"$1\" in\n--version) printf '2.1.263 (Claude Code)\\n';;\n--help) printf '  --permission-mode <mode>\\n    (choices: \"plan\", \"manual\")\\n';;\nesac\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let program = path.to_str().unwrap();
        let cwd = root.path().to_str().unwrap();
        assert!(super::check(program, cwd, "claude", "ask").is_ok());
        assert_eq!(
            super::check(program, cwd, "claude", "bypass")
                .unwrap_err()
                .code,
            "unsupported_capability"
        );
    }
    #[test]
    fn large_source_is_a_private_file_and_never_an_argument() {
        let root = tempfile::tempdir().unwrap();
        let text = "Unchanged $(never_execute) 🧪\n".repeat(20_000);
        let brief = BriefFile::under(root.path(), &text).unwrap();
        assert_eq!(std::fs::read_to_string(&brief.path).unwrap(), text);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&brief.path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let args = arguments("codex", "edit", false, "/checkout with spaces", &brief.path).unwrap();
        assert!(args
            .iter()
            .all(|a| !a.contains("never_execute") && a.len() < 16 * 1024));
        assert!(args.iter().any(|a| a == "/checkout with spaces"));
        let path = brief.path.clone();
        drop(brief);
        assert!(!path.exists());
    }
    #[test]
    fn each_provider_mode_is_explicit_and_bypass_is_never_inherited() {
        for provider in ["codex", "claude"] {
            for mode in [
                "inspect",
                "ask",
                "edit",
                "auto_review",
                "preapproved",
                "bypass",
            ] {
                let args = arguments(
                    provider,
                    mode,
                    mode == "bypass",
                    "/repo",
                    std::path::Path::new("/brief.md"),
                )
                .unwrap();
                assert_eq!(args.iter().any(|a| a.contains("bypass")), mode == "bypass");
                assert!(arguments(
                    provider,
                    mode,
                    mode != "bypass",
                    "/repo",
                    std::path::Path::new("/brief.md")
                )
                .is_err());
            }
        }
        assert!(arguments(
            "shell",
            "edit",
            false,
            "/repo",
            std::path::Path::new("/brief.md")
        )
        .is_err());
    }
}
