//! Pre-upgrade copies of the workbench profile: `<profile>.pre-schema-<N>.bak`
//! and the other `<profile>.<stamp>.bak` copies the profile store writes beside
//! the profile with `VACUUM INTO` before it migrates.
//!
//! They are the only way back from a schema upgrade, so they are listed here
//! with size and age and removed only through the user-confirmed hygiene path
//! (`prepare` then `execute` on a `backup:<file name>` target). Every
//! automatic path skips them: [`super::requires_user_confirmation`] names the
//! target kind, and the steward and the cache inventory consumers honour it.
use super::{tree, CacheEntry};
use crate::ipc_error::CommandError;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::UNIX_EPOCH;

/// Target prefix of a backup removal; the rest is the bare file name.
pub(super) const TARGET_PREFIX: &str = "backup:";
/// `HygienePlan.scope` of a backup removal: no retention age applies, because
/// the user chose this exact file.
pub(super) const SCOPE: &str = "profile-backup";
/// Listing bound. Past it the listing says so instead of looking complete.
const MAX_LISTED: usize = 100;
const NOTE: &str = "Written before a schema upgrade so the upgrade can be rolled back by restoring this copy. It is never removed automatically; delete it only once the upgraded profile is known to be good.";

struct Backup {
    name: String,
    path: PathBuf,
    bytes: u64,
    modified: u64,
}

/// Whether `name` is a copy the profile store writes for the profile file
/// called `profile_name`. An in-flight `.bak.partial` is not one.
fn is_backup_name(profile_name: &str, name: &str) -> bool {
    name.strip_prefix(profile_name)
        .and_then(|rest| rest.strip_prefix('.'))
        .and_then(|rest| rest.strip_suffix(".bak"))
        .is_some_and(|stamp| {
            !stamp.is_empty()
                && stamp
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
}

fn profile_parts(profile: &Path) -> Result<(&Path, &str), CommandError> {
    let dir = profile
        .parent()
        .ok_or("The profile path has no directory")?;
    let name = profile
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("The profile file name is not UTF-8")?;
    Ok((dir, name))
}

fn list(profile: &Path) -> Result<(Vec<Backup>, usize), CommandError> {
    let (dir, profile_name) = profile_parts(profile)?;
    let reader = match fs::read_dir(dir) {
        Ok(reader) => reader,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
        Err(error) => return Err(format!("Cannot read {}: {error}", dir.display()).into()),
    };
    let mut found = Vec::new();
    for entry in reader {
        let entry = entry.map_err(|e| format!("Cannot read {}: {e}", dir.display()))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !is_backup_name(profile_name, &name) {
            continue;
        }
        let meta = fs::symlink_metadata(entry.path())
            .map_err(|e| format!("Cannot inspect {name}: {e}"))?;
        if !meta.is_file() {
            continue;
        }
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        found.push(Backup {
            path: entry.path(),
            name,
            bytes: meta.len(),
            modified,
        });
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    let omitted = found.len().saturating_sub(MAX_LISTED);
    found.truncate(MAX_LISTED);
    Ok((found, omitted))
}

/// The inventory rows for the backups beside `profile`. A listing that could
/// not run is one row carrying the error, never an empty list.
pub(super) fn entries(profile: Result<&Path, &CommandError>) -> Vec<CacheEntry> {
    let unavailable = |error: String| CacheEntry {
        id: "profile-backups".into(),
        label: "Workbench profile backups".into(),
        path: None,
        bytes: None,
        action: None,
        note: NOTE.into(),
        error: Some(error),
        modified_at: None,
        confirm_only: true,
    };
    let profile = match profile {
        Ok(profile) => profile,
        Err(error) => return vec![unavailable(error.message().to_owned())],
    };
    let (found, omitted) = match list(profile) {
        Ok(listing) => listing,
        Err(error) => return vec![unavailable(error.into_message())],
    };
    let mut rows: Vec<CacheEntry> = found
        .into_iter()
        .map(|backup| CacheEntry {
            label: format!("Workbench profile backup {}", backup.name),
            id: backup.name,
            path: Some(backup.path.to_string_lossy().into_owned()),
            bytes: Some(backup.bytes),
            action: Some("Delete this backup".into()),
            note: NOTE.into(),
            error: None,
            modified_at: Some(backup.modified),
            confirm_only: true,
        })
        .collect();
    if omitted > 0 {
        rows.push(unavailable(format!(
            "{omitted} more backups were found and are not listed"
        )));
    }
    rows
}

/// Unlinks the reviewed backup. The tree remover works on directories, so a
/// single file is checked against its reviewed stamp here and unlinked by name;
/// a symlink put in its place is unlinked itself, never followed.
pub(super) fn remove(
    path: &Path,
    reviewed: &tree::Snapshot,
    cancel: &AtomicBool,
) -> Result<(), CommandError> {
    if tree::snapshot(path, false, cancel)? != *reviewed {
        return Err("Backup changed after preview".into());
    }
    fs::remove_file(path).map_err(|e| CommandError::from(e.to_string()))
}

/// The one file a `backup:<name>` target may remove: a regular file beside the
/// profile that [`is_backup_name`] accepts. Anything else is refused.
pub(super) fn resolve(profile: &Path, name: &str) -> Result<PathBuf, CommandError> {
    let (dir, profile_name) = profile_parts(profile)?;
    if name.contains(['/', '\\']) || !is_backup_name(profile_name, name) {
        return Err("Not a workbench profile backup".into());
    }
    let path = dir.join(name);
    let meta = fs::symlink_metadata(&path).map_err(|e| format!("Backup unavailable: {e}"))?;
    if !meta.is_file() {
        return Err("Backup is not a regular file".into());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::hygiene::{
        execute_with_profile, prepare_with_profile, requires_user_confirmation,
    };
    use std::time::{Duration, SystemTime};

    const BACKUP: &str = "workbench.sqlite.pre-schema-11.bak";

    fn profile_dir() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let dir = crate::engine::process::canonicalize_plain(temp.path()).unwrap();
        fs::write(dir.join("workbench.sqlite"), "live profile").unwrap();
        fs::write(dir.join(BACKUP), vec![7u8; 4096]).unwrap();
        fs::write(dir.join("workbench.sqlite.20261121T010203Z.bak"), "stamped").unwrap();
        fs::write(
            dir.join("workbench.sqlite.pre-schema-12.bak.partial"),
            "in flight",
        )
        .unwrap();
        fs::write(dir.join("other.sqlite.pre-schema-11.bak"), "someone else's").unwrap();
        fs::write(dir.join("workbench.sqlite..bak"), "no stamp").unwrap();
        (temp, dir)
    }

    fn repo() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let repo = crate::engine::process::canonicalize_plain(temp.path()).unwrap();
        crate::engine::git_cli::git_global(&["init", "-q", repo.to_str().unwrap()]).unwrap();
        crate::test_support::trust_repo(&repo);
        (temp, repo)
    }

    fn names(rows: &[CacheEntry]) -> Vec<&str> {
        rows.iter().map(|r| r.id.as_str()).collect()
    }

    #[test]
    fn the_listing_reports_size_and_age_of_pre_schema_and_stamped_backups_only() {
        let _serial = crate::storage::hygiene::tests::serial();
        let (_keep, dir) = profile_dir();
        let old = SystemTime::now() - Duration::from_secs(40 * 86_400);
        crate::test_support::set_modified(&dir.join(BACKUP), old).unwrap();
        let profile = dir.join("workbench.sqlite");

        let rows = entries(Ok(&profile));

        assert_eq!(
            names(&rows),
            ["workbench.sqlite.20261121T010203Z.bak", BACKUP]
        );
        let backup = rows.iter().find(|r| r.id == BACKUP).unwrap();
        assert_eq!(backup.bytes, Some(4096));
        let age = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - backup.modified_at.expect("age is reported");
        assert!((40 * 86_400..41 * 86_400).contains(&age), "age {age}s");
        assert_eq!(backup.path.as_deref(), dir.join(BACKUP).to_str());
        assert!(rows.iter().all(|r| r.confirm_only && r.action.is_some()));
    }

    #[test]
    fn a_listing_that_cannot_run_is_an_error_row_not_an_empty_list() {
        let blocked = CommandError::from("profile directory unavailable");
        let rows = entries(Err(&blocked));
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].error.as_deref(),
            Some("profile directory unavailable")
        );
        assert!(rows[0].action.is_none() && rows[0].confirm_only);
    }

    #[test]
    fn no_scheduled_or_agent_path_can_select_a_backup_and_a_hygiene_run_leaves_it() {
        let _serial = crate::storage::hygiene::tests::serial();
        let (_keep, dir) = profile_dir();
        let profile = dir.join("workbench.sqlite");
        let (_repo_keep, repo) = repo();
        assert!(requires_user_confirmation(&format!(
            "{TARGET_PREFIX}{BACKUP}"
        )));
        assert!(!requires_user_confirmation("cache:go"));
        assert!(!requires_user_confirmation("local:target"));

        // The headless hygiene entry points are repository and cache targets;
        // running them with the backups beside the profile removes nothing.
        let _ = prepare_with_profile(
            repo.to_str().unwrap(),
            "local:target",
            30,
            |_| Ok(()),
            || Ok(profile.clone()),
        );
        assert!(dir.join(BACKUP).exists());
        assert!(entries(Ok(&profile)).iter().all(|r| r.confirm_only));
    }

    #[test]
    fn a_confirmed_preview_removes_exactly_that_backup_and_nothing_else() {
        let _serial = crate::storage::hygiene::tests::serial();
        let (_keep, dir) = profile_dir();
        let profile = dir.join("workbench.sqlite");
        let (_repo_keep, repo) = repo();
        let repo = repo.to_str().unwrap();
        let resolver = || Ok(profile.clone());
        // A just-written backup and an activity check that would refuse: neither
        // retention age nor the directory scan applies to a chosen file.
        let busy = |_: &Path| Err(CommandError::from("the activity scan must not run"));

        let plan = prepare_with_profile(
            repo,
            &format!("{TARGET_PREFIX}{BACKUP}"),
            30,
            busy,
            resolver,
        )
        .unwrap();
        assert_eq!(
            (plan.scope.as_str(), plan.bytes, plan.files),
            (SCOPE, 4096, 1)
        );
        assert!(dir.join(BACKUP).exists(), "a preview removes nothing");

        let mut gated = None;
        let (_, outcome) = execute_with_profile(
            repo,
            &plan.id,
            |argv, path| {
                gated = Some((argv.is_none(), path.to_owned()));
                Ok(())
            },
            busy,
            resolver,
        )
        .unwrap();

        assert!(outcome.success, "{}", outcome.message);
        assert_eq!((outcome.bytes_before, outcome.bytes_after), (4096, Some(0)));
        assert_eq!(
            gated,
            Some((true, dir.join(BACKUP).to_string_lossy().into_owned()))
        );
        assert!(!dir.join(BACKUP).exists());
        for kept in [
            "workbench.sqlite",
            "workbench.sqlite.20261121T010203Z.bak",
            "workbench.sqlite.pre-schema-12.bak.partial",
            "other.sqlite.pre-schema-11.bak",
        ] {
            assert!(dir.join(kept).exists(), "{kept} must survive");
        }
    }

    #[test]
    fn targets_that_are_not_a_plain_backup_file_are_refused() {
        let _serial = crate::storage::hygiene::tests::serial();
        let (_keep, dir) = profile_dir();
        let profile = dir.join("workbench.sqlite");
        let (_repo_keep, repo) = repo();
        std::os::unix::fs::symlink(
            dir.join("workbench.sqlite"),
            dir.join("workbench.sqlite.link.bak"),
        )
        .unwrap();
        fs::create_dir(dir.join("workbench.sqlite.dir.bak")).unwrap();
        for target in [
            "backup:workbench.sqlite",
            "backup:other.sqlite.pre-schema-11.bak",
            "backup:workbench.sqlite.pre-schema-12.bak.partial",
            "backup:../workbench.sqlite.pre-schema-11.bak",
            "backup:workbench.sqlite.link.bak",
            "backup:workbench.sqlite.dir.bak",
            "backup:workbench.sqlite.gone.bak",
            "backup:",
        ] {
            assert!(
                prepare_with_profile(
                    repo.to_str().unwrap(),
                    target,
                    30,
                    |_| Ok(()),
                    || Ok(profile.clone())
                )
                .is_err(),
                "{target} must be refused"
            );
        }
        assert!(dir.join("workbench.sqlite").exists() && dir.join(BACKUP).exists());
    }
}
