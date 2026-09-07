//! Graph intelligence: the hubs a repository leans on, and its import cycles.
//!
//! This is the port of `src/devcouncil/indexing/graph/intel.py`'s
//! `god_nodes` and `circular_imports`, which reached `code_graph.json`'s
//! `meta` through `enrich_graph_intel`. That function lost both of its call
//! sites in `d232dea` when the Python graph builder was retired, and nothing
//! has called it since — so the two panels `viz.py:520-522` renders from those
//! keys have shown `(none)` on every graph written after the cutover,
//! regardless of what the repository contains.
//!
//! Recomputing them here rather than reviving the Python is the same decision
//! `d232dea` made: the kernel already holds the edges, and a second producer
//! of an artifact the kernel owns is exactly what that commit removed.
//!
//! `hotspots` is here too, and its shape is the reason it arrived late: it is
//! churn × coupling — `git log --since=90.days --name-only` scored against
//! fan-in — and the churn half is repository *history*, which this crate does
//! not and should not read. So the reading is `devmap-query`'s
//! (`inventory::churn`, one bounded subprocess) and the scoring is here, which
//! keeps this crate a pure function of what it is handed and puts one owner on
//! each half. Handing this function an empty churn map is not the same as
//! handing it none: the caller carries the reason and emits it, so an empty
//! `hotspots` from a repository with no history is never the same answer as an
//! empty one from a repository whose files nothing has touched.

use crate::dead_clusters::strongly_connected_components;
use std::collections::{BTreeMap, BTreeSet};

use devmap_extract::model::EdgeKind;
use devmap_resolve::model::ResolvedEdge;

/// Most ranked hubs to emit. The Python writer used 15 and `viz.py` slices to
/// 30; the smaller of the two is the one that ever bound the output.
pub const GOD_NODE_CAP: usize = 15;

/// Most import cycles to emit. `circular_imports` used 50 and `viz.py` slices
/// to 30, so 30 is what a reader could ever see.
pub const IMPORT_CYCLE_CAP: usize = 30;

/// Most hotspots to emit. The Python original's `top_n = 20`; `viz.py` slices
/// to 30, so 20 is the bound that ever bit.
pub const HOTSPOT_CAP: usize = 20;

/// One heavily-connected node, in the shape `viz.py:967` indexes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GodNode {
    pub id: String,
    pub path: String,
    pub name: String,
    pub degree: u32,
    pub fan_in: u32,
    pub fan_out: u32,
}

/// One strongly connected component of the file import graph.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ImportCycle {
    pub nodes: Vec<String>,
    pub length: usize,
}

/// One churn × coupling hotspot, in the shape `viz.py:963` indexes.
///
/// `score` is a `f64` rounded to two decimals rather than kept at full
/// precision, matching the Python original's `round(..., 2)`. The rounding is
/// part of the artifact's contract, not a display choice: the value is written
/// into JSON that two runs must render identically, and a 17-digit float whose
/// last digits depend on summation order is how that stops being true.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Hotspot {
    pub path: String,
    pub churn: u32,
    pub fan_in: u32,
    pub score: f64,
}

/// How often each file changed inside the churn window, and whether anyone
/// looked.
///
/// Defined here, where the score that consumes it is defined, rather than in
/// the `devmap-query` module that fills it: `devmap-analyze` cannot depend on
/// `devmap-query` (the dependency runs the other way), and a bare
/// `BTreeMap` parameter would have dropped exactly the `computed` bit this
/// whole change exists to carry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileChurn {
    /// Repo-relative path → commits touching it, within the window.
    pub commits_by_path: BTreeMap<String, u32>,
    /// Whether the history was read at all.
    pub computed: bool,
    /// Why it was not, when it was not.
    pub unavailable_reason: String,
    /// Whether a bound cut the history short, making the counts a lower bound.
    pub truncated: bool,
}

impl FileChurn {
    /// A reading that did not happen, with the reason attached.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            unavailable_reason: reason.into(),
            ..Self::default()
        }
    }
}

/// The whole report, each list with the bounds of its own cap.
#[derive(Debug, Clone, Default)]
pub struct GraphIntel {
    pub god_nodes: Vec<GodNode>,
    /// Ranked candidates before the cap. Carried so a reader never mistakes
    /// the sample for the population.
    pub god_nodes_total: usize,
    pub circular_imports: Vec<ImportCycle>,
    pub circular_imports_total: usize,
    pub hotspots: Vec<Hotspot>,
    /// Scored candidates before the cap.
    pub hotspots_total: usize,
    /// Whether the churn half was read. `false` means `hotspots` is empty
    /// because nothing looked, and [`Self::hotspots_unavailable_reason`] says
    /// why — the distinction `hotspots_computed` publishes.
    pub hotspots_computed: bool,
    pub hotspots_unavailable_reason: String,
    /// Whether a churn bound cut the history short.
    pub hotspots_churn_truncated: bool,
}

impl GraphIntel {
    pub fn god_nodes_truncated(&self) -> bool {
        self.god_nodes_total > self.god_nodes.len()
    }

    pub fn circular_imports_truncated(&self) -> bool {
        self.circular_imports_total > self.circular_imports.len()
    }

    pub fn hotspots_truncated(&self) -> bool {
        self.hotspots_total > self.hotspots.len()
    }
}

/// The `inferred` floor, matching `code_graph.rs`'s `INFERRED_FLOOR_MILLIS`.
///
/// Compared in milliconfidence rather than as a float, through the same
/// `confidence_millis` the graph writer uses, because an edge persisted at 0.4
/// reads back from SQLite as something a `>= 0.4` float comparison can reject.
/// Two surfaces disagreeing about which edges count is precisely what a shared
/// rounding helper exists to prevent.
const METRIC_FLOOR_MILLIS: i64 = 400;

/// Whether an edge should influence the ranking.
///
/// `Imports` and `Calls` only, and only at or above the `inferred` tier — the
/// same `{extracted, inferred}` set the Python original used. Its reason is
/// worth keeping: one ambiguous call fans out to every candidate, so admitting
/// those inflates exactly the hubs this ranking exists to find. An edge the
/// resolver declined to resolve is not evidence that a symbol is central.
fn is_metric_edge(edge: &ResolvedEdge) -> bool {
    matches!(edge.edge_kind, EdgeKind::Imports | EdgeKind::Calls)
        && devmap_extract::model::confidence_millis(edge.confidence.0) >= METRIC_FLOOR_MILLIS
}

/// A node's file, which is everything before the `::` in its id.
fn file_of(node_id: &str) -> &str {
    match node_id.find("::") {
        Some(at) => &node_id[..at],
        None => node_id,
    }
}

/// A node's own name, which is everything after the last `::`.
fn name_of(node_id: &str) -> &str {
    match node_id.rfind("::") {
        Some(at) => &node_id[at + 2..],
        None => node_id,
    }
}

/// Python package barrels. `__init__.py` files import their siblings and are
/// imported by them, so nearly every package is a cycle through one — true,
/// and not actionable. The Python original excluded them for the same reason
/// and counted them separately.
fn is_package_init(path: &str) -> bool {
    path.replace('\\', "/").rsplit('/').next() == Some("__init__.py")
}

/// Rank the graph's hubs, find its import cycles, and score its hotspots.
///
/// `known_files` is the set of paths this generation actually indexed. Churn
/// names files git knows about, which includes every deleted, ignored and
/// unindexable one; scoring those would put paths in the artifact that no other
/// list in it mentions, which is the Python original's `if path not in
/// file_paths` rule and the reason it exists.
pub fn graph_intel(
    edges: &[ResolvedEdge],
    churn: &FileChurn,
    known_files: &BTreeSet<&str>,
) -> GraphIntel {
    GraphIntel::default()
        .with_god_nodes(edges)
        .with_import_cycles(edges)
        .with_hotspots(edges, churn, known_files)
}

impl GraphIntel {
    fn with_god_nodes(mut self, edges: &[ResolvedEdge]) -> Self {
        // BTreeMap, not HashMap: the ranking is emitted into an artifact whose
        // bytes must be identical for two renderings of one generation (R4),
        // and a degree tie broken by hash order is exactly how that fails.
        let mut degree: BTreeMap<&str, u32> = BTreeMap::new();
        let mut fan_in: BTreeMap<&str, u32> = BTreeMap::new();
        let mut fan_out: BTreeMap<&str, u32> = BTreeMap::new();
        for edge in edges {
            if !is_metric_edge(edge) {
                continue;
            }
            let (source, target) = (edge.source_symbol.as_str(), edge.target_symbol.as_str());
            *degree.entry(source).or_insert(0) += 1;
            *degree.entry(target).or_insert(0) += 1;
            *fan_out.entry(source).or_insert(0) += 1;
            *fan_in.entry(target).or_insert(0) += 1;
        }
        // Test files are excluded, not merely down-weighted. A shared fixture
        // or mock accumulates enormous fan-in and would crowd out every real
        // hub, which is the failure the Python original names in its docstring.
        let mut ranked: Vec<(&str, u32)> = degree
            .into_iter()
            .filter(|(id, _)| !devmap_extract::wiring::is_test_path(file_of(id)))
            .collect();
        // By degree descending, then by id ascending. The second key is not
        // cosmetic: without it a tie is resolved by whatever order the map
        // yielded, and the artifact's bytes change under a reader with nothing
        // about the repository having changed.
        ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));

        self.god_nodes_total = ranked.len();
        self.god_nodes = ranked
            .into_iter()
            .take(GOD_NODE_CAP)
            .map(|(id, deg)| GodNode {
                id: id.to_string(),
                path: file_of(id).to_string(),
                name: name_of(id).to_string(),
                degree: deg,
                fan_in: fan_in.get(id).copied().unwrap_or(0),
                fan_out: fan_out.get(id).copied().unwrap_or(0),
            })
            .collect();
        self
    }

    fn with_import_cycles(mut self, edges: &[ResolvedEdge]) -> Self {
        // File-level import edges only. A symbol-level edge cannot make a file
        // cycle, and a self-import is not one either.
        let mut index_of: BTreeMap<&str, u32> = BTreeMap::new();
        let mut names: Vec<&str> = Vec::new();
        let mut pairs: BTreeSet<(u32, u32)> = BTreeSet::new();
        for edge in edges {
            if edge.edge_kind != EdgeKind::Imports {
                continue;
            }
            let (source, target) = (edge.source_file.as_str(), edge.target_file.as_str());
            if source == target || is_package_init(source) || is_package_init(target) {
                continue;
            }
            let source_index = match index_of.get(source) {
                Some(existing) => *existing,
                None => {
                    let next = names.len() as u32;
                    names.push(source);
                    index_of.insert(source, next);
                    next
                }
            };
            let target_index = match index_of.get(target) {
                Some(existing) => *existing,
                None => {
                    let next = names.len() as u32;
                    names.push(target);
                    index_of.insert(target, next);
                    next
                }
            };
            pairs.insert((source_index, target_index));
        }
        let mut adjacency: Vec<Vec<u32>> = vec![Vec::new(); names.len()];
        for (source, target) in pairs {
            adjacency[source as usize].push(target);
        }

        let mut components: Vec<Vec<String>> = strongly_connected_components(&adjacency)
            .into_iter()
            // A component of one is a file, not a cycle.
            .filter(|component| component.len() >= 2)
            .map(|component| {
                let mut members: Vec<String> = component
                    .into_iter()
                    .map(|index| names[index as usize].to_string())
                    .collect();
                members.sort();
                members
            })
            .collect();
        // Smallest first: a two-file cycle is the one somebody can actually
        // break, and a fifty-file component is a fact about the architecture.
        // Tie-broken by contents so the order is not the traversal's.
        components
            .sort_by(|left, right| left.len().cmp(&right.len()).then_with(|| left.cmp(right)));

        self.circular_imports_total = components.len();
        self.circular_imports = components
            .into_iter()
            .take(IMPORT_CYCLE_CAP)
            .map(|nodes| ImportCycle {
                length: nodes.len(),
                nodes,
            })
            .collect();
        self
    }

    /// Churn × coupling: how often a file changes, weighted by how much of the
    /// repository would feel it if it changed again.
    ///
    /// `score = commits * (1 + ln(1 + fan_in))`, the Python original's
    /// `count * (1 + math.log1p(fi))`. The logarithm is what stops the ranking
    /// being fan-in alone on a repository with one enormous hub: doubling a
    /// file's importers moves it much less than doubling how often it is
    /// rewritten, which is the "refactor risk" this metric is for.
    fn with_hotspots(
        mut self,
        edges: &[ResolvedEdge],
        churn: &FileChurn,
        known_files: &BTreeSet<&str>,
    ) -> Self {
        self.hotspots_computed = churn.computed;
        self.hotspots_unavailable_reason = churn.unavailable_reason.clone();
        self.hotspots_churn_truncated = churn.truncated;
        if !churn.computed {
            return self;
        }

        // File-level import fan-in, deduplicated by (importer, imported): a
        // file that imports five symbols from another is one importer of it,
        // not five. The Python original reached the same number by counting
        // only edges whose *both* endpoints were file nodes; this counts the
        // distinct file pairs behind every import edge, which is the same
        // question asked of a model that carries the file on every edge.
        let mut pairs: BTreeSet<(&str, &str)> = BTreeSet::new();
        for edge in edges {
            if edge.edge_kind != EdgeKind::Imports {
                continue;
            }
            let (source, target) = (edge.source_file.as_str(), edge.target_file.as_str());
            if source == target {
                continue;
            }
            pairs.insert((source, target));
        }
        let mut fan_in: BTreeMap<&str, u32> = BTreeMap::new();
        for (_, target) in pairs {
            *fan_in.entry(target).or_insert(0) += 1;
        }

        let mut scored: Vec<Hotspot> = churn
            .commits_by_path
            .iter()
            // Only files this generation indexed. Churn names every path git
            // touched, including ones deleted since and ones no extractor can
            // read; a hotspot the rest of the artifact has never heard of is
            // not actionable.
            .filter(|(path, _)| known_files.contains(path.as_str()))
            .map(|(path, commits)| {
                let inbound = fan_in.get(path.as_str()).copied().unwrap_or(0);
                let raw = f64::from(*commits) * (1.0 + f64::from(inbound).ln_1p());
                Hotspot {
                    path: path.clone(),
                    churn: *commits,
                    fan_in: inbound,
                    // Two decimals, as the Python original rounded, so two
                    // renderings of one generation are the same bytes.
                    score: (raw * 100.0).round() / 100.0,
                }
            })
            .collect();
        // By score descending, then by path ascending. The second key is not
        // cosmetic: the list is cut at HOTSPOT_CAP, and a tie broken by
        // traversal order changes the artifact's bytes with nothing about the
        // repository having changed.
        scored.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.path.cmp(&right.path))
        });
        self.hotspots_total = scored.len();
        scored.truncate(HOTSPOT_CAP);
        self.hotspots = scored;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_two_node_cycle_is_one_component() {
        let components = strongly_connected_components(&[vec![1], vec![0]]);
        assert_eq!(components.len(), 1);
        assert_eq!(components[0].len(), 2);
    }

    #[test]
    fn a_chain_is_all_singletons() {
        let components = strongly_connected_components(&[vec![1], vec![2], vec![]]);
        assert_eq!(components.len(), 3);
        assert!(components.iter().all(|component| component.len() == 1));
    }

    #[test]
    fn a_long_chain_does_not_overflow_the_stack() {
        // The reason this walk is iterative. A recursive Tarjan on a chain this
        // long overflows a default thread stack.
        let depth = 200_000usize;
        let mut adjacency: Vec<Vec<u32>> = Vec::with_capacity(depth);
        for index in 0..depth {
            adjacency.push(if index + 1 < depth {
                vec![index as u32 + 1]
            } else {
                Vec::new()
            });
        }
        assert_eq!(strongly_connected_components(&adjacency).len(), depth);
    }

    #[test]
    fn package_inits_are_recognised_on_both_separators() {
        assert!(is_package_init("a/b/__init__.py"));
        assert!(is_package_init("a\\b\\__init__.py"));
        assert!(!is_package_init("a/b/init.py"));
    }

    /// A file-level import edge, in the shape the resolver emits.
    fn import_edge(source: &str, target: &str) -> ResolvedEdge {
        ResolvedEdge {
            source_file: source.to_string(),
            target_file: target.to_string(),
            source_symbol: source.to_string(),
            target_symbol: target.to_string(),
            edge_kind: EdgeKind::Imports,
            confidence: devmap_extract::model::Confidence::DETERMINISTIC,
            resolution: None,
            details: None,
            evidence: None,
        }
    }

    fn churn_of(pairs: &[(&str, u32)]) -> FileChurn {
        FileChurn {
            commits_by_path: pairs
                .iter()
                .map(|(path, count)| ((*path).to_string(), *count))
                .collect(),
            computed: true,
            unavailable_reason: String::new(),
            truncated: false,
        }
    }

    #[test]
    fn an_unread_history_produces_no_hotspots_and_keeps_its_reason() {
        let churn = FileChurn::unavailable("not a git repository");
        let known: BTreeSet<&str> = ["a.py"].into_iter().collect();
        let intel = graph_intel(&[], &churn, &known);
        assert!(!intel.hotspots_computed);
        assert_eq!(intel.hotspots_unavailable_reason, "not a git repository");
        assert!(intel.hotspots.is_empty());
        assert_eq!(intel.hotspots_total, 0);
    }

    #[test]
    fn a_read_history_with_nothing_in_it_is_a_computed_empty_answer() {
        // The distinction the marker exists for: this is *not* the same state
        // as the test above, and the artifact must not render them alike.
        let intel = graph_intel(&[], &churn_of(&[]), &BTreeSet::new());
        assert!(intel.hotspots_computed);
        assert!(intel.hotspots.is_empty());
    }

    #[test]
    fn a_churned_path_the_generation_never_indexed_is_not_a_hotspot() {
        let churn = churn_of(&[("deleted.py", 40), ("kept.py", 1)]);
        let known: BTreeSet<&str> = ["kept.py"].into_iter().collect();
        let intel = graph_intel(&[], &churn, &known);
        assert_eq!(intel.hotspots_total, 1);
        assert_eq!(intel.hotspots[0].path, "kept.py");
    }

    #[test]
    fn the_score_is_commits_times_one_plus_log1p_of_fan_in() {
        let churn = churn_of(&[("hub.py", 4)]);
        let known: BTreeSet<&str> = ["hub.py"].into_iter().collect();
        // Three distinct importers, one of them importing twice: fan-in is the
        // number of files, not the number of edges.
        let edges: Vec<ResolvedEdge> = [
            ("a.py", "hub.py"),
            ("b.py", "hub.py"),
            ("c.py", "hub.py"),
            ("a.py", "hub.py"),
        ]
        .into_iter()
        .map(|(source, target)| import_edge(source, target))
        .collect();
        let intel = graph_intel(&edges, &churn, &known);
        assert_eq!(intel.hotspots[0].fan_in, 3);
        let expected = (4.0f64 * (1.0 + 3.0f64.ln_1p()) * 100.0).round() / 100.0;
        assert_eq!(intel.hotspots[0].score, expected);
    }

    #[test]
    fn a_tie_is_broken_by_path_so_two_renderings_agree() {
        let churn = churn_of(&[("b.py", 3), ("a.py", 3), ("c.py", 3)]);
        let known: BTreeSet<&str> = ["a.py", "b.py", "c.py"].into_iter().collect();
        let intel = graph_intel(&[], &churn, &known);
        let paths: Vec<&str> = intel.hotspots.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec!["a.py", "b.py", "c.py"]);
    }

    #[test]
    fn a_capped_hotspot_list_still_reports_the_population() {
        let pairs: Vec<(String, u32)> = (0..HOTSPOT_CAP + 7)
            .map(|index| (format!("f{index:03}.py"), index as u32 + 1))
            .collect();
        let churn = FileChurn {
            commits_by_path: pairs.iter().cloned().collect(),
            computed: true,
            ..Default::default()
        };
        let known: BTreeSet<&str> = pairs.iter().map(|(path, _)| path.as_str()).collect();
        let intel = graph_intel(&[], &churn, &known);
        assert_eq!(intel.hotspots.len(), HOTSPOT_CAP);
        assert_eq!(intel.hotspots_total, HOTSPOT_CAP + 7);
        assert!(intel.hotspots_truncated());
        // Ranked before cut: the most-churned file survives the cap.
        assert_eq!(
            intel.hotspots[0].path,
            format!("f{:03}.py", HOTSPOT_CAP + 6)
        );
    }

    #[test]
    fn identity_helpers_split_on_the_symbol_separator() {
        assert_eq!(file_of("a/b.py::Klass::method"), "a/b.py");
        assert_eq!(name_of("a/b.py::Klass::method"), "method");
        assert_eq!(file_of("a/b.py"), "a/b.py");
        assert_eq!(name_of("a/b.py"), "a/b.py");
    }
}
