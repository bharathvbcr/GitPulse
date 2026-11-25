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

/// The text a native dialog may show for a backup: its bare file name (which
/// `is_backup_name` already constrains), with every control, bidi and format
/// character written as a visible `\u{..}` escape and the length capped, so a
/// hostile path cannot put misleading text ahead of the warning.
pub(super) fn dialog_name(path: &Path) -> String {
    const MAX: usize = 120;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut shown = String::new();
    for (index, c) in name.chars().enumerate() {
        if index == MAX {
            shown.push('…');
            break;
        }
        let hidden = c.is_control()
            || matches!(
                c,
                '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
                    | '\u{061c}' | '\u{feff}'
            );
        if hidden {
            shown.push_str(&format!("\\u{{{:x}}}", c as u32));
        } else {
            shown.push(c);
        }
    }
    shown
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
        assert!(asked[0].1.contains(BACKUP));
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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

    // ---- Adversarial tests for the native confirmation (ft-845b7f1b) ----

    struct RemoveOnDrop(PathBuf);
    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    type Fixture = (
        tempfile::TempDir,
        PathBuf,
        tempfile::TempDir,
        String,
        PathBuf,
    );

    fn setup() -> Fixture {
        let (keep, dir) = profile_dir();
        let (repo_keep, repo) = repo();
        let profile = dir.join("workbench.sqlite");
        (
            keep,
            dir,
            repo_keep,
            repo.to_str().unwrap().to_owned(),
            profile,
        )
    }

    /// Previews BACKUP, then executes with a confirmation that runs
    /// `mutate` while the dialog is "open" and then answers yes. Returns the
    /// refusal text (or "removed") and whether the live profile survived.
    fn accept_after(mutate: impl Fn(&Path, &Path)) -> (String, bool, bool) {
        let (_k, dir, _rk, repo, profile) = setup();
        let resolver = || Ok(profile.clone());
        let id = backup_plan(&repo, resolver);
        let backup = dir.join(BACKUP);
        let result = execute_with_profile(
            &repo,
            &id,
            |_, _| Ok(()),
            |_| Ok(()),
            resolver,
            |_, _| {
                mutate(&backup, &profile);
                true
            },
        );
        let text = match result {
            Ok((_, outcome)) if outcome.success => "removed".to_owned(),
            Ok((_, outcome)) => format!("outcome-failed: {}", outcome.message),
            Err(error) => error.message().to_owned(),
        };
        let exists = fs::symlink_metadata(&backup).is_ok();
        let live = fs::read_to_string(&profile).is_ok_and(|s| s == "live profile");
        (text, exists, live)
    }

    #[test]
    fn adv_confirm_open_then_backup_appended_is_refused() {
        let _serial = crate::storage::hygiene::tests::serial();
        let (text, exists, live) = accept_after(|backup, _| {
            use std::io::Write;
            fs::OpenOptions::new()
                .append(true)
                .open(backup)
                .unwrap()
                .write_all(b"x")
                .unwrap();
        });
        assert!(text.contains("changed"), "{text}");
        assert!(exists && live);
    }

    #[test]
    fn adv_confirm_open_then_same_size_rewrite_with_mtime_restored_is_refused() {
        let _serial = crate::storage::hygiene::tests::serial();
        let (text, exists, live) = accept_after(|backup, _| {
            let before = fs::metadata(backup).unwrap().modified().unwrap();
            fs::write(backup, vec![9u8; 4096]).unwrap();
            crate::test_support::set_modified(backup, before).unwrap();
        });
        assert!(text != "removed", "removed a rewritten file: {text}");
        assert!(exists && live, "{text}");
    }

    #[test]
    fn adv_confirm_open_then_replaced_by_new_inode_with_same_size_and_mtime_is_refused() {
        let _serial = crate::storage::hygiene::tests::serial();
        let (text, exists, live) = accept_after(|backup, _| {
            let before = fs::metadata(backup).unwrap().modified().unwrap();
            let fresh = backup.with_extension("bak.swap");
            fs::write(&fresh, vec![9u8; 4096]).unwrap();
            crate::test_support::set_modified(&fresh, before).unwrap();
            fs::rename(&fresh, backup).unwrap();
        });
        assert!(text != "removed", "removed a replaced file: {text}");
        assert!(exists && live, "{text}");
    }

    #[test]
    #[cfg(unix)]
    fn adv_confirm_open_then_swapped_for_symlink_to_live_profile_is_refused() {
        let _serial = crate::storage::hygiene::tests::serial();
        let (text, _exists, live) = accept_after(|backup, profile| {
            fs::remove_file(backup).unwrap();
            std::os::unix::fs::symlink(profile, backup).unwrap();
        });
        assert!(text != "removed", "{text}");
        assert!(live, "the live profile must survive: {text}");
    }

    #[test]
    fn adv_confirm_open_then_swapped_for_a_directory_or_deleted_is_refused() {
        let _serial = crate::storage::hygiene::tests::serial();
        let (text, exists, live) = accept_after(|backup, _| {
            fs::remove_file(backup).unwrap();
            fs::create_dir(backup).unwrap();
            fs::write(backup.join("inner"), "x").unwrap();
        });
        assert!(text != "removed", "{text}");
        assert!(exists && live, "{text}");
        let (text, _exists, live) = accept_after(|backup, _| fs::remove_file(backup).unwrap());
        assert!(text != "removed" || live, "{text}");
        assert!(live);
    }

    #[test]
    fn adv_confirm_accepts_but_the_gate_refuses_removes_nothing() {
        let _serial = crate::storage::hygiene::tests::serial();
        let (_k, dir, _rk, repo, profile) = setup();
        let resolver = || Ok(profile.clone());
        let id = backup_plan(&repo, resolver);
        let result = execute_with_profile(
            &repo,
            &id,
            |_, _| Err::<(), _>(CommandError::from("policy refused")),
            |_| Ok(()),
            resolver,
            |_, _| true,
        );
        assert!(result.is_err());
        assert!(dir.join(BACKUP).exists());
    }

    #[test]
    fn adv_a_declined_plan_is_consumed_and_a_retry_with_the_same_id_is_refused() {
        let _serial = crate::storage::hygiene::tests::serial();
        let (_k, dir, _rk, repo, profile) = setup();
        let resolver = || Ok(profile.clone());
        let id = backup_plan(&repo, resolver);
        let first = execute_with_profile(
            &repo,
            &id,
            |_, _| Ok(()),
            |_| Ok(()),
            resolver,
            |_, _| false,
        );
        assert!(first.is_err());
        let asked = std::cell::Cell::new(false);
        let retry = execute_with_profile(
            &repo,
            &id,
            |_, _| Ok(()),
            |_| Ok(()),
            resolver,
            |_, _| {
                asked.set(true);
                true
            },
        );
        let Err(error) = retry else {
            panic!("a retry of a declined plan must be refused");
        };
        assert!(
            error.message().contains("already used"),
            "{}",
            error.message()
        );
        assert!(!asked.get(), "no dialog for a consumed plan");
        assert!(dir.join(BACKUP).exists());
        assert!(crate::storage::hygiene::cancel(&repo, &id).is_ok());
    }

    #[test]
    fn adv_the_decline_all_entry_points_cannot_remove_a_real_backup() {
        use crate::storage::hygiene::{execute, execute_with_activity, prepare};
        let _serial = crate::storage::hygiene::tests::serial();
        let (_repo_keep, repo) = repo();
        let repo = repo.to_str().unwrap();
        let profile = crate::storage::hygiene::profile_path().unwrap();
        fs::create_dir_all(profile.parent().unwrap()).unwrap();
        let path = profile.with_file_name("workbench.sqlite.advw3decline.bak");
        fs::write(&path, "backup body").unwrap();
        let _cleanup = RemoveOnDrop(path.clone());
        let target = "backup:workbench.sqlite.advw3decline.bak";

        let gated = std::cell::Cell::new(false);
        let id = prepare(repo, target, 30).unwrap().id;
        let steward = execute(repo, &id, |_, _| {
            gated.set(true);
            Ok(())
        });
        assert!(
            steward.is_err(),
            "steward path removed or accepted a backup"
        );
        assert!(path.exists() && !gated.get(), "steward reached the gate");

        let id = prepare(repo, target, 30).unwrap().id;
        let cleaner = execute_with_activity(
            repo,
            &id,
            |_, _| {
                gated.set(true);
                Ok(())
            },
            |_| Ok(()),
        );
        assert!(
            cleaner.is_err(),
            "cleaner path removed or accepted a backup"
        );
        assert!(path.exists() && !gated.get(), "cleaner reached the gate");
    }

    #[test]
    fn adv_a_plan_whose_scope_was_not_profile_backup_for_a_backup_target_is_not_removed() {
        let _serial = crate::storage::hygiene::tests::serial();
        let (_k, dir, _rk, repo, profile) = setup();
        let resolver = || Ok(profile.clone());
        let id = backup_plan(&repo, resolver);
        crate::test_support::set_modified(
            &dir.join(BACKUP),
            SystemTime::now() - Duration::from_secs(100 * 86_400),
        )
        .unwrap();
        // Not reachable through the public API (prepare is the only inserter and
        // sets scope and target together); forced here to see what the confirm
        // gate, which keys on scope, would do if the two ever disagreed.
        crate::storage::hygiene::lock_plans()
            .pending
            .get_mut(&id)
            .unwrap()
            .view
            .scope = "repository".into();
        let result = execute_with_profile(
            &repo,
            &id,
            |_, _| Ok(()),
            |_| Ok(()),
            resolver,
            |_, _| false,
        );
        let exists = dir.join(BACKUP).exists();
        eprintln!(
            "MISMATCH result: {:?}",
            result
                .as_ref()
                .map(|r| r.1.message.clone())
                .map_err(|e| e.message().to_owned())
        );
        assert!(
            exists,
            "backup removed with no confirmation: {:?}",
            result
                .as_ref()
                .map(|r| r.1.message.clone())
                .map_err(|e| e.message().to_owned())
        );
    }

    #[test]
    fn adv_no_local_or_cache_target_can_name_the_profile_directory() {
        let profile = crate::storage::hygiene::profile_path().unwrap();
        let dir = profile.parent().unwrap();
        let home = std::env::var("HOME").unwrap();
        let relative = dir
            .strip_prefix(&home)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            crate::storage::hygiene::providers::local_provider(Path::new(&home), &relative)
                .is_err(),
            "{relative} is a recognised local artifact directory"
        );
    }

    #[test]
    fn adv_a_confirm_that_panics_leaves_no_lock_held() {
        use crate::storage::hygiene::{execute_confirmed, prepare};
        let _serial = crate::storage::hygiene::tests::serial();
        let (_repo_keep, repo) = repo();
        let repo = repo.to_str().unwrap();
        let profile = crate::storage::hygiene::profile_path().unwrap();
        fs::create_dir_all(profile.parent().unwrap()).unwrap();
        let path = profile.with_file_name("workbench.sqlite.advw3panic.bak");
        fs::write(&path, "backup body").unwrap();
        let _cleanup = RemoveOnDrop(path.clone());
        let target = "backup:workbench.sqlite.advw3panic.bak";
        let id = prepare(repo, target, 30).unwrap().id;
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = execute_confirmed(repo, &id, |_, _| Ok(()), |_, _| panic!("dialog failed"));
        }));
        assert!(unwound.is_err());
        assert!(path.exists());
        let id = prepare(repo, target, 30).unwrap().id;
        let (_, outcome) = execute_confirmed(repo, &id, |_, _| Ok(()), |_, _| true)
            .expect("locks must be free after a panicking confirm");
        assert!(outcome.success && !path.exists());
    }

    #[test]
    fn adv_liveness_what_is_refused_while_the_confirmation_is_open() {
        use crate::storage::hygiene::{
            cancel, execute, execute_confirmed, execute_with_activity, global, prepare,
        };
        use std::sync::mpsc;
        let _serial = crate::storage::hygiene::tests::serial();
        let (_repo_keep, repo) = repo();
        let repo = repo.to_str().unwrap().to_owned();
        let profile = crate::storage::hygiene::profile_path().unwrap();
        fs::create_dir_all(profile.parent().unwrap()).unwrap();
        let first = profile.with_file_name("workbench.sqlite.advw3live1.bak");
        let second = profile.with_file_name("workbench.sqlite.advw3live2.bak");
        fs::write(&first, "one").unwrap();
        fs::write(&second, "two").unwrap();
        let _c1 = RemoveOnDrop(first.clone());
        let _c2 = RemoveOnDrop(second.clone());
        let id1 = prepare(&repo, "backup:workbench.sqlite.advw3live1.bak", 30)
            .unwrap()
            .id;
        let id2 = prepare(&repo, "backup:workbench.sqlite.advw3live2.bak", 30)
            .unwrap()
            .id;

        let (open_tx, open_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (repo_ref, id1_ref) = (&repo, &id1);
        let probes = std::thread::scope(|scope| {
            let holder = scope.spawn(move || {
                execute_confirmed(
                    repo_ref,
                    id1_ref,
                    |_, _| Ok(()),
                    move |_, _| {
                        open_tx.send(()).unwrap();
                        release_rx.recv_timeout(Duration::from_secs(60)).is_ok()
                    },
                )
            });
            open_rx
                .recv_timeout(Duration::from_secs(60))
                .expect("the confirmation never opened");
            let msg = |e: CommandError| e.message().to_owned();
            let started = std::time::Instant::now();
            let lock = global::mutation_lock().err().map(msg);
            let steward = execute(&repo, &id2, |_, _| Ok(())).err().map(msg);
            let confirmed =
                execute_confirmed(&repo, &id2, |_, _| Ok(()), |_, _| panic!("must not ask"))
                    .err()
                    .map(msg);
            let cleaner = execute_with_activity(&repo, &id2, |_, _| Ok(()), |_| Ok(()))
                .err()
                .map(msg);
            let prepared = prepare(&repo, "backup:workbench.sqlite.advw3live2.bak", 30);
            let prepared_ok = prepared.is_ok();
            let cancelled = prepared
                .map(|p| cancel(&repo, &p.id).is_ok())
                .unwrap_or(false);
            let probe_ms = started.elapsed().as_millis();
            release_tx.send(()).unwrap();
            let first_result = holder.join().unwrap();
            (
                lock,
                steward,
                confirmed,
                cleaner,
                prepared_ok,
                cancelled,
                probe_ms,
                first_result
                    .map(|r| r.1.success)
                    .map_err(|e| e.message().to_owned()),
            )
        });
        eprintln!("LIVENESS while confirm open: {probes:#?}");
        let (lock, steward, confirmed, cleaner, prepared_ok, cancelled, probe_ms, first) = probes;
        // The second plan was only probed against: release its pending slot so
        // the shared plan budget is not left spent for other tests.
        let _ = cancel(&repo, &id2);
        assert!(
            lock.is_some(),
            "mutation_lock was free while the dialog was open"
        );
        assert!(steward.is_some() && confirmed.is_some() && cleaner.is_some());
        assert!(
            prepared_ok && cancelled,
            "prepare/cancel must not be blocked"
        );
        assert!(
            probe_ms < 5_000,
            "probes waited {probe_ms} ms: refusals must be immediate"
        );
        assert_eq!(first, Ok(true));
        assert!(!first_path_exists(&profile));
        assert!(second.exists());
    }

    fn first_path_exists(profile: &Path) -> bool {
        profile
            .with_file_name("workbench.sqlite.advw3live1.bak")
            .exists()
    }

    fn hostile_message() -> (String, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::engine::process::canonicalize_plain(temp.path()).unwrap();
        // Windows cannot hold a name with a newline or a `"` (every control
        // character and `"` is refused), so the part of the attack a path
        // there can carry is the bidi override, the quote and the length.
        let separator = if cfg!(windows) { "" } else { "\n\n" };
        let quote = if cfg!(windows) { "" } else { "\"" };
        let evil = format!(
            "evil{separator}Nothing will be removed.\u{202e}{quote}'{}",
            "x".repeat(150)
        );
        let dir = root.join(&evil).join("y".repeat(200));
        fs::create_dir_all(&dir).unwrap();
        let profile = dir.join("workbench.sqlite");
        fs::write(&profile, "live profile").unwrap();
        fs::write(dir.join(BACKUP), "body").unwrap();
        let (_repo_keep, repo) = repo();
        let repo = repo.to_str().unwrap();
        let resolver = || Ok(profile.clone());
        let id = backup_plan(repo, resolver);
        let seen = std::cell::RefCell::new(String::new());
        let _ = execute_with_profile(
            repo,
            &id,
            |_, _| Ok(()),
            |_| Ok(()),
            resolver,
            |_, message| {
                *seen.borrow_mut() = message.to_owned();
                false
            },
        );
        let message = seen.into_inner();
        (message, dir)
    }

    #[test]
    fn adv_hostile_path_still_ends_with_the_rollback_warning() {
        let _serial = crate::storage::hygiene::tests::serial();
        let (message, dir) = hostile_message();
        assert!(message.ends_with(WARNING), "{message:?}");
        assert!(message.starts_with(BACKUP), "{message:?}");
        assert!(!message.contains(&*dir.to_string_lossy()));
        assert!(message.contains("cannot be rolled back"));
    }

    #[test]
    fn adv_hostile_path_is_neutralised_before_it_reaches_the_dialog() {
        let _serial = crate::storage::hygiene::tests::serial();
        let (message, _dir) = hostile_message();
        let shown = message
            .strip_suffix(WARNING)
            .unwrap()
            .trim_end_matches('\n');
        assert!(
            !shown.chars().any(|c| c.is_control()
                || ('\u{202a}'..='\u{202e}').contains(&c)
                || ('\u{2066}'..='\u{2069}').contains(&c)),
            "control or bidi characters reach the dialog: {shown:?}"
        );
        assert!(
            shown.chars().count() < 400,
            "{} chars",
            shown.chars().count()
        );
    }
}
