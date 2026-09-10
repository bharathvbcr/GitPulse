//! Code intelligence module: in-process devmap querying over devmap's resolved state directory.
//!
//! Links `devmap-query` and `devmap-store` directly without requiring a background
//! daemon or Unix socket. Provides fast symbol search, impact analysis, dependency
//! tracing, and dead code detection.

use crate::engine::git_cli::validate_repo;
use devmap_query::{
    Cancel, Request, ResolutionAvailability, Response, Rung, RungHistogram, StoreQueryEngine,
};
use devmap_resolve::model::ResolvedEdge;
use devmap_store::{Store, CURRENT_SCHEMA_VERSION};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Default token budget for in-process query operations.
pub const DEFAULT_CODEINTEL_BUDGET: u32 = 2000;

/// Schema version this GitPulse build can read. Pinned to the linked
/// `devmap_store` constant so a re-vendor that drifts fails a test rather than
/// shipping a dead panel.
pub const SUPPORTED_STORE_SCHEMA: i32 = CURRENT_SCHEMA_VERSION;

/// Upstream `MAX_NEIGHBOR_TARGETS` — chunk affected-tests / neighbors seeds here.
pub const MAX_NEIGHBOR_TARGETS: usize = 16;

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
    /// Bytes withheld by the query budget, distinct from unreadable source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_span_omitted_bytes: Option<u32>,
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
pub struct CodeintelRungHistogram {
    pub deterministic: usize,
    pub high: usize,
    pub speculative: usize,
    pub filtered_out: usize,
}

impl From<RungHistogram> for CodeintelRungHistogram {
    fn from(h: RungHistogram) -> Self {
        Self {
            deterministic: h.deterministic,
            high: h.high,
            speculative: h.speculative,
            filtered_out: h.filtered_out,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelResponse<T> {
    /// Null means this query did not verify whole-tree freshness. Availability
    /// and complete traversal of a stored snapshot cannot establish it.
    #[serde(default)]
    pub source_freshness: Option<bool>,
    pub available: bool,
    pub reason: Option<String>,
    pub items: Vec<T>,
    pub total: u32,
    pub shown: u32,
    pub truncated: bool,
    /// Set when the producer stopped early — distinct from budget truncation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub walk_incomplete: Option<String>,
    /// Population across the resolution ladder before any `min_rung` filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rungs: Option<CodeintelRungHistogram>,
}

impl<T> CodeintelResponse<T> {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            source_freshness: None,
            available: false,
            reason: Some(reason.into()),
            items: Vec::new(),
            total: 0,
            shown: 0,
            truncated: false,
            walk_incomplete: None,
            rungs: None,
        }
    }

    pub fn ok(items: Vec<T>, total: u32, shown: u32, truncated: bool) -> Self {
        Self {
            source_freshness: None,
            available: true,
            reason: None,
            items,
            total,
            shown,
            truncated,
            walk_incomplete: None,
            rungs: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelStatus {
    pub available: bool,
    #[serde(default)]
    pub is_fresh: Option<bool>,
    #[serde(default)]
    pub freshness_reason: Option<String>,
    #[serde(default)]
    pub pending_count: Option<usize>,
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
    devmap_query::paths::store_path(repo)
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

fn rewrite_schema_mismatch(raw: &str) -> Option<String> {
    // Vendored store refuses future schemas with this wording. Surface the
    // handshake the UI can act on rather than the rusqlite parameter name.
    const PREFIX: &str = "unsupported future schema version ";
    let idx = raw.find(PREFIX)?;
    let rest = &raw[idx + PREFIX.len()..];
    let version: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if version.is_empty() {
        return None;
    }
    Some(format!(
        "map built by devmap schema {version}, this build reads {SUPPORTED_STORE_SCHEMA}"
    ))
}

fn open_store(repo: &Path) -> Result<Store, String> {
    let db_path = map_path(repo);
    if !db_path.exists() {
        return Err(format!(
            "No devmap database at {}",
            db_path.to_string_lossy()
        ));
    }
    let store = Store::open_read_only(&db_path).map_err(|e| {
        let raw = e.to_string();
        if let Some(friendly) = rewrite_schema_mismatch(&raw) {
            return friendly;
        }
        format!("Failed to open devmap database: {raw}")
    })?;
    store
        .validate_repo_root(repo)
        .map_err(|error| error.to_string())?;
    Ok(store)
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
        walk_incomplete,
        rungs,
        source_freshness,
        ..
    } = response;
    let mut out = match resolution {
        ResolutionAvailability::Unavailable { reason } => CodeintelResponse::unavailable(reason),
        ResolutionAvailability::Available => CodeintelResponse::ok(
            items.into_iter().map(map).collect(),
            total,
            shown,
            truncated,
        ),
    };
    out.walk_incomplete = walk_incomplete;
    out.rungs = rungs.map(CodeintelRungHistogram::from);
    out.source_freshness = source_freshness;
    out
}

fn parse_min_rung(min_rung: Option<&str>) -> Result<Option<Rung>, String> {
    match min_rung.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some("deterministic") => Ok(Some(Rung::Deterministic)),
        Some("high") => Ok(Some(Rung::High)),
        Some("speculative") => Ok(Some(Rung::Speculative)),
        Some(other) => Err(format!(
            "min_rung must be deterministic|high|speculative, got {other}"
        )),
    }
}

fn map_edge(edge: ResolvedEdge) -> CodeintelEdge {
    CodeintelEdge {
        source_file: edge.source_file,
        target_file: edge.target_file,
        source_symbol: edge.source_symbol,
        target_symbol: edge.target_symbol,
        confidence: edge.confidence.0,
    }
}

/// Cooperative cancel token for long walks — thin alias over the kernel's.
pub type QueryCancel = Cancel;

/// Wall-clock backstop for long UI walks. Freeing the await without tripping
/// this flag leaves the blocking traversal running on the pool.
pub const QUERY_CANCEL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

fn cancel_registry() -> &'static std::sync::Mutex<std::collections::HashMap<String, Cancel>> {
    static REGISTRY: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Cancel>>,
    > = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Register a cancel flag for `token`, also tripped after [`QUERY_CANCEL_DEADLINE`].
///
/// UI dismiss (file switch, unmount) calls [`cancel_query`] with the same token
/// so the walk stops rather than only having its answer ignored.
pub fn begin_cancellable_query(token: Option<&str>) -> Cancel {
    let cancel = Cancel::new();
    if let Some(token) = token.filter(|t| !t.is_empty()) {
        let mut map = cancel_registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.insert(token.to_string(), cancel.clone());
    }
    let timed = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(QUERY_CANCEL_DEADLINE);
        timed.cancel();
    });
    cancel
}

/// Trip a previously registered query cancel token (user dismiss / navigation).
pub fn cancel_query(token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let mut map = cancel_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(cancel) = map.remove(token) {
        cancel.cancel();
        true
    } else {
        false
    }
}

/// Drop a finished token so the registry cannot grow with every request.
pub fn finish_cancellable_query(token: Option<&str>) {
    if let Some(token) = token.filter(|t| !t.is_empty()) {
        let mut map = cancel_registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.remove(token);
    }
}

/// An unavailable status, carrying the path we looked at and why we stopped.
fn status_unavailable(db_path: String, reason: String) -> CodeintelStatus {
    CodeintelStatus {
        available: false,
        is_fresh: None,
        freshness_reason: Some(reason.clone()),
        pending_count: None,
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
        is_fresh: Some(summary.is_fresh()),
        freshness_reason: summary.freshness_reason(),
        pending_count: Some(summary.pending_count),
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
            source_span_omitted_bytes: hit.source_span_omitted_bytes,
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
    impact_at_rung(repo_path, target, token_budget, None, None)
}

/// Impact narrowed to a named resolution rung.
pub fn impact_at_rung(
    repo_path: &str,
    target: &str,
    token_budget: Option<u32>,
    min_rung: Option<&str>,
    cancel: Option<Cancel>,
) -> CodeintelResponse<CodeintelEdge> {
    if let Err(e) = require_argument("target", target) {
        return CodeintelResponse::unavailable(e);
    }
    let rung = match parse_min_rung(min_rung) {
        Ok(r) => r,
        Err(e) => return CodeintelResponse::unavailable(e),
    };
    let store = match open_repo_map(repo_path) {
        Ok(s) => s,
        Err(e) => return CodeintelResponse::unavailable(e),
    };
    let mut engine = StoreQueryEngine::new(&store);
    if let Some(cancel) = cancel {
        engine = engine.with_cancel(cancel);
    }
    let req = Request {
        query: target.to_string(),
        token_budget: token_budget.unwrap_or(DEFAULT_CODEINTEL_BUDGET),
        min_confidence: 0.0,
        max_depth: 10,
    };

    match engine.impact_at_rung(req, rung) {
        Ok(res) => from_engine(res, map_edge),
        Err(e) => CodeintelResponse::unavailable(format!("Impact computation failed: {e}")),
    }
}

/// Finds dependencies for a file.
pub fn dependencies(
    repo_path: &str,
    file_path: &str,
    token_budget: Option<u32>,
) -> CodeintelResponse<CodeintelEdge> {
    dependencies_at_rung(repo_path, file_path, token_budget, None, None)
}

/// Dependencies narrowed to a named resolution rung.
pub fn dependencies_at_rung(
    repo_path: &str,
    file_path: &str,
    token_budget: Option<u32>,
    min_rung: Option<&str>,
    cancel: Option<Cancel>,
) -> CodeintelResponse<CodeintelEdge> {
    if let Err(e) = require_argument("file_path", file_path) {
        return CodeintelResponse::unavailable(e);
    }
    let rung = match parse_min_rung(min_rung) {
        Ok(r) => r,
        Err(e) => return CodeintelResponse::unavailable(e),
    };
    let store = match open_repo_map(repo_path) {
        Ok(s) => s,
        Err(e) => return CodeintelResponse::unavailable(e),
    };
    let mut engine = StoreQueryEngine::new(&store);
    if let Some(cancel) = cancel {
        engine = engine.with_cancel(cancel);
    }
    let req = Request {
        query: file_path.to_string(),
        token_budget: token_budget.unwrap_or(DEFAULT_CODEINTEL_BUDGET),
        min_confidence: 0.0,
        max_depth: 10,
    };

    match engine.dependencies_at_rung(req, rung) {
        Ok(res) => from_engine(res, map_edge),
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
    trace_between_at_rung(repo_path, from, to, token_budget, None, None)
}

/// Trace narrowed to a named resolution rung.
pub fn trace_between_at_rung(
    repo_path: &str,
    from: &str,
    to: &str,
    token_budget: Option<u32>,
    min_rung: Option<&str>,
    cancel: Option<Cancel>,
) -> CodeintelResponse<CodeintelEdge> {
    if let Err(e) = require_argument("from", from).and_then(|()| require_argument("to", to)) {
        return CodeintelResponse::unavailable(e);
    }
    let rung = match parse_min_rung(min_rung) {
        Ok(r) => r,
        Err(e) => return CodeintelResponse::unavailable(e),
    };
    let store = match open_repo_map(repo_path) {
        Ok(s) => s,
        Err(e) => return CodeintelResponse::unavailable(e),
    };
    let mut engine = StoreQueryEngine::new(&store);
    if let Some(cancel) = cancel {
        engine = engine.with_cancel(cancel);
    }
    let req = Request {
        query: (from.to_string(), to.to_string()),
        token_budget: token_budget.unwrap_or(DEFAULT_CODEINTEL_BUDGET),
        min_confidence: 0.0,
        max_depth: 10,
    };

    match engine.trace_between(req) {
        Ok(mut res) => {
            if rung.is_some() {
                let (kept, histogram) =
                    devmap_query::rung::narrow(std::mem::take(&mut res.items), rung);
                res.items = kept;
                res.rungs = Some(histogram);
            }
            from_engine(res, map_edge)
        }
        Err(e) => CodeintelResponse::unavailable(format!("Trace between failed: {e}")),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelNeighbors {
    pub target: String,
    pub callers: CodeintelResponse<CodeintelEdge>,
    pub callees: CodeintelResponse<CodeintelEdge>,
}

/// Callers and callees for one or more targets (chunked at [`MAX_NEIGHBOR_TARGETS`]).
pub fn neighbors(
    repo_path: &str,
    targets: &[String],
    token_budget: Option<u32>,
    min_rung: Option<&str>,
) -> Result<Vec<CodeintelNeighbors>, String> {
    if targets.is_empty() {
        return Err("neighbors requires at least one target".into());
    }
    let rung = parse_min_rung(min_rung)?;
    let store = open_repo_map(repo_path)?;
    let engine = StoreQueryEngine::new(&store);
    let budget = token_budget.unwrap_or(DEFAULT_CODEINTEL_BUDGET);
    let mut out = Vec::new();
    for chunk in targets.chunks(MAX_NEIGHBOR_TARGETS) {
        let report = engine
            .neighbors_at_rung(chunk, budget, 0.0, 10, rung)
            .map_err(|e| format!("neighbors failed: {e}"))?;
        for entry in report {
            out.push(CodeintelNeighbors {
                target: entry.target,
                callers: from_engine(entry.callers, map_edge),
                callees: from_engine(entry.callees, map_edge),
            });
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelBlastLayer {
    pub depth: usize,
    pub nodes: Vec<String>,
    pub node_count: u32,
    pub nodes_omitted: u32,
    pub lowest_confidence: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelBlastRadius {
    pub seeds: Vec<String>,
    pub unmatched_targets: Vec<String>,
    pub layers: CodeintelResponse<CodeintelBlastLayer>,
    pub total_impacted: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelLayeredImpact {
    pub available: bool,
    pub reason: Option<String>,
    pub edges: CodeintelResponse<CodeintelEdge>,
    pub blast_radius: CodeintelBlastRadius,
}

fn map_blast_radius(radius: devmap_query::BlastRadius) -> CodeintelBlastRadius {
    CodeintelBlastRadius {
        seeds: radius.seeds,
        unmatched_targets: radius.unmatched_targets,
        layers: from_engine(radius.layers, |layer| CodeintelBlastLayer {
            depth: layer.depth,
            nodes: layer.nodes,
            node_count: layer.node_count,
            nodes_omitted: layer.nodes_omitted,
            lowest_confidence: layer.lowest_confidence,
        }),
        total_impacted: radius.total_impacted,
    }
}

/// Layered impact for one target. Do not combine with `min_rung` — the kernel
/// refuses that pairing; the UI must not offer both.
pub fn impact_layered(
    repo_path: &str,
    target: &str,
    token_budget: Option<u32>,
) -> CodeintelLayeredImpact {
    impact_layered_with_cancel(repo_path, target, token_budget, None)
}

/// Layered impact with an optional cooperative cancel token.
pub fn impact_layered_with_cancel(
    repo_path: &str,
    target: &str,
    token_budget: Option<u32>,
    cancel: Option<Cancel>,
) -> CodeintelLayeredImpact {
    if let Err(e) = require_argument("target", target) {
        return CodeintelLayeredImpact {
            available: false,
            reason: Some(e),
            edges: CodeintelResponse::unavailable("target must not be blank"),
            blast_radius: CodeintelBlastRadius {
                seeds: Vec::new(),
                unmatched_targets: Vec::new(),
                layers: CodeintelResponse::unavailable("no target"),
                total_impacted: 0,
            },
        };
    }
    let store = match open_repo_map(repo_path) {
        Ok(s) => s,
        Err(e) => {
            return CodeintelLayeredImpact {
                available: false,
                reason: Some(e.clone()),
                edges: CodeintelResponse::unavailable(e.clone()),
                blast_radius: CodeintelBlastRadius {
                    seeds: Vec::new(),
                    unmatched_targets: vec![target.to_string()],
                    layers: CodeintelResponse::unavailable(e),
                    total_impacted: 0,
                },
            }
        }
    };
    let mut engine = StoreQueryEngine::new(&store);
    if let Some(cancel) = cancel {
        engine = engine.with_cancel(cancel);
    }
    let req = Request {
        query: target.to_string(),
        token_budget: token_budget.unwrap_or(DEFAULT_CODEINTEL_BUDGET),
        min_confidence: 0.0,
        max_depth: 10,
    };
    match engine.impact_layered(req) {
        Ok(layered) => {
            let edges = from_engine(layered.edges, map_edge);
            let available = edges.available;
            let reason = edges.reason.clone();
            CodeintelLayeredImpact {
                available,
                reason,
                blast_radius: map_blast_radius(layered.blast_radius),
                edges,
            }
        }
        Err(e) => CodeintelLayeredImpact {
            available: false,
            reason: Some(format!("layered impact failed: {e}")),
            edges: CodeintelResponse::unavailable(format!("layered impact failed: {e}")),
            blast_radius: CodeintelBlastRadius {
                seeds: Vec::new(),
                unmatched_targets: vec![target.to_string()],
                layers: CodeintelResponse::unavailable(format!("layered impact failed: {e}")),
                total_impacted: 0,
            },
        },
    }
}

/// Compose layered impact over a changed-file set, chunked at 16 targets.
pub fn impact_layered_many(
    repo_path: &str,
    targets: &[String],
    token_budget: Option<u32>,
) -> Vec<CodeintelLayeredImpact> {
    impact_layered_many_with_cancel(repo_path, targets, token_budget, None)
}

/// Layered-many with a shared cancel token (checked between targets).
pub fn impact_layered_many_with_cancel(
    repo_path: &str,
    targets: &[String],
    token_budget: Option<u32>,
    cancel: Option<Cancel>,
) -> Vec<CodeintelLayeredImpact> {
    targets
        .iter()
        .map(|t| {
            if let Some(cancel) = cancel.as_ref() {
                if cancel.is_cancelled() {
                    return CodeintelLayeredImpact {
                        available: false,
                        reason: Some("query cancelled".into()),
                        edges: CodeintelResponse::unavailable("query cancelled"),
                        blast_radius: CodeintelBlastRadius {
                            seeds: Vec::new(),
                            unmatched_targets: vec![t.clone()],
                            layers: CodeintelResponse::unavailable("query cancelled"),
                            total_impacted: 0,
                        },
                    };
                }
            }
            impact_layered_with_cancel(repo_path, t, token_budget, cancel.clone())
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelAffectedTest {
    pub path: String,
    pub depth: usize,
    pub symbols: Vec<String>,
    pub reached_symbols: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelAffectedTests {
    pub available: bool,
    pub reason: Option<String>,
    pub targets: Vec<String>,
    pub tests: CodeintelResponse<CodeintelAffectedTest>,
    pub blast_radius: CodeintelBlastRadius,
    /// True when the map was stale, an answer incomplete or truncated, or a seed unmatched —
    /// callers must fall back to the full suite and say why.
    pub fail_closed: bool,
    pub fail_closed_reason: Option<String>,
}

/// Affected test **files** for a set of seeds. Chunks at [`MAX_NEIGHBOR_TARGETS`].
pub fn affected_tests(
    repo_path: &str,
    targets: &[String],
    token_budget: Option<u32>,
    max_depth: Option<usize>,
) -> CodeintelAffectedTests {
    if targets.is_empty() {
        return CodeintelAffectedTests {
            available: false,
            reason: Some("affected_tests requires at least one target".into()),
            targets: Vec::new(),
            tests: CodeintelResponse::unavailable("no targets"),
            blast_radius: CodeintelBlastRadius {
                seeds: Vec::new(),
                unmatched_targets: Vec::new(),
                layers: CodeintelResponse::unavailable("no targets"),
                total_impacted: 0,
            },
            fail_closed: true,
            fail_closed_reason: Some("no seeds provided".into()),
        };
    }
    let repo = match resolve_repo(repo_path) {
        Ok(r) => r,
        Err(e) => {
            return CodeintelAffectedTests {
                available: false,
                reason: Some(e.clone()),
                targets: targets.to_vec(),
                tests: CodeintelResponse::unavailable(e.clone()),
                blast_radius: CodeintelBlastRadius {
                    seeds: Vec::new(),
                    unmatched_targets: targets.to_vec(),
                    layers: CodeintelResponse::unavailable(e.clone()),
                    total_impacted: 0,
                },
                fail_closed: true,
                fail_closed_reason: Some(e),
            }
        }
    };
    let store = match open_indexed_store(&repo) {
        Ok(s) => s,
        Err(e) => {
            return CodeintelAffectedTests {
                available: false,
                reason: Some(e.clone()),
                targets: targets.to_vec(),
                tests: CodeintelResponse::unavailable(e.clone()),
                blast_radius: CodeintelBlastRadius {
                    seeds: Vec::new(),
                    unmatched_targets: targets.to_vec(),
                    layers: CodeintelResponse::unavailable(e.clone()),
                    total_impacted: 0,
                },
                fail_closed: true,
                fail_closed_reason: Some(e),
            }
        }
    };
    // A map with pending paths is not a complete picture of this checkout.
    // Querying it and treating the answer as "these are the tests to run"
    // is exactly the failure mode fail_closed exists to prevent.
    let db_str = map_path(&repo).to_string_lossy().into_owned();
    let freshness_failure = match store.status(&db_str) {
        Ok(summary) => summary
            .freshness_reason()
            .map(|reason| (true, format!("code map is stale or degraded: {reason}"))),
        Err(error) => Some((false, format!("code map freshness check failed: {error}"))),
    };
    if let Some((available, reason)) = freshness_failure {
        return CodeintelAffectedTests {
            available,
            reason: Some(reason.clone()),
            targets: targets.to_vec(),
            tests: CodeintelResponse::unavailable(reason.clone()),
            blast_radius: CodeintelBlastRadius {
                seeds: Vec::new(),
                unmatched_targets: targets.to_vec(),
                layers: CodeintelResponse::unavailable(reason.clone()),
                total_impacted: 0,
            },
            fail_closed: true,
            fail_closed_reason: Some(reason),
        };
    }
    affected_tests_from_store(
        &store,
        targets,
        token_budget.unwrap_or(DEFAULT_CODEINTEL_BUDGET),
        max_depth.unwrap_or(10),
    )
}

fn affected_tests_from_store(
    store: &Store,
    targets: &[String],
    budget: u32,
    depth: usize,
) -> CodeintelAffectedTests {
    let engine = StoreQueryEngine::new(store);
    let mut tests = None;
    let mut layers = None;
    let mut unmatched = Vec::new();
    let mut seeds = Vec::new();
    let mut total_impacted = 0u32;
    let chunks = targets.chunks(MAX_NEIGHBOR_TARGETS);
    let chunk_count = u32::try_from(chunks.len()).unwrap_or(u32::MAX).max(1);
    for (index, chunk) in chunks.enumerate() {
        // One caller budget across all batches, including the remainder.
        let chunk_budget =
            budget / chunk_count + u32::from(index < (budget % chunk_count) as usize);
        match engine.affected_tests(chunk, chunk_budget, 0.0, depth) {
            Ok(report) => {
                unmatched.extend(report.blast_radius.unmatched_targets);
                seeds.extend(report.blast_radius.seeds);
                total_impacted = total_impacted.saturating_add(report.blast_radius.total_impacted);
                append_affected_response(
                    &mut tests,
                    from_engine(report.tests, |t| CodeintelAffectedTest {
                        path: t.path,
                        depth: t.depth,
                        symbols: t.symbols,
                        reached_symbols: t.reached_symbols,
                    }),
                );
                append_affected_response(
                    &mut layers,
                    from_engine(report.blast_radius.layers, |layer| CodeintelBlastLayer {
                        depth: layer.depth,
                        nodes: layer.nodes,
                        node_count: layer.node_count,
                        nodes_omitted: layer.nodes_omitted,
                        lowest_confidence: layer.lowest_confidence,
                    }),
                );
            }
            Err(e) => {
                return CodeintelAffectedTests {
                    available: false,
                    reason: Some(format!("affected_tests failed: {e}")),
                    targets: targets.to_vec(),
                    tests: CodeintelResponse::unavailable(format!("affected_tests failed: {e}")),
                    blast_radius: CodeintelBlastRadius {
                        seeds: Vec::new(),
                        unmatched_targets: targets.to_vec(),
                        layers: CodeintelResponse::unavailable(format!(
                            "affected_tests failed: {e}"
                        )),
                        total_impacted: 0,
                    },
                    fail_closed: true,
                    fail_closed_reason: Some(format!("affected_tests failed: {e}")),
                };
            }
        }
    }
    let mut tests = tests.unwrap_or_else(|| CodeintelResponse::unavailable("no target batches"));
    let mut layers = layers.unwrap_or_else(|| CodeintelResponse::unavailable("no target batches"));
    if chunk_count > 1 {
        // Batches can overlap and observe different generations. Do not claim
        // a unique, coherent union from their independently bounded answers.
        let scope = format!("combined {chunk_count} independently queried target batches; totals count occurrences across batches and may repeat symbols or files");
        append_affected_reason(&mut tests.walk_incomplete, Some(scope.clone()));
        append_affected_reason(&mut layers.walk_incomplete, Some(scope));
    }
    let available = tests.available && layers.available;
    let mut reason = tests.reason.clone();
    append_affected_reason(&mut reason, layers.reason.clone());
    let mut fail_closed_reason = reason.clone();
    append_affected_reason(&mut fail_closed_reason, tests.walk_incomplete.clone());
    append_affected_reason(&mut fail_closed_reason, layers.walk_incomplete.clone());
    if tests.truncated || layers.truncated {
        append_affected_reason(
            &mut fail_closed_reason,
            Some("query budget omitted affected tests or blast-radius layers".into()),
        );
    }
    if !unmatched.is_empty() {
        append_affected_reason(
            &mut fail_closed_reason,
            Some(format!(
                "{} seed(s) matched nothing in the map",
                unmatched.len()
            )),
        );
    }
    if !available && fail_closed_reason.is_none() {
        fail_closed_reason = Some("query unavailable".into());
    }
    CodeintelAffectedTests {
        available,
        reason,
        targets: targets.to_vec(),
        tests,
        blast_radius: CodeintelBlastRadius {
            seeds,
            unmatched_targets: unmatched,
            layers,
            total_impacted,
        },
        fail_closed: fail_closed_reason.is_some(),
        fail_closed_reason,
    }
}

fn append_affected_reason(target: &mut Option<String>, incoming: Option<String>) {
    if let Some(incoming) = incoming {
        match target {
            Some(existing) if *existing != incoming => {
                existing.push_str("; ");
                existing.push_str(&incoming);
            }
            None => *target = Some(incoming),
            _ => {}
        }
    }
}

/// Both lists in an affected-test answer retain the same envelope semantics.
fn append_affected_response<T>(
    target: &mut Option<CodeintelResponse<T>>,
    incoming: CodeintelResponse<T>,
) {
    let Some(existing) = target else {
        *target = Some(incoming);
        return;
    };
    existing.available &= incoming.available;
    existing.total = existing.total.saturating_add(incoming.total);
    existing.shown = existing.shown.saturating_add(incoming.shown);
    existing.truncated |= incoming.truncated;
    existing.items.extend(incoming.items);
    append_affected_reason(&mut existing.reason, incoming.reason);
    append_affected_reason(&mut existing.walk_incomplete, incoming.walk_incomplete);
    existing.source_freshness = match (existing.source_freshness, incoming.source_freshness) {
        (Some(false), _) | (_, Some(false)) => Some(false),
        (Some(true), Some(true)) => Some(true),
        _ => None,
    };
    existing.rungs = match (existing.rungs.take(), incoming.rungs) {
        (Some(left), Some(right)) => Some(CodeintelRungHistogram {
            deterministic: left.deterministic.saturating_add(right.deterministic),
            high: left.high.saturating_add(right.high),
            speculative: left.speculative.saturating_add(right.speculative),
            filtered_out: left.filtered_out.saturating_add(right.filtered_out),
        }),
        _ => None,
    };
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelExploreDefinition {
    pub symbol_name: String,
    pub file_path: String,
    pub kind: String,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelExplore {
    pub available: bool,
    pub reason: Option<String>,
    pub definitions: CodeintelResponse<CodeintelExploreDefinition>,
    pub blast_radius: CodeintelBlastRadius,
    pub limit: u32,
}

/// Explore a symbol: definitions plus blast radius.
pub fn explore(
    repo_path: &str,
    query: &str,
    token_budget: Option<u32>,
    limit: Option<u32>,
) -> CodeintelExplore {
    if let Err(e) = require_argument("query", query) {
        return CodeintelExplore {
            available: false,
            reason: Some(e),
            definitions: CodeintelResponse::unavailable("query must not be blank"),
            blast_radius: CodeintelBlastRadius {
                seeds: Vec::new(),
                unmatched_targets: Vec::new(),
                layers: CodeintelResponse::unavailable("no query"),
                total_impacted: 0,
            },
            limit: 0,
        };
    }
    let store = match open_repo_map(repo_path) {
        Ok(s) => s,
        Err(e) => {
            return CodeintelExplore {
                available: false,
                reason: Some(e.clone()),
                definitions: CodeintelResponse::unavailable(e.clone()),
                blast_radius: CodeintelBlastRadius {
                    seeds: Vec::new(),
                    unmatched_targets: vec![query.to_string()],
                    layers: CodeintelResponse::unavailable(e),
                    total_impacted: 0,
                },
                limit: 0,
            }
        }
    };
    let engine = StoreQueryEngine::new(&store);
    let budget = token_budget.unwrap_or(DEFAULT_CODEINTEL_BUDGET);
    let limit = limit.unwrap_or(20) as usize;
    match engine.explore(query, limit, budget, 0.0, 10) {
        Ok(report) => {
            let definitions = from_engine(report.definitions, |d| CodeintelExploreDefinition {
                symbol_name: d.symbol_name,
                file_path: d.file_path,
                kind: d.kind,
                id: d.id,
            });
            CodeintelExplore {
                available: definitions.available,
                reason: definitions.reason.clone(),
                blast_radius: map_blast_radius(report.blast_radius),
                definitions,
                limit: report.limit,
            }
        }
        Err(e) => CodeintelExplore {
            available: false,
            reason: Some(format!("explore failed: {e}")),
            definitions: CodeintelResponse::unavailable(format!("explore failed: {e}")),
            blast_radius: CodeintelBlastRadius {
                seeds: Vec::new(),
                unmatched_targets: vec![query.to_string()],
                layers: CodeintelResponse::unavailable(format!("explore failed: {e}")),
                total_impacted: 0,
            },
            limit: u32::try_from(limit).unwrap_or(u32::MAX),
        },
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelCloneGroup {
    pub size: usize,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeintelClones {
    pub available: bool,
    pub reason: Option<String>,
    pub groups: CodeintelResponse<CodeintelCloneGroup>,
    pub signed_symbols: usize,
    pub unsigned_symbols: usize,
}

/// Duplicate-code groups with coverage honesty.
pub fn clones(repo_path: &str, token_budget: Option<u32>) -> CodeintelClones {
    let store = match open_repo_map(repo_path) {
        Ok(s) => s,
        Err(e) => {
            return CodeintelClones {
                available: false,
                reason: Some(e.clone()),
                groups: CodeintelResponse::unavailable(e),
                signed_symbols: 0,
                unsigned_symbols: 0,
            }
        }
    };
    let engine = StoreQueryEngine::new(&store);
    let budget = token_budget.unwrap_or(DEFAULT_CODEINTEL_BUDGET);
    match engine.clones(budget, None, 0) {
        Ok(report) => {
            let groups = from_engine(report.groups, |g| CodeintelCloneGroup {
                size: g.members.len(),
                members: g
                    .members
                    .into_iter()
                    .map(|m| format!("{}::{}", m.file_path, m.symbol_name))
                    .collect(),
            });
            CodeintelClones {
                available: groups.available,
                reason: groups.reason.clone(),
                groups,
                signed_symbols: report.signed_symbols,
                unsigned_symbols: report.unsigned_symbols,
            }
        }
        Err(e) => CodeintelClones {
            available: false,
            reason: Some(format!("clones failed: {e}")),
            groups: CodeintelResponse::unavailable(format!("clones failed: {e}")),
            signed_symbols: 0,
            unsigned_symbols: 0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reader_refuses_a_store_bound_to_another_worktree() {
        let repo = repo_with_empty_store();
        let other = tempfile::tempdir().unwrap();
        let store = open_fixture_store(repo.path());
        store.bind_repo_root(other.path()).unwrap();
        drop(store);
        let error = open_store(repo.path())
            .err()
            .expect("cross-worktree store was accepted");
        assert!(error.contains("belongs to worktree"), "{error}");
    }

    #[test]
    fn supported_schema_matches_linked_store_constant() {
        assert_eq!(
            SUPPORTED_STORE_SCHEMA, CURRENT_SCHEMA_VERSION,
            "SUPPORTED_STORE_SCHEMA must track the linked devmap_store constant"
        );
        const {
            assert!(
                SUPPORTED_STORE_SCHEMA >= 19,
                "GitPulse must read schema 19+ maps"
            );
        }
    }

    #[test]
    fn schema_mismatch_reason_names_both_versions() {
        let friendly =
            rewrite_schema_mismatch("unsupported future schema version 19").expect("parse");
        assert!(friendly.contains("schema 19"), "{friendly}");
        assert!(
            friendly.contains(&format!("reads {SUPPORTED_STORE_SCHEMA}")),
            "{friendly}"
        );
    }

    #[test]
    fn devmap_db_path_follows_the_canonical_state_directory_precedence() {
        let repo = tempfile::TempDir::new().expect("tempdir");
        let standalone = repo.path().join(".devmap");
        let legacy = repo.path().join(".devcouncil");

        assert_eq!(
            devmap_db_path(repo.path().to_str().unwrap()),
            standalone.join("codeintel/devmap.sqlite"),
            "a fresh repository uses the standalone layout"
        );

        std::fs::create_dir_all(&legacy).expect("legacy state");
        assert_eq!(
            devmap_db_path(repo.path().to_str().unwrap()),
            legacy.join("codeintel/devmap.sqlite"),
            "a legacy-only repository remains readable"
        );

        std::fs::create_dir_all(&standalone).expect("standalone state");
        assert_eq!(
            devmap_db_path(repo.path().to_str().unwrap()),
            standalone.join("codeintel/devmap.sqlite"),
            "a migrated repository prefers the standalone layout"
        );
    }

    #[test]
    fn devmap_home_override_is_observed_in_an_isolated_process() {
        if let Some(expected) = std::env::var_os("GITPULSE_DEVMAP_HOME_CHILD") {
            assert_eq!(
                devmap_db_path("/ignored/repository"),
                PathBuf::from(expected).join("codeintel/devmap.sqlite")
            );
            return;
        }

        let home = tempfile::TempDir::new().expect("override");
        let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "codeintel::tests::devmap_home_override_is_observed_in_an_isolated_process",
            ])
            .env("DEVMAP_HOME", home.path())
            .env("GITPULSE_DEVMAP_HOME_CHILD", home.path())
            .status()
            .expect("run isolated test process");
        assert!(
            status.success(),
            "isolated DEVMAP_HOME resolver test failed"
        );
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
            // Since schema v17 `generation_files` is a view over
            // `file_payloads` + `generation_file_rows` — insert the base tables.
            conn.execute(
                "INSERT INTO file_payloads
                   (payload_id, file_id, content_hash, language,
                    parse_outcome_json, engine_json, extraction_json,
                    grammar_version, analyzer_version)
                 VALUES (?1, ?1, ?2, 'rust', '\"Clean\"', '\"ConfigScanner\"', 'null', 'v1', 'v1')",
                // Signed SQLite value emitted by the real devmap writer for
                // CALLER_SOURCE. The readable-source assertion below pins the
                // fixture bytes and this identity together.
                (
                    id,
                    if id == 1 {
                        -477_759_399_776_341_435i64
                    } else {
                        id
                    },
                ),
            )
            .expect("insert file payload");
            conn.execute(
                "INSERT INTO generation_file_rows (generation_id, file_id, payload_id)
                 VALUES (1, ?1, ?1)",
                [id],
            )
            .expect("insert generation file row");
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

        // Since schema v18 `generation_edges` is a view over `edge_rows`
        // joined to generations by validity range.
        conn.execute(
            "INSERT INTO edge_rows
               (edge_id, source_file_id, target_file_id,
                source_symbol, target_symbol, edge_kind, confidence,
                valid_from, valid_to)
             VALUES (0, 1, 2, 'probe_caller', 'probe_callee', 'Calls', 0.9, 1, NULL)",
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

    #[test]
    fn affected_test_aggregation_keeps_budget_omissions_and_refusals() {
        let repo = repo_with_one_generation();
        let conn = rusqlite::Connection::open(map_path(repo.path())).unwrap();
        conn.execute(
            "UPDATE paths SET path = 'tests/probe_test.rs' WHERE id = 1",
            [],
        )
        .unwrap();
        drop(conn);
        let store = Store::open_read_only(map_path(repo.path())).unwrap();
        let targets = vec!["probe_callee".to_owned()];
        let raw = StoreQueryEngine::new(&store)
            .affected_tests(&targets, 0, 0.0, 10)
            .unwrap();
        assert!(raw.tests.truncated);
        assert_eq!(raw.tests.total, 1);
        assert!(raw.blast_radius.layers.truncated);
        let report = affected_tests_from_store(&store, &targets, 0, 10);
        assert_eq!(report.tests.total, 1);
        assert_eq!(report.tests.shown, 0);
        assert!(report.tests.truncated);
        assert_eq!(
            report.blast_radius.layers.total,
            raw.blast_radius.layers.total
        );
        assert!(report.blast_radius.layers.truncated);
        assert!(report.fail_closed);
        assert!(report.fail_closed_reason.is_some());

        let empty = Store::open_in_memory().unwrap();
        let refused = affected_tests_from_store(&empty, &targets, 2000, 10);
        assert!(!refused.tests.available);
        assert!(!refused.blast_radius.layers.available);
        assert!(refused.fail_closed);
    }

    #[test]
    fn affected_test_batches_share_budget_and_disclose_overlap() {
        let repo = repo_with_one_generation();
        let conn = rusqlite::Connection::open(map_path(repo.path())).unwrap();
        conn.execute(
            "UPDATE paths SET path = 'tests/probe_test.rs' WHERE id = 1",
            [],
        )
        .unwrap();
        drop(conn);
        let store = Store::open_read_only(map_path(repo.path())).unwrap();
        let targets = vec!["probe_callee".to_owned(); MAX_NEIGHBOR_TARGETS + 1];
        let report = affected_tests_from_store(&store, &targets, 100, 10);
        let engine = StoreQueryEngine::new(&store);
        let mut expected_shown = 0;
        let mut tokens = 0;
        for chunk in targets.chunks(MAX_NEIGHBOR_TARGETS) {
            let part = engine.affected_tests(chunk, 50, 0.0, 10).unwrap();
            tokens += part.tests.tokens_used + part.blast_radius.layers.tokens_used;
            expected_shown += part.tests.shown;
        }
        assert!(tokens <= 100);
        assert_eq!(report.tests.shown, expected_shown);
        assert_eq!(report.tests.total, 2);
        assert!(report
            .tests
            .walk_incomplete
            .as_deref()
            .unwrap()
            .contains("count occurrences"));
        assert!(report.blast_radius.layers.walk_incomplete.is_some());
        assert!(report.fail_closed);
    }

    #[test]
    fn affected_response_merge_keeps_independent_failure_metadata() {
        let mut first = CodeintelResponse::ok(vec![1], 2, 1, true);
        first.walk_incomplete = Some("parse loss".into());
        first.source_freshness = Some(true);
        let mut second = CodeintelResponse::unavailable("index unavailable");
        second.walk_incomplete = Some("depth capped".into());
        second.source_freshness = Some(false);
        let mut combined = Some(first);
        append_affected_response(&mut combined, second);
        let result = combined.unwrap();
        assert!(!result.available);
        assert_eq!(result.reason.as_deref(), Some("index unavailable"));
        assert_eq!(result.total, 2);
        assert_eq!(result.shown, 1);
        assert!(result.truncated);
        assert_eq!(result.source_freshness, Some(false));
        let reason = result.walk_incomplete.unwrap();
        assert!(reason.contains("parse loss") && reason.contains("depth capped"));
    }

    #[test]
    fn unavailable_answers_preserve_coverage_and_resolution_metadata() {
        let store = Store::open_in_memory().unwrap();
        let mut response = StoreQueryEngine::new(&store)
            .impact(Request {
                query: "missing".into(),
                token_budget: 2000,
                min_confidence: 0.0,
                max_depth: 3,
            })
            .unwrap();
        response.walk_incomplete = Some("repository-wide attribution coverage is unknown".into());
        response.source_freshness = Some(false);
        response.rungs = Some(RungHistogram {
            deterministic: 2,
            high: 1,
            speculative: 3,
            filtered_out: 4,
        });
        let mapped = from_engine(response, |edge| edge.source_symbol);
        assert!(!mapped.available);
        assert_eq!(mapped.source_freshness, Some(false));
        assert_eq!(
            mapped.walk_incomplete.as_deref(),
            Some("repository-wide attribution coverage is unknown")
        );
        assert_eq!(mapped.rungs.unwrap().filtered_out, 4);
    }

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

    #[test]
    fn edited_source_is_withheld_without_losing_the_stored_hit() {
        let repo = repo_with_one_generation();
        let root = repo.path().to_string_lossy().to_string();
        std::fs::write(repo.path().join("src/caller.rs"), "fn renamed() {}\n").unwrap();
        let hits = search(&root, "probe_caller", None);
        assert!(hits.available, "{:?}", hits.reason);
        let hit = hits
            .items
            .first()
            .expect("the indexed symbol remains visible");
        assert_eq!(hit.symbol_name, "probe_caller");
        assert!(hit.source_span.is_empty());
        assert!(hit
            .source_unavailable_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("changed")));
    }

    #[test]
    fn staleness_audit_query_envelopes_preserve_unknown_freshness() {
        let repo = repo_with_one_generation();
        let root = repo.path().to_str().unwrap();
        let responses = [
            serde_json::to_value(search(root, "probe", None)).unwrap(),
            serde_json::to_value(dependencies(root, "src/caller.rs", None)).unwrap(),
            serde_json::to_value(impact(root, "probe_callee", None)).unwrap(),
            serde_json::to_value(trace_between(root, "probe_caller", "probe_callee", None))
                .unwrap(),
            serde_json::to_value(search(root, "missing", None)).unwrap(),
            serde_json::to_value(search(root, " ", None)).unwrap(),
        ];
        for response in responses {
            assert_eq!(
                response.get("source_freshness"),
                Some(&serde_json::Value::Null),
                "query availability does not verify source freshness: {response}"
            );
        }
    }

    #[test]
    fn staleness_audit_adapter_preserves_each_engine_freshness_verdict() {
        let repo = repo_with_one_generation();
        let store = open_repo_map(repo.path().to_str().unwrap()).unwrap();
        let engine = StoreQueryEngine::new(&store);
        for verdict in [None, Some(false), Some(true)] {
            let mut response = engine
                .search(Request {
                    query: "probe_caller".into(),
                    token_budget: 2000,
                    min_confidence: 0.0,
                    max_depth: 1,
                })
                .unwrap();
            response.source_freshness = verdict;
            let mapped = from_engine(response, |hit| hit.symbol_name);
            assert_eq!(mapped.source_freshness, verdict);
            assert_eq!(mapped.items, vec!["probe_caller"]);
        }
        let missing = git_repo();
        let unavailable = status(missing.path().to_str().unwrap());
        assert!(!unavailable.available);
        assert_eq!(unavailable.is_fresh, None);
        assert_eq!(unavailable.pending_count, None);
        assert!(unavailable.freshness_reason.is_some());
    }

    #[test]
    fn staleness_audit_budgeted_source_reports_omitted_bytes() {
        let repo = repo_with_one_generation();
        let response = serde_json::to_value(search(
            repo.path().to_str().unwrap(),
            "probe_caller",
            Some(80),
        ))
        .unwrap();
        assert_eq!(response["shown"], 1);
        let hit = &response["items"][0];
        assert_eq!(hit["source_unavailable_reason"], serde_json::Value::Null);
        assert_eq!(
            hit["source_span"].as_str().unwrap().len()
                + hit["source_span_omitted_bytes"]
                    .as_u64()
                    .expect("budget omission must be preserved") as usize,
            CALLER_SOURCE.len()
        );
    }

    #[test]
    fn staleness_audit_status_preserves_the_stores_degraded_reason() {
        let repo = repo_with_one_generation();
        let root = repo.path().to_str().unwrap();
        let response = serde_json::to_value(status(root)).unwrap();
        assert_eq!(response["available"], true, "stale navigation stays usable");
        assert_eq!(response.get("is_fresh"), Some(&serde_json::json!(false)));
        assert!(
            response["freshness_reason"]
                .as_str()
                .is_some_and(|s| s.contains("analyzer")),
            "fixture analyzer identity cannot be certified by the embedded reader: {response}"
        );
        assert_eq!(response.get("pending_count"), Some(&serde_json::json!(0)));
    }

    #[test]
    fn staleness_audit_failed_status_cannot_authorize_test_selection() {
        let repo = repo_with_one_generation();
        let root = repo.path().to_str().unwrap();
        let conn = rusqlite::Connection::open(map_path(repo.path())).unwrap();
        conn.execute(
            "INSERT INTO generation_coverage_gaps (generation_id, gap, path, reason)
            VALUES (1, 'parse_failed', 'src/caller.rs', X'FF')",
            [],
        )
        .unwrap();
        drop(conn);
        let store = open_repo_map(root).unwrap();
        let error = store
            .status("")
            .expect_err("corrupt status evidence must fail");
        assert!(error.to_string().contains("Invalid column type"), "{error}");
        let selected = affected_tests(root, &["probe_callee".to_string()], None, None);
        assert!(
            selected.fail_closed,
            "status read failed but selection was authorized: {selected:?}"
        );
        assert!(
            !selected.tests.available,
            "unverified test selection must be unavailable"
        );
        assert!(
            selected
                .fail_closed_reason
                .as_deref()
                .is_some_and(|s| s.contains("Invalid column type")),
            "the actual status failure must be preserved: {selected:?}"
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

    /// Pending paths make the map stale: affected_tests must fail closed
    /// rather than hand CI a partial test list wearing a complete badge.
    ///
    /// Uses a minimal generation row only — the richer `repo_with_one_generation`
    /// fixture inserts into `generation_files`, which is a view on current
    /// schemas and is out of scope to rewrite here.
    #[test]
    fn affected_tests_fail_closed_when_the_map_has_pending_paths() {
        let dir = git_repo();
        let root = dir.path().to_string_lossy().to_string();
        let store = open_fixture_store(dir.path());
        let db = map_path(dir.path());
        drop(store);

        let conn = rusqlite::Connection::open(&db).expect("open fixture db");
        conn.execute(
            "INSERT INTO generations (id, created_at, head_sha, analysis_json, repo_root)
             VALUES (1, 0.0, 'fixture', '{}', ?1)",
            [&root],
        )
        .expect("insert generation");
        conn.close().expect("close");

        let store = Store::open(&db).expect("reopen store");
        store
            .enqueue_pending_paths(&["src/new_file.rs".into()])
            .expect("enqueue pending");
        drop(store);

        let report = affected_tests(&root, &["src/caller.rs".into()], None, None);
        assert!(
            report.fail_closed,
            "a stale map must not produce a trusted affected-tests list"
        );
        let reason = report
            .fail_closed_reason
            .expect("stale map states why it failed closed");
        assert!(
            reason.contains("stale") && reason.contains("pending"),
            "reason must name staleness, got {reason}"
        );
        assert!(
            report.tests.items.is_empty(),
            "must not return test paths from a stale map"
        );
    }
}
