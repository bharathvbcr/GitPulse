//! Adversarial verification of `ContendedLine` interval math `[s, e + 1]` in `entity_collision.rs`.
//! Tests boundary conditions: adjacent, touching, overlapping, point insertions,
//! asymmetric orders, multi-party intersections, and differential fuzzing against a naive oracle.

use super::*;
use std::collections::BTreeSet;

fn make_party(footprint: Vec<(u64, u64)>) -> PartyEdits {
    let mut sorted = footprint;
    sorted.sort_unstable();
    PartyEdits {
        keys: BTreeSet::new(),
        footprint: sorted,
    }
}

#[test]
fn test_exact_overlap_interval() {
    let p1 = make_party(vec![(10, 15)]);
    let p2 = make_party(vec![(10, 15)]);
    let verdict = classify_parties("test.rs", &[p1, p2], &[]);
    assert_eq!(verdict.kind, EntityCollisionKind::ContendedLine);
    assert_eq!(verdict.contended_lines, vec![(10, 15)]);
}

#[test]
fn test_partial_overlap_interval() {
    let p1 = make_party(vec![(10, 16)]);
    let p2 = make_party(vec![(12, 19)]);
    let verdict = classify_parties("test.rs", &[p1, p2], &[]);
    assert_eq!(verdict.kind, EntityCollisionKind::ContendedLine);
    assert_eq!(verdict.contended_lines, vec![(12, 16)]);
}

#[test]
fn test_touching_boundary_adjacent_lines() {
    // Party A modifies old lines 10..=14: footprint [10, 15]
    // Party B modifies old lines 15..=20: footprint [15, 21]
    // In git diff coordinates, lines 14 and 15 are adjacent.
    // Git conflicts adjacent modifications. The math [s, e + 1] produces
    // intersection at boundary point 15.
    let p1 = make_party(vec![(10, 15)]);
    let p2 = make_party(vec![(15, 21)]);
    let verdict = classify_parties("test.rs", &[p1, p2], &[]);
    assert_eq!(verdict.kind, EntityCollisionKind::ContendedLine);
    assert_eq!(verdict.contended_lines, vec![(15, 15)]);
    assert!(verdict.reason.contains("edits touch at line 15"));
}

#[test]
fn test_non_touching_adjacent_gap_of_one_line() {
    // Party A modifies old lines 10..=14: footprint [10, 15]
    // Party B modifies old lines 16..=20: footprint [16, 21]
    // Old line 15 is UNCHANGED between them.
    let p1 = make_party(vec![(10, 15)]);
    let p2 = make_party(vec![(16, 21)]);
    let verdict = classify_parties("test.rs", &[p1, p2], &[]);
    assert_eq!(verdict.kind, EntityCollisionKind::DisjointSymbols);
    assert!(verdict.contended_lines.is_empty());
}

#[test]
fn test_insertion_touching_preceding_modification() {
    // Party A modifies lines 10..=14: [10, 15]
    // Party B inserts after line 14: point [15, 15]
    let p1 = make_party(vec![(10, 15)]);
    let p2 = make_party(vec![(15, 15)]);
    let verdict = classify_parties("test.rs", &[p1, p2], &[]);
    assert_eq!(verdict.kind, EntityCollisionKind::ContendedLine);
    assert_eq!(verdict.contended_lines, vec![(15, 15)]);
}

#[test]
fn test_insertion_touching_subsequent_modification() {
    // Party A inserts after line 14: point [15, 15]
    // Party B modifies lines 15..=20: [15, 21]
    let p1 = make_party(vec![(15, 15)]);
    let p2 = make_party(vec![(15, 21)]);
    let verdict = classify_parties("test.rs", &[p1, p2], &[]);
    assert_eq!(verdict.kind, EntityCollisionKind::ContendedLine);
    assert_eq!(verdict.contended_lines, vec![(15, 15)]);
}

#[test]
fn test_two_identical_insertions() {
    // Both insert after line 14: point [15, 15]
    let p1 = make_party(vec![(15, 15)]);
    let p2 = make_party(vec![(15, 15)]);
    let verdict = classify_parties("test.rs", &[p1, p2], &[]);
    assert_eq!(verdict.kind, EntityCollisionKind::ContendedLine);
    assert_eq!(verdict.contended_lines, vec![(15, 15)]);
}

#[test]
fn test_two_adjacent_insertions_are_disjoint() {
    // Party A inserts after line 14: point [15, 15]
    // Party B inserts after line 15: point [16, 16]
    // Line 15 separates the two insertion points.
    let p1 = make_party(vec![(15, 15)]);
    let p2 = make_party(vec![(16, 16)]);
    let verdict = classify_parties("test.rs", &[p1, p2], &[]);
    assert_eq!(verdict.kind, EntityCollisionKind::DisjointSymbols);
    assert!(verdict.contended_lines.is_empty());
}

#[test]
fn test_file_start_boundary_insertion() {
    // Insertion after line 0: point [1, 1]
    // Modification of line 1: [1, 2]
    let p1 = make_party(vec![(1, 1)]);
    let p2 = make_party(vec![(1, 2)]);
    let verdict = classify_parties("test.rs", &[p1.clone(), p2], &[]);
    assert_eq!(verdict.kind, EntityCollisionKind::ContendedLine);
    assert_eq!(verdict.contended_lines, vec![(1, 1)]);

    // Modification of line 2: [2, 3] -> disjoint from insertion at line 0
    let p3 = make_party(vec![(2, 3)]);
    let verdict2 = classify_parties("test.rs", &[p1, p3], &[]);
    assert_eq!(verdict2.kind, EntityCollisionKind::DisjointSymbols);
}

#[test]
fn test_three_way_pairwise_contact_and_deduplication() {
    // Party A: [10, 15]
    // Party B: [20, 25]
    // Party C: [15, 20]
    // A and C touch at 15. B and C touch at 20. A and B do not touch.
    let p1 = make_party(vec![(10, 15)]);
    let p2 = make_party(vec![(20, 25)]);
    let p3 = make_party(vec![(15, 20)]);
    let verdict = classify_parties("test.rs", &[p1, p2, p3], &[]);
    assert_eq!(verdict.kind, EntityCollisionKind::ContendedLine);
    assert_eq!(verdict.contended_lines, vec![(15, 15), (20, 20)]);
}

#[test]
fn test_randomized_interval_fuzzing_against_oracle() {
    // Differential fuzzer: 1,000 randomized test cases comparing classify_parties against naive oracle
    let mut rng_state: u64 = 0xdeadbeef12345678;
    let mut next_rand = || -> u64 {
        rng_state ^= rng_state << 13;
        rng_state ^= rng_state >> 7;
        rng_state ^= rng_state << 17;
        rng_state
    };

    for _ in 0..1000 {
        let num_a = (next_rand() % 6 + 1) as usize;
        let num_b = (next_rand() % 6 + 1) as usize;

        let mut a_intervals = Vec::new();
        for _ in 0..num_a {
            let s = next_rand() % 200 + 1;
            let len = next_rand() % 15;
            a_intervals.push((s, s + len));
        }

        let mut b_intervals = Vec::new();
        for _ in 0..num_b {
            let s = next_rand() % 200 + 1;
            let len = next_rand() % 15;
            b_intervals.push((s, s + len));
        }

        let p1 = make_party(a_intervals.clone());
        let p2 = make_party(b_intervals.clone());

        // Naive reference oracle:
        let mut expected_contended = Vec::new();
        for &(s1, e1) in &p1.footprint {
            for &(s2, e2) in &p2.footprint {
                if s1.max(s2) <= e1.min(e2) {
                    expected_contended.push((s1.max(s2), e1.min(e2)));
                }
            }
        }
        expected_contended.sort_unstable();
        expected_contended.dedup();

        let verdict = classify_parties("fuzz.rs", &[p1, p2], &[]);
        if expected_contended.is_empty() {
            assert_eq!(verdict.kind, EntityCollisionKind::DisjointSymbols);
            assert!(verdict.contended_lines.is_empty());
        } else {
            assert_eq!(verdict.kind, EntityCollisionKind::ContendedLine);
            assert_eq!(verdict.contended_lines, expected_contended);
        }
    }
}
