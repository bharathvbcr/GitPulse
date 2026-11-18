//! Adversarial tests for ft-38ec498d (transcripts under every Claude config
//! directory): what a pass does when a config directory vanishes, dangles, is
//! spelled many ways, cannot be read, or is named oddly. A test left `#[ignore]`
//! pins a known limit outside this change, with the reason on the attribute.
//!
//! Every test binds its own `HOME` under the shared env lock, as
//! `config_dir_tests` does, so none reads the developer's transcripts.

use super::*;

#[cfg(unix)]
use std::ffi::OsString;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

const ROOT_VARS: [&str; 5] = [
    "GITPULSE_TRANSCRIPT_ROOT",
    "GITPULSE_CLAUDE_CONFIG_DIRS",
    "CLAUDE_CONFIG_DIR",
    "USERPROFILE",
    "GITPULSE_CODEX_TRANSCRIPT_ROOT",
];

fn edit_line(cwd: &str, session: &str, file: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "sessionId": session,
        "timestamp": "2026-09-01T12:00:00.000Z",
        "version": "2.1.241",
        "cwd": cwd,
        "message": { "content": [
            { "type": "tool_use", "name": "Edit", "input": { "file_path": file } }
        ]}
    })
    .to_string()
}

/// A transcript of one edit in `repo`, in `<config_dir>/projects/slug/`.
fn transcript_in(config_dir: &Path, repo: &str, session: &str) {
    let dir = config_dir.join("projects").join("slug");
    std::fs::create_dir_all(&dir).unwrap();
    let line = edit_line(repo, session, &format!("{repo}/{session}.rs"));
    std::fs::write(dir.join(format!("{session}.jsonl")), line + "\n").unwrap();
}

/// The cursor key the pass gives that transcript when it is listed under
/// `config_dir` spelled exactly so.
fn key_in(config_dir: &Path, session: &str) -> String {
    ledger::transcript_key(
        &config_dir
            .join("projects")
            .join("slug")
            .join(format!("{session}.jsonl")),
    )
}

/// Runs `f` with `home` as the only home, no root override, and `extra` as
/// `GITPULSE_CLAUDE_CONFIG_DIRS` when given.
fn with_home<T>(home: &Path, extra: Option<&std::ffi::OsStr>, f: impl FnOnce() -> T) -> T {
    let serial = crate::test_support::env::env_serial();
    let mut env = crate::test_support::env::bind_env(&serial)
        .set("HOME", home)
        .set("XDG_CONFIG_HOME", home.join(".config"))
        .remove("CODEX_HOME");
    for var in ROOT_VARS {
        env = env.remove(var);
    }
    if let Some(extra) = extra {
        env = env.set("GITPULSE_CLAUDE_CONFIG_DIRS", extra);
    }
    let _env = env;
    f()
}

fn progress_keys(repo: &str) -> std::collections::HashSet<String> {
    ledger::transcript_progress(repo, repo)
        .unwrap()
        .offsets
        .into_keys()
        .collect()
}

// ---- forgetting -----------------------------------------------------------

/// Pass 1 reads `.claude` and `.claude-gone`; `.claude-gone` is then moved out
/// of the way (an unmounted or deleted account directory); pass 2 runs; the
/// directory returns; pass 3 runs. Returns (cursor of the gone directory's
/// transcript survived pass 2, pass 2, pass 3).
fn vanish_and_return() -> (bool, CatchUp, CatchUp) {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let repo = repo.path().to_str().unwrap();
    let gone = home.path().join(".claude-gone");
    transcript_in(&home.path().join(".claude"), repo, "S-default");
    transcript_in(&gone, repo, "S-gone");
    let key = key_in(&gone, "S-gone");
    let parked = home.path().join("parked");
    with_home(home.path(), None, || {
        let first = ingest_transcripts(repo);
        assert_eq!(first.recorded, 2, "setup: {first:?}");
        assert!(progress_keys(repo).contains(&key), "setup: cursor saved");
        std::fs::rename(&gone, &parked).unwrap();
        let second = ingest_transcripts(repo);
        let kept = progress_keys(repo).contains(&key);
        std::fs::rename(&parked, &gone).unwrap();
        let third = ingest_transcripts(repo);
        (kept, second, third)
    })
}

#[test]
#[ignore = "forgetting is not scoped to the roots a pass walked: cursors are keyed by a hash of the file path, so a vanished config dir's offsets are forgotten on the next complete pass and the returning file is re-read once (rows are refused as duplicates); closing it needs a ledger field (ft-38ec498d, documented on ClaudeSource)"]
fn a_config_dir_that_vanishes_between_passes_keeps_its_saved_offsets() {
    let (kept, second, _third) = vanish_and_return();
    assert!(
        kept,
        "the vanished directory's offset was forgotten: {second:?}"
    );
    // It is said, and the pass is not called complete: a root that held
    // transcripts is gone, a seat that never ran Claude is not remembered.
    assert!(second.error.contains(".claude-gone"), "{second:?}");
    assert_eq!(second.walk_complete, Some(false), "{second:?}");
}

#[test]
fn a_config_dir_that_returns_adds_no_duplicate_rows() {
    // Holds either way: row identity uses the canonical file key
    // (pass.rs canonical_file_key) so a replay after a forget is refused.
    let (kept, _second, third) = vanish_and_return();
    assert_eq!(third.recorded, 0, "{third:?}");
    // The limit, pinned: the vanished directory's offset was forgotten, so the
    // returning file is read again once, and each of its rows is refused as a
    // duplicate by row identity rather than recorded twice.
    assert!(!kept);
    assert_eq!(third.transcripts, 1, "{third:?}");
    assert_eq!(third.duplicates_refused, 1, "{third:?}");
}

#[test]
#[ignore = "the same limit: the returning file is re-read once"]
fn a_config_dir_that_returns_is_not_read_again() {
    let (_kept, _second, third) = vanish_and_return();
    assert_eq!(third.transcripts, 0, "{third:?}");
}

#[test]
#[cfg(unix)]
fn a_dangling_projects_symlink_is_not_called_complete() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let repo = repo.path().to_str().unwrap();
    transcript_in(&home.path().join(".claude"), repo, "S-default");
    // `projects` points at a volume that is not mounted.
    let ext = home.path().join(".claude-ext");
    std::fs::create_dir_all(&ext).unwrap();
    std::os::unix::fs::symlink(
        home.path().join("not-mounted/projects"),
        ext.join("projects"),
    )
    .unwrap();

    let out = with_home(home.path(), None, || ingest_transcripts(repo));

    assert_eq!(out.recorded, 1, "{out:?}");
    assert_eq!(out.walk_complete, Some(false), "{out:?}");
    assert!(out.error.contains(".claude-ext"), "{out:?}");
}

// ---- the bound and its ordering ------------------------------------------

#[test]
fn a_new_dir_that_pushes_the_last_root_past_the_bound_does_not_forget_it() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let repo = repo.path().to_str().unwrap();
    let n = source::MAX_CLAUDE_ROOTS;
    for i in 0..n {
        transcript_in(
            &home.path().join(format!(".claude-seat{i:02}")),
            repo,
            &format!("S-{i:02}"),
        );
    }
    let last = home.path().join(format!(".claude-seat{:02}", n - 1));
    let last_key = key_in(&last, &format!("S-{:02}", n - 1));

    with_home(home.path(), None, || {
        let first = ingest_transcripts(repo);
        assert_eq!(first.recorded, i64::try_from(n).unwrap(), "{first:?}");
        assert_eq!(first.walk_complete, Some(true), "{first:?}");

        // `.claude-aaa` sorts before every seat: the sibling list is sorted, so
        // the previously read last root now falls outside the first 16.
        transcript_in(&home.path().join(".claude-aaa"), repo, "S-aaa");
        let second = ingest_transcripts(repo);
        assert_eq!(second.recorded, 1, "{second:?}");
        assert_eq!(second.walk_complete, Some(false), "{second:?}");
        assert!(
            second.error.contains("1 more Claude config directories"),
            "{second:?}"
        );
        // The root that fell out was not walked: its offset must survive
        // (gap sets blocks_forget in pass.rs).
        assert!(
            progress_keys(repo).contains(&last_key),
            "the root pushed past the bound had its offset forgotten: {second:?}"
        );

        // The set is order-dependent, but the dropped root is read again,
        // once, with nothing recorded twice, as soon as there is room.
        std::fs::remove_dir_all(home.path().join(".claude-aaa")).unwrap();
        let third = ingest_transcripts(repo);
        assert_eq!(third.recorded, 0, "{third:?}");
        assert_eq!(third.transcripts, 0, "{third:?}");
    });
}

#[test]
#[cfg(unix)]
fn a_thousand_entries_are_bounded_and_the_surplus_is_named() {
    let home = tempfile::tempdir().unwrap();
    let many = tempfile::tempdir().unwrap();
    let mut extra = OsString::new();
    for n in 0..1000 {
        let dir = many.path().join(format!("d{n:04}"));
        std::fs::create_dir_all(dir.join("projects")).unwrap();
        // Empty segments and an exact duplicate between the real entries.
        extra.push(":");
        extra.push(&dir);
        extra.push("::");
        extra.push(&dir);
    }

    let (found, gap) = with_home(home.path(), Some(&extra), || {
        let found = source::sources()[0].roots();
        (found.list.len(), found.gap)
    });

    assert_eq!(found, source::MAX_CLAUDE_ROOTS);
    // Duplicates are not counted as surplus: 1000 distinct - 16 read.
    let gap = gap.expect("the surplus is reported");
    assert!(gap.contains("984 more Claude config directories"), "{gap}");
}

#[test]
#[cfg(unix)]
fn a_thousand_nonexistent_entries_make_no_gap_and_no_error() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let repo = repo.path().to_str().unwrap();
    transcript_in(&home.path().join(".claude"), repo, "S-default");
    let mut extra = OsString::new();
    for n in 0..1000 {
        extra.push(format!(":{}/nope{n}", home.path().display()));
    }

    let out = with_home(home.path(), Some(&extra), || ingest_transcripts(repo));

    assert_eq!(out.recorded, 1, "{out:?}");
    assert_eq!(out.walk_complete, Some(true), "{out:?}");
    assert_eq!(out.error, "", "{out:?}");
}

// ---- one directory, many spellings ----------------------------------------

#[test]
#[cfg(unix)]
fn textual_spellings_of_one_directory_are_read_once() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let repo = repo.path().to_str().unwrap();
    let real = home.path().join(".claude-chandra");
    transcript_in(&real, repo, "S-chandra");
    let mut extra = OsString::new();
    let spell = |suffix: &str| {
        let mut p = real.clone().into_os_string();
        p.push(suffix);
        p
    };
    for s in [spell("/"), spell("/./"), spell("//"), spell("")] {
        extra.push(s);
        extra.push(":");
    }
    // The canonical spelling too (`/var` vs `/private/var` on macOS).
    extra.push(real.canonicalize().unwrap());

    let out = with_home(home.path(), Some(&extra), || ingest_transcripts(repo));

    assert_eq!(out.transcripts, 1, "{out:?}");
    assert_eq!(out.recorded, 1, "{out:?}");
    assert_eq!(out.transcripts_listed, 1, "{out:?}");
}

#[test]
fn a_case_variant_spelling_is_read_once() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let repo = repo.path().to_str().unwrap();
    let real = home.path().join(".claude-chandra");
    transcript_in(&real, repo, "S-chandra");
    let variant = home.path().join(".CLAUDE-CHANDRA");
    if !variant.exists() {
        // A case-sensitive volume: the variant is a different (absent) path.
        return;
    }

    let out = with_home(home.path(), Some(variant.as_os_str()), || {
        ingest_transcripts(repo)
    });

    assert_eq!(out.transcripts, 1, "{out:?}");
    assert_eq!(out.recorded, 1, "{out:?}");
}

#[test]
#[cfg(unix)]
#[ignore = "the cursor key hashes the listed spelling of the path, so a flip re-reads the file once; row identity still refuses the duplicate rows"]
fn a_spelling_flip_between_passes_neither_duplicates_nor_rereads() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let repo = repo.path().to_str().unwrap();
    let real = home.path().join(".claude-chandra");
    transcript_in(&real, repo, "S-chandra");
    let alias = home.path().join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();

    // Pass 1: launched with the alias named first (e.g. CLAUDE_CONFIG_DIR set
    // in a terminal). Pass 2: launched from Finder, so only the real name.
    let first = with_home(home.path(), Some(alias.as_os_str()), || {
        ingest_transcripts(repo)
    });
    assert_eq!(first.recorded, 1, "{first:?}");
    let second = with_home(home.path(), None, || ingest_transcripts(repo));

    // Holds: row identity is the canonical file key (pass.rs canonical_file_key).
    assert_eq!(second.recorded, 0, "{second:?}");
    assert_eq!(second.transcripts, 0, "{second:?}");
}

#[test]
#[ignore = "row identity includes the file key, so one session copied into two accounts records twice; ledger-level dedupe is outside this change"]
fn the_same_session_copied_into_two_accounts_is_recorded_once() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let repo = repo.path().to_str().unwrap();
    // A session moved between accounts with `cp`: same session id, same lines,
    // two distinct files under two config dirs.
    transcript_in(&home.path().join(".claude"), repo, "S-same");
    transcript_in(&home.path().join(".claude-work"), repo, "S-same");

    let out = with_home(home.path(), None, || ingest_transcripts(repo));

    assert_eq!(out.recorded, 1, "{out:?}");
}

// ---- attribution ----------------------------------------------------------

#[test]
fn rows_from_a_second_account_carry_the_actor_and_session() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let repo = repo.path().to_str().unwrap();
    transcript_in(&home.path().join(".claude-x"), repo, "S-x");

    let events = with_home(home.path(), None, || {
        let out = ingest_transcripts(repo);
        assert_eq!(out.recorded, 1, "{out:?}");
        ledger::tail(repo, 0, 1000).unwrap()
    });

    let rows: Vec<_> = events
        .iter()
        .filter(|event| transcript::ACTIONS.contains(&event.action.as_str()))
        .collect();
    assert_eq!(rows.len(), 1, "{events:?}");
    assert_eq!(rows[0].actor_id.as_deref(), Some("claude-code"), "{rows:?}");
    assert!(
        rows[0]
            .session_id
            .as_deref()
            .is_some_and(|session| session.contains("S-x")),
        "{rows:?}"
    );
}

// ---- inputs ----------------------------------------------------------------

#[test]
#[cfg(unix)]
fn a_relative_config_dir_is_refused_not_read_against_the_app_cwd() {
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let repo = repo.path().to_str().unwrap();
    transcript_in(cfg.path(), repo, "S-rel");
    // The same directory, spelled relative to this process's cwd.
    let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
    let up = cwd.components().count() - 1;
    let target = cfg.path().canonicalize().unwrap();
    let rel: PathBuf = (0..up)
        .map(|_| "..")
        .collect::<PathBuf>()
        .join(target.strip_prefix("/").unwrap());
    assert!(rel.is_relative());

    let out = with_home(home.path(), Some(rel.as_os_str()), || {
        ingest_transcripts(repo)
    });

    assert_eq!(out.recorded, 0, "{out:?}");
    assert!(out.error.contains("not an absolute path"), "{out:?}");
}

#[test]
fn an_empty_explicit_root_is_ignored_like_an_unset_one() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let repo = repo.path().to_str().unwrap();
    transcript_in(&home.path().join(".claude"), repo, "S-default");

    let serial = crate::test_support::env::env_serial();
    let _env = crate::test_support::env::bind_env(&serial)
        .set("HOME", home.path())
        .set("GITPULSE_TRANSCRIPT_ROOT", "");
    let out = ingest_transcripts(repo);

    assert_eq!(out.recorded, 1, "{out:?}");
}

#[test]
fn an_empty_home_falls_back_to_userprofile_for_every_source() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let repo = repo.path().to_str().unwrap();
    transcript_in(&home.path().join(".claude"), repo, "S-win");

    let serial = crate::test_support::env::env_serial();
    let mut env = crate::test_support::env::bind_env(&serial)
        .set("HOME", "")
        .set("USERPROFILE", home.path())
        .set("XDG_CONFIG_HOME", home.path().join(".config"))
        .remove("CODEX_HOME");
    for var in ROOT_VARS.iter().filter(|v| **v != "USERPROFILE") {
        env = env.remove(var);
    }
    let _env = env;
    let out = ingest_transcripts(repo);
    // Claude resolves through workbench `var()`, which drops the empty HOME
    // before `or_else`: holds.
    assert_eq!(out.recorded, 1, "{out:?}");
    let codex = source::sources()[1].roots();

    assert_eq!(codex.list.len(), 1, "{codex:?}");
}

#[test]
fn no_home_and_no_override_says_so_instead_of_calling_it_clean() {
    let repo = tempfile::tempdir().unwrap();
    let repo = repo.path().to_str().unwrap();

    let serial = crate::test_support::env::env_serial();
    let mut env = crate::test_support::env::bind_env(&serial)
        .remove("HOME")
        .remove("XDG_CONFIG_HOME")
        .remove("CODEX_HOME");
    for var in ROOT_VARS {
        env = env.remove(var);
    }
    let _env = env;
    let out = ingest_transcripts(repo);

    assert_eq!(out.error, NO_TRANSCRIPT_ROOT, "{out:?}");
    assert_eq!(out.walk_complete, None, "{out:?}");
}

// ---- unreadable roots beside readable ones ---------------------------------

#[cfg(unix)]
struct Restore(PathBuf);

#[cfg(unix)]
impl Drop for Restore {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

#[test]
#[cfg(unix)]
fn a_projects_dir_that_cannot_be_listed_is_named_and_the_others_still_ingest() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let repo = repo.path().to_str().unwrap();
    transcript_in(&home.path().join(".claude"), repo, "S-default");
    let locked = home.path().join(".claude-locked");
    transcript_in(&locked, repo, "S-locked");
    let projects = locked.join("projects");
    let _restore = Restore(projects.clone());
    std::fs::set_permissions(&projects, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&projects).is_ok() {
        // Running as root: permissions do not bind.
        return;
    }

    let out = with_home(home.path(), None, || ingest_transcripts(repo));

    assert_eq!(out.recorded, 1, "the readable root still counts: {out:?}");
    assert_eq!(out.walk_complete, Some(false), "{out:?}");
    assert!(out.error.contains(".claude-locked"), "{out:?}");
}

#[test]
#[cfg(unix)]
fn a_config_dir_that_cannot_be_entered_is_named_and_the_others_still_ingest() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let repo = repo.path().to_str().unwrap();
    transcript_in(&home.path().join(".claude"), repo, "S-default");
    let locked = home.path().join(".claude-locked");
    transcript_in(&locked, repo, "S-locked");
    let _restore = Restore(locked.clone());
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&locked).is_ok() {
        return;
    }

    let out = with_home(home.path(), None, || ingest_transcripts(repo));

    // Holds: stat of `projects` fails with a non-NotFound error, the candidate
    // is kept, and pass.rs reports it (`note || !root.explicit`).
    assert_eq!(out.recorded, 1, "{out:?}");
    assert_eq!(out.walk_complete, Some(false), "{out:?}");
    assert!(out.error.contains(".claude-locked"), "{out:?}");
}

#[test]
#[cfg(unix)]
fn a_projects_symlink_to_a_file_and_a_symlink_loop_are_reported() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let repo = repo.path().to_str().unwrap();
    transcript_in(&home.path().join(".claude"), repo, "S-default");
    let to_file = home.path().join(".claude-file");
    std::fs::create_dir_all(&to_file).unwrap();
    std::fs::write(home.path().join("plain-file"), "x").unwrap();
    std::os::unix::fs::symlink(home.path().join("plain-file"), to_file.join("projects")).unwrap();
    let looped = home.path().join(".claude-loop");
    std::fs::create_dir_all(&looped).unwrap();
    std::os::unix::fs::symlink("projects", looped.join("projects")).unwrap();

    let out = with_home(home.path(), None, || ingest_transcripts(repo));

    // Holds: neither is NotFound, so both candidates are kept and reported.
    assert_eq!(out.recorded, 1, "{out:?}");
    assert_eq!(out.walk_complete, Some(false), "{out:?}");
    assert!(out.error.contains(".claude-file"), "{out:?}");
    assert!(out.error.contains(".claude-loop"), "{out:?}");
}

#[test]
#[cfg(unix)]
fn a_whole_config_dir_symlinked_to_the_default_is_read_once() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let repo = repo.path().to_str().unwrap();
    transcript_in(&home.path().join(".claude"), repo, "S-default");
    std::os::unix::fs::symlink(
        home.path().join(".claude"),
        home.path().join(".claude-work"),
    )
    .unwrap();
    // `.claude-work` is a symlink: the sibling scan requires a directory file
    // type (conversation.rs `is_dir` on DirEntry::file_type does not follow
    // links), so it is not even a candidate; name it explicitly to force the case.
    let extra = home.path().join(".claude-work");

    let out = with_home(home.path(), Some(extra.as_os_str()), || {
        ingest_transcripts(repo)
    });

    assert_eq!(out.transcripts, 1, "{out:?}");
    assert_eq!(out.recorded, 1, "{out:?}");
    assert_eq!(out.transcripts_listed, 1, "{out:?}");
}
