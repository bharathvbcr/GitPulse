//! `binding_as_of` and `BindingHistory`: what held for a worktree at a moment.
//!
//! Rows are written straight into the table so their `ts_utc` and `ulid` are
//! the test's to choose; `bind` and `unbind` stamp the append clock.

use super::*;

use crate::ledger::ids::{iso8601_utc, parse_event_time};
use crate::ledger::{latest_binding, with_conn};
use crate::sysload::{cpu_since, if_clock, try_thread_cpu};

/// Year 9999: later than any row a test writes, standing in for "now".
const FOREVER_MS: u64 = 253_402_300_799_999;

fn repo() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("temp repository");
    let path = dir.path().canonicalize().expect("canonical path");
    crate::test_support::git_in(&path, &["init", "-b", "main"]);
    std::fs::write(path.join("seed.txt"), "seed").expect("seed repository");
    crate::test_support::git_in(&path, &["add", "seed.txt"]);
    crate::test_support::git_in(&path, &["commit", "-m", "seed"]);
    let text = path.to_string_lossy().into_owned();
    (dir, text)
}

fn address(repo: &str) -> BindingAddress {
    BindingAddress {
        anchor: repo.to_string(),
        worktree: repo.to_string(),
    }
}

/// One bind (`Some(task)`) or unbind (`None`) of the repository itself.
fn put(repo: &str, ms: u64, ulid: &str, task: Option<&str>) {
    with_conn(repo, |conn| {
        conn.execute(
            "INSERT INTO events (ulid, ts_utc, repo_path, worktree_path, actor_kind, task_id,
                                 action, outcome)
             VALUES (?1, ?2, ?3, ?3, 'human', ?4, ?5, 'ok')",
            rusqlite::params![
                ulid,
                iso8601_utc(ms),
                repo,
                task,
                if task.is_some() { BIND } else { UNBIND },
            ],
        )
        .map_err(|e| LedgerError::new("insert_failed", e.to_string()))?;
        Ok(())
    })
    .expect("insert a binding row");
}

fn ulid(n: usize) -> String {
    format!("01H{n:023}")
}

fn bound(task: &str) -> TaskAtTime {
    TaskAtTime::Bound(task.to_string())
}

const T0: u64 = 1_790_000_000_000;

#[test]
fn a_moment_resolves_to_the_newest_row_at_or_before_it() {
    let (_dir, repo) = repo();
    put(&repo, T0 + 1_000, &ulid(1), Some("T1"));
    put(&repo, T0 + 3_000, &ulid(2), None);
    put(&repo, T0 + 5_000, &ulid(3), Some("T2"));
    let history = BindingHistory::load(&repo, &repo).unwrap();

    for (at, expected) in [
        (T0, TaskAtTime::Unresolved),
        (T0 + 999, TaskAtTime::Unresolved),
        (T0 + 1_000, bound("T1")),
        (T0 + 2_999, bound("T1")),
        (T0 + 3_000, TaskAtTime::Unbound),
        (T0 + 4_999, TaskAtTime::Unbound),
        (T0 + 5_000, bound("T2")),
        (FOREVER_MS, bound("T2")),
    ] {
        assert_eq!(history.as_of(at), expected, "history at +{}", at - T0);
        assert_eq!(
            binding_as_of(&address(&repo), at).unwrap(),
            expected,
            "query at +{}",
            at - T0
        );
    }
}

#[test]
fn rows_in_one_millisecond_go_to_the_last_inserted_whatever_their_ulids() {
    let (_dir, repo) = repo();
    // The ulids are out of order against the insertions on purpose.
    put(&repo, T0, &ulid(2), Some("LATER"));
    put(&repo, T0, &ulid(3), None);
    put(&repo, T0, &ulid(1), Some("LAST-INSERTED"));
    let history = BindingHistory::load(&repo, &repo).unwrap();
    assert_eq!(history.as_of(T0), bound("LAST-INSERTED"));
    assert_eq!(
        binding_as_of(&address(&repo), T0).unwrap(),
        bound("LAST-INSERTED")
    );
    assert_eq!(history.as_of(T0 - 1), TaskAtTime::Unresolved);
}

#[test]
fn a_second_with_a_row_in_it_is_undecidable() {
    let (_dir, repo) = repo();
    let second = T0;
    put(&repo, second - 1_500, &ulid(1), Some("BEFORE"));
    put(&repo, second + 500, &ulid(2), Some("INSIDE"));
    put(&repo, second + 2_000, &ulid(3), Some("AFTER"));
    let history = BindingHistory::load(&repo, &repo).unwrap();

    // The row at +500 may be before or after the event somewhere in the second.
    assert_eq!(history.as_of_second(second), TaskAtTime::Unresolved);
    // The seconds on either side have nothing inside them.
    assert_eq!(history.as_of_second(second - 1_000), bound("BEFORE"));
    assert_eq!(history.as_of_second(second + 1_000), bound("INSIDE"));
    assert_eq!(history.as_of_second(second + 2_000), TaskAtTime::Unresolved);
}

#[test]
fn a_row_at_either_edge_of_the_second_is_inside_it() {
    let (_dir, repo) = repo();
    put(&repo, T0, &ulid(1), Some("AT-START"));
    put(&repo, T0 + 3_999, &ulid(2), None);
    let history = BindingHistory::load(&repo, &repo).unwrap();
    assert_eq!(history.as_of_second(T0), TaskAtTime::Unresolved);
    assert_eq!(history.as_of_second(T0 + 3_000), TaskAtTime::Unresolved);
    assert_eq!(history.as_of_second(T0 + 1_000), bound("AT-START"));
}

/// The oracle: the row inserted last among those stamped at or before `at`, by
/// brute force (`rows` is in insertion order).
fn oracle(rows: &[(u64, String, Option<String>)], at: u64) -> TaskAtTime {
    match rows.iter().rev().find(|(ms, _, _)| *ms <= at) {
        None => TaskAtTime::Unresolved,
        Some((_, _, Some(task))) => TaskAtTime::Bound(task.clone()),
        Some((_, _, None)) => TaskAtTime::Unbound,
    }
}

#[test]
fn as_of_agrees_with_the_oracle_and_with_the_live_read_at_the_end() {
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move |bound: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % bound
    };
    for round in 0..12 {
        let (_dir, repo) = repo();
        let mut rows = Vec::new();
        let mut ms = T0;
        for n in 0..(1 + next(14) as usize) {
            // Gaps of 0 put several rows in one millisecond.
            ms += next(3);
            let task = (next(3) > 0).then(|| format!("T{}", next(4)));
            let ulid = ulid(1 + (round * 100 + n) * 7 % 997 + next(5) as usize * 1000);
            if rows.iter().any(|(_, existing, _)| *existing == ulid) {
                continue;
            }
            put(&repo, ms, &ulid, task.as_deref());
            rows.push((ms, ulid, task));
        }
        let history = BindingHistory::load(&repo, &repo).unwrap();
        for at in T0 - 1..=ms + 2 {
            let expected = oracle(&rows, at);
            assert_eq!(history.as_of(at), expected, "round {round}, +{}", at - T0);
            assert_eq!(
                binding_as_of(&address(&repo), at).unwrap(),
                expected,
                "round {round}, query +{}",
                at - T0
            );
        }
        // After the last row, "as of" is "now": the live gate's answer, the row
        // written last.
        let live = latest_binding(&repo, &repo, &crate::ledger::redact::text(&repo)).unwrap();
        let end = binding_as_of(&address(&repo), FOREVER_MS).unwrap();
        let last_written = rows.last().map(|(_, _, task)| task.clone());
        assert_eq!(
            live, last_written,
            "round {round}: the live read is insertion order"
        );
        assert_eq!(
            end == TaskAtTime::Unresolved,
            live.is_none(),
            "round {round}"
        );
        assert_eq!(end, oracle(&rows, FOREVER_MS), "round {round}");
        assert_eq!(history.as_of(FOREVER_MS), end, "round {round}");
    }
}

#[test]
fn the_real_clock_agrees_with_the_history() {
    let (_dir, repo) = repo();
    bind(&repo, &repo, "T1").unwrap();
    let live = binding_at(&address(&repo)).unwrap().map(|b| b.task_id);
    let now = crate::ledger::ids::now_millis();
    assert_eq!(
        binding_as_of(&address(&repo), now).unwrap().task_id(),
        live.as_deref()
    );
    assert_eq!(
        BindingHistory::load(&repo, &repo).unwrap().as_of(now),
        bound("T1")
    );
}

#[test]
fn a_repository_with_no_ledger_decides_nothing_and_is_not_given_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().canonicalize().unwrap();
    let repo = path.to_string_lossy().into_owned();
    assert_eq!(
        binding_as_of(&address(&repo), T0).unwrap(),
        TaskAtTime::Unresolved
    );
    assert_eq!(
        BindingHistory::load(&repo, &repo).unwrap().as_of(T0),
        TaskAtTime::Unresolved
    );
    assert!(!crate::ledger::is_initialised(&repo));
}

#[test]
fn a_history_that_could_not_be_read_decides_nothing() {
    let history = BindingHistory::unreadable();
    assert_eq!(history.as_of(FOREVER_MS), TaskAtTime::Unresolved);
    assert_eq!(history.as_of_second(T0), TaskAtTime::Unresolved);
    assert_eq!(TaskAtTime::Unresolved.basis(), "unresolved");
    assert_eq!(TaskAtTime::Unbound.basis(), "unbound");
    assert_eq!(bound("T").basis(), "bound");
}

#[test]
fn a_foreign_row_refuses_the_history_as_it_refuses_the_live_read() {
    let (_dir, repo) = repo();
    put(&repo, T0, &ulid(1), Some("T1"));
    with_conn(&repo, |conn| {
        conn.execute(
            "UPDATE events SET repo_path = '/somewhere/else' WHERE ulid = ?1",
            [ulid(1)],
        )
        .unwrap();
        Ok(())
    })
    .unwrap();
    assert_eq!(
        binding_as_of(&address(&repo), T0).unwrap_err().code,
        "repository_mismatch"
    );
    assert_eq!(
        BindingHistory::load(&repo, &repo).unwrap_err().code,
        "repository_mismatch"
    );
}

#[test]
fn both_reads_stay_on_the_binding_index_and_share_one_order() {
    let (_dir, repo) = repo();
    bind(&repo, &repo, "T1").unwrap();
    let explain = |sql: &str, params: &[&dyn rusqlite::ToSql]| {
        with_conn(&repo, |conn| {
            let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
            let rows = stmt
                .query_map(params, |row| row.get::<_, String>(3))
                .unwrap();
            Ok(rows.map(Result::unwrap).collect::<Vec<_>>().join(" | "))
        })
        .unwrap()
    };
    let as_of = explain(crate::ledger::BINDING_AS_OF_SQL, &[&"a", &"b", &"c"]);
    assert!(as_of.contains("idx_events_binding"), "{as_of}");
    let history = explain(crate::ledger::BINDING_HISTORY_SQL, &[&"a", &"b"]);
    assert!(history.contains("idx_events_binding"), "{history}");

    // One precedence order: the live read, the as-of read and the history all
    // sort by the shared binding order (insertion), newest first.
    for sql in [
        crate::ledger::LATEST_BINDING_SQL,
        crate::ledger::BINDING_AS_OF_SQL,
        crate::ledger::BINDING_HISTORY_SQL,
    ] {
        assert!(sql.contains(crate::ledger::BINDING_ORDER), "{sql}");
    }
}

#[test]
fn an_offset_call_time_resolves_at_its_utc_instant() {
    let (_dir, repo) = repo();
    // Bound at 12:00:00Z; a call spelled 13:30 +02:00 is 11:30Z, before it.
    let bound_at = parse_event_time("2026-09-01T12:00:00Z").unwrap();
    put(&repo, bound_at, &ulid(1), Some("T1"));
    let history = BindingHistory::load(&repo, &repo).unwrap();
    let before = parse_event_time("2026-09-01T13:30:00+02:00").unwrap();
    let after = parse_event_time("2026-09-01T14:30:00+02:00").unwrap();
    assert_eq!(history.as_of(before), TaskAtTime::Unresolved);
    assert_eq!(history.as_of(after), bound("T1"));
}

/// A bind row naming this worktree's path but written under another repository
/// identity is not this repository's binding. The history read refuses a ledger
/// holding one, so a pass stamps `unresolved` and no task rather than using it.
#[test]
fn a_bind_row_under_another_repository_identity_is_never_used() {
    let (_dir, repo) = repo();
    bind(&repo, &repo, "T-OTHER").unwrap();
    with_conn(&repo, |conn| {
        conn.execute(
            "UPDATE events SET repo_path = '/some/other/repository' WHERE action = 'worktree.bind'",
            [],
        )
        .unwrap();
        Ok(())
    })
    .unwrap();
    assert_eq!(
        BindingHistory::load(&repo, &repo).unwrap_err().code,
        "repository_mismatch"
    );

    let corpus = tempfile::tempdir().unwrap();
    let slug = corpus.path().join("slug");
    std::fs::create_dir_all(&slug).unwrap();
    let file = format!("{repo}/a.rs");
    let call = serde_json::json!({
        "type": "assistant",
        "sessionId": "ID",
        "timestamp": iso8601_utc(crate::ledger::ids::now_millis()),
        "version": "2.1.241",
        "cwd": repo,
        "message": { "content": [
            { "type": "tool_use", "name": "Edit", "input": { "file_path": file } }
        ]}
    })
    .to_string();
    std::fs::write(slug.join("ID.jsonl"), call + "\n").unwrap();
    let serial = crate::test_support::env::env_serial();
    let _env =
        crate::test_support::env::bind_env(&serial).set("GITPULSE_TRANSCRIPT_ROOT", corpus.path());
    let out = crate::ingest::ingest_transcripts_into(&repo, &repo);
    assert!(!out.error.is_empty(), "the refusal is reported: {out:?}");

    // The pass fails closed on a ledger holding another repository's rows: it
    // records nothing, so nothing is filed under the other identity's task.
    assert_eq!(out.recorded, 0, "{out:?}");
    let used: i64 = with_conn(&repo, |conn| {
        Ok(conn
            .query_row(
                "SELECT COUNT(*) FROM events
                 WHERE task_id = 'T-OTHER' AND action NOT LIKE 'worktree.%'",
                [],
                |row| row.get(0),
            )
            .unwrap())
    })
    .unwrap();
    assert_eq!(used, 0, "the other identity's task was used");
}

/// The history read is for replayed events. The live gate and the hook still
/// authenticate the worktree through git: a checkout that is not a registered
/// member of the repository's family is refused, not answered from its rows.
#[test]
fn the_live_gate_and_the_hook_still_authenticate_through_git() {
    let (dir, repo) = repo();
    bind(&repo, &repo, "T1").unwrap();
    let impostor = tempfile::tempdir().unwrap();
    std::fs::write(
        impostor.path().join(".git"),
        format!("gitdir: {}\n", dir.path().join(".git").display()),
    )
    .unwrap();
    crate::test_support::trust_repo(impostor.path());
    let forged = impostor.path().canonicalize().unwrap();
    let forged = forged.to_string_lossy().into_owned();

    for error in [
        resolve_binding(&repo, &forged).expect_err("the gate's resolution"),
        resolve_binding_readonly(&repo, &forged).expect_err("the hook's resolution"),
    ] {
        assert_eq!(error.code, "unrelated_worktree", "{error}");
    }
    // And the registered worktree still resolves through the same path.
    assert_eq!(resolve(&repo, &repo).unwrap(), Some("T1".into()));
}

// ---- adversarial cases -------------------------------------------------

#[test]
fn a_bind_and_an_unbind_in_one_millisecond_resolve_by_insertion_either_way() {
    // The ulids order the two rows both ways; the unbind is inserted last.
    for (bind_ulid, unbind_ulid) in [(1, 2), (2, 1)] {
        let (_dir, repo) = repo();
        put(&repo, T0, &ulid(bind_ulid), Some("T"));
        put(&repo, T0, &ulid(unbind_ulid), None);
        let history = BindingHistory::load(&repo, &repo).unwrap();
        assert_eq!(history.as_of(T0), TaskAtTime::Unbound);
        assert_eq!(
            binding_as_of(&address(&repo), T0).unwrap(),
            TaskAtTime::Unbound
        );
        assert_eq!(history.as_of(T0 - 1), TaskAtTime::Unresolved);
        assert_eq!(history.as_of(T0 + 1), TaskAtTime::Unbound);
        // The live read, the as-of read and the history agree.
        let live = latest_binding(&repo, &repo, &repo).unwrap();
        assert_eq!(live, Some(None), "the unbind was inserted last");
    }
}

#[test]
fn the_reflog_second_next_to_a_row_resolves_exactly_at_the_edges() {
    let (_dir, repo) = repo();
    put(&repo, T0 - 1, &ulid(1), Some("PREV-SECOND"));
    put(&repo, T0 + 1_000, &ulid(2), Some("NEXT-SECOND"));
    let history = BindingHistory::load(&repo, &repo).unwrap();
    // A row 1 ms before the second began is decided; one at the next second's
    // first millisecond is outside this second.
    assert_eq!(history.as_of_second(T0), bound("PREV-SECOND"));
    assert_eq!(history.as_of_second(T0 + 1_000), TaskAtTime::Unresolved);
    assert_eq!(history.as_of_second(T0 + 2_000), bound("NEXT-SECOND"));
    // The far end of the range must not panic (the answer is not asserted).
    let _ = history.as_of_second(u64::MAX);
    let _ = history.as_of(u64::MAX);
}

/// A clock that stepped backwards between two binds: the later append carries
/// the earlier time. Both the history and the live gate follow insertion, so the
/// later bind holds from the moment its row is stamped and the history never
/// names a task the gate would not have enforced.
#[test]
fn a_clock_that_went_backwards_orders_history_and_the_live_read_by_insertion() {
    let (_dir, repo) = repo();
    put(&repo, T0 + 10_000, &ulid(1), Some("APPENDED-FIRST"));
    put(&repo, T0, &ulid(2), Some("APPENDED-SECOND"));
    let history = BindingHistory::load(&repo, &repo).unwrap();
    assert_eq!(history.as_of(T0 - 1), TaskAtTime::Unresolved);
    assert_eq!(history.as_of(T0 + 5_000), bound("APPENDED-SECOND"));
    assert_eq!(history.as_of(T0 + 10_000), bound("APPENDED-SECOND"));
    assert_eq!(history.as_of(FOREVER_MS), bound("APPENDED-SECOND"));
    assert_eq!(
        latest_binding(&repo, &repo, &repo).unwrap(),
        Some(Some("APPENDED-SECOND".to_string()))
    );
}

#[test]
fn ten_thousand_binds_load_and_answer_within_a_pass_budget() {
    let (_dir, repo) = repo();
    with_conn(&repo, |conn| {
        let tx = conn.unchecked_transaction().unwrap();
        for n in 0..10_000usize {
            let task = (n % 3 != 2).then(|| format!("T{n}"));
            tx.execute(
                "INSERT INTO events (ulid, ts_utc, repo_path, worktree_path, actor_kind, task_id,
                                     action, outcome)
                 VALUES (?1, ?2, ?3, ?3, 'human', ?4, ?5, 'ok')",
                rusqlite::params![
                    ulid(n),
                    iso8601_utc(T0 + n as u64 * 10),
                    &repo,
                    task,
                    if task.is_some() { BIND } else { UNBIND },
                ],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        Ok(())
    })
    .unwrap();
    // The load and the lookups run, and their answer is checked, with or
    // without a clock; only the CPU bound below needs one.
    let started = try_thread_cpu();
    let history = BindingHistory::load(&repo, &repo).unwrap();
    let load = started.map(cpu_since);
    let mut resolved = 0u64;
    for call in 0..1_000_000u64 {
        let at = T0 + (call * 7_919) % 100_000;
        resolved += u64::from(history.as_of(at).task_id().is_some());
    }
    let ask = started
        .map(cpu_since)
        .zip(load)
        .map(|(total, load)| total - load);
    eprintln!("10k binds: load {load:?}, 1M as_of {ask:?}, resolved {resolved}");
    assert_eq!(history.as_of(T0 + 9_999 * 10 + 5), bound("T9999"));
    // CPU time this thread spent, not the wall clock: the budget is about the
    // work (a pass over 10k rows, a million lookups), and a loaded host
    // stretches wall time by the queue the thread waits in without changing
    // that work. Still scaled, because instrumentation does change the work.
    if_clock(
        "ten_thousand_binds_load_and_answer_within_a_pass_budget",
        load.zip(ask),
        |(load, ask)| {
            assert!(
                load < crate::test_support::coverage_relaxed(std::time::Duration::from_secs(2)),
                "load {load:?}"
            );
            assert!(
                ask < crate::test_support::coverage_relaxed(std::time::Duration::from_secs(5)),
                "1M as_of took {ask:?}"
            );
        },
    );
}

/// Without a thread CPU clock the 10k-row test still loads, looks up and checks
/// the answer, and skips only its CPU bound.
#[test]
fn ten_thousand_binds_skips_its_cpu_bound_where_there_is_no_thread_cpu_clock() {
    crate::sysload::no_clock::assert_skips(
        "ten_thousand_binds_load_and_answer_within_a_pass_budget",
        ten_thousand_binds_load_and_answer_within_a_pass_budget,
    );
}

/// A bind row naming no task is an unbind to every reader; an unbind as the
/// newest row decides "unbound", not "unresolved".
#[test]
fn an_unbind_as_the_newest_row_is_a_decided_state() {
    let (_dir, repo) = repo();
    put(&repo, T0, &ulid(1), Some("T1"));
    put(&repo, T0 + 100, &ulid(2), None);
    let history = BindingHistory::load(&repo, &repo).unwrap();
    assert_eq!(history.as_of(FOREVER_MS), TaskAtTime::Unbound);
    assert_eq!(history.as_of_second(T0 + 5_000), TaskAtTime::Unbound);
    assert_eq!(resolve(&repo, &repo).unwrap(), None);
}

/// A bind row stored under the redacted spelling of the path still resolves:
/// both spellings are queried, as they are for the live read.
#[test]
fn a_binding_stored_under_the_redacted_path_spelling_is_found() {
    let (_dir, repo) = repo();
    let odd = format!("{repo}/ghp_0123456789abcdefghijklmnopqrstuvwxyzA");
    let stored = crate::ledger::redact::text(&odd);
    assert_ne!(
        stored, odd,
        "the fixture must be a path the redactor rewrites"
    );
    with_conn(&repo, |conn| {
        conn.execute(
            "INSERT INTO events (ulid, ts_utc, repo_path, worktree_path, actor_kind, task_id,
                                 action, outcome)
             VALUES (?1, ?2, ?3, ?4, 'human', 'T9', 'worktree.bind', 'ok')",
            rusqlite::params![ulid(1), iso8601_utc(T0), &repo, &stored],
        )
        .unwrap();
        Ok(())
    })
    .unwrap();
    let history = BindingHistory::load(&repo, &odd).unwrap();
    assert_eq!(history.as_of(T0 + 1), bound("T9"));
}
