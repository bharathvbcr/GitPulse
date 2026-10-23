//! Round-2 adversarial review of worktree binding authority (lane W3-L4, B+).
//!
//! A test that asserts the SAFE outcome fails when the attack lands; its
//! message starts `BROKEN:`.

#![allow(clippy::disallowed_methods)]

use super::*;

use crate::harness::{check_file_in_scope, FileScopeCheck};
use crate::ledger::ids::{now_millis, with_test_clock};
use crate::ledger::tail;
use crate::test_support::git_in;

const HOUR_MS: u64 = 3_600_000;

fn canonical_repo() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("temp repository");
    git_in(dir.path(), &["init", "-b", "main"]);
    std::fs::write(dir.path().join("seed.txt"), "seed").expect("seed");
    git_in(dir.path(), &["add", "seed.txt"]);
    git_in(dir.path(), &["commit", "-m", "seed"]);
    let path = dir
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    (dir, path)
}

fn linked(repo: &str, name: &str) -> (tempfile::TempDir, String) {
    let parent = tempfile::tempdir().expect("worktree parent");
    let path = parent.path().join(name);
    git_in(
        std::path::Path::new(repo),
        &["worktree", "add", "--detach", path.to_str().unwrap()],
    );
    crate::test_support::trust_repo(&path);
    let canonical = path.canonicalize().unwrap().to_string_lossy().into_owned();
    (parent, canonical)
}

fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(30));
}

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

fn raw(repo: &str) -> rusqlite::Connection {
    rusqlite::Connection::open(crate::ledger::ledger_path(repo)).expect("raw ledger")
}

fn fenced(worktree: &str) -> bool {
    matches!(
        check_file_in_scope(worktree, "src/anything.rs", "modify"),
        FileScopeCheck::Judged(_)
    )
}

/// Attack 1. Sibling ledger of ~2500 rows (3 pages): an old bind on row 1 and
/// the legacy binding in force on the LAST row. The import fails (a row that
/// cannot be imported) at the start of / inside page k, so pages before k are
/// committed; the row is then repaired and consolidation rerun. Pre-change code
/// resolves TASK-LAST at the end.
fn crash_then_recover(poison_id: i64) {
    crash_then_recover_with(poison_id, true);
}

/// Attack 1c. The ONLY legacy bind sits on the last page; a crash at a page
/// boundary then recovery leaves the worktree with no binding at all (unfenced).
#[test]
fn crash_at_page_two_start_with_only_a_late_bind_keeps_the_binding_and_the_fence() {
    crash_then_recover_with(1001, false);
}

fn crash_then_recover_with(poison_id: i64, early_bind: bool) {
    let (_dir, main) = canonical_repo();
    note_in(&main);
    settle();
    let (_p, sibling) = linked(&main, "legacy");
    if early_bind {
        write_row(&sibling, &sibling, Some("TASK-OLD"), now_millis());
    } else {
        note_in(&sibling);
    }
    for _ in 0..2498 {
        note_in(&sibling);
    }
    write_row(&sibling, &sibling, Some("TASK-LAST"), now_millis());
    let original: String = raw(&sibling)
        .query_row(
            "SELECT ts_utc FROM events WHERE id = ?1",
            [poison_id],
            |r| r.get(0),
        )
        .expect("row to poison");
    raw(&sibling)
        .execute(
            "UPDATE events SET ts_utc = 'garbage' WHERE id = ?1",
            [poison_id],
        )
        .expect("poison");

    let crashed = resolve(&main, &sibling);
    assert!(
        crashed.is_err(),
        "the import must fail mid-pass: {crashed:?}"
    );
    assert!(
        fenced(&sibling),
        "hook must fail closed while the pass is cut short"
    );

    raw(&sibling)
        .execute(
            "UPDATE events SET ts_utc = ?2 WHERE id = ?1",
            rusqlite::params![poison_id, original],
        )
        .expect("repair");
    let resolved = resolve(&main, &sibling).expect("resolve after recovery");
    let hook = fenced(&sibling);
    assert_eq!(
        resolved,
        Some("TASK-LAST".into()),
        "BROKEN: a legacy binding stopped resolving after crash recovery (poison id {poison_id}); hook fenced={hook}"
    );
    assert!(hook, "BROKEN: hook is open after recovery");
}

#[test]
fn crash_at_page_two_start_keeps_the_legacy_binding() {
    crash_then_recover(1001);
}

#[test]
fn crash_mid_page_two_keeps_the_legacy_binding() {
    crash_then_recover(1500);
}

#[test]
fn crash_at_page_three_start_keeps_the_legacy_binding() {
    crash_then_recover(2001);
}

/// Attack 1b. Crash between siblings: A imports completely, B fails at its
/// first row. Whichever order they run in, after recovery both must resolve.
#[test]
fn crash_between_siblings_keeps_both_legacy_bindings() {
    for poisoned in ["a", "b"] {
        let (_dir, main) = canonical_repo();
        note_in(&main);
        settle();
        let (_pa, a) = linked(&main, "sib-a");
        let (_pb, b) = linked(&main, "sib-b");
        write_row(&a, &a, Some("TASK-A"), now_millis());
        write_row(&b, &b, Some("TASK-B"), now_millis());
        let target = if poisoned == "a" { &a } else { &b };
        raw(target)
            .execute("UPDATE events SET ts_utc = 'garbage' WHERE id = 1", [])
            .expect("poison");
        assert!(resolve(&main, &a).is_err() || resolve(&main, &b).is_err());
        let ts = iso(now_millis());
        raw(target)
            .execute("UPDATE events SET ts_utc = ?1 WHERE id = 1", [ts])
            .expect("repair");
        let (ra, rb) = (resolve(&main, &a).unwrap(), resolve(&main, &b).unwrap());
        assert_eq!(
            (ra.clone(), rb.clone()),
            (Some("TASK-A".into()), Some("TASK-B".into())),
            "BROKEN: poisoned={poisoned} resolved a={ra:?} b={rb:?}"
        );
    }
}

fn iso(ms: u64) -> String {
    crate::ledger::ids::iso8601_utc(ms)
}

/// Attack 2. Realistic legacy family; all five resolve and are fenced.
#[test]
fn upgrade_five_siblings_all_resolve_and_stay_fenced() {
    let (_dir, main) = canonical_repo();
    note_in(&main);
    settle();
    let mut keep = Vec::new();
    let mut sibs = Vec::new();
    for i in 0..5 {
        let (p, s) = linked(&main, &format!("agent-{i}"));
        write_row(&s, &s, Some(&format!("TASK-{i}")), now_millis());
        settle();
        keep.push(p);
        sibs.push(s);
    }
    for s in &sibs {
        assert!(fenced(s), "pre-consolidation hook must fail closed");
    }
    for (i, s) in sibs.iter().enumerate() {
        assert_eq!(resolve(&main, s).unwrap(), Some(format!("TASK-{i}")));
    }
    for (i, s) in sibs.iter().enumerate() {
        assert!(
            fenced(s),
            "BROKEN: sibling {i} hook open after consolidation"
        );
        let direct = resolve_binding_readonly(s, s);
        assert_eq!(
            direct.unwrap().map(|b| b.task_id),
            Some(format!("TASK-{i}")),
            "readonly (hook) resolution"
        );
    }
}

/// Attack 3. After the initial pass completes: new sibling, backdated bind;
/// row appended to an imported sibling; mv of an old ledger into a new sibling
/// path. All history-only.
#[test]
fn post_initial_pass_forgeries_are_history_only() {
    let (_dir, main) = canonical_repo();
    note_in(&main);
    settle();
    let (_p0, old) = linked(&main, "old");
    write_row(&old, &old, Some("TASK-OLD"), now_millis() - 5 * HOUR_MS);
    assert_eq!(resolve(&main, &old).unwrap(), Some("TASK-OLD".into()));

    // a new sibling with a bind backdated before the anchor
    settle();
    let (_p1, fresh) = linked(&main, "fresh");
    write_row(
        &fresh,
        &fresh,
        Some("TASK-FORGED"),
        now_millis() - 9 * HOUR_MS,
    );
    assert_eq!(
        resolve(&main, &fresh).unwrap(),
        None,
        "BROKEN: new sibling bound"
    );

    // a row appended to the imported sibling: an unbind of the legacy binding
    write_row(&old, &old, None, now_millis() - 9 * HOUR_MS);
    assert_eq!(
        resolve(&main, &old).unwrap(),
        Some("TASK-OLD".into()),
        "BROKEN: appended row to imported sibling took effect"
    );
    // ... and a bind for an unrelated worktree
    write_row(&old, &fresh, Some("TASK-APPENDED"), now_millis() + 1);
    assert_eq!(
        resolve(&main, &fresh).unwrap(),
        None,
        "BROKEN: appended bind"
    );

    // mv of an old ledger file into a new sibling path
    let (_p2, moved) = linked(&main, "moved");
    let src = crate::ledger::ledger_path(&old);
    let dst = crate::ledger::ledger_path(&moved);
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    for ext in ["", "-wal", "-shm"] {
        let (s, d) = (
            std::path::PathBuf::from(format!("{}{ext}", src.display())),
            std::path::PathBuf::from(format!("{}{ext}", dst.display())),
        );
        if s.exists() {
            std::fs::copy(&s, &d).unwrap();
        }
    }
    write_row(&moved, &moved, Some("TASK-MV"), now_millis());
    assert_eq!(
        resolve(&main, &moved).unwrap(),
        None,
        "BROKEN: mv-ed ledger bound"
    );
}

/// Attack 3 residual. The marker lives in the anchor. Deleting the epoch row, or
/// resetting it to cursor 0, reopens the window: with no row no pass has
/// finished, which is what an upgrade from a build that kept none looks like. Both
/// need a write to the anchor itself, which is already full power.
#[test]
fn marker_delete_and_reset_to_zero_reopen_the_window_like_an_upgrade() {
    let (_dir, main) = canonical_repo();
    note_in(&main);
    settle();
    let (_p0, old) = linked(&main, "old");
    write_row(&old, &old, Some("TASK-OLD"), now_millis());
    resolve(&main, &old).unwrap();
    let rows: Vec<(String, i64)> = raw(&main)
        .prepare("SELECT source_key, source_cursor FROM ledger_family_imports")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    eprintln!("MARKERS={rows:?}");

    raw(&main)
        .execute(
            "DELETE FROM ledger_family_imports WHERE source_key = 'family-initial-pass'",
            [],
        )
        .unwrap();
    settle();
    let (_p1, f1) = linked(&main, "forger-delete");
    write_row(&f1, &f1, Some("TASK-F1"), now_millis());
    assert_eq!(
        resolve(&main, &f1).unwrap(),
        Some("TASK-F1".into()),
        "a deleted marker reads as an upgrade: the sibling is legacy"
    );

    raw(&main)
        .execute(
            "INSERT OR REPLACE INTO ledger_family_imports (source_key, source_label, source_cursor) VALUES ('family-initial-pass','x',0)",
            [],
        )
        .unwrap();
    let (_p2, f2) = linked(&main, "forger-reset");
    write_row(&f2, &f2, Some("TASK-F2"), now_millis());
    let reopened = resolve(&main, &f2).unwrap();
    eprintln!("RESET_REOPENED={}", reopened.is_some());
}

/// Attack 3 (read-only open). A read-only status/hook must never write the
/// marker; a non-writable anchor must not record.
#[test]
fn readonly_paths_never_write_the_marker() {
    let (_dir, main) = canonical_repo();
    note_in(&main);
    let before: i64 = raw(&main)
        .query_row("SELECT COUNT(*) FROM ledger_family_imports", [], |r| {
            r.get(0)
        })
        .unwrap();
    let (_p, s) = linked(&main, "s");
    let _ = repository_status_readonly(&main);
    let _ = resolve_binding_readonly(&s, &s);
    let after: i64 = raw(&main)
        .query_row("SELECT COUNT(*) FROM ledger_family_imports", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(before, after, "BROKEN: a read wrote the marker");
}

/// Attack 4a. Fresh anchor, zero siblings ever: a consolidation must close the
/// window. Records the marker state after the first pass.
#[test]
fn fresh_anchor_zero_siblings_first_pass_closes_window() {
    let (_dir, main) = canonical_repo();
    note_in(&main);
    repository_address(&main).unwrap();
    let marker: Option<i64> = raw(&main)
        .query_row(
            "SELECT source_cursor FROM ledger_family_imports WHERE source_key='family-initial-pass'",
            [],
            |r| r.get(0),
        )
        .ok();
    assert_eq!(
        marker,
        Some(1),
        "BROKEN: zero-sibling pass left window open: {marker:?}"
    );
}

/// Attack 4b. Fresh install where the anchor is created by the FIRST bind
/// (consolidation ran before the anchor existed, so no marker). A forger
/// creates a sibling ledger before the next consolidation.
#[test]
fn fresh_install_bind_then_forged_sibling_before_next_consolidation() {
    let (_dir, main) = canonical_repo();
    let (_p, sib) = linked(&main, "wt");
    bind(&main, &sib, "TASK-REAL").expect("first bind creates the anchor");
    // forger: a brand new sibling ledger that unbinds the worktree
    settle();
    write_row(&sib, &sib, None, now_millis() + HOUR_MS);
    let resolved = resolve(&main, &sib).unwrap();
    assert_eq!(
        resolved,
        Some("TASK-REAL".into()),
        "BROKEN: sibling unbind accepted as legacy on a fresh install (window open until 2nd consolidation): {resolved:?}"
    );
}

/// Attack 4c. Anchor created by a plain append (hook/ingest) before any
/// consolidation; forger sibling appears before the first consolidation.
#[test]
fn fresh_install_anchor_by_append_then_forged_sibling() {
    let (_dir, main) = canonical_repo();
    let (_p, sib) = linked(&main, "wt");
    note_in(&main);
    write_row(&sib, &sib, Some("TASK-FORGED"), now_millis());
    let resolved = resolve(&main, &sib).unwrap();
    eprintln!("FRESH_APPEND_FORGED={resolved:?}");
}

/// Upgrade from a build that kept no epoch: sources partially imported, a
/// sibling's legacy bind not yet imported. B+ reads the markers as Settled.
#[test]
fn old_build_partially_imported_family_demotes_a_pending_legacy_bind() {
    let (_dir, main) = canonical_repo();
    note_in(&main);
    settle();
    let (_pa, a) = linked(&main, "a");
    write_row(&a, &a, Some("TASK-A"), now_millis());
    resolve(&main, &a).unwrap(); // old build imported A
                                 // old build: no epoch row
    raw(&main)
        .execute(
            "DELETE FROM ledger_family_imports WHERE source_key = 'family-initial-pass'",
            [],
        )
        .unwrap();
    settle();
    let (_pb, b) = linked(&main, "b");
    // legacy bind written by the old build before upgrade, never imported
    write_row(&b, &b, Some("TASK-B"), now_millis());
    let resolved = resolve(&main, &b).unwrap();
    assert_eq!(
        resolved,
        Some("TASK-B".into()),
        "BROKEN: legacy bind of a sibling the old build had not imported yet is demoted on upgrade; fenced={}",
        fenced(&b)
    );
}

/// Attack 5. Reach of the demotion notice.
#[test]
fn demotion_notice_reach() {
    let (_dir, main) = canonical_repo();
    note_in(&main);
    settle();
    let (_p0, old) = linked(&main, "old");
    write_row(&old, &old, Some("TASK-OLD"), now_millis());
    resolve(&main, &old).unwrap();
    settle();
    let (_p1, late) = linked(&main, "late");
    write_row(&late, &late, Some("TASK-LATE"), now_millis());
    assert_eq!(resolve(&main, &late).unwrap(), None);
    let demoted = tail(&main, 0, 1000)
        .unwrap()
        .iter()
        .filter(|r| r.action.ends_with(".imported"))
        .count();
    let writable = repository_status(&main).unwrap();
    let readonly = repository_status_readonly(&main).unwrap();
    let view = &readonly;
    eprintln!("DEMOTED_ROWS_IN_EVENTS={demoted}");
    eprintln!("WRITABLE_STATUS code={:?}", writable.error_code);
    eprintln!("READONLY_STATUS code={:?}", readonly.error_code);
    eprintln!(
        "MCP_VIEW code={:?}",
        format!("{view:?}").contains("binding_rows_history_only")
    );
    assert_eq!(
        readonly.error_code, HISTORY_ONLY_BINDINGS_CODE,
        "BROKEN(honesty): read-only status hides the demotion"
    );
}
