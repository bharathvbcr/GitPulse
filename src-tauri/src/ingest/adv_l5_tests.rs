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
