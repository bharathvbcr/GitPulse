//! Whether the login Keychain may be asked at all.
//!
//! `security` finds the login Keychain under `$HOME`. When `HOME` points
//! anywhere but the account's own home (a hermetic test home, a probe's
//! scratch home), there is no login Keychain there, and macOS does not fail
//! the call: it puts a modal "Keychain Not Found" dialog on the person's
//! screen, once per call, for as long as the caller keeps asking. Every
//! driver of the default Keychain (`claude_key`, `account::keychain`) asks
//! [`refusal`] first and refuses instead of spawning `security`.

use std::path::{Path, PathBuf};

/// Why the default (login) Keychain must not be asked in this process, or
/// `None` when it may.
pub(crate) fn refusal() -> Option<String> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    default_keychain_refusal(home.as_deref(), account_home().as_deref())
}

/// The decision behind [`refusal`], with its inputs passed in. The default
/// Keychain is used only when `HOME` is the account's home, or unset
/// (`security` then resolves the account's home itself). When the account's
/// home cannot be resolved the answer is a refusal, not a guess.
pub(crate) fn default_keychain_refusal(
    home: Option<&Path>,
    account_home: Option<&Path>,
) -> Option<String> {
    let home = home.filter(|home| !home.as_os_str().is_empty())?;
    let Some(account_home) = account_home else {
        return Some("the account's home directory could not be resolved".to_string());
    };
    let same = |a: &Path, b: &Path| match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a.components().eq(b.components()),
    };
    if same(home, account_home) {
        None
    } else {
        Some(format!(
            "HOME ({}) is not the account's home ({}), so there is no login Keychain to use",
            home.display(),
            account_home.display()
        ))
    }
}

/// The current user's home directory from the password database, independent
/// of `HOME`.
#[cfg(unix)]
pub(crate) fn account_home() -> Option<PathBuf> {
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;
    const MAX_BUF: usize = 64 * 1024;
    let mut len = 4096;
    loop {
        let mut buf = vec![0u8; len];
        // SAFETY: `passwd` is plain C data; all-zero is a valid value to be
        // overwritten by `getpwuid_r`.
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: every pointer is to a live local, `buf` is `len` bytes, and
        // the strings `pwd` points at live in `buf`, read before it drops.
        let rc = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                &mut pwd,
                buf.as_mut_ptr().cast(),
                buf.len(),
                &mut found,
            )
        };
        if rc == libc::ERANGE && len < MAX_BUF {
            len *= 2;
            continue;
        }
        if rc != 0 || found.is_null() || pwd.pw_dir.is_null() {
            return None;
        }
        // SAFETY: `pw_dir` is a NUL-terminated string inside `buf`.
        let dir = unsafe { CStr::from_ptr(pwd.pw_dir) };
        let dir = PathBuf::from(OsStr::from_bytes(dir.to_bytes()));
        return (!dir.as_os_str().is_empty()).then_some(dir);
    }
}

#[cfg(not(unix))]
pub(crate) fn account_home() -> Option<PathBuf> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Lib tests run under the hermetic `HOME` (`test_support::env`), which
    /// has no login Keychain: the very case that must be refused.
    #[test]
    fn the_hermetic_test_home_is_refused() {
        let account = account_home().expect("the password database names this user's home");
        let why = refusal().expect("the hermetic HOME must not reach the login Keychain");
        // The messages name what is missing without echoing `why`, which
        // carries the account's home path, into the test log.
        assert!(
            why.contains("not the account's home"),
            "the refusal does not say the HOME is not the account's home"
        );
        assert!(
            why.contains(&account.display().to_string()),
            "the refusal does not name the account's home"
        );
    }

    #[test]
    fn the_default_keychain_is_allowed_only_for_the_accounts_own_home() {
        let dir = tempfile::TempDir::new().unwrap();
        let account = dir.path().join("me");
        let other = dir.path().join("probe-home");
        std::fs::create_dir_all(&account).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        // The account's own home, also spelled through a symlink and with a
        // trailing separator.
        assert_eq!(
            default_keychain_refusal(Some(&account), Some(&account)),
            None
        );
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&account, &link).unwrap();
        assert_eq!(default_keychain_refusal(Some(&link), Some(&account)), None);
        let trailing = PathBuf::from(format!("{}/", account.display()));
        assert_eq!(
            default_keychain_refusal(Some(&trailing), Some(&account)),
            None
        );
        // HOME unset or empty: `security` resolves the account's home itself.
        assert_eq!(default_keychain_refusal(None, Some(&account)), None);
        assert_eq!(
            default_keychain_refusal(Some(Path::new("")), Some(&account)),
            None
        );
        // A different directory, one that does not exist, or an unknown
        // account home is refused.
        assert!(default_keychain_refusal(Some(&other), Some(&account)).is_some());
        assert!(default_keychain_refusal(Some(&dir.path().join("gone")), Some(&account)).is_some());
        assert!(default_keychain_refusal(Some(&account), None).is_some());
    }
}
