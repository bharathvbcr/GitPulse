//! Transcripts under every Claude configuration directory, not only one.
//!
//! A person with several accounts keeps one directory per account
//! (`~/.claude-work`, `~/.claude-lab`); the pass read `~/.claude/projects` alone
//! and reported a clean catch-up, so the other accounts' edits were never
//! recovered. Every test binds its own `HOME`, so none reads the developer's.

use super::*;

const ROOT_VARS: [&str; 4] = [
    "GITPULSE_TRANSCRIPT_ROOT",
    "GITPULSE_CLAUDE_CONFIG_DIRS",
    "CLAUDE_CONFIG_DIR",
    "USERPROFILE",
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
fn transcript_in(config_dir: &std::path::Path, repo: &str, session: &str) {
    let dir = config_dir.join("projects").join("slug");
    std::fs::create_dir_all(&dir).unwrap();
    let line = edit_line(repo, session, &format!("{repo}/{session}.rs"));
    std::fs::write(dir.join(format!("{session}.jsonl")), line + "\n").unwrap();
}

/// Runs `f` with `home` as the only home and no root override.
fn with_home<T>(
    home: &std::path::Path,
    extra: Option<&std::ffi::OsStr>,
    f: impl FnOnce() -> T,
) -> T {
    let serial = crate::test_support::env::env_serial();
    let mut env = crate::test_support::env::bind_env(&serial)
        .set("HOME", home)
        .set("XDG_CONFIG_HOME", home.join(".config"));
    for var in ROOT_VARS {
        env = env.remove(var);
    }
    if let Some(extra) = extra {
        env = env.set("GITPULSE_CLAUDE_CONFIG_DIRS", extra);
    }
    let _env = env;
    f()
}

fn repo_dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn transcripts_under_two_config_dirs_are_both_ingested() {
    let home = tempfile::tempdir().unwrap();
    let repo = repo_dir();
    let repo = repo.path().to_str().unwrap();
    transcript_in(&home.path().join(".claude"), repo, "S-default");
    transcript_in(&home.path().join(".claude-chandra"), repo, "S-chandra");
    transcript_in(&home.path().join(".claude-lappi"), repo, "S-lappi");

    let out = with_home(home.path(), None, || ingest_transcripts(repo));

    assert_eq!(out.transcripts, 3, "{out:?}");
    assert_eq!(out.recorded, 3, "{out:?}");
    assert_eq!(out.walk_complete, Some(true), "{out:?}");
    assert_eq!(out.error, "", "{out:?}");
}

#[test]
fn an_account_dir_without_projects_is_not_a_gap() {
    let home = tempfile::tempdir().unwrap();
    let repo = repo_dir();
    let repo = repo.path().to_str().unwrap();
    transcript_in(&home.path().join(".claude-chandra"), repo, "S-chandra");
    std::fs::create_dir_all(home.path().join(".claude-never-ran")).unwrap();

    let out = with_home(home.path(), None, || ingest_transcripts(repo));

    assert_eq!(out.recorded, 1, "{out:?}");
    assert_eq!(out.walk_complete, Some(true), "{out:?}");
    assert_eq!(out.error, "", "{out:?}");
}

#[test]
fn an_unreadable_second_root_is_reported_not_called_complete() {
    let home = tempfile::tempdir().unwrap();
    let repo = repo_dir();
    let repo = repo.path().to_str().unwrap();
    transcript_in(&home.path().join(".claude"), repo, "S-default");
    // `projects` is a file, so the account's transcripts cannot be listed.
    let broken = home.path().join(".claude-broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join("projects"), "not a directory").unwrap();

    let out = with_home(home.path(), None, || ingest_transcripts(repo));

    assert_eq!(out.recorded, 1, "the readable root still counts: {out:?}");
    assert_eq!(out.walk_complete, Some(false), "{out:?}");
    assert!(out.error.contains(".claude-broken"), "{out:?}");
}

#[test]
#[cfg(unix)]
fn one_directory_reached_by_two_spellings_is_read_once() {
    let home = tempfile::tempdir().unwrap();
    let repo = repo_dir();
    let repo = repo.path().to_str().unwrap();
    let real = home.path().join(".claude-chandra");
    transcript_in(&real, repo, "S-chandra");
    let alias = home.path().join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let extra = std::env::join_paths([&alias, &real]).unwrap();

    let out = with_home(home.path(), Some(&extra), || ingest_transcripts(repo));

    assert_eq!(out.transcripts, 1, "{out:?}");
    assert_eq!(out.recorded, 1, "{out:?}");
    assert_eq!(out.transcripts_listed, 1, "{out:?}");
}

#[test]
fn roots_past_the_bound_are_reported_and_not_read() {
    let home = tempfile::tempdir().unwrap();
    let repo = repo_dir();
    let repo = repo.path().to_str().unwrap();
    let many = source::MAX_CLAUDE_ROOTS + 9;
    for n in 0..many {
        transcript_in(
            &home.path().join(format!(".claude-seat{n:02}")),
            repo,
            &format!("S-{n:02}"),
        );
    }

    let out = with_home(home.path(), None, || ingest_transcripts(repo));

    assert_eq!(
        out.transcripts,
        i64::try_from(source::MAX_CLAUDE_ROOTS).unwrap(),
        "{out:?}"
    );
    assert_eq!(out.walk_complete, Some(false), "{out:?}");
    assert!(
        out.error.contains("9 more Claude config directories"),
        "{out:?}"
    );
}

#[test]
fn with_home_unset_and_userprofile_set_transcripts_are_found() {
    let home = tempfile::tempdir().unwrap();
    let repo = repo_dir();
    let repo = repo.path().to_str().unwrap();
    transcript_in(&home.path().join(".claude"), repo, "S-win");

    let serial = crate::test_support::env::env_serial();
    let mut env = crate::test_support::env::bind_env(&serial)
        .remove("HOME")
        .set("USERPROFILE", home.path())
        .set("XDG_CONFIG_HOME", home.path().join(".config"));
    for var in ROOT_VARS.iter().filter(|v| **v != "USERPROFILE") {
        env = env.remove(var);
    }
    let _env = env;
    let out = ingest_transcripts(repo);

    assert_eq!(out.recorded, 1, "{out:?}");
    assert_eq!(out.error, "", "{out:?}");
}

#[test]
fn the_explicit_root_override_still_reads_only_itself() {
    let home = tempfile::tempdir().unwrap();
    let only = tempfile::tempdir().unwrap();
    let repo = repo_dir();
    let repo = repo.path().to_str().unwrap();
    transcript_in(&home.path().join(".claude-chandra"), repo, "S-chandra");
    transcript_in(only.path(), repo, "S-only");

    let serial = crate::test_support::env::env_serial();
    let _env = crate::test_support::env::bind_env(&serial)
        .set("HOME", home.path())
        .set("GITPULSE_TRANSCRIPT_ROOT", only.path().join("projects"));
    let out = ingest_transcripts(repo);

    assert_eq!(out.transcripts, 1, "{out:?}");
    assert_eq!(out.recorded, 1, "{out:?}");
}

/// What an override that is not a directory does today, pinned so a change to
/// it is a decision: a root a person named stays as quiet as it always was.
/// The pass finds nothing to read and reports no error; only a root found by
/// looking (a config directory) is told about when it cannot be read.
#[test]
fn an_explicit_override_that_is_a_file_keeps_its_silence() {
    let home = tempfile::tempdir().unwrap();
    let repo = repo_dir();
    let repo = repo.path().to_str().unwrap();
    transcript_in(&home.path().join(".claude"), repo, "S-default");
    let file = home.path().join("not-a-directory");
    std::fs::write(&file, "x").unwrap();

    let serial = crate::test_support::env::env_serial();
    let _env = crate::test_support::env::bind_env(&serial)
        .set("HOME", home.path())
        .set("GITPULSE_TRANSCRIPT_ROOT", &file)
        .remove("CODEX_HOME")
        .remove("GITPULSE_CODEX_TRANSCRIPT_ROOT");
    let out = ingest_transcripts(repo);

    assert_eq!(
        out.recorded, 0,
        "the override replaces the config dirs: {out:?}"
    );
    assert_eq!(out.error, "", "{out:?}");
}
