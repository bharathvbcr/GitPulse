//! A symbol's identity is `payload_id << NODE_ORD_BITS | ord_in_file`, owned by
//! `Store::fts_rowid`, decoded in SQL by `FTS_PAYLOAD_SQL` and `FTS_ORD_SQL`, and
//! spelled once more in the DDL's `generation_nodes` view. Four places, one
//! number; these hold them together.

use super::*;
use crate::schema::{MAX_NODES_PER_PAYLOAD, MIGRATION_V27_TO_V28, NODE_ORD_BITS};

#[test]
fn the_sql_that_decodes_a_rowid_uses_the_bits_the_encoder_packs() {
    assert_eq!(
        FTS_PAYLOAD_SQL,
        format!("(nodes_fts.rowid >> {NODE_ORD_BITS})")
    );
    assert_eq!(
        FTS_ORD_SQL,
        format!("(nodes_fts.rowid & {})", MAX_NODES_PER_PAYLOAD - 1)
    );
    assert_eq!(MAX_NODES_PER_PAYLOAD, 1 << NODE_ORD_BITS);
    // And the DDL: the view's ordinal, the map's rowid and the full-text rebuild
    // all pack with the same shift, and the column's CHECK bounds the same range.
    let shifts = MIGRATION_V27_TO_V28
        .matches(&format!("<< {NODE_ORD_BITS})"))
        .count();
    assert!(
        shifts >= 3,
        "the view ordinal, the map rowid and the index rebuild each pack by {NODE_ORD_BITS}: {shifts}"
    );
    assert!(MIGRATION_V27_TO_V28.contains(&format!("ord_in_file < {MAX_NODES_PER_PAYLOAD}")));
}

#[test]
fn a_symbol_identity_round_trips_and_a_wrapped_one_is_refused() {
    for (payload, ord) in [(1i64, 0usize), (1, 1), (7, 16_777_215), (123_456_789, 42)] {
        let rowid = Store::fts_rowid(payload, ord).unwrap();
        assert_eq!(rowid >> NODE_ORD_BITS, payload);
        assert_eq!(rowid & i64::from(MAX_NODES_PER_PAYLOAD - 1), ord as i64);
    }
    // One symbol too many would spill into the payload's bits and name a
    // neighbour's first symbol: refused, with the payload named.
    let error = Store::fts_rowid(5, MAX_NODES_PER_PAYLOAD as usize).unwrap_err();
    assert!(error.to_string().contains("payload 5"), "{error}");
    for payload in [0i64, -1, 1i64 << 40] {
        assert!(
            Store::fts_rowid(payload, 0).is_err(),
            "payload id {payload} does not fit a symbol identity"
        );
    }
}
