//! The corpus walk's honesty: what it reads, what it leaves out and says so,
//! and when it may forget a transcript's offset.

use super::*;

fn with_transcript_root<T>(root: &std::path::Path, f: impl FnOnce() -> T) -> T {
    let serial = crate::test_support::env::env_serial();
    let _env = crate::test_support::env::bind_env(&serial).set("GITPULSE_TRANSCRIPT_ROOT", root);
    f()
}

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

fn write_at(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

fn walk(root: &std::path::Path) -> (Vec<std::path::PathBuf>, Walk) {
    let mut files = Vec::new();
    let walk = collect_jsonl_until(root, &mut files, 0, &mut || false);
    (files, walk)
}

fn cursor_count(repo: &str) -> i64 {
    let db = rusqlite::Connection::open_with_flags(
        ledger::ledger_path(repo),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    db.query_row("SELECT COUNT(*) FROM transcript_cursors", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn the_depth_five_workflow_shape_is_read() {
    let root = tempfile::tempdir().unwrap();
    let deep = root.path().join("slug/session/subagents/workflows/wf_1");
    write_at(&deep, "agent-a.jsonl", "");

    let (files, walk) = walk(root.path());

    assert_eq!(
        files.len(),
        1,
        "slug/session/subagents/workflows/wf_*/agent-*.jsonl"
    );
    assert!(walk.complete, "{walk:?}");
    assert_eq!(walk.unlisted_depth, 0);
}

#[test]
fn the_depth_five_shape_records_an_edit_through_a_pass() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path().to_str().unwrap();
    let root = tempfile::tempdir().unwrap();
    let deep = root.path().join("slug/session/subagents/workflows/wf_1");
    write_at(
        &deep,
        "agent-a.jsonl",
        &(edit_line(repo, "S-deep", &format!("{repo}/a.rs")) + "\n"),
    );

    let out = with_transcript_root(root.path(), || ingest_transcripts(repo));

    assert_eq!(out.recorded, 1, "{out:?}");
    assert_eq!(out.walk_complete, Some(true));
    assert_eq!(out.transcripts_unlisted, 0);
}

#[test]
fn a_directory_below_the_depth_bound_is_counted_not_called_complete() {
    let root = tempfile::tempdir().unwrap();
    let too_deep = root.path().join("a/b/c/d/e/f");
    write_at(&too_deep, "x.jsonl", "");
    write_at(&too_deep, "y.jsonl", "");
    write_at(&root.path().join("a"), "ok.jsonl", "");

    let (files, walk) = walk(root.path());

    assert_eq!(files.len(), 1, "only the shallow file is listed");
    assert!(!walk.complete, "files below the bound were not listed");
    assert_eq!(walk.unlisted_depth, 2);
    assert_eq!(walk.unlisted_cap, 0);
}

#[test]
fn an_empty_directory_below_the_bound_loses_nothing() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("a/b/c/d/e/f/g")).unwrap();
    write_at(&root.path().join("a"), "ok.jsonl", "");

    let (files, walk) = walk(root.path());

    assert_eq!(files.len(), 1);
    assert!(walk.complete, "nothing was left unlisted: {walk:?}");
}

#[test]
fn the_5001st_file_is_counted_as_unlisted_by_the_cap() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("slug");
    std::fs::create_dir_all(&dir).unwrap();
    for n in 0..5001 {
        std::fs::File::create(dir.join(format!("{n}.jsonl"))).unwrap();
    }

    let (files, walk) = walk(root.path());

    assert_eq!(files.len(), 5000);
    assert!(!walk.complete);
    assert_eq!(walk.unlisted_cap, 1);
    assert_eq!(walk.unlisted_depth, 0);
    assert!(!walk.unlisted_lower_bound);
}

#[test]
fn a_pass_over_5001_files_says_truncated_with_both_counts_and_forgets_nothing() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path().to_str().unwrap();
    ledger::append(ledger::Draft {
        repo_path: repo.to_string(),
        action: "git.commit".into(),
        actor_kind: Some(ledger::ActorKind::Human),
        outcome: Some(ledger::Outcome::Ok),
        ..Default::default()
    })
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let slug = root.path().join("slug");
    // A transcript read now, then deleted: its offset is the thing that must
    // survive an incomplete walk.
    let vanishing = write_at(
        &slug,
        "vanishing.jsonl",
        &(edit_line(repo, "S-gone", &format!("{repo}/a.rs")) + "\n"),
    );
    let first = with_transcript_root(root.path(), || ingest_transcripts(repo));
    assert_eq!(first.walk_complete, Some(true), "{first:?}");
    assert_eq!(cursor_count(repo), 1);
    std::fs::remove_file(&vanishing).unwrap();
    for n in 0..5001 {
        std::fs::File::create(slug.join(format!("{n}.jsonl"))).unwrap();
    }

    let out = with_transcript_root(root.path(), || ingest_transcripts(repo));

    assert!(out.truncated, "{out:?}");
    assert_eq!(out.walk_complete, Some(false));
    assert_eq!(out.transcripts_listed, 5000);
    assert_eq!(out.transcripts_unlisted, 1);
    assert_eq!(out.unlisted_cap, 1);
    assert_eq!(out.unlisted_depth, 0);
    assert!(
        out.error.contains("1 file(s) unlisted") && out.error.contains("past the file cap"),
        "{}",
        out.error
    );
    assert!(
        cursor_count(repo) >= 1,
        "an incomplete walk forgot the offset of a transcript it never confirmed gone"
    );
}

#[test]
fn a_complete_walk_still_forgets_a_transcript_that_is_gone() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path().to_str().unwrap();
    ledger::append(ledger::Draft {
        repo_path: repo.to_string(),
        action: "git.commit".into(),
        actor_kind: Some(ledger::ActorKind::Human),
        outcome: Some(ledger::Outcome::Ok),
        ..Default::default()
    })
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let gone = write_at(
        &root.path().join("slug"),
        "gone.jsonl",
        &(edit_line(repo, "S-gone", &format!("{repo}/a.rs")) + "\n"),
    );
    write_at(&root.path().join("slug"), "kept.jsonl", "");
    with_transcript_root(root.path(), || ingest_transcripts(repo));
    let before = cursor_count(repo);
    std::fs::remove_file(&gone).unwrap();

    let out = with_transcript_root(root.path(), || ingest_transcripts(repo));

    assert_eq!(out.walk_complete, Some(true), "{out:?}");
    assert!(!out.truncated);
    assert!(
        cursor_count(repo) < before,
        "the control: a complete walk must still forget ({before} cursors before)"
    );
}

#[cfg(unix)]
#[test]
fn a_symlink_loop_terminates_and_lists_each_file_once() {
    let root = tempfile::tempdir().unwrap();
    let slug = root.path().join("slug");
    write_at(&slug, "a.jsonl", "");
    // A directory that is a symlink to itself, to its parent, and to the root.
    std::os::unix::fs::symlink(&slug, slug.join("self")).unwrap();
    std::os::unix::fs::symlink(root.path(), slug.join("up")).unwrap();
    std::os::unix::fs::symlink(root.path(), root.path().join("again")).unwrap();

    let (files, walk) = walk(root.path());

    assert_eq!(files.len(), 1, "{files:?}: the loop re-listed a file");
    assert!(walk.complete, "a loop revisits listed files; it hides none");
    assert_eq!(walk.unlisted_depth + walk.unlisted_cap, 0);
}

#[cfg(unix)]
#[test]
fn a_symlink_loop_in_the_depth_five_shape_does_not_multiply_files() {
    let root = tempfile::tempdir().unwrap();
    let wf = root.path().join("slug/session/subagents/workflows/wf_1");
    write_at(&wf, "agent-a.jsonl", "");
    std::os::unix::fs::symlink(root.path().join("slug"), wf.join("loop")).unwrap();

    let (files, walk) = walk(root.path());

    assert_eq!(files.len(), 1, "{files:?}");
    assert!(walk.complete);
}

#[test]
fn unicode_transcript_names_are_listed() {
    let root = tempfile::tempdir().unwrap();
    write_at(&root.path().join("slug-日本語"), "séssion-日本語.jsonl", "");

    let (files, walk) = walk(root.path());

    assert_eq!(files.len(), 1, "{files:?}");
    assert!(walk.complete);
}

/// Linux only: macOS filesystems refuse a name that is not valid UTF-8.
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_transcript_names_are_listed() {
    use std::os::unix::ffi::OsStrExt;
    let root = tempfile::tempdir().unwrap();
    let slug = root.path().join("slug");
    write_at(&slug, "plain.jsonl", "");
    let raw = std::ffi::OsStr::from_bytes(b"bad-\xff\xfe.jsonl");
    std::fs::write(slug.join(raw), "").unwrap();
    let odd_dir = root.path().join(std::ffi::OsStr::from_bytes(b"dir-\xff"));
    std::fs::create_dir_all(&odd_dir).unwrap();
    std::fs::write(odd_dir.join("inside.jsonl"), "").unwrap();

    let (files, walk) = walk(root.path());

    assert_eq!(files.len(), 3, "{files:?}");
    assert!(walk.complete);
}

#[test]
fn a_zero_byte_transcript_is_listed_and_read_as_nothing() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path().to_str().unwrap();
    let root = tempfile::tempdir().unwrap();
    write_at(&root.path().join("slug"), "empty.jsonl", "");

    let out = with_transcript_root(root.path(), || ingest_transcripts(repo));

    assert_eq!(out.transcripts_listed, 1);
    assert_eq!(out.skipped_lines, 0, "an empty file has no unreadable line");
    assert_eq!(out.walk_complete, Some(true));
    assert!(!out.truncated);
}

#[test]
fn a_stopped_walk_is_cut_and_not_complete() {
    let root = tempfile::tempdir().unwrap();
    write_at(&root.path().join("slug"), "a.jsonl", "");
    let mut files = Vec::new();

    let walk = collect_jsonl_until(root.path(), &mut files, 0, &mut || true);

    assert!(walk.cut);
    assert!(!walk.complete);
}

#[test]
fn a_catch_up_persists_what_it_covered_for_a_reader_in_another_process() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path().to_str().unwrap();
    crate::test_support::git_in(repo_dir.path(), &["init", "-b", "main"]);
    std::fs::write(repo_dir.path().join("seed.txt"), "seed").unwrap();
    crate::test_support::git_in(repo_dir.path(), &["add", "seed.txt"]);
    crate::test_support::git_in(repo_dir.path(), &["commit", "-m", "seed"]);
    ledger::append(ledger::Draft {
        repo_path: repo.to_string(),
        action: "git.commit".into(),
        actor_kind: Some(ledger::ActorKind::Human),
        outcome: Some(ledger::Outcome::Ok),
        ..Default::default()
    })
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    // One unreadable line: it must reach the reader as a number.
    write_at(
        &root.path().join("slug"),
        "bad.jsonl",
        "{\"type\":\"assistant\", this is not json}\n",
    );
    write_at(&root.path().join("a/b/c/d/e/f"), "deep.jsonl", "");

    let out = with_transcript_root(root.path(), || catch_up_into_bounded(repo, repo, None));

    assert!(out.truncated, "{out:?}");
    let read = ledger::freshness_readonly(repo);
    assert_eq!(read.last_catch_up_basis, "catch_up_state", "{}", out.error);
    assert!(!read.stale, "{read:?}");
    let covered = read.catch_up.expect("the pass left its outcome");
    assert!(covered.truncated);
    assert_eq!(covered.transcripts_unlisted, 1);
    assert_eq!(covered.skipped_lines, out.skipped_lines);
    assert!(covered.skipped_lines > 0, "{covered:?}");
}
