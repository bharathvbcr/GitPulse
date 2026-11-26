//! One-time carry-over of what macOS keys by the bundle identifier, from the
//! identifier GitPulse shipped under before VBCR Dev to
//! [`SHIPPED_IDENTIFIER`](super::launch).
//!
//! WKWebView keeps the page's website data (localStorage, IndexedDB: the
//! frontend's settings, layout and drafts) under
//! `~/Library/WebKit/<bundle identifier>/WebsiteData`. A renamed bundle opens
//! an empty store there, so without this every upgraded user would find the
//! app reset. Nothing else GitPulse owns is keyed this way: its profile lives
//! in `~/Library/Application Support/GitPulse`, the launch-at-login agent is
//! labelled by product name, and the Keychain items are migrated by
//! `harness::claude_key`.
//!
//! The copy runs before Tauri builds the webview, after the single-instance
//! guard has made this process the owner (so no older GitPulse is writing the
//! store), and never moves or deletes the old data: a downgrade, or a copy that
//! went wrong, still has the original.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The identifier GitPulse shipped under until the VBCR Dev rename.
pub const LEGACY_IDENTIFIER: &str = "com.gitpulse.desktop";

/// The WebKit directory every origin's data lives under, per identifier.
const WEBSITE_DATA: &str = "WebsiteData";
/// Where WebKit puts per-origin data. `Default/salt` seeds the hashed
/// directory names, so the salt must travel with the data it names.
const ORIGIN_DATA: &str = "Default";
const SALT: &str = "salt";
/// A WebKit data store is a few levels deep; refuse anything deeper rather
/// than follow a loop.
const MAX_DEPTH: usize = 16;
const MAX_ENTRIES: usize = 200_000;

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The old store was copied to the new identifier.
    Copied { files: usize },
    /// The old identifier left no website data.
    NothingToMigrate,
    /// The new identifier already holds website data; it is left alone.
    NewStoreInUse,
}

fn webkit_data(home: &Path, identifier: &str) -> PathBuf {
    home.join("Library/WebKit")
        .join(identifier)
        .join(WEBSITE_DATA)
}

/// Whether `dir` holds any file beyond the data-store salt.
fn holds_origin_data(dir: &Path) -> io::Result<bool> {
    let origin = dir.join(ORIGIN_DATA);
    let mut stack = vec![(origin, 0usize)];
    let mut seen = 0usize;
    while let Some((path, depth)) = stack.pop() {
        let entries = match fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            seen += 1;
            if seen > MAX_ENTRIES || depth >= MAX_DEPTH {
                // Too big or too deep to be an empty store.
                return Ok(true);
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                stack.push((entry.path(), depth + 1));
            } else if !(depth == 0 && entry.file_name() == SALT) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Copies `from` to `to`, regular files and directories only: a symlink or
/// special file in a WebKit store is not WebKit's, and is not followed.
fn copy_tree(from: &Path, to: &Path, depth: usize, files: &mut usize) -> io::Result<()> {
    if depth > MAX_DEPTH {
        return Err(io::Error::other("website data is nested too deeply"));
    }
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target, depth + 1, files)?;
        } else if kind.is_file() {
            *files += 1;
            if *files > MAX_ENTRIES {
                return Err(io::Error::other("website data has too many files"));
            }
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Carries `old` identifier's WebKit website data to `new` when `new` has none.
///
/// Copies into a sibling directory and renames it into place, so an
/// interrupted run leaves no half-populated store for WebKit to adopt; the
/// next run clears the leftover and starts again.
pub fn migrate_website_data(home: &Path, old: &str, new: &str) -> io::Result<Outcome> {
    let from = webkit_data(home, old);
    let to = webkit_data(home, new);
    if from == to || !holds_origin_data(&from)? {
        return Ok(Outcome::NothingToMigrate);
    }
    if holds_origin_data(&to)? {
        return Ok(Outcome::NewStoreInUse);
    }
    let parent = to
        .parent()
        .ok_or_else(|| io::Error::other("no WebKit directory"))?;
    fs::create_dir_all(parent)?;
    let staging = parent.join(format!("{WEBSITE_DATA}.migrating"));
    match fs::remove_dir_all(&staging) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut files = 0;
    if let Err(error) = copy_tree(&from, &staging, 0, &mut files) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    // `to` holds at most WebKit's salt and empty directories: nothing to lose.
    match fs::remove_dir_all(&to) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    }
    fs::rename(&staging, &to)?;
    Ok(Outcome::Copied { files })
}

/// Migrates for the running bundle. Only the shipped identifier inherits: a
/// test or e2e bundle under another identifier, or an unbundled binary, has
/// nothing to inherit.
fn migrate_for_bundle(
    home: &Path,
    own_identifier: Option<&str>,
    shipped_identifier: &str,
) -> io::Result<Outcome> {
    if own_identifier != Some(shipped_identifier) {
        return Ok(Outcome::NothingToMigrate);
    }
    migrate_website_data(home, LEGACY_IDENTIFIER, shipped_identifier)
}

/// Runs the migration for the running bundle. A failure is logged and never
/// stops the app from starting.
pub fn run(own_identifier: Option<&str>, shipped_identifier: &str) {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };
    match migrate_for_bundle(&home, own_identifier, shipped_identifier) {
        Ok(Outcome::Copied { files }) => {
            log::info!(target: "setup", "carried {files} website-data files from {LEGACY_IDENTIFIER} to {shipped_identifier}")
        }
        Ok(_) => {}
        Err(error) => {
            log::warn!(target: "setup", "website data was not carried over from {LEGACY_IDENTIFIER}: {error}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "com.gitpulse.desktop";
    const NEW: &str = "dev.vbcr.gitpulse";

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn legacy_store(home: &Path) {
        let data = webkit_data(home, OLD);
        write(&data.join("Default/salt"), "salt-old");
        write(
            &data.join("Default/h1/h1/LocalStorage/localstorage.sqlite3"),
            "settings",
        );
        write(
            &data.join("Default/h1/h1/LocalStorage/localstorage.sqlite3-wal"),
            "wal",
        );
        write(
            &data.join("Default/h1/h1/IndexedDB/o/IndexedDB.sqlite3"),
            "idb",
        );
    }

    #[test]
    fn the_old_stores_pages_data_and_salt_reach_the_new_identifier() {
        let home = tempfile::tempdir().unwrap();
        legacy_store(home.path());
        let outcome = migrate_website_data(home.path(), OLD, NEW).unwrap();
        assert_eq!(outcome, Outcome::Copied { files: 4 });
        let new = webkit_data(home.path(), NEW);
        assert_eq!(
            fs::read_to_string(new.join("Default/h1/h1/LocalStorage/localstorage.sqlite3"))
                .unwrap(),
            "settings"
        );
        assert_eq!(
            fs::read_to_string(new.join("Default/salt")).unwrap(),
            "salt-old",
            "hashed directory names are only meaningful with their salt"
        );
        assert!(new
            .join("Default/h1/h1/LocalStorage/localstorage.sqlite3-wal")
            .exists());
        assert!(!new.parent().unwrap().join("WebsiteData.migrating").exists());
        // Copied, not moved: a downgrade still finds its data.
        assert!(webkit_data(home.path(), OLD)
            .join("Default/h1/h1/LocalStorage/localstorage.sqlite3")
            .exists());
    }

    #[test]
    fn a_second_run_changes_nothing() {
        let home = tempfile::tempdir().unwrap();
        legacy_store(home.path());
        migrate_website_data(home.path(), OLD, NEW).unwrap();
        // The page has since written to the new store.
        let live =
            webkit_data(home.path(), NEW).join("Default/h1/h1/LocalStorage/localstorage.sqlite3");
        fs::write(&live, "newer").unwrap();
        assert_eq!(
            migrate_website_data(home.path(), OLD, NEW).unwrap(),
            Outcome::NewStoreInUse
        );
        assert_eq!(fs::read_to_string(live).unwrap(), "newer");
    }

    #[test]
    fn a_store_the_new_app_already_filled_is_never_overwritten() {
        let home = tempfile::tempdir().unwrap();
        legacy_store(home.path());
        let own =
            webkit_data(home.path(), NEW).join("Default/x/x/LocalStorage/localstorage.sqlite3");
        write(&own, "mine");
        assert_eq!(
            migrate_website_data(home.path(), OLD, NEW).unwrap(),
            Outcome::NewStoreInUse
        );
        assert_eq!(fs::read_to_string(own).unwrap(), "mine");
    }

    #[test]
    fn a_store_holding_only_the_salt_is_replaced() {
        let home = tempfile::tempdir().unwrap();
        legacy_store(home.path());
        write(
            &webkit_data(home.path(), NEW).join("Default/salt"),
            "salt-new",
        );
        fs::create_dir_all(webkit_data(home.path(), NEW).join("Default/empty/dir")).unwrap();
        assert!(matches!(
            migrate_website_data(home.path(), OLD, NEW).unwrap(),
            Outcome::Copied { .. }
        ));
        assert_eq!(
            fs::read_to_string(webkit_data(home.path(), NEW).join("Default/salt")).unwrap(),
            "salt-old"
        );
    }

    #[test]
    fn nothing_is_created_when_the_old_identifier_left_no_data() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            migrate_website_data(home.path(), OLD, NEW).unwrap(),
            Outcome::NothingToMigrate
        );
        // A salt alone is not user data.
        write(&webkit_data(home.path(), OLD).join("Default/salt"), "s");
        assert_eq!(
            migrate_website_data(home.path(), OLD, NEW).unwrap(),
            Outcome::NothingToMigrate
        );
        assert!(!home.path().join("Library/WebKit").join(NEW).exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_in_the_old_store_is_not_followed() {
        let home = tempfile::tempdir().unwrap();
        legacy_store(home.path());
        let secret = home.path().join("secret");
        fs::write(&secret, "private").unwrap();
        std::os::unix::fs::symlink(&secret, webkit_data(home.path(), OLD).join("Default/link"))
            .unwrap();
        migrate_website_data(home.path(), OLD, NEW).unwrap();
        assert!(!webkit_data(home.path(), NEW).join("Default/link").exists());
    }

    #[test]
    fn a_leftover_staging_directory_from_an_interrupted_run_is_cleared() {
        let home = tempfile::tempdir().unwrap();
        legacy_store(home.path());
        let staging = home
            .path()
            .join("Library/WebKit")
            .join(NEW)
            .join("WebsiteData.migrating");
        write(&staging.join("Default/junk/file"), "half");
        assert!(matches!(
            migrate_website_data(home.path(), OLD, NEW).unwrap(),
            Outcome::Copied { .. }
        ));
        assert!(!staging.exists());
        assert!(!webkit_data(home.path(), NEW).join("Default/junk").exists());
    }

    #[test]
    fn only_the_shipped_identifier_migrates() {
        let home = tempfile::tempdir().unwrap();
        legacy_store(home.path());
        for other in [None, Some("dev.vbcr.gitpulse.e2e"), Some(OLD)] {
            assert_eq!(
                migrate_for_bundle(home.path(), other, NEW).unwrap(),
                Outcome::NothingToMigrate
            );
        }
        assert!(!webkit_data(home.path(), NEW).exists());
        assert!(matches!(
            migrate_for_bundle(home.path(), Some(NEW), NEW).unwrap(),
            Outcome::Copied { .. }
        ));
    }
}
