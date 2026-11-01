//! The keyed ledger read (`Store::unresolved_sites_keyed`) — what it answers,
//! and the shape of the plan it answers with.
//!
//! The statement filtered `p.path IN (…)` and `c.text IN (…)` — columns of the
//! two *joined* dimension tables — so SQLite drove the join from `paths` and
//! `unresolved_texts` and rescanned the whole of `unresolved_rows` once per
//! (path x class label) pair: eight scans of a 613,000-row table for a
//! three-file read. Filtering the ledger's own id columns through a semi-join
//! scans it once. These tests pin both halves: the rewrite changed no answer,
//! and the plan stays a single scan.

use super::generation::keyed_ledger_sql;
use super::*;
use devmap_analyze::analyze;
use devmap_extract::extract_file;
use devmap_resolve::model::{
    Resolution, ResolutionResult, UnresolvedClass, UnresolvedKind, UnresolvedReference,
};
use rusqlite::types::Value;

const FILES: [&str; 4] = ["alpha.py", "beta.py", "gamma.py", "delta.py"];

/// Every class, so the label filter has both sides to be right about: the four
/// `UNATTRIBUTED_LABELS` and two explained classes.
fn classes() -> [UnresolvedClass; 6] {
    [
        UnresolvedClass::Unresolved,
        UnresolvedClass::Builtin,
        UnresolvedClass::UninferredReceiver,
        UnresolvedClass::NoNamesake,
        UnresolvedClass::UnresolvedMember,
        UnresolvedClass::LocalBinding,
    ]
}

/// The ledger of one generation: four files x four symbols x three callees,
/// with the class cycling and the receiver alternating between `None` and
/// `Some`. `hub` is a symbol the generation declares, `ghost` and `other` are
/// not, so the namesake clause keeps a receiver call to `hub` and drops one to
/// `ghost`. `skip` thins the ledger so a second generation closes some rows.
fn ledger(skip: usize) -> Vec<UnresolvedReference> {
    let classes = classes();
    let mut rows = Vec::new();
    let mut n = 0usize;
    for file in FILES {
        for sym in 0..4 {
            for callee in ["hub", "ghost", "other"] {
                n += 1;
                if skip != 0 && n % skip == 0 {
                    continue;
                }
                rows.push(UnresolvedReference {
                    source_file: file.into(),
                    source_symbol: format!("{file}::sym_{sym}"),
                    callee_name: callee.into(),
                    kind: UnresolvedKind::Call,
                    resolution: Resolution::Unresolved {
                        reason: format!("reason_{n}"),
                    },
                    class: classes[n % classes.len()].clone(),
                    receiver: if n % 2 == 0 {
                        None
                    } else {
                        Some(format!("recv_{n}"))
                    },
                    site: None,
                });
            }
        }
    }
    rows
}

/// Two generations over one extraction that declares `hub`: the second writes a
/// thinner ledger, so the first generation's thinned rows are closed rows and
/// a read at either generation has to honour `valid_from`/`valid_to`.
fn two_generations() -> (Store, [u32; 2]) {
    let extractions = vec![extract_file("hub.py", "def hub():\n    return 1\n")];
    let store = Store::open_in_memory().unwrap();
    let mut ids = [0u32; 2];
    for (slot, skip) in [0usize, 5].into_iter().enumerate() {
        let resolution = ResolutionResult {
            edges: Vec::new(),
            receiver_types: Default::default(),
            reexport_chains: Default::default(),
            unresolved: ledger(skip),
        };
        let analysis = analyze(&extractions, &resolution);
        store
            .save_generation(&extractions, &resolution, &analysis)
            .unwrap();
        ids[slot] = store.latest_generation_id().unwrap().unwrap();
    }
    assert!(ids[0] < ids[1], "two distinct generations were written");
    (store, ids)
}

/// The statement as it stood before the semi-join: the key and the class label
/// were compared on the *joined* `p.path` / `c.text`. Kept verbatim, as the
/// oracle every row of the rewritten statement is compared against — the same
/// device `the_rust_edge_order_is_the_sql_order_it_replaced` uses for edges.
fn statement_before_the_semi_join(
    column: &str,
    key_count: usize,
    unattributed_only: bool,
) -> String {
    let placeholders = vec!["?"; key_count].join(", ");
    let class_filter = if unattributed_only {
        format!(
            "AND c.text IN ({})
                   AND (u.receiver IS NULL
                        OR u.callee_name IN (SELECT n.name FROM generation_nodes n
                                             WHERE n.generation_id = ?))",
            vec!["?"; devmap_resolve::UNATTRIBUTED_LABELS.len()].join(", ")
        )
    } else {
        String::new()
    };
    format!(
        "SELECT key, path, source_symbol, receiver, class, rn, callee_name FROM (
                 SELECT {column} AS key, p.path AS path,
                        u.source_symbol AS source_symbol, u.receiver AS receiver,
                        c.text AS class, u.callee_name AS callee_name,
                        ROW_NUMBER() OVER (
                            PARTITION BY {column}
                            ORDER BY p.path, u.source_symbol, u.unresolved_id
                        ) AS rn
                 FROM unresolved_rows u
                 JOIN paths p            ON p.id = u.source_file_id
                 JOIN unresolved_texts c ON c.id = u.classification_id
                 WHERE {column} IN ({placeholders})
                   AND u.valid_from <= ?
                   AND (u.valid_to IS NULL OR u.valid_to > ?)
                   {class_filter}
             )
             WHERE rn <= ?
             ORDER BY key, rn"
    )
}

/// The values a statement of either shape binds, in its placeholder order.
fn bindings(
    keys: &[String],
    generation: u32,
    unattributed_only: bool,
    limit_per_key: usize,
) -> Vec<Value> {
    let generation = i64::from(generation);
    let mut values: Vec<Value> = keys.iter().map(|key| Value::Text(key.clone())).collect();
    values.push(Value::Integer(generation));
    values.push(Value::Integer(generation));
    if unattributed_only {
        values.extend(
            devmap_resolve::UNATTRIBUTED_LABELS
                .iter()
                .map(|label| Value::Text((*label).to_string())),
        );
        values.push(Value::Integer(generation));
    }
    values.push(Value::Integer(
        i64::try_from(limit_per_key + 1).expect("a small cap"),
    ));
    values
}

/// Run `sql` as the store does and fold its rows into the answer shape: the
/// per-key cap applied on rank, the flag set when a row past it exists.
fn answer_of(
    store: &Store,
    sql: &str,
    keys: &[String],
    generation: u32,
    unattributed_only: bool,
    limit_per_key: usize,
) -> UnresolvedSitesByName {
    let mut found: UnresolvedSitesByName = keys
        .iter()
        .map(|key| (key.clone(), (Vec::new(), false)))
        .collect();
    let conn = lock_conn(&store.conn).unwrap();
    let mut stmt = conn.prepare(sql).unwrap();
    let values = bindings(keys, generation, unattributed_only, limit_per_key);
    let mut rows = stmt
        .query(rusqlite::params_from_iter(values.iter()))
        .unwrap();
    while let Some(row) = rows.next().unwrap() {
        let key: String = row.get(0).unwrap();
        let rank: i64 = row.get(5).unwrap();
        let (sites, truncated) = found.get_mut(&key).expect("a key that was asked for");
        if usize::try_from(rank).unwrap() > limit_per_key {
            *truncated = true;
            continue;
        }
        sites.push(UnresolvedSiteRow {
            source_file: row.get(1).unwrap(),
            source_symbol: row.get(2).unwrap(),
            receiver: row.get(3).unwrap(),
            classification: row.get(4).unwrap(),
            callee_name: row.get(6).unwrap(),
        });
    }
    found
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

/// The rewrite changed the plan and nothing else: every one of the four public
/// reads returns, row for row and flag for flag, what the statement it replaced
/// returned, at both generations and at caps that do and do not bite.
///
/// The fixture is built to make a wrong rewrite visible: two of the four files
/// are keys and two are not; one key is absent from the ledger; classes the
/// filter must keep sit beside ones it must drop; receivers are `NULL` and set;
/// a callee with a namesake sits beside two without; and the caps (1, 2) cut
/// keys that hold a dozen rows while 100 cuts none.
#[test]
fn every_keyed_read_returns_what_the_statement_it_replaced_returned() {
    let (store, generations) = two_generations();
    let files = strings(&["alpha.py", "gamma.py", "absent.py"]);
    let symbols = strings(&["alpha.py::sym_0", "gamma.py::sym_2", "nosuch::sym"]);
    let callees = strings(&["hub", "ghost", "missing"]);

    // (what the public read keys on, the column the old statement named,
    // whether the public read filters to the unattributed classes)
    type Read = fn(&Store, u32, &[String], usize) -> Result<Option<UnresolvedSitesByName>>;
    let reads: [(&str, LedgerKey, &str, bool, &[String], Read); 4] = [
        (
            "unresolved_sites_naming",
            LedgerKey::Callee,
            "u.callee_name",
            false,
            &callees,
            Store::unresolved_sites_naming,
        ),
        (
            "unattributed_sites_naming",
            LedgerKey::Callee,
            "u.callee_name",
            true,
            &callees,
            Store::unattributed_sites_naming,
        ),
        (
            "unattributed_sites_within",
            LedgerKey::SourceSymbol,
            "u.source_symbol",
            true,
            &symbols,
            Store::unattributed_sites_within,
        ),
        (
            "unattributed_sites_in_files",
            LedgerKey::SourceFile,
            "p.path",
            true,
            &files,
            Store::unattributed_sites_in_files,
        ),
    ];

    let mut non_empty = 0usize;
    let mut truncated_somewhere = 0usize;
    for (name, key, column, unattributed_only, keys, read) in reads {
        for generation in generations {
            for cap in [1usize, 2, 100] {
                let oracle = answer_of(
                    &store,
                    &statement_before_the_semi_join(column, keys.len(), unattributed_only),
                    keys,
                    generation,
                    unattributed_only,
                    cap,
                );
                let rewritten = answer_of(
                    &store,
                    &keyed_ledger_sql(key, keys.len(), unattributed_only),
                    keys,
                    generation,
                    unattributed_only,
                    cap,
                );
                assert_eq!(
                    rewritten, oracle,
                    "{name} at generation {generation}, cap {cap}: the rewritten statement \
                     disagrees with the one it replaced"
                );
                let served = read(&store, generation, keys, cap)
                    .unwrap()
                    .expect("the generation is retained");
                assert_eq!(
                    served, oracle,
                    "{name} at generation {generation}, cap {cap}: the store's answer \
                     disagrees with the statement it replaced"
                );
                non_empty += oracle.values().filter(|(sites, _)| !sites.is_empty()).count();
                truncated_somewhere += oracle.values().filter(|(_, cut)| *cut).count();
            }
        }
    }
    // A parity test over an empty or never-truncated answer proves nothing.
    assert!(non_empty >= 40, "the fixture produced almost no rows: {non_empty}");
    assert!(
        truncated_somewhere >= 8,
        "the fixture never exercised the per-key cap: {truncated_somewhere}"
    );
}

/// The plan rows of `sql`, as `(id, parent, detail)`.
fn plan_of(store: &Store, sql: &str, bound: &[Value]) -> Vec<(i64, i64, String)> {
    let conn = lock_conn(&store.conn).unwrap();
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    let rows = stmt
        .query_map(rusqlite::params_from_iter(bound.iter()), |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>>>()
        .unwrap();
    rows
}

/// A keyed read scans the ledger once, as the *outer* loop.
///
/// Before the semi-join the planner drove the join from `paths` and
/// `unresolved_texts` and put `SCAN u` innermost: one scan of `unresolved_rows`
/// per (path x label) pair. A plan reads as nested loops in the order its
/// sibling rows are listed, so "`SCAN u` is the first loop of its level" is
/// the statement that no dimension-table loop encloses it.
#[test]
fn a_keyed_ledger_read_scans_the_ledger_once_and_not_per_key_and_label() {
    let (store, [_, generation]) = two_generations();
    for (key, key_count) in [
        (LedgerKey::SourceFile, 3usize),
        (LedgerKey::SourceSymbol, 3),
        (LedgerKey::Callee, 3),
    ] {
        for unattributed_only in [true, false] {
            if !unattributed_only && key != LedgerKey::Callee {
                // Not reachable through the public reads, and the old shape had
                // no label loop to multiply by.
                continue;
            }
            let sql = keyed_ledger_sql(key, key_count, unattributed_only);
            let keys: Vec<String> = (0..key_count).map(|i| format!("k{i}")).collect();
            let plan = plan_of(
                &store,
                &sql,
                &bindings(&keys, generation, unattributed_only, 5),
            );
            let shown = || {
                plan.iter()
                    .map(|(id, parent, detail)| format!("  {id} <- {parent}  {detail}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let is_ledger_scan = |detail: &str| {
                detail == "SCAN u"
                    || detail.starts_with("SCAN u ")
                    || detail == "SEARCH u"
                    || detail.starts_with("SEARCH u ")
            };
            let scans: Vec<&(i64, i64, String)> = plan
                .iter()
                .filter(|(_, _, detail)| is_ledger_scan(detail))
                .collect();
            assert_eq!(
                scans.len(),
                1,
                "{key:?}/unattributed={unattributed_only}: the ledger is scanned once, not \
                 once per join: \n{}",
                shown()
            );
            let (_, parent, _) = scans[0];
            let first_loop = plan
                .iter()
                .find(|(_, level, detail)| {
                    level == parent && (detail.starts_with("SCAN ") || detail.starts_with("SEARCH "))
                })
                .expect("the scan's own level has a loop");
            assert!(
                is_ledger_scan(&first_loop.2),
                "{key:?}/unattributed={unattributed_only}: a dimension-table loop encloses the \
                 ledger scan, so it runs once per key and label: \n{}",
                shown()
            );
        }
    }
}
