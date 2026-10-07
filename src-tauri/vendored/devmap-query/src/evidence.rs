//! An `ask` answer shaped for reading: files first, then the source.
//!
//! `ask` already returns each hit's verbatim, hash-verified source. What it
//! does not return is the shape a reader starts from: which *files* the answer
//! lives in, which of them are tests, and how the hits relate to each other.
//! A flat list of symbols leaves the caller to rebuild that, usually by opening
//! the files the hits already quoted.
//!
//! This module rebuilds it from what the map already holds, and nothing else:
//!
//!   - **Files in rank order.** A file sits where its best hit sits.
//!   - **Role.** Per unit: `test` when its file is a test path
//!     ([`is_test_path`]) or a test runner invokes it — a `#[test] fn`, a
//!     pytest `test_*`, a JUnit `@Test`, as the extractor recorded them —
//!     otherwise `implementation`. Per file: `test` when the path is a test
//!     path or every unit in it is a test. Neither reads content: a helper
//!     that only tests call is still `implementation`.
//!   - **Relations.** The admitted call edges *between hits*, as `calls` and
//!     `called_by` on each unit. Only edges at or above the same
//!     `min_confidence` floor the ask walk used, so the pack never shows a
//!     relation the ranking was not allowed to use.
//!   - **Folding.** A hit whose lines sit inside another hit's complete source
//!     keeps its lead and relations but not a second copy of the text;
//!     `contained_in` names the unit that prints it — the outermost one, when
//!     containers nest, since an intermediate container is folded too. Two hits
//!     with identical lines fold the lower-ranked into the higher. A container
//!     whose source was capped does not fold anything — the inner text may be
//!     the part that was cut.
//!   - **Related tests.** Test files that reach the implementation hits over
//!     inbound call edges, from the same walk `affected_tests` runs. Test files
//!     already in `files` are not repeated.
//!
//! Nothing here reads a file or re-ranks: order and scores are `ask`'s. The
//! page is budgeted by [`fold_aware_take`] rather than `ask`'s take, so a hit
//! an earlier hit already prints costs only its lead and the same budget can
//! hold more hits; `tokens_used` is recomputed after folding, so it describes
//! what the pack actually carries.

use std::collections::{BTreeSet, HashMap, HashSet};

use devmap_extract::model::EdgeKind;
use devmap_store::GenerationEdges;
use serde::{Deserialize, Serialize};

use crate::cancel::{Cancel, QueryCancelled};
use crate::engine::{budget_take, is_test_path, search_hit_tokens, SEARCH_HIT_OVERHEAD_TOKENS};
use crate::model::{AffectedTest, ResolutionAvailability, Response, SourceFreshness, SymbolHit};

/// What a file is to the question, by path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceRole {
    Implementation,
    Test,
}

impl EvidenceRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Implementation => "implementation",
            Self::Test => "test",
        }
    }
}

/// One `ask` hit, with its place among the other hits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceUnit {
    #[serde(flatten)]
    pub hit: SymbolHit,
    pub qualified_name: String,
    pub role: EvidenceRole,
    /// Qualified names of other hits this one calls, over admitted edges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<String>,
    /// Qualified names of other hits that call this one, over admitted edges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub called_by: Vec<String>,
    /// Set when this unit's lines are inside another unit's complete source,
    /// which then carries the text. `hit.source_span` is empty when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contained_in: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceFile {
    pub file_path: String,
    pub role: EvidenceRole,
    /// The best score among this file's units: the file's rank.
    pub score: f32,
    /// Units in line order, which is the order a reader meets them.
    pub units: Vec<EvidenceUnit>,
}

/// An `ask` answer grouped by file. Envelope fields are `ask`'s, unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidencePack {
    pub files: Vec<EvidenceFile>,
    /// Test files reaching the implementation hits over inbound call edges,
    /// nearest first, excluding test files already in `files`. Its own
    /// counters and `walk_incomplete` describe it; they are not the hits'.
    pub related_tests: Response<AffectedTest>,
    /// Where calls the resolver could not bind touch *this* pack: sites inside
    /// a hit (its `calls` may fall short) and sites naming a symbol the
    /// related-test walk reached (`called_by` and `related_tests` may), plus
    /// any corpus-level hole. Stated once for the pack rather than repeated on
    /// each list; `None` when the ledger holds no such site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage_gap: Option<String>,
    pub source_freshness: SourceFreshness,
    pub shown: u32,
    pub hidden: u32,
    pub total: u32,
    pub truncated: bool,
    pub tokens_used: u32,
    pub resolution: ResolutionAvailability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub walk_incomplete: Option<String>,
    /// The scope the hits were ranked within; see [`crate::Response::scope`].
    /// `related_tests` is restricted to it too, and
    /// `related_tests_outside_scope` counts what that left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<crate::scope::ScopeReport>,
}

/// Build the pack from an `ask` response and its index-aligned qualified names.
///
/// `edges` is `None` when the generation has no edge index; the pack is then
/// the same files and units with no relations, which is what the map knows.
/// `related_tests` is attached as given; its tokens are added to the pack's.
pub fn assemble(
    response: Response<SymbolHit>,
    qualified: &[String],
    edges: Option<&GenerationEdges>,
    min_confidence: f32,
    test_symbols: &HashSet<String>,
    related_tests: Response<AffectedTest>,
    cancel: &Cancel,
) -> Result<EvidencePack, QueryCancelled> {
    debug_assert_eq!(response.items.len(), qualified.len());
    let Response {
        source_freshness,
        items,
        shown,
        hidden,
        total,
        truncated,
        resolution,
        walk_incomplete,
        scope,
        ..
    } = response;

    let mut units: Vec<EvidenceUnit> = items
        .into_iter()
        .zip(qualified.iter().cloned())
        .map(|(hit, qualified_name)| EvidenceUnit {
            role: if is_test_path(&hit.file_path) || test_symbols.contains(&qualified_name) {
                EvidenceRole::Test
            } else {
                EvidenceRole::Implementation
            },
            hit,
            qualified_name,
            calls: Vec::new(),
            called_by: Vec::new(),
            contained_in: None,
        })
        .collect();

    if let Some(edges) = edges {
        relate(&mut units, edges, min_confidence, cancel)?;
    }
    fold_nested(&mut units);
    let tokens_used = units
        .iter()
        .map(|unit| search_hit_tokens(&unit.hit))
        .fold(related_tests.tokens_used, u32::saturating_add);

    // Group in rank order: a file's position is its first (best) unit's.
    let mut order: Vec<String> = Vec::new();
    let mut by_file: HashMap<String, Vec<EvidenceUnit>> = HashMap::new();
    for unit in units {
        let path = unit.hit.file_path.clone();
        if !by_file.contains_key(&path) {
            order.push(path.clone());
        }
        by_file.entry(path).or_default().push(unit);
    }
    let files = order
        .into_iter()
        .map(|file_path| {
            let mut units = by_file.remove(&file_path).unwrap_or_default();
            let score = units
                .iter()
                .map(|unit| unit.hit.score)
                .fold(f32::NEG_INFINITY, f32::max);
            units.sort_by(|a, b| {
                a.hit
                    .span
                    .0
                    .cmp(&b.hit.span.0)
                    .then(b.hit.span.1.cmp(&a.hit.span.1))
                    .then(a.qualified_name.cmp(&b.qualified_name))
            });
            let role = if is_test_path(&file_path)
                || units.iter().all(|unit| unit.role == EvidenceRole::Test)
            {
                EvidenceRole::Test
            } else {
                EvidenceRole::Implementation
            };
            EvidenceFile {
                file_path,
                role,
                score,
                units,
            }
        })
        .collect();

    Ok(EvidencePack {
        files,
        related_tests,
        coverage_gap: None,
        source_freshness,
        shown,
        hidden,
        total,
        truncated,
        tokens_used,
        resolution,
        walk_incomplete,
        scope,
    })
}

/// Attach the admitted call edges whose two ends are both hits.
///
/// Joined on `(file, qualified name)`, not the name alone: the same qualified
/// name in two files is two symbols, and an edge between one pair must not be
/// reported for the other.
fn relate(
    units: &mut [EvidenceUnit],
    edges: &GenerationEdges,
    min_confidence: f32,
    cancel: &Cancel,
) -> Result<(), QueryCancelled> {
    let index: HashMap<(&str, &str), usize> = units
        .iter()
        .enumerate()
        .map(|(i, unit)| {
            (
                (unit.hit.file_path.as_str(), unit.qualified_name.as_str()),
                i,
            )
        })
        .collect();
    let mut calls: Vec<BTreeSet<String>> = vec![BTreeSet::new(); units.len()];
    let mut called_by: Vec<BTreeSet<String>> = vec![BTreeSet::new(); units.len()];
    for id in 0..edges.len() as u32 {
        cancel.check_every(id as usize)?;
        if edges.kind(id) != EdgeKind::Calls || !edges.admits(id, min_confidence) {
            continue;
        }
        let Some(&from) = index.get(&(edges.source_file(id), edges.source_symbol(id))) else {
            continue;
        };
        let Some(&to) = index.get(&(edges.target_file(id), edges.target_symbol(id))) else {
            continue;
        };
        if from == to {
            continue;
        }
        calls[from].insert(units[to].qualified_name.clone());
        called_by[to].insert(units[from].qualified_name.clone());
    }
    for (unit, (calls, called_by)) in units.iter_mut().zip(calls.into_iter().zip(called_by)) {
        unit.calls = calls.into_iter().collect();
        unit.called_by = called_by.into_iter().collect();
    }
    Ok(())
}

/// Whether a hit prints its symbol's complete source, and so can stand in for
/// any hit whose lines it contains.
///
/// One definition for both places that ask: [`fold_aware_take`], which charges
/// a contained hit only its lead, and [`fold_nested`], which then drops that
/// hit's text. If they disagreed, a hit charged as folded could print in full
/// and the pack would exceed its budget.
pub(crate) fn shows_whole(hit: &SymbolHit) -> bool {
    hit.source_unavailable_reason.is_none()
        && hit.source_span_omitted_bytes.is_none()
        && !hit.source_span.is_empty()
        && hit.span != (0, 0)
}

/// Whether `outer`'s lines contain `inner`'s, in the same file.
pub(crate) fn encloses(outer: &SymbolHit, inner: &SymbolHit) -> bool {
    inner.span != (0, 0)
        && outer.file_path == inner.file_path
        && outer.span.0 <= inner.span.0
        && inner.span.1 <= outer.span.1
}

/// Budget a ranked page as the pack prints it.
///
/// `budget_take`'s rule — a prefix, stopping at the first hit that does not
/// fit — with one change of price: a hit enclosed by an *earlier accepted* hit
/// that [`shows_whole`] costs only [`SEARCH_HIT_OVERHEAD_TOKENS`], since
/// [`fold_nested`] is certain to fold it (it finds a container whenever one
/// qualifies, and an earlier one qualifies even when the spans are equal). A
/// class's methods then cost their names, not a second copy of the class, and
/// the budget buys more hits. Folding may still find containers accepted
/// *after* a hit; that only makes the pack smaller than what was charged.
///
/// A whole-file hit whose text was capped becomes a lead first
/// ([`file_hit_as_lead`]): its text is the file's first lines, and a capped
/// span can stand in for nothing.
pub fn fold_aware_take(hits: Vec<SymbolHit>, token_budget: u32) -> Response<SymbolHit> {
    let mut accepted: Vec<SymbolHit> = Vec::new();
    let mut used = 0u32;
    let mut truncated = false;
    for mut hit in hits {
        file_hit_as_lead(&mut hit);
        let folds = accepted
            .iter()
            .any(|outer| shows_whole(outer) && encloses(outer, &hit));
        let cost = if folds {
            SEARCH_HIT_OVERHEAD_TOKENS
        } else {
            search_hit_tokens(&hit)
        };
        if cost > token_budget.saturating_sub(used) {
            truncated = true;
            break;
        }
        used += cost;
        accepted.push(hit);
    }
    let mut response = budget_take(accepted, u32::MAX, |_| 0);
    response.tokens_used = used;
    response.truncated = truncated;
    response
}

/// Turn a capped whole-file hit into a lead: no text, every byte reported as
/// not shown.
///
/// The cap keeps a file's first quarter-budget of bytes — imports and a module
/// comment — which rarely answers anything and spent up to a quarter of the
/// pack. The path stays, as the lead to read; `source_span_omitted_bytes` says
/// the whole symbol was withheld, so an empty span is never mistaken for an
/// empty file. An uncapped file hit keeps its text: it is complete, and it can
/// fold every other hit in the file.
fn file_hit_as_lead(hit: &mut SymbolHit) {
    let Some(omitted) = hit.source_span_omitted_bytes else {
        return;
    };
    if hit.kind != "File" {
        return;
    }
    let whole = u32::try_from(hit.source_span.len())
        .unwrap_or(u32::MAX)
        .saturating_add(omitted);
    hit.source_span.clear();
    hit.source_span_omitted_bytes = Some(whole);
}

/// Drop the second copy of text a containing unit already shows.
///
/// The container must have its complete source (available and not capped).
/// Among several candidates the tightest is chosen, then the chain is followed
/// outward: when the class holding a method is itself inside a shown file, the
/// class is folded too, and the method must point at the file — the block that
/// is actually printed. Identical spans fold the later (lower-ranked) unit into
/// the earlier, which keeps every chain acyclic: each step goes to a strictly
/// larger span or to a strictly earlier unit.
fn fold_nested(units: &mut [EvidenceUnit]) {
    let container: Vec<Option<usize>> = units
        .iter()
        .enumerate()
        .map(|(i, inner)| {
            units
                .iter()
                .enumerate()
                .filter(|&(j, outer)| {
                    j != i
                        && shows_whole(&outer.hit)
                        && encloses(&outer.hit, &inner.hit)
                        && (outer.hit.span != inner.hit.span || j < i)
                })
                .min_by_key(|&(j, outer)| (outer.hit.span.1 - outer.hit.span.0, j))
                .map(|(j, _)| j)
        })
        .collect();
    for i in 0..units.len() {
        let Some(mut shown) = container[i] else {
            continue;
        };
        while let Some(next) = container[shown] {
            shown = next;
        }
        let name = units[shown].qualified_name.clone();
        units[i].hit.source_span.clear();
        units[i].hit.source_span_omitted_bytes = None;
        units[i].contained_in = Some(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(path: &str, name: &str, span: (u32, u32), source: &str, score: f32) -> SymbolHit {
        SymbolHit {
            symbol_name: name.to_string(),
            file_path: path.to_string(),
            kind: "function".to_string(),
            span,
            source_span: source.to_string(),
            source_unavailable_reason: None,
            source_span_omitted_bytes: None,
            source_indent: None,
            score,
        }
    }

    fn unit(hit: SymbolHit) -> EvidenceUnit {
        let qualified_name = format!("{}::{}", hit.file_path, hit.symbol_name);
        EvidenceUnit {
            hit,
            qualified_name,
            role: EvidenceRole::Implementation,
            calls: Vec::new(),
            called_by: Vec::new(),
            contained_in: None,
        }
    }

    #[test]
    fn a_method_inside_a_shown_class_is_folded_into_it() {
        let mut units = vec![
            unit(hit("a.py", "Cache", (1, 10), "class Cache: ...", 0.9)),
            unit(hit("a.py", "get", (3, 5), "def get(self): ...", 0.8)),
        ];
        fold_nested(&mut units);
        assert_eq!(units[1].contained_in.as_deref(), Some("a.py::Cache"));
        assert!(units[1].hit.source_span.is_empty());
        assert!(units[0].contained_in.is_none());
    }

    #[test]
    fn nested_containers_point_at_the_block_that_is_printed() {
        let mut units = vec![
            unit(hit("a.py", "a.py", (1, 40), "whole file", 0.9)),
            unit(hit("a.py", "Cache", (5, 20), "class Cache: ...", 0.8)),
            unit(hit("a.py", "get", (7, 9), "def get(self): ...", 0.7)),
        ];
        fold_nested(&mut units);
        assert!(units[0].contained_in.is_none());
        assert_eq!(units[1].contained_in.as_deref(), Some("a.py::a.py"));
        assert_eq!(
            units[2].contained_in.as_deref(),
            Some("a.py::a.py"),
            "the class is folded, so the method must name the file"
        );
    }

    #[test]
    fn identical_spans_fold_the_lower_ranked_into_the_higher() {
        let mut units = vec![
            unit(hit("a.py", "handler", (3, 5), "def handler(): ...", 0.9)),
            unit(hit("a.py", "route", (3, 5), "def handler(): ...", 0.4)),
        ];
        fold_nested(&mut units);
        assert!(units[0].contained_in.is_none());
        assert_eq!(units[1].contained_in.as_deref(), Some("a.py::handler"));
    }

    #[test]
    fn a_capped_container_folds_nothing() {
        let mut outer = hit("a.py", "Cache", (1, 400), "class Cache: ...", 0.9);
        outer.source_span_omitted_bytes = Some(9_000);
        let mut units = vec![
            unit(outer),
            unit(hit("a.py", "get", (300, 310), "def get(self): ...", 0.8)),
        ];
        fold_nested(&mut units);
        assert!(units[1].contained_in.is_none(), "{:?}", units[1]);
        assert!(!units[1].hit.source_span.is_empty());
    }

    #[test]
    fn same_lines_in_another_file_do_not_fold() {
        let mut units = vec![
            unit(hit("a.py", "Cache", (1, 10), "class Cache: ...", 0.9)),
            unit(hit("b.py", "get", (3, 5), "def get(self): ...", 0.8)),
        ];
        fold_nested(&mut units);
        assert!(units[1].contained_in.is_none());
    }

    #[test]
    fn files_keep_rank_order_and_units_read_top_down() {
        let response = crate::engine::budget_take(
            vec![
                hit("src/b.py", "late", (20, 22), "def late(): ...", 0.9),
                hit(
                    "tests/test_b.py",
                    "test_late",
                    (1, 3),
                    "def test_late(): ...",
                    0.7,
                ),
                hit("src/b.py", "early", (1, 3), "def early(): ...", 0.5),
            ],
            10_000,
            |_| 1,
        );
        let qualified = vec![
            "b.late".to_string(),
            "test_b.test_late".to_string(),
            "b.early".to_string(),
        ];
        let pack = assemble(
            response,
            &qualified,
            None,
            1.0,
            &HashSet::from(["b.late".to_string()]),
            crate::engine::budget_take(Vec::new(), 0, |_| 0),
            &Cancel::default(),
        )
        .unwrap();
        let paths: Vec<&str> = pack.files.iter().map(|f| f.file_path.as_str()).collect();
        assert_eq!(paths, ["src/b.py", "tests/test_b.py"]);
        assert_eq!(pack.files[0].role, EvidenceRole::Implementation);
        let roles: Vec<EvidenceRole> = pack.files[0].units.iter().map(|u| u.role).collect();
        assert_eq!(
            roles,
            [EvidenceRole::Implementation, EvidenceRole::Test],
            "a runner-invoked unit is a test beside the code it tests"
        );
        assert_eq!(pack.files[1].role, EvidenceRole::Test);
        assert_eq!(pack.files[0].score, 0.9);
        let names: Vec<&str> = pack.files[0]
            .units
            .iter()
            .map(|u| u.hit.symbol_name.as_str())
            .collect();
        assert_eq!(names, ["early", "late"]);
        assert_eq!(pack.shown, 3);
        let expected: u32 = pack
            .files
            .iter()
            .flat_map(|file| &file.units)
            .map(|unit| search_hit_tokens(&unit.hit))
            .sum();
        assert_eq!(pack.tokens_used, expected, "tokens are the pack's own");
    }
}
