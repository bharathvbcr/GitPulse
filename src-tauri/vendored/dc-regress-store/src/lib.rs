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

use dc_regress::{BlobIdentity, CodeGraph, ConeEntry, GraphSymbol};
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
    by_file: HashMap<String, Vec<GraphSymbol>>,
}

impl<'a> StoreGraph<'a> {
    pub fn new(store: &'a Store, repo: &Path) -> anyhow::Result<Self> {
        let head_sha = store.latest_generation_head_sha()?;
        let mut by_file: HashMap<String, Vec<GraphSymbol>> = HashMap::new();
        for symbol in store.all_symbols()? {
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
        })
    }

    /// The commit the analysis must run against for the basis to line up.
    pub fn indexed_head(&self) -> Option<&str> {
        self.head_sha.as_deref()
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

    fn resolve_symptom(&self, symptom: &str) -> Vec<String> {
        // An exact node id wins outright: a caller who pasted one from another
        // devmap answer means that symbol and not everything sharing its name.
        if self
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
    use super::file_of;

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
