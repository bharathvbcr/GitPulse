//! The one definition of "this dead row is in the confident branch".
//!
//! The accuracy harness scores a dead row by whether the analysis called it
//! confidently dead, and the eligibility gate in `devmap-cli` asks the same
//! question of the rows `devmap dead` prints. Two private copies of the answer
//! disagreed on a coverage-capped row (0.663, [`COVERAGE_LOSS_REASON`]), so a
//! record re-measured by the runner dropped Kotlin's proof. Both now call
//! [`in_confident_branch`].
//!
//! Distinct from [`crate::earned_confident_tier`], which answers a narrower
//! question about an in-process report (published, or unproven-language) and
//! deliberately excludes a coverage-capped row: that row did not earn the tier,
//! but it *reached the branch* and was demoted by an incomplete scan.

use crate::liveness::{CONFIDENT_TIER_UNPROVEN, COVERAGE_LOSS_REASON, HIGHEST_DEGRADED_CONFIDENCE};

/// Whether a non-exempt dead row reached the confident branch of the liveness
/// pass: it published above [`HIGHEST_DEGRADED_CONFIDENCE`], it carries
/// [`CONFIDENT_TIER_UNPROVEN`] (its language has not earned the tier), or it
/// carries [`COVERAGE_LOSS_REASON`] (an incomplete scan capped it).
///
/// `confidence` is the published value; `exemption_reason` is the row's reason
/// text, `None` when it has none. A non-finite confidence never exceeds the
/// threshold, so only the reason can place such a row in the branch. Callers
/// filter exempt rows first, as every dead list does.
pub fn in_confident_branch(confidence: f64, exemption_reason: Option<&str>) -> bool {
    confidence > f64::from(HIGHEST_DEGRADED_CONFIDENCE)
        || exemption_reason.is_some_and(|reason| {
            reason.starts_with(CONFIDENT_TIER_UNPROVEN) || reason.starts_with(COVERAGE_LOSS_REASON)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        confident_tier_unproven_reason, CALL_BLIND_REASON, EXPORTED_UNREFERENCED,
        UNRESOLVED_NAMESAKE_REASON,
    };

    const THRESHOLD: f64 = HIGHEST_DEGRADED_CONFIDENCE as f64;

    #[test]
    fn a_published_confident_row_is_in_the_branch() {
        assert!(in_confident_branch(0.9, None));
        assert!(in_confident_branch(1.0, None));
    }

    #[test]
    fn the_threshold_is_exclusive() {
        assert!(!in_confident_branch(THRESHOLD, None));
        assert!(in_confident_branch(THRESHOLD + 1e-6, None));
        assert!(!in_confident_branch(THRESHOLD - 1e-6, None));
    }

    #[test]
    fn a_coverage_capped_row_is_in_the_branch_at_any_confidence() {
        // 0.663 is the graded cap Kotlin's ChronosFlow rows publish at.
        for confidence in [0.663, 0.4, 0.35, 0.0] {
            assert!(
                in_confident_branch(confidence, Some(COVERAGE_LOSS_REASON)),
                "{confidence}"
            );
        }
    }

    #[test]
    fn an_unproven_language_row_is_in_the_branch() {
        let reason = confident_tier_unproven_reason("python");
        assert!(in_confident_branch(0.4, Some(&reason)));
    }

    #[test]
    fn other_reasons_do_not_place_a_row_in_the_branch() {
        for reason in [
            "",
            CALL_BLIND_REASON,
            UNRESOLVED_NAMESAKE_REASON,
            "only_ambiguous_callers",
            EXPORTED_UNREFERENCED,
        ] {
            assert!(!in_confident_branch(0.663, Some(reason)), "{reason}");
        }
        assert!(!in_confident_branch(0.663, None));
    }

    #[test]
    fn a_non_finite_or_out_of_range_confidence_is_judged_by_value_and_reason() {
        assert!(!in_confident_branch(f64::NAN, None));
        assert!(in_confident_branch(f64::NAN, Some(COVERAGE_LOSS_REASON)));
        assert!(!in_confident_branch(-1.0, None));
        assert!(!in_confident_branch(f64::NEG_INFINITY, None));
        assert!(in_confident_branch(f64::INFINITY, None));
        assert!(in_confident_branch(7.0, None));
    }

    /// Attack Q5: the same published 0.89 judged in-process (f32 widened) and
    /// after a decimal round trip through JSON (f64 parse of "0.89").
    #[test]
    fn attack_a_published_0_89_is_judged_alike_in_process_and_after_json() {
        let in_process = f64::from(HIGHEST_DEGRADED_CONFIDENCE);
        // The CLI's JSON carries the widened f32 (0.8899999856948853), so the
        // CLI path agrees with in-process: not in the branch.
        let widened_json: f64 = format!("{:?}", in_process).parse().unwrap();
        assert!(!in_confident_branch(widened_json, None));
        // A hand-written decimal 0.89 (a record file, a test literal) is NOT
        // the same number and lands in the branch: documented hazard.
        let decimal_literal: f64 = "0.89".parse().unwrap();
        assert!(in_confident_branch(decimal_literal, None));
    }

    #[test]
    fn a_reason_must_start_with_the_token_not_merely_contain_it() {
        let embedded = format!("see also {COVERAGE_LOSS_REASON}");
        assert!(!in_confident_branch(0.4, Some(&embedded)));
        assert!(!in_confident_branch(0.4, Some("confident_tier")));
    }
}
