//! Code intelligence module: in-process devmap querying over `.devcouncil/codeintel/devmap.sqlite`.
//!
//! Links `devmap-query` and `devmap-store` directly without requiring a background
//! daemon or Unix socket. Provides fast symbol search, impact analysis, dependency
//! tracing, and dead code detection.

use crate::engine::git_cli::validate_repo;
use devmap_query::{Request, ResolutionAvailability, Response, StoreQueryEngine};
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
    /// Why `source_span` is empty, when it is empty because the file could not
    /// be read rather than because the symbol has no body.
    ///
    /// The engine computes this (`SymbolHit::source_unavailable_reason`) for
    /// exactly the case where the map names a file the working tree no longer
    /// has — a stale generation, a different checkout, a deleted file. Dropping
    /// it made a hit whose source could not be read render identically to one
    /// whose source is genuinely blank.
    pub source_unavailable_reason: Option<String>,
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
    map_path(Path::new(repo_path))
}

fn map_path(repo: &Path) -> PathBuf {
    repo.join(".devcouncil")
        .join("codeintel")
        .join("devmap.sqlite")
}

/// Resolves `repo_path` through the same gate every other repository-scoped
/// surface uses, and returns the canonical work tree.
///
/// The six entry points below took the caller's string and joined it straight
/// onto the map path, checking only that the resulting file existed. So `""`
/// resolved against the *server process's own* working directory and answered
/// about whatever repository it happened to be started in, and `"../other"`
/// reached out of the repository the caller named — a cross-repository read
/// driven purely by an argument documented as "absolute path to a git
/// repository". `gitpulse_status`, `gitpulse_ledger_events` and
/// `gitpulse_task_view` already refuse both; these did not.
///
/// [`validate_repo`] canonicalizes, so `..` segments and symlinks are resolved
/// *before* the map path is built rather than left for the filesystem to follow
/// afterwards, and it is the existing owner of this decision rather than a
/// second path policy standing beside it.
fn resolve_repo(repo_path: &str) -> Result<PathBuf, String> {
    validate_repo(repo_path)
}

fn open_store(repo: &Path) -> Result<Store, String> {
    let db_path = map_path(repo);
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
fn open_indexed_store(repo: &Path) -> Result<Store, String> {
    let store = open_store(repo)?;
    match store.latest_generation_id() {
        Ok(Some(_)) => Ok(store),
        Ok(None) => Err(format!(
            "No generation indexed in {}; the code map has not been built yet",
            map_path(repo).to_string_lossy()
        )),
        Err(e) => Err(format!("Failed to read devmap generation: {e}")),
    }
}

/// Validates the repository and opens its built map — the whole preamble every
/// query surface shares.
fn open_repo_map(repo_path: &str) -> Result<Store, String> {
    open_indexed_store(&resolve_repo(repo_path)?)
}

/// Refuses a blank query argument instead of answering it.
///
/// A whitespace-only search query is not a search that found nothing: the
/// engine short-circuits it to an empty *Available* response, which renders as
/// "no symbol matches that" for a caller who asked nothing. The other surfaces
/// funnel a blank argument into "… is not indexed" or "… has no indexed
/// traversal start" — true statements that name the map rather than the
/// mistake, and that read as a finding about the repository.
fn require_argument(label: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{label} must not be blank"));
    }
    Ok(())
}

/// Carries the engine's own refusal across the wire boundary.
///
/// `devmap-query` answers `Unavailable { reason }` whenever it could not resolve
/// what was asked: the file is not indexed, the file could not be parsed, the
/// target has no indexed traversal start, there is no indexed path between two
/// symbols. Every call site here used to funnel that through
/// [`CodeintelResponse::ok`], which hardcodes `available: true, reason: None` —
/// so a typo'd or newly added symbol came back `total: 0, items: []`, byte for
/// byte what a symbol with genuinely no callers returns. An agent following this
/// repository's own instruction to check its blast radius before editing read
/// that as "nothing calls this, safe to change". The reason the engine went to
/// the trouble of computing is the only thing separating the two cases, so it is
/// what crosses the boundary.
fn from_engine<S, T>(response: Response<S>, map: impl Fn(S) -> T) -> CodeintelResponse<T> {
    let Response {
        items,
        total,
        shown,
        truncated,
        resolution,
        ..
    } = response;
    match resolution {
        ResolutionAvailability::Unavailable { reason } => CodeintelResponse::unavailable(reason),
        ResolutionAvailability::Available => CodeintelResponse::ok(
            items.into_iter().map(map).collect(),
            total,
            shown,
            truncated,
        ),
    }
}

/// An unavailable status, carrying the path we looked at and why we stopped.
fn status_unavailable(db_path: String, reason: String) -> CodeintelStatus {
    CodeintelStatus {
        available: false,
        db_path,
        generation_id: None,
        total_files: None,
        total_symbols: None,
        total_edges: None,
        reason: Some(reason),
    }
}

/// Reports the availability and metrics of the repository's code intelligence graph.
pub fn status(repo_path: &str) -> CodeintelStatus {
    // A repository path we refused to resolve has no map path to report. The
    // empty string is what `HealthPanel` already renders as "unknown"; echoing
    // the joined candidate instead would print a relative
    // `.devcouncil/codeintel/devmap.sqlite` for `repo_path: ""` — the exact
    // resolve-against-our-own-cwd behaviour this gate exists to refuse.
    let repo = match resolve_repo(repo_path) {
        Ok(repo) => repo,
        Err(e) => return status_unavailable(String::new(), e),
    };
    let db_str = map_path(&repo).to_string_lossy().into_owned();

    let store = match open_store(&repo) {
        Ok(s) => s,
        Err(e) => return status_unavailable(db_str, e),
    };

    // `Store::status` is devmap's own summary of a generation, and it answers
    // with `COUNT(*)` over the generation's primary-key range. This used to call
    // `latest_file_hashes()` and `latest_edges(0.0)` and immediately throw the
    // rows away through `.len()`, materializing the entire edge table — every
    // edge as four owned strings — to learn one integer. On this repository's own
    // 65 MB map (703 files, 53,622 edges) that cost 261 ms and ~29 MiB of peak
    // RSS on a call that runs on every `gitpulse_status` and every
    // `gitpulse_insights`.
    let summary = match store.status(&db_str) {
        Ok(summary) => summary,
        Err(e) => return status_unavailable(db_str, format!("Database error: {e}")),
    };
    let Some(gen_id) = summary.latest_generation else {
        return status_unavailable(db_str, "No generation indexed in database".into());
    };

    CodeintelStatus {
        available: true,
        db_path: db_str,
        generation_id: Some(gen_id),
        total_files: latest_file_count(&store, gen_id),
        // Was hardcoded `None`, so the hook renderer's "N symbols across M
        // files" always said zero symbols and the health panel always showed a
        // dash. `Store::status` counts them in the same pass as the edges.
        total_symbols: u32::try_from(summary.node_count).ok(),
        total_edges: u32::try_from(summary.edge_count).ok(),
        reason: None,
    }
}

/// Files in generation `gen_id`, without reading the generation's file rows.
///
/// devmap has no `COUNT(*)` over `generation_files` on its public surface, but
/// it records the number in `build_history` inside the generation's own write
/// transaction — so for the generation that row describes, it *is* that count
/// (measured identical to `latest_file_hashes().len()` on the 65 MB map, at
/// 49 µs against 50 ms). The generation id is checked rather than assumed:
/// history rows are retained 500 deep while generations are pruned to 2, and a
/// database migrated up from before the table existed carries generations with
/// no history row at all. When the newest row describes some other generation,
/// the count is recomputed the slow, certain way rather than attributed to the
/// wrong build.
fn latest_file_count(store: &Store, gen_id: u32) -> Option<u32> {
    if let Ok(history) = store.build_history(1) {
        if let Some(row) = history.first().filter(|row| row.generation_id == gen_id) {
            return u32::try_from(row.files).ok();
        }
    }
    store
        .latest_file_hashes()
        .ok()
        .and_then(|files| u32::try_from(files.len()).ok())
}

/// Searches symbols using in-process code graph index.
pub fn search(
    repo_path: &str,
    query: &str,
    token_budget: Option<u32>,
) -> CodeintelResponse<CodeintelSymbolHit> {
    if let Err(e) = require_argument("query", query) {
        return CodeintelResponse::unavailable(e);
    }
    let store = match open_repo_map(repo_path) {
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
        Ok(res) => from_engine(res, |hit| CodeintelSymbolHit {
            symbol_name: hit.symbol_name,
            file_path: hit.file_path,
            kind: hit.kind,
            span_start_line: hit.span.0,
            span_end_line: hit.span.1,
            source_span: hit.source_span,
            source_unavailable_reason: hit.source_unavailable_reason,
            score: hit.score,
        }),
        Err(e) => CodeintelResponse::unavailable(format!("Search failed: {e}")),
    }
}

/// Computes blast radius / impact for a symbol or file.
pub fn impact(
    repo_path: &str,
    target: &str,
    token_budget: Option<u32>,
) -> CodeintelResponse<CodeintelEdge> {
    if let Err(e) = require_argument("target", target) {
        return CodeintelResponse::unavailable(e);
    }
    let store = match open_repo_map(repo_path) {
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
        Ok(res) => from_engine(res, |edge| CodeintelEdge {
            source_file: edge.source_file,
            target_file: edge.target_file,
            source_symbol: edge.source_symbol,
            target_symbol: edge.target_symbol,
            confidence: edge.confidence.0,
        }),
        Err(e) => CodeintelResponse::unavailable(format!("Impact computation failed: {e}")),
    }
}

/// Finds dependencies for a file.
pub fn dependencies(
    repo_path: &str,
    file_path: &str,
    token_budget: Option<u32>,
) -> CodeintelResponse<CodeintelEdge> {
    if let Err(e) = require_argument("file_path", file_path) {
        return CodeintelResponse::unavailable(e);
    }
    let store = match open_repo_map(repo_path) {
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
        Ok(res) => from_engine(res, |edge| CodeintelEdge {
            source_file: edge.source_file,
            target_file: edge.target_file,
            source_symbol: edge.source_symbol,
            target_symbol: edge.target_symbol,
            confidence: edge.confidence.0,
        }),
        Err(e) => CodeintelResponse::unavailable(format!("Dependencies lookup failed: {e}")),
    }
}

/// Identifies dead / unreferenced symbols.
pub fn dead_symbols(
    repo_path: &str,
    token_budget: Option<u32>,
) -> CodeintelResponse<CodeintelDeadSymbol> {
    let store = match open_repo_map(repo_path) {
        Ok(s) => s,
        Err(e) => return CodeintelResponse::unavailable(e),
    };
    let engine = StoreQueryEngine::new(&store);
    let budget = token_budget.unwrap_or(DEFAULT_CODEINTEL_BUDGET);

    match engine.dead_symbols(budget) {
        Ok(res) => from_engine(res, |dead| CodeintelDeadSymbol {
            symbol_name: dead.symbol_name,
            file_path: dead.file_path,
            confidence: dead.confidence,
            is_exempt: dead.is_exempt,
            exemption_reason: dead.exemption_reason,
        }),
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
    if let Err(e) = require_argument("from", from).and_then(|()| require_argument("to", to)) {
        return CodeintelResponse::unavailable(e);
    }
    let store = match open_repo_map(repo_path) {
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
        Ok(res) => from_engine(res, |edge| CodeintelEdge {
            source_file: edge.source_file,
            target_file: edge.target_file,
            source_symbol: edge.source_symbol,
            target_symbol: edge.target_symbol,
            confidence: edge.confidence.0,
        }),
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

    /// A real, empty Git work tree.
    ///
    /// Every entry point now goes through `validate_repo`, so a bare temporary
    /// directory is refused before the map is ever looked for — which is the
    /// point, but it means these fixtures have to be repositories to reach the
    /// behaviour under test. `git init` rather than a hand-made `.git`
    /// directory: the gate is the real one, so the fixture is too.
    fn git_repo() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let output = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .expect("spawn git init");
        assert!(
            output.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        dir
    }

    /// A repository directory with a devmap database at the canonical path.
    ///
    /// `Store::open` creates the schema, so this yields a real, readable
    /// database that simply has no generation in it yet — the middle state
    /// between "no map" and "a map with answers".
    fn repo_with_empty_store() -> tempfile::TempDir {
        let dir = git_repo();
        drop(open_fixture_store(dir.path()));
        dir
    }

    /// Opens (creating and migrating) the map database for a fixture repository.
    fn open_fixture_store(repo: &Path) -> Store {
        let db = map_path(repo);
        std::fs::create_dir_all(db.parent().expect("db parent")).expect("mkdir codeintel");
        Store::open(&db).expect("store opens and creates its schema")
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
        // A real repository that simply has no map. It must be a real one: a
        // path that does not exist is now refused by the path gate before the
        // map is ever looked for, which is a fourth state ("that is not a
        // repository") and not the one under test here.
        let unmapped = git_repo();
        let missing = status(&unmapped.path().to_string_lossy());
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
        let corrupt = git_repo();
        let db = map_path(corrupt.path());
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
        let repo = git_repo();
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

    /* ── A repository whose map holds one real generation ─────────────────── */

    /// The source of `src/caller.rs`, written to disk so one search hit has a
    /// readable span and the other does not.
    const CALLER_SOURCE: &str = "fn probe_caller() { probe_callee(); }\n";

    /// A repository with a built map: two connected files and one island.
    ///
    /// devmap's writer (`Store::save_generation`) sits behind the `parse`
    /// feature, which GitPulse turns off on purpose — it reads a map DevCouncil
    /// built and never indexes anything itself — so there is no library call
    /// here that can produce a populated generation. The rows go in directly,
    /// through the schema `Store::open` has just created and migrated. That is
    /// also what keeps the fixture honest: a schema change fails at the INSERT
    /// rather than leaving this quietly describing a shape the reader stopped
    /// reading.
    ///
    /// The graph is the smallest one that separates the answers under test:
    ///
    ///   * `src/caller.rs::probe_caller` calls `src/callee.rs::probe_callee`,
    ///     so both files have dependencies and both symbols are reachable.
    ///   * `src/island.rs` is indexed with no edge touching it at all — a file
    ///     that genuinely has no dependencies, as against one that is not in
    ///     the map.
    ///   * only `src/caller.rs` exists on disk, so one search hit can read its
    ///     source span and the other cannot.
    fn repo_with_one_generation() -> tempfile::TempDir {
        let dir = git_repo();
        let root = dir.path().to_string_lossy().into_owned();
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir src");
        std::fs::write(dir.path().join("src/caller.rs"), CALLER_SOURCE).expect("write caller");

        let store = open_fixture_store(dir.path());
        let db = map_path(dir.path());
        drop(store);

        let conn = rusqlite::Connection::open(&db).expect("open fixture db");
        let files = ["src/caller.rs", "src/callee.rs", "src/island.rs"];
        for (id, path) in (1i64..).zip(files) {
            conn.execute("INSERT INTO paths (id, path) VALUES (?1, ?2)", (id, path))
                .expect("insert path");
        }
        conn.execute(
            "INSERT INTO generations (id, created_at, head_sha, analysis_json, repo_root)
             VALUES (1, 0.0, 'fixture', ?1, ?2)",
            (
                // Only `latest_analysis` reads this column, and nothing on the
                // paths under test does — but a well-formed summary keeps the
                // row readable if something starts to.
                r#"{"total_files":3,"total_symbols":2,"total_edges":1,
                    "dead_symbols":[],"communities":[],"status":"Ok",
                    "unresolved_calls":0}"#,
                &root,
            ),
        )
        .expect("insert generation");

        for id in 1i64..=3 {
            conn.execute(
                "INSERT INTO generation_files
                   (generation_id, file_id, language, content_hash,
                    parse_outcome_json, engine_json, extraction_json,
                    grammar_version, analyzer_version)
                 VALUES (1, ?1, 'rust', ?1, '\"Clean\"', '\"ConfigScanner\"', 'null', 'v1', 'v1')",
                [id],
            )
            .expect("insert generation file");
        }

        // `search` reads nodes through the FTS index, whose rowid encodes the
        // generation and the node ordinal — see `Store::fts_rowid`.
        for (ordinal, (file_id, name)) in [(1i64, "probe_caller"), (2, "probe_callee")]
            .into_iter()
            .enumerate()
        {
            let ordinal = ordinal as i64;
            let span_end = if file_id == 1 {
                CALLER_SOURCE.len() as i64
            } else {
                20
            };
            conn.execute(
                "INSERT INTO generation_nodes
                   (generation_id, ordinal, file_id, name, qualified_name, kind,
                    span_start, span_end, is_exported)
                 VALUES (1, ?1, ?2, ?3, ?3, 'Function', 0, ?4, 1)",
                (ordinal, file_id, name, span_end),
            )
            .expect("insert node");
            let fts_rowid = (1i64 << 32) | ordinal;
            conn.execute(
                "INSERT INTO nodes_fts (rowid, name, qualified_name, path)
                 VALUES (?1, ?2, ?2, ?3)",
                (fts_rowid, name, files[(file_id - 1) as usize]),
            )
            .expect("insert fts row");
            conn.execute(
                "INSERT INTO nodes_fts_map (rowid_ref, generation_id) VALUES (?1, 1)",
                [fts_rowid],
            )
            .expect("insert fts map row");
        }

        conn.execute(
            "INSERT INTO generation_edges
               (generation_id, ordinal, source_file_id, target_file_id,
                source_symbol, target_symbol, edge_kind, confidence)
             VALUES (1, 0, 1, 2, 'probe_caller', 'probe_callee', 'Calls', 0.9)",
            [],
        )
        .expect("insert edge");
        conn.execute(
            "INSERT INTO build_history
               (generation_id, built_at, head_sha, files, symbols, edges,
                dead_confident, dead_ambiguous, parse_failed, languages_covered,
                build_ms, db_bytes)
             VALUES (1, 0.0, 'fixture', 3, 2, 1, 0, 0, 0, 1, NULL, 0)",
            [],
        )
        .expect("insert build history");
        conn.close().expect("close fixture db");
        dir
    }

    /* ── Finding 1: a refusal must not render as an answer ────────────────── */

    /// The defect this whole module's honesty rests on.
    ///
    /// `dependencies` answers `Unavailable { reason }` for a path the map does
    /// not contain, and an ordinary empty `Available` response for a file that
    /// is indexed and genuinely depends on nothing. Both used to leave here as
    /// `available: true, reason: null, total: 0, items: []` — byte for byte
    /// identical — because every call site funnelled the engine's response
    /// through `CodeintelResponse::ok`, which hardcodes availability. This
    /// repository's own instructions tell an agent to check its blast radius
    /// before editing and to trust the result, so a typo'd or newly added
    /// symbol read as "nothing depends on this, safe to change".
    #[test]
    fn an_unresolvable_target_and_a_target_with_genuinely_no_edges_do_not_read_alike() {
        let repo = repo_with_one_generation();
        let root = repo.path().to_string_lossy().to_string();

        let island = dependencies(&root, "src/island.rs", None);
        let absent = dependencies(&root, "src/not/in/the/map.rs", None);

        assert!(
            island.available,
            "an indexed file with no edges is a completed lookup: {:?}",
            island.reason
        );
        assert_eq!(island.reason, None);
        assert_eq!(island.total, 0);
        assert!(island.items.is_empty());

        assert!(
            !absent.available,
            "a file the map does not contain must not report as a clean empty result"
        );
        let reason = absent.reason.clone().expect("a refusal states its reason");
        assert!(
            reason.contains("src/not/in/the/map.rs") && reason.contains("not indexed"),
            "the engine's own reason did not survive the boundary: {reason}"
        );
        assert_ne!(
            (island.available, island.reason),
            (absent.available, absent.reason),
            "a lookup that could not run is indistinguishable from one that ran and found nothing"
        );
    }

    /// Each engine refusal, on each surface that can produce one.
    ///
    /// The engine has four distinct ways to say "I could not answer that" and
    /// they arrive on three different surfaces. Enumerated rather than sampled,
    /// so a surface whose mapping regresses to `CodeintelResponse::ok` is not
    /// covered by nothing — and each reason is required to name what was asked,
    /// which is what proves the engine's own words came through rather than a
    /// generic refusal invented here.
    #[test]
    fn every_engine_refusal_reaches_the_caller_with_the_engine_s_own_reason() {
        let repo = repo_with_one_generation();
        let root = repo.path().to_string_lossy().to_string();

        let cases: Vec<(&str, bool, Option<String>, &str)> = vec![
            {
                let r = impact(&root, "no_such_symbol_12345", None);
                ("impact", r.available, r.reason, "no_such_symbol_12345")
            },
            {
                let r = dependencies(&root, "src/nowhere.rs", None);
                ("dependencies", r.available, r.reason, "src/nowhere.rs")
            },
            {
                let r = trace_between(&root, "probe_caller", "no_such_symbol_12345", None);
                (
                    "trace_between",
                    r.available,
                    r.reason,
                    "no_such_symbol_12345",
                )
            },
        ];
        for (surface, available, reason, needle) in cases {
            assert!(
                !available,
                "{surface} reported an unresolvable target as an answer"
            );
            let reason = reason.unwrap_or_else(|| panic!("{surface} refused without a reason"));
            assert!(
                reason.contains(needle),
                "{surface} refusal does not name what was asked: {reason}"
            );
        }
    }

    /// A resolvable target still answers, so the guard did not simply refuse
    /// everything.
    ///
    /// A fix that turned every response into `available: false` would satisfy
    /// the tests above and destroy the module. These are the same three
    /// surfaces on inputs the map can resolve.
    #[test]
    fn a_resolvable_target_still_answers_after_refusals_are_carried_through() {
        let repo = repo_with_one_generation();
        let root = repo.path().to_string_lossy().to_string();

        let blast = impact(&root, "probe_callee", None);
        assert!(blast.available, "impact refused: {:?}", blast.reason);
        assert_eq!(blast.total, 1, "the one caller in the fixture");
        assert_eq!(blast.items[0].source_symbol, "probe_caller");

        let deps = dependencies(&root, "src/caller.rs", None);
        assert!(deps.available, "dependencies refused: {:?}", deps.reason);
        assert_eq!(deps.total, 1);

        let path = trace_between(&root, "probe_caller", "probe_callee", None);
        assert!(path.available, "trace refused: {:?}", path.reason);
        assert_eq!(path.total, 1);
    }

    /// A blank query is a caller mistake, not a search that found nothing.
    ///
    /// The engine short-circuits a whitespace-only query to an empty
    /// *Available* response, which renders as "no symbol matches that" for a
    /// caller who asked nothing at all — the same collapse as an unresolvable
    /// target, arriving through a different door.
    #[test]
    fn a_blank_query_argument_is_refused_rather_than_answered_as_no_matches() {
        let repo = repo_with_one_generation();
        let root = repo.path().to_string_lossy().to_string();

        let blank = search(&root, "   ", None);
        assert!(
            !blank.available,
            "a blank query reported as a completed search"
        );
        assert!(blank
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("query")));

        let real = search(&root, "probe", None);
        assert!(real.available, "search refused: {:?}", real.reason);
        assert_eq!(real.total, 2, "both fixture symbols match the prefix");

        // The other three argument-taking surfaces, so a blank argument is not
        // reported as a fact about the repository on any of them.
        for (surface, available) in [
            ("impact", impact(&root, "  ", None).available),
            ("dependencies", dependencies(&root, "", None).available),
            (
                "trace_from",
                trace_between(&root, "", "probe_callee", None).available,
            ),
            (
                "trace_to",
                trace_between(&root, "probe_caller", " ", None).available,
            ),
        ] {
            assert!(!available, "{surface} answered a blank argument");
        }
    }

    /// A hit whose source could not be read must say so.
    ///
    /// `SymbolHit::source_unavailable_reason` is computed by the engine for
    /// exactly the case where the map names a file the working tree no longer
    /// has, and this module dropped it — so a hit whose source could not be
    /// read rendered as one whose source is genuinely empty. The fixture writes
    /// only one of its two files to disk, so both cases appear in a single
    /// response.
    #[test]
    fn a_hit_whose_source_could_not_be_read_says_so_instead_of_showing_a_blank_span() {
        let repo = repo_with_one_generation();
        let root = repo.path().to_string_lossy().to_string();

        let hits = search(&root, "probe", None);
        assert!(hits.available, "search refused: {:?}", hits.reason);

        let readable = hits
            .items
            .iter()
            .find(|hit| hit.file_path == "src/caller.rs")
            .expect("the file that exists on disk");
        assert_eq!(readable.source_unavailable_reason, None);
        assert_eq!(readable.source_span, CALLER_SOURCE);

        let missing = hits
            .items
            .iter()
            .find(|hit| hit.file_path == "src/callee.rs")
            .expect("the file that does not exist on disk");
        assert!(missing.source_span.is_empty());
        let reason = missing
            .source_unavailable_reason
            .as_deref()
            .expect("an unreadable source states why");
        assert!(
            reason.contains("src/callee.rs"),
            "the reason does not name the file it is about: {reason}"
        );
    }

    /* ── Finding 2: the repository path is an argument, not a fact ────────── */

    /// No entry point may answer about a repository it did not validate.
    ///
    /// `repo_path` is documented as an absolute path to a git repository, and
    /// nothing checked. `""` joined to the map path yields a *relative* path,
    /// which the filesystem resolves against the server process's own working
    /// directory — so an empty argument read whatever repository the process
    /// happened to be started in, and `"../elsewhere"` reached out of the one
    /// the caller named. Both are cross-repository reads driven purely by an
    /// argument.
    ///
    /// The map is built inside a directory that is deliberately *not* a git
    /// repository: before this gate every surface answered from it, so a
    /// refusal here cannot be produced by the map simply being absent.
    #[test]
    fn no_entry_point_answers_from_a_repository_path_it_did_not_validate() {
        let not_a_repo = tempfile::TempDir::new().expect("tempdir");
        drop(open_fixture_store(not_a_repo.path()));
        let unvalidated = not_a_repo.path().to_string_lossy().to_string();

        for (label, root, expected) in [
            ("empty", "", "Invalid repository path"),
            ("relative", "some/relative/repo", "must be absolute"),
            ("parent-relative", "../sibling", "must be absolute"),
            ("not a git repository", &unvalidated, "Not a Git repository"),
        ] {
            let stat = status(root);
            assert!(!stat.available, "status answered for {label} repo_path");
            let reason = stat.reason.unwrap_or_else(|| panic!("{label}: no reason"));
            assert!(
                reason.contains(expected),
                "{label}: refusal blames the wrong thing: {reason}"
            );
            assert!(
                !reason.contains("devmap.sqlite"),
                "{label}: refused for a missing map rather than for the path: {reason}"
            );
            assert!(
                stat.db_path.is_empty(),
                "{label}: reported a map path built from a path we refused: {}",
                stat.db_path
            );

            // Each surface is reduced to `(name, available, reason)` at the
            // call site rather than collected first: `search` and
            // `dead_symbols` answer over symbols while `impact`,
            // `dependencies` and `trace_between` answer over edges, so the
            // responses have no common type until the generic is dropped.
            let probes: Vec<(&str, bool, Option<String>)> = vec![
                {
                    let r = search(root, "probe", None);
                    ("search", r.available, r.reason)
                },
                {
                    let r = impact(root, "probe_callee", None);
                    ("impact", r.available, r.reason)
                },
                {
                    let r = dependencies(root, "src/caller.rs", None);
                    ("dependencies", r.available, r.reason)
                },
                {
                    let r = dead_symbols(root, None);
                    ("dead_symbols", r.available, r.reason)
                },
                {
                    let r = trace_between(root, "probe_caller", "probe_callee", None);
                    ("trace_between", r.available, r.reason)
                },
            ];
            let refused: Vec<&str> = probes
                .into_iter()
                .filter_map(|(name, available, reason)| {
                    let refused = !available && reason.is_some_and(|r| r.contains(expected));
                    (!refused).then_some(name)
                })
                .collect();
            assert!(
                refused.is_empty(),
                "{label}: these surfaces did not refuse the path: {refused:?}"
            );
        }
    }

    /* ── Finding 3: counting without materializing what is counted ────────── */

    /// The fast counts must equal the rows they stand in for.
    ///
    /// `status` used to call `latest_file_hashes()` and `latest_edges(0.0)` and
    /// throw both result sets away through `.len()`, materializing the whole
    /// edge table — every edge as four owned strings — to learn one integer, on
    /// a call that runs on every `gitpulse_status` and every `gitpulse_insights`.
    /// The replacement counts in SQL, so the contract is that it counts the same
    /// thing: asserted against the row-returning queries themselves rather than
    /// against numbers written down here.
    #[test]
    fn status_counts_agree_with_the_rows_they_stand_in_for() {
        let repo = repo_with_one_generation();
        let root = repo.path().to_string_lossy().to_string();

        let stat = status(&root);
        assert!(stat.available, "status refused: {:?}", stat.reason);
        assert_eq!(stat.generation_id, Some(1));

        let store = Store::open(map_path(repo.path())).expect("open fixture store");
        let files = store.latest_file_hashes().expect("file hashes").len() as u32;
        let edges = store.latest_edges(0.0).expect("edges").len() as u32;
        assert_eq!(stat.total_files, Some(files), "file count drifted");
        assert_eq!(stat.total_edges, Some(edges), "edge count drifted");
        assert_eq!(
            stat.total_symbols,
            Some(2),
            "total_symbols was hardcoded None, so the hook renderer's \
             'N symbols across M files' always said zero"
        );
    }

    /// The file count falls back rather than attributing another build's number.
    ///
    /// `build_history` is where devmap records how many files a generation held,
    /// written inside that generation's own transaction — but history rows are
    /// retained 500 deep while generations are pruned to 2, and a database
    /// migrated up from before the table existed carries generations with no
    /// history row at all. Taking the newest row unconditionally would report
    /// some other build's file count as this one's.
    #[test]
    fn a_generation_whose_history_row_is_missing_still_counts_its_own_files() {
        let repo = repo_with_one_generation();
        let root = repo.path().to_string_lossy().to_string();
        let expected = status(&root).total_files;

        let conn = rusqlite::Connection::open(map_path(repo.path())).expect("open fixture db");
        conn.execute("DELETE FROM build_history", [])
            .expect("drop history");
        // A row describing a generation that is not the latest: taken at face
        // value it would report 9,999 files for a three-file generation.
        conn.execute(
            "INSERT INTO build_history
               (generation_id, built_at, head_sha, files, symbols, edges,
                dead_confident, dead_ambiguous, parse_failed, languages_covered,
                build_ms, db_bytes)
             VALUES (2, 1.0, 'other', 9999, 9999, 9999, 0, 0, 0, 1, NULL, 0)",
            [],
        )
        .expect("insert foreign history row");
        conn.close().expect("close fixture db");

        assert_eq!(
            status(&root).total_files,
            expected,
            "a history row for another generation was reported as this one's file count"
        );
    }
}
