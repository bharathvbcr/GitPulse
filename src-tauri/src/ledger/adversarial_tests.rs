//! Adversarial review of card 15 (S3-11 + S6-06), ledger half: the five new
//! indexes must not move any existing query off its old plan, the filtered
//! read must bind every value, and the appends must not pay much for them.
//!
//! A test that fails today is `#[ignore = "break: ..."]`.

use super::*;
use std::time::Instant;

/// The five indexes this change added.
const NEW_INDEXES: [&str; 5] = [
    "idx_events_ts",
    "idx_events_action",
    "idx_events_object",
    "idx_events_worktree",
    "idx_events_after_ref",
];

fn drop_new_indexes(conn: &Connection) {
    for name in NEW_INDEXES {
        conn.execute_batch(&format!("DROP INDEX IF EXISTS {name}"))
            .expect("drop");
    }
}

/// A ledger shaped like a working one: sessions, tasks, worktrees, bind and
/// unbind rows, reflog rows, re-anchor rows, other repositories' rows.
fn populated(repo: &str, rows: usize) {
    fill(repo, rows);
    with_conn(repo, |conn| {
        let stored = redact::text(&canonical_repo(repo));
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| LedgerError::new("t", e.to_string()))?;
        for n in 0..300 {
            tx.execute(
                "INSERT INTO events (ulid, ts_utc, repo_path, worktree_path, actor_kind,
                                     session_id, task_id, action, object, outcome)
                 VALUES (?1, ?2, ?3, ?4, 'system', ?5, ?6, ?7, ?8, 'ok')",
                params![
                    format!("Z{n:025}"),
                    format!("2026-02-{:02}T00:{:02}:00.000Z", 1 + n / 60, n % 60),
                    stored,
                    format!("/w/{}", n % 5),
                    (n % 3 == 0).then_some("gitpulse-ledger-identity"),
                    (n % 4 == 0).then(|| format!("T{}", n % 7)),
                    ["worktree.bind", "worktree.unbind", "reflog.commit", "ledger.reanchor"][n % 4],
                    format!("obj-z{n}"),
                ],
            )
            .map_err(|e| LedgerError::new("t", e.to_string()))?;
        }
        tx.commit()
            .map_err(|e| LedgerError::new("t", e.to_string()))
    })
    .unwrap();
}

/// `(label, sql, parameters)` for every existing ledger read, text copied from
/// the call sites (`ledger/mod.rs`, `ledger/anchor.rs`) or the shared consts.
fn existing_queries() -> Vec<(&'static str, String, Vec<String>)> {
    let s = |v: &str| v.to_string();
    let two = vec![s("/w/1"), s("/w/1")];
    let mut three = two.clone();
    three.push(s("2026-02-01T00:30:00.000Z"));
    vec![
        ("latest binding", LATEST_BINDING_SQL.to_string(), two.clone()),
        ("binding as of", BINDING_AS_OF_SQL.to_string(), three),
        ("binding history", BINDING_HISTORY_SQL.to_string(), two.clone()),
        (
            "newest binding of a sibling",
            "SELECT ts_utc, ulid, action, task_id FROM events
             WHERE action IN ('worktree.bind', 'worktree.unbind')
               AND worktree_path IN (?1, ?2)
             ORDER BY ts_utc DESC, ulid DESC
             LIMIT 1"
                .to_string(),
            two.clone(),
        ),
        (
            "foreign row probe",
            format!("SELECT 1 FROM events WHERE {} LIMIT 1", foreign_row_sql(2)),
            vec![s("/a"), s("/b")],
        ),
        (
            "foreign paths",
            format!(
                "SELECT DISTINCT repo_path FROM events WHERE {} ORDER BY repo_path LIMIT 11",
                foreign_row_sql(2)
            ),
            vec![s("/a"), s("/b")],
        ),
        (
            "reflog keys",
            "SELECT object,
                    CASE WHEN json_valid(detail_json)
                         THEN json_extract(detail_json, '$.object_truncated.hash') END
             FROM events
             WHERE action LIKE 'reflog.%' AND object IS NOT NULL
               AND (worktree_path IN (?1, ?2) OR (?3 AND worktree_path IS NULL))"
                .to_string(),
            vec![s("/w/1"), s("/w/1"), s("0")],
        ),
        (
            "re-anchor aliases",
            "SELECT detail_json FROM events
             WHERE session_id = ?1 AND action = ?2 AND repo_path IN (?3, ?4)
             ORDER BY id LIMIT 65"
                .to_string(),
            vec![s("gitpulse-ledger-identity"), s("ledger.reanchor"), s("/a"), s("/b")],
        ),
        (
            "latest identity",
            "SELECT detail_json FROM events WHERE session_id = ?1 AND action IN (?2, ?3)
             ORDER BY id DESC LIMIT 1"
                .to_string(),
            vec![s("gitpulse-ledger-identity"), s("ledger.anchor"), s("ledger.reanchor")],
        ),
        (
            "by ulid",
            "SELECT id FROM events WHERE ulid = ?1".to_string(),
            vec![s("x")],
        ),
        (
            "prune",
            "SELECT id FROM events
             WHERE ts_utc < ?1
               AND action NOT IN ('worktree.bind', 'worktree.unbind')
               AND id <= (SELECT COALESCE(MAX(id), 0) FROM events) - 1000
             ORDER BY id LIMIT 500"
                .to_string(),
            vec![s("2026-01-15T00:00:00.000Z")],
        ),
        (
            "count by action",
            "SELECT count(*) FROM events WHERE action = ?1".to_string(),
            vec![s("ledger.reanchor")],
        ),
        (
            "legacy copy",
            "SELECT id FROM events WHERE id > 5 AND id <= 100 ORDER BY id ASC LIMIT 1000"
                .to_string(),
            vec![],
        ),
    ]
}

fn plans(conn: &Connection) -> Vec<(String, Vec<String>)> {
    existing_queries()
        .into_iter()
        .map(|(label, sql, values)| {
            let mut stmt = conn
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap_or_else(|e| panic!("{label}: {e}"));
            let steps = stmt
                .query_map(rusqlite::params_from_iter(values.iter()), |row| {
                    row.get::<_, String>(3)
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            (label.to_string(), steps)
        })
        .collect()
}

#[test]
fn no_existing_ledger_query_changes_plan_because_of_the_new_indexes() {
    for analyzed in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_str().unwrap().to_string();
        populated(&repo, 4_000);
        with_conn(&repo, |conn| {
            if analyzed {
                conn.execute_batch("ANALYZE").unwrap();
            }
            let with = plans(conn);
            drop_new_indexes(conn);
            if analyzed {
                conn.execute_batch("ANALYZE").unwrap();
            }
            let without = plans(conn);
            let mut moved = Vec::new();
            for ((label, after), (_, before)) in with.iter().zip(&without) {
                println!(
                    "PLAN analyzed={analyzed} {label}: {} || was: {}",
                    after.join(" | "),
                    before.join(" | ")
                );
                if after != before {
                    moved.push(format!("{label}\n   now: {after:?}\n   was: {before:?}"));
                }
            }
            assert!(
                moved.is_empty(),
                "analyzed={analyzed}: plans changed:\n{}",
                moved.join("\n")
            );
            Ok(())
        })
        .unwrap();
    }
}

/// `since_ts` alone names `INDEXED BY idx_events_ts`, whose range is then
/// sorted by id: a newest-N read with an old `since_ts` reads and sorts every
/// row since it instead of walking the primary key and stopping at N.
#[test]
#[ignore = "break: {order:desc, limit:5, since_ts:<old>} reads every row since `since_ts` and sorts them (INDEXED BY idx_events_ts + TEMP B-TREE) instead of stopping after 5 on the primary key"]
fn a_newest_five_with_an_old_since_ts_stops_after_five_rows() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().to_str().unwrap().to_string();
    fill(&repo, 45_000);
    let query = EventQuery {
        descending: true,
        limit: 5,
        since_ts: Some("2000-01-01T00:00:00.000Z".into()),
        ..EventQuery::default()
    };
    with_conn(&repo, |conn| {
        let plan = explain_query(conn, &query);
        println!("SINCE plan: {plan:?}");
        assert!(
            plan.iter().all(|step| !step.contains("TEMP B-TREE")),
            "{plan:?}"
        );
        Ok(())
    })
    .unwrap();
}

fn min_of_5(repo: &str, query: &EventQuery) -> std::time::Duration {
    (0..5)
        .map(|_| {
            let started = Instant::now();
            query_readonly_page(repo, query, PAGE_MAX_BYTES)
                .unwrap()
                .unwrap();
            started.elapsed()
        })
        .min()
        .unwrap()
}

#[test]
fn since_ts_timing_old_versus_recent_on_45k_rows() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().to_str().unwrap().to_string();
    fill(&repo, 45_000);
    let newest = |since: Option<&str>| {
        min_of_5(
            &repo,
            &EventQuery {
                descending: true,
                limit: 5,
                since_ts: since.map(str::to_string),
                ..EventQuery::default()
            },
        )
    };
    println!(
        "TIMING newest 5 on 45k rows, min of 5: no filter {:?}; since_ts=2000 {:?}; \
         since_ts=2026-01-20 {:?}; since_ts=2099 {:?}",
        newest(None),
        newest(Some("2000-01-01T00:00:00.000Z")),
        newest(Some("2026-01-20T00:00:00.000Z")),
        newest(Some("2099-01-01T00:00:00.000Z")),
    );
}

#[test]
fn filters_are_bound_values_whatever_they_contain() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().to_str().unwrap().to_string();
    populated(&repo, 500);
    let hostile = [
        "%",
        "_",
        "'",
        "' OR '1'='1",
        "'; DROP TABLE events; --",
        "--",
        "obj-1%",
        "obj-_",
        "ünï—✓ 日本語",
        "\0",
        "line\nbreak",
    ];
    for value in hostile {
        for column in 0..6 {
            let mut query = EventQuery {
                limit: 1000,
                ..EventQuery::default()
            };
            let slot = [
                &mut query.action,
                &mut query.object,
                &mut query.worktree_path,
                &mut query.task_id,
                &mut query.session_id,
                &mut query.after_ref,
            ]
            .into_iter()
            .nth(column)
            .unwrap();
            *slot = Some(value.to_string());
            let page = query_readonly_page(&repo, &query, PAGE_MAX_BYTES)
                .unwrap_or_else(|e| panic!("{value:?} in column {column}: {e}"))
                .expect("ledger");
            assert!(
                page.events.is_empty(),
                "{value:?} in column {column} matched {} rows: a pattern, not a value",
                page.events.len()
            );
        }
    }
    with_conn(&repo, |conn| {
        let count: i64 = conn
            .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert!(count >= 500);
        Ok(())
    })
    .unwrap();
}

#[test]
fn a_stored_unicode_or_quoted_object_is_found_by_exactly_that_value() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().to_str().unwrap().to_string();
    for object in ["ünï—✓ 日本語", "it's", "50%", "a_b"] {
        append(Draft {
            repo_path: repo.clone(),
            action: "session.command".into(),
            object: Some(object.into()),
            actor_kind: Some(ActorKind::Agent),
            outcome: Some(Outcome::Ok),
            ..Draft::default()
        })
        .unwrap();
    }
    for (needle, expected) in [
        ("ünï—✓ 日本語", 1),
        ("it's", 1),
        ("50%", 1),
        ("a_b", 1),
        ("a%", 0),
        ("a_", 0),
    ] {
        let found = page(
            &repo,
            &EventQuery {
                object: Some(needle.into()),
                ..EventQuery::default()
            },
        );
        assert_eq!(found.events.len(), expected, "{needle:?}");
    }
}

#[test]
fn a_very_long_object_filter_and_every_filter_at_once_are_answered() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().to_str().unwrap().to_string();
    populated(&repo, 500);
    let found = page(
        &repo,
        &EventQuery {
            object: Some("x".repeat(1 << 20)),
            ..EventQuery::default()
        },
    );
    assert!(found.events.is_empty());
    let everything = EventQuery {
        after: 3,
        before: Some(100_000),
        limit: 7,
        descending: true,
        since_ts: Some("2026-01-01T00:00:00.000Z".into()),
        action: Some("session.stop".into()),
        object: Some("obj-1".into()),
        worktree_path: Some("/w/1".into()),
        task_id: Some("T1".into()),
        session_id: Some("S1".into()),
        after_ref: Some("x".into()),
    };
    page(&repo, &everything);
    with_conn(&repo, |conn| {
        let plan = explain_query(conn, &everything);
        println!("ALL-FILTERS plan: {plan:?}");
        assert!(
            plan.iter().all(|step| !step.starts_with("SCAN")),
            "{plan:?}"
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn limit_extremes_are_clamped_not_refused_or_unbounded() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().to_str().unwrap().to_string();
    fill(&repo, 1_500);
    for (limit, expected) in [
        (0, 1),
        (1, 1),
        (1_000, 1_000),
        (1_001, 1_000),
        (u32::MAX, 1_000),
    ] {
        for descending in [false, true] {
            let found = query_readonly_page(
                &repo,
                &EventQuery {
                    limit,
                    descending,
                    ..EventQuery::default()
                },
                usize::MAX / 2,
            )
            .unwrap()
            .unwrap();
            assert_eq!(found.events.len(), expected, "limit {limit} desc {descending}");
            assert!(found.more);
        }
    }
}

#[test]
fn cursor_edges_for_either_order() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().to_str().unwrap().to_string();
    fill(&repo, 30);
    // desc, upper bound 0: nothing is below it.
    let p = page(
        &repo,
        &EventQuery {
            before: Some(0),
            descending: true,
            limit: 5,
            ..EventQuery::default()
        },
    );
    assert!(p.events.is_empty() && !p.more);
    // desc, upper bound far beyond the newest: the newest page.
    let p = page(
        &repo,
        &EventQuery {
            before: Some(i64::MAX),
            descending: true,
            limit: 3,
            ..EventQuery::default()
        },
    );
    assert_eq!(ids(&p), vec![30, 29, 28]);
    // asc, lower bound beyond the newest: empty, not more.
    let p = page(
        &repo,
        &EventQuery {
            after: 1_000,
            limit: 3,
            ..EventQuery::default()
        },
    );
    assert!(p.events.is_empty() && !p.more);
    // A negative lower bound is everything; a negative upper bound nothing.
    let p = page(
        &repo,
        &EventQuery {
            after: -5,
            limit: 2,
            ..EventQuery::default()
        },
    );
    assert_eq!(ids(&p), vec![1, 2]);
    let p = page(
        &repo,
        &EventQuery {
            before: Some(-5),
            descending: true,
            limit: 2,
            ..EventQuery::default()
        },
    );
    assert!(p.events.is_empty());
    // An `asc` page's last id reused as a `desc` cursor flips the direction
    // silently: the rows below it, newest first.
    let asc = page(
        &repo,
        &EventQuery {
            limit: 5,
            ..EventQuery::default()
        },
    );
    let last = asc.events.last().unwrap().id;
    let flipped = page(
        &repo,
        &EventQuery {
            before: Some(last),
            descending: true,
            limit: 5,
            ..EventQuery::default()
        },
    );
    assert_eq!(ids(&flipped), vec![4, 3, 2, 1]);
}

/// Paging newest-first by `next_cursor` visits exactly the matching rows,
/// once each, in reverse id order, for every filter.
#[test]
fn desc_paging_with_a_filter_matches_ascending_reversed() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().to_str().unwrap().to_string();
    populated(&repo, 1_200);
    for filter in [
        EventQuery::default(),
        EventQuery {
            action: Some("session.stop".into()),
            ..EventQuery::default()
        },
        EventQuery {
            task_id: Some("T2".into()),
            session_id: Some("S2".into()),
            ..EventQuery::default()
        },
        EventQuery {
            worktree_path: Some("/w/3".into()),
            since_ts: Some("2026-01-01T10:00:00.000Z".into()),
            ..EventQuery::default()
        },
    ] {
        let mut asc = Vec::new();
        let mut cursor = 0;
        loop {
            let p = page(
                &repo,
                &EventQuery {
                    after: cursor,
                    limit: 97,
                    ..filter.clone()
                },
            );
            let Some(last) = p.events.last().map(|e| e.id) else {
                break;
            };
            asc.extend(ids(&p));
            cursor = last;
            if !p.more {
                break;
            }
        }
        let mut desc = Vec::new();
        let mut before = None;
        loop {
            let p = page(
                &repo,
                &EventQuery {
                    before,
                    descending: true,
                    limit: 89,
                    ..filter.clone()
                },
            );
            let Some(last) = p.events.last().map(|e| e.id) else {
                break;
            };
            desc.extend(ids(&p));
            before = Some(last);
            if !p.more {
                break;
            }
        }
        asc.reverse();
        assert_eq!(asc, desc, "{filter:?}");
    }
}

/// Interleaved A/B, min-of-N: 10 000 real appends into a fresh ledger with the
/// five indexes, and into one that had them dropped (the old schema). Prints
/// the per-append delta; asserts only that the indexes are what differs.
#[test]
#[ignore = "bench: 60 000 real appends exceed the suite's 40 MiB/s disk-write guard; run alone with GITPULSE_TEST_WRITE_BUDGET=0 and --nocapture"]
fn append_cost_of_the_five_new_indexes() {
    fn run(with_indexes: bool, rows: usize) -> std::time::Duration {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_str().unwrap().to_string();
        with_conn(&repo, |conn| {
            if !with_indexes {
                drop_new_indexes(conn);
            }
            Ok(())
        })
        .unwrap();
        let started = Instant::now();
        for n in 0..rows {
            append(Draft {
                repo_path: repo.clone(),
                worktree_path: Some(format!("/w/{}", n % 5)),
                session_id: Some(format!("S{}", n % 4)),
                task_id: (n % 2 == 0).then(|| format!("T{}", n % 7)),
                action: format!("session.{}", ["command", "stop", "start"][n % 3]),
                object: Some(format!("echo {n}")),
                actor_kind: Some(ActorKind::Agent),
                outcome: Some(Outcome::Ok),
                after_ref: (n % 10 == 0).then(|| format!("{n:040x}")),
                ..Draft::default()
            })
            .unwrap();
        }
        let elapsed = started.elapsed();
        with_conn(&repo, |conn| {
            let present: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='index' AND name IN \
                     ('idx_events_ts','idx_events_action','idx_events_object',\
                      'idx_events_worktree','idx_events_after_ref')",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(present, if with_indexes { 5 } else { 0 });
            Ok(())
        })
        .unwrap();
        elapsed
    }
    let rows = 10_000;
    let (mut before, mut after) = (std::time::Duration::MAX, std::time::Duration::MAX);
    for _ in 0..3 {
        before = before.min(run(false, rows));
        after = after.min(run(true, rows));
    }
    println!(
        "APPENDBENCH {rows} appends, min of 3 interleaved: before {before:?} ({:?}/append), \
         after {after:?} ({:?}/append), delta {:?}/append, ratio {:.3}",
        before / rows as u32,
        after / rows as u32,
        after.saturating_sub(before) / rows as u32,
        after.as_secs_f64() / before.as_secs_f64()
    );
}

/// What the first writable open of an existing large ledger pays to add the
/// five indexes (`CREATE INDEX IF NOT EXISTS` runs inside `open`).
#[test]
fn first_open_index_build_time_on_45k_rows() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().to_str().unwrap().to_string();
    fill(&repo, 45_000);
    with_conn(&repo, |conn| {
        drop_new_indexes(conn);
        let started = Instant::now();
        conn.execute_batch(
            "CREATE INDEX idx_events_ts ON events(ts_utc);
             CREATE INDEX idx_events_action ON events(action) WHERE id > 0;
             CREATE INDEX idx_events_object ON events(object) WHERE id > 0;
             CREATE INDEX idx_events_worktree ON events(worktree_path) WHERE id > 0;
             CREATE INDEX idx_events_after_ref ON events(after_ref) WHERE after_ref IS NOT NULL;",
        )
        .unwrap();
        println!("INDEXBUILD 45k rows, five indexes: {:?}", started.elapsed());
        Ok(())
    })
    .unwrap();
}
