//! Ranking commits by how likely they are to have caused the symptom.
//!
//! Three signals, in the order they matter:
//!
//! 1. **Is the touched symbol in the cone at all.** A commit that touched
//!    nothing the symptom depends on is not a suspect, however recent. This is
//!    a filter, not a weight.
//! 2. **How far the touched symbol sits from the symptom.** A change one edge
//!    away is a better explanation than one five edges away.
//! 3. **Whether the body actually changed.** This is the signal blame cannot
//!    provide and the graph can: a commit that moved a symbol forty lines down
//!    without altering its body is a reformat, and reformats are the single
//!    largest source of false attribution in blame-based tooling. The graph
//!    stores a body signature precisely so this question is answerable.
//!
//! # Why the arithmetic is integer
//!
//! Scores are summed over a set whose iteration order is not guaranteed, and
//! floating-point addition is not associative — so a float score can differ
//! between two runs over identical input, and a report that cannot reproduce
//! itself cannot be used as evidence. Every weight here is a scaled integer
//! with saturating arithmetic, and the final ordering falls back through
//! enough keys to be total.

use serde::{Deserialize, Serialize};

/// Fixed-point scale. A distance-0 touch of one line scores this much.
const SCALE: u64 = 1_000;

/// What a commit did to one symbol in the cone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TouchedSymbol {
    pub qualified_name: String,
    pub file_path: String,
    /// Call edges from the symptom down to this symbol. Zero is the symptom
    /// itself.
    pub distance: u32,
    /// Lines of this symbol the commit last touched.
    pub lines: u32,
    /// `Some(true)` when the body signature differs between the generation
    /// before the commit and the one after; `Some(false)` when it is
    /// unchanged and the symbol merely moved; `None` when only one generation
    /// was available, so the question was not asked.
    ///
    /// The three states are deliberately not collapsed into a bool. "The body
    /// did not change" and "we could not tell whether the body changed" lead
    /// to opposite conclusions about a suspect, and a bool would make the
    /// cautious reading indistinguishable from the confident one.
    pub body_changed: Option<bool>,
}

/// How strong a commit's evidence is, as a class rather than a number.
///
/// This is the **primary** ranking key, and it has to be, because a weight
/// cannot express it. A multiplier lets a large enough reformat next to the
/// symptom climb over a small real edit — 100 moved lines at distance 0
/// outscored 5 changed lines at distance 1 even after an order-of-magnitude
/// demotion, and no constant fixes that in general, because the attacker is
/// the size of the diff and it is unbounded.
///
/// So size never crosses the class boundary. Within a class it decides; across
/// classes it does not get a vote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceClass {
    /// Nothing in this commit's touches changed a body. Ordered first in
    /// declaration order, which is *weakest* — see the `Ord` use in `rank`,
    /// which sorts descending.
    MovedOnly,
    /// The graph could not say whether bodies changed.
    Unknown,
    /// At least one touched symbol's body signature differs. This is the shape
    /// a behaviour regression has.
    BodyChanged,
}

/// A ranked suspect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suspect {
    pub commit: String,
    pub author: String,
    pub author_time: i64,
    /// The strongest evidence any of this commit's touches carries.
    pub evidence: EvidenceClass,
    /// Scaled integer; comparable only against other scores in the same
    /// report. Reported so a reader can see *why* one commit outranks another,
    /// not as a probability.
    pub score: u64,
    /// The closest any touched symbol sits to the symptom.
    pub nearest_distance: u32,
    /// Every cone symbol this commit touched, nearest first.
    pub touched: Vec<TouchedSymbol>,
}

/// Everything known about one commit's footprint, before ranking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RankInputs {
    pub commit: String,
    pub author: String,
    pub author_time: i64,
    pub touched: Vec<TouchedSymbol>,
}

/// The weight one touch contributes.
///
/// Proximity decays as `SCALE / (1 + distance)`, so distance 0 is `SCALE`,
/// distance 1 is half of it, distance 4 is a fifth. A hyperbola rather than an
/// exponential because the cone is already depth-capped: an exponential would
/// drive everything past depth three to zero and make the cap's exact value
/// the dominant input to the ranking.
///
/// The body multiplier is applied last, as a ratio, so it scales the whole
/// contribution rather than being added to it — a reformat of a hundred lines
/// next to the symptom must not outrank a one-line logic change.
fn touch_weight(touch: &TouchedSymbol) -> u64 {
    let proximity = SCALE / (u64::from(touch.distance).saturating_add(1));
    let base = proximity.saturating_mul(u64::from(touch.lines));
    match touch.body_changed {
        // The body changed: this is the shape a regression has.
        Some(true) => base,
        // The body is byte-identical and only its position moved. Demoted by
        // an order of magnitude rather than dropped: a pure move can still
        // break something (an ordering dependency, a `#[cfg]` boundary), so it
        // stays in the list where a reader can see it and falls below every
        // real edit.
        Some(false) => base / 10,
        // Not asked. Between the two, and closer to the cautious reading — an
        // unexamined commit must not outrank one proven to have changed logic.
        None => base.saturating_mul(6) / 10,
    }
}

/// Rank commits, most suspect first.
///
/// Deterministic for a given input: the score is integer, and ties break
/// through distance, then time, then commit id — which is total, because two
/// distinct suspects cannot share an id.
pub fn rank(inputs: Vec<RankInputs>) -> Vec<Suspect> {
    let mut suspects: Vec<Suspect> = inputs
        .into_iter()
        .filter(|input| !input.touched.is_empty())
        .map(|input| {
            let mut touched = input.touched;
            // Sort before summing. The sum of integers is order-independent,
            // but the *report* is read by humans and diffed by tests, so its
            // order must not depend on how the map that produced it iterated.
            touched.sort_by(|a, b| {
                a.distance
                    .cmp(&b.distance)
                    .then(b.lines.cmp(&a.lines))
                    .then(a.qualified_name.cmp(&b.qualified_name))
            });
            let score = touched
                .iter()
                .map(touch_weight)
                .fold(0u64, |total, weight| total.saturating_add(weight));
            let nearest_distance = touched
                .iter()
                .map(|touch| touch.distance)
                .min()
                .unwrap_or(u32::MAX);
            // The strongest class any touch carries. A commit that made one
            // real edit and reformatted five hundred lines around it did
            // change behaviour, and must be ranked as having done so.
            let evidence = touched
                .iter()
                .map(|touch| match touch.body_changed {
                    Some(true) => EvidenceClass::BodyChanged,
                    None => EvidenceClass::Unknown,
                    Some(false) => EvidenceClass::MovedOnly,
                })
                .max()
                .unwrap_or(EvidenceClass::Unknown);
            Suspect {
                commit: input.commit,
                author: input.author,
                author_time: input.author_time,
                evidence,
                score,
                nearest_distance,
                touched,
            }
        })
        .collect();

    suspects.sort_by(|a, b| {
        // Class first, descending: no amount of churn promotes a pure move
        // above a proven behaviour change.
        b.evidence
            .cmp(&a.evidence)
            .then(b.score.cmp(&a.score))
            .then(a.nearest_distance.cmp(&b.nearest_distance))
            .then(b.author_time.cmp(&a.author_time))
            .then(a.commit.cmp(&b.commit))
    });
    suspects
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(name: &str, distance: u32, lines: u32, body_changed: Option<bool>) -> TouchedSymbol {
        TouchedSymbol {
            qualified_name: name.to_string(),
            file_path: "f.rs".to_string(),
            distance,
            lines,
            body_changed,
        }
    }

    fn input(commit: &str, time: i64, touched: Vec<TouchedSymbol>) -> RankInputs {
        RankInputs {
            commit: commit.to_string(),
            author: "ada".to_string(),
            author_time: time,
            touched,
        }
    }

    #[test]
    fn a_commit_that_touched_nothing_in_the_cone_is_not_a_suspect() {
        let ranked = rank(vec![input("c1", 1, Vec::new())]);
        assert!(
            ranked.is_empty(),
            "the cone is a filter: a commit outside it is not a weak suspect, it is not one"
        );
    }

    #[test]
    fn a_nearer_change_outranks_a_farther_one_of_the_same_size() {
        let ranked = rank(vec![
            input("far", 1, vec![touch("x", 5, 10, Some(true))]),
            input("near", 1, vec![touch("y", 0, 10, Some(true))]),
        ]);
        assert_eq!(ranked[0].commit, "near");
    }

    /// The signal blame cannot supply. A large reformat next to the symptom
    /// must lose to a small real edit further away.
    #[test]
    fn a_reformat_loses_to_a_real_edit_even_when_it_is_nearer_and_larger() {
        let ranked = rank(vec![
            input("reformat", 9, vec![touch("x", 0, 100, Some(false))]),
            input("real", 1, vec![touch("y", 1, 5, Some(true))]),
        ]);
        assert_eq!(
            ranked[0].commit, "real",
            "a hundred moved lines are not evidence of a behaviour change; five \
             changed ones are"
        );
    }

    /// And it must keep losing at *any* size. This is why the class is a sort
    /// key rather than a multiplier: the size of a reformat is unbounded, so
    /// no constant demotion survives a large enough one.
    #[test]
    fn no_reformat_is_large_enough_to_outrank_a_single_changed_line() {
        let ranked = rank(vec![
            input(
                "enormous_reformat",
                9,
                vec![touch("x", 0, u32::MAX, Some(false))],
            ),
            input("one_line", 1, vec![touch("y", u32::MAX, 1, Some(true))]),
        ]);
        assert_eq!(
            ranked[0].commit, "one_line",
            "four billion moved lines at distance zero still moved nothing; one \
             changed line is still a change"
        );
    }

    /// A commit is classed by its strongest touch. Real work bundled with a
    /// reformat is real work.
    #[test]
    fn a_commit_that_both_edited_and_reformatted_is_classed_as_an_edit() {
        let ranked = rank(vec![input(
            "mixed",
            1,
            vec![
                touch("reformatted", 0, 500, Some(false)),
                touch("edited", 3, 1, Some(true)),
            ],
        )]);
        assert_eq!(ranked[0].evidence, EvidenceClass::BodyChanged);
    }

    /// Classes order strictly: changed, then unknown, then moved.
    #[test]
    fn the_three_classes_order_strictly() {
        let ranked = rank(vec![
            input("moved", 1, vec![touch("a", 0, 100, Some(false))]),
            input("unknown", 1, vec![touch("b", 0, 100, None)]),
            input("changed", 1, vec![touch("c", 0, 100, Some(true))]),
        ]);
        let order: Vec<&str> = ranked.iter().map(|s| s.commit.as_str()).collect();
        assert_eq!(order, vec!["changed", "unknown", "moved"]);
    }

    /// And an unexamined commit must not outrank a proven one.
    #[test]
    fn an_unknown_body_change_ranks_below_a_proven_one_of_the_same_shape() {
        let ranked = rank(vec![
            input("unknown", 1, vec![touch("x", 1, 10, None)]),
            input("proven", 1, vec![touch("y", 1, 10, Some(true))]),
        ]);
        assert_eq!(ranked[0].commit, "proven");
        assert_eq!(ranked[1].commit, "unknown");
    }

    /// ...but above one proven *not* to have changed anything, because
    /// "not asked" is not evidence of innocence.
    #[test]
    fn an_unknown_body_change_ranks_above_a_proven_non_change() {
        let ranked = rank(vec![
            input("unknown", 1, vec![touch("x", 1, 10, None)]),
            input("moved", 1, vec![touch("y", 1, 10, Some(false))]),
        ]);
        assert_eq!(ranked[0].commit, "unknown");
    }

    #[test]
    fn ranking_is_deterministic_across_runs() {
        let build = || {
            vec![
                input("bbb", 5, vec![touch("x", 1, 10, Some(true))]),
                input("aaa", 5, vec![touch("y", 1, 10, Some(true))]),
                input("ccc", 5, vec![touch("z", 1, 10, Some(true))]),
            ]
        };
        let first = rank(build());
        let second = rank(build());
        assert_eq!(first, second);
        // Identical scores, identical times: the id is what orders them, and
        // it must order them the same way every time.
        assert_eq!(
            first.iter().map(|s| s.commit.as_str()).collect::<Vec<_>>(),
            vec!["aaa", "bbb", "ccc"]
        );
    }

    /// Adversarial input must not panic or wrap. A line count near `u32::MAX`
    /// on a distance-0 touch is the largest product this arithmetic can be
    /// asked for.
    #[test]
    fn an_enormous_touch_saturates_rather_than_wrapping() {
        let ranked = rank(vec![input(
            "huge",
            1,
            vec![touch("x", 0, u32::MAX, Some(true))],
        )]);
        assert_eq!(ranked.len(), 1);
        assert!(
            ranked[0].score > 0,
            "saturating arithmetic never wraps to zero"
        );
    }

    #[test]
    fn many_touches_sum_exactly_rather_than_wrapping() {
        const TOUCHES: u64 = 1_000;
        let touched: Vec<TouchedSymbol> = (0..TOUCHES)
            .map(|i| touch(&format!("s{i}"), 0, u32::MAX, Some(true)))
            .collect();
        let ranked = rank(vec![input("huge", 1, touched)]);

        // The exact sum, not a bound. `assert_eq!(score, u64::MAX.min(score))`
        // stood here and asserted nothing at all — clippy named it — which is
        // the failure this test was written to catch happening to the test
        // itself. One touch is `SCALE / (1 + 0) * u32::MAX`, and the fold adds
        // `TOUCHES` of them.
        let one = SCALE * u64::from(u32::MAX);
        let expected = one * TOUCHES;
        assert_eq!(
            ranked[0].score, expected,
            "the fold must add every touch exactly; a wrap would land far below this"
        );
        // And the headroom that makes the exact sum the right expectation:
        // this input is nowhere near the saturation point, so the saturating
        // operators are a guard against a future input rather than something
        // this case exercises. Saying so keeps the test honest about what it
        // proves.
        assert!(
            expected < u64::MAX / 1_000,
            "if this ever stops holding the assertion above must become a saturation check"
        );
    }

    #[test]
    fn a_maximal_distance_does_not_divide_by_zero() {
        let ranked = rank(vec![input(
            "far",
            1,
            vec![touch("x", u32::MAX, 10, Some(true))],
        )]);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].nearest_distance, u32::MAX);
    }

    #[test]
    fn touched_symbols_are_reported_nearest_first() {
        let ranked = rank(vec![input(
            "c",
            1,
            vec![
                touch("far", 4, 1, Some(true)),
                touch("near", 0, 1, Some(true)),
                touch("mid", 2, 1, Some(true)),
            ],
        )]);
        let order: Vec<&str> = ranked[0]
            .touched
            .iter()
            .map(|t| t.qualified_name.as_str())
            .collect();
        assert_eq!(order, vec!["near", "mid", "far"]);
    }
}
