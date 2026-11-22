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

/// Shown on the preview and in the native confirmation: one copy of the text.
pub(super) const WARNING: &str = "Removes this pre-upgrade copy of the workbench profile permanently. Without it that schema upgrade cannot be rolled back. Only this confirmed action removes it; no scheduled run does.";

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
            |_, _| true,
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

    fn backup_plan(repo: &str, resolver: impl Fn() -> Result<PathBuf, CommandError>) -> String {
        prepare_with_profile(
            repo,
            &format!("{TARGET_PREFIX}{BACKUP}"),
            30,
            |_| Ok(()),
            resolver,
        )
        .unwrap()
        .id
    }

    #[test]
    fn a_declined_confirmation_leaves_the_backup_and_returns_a_refusal() {
        let _serial = crate::storage::hygiene::tests::serial();
        let (_keep, dir) = profile_dir();
        let profile = dir.join("workbench.sqlite");
        let (_repo_keep, repo) = repo();
        let repo = repo.to_str().unwrap();
        let resolver = || Ok(profile.clone());
        let id = backup_plan(repo, resolver);
        let asked = std::cell::RefCell::new(Vec::new());

        let result = execute_with_profile(
            repo,
            &id,
            |_, _| Ok(()),
            |_| Ok(()),
            resolver,
            |title, message| {
                asked
                    .borrow_mut()
                    .push((title.to_owned(), message.to_owned()));
                false
            },
        );

        let Err(error) = result else {
            panic!("a declined removal is a refusal");
        };
        assert!(error.message().contains("declined"), "{}", error.message());
        assert!(dir.join(BACKUP).exists(), "nothing is removed on a decline");
        let asked = asked.borrow();
        assert_eq!(asked.len(), 1);
        assert!(asked[0]
            .1
            .contains(&dir.join(BACKUP).to_string_lossy().into_owned()));
        assert!(asked[0].1.contains("cannot be rolled back"));
    }

    #[test]
    fn an_accepted_confirmation_removes_the_backup() {
        let _serial = crate::storage::hygiene::tests::serial();
        let (_keep, dir) = profile_dir();
        let profile = dir.join("workbench.sqlite");
        let (_repo_keep, repo) = repo();
        let repo = repo.to_str().unwrap();
        let resolver = || Ok(profile.clone());
        let id = backup_plan(repo, resolver);

        let (_, outcome) =
            execute_with_profile(repo, &id, |_, _| Ok(()), |_| Ok(()), resolver, |_, _| true)
                .unwrap();

        assert!(outcome.success, "{}", outcome.message);
        assert!(!dir.join(BACKUP).exists());
    }

    #[test]
    fn the_confirmation_is_asked_before_the_policy_gate() {
        let _serial = crate::storage::hygiene::tests::serial();
        let (_keep, dir) = profile_dir();
        let profile = dir.join("workbench.sqlite");
        let (_repo_keep, repo) = repo();
        let repo = repo.to_str().unwrap();
        let resolver = || Ok(profile.clone());
        let id = backup_plan(repo, resolver);
        let order = std::cell::RefCell::new(Vec::new());

        execute_with_profile(
            repo,
            &id,
            |_, _| {
                order.borrow_mut().push("gate");
                Ok(())
            },
            |_| Ok(()),
            resolver,
            |_, _| {
                order.borrow_mut().push("confirm");
                true
            },
        )
        .unwrap();

        assert_eq!(*order.borrow(), ["confirm", "gate"]);
    }

    #[test]
    fn a_declined_confirmation_never_reaches_the_gate() {
        let _serial = crate::storage::hygiene::tests::serial();
        let (_keep, dir) = profile_dir();
        let profile = dir.join("workbench.sqlite");
        let (_repo_keep, repo) = repo();
        let repo = repo.to_str().unwrap();
        let resolver = || Ok(profile.clone());
        let id = backup_plan(repo, resolver);
        let mut gated = false;

        let result = execute_with_profile(
            repo,
            &id,
            |_, _| {
                gated = true;
                Ok(())
            },
            |_| Ok(()),
            resolver,
            |_, _| false,
        );

        assert!(result.is_err());
        assert!(!gated);
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

    fn refused(profile: &Path, name: &str) -> bool {
        resolve(profile, name).is_err()
    }

    #[test]
    fn hostile_names_never_resolve_to_anything() {
        let (_keep, dir) = profile_dir();
        let profile = dir.join("workbench.sqlite");
        let long = format!("workbench.sqlite.{}.bak", "a".repeat(5000));
        let hostile = [
            "..",
            ".",
            "/etc/passwd",
            "workbench.sqlite.x.bak/",
            "workbench.sqlite.x/../workbench.sqlite.bak",
            "..\\workbench.sqlite.x.bak",
            "workbench.sqlite.x\\y.bak",
            "workbench.sqlite.x\0.bak",
            "workbench.sqlite.\u{2215}etc.bak",
            "workbench.sqlite.\u{ff0e}\u{ff0e}.bak",
            "workbench.sqlite.caf\u{e9}.bak",
            "workbench.sqlite.x .bak",
            "workbench.sqlite.x.bak ",
            "workbench.sqlite.bak",
            "workbench.sqlite..bak",
            "workbench.sqlite.x.bak.partial",
            "workbench.sqlite-wal",
            "workbench.sqlite-shm",
            "workbench.sqlite",
            "WORKBENCH.SQLITE.pre-schema-11.bak",
            "workbench.sqlite.pre-schema-11.BAK",
            "workbench.sqlite.pre-schema-11.bak\n",
            "",
            long.as_str(),
        ];
        for name in hostile {
            assert!(refused(&profile, name), "{name:?} must be refused");
            assert!(
                !is_backup_name("workbench.sqlite", name) || name == long,
                "{name:?}"
            );
        }
        assert!(dir.join("workbench.sqlite").exists());
    }

    #[test]
    fn a_directory_a_fifo_and_a_symlink_named_like_a_backup_are_refused_and_untouched() {
        let (_keep, dir) = profile_dir();
        let profile = dir.join("workbench.sqlite");
        fs::create_dir(dir.join("workbench.sqlite.d.bak")).unwrap();
        let fifo = dir.join("workbench.sqlite.f.bak");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: c is a valid NUL-terminated path that outlives the call.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        std::os::unix::fs::symlink(&profile, dir.join("workbench.sqlite.s.bak")).unwrap();
        std::os::unix::fs::symlink(
            dir.join("nowhere"),
            dir.join("workbench.sqlite.dangling.bak"),
        )
        .unwrap();
        for name in [
            "workbench.sqlite.d.bak",
            "workbench.sqlite.f.bak",
            "workbench.sqlite.s.bak",
            "workbench.sqlite.dangling.bak",
        ] {
            assert!(refused(&profile, name), "{name} must be refused");
        }
        let (rows, omitted) = list(&profile).unwrap();
        let listed: Vec<&str> = rows.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(omitted, 0);
        assert!(
            listed.iter().all(|n| !n.ends_with(".d.bak")
                && !n.ends_with(".f.bak")
                && !n.ends_with(".s.bak")
                && !n.ends_with(".dangling.bak")),
            "{listed:?}"
        );
        assert_eq!(fs::read_to_string(&profile).unwrap(), "live profile");
    }

    #[test]
    fn a_hardlink_to_the_live_profile_removes_only_its_own_name() {
        let _serial = crate::storage::hygiene::tests::serial();
        let (_keep, dir) = profile_dir();
        let profile = dir.join("workbench.sqlite");
        let (_repo_keep, repo) = repo();
        let repo = repo.to_str().unwrap();
        fs::hard_link(&profile, dir.join("workbench.sqlite.hard.bak")).unwrap();
        let resolver = || Ok(profile.clone());
        let plan = prepare_with_profile(
            repo,
            &format!("{TARGET_PREFIX}workbench.sqlite.hard.bak"),
            30,
            |_| Ok(()),
            resolver,
        )
        .unwrap();
        let (_, outcome) = execute_with_profile(
            repo,
            &plan.id,
            |_, _| Ok(()),
            |_| Ok(()),
            resolver,
            |_, _| true,
        )
        .unwrap();
        assert!(outcome.success, "{}", outcome.message);
        assert!(!dir.join("workbench.sqlite.hard.bak").exists());
        assert_eq!(fs::read_to_string(&profile).unwrap(), "live profile");
    }

    #[test]
    fn a_backup_swapped_for_a_symlink_to_the_live_profile_between_preview_and_execute_is_refused() {
        let _serial = crate::storage::hygiene::tests::serial();
        let (_keep, dir) = profile_dir();
        let profile = dir.join("workbench.sqlite");
        let (_repo_keep, repo) = repo();
        let repo = repo.to_str().unwrap();
        let resolver = || Ok(profile.clone());
        let plan = prepare_with_profile(
            repo,
            &format!("{TARGET_PREFIX}{BACKUP}"),
            30,
            |_| Ok(()),
            resolver,
        )
        .unwrap();
        fs::remove_file(dir.join(BACKUP)).unwrap();
        std::os::unix::fs::symlink(&profile, dir.join(BACKUP)).unwrap();

        let result = execute_with_profile(
            repo,
            &plan.id,
            |_, _| Ok(()),
            |_| Ok(()),
            resolver,
            |_, _| true,
        );

        assert!(
            result.is_err(),
            "a symlink in place of the backup must be refused"
        );
        assert_eq!(fs::read_to_string(&profile).unwrap(), "live profile");
    }

    #[test]
    fn a_swap_to_a_symlink_inside_the_policy_gate_never_reaches_the_live_profile() {
        let _serial = crate::storage::hygiene::tests::serial();
        let (_keep, dir) = profile_dir();
        let profile = dir.join("workbench.sqlite");
        let (_repo_keep, repo) = repo();
        let repo = repo.to_str().unwrap();
        let resolver = || Ok(profile.clone());
        let plan = prepare_with_profile(
            repo,
            &format!("{TARGET_PREFIX}{BACKUP}"),
            30,
            |_| Ok(()),
            resolver,
        )
        .unwrap();
        let swapped = dir.join(BACKUP);
        let target = profile.clone();
        // The gate runs after the first stamp check: swap there.
        let result = execute_with_profile(
            repo,
            &plan.id,
            |_, _| {
                fs::remove_file(&swapped).unwrap();
                std::os::unix::fs::symlink(&target, &swapped).unwrap();
                Ok(())
            },
            |_| Ok(()),
            resolver,
            |_, _| true,
        );
        assert!(result.is_err() || fs::symlink_metadata(&swapped).is_err());
        assert_eq!(fs::read_to_string(&profile).unwrap(), "live profile");
    }

    #[test]
    fn remove_refuses_a_symlink_and_never_touches_the_file_it_points_at() {
        let (_keep, dir) = profile_dir();
        let profile = dir.join("workbench.sqlite");
        let cancel = AtomicBool::new(false);
        let link = dir.join("workbench.sqlite.l.bak");
        std::os::unix::fs::symlink(&profile, &link).unwrap();
        // The reviewer refuses to measure a symlink at all, so no stamp can
        // authorise unlinking one through the backup path.
        assert!(tree::snapshot(&link, false, &cancel).is_err());
        let foreign = tree::snapshot(&dir.join(BACKUP), false, &cancel).unwrap();
        assert!(remove(&link, &foreign, &cancel).is_err());
        assert!(fs::symlink_metadata(&link).is_ok());
        assert_eq!(fs::read_to_string(&profile).unwrap(), "live profile");

        // A stamp taken of a real backup does not authorise unlinking a swap.
        let reviewed = tree::snapshot(&dir.join(BACKUP), false, &cancel).unwrap();
        fs::remove_file(dir.join(BACKUP)).unwrap();
        std::os::unix::fs::symlink(&profile, dir.join(BACKUP)).unwrap();
        assert!(remove(&dir.join(BACKUP), &reviewed, &cancel).is_err());
        assert_eq!(fs::read_to_string(&profile).unwrap(), "live profile");
    }

    #[test]
    fn more_than_the_listing_bound_says_so_and_stays_sorted() {
        let temp = tempfile::tempdir().unwrap();
        let dir = crate::engine::process::canonicalize_plain(temp.path()).unwrap();
        for n in 0..(MAX_LISTED + 7) {
            fs::write(dir.join(format!("workbench.sqlite.{n:04}.bak")), "x").unwrap();
        }
        let rows = entries(Ok(&dir.join("workbench.sqlite")));
        assert_eq!(rows.len(), MAX_LISTED + 1);
        let last = rows.last().unwrap();
        assert_eq!(
            last.error.as_deref(),
            Some("7 more backups were found and are not listed")
        );
        assert!(last.action.is_none() && last.confirm_only);
        assert!(rows[..MAX_LISTED]
            .iter()
            .all(|r| r.error.is_none() && r.confirm_only));
    }

    #[test]
    fn an_unreadable_profile_directory_is_an_error_row_and_a_missing_one_is_empty() {
        use std::os::unix::fs::PermissionsExt;
        let (_keep, dir) = profile_dir();
        let profile = dir.join("workbench.sqlite");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).unwrap();
        let rows = entries(Ok(&profile));
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        if fs::read_dir(&dir).is_ok() && rows.iter().all(|r| r.error.is_none()) && rows.len() > 1 {
            // Running with privileges that ignore directory modes (root): the
            // denial cannot be constructed here.
            return;
        }
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("Cannot read")));
        assert!(rows[0].action.is_none() && rows[0].confirm_only);

        let gone = dir.join("no-such-dir").join("workbench.sqlite");
        assert!(entries(Ok(&gone)).is_empty());
    }

    #[test]
    fn a_profile_directory_reached_through_a_symlink_lists_and_still_refuses_traversal() {
        let (_keep, dir) = profile_dir();
        let holder = tempfile::tempdir().unwrap();
        let via = crate::engine::process::canonicalize_plain(holder.path())
            .unwrap()
            .join("alias");
        std::os::unix::fs::symlink(&dir, &via).unwrap();
        let profile = via.join("workbench.sqlite");
        let rows = entries(Ok(&profile));
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(refused(&profile, "../workbench.sqlite.x.bak"));
        assert!(resolve(&profile, BACKUP).is_ok());
    }

    #[test]
    fn a_profile_path_that_is_not_utf8_or_has_no_directory_is_an_error_row() {
        let rows = entries(Ok(Path::new("/")));
        assert_eq!(rows.len(), 1);
        assert!(rows[0].error.is_some() && rows[0].confirm_only);
        assert!(resolve(Path::new("/"), BACKUP).is_err());
    }

    #[test]
    fn every_non_ui_hygiene_target_builder_is_refused_for_backup_targets_by_the_gate() {
        assert!(requires_user_confirmation("backup:x"));
        assert!(!requires_user_confirmation(" backup:x"));
        assert!(!requires_user_confirmation("BACKUP:x"));
        assert!(!requires_user_confirmation("cache:backup:x"));
        assert!(!requires_user_confirmation("local:backup:x"));
    }
}
