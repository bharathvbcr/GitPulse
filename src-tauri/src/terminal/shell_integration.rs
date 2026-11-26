//! Optional shell-integration bootstrap (`OSC 7` working-directory reporting and
//! `OSC 133` FinalTerm semantic prompt/command/exit markers) for interactive
//! `zsh`, `bash`, and `fish` sessions spawned by Waylode.
//!
//! Opt-out via `WAYLODE_SHELL_INTEGRATION=0` (or legacy
//! `GITPULSE_SHELL_INTEGRATION=0`) in the user's environment. Never applied to
//! coding-agent CLIs (`claude`, `agy`, `codex`, `grok`, `manvi`) or custom
//! non-interactive commands.

use portable_pty::CommandBuilder;
use std::path::{Path, PathBuf};

pub const SHELL_INTEGRATION_ENV: &str = "WAYLODE_SHELL_INTEGRATION";
pub const LEGACY_SHELL_INTEGRATION_ENV: &str = "GITPULSE_SHELL_INTEGRATION";
pub const USER_ZDOTDIR_ENV: &str = "WAYLODE_USER_ZDOTDIR";
pub const LEGACY_USER_ZDOTDIR_ENV: &str = "GITPULSE_USER_ZDOTDIR";
pub const ZSH_SHIM_DIR_ENV: &str = "WAYLODE_ZSH_SHIM_DIR";
pub const LEGACY_ZSH_SHIM_DIR_ENV: &str = "GITPULSE_ZSH_SHIM_DIR";
pub const BASH_INTEGRATION_PATH_ENV: &str = "WAYLODE_BASH_INTEGRATION_PATH";
pub const LEGACY_BASH_INTEGRATION_PATH_ENV: &str = "GITPULSE_BASH_INTEGRATION_PATH";

const ZSH_ENV_SCRIPT: &str = r#"# Managed by Waylode terminal shell integration.
__gp_shim_dir="${ZDOTDIR:-}"
__gp_user_zdotdir="${WAYLODE_USER_ZDOTDIR:-${GITPULSE_USER_ZDOTDIR:-}}"
__gp_shell_integration="${WAYLODE_SHELL_INTEGRATION:-${GITPULSE_SHELL_INTEGRATION:-1}}"
if [[ -n "$__gp_user_zdotdir" ]]; then
  export ZDOTDIR="$__gp_user_zdotdir"
else
  unset ZDOTDIR
fi
if [[ -f "${ZDOTDIR:-$HOME}/.zshenv" ]]; then
  builtin source "${ZDOTDIR:-$HOME}/.zshenv"
fi
if [[ -o interactive && "$__gp_shell_integration" != "0" ]]; then
  export __GITPULSE_ZSH_REAL_ZDOTDIR="${ZDOTDIR:-$HOME}"
  export ZDOTDIR="${WAYLODE_ZSH_SHIM_DIR:-${GITPULSE_ZSH_SHIM_DIR:-$__gp_shim_dir}}"
fi
unset __gp_shim_dir __gp_user_zdotdir __gp_shell_integration
"#;

const ZSH_PROFILE_SCRIPT: &str = r#"# Managed by Waylode terminal shell integration.
if [[ -f "${__GITPULSE_ZSH_REAL_ZDOTDIR:-$HOME}/.zprofile" ]]; then
  ZDOTDIR="${__GITPULSE_ZSH_REAL_ZDOTDIR:-$HOME}" builtin source "${__GITPULSE_ZSH_REAL_ZDOTDIR:-$HOME}/.zprofile"
fi
"#;

const ZSH_LOGIN_SCRIPT: &str = r#"# Managed by Waylode terminal shell integration.
__gp_user_zdotdir="${WAYLODE_USER_ZDOTDIR:-${GITPULSE_USER_ZDOTDIR:-}}"
if [[ -n "${__GITPULSE_ZSH_REAL_ZDOTDIR:-}" ]]; then
  if [[ "${__GITPULSE_ZSH_REAL_ZDOTDIR}" == "${HOME}" && -z "$__gp_user_zdotdir" ]]; then
    unset ZDOTDIR
  else
    export ZDOTDIR="${__GITPULSE_ZSH_REAL_ZDOTDIR}"
  fi
fi
unset __gp_user_zdotdir
if [[ -f "${__GITPULSE_ZSH_REAL_ZDOTDIR:-$HOME}/.zlogin" ]]; then
  builtin source "${__GITPULSE_ZSH_REAL_ZDOTDIR:-$HOME}/.zlogin"
fi
unset __GITPULSE_ZSH_REAL_ZDOTDIR
"#;

const ZSH_RC_SCRIPT: &str = r#"# Managed by Waylode terminal shell integration.
__gp_user_zdotdir="${WAYLODE_USER_ZDOTDIR:-${GITPULSE_USER_ZDOTDIR:-}}"
__gp_shell_integration="${WAYLODE_SHELL_INTEGRATION:-${GITPULSE_SHELL_INTEGRATION:-1}}"
if [[ -n "${__GITPULSE_ZSH_REAL_ZDOTDIR:-}" ]]; then
  if [[ "${__GITPULSE_ZSH_REAL_ZDOTDIR}" == "${HOME}" && -z "$__gp_user_zdotdir" ]]; then
    unset ZDOTDIR
  else
    export ZDOTDIR="${__GITPULSE_ZSH_REAL_ZDOTDIR}"
  fi
fi
unset __gp_user_zdotdir
if [[ -f "${__GITPULSE_ZSH_REAL_ZDOTDIR:-$HOME}/.zshrc" ]]; then
  builtin source "${__GITPULSE_ZSH_REAL_ZDOTDIR:-$HOME}/.zshrc"
fi

if [[ -o interactive && "$__gp_shell_integration" != "0" && -z "${__GITPULSE_OSC_INSTALLED:-}" ]]; then
  typeset -g __GITPULSE_OSC_INSTALLED=1
  typeset -g __GITPULSE_CMD_ACTIVE=0

  __gitpulse_urlencode_pwd() {
    emulate -L zsh
    setopt extendedglob
    unsetopt multibyte
    local LC_ALL=C
    local raw="$PWD"
    local encoded=""
    local i ch hex
    for (( i = 1; i <= ${#raw}; i++ )); do
      ch="${raw[i]}"
      if [[ "$ch" == [/._~A-Za-z0-9-] ]]; then
        encoded+="$ch"
      else
        printf -v hex '%%%02X' "'$ch"
        encoded+="$hex"
      fi
    done
    REPLY="$encoded"
  }

  __gitpulse_precmd() {
    local exit_status=$?
    if (( __GITPULSE_CMD_ACTIVE )); then
      builtin printf '\e]133;D;%d\a' "$exit_status"
      __GITPULSE_CMD_ACTIVE=0
    fi
    local REPLY=""
    __gitpulse_urlencode_pwd
    builtin printf '\e]7;file://%s%s\a\e]133;A\a' "${HOST:-localhost}" "$REPLY"
  }

  __gitpulse_preexec() {
    __GITPULSE_CMD_ACTIVE=1
    builtin printf '\e]133;C\a'
  }

  autoload -Uz add-zsh-hook
  add-zsh-hook precmd __gitpulse_precmd
  add-zsh-hook preexec __gitpulse_preexec
fi
"#;

const BASH_RC_SCRIPT: &str = r#"# Managed by GitPulse terminal shell integration.
if [ -f /etc/profile ]; then
  . /etc/profile
fi
if [ -f "$HOME/.bash_profile" ]; then
  . "$HOME/.bash_profile"
elif [ -f "$HOME/.bash_login" ]; then
  . "$HOME/.bash_login"
elif [ -f "$HOME/.profile" ]; then
  . "$HOME/.profile"
elif [ -f "$HOME/.bashrc" ]; then
  . "$HOME/.bashrc"
fi

if [[ $- == *i* && "${WAYLODE_SHELL_INTEGRATION:-${GITPULSE_SHELL_INTEGRATION:-1}}" != "0" && -z "${__GITPULSE_OSC_INSTALLED:-}" ]]; then
  __GITPULSE_OSC_INSTALLED=1
  __GITPULSE_CMD_ACTIVE=0
  __GITPULSE_IN_PROMPT=0

  __gitpulse_urlencode_pwd() {
    local LC_ALL=C
    local raw="$PWD"
    local i ch ord hex out=""
    for (( i = 0; i < ${#raw}; i++ )); do
      ch="${raw:i:1}"
      case "$ch" in
        [/._~A-Za-z0-9-]) out+="$ch" ;;
        *)
          printf -v ord '%d' "'$ch"
          (( ord < 0 )) && (( ord += 256 ))
          printf -v hex '%%%02X' "$ord"
          out+="$hex"
          ;;
      esac
    done
    printf '%s' "$out"
  }

  __gitpulse_precmd() {
    local exit_status=$?
    __GITPULSE_IN_PROMPT=1
    if [[ "${__GITPULSE_CMD_ACTIVE:-0}" == "1" ]]; then
      printf '\e]133;D;%d\a' "$exit_status"
      __GITPULSE_CMD_ACTIVE=0
    fi
    local encoded_pwd
    encoded_pwd="$(__gitpulse_urlencode_pwd)"
    printf '\e]7;file://%s%s\a\e]133;A\a' "${HOSTNAME:-localhost}" "$encoded_pwd"
  }

  __gitpulse_prompt_end() {
    __GITPULSE_IN_PROMPT=0
  }

  # The DEBUG trap fires before every simple command, the prompt hook's own
  # included. `__gitpulse_precmd` runs before it can set __GITPULSE_IN_PROMPT,
  # so without the name check its own call counted as a user command: the
  # first prompt, and every empty Enter, emitted a 133;C/133;D pair (bash < 4.4)
  # or a lone 133;D (bash >= 4.4) for a command nobody ran. A wait for "the
  # command finished" then answered before the real command had started.
  # $BASH_COMMAND is passed from the trap string because inside the handler
  # function it names the handler's own commands instead.
  __gitpulse_preexec() {
    local ret=$?
    case "$1" in __gitpulse_*) return $ret ;; esac
    if [[ "${__GITPULSE_IN_PROMPT:-0}" == "0" && "${__GITPULSE_CMD_ACTIVE:-0}" == "0" ]]; then
      __GITPULSE_CMD_ACTIVE=1
      printf '\e]133;C\a'
    fi
    return $ret
  }

  if [[ -z "${PROMPT_COMMAND:-}" ]]; then
    PROMPT_COMMAND="__gitpulse_precmd;__gitpulse_prompt_end"
  elif [[ "${PROMPT_COMMAND}" != *"__gitpulse_precmd"* ]]; then
    PROMPT_COMMAND="__gitpulse_precmd;${PROMPT_COMMAND};__gitpulse_prompt_end"
  fi

  # Installed last: once it is set it fires for every simple command, and the
  # setup commands above would otherwise count as a user command before the
  # first prompt.
  if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
    PS0=$'\e]133;C\a'"${PS0:-}"
    trap '[[ "${__GITPULSE_IN_PROMPT:-0}" == "1" || "$BASH_COMMAND" == __gitpulse_* ]] || __GITPULSE_CMD_ACTIVE=1' DEBUG
  else
    trap '__gitpulse_preexec "$BASH_COMMAND"' DEBUG
  fi
fi
"#;

const FISH_CONF_SCRIPT: &str = r#"# Managed by Waylode terminal shell integration.
if status is-interactive; and test "$WAYLODE_SHELL_INTEGRATION" != "0"; and test "$GITPULSE_SHELL_INTEGRATION" != "0"; and not set -q __GITPULSE_OSC_INSTALLED
    set -g __GITPULSE_OSC_INSTALLED 1
    set -g __GITPULSE_CMD_ACTIVE 0

    function __gitpulse_fish_prompt --on-event fish_prompt
        set -l last_status $status
        if test "$__GITPULSE_CMD_ACTIVE" = "1"
            printf '\e]133;D;%d\a' $last_status
            set -g __GITPULSE_CMD_ACTIVE 0
        end
        set -l encoded_pwd (string escape --style=url -- $PWD)
        # Restore literal '/' so OSC 7 carries a standard POSIX path.
        set encoded_pwd (string replace -a '%2F' '/' -- $encoded_pwd)
        printf '\e]7;file://%s%s\a\e]133;A\a' (hostname) $encoded_pwd
    end

    function __gitpulse_fish_preexec --on-event fish_preexec
        set -g __GITPULSE_CMD_ACTIVE 1
        printf '\e]133;C\a'
    end
end
"#;

/// Materializes the shell integration scripts under `base_dir` with owner-only
/// permissions (`0700` directories, `0600` scripts).
pub fn ensure_scripts_in(base_dir: &Path) -> Result<PathBuf, String> {
    let root = base_dir.join("shell-integration");
    let zsh_dir = root.join("zsh");
    let bash_dir = root.join("bash");
    let fish_vendor_dir = root.join("fish").join("fish").join("vendor_conf.d");

    for dir in [&root, &zsh_dir, &bash_dir, &fish_vendor_dir] {
        create_private_dir(dir)?;
    }

    write_private_file(&zsh_dir.join(".zshenv"), ZSH_ENV_SCRIPT)?;
    write_private_file(&zsh_dir.join(".zprofile"), ZSH_PROFILE_SCRIPT)?;
    write_private_file(&zsh_dir.join(".zshrc"), ZSH_RC_SCRIPT)?;
    write_private_file(&zsh_dir.join(".zlogin"), ZSH_LOGIN_SCRIPT)?;
    write_private_file(&bash_dir.join("gitpulse.bash"), BASH_RC_SCRIPT)?;
    write_private_file(&fish_vendor_dir.join("gitpulse.fish"), FISH_CONF_SCRIPT)?;

    Ok(root)
}

fn create_private_dir(dir: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder
            .create(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))
    }
}

fn write_private_file(path: &Path, contents: &str) -> Result<(), String> {
    if let Ok(existing) = std::fs::read_to_string(path) {
        if existing == contents {
            return Ok(());
        }
    }
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
        let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
        file.write_all(contents.as_bytes())
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents.as_bytes())
            .map_err(|e| format!("could not write {}: {e}", path.display()))
    }
}

/// Applies `zsh`, `bash`, or `fish` shell integration environment wiring to an
/// interactive default shell command unless opted out via
/// `GITPULSE_SHELL_INTEGRATION=0`.
pub fn apply_to_default_shell(
    cmd: &mut CommandBuilder,
    resolved_shell: &str,
    is_default_shell: bool,
    has_custom_args: bool,
) {
    let Some(config_dir) = crate::tool_config::default_config_dir() else {
        return;
    };
    apply_to_default_shell_in(
        cmd,
        resolved_shell,
        is_default_shell,
        has_custom_args,
        &config_dir,
    );
}

pub fn apply_to_default_shell_in(
    cmd: &mut CommandBuilder,
    resolved_shell: &str,
    is_default_shell: bool,
    has_custom_args: bool,
    base_dir: &Path,
) {
    if !is_default_shell || has_custom_args || cfg!(windows) {
        return;
    }
    let disabled = cmd
        .get_env(SHELL_INTEGRATION_ENV)
        .and_then(|v| v.to_str())
        .filter(|v| !v.is_empty())
        .or_else(|| {
            cmd.get_env(LEGACY_SHELL_INTEGRATION_ENV)
                .and_then(|v| v.to_str())
                .filter(|v| !v.is_empty())
        })
        .is_some_and(|v| v.trim() == "0");
    if disabled {
        return;
    }
    let Some(shell_name) = Path::new(resolved_shell)
        .file_name()
        .and_then(|n| n.to_str())
    else {
        return;
    };
    if !matches!(shell_name, "zsh" | "bash" | "fish") {
        return;
    }

    let Ok(root) = ensure_scripts_in(base_dir) else {
        return;
    };

    match shell_name {
        "zsh" => {
            let existing_zdotdir = cmd
                .get_env("ZDOTDIR")
                .map(|v| v.to_os_string())
                .filter(|v| !v.is_empty());
            match existing_zdotdir {
                Some(orig) => {
                    cmd.env(USER_ZDOTDIR_ENV, &orig);
                    cmd.env(LEGACY_USER_ZDOTDIR_ENV, orig);
                }
                None => {
                    cmd.env_remove(USER_ZDOTDIR_ENV);
                    cmd.env_remove(LEGACY_USER_ZDOTDIR_ENV);
                }
            }
            let zsh_shim = root.join("zsh");
            cmd.env(ZSH_SHIM_DIR_ENV, &zsh_shim);
            cmd.env(LEGACY_ZSH_SHIM_DIR_ENV, &zsh_shim);
            cmd.env("ZDOTDIR", &zsh_shim);
        }
        "fish" => {
            let fish_data = root.join("fish");
            let existing = cmd
                .get_env("XDG_DATA_DIRS")
                .and_then(|v| v.to_str())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("/usr/local/share:/usr/share");
            let combined = format!("{}:{existing}", fish_data.display());
            cmd.env("XDG_DATA_DIRS", combined);
        }
        "bash" => {
            let bash_rc = root.join("bash").join("gitpulse.bash");
            cmd.env(BASH_INTEGRATION_PATH_ENV, &bash_rc);
            cmd.env(LEGACY_BASH_INTEGRATION_PATH_ENV, &bash_rc);
            cmd.get_argv_mut().truncate(1);
            cmd.arg("--rcfile");
            cmd.arg(&bash_rc);
            cmd.arg("-i");
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materializes_zsh_bash_and_fish_scripts_and_wires_zsh_zdotdir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = ensure_scripts_in(tmp.path()).expect("scripts written");
        assert!(root.join("zsh/.zshrc").is_file());
        assert!(root.join("zsh/.zshenv").is_file());
        assert!(root.join("bash/gitpulse.bash").is_file());
        assert!(root.join("fish/fish/vendor_conf.d/gitpulse.fish").is_file());

        let mut cmd = CommandBuilder::new("/bin/zsh");
        cmd.env("ZDOTDIR", "/Users/dev/.zsh");
        apply_to_default_shell_in(&mut cmd, "/bin/zsh", true, false, tmp.path());
        if cfg!(windows) {
            // No wiring on Windows by design: the scripts are written, the
            // shell is launched as it was, and the user's own ZDOTDIR stays.
            assert_eq!(cmd.get_env(USER_ZDOTDIR_ENV), None);
            assert_eq!(
                cmd.get_env("ZDOTDIR").and_then(|v| v.to_str()),
                Some("/Users/dev/.zsh")
            );
            return;
        }
        assert_eq!(
            cmd.get_env(USER_ZDOTDIR_ENV).and_then(|v| v.to_str()),
            Some("/Users/dev/.zsh")
        );
        assert_eq!(
            cmd.get_env(LEGACY_USER_ZDOTDIR_ENV)
                .and_then(|v| v.to_str()),
            Some("/Users/dev/.zsh")
        );
        assert_eq!(
            cmd.get_env(ZSH_SHIM_DIR_ENV).and_then(|v| v.to_str()),
            Some(root.join("zsh").to_str().unwrap())
        );
        assert_eq!(
            cmd.get_env(LEGACY_ZSH_SHIM_DIR_ENV)
                .and_then(|v| v.to_str()),
            Some(root.join("zsh").to_str().unwrap())
        );
        assert_eq!(
            cmd.get_env("ZDOTDIR").and_then(|v| v.to_str()),
            Some(root.join("zsh").to_str().unwrap())
        );
    }

    #[test]
    fn skips_agent_launchers_and_honors_opt_out_env() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut agent_cmd = CommandBuilder::new("/usr/local/bin/claude");
        apply_to_default_shell_in(
            &mut agent_cmd,
            "/usr/local/bin/claude",
            false,
            false,
            tmp.path(),
        );
        assert!(agent_cmd.get_env("ZDOTDIR").is_none());

        let mut opted_out = CommandBuilder::new("/bin/zsh");
        opted_out.env(SHELL_INTEGRATION_ENV, "0");
        apply_to_default_shell_in(&mut opted_out, "/bin/zsh", true, false, tmp.path());
        assert!(opted_out.get_env(ZSH_SHIM_DIR_ENV).is_none());
        assert!(opted_out.get_env(LEGACY_ZSH_SHIM_DIR_ENV).is_none());

        let mut legacy_opted_out = CommandBuilder::new("/bin/zsh");
        legacy_opted_out.env(LEGACY_SHELL_INTEGRATION_ENV, "0");
        apply_to_default_shell_in(&mut legacy_opted_out, "/bin/zsh", true, false, tmp.path());
        assert!(legacy_opted_out.get_env(ZSH_SHIM_DIR_ENV).is_none());
    }

    #[test]
    fn prepends_fish_vendor_conf_without_losing_system_xdg_data_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut cmd = CommandBuilder::new("/opt/homebrew/bin/fish");
        apply_to_default_shell_in(&mut cmd, "/opt/homebrew/bin/fish", true, false, tmp.path());
        if cfg!(windows) {
            // No wiring on Windows by design.
            assert_eq!(cmd.get_env("XDG_DATA_DIRS"), None);
            return;
        }
        let xdg = cmd
            .get_env("XDG_DATA_DIRS")
            .and_then(|v| v.to_str())
            .expect("XDG_DATA_DIRS set");
        assert!(xdg.contains("shell-integration/fish:/usr/local/share:/usr/share"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn zsh_and_bash_integration_encode_utf8_cwd_and_wire_bash_rcfile() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = ensure_scripts_in(tmp.path()).expect("scripts written");

        let mut bash_cmd = CommandBuilder::new("/bin/bash");
        bash_cmd.arg("-l");
        apply_to_default_shell_in(&mut bash_cmd, "/bin/bash", true, false, tmp.path());
        let argv: Vec<String> = bash_cmd
            .get_argv()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            argv,
            vec![
                "/bin/bash".to_string(),
                "--rcfile".to_string(),
                root.join("bash/gitpulse.bash")
                    .to_string_lossy()
                    .into_owned(),
                "-i".to_string(),
            ]
        );

        use crate::procguard::LockedSpawn;

        let cafe = tmp.path().join("caf\u{00e9}");
        std::fs::create_dir_all(&cafe).expect("mkdir cafe");
        let zsh_out = std::process::Command::new("/bin/zsh")
            .current_dir(&cafe)
            .env("ZDOTDIR", root.join("zsh"))
            .env("GITPULSE_ZSH_SHIM_DIR", root.join("zsh"))
            .env(USER_ZDOTDIR_ENV, tmp.path())
            .args(["-i", "-c", "__gitpulse_precmd"])
            .output_locked()
            .expect("run zsh");
        let zsh_raw = String::from_utf8_lossy(&zsh_out.stdout);
        assert!(
            zsh_raw.contains("caf%C3%A9") || zsh_raw.contains("cafe%CC%81"),
            "zsh OSC 7 did not UTF-8 percent-encode non-ASCII path: {zsh_raw:?}"
        );
        assert!(
            !zsh_raw.contains("caf%E9"),
            "zsh encoded codepoint instead of UTF-8 bytes: {zsh_raw:?}"
        );

        let bash_out = std::process::Command::new("/bin/bash")
            .current_dir(&cafe)
            .env("HOME", tmp.path())
            .args([
                "--rcfile",
                &root.join("bash/gitpulse.bash").to_string_lossy(),
                "-i",
                "-c",
                "__gitpulse_precmd",
            ])
            .output_locked()
            .expect("run bash");
        let bash_raw = String::from_utf8_lossy(&bash_out.stdout);
        assert!(
            bash_raw.contains("caf%C3%A9") || bash_raw.contains("cafe%CC%81"),
            "bash OSC 7 did not UTF-8 percent-encode non-ASCII path: {bash_raw:?}"
        );
        assert!(
            !bash_raw.contains("-%"),
            "bash produced signed-char hex: {bash_raw:?}"
        );
    }

    /// The OSC 133 marks an interactive bash with the integration emits, in order:
    /// `C`, `A`, `D;<code>`.
    #[cfg(unix)]
    fn bash_marks(script: &str) -> Vec<String> {
        use crate::procguard::LockedSpawn;
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = ensure_scripts_in(tmp.path()).expect("scripts written");
        let out = std::process::Command::new("/bin/bash")
            .env("HOME", tmp.path())
            .env("BASH_SILENCE_DEPRECATION_WARNING", "1")
            .args([
                "-c",
                // An interactive bash reading its commands from a pipe still
                // runs PROMPT_COMMAND and the DEBUG trap for each line.
                r#"printf '%s' "$1" | /bin/bash --rcfile "$0" -i 2>&1"#,
                &root.join("bash/gitpulse.bash").to_string_lossy(),
                script,
            ])
            .output_locked()
            .expect("run bash");
        let raw = String::from_utf8_lossy(&out.stdout);
        raw.split("\x1b]133;")
            .skip(1)
            .filter_map(|rest| rest.split('\x07').next())
            .map(str::to_owned)
            .collect()
    }

    /// The DEBUG trap fires for the prompt hook's own commands too. Before it
    /// skipped them, the first prompt and every empty Enter emitted a command
    /// start and finish (bash < 4.4) or a lone finish (bash >= 4.4) for a command
    /// nobody ran, and a wait for "the sent command finished" answered with that
    /// phantom: `sticky_herdr_status_does_not_wake_subsequent_send_and_wait_in_zero_ms`
    /// failed on macOS CI (bash 3.2) when the first prompt landed after the send.
    #[cfg(unix)]
    #[test]
    fn bash_marks_only_commands_that_ran() {
        // An empty line, a command that succeeds, a command that fails.
        let marks = bash_marks("\nprintf hi\nfalse\n");
        let first_prompt = marks
            .iter()
            .position(|mark| mark == "A")
            .unwrap_or_else(|| panic!("no prompt mark at all: {marks:?}"));
        assert!(
            marks[..first_prompt].is_empty(),
            "a command mark before the first prompt, for no command: {marks:?}"
        );
        // These hold whether `C` comes from the DEBUG trap (bash < 4.4) or
        // from PS0 (bash >= 4.4): one finish per command that ran, none for the
        // empty line, with each command's own exit status.
        let finishes: Vec<&str> = marks
            .iter()
            .filter(|mark| mark.starts_with('D'))
            .map(String::as_str)
            .collect();
        assert_eq!(
            finishes,
            vec!["D;0", "D;1"],
            "a finish for a command that did not run: {marks:?}"
        );
    }
}
