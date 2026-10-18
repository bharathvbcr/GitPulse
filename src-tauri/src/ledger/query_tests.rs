//! Filtered, ordered ledger reads: every filter is served by an index, a
//! newest-first read stops at its limit, and the page says whether more follow.

use super::*;
use std::time::Instant;

fn repo_dir() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().to_str().unwrap().to_string();
    (dir, repo)
}

/// `rows` events, in one transaction, varied across every filtered column.
/// Written by SQL rather than [`append`] so a 45 000-row ledger builds in a
/// moment; the columns are the ones `append` fills for the same shapes.
fn fill(repo: &str, rows: usize) {
    with_conn(repo, |conn| {
        let stored_repo = redact::text(&canonical_repo(repo));
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| LedgerError::new("t", e.to_string()))?;
        {
            let mut insert = tx
                .prepare(
                    "INSERT INTO events (ulid, ts_utc, repo_path, worktree_path, actor_kind,
                                         session_id, task_id, action, object, outcome, after_ref)
                     VALUES (?1, ?2, ?3, ?4, 'agent', ?5, ?6, ?7, ?8, 'ok', ?9)",
                )
                .map_err(|e| LedgerError::new("t", e.to_string()))?;
            for n in 0..rows {
                insert
                    .execute(params![
                        format!("{n:026}"),
                        // One minute per row, fixed width, so `ts_utc` orders like `id`.
                        format!(
                            "2026-01-{:02}T{:02}:{:02}:00.000Z",
                            1 + n / 1440 % 28,
                            n / 60 % 24,
                            n % 60
                        ),
                        stored_repo,
                        format!("/w/{}", n % 5),
                        (n % 4 != 0).then(|| format!("S{}", n % 4)),
                        (n % 2 == 0).then(|| format!("T{}", n % 7)),
                        format!("session.{}", ["command", "stop", "start"][n % 3]),
                        format!("obj-{n}"),
                        (n % 10 == 0).then(|| format!("{n:040x}")),
                    ])
                    .map_err(|e| LedgerError::new("t", e.to_string()))?;
            }
        }
        tx.commit()
            .map_err(|e| LedgerError::new("t", e.to_string()))
    })
    .unwrap();
}

fn page(repo: &str, query: &EventQuery) -> EventPage {
    query_readonly_page(repo, query, PAGE_MAX_BYTES)
        .unwrap()
        .expect("a ledger")
}

fn ids(page: &EventPage) -> Vec<i64> {
    page.events.iter().map(|event| event.id).collect()
}

#[test]
fn newest_first_with_a_limit_is_the_newest_rows_and_says_more_follow() {
    let (_dir, repo) = repo_dir();
    fill(&repo, 100);
    let newest = page(
        &repo,
        &EventQuery {
            limit: 5,
            descending: true,
            ..EventQuery::default()
        },
    );
    assert_eq!(ids(&newest), vec![100, 99, 98, 97, 96]);
    assert!(newest.more && !newest.byte_capped);

    // The cursor of a newest-first read is an upper bound: continuing from the
    // lowest id returned gives the next-older rows with nothing repeated.
    let older = page(
        &repo,
        &EventQuery {
            limit: 5,
            descending: true,
            before: Some(96),
            ..EventQuery::default()
        },
    );
    assert_eq!(ids(&older), vec![95, 94, 93, 92, 91]);

    let last = page(
        &repo,
        &EventQuery {
            limit: 5,
            descending: true,
            before: Some(6),
            ..EventQuery::default()
        },
    );
    assert_eq!(ids(&last), vec![5, 4, 3, 2, 1]);
    assert!(!last.more, "an exactly-full final page is not truncated");
}

#[test]
fn oldest_first_is_unchanged_by_the_new_query() {
    let (_dir, repo) = repo_dir();
    fill(&repo, 30);
    let first = page(
        &repo,
        &EventQuery {
            limit: 10,
            ..EventQuery::default()
        },
    );
    assert_eq!(ids(&first), (1..=10).collect::<Vec<_>>());
    assert!(first.more);
    let rest = page(
        &repo,
        &EventQuery {
            after: 10,
            limit: 50,
            ..EventQuery::default()
        },
    );
    assert_eq!(ids(&rest), (11..=30).collect::<Vec<_>>());
    assert!(!rest.more);
}

fn every_event(repo: &str) -> Vec<LedgerEvent> {
    page(
        repo,
        &EventQuery {
            limit: 1000,
            ..EventQuery::default()
        },
    )
    .events
}

#[test]
fn each_filter_returns_exactly_the_matching_rows_in_either_order() {
    let (_dir, repo) = repo_dir();
    fill(&repo, 400);
    let all = every_event(&repo);
    type Select = fn(&LedgerEvent) -> bool;
    let cases: Vec<(EventQuery, Select)> = vec![
        (
            EventQuery {
                action: Some("session.stop".into()),
                ..EventQuery::default()
            },
            |e| e.action == "session.stop",
        ),
        (
            EventQuery {
                object: Some("obj-77".into()),
                ..EventQuery::default()
            },
            |e| e.object.as_deref() == Some("obj-77"),
        ),
        (
            EventQuery {
                worktree_path: Some("/w/3".into()),
                ..EventQuery::default()
            },
            |e| e.worktree_path.as_deref() == Some("/w/3"),
        ),
        (
            EventQuery {
                task_id: Some("T4".into()),
                ..EventQuery::default()
            },
            |e| e.task_id.as_deref() == Some("T4"),
        ),
        (
            EventQuery {
                session_id: Some("S2".into()),
                ..EventQuery::default()
            },
            |e| e.session_id.as_deref() == Some("S2"),
        ),
        (
            EventQuery {
                after_ref: Some(format!("{:040x}", 120)),
                ..EventQuery::default()
            },
            |e| e.after_ref.as_deref() == Some(&format!("{:040x}", 120)),
        ),
        (
            EventQuery {
                since_ts: Some("2026-01-01T03:00:00.000Z".into()),
                ..EventQuery::default()
            },
            |e| e.ts_utc.as_str() >= "2026-01-01T03:00:00.000Z",
        ),
        (
            EventQuery {
                action: Some("session.command".into()),
                task_id: Some("T4".into()),
                session_id: Some("S2".into()),
                ..EventQuery::default()
            },
            |e| {
                e.action == "session.command"
                    && e.task_id.as_deref() == Some("T4")
                    && e.session_id.as_deref() == Some("S2")
            },
        ),
    ];
    for (query, select) in cases {
        let expected: Vec<i64> = all.iter().filter(|e| select(e)).map(|e| e.id).collect();
        assert!(
            !expected.is_empty(),
            "{query:?} selects nothing: a vacuous case"
        );
        for descending in [false, true] {
            let got = page(
                &repo,
                &EventQuery {
                    limit: 1000,
                    descending,
                    ..query.clone()
                },
            );
            let mut want = expected.clone();
            if descending {
                want.reverse();
            }
            assert_eq!(ids(&got), want, "{query:?} descending={descending}");
        }
    }
}

#[test]
fn a_filtered_page_is_bounded_and_continues_by_cursor() {
    let (_dir, repo) = repo_dir();
    fill(&repo, 400);
    let query = EventQuery {
        action: Some("session.stop".into()),
        limit: 7,
        descending: true,
        ..EventQuery::default()
    };
    let first = page(&repo, &query);
    assert_eq!(first.events.len(), 7);
    assert!(first.more);
    let second = page(
        &repo,
        &EventQuery {
            before: first.events.last().map(|e| e.id),
            ..query
        },
    );
    assert!(ids(&second)
        .iter()
        .all(|id| *id < *ids(&first).last().unwrap()));
    assert!(second.events.iter().all(|e| e.action == "session.stop"));
}

#[test]
fn every_filter_is_served_by_an_index_never_a_full_scan() {
    let (_dir, repo) = repo_dir();
    fill(&repo, 2_000);
    let cases = [
        (
            EventQuery {
                since_ts: Some("2026-01-01T03:00:00.000Z".into()),
                ..EventQuery::default()
            },
            "idx_events_ts",
        ),
        (
            EventQuery {
                action: Some("session.stop".into()),
                ..EventQuery::default()
            },
            "idx_events_action",
        ),
        (
            EventQuery {
                object: Some("obj-77".into()),
                ..EventQuery::default()
            },
            "idx_events_object",
        ),
        (
            EventQuery {
                worktree_path: Some("/w/3".into()),
                ..EventQuery::default()
            },
            "idx_events_worktree",
        ),
        (
            EventQuery {
                task_id: Some("T4".into()),
                ..EventQuery::default()
            },
            "idx_events_task",
        ),
        (
            EventQuery {
                session_id: Some("S2".into()),
                ..EventQuery::default()
            },
            "idx_events_session",
        ),
        (
            EventQuery {
                after_ref: Some("x".into()),
                ..EventQuery::default()
            },
            "idx_events_after_ref",
        ),
    ];
    with_conn(&repo, |conn| {
        for (query, index) in &cases {
            for descending in [false, true] {
                let plan = explain_query(
                    conn,
                    &EventQuery {
                        descending,
                        limit: 5,
                        ..query.clone()
                    },
                );
                let text = plan.join(" | ");
                assert!(
                    text.contains(index),
                    "{index} descending={descending}: {text}"
                );
                assert!(
                    plan.iter().all(|step| !step.starts_with("SCAN")),
                    "{index} descending={descending} scans: {text}"
                );
            }
        }
        // The unfiltered newest-N read is a seek on the primary key, with the
        // LIMIT in the statement.
        let newest = explain_query(
            conn,
            &EventQuery {
                descending: true,
                limit: 5,
                ..EventQuery::default()
            },
        );
        assert!(
            newest
                .iter()
                .all(|step| step.starts_with("SEARCH") && !step.contains("TEMP B-TREE")),
            "{newest:?}"
        );
        let (sql, _) = EventQuery {
            descending: true,
            limit: 5,
            ..EventQuery::default()
        }
        .sql(6, true);
        assert!(sql.contains("ORDER BY id DESC LIMIT ?"), "{sql}");
        Ok(())
    })
    .unwrap();
}

/// A ledger written before these indexes existed gains them the next time a
/// writer opens it, by the same additive `CREATE INDEX IF NOT EXISTS` batch
/// that creates the rest.
#[test]
fn an_older_ledger_gains_the_indexes_on_the_next_writable_open() {
    let (_dir, repo) = repo_dir();
    fill(&repo, 10);
    with_conn(&repo, |conn| {
        conn.execute_batch(
            "DROP INDEX idx_events_ts; DROP INDEX idx_events_action; DROP INDEX idx_events_object;
             DROP INDEX idx_events_worktree; DROP INDEX idx_events_after_ref;",
        )
        .map_err(|e| LedgerError::new("t", e.to_string()))
    })
    .unwrap();
    tests_support::reset_registry_for(&repo);
    let names: Vec<String> = with_conn(&repo, |conn| {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'index'")
            .map_err(|e| LedgerError::new("t", e.to_string()))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| LedgerError::new("t", e.to_string()))?;
        rows.collect::<Result<_, _>>()
            .map_err(|e| LedgerError::new("t", e.to_string()))
    })
    .unwrap();
    for index in [
        "idx_events_ts",
        "idx_events_action",
        "idx_events_object",
        "idx_events_worktree",
        "idx_events_after_ref",
    ] {
        assert!(names.iter().any(|name| name == index), "{index}: {names:?}");
    }
}

/// Newest-5 on a 45 000-row ledger: the old way (page oldest-first until the
/// history is exhausted, then keep the last five) against one newest-first
/// read with a limit. Interleaved A/B, minimum of N. Run with:
/// `cargo test --manifest-path src-tauri/Cargo.toml --release --lib -- --ignored --nocapture ledger::query_tests::bench`
#[test]
#[ignore = "benchmark: prints timings, asserts only that the newest-first read is not slower"]
fn bench_newest_five_on_a_45k_row_ledger() {
    let (_dir, repo) = repo_dir();
    fill(&repo, 45_000);
    let paged = || {
        let mut cursor = 0;
        let mut last = Vec::new();
        loop {
            let (events, _) = tail_readonly_page(&repo, cursor, 1000, PAGE_MAX_BYTES)
                .unwrap()
                .unwrap();
            let Some(tip) = events.last().map(|event| event.id) else {
                break;
            };
            cursor = tip;
            last = events;
        }
        last.split_off(last.len() - 5)
    };
    let indexed = || {
        page(
            &repo,
            &EventQuery {
                limit: 5,
                descending: true,
                ..EventQuery::default()
            },
        )
        .events
    };
    let (mut best_paged, mut best_indexed) = (u128::MAX, u128::MAX);
    for _ in 0..7 {
        let started = Instant::now();
        let old = paged();
        best_paged = best_paged.min(started.elapsed().as_micros());
        let started = Instant::now();
        let mut new = indexed();
        best_indexed = best_indexed.min(started.elapsed().as_micros());
        new.reverse();
        assert_eq!(
            old.iter().map(|e| e.id).collect::<Vec<_>>(),
            new.iter().map(|e| e.id).collect::<Vec<_>>()
        );
    }
    println!(
        "newest-5 on 45000 rows: paged-oldest-first min {best_paged} us, newest-first limit 5 min {best_indexed} us"
    );
    assert!(best_indexed <= best_paged);
}

#[path = "adversarial_tests.rs"]
mod adversarial;
