//! `resolve_pass` orders its edges and its unresolved ledger with rayon's
//! `par_sort_by`. The ledger comparator is deliberately not total, and the
//! edge comparator ties on rows that differ only in `evidence`, so which tied
//! row comes first is part of the output. `par_sort_by` is a stable merge sort,
//! like `sort_by`; this pins that the two emit the same bytes on input that is
//! scrambled and full of ties, large enough that rayon splits it across
//! threads.
//!
//! A perf-only change: no behaviour differs, so there is no pre-fix failure to
//! show. The parallel arm calls `sort_edges` / `sort_unresolved`, the same
//! functions `resolve_pass` calls, so moving them to an unstable sort fails
//! these tests; the graph-parity dump hashes remain the end-to-end check.

use super::resolve_all::{edge_order, sort_edges, sort_unresolved, unresolved_order};
use super::*;

const ROWS: u64 = 60_000;

/// A fixed xorshift, so the scramble is the same on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn edges() -> Vec<ResolvedEdge> {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    (0..ROWS)
        .map(|_| {
            let resolution = match rng.below(3) {
                0 => Resolution::SameFile {
                    target_symbol: format!("t{}", rng.below(2)),
                    target_file: "f".into(),
                },
                1 => Resolution::Structural {
                    target_symbol: format!("t{}", rng.below(2)),
                    target_file: "f".into(),
                },
                _ => Resolution::Unresolved {
                    reason: format!("r{}", rng.below(2)),
                },
            };
            let kind = if rng.below(2) == 0 {
                EdgeKind::Calls
            } else {
                EdgeKind::References
            };
            let mut edge = ResolvedEdge::resolved(
                format!("s{}", rng.below(4)),
                format!("g{}", rng.below(3)),
                format!("a{}", rng.below(3)),
                format!("b{}", rng.below(2)),
                kind,
                Arc::new(resolution),
                if rng.below(2) == 0 {
                    None
                } else {
                    Some("d".into())
                },
            );
            // The one field no comparator key reads: it tells tied rows apart,
            // so a reordered tie shows up in the dump.
            edge.evidence = Some(Evidence {
                kind: ResolutionKind::ALL[rng.below(ResolutionKind::ALL.len() as u64) as usize],
                source: match rng.below(3) {
                    0 => ResolutionSource::Resolver,
                    1 => ResolutionSource::Stored,
                    _ => ResolutionSource::Reconstructed,
                },
            });
            edge
        })
        .collect()
}

fn ledger() -> Vec<UnresolvedReference> {
    let mut rng = Rng(0xdead_beef_cafe_f00d);
    (0..ROWS)
        .map(|i| UnresolvedReference {
            source_file: format!("s{}", rng.below(6)),
            source_symbol: format!("a{}", rng.below(4)),
            callee_name: format!("c{}", rng.below(4)),
            kind: UnresolvedKind::Call,
            resolution: Resolution::Unresolved {
                reason: format!("reason {i}"),
            },
            class: UnresolvedClass::Unresolved,
            // Not a sort key: unique per row, so every tie is visible.
            receiver: Some(format!("recv{i}")),
            site: None,
        })
        .collect()
}

#[test]
fn edges_sort_to_the_same_bytes_as_the_serial_sort() {
    let input = edges();
    let mut serial = input.clone();
    let mut parallel = input;
    serial.sort_by(edge_order);
    sort_edges(&mut parallel);

    let ties = serial
        .windows(2)
        .filter(|w| edge_order(&w[0], &w[1]).is_eq())
        .count();
    assert!(
        ties > 10_000,
        "the fixture must be full of ties to prove anything: {ties}"
    );
    assert_eq!(format!("{serial:?}"), format!("{parallel:?}"));
}

#[test]
fn the_ledger_sorts_to_the_same_bytes_as_the_serial_sort() {
    let input = ledger();
    let mut serial = input.clone();
    let mut parallel = input;
    serial.sort_by(unresolved_order);
    sort_unresolved(&mut parallel);

    let ties = serial
        .windows(2)
        .filter(|w| unresolved_order(&w[0], &w[1]).is_eq())
        .count();
    assert!(
        ties > 10_000,
        "the fixture must be full of ties to prove anything: {ties}"
    );
    assert_eq!(serial, parallel);
}
