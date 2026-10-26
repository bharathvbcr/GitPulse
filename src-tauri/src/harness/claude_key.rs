//! The Claude API key GitPulse hands Manvi when a person picks Claude as
//! Manvi's model.
//!
//! It lives in the login Keychain and is read only at the moment a `manvi
//! serve` child is spawned with a Claude selection: never written to the
//! profile, a settings file, the log or the webview. The webview can store a
//! key, delete it and ask whether one is present, and that is all.
//!
//! The Keychain is driven through `/usr/bin/security` rather than a binding
//! crate. The key travels on the tool's stdin (`security -i`), never in its
//! argv, where any process on the machine could read it from the process
//! table. `security -i` exits 0 when a command inside it fails (a missing
//! keychain is reported nowhere), so a save is confirmed by reading the item
//! back, not by the exit status.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crate::engine::git_cli::run_bounded_capped;

/// The variable Manvi's Anthropic adapter reads its key from. Also the name
/// the spawned harness must promise to withhold from its own children.
pub(crate) const CLAUDE_KEY_ENV: &str = "ANTHROPIC_API_KEY";

const SERVICE: &str = "com.gitpulse.desktop.anthropic-api-key";
const ACCOUNT: &str = "manvi";
/// `security`'s exit status for "no such item".
const NOT_FOUND: i32 = 44;
/// Long enough for a person to answer a Keychain unlock prompt; a locked
/// keychain otherwise blocks the call until someone does.
const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(60);
const OUTPUT_CAP: usize = 16 * 1024;
const MIN_KEY_LEN: usize = 20;
const MAX_KEY_LEN: usize = 256;

/// Checks that `raw` looks like an Anthropic API key and returns it trimmed.
///
/// Strict on purpose. The key is interpolated into a `security -i` command
/// line, so only characters that cannot split or quote one are accepted, and
/// the `sk-ant-` prefix catches a paste of the wrong thing (a workspace id, an
/// OAuth token) before it is stored and only fails later as a 401.
pub(crate) fn validate(raw: &str) -> Result<&str, String> {
    let key = raw.trim();
    if !key.starts_with("sk-ant-") {
        return Err("That is not an Anthropic API key: keys start with `sk-ant-`.".into());
    }
    if key.len() < MIN_KEY_LEN || key.len() > MAX_KEY_LEN {
        return Err(format!(
            "An Anthropic API key is between {MIN_KEY_LEN} and {MAX_KEY_LEN} characters."
        ));
    }
    if !key
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("An Anthropic API key contains only letters, digits, `-` and `_`.".into());
    }
    Ok(key)
}

/// The login Keychain, as reached through one `security` program.
pub(crate) struct Keychain {
    program: PathBuf,
}

impl Keychain {
    /// The system's `security`. Keychain storage exists only on macOS; on any
    /// other platform every operation is refused rather than silently storing
    /// nothing.
    pub(crate) fn system() -> Result<Self, String> {
        if cfg!(target_os = "macos") {
            Ok(Self {
                program: PathBuf::from("/usr/bin/security"),
            })
        } else {
            Err("Saving a Claude API key needs the macOS Keychain.".into())
        }
    }

    #[cfg(all(test, unix))]
    fn at(program: PathBuf) -> Self {
        Self { program }
    }

    /// Stores `raw` (validated first), replacing any key already saved.
    pub(crate) fn save(&self, raw: &str) -> Result<(), String> {
        let key = validate(raw)?;
        let script = format!("add-generic-password -U -s {SERVICE} -a {ACCOUNT} -w {key}\n");
        let (ok, _, stderr, _) = self.run_with_code(&["-i"], Some(script.as_bytes()), key)?;
        if !ok {
            return Err(format!("The Keychain refused the key: {stderr}"));
        }
        match self.load()? {
            Some(stored) if stored == key => Ok(()),
            _ => Err("The Keychain did not keep the key: reading it back found a different value or none.".into()),
        }
    }

    /// The saved key, or `None` when there is none. A stored value that is not
    /// a key is an error, not a key: it would only fail later as a 401.
    pub(crate) fn load(&self) -> Result<Option<String>, String> {
        let (ok, stdout, stderr, code) = self.run_with_code(
            &["find-generic-password", "-s", SERVICE, "-a", ACCOUNT, "-w"],
            None,
            "",
        )?;
        if code == NOT_FOUND {
            return Ok(None);
        }
        if !ok {
            return Err(format!(
                "Could not read the Claude API key from the Keychain: {stderr}"
            ));
        }
        let key = validate(&stdout).map_err(|_| {
            "The Keychain item for the Claude API key does not hold a valid key; save it again."
                .to_string()
        })?;
        Ok(Some(key.to_string()))
    }

    /// Whether a key is saved, without handing it to the caller.
    pub(crate) fn present(&self) -> Result<bool, String> {
        self.load().map(|key| key.is_some())
    }

    /// Removes the saved key. Removing a key that is not there succeeds.
    pub(crate) fn delete(&self) -> Result<(), String> {
        let (ok, _, stderr, code) = self.run_with_code(
            &["delete-generic-password", "-s", SERVICE, "-a", ACCOUNT],
            None,
            "",
        )?;
        if ok || code == NOT_FOUND {
            Ok(())
        } else {
            Err(format!(
                "Could not remove the Claude API key from the Keychain: {stderr}"
            ))
        }
    }

    /// Runs `security` with a minimal environment and returns its success,
    /// stdout, stderr (with `secret` scrubbed) and exit status.
    fn run_with_code(
        &self,
        args: &[&str],
        stdin: Option<&[u8]>,
        secret: &str,
    ) -> Result<(bool, String, String, i32), String> {
        let mut command = Command::new(&self.program);
        command.args(args).env_clear();
        // `security` finds the login keychain through the user's home.
        for name in ["HOME", "USER", "LOGNAME", "TMPDIR"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let run = run_bounded_capped(command, "security", KEYCHAIN_TIMEOUT, stdin, OUTPUT_CAP)?
            .require_complete("security")?;
        let scrub = |bytes: &[u8]| {
            let text = String::from_utf8_lossy(bytes).trim().to_string();
            // A tool that echoes its command line on a parse error would echo
            // the key with it.
            if secret.is_empty() {
                text
            } else {
                text.replace(secret, "[redacted]")
            }
        };
        Ok((
            run.success,
            String::from_utf8_lossy(&run.stdout).into_owned(),
            scrub(&run.stderr),
            run.status_code,
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const KEY: &str = "sk-ant-api03-Test_key-0123456789abcdef";

    /// A `security` stand-in that keeps one item in a file beside it, with
    /// the real tool's exit statuses. `-i` reads its command from stdin, as
    /// the real one does, and like the real one exits 0 whatever happens.
    fn fake_security(dir: &std::path::Path, drop_writes: bool) -> Keychain {
        let item = dir.join("item");
        let argv = dir.join("argv");
        let write = if drop_writes {
            ":".to_string()
        } else {
            format!("printf '%s' \"$key\" > '{}'", item.display())
        };
        let script = format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{argv}'
case "$1" in
  -i)
    read -r line
    key=${{line##* -w }}
    {write}
    exit 0 ;;
  find-generic-password)
    [ -f '{item}' ] || {{ echo "could not be found" >&2; exit 44; }}
    cat '{item}'; echo; exit 0 ;;
  delete-generic-password)
    [ -f '{item}' ] || exit 44
    rm '{item}'; exit 0 ;;
esac
exit 2
"#,
            argv = argv.display(),
            item = item.display(),
        );
        let program = dir.join("security");
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        Keychain::at(program)
    }

    #[test]
    fn validate_accepts_keys_and_refuses_what_could_split_a_command_line() {
        assert_eq!(validate(&format!("  {KEY}\n")), Ok(KEY));
        for bad in [
            "",
            "sk-proj-0123456789abcdefghij",
            "sk-ant-short",
            "sk-ant-api03-has space-0123456789",
            "sk-ant-api03-0123456789abcdef\n-w other",
            "sk-ant-api03-0123456789abcdef;rm",
            "sk-ant-api03-0123456789abcdef\"quoted",
            &format!("sk-ant-{}", "a".repeat(300)),
        ] {
            assert!(validate(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn save_load_delete_round_trip_and_the_key_never_reaches_argv() {
        let dir = tempfile::TempDir::new().unwrap();
        let keychain = fake_security(dir.path(), false);
        assert_eq!(keychain.load(), Ok(None));
        assert_eq!(keychain.present(), Ok(false));
        keychain.save(&format!(" {KEY} ")).unwrap();
        assert_eq!(keychain.load(), Ok(Some(KEY.to_string())));
        assert_eq!(keychain.present(), Ok(true));
        keychain.delete().unwrap();
        assert_eq!(keychain.load(), Ok(None));
        // Deleting what is not there is not a failure.
        keychain.delete().unwrap();
        let argv = std::fs::read_to_string(dir.path().join("argv")).unwrap();
        assert!(
            !argv.contains(KEY),
            "the key must travel on stdin, never argv:\n{argv}"
        );
    }

    #[test]
    fn save_refuses_when_the_keychain_did_not_keep_the_key() {
        let dir = tempfile::TempDir::new().unwrap();
        let keychain = fake_security(dir.path(), true);
        let error = keychain.save(KEY).unwrap_err();
        assert!(error.contains("did not keep"), "{error}");
    }

    #[test]
    fn save_refuses_an_invalid_key_before_running_anything() {
        let dir = tempfile::TempDir::new().unwrap();
        let keychain = fake_security(dir.path(), false);
        assert!(keychain.save("sk-ant-x -w injected").is_err());
        assert!(!dir.path().join("argv").exists(), "security must not run");
    }

    #[test]
    fn a_stored_value_that_is_not_a_key_is_an_error_not_a_key() {
        let dir = tempfile::TempDir::new().unwrap();
        let keychain = fake_security(dir.path(), false);
        std::fs::write(dir.path().join("item"), "not-a-key").unwrap();
        let error = keychain.load().unwrap_err();
        assert!(error.contains("does not hold a valid key"), "{error}");
        assert!(
            !error.contains("not-a-key"),
            "the stored value must not be echoed: {error}"
        );
    }
}
