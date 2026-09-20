//! "Which commits could have caused this?" — blame joined to the code graph.
//!
//! The order of operations is the whole argument. A blame-first tool reads a
//! file's gutter and ranks by recency; this reads the *graph* first, so the
//! only lines ever blamed belong to symbols the symptom actually depends on.
//! Everything else in the file is noise by construction.
//!
//! GitPulse does not reimplement any of that. `dc-regress` is the analysis and
//! takes its graph through a trait; `dc-regress-store` presents the persisted
//! devmap generation through that trait, and is the same adapter `devmap
//! suspects` uses. A second adapter here would have had to agree, silently,
//! with that one about which direction the dependency cone runs — and that
//! direction was already wrong once upstream, producing a cone holding nothing
//! but the seed.
//!
//! # The window is closed at both ends by the index, not by the caller
//!
//! The analysis is only sound when the byte offsets in the graph and the lines
//! in the blame describe the same content. The store records the `head_sha` its
//! generation was built at, so `until` is always that commit — never `HEAD`,
//! which is a different tree the moment anyone commits. The caller chooses
//! `since` and nothing else. A working tree that was dirty when the index was
//! built surfaces as a refusal rather than as a wrong answer.

use dc_regress::{SuspectReport, Unavailable};
use serde::{Deserialize, Serialize};

use super::{open_repo_map, CodeintelResponse};

/// Deepest dependency cone this surface will walk.
///
/// Kept in step with the `maximum` the MCP schema advertises for `depth`; the
/// schema is what a client sees, this is what actually holds. Ten is already
/// far past the point where a call chain explains a regression, and each extra
/// level multiplies the files blamed.
pub const MAX_CONE_DEPTH: u32 = 10;

/// One cone symbol a suspect commit touched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelTouchedSymbol {
    pub qualified_name: String,
    pub file_path: String,
    /// Call edges from the symptom down to this symbol. Zero is the symptom.
    pub distance: u32,
    pub lines: u32,
    /// `Some(true)` the body changed, `Some(false)` it only moved, `None` the
    /// question could not be asked. Three states, not a bool: "did not change"
    /// and "could not tell" rank differently and must not be collapsed here.
    pub body_changed: Option<bool>,
}

/// One commit that could have caused the symptom.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelSuspect {
    pub commit: String,
    pub author: String,
    pub author_time: i64,
    /// `body_changed`, `moved_only` or `unknown` — the strongest evidence any
    /// of this commit's touches carries, and the primary sort key.
    pub evidence: String,
    /// Scaled integer, comparable only against other scores in the same
    /// answer. Surfaced so a reader can see *why* one commit outranks another;
    /// it is not a probability and must not be rendered as one.
    pub score: u64,
    pub nearest_distance: u32,
    pub touched: Vec<CodeintelTouchedSymbol>,
}

/// What the analysis examined, so an empty list can be read correctly.
///
/// An empty `items` with `available: true` means "nothing in the window touched
/// the cone", which is a finding. An empty `items` with `available: false`
/// means something could not be examined. Those are different answers, and the
/// panel must not render them the same way.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelSuspectScope {
    /// The commit the index was built at — the window's upper bound.
    pub indexed_head: String,
    pub since: String,
    /// Symbols in the dependency cone, and how many of them were blamed.
    pub cone_size: u32,
    pub blamed_symbols: u32,
    /// Every step that could not be taken, already in human-readable form.
    pub refusals: Vec<String>,
}

/// What crosses the Tauri boundary: the ranked list and what was examined.
///
/// One struct rather than a tuple, because the two halves are only meaningful
/// together. `response.items` empty with `response.available` true is "nothing
/// in the window touched the cone" — a finding — and `scope` is the only thing
/// that lets a caller distinguish that from an answer that was cut short.
/// A tuple would serialise as a bare array and invite reading the first element
/// alone, which is exactly that mistake.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelSuspectsPayload {
    pub response: CodeintelResponse<CodeintelSuspect>,
    /// `None` only when the run was refused before it could establish a window.
    pub scope: Option<CodeintelSuspectScope>,
}

fn evidence_label(evidence: dc_regress::EvidenceClass) -> &'static str {
    match evidence {
        dc_regress::EvidenceClass::BodyChanged => "body_changed",
        dc_regress::EvidenceClass::MovedOnly => "moved_only",
        dc_regress::EvidenceClass::Unknown => "unknown",
    }
}

/// Whether a refusal means the whole answer is void rather than merely partial.
///
/// `SymptomNotFound` and `GitUnavailable` leave nothing to report; the rest
/// narrow the answer and belong on a result that is still worth showing. The
/// split is what keeps `available: false` meaning "could not examine" instead
/// of degrading into "found nothing interesting".
fn is_fatal(refusal: &Unavailable) -> bool {
    matches!(
        refusal,
        Unavailable::SymptomNotFound { .. }
            | Unavailable::GitUnavailable { .. }
            | Unavailable::GraphEmpty { .. }
    )
}

fn to_response(
    report: SuspectReport,
    indexed_head: &str,
    since: &str,
) -> (CodeintelResponse<CodeintelSuspect>, CodeintelSuspectScope) {
    let refusals: Vec<String> = report.unavailable.iter().map(|u| u.describe()).collect();
    let scope = CodeintelSuspectScope {
        indexed_head: indexed_head.to_string(),
        since: since.to_string(),
        cone_size: report.cone_size as u32,
        blamed_symbols: report.blamed_symbols as u32,
        refusals: refusals.clone(),
    };

    if let Some(fatal) = report.unavailable.iter().find(|u| is_fatal(u)) {
        return (CodeintelResponse::unavailable(fatal.describe()), scope);
    }

    let items: Vec<CodeintelSuspect> = report
        .suspects
        .into_iter()
        .map(|suspect| CodeintelSuspect {
            commit: suspect.commit,
            author: suspect.author,
            author_time: suspect.author_time,
            evidence: evidence_label(suspect.evidence).to_string(),
            score: suspect.score,
            nearest_distance: suspect.nearest_distance,
            touched: suspect
                .touched
                .into_iter()
                .map(|touch| CodeintelTouchedSymbol {
                    qualified_name: touch.qualified_name,
                    file_path: touch.file_path,
                    distance: touch.distance,
                    lines: touch.lines,
                    body_changed: touch.body_changed,
                })
                .collect(),
        })
        .collect();

    let total = items.len() as u32;
    let mut response = CodeintelResponse::ok(items, total, total, false);
    // A non-fatal refusal makes the list a lower bound, and this envelope
    // already has the field that says so. Leaving it unset would present a
    // partial answer as a complete one — the single failure this whole
    // analysis is built to avoid.
    if !refusals.is_empty() {
        response.walk_incomplete = Some(refusals.join("; "));
    }
    (response, scope)
}

/// A refusal reached before a window could be established.
///
/// `scope: None` is the honest shape for these: there is no indexed head, no
/// cone and nothing blamed, so reporting zeroes for them would be inventing
/// measurements of work that never started.
fn refused(reason: impl Into<String>) -> CodeintelSuspectsPayload {
    CodeintelSuspectsPayload {
        response: CodeintelResponse::unavailable(reason),
        scope: None,
    }
}

/// Rank the commits that could have caused `symptom`, between `since` and the
/// commit the index was built at.
pub fn suspects(
    repo_path: &str,
    symptom: &str,
    since: &str,
    depth: Option<u32>,
) -> CodeintelSuspectsPayload {
    // A blank `since` would make the window `..HEAD`, which git reads as every
    // commit ever — the opposite of the bounded question this asks. Refused
    // before any work, the same way the CLI refuses it.
    if since.trim().is_empty() {
        return refused(
            "`since` must name a revision: an empty window would blame the whole history",
        );
    }
    if symptom.trim().is_empty() {
        return refused("`symptom` must name a symbol");
    }
    // Bounded here rather than only in the MCP schema, because the Tauri
    // command reaches this with no schema between it and the caller. The cone
    // grows with depth and every file it reaches costs a `git blame`
    // subprocess, so one integer can otherwise ask for an unbounded amount of
    // work. Refused rather than clamped: silently answering a narrower question
    // than the one asked is how a caller comes to trust a list that was never
    // the list they requested.
    if let Some(depth) = depth {
        if depth == 0 || depth > MAX_CONE_DEPTH {
            return refused(format!(
                "`depth` must be between 1 and {MAX_CONE_DEPTH}; got {depth}"
            ));
        }
    }

    let store = match open_repo_map(repo_path) {
        Ok(store) => store,
        Err(reason) => return refused(reason),
    };
    let repo = std::path::Path::new(repo_path);
    let graph = match dc_regress_store::StoreGraph::new(&store, repo) {
        Ok(graph) => graph,
        Err(error) => return refused(format!("Could not read the code graph: {error}")),
    };
    // The index's own commit, not `HEAD`. See the module note: this is what
    // makes the spans and the blamed lines describe the same bytes.
    let Some(until) = graph.indexed_head().map(str::to_string) else {
        return refused(
            "This map records no commit it was built at, so blame lines cannot be matched \
             to graph spans. Rebuild the index inside a git checkout.",
        );
    };

    let report = dc_regress::suspects(
        repo,
        &graph,
        symptom,
        since,
        &until,
        depth.unwrap_or(dc_regress::DEFAULT_CONE_DEPTH),
    );
    let (response, scope) = to_response(report, &until, since);
    CodeintelSuspectsPayload {
        response,
        scope: Some(scope),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dc_regress::{EvidenceClass, Suspect, SuspectReport};

    fn report(suspects: Vec<Suspect>, unavailable: Vec<Unavailable>) -> SuspectReport {
        SuspectReport::new("sym".into(), suspects, 7, 5, unavailable)
    }

    fn suspect() -> Suspect {
        Suspect {
            commit: "abc123".into(),
            author: "Ada".into(),
            author_time: 1,
            evidence: EvidenceClass::BodyChanged,
            score: 10,
            nearest_distance: 0,
            touched: Vec::new(),
        }
    }

    /// The property the whole envelope exists for: an empty list that *is* the
    /// answer must not read like an empty list that is a failure.
    #[test]
    fn nothing_found_and_nothing_examined_do_not_read_alike() {
        let (found_nothing, _) = to_response(report(Vec::new(), Vec::new()), "head", "base");
        assert!(found_nothing.available, "an empty window is a finding");
        assert!(found_nothing.items.is_empty());
        assert!(found_nothing.walk_incomplete.is_none());

        let (refused, _) = to_response(
            report(
                Vec::new(),
                vec![Unavailable::SymptomNotFound {
                    symptom: "nope".into(),
                }],
            ),
            "head",
            "base",
        );
        assert!(
            !refused.available,
            "a symptom that resolved to nothing was not examined"
        );
        assert!(refused.reason.is_some(), "and it says why");
    }

    /// A partial answer is still worth showing, and must carry the reason it is
    /// partial. Silently dropping the refusal would present a lower bound as
    /// the whole list.
    #[test]
    fn a_narrowed_answer_is_returned_and_says_it_was_narrowed() {
        let (response, scope) = to_response(
            report(
                vec![suspect()],
                vec![Unavailable::ConeIncomplete {
                    depth: 3,
                    reached: 7,
                }],
            ),
            "head",
            "base",
        );
        assert!(
            response.available,
            "the suspects found are still reportable"
        );
        assert_eq!(response.items.len(), 1);
        assert!(
            response
                .walk_incomplete
                .as_deref()
                .is_some_and(|note| note.contains("depth")),
            "the refusal reaches the caller: {:?}",
            response.walk_incomplete
        );
        assert_eq!(scope.refusals.len(), 1);
    }

    /// A refusal that leaves nothing to report wins over any partial one, so
    /// the caller is never handed an empty `available: true` because the fatal
    /// reason happened to sort second.
    #[test]
    fn a_fatal_refusal_outranks_a_narrowing_one() {
        let (response, _) = to_response(
            report(
                Vec::new(),
                vec![
                    Unavailable::WindowCapped {
                        considered: 2000,
                        cap: 2000,
                    },
                    Unavailable::GitUnavailable {
                        reason: "no git".into(),
                    },
                ],
            ),
            "head",
            "base",
        );
        assert!(!response.available);
        assert!(
            response
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("git")),
            "the fatal reason is the one reported: {:?}",
            response.reason
        );
    }

    /// The scope is reported even when the answer is void, because "7 symbols
    /// in the cone, 5 blamed, and then git failed" is a different situation
    /// from one where nothing was ever examined.
    #[test]
    fn a_refused_report_still_says_what_it_had_examined() {
        let (_, scope) = to_response(
            report(
                Vec::new(),
                vec![Unavailable::GitUnavailable {
                    reason: "no git".into(),
                }],
            ),
            "deadbeef",
            "base",
        );
        assert_eq!(scope.cone_size, 7);
        assert_eq!(scope.blamed_symbols, 5);
        assert_eq!(scope.indexed_head, "deadbeef");
    }

    /// Every evidence class maps to a distinct string. Collapsing two of them
    /// would make the primary sort key unreadable in the UI while the ordering
    /// still obeyed it.
    #[test]
    fn each_evidence_class_has_its_own_label() {
        let labels = [
            evidence_label(EvidenceClass::BodyChanged),
            evidence_label(EvidenceClass::MovedOnly),
            evidence_label(EvidenceClass::Unknown),
        ];
        let unique: std::collections::BTreeSet<&str> = labels.iter().copied().collect();
        assert_eq!(unique.len(), labels.len(), "{labels:?}");
    }

    /// The bounds hold at this layer, not only in the MCP schema — the Tauri
    /// command reaches here with nothing between it and the caller.
    #[test]
    fn the_window_and_the_depth_are_bounded_before_any_work() {
        for (symptom, since, depth, expect) in [
            ("sym", "   ", None, "since"),
            ("", "base", None, "symptom"),
            ("sym", "base", Some(0), "depth"),
            ("sym", "base", Some(MAX_CONE_DEPTH + 1), "depth"),
        ] {
            let payload = suspects("/nonexistent-repo", symptom, since, depth);
            assert!(!payload.response.available, "{expect} was accepted");
            assert!(
                payload.scope.is_none(),
                "a run refused before it started reports no scope"
            );
            let reason = payload.response.reason.unwrap_or_default();
            assert!(
                reason.contains(expect),
                "the refusal names the argument: wanted {expect:?}, got {reason:?}"
            );
        }
    }

    /// The advertised maximum and the enforced one are the same number.
    #[test]
    fn the_depth_bound_matches_the_one_the_mcp_schema_advertises() {
        let tools = crate::mcp::tools();
        let schema = tools
            .iter()
            .find(|t| t["name"] == "gitpulse_codeintel_suspects")
            .expect("the tool is advertised");
        assert_eq!(
            schema["inputSchema"]["properties"]["depth"]["maximum"]
                .as_u64()
                .expect("depth declares a maximum"),
            u64::from(MAX_CONE_DEPTH),
            "the schema promises a bound this module does not enforce"
        );
    }
}
