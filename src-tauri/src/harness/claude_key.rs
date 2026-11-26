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

use crate::ipc_error::CommandError;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crate::engine::process::run_bounded_capped;

/// The variable Manvi's Anthropic adapter reads its key from. Also the name
/// the spawned harness must promise to withhold from its own children.
pub(crate) const CLAUDE_KEY_ENV: &str = "ANTHROPIC_API_KEY";

const SERVICE: &str = "dev.vbcr.waylode.anthropic-api-key";
/// Pre-rename service names, ordered newest-first. Read as a fallback and moved
/// to [`SERVICE`] on first use; see [`Keychain::load`].
const LEGACY_SERVICES: [&str; 2] = [
    "dev.vbcr.gitpulse.anthropic-api-key",
    "com.gitpulse.desktop.anthropic-api-key",
];
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
        if !cfg!(target_os = "macos") {
            return Err("Saving a Claude API key needs the macOS Keychain.".into());
        }
        if let Some(why) = super::login_keychain::refusal() {
            return Err(format!(
                "The Claude API key cannot use the login Keychain: {why}"
            ));
        }
        Ok(Self {
            program: PathBuf::from("/usr/bin/security"),
        })
    }

    #[cfg(all(test, unix))]
    pub(crate) fn at(program: PathBuf) -> Self {
        Self { program }
    }

    /// Stores `raw` (validated first), replacing any key already saved.
    pub(crate) fn save(&self, raw: &str) -> Result<(), CommandError> {
        let key = validate(raw)?;
        self.store(SERVICE, key)?;
        // The new item wins over old ones on every read, so leftover old items
        // are stale, not wrong; they are removed so that Remove cannot be
        // undone by them. Failing to remove one does not fail the save.
        self.remove_legacy_items();
        Ok(())
    }

    fn remove_legacy_items(&self) {
        for legacy in LEGACY_SERVICES {
            if let Err(error) = self.remove(legacy) {
                log::warn!(target: "harness", "the pre-rename Claude API key item ({legacy}) was not removed: {error}", error = error.message());
            }
        }
    }

    /// Writes `key` under `service` and confirms it by reading it back.
    fn store(&self, service: &str, key: &str) -> Result<(), CommandError> {
        let script = format!("add-generic-password -U -s {service} -a {ACCOUNT} -w {key}\n");
        let (ok, _, stderr, _) = self.run_with_code(&["-i"], Some(script.as_bytes()), key)?;
        if !ok {
            return Err(format!("The Keychain refused the key: {stderr}").into());
        }
        match self.load_service(service)? {
            Some(stored) if stored == key => Ok(()),
            _ => Err("The Keychain did not keep the key: reading it back found a different value or none.".into()),
        }
    }

    /// The saved key, or `None` when there is none. A stored value that is not
    /// a key is an error, not a key: it would only fail later as a 401.
    ///
    /// Keys saved before the rename live under `LEGACY_SERVICES` (ordered
    /// newest-first). The first tier that holds a key is copied to [`SERVICE`],
    /// and legacy items are deleted only once the copy reads back. Any step of
    /// that move can fail without losing the key.
    pub(crate) fn load(&self) -> Result<Option<String>, CommandError> {
        if let Some(key) = self.load_service(SERVICE)? {
            return Ok(Some(key));
        }
        for legacy in LEGACY_SERVICES {
            let Some(key) = self.load_service(legacy)? else {
                continue;
            };
            match self.store(SERVICE, &key) {
                Ok(()) => self.remove_legacy_items(),
                Err(error) => {
                    log::warn!(target: "harness", "the Claude API key was not moved from {legacy} to its new Keychain item: {error}", error = error.message());
                }
            }
            return Ok(Some(key));
        }
        Ok(None)
    }

    fn load_service(&self, service: &str) -> Result<Option<String>, CommandError> {
        let (ok, stdout, stderr, code) = self.run_with_code(
            &["find-generic-password", "-s", service, "-a", ACCOUNT, "-w"],
            None,
            "",
        )?;
        if code == NOT_FOUND {
            return Ok(None);
        }
        if !ok {
            return Err(
                format!("Could not read the Claude API key from the Keychain: {stderr}").into(),
            );
        }
        let key = validate(&stdout).map_err(|_| {
            "The Keychain item for the Claude API key does not hold a valid key; save it again."
                .to_string()
        })?;
        Ok(Some(key.to_string()))
    }

    /// Whether a key is saved, without handing it to the caller.
    pub(crate) fn present(&self) -> Result<bool, CommandError> {
        self.load().map(|key| key.is_some())
    }

    /// Whether the Keychain holds an item for Waylode's Claude key, checked
    /// from its unencrypted attributes (`find-generic-password` without `-w`)
    /// so periodic background checks never decrypt the secret into memory or
    /// trigger a macOS login-Keychain unlock prompt while the screen is locked.
    pub(crate) fn item_exists(&self) -> Result<bool, CommandError> {
        // Keys not yet moved to the new service still count: the check must
        // not report a saved key missing just because nothing has read it yet.
        if self.item_exists_in(SERVICE)? {
            return Ok(true);
        }
        for legacy in LEGACY_SERVICES {
            if self.item_exists_in(legacy)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn item_exists_in(&self, service: &str) -> Result<bool, CommandError> {
        let (ok, _, stderr, code) = self.run_with_code(
            &["find-generic-password", "-s", service, "-a", ACCOUNT],
            None,
            "",
        )?;
        if code == NOT_FOUND {
            return Ok(false);
        }
        if !ok {
            return Err(
                format!("Could not check the Claude API key in the Keychain: {stderr}").into(),
            );
        }
        Ok(true)
    }

    /// Removes the saved key, under all service names: a key removed from the
    /// new item alone would come back from a legacy one. Removing a key that is
    /// not there succeeds.
    pub(crate) fn delete(&self) -> Result<(), CommandError> {
        let mut first_err = self.remove(SERVICE).err();
        for legacy in LEGACY_SERVICES {
            if let Err(err) = self.remove(legacy) {
                if first_err.is_none() {
                    first_err = Some(err);
                }
            }
        }
        match first_err {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    fn remove(&self, service: &str) -> Result<(), CommandError> {
        let (ok, _, stderr, code) = self.run_with_code(
            &["delete-generic-password", "-s", service, "-a", ACCOUNT],
            None,
            "",
        )?;
        if ok || code == NOT_FOUND {
            Ok(())
        } else {
            Err(format!("Could not remove the Claude API key from the Keychain: {stderr}").into())
        }
    }

    /// Runs `security` with a minimal environment and returns its success,
    /// stdout, stderr (with `secret` scrubbed) and exit status.
    fn run_with_code(
        &self,
        args: &[&str],
        stdin: Option<&[u8]>,
        secret: &str,
    ) -> Result<(bool, String, String, i32), CommandError> {
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
        fake_security_with(dir, drop_writes, false)
    }

    /// Items are kept per service: `item` for `dev.vbcr.waylode.*`,
    /// `item-v2` for `dev.vbcr.gitpulse.*`, and `item-v1` for
    /// `com.gitpulse.desktop.*` (which `protect_legacy` refuses to delete).
    fn fake_security_with(
        dir: &std::path::Path,
        drop_writes: bool,
        protect_legacy: bool,
    ) -> Keychain {
        let argv = dir.join("argv");
        let new = dir.join("item");
        let legacy_v2 = dir.join("item-v2");
        let legacy_v1 = dir.join("item-v1");
        let write = if drop_writes {
            ":".to_string()
        } else {
            "printf '%s' \"$key\" > \"$item\"".to_string()
        };
        let refuse = if protect_legacy {
            r#"[ "$item" = '{legacy_v2}' ] || [ "$item" = '{legacy_v1}' ] && exit 1"#
                .replace("{legacy_v2}", &legacy_v2.display().to_string())
                .replace("{legacy_v1}", &legacy_v1.display().to_string())
        } else {
            ":".to_string()
        };
        let script = format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{argv}'
pick() {{
  case "$1" in
    dev.vbcr.gitpulse*) item='{legacy_v2}' ;;
    com.gitpulse.desktop*) item='{legacy_v1}' ;;
    *) item='{new}' ;;
  esac
}}
case "$1" in
  -i)
    read -r line
    key=${{line##* -w }}
    svc=${{line#* -s }}; svc=${{svc%% *}}
    pick "$svc"
    {write}
    exit 0 ;;
  find-generic-password)
    pick "$3"
    [ -f "$item" ] || {{ echo "could not be found" >&2; exit 44; }}
    cat "$item"; echo; exit 0 ;;
  delete-generic-password)
    pick "$3"
    [ -f "$item" ] || exit 44
    {refuse}
    rm "$item"; exit 0 ;;
esac
exit 2
"#,
            argv = argv.display(),
            new = new.display(),
            legacy_v2 = legacy_v2.display(),
            legacy_v1 = legacy_v1.display(),
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
        assert!(
            error.contains("did not keep"),
            "{error}",
            error = error.message()
        );
    }

    /// Lib tests run under the hermetic `HOME`, which has no login Keychain:
    /// asking `security` there puts a "Keychain Not Found" dialog on screen.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_system_keychain_is_refused_under_a_home_that_is_not_the_accounts() {
        let error = Keychain::system()
            .err()
            .expect("the login Keychain must not be asked under a foreign HOME");
        assert!(error.contains("not the account's home"), "{error}");
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
        assert!(
            error.contains("does not hold a valid key"),
            "{error}",
            error = error.message()
        );
        assert!(
            !error.contains("not-a-key"),
            "the stored value must not be echoed: {error}",
            error = error.message()
        );
    }

    #[test]
    fn item_exists_checks_metadata_without_passing_password_decrypt_flags() {
        let dir = tempfile::TempDir::new().unwrap();
        let keychain = fake_security(dir.path(), false);
        assert_eq!(keychain.item_exists(), Ok(false));
        std::fs::write(dir.path().join("item"), KEY).unwrap();
        assert_eq!(keychain.item_exists(), Ok(true));
        let argv = std::fs::read_to_string(dir.path().join("argv")).unwrap();
        for line in argv.lines() {
            assert!(
                !line
                    .split_whitespace()
                    .any(|arg| arg == "-w" || arg == "-g"),
                "item_exists must never pass -w or -g: {line}"
            );
        }
    }

    fn argv(dir: &std::path::Path) -> String {
        std::fs::read_to_string(dir.join("argv")).unwrap_or_default()
    }

    #[test]
    fn the_service_is_under_the_vbcr_namespace() {
        assert_eq!(SERVICE, "dev.vbcr.waylode.anthropic-api-key");
        assert_eq!(
            LEGACY_SERVICES,
            [
                "dev.vbcr.gitpulse.anthropic-api-key",
                "com.gitpulse.desktop.anthropic-api-key",
            ]
        );
    }

    #[test]
    fn a_key_saved_before_the_rename_is_found_and_moved_to_the_new_service() {
        let dir = tempfile::TempDir::new().unwrap();
        let keychain = fake_security(dir.path(), false);
        std::fs::write(dir.path().join("item-v1"), KEY).unwrap();
        assert_eq!(keychain.load(), Ok(Some(KEY.to_string())));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("item")).unwrap(),
            KEY,
            "the key must now live under the new service"
        );
        assert!(
            !dir.path().join("item-v1").exists(),
            "the old item goes only after the copy read back"
        );
        // Idempotent: the second read is served by the new item alone.
        let before = argv(dir.path());
        assert_eq!(keychain.load(), Ok(Some(KEY.to_string())));
        let after = argv(dir.path());
        assert!(
            !after[before.len()..].contains("com.gitpulse.desktop")
                && !after[before.len()..].contains("dev.vbcr.gitpulse"),
            "a migrated key must not consult legacy services: {after}"
        );
        assert!(
            !argv(dir.path()).contains(KEY),
            "the key must stay off argv"
        );
    }

    #[test]
    fn three_tier_precedence_prefers_dev_vbcr_gitpulse_over_stale_com_gitpulse_desktop() {
        let dir = tempfile::TempDir::new().unwrap();
        let keychain = fake_security(dir.path(), false);
        let v1_stale = "sk-ant-api03-Stale_v1-0123456789abcdef";
        let v2_fresh = "sk-ant-api03-Fresh_v2-0123456789abcdef";
        std::fs::write(dir.path().join("item-v1"), v1_stale).unwrap();
        std::fs::write(dir.path().join("item-v2"), v2_fresh).unwrap();

        assert_eq!(keychain.load(), Ok(Some(v2_fresh.to_string())));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("item")).unwrap(),
            v2_fresh
        );
        assert!(!dir.path().join("item-v2").exists());
        assert!(!dir.path().join("item-v1").exists());
    }

    #[test]
    fn a_failed_copy_keeps_the_old_item_and_still_returns_the_key() {
        let dir = tempfile::TempDir::new().unwrap();
        let keychain = fake_security(dir.path(), true);
        std::fs::write(dir.path().join("item-v2"), KEY).unwrap();
        assert_eq!(keychain.load(), Ok(Some(KEY.to_string())));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("item-v2")).unwrap(),
            KEY,
            "an unconfirmed copy must never cost the only copy"
        );
        assert!(!dir.path().join("item").exists());
    }

    #[test]
    fn a_failed_delete_of_the_old_item_does_not_lose_or_hide_the_key() {
        let dir = tempfile::TempDir::new().unwrap();
        let keychain = fake_security_with(dir.path(), false, true);
        std::fs::write(dir.path().join("item-v2"), KEY).unwrap();
        assert_eq!(keychain.load(), Ok(Some(KEY.to_string())));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("item")).unwrap(),
            KEY
        );
        // The new item answers from now on.
        assert_eq!(keychain.load(), Ok(Some(KEY.to_string())));
    }

    #[test]
    fn the_new_item_wins_over_stale_old_ones() {
        let dir = tempfile::TempDir::new().unwrap();
        let keychain = fake_security(dir.path(), false);
        let newer = "sk-ant-api03-Newer_key-0123456789abcdef";
        std::fs::write(dir.path().join("item-v1"), KEY).unwrap();
        std::fs::write(dir.path().join("item-v2"), KEY).unwrap();
        std::fs::write(dir.path().join("item"), newer).unwrap();
        assert_eq!(keychain.load(), Ok(Some(newer.to_string())));
    }

    #[test]
    fn saving_replaces_old_items_and_deleting_removes_all_three_tiers() {
        let dir = tempfile::TempDir::new().unwrap();
        let keychain = fake_security(dir.path(), false);
        std::fs::write(dir.path().join("item-v1"), KEY).unwrap();
        std::fs::write(dir.path().join("item-v2"), KEY).unwrap();
        let newer = "sk-ant-api03-Newer_key-0123456789abcdef";
        keychain.save(newer).unwrap();
        assert!(!dir.path().join("item-v1").exists());
        assert!(!dir.path().join("item-v2").exists());
        assert_eq!(keychain.load(), Ok(Some(newer.to_string())));

        // A removed key must not come back out of either pre-rename tier.
        std::fs::write(dir.path().join("item-v1"), KEY).unwrap();
        std::fs::write(dir.path().join("item-v2"), KEY).unwrap();
        keychain.delete().unwrap();
        assert!(!dir.path().join("item-v1").exists());
        assert!(!dir.path().join("item-v2").exists());
        assert_eq!(keychain.load(), Ok(None));
        assert_eq!(keychain.present(), Ok(false));
    }

    #[test]
    fn item_exists_counts_an_unmigrated_key_without_decrypting_it() {
        for tier in ["item-v2", "item-v1"] {
            let dir = tempfile::TempDir::new().unwrap();
            let keychain = fake_security(dir.path(), false);
            assert_eq!(keychain.item_exists(), Ok(false));
            std::fs::write(dir.path().join(tier), KEY).unwrap();
            assert_eq!(keychain.item_exists(), Ok(true));
            assert!(
                dir.path().join(tier).exists() && !dir.path().join("item").exists(),
                "the metadata check must not migrate, which would need the secret"
            );
            for line in argv(dir.path()).lines() {
                assert!(
                    !line
                        .split_whitespace()
                        .any(|arg| arg == "-w" || arg == "-g"),
                    "{line}"
                );
            }
        }
    }
}
