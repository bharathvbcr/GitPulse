//! One-time carry-over of WebView data keyed by the bundle identifier, from
//! legacy GitPulse identifiers (`dev.vbcr.gitpulse`, `com.gitpulse.desktop`) to
//! [`SHIPPED_IDENTIFIER`].
//!
//! Each platform stores WebView origin data (localStorage, IndexedDB: the
//! frontend's settings, layout, and drafts) under the bundle identifier:
//! - macOS: `~/Library/WebKit/<bundle identifier>/WebsiteData`
//! - Windows: `%LOCALAPPDATA%\<bundle identifier>\EBWebView`
//! - Linux: `$XDG_DATA_HOME/<bundle identifier>` and `$XDG_CACHE_HOME/<bundle identifier>`
//!
//! Sources are checked in strict precedence order (`dev.vbcr.gitpulse` before
//! `com.gitpulse.desktop`) so a user who already migrated to `dev.vbcr.gitpulse`
//! never inherits stale `com.gitpulse.desktop` state.
//!
//! The copy runs before Tauri builds the `"main"` webview, after the
//! single-instance guard has made this process the owner (so no older instance
//! is writing the store), and never moves or deletes the old data: a downgrade,
//! or a copy that went wrong, still has the original.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Current Waylode bundle identifier.
pub const SHIPPED_IDENTIFIER: &str = "dev.vbcr.waylode";

/// Legacy GitPulse bundle identifiers, ordered newest-first so `dev.vbcr.gitpulse`
/// always beats stale `com.gitpulse.desktop` data.
pub const LEGACY_IDENTIFIERS: [&str; 2] = ["dev.vbcr.gitpulse", "com.gitpulse.desktop"];

/// Most recent legacy identifier (kept for callers inspecting the primary predecessor).
pub const LEGACY_IDENTIFIER: &str = LEGACY_IDENTIFIERS[0];

const WEBSITE_DATA: &str = "WebsiteData";
const EB_WEBVIEW: &str = "EBWebView";
const ORIGIN_DATA: &str = "Default";
const SALT: &str = "salt";
const WINDOWS_LOCKFILE: &str = "Lockfile";

/// A WebView data store is a few levels deep; refuse anything deeper rather
/// than follow a loop.
const MAX_DEPTH: usize = 16;
const MAX_ENTRIES: usize = 200_000;

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The winning legacy store was copied to the target identifier.
    Copied { files: usize, source: String },
    /// No legacy identifier held origin data.
    NothingToMigrate,
    /// The target identifier already holds website data; it is left alone.
    NewStoreInUse,
}

#[derive(Clone)]
struct StoreTarget<'a> {
    path: PathBuf,
    origin_subpath: Option<&'a str>,
    ignore_root_files: &'a [&'a str],
}

pub fn webkit_data(home: &Path, identifier: &str) -> PathBuf {
    home.join("Library")
        .join("WebKit")
        .join(identifier)
        .join(WEBSITE_DATA)
}

pub fn windows_webview_data(local_app_data: &Path, identifier: &str) -> PathBuf {
    local_app_data.join(identifier).join(EB_WEBVIEW)
}

pub fn linux_data_dir(xdg_data_home: &Path, identifier: &str) -> PathBuf {
    xdg_data_home.join(identifier)
}

pub fn linux_cache_dir(xdg_cache_home: &Path, identifier: &str) -> PathBuf {
    xdg_cache_home.join(identifier)
}

fn should_skip_copy(name: &std::ffi::OsStr) -> bool {
    let text = name.to_string_lossy();
    text.ends_with(".lock") || text.ends_with(".sock") || text == WINDOWS_LOCKFILE
}

/// Whether `dir` (or `dir/origin_subpath`) holds any regular file beyond
/// depth-0 placeholder files like WebKit's `Default/salt` or WebView2's `Lockfile`.
fn holds_origin_data(
    dir: &Path,
    origin_subpath: Option<&str>,
    ignore_root_files: &[&str],
) -> io::Result<bool> {
    let root = match origin_subpath {
        Some(sub) => dir.join(sub),
        None => dir.to_path_buf(),
    };
    let mut stack = vec![(root, 0usize)];
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
                return Ok(true);
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                stack.push((entry.path(), depth + 1));
            } else if kind.is_file() {
                let name = entry.file_name();
                let ignored =
                    depth == 0 && ignore_root_files.iter().any(|candidate| name == *candidate);
                if !ignored && !should_skip_copy(&name) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

/// Copies `from` to `to`, regular files and directories only: symlinks, sockets,
/// and lockfiles are never copied or followed.
fn copy_tree(from: &Path, to: &Path, depth: usize, files: &mut usize) -> io::Result<()> {
    if depth > MAX_DEPTH {
        return Err(io::Error::other("website data is nested too deeply"));
    }
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let name = entry.file_name();
        if should_skip_copy(&name) {
            continue;
        }
        let target = to.join(name);
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

fn copy_store_atomically(from: &Path, to: &Path) -> io::Result<usize> {
    let parent = to
        .parent()
        .ok_or_else(|| io::Error::other("no WebView storage parent directory"))?;
    let name = to
        .file_name()
        .ok_or_else(|| io::Error::other("no WebView storage directory name"))?;
    fs::create_dir_all(parent)?;
    let staging = parent.join(format!("{}.migrating", name.to_string_lossy()));
    match fs::remove_dir_all(&staging) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut files = 0;
    if let Err(error) = copy_tree(from, &staging, 0, &mut files) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    match fs::remove_dir_all(to) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    }
    fs::rename(&staging, to)?;
    Ok(files)
}

fn migrate_ordered_stores<'a>(
    sources: &[&str],
    target: &str,
    build_stores: impl Fn(&str) -> Vec<StoreTarget<'a>>,
) -> io::Result<Outcome> {
    let target_stores = build_stores(target);
    for store in &target_stores {
        if holds_origin_data(&store.path, store.origin_subpath, store.ignore_root_files)? {
            return Ok(Outcome::NewStoreInUse);
        }
    }

    for &source in sources {
        if source == target {
            continue;
        }
        let source_stores = build_stores(source);
        let mut active = Vec::with_capacity(source_stores.len());
        let mut any_origin = false;
        for src in &source_stores {
            let has_data = holds_origin_data(&src.path, src.origin_subpath, src.ignore_root_files)?;
            if has_data {
                any_origin = true;
            }
            active.push(has_data);
        }
        if !any_origin {
            continue;
        }
        let mut total_files = 0;
        for ((src, dst), has_data) in source_stores
            .iter()
            .zip(target_stores.iter())
            .zip(active)
        {
            if has_data {
                total_files += copy_store_atomically(&src.path, &dst.path)?;
            }
        }
        return Ok(Outcome::Copied {
            files: total_files,
            source: source.to_string(),
        });
    }

    Ok(Outcome::NothingToMigrate)
}

pub fn migrate_macos_stores(home: &Path, sources: &[&str], target: &str) -> io::Result<Outcome> {
    migrate_ordered_stores(sources, target, |id| {
        vec![StoreTarget {
            path: webkit_data(home, id),
            origin_subpath: Some(ORIGIN_DATA),
            ignore_root_files: &[SALT],
        }]
    })
}

pub fn migrate_windows_stores(
    local_app_data: &Path,
    sources: &[&str],
    target: &str,
) -> io::Result<Outcome> {
    migrate_ordered_stores(sources, target, |id| {
        vec![StoreTarget {
            path: windows_webview_data(local_app_data, id),
            origin_subpath: None,
            ignore_root_files: &[WINDOWS_LOCKFILE],
        }]
    })
}

pub fn migrate_linux_stores(
    xdg_data_home: &Path,
    xdg_cache_home: &Path,
    sources: &[&str],
    target: &str,
) -> io::Result<Outcome> {
    migrate_ordered_stores(sources, target, |id| {
        vec![
            StoreTarget {
                path: linux_data_dir(xdg_data_home, id),
                origin_subpath: None,
                ignore_root_files: &[],
            },
            StoreTarget {
                path: linux_cache_dir(xdg_cache_home, id),
                origin_subpath: None,
                ignore_root_files: &[],
            },
        ]
    })
}

/// Carries `old` identifier's macOS WebKit website data to `new` when `new` has none.
pub fn migrate_website_data(home: &Path, old: &str, new: &str) -> io::Result<Outcome> {
    migrate_macos_stores(home, &[old], new)
}

/// Migrates for the running bundle on macOS. Only the shipped identifier inherits.
#[cfg(test)]
fn migrate_for_bundle(
    home: &Path,
    own_identifier: Option<&str>,
    shipped_identifier: &str,
) -> io::Result<Outcome> {
    if own_identifier != Some(shipped_identifier) {
        return Ok(Outcome::NothingToMigrate);
    }
    migrate_macos_stores(home, &LEGACY_IDENTIFIERS, shipped_identifier)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn resolve_xdg_dir(env_name: &str, fallback_suffix: &str) -> Option<PathBuf> {
    std::env::var_os(env_name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|home| PathBuf::from(home).join(fallback_suffix))
        })
}

fn log_outcome(outcome: io::Result<Outcome>, target: &str) {
    match outcome {
        Ok(Outcome::Copied { files, source }) => {
            log::info!(
                target: "setup",
                "carried {files} WebView files from {source} to {target}"
            );
        }
        Ok(_) => {}
        Err(error) => {
            log::warn!(
                target: "setup",
                "WebView data was not carried over to {target}: {error}"
            );
        }
    }
}

/// Runs the migration for the running bundle. A failure is logged and never
/// stops the app from starting.
pub fn run(own_identifier: Option<&str>, shipped_identifier: &str) {
    if own_identifier != Some(shipped_identifier) {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return;
        };
        log_outcome(
            migrate_macos_stores(&home, &LEGACY_IDENTIFIERS, shipped_identifier),
            shipped_identifier,
        );
    }
    #[cfg(target_os = "windows")]
    {
        let Some(local_app_data) = std::env::var_os("LOCALAPPDATA")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
        else {
            return;
        };
        log_outcome(
            migrate_windows_stores(&local_app_data, &LEGACY_IDENTIFIERS, shipped_identifier),
            shipped_identifier,
        );
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let (Some(data_home), Some(cache_home)) = (
            resolve_xdg_dir("XDG_DATA_HOME", ".local/share"),
            resolve_xdg_dir("XDG_CACHE_HOME", ".cache"),
        ) else {
            return;
        };
        log_outcome(
            migrate_linux_stores(
                &data_home,
                &cache_home,
                &LEGACY_IDENTIFIERS,
                shipped_identifier,
            ),
            shipped_identifier,
        );
    }
}

/// Invoked in `lib.rs` on all 3 OSes before `Builder::run` creates `"main"`.
///
/// On macOS, unbundled binaries (`cargo run`, tests) report `None` from
/// `NSBundle::mainBundle().bundleIdentifier()` and do not inherit. On Windows
/// and Linux, Tauri resolves WebView storage directly from `config_identifier`.
pub fn run_pre_webview(config_identifier: &str) {
    #[cfg(target_os = "macos")]
    let own = {
        let _ = config_identifier;
        objc2_foundation::NSBundle::mainBundle()
            .bundleIdentifier()
            .map(|identifier| identifier.to_string())
    };
    #[cfg(not(target_os = "macos"))]
    let own = Some(config_identifier.to_string());

    run(own.as_deref(), SHIPPED_IDENTIFIER);
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD_V2: &str = "dev.vbcr.gitpulse";
    const OLD_V1: &str = "com.gitpulse.desktop";
    const NEW: &str = "dev.vbcr.waylode";

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn seed_macos_store(home: &Path, id: &str, marker: &str) {
        let data = webkit_data(home, id);
        write(&data.join("Default/salt"), &format!("salt-{marker}"));
        write(
            &data.join("Default/h1/h1/LocalStorage/localstorage.sqlite3"),
            &format!("settings-{marker}"),
        );
        write(
            &data.join("Default/h1/h1/LocalStorage/localstorage.sqlite3-wal"),
            "wal",
        );
        write(
            &data.join("Default/h1/h1/IndexedDB/o/IndexedDB.sqlite3"),
            &format!("idb-{marker}"),
        );
    }

    #[test]
    fn ordered_precedence_picks_dev_vbcr_gitpulse_over_stale_com_gitpulse_desktop() {
        let home = tempfile::tempdir().unwrap();
        seed_macos_store(home.path(), OLD_V1, "v1-stale");
        seed_macos_store(home.path(), OLD_V2, "v2-fresh");

        let outcome = migrate_macos_stores(home.path(), &LEGACY_IDENTIFIERS, NEW).unwrap();
        assert_eq!(
            outcome,
            Outcome::Copied {
                files: 4,
                source: OLD_V2.to_string(),
            }
        );

        let new = webkit_data(home.path(), NEW);
        assert_eq!(
            fs::read_to_string(new.join("Default/h1/h1/LocalStorage/localstorage.sqlite3"))
                .unwrap(),
            "settings-v2-fresh"
        );
        assert_eq!(
            fs::read_to_string(new.join("Default/salt")).unwrap(),
            "salt-v2-fresh"
        );
        // Legacy stores remain intact for rollback.
        assert!(webkit_data(home.path(), OLD_V2)
            .join("Default/h1/h1/LocalStorage/localstorage.sqlite3")
            .exists());
        assert!(webkit_data(home.path(), OLD_V1)
            .join("Default/h1/h1/LocalStorage/localstorage.sqlite3")
            .exists());
    }

    #[test]
    fn falls_back_to_com_gitpulse_desktop_when_dev_vbcr_gitpulse_has_no_origin_data() {
        let home = tempfile::tempdir().unwrap();
        seed_macos_store(home.path(), OLD_V1, "v1-only");
        // An empty or salt-only dev.vbcr.gitpulse store must not block fallback.
        write(&webkit_data(home.path(), OLD_V2).join("Default/salt"), "s");

        let outcome = migrate_macos_stores(home.path(), &LEGACY_IDENTIFIERS, NEW).unwrap();
        assert_eq!(
            outcome,
            Outcome::Copied {
                files: 4,
                source: OLD_V1.to_string(),
            }
        );
        assert_eq!(
            fs::read_to_string(
                webkit_data(home.path(), NEW)
                    .join("Default/h1/h1/LocalStorage/localstorage.sqlite3")
            )
            .unwrap(),
            "settings-v1-only"
        );
    }

    #[test]
    fn windows_ebwebview_migrates_with_ordered_precedence_and_skips_lockfiles() {
        let local_app_data = tempfile::tempdir().unwrap();
        let v1 = windows_webview_data(local_app_data.path(), OLD_V1);
        write(&v1.join("Default/Local Storage/leveldb/000003.log"), "v1");
        let v2 = windows_webview_data(local_app_data.path(), OLD_V2);
        write(&v2.join("Default/Local Storage/leveldb/000003.log"), "v2");
        write(&v2.join("Default/Preferences"), "{\"k\":1}");
        write(&v2.join("Lockfile"), "pid");

        let outcome =
            migrate_windows_stores(local_app_data.path(), &LEGACY_IDENTIFIERS, NEW).unwrap();
        assert_eq!(
            outcome,
            Outcome::Copied {
                files: 2,
                source: OLD_V2.to_string(),
            }
        );

        let dst = windows_webview_data(local_app_data.path(), NEW);
        assert_eq!(
            fs::read_to_string(dst.join("Default/Local Storage/leveldb/000003.log")).unwrap(),
            "v2"
        );
        assert_eq!(
            fs::read_to_string(dst.join("Default/Preferences")).unwrap(),
            "{\"k\":1}"
        );
        assert!(!dst.join("Lockfile").exists());
        assert!(!dst.parent().unwrap().join("EBWebView.migrating").exists());

        // Idempotent second run.
        assert_eq!(
            migrate_windows_stores(local_app_data.path(), &LEGACY_IDENTIFIERS, NEW).unwrap(),
            Outcome::NewStoreInUse
        );
    }

    #[test]
    fn linux_xdg_data_and_cache_migrate_together_from_winning_source() {
        let root = tempfile::tempdir().unwrap();
        let data_home = root.path().join("data");
        let cache_home = root.path().join("cache");

        // Stale v1 has both data and cache.
        write(
            &linux_data_dir(&data_home, OLD_V1).join("localstorage/db.sqlite"),
            "v1-data",
        );
        write(
            &linux_cache_dir(&cache_home, OLD_V1).join("WebKitCache/entry"),
            "v1-cache",
        );
        // Winning v2 has data (and no cache); v1's stale cache must NOT mix in.
        write(
            &linux_data_dir(&data_home, OLD_V2).join("localstorage/db.sqlite"),
            "v2-data",
        );

        let outcome =
            migrate_linux_stores(&data_home, &cache_home, &LEGACY_IDENTIFIERS, NEW).unwrap();
        assert_eq!(
            outcome,
            Outcome::Copied {
                files: 1,
                source: OLD_V2.to_string(),
            }
        );
        assert_eq!(
            fs::read_to_string(linux_data_dir(&data_home, NEW).join("localstorage/db.sqlite"))
                .unwrap(),
            "v2-data"
        );
        assert!(!linux_cache_dir(&cache_home, NEW).exists());
        assert!(!data_home.join(format!("{NEW}.migrating")).exists());

        // Idempotent second run.
        assert_eq!(
            migrate_linux_stores(&data_home, &cache_home, &LEGACY_IDENTIFIERS, NEW).unwrap(),
            Outcome::NewStoreInUse
        );
    }

    #[test]
    fn linux_migrates_both_data_and_cache_when_winning_source_has_both() {
        let root = tempfile::tempdir().unwrap();
        let data_home = root.path().join("data");
        let cache_home = root.path().join("cache");

        write(
            &linux_data_dir(&data_home, OLD_V2).join("cookies"),
            "cookie-data",
        );
        write(
            &linux_cache_dir(&cache_home, OLD_V2).join("cache.bin"),
            "cache-data",
        );

        let outcome =
            migrate_linux_stores(&data_home, &cache_home, &LEGACY_IDENTIFIERS, NEW).unwrap();
        assert_eq!(
            outcome,
            Outcome::Copied {
                files: 2,
                source: OLD_V2.to_string(),
            }
        );
        assert_eq!(
            fs::read_to_string(linux_data_dir(&data_home, NEW).join("cookies")).unwrap(),
            "cookie-data"
        );
        assert_eq!(
            fs::read_to_string(linux_cache_dir(&cache_home, NEW).join("cache.bin")).unwrap(),
            "cache-data"
        );
    }

    #[test]
    fn a_second_run_changes_nothing() {
        let home = tempfile::tempdir().unwrap();
        seed_macos_store(home.path(), OLD_V2, "old");
        migrate_website_data(home.path(), OLD_V2, NEW).unwrap();
        let live =
            webkit_data(home.path(), NEW).join("Default/h1/h1/LocalStorage/localstorage.sqlite3");
        fs::write(&live, "newer").unwrap();
        assert_eq!(
            migrate_macos_stores(home.path(), &LEGACY_IDENTIFIERS, NEW).unwrap(),
            Outcome::NewStoreInUse
        );
        assert_eq!(fs::read_to_string(live).unwrap(), "newer");
    }

    #[test]
    fn a_store_the_new_app_already_filled_is_never_overwritten() {
        let home = tempfile::tempdir().unwrap();
        seed_macos_store(home.path(), OLD_V2, "old");
        let own =
            webkit_data(home.path(), NEW).join("Default/x/x/LocalStorage/localstorage.sqlite3");
        write(&own, "mine");
        assert_eq!(
            migrate_macos_stores(home.path(), &LEGACY_IDENTIFIERS, NEW).unwrap(),
            Outcome::NewStoreInUse
        );
        assert_eq!(fs::read_to_string(own).unwrap(), "mine");
    }

    #[test]
    fn a_store_holding_only_the_salt_is_replaced() {
        let home = tempfile::tempdir().unwrap();
        seed_macos_store(home.path(), OLD_V2, "old");
        write(
            &webkit_data(home.path(), NEW).join("Default/salt"),
            "salt-new",
        );
        fs::create_dir_all(webkit_data(home.path(), NEW).join("Default/empty/dir")).unwrap();
        assert!(matches!(
            migrate_macos_stores(home.path(), &LEGACY_IDENTIFIERS, NEW).unwrap(),
            Outcome::Copied { .. }
        ));
        assert_eq!(
            fs::read_to_string(webkit_data(home.path(), NEW).join("Default/salt")).unwrap(),
            "salt-old"
        );
    }

    #[test]
    fn nothing_is_created_when_the_old_identifiers_left_no_data() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            migrate_macos_stores(home.path(), &LEGACY_IDENTIFIERS, NEW).unwrap(),
            Outcome::NothingToMigrate
        );
        write(&webkit_data(home.path(), OLD_V2).join("Default/salt"), "s");
        assert_eq!(
            migrate_macos_stores(home.path(), &LEGACY_IDENTIFIERS, NEW).unwrap(),
            Outcome::NothingToMigrate
        );
        assert!(!home.path().join("Library/WebKit").join(NEW).exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_in_the_old_store_is_not_followed() {
        let home = tempfile::tempdir().unwrap();
        seed_macos_store(home.path(), OLD_V2, "old");
        let secret = home.path().join("secret");
        fs::write(&secret, "private").unwrap();
        std::os::unix::fs::symlink(
            &secret,
            webkit_data(home.path(), OLD_V2).join("Default/link"),
        )
        .unwrap();
        migrate_macos_stores(home.path(), &LEGACY_IDENTIFIERS, NEW).unwrap();
        assert!(!webkit_data(home.path(), NEW).join("Default/link").exists());
    }

    #[test]
    fn a_leftover_staging_directory_from_an_interrupted_run_is_cleared() {
        let home = tempfile::tempdir().unwrap();
        seed_macos_store(home.path(), OLD_V2, "old");
        let staging = home
            .path()
            .join("Library/WebKit")
            .join(NEW)
            .join("WebsiteData.migrating");
        write(&staging.join("Default/junk/file"), "half");
        assert!(matches!(
            migrate_macos_stores(home.path(), &LEGACY_IDENTIFIERS, NEW).unwrap(),
            Outcome::Copied { .. }
        ));
        assert!(!staging.exists());
        assert!(!webkit_data(home.path(), NEW).join("Default/junk").exists());
    }

    #[test]
    fn only_the_shipped_identifier_migrates() {
        let home = tempfile::tempdir().unwrap();
        seed_macos_store(home.path(), OLD_V2, "old");
        for other in [
            None,
            Some("dev.vbcr.waylode.e2e"),
            Some(OLD_V2),
            Some(OLD_V1),
        ] {
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
