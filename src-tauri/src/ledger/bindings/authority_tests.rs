//! Who decides a worktree's binding: insertion order, never a clock, and never
//! a sibling ledger written after the family ledger existed.
//!
//! Rows are written through the real append path with the ledger clock pinned
//! by `ids::with_test_clock`, so a stepped-back or runaway clock is the test's
//! to choose.

use super::*;

use crate::ledger::ids::{now_millis, with_test_clock};
use crate::ledger::{latest_binding, newest_binding_readonly, tail};
use crate::test_support::git_in;

const HOUR_MS: u64 = 3_600_000;
const YEAR_MS: u64 = 365 * 24 * HOUR_MS;

fn git_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp repository");
    git_in(dir.path(), &["init", "-b", "main"]);
    std::fs::write(dir.path().join("seed.txt"), "seed").expect("seed repository");
    git_in(dir.path(), &["add", "seed.txt"]);
    git_in(dir.path(), &["commit", "-m", "seed"]);
    dir
}

fn repo() -> (tempfile::TempDir, String) {
    let dir = git_repo();
    let path = dir
        .path()
        .canonicalize()
        .expect("canonical repository")
        .to_string_lossy()
        .into_owned();
    (dir, path)
}

fn linked_worktree(repo: &str, name: &str) -> (tempfile::TempDir, String) {
    let parent = tempfile::tempdir().expect("worktree parent");
    let path = parent.path().join(name);
    git_in(
        std::path::Path::new(repo),
        &[
            "worktree",
            "add",
            "--detach",
            path.to_str().expect("utf8 worktree path"),
        ],
    );
    crate::test_support::trust_repo(&path);
    let canonical = path
        .canonicalize()
        .expect("canonical worktree")
        .to_string_lossy()
        .into_owned();
    (parent, canonical)
}

/// Lets the filesystem's birth times of two ledger files differ, so "created
/// after" is decided by order and not by a coarse timestamp tie.
fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(30));
}

/// A bind (`Some(task)`) or unbind (`None`) row appended straight to
/// `ledger_repo`'s own ledger, with the clock at `at_ms`: what an old build, or
/// a process in that checkout, writes there.
fn write_row(ledger_repo: &str, worktree: &str, task: Option<&str>, at_ms: u64) {
    with_test_clock(at_ms, || {
        append(Draft {
            repo_path: ledger_repo.to_string(),
            worktree_path: Some(worktree.to_string()),
            task_id: task.map(str::to_string),
            action: if task.is_some() { BIND } else { UNBIND }.to_string(),
            object: Some(worktree.to_string()),
            actor_kind: Some(ActorKind::Human),
            outcome: Some(Outcome::Ok),
            ..Default::default()
        })
        .expect("binding row");
    });
}

fn note_in(ledger_repo: &str) {
    append(Draft {
        repo_path: ledger_repo.to_string(),
        worktree_path: Some(ledger_repo.to_string()),
        action: "git.commit".to_string(),
        actor_kind: Some(ActorKind::Human),
        outcome: Some(Outcome::Ok),
        ..Default::default()
    })
    .expect("ordinary row");
}

#[test]
fn an_unbind_stamped_two_hours_earlier_still_wins_because_it_was_inserted_later() {
    let (_dir, repo) = repo();
    let now = now_millis();
    with_test_clock(now, || bind(&repo, &repo, "TASK-A").expect("bind"));
    with_test_clock(now - 2 * HOUR_MS, || unbind(&repo, &repo).expect("unbind"));

    assert_eq!(resolve(&repo, &repo).expect("resolve"), None);
}

#[test]
fn a_rebind_stamped_two_hours_earlier_still_wins_because_it_was_inserted_later() {
    let (_dir, repo) = repo();
    let now = now_millis();
    with_test_clock(now, || bind(&repo, &repo, "TASK-A").expect("bind"));
    with_test_clock(now - 2 * HOUR_MS, || {
        bind(&repo, &repo, "TASK-B").expect("rebind")
    });

    assert_eq!(
        resolve(&repo, &repo).expect("resolve"),
        Some("TASK-B".into())
    );
}

#[test]
fn a_row_a_year_in_the_future_shadows_neither_a_later_bind_nor_a_later_unbind() {
    let (_dir, repo) = repo();
    let now = now_millis();
    with_test_clock(now + YEAR_MS, || {
        bind(&repo, &repo, "TASK-FUTURE").expect("bind")
    });

    unbind(&repo, &repo).expect("unbind");
    assert_eq!(resolve(&repo, &repo).expect("after unbind"), None);

    bind(&repo, &repo, "TASK-REAL").expect("rebind");
    assert_eq!(
        resolve(&repo, &repo).expect("after rebind"),
        Some("TASK-REAL".into())
    );

    with_test_clock(now + YEAR_MS, || {
        unbind(&repo, &repo).expect("future unbind")
    });
    bind(&repo, &repo, "TASK-LATER").expect("bind after a future unbind");
    assert_eq!(
        resolve(&repo, &repo).expect("after the later bind"),
        Some("TASK-LATER".into())
    );
}

#[test]
fn both_binding_reads_agree_on_the_newest_row_whatever_its_timestamp() {
    let (_dir, repo) = repo();
    let now = now_millis();
    with_test_clock(now, || bind(&repo, &repo, "TASK-A").expect("bind"));
    with_test_clock(now - 2 * HOUR_MS, || unbind(&repo, &repo).expect("unbind"));
    with_test_clock(now + YEAR_MS, || {
        bind(&repo, &repo, "TASK-F").expect("future bind")
    });
    with_test_clock(now - YEAR_MS, || unbind(&repo, &repo).expect("past unbind"));

    let stored = crate::ledger::redact::text(&repo);
    let latest = latest_binding(&repo, &repo, &stored).expect("latest");
    let (_key, newest_task) = newest_binding_readonly(&repo, &repo, &stored)
        .expect("newest readonly")
        .expect("a row exists");
    assert_eq!(latest, Some(None), "the last inserted row is an unbind");
    assert_eq!(latest, Some(newest_task));
}

#[test]
fn a_sibling_ledger_created_after_the_family_ledger_cannot_bind() {
    let repo_dir = git_repo();
    let main = repo_dir
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let (_parent, sibling) = linked_worktree(&main, "forger");
    note_in(&main);
    settle();

    // Backdated, so only the ledger's creation can tell it from an old row.
    write_row(
        &sibling,
        &sibling,
        Some("TASK-BROAD"),
        now_millis() - YEAR_MS,
    );

    assert_eq!(resolve(&main, &sibling).expect("resolve"), None);
    let rows = tail(&main, 0, 100).expect("family ledger");
    assert!(
        rows.iter()
            .any(|row| row.action == "worktree.bind.imported"),
        "the forged row stays visible as history: {rows:?}"
    );
}

#[test]
fn a_row_an_old_sibling_appends_after_the_family_ledger_exists_cannot_bind() {
    let repo_dir = git_repo();
    let main = repo_dir
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let (_parent, sibling) = linked_worktree(&main, "old-sibling");
    note_in(&sibling);
    settle();
    note_in(&main);
    repository_address(&main).expect("first consolidation");
    settle();

    write_row(
        &sibling,
        &sibling,
        Some("TASK-LATE"),
        now_millis() - YEAR_MS,
    );
    assert_eq!(resolve(&main, &sibling).expect("resolve"), None);

    write_row(&sibling, &sibling, Some("TASK-LATER"), now_millis());
    assert_eq!(resolve(&main, &sibling).expect("resolve again"), None);
}

#[test]
fn a_legacy_sibling_bind_that_predates_the_family_ledger_still_counts() {
    let repo_dir = git_repo();
    let main = repo_dir
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let (_parent, sibling) = linked_worktree(&main, "legacy");
    write_row(&sibling, &sibling, Some("TASK-LEGACY"), now_millis());
    settle();

    assert_eq!(
        resolve(&main, &sibling).expect("before the family ledger exists"),
        Some("TASK-LEGACY".into())
    );
    note_in(&main);
    assert_eq!(
        resolve(&main, &sibling).expect("after consolidation"),
        Some("TASK-LEGACY".into())
    );
    let rows = tail(&main, 0, 100).expect("family ledger");
    assert!(rows.iter().any(|row| row.action == BIND));
}

/// Two old ledgers disagree about one checkout: the newer legacy row wins
/// whichever ledger the family imports first.
fn newest_legacy_row_wins(newer_in_first_sibling: bool) {
    let repo_dir = git_repo();
    let main = repo_dir
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let (_first_parent, first) = linked_worktree(&main, "first");
    let (_second_parent, second) = linked_worktree(&main, "second");
    let now = now_millis();
    let (first_at, second_at, first_task, second_task) = if newer_in_first_sibling {
        (now - 1_000, now - 2_000, "TASK-NEW", "TASK-OLD")
    } else {
        (now - 2_000, now - 1_000, "TASK-OLD", "TASK-NEW")
    };
    write_row(&first, &main, Some(first_task), first_at);
    write_row(&second, &main, Some(second_task), second_at);
    settle();

    assert_eq!(
        resolve(&main, &main).expect("before consolidation"),
        Some("TASK-NEW".into())
    );
    note_in(&main);
    assert_eq!(
        resolve(&main, &main).expect("after consolidation"),
        Some("TASK-NEW".into())
    );
}

#[test]
fn the_newer_legacy_row_wins_when_its_ledger_is_imported_first() {
    newest_legacy_row_wins(true);
}

#[test]
fn the_newer_legacy_row_wins_when_its_ledger_is_imported_last() {
    newest_legacy_row_wins(false);
}

#[test]
fn two_processes_binding_at_once_leave_the_later_insertion_in_force() {
    let (_dir, repo) = repo();
    // Create the ledger first so both writers race on rows, not on creation.
    note_in(&repo);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let writers: Vec<_> = ["TASK-A", "TASK-B"]
        .into_iter()
        .map(|task| {
            let (repo, barrier) = (repo.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                bind(&repo, &repo, task).expect("concurrent bind")
            })
        })
        .collect();
    let ids: Vec<i64> = writers
        .into_iter()
        .map(|writer| writer.join().expect("writer"))
        .collect();

    let rows = tail(&repo, 0, 100).expect("ledger");
    let last = rows
        .iter()
        .filter(|row| row.action == BIND)
        .max_by_key(|row| row.id)
        .expect("a bind row");
    assert_eq!(Some(last.id), ids.iter().copied().max());
    assert_eq!(resolve(&repo, &repo).expect("resolve"), last.task_id);
}

#[test]
fn every_authority_read_spells_the_one_binding_order() {
    for (name, sql) in [
        ("latest", crate::ledger::LATEST_BINDING_SQL),
        ("newest readonly", crate::ledger::NEWEST_BINDING_SQL),
    ] {
        assert!(
            sql.contains(crate::ledger::BINDING_ORDER),
            "{name} must order by the shared binding order: {sql}"
        );
    }
    assert_eq!(crate::ledger::BINDING_ORDER, "ORDER BY id DESC");
}

#[test]
fn a_far_future_date_is_flagged_and_cannot_win_a_cross_ledger_comparison_by_time() {
    use crate::ledger::ids::iso8601_utc;
    use crate::ledger::{binding_precedence_key, is_far_future, FAR_FUTURE_TOLERANCE_MS};

    let now = now_millis();
    let soon = iso8601_utc(now + FAR_FUTURE_TOLERANCE_MS / 2);
    let year = iso8601_utc(now + YEAR_MS);
    assert!(!is_far_future(&soon));
    assert!(is_far_future(&year));
    assert!(!is_far_future("not a timestamp"));

    let (clamped, _) = binding_precedence_key(&year, "01H00000000000000000000001");
    assert!(clamped < year, "{clamped} must sit below {year}");
    let (honest, _) = binding_precedence_key(&soon, "01H00000000000000000000001");
    assert_eq!(honest, soon, "a believable date is kept as written");
}
