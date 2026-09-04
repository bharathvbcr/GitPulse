//! Code intelligence module: in-process devmap querying over `.devcouncil/codeintel/devmap.sqlite`.
//!
//! Links `devmap-query` and `devmap-store` directly without requiring a background
//! daemon or Unix socket. Provides fast symbol search, impact analysis, dependency
//! tracing, and dead code detection.

use devmap_query::{Request, StoreQueryEngine};
use devmap_store::Store;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Default token budget for in-process query operations.
pub const DEFAULT_CODEINTEL_BUDGET: u32 = 2000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelSymbolHit {
    pub symbol_name: String,
    pub file_path: String,
    pub kind: String,
    pub span_start_line: u32,
    pub span_end_line: u32,
    pub source_span: String,
    pub score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelEdge {
    pub source_file: String,
    pub target_file: String,
    pub source_symbol: String,
    pub target_symbol: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelDeadSymbol {
    pub symbol_name: String,
    pub file_path: String,
    pub confidence: f32,
    pub is_exempt: bool,
    pub exemption_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelResponse<T> {
    pub available: bool,
    pub reason: Option<String>,
    pub items: Vec<T>,
    pub total: u32,
    pub shown: u32,
    pub truncated: bool,
}

impl<T> CodeintelResponse<T> {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            reason: Some(reason.into()),
            items: Vec::new(),
            total: 0,
            shown: 0,
            truncated: false,
        }
    }

    pub fn ok(items: Vec<T>, total: u32, shown: u32, truncated: bool) -> Self {
        Self {
            available: true,
            reason: None,
            items,
            total,
            shown,
            truncated,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelStatus {
    pub available: bool,
    pub db_path: String,
    pub generation_id: Option<u32>,
    pub total_files: Option<u32>,
    pub total_symbols: Option<u32>,
    pub total_edges: Option<u32>,
    pub reason: Option<String>,
}

/// Locates the code intelligence SQLite database for a repository.
pub fn devmap_db_path(repo_path: &str) -> PathBuf {
    Path::new(repo_path)
        .join(".devcouncil")
        .join("codeintel")
        .join("devmap.sqlite")
}

fn open_store(repo_path: &str) -> Result<Store, String> {
    let db_path = devmap_db_path(repo_path);
    if !db_path.exists() {
        return Err(format!(
            "No devmap database at {}",
            db_path.to_string_lossy()
        ));
    }
    Store::open(&db_path).map_err(|e| format!("Failed to open devmap database: {e}"))
}

/// Opens the store AND requires that it actually hold an indexed generation.
///
/// The five query surfaces below all answered from a store that opened but held
/// nothing, and `CodeintelResponse::ok` with an empty list is indistinguishable
/// from a completed search that found no matches. So a repository whose map had
/// never been built — or was still building — reported "no callers", "no
/// dependencies" and "no dead symbols": a check that could not run, rendering
/// exactly like one that ran and found nothing clean.
///
/// `status` deliberately does NOT use this. It has to tell "no database",
/// "no generation yet" and "database unreadable" apart, because those call for
/// three different actions, and collapsing them here would lose that.
fn open_indexed_store(repo_path: &str) -> Result<Store, String> {
    let store = open_store(repo_path)?;
    match store.latest_generation_id() {
        Ok(Some(_)) => Ok(store),
        Ok(None) => Err(format!(
            "No generation indexed in {}; the code map has not been built yet",
            devmap_db_path(repo_path).to_string_lossy()
        )),
        Err(e) => Err(format!("Failed to read devmap generation: {e}")),
    }
}

/// Reports the availability and metrics of the repository's code intelligence graph.
pub fn status(repo_path: &str) -> CodeintelStatus {
    let db_path = devmap_db_path(repo_path);
    let db_str = db_path.to_string_lossy().into_owned();

    let store = match open_store(repo_path) {
        Ok(s) => s,
        Err(e) => {
            return CodeintelStatus {
                available: false,
                db_path: db_str,
                generation_id: None,
                total_files: None,
                total_symbols: None,
                total_edges: None,
                reason: Some(e),
            }
        }
    };

    match store.latest_generation_id() {
        Ok(Some(gen_id)) => {
            let files = store.latest_file_hashes().map(|f| f.len() as u32).ok();
            let edges = store.latest_edges(0.0).map(|e| e.len() as u32).ok();
            CodeintelStatus {
                available: true,
                db_path: db_str,
                generation_id: Some(gen_id),
                total_files: files,
                total_symbols: None,
                total_edges: edges,
                reason: None,
            }
        }
        Ok(None) => CodeintelStatus {
            available: false,
            db_path: db_str,
            generation_id: None,
            total_files: None,
            total_symbols: None,
            total_edges: None,
            reason: Some("No generation indexed in database".into()),
        },
        Err(e) => CodeintelStatus {
            available: false,
            db_path: db_str,
            generation_id: None,
            total_files: None,
            total_symbols: None,
            total_edges: None,
            reason: Some(format!("Database error: {e}")),
        },
    }
}

/// Searches symbols using in-process code graph index.
pub fn search(
    repo_path: &str,
    query: &str,
    token_budget: Option<u32>,
) -> CodeintelResponse<CodeintelSymbolHit> {
    let store = match open_indexed_store(repo_path) {
        Ok(s) => s,
        Err(e) => return CodeintelResponse::unavailable(e),
    };
    let engine = StoreQueryEngine::new(&store);
    let req = Request {
        query: query.to_string(),
        token_budget: token_budget.unwrap_or(DEFAULT_CODEINTEL_BUDGET),
        min_confidence: 0.0,
        max_depth: 10,
    };

    match engine.search(req) {
        Ok(res) => {
            let items = res
                .items
                .into_iter()
                .map(|hit| CodeintelSymbolHit {
                    symbol_name: hit.symbol_name,
                    file_path: hit.file_path,
                    kind: hit.kind,
                    span_start_line: hit.span.0,
                    span_end_line: hit.span.1,
                    source_span: hit.source_span,
                    score: hit.score,
                })
                .collect();
            CodeintelResponse::ok(items, res.total, res.shown, res.truncated)
        }
        Err(e) => CodeintelResponse::unavailable(format!("Search failed: {e}")),
    }
}

/// Computes blast radius / impact for a symbol or file.
pub fn impact(
    repo_path: &str,
    target: &str,
    token_budget: Option<u32>,
) -> CodeintelResponse<CodeintelEdge> {
    let store = match open_indexed_store(repo_path) {
        Ok(s) => s,
        Err(e) => return CodeintelResponse::unavailable(e),
    };
    let engine = StoreQueryEngine::new(&store);
    let req = Request {
        query: target.to_string(),
        token_budget: token_budget.unwrap_or(DEFAULT_CODEINTEL_BUDGET),
        min_confidence: 0.0,
        max_depth: 10,
    };

    match engine.impact(req) {
        Ok(res) => {
            let items = res
                .items
                .into_iter()
                .map(|edge| CodeintelEdge {
                    source_file: edge.source_file,
                    target_file: edge.target_file,
                    source_symbol: edge.source_symbol,
                    target_symbol: edge.target_symbol,
                    confidence: edge.confidence.0,
                })
                .collect();
            CodeintelResponse::ok(items, res.total, res.shown, res.truncated)
        }
        Err(e) => CodeintelResponse::unavailable(format!("Impact computation failed: {e}")),
    }
}

/// Finds dependencies for a file.
pub fn dependencies(
    repo_path: &str,
    file_path: &str,
    token_budget: Option<u32>,
) -> CodeintelResponse<CodeintelEdge> {
    let store = match open_indexed_store(repo_path) {
        Ok(s) => s,
        Err(e) => return CodeintelResponse::unavailable(e),
    };
    let engine = StoreQueryEngine::new(&store);
    let req = Request {
        query: file_path.to_string(),
        token_budget: token_budget.unwrap_or(DEFAULT_CODEINTEL_BUDGET),
        min_confidence: 0.0,
        max_depth: 10,
    };

    match engine.dependencies(req) {
        Ok(res) => {
            let items = res
                .items
                .into_iter()
                .map(|edge| CodeintelEdge {
                    source_file: edge.source_file,
                    target_file: edge.target_file,
                    source_symbol: edge.source_symbol,
                    target_symbol: edge.target_symbol,
                    confidence: edge.confidence.0,
                })
                .collect();
            CodeintelResponse::ok(items, res.total, res.shown, res.truncated)
        }
        Err(e) => CodeintelResponse::unavailable(format!("Dependencies lookup failed: {e}")),
    }
}

/// Identifies dead / unreferenced symbols.
pub fn dead_symbols(
    repo_path: &str,
    token_budget: Option<u32>,
) -> CodeintelResponse<CodeintelDeadSymbol> {
    let store = match open_indexed_store(repo_path) {
        Ok(s) => s,
        Err(e) => return CodeintelResponse::unavailable(e),
    };
    let engine = StoreQueryEngine::new(&store);
    let budget = token_budget.unwrap_or(DEFAULT_CODEINTEL_BUDGET);

    match engine.dead_symbols(budget) {
        Ok(res) => {
            let items = res
                .items
                .into_iter()
                .map(|dead| CodeintelDeadSymbol {
                    symbol_name: dead.symbol_name,
                    file_path: dead.file_path,
                    confidence: dead.confidence,
                    is_exempt: dead.is_exempt,
                    exemption_reason: dead.exemption_reason,
                })
                .collect();
            CodeintelResponse::ok(items, res.total, res.shown, res.truncated)
        }
        Err(e) => CodeintelResponse::unavailable(format!("Dead symbols analysis failed: {e}")),
    }
}

/// Traces path between two symbols.
pub fn trace_between(
    repo_path: &str,
    from: &str,
    to: &str,
    token_budget: Option<u32>,
) -> CodeintelResponse<CodeintelEdge> {
    let store = match open_indexed_store(repo_path) {
        Ok(s) => s,
        Err(e) => return CodeintelResponse::unavailable(e),
    };
    let engine = StoreQueryEngine::new(&store);
    let req = Request {
        query: (from.to_string(), to.to_string()),
        token_budget: token_budget.unwrap_or(DEFAULT_CODEINTEL_BUDGET),
        min_confidence: 0.0,
        max_depth: 10,
    };

    match engine.trace_between(req) {
        Ok(res) => {
            let items = res
                .items
                .into_iter()
                .map(|edge| CodeintelEdge {
                    source_file: edge.source_file,
                    target_file: edge.target_file,
                    source_symbol: edge.source_symbol,
                    target_symbol: edge.target_symbol,
                    confidence: edge.confidence.0,
                })
                .collect();
            CodeintelResponse::ok(items, res.total, res.shown, res.truncated)
        }
        Err(e) => CodeintelResponse::unavailable(format!("Trace between failed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devmap_db_path_construction() {
        let path = devmap_db_path("/test/repo");
        assert!(path.ends_with(".devcouncil/codeintel/devmap.sqlite"));
    }

    #[test]
    fn codeintel_status_on_nonexistent_repo() {
        let stat = status("/nonexistent/repo/path");
        assert!(!stat.available);
        assert!(stat.reason.is_some());
    }

    #[test]
    fn codeintel_search_on_nonexistent_repo() {
        let res = search("/nonexistent/repo/path", "test", None);
        assert!(!res.available);
        assert!(res.items.is_empty());
    }

    /// A repository directory with a devmap database at the canonical path.
    ///
    /// `Store::open` creates the schema, so this yields a real, readable
    /// database that simply has no generation in it yet — the middle state
    /// between "no map" and "a map with answers".
    fn repo_with_empty_store() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let db = devmap_db_path(&dir.path().to_string_lossy());
        std::fs::create_dir_all(db.parent().expect("db parent")).expect("mkdir codeintel");
        drop(Store::open(&db).expect("store opens and creates its schema"));
        dir
    }

    /// The three unavailable states must be distinguishable.
    ///
    /// `available: false` is the same boolean for a repository that has no map,
    /// one whose map holds no generation, and one whose database cannot be
    /// read at all — but they call for three different actions (build a map,
    /// wait for indexing, repair a file). Only `reason` separates them, so a
    /// reason that went missing or collapsed to one string would leave the UI
    /// reporting "no code intelligence" for a corrupt database.
    #[test]
    fn the_three_unavailable_states_do_not_read_alike() {
        let missing = status("/nonexistent/repo/path");
        assert!(!missing.available);
        let missing_reason = missing.reason.expect("missing map states a reason");
        assert!(
            missing_reason.contains("No devmap database"),
            "{missing_reason}"
        );

        let empty = repo_with_empty_store();
        let indexed = status(&empty.path().to_string_lossy());
        assert!(!indexed.available, "an empty store is not available");
        let indexed_reason = indexed.reason.expect("empty store states a reason");
        assert!(
            indexed_reason.contains("No generation indexed"),
            "{indexed_reason}"
        );
        assert_ne!(
            missing_reason, indexed_reason,
            "a repository with no map and one still indexing must not read alike"
        );

        // A file that exists at the database path but is not a database.
        let corrupt = tempfile::TempDir::new().expect("tempdir");
        let db = devmap_db_path(&corrupt.path().to_string_lossy());
        std::fs::create_dir_all(db.parent().expect("db parent")).expect("mkdir");
        std::fs::write(&db, b"this is not a sqlite file").expect("write junk");
        let broken = status(&corrupt.path().to_string_lossy());
        assert!(!broken.available, "a corrupt database is not available");
        let broken_reason = broken.reason.expect("corrupt store states a reason");
        assert_ne!(
            broken_reason, indexed_reason,
            "a corrupt database must not read as one that is merely empty"
        );
    }

    /// Every read surface must refuse rather than answer emptily.
    ///
    /// An empty `items` list with `available: true` would say "this symbol has
    /// no callers" for a repository whose map was never built — the exact
    /// shape of a check that could not run reporting as one that ran and found
    /// nothing. Enumerated over all five surfaces so a newly added one that
    /// forgets the guard is not covered by nothing.
    #[test]
    fn no_read_surface_answers_emptily_when_there_is_no_map() {
        let repo = tempfile::TempDir::new().expect("tempdir");
        let root = repo.path().to_string_lossy().to_string();

        let surfaces: Vec<(&str, bool, Option<String>)> = vec![
            {
                let r = search(&root, "anything", None);
                ("search", r.available, r.reason)
            },
            {
                let r = impact(&root, "anything", None);
                ("impact", r.available, r.reason)
            },
            {
                let r = dependencies(&root, "anything", None);
                ("dependencies", r.available, r.reason)
            },
            {
                let r = dead_symbols(&root, None);
                ("dead_symbols", r.available, r.reason)
            },
            {
                let r = trace_between(&root, "a", "b", None);
                ("trace_between", r.available, r.reason)
            },
        ];
        assert_eq!(surfaces.len(), 5, "all read surfaces must be covered");
        for (name, available, reason) in surfaces {
            assert!(!available, "{name} claimed availability with no map");
            let reason = reason.unwrap_or_else(|| panic!("{name} gave no reason"));
            assert!(
                reason.contains("No devmap database"),
                "{name} reason does not name the missing map: {reason}"
            );
        }
    }

    /// The same five surfaces against a database that opens but holds nothing.
    ///
    /// This is the state a repository sits in while its first index builds, and
    /// it must not be reported as a completed search that found no matches.
    #[test]
    fn no_read_surface_answers_emptily_for_a_store_without_a_generation() {
        let repo = repo_with_empty_store();
        let root = repo.path().to_string_lossy().to_string();

        assert!(!status(&root).available);
        let claiming: Vec<&str> = [
            ("search", search(&root, "anything", None).available),
            ("impact", impact(&root, "anything", None).available),
            (
                "dependencies",
                dependencies(&root, "anything", None).available,
            ),
            ("dead_symbols", dead_symbols(&root, None).available),
            (
                "trace_between",
                trace_between(&root, "a", "b", None).available,
            ),
        ]
        .into_iter()
        .filter_map(|(name, available)| available.then_some(name))
        .collect();
        assert!(
            claiming.is_empty(),
            "these surfaces reported availability for a store with no generation, \
             while status() reports the same repository as unavailable: {claiming:?}"
        );
    }
}
