//! The store behind `dc_regress::CodeGraph`.
//!
//! `dc-regress` answers "which commits could have caused this" from a graph it
//! is handed, deliberately knowing nothing about how that graph is stored. This
//! crate is the one place that knows both — it reads the persisted generation
//! and presents it through the trait.
//!
//! # Why this is a crate and not a module
//!
//! It began as a private module inside `devmap-cli`, which is a *binary*: no
//! other host could reach it. GitPulse links the same `devmap-store` and wants
//! the same answer, so it would have had to write a second adapter — and the
//! two would have had to agree, silently, about the cone direction, the walk
//! budget and how a symptom name resolves. The cone direction alone was already
//! wrong once here (see [`CodeGraph::cone`]); getting it wrong in one of two
//! copies is the shape where one host answers `[]` and nobody can say why.
//!
//! It is also not a feature of `dc-regress`. That crate's manifest states the
//! reason: `CodeGraph` is a trait precisely so the analysis has no dependency
//! on a store and no configuration that compiles differently. An optional
//! feature there would have taken that away. An adapter crate depends on both
//! and neither depends on it, which leaves both ends as they were.
//!
//! # Where the basis comes from
//!
//! The analysis is only sound if the byte offsets in the graph and the lines in
//! the blame describe the same content, and the store does not record a git
//! blob id per file. What it does record is the `head_sha` its generation was
//! built at. So this adapter resolves `<head_sha>:<path>` through git to get the
//! blob id of exactly the content that was indexed, and the caller runs the
//! analysis with `until = head_sha`. Both halves then name the same object by
//! construction, and `dc_regress::join::attribute` can check it rather than
//! assume it.
//!
//! When the index was built over a dirty working tree, the committed blob is
//! *not* what was indexed. That is exactly the case the basis check exists to
//! catch, and it surfaces as a `BlobMismatch` rather than as a wrong answer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use dc_regress::{AffectedTestFile, BlobIdentity, CodeGraph, ConeEntry, GraphSymbol};
use devmap_query::model::Request;
use devmap_query::StoreQueryEngine;
use devmap_store::Store;

/// Token budget for the cone walk.
///
/// Generous relative to an interactive query: a truncated cone silently
/// narrows the suspect list, which is the failure this whole crate is built to
/// avoid, so the budget is set where the walk is bounded by its depth rather
/// than by its allowance.
const CONE_BUDGET: u32 = 60_000;

/// Token budget for one chunk of the affected-test walk.
///
/// Same reasoning as [`CONE_BUDGET`]: a budget-trimmed test list is a list
/// missing exactly the tests nobody will run, and the report can only say the
/// list is short — not which tests fell off it.
const TESTS_BUDGET: u32 = 60_000;

pub struct StoreGraph<'a> {
    store: &'a Store,
    repo: PathBuf,
    /// The commit the generation was built at. `None` when the store never
    /// recorded one, which makes every basis `Unknown` and every file refuse —
    /// loudly, which is the intent.
    head_sha: Option<String>,
    /// Symbols by file, read once. The alternative is a full table scan per
    /// file in the cone, and the cone is chosen precisely because it spans
    /// several files.
    ///
    /// **File nodes are not in here.** The store holds one node per file whose
    /// span is the entire file and whose id is the bare path, and presenting
    /// it as a symbol is not a cosmetic wart: every line of every change falls
    /// inside it, so it matches everything and `blast` can never report a line
    /// as landing outside every symbol. A changed import, a changed top-level
    /// constant and a changed license header would all be silently credited to
    /// "the file", and the report would claim to have placed lines it had not
    /// placed. They are kept in [`Self::file_nodes`] for resolution, where
    /// naming a whole file is a legitimate thing to ask for.
    by_file: HashMap<String, Vec<GraphSymbol>>,
    /// Node ids that name a file rather than a symbol in one.
    ///
    /// Retained so `resolve_symptom` still accepts a path — asking about a
    /// whole file is a real question — without letting the file node reach
    /// [`CodeGraph::symbols_in`].
    file_nodes: std::collections::BTreeSet<String>,
}

impl<'a> StoreGraph<'a> {
    pub fn new(store: &'a Store, repo: &Path) -> anyhow::Result<Self> {
        let head_sha = store.latest_generation_head_sha()?;
        let mut by_file: HashMap<String, Vec<GraphSymbol>> = HashMap::new();
        let mut file_nodes: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for symbol in store.all_symbols()? {
            // The node-id shape decides, not the stored `kind`: `file_of` and
            // the cone walk already agree that an id without `::` names a
            // file, and a second rule here could disagree with them about the
            // same node. One convention, checked in one way.
            if file_of(&symbol.qualified_name).is_none() {
                file_nodes.insert(symbol.qualified_name);
                continue;
            }
            by_file
                .entry(symbol.path.clone())
                .or_default()
                .push(GraphSymbol {
                    qualified_name: symbol.qualified_name,
                    file_path: symbol.path,
                    span_start: symbol.span_start,
                    span_end: symbol.span_end,
                    // The store keeps two generations, which is not enough to
                    // compare a symbol's body signature across an arbitrary
                    // commit in the window. Carried as `None` — "not asked" —
                    // rather than invented.
                    body_exact: None,
                });
        }
        Ok(Self {
            store,
            repo: repo.to_path_buf(),
            head_sha,
            by_file,
            file_nodes,
        })
    }

    /// A graph rooted at the repository the generation was *built from*,
    /// rather than at one the caller names.
    ///
    /// For hosts that have a store but no root to hand: the daemon dispatches
    /// from a `&Store` alone, so this is the only root available to it.
    ///
    /// **It is the weaker of the two, not the stronger, and the difference
    /// matters when a store has been copied between checkouts** — something
    /// this repository's own guidance tells agents never to do, which is
    /// exactly why it happens. Given a caller-supplied root, a copied store
    /// fails loudly: the blobs under the caller's checkout do not match the
    /// spans, and the basis check refuses by name. Given the *recorded* root,
    /// the analysis reads git from wherever the store was built, finds blobs
    /// that match perfectly, and answers — correctly about a checkout the
    /// caller is not looking at. Prefer [`Self::new`] with a real root
    /// wherever one exists.
    ///
    /// Fails rather than defaulting to the current directory. A wrong root
    /// does not error — it answers, from another repository's history.
    pub fn for_indexed_repo(store: &'a Store) -> anyhow::Result<Self> {
        let root = store.latest_generation_repo_root()?.ok_or_else(|| {
            anyhow::anyhow!(
                "this store recorded no repository root, so there is no checkout its spans \
                 are known to describe — run `devmap build` in a git repository first"
            )
        })?;
        Self::new(store, Path::new(&root))
    }

    /// The commit the analysis must run against for the basis to line up.
    pub fn indexed_head(&self) -> Option<&str> {
        self.head_sha.as_deref()
    }

    /// The checkout this graph reads git from.
    ///
    /// Exposed so a host that built the graph with
    /// [`Self::for_indexed_repo`] can pass the same root to the analysis
    /// rather than resolving one of its own — two roots for one graph is the
    /// disagreement that constructor exists to remove.
    pub fn indexed_repo(&self) -> &Path {
        &self.repo
    }

    /// The blob id of `file` as of the indexed commit.
    fn basis_for(&self, file: &str) -> BlobIdentity {
        let Some(head) = &self.head_sha else {
            return BlobIdentity::Unknown;
        };
        match dc_regress::history::resolve_blob(&self.repo, head, file) {
            Ok(resolved) => resolved.identity,
            // A file the indexed commit does not contain has no blob, and
            // `Unknown` compares equal to nothing — so the join refuses it by
            // name instead of reading its spans against someone else's lines.
            Err(_) => BlobIdentity::Unknown,
        }
    }
}

/// Split a node id of the form `path::symbol` into its file half.
///
/// On the **first** separator, not the last: a symbol name legitimately
/// contains `.` and a qualified method is `Type.method`, while the path half
/// is everything before the first `::`. Splitting from the right would cut a
/// nested qualified name in the wrong place.
fn file_of(node_id: &str) -> Option<&str> {
    node_id.split_once("::").map(|(file, _)| file)
}

/// Fold one distance band of an inbound walk into the nearest-distance map.
///
/// A free function, not an inlined loop, because it is the only place the
/// file-node rule is applied to the walk's *output* and it needs to be
/// testable without a store. That matters here more than usual: the same rule
/// was applied to [`CodeGraph::symbols_in`] first and missed here, and the
/// end-to-end fixture that was supposed to catch it could not — a small
/// repository's inbound walk reaches its answer along call edges and never
/// routes through a file node, so the test passed either way. It took a run
/// against DevCouncil's own index, where a symbol with no callers is reached
/// through its file, to show 68 file nodes sitting in one `impacted` list.
///
/// A file node has no span, so reporting one as an impacted *symbol* puts a
/// thing that cannot be looked at into a list of things to look at, and
/// `roll_up_files` counts it among its own file's symbols.
fn fold_band(nodes: &[String], distance: u32, nearest: &mut HashMap<String, (String, u32)>) {
    for node in nodes {
        let Some(file) = file_of(node) else {
            continue;
        };
        nearest
            .entry(node.clone())
            .and_modify(|held| {
                if distance < held.1 {
                    held.1 = distance;
                }
            })
            .or_insert((file.to_string(), distance));
    }
}

impl CodeGraph for StoreGraph<'_> {
    fn symbols_in(&self, file: &str) -> (Vec<GraphSymbol>, BlobIdentity) {
        let symbols = self.by_file.get(file).cloned().unwrap_or_default();
        (symbols, self.basis_for(file))
    }

    fn cone(&self, symbol: &str, depth: u32) -> (Vec<ConeEntry>, bool) {
        // **Forward**, not inbound. This walked `impact_layered` first, which
        // is the wrong direction and produced a cone holding nothing but the
        // seed: impact answers "what breaks if I change this", and the
        // question here is the opposite one — "what could have broken this".
        //
        // A symptom is caused by the symptom's own body or by something it
        // calls, transitively. Its *callers* cannot have caused it; they are
        // downstream of the failure, not upstream of it. So the cone is the
        // transitive callee closure, which `trace` walks.
        let engine = StoreQueryEngine::new(self.store);
        let request = Request {
            query: symbol.to_string(),
            token_budget: CONE_BUDGET,
            min_confidence: 0.0,
            max_depth: depth as usize,
        };
        let Ok(traced) = engine.trace(request) else {
            return (Vec::new(), true);
        };

        // `trace` hands back an edge set, not bands, so the distances are
        // assigned here by breadth-first search from the seed. BFS rather than
        // any cheaper pass because the first time a symbol is reached is its
        // shortest route, and the ranking reads that distance directly.
        let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();
        for edge in &traced.items {
            adjacency
                .entry(edge.source_symbol.as_str())
                .or_default()
                .push(edge.target_symbol.as_str());
        }

        let mut entries = Vec::new();
        let mut seen: HashMap<&str, u32> = HashMap::new();
        let mut queue: std::collections::VecDeque<(&str, u32)> = std::collections::VecDeque::new();
        seen.insert(symbol, 0);
        queue.push_back((symbol, 0));
        while let Some((node, distance)) = queue.pop_front() {
            if let Some(file) = file_of(node) {
                entries.push(ConeEntry {
                    qualified_name: node.to_string(),
                    file_path: file.to_string(),
                    distance,
                });
            }
            // A node id with no `::` names a file rather than a symbol in one.
            // It has no span to blame, so it is not a cone entry — but it is
            // still walked through, because an edge may route onward from it.
            if distance >= depth {
                continue;
            }
            for next in adjacency.get(node).into_iter().flatten() {
                if seen.contains_key(*next) {
                    continue;
                }
                seen.insert(next, distance + 1);
                queue.push_back((next, distance + 1));
            }
        }

        // A truncated edge set means the walk is a lower bound, and so is every
        // suspect list built on it.
        (entries, traced.truncated)
    }

    fn impacted(&self, seeds: &[String], depth: u32) -> (Vec<ConeEntry>, bool) {
        // **Inbound**, the opposite of `cone` above. A change breaks what
        // calls it; what it calls is unaffected by it. `impact_layered` is the
        // inbound walk and — unlike `trace` — it already bands by distance, so
        // no BFS is re-derived here. Re-deriving it would be a second place
        // for the banding to drift from the one `explore` and `affected`
        // publish.
        if seeds.is_empty() {
            return (Vec::new(), false);
        }
        let engine = StoreQueryEngine::new(self.store);
        let mut nearest: HashMap<String, (String, u32)> = HashMap::new();
        let mut incomplete = false;

        // One request per seed. `impact_layered` takes a single query, and a
        // seed set of hundreds is the normal case for a change — so the bands
        // are unioned by shortest distance here rather than asking the kernel
        // for a multi-seed walk it does not offer.
        for seed in seeds {
            let request = Request {
                query: seed.clone(),
                token_budget: CONE_BUDGET,
                min_confidence: 0.0,
                max_depth: depth as usize,
            };
            let Ok(layered) = engine.impact_layered(request) else {
                incomplete = true;
                continue;
            };
            // A target the walk could not resolve contributes nothing, and a
            // silent nothing is the failure this whole crate is built around.
            if !layered.blast_radius.unmatched_targets.is_empty() {
                incomplete = true;
            }
            if layered.blast_radius.layers.truncated || layered.edges.truncated {
                incomplete = true;
            }
            for layer in &layered.blast_radius.layers.items {
                // A band that was trimmed by the per-layer cap is a band
                // missing symbols, and nothing downstream could tell. The
                // count is right there, so read it rather than trusting the
                // list's length.
                if layer.nodes_omitted > 0 {
                    incomplete = true;
                }
                fold_band(&layer.nodes, layer.depth as u32, &mut nearest);
            }
        }

        let mut entries: Vec<ConeEntry> = nearest
            .into_iter()
            .map(|(qualified_name, (file_path, distance))| ConeEntry {
                qualified_name,
                file_path,
                distance,
            })
            .collect();
        // A total order, so two runs over one index produce one report.
        entries.sort_by(|a, b| {
            a.distance
                .cmp(&b.distance)
                .then(a.file_path.cmp(&b.file_path))
                .then(a.qualified_name.cmp(&b.qualified_name))
        });
        (entries, incomplete)
    }

    fn affected_tests(&self, seeds: &[String], depth: u32) -> (Vec<AffectedTestFile>, bool) {
        if seeds.is_empty() {
            return (Vec::new(), false);
        }
        // `affected_tests` refuses more than `MAX_NEIGHBOR_TARGETS` targets
        // outright rather than trimming, so a change touching more symbols
        // than that is asked in chunks and the answers unioned. Chunking
        // keeps the answer complete: each chunk is a full walk from its own
        // seeds, and a test's distance is the shortest any chunk reported.
        // Trimming to the cap instead would drop tests reachable only from the
        // seeds that did not fit, with nothing in the report to say so.
        let engine = StoreQueryEngine::new(self.store);
        let mut nearest: HashMap<String, u32> = HashMap::new();
        let mut incomplete = false;

        for chunk in seeds.chunks(devmap_query::MAX_NEIGHBOR_TARGETS) {
            let Ok(report) = engine.affected_tests(chunk, TESTS_BUDGET, 0.0, depth as usize) else {
                incomplete = true;
                continue;
            };
            if report.tests.truncated || report.blast_radius.layers.truncated {
                incomplete = true;
            }
            for test in &report.tests.items {
                let distance = test.depth as u32;
                nearest
                    .entry(test.path.clone())
                    .and_modify(|held| *held = (*held).min(distance))
                    .or_insert(distance);
            }
        }

        let mut tests: Vec<AffectedTestFile> = nearest
            .into_iter()
            .map(|(path, distance)| AffectedTestFile { path, distance })
            .collect();
        tests.sort_by(|a, b| a.distance.cmp(&b.distance).then(a.path.cmp(&b.path)));
        (tests, incomplete)
    }

    fn resolve_symptom(&self, symptom: &str) -> Vec<String> {
        // An exact node id wins outright: a caller who pasted one from another
        // devmap answer means that symbol and not everything sharing its name.
        // A bare path is checked too — naming a whole file is a legitimate
        // question, and the file node is the only node that answers it.
        if self.file_nodes.contains(symptom)
            || self
                .by_file
                .values()
                .flatten()
                .any(|s| s.qualified_name == symptom)
        {
            return vec![symptom.to_string()];
        }
        // Otherwise match by name against the symbol table directly rather
        // than through `search`. `SymbolHit` carries a *bare* `symbol_name`
        // and a `file_path` but no qualified name, and rebuilding the node id
        // from those two is wrong for anything a type owns — `Thing.method`
        // would be reconstructed as `file::method`, which names nothing. The
        // table already holds the qualified name, so this reads it.
        let mut matches: Vec<String> = self
            .by_file
            .values()
            .flatten()
            .filter(|symbol| {
                let Some((_, name)) = symbol.qualified_name.split_once("::") else {
                    return false;
                };
                // Either the whole symbol half (`Thing.method`) or its last
                // dotted segment (`method`), so both spellings a caller might
                // reach for resolve.
                name == symptom || name.rsplit('.').next() == Some(symptom)
            })
            .map(|symbol| symbol.qualified_name.clone())
            .collect();
        // A total order, so two runs over one index seed the cone identically.
        matches.sort();
        matches.dedup();
        matches
    }
}

#[cfg(test)]
mod tests {
    use super::{file_of, fold_band};
    use std::collections::HashMap;

    fn band(nodes: &[&str], distance: u32) -> HashMap<String, (String, u32)> {
        let mut nearest = HashMap::new();
        let owned: Vec<String> = nodes.iter().map(|n| n.to_string()).collect();
        fold_band(&owned, distance, &mut nearest);
        nearest
    }

    /// The defect an end-to-end fixture could not reach. A walk's own results
    /// include the file nodes it routed through, and each of those has no span
    /// to look at — so none of them is an impacted symbol.
    #[test]
    fn a_file_node_in_a_band_is_not_an_impacted_symbol() {
        let folded = band(
            &[
                "src/lib.rs::caller",
                "src/lib.rs",
                "src/other.rs",
                "src/other.rs::thing",
            ],
            1,
        );
        let mut kept: Vec<&String> = folded.keys().collect();
        kept.sort();
        assert_eq!(
            kept,
            vec!["src/lib.rs::caller", "src/other.rs::thing"],
            "only nodes naming a symbol survive; a bare path has no span and \
             would be counted among its own file's symbols"
        );
    }

    #[test]
    fn a_symbol_reached_twice_keeps_its_shortest_distance() {
        let mut nearest = HashMap::new();
        fold_band(&["a.rs::x".to_string()], 3, &mut nearest);
        fold_band(&["a.rs::x".to_string()], 1, &mut nearest);
        assert_eq!(nearest["a.rs::x"].1, 1);
        // And a later, longer route must not push it back out.
        fold_band(&["a.rs::x".to_string()], 5, &mut nearest);
        assert_eq!(nearest["a.rs::x"].1, 1);
    }

    #[test]
    fn a_folded_symbol_carries_the_file_its_id_names() {
        let folded = band(&["src/deep/mod.rs::Thing.method"], 2);
        assert_eq!(folded["src/deep/mod.rs::Thing.method"].0, "src/deep/mod.rs");
    }

    #[test]
    fn a_node_id_splits_on_its_first_separator() {
        assert_eq!(
            file_of("src/lib.rs::Thing.method"),
            Some("src/lib.rs"),
            "the symbol half may itself be qualified; the path is what precedes \
             the first separator"
        );
    }

    #[test]
    fn a_bare_file_node_has_no_symbol_half() {
        assert_eq!(file_of("src/lib.rs"), None);
    }

    #[test]
    fn a_path_containing_a_colon_still_splits_at_the_separator() {
        assert_eq!(file_of("src/od:d/name.rs::f"), Some("src/od:d/name.rs"));
    }
}
