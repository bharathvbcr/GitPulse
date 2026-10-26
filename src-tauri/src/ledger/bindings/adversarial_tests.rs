//! Adversarial review of worktree binding authority (lane W3-L4).
//!
//! Each test names the attack. A test that asserts a *bad* outcome is marked
//! `BROKEN:` in its name so a reader can tell a held attack from a landed one.

#![allow(clippy::disallowed_methods)]

use super::*;

use crate::ledger::ids::{iso8601_utc, now_millis, with_test_clock};
use crate::ledger::{tail, FAR_FUTURE_TOLERANCE_MS};
use crate::test_support::git_in;

const HOUR_MS: u64 = 3_600_000;
const YEAR_MS: u64 = 365 * 24 * HOUR_MS;

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

/// The family's initial consolidation pass: the epoch after which a sibling
/// ledger is no longer legacy. The forgery tests below run it before the
/// forger exists, which is what separates an attacker from an upgrade.
fn initial_pass(main: &str) {
    repository_address(main).expect("initial consolidation pass");
}

#[cfg(unix)]
fn ledger_file(repo: &str) -> std::path::PathBuf {
    crate::ledger::ledger_path(repo)
}

// ---------------------------------------------------------------- attack 1

/// Attack 1. A worktree whose binding demotion turns into "unbound" is not
/// fenced by the file-tool hook: `check_file_in_scope` answers `Unbound` and
/// never asks the harness. The same worktree bound to a task is `Judged`.
/// So demotion of a legacy binding removes the scope fence (widens access).
#[test]
fn widening_demotion_of_a_legacy_binding_turns_a_fenced_worktree_into_an_unfenced_one() {
    let (_dir, main) = canonical_repo();
    note_in(&main);
    settle();
    let (_p, sibling) = linked(&main, "agent-a");

    // Legacy upgrade: the old build wrote this worktree's bind in the
    // worktree's own ledger. Before the app consolidates, the hook fails
    // closed (it cannot rule the binding out).
    write_row(&sibling, &sibling, Some("TASK-A"), now_millis());
    settle();
    let before = crate::harness::check_file_in_scope(&sibling, "src/anything.rs", "modify");
    assert!(
        matches!(before, crate::harness::FileScopeCheck::Judged(_)),
        "pre-consolidation hook must be fenced: {before:?}"
    );

    let resolved = resolve(&main, &sibling).expect("resolve");
    let after = crate::harness::check_file_in_scope(&sibling, "src/anything.rs", "modify");
    eprintln!("RESOLVED={resolved:?} AFTER={after:?}");

    // Control: the same worktree bound on the anchor is fenced.
    let (_p2, control) = linked(&main, "agent-control");
    bind(&main, &control, "TASK-A").expect("bind control");
    let fenced = crate::harness::check_file_in_scope(&control, "src/anything.rs", "modify");
    assert!(matches!(fenced, crate::harness::FileScopeCheck::Judged(_)));

    // BROKEN if the legacy binding no longer resolves AND the hook is open.
    if resolved.is_none() {
        assert!(
            matches!(after, crate::harness::FileScopeCheck::Judged(_)),
            "BROKEN: demotion made the worktree unbound; the write hook no longer fences it: {after:?}"
        );
    }
}

// ---------------------------------------------------------------- attack 2

/// Attack 2. A realistic legacy family: main ledger first, sibling ledgers
/// later, each sibling bound in its own ledger. Counts how many resolve.
#[test]
fn upgrade_family_quantified_binds_in_later_born_siblings() {
    let (_dir, main) = canonical_repo();
    note_in(&main);
    settle();
    let mut keep = Vec::new();
    let mut siblings = Vec::new();
    for i in 0..5 {
        let (p, s) = linked(&main, &format!("agent-{i}"));
        keep.push(p);
        siblings.push(s);
    }
    for (i, s) in siblings.iter().enumerate() {
        write_row(s, s, Some(&format!("TASK-{i}")), now_millis());
        settle();
    }
    let resolved = siblings
        .iter()
        .filter(|s| resolve(&main, s).expect("resolve").is_some())
        .count();
    eprintln!("LEGACY_BINDS_STILL_RESOLVING={resolved}/5");
    let imported = tail(&main, 0, 1000)
        .unwrap()
        .iter()
        .filter(|r| r.action == "worktree.bind.imported")
        .count();
    eprintln!("IMPORTED_AS_HISTORY={imported}");
    assert_eq!(
        resolved,
        5,
        "BROKEN (upgrade): {} of 5 legacy sibling bindings silently stopped resolving",
        5 - resolved
    );
}

// ---------------------------------------------------------------- attack 3

#[cfg(target_os = "macos")]
fn backdate_birth(path: &std::path::Path, stamp: &str) {
    // On macOS, setting an mtime earlier than the birth time lowers it too.
    let status = std::process::Command::new("touch")
        .args(["-t", stamp])
        .arg(path)
        .status()
        .expect("touch");
    assert!(status.success());
}

#[cfg(target_os = "macos")]
fn birth_ms(path: &std::path::Path) -> u64 {
    std::fs::metadata(path)
        .unwrap()
        .created()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// Attack 3a. A same-user process creates a sibling ledger after the anchor,
/// backdates its birth time with `touch -t`, and plants a backdated bind.
#[cfg(target_os = "macos")]
#[test]
fn forgery_birthtime_rewind_with_touch_makes_a_post_anchor_sibling_row_authoritative() {
    let (_dir, main) = canonical_repo();
    note_in(&main);
    initial_pass(&main);
    settle();
    let (_p, sibling) = linked(&main, "forger");
    write_row(
        &sibling,
        &sibling,
        Some("TASK-BROAD"),
        now_millis() - YEAR_MS,
    );
    // WAL: checkpoint so the main db file carries the data, then rewind.
    let db = ledger_file(&sibling);
    backdate_birth(&db, "200001010000");
    eprintln!(
        "sibling birth={} anchor birth={}",
        birth_ms(&db),
        birth_ms(&ledger_file(&main))
    );
    let got = resolve(&main, &sibling).expect("resolve");
    assert_eq!(
        got, None,
        "BROKEN: forged sibling row (birthtime rewound by touch -t) is authoritative: {got:?}"
    );
}

/// Attack 3a'. Same, but with `SetFile -d`.
#[cfg(target_os = "macos")]
#[test]
fn forgery_birthtime_rewind_with_setfile_makes_a_post_anchor_sibling_row_authoritative() {
    let (_dir, main) = canonical_repo();
    note_in(&main);
    initial_pass(&main);
    settle();
    let (_p, sibling) = linked(&main, "forger2");
    write_row(
        &sibling,
        &sibling,
        Some("TASK-BROAD"),
        now_millis() - YEAR_MS,
    );
    let db = ledger_file(&sibling);
    let status = std::process::Command::new("/usr/bin/SetFile")
        .args(["-d", "01/01/2000 00:00:00"])
        .arg(&db)
        .status();
    eprintln!("SetFile status={status:?}");
    let got = resolve(&main, &sibling).expect("resolve");
    assert_eq!(
        got, None,
        "BROKEN: forged row (birthtime rewound by SetFile) is authoritative: {got:?}"
    );
}

/// Attack 3b. A post-anchor sibling file replaced by `mv` of an old file: the
/// moved inode keeps its birth. A same-user attacker copies an *old* ledger
/// (e.g. another checkout's) over the sibling path with `mv`.
#[cfg(unix)]
#[test]
fn forgery_mv_of_an_old_ledger_file_into_a_new_sibling_keeps_its_old_birth() {
    let (_dir, main) = canonical_repo();
    let scratch = tempfile::tempdir().unwrap();
    // An old, attacker-prepared ledger created BEFORE the anchor.
    let (_p, sibling) = linked(&main, "mv-forger");
    let staging = scratch.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    let staging_repo = staging.to_string_lossy().into_owned();
    // A directory that is not a git repo still gets a ledger file.
    write_row(
        &staging_repo,
        &sibling,
        Some("TASK-BROAD"),
        now_millis() - YEAR_MS,
    );
    settle();
    note_in(&main);
    initial_pass(&main);
    settle();
    let dest = ledger_file(&sibling);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    for ext in ["", "-wal", "-shm"] {
        let from =
            std::path::PathBuf::from(format!("{}{ext}", ledger_file(&staging_repo).display()));
        let to = std::path::PathBuf::from(format!("{}{ext}", dest.display()));
        if from.exists() {
            std::fs::rename(&from, &to).unwrap();
        }
    }
    let got = resolve(&main, &sibling).expect("resolve");
    assert_eq!(
        got, None,
        "BROKEN: an old ledger file mv'd into place after the anchor binds (birth is preserved by rename): {got:?}"
    );
}

/// Attack 3c. A row with ts before the anchor creation inserted later into a
/// sibling that was ALREADY imported once.
#[test]
fn forgery_backdated_row_in_an_already_imported_sibling_is_history_only() {
    let (_dir, main) = canonical_repo();
    let (_p, sibling) = linked(&main, "late-row");
    write_row(
        &sibling,
        &main,
        Some("TASK-OLD"),
        now_millis() - 2 * HOUR_MS,
    );
    settle();
    note_in(&main);
    assert_eq!(resolve(&main, &main).unwrap(), Some("TASK-OLD".into()));
    write_row(
        &sibling,
        &main,
        Some("TASK-FORGED"),
        now_millis() - 3 * HOUR_MS,
    );
    assert_eq!(resolve(&main, &main).unwrap(), Some("TASK-OLD".into()));
}

/// Attack 3d. Crash case: sibling ledger created after the anchor but the
/// anchor has no import marker for it (cursor 0) because no consolidation ran.
/// Birth time must still disqualify it.
#[test]
fn forgery_first_import_after_a_crash_of_a_post_anchor_sibling_is_history_only() {
    let (_dir, main) = canonical_repo();
    note_in(&main);
    initial_pass(&main);
    settle();
    let (_p, sibling) = linked(&main, "crash");
    write_row(&sibling, &main, Some("TASK-CRASH"), now_millis() - YEAR_MS);
    // No consolidation has ever seen it: cursor 0 on first sight.
    assert_eq!(resolve(&main, &main).unwrap(), None);
}

/// Attack 3e. Two siblings both create rows pre-anchor for the same worktree
/// and race their first import from several threads: the newer key must win
/// deterministically every time.
#[test]
fn forgery_two_siblings_racing_first_import_resolve_deterministically() {
    for round in 0..6 {
        let (_dir, main) = canonical_repo();
        let (_p1, a) = linked(&main, "race-a");
        let (_p2, b) = linked(&main, "race-b");
        let now = now_millis();
        write_row(&a, &main, Some("TASK-NEW"), now - 1_000);
        write_row(&b, &main, Some("TASK-OLD"), now - 5_000);
        settle();
        note_in(&main);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let hs: Vec<_> = (0..4)
            .map(|_| {
                let (m, bar) = (main.clone(), barrier.clone());
                std::thread::spawn(move || {
                    bar.wait();
                    resolve(&m, &m)
                })
            })
            .collect();
        for h in hs {
            let _ = h.join().unwrap();
        }
        assert_eq!(
            resolve(&main, &main).unwrap(),
            Some("TASK-NEW".into()),
            "round {round}"
        );
    }
}

// ---------------------------------------------------------------- attack 4

/// Attack 4. Horizon edges with the clock pinned.
#[test]
fn clock_row_exactly_at_the_horizon_plus_or_minus_one_ms() {
    use crate::ledger::{binding_precedence_key, is_far_future};
    let now = now_millis();
    with_test_clock(now, || {
        let at = iso8601_utc(now + FAR_FUTURE_TOLERANCE_MS);
        let over = iso8601_utc(now + FAR_FUTURE_TOLERANCE_MS + 1);
        let under = iso8601_utc(now + FAR_FUTURE_TOLERANCE_MS - 1);
        assert!(!is_far_future(&at));
        assert!(!is_far_future(&under));
        assert!(is_far_future(&over));
        let k_at = binding_precedence_key(&at, "A");
        let k_over = binding_precedence_key(&over, "A");
        let k_under = binding_precedence_key(&under, "A");
        assert_eq!(k_at, k_over, "clamped row ties with the horizon row");
        assert!(k_under < k_at);
    });
}

/// Attack 4b. A far-future legacy row in force makes a later, honest legacy
/// row from another sibling lose, even though both are pre-anchor.
#[test]
fn clock_a_far_future_legacy_row_cannot_outrank_an_honest_newer_legacy_row() {
    let (_dir, main) = canonical_repo();
    let (_p1, a) = linked(&main, "future");
    let (_p2, b) = linked(&main, "honest");
    let now = now_millis();
    // Far-future row would be rejected by the anchor-creation check anyway
    // (ts > anchor creation), so it becomes history: the honest one wins.
    write_row(&a, &main, Some("TASK-FUTURE"), now + YEAR_MS);
    write_row(&b, &main, Some("TASK-HONEST"), now - 1_000);
    settle();
    note_in(&main);
    assert_eq!(resolve(&main, &main).unwrap(), Some("TASK-HONEST".into()));
}

/// Attack 4c. Clock stepped back below a legacy row when the app binds, then a
/// not-yet-consolidated legacy sibling arrives: a deliberate newer rebind must
/// not be overridden by an older legacy row.
#[test]
fn clock_backward_step_app_rebind_is_not_overridden_by_a_later_arriving_legacy_row() {
    let (_dir, main) = canonical_repo();
    let (_p1, legacy) = linked(&main, "legacy-late");
    let t = now_millis();
    write_row(&legacy, &main, Some("TASK-LEGACY"), t - 10_000);
    settle();
    note_in(&main);
    // The anchor exists; the legacy sibling has been imported already by the
    // first consolidation above. Use a second legacy sibling discovered later.
    let (_p2, legacy2) = linked(&main, "legacy-late-2");
    // Its file is created AFTER the anchor, so make it old via touch.
    write_row(&legacy2, &main, Some("TASK-LEGACY-2"), t - 5_000);
    // The app rebinds with its clock stepped back before the legacy rows.
    with_test_clock(t - 60_000, || {
        bind(&main, &main, "TASK-APP").expect("bind");
    });
    assert_eq!(resolve(&main, &main).unwrap(), Some("TASK-APP".into()));
}

// ---------------------------------------------------------------- attack 5

/// Attack 5. Live enforcement vs `binding_as_of`/history under a stepped-back
/// clock: documents the divergence and where each is read.
#[test]
fn consistency_enforcement_and_as_of_history_can_name_different_tasks() {
    let (_dir, main) = canonical_repo();
    with_test_clock(now_millis() + HOUR_MS, || {
        bind(&main, &main, "TASK-A").expect("bind a")
    });
    bind(&main, &main, "TASK-B").expect("bind b");
    let live = resolve(&main, &main).unwrap();
    let address = repository_address(&main).unwrap();
    let as_of = binding_as_of(&address, now_millis() + 2 * HOUR_MS).unwrap();
    eprintln!("LIVE={live:?} AS_OF={as_of:?}");
    assert_eq!(
        live.as_deref(),
        as_of.task_id(),
        "DIVERGES: enforcement uses {live:?}, as-of/history says {as_of:?}"
    );
}

// ---------------------------------------------------------------- attack 6

/// Attack 6. Bind, unbind and consolidation interleaved across 4 threads; the
/// final answer equals the last inserted bind/unbind row.
#[test]
fn concurrency_interleaved_bind_unbind_and_consolidation_end_at_the_last_insertion() {
    for round in 0..4 {
        let (_dir, main) = canonical_repo();
        let (_p, sibling) = linked(&main, "noise");
        write_row(&sibling, &main, Some("TASK-LEGACY"), now_millis() - HOUR_MS);
        settle();
        note_in(&main);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let hs: Vec<_> = (0..4)
            .map(|t| {
                let (m, bar) = (main.clone(), barrier.clone());
                std::thread::spawn(move || {
                    bar.wait();
                    for i in 0..15 {
                        match (t + i) % 3 {
                            0 => {
                                bind(&m, &m, &format!("TASK-{t}-{i}")).expect("bind");
                            }
                            1 => {
                                unbind(&m, &m).expect("unbind");
                            }
                            _ => {
                                resolve(&m, &m).expect("resolve");
                            }
                        }
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        let rows = tail(&main, 0, 10_000).unwrap();
        let last = rows
            .iter()
            .filter(|r| r.action == BIND || r.action == UNBIND)
            .max_by_key(|r| r.id)
            .unwrap();
        let expected = if last.action == BIND {
            last.task_id.clone()
        } else {
            None
        };
        assert_eq!(resolve(&main, &main).unwrap(), expected, "round {round}");
    }
}
