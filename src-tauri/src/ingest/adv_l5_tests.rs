//! Adversarial probes of lane L5 (corpus walk honesty, `forget`, persisted
//! passes). Tests only.

use super::*;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

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

fn do_walk(root: &std::path::Path) -> (Vec<std::path::PathBuf>, Walk) {
    let mut files = Vec::new();
    let walk = collect_jsonl_until(root, &mut files, 0, &mut || false);
    (files, walk)
}

#[cfg(unix)]
fn cursor_count(repo: &str) -> i64 {
    let db = rusqlite::Connection::open_with_flags(
        ledger::ledger_path(repo),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    db.query_row("SELECT COUNT(*) FROM transcript_cursors", [], |r| r.get(0))
        .unwrap()
}

fn seeded_repo() -> (tempfile::TempDir, String) {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path().to_str().unwrap().to_string();
    ledger::append(ledger::Draft {
        repo_path: repo.clone(),
        action: "git.commit".into(),
        actor_kind: Some(ledger::ActorKind::Human),
        outcome: Some(ledger::Outcome::Ok),
        ..Default::default()
    })
    .unwrap();
    (repo_dir, repo)
}

#[cfg(unix)]
#[test]
fn adv_walk_symlink_to_slash_terminates_and_stays_inside_the_corpus() {
    let root = tempfile::tempdir().unwrap();
    write_at(&root.path().join("slug"), "real.jsonl", "");
    std::os::unix::fs::symlink("/", root.path().join("slash")).unwrap();
    let started = std::time::Instant::now();
    let mut files = Vec::new();
    let mut stop = || started.elapsed().as_secs() > 25;
    let walk = collect_jsonl_until(root.path(), &mut files, 0, &mut stop);
    let canon = root.path().canonicalize().unwrap();
    let outside: Vec<_> = files
        .iter()
        .filter(|f| !f.canonicalize().is_ok_and(|c| c.starts_with(&canon)))
        .take(3)
        .collect();
    assert!(
        outside.is_empty() && walk.complete,
        "a symlink to / made the walk list {} files outside the corpus (e.g. {:?}); complete={} cut={} unlisted={}+{}",
        files.len(),
        outside,
        walk.complete,
        walk.cut,
        walk.unlisted_depth,
        walk.unlisted_cap
    );
}

#[cfg(unix)]
#[test]
fn adv_walk_symlink_to_an_ancestor_does_not_list_files_outside_the_corpus() {
    let top = tempfile::tempdir().unwrap();
    let corpus = top.path().join("corpus");
    write_at(&corpus.join("slug"), "real.jsonl", "");
    write_at(top.path(), "outside.jsonl", "");
    std::os::unix::fs::symlink(top.path(), corpus.join("up")).unwrap();
    let (files, walk) = do_walk(&corpus);
    let canon = corpus.canonicalize().unwrap();
    let outside: Vec<_> = files
        .iter()
        .filter(|f| !f.canonicalize().unwrap().starts_with(&canon))
        .collect();
    assert!(
        files.iter().filter(|f| f.ends_with("real.jsonl")).count() == 1,
        "{files:?}"
    );
    assert!(
        outside.is_empty(),
        "an ancestor symlink listed files outside the corpus: {outside:?} (complete={})",
        walk.complete
    );
}

#[cfg(unix)]
#[test]
fn adv_walk_two_symlinks_to_one_dir_list_each_distinct_file_once() {
    // AMENDED (item 7): the shared directory is inside the corpus. A link to a
    // directory outside it is refused, so it can no longer be the shared one.
    let root = tempfile::tempdir().unwrap();
    let shared = root.path().join("shared");
    for n in 0..3 {
        write_at(&shared, &format!("f{n}.jsonl"), "");
    }
    std::os::unix::fs::symlink(&shared, root.path().join("l1")).unwrap();
    std::os::unix::fs::symlink(&shared, root.path().join("l2")).unwrap();
    let (files, walk) = do_walk(root.path());
    assert_eq!(files.len(), 3, "{files:?}");
    assert!(walk.complete, "{walk:?}");
}

/// The dedup remembers the first name a directory was reached by, whatever
/// depth that was. A directory reached first below the depth bound (counted,
/// not listed) and again by a shallow link is never listed.
#[cfg(unix)]
#[test]
fn adv_walk_dedup_does_not_hide_a_shallow_directory_behind_a_deep_alias() {
    for (deep_name, shallow_name) in [("a-deep", "z-shallow"), ("z-deep", "a-shallow")] {
        let root = tempfile::tempdir().unwrap();
        let shared = root.path().join("slug/s1/s2/s3/s4/s5/shared");
        for n in 0..2 {
            write_at(&shared, &format!("f{n}.jsonl"), "");
        }
        // The deep alias lives at depth 6 under `deep_name`; the shallow one at depth 1.
        let deep_parent = root.path().join(deep_name).join("b/c/d/e");
        std::fs::create_dir_all(&deep_parent).unwrap();
        std::os::unix::fs::symlink(&shared, deep_parent.join("alias")).unwrap();
        std::os::unix::fs::symlink(&shared, root.path().join(shallow_name)).unwrap();
        let (files, walk) = do_walk(root.path());
        let from_shared = files
            .iter()
            .filter(|f| {
                f.to_string_lossy().contains("f0.jsonl") || f.to_string_lossy().contains("f1.jsonl")
            })
            .count();
        assert!(
            from_shared == 2,
            "[{deep_name}/{shallow_name}] files reachable at depth 1 were left unlisted: listed {from_shared}/2, \
             unlisted_depth={}, complete={}",
            walk.unlisted_depth,
            walk.complete
        );
    }
}

#[test]
fn adv_walk_hard_links_are_one_file_listed_once() {
    // AMENDED (item 8): two names for one inode are one transcript.
    let root = tempfile::tempdir().unwrap();
    let a = write_at(&root.path().join("x"), "a.jsonl", "");
    std::fs::create_dir_all(root.path().join("y")).unwrap();
    std::fs::hard_link(&a, root.path().join("y/b.jsonl")).unwrap();
    let (files, walk) = do_walk(root.path());
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(walk.complete);
}

#[test]
fn adv_walk_six_and_twelve_deep_trees_are_counted_unlisted_not_complete() {
    let root = tempfile::tempdir().unwrap();
    write_at(&root.path().join("a/b/c/d/e/f"), "six.jsonl", "");
    write_at(
        &root.path().join("a/b/c/d/e/f/g/h/i/j/k/l"),
        "twelve.jsonl",
        "",
    );
    let (files, walk) = do_walk(root.path());
    assert!(files.is_empty(), "{files:?}");
    assert_eq!(walk.unlisted_depth, 2, "{walk:?}");
    assert!(!walk.complete);
}

#[test]
fn adv_walk_exactly_5000_is_complete_and_5001_is_not_across_directories() {
    let root = tempfile::tempdir().unwrap();
    for d in 0..5 {
        for n in 0..1000 {
            let dir = root.path().join(format!("d{d}"));
            if n == 0 {
                std::fs::create_dir_all(&dir).unwrap();
            }
            std::fs::File::create(dir.join(format!("{n}.jsonl"))).unwrap();
        }
    }
    let (files, walk) = do_walk(root.path());
    assert_eq!(files.len(), 5000);
    assert!(walk.complete, "{walk:?}");
    assert_eq!(walk.unlisted_cap, 0);
    std::fs::File::create(root.path().join("d4").join("extra.jsonl")).unwrap();
    let (files, walk) = do_walk(root.path());
    assert_eq!(files.len(), 5000);
    assert!(!walk.complete);
    assert_eq!(walk.unlisted_cap, 1);
}

#[test]
fn adv_walk_unlisted_past_the_entry_budget_is_a_floor_and_the_pass_says_at_least() {
    let (_repo_dir, repo) = seeded_repo();
    let root = tempfile::tempdir().unwrap();
    let slug = root.path().join("slug");
    std::fs::create_dir_all(&slug).unwrap();
    for n in 0..(5000 + 100_001) {
        std::fs::File::create(slug.join(format!("{n}.jsonl"))).unwrap();
    }
    let (files, w) = do_walk(root.path());
    assert_eq!(files.len(), 5000);
    assert!(w.unlisted_lower_bound, "{w:?}");
    assert!(!w.complete);
    assert_eq!(w.unlisted_cap, 100_000, "{w:?}");

    let out = with_transcript_root(root.path(), || ingest_transcripts(&repo));
    assert!(out.truncated, "{out:?}");
    assert!(out.unlisted_lower_bound);
    assert_eq!(out.walk_complete, Some(false));
    assert!(out.error.contains("at least 100000"), "{}", out.error);
}

#[test]
fn adv_walk_unicode_and_very_long_names() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("日本語-ディレクトリ-🧪");
    write_at(&dir, "τράνσκριπτ-🧪.jsonl", "");
    // 255 bytes is the name limit on APFS and ext4.
    let long = format!("{}.jsonl", "x".repeat(255 - ".jsonl".len()));
    write_at(&dir, &long, "");
    let (files, walk) = do_walk(root.path());
    assert_eq!(files.len(), 2, "{files:?}");
    assert!(walk.complete);
}

#[cfg(unix)]
#[test]
fn adv_permission_denied_directory_is_incomplete_and_forgets_nothing() {
    let (_repo_dir, repo) = seeded_repo();
    let root = tempfile::tempdir().unwrap();
    let keep = write_at(
        &root.path().join("keep"),
        "k.jsonl",
        &(edit_line(&repo, "S-k", &format!("{repo}/a.rs")) + "\n"),
    );
    let locked = root.path().join("locked");
    write_at(
        &locked,
        "l.jsonl",
        &(edit_line(&repo, "S-l", &format!("{repo}/b.rs")) + "\n"),
    );
    let first = with_transcript_root(root.path(), || ingest_transcripts(&repo));
    assert_eq!(first.walk_complete, Some(true), "{first:?}");
    let before = cursor_count(&repo);
    assert!(before >= 2, "{before}");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let out = with_transcript_root(root.path(), || ingest_transcripts(&repo));
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _ = keep;
    assert_eq!(out.walk_complete, Some(false), "{out:?}");
    assert!(
        cursor_count(&repo) >= before,
        "forgot a transcript behind a denied directory"
    );
}

/// A directory that can be listed but not searched (mode 0444): `read_dir`
/// succeeds, every `stat` of its entries fails, and `Path::is_dir` reports
/// false for a failed stat.
#[cfg(unix)]
#[test]
fn adv_listable_but_unsearchable_directory_must_not_forget_what_is_below_it() {
    let (_repo_dir, repo) = seeded_repo();
    let root = tempfile::tempdir().unwrap();
    let slug = root.path().join("slug");
    write_at(
        &slug.join("session"),
        "agent-a.jsonl",
        &(edit_line(&repo, "S-a", &format!("{repo}/a.rs")) + "\n"),
    );
    let first = with_transcript_root(root.path(), || ingest_transcripts(&repo));
    assert_eq!(first.walk_complete, Some(true), "{first:?}");
    let before = cursor_count(&repo);
    assert!(before >= 1);
    std::fs::set_permissions(&slug, std::fs::Permissions::from_mode(0o444)).unwrap();
    let out = with_transcript_root(root.path(), || ingest_transcripts(&repo));
    std::fs::set_permissions(&slug, std::fs::Permissions::from_mode(0o755)).unwrap();
    let after = cursor_count(&repo);
    assert!(
        after >= before && out.walk_complete != Some(true),
        "walk_complete={:?}, cursors {before} -> {after}: a directory it could not search was walked as empty and its transcripts forgotten ({out:?})",
        out.walk_complete
    );
}

#[cfg(unix)]
#[test]
fn adv_vanished_or_dangling_transcript_keeps_its_offset_and_is_counted() {
    let (_repo_dir, repo) = seeded_repo();
    let root = tempfile::tempdir().unwrap();
    let slug = root.path().join("slug");
    let t = write_at(
        &slug,
        "t.jsonl",
        &(edit_line(&repo, "S-t", &format!("{repo}/a.rs")) + "\n"),
    );
    with_transcript_root(root.path(), || ingest_transcripts(&repo));
    let before = cursor_count(&repo);
    // Listed, then gone before it is read: a dangling link is that state.
    let held = slug.join("held.jsonl");
    std::os::unix::fs::symlink(root.path().join("nowhere.jsonl"), &held).unwrap();
    let out = with_transcript_root(root.path(), || ingest_transcripts(&repo));
    assert!(
        out.skipped_lines >= 1,
        "a file that vanished must be counted: {out:?}"
    );
    assert!(cursor_count(&repo) >= before);
    let _ = t;
}

#[test]
fn adv_hard_linked_transcript_is_not_recorded_twice() {
    let (_repo_dir, repo) = seeded_repo();
    let root = tempfile::tempdir().unwrap();
    let a = write_at(
        &root.path().join("x"),
        "a.jsonl",
        &(edit_line(&repo, "S-h", &format!("{repo}/a.rs")) + "\n"),
    );
    std::fs::create_dir_all(root.path().join("y")).unwrap();
    std::fs::hard_link(&a, root.path().join("y/b.jsonl")).unwrap();
    let out = with_transcript_root(root.path(), || ingest_transcripts(&repo));
    assert_eq!(out.recorded, 1, "{out:?}");
}

/// A pass that errored half-way (the deadline cut it, a write failed) must not
/// be persisted as a successful catch-up that makes the ledger read fresh.
#[test]
fn adv_a_pass_cut_by_the_deadline_before_reading_anything_does_not_refresh_staleness() {
    let (_repo_dir, repo) = seeded_repo();
    let mut total = CatchUp {
        truncated: true,
        error: "catch-up deadline elapsed".into(),
        ..CatchUp::default()
    };
    persist_pass(&repo, &repo, &mut total);
    let f = ledger::freshness_readonly_at(&repo, ledger::ids::now_millis() + 3_600_000);
    // An hour later the only completion is the cut pass: it must not be the
    // basis of "last catch-up" with a completion time.
    assert!(
        f.last_catch_up_basis != "catch_up_state",
        "a pass that read nothing and was cut by its deadline is persisted as a completed catch-up: {f:?}"
    );
}

#[test]
fn adv_a_pass_with_failed_writes_is_not_a_clean_completion() {
    let (_repo_dir, repo) = seeded_repo();
    let mut total = CatchUp {
        truncated: true,
        failed_records: 7,
        error: "7 record(s) could not be written".into(),
        ..CatchUp::default()
    };
    persist_pass(&repo, &repo, &mut total);
    let f = ledger::freshness_readonly_at(&repo, ledger::ids::now_millis());
    let c = f.catch_up.clone().expect("persisted");
    assert_eq!(c.failed_records, 7);
    assert!(c.truncated && !c.error.is_empty());
    assert!(
        f.last_catch_up_basis != "catch_up_state" || f.stale,
        "stale:false although the only pass failed 7 writes: {f:?}"
    );
}

// A transcript this pass could not read (stat, open, seek, read) is owed to
// the next pass, so the pass is unfinished: never stored as a completed
// catch-up and never the evidence that makes the ledger look fresh.

fn git_seeded_repo() -> (tempfile::TempDir, String) {
    let (dir, repo) = seeded_repo();
    crate::test_support::git_in(dir.path(), &["init", "-b", "main"]);
    std::fs::write(dir.path().join("seed.txt"), "seed").unwrap();
    crate::test_support::git_in(dir.path(), &["add", "seed.txt"]);
    crate::test_support::git_in(dir.path(), &["commit", "-m", "seed"]);
    (dir, repo)
}

fn good_session(root: &std::path::Path, repo: &str) {
    write_at(
        &root.join("slug"),
        "good.jsonl",
        &(edit_line(repo, "S-good", &format!("{repo}/a.rs")) + "\n"),
    );
}

fn catch_up_under(root: &std::path::Path, repo: &str) -> CatchUp {
    with_transcript_root(root, || catch_up_into(repo, repo))
}

#[cfg(unix)]
fn running_as_root() -> bool {
    let probe = tempfile::NamedTempFile::new().unwrap();
    std::fs::set_permissions(probe.path(), std::fs::Permissions::from_mode(0o000)).unwrap();
    std::fs::File::open(probe.path()).is_ok()
}

#[cfg(unix)]
#[test]
fn an_unopenable_transcript_leaves_the_pass_unfinished_and_unrecorded_as_completed() {
    if running_as_root() {
        return;
    }
    let (_repo_dir, repo) = git_seeded_repo();
    let root = tempfile::tempdir().unwrap();
    good_session(root.path(), &repo);
    let locked = write_at(
        &root.path().join("slug"),
        "locked.jsonl",
        &(edit_line(&repo, "S-locked", &format!("{repo}/b.rs")) + "\n"),
    );
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

    let out = catch_up_under(root.path(), &repo);

    assert_eq!(out.io_errors, 1, "{out:?}");
    assert!(
        out.recorded >= 1,
        "the readable file still records: {out:?}"
    );
    assert!(
        !out.finished(),
        "an open failure is an unfinished pass: {out:?}"
    );
    assert!(out.error.contains("transcript open failed"), "{out:?}");
    let f = ledger::freshness_readonly(&repo);
    assert_ne!(
        f.last_catch_up_basis, "catch_up_state",
        "a pass that could not read a transcript was stored as a completed catch-up: {f:?}"
    );

    // The error clears: the next pass reads the file and completes.
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
    let again = catch_up_under(root.path(), &repo);
    assert_eq!(again.io_errors, 0, "{again:?}");
    assert!(again.finished(), "{again:?}");
    assert!(again.recorded >= 1, "the owed file was retried: {again:?}");
    let f = ledger::freshness_readonly(&repo);
    assert_eq!(f.last_catch_up_basis, "catch_up_state", "{f:?}");
}

#[cfg(unix)]
#[test]
fn a_transcript_that_vanishes_after_listing_leaves_the_pass_unfinished() {
    let (_repo_dir, repo) = git_seeded_repo();
    let root = tempfile::tempdir().unwrap();
    good_session(root.path(), &repo);
    std::os::unix::fs::symlink(
        root.path().join("nowhere.jsonl"),
        root.path().join("slug/held.jsonl"),
    )
    .unwrap();

    let out = catch_up_under(root.path(), &repo);

    assert_eq!(out.io_errors, 1, "{out:?}");
    assert!(!out.finished(), "{out:?}");
    assert_ne!(
        ledger::freshness_readonly(&repo).last_catch_up_basis,
        "catch_up_state"
    );
}

#[test]
fn bad_utf8_still_advances_and_is_not_an_io_failure() {
    let (_repo_dir, repo) = git_seeded_repo();
    let root = tempfile::tempdir().unwrap();
    let mut body = vec![0xff, 0xfe, b'\n'];
    body.extend((edit_line(&repo, "S-u", &format!("{repo}/a.rs")) + "\n").into_bytes());
    std::fs::create_dir_all(root.path().join("slug")).unwrap();
    std::fs::write(root.path().join("slug/u.jsonl"), body).unwrap();

    let first = catch_up_under(root.path(), &repo);
    assert_eq!((first.io_errors, first.skipped_lines), (0, 1), "{first:?}");
    assert!(first.finished(), "{first:?}");
    assert_eq!(first.transcripts, 1, "{first:?}");

    let second = catch_up_under(root.path(), &repo);
    assert_eq!(
        second.transcripts, 0,
        "the cursor moved past the bad line: {second:?}"
    );
    assert!(second.finished(), "{second:?}");
}

#[test]
fn every_io_stage_makes_a_pass_unfinished_and_only_the_first_is_named() {
    for stage in ["stat", "open", "seek", "read"] {
        let mut pass = CatchUp::default();
        assert!(pass.finished());
        let err = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        pass.note_io_failure(std::path::Path::new("/t/a.jsonl"), stage, &err);
        pass.note_io_failure(std::path::Path::new("/t/b.jsonl"), stage, &err);
        assert!(!pass.finished(), "{stage}: {pass:?}");
        assert_eq!((pass.io_errors, pass.skipped_lines), (2, 2));
        assert!(pass.error.contains(&format!("transcript {stage} failed")));
        assert!(pass.error.contains("a.jsonl") && !pass.error.contains("b.jsonl"));
    }
}

// ---- adversarial review of the I/O-unfinished fix (adv:) ----

#[test]
fn adv_io_failure_is_named_even_when_the_reflog_already_reported_an_error() {
    let mut reflog = CatchUp::default();
    reflog.add_error("the reflog entries of refs/heads/x could not be written: busy".into());
    let mut transcripts = CatchUp::default();
    transcripts.note_io_failure(
        std::path::Path::new("/t/locked.jsonl"),
        "open",
        &std::io::Error::from(std::io::ErrorKind::PermissionDenied),
    );

    let total = reflog.with_transcripts(&transcripts);

    assert!(!total.finished(), "{total:?}");
    assert_eq!(total.io_errors, 1);
    assert!(
        total.error.contains("transcript open failed"),
        "the first I/O failure is not named once another error is already set: {total:?}"
    );
}

#[test]
fn adv_io_failure_plus_deadline_unlisted_truncation_is_not_finished() {
    let mut pass = CatchUp::default();
    pass.note_io_failure(
        std::path::Path::new("/t/a.jsonl"),
        "read",
        &std::io::Error::from(std::io::ErrorKind::PermissionDenied),
    );
    pass.truncated = true;
    pass.transcripts_unlisted = 2;
    pass.add_error(
        "catch-up truncated: the transcript walk left 2 file(s) unlisted and unread".into(),
    );
    assert!(!pass.is_unlisted_only_truncation(), "{pass:?}");
    assert!(!pass.finished(), "{pass:?}");
    // Order reversed: unlisted note first, then the I/O note.
    let mut other = CatchUp {
        truncated: true,
        transcripts_unlisted: 2,
        ..CatchUp::default()
    };
    other.add_error(
        "catch-up truncated: the transcript walk left 2 file(s) unlisted and unread".into(),
    );
    assert!(other.finished());
    other.note_io_failure(
        std::path::Path::new("/t/a.jsonl"),
        "stat",
        &std::io::Error::from(std::io::ErrorKind::PermissionDenied),
    );
    assert!(!other.finished(), "{other:?}");
    assert!(other.error.contains("truncated"), "{other:?}");
}

#[cfg(unix)]
#[test]
fn adv_many_dangling_transcripts_name_one_and_count_all() {
    let (_repo_dir, repo) = git_seeded_repo();
    let root = tempfile::tempdir().unwrap();
    good_session(root.path(), &repo);
    for n in 0..300 {
        std::os::unix::fs::symlink(
            root.path().join("nowhere.jsonl"),
            root.path().join(format!("slug/held{n}.jsonl")),
        )
        .unwrap();
    }
    let out = catch_up_under(root.path(), &repo);
    assert_eq!(out.io_errors, 300, "{}", out.error);
    assert!(!out.finished());
    assert!(
        out.error.len() < 2_000,
        "error grew with the failure count: {} bytes",
        out.error.len()
    );
}

#[cfg(unix)]
#[test]
fn adv_permanent_unreadable_file_is_unfinished_every_pass_with_a_stable_error() {
    if running_as_root() {
        return;
    }
    let (_repo_dir, repo) = git_seeded_repo();
    let root = tempfile::tempdir().unwrap();
    good_session(root.path(), &repo);
    let locked = write_at(
        &root.path().join("slug"),
        "locked.jsonl",
        &(edit_line(&repo, "S-locked", &format!("{repo}/b.rs")) + "\n"),
    );
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let mut lens = Vec::new();
    for _ in 0..4 {
        let out = catch_up_under(root.path(), &repo);
        assert_eq!(out.io_errors, 1, "{out:?}");
        assert!(!out.finished());
        lens.push(out.error.len());
    }
    assert!(
        lens.windows(2).all(|w| w[0] == w[1]),
        "error drifts: {lens:?}"
    );
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
}

#[cfg(unix)]
#[test]
fn adv_a_directory_named_jsonl_is_not_a_transcript_and_not_an_io_failure() {
    let (_repo_dir, repo) = git_seeded_repo();
    let root = tempfile::tempdir().unwrap();
    good_session(root.path(), &repo);
    std::fs::create_dir_all(root.path().join("slug/dir.jsonl")).unwrap();
    let out = catch_up_under(root.path(), &repo);
    assert_eq!(out.io_errors, 0, "{out:?}");
    assert!(out.finished(), "{out:?}");
}

#[cfg(unix)]
fn failed_after_good_pass() -> (
    tempfile::TempDir,
    String,
    tempfile::TempDir,
    std::path::PathBuf,
    String,
) {
    let (repo_dir, repo) = git_seeded_repo();
    let root = tempfile::tempdir().unwrap();
    let path = write_at(
        &root.path().join("slug"),
        "s.jsonl",
        &(edit_line(&repo, "S-a", &format!("{repo}/a.rs")) + "\n"),
    );
    let first = catch_up_under(root.path(), &repo);
    assert!(first.finished(), "{first:?}");
    let good_at = ledger::freshness_readonly(&repo).last_catch_up.unwrap();
    let mut body = std::fs::read_to_string(&path).unwrap();
    body.push_str(&(edit_line(&repo, "S-a", &format!("{repo}/c.rs")) + "\n"));
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let failed = catch_up_under(root.path(), &repo);
    assert_eq!(failed.io_errors, 1, "{failed:?}");
    (repo_dir, repo, root, path, good_at)
}

#[cfg(unix)]
#[test]
fn adv_freshness_after_io_failure_goes_stale_at_the_window_and_basis_is_the_good_pass() {
    if running_as_root() {
        return;
    }
    let (_d, repo, _r, path, good_at) = failed_after_good_pass();
    let after = ledger::freshness_readonly(&repo);
    assert_eq!(
        after.last_catch_up.as_deref(),
        Some(good_at.as_str()),
        "a failed pass advanced last_catch_up: {after:?}"
    );
    assert_eq!(after.last_catch_up_basis, "catch_up_state");
    let at = ledger::ids::parse_event_time(&good_at).unwrap();
    let window = ledger::STALE_AFTER_SECS * 1000;
    for (offset, stale) in [
        (window - 1000, false),
        (window, false),
        (window + 1000, true),
    ] {
        let f = ledger::freshness_readonly_at(&repo, at + offset);
        assert_eq!(f.stale, stale, "offset {offset}: {f:?}");
    }
    let inside = ledger::freshness_readonly_at(&repo, at + window - 1000);
    assert!(
        inside
            .catch_up
            .as_ref()
            .is_some_and(|c| c.error.contains("transcript open failed")),
        "the persisted row does not say why: {inside:?}"
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
}

#[cfg(unix)]
#[test]
fn adv_stale_reason_names_the_unreadable_transcript_that_made_the_pass_unfinished() {
    if running_as_root() {
        return;
    }
    let (_d, repo, _r, path, good_at) = failed_after_good_pass();
    let at = ledger::ids::parse_event_time(&good_at).unwrap();
    let window = ledger::STALE_AFTER_SECS * 1000;
    let f = ledger::freshness_readonly_at(&repo, at + window + 1000);
    assert!(f.stale, "{f:?}");
    let explained = f.stale_reason.contains("transcript")
        || f.catch_up
            .as_ref()
            .is_some_and(|c| c.error.contains("transcript open failed"));
    assert!(
        explained,
        "stale with no hint that the last pass failed on an unreadable transcript: {f:?}"
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn adv_line_reader_returns_consumed_lines_before_a_read_error_and_repeats_the_error() {
    struct Flaky(std::io::Cursor<Vec<u8>>, bool);
    impl std::io::Read for Flaky {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.0.read(buf)?;
            if n == 0 {
                self.1 = true;
                return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
            }
            Ok(n)
        }
    }
    let data = b"one\ntwo\npart".to_vec();
    let span = data.len() as u64 + 10;
    let mut reader = lines::LineReader::with_limits(
        Flaky(std::io::Cursor::new(data), false),
        0,
        span,
        1024,
        1024,
    );
    let mut consumed = 0;
    for _ in 0..2 {
        consumed += reader.next_line().unwrap().unwrap().len;
    }
    assert_eq!(consumed, 8);
    assert!(reader.next_line().is_err());
}
