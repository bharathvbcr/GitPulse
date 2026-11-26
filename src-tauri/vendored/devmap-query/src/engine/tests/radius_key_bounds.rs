//! The unresolved-ledger check of a walk is capped, and a cap is never a clean
//! bill.
//!
//! The keyed ledger read binds one SQLite variable per key, and SQLite refuses
//! to prepare a statement past `SQLITE_MAX_VARIABLE_NUMBER`. The caller
//! (`radius_attribution_gap`) therefore cuts the key list at
//! [`MAX_RADIUS_LEDGER_KEYS`] and says how many it left unchecked. These tests
//! pin that boundary: the capped statement prepares on a connection, the key list is cut exactly at the cap and not before it, and the
//! note carries both counts.

use std::collections::BTreeSet;

use super::gaps::{radius_keys, radius_note, RadiusSide, MAX_RADIUS_LEDGER_KEYS};

/// `n` walked symbols, each in its own file.
fn reached(n: usize) -> BTreeSet<(String, String)> {
    (0..n)
        .map(|i| (format!("src/f{i:05}.py::run"), format!("src/f{i:05}.py")))
        .collect()
}

/// Binds the keyed ledger statement carries beside its keys: the generation
/// (three times), the cap, and one per unattributed label.
fn fixed_binds() -> usize {
    devmap_resolve::UNATTRIBUTED_LABELS.len() + 4
}

#[test]
fn the_key_cap_plus_the_fixed_binds_fits_the_connections_variable_limit() {
    // The limit is a compile-time option of the SQLite this binary links, so
    // it is probed on a connection rather than quoted: the statement at the
    // cap, with every bind the real one carries, must prepare.
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    let binds = MAX_RADIUS_LEDGER_KEYS + fixed_binds();
    let placeholders = (1..=binds)
        .map(|n| format!("?{n}"))
        .collect::<Vec<_>>()
        .join(", ");
    connection
        .prepare(&format!("SELECT {placeholders}"))
        .unwrap_or_else(|error| panic!("{binds} variables do not prepare: {error}"));
}

#[test]
fn a_file_key_list_at_the_cap_is_checked_whole() {
    let (keys, not_checked) =
        radius_keys(&reached(MAX_RADIUS_LEDGER_KEYS), RadiusSide::FileCallees);
    assert_eq!(keys.len(), MAX_RADIUS_LEDGER_KEYS);
    assert_eq!(not_checked, 0);
    assert_eq!(
        radius_note(
            RadiusSide::FileCallees,
            &keys,
            not_checked,
            Default::default()
        ),
        None,
        "a complete check that found nothing must stay silent"
    );
}

#[test]
fn a_file_key_list_one_past_the_cap_is_cut_and_the_cut_is_reported() {
    let (keys, not_checked) = radius_keys(
        &reached(MAX_RADIUS_LEDGER_KEYS + 1),
        RadiusSide::FileCallees,
    );
    assert_eq!(keys.len(), MAX_RADIUS_LEDGER_KEYS);
    assert_eq!(not_checked, 1);
    let note = radius_note(
        RadiusSide::FileCallees,
        &keys,
        not_checked,
        Default::default(),
    )
    .expect("a capped check must not read as a clean one");
    assert!(
        note.contains(&format!(
            "only {MAX_RADIUS_LEDGER_KEYS} of {} reached key(s)",
            MAX_RADIUS_LEDGER_KEYS + 1
        )),
        "the note does not carry both counts: {note}"
    );
}
