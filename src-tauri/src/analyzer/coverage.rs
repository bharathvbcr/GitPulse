//! Language-aware test-coverage scanning.
//!
//! GitPulse does not run tests. It finds coverage artifacts the opened
//! repository's languages actually produce, parses them, and folds hit maps
//! into one report. A Rust tree is not searched for JaCoCo; a Python tree is
//! not searched for `coverage.out`.

use crate::analyzer::language::LanguageDetector;
use crate::coverage_toolchain::{
    managed_venv_python, pytest_install_arguments, DEFAULT_JS_COVERAGE_PROVIDER,
    JS_COVERAGE_PROVIDERS, MANAGED_VENV_DIR, VENV_PYTHON_RELPATHS,
};
use crate::engine::git_cli::{
    capture_command, git_text_partial, sandbox_join, sandbox_join_canonical, validate_repo,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::time::Duration;

type HitMap = BTreeMap<usize, u64>;
type FileHitMaps = HashMap<String, HitMap>;

const MAX_LINE_NO: usize = 2_000_000;
const MAX_DIR_ENTRIES: usize = 64;
/// Cap on how many `ls-files` entries family detection classifies. Giant
/// monorepos can hold millions of paths; the prefix is plenty to seed
/// families and cargo dirs, so beyond it we stop splitting and flag the
/// report truncated instead of burning seconds of CPU per scan.
const LISTING_ENTRY_CAP: usize = 500_000;
const DEFAULT_MAX_ARTIFACT_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_MAX_ARTIFACTS: usize = 48;
const DEFAULT_MAX_FILES: usize = 4_000;
const DEFAULT_MAX_TOTAL_ENTRIES: usize = 4_000_000;
/// Per-file cap on `FileCoverage.lines` returned over IPC. A detail payload of
/// 100k lines is already far beyond what a gutter render can use; without it a
/// hostile merge (many artifacts stacking one file) ships megabytes of JSON.
const MAX_DETAIL_LINES: usize = 100_000;
/// Repos held in the process-wide scan cache. Bounded so hopping across many
/// repos in one session cannot accumulate unbounded hit maps in memory.
const CACHE_MAX_REPOS: usize = 8;
/// Large scans are returned but not retained. A 4M-entry BTreeMap can consume
/// hundreds of MiB; caching eight such maps would turn ordinary repo switching
/// into process-wide memory exhaustion.
const CACHE_MAX_ENTRIES_PER_REPO: usize = 250_000;

/// Hard ceiling on hit-map entries recorded across one scan.
///
/// Every parser funnels inserts through [`record_hit`], which debits this
/// budget only for *new* line keys (repeat hits are free). Once spent, further
/// lines are dropped and the report is flagged `truncated`, so hostile or
/// pathological artifacts bound memory instead of exhausting it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanLimits {
    pub max_artifact_bytes: u64,
    pub max_artifacts: usize,
    pub max_files: usize,
    pub max_total_entries: usize,
}

impl Default for ScanLimits {
    fn default() -> Self {
        Self {
            max_artifact_bytes: DEFAULT_MAX_ARTIFACT_BYTES,
            max_artifacts: DEFAULT_MAX_ARTIFACTS,
            max_files: DEFAULT_MAX_FILES,
            max_total_entries: DEFAULT_MAX_TOTAL_ENTRIES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EntryBudget {
    remaining: usize,
    /// True only when a new line was actually refused. Reaching zero exactly
    /// is complete; treating an exact fit as truncation makes capped reports
    /// lie about work they did finish.
    dropped: bool,
}

impl EntryBudget {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            remaining: capacity,
            dropped: false,
        }
    }

    fn spend(&mut self) -> bool {
        if self.remaining == 0 {
            self.dropped = true;
            return false;
        }
        self.remaining -= 1;
        true
    }

    fn mark_dropped(&mut self) {
        self.dropped = true;
    }

    fn dropped(&self) -> bool {
        self.dropped
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageFormat {
    Lcov,
    Cobertura,
    GoCover,
    Istanbul,
    Jacoco,
    Clover,
    CoveragePyDb,
}

impl CoverageFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lcov => "lcov",
            Self::Cobertura => "cobertura",
            Self::GoCover => "go_cover",
            Self::Istanbul => "istanbul",
            Self::Jacoco => "jacoco",
            Self::Clover => "clover",
            Self::CoveragePyDb => "coverage_py_db",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct CoverageTotals {
    pub lines_found: usize,
    pub lines_hit: usize,
    pub percentage: f64,
}

impl CoverageTotals {
    fn from_counts(found: usize, hit: usize) -> Self {
        let percentage = if found == 0 {
            0.0
        } else {
            ((hit as f64 / found as f64) * 1000.0).round() / 10.0
        };
        Self {
            lines_found: found,
            lines_hit: hit,
            percentage,
        }
    }

    fn from_map(lines: &BTreeMap<usize, u64>) -> Self {
        let found = lines.len();
        let hit = lines.values().filter(|h| **h > 0).count();
        Self::from_counts(found, hit)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoverageFamilyStatus {
    pub family: String,
    pub languages: Vec<String>,
    pub color_hex: String,
    pub expected_formats: Vec<String>,
    pub expected_paths: Vec<String>,
    pub found: bool,
    /// Repo-aware commands that produce artifacts this family can parse.
    /// Empty when GitPulse cannot plan a no-shell command for the layout.
    /// Planned even when `found` is true so a stale report can be regenerated.
    pub suggested_commands: Vec<String>,
    /// Tool-install steps MANVI must run before `suggested_commands` when
    /// `tool_ready` is false (e.g. `cargo install cargo-llvm-cov --locked`).
    pub setup_commands: Vec<String>,
    /// True when a version probe confirmed the generator toolchain. False
    /// means setup is required; it is never reported as a successful generate.
    pub tool_ready: bool,
    /// Why `tool_ready` is false, or empty when the toolchain answered.
    pub tool_detail: String,
    /// Honest bound for the UI so a multi-minute generate is not a hang.
    pub duration_hint: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoverageArtifact {
    pub path: String,
    pub format: String,
    pub family: String,
    pub skipped: bool,
    pub skip_reason: Option<String>,
    pub totals: CoverageTotals,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileCoverageSummary {
    pub path: String,
    pub language: String,
    pub color_hex: String,
    pub lines_found: usize,
    pub lines_hit: usize,
    pub percentage: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoveredLine {
    pub line_no: usize,
    pub hits: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileCoverage {
    pub path: String,
    pub language: String,
    pub color_hex: String,
    pub lines: Vec<CoveredLine>,
    pub totals: CoverageTotals,
    /// True when the scan that produced this data hit a cap (artifact count,
    /// entry budget, file count). The file may be missing lines it actually
    /// has — an empty result is then "unknown", not "uncovered".
    pub truncated: bool,
    /// True when `lines` alone was capped at [`MAX_DETAIL_LINES`]; `totals`
    /// still reflect every recorded line.
    pub lines_truncated: bool,
}

/// Exact retained/observed counts for one safety budget that fired during a
/// scan.
///
/// `truncated` alone says only "something was cut". A renderer holding just
/// that has to headline the counts it *kept* — 30 of 141 — which reads as
/// complete coverage of a 141-file repo even when 12,873 files were seen.
/// Mirrors `analyzer::deps::ScanLimitNotice`, whose report already carries
/// these numbers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageScanLimit {
    pub resource: String,
    pub kept: usize,
    pub total: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoverageReport {
    pub families: Vec<CoverageFamilyStatus>,
    /// Per-language rollup of the merged file data (the language split).
    pub languages: Vec<CoverageLanguageSplit>,
    pub artifacts: Vec<CoverageArtifact>,
    pub files: Vec<FileCoverageSummary>,
    pub overall: CoverageTotals,
    pub truncated: bool,
    /// Exact retained/observed counts for every cap that fired. A subset of
    /// what `truncated` covers: budget exhaustion and partial directory
    /// listings have no honest total to report, so they raise the flag alone.
    /// `serde(default)` keeps older cached reports loadable.
    #[serde(default)]
    pub limit_notices: Vec<CoverageScanLimit>,
    /// Go module directories found without the git listing.
    ///
    /// Populated only when the listing found none, which is exactly when the
    /// planner falls back to a root-level `go test ./...` that can answer
    /// "directory prefix . does not contain main module". The frontend uses
    /// this to retry per module after that failure; it is empty for every
    /// repository whose modules the listing already found.
    #[serde(default)]
    pub go_modules: Vec<String>,
    /// Whether a bound cut the module search short. A partial module list
    /// means partial coverage, so it is never left implicit.
    #[serde(default)]
    pub go_modules_partial: bool,
}

/// Aggregated coverage for one detected language across every artifact that
/// mentions it. Lets the UI present "Rust 82% · TypeScript 91%" instead of a
/// single blended number dominated by whichever language has more lines.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoverageLanguageSplit {
    pub language: String,
    pub color_hex: String,
    pub files: usize,
    pub lines_found: usize,
    pub lines_hit: usize,
    pub percentage: f64,
}

// Test instrumentation: how many artifact parses this thread's scan pipeline
// ran. Thread-local because the suite runs coverage tests in parallel — a
// process-global counter gets incremented by every concurrent test's parses
// and makes cache-hit assertions nondeterministically fail.
#[cfg(test)]
thread_local! {
    pub(crate) static SCAN_PARSE_COUNT: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
static CACHE_SEQUENCE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Test-only: freezes FIFO eviction while a cache-count assertion window is
/// open. Parallel tests still insert entries, but cannot evict ours mid-test.
#[cfg(test)]
static EVICTION_FROZEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
struct FreezeEvictionGuard;
#[cfg(test)]
impl FreezeEvictionGuard {
    fn new() -> Self {
        EVICTION_FROZEN.store(true, std::sync::atomic::Ordering::SeqCst);
        Self
    }
}
#[cfg(test)]
impl Drop for FreezeEvictionGuard {
    fn drop(&mut self) {
        EVICTION_FROZEN.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Cache key: canonical repo path plus a tag of the limits used. Two callers
/// scanning the same repo with different caps must never share an entry — the
/// caps shape the report (truncation, survivor set), not just its size.
#[derive(Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    repo: std::path::PathBuf,
    limits_tag: u64,
}

struct CoverageCacheEntry {
    fingerprint: Vec<(String, u64, u64, u64)>,
    report: std::sync::Arc<CoverageReport>,
    hit_maps: std::sync::Arc<FileHitMaps>,
}

static COVERAGE_CACHE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<CacheKey, (CoverageCacheEntry, u64)>>,
> = std::sync::OnceLock::new();

/// Monotonic insertion counter for FIFO eviction order.
static CACHE_TICK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn limits_tag(limits: &ScanLimits) -> u64 {
    // Stable across processes for identical limit values; only used to
    // distinguish limit sets from each other, never persisted.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in [
        limits.max_artifact_bytes,
        limits.max_artifacts as u64,
        limits.max_files as u64,
        limits.max_total_entries as u64,
    ]
    .iter()
    .flat_map(|v| v.to_le_bytes())
    {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

/// Complete bounded-file digest mixed into the fingerprint. mtime granularity
/// is filesystem-dependent (1s on HFS+/ext3/network mounts), so an equal-size
/// rewrite inside one tick can otherwise serve stale parses. Sampling a prefix
/// is insufficient: coverage generators commonly rewrite counters near the end
/// while headers and file lists remain identical.
///
/// Only regular files at or below the parser's byte limit are read, and the
/// canonical target must stay inside the repository. That prevents a FIFO from
/// blocking and prevents a symlink candidate from fingerprinting outside data.
fn content_probe(repo: &Path, path: &Path, max_bytes: u64) -> u64 {
    use std::io::Read;
    let Ok(canon) = path.canonicalize() else {
        return 0;
    };
    if !canon.starts_with(repo) {
        return 0;
    }
    let Ok(meta) = std::fs::metadata(&canon) else {
        return 0;
    };
    if !meta.is_file() || meta.len() > max_bytes {
        return 0;
    }
    let Ok(mut file) = std::fs::File::open(canon) else {
        return 0;
    };
    let mut buf = [0u8; 64 * 1024];
    let mut hash: u64 = 0x8422_2325_cbf2_9ce4;
    loop {
        let Ok(read) = file.read(&mut buf) else {
            return 0;
        };
        if read == 0 {
            break;
        }
        for byte in &buf[..read] {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    }
    hash
}

/// ASCII-case-insensitive map lookup fallback for `file_coverage`.
fn lookup_folded(maps: &FileHitMaps, rel: &str) -> Option<HitMap> {
    maps.iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(rel))
        .map(|(_, value)| value.clone())
}

pub struct CoverageScanner;

impl CoverageScanner {
    pub fn scan(repo_path: &str) -> Result<CoverageReport, String> {
        Self::scan_with_limits(repo_path, ScanLimits::default()).map(|(report, _)| report)
    }

    pub fn file_coverage(repo_path: &str, file_path: &str) -> Result<FileCoverage, String> {
        let repo = validate_repo(repo_path)?;
        let joined = sandbox_join_canonical(&repo, file_path)?;
        let rel = joined
            .strip_prefix(&repo)
            .map_err(|_| "File path escapes the repository".to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        let rel = LanguageDetector::normalize_rel_path(&rel);
        if rel.is_empty() {
            return Err("Invalid file path".into());
        }
        let (report, maps) = Self::scan_with_limits(repo_path, ScanLimits::default())?;
        // Exact key first. On case-insensitive filesystems (APFS default,
        // NTFS) a user-facing path can differ in case from the artifact's
        // recorded path; fall back to an ASCII-case-insensitive match so the
        // detail view doesn't report "no coverage" for covered files.
        let lines_map = maps
            .get(&rel)
            .cloned()
            .or_else(|| lookup_folded(&maps, &rel))
            .unwrap_or_default();
        let lang = LanguageDetector::detect_from_path(&rel);
        let totals = CoverageTotals::from_map(&lines_map);
        let mut lines: Vec<CoveredLine> = lines_map
            .into_iter()
            .map(|(line_no, hits)| CoveredLine { line_no, hits })
            .collect();
        let lines_truncated = lines.len() > MAX_DETAIL_LINES;
        if lines_truncated {
            lines.truncate(MAX_DETAIL_LINES);
        }
        Ok(FileCoverage {
            path: rel,
            language: lang.name.to_string(),
            color_hex: lang.color_hex.to_string(),
            lines,
            totals,
            // A truncated scan's merged map is a partial sample: absence of
            // this file means "unknown", not "uncovered", so flag every
            // detail served from such a scan.
            truncated: report.truncated,
            lines_truncated,
        })
    }

    pub fn scan_with_limits(
        repo_path: &str,
        limits: ScanLimits,
    ) -> Result<(CoverageReport, FileHitMaps), String> {
        let repo = validate_repo(repo_path)?;
        let mut detected = detect_families(&repo)?;
        let mut families = std::mem::take(&mut detected.families);
        if families.is_empty() {
            return Ok((
                CoverageReport {
                    families: Vec::new(),
                    languages: Vec::new(),
                    artifacts: Vec::new(),
                    files: Vec::new(),
                    overall: CoverageTotals::default(),
                    // An empty report from a cut listing is "unknown", not a
                    // clean "nothing to scan".
                    truncated: detected.listing_partial,
                    limit_notices: Vec::new(),
                    go_modules: Vec::new(),
                    go_modules_partial: false,
                },
                HashMap::new(),
            ));
        }

        let mut truncated = detected.listing_partial;
        let mut limit_notices: Vec<CoverageScanLimit> = Vec::new();
        let candidates = {
            let mut c = collect_candidates(&families, &detected.cargo_dirs, &detected.go_mod_dirs);
            if extend_directory_candidates(&repo, &families, &detected.cargo_dirs, &mut c) {
                // A probed directory dropped an artifact-shaped entry beyond
                // the MAX_DIR_ENTRIES window; readdir order is arbitrary, so
                // real artifacts may have been left unseen.
                truncated = true;
            }
            c
        };

        // Stat once up front and consider existing artifacts first: static
        // spec paths that don't exist must not burn the `max_artifacts` cap
        // and starve families whose real artifacts sit later in the list.
        let mut present: Vec<Candidate> = Vec::new();
        let mut absent: Vec<Candidate> = Vec::new();
        for cand in candidates {
            if repo.join(&cand.rel).metadata().is_ok() {
                present.push(cand);
            } else {
                absent.push(cand);
            }
        }

        let mut fingerprint = Vec::new();
        for cand in &present {
            if let Ok(meta) = repo.join(&cand.rel).metadata() {
                // Nanosecond precision where the FS provides it; coarse
                // filesystems are covered by the content probe below.
                let mtime = meta
                    .modified()
                    .map(|t| {
                        t.duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_nanos() as u64
                    })
                    .unwrap_or(0);
                // Probe regular files only — opening a FIFO would block.
                let probe = if meta.is_file() {
                    content_probe(&repo, &repo.join(&cand.rel), limits.max_artifact_bytes)
                } else {
                    0
                };
                fingerprint.push((cand.rel.clone(), meta.len(), mtime, probe));
            }
        }

        let key = CacheKey {
            repo: repo.clone(),
            limits_tag: limits_tag(&limits),
        };
        let cache = COVERAGE_CACHE.get_or_init(|| std::sync::Mutex::new(Default::default()));
        // Scope the lookup so the lock is released before the scan body runs:
        // this same mutex is re-acquired to publish the result, and holding a
        // std Mutex across that path deadlocks against itself.
        let cache_hit = {
            let guard = cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match guard.get(&key) {
                Some((entry, _tick))
                    if entry.fingerprint == fingerprint && !fingerprint.is_empty() =>
                {
                    Some((
                        entry.report.as_ref().clone(),
                        entry.hit_maps.as_ref().clone(),
                    ))
                }
                _ => None,
            }
        };
        if let Some((mut report, maps)) = cache_hit {
            // Suggestions read live manifests (package.json, Cargo.toml), not
            // the artifact fingerprint, so a cache hit must still refresh them.
            fill_suggested_commands(
                &repo,
                &mut report.families,
                CoverageCommandLayout::from_scan(&detected),
            );
            return Ok((report, maps));
        }

        let mut artifacts = Vec::new();
        let mut merged: FileHitMaps = HashMap::new();
        let mut budget = EntryBudget::new(limits.max_total_entries);
        // Set by parse_go_cover when a file's expansion allowance dropped
        // ranges or range tails; OR-ed into `truncated` below so totals over
        // reduced go-cover data don't read as authoritative.
        let mut go_expansion_capped = false;

        let present_artifacts = present.len();
        for (considered, cand) in present.into_iter().enumerate() {
            if considered >= limits.max_artifacts {
                truncated = true;
                limit_notices.push(CoverageScanLimit {
                    resource: "coverage artifacts".into(),
                    kept: limits.max_artifacts,
                    total: present_artifacts,
                });
                break;
            }
            match read_artifact(&repo, &cand.rel, limits.max_artifact_bytes) {
                ArtifactRead::Missing => continue,
                ArtifactRead::Skipped { reason } => {
                    artifacts.push(CoverageArtifact {
                        path: cand.rel,
                        format: cand.format.as_str().to_string(),
                        family: cand.family,
                        skipped: true,
                        skip_reason: Some(reason),
                        totals: CoverageTotals::default(),
                    });
                }
                ArtifactRead::Text(text) => {
                    if cand.format == CoverageFormat::CoveragePyDb {
                        artifacts.push(CoverageArtifact {
                            path: cand.rel,
                            format: cand.format.as_str().to_string(),
                            family: cand.family,
                            skipped: true,
                            skip_reason: Some(
                                "binary .coverage database; export coverage.xml or lcov.info"
                                    .into(),
                            ),
                            totals: CoverageTotals::default(),
                        });
                        continue;
                    }
                    #[cfg(test)]
                    SCAN_PARSE_COUNT.with(|c| c.set(c.get() + 1));
                    // Parsing builds an artifact-local staging map; merging it
                    // into the report is where the scan-wide budget is spent.
                    // Reusing the global budget for both phases charged every
                    // unique line twice and could drop an exact-fit artifact.
                    let mut artifact_budget = EntryBudget::new(limits.max_total_entries);
                    match parse_artifact(
                        cand.format,
                        &text,
                        &repo,
                        &mut artifact_budget,
                        &mut go_expansion_capped,
                    ) {
                        Ok(map) => {
                            if artifact_budget.dropped() {
                                truncated = true;
                            }
                            // A parse that yields no records is not "0%
                            // covered" data — it's an artifact we understood
                            // but found nothing usable in (empty file,
                            // totals-only JSON, foreign schema). Say so.
                            if map.is_empty() {
                                artifacts.push(CoverageArtifact {
                                    path: cand.rel,
                                    format: cand.format.as_str().to_string(),
                                    family: cand.family,
                                    skipped: true,
                                    skip_reason: Some(
                                        "parsed but contained no coverage records".into(),
                                    ),
                                    totals: CoverageTotals::default(),
                                });
                                continue;
                            }
                            // Totals describe what the artifact contained at
                            // parse time; budget-driven drops during merge are
                            // reported via `truncated` instead of rewriting
                            // per-artifact rows retroactively.
                            let totals = totals_of(&map);
                            let exhausted = merge_into(&mut merged, map, &mut budget);
                            artifacts.push(CoverageArtifact {
                                path: cand.rel,
                                format: cand.format.as_str().to_string(),
                                family: cand.family,
                                skipped: false,
                                skip_reason: None,
                                totals,
                            });
                            if exhausted {
                                truncated = true;
                                break;
                            }
                        }
                        Err(reason) => {
                            artifacts.push(CoverageArtifact {
                                path: cand.rel,
                                format: cand.format.as_str().to_string(),
                                family: cand.family,
                                skipped: true,
                                skip_reason: Some(reason),
                                totals: CoverageTotals::default(),
                            });
                        }
                    }
                }
            }
        }
        // Candidates that did not exist at partition time were no-ops; they
        // never consumed the artifact cap above.

        if go_expansion_capped {
            truncated = true;
        }

        if merged.len() > limits.max_files {
            truncated = true;
            limit_notices.push(CoverageScanLimit {
                resource: "covered files".into(),
                kept: limits.max_files,
                total: merged.len(),
            });
            // Drop lowest-value files first: files with no hit lines carry no
            // information beyond presence, then alphabetical order keeps the
            // survivor set deterministic across runs (HashMap iteration order
            // is randomized per process).
            let mut ranked: Vec<(usize, String)> = merged
                .iter()
                .map(|(path, lines)| {
                    (
                        lines.values().filter(|hits| **hits > 0).count(),
                        path.clone(),
                    )
                })
                .collect();
            ranked.sort();
            let excess = merged.len() - limits.max_files;
            for (_, path) in ranked.into_iter().take(excess) {
                merged.remove(&path);
            }
        }

        let mut files: Vec<FileCoverageSummary> = merged
            .iter()
            .map(|(path, lines)| {
                let lang = LanguageDetector::detect_from_path(path);
                let totals = CoverageTotals::from_map(lines);
                FileCoverageSummary {
                    path: path.clone(),
                    language: lang.name.to_string(),
                    color_hex: lang.color_hex.to_string(),
                    lines_found: totals.lines_found,
                    lines_hit: totals.lines_hit,
                    percentage: totals.percentage,
                }
            })
            .collect();
        files.sort_by(|a, b| {
            a.percentage
                .partial_cmp(&b.percentage)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.path.cmp(&b.path))
        });

        // A family counts as "found" only when the merged data actually
        // contains files of that family's languages. Path-based marking is not
        // enough: `coverage/lcov.info` is claimed by both the javascript and
        // rust families, and a JS-only report must not present rust as covered.
        for status in families.values_mut() {
            status.found = files.iter().any(|f| {
                LanguageDetector::coverage_family(&f.language) == Some(status.family.as_str())
            });
        }

        let languages = language_split(&files);
        let overall = {
            let found = files.iter().map(|f| f.lines_found).sum();
            let hit = files.iter().map(|f| f.lines_hit).sum();
            CoverageTotals::from_counts(found, hit)
        };

        let mut family_list: Vec<CoverageFamilyStatus> = families.into_values().collect();
        family_list.sort_by(|a, b| a.family.cmp(&b.family));
        fill_suggested_commands(
            &repo,
            &mut family_list,
            CoverageCommandLayout::from_scan(&detected),
        );

        // Populated for exactly the repositories whose plan contains a
        // root-level `go test ./...`, which is the command that can answer
        // "directory prefix . does not contain main module":
        //
        //   - a root `go.work`, where that command is planned unconditionally
        //     because a workspace root normally covers every used module; and
        //   - no module directory in the listing, where it is the fallback.
        //
        // Keying this on an empty listing alone missed the first case
        // entirely: a workspace plans the root command however many modules
        // the listing found.
        let go_root_command_planned = detected.go_work_at_root || detected.go_mod_dirs.is_empty();
        let (go_modules, go_modules_partial) =
            if go_root_command_planned && family_list.iter().any(|f| f.family == "go") {
                discover_go_modules(&repo)
            } else {
                (Vec::new(), false)
            };

        let report = CoverageReport {
            families: family_list,
            languages,
            artifacts,
            files,
            overall,
            truncated,
            limit_notices,
            go_modules,
            go_modules_partial,
        };

        let merged_entries: usize = merged.values().map(BTreeMap::len).sum();
        if merged_entries > CACHE_MAX_ENTRIES_PER_REPO {
            return Ok((report, merged));
        }

        let mut guard = cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tick = CACHE_TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        guard.insert(
            key,
            (
                CoverageCacheEntry {
                    fingerprint,
                    report: std::sync::Arc::new(report.clone()),
                    hit_maps: std::sync::Arc::new(merged.clone()),
                },
                tick,
            ),
        );
        let mut evictions = 0usize;
        while guard.len() > CACHE_MAX_REPOS {
            #[cfg(test)]
            if EVICTION_FROZEN.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            let oldest = guard
                .iter()
                .min_by_key(|(_, (_, t))| *t)
                .map(|(k, _)| k.clone());
            match oldest {
                Some(k) => {
                    guard.remove(&k);
                    evictions += 1;
                }
                None => break,
            }
            if evictions > CACHE_MAX_REPOS * 2 {
                break; // paranoia: never loop forever
            }
        }

        Ok((report, merged))
    }
}

#[derive(Clone)]
struct Candidate {
    rel: String,
    format: CoverageFormat,
    family: String,
}

fn specs_for(family: &str) -> &'static [(&'static str, CoverageFormat)] {
    match family {
        "javascript" => &[
            ("coverage/lcov.info", CoverageFormat::Lcov),
            ("lcov.info", CoverageFormat::Lcov),
            ("coverage/coverage-final.json", CoverageFormat::Istanbul),
            ("coverage/cobertura-coverage.xml", CoverageFormat::Cobertura),
            ("coverage/clover.xml", CoverageFormat::Clover),
        ],
        "rust" => &[
            ("lcov.info", CoverageFormat::Lcov),
            ("coverage/lcov.info", CoverageFormat::Lcov),
            ("cobertura.xml", CoverageFormat::Cobertura),
            ("target/llvm-cov/lcov.info", CoverageFormat::Lcov),
            ("target/llvm-cov/cobertura.xml", CoverageFormat::Cobertura),
            ("target/llvm-cov-target/lcov.info", CoverageFormat::Lcov),
            ("target/tarpaulin/cobertura.xml", CoverageFormat::Cobertura),
            ("target/coverage/lcov.info", CoverageFormat::Lcov),
        ],
        "python" => &[
            ("coverage.xml", CoverageFormat::Cobertura),
            ("htmlcov/coverage.xml", CoverageFormat::Cobertura),
            ("coverage/coverage.xml", CoverageFormat::Cobertura),
            ("lcov.info", CoverageFormat::Lcov),
            ("coverage/lcov.info", CoverageFormat::Lcov),
            (".coverage", CoverageFormat::CoveragePyDb),
        ],
        "go" => &[
            ("coverage.out", CoverageFormat::GoCover),
            ("cover.out", CoverageFormat::GoCover),
            ("coverage.txt", CoverageFormat::GoCover),
        ],
        "jvm" => &[
            ("target/site/jacoco/jacoco.xml", CoverageFormat::Jacoco),
            (
                "build/reports/jacoco/test/jacocoTestReport.xml",
                CoverageFormat::Jacoco,
            ),
            ("build/reports/jacoco/jacoco.xml", CoverageFormat::Jacoco),
            ("cobertura.xml", CoverageFormat::Cobertura),
        ],
        "native" => &[
            ("lcov.info", CoverageFormat::Lcov),
            ("coverage.info", CoverageFormat::Lcov),
            ("coverage/lcov.info", CoverageFormat::Lcov),
            ("build/coverage.info", CoverageFormat::Lcov),
            ("build/lcov.info", CoverageFormat::Lcov),
        ],
        "swift" => &[
            ("cobertura.xml", CoverageFormat::Cobertura),
            ("coverage/cobertura.xml", CoverageFormat::Cobertura),
            ("coverage/lcov.info", CoverageFormat::Lcov),
            ("lcov.info", CoverageFormat::Lcov),
        ],
        "dotnet" => &[
            ("coverage.cobertura.xml", CoverageFormat::Cobertura),
            ("coverage.xml", CoverageFormat::Cobertura),
            (
                "TestResults/coverage.cobertura.xml",
                CoverageFormat::Cobertura,
            ),
        ],
        "php" => &[
            ("clover.xml", CoverageFormat::Clover),
            ("coverage.xml", CoverageFormat::Cobertura),
            ("build/logs/clover.xml", CoverageFormat::Clover),
        ],
        "ruby" => &[
            ("coverage/coverage.json", CoverageFormat::Istanbul),
            ("coverage/lcov.info", CoverageFormat::Lcov),
            ("coverage.xml", CoverageFormat::Cobertura),
        ],
        "dart" => &[
            ("coverage/lcov.info", CoverageFormat::Lcov),
            ("lcov.info", CoverageFormat::Lcov),
        ],
        _ => &[],
    }
}

fn extra_dirs_for(family: &str) -> &'static [&'static str] {
    match family {
        "javascript" => &["coverage"],
        "python" => &["coverage", "htmlcov"],
        "rust" => &[
            "coverage",
            "target/llvm-cov",
            "target/llvm-cov-target",
            "target/tarpaulin",
            "target/coverage",
        ],
        "jvm" => &[
            "target/site/jacoco",
            "build/reports/jacoco",
            "build/reports/jacoco/test",
        ],
        "native" => &["coverage", "build", "build/coverage"],
        "swift" => &["coverage", ".build"],
        "dotnet" => &["TestResults", "coverage"],
        "php" => &["coverage", "build/logs"],
        "ruby" => &["coverage"],
        "dart" => &["coverage"],
        _ => &[],
    }
}

/// Cap on cargo workspaces we emit a coverage command for. A monorepo with
/// dozens of `Cargo.toml` files must not drown the UI in buttons.
const MAX_RUST_COVERAGE_COMMANDS: usize = 4;
/// Cap on Go modules we emit a coverage command for. Matches the health
/// scanner's `MAX_GO_MODS` bound so a polyglot monorepo cannot drown the UI.
const MAX_GO_COVERAGE_COMMANDS: usize = 4;
/// Bound on `package.json` we will parse for a coverage script. A hostile
/// multi-megabyte manifest is skipped; we do not invent npx vitest/jest.
const MAX_PACKAGE_JSON_BYTES: u64 = 256 * 1024;

/// Characters the frontend tokenizer refuses (no-shell argv runner). A
/// planned command that contains one would silently no-op in the UI.
fn command_line_is_argv_safe(line: &str) -> bool {
    !line.bytes().any(|b| {
        matches!(
            b,
            b'|' | b'&' | b';' | b'<' | b'>' | b'(' | b')' | b'`' | b'$' | 0
        )
    })
}

fn rel_is_command_unsafe(rel: &str) -> bool {
    rel.bytes().any(|b| {
        matches!(
            b,
            b'|' | b'&'
                | b';'
                | b'<'
                | b'>'
                | b'('
                | b')'
                | b'`'
                | b'$'
                | b'\n'
                | b'\r'
                | b'"'
                | b'\''
                | 0
        )
    })
}

fn quote_rel(rel: &str) -> String {
    if rel.chars().any(char::is_whitespace) {
        format!("\"{rel}\"")
    } else {
        rel.to_string()
    }
}

/// Bound on the `go.work` we will parse. A hostile multi-megabyte manifest is
/// skipped rather than read; no module list is better than a wrong one.
const MAX_GO_WORK_BYTES: u64 = 64 * 1024;
/// Bound on discovered Go modules. Hitting it is disclosed, never silently
/// truncated — a partial module list means partial coverage.
const MAX_GO_MODULES: usize = 16;
/// Depth and breadth bounds for the `go.mod` fallback search. A repository
/// with a deep vendor tree must not turn a scan into a full-disk walk.
const MAX_GO_WALK_DEPTH: usize = 3;
const MAX_GO_WALK_DIRS: usize = 512;

/// Directories named by a `go.work` `use` directive.
///
/// Handles both spellings the tool accepts: a parenthesised block and one or
/// more single-line `use` lines. Comments are stripped, and a `use` naming
/// the workspace root itself comes back as `""` like every other module dir
/// in this file.
fn parse_go_work_use(text: &str) -> (Vec<String>, bool) {
    let mut dirs: Vec<String> = Vec::new();
    let mut partial = false;
    let mut in_block = false;
    for raw in text.lines() {
        let line = match raw.split_once("//") {
            Some((before, _)) => before,
            None => raw,
        }
        .trim();
        if line.is_empty() {
            continue;
        }
        if in_block {
            if line.starts_with(')') {
                in_block = false;
                continue;
            }
            partial |= push_go_work_dir(&mut dirs, line);
            continue;
        }
        let Some(rest) = line.strip_prefix("use") else {
            continue;
        };
        // `used` and `usefoo` are not `use`.
        if !rest.is_empty() && !rest.starts_with(|c: char| c.is_whitespace() || c == '(') {
            continue;
        }
        let rest = rest.trim();
        if let Some(inline) = rest.strip_prefix('(') {
            in_block = true;
            // `use (./a)` and `use ( ./a` both occur in hand-written files.
            let inline = inline.trim();
            if let Some(before) = inline.strip_suffix(')') {
                in_block = false;
                partial |= push_go_work_dir(&mut dirs, before.trim());
            } else {
                partial |= push_go_work_dir(&mut dirs, inline);
            }
            continue;
        }
        partial |= push_go_work_dir(&mut dirs, rest);
    }
    (dirs, partial)
}

/// Adds one valid, unique workspace directory and reports whether the module
/// cap prevented it from being retained.
fn push_go_work_dir(dirs: &mut Vec<String>, raw: &str) -> bool {
    let trimmed = raw.trim().trim_matches('"');
    if trimmed.is_empty() {
        return false;
    }
    let rel = trimmed
        .trim_start_matches("./")
        .trim_end_matches('/')
        .replace('\\', "/");
    // `use .` is the workspace root, which this file spells "".
    let rel = if rel == "." { String::new() } else { rel };
    if rel.starts_with('/') || rel.split('/').any(|seg| seg == "..") {
        return false;
    }
    if rel_is_command_unsafe(&rel) {
        return false;
    }
    if dirs.contains(&rel) {
        return false;
    }
    if dirs.len() >= MAX_GO_MODULES {
        return true;
    }
    dirs.push(rel);
    false
}

/// The workspace's module directories, or `None` when there is no root
/// `go.work`. A present file that cannot be read safely is an explicitly
/// partial result, not permission to substitute a filesystem guess for the
/// workspace's authoritative `use` set.
fn read_go_work_modules(repo: &Path) -> Option<(Vec<String>, bool)> {
    let raw_path = repo.join("go.work");
    match std::fs::symlink_metadata(&raw_path) {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some((Vec::new(), true)),
    }
    let path = match sandbox_join_canonical(repo, "go.work") {
        Ok(path) => path,
        Err(_) => return Some((Vec::new(), true)),
    };
    let meta = match std::fs::metadata(&path) {
        Ok(meta) => meta,
        Err(_) => return Some((Vec::new(), true)),
    };
    if !meta.is_file() || meta.len() > MAX_GO_WORK_BYTES {
        return Some((Vec::new(), true));
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(_) => return Some((Vec::new(), true)),
    };
    Some(parse_go_work_use(&text))
}

/// Bounded search for `go.mod` directories.
///
/// The scan's own module list comes from `git ls-files --exclude-standard`,
/// which cannot see a `go.mod` that is git-ignored and stops at
/// `LISTING_ENTRY_CAP` on a large checkout. This walks the tree instead, so
/// the two methods fail for different reasons. Returns the directories and
/// whether a bound cut the search short.
fn walk_for_go_mod(repo: &Path) -> (Vec<String>, bool) {
    let mut found: Vec<String> = Vec::new();
    let mut partial = false;
    let mut visited = 0usize;
    // (relative dir, depth). Breadth-first so shallow modules — the ones a
    // workspace actually uses — are found before deep vendored copies.
    let mut queue: std::collections::VecDeque<(String, usize)> =
        std::collections::VecDeque::from([(String::new(), 0usize)]);
    while let Some((rel, depth)) = queue.pop_front() {
        if visited >= MAX_GO_WALK_DIRS {
            partial = true;
            break;
        }
        visited += 1;
        let Ok(dir) = sandbox_join_canonical(repo, if rel.is_empty() { "." } else { &rel }) else {
            partial = true;
            continue;
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            partial = true;
            continue;
        };
        for (seen_entries, entry) in entries.enumerate() {
            if seen_entries >= MAX_DIR_ENTRIES {
                partial = true;
                break;
            }
            let Ok(entry) = entry else {
                partial = true;
                continue;
            };
            let name = entry.file_name().to_string_lossy().to_string();
            // `file_type` does not follow symlinks, so a link is never
            // descended into: no cycle, and no module reported twice under
            // both its real path and an alias. Escaping the repository is
            // separately impossible — `sandbox_join_canonical` resolves each
            // directory before it is read.
            let Ok(kind) = entry.file_type() else {
                partial = true;
                continue;
            };
            if kind.is_file() && name == "go.mod" {
                if found.len() >= MAX_GO_MODULES {
                    partial = true;
                } else if !rel_is_command_unsafe(&rel) && !found.contains(&rel) {
                    found.push(rel.clone());
                }
                continue;
            }
            if !kind.is_dir() {
                continue;
            }
            if name.starts_with('.')
                || matches!(name.as_str(), "node_modules" | "vendor" | "target")
            {
                continue;
            }
            if depth + 1 > MAX_GO_WALK_DEPTH {
                partial = true;
                continue;
            }
            let child = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            queue.push_back((child, depth + 1));
        }
    }
    found.sort();
    (found, partial)
}

/// Every directory in this repository that holds a Go module, found without
/// relying on the git listing.
///
/// The `go.work` `use` set is authoritative when there is one: it says which
/// modules the workspace actually builds, which a filesystem search cannot.
/// The search is the fallback for repositories with no workspace file.
fn discover_go_modules(repo: &Path) -> (Vec<String>, bool) {
    if let Some((workspace, partial)) = read_go_work_modules(repo) {
        // A `use` entry whose directory has no go.mod is stale; running it
        // would reproduce the very failure this is here to get past.
        let live: Vec<String> = workspace
            .into_iter()
            .filter(|dir| {
                let manifest = if dir.is_empty() {
                    "go.mod".to_string()
                } else {
                    format!("{dir}/go.mod")
                };
                manifest_is_file(repo, &manifest)
            })
            .collect();
        return (live, partial);
    }
    walk_for_go_mod(repo)
}

fn manifest_is_file(repo: &Path, rel: &str) -> bool {
    sandbox_join_canonical(repo, rel)
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.is_file())
        .unwrap_or(false)
}

fn read_root_package_json(repo: &Path) -> Option<serde_json::Value> {
    let path = sandbox_join_canonical(repo, "package.json").ok()?;
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_PACKAGE_JSON_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&text).ok()
}

fn package_has_dep(pkg: &serde_json::Value, name: &str) -> bool {
    ["dependencies", "devDependencies", "optionalDependencies"]
        .iter()
        .any(|key| pkg.get(*key).and_then(|deps| deps.get(name)).is_some())
}

fn package_script<'a>(pkg: &'a serde_json::Value, name: &str) -> Option<&'a str> {
    pkg.get("scripts")
        .and_then(|s| s.get(name))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
}

fn javascript_has_vitest_coverage_provider(pkg: &serde_json::Value) -> bool {
    JS_COVERAGE_PROVIDERS
        .iter()
        .any(|provider| package_has_dep(pkg, provider))
}

fn javascript_coverage_commands(repo: &Path) -> Vec<String> {
    let mut commands = Vec::new();
    let Some(pkg) = read_root_package_json(repo) else {
        return commands;
    };
    // Prefer a declared coverage script over a generic vitest/jest argv.
    // `coverage` is the historical name; `test:coverage` is the npm
    // convention ScholarLM and others actually ship. Do not pick every
    // script whose name contains "coverage" (`test:rust:coverage` is not
    // a JavaScript generator). Never invent `npx --no-install vitest|jest`
    // when the checkout has no local runner — npx then fails with
    // "missing packages and no YES option" or a missing coverage provider.
    if package_script(&pkg, "coverage").is_some() {
        commands.push("npm run coverage".into());
    } else if package_script(&pkg, "test:coverage").is_some() {
        commands.push("npm run test:coverage".into());
    } else {
        if package_has_dep(&pkg, "vitest") && javascript_has_vitest_coverage_provider(&pkg) {
            commands.push("npx --no-install vitest run --coverage".into());
        }
        if package_has_dep(&pkg, "jest") || package_has_dep(&pkg, "@jest/core") {
            commands.push("npx --no-install jest --coverage".into());
        }
    }
    commands
}

fn javascript_unready_detail(repo: &Path) -> String {
    match read_root_package_json(repo) {
        None => "No package.json coverage script or local vitest/jest runner.".into(),
        Some(pkg)
            if package_has_dep(&pkg, "vitest") && !javascript_has_vitest_coverage_provider(&pkg) =>
        {
            "Vitest is present but no coverage provider (@vitest/coverage-v8 or @vitest/coverage-istanbul) is declared.".into()
        }
        Some(_) => {
            "No coverage script or local vitest/jest coverage runner in package.json.".into()
        }
    }
}

fn rust_coverage_commands(repo: &Path, cargo_dirs: &[String]) -> Vec<String> {
    let mut dirs: Vec<String> = cargo_dirs
        .iter()
        .filter(|d| !rel_is_command_unsafe(d))
        .cloned()
        .collect();
    if dirs.is_empty() {
        // Listing missed every Cargo.toml (ignored, or only `.rs` files).
        // Same two-path fallback CI:local uses for Tauri checkouts.
        if manifest_is_file(repo, "Cargo.toml") {
            dirs.push(String::new());
        } else if manifest_is_file(repo, "src-tauri/Cargo.toml") {
            dirs.push("src-tauri".into());
        }
    }
    let mut commands = Vec::new();
    for dir in dirs.into_iter().take(MAX_RUST_COVERAGE_COMMANDS) {
        let command = if dir.is_empty() {
            "cargo llvm-cov --workspace --lcov --output-path lcov.info".to_string()
        } else {
            let manifest = quote_rel(&format!("{dir}/Cargo.toml"));
            let output = quote_rel(&format!("{dir}/lcov.info"));
            format!(
                "cargo llvm-cov --manifest-path {manifest} --workspace --lcov --output-path {output}"
            )
        };
        if command_line_is_argv_safe(&command) {
            commands.push(command);
        }
    }
    if commands.is_empty() {
        commands.push("cargo llvm-cov --workspace --lcov --output-path lcov.info".into());
    }
    commands
}

/// Repo-relative interpreter paths a project virtualenv puts its Python at.
/// Unix and Windows layouts differ; both are probed so a checkout made on one
/// platform is still recognized on the other.
/// The project's existing virtualenv interpreter, if one is checked out.
///
/// Presence is not enough: the interpreter must also pass the same trust rule
/// the command gate applies before executing it, or planning a command around
/// it would produce a step that is guaranteed to be refused.
fn existing_venv_python(repo: &Path) -> Result<Option<&'static str>, &'static str> {
    let mut rejection = None;
    for rel in VENV_PYTHON_RELPATHS.iter().copied() {
        // Lexical existence check: a virtualenv interpreter is a symlink out
        // to the host toolchain by construction, so canonical containment
        // (`manifest_is_file`) would report every real virtualenv as absent.
        if !repo.join(rel).is_file() {
            continue;
        }
        match crate::terminal::check_venv_interpreter(repo, rel) {
            Ok(()) => return Ok(Some(rel)),
            Err(reason) => rejection = Some(reason),
        }
    }
    match rejection {
        Some(reason) => Err(reason),
        None => Ok(None),
    }
}

/// True when the virtualenv actually has pytest installed.
///
/// Decided from the filesystem, never by running the interpreter. Probing with
/// `python -m pytest --version` cost 0.1s from a shell and hung *indefinitely*
/// in the installed app: a Finder-launched bundle executing an unsigned
/// interpreter reached through a symlink chain into the host toolchain never
/// returned, and `TOOL_PROBE_TIMEOUT` did not bound it, so the whole coverage
/// scan wedged on any repository that had a virtualenv. A scan must never be
/// able to hang on an optional readiness check; `pip install pytest` writes
/// the console script next to the interpreter, so its presence answers the
/// same question for free.
fn interpreter_has_pytest(repo: &Path, python_rel: &str) -> bool {
    let Some((bin_dir, _)) = python_rel.rsplit_once('/') else {
        return false;
    };
    let candidates = if cfg!(windows) {
        ["pytest.exe", "pytest"]
    } else {
        ["pytest", "pytest.exe"]
    };
    candidates.iter().any(|name| {
        sandbox_join(repo, &format!("{bin_dir}/{name}"))
            .map(|path| path.is_file())
            .unwrap_or(false)
    })
}

/// The interpreter GitPulse creates virtualenvs with. `python3` first: on
/// macOS and most Linux distributions a bare `python` is either absent or
/// Python 2.
fn host_python_program(repo: &Path) -> Option<&'static str> {
    let _ = repo;
    ["python3", "python"]
        .into_iter()
        .find(|program| program_on_path(program))
}

fn pytest_generate_command(python_rel: Option<&str>) -> String {
    match python_rel {
        Some(python) => format!("{python} -m pytest --cov --cov-report=xml"),
        None => "pytest --cov --cov-report=xml".to_string(),
    }
}

fn go_test_cover_command(dir: &str) -> Option<String> {
    let command = if dir.is_empty() {
        "go test ./... -coverprofile=coverage.out".to_string()
    } else {
        let module = quote_rel(dir);
        format!("go -C {module} test ./... -coverprofile=coverage.out")
    };
    command_line_is_argv_safe(&command).then_some(command)
}

fn go_coverage_commands(repo: &Path, go_mod_dirs: &[String], go_work_at_root: bool) -> Vec<String> {
    // A root go.work is a workspace: `go test ./...` from the repo root
    // covers every `use`d module. Nested `-C` commands would duplicate work
    // and write coverprofiles into each module instead of the workspace root.
    if go_work_at_root {
        return vec!["go test ./... -coverprofile=coverage.out".into()];
    }
    let mut dirs: Vec<String> = go_mod_dirs
        .iter()
        .filter(|d| !rel_is_command_unsafe(d))
        .cloned()
        .collect();
    if dirs.is_empty() {
        // Listing missed every go.mod (ignored). A root go.work or go.mod
        // still lets `./...` resolve. Do not invent `go test ./...` at a
        // checkout that has only `.go` files — that is the
        // "directory prefix . does not contain main module" failure.
        if manifest_is_file(repo, "go.work") || manifest_is_file(repo, "go.mod") {
            dirs.push(String::new());
        }
    }
    let mut commands = Vec::new();
    for dir in dirs.into_iter().take(MAX_GO_COVERAGE_COMMANDS) {
        if let Some(command) = go_test_cover_command(&dir) {
            commands.push(command);
        }
    }
    commands
}

fn jvm_coverage_commands(repo: &Path, mvn_ready: bool) -> Vec<String> {
    let mut commands = Vec::new();
    if manifest_is_file(repo, "gradlew") || manifest_is_file(repo, "gradlew.bat") {
        commands.push("./gradlew test jacocoTestReport".into());
    }
    if manifest_is_file(repo, "pom.xml") {
        // A checked-in Maven wrapper is the project's own Maven. Requiring a
        // system `mvn` made GitPulse report "mvn is not installed" for repos
        // that ship `mvnw` precisely so nobody needs one.
        if manifest_is_file(repo, "mvnw") || manifest_is_file(repo, "mvnw.cmd") {
            commands.push("./mvnw verify".into());
        } else if mvn_ready {
            commands.push("mvn verify".into());
        }
    }
    commands
}

const TOOL_PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// Version probe for the cargo-llvm-cov subcommand. A missing binary, a
/// non-zero exit, or a timeout all mean "not ready" — never "generate
/// succeeded". The scan itself still succeeds; setup is planned instead.
fn cargo_llvm_cov_available() -> bool {
    match capture_command(
        "cargo",
        &["llvm-cov", "--version"],
        None,
        TOOL_PROBE_TIMEOUT,
        &[],
    ) {
        Ok(out) => out.success,
        Err(_) => false,
    }
}

/// True when `program` can be spawned. A non-zero `--version` still counts:
/// the binary exists. Spawn failure (os error 2) does not.
fn program_on_path(program: &str) -> bool {
    capture_command(program, &["--version"], None, TOOL_PROBE_TIMEOUT, &[]).is_ok()
        || capture_command(program, &["version"], None, TOOL_PROBE_TIMEOUT, &[]).is_ok()
}

fn family_present(families: &[CoverageFamilyStatus], name: &str) -> bool {
    families.iter().any(|status| status.family == name)
}

#[derive(Clone)]
struct LanguageCoveragePlan {
    generate: Vec<String>,
    setup: Vec<String>,
    tool_ready: bool,
    tool_detail: String,
    duration_hint: String,
}

impl LanguageCoveragePlan {
    fn ready(generate: Vec<String>, duration_hint: &str) -> Self {
        let duration_hint = if generate.is_empty() {
            String::new()
        } else {
            duration_hint.to_string()
        };
        Self {
            generate,
            setup: Vec::new(),
            tool_ready: true,
            tool_detail: String::new(),
            duration_hint,
        }
    }

    /// No generate command: the UI shows `tool_detail` and does not offer a
    /// Run button that would spawn a missing binary or fail the allowlist.
    fn unavailable(detail: &str) -> Self {
        Self {
            generate: Vec::new(),
            setup: Vec::new(),
            tool_ready: false,
            tool_detail: detail.to_string(),
            duration_hint: String::new(),
        }
    }
}

fn rust_coverage_plan(
    repo: &Path,
    cargo_dirs: &[String],
    llvm_cov_ready: bool,
) -> LanguageCoveragePlan {
    let generate = rust_coverage_commands(repo, cargo_dirs);
    if llvm_cov_ready {
        return LanguageCoveragePlan::ready(
            generate,
            "Generating Rust coverage can take several minutes.",
        );
    }
    LanguageCoveragePlan {
        generate,
        setup: vec![
            "rustup component add llvm-tools-preview".into(),
            "cargo install cargo-llvm-cov --locked".into(),
        ],
        tool_ready: false,
        tool_detail: "cargo-llvm-cov is not installed.".into(),
        duration_hint:
            "Installing cargo-llvm-cov and generating Rust coverage can take several minutes."
                .into(),
    }
}

/// The one JavaScript setup step that is both bounded and correct: a checkout
/// that already depends on vitest but declares no coverage provider. The
/// provider is a devDependency of this project, so installing it is a
/// repository change (package.json + lockfile), not a host change.
///
/// Deliberately narrow: a checkout with no test runner at all is not a missing
/// package, it is a missing test suite, and `npm install vitest` would leave
/// the user with a runner and nothing to run.
fn javascript_provider_setup(repo: &Path) -> Option<(String, String)> {
    let pkg = read_root_package_json(repo)?;
    if !package_has_dep(&pkg, "vitest") || javascript_has_vitest_coverage_provider(&pkg) {
        return None;
    }
    Some((
        format!("npm install --save-dev {DEFAULT_JS_COVERAGE_PROVIDER}"),
        format!(
            "Vitest is present but no coverage provider is declared. GitPulse will add {DEFAULT_JS_COVERAGE_PROVIDER} to devDependencies."
        ),
    ))
}

fn javascript_coverage_plan(repo: &Path) -> LanguageCoveragePlan {
    let generate = javascript_coverage_commands(repo);
    if !generate.is_empty() {
        return LanguageCoveragePlan::ready(
            generate,
            "Frontend coverage usually finishes in about a minute.",
        );
    }
    if let Some((setup, detail)) = javascript_provider_setup(repo) {
        return LanguageCoveragePlan {
            generate: vec!["npx --no-install vitest run --coverage".into()],
            setup: vec![setup],
            tool_ready: false,
            tool_detail: detail,
            duration_hint:
                "Installing the coverage provider and running the suite usually takes a minute or two."
                    .into(),
        };
    }
    LanguageCoveragePlan::unavailable(&javascript_unready_detail(repo))
}

/// Python coverage, including the setup needed to get there.
///
/// "pytest is not installed" used to end the story: no generate command, no
/// setup, nothing to press. But pytest is installable, so the honest answer is
/// a plan, not a dead end. Installation is deliberately confined to a
/// project-local virtualenv: the host interpreter may be externally managed
/// (PEP 668), where a plain `pip install` either fails or requires
/// `--break-system-packages`, and mutating the system Python to render a
/// coverage number is not a trade this app gets to make on the user's behalf.
///
/// An interpreter that already has pytest is used as-is — GitPulse does not
/// impose a virtualenv on a project whose toolchain already works.
fn python_coverage_plan(repo: &Path) -> LanguageCoveragePlan {
    let venv = match existing_venv_python(repo) {
        Ok(found) => found,
        Err(reason) => {
            // A virtualenv exists but GitPulse will not run its interpreter.
            // Say which rule it failed rather than silently falling back to
            // the host interpreter or planning a step the gate would refuse.
            return LanguageCoveragePlan::unavailable(&format!(
                "The project virtualenv {reason}, so GitPulse will not use it. Recreate it with `python3 -m venv .venv`, then rescan."
            ));
        }
    };
    if let Some(python) = venv {
        let generate = vec![pytest_generate_command(Some(python))];
        if interpreter_has_pytest(repo, python) {
            return LanguageCoveragePlan::ready(
                generate,
                "Python coverage can take a few minutes.",
            );
        }
        return LanguageCoveragePlan {
            generate,
            setup: vec![format!(
                "{python} -m pip install {}",
                pytest_install_arguments()
            )],
            tool_ready: false,
            tool_detail: format!("pytest is not installed in the project virtualenv ({python})."),
            duration_hint:
                "Installing pytest and generating Python coverage can take a few minutes.".into(),
        };
    }

    if program_on_path("pytest") {
        return LanguageCoveragePlan::ready(
            vec![pytest_generate_command(None)],
            "Python coverage can take a few minutes.",
        );
    }

    let Some(host_python) = host_python_program(repo) else {
        // Nothing to build a virtualenv with. Installing a Python runtime is a
        // host-level change with no bounded command; say so instead of
        // planning a step that cannot succeed.
        return LanguageCoveragePlan::unavailable(
            "No Python interpreter on PATH, so pytest cannot be installed. Install Python, then rescan.",
        );
    };

    let python = managed_venv_python();
    LanguageCoveragePlan {
        generate: vec![pytest_generate_command(Some(python))],
        setup: vec![
            format!("{host_python} -m venv {MANAGED_VENV_DIR}"),
            format!("{python} -m pip install {}", pytest_install_arguments()),
        ],
        tool_ready: false,
        tool_detail: format!(
            "pytest is not installed. GitPulse will create {MANAGED_VENV_DIR} in this repository and install pytest there; your system Python is not touched."
        ),
        duration_hint:
            "Creating the virtualenv, installing pytest and generating coverage can take a few minutes."
                .into(),
    }
}

fn go_coverage_plan(
    repo: &Path,
    go_mod_dirs: &[String],
    go_work_at_root: bool,
) -> LanguageCoveragePlan {
    let generate = go_coverage_commands(repo, go_mod_dirs, go_work_at_root);
    if generate.is_empty() {
        return LanguageCoveragePlan::unavailable("No go.mod or go.work in this repository.");
    }
    LanguageCoveragePlan::ready(generate, "Go coverage can take a few minutes.")
}

fn jvm_coverage_plan(repo: &Path, mvn_ready: bool) -> LanguageCoveragePlan {
    let generate = jvm_coverage_commands(repo, mvn_ready);
    if !generate.is_empty() {
        return LanguageCoveragePlan::ready(generate, "JVM coverage can take a few minutes.");
    }
    if manifest_is_file(repo, "pom.xml") && !mvn_ready {
        return LanguageCoveragePlan::unavailable(
            "mvn is not installed and this project ships no Maven wrapper (mvnw).",
        );
    }
    LanguageCoveragePlan::unavailable("No Gradle wrapper or pom.xml in the repository.")
}

/// C/C++ has no planned coverage generator, and says so.
///
/// This used to publish `ctest --output-on-failure` / `make test` as a ready
/// generator. Both were wrong, in two independent ways, and the row shipped
/// with a Run button that could not work:
///
/// 1. Neither command was ever on the coverage-generation allowlist, so the
///    gate refused it the instant the button was pressed. The planner-gate
///    contract test now catches that class outright.
/// 2. Even spawned, neither produces what this family looks for. GitPulse
///    expects `lcov` under `coverage/` or `build/`; running a test suite emits
///    coverage only if the binaries were *built* instrumented
///    (`-fprofile-arcs -ftest-coverage`) and a `gcov`/`lcov`/`gcovr` capture
///    step then collects it. Those flags live in the project's own CMake or
///    Make configuration, which GitPulse cannot edit or infer for an arbitrary
///    checkout, and inventing a capture command for a build that was never
///    instrumented would produce an empty report, not coverage.
///
/// So the honest answer is the same one `beam` gives: no generator, and the
/// reason. A row that explains itself is worth more than a button that lies.
fn native_coverage_plan(repo: &Path) -> LanguageCoveragePlan {
    let has_build = manifest_is_file(repo, "CMakeLists.txt")
        || manifest_is_file(repo, "Makefile")
        || manifest_is_file(repo, "GNUmakefile");
    if has_build {
        return LanguageCoveragePlan::unavailable(
            "C/C++ coverage needs an instrumented build (-fprofile-arcs -ftest-coverage) and a gcovr/lcov capture step, which GitPulse cannot add to this project's build files. Generate lcov into coverage/ yourself, then rescan.",
        );
    }
    LanguageCoveragePlan::unavailable(
        "No CMakeLists.txt or Makefile in this repository, so no C/C++ coverage build can be planned.",
    )
}

fn swift_coverage_plan(has_package_swift: bool, swift_ready: bool) -> LanguageCoveragePlan {
    if !has_package_swift {
        return LanguageCoveragePlan::unavailable("No Package.swift in this repository.");
    }
    if !swift_ready {
        return LanguageCoveragePlan::unavailable("swift is not installed.");
    }
    LanguageCoveragePlan::ready(
        vec!["swift test --enable-code-coverage".into()],
        "Swift test coverage usually finishes in a few minutes.",
    )
}

fn dotnet_coverage_plan(has_dotnet_proj: bool, dotnet_ready: bool) -> LanguageCoveragePlan {
    if !has_dotnet_proj {
        return LanguageCoveragePlan::unavailable("No .sln/.csproj/.fsproj in this repository.");
    }
    if !dotnet_ready {
        return LanguageCoveragePlan::unavailable("dotnet is not installed.");
    }
    LanguageCoveragePlan::ready(
        vec!["dotnet test --collect:\"XPlat Code Coverage\"".into()],
        ".NET test coverage usually finishes in a few minutes.",
    )
}

fn php_coverage_commands(repo: &Path) -> Vec<String> {
    let mut commands = Vec::new();
    if manifest_is_file(repo, "vendor/bin/phpunit") {
        commands.push("vendor/bin/phpunit --coverage-clover coverage.xml".into());
    }
    if manifest_is_file(repo, "composer.json") {
        commands.push("composer test".into());
    }
    commands
}

fn php_coverage_plan(repo: &Path) -> LanguageCoveragePlan {
    let generate = php_coverage_commands(repo);
    if generate.is_empty() {
        return LanguageCoveragePlan::unavailable(
            "No composer.json or vendor/bin/phpunit in this repository.",
        );
    }
    // A composer project whose vendor/ has never been installed has the
    // manifest but not the runner. `composer install` is a project-local
    // materialization of dependencies already pinned in composer.lock.
    let vendored = manifest_is_file(repo, "vendor/bin/phpunit");
    if !vendored && manifest_is_file(repo, "composer.json") {
        if !program_on_path("composer") {
            return LanguageCoveragePlan::unavailable(
                "composer is not installed, so PHP dependencies cannot be installed. Install Composer, then rescan.",
            );
        }
        return LanguageCoveragePlan {
            generate,
            setup: vec!["composer install".into()],
            tool_ready: false,
            tool_detail: "PHP dependencies are not installed (no vendor/bin/phpunit).".into(),
            duration_hint: "Installing dependencies and running the suite can take a few minutes."
                .into(),
        };
    }
    LanguageCoveragePlan::ready(
        generate,
        "PHP test coverage usually finishes in about a minute.",
    )
}

fn ruby_coverage_commands(repo: &Path) -> Vec<String> {
    let mut commands = Vec::new();
    if manifest_is_file(repo, "Gemfile") {
        commands.push("bundle exec rspec".into());
    }
    commands
}

fn ruby_coverage_plan(repo: &Path) -> LanguageCoveragePlan {
    let generate = ruby_coverage_commands(repo);
    if generate.is_empty() {
        return LanguageCoveragePlan::unavailable("No Gemfile in this repository.");
    }
    if !program_on_path("bundle") {
        return LanguageCoveragePlan::unavailable(
            "bundler is not installed, so Ruby dependencies cannot be installed. Install Bundler, then rescan.",
        );
    }
    // No Gemfile.lock means the bundle has never been resolved, so
    // `bundle exec` would fail before the suite ever starts.
    if !manifest_is_file(repo, "Gemfile.lock") {
        return LanguageCoveragePlan {
            generate,
            setup: vec!["bundle install".into()],
            tool_ready: false,
            tool_detail: "Ruby dependencies are not installed (no Gemfile.lock).".into(),
            duration_hint: "Installing gems and running the suite can take a few minutes.".into(),
        };
    }
    LanguageCoveragePlan::ready(
        generate,
        "Ruby test coverage usually finishes in about a minute.",
    )
}

fn dart_coverage_plan(has_pubspec: bool, dart_ready: bool) -> LanguageCoveragePlan {
    if !has_pubspec {
        return LanguageCoveragePlan::unavailable("No pubspec.yaml in this repository.");
    }
    if !dart_ready {
        return LanguageCoveragePlan::unavailable("dart is not installed.");
    }
    LanguageCoveragePlan::ready(
        vec!["dart test --coverage=coverage".into()],
        "Dart test coverage usually finishes in about a minute.",
    )
}

/// Applies a plan and enforces the family-row invariant.
///
/// `tool_ready == true` asserts "a generator for this family is available".
/// Every `ready(...)` constructor took a command list that could be empty
/// (`native` with no CMakeLists/Makefile, `php` with no composer.json, `ruby`
/// with no Gemfile, and the catch-all arm), so that assertion was published
/// without ever being checked: the row rendered as a bare label with no Run
/// button, no reason, and no duration — byte-identical to a family GitPulse
/// simply had no opinion about. Enforcing it here rather than at each call
/// site covers families added later too.
fn apply_language_plan(status: &mut CoverageFamilyStatus, plan: LanguageCoveragePlan) {
    status.suggested_commands = plan.generate;
    status.setup_commands = plan.setup;
    status.tool_ready = plan.tool_ready;
    status.tool_detail = plan.tool_detail;
    status.duration_hint = plan.duration_hint;
    if status.suggested_commands.is_empty() {
        status.tool_ready = false;
        if status.tool_detail.is_empty() {
            status.tool_detail =
                "GitPulse could not plan a coverage generator for this repository layout.".into();
        }
        // A duration for a command that does not exist is noise.
        status.duration_hint.clear();
    }
}

/// Manifests and listing flags the command planner needs besides `families`.
struct CoverageCommandLayout<'a> {
    cargo_dirs: &'a [String],
    go_mod_dirs: &'a [String],
    go_work_at_root: bool,
    has_package_swift: bool,
    has_pubspec: bool,
    has_dotnet_proj: bool,
}

impl<'a> CoverageCommandLayout<'a> {
    fn from_scan(detected: &'a FamilyScan) -> Self {
        CoverageCommandLayout {
            cargo_dirs: &detected.cargo_dirs,
            go_mod_dirs: &detected.go_mod_dirs,
            go_work_at_root: detected.go_work_at_root,
            has_package_swift: detected.has_package_swift,
            has_pubspec: detected.has_pubspec,
            has_dotnet_proj: detected.has_dotnet_proj,
        }
    }
}

/// Fills generate/setup commands from the checkout's actual manifests and a
/// live toolchain probe so the UI never offers `cargo llvm-cov` at a repo
/// root that has no Cargo.toml, `go test ./...` at a root with no go.mod,
/// `swift test` without Package.swift, or a generator whose binary is missing.
fn fill_suggested_commands(
    repo: &Path,
    families: &mut [CoverageFamilyStatus],
    layout: CoverageCommandLayout<'_>,
) {
    let rust_present = family_present(families, "rust");
    let llvm_cov_ready = if rust_present {
        cargo_llvm_cov_available()
    } else {
        true
    };
    let javascript = javascript_coverage_plan(repo);
    let rust = rust_coverage_plan(repo, layout.cargo_dirs, llvm_cov_ready);
    let python = if family_present(families, "python") {
        python_coverage_plan(repo)
    } else {
        LanguageCoveragePlan::ready(Vec::new(), "")
    };
    let go = go_coverage_plan(repo, layout.go_mod_dirs, layout.go_work_at_root);
    let jvm = if family_present(families, "jvm") {
        let mvn_needed = manifest_is_file(repo, "pom.xml");
        jvm_coverage_plan(repo, !mvn_needed || program_on_path("mvn"))
    } else {
        LanguageCoveragePlan::ready(Vec::new(), "")
    };
    let native = native_coverage_plan(repo);
    let swift = if family_present(families, "swift") {
        swift_coverage_plan(
            layout.has_package_swift,
            layout.has_package_swift && program_on_path("swift"),
        )
    } else {
        LanguageCoveragePlan::ready(Vec::new(), "")
    };
    let dotnet = if family_present(families, "dotnet") {
        dotnet_coverage_plan(
            layout.has_dotnet_proj,
            layout.has_dotnet_proj && program_on_path("dotnet"),
        )
    } else {
        LanguageCoveragePlan::ready(Vec::new(), "")
    };
    let php = php_coverage_plan(repo);
    let ruby = ruby_coverage_plan(repo);
    let dart = if family_present(families, "dart") {
        dart_coverage_plan(
            layout.has_pubspec,
            layout.has_pubspec && program_on_path("dart"),
        )
    } else {
        LanguageCoveragePlan::ready(Vec::new(), "")
    };
    for status in families.iter_mut() {
        let plan = match status.family.as_str() {
            "javascript" => javascript.clone(),
            "rust" => rust.clone(),
            "python" => python.clone(),
            "go" => go.clone(),
            "jvm" => jvm.clone(),
            "native" => native.clone(),
            "swift" => swift.clone(),
            "dotnet" => dotnet.clone(),
            "php" => php.clone(),
            "ruby" => ruby.clone(),
            "dart" => dart.clone(),
            // `beam` is detected (Elixir/Erlang sources seed it) but GitPulse
            // parses none of the formats the BEAM tooling emits, so there is
            // no artifact for a generate command to produce. Say that rather
            // than leaving a row the user cannot act on or explain.
            "beam" => LanguageCoveragePlan::unavailable(
                "GitPulse cannot parse Elixir/Erlang coverage output, so no generator is planned.",
            ),
            // Fail closed: an unrecognized family has had no readiness check
            // at all, and must not claim one.
            _ => LanguageCoveragePlan::unavailable(
                "GitPulse has no coverage generator for this ecosystem.",
            ),
        };
        apply_language_plan(status, plan);
    }
}

/// Output of [`detect_families`]: seeded families, cargo/Go module dirs, and
/// whether the `ls-files` listing was cut short (git's 64 MB cap or
/// [`LISTING_ENTRY_CAP`] entries).
struct FamilyScan {
    families: BTreeMap<String, CoverageFamilyStatus>,
    cargo_dirs: Vec<String>,
    go_mod_dirs: Vec<String>,
    go_work_at_root: bool,
    has_package_swift: bool,
    has_pubspec: bool,
    has_dotnet_proj: bool,
    listing_partial: bool,
}

/// Returns the detected coverage families, every directory that holds a
/// `Cargo.toml` or `go.mod` (repo root as `""`), whether a root `go.work`
/// is present, and whether the `ls-files` listing was incomplete.
///
/// The listing degrades to a prefix instead of failing: on a huge checkout,
/// erroring here would kill the whole coverage scan even though family
/// seeding from a partial path list is still useful. The caller surfaces the
/// partial flag through the report's `truncated` so the UI says "scan capped"
/// rather than presenting a silently reduced view as complete.
fn detect_families(repo: &Path) -> Result<FamilyScan, String> {
    let (stdout, mut listing_partial) = git_text_partial(
        repo,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
    let mut families: BTreeMap<String, CoverageFamilyStatus> = BTreeMap::new();
    let mut cargo_dirs: Vec<String> = Vec::new();
    let mut go_mod_dirs: Vec<String> = Vec::new();
    let mut go_work_at_root = false;
    let mut has_package_swift = false;
    let mut has_pubspec = false;
    let mut has_dotnet_proj = false;
    let mut classified = 0usize;
    for rel in stdout.split('\0') {
        let rel = LanguageDetector::normalize_rel_path(rel);
        if rel.is_empty() || skip_source(&rel) {
            continue;
        }
        if classified >= LISTING_ENTRY_CAP {
            // Enough entries to seed every family; further splitting is pure
            // cost. Flag rather than fail — see the doc comment above.
            listing_partial = true;
            break;
        }
        classified += 1;
        let name = file_name_of_rel(&rel);
        if name == "Cargo.toml" {
            let dir = rel_parent_dir(&rel);
            if !cargo_dirs.contains(&dir) {
                cargo_dirs.push(dir);
            }
        } else if name == "go.mod" {
            let dir = rel_parent_dir(&rel);
            if !go_mod_dirs.contains(&dir) {
                go_mod_dirs.push(dir);
            }
        } else if name == "go.work" && rel_parent_dir(&rel).is_empty() {
            go_work_at_root = true;
        } else if name.eq_ignore_ascii_case("Package.swift") {
            has_package_swift = true;
        } else if name.eq_ignore_ascii_case("pubspec.yaml") {
            has_pubspec = true;
        } else {
            let lower = name.to_ascii_lowercase();
            if lower.ends_with(".csproj")
                || lower.ends_with(".fsproj")
                || lower.ends_with(".vbproj")
                || lower.ends_with(".sln")
            {
                has_dotnet_proj = true;
            }
        }
        let info = LanguageDetector::detect_from_path(&rel);
        let Some(family) = LanguageDetector::coverage_family_hint(&rel, &info) else {
            continue;
        };
        let entry = families.entry(family.to_string()).or_insert_with(|| {
            let specs = specs_for(family);
            let mut formats = BTreeSet::new();
            let mut paths = Vec::new();
            for (path, fmt) in specs {
                formats.insert(fmt.as_str().to_string());
                paths.push((*path).to_string());
            }
            CoverageFamilyStatus {
                family: family.to_string(),
                languages: Vec::new(),
                color_hex: if family == "rust" {
                    // `info` may be a manifest (TOML) when the family was
                    // seeded by Cargo.toml alone; take Rust's real color.
                    LanguageDetector::detect_from_path("src/lib.rs")
                        .color_hex
                        .to_string()
                } else {
                    info.color_hex.to_string()
                },
                expected_formats: formats.into_iter().collect(),
                expected_paths: paths,
                found: false,
                suggested_commands: Vec::new(),
                setup_commands: Vec::new(),
                tool_ready: true,
                tool_detail: String::new(),
                duration_hint: String::new(),
            }
        });
        let label = if family == "rust" { "Rust" } else { info.name };
        if (info.is_programming() || family == "rust")
            && !entry.languages.iter().any(|l| l == label)
        {
            entry.languages.push(label.to_string());
        }
    }
    for status in families.values_mut() {
        status.languages.sort();
    }
    cargo_dirs.sort();
    go_mod_dirs.sort();
    Ok(FamilyScan {
        families,
        cargo_dirs,
        go_mod_dirs,
        go_work_at_root,
        has_package_swift,
        has_pubspec,
        has_dotnet_proj,
        listing_partial,
    })
}

fn collect_candidates(
    families: &BTreeMap<String, CoverageFamilyStatus>,
    cargo_dirs: &[String],
    go_mod_dirs: &[String],
) -> Vec<Candidate> {
    let mut out = Vec::new();
    // One candidate per artifact PATH: several families can list the same
    // file (lcov.info is claimed by more than one ecosystem), but its bytes
    // parse to exactly one result. Parsing it once is both cheaper and what
    // the report contract promises (a single artifact row per path).
    let mut seen = BTreeSet::new();
    for family in families.keys() {
        for (rel, format) in specs_for(family) {
            if seen.insert((*rel).to_string()) {
                out.push(Candidate {
                    rel: (*rel).to_string(),
                    format: *format,
                    family: family.clone(),
                });
            }
        }
    }
    for dir in cargo_dirs {
        if dir.is_empty() {
            continue; // root specs already cover the repo root
        }
        for (tail, format) in nested_rust_specs() {
            let rel = format!("{dir}/{tail}");
            if seen.insert(rel.clone()) {
                out.push(Candidate {
                    rel,
                    format: *format,
                    family: "rust".to_string(),
                });
            }
        }
    }
    for dir in go_mod_dirs {
        if dir.is_empty() {
            continue; // root specs already cover the repo root
        }
        for (tail, format) in nested_go_specs() {
            let rel = format!("{dir}/{tail}");
            if seen.insert(rel.clone()) {
                out.push(Candidate {
                    rel,
                    format: *format,
                    family: "go".to_string(),
                });
            }
        }
    }
    out
}

/// Artifact locations relative to a non-root Go module directory.
fn nested_go_specs() -> &'static [(&'static str, CoverageFormat)] {
    &[
        ("coverage.out", CoverageFormat::GoCover),
        ("cover.out", CoverageFormat::GoCover),
        ("coverage.txt", CoverageFormat::GoCover),
    ]
}

/// Artifact locations relative to a non-root cargo workspace directory.
fn nested_rust_specs() -> &'static [(&'static str, CoverageFormat)] {
    &[
        ("lcov.info", CoverageFormat::Lcov),
        ("cobertura.xml", CoverageFormat::Cobertura),
        ("target/llvm-cov/lcov.info", CoverageFormat::Lcov),
        ("target/llvm-cov/cobertura.xml", CoverageFormat::Cobertura),
        ("target/llvm-cov-target/lcov.info", CoverageFormat::Lcov),
        ("target/tarpaulin/cobertura.xml", CoverageFormat::Cobertura),
        ("target/coverage/lcov.info", CoverageFormat::Lcov),
    ]
}

/// Directories probed for unknown-named artifacts relative to a non-root
/// cargo workspace directory.
fn nested_rust_dirs() -> &'static [&'static str] {
    &[
        "coverage",
        "target/llvm-cov",
        "target/llvm-cov-target",
        "target/coverage",
    ]
}

/// Returns true when any probed directory held an artifact-shaped filename
/// beyond its MAX_DIR_ENTRIES window (so a real artifact may have been
/// dropped unseen).
///
/// Contract (audit M2): junk beyond the window must NOT raise the flag.
/// Flagging every over-64 probe dir marked nearly every Python repo (large
/// generated `htmlcov/`) "scan capped" forever, teaching users to ignore the
/// chip. Within the window behavior is unchanged; past it we stop collecting
/// candidates but still screen names, so a genuinely missed artifact still
/// fires exactly once, at the first hit.
fn extend_directory_candidates(
    repo: &Path,
    families: &BTreeMap<String, CoverageFamilyStatus>,
    cargo_dirs: &[String],
    out: &mut Vec<Candidate>,
) -> bool {
    let mut seen: BTreeSet<String> = out.iter().map(|c| c.rel.clone()).collect();
    let mut was_truncated = false;
    let mut scan_dir =
        |dir: &str, family: &str, seen: &mut BTreeSet<String>, out: &mut Vec<Candidate>| {
            let Ok(joined) = sandbox_join(repo, dir) else {
                return;
            };
            let Ok(entries) = std::fs::read_dir(&joined) else {
                return;
            };
            for (idx, entry) in entries.filter_map(|e| e.ok()).enumerate() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if idx >= MAX_DIR_ENTRIES {
                    // Beyond the window we no longer collect candidates (the
                    // window bounds the probe), but an artifact-shaped name
                    // here is precisely the "dropped unseen" case the flag
                    // exists for. Junk (.html pages and friends) is noise,
                    // not signal.
                    if format_from_filename(&name).is_some() {
                        was_truncated = true;
                        break;
                    }
                    continue;
                }
                let Some(format) = format_from_filename(&name) else {
                    continue;
                };
                let rel = format!("{}/{}", dir.trim_end_matches('/'), name);
                if !seen.insert(rel.clone()) {
                    continue;
                }
                out.push(Candidate {
                    rel,
                    format,
                    family: family.to_string(),
                });
            }
        };
    for family in families.keys() {
        for dir in extra_dirs_for(family) {
            scan_dir(dir, family, &mut seen, out);
        }
    }
    for dir in cargo_dirs {
        if dir.is_empty() {
            continue; // root extras already cover the repo root
        }
        for tail in nested_rust_dirs() {
            scan_dir(&format!("{dir}/{tail}"), "rust", &mut seen, out);
        }
    }
    was_truncated
}

fn format_from_filename(name: &str) -> Option<CoverageFormat> {
    let lower = name.to_ascii_lowercase();
    // Any ".info" is treated as lcov: custom names like custom.info are
    // legitimate (dir-listing discovery is deliberately permissive). Junk
    // that parses to no records surfaces as a skipped artifact row with a
    // reason instead of a fake success, so permissiveness stays honest.
    if lower == "lcov.info" || lower.ends_with(".info") {
        Some(CoverageFormat::Lcov)
    } else if lower == "clover.xml" {
        Some(CoverageFormat::Clover)
    } else if lower.ends_with(".xml")
        && (lower.contains("cobertura") || lower.contains("coverage") || lower.contains("jacoco"))
    {
        if lower.contains("jacoco") {
            Some(CoverageFormat::Jacoco)
        } else {
            Some(CoverageFormat::Cobertura)
        }
    } else if lower.ends_with(".json") && lower.contains("coverage") {
        // Totals-only summaries (`coverage-summary.json` from vitest's
        // json-summary reporter) carry no per-line records; parsing them as
        // Istanbul JSON yields an empty-but-successful map. Skip them —
        // `coverage-final.json` remains the Istanbul source of truth.
        if lower.ends_with("-summary.json") || lower == "coverage-summary.json" {
            None
        } else {
            Some(CoverageFormat::Istanbul)
        }
    } else {
        None
    }
}

enum ArtifactRead {
    Missing,
    Skipped { reason: String },
    Text(String),
}

fn read_artifact(repo: &Path, rel: &str, max_bytes: u64) -> ArtifactRead {
    let dest = match sandbox_join(repo, rel) {
        Ok(p) => p,
        Err(_) => return ArtifactRead::Missing,
    };
    let canon = match dest.canonicalize() {
        Ok(p) => p,
        Err(_) => return ArtifactRead::Missing,
    };
    if !canon.starts_with(repo) {
        return ArtifactRead::Skipped {
            reason: "artifact path escaped the repository".into(),
        };
    }
    let meta = match std::fs::metadata(&canon) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            return ArtifactRead::Skipped {
                reason: "permission denied".into(),
            };
        }
        Err(_) => return ArtifactRead::Missing,
    };
    // Regular files only. FIFOs, sockets, and device nodes pass size checks
    // (len() is 0 for a FIFO) and would block `fs::read` forever on open
    // with no writer, pinning a scan thread.
    if !meta.is_file() {
        return ArtifactRead::Skipped {
            reason: "not a regular file".into(),
        };
    }
    if meta.len() > max_bytes {
        return ArtifactRead::Skipped {
            reason: format!("artifact exceeds {} byte limit ({})", max_bytes, meta.len()),
        };
    }
    // Residual TOCTOU: the path can be swapped (e.g. re-pointed at a symlink
    // outside the repo) between canonicalize and read, same as the documented
    // write-path residual in git_cli.rs. Read-only exposure; attacker already
    // needs local repo-write access.
    match std::fs::read(&canon) {
        Ok(bytes) => {
            if bytes.contains(&0) {
                ArtifactRead::Skipped {
                    reason: "binary coverage file".into(),
                }
            } else {
                ArtifactRead::Text(String::from_utf8_lossy(&bytes).into_owned())
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => ArtifactRead::Skipped {
            reason: "permission denied".into(),
        },
        Err(_) => ArtifactRead::Missing,
    }
}

fn parse_artifact(
    format: CoverageFormat,
    text: &str,
    repo: &Path,
    budget: &mut EntryBudget,
    go_expansion_capped: &mut bool,
) -> Result<HashMap<String, BTreeMap<usize, u64>>, String> {
    clear_existence_memo();
    let text = text.trim_start_matches('\u{feff}');
    match format {
        CoverageFormat::Lcov => Ok(parse_lcov(text, repo, budget)),
        CoverageFormat::Cobertura => Ok(parse_cobertura(text, repo, budget)),
        CoverageFormat::GoCover => Ok(parse_go_cover(text, repo, budget, go_expansion_capped)),
        CoverageFormat::Istanbul => parse_istanbul(text, repo, budget),
        CoverageFormat::Jacoco => Ok(parse_jacoco(text, repo, budget)),
        CoverageFormat::Clover => Ok(parse_clover(text, repo, budget)),
        CoverageFormat::CoveragePyDb => Err("binary coverage.py database".into()),
    }
}

fn parse_lcov(
    text: &str,
    repo: &Path,
    budget: &mut EntryBudget,
) -> HashMap<String, BTreeMap<usize, u64>> {
    fn flush_record(
        out: &mut HashMap<String, BTreeMap<usize, u64>>,
        current: &mut Option<String>,
        lines: &mut BTreeMap<usize, u64>,
        budget: &mut EntryBudget,
    ) {
        if let Some(path) = current.take() {
            if !lines.is_empty() {
                merge_file(out, path, std::mem::take(lines), budget);
            } else {
                lines.clear();
            }
        } else {
            lines.clear();
        }
    }
    let mut out = HashMap::new();
    let mut current: Option<String> = None;
    let mut lines = BTreeMap::new();
    for raw in text.lines() {
        if let Some(sf) = raw.strip_prefix("SF:") {
            flush_record(&mut out, &mut current, &mut lines, budget);
            current = relativize_or_suffix(repo, sf.trim());
        } else if raw.trim() == "end_of_record" {
            flush_record(&mut out, &mut current, &mut lines, budget);
        } else if let Some(da) = raw.strip_prefix("DA:") {
            // DA:<line>,<hits>[,<checksum>] — the optional checksum must be
            // ignored rather than glued onto the hits field.
            let mut parts = da.split(',');
            if let (Some(ln), Some(hits)) = (parts.next(), parts.next()) {
                if let (Ok(ln), Ok(hits)) = (ln.trim().parse::<usize>(), hits.trim().parse::<u64>())
                {
                    record_hit_unbudgeted(&mut lines, ln, hits);
                }
            }
        }
    }
    flush_record(&mut out, &mut current, &mut lines, budget);
    out
}

fn strip_tag<'a>(fragment: &'a str, name: &str) -> Option<&'a str> {
    let rest = fragment.strip_prefix(name)?;
    let next = rest.chars().next();
    if matches!(next, None | Some(' ' | '\t' | '\n' | '\r' | '/' | '>')) {
        Some(rest.trim_start())
    } else {
        None
    }
}

fn parse_cobertura(
    text: &str,
    repo: &Path,
    budget: &mut EntryBudget,
) -> HashMap<String, BTreeMap<usize, u64>> {
    let mut out = HashMap::new();
    let mut current: Option<String> = None;
    let mut lines = BTreeMap::new();
    for piece in strip_xml_comments(text).split('<') {
        let trimmed = piece.trim();
        if strip_tag(trimmed, "class").is_some() || strip_tag(trimmed, "file").is_some() {
            if let Some(path) = current.take() {
                merge_file(&mut out, path, std::mem::take(&mut lines), budget);
            }
            let reported = xml_attr(trimmed, "filename")
                .or_else(|| xml_attr(trimmed, "name"))
                .map(decode_xml_entities)
                .unwrap_or_default();
            current = relativize_or_suffix(repo, &reported);
        } else if current.is_some() {
            if let Some(rest) = strip_tag(trimmed, "line") {
                let ln = xml_attr(rest, "number").or_else(|| xml_attr(rest, "nr"));
                let hits = xml_attr(rest, "hits").or_else(|| xml_attr(rest, "count"));
                if let (Some(ln), Some(hits)) = (ln, hits) {
                    if let (Ok(ln), Ok(hits)) = (ln.parse::<usize>(), hits.parse::<u64>()) {
                        record_hit_unbudgeted(&mut lines, ln, hits);
                    }
                }
            }
        }
    }
    if let Some(path) = current.take() {
        merge_file(&mut out, path, lines, budget);
    }
    out
}

fn parse_go_cover(
    text: &str,
    repo: &Path,
    budget: &mut EntryBudget,
    expansion_capped: &mut bool,
) -> HashMap<String, BTreeMap<usize, u64>> {
    // A few KB of go-cover text can describe tens of millions of covered
    // lines via block ranges. Expanding them all would explode memory and
    // CPU, so each file gets a hard expansion budget; once spent, further
    // ranges for that file are ignored (coverage stays representative, the
    // scan stays bounded). Those drops set `expansion_capped` (audit M3):
    // without the signal, totals over reduced data read as authoritative.
    const MAX_EXPANDED_LINES_PER_FILE: usize = 200_000;
    let mut out = HashMap::new();
    let mut expansion: HashMap<String, usize> = HashMap::new();
    // One artifact can repeat a path across thousands of block lines; each
    // resolution stats the filesystem, so resolve every distinct path once.
    let mut resolved_paths: HashMap<String, Option<String>> = HashMap::new();
    for raw in text.lines() {
        if raw.starts_with("mode:") || raw.trim().is_empty() {
            continue;
        }
        let Some((path_part, rest)) = raw.rsplit_once(':') else {
            continue;
        };
        let trimmed_path = path_part.trim().to_string();
        let Some(path) = resolved_paths
            .entry(trimmed_path.clone())
            .or_insert_with(|| relativize_or_suffix(repo, &trimmed_path))
            .clone()
        else {
            continue;
        };
        let Some((range, counts)) = rest.split_once(' ') else {
            continue;
        };
        let mut count_parts = counts.split_whitespace();
        let _stmts = count_parts.next();
        let Some(count) = count_parts.next() else {
            continue;
        };
        let Ok(hits) = count.parse::<u64>() else {
            continue;
        };
        let Some((start, end)) = range.split_once(',') else {
            continue;
        };
        let Ok(start_line) = start.split('.').next().unwrap_or("").parse::<usize>() else {
            continue;
        };
        let Ok(end_line) = end.split('.').next().unwrap_or("").parse::<usize>() else {
            continue;
        };
        if budget.remaining == 0 {
            budget.mark_dropped();
            break;
        }
        let allowance = expansion
            .entry(path.clone())
            .or_insert(MAX_EXPANDED_LINES_PER_FILE);
        if *allowance == 0 {
            // Allowance spent: this range (and any later ones for the file)
            // never reach the map. Surface it instead of dropping silently.
            *expansion_capped = true;
            continue;
        }
        let file = out.entry(path.clone()).or_default();
        let bounded_end = end_line
            .max(start_line)
            .min(start_line.saturating_add(10_000))
            .min(MAX_LINE_NO);
        if bounded_end < end_line.max(start_line) {
            *expansion_capped = true;
        }
        // Clamp the span to whatever budget remains; a range that straddles
        // the boundary still records its reachable head.
        let span_end = start_line
            .saturating_add((*allowance).saturating_sub(1))
            .min(bounded_end);
        if span_end < start_line {
            *allowance = 0;
            *expansion_capped = true;
            continue;
        }
        if span_end < bounded_end {
            // The allowance, not the parser's own per-range policy, cut this
            // range: the tail is real coverage data the map will not hold.
            *expansion_capped = true;
        }
        let expanded = span_end - start_line + 1;
        *allowance -= expanded;
        for ln in start_line..=span_end {
            record_hit(file, ln, hits, budget);
        }
    }
    out
}

fn parse_istanbul(
    text: &str,
    repo: &Path,
    budget: &mut EntryBudget,
) -> Result<HashMap<String, BTreeMap<usize, u64>>, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}"))?;
    let obj = value
        .as_object()
        .ok_or_else(|| "istanbul root must be an object".to_string())?;
    let mut out = HashMap::new();
    for (key, file) in obj {
        let reported = file
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(key.as_str());
        let Some(path) = relativize_or_suffix(repo, reported) else {
            continue;
        };
        let dest = out.entry(path).or_default();
        if let Some(lines) = file.get("l").and_then(|v| v.as_object()) {
            for (ln, hits) in lines {
                if let Ok(ln) = ln.parse::<usize>() {
                    let hits = hits
                        .as_f64()
                        .map(|f| f.round() as u64)
                        .or_else(|| hits.as_u64())
                        .unwrap_or(0);
                    record_hit(dest, ln, hits, budget);
                }
            }
            continue;
        }
        let Some(s) = file.get("s").and_then(|v| v.as_object()) else {
            continue;
        };
        let Some(map) = file.get("statementMap").and_then(|v| v.as_object()) else {
            continue;
        };
        for (id, hits) in s {
            let Some(ln) = map
                .get(id)
                .and_then(|stmt| stmt.get("start"))
                .and_then(|s| s.get("line"))
                .and_then(|v| v.as_u64())
            else {
                continue;
            };
            // Istanbul records fractional hits for partial statement
            // coverage; round to nearest so 0.5 counts as hit (genhtml-style)
            // instead of truncating to "uncovered".
            let hits = hits
                .as_f64()
                .map(|f| f.round() as u64)
                .or_else(|| hits.as_u64())
                .unwrap_or(0);
            record_hit(dest, ln as usize, hits, budget);
        }
    }
    Ok(out)
}

fn parse_jacoco(
    text: &str,
    repo: &Path,
    budget: &mut EntryBudget,
) -> HashMap<String, BTreeMap<usize, u64>> {
    let mut out = HashMap::new();
    let mut package = String::new();
    let mut current: Option<String> = None;
    let mut lines = BTreeMap::new();
    for piece in strip_xml_comments(text).split('<') {
        let trimmed = piece.trim();
        if strip_tag(trimmed, "package").is_some() {
            package = xml_attr(trimmed, "name")
                .map(decode_xml_entities)
                .unwrap_or_default()
                .replace('.', "/");
        } else if strip_tag(trimmed, "sourcefile").is_some() {
            if let Some(path) = current.take() {
                merge_file(&mut out, path, std::mem::take(&mut lines), budget);
            }
            let name = xml_attr(trimmed, "name")
                .map(decode_xml_entities)
                .unwrap_or_default();
            let reported = if package.is_empty() {
                name.to_string()
            } else {
                format!("{package}/{name}")
            };
            current = relativize_or_suffix(repo, &reported);
        } else if current.is_some() && strip_tag(trimmed, "line").is_some() {
            let ln = xml_attr(trimmed, "nr");
            let ci = xml_attr(trimmed, "ci")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            let mi = xml_attr(trimmed, "mi")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            if ci.saturating_add(mi) == 0 {
                continue;
            }
            if let Some(ln) = ln.and_then(|v| v.parse::<usize>().ok()) {
                record_hit_unbudgeted(&mut lines, ln, ci);
            }
        }
    }
    if let Some(path) = current.take() {
        merge_file(&mut out, path, lines, budget);
    }
    out
}

fn parse_clover(
    text: &str,
    repo: &Path,
    budget: &mut EntryBudget,
) -> HashMap<String, BTreeMap<usize, u64>> {
    let mut out = HashMap::new();
    let mut current: Option<String> = None;
    let mut lines = BTreeMap::new();
    for piece in strip_xml_comments(text).split('<') {
        let trimmed = piece.trim();
        if strip_tag(trimmed, "file").is_some() {
            if let Some(path) = current.take() {
                merge_file(&mut out, path, std::mem::take(&mut lines), budget);
            }
            let reported = xml_attr(trimmed, "path")
                .or_else(|| xml_attr(trimmed, "name"))
                .map(decode_xml_entities);
            current = reported
                .as_deref()
                .and_then(|r| relativize_or_suffix(repo, r));
        } else if current.is_some() && strip_tag(trimmed, "line").is_some() {
            let ln = xml_attr(trimmed, "num");
            let hits = xml_attr(trimmed, "count");
            if let (Some(ln), Some(hits)) = (ln, hits) {
                if let (Ok(ln), Ok(hits)) = (ln.parse::<usize>(), hits.parse::<u64>()) {
                    record_hit_unbudgeted(&mut lines, ln, hits);
                }
            }
        }
    }
    if let Some(path) = current.take() {
        merge_file(&mut out, path, lines, budget);
    }
    out
}

/// Removes `<!-- … -->` comments before tag splitting. Comments may legally
/// contain `<class filename="…">`-shaped text; without stripping, a comment
/// can forge coverage records.
fn strip_xml_comments(text: &str) -> String {
    const OPEN: &str = "<!--";
    const CLOSE: &str = "-->";
    if !text.contains(OPEN) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        let after = &rest[start + OPEN.len()..];
        match after.find(CLOSE) {
            Some(end) => rest = &after[end + CLOSE.len()..],
            // Unterminated comment swallows the remainder — same as XML.
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Decodes the five predefined XML entities plus decimal/hex character
/// references in attribute values. Filenames arrive entity-encoded (a file
/// literally named `a&b.ts` is recorded as `a&amp;b.ts`); without decoding,
/// exact matching fails. Invalid escapes are kept verbatim.
fn decode_xml_entities(value: &str) -> String {
    if !value.contains('&') {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        let semi = tail.find(';');
        let decode = semi.and_then(|s| {
            let body = &tail[1..s];
            match body {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => {
                    if let Some(hex) = body.strip_prefix("#x").or_else(|| body.strip_prefix("#X")) {
                        u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
                    } else if let Some(dec) = body.strip_prefix('#') {
                        dec.parse::<u32>().ok().and_then(char::from_u32)
                    } else {
                        None
                    }
                }
            }
        });
        match (semi, decode) {
            (Some(s), Some(ch)) => {
                out.push(ch);
                rest = &tail[s + 1..];
            }
            // Unknown or malformed entity: keep the '&' literally.
            _ => {
                out.push('&');
                rest = &rest[amp + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Extracts `name="value"` or `name='value'` from a tag fragment. Some
/// coverage generators emit single-quoted XML attributes. The attribute name
/// must start at a whitespace boundary so a decoy like
/// `classname="filename=x"` cannot shadow a missing real `filename`.
fn xml_attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    for quote in ['"', '\''] {
        let mut search_from = 0usize;
        while let Some(rel) = tag[search_from..].find(name) {
            let start = search_from + rel;
            let boundary_ok = start == 0 || {
                let prev = tag[..start].chars().next_back();
                matches!(prev, Some(c) if c.is_whitespace() || c == '<')
            };
            if boundary_ok {
                let after_name = &tag[start + name.len()..];
                let after_ws = after_name.trim_start();
                if let Some(after_eq) = after_ws.strip_prefix('=') {
                    let after_eq_ws = after_eq.trim_start();
                    if let Some(after_quote) = after_eq_ws.strip_prefix(quote) {
                        if let Some(end) = after_quote.find(quote) {
                            return Some(&after_quote[..end]);
                        }
                    }
                }
            }
            search_from = start + name.len();
            if search_from >= tag.len() {
                break;
            }
        }
    }
    None
}

/// Records one line hit. Duplicate records for the same line keep the MAXIMUM
/// hit count rather than summing: lcov tools emit repeated SF/DA blocks for
/// concatenated test runs, and max keeps counts stable under re-merges of
/// overlapping data (presence/absence — what the UI gates on — is identical).
///
/// Hits are clamped to u32::MAX before storage: the counts cross Tauri IPC
/// into JavaScript, where values beyond Number.MAX_SAFE_INTEGER (2^53) — and
/// u64::MAX especially, which serde_json renders as a corrupted float like
/// 18446744073709552000 — cannot be displayed faithfully in the gutter badge.
/// u32::MAX stays exact end to end while remaining far above any real
/// per-line execution count; presence/percentage semantics are unchanged
/// because any value >= 1 stays >= 1.
fn record_hit(lines: &mut BTreeMap<usize, u64>, ln: usize, hits: u64, budget: &mut EntryBudget) {
    if ln == 0 || ln > MAX_LINE_NO {
        return;
    }
    let hits = hits.min(u32::MAX as u64);
    match lines.entry(ln) {
        std::collections::btree_map::Entry::Vacant(slot) => {
            if budget.spend() {
                slot.insert(hits);
            }
        }
        std::collections::btree_map::Entry::Occupied(mut slot) => {
            let entry = slot.get_mut();
            *entry = (*entry).max(hits);
        }
    }
}

/// Records into a short-lived parser record without spending a budget yet.
/// The record is charged when it merges into the artifact map, where duplicate
/// SF/class/file blocks can be recognized and counted only once.
fn record_hit_unbudgeted(lines: &mut BTreeMap<usize, u64>, ln: usize, hits: u64) {
    if ln == 0 || ln > MAX_LINE_NO {
        return;
    }
    let hits = hits.min(u32::MAX as u64);
    lines
        .entry(ln)
        .and_modify(|value| *value = (*value).max(hits))
        .or_insert(hits);
}

/// Merges one record into a budgeted map. This owns the unique-key debit for
/// both artifact-local maps and the final scan map.
fn merge_file(
    out: &mut HashMap<String, BTreeMap<usize, u64>>,
    path: String,
    lines: BTreeMap<usize, u64>,
    budget: &mut EntryBudget,
) {
    if skip_source(&path) || lines.is_empty() {
        return;
    }
    if !out.contains_key(&path) && budget.remaining == 0 {
        budget.mark_dropped();
        return;
    }
    let dest = out.entry(path).or_default();
    for (ln, hits) in lines {
        record_hit(dest, ln, hits, budget);
    }
}

fn merge_into(
    dest: &mut HashMap<String, BTreeMap<usize, u64>>,
    src: HashMap<String, BTreeMap<usize, u64>>,
    budget: &mut EntryBudget,
) -> bool {
    // Deterministic order: HashMap iteration is randomized per process, and
    // which files survive a mid-artifact budget exhaustion must not depend on
    // the launch.
    let mut ordered: Vec<(String, HitMap)> = src.into_iter().collect();
    ordered.sort_by(|a, b| a.0.cmp(&b.0));
    let mut exhausted = false;
    for (path, lines) in ordered {
        merge_file(dest, path, lines, budget);
        exhausted |= budget.dropped();
    }
    exhausted
}

fn totals_of(map: &HashMap<String, BTreeMap<usize, u64>>) -> CoverageTotals {
    let found = map.values().map(|l| l.len()).sum();
    let hit = map
        .values()
        .map(|l| l.values().filter(|h| **h > 0).count())
        .sum();
    CoverageTotals::from_counts(found, hit)
}

fn skip_source(path: &str) -> bool {
    LanguageDetector::is_ignored_source_path(path)
}

fn file_name_of_rel(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

fn rel_parent_dir(rel: &str) -> String {
    std::path::Path::new(rel)
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default()
}

/// Rolls per-file coverage up into per-language totals (the language split).
/// Sorted by volume (lines found, descending) so the UI lists the dominant
/// language first; ties break alphabetically for stable output.
fn language_split(files: &[FileCoverageSummary]) -> Vec<CoverageLanguageSplit> {
    let mut order: Vec<String> = Vec::new();
    // (files, lines_found, lines_hit, first-seen color)
    let mut acc: HashMap<String, (usize, usize, usize, String)> = HashMap::new();
    for file in files {
        let entry = acc.entry(file.language.clone()).or_insert_with(|| {
            order.push(file.language.clone());
            (0, 0, 0, file.color_hex.clone())
        });
        entry.0 += 1;
        entry.1 += file.lines_found;
        entry.2 += file.lines_hit;
    }
    let mut split: Vec<CoverageLanguageSplit> = order
        .into_iter()
        .map(|language| {
            let (files, found, hit, color_hex) = acc[&language].clone();
            CoverageLanguageSplit {
                language,
                color_hex,
                files,
                lines_found: found,
                lines_hit: hit,
                percentage: CoverageTotals::from_counts(found, hit).percentage,
            }
        })
        .collect();
    split.sort_by(|a, b| {
        b.lines_found
            .cmp(&a.lines_found)
            .then_with(|| a.language.cmp(&b.language))
    });
    split
}

fn is_safe_rel(path: &str) -> bool {
    if path.is_empty() || path.starts_with('/') || path.contains('\0') {
        return false;
    }
    !path.split('/').any(|c| c == "..")
}

/// Decodes `%XX` escapes in a `file://` URI path. Invalid escapes are kept
/// verbatim; only URIs are decoded so literal `%` in plain paths survives.
fn percent_decode_uri_path(uri: &str) -> String {
    let bytes = uri.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit()
        {
            let hi = (bytes[i + 1] as char).to_digit(16).unwrap_or(0) as u8;
            let lo = (bytes[i + 2] as char).to_digit(16).unwrap_or(0) as u8;
            out.push(hi * 16 + lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| uri.to_string())
}

fn relativize(repo: &Path, reported: &str) -> Option<String> {
    let reported = reported.trim().trim_matches('"');
    if reported.is_empty() || reported.contains('\0') {
        return None;
    }
    let decoded;
    let reported = match reported.strip_prefix("file://") {
        Some(rest) => {
            decoded = percent_decode_uri_path(rest);
            decoded.as_str()
        }
        None => reported,
    };
    let path = Path::new(reported);
    if path.is_absolute() {
        let repo_canon = repo.canonicalize().ok()?;
        let canon = path.canonicalize().ok()?;
        let rel = canon.strip_prefix(&repo_canon).ok()?;
        let s = rel.to_string_lossy().replace('\\', "/");
        if is_safe_rel(&s) && !skip_source(&s) {
            return Some(s);
        }
        return None;
    }
    let s = reported.replace('\\', "/");
    let s = s.trim_start_matches("./").to_string();
    if is_safe_rel(&s) && !skip_source(&s) {
        Some(s)
    } else {
        None
    }
}

// Existence-check memo shared by one artifact parse. `relativize_or_suffix`
// stats the filesystem per candidate path; a hostile 8 MB artifact full of
// deep paths would otherwise issue millions of stats. Cleared at the start
// of every artifact parse so results stay correct as files appear/disappear.
thread_local! {
    static EXISTENCE_MEMO: std::cell::RefCell<HashMap<String, bool>> =
        std::cell::RefCell::new(HashMap::new());
}

fn path_exists_cached(repo: &Path, rel: &str) -> bool {
    let key = format!("{}\0{}", repo.display(), rel);
    EXISTENCE_MEMO.with(|memo| {
        *memo.borrow_mut().entry(key).or_insert_with(|| {
            sandbox_join_canonical(repo, rel)
                .map(|path| path.is_file())
                .unwrap_or(false)
        })
    })
}

fn clear_existence_memo() {
    EXISTENCE_MEMO.with(|memo| memo.borrow_mut().clear());
}

fn relativize_or_suffix(repo: &Path, reported: &str) -> Option<String> {
    let rel = relativize(repo, reported);
    if let Some(ref path) = rel {
        if path_exists_cached(repo, path) {
            return rel;
        }
    }
    let cleaned = reported.replace('\\', "/");
    let parts: Vec<&str> = cleaned
        .split('/')
        .filter(|p| !p.is_empty() && *p != "..")
        .collect();
    // Longest-suffix-first (most specific match wins), bounded to the eight
    // most specific attempts: deeper tails cost a stat each and ambiguity,
    // not precision, dominates beyond that.
    for i in parts.len().saturating_sub(8)..parts.len() {
        let candidate = parts[i..].join("/");
        if is_safe_rel(&candidate)
            && !skip_source(&candidate)
            && path_exists_cached(repo, &candidate)
        {
            return Some(candidate);
        }
    }
    rel
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::language::LanguageInfo;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    /// Regression (audit M4): every per-file detail request re-parsed every
    /// artifact on disk. A second identical scan must be served from the
    /// mtime-keyed cache without a single new parse, and an artifact change
    /// must invalidate it.
    #[test]
    fn repeated_scans_are_cached_until_artifacts_change() {
        // Cache-count assertions require our entry to survive; serialize
        // against other tests' cache churn (bounded cache eviction).
        let _sequence = CACHE_SEQUENCE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _frozen = FreezeEvictionGuard::new();
        let repo = git_repo();
        write(repo.path(), "src/main.go", "package main\n");
        write(
            repo.path(),
            "coverage.out",
            "mode: set\nsrc/main.go:1.1,2.10 2 1\n",
        );
        let path = repo.path().to_str().unwrap();

        SCAN_PARSE_COUNT.with(|c| c.set(0));
        let first = CoverageScanner::scan(path).expect("first scan");
        let after_first = SCAN_PARSE_COUNT.with(|c| c.get());
        assert!(after_first >= 1, "first scan must parse");

        let second = CoverageScanner::scan(path).expect("second scan");
        assert_eq!(SCAN_PARSE_COUNT.with(|c| c.get()), after_first);
        assert_eq!(first.overall, second.overall);

        // Detail requests ride the same cache instead of rescanning.
        let detail =
            CoverageScanner::file_coverage(path, "src/main.go").expect("detail from cache");
        assert_eq!(detail.totals.lines_hit, first.files[0].lines_hit);
        assert_eq!(SCAN_PARSE_COUNT.with(|c| c.get()), after_first);

        // Changing the artifact invalidates the entry.
        write(
            repo.path(),
            "coverage.out",
            "mode: set\nsrc/main.go:1.1,2.10 0 0\n",
        );
        let third = CoverageScanner::scan(path).expect("scan after change");
        assert_eq!(
            SCAN_PARSE_COUNT.with(|c| c.get()),
            after_first * 2,
            "changed artifact must force one fresh parse"
        );
        assert_ne!(third.overall.lines_hit, second.overall.lines_hit);
    }

    #[test]
    fn go_work_use_block_is_parsed() {
        let (dirs, partial) = parse_go_work_use(
            "go 1.22\n\nuse (\n\t./manvi\n\t./bench/live // the live bench\n\t.\n)\n",
        );
        assert_eq!(dirs, vec!["manvi", "bench/live", ""]);
        assert!(!partial);
    }

    #[test]
    fn go_work_single_line_and_inline_block_spellings_are_parsed() {
        assert_eq!(parse_go_work_use("use ./a\nuse ./b\n").0, vec!["a", "b"]);
        assert_eq!(parse_go_work_use("use (./a)\n").0, vec!["a"]);
        assert_eq!(parse_go_work_use("use \"./quoted\"\n").0, vec!["quoted"]);
    }

    #[test]
    fn go_work_parser_refuses_paths_that_leave_the_repository() {
        // These become `go -C <dir>` arguments. The backend gate refuses an
        // escaping path too, but a planner that offers one is already wrong.
        let dirs = parse_go_work_use("use (\n\t/etc\n\t../sibling\n\t./ok\n)\n").0;
        assert_eq!(dirs, vec!["ok"]);
    }

    #[test]
    fn go_work_parser_refuses_shell_unsafe_directories() {
        let dirs = parse_go_work_use("use (\n\t./a;rm -rf\n\t./b$(x)\n\t./fine\n)\n").0;
        assert_eq!(dirs, vec!["fine"]);
    }

    #[test]
    fn go_work_parser_ignores_words_that_merely_start_with_use() {
        assert!(parse_go_work_use("used ./a\nuseful ./b\ngo 1.22\n")
            .0
            .is_empty());
    }

    #[test]
    fn go_work_parser_is_bounded_and_never_hangs_on_hostile_input() {
        let many = (0..1000)
            .map(|i| format!("use ./m{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (dirs, partial) = parse_go_work_use(&many);
        assert_eq!(dirs.len(), MAX_GO_MODULES);
        assert!(partial, "a capped workspace module list must be disclosed");
        // Unclosed block, stray parens, empty entries: no panic, no garbage.
        assert!(parse_go_work_use("use (\n\n\n").0.is_empty());
        assert!(parse_go_work_use(")\n)\nuse\n").0.is_empty());
        assert!(parse_go_work_use("").0.is_empty());
    }

    #[test]
    fn workspace_modules_win_over_the_filesystem_search() {
        let repo = git_repo();
        write(
            repo.path(),
            "go.work",
            "go 1.22\n\nuse (\n\t./svc\n\t./tool\n)\n",
        );
        write(repo.path(), "svc/go.mod", "module svc\n");
        write(repo.path(), "tool/go.mod", "module tool\n");
        // Present on disk but not in the workspace: `go.work` says which
        // modules the workspace actually builds, and a search cannot.
        write(repo.path(), "extra/go.mod", "module extra\n");
        let (dirs, partial) = discover_go_modules(repo.path());
        assert_eq!(dirs, vec!["svc", "tool"]);
        assert!(!partial);
    }

    #[test]
    fn a_stale_workspace_entry_is_dropped_rather_than_retried() {
        // Running `go -C gone` would reproduce the very failure this exists
        // to get past.
        let repo = git_repo();
        write(repo.path(), "go.work", "use (\n\t./live\n\t./gone\n)\n");
        write(repo.path(), "live/go.mod", "module live\n");
        let (dirs, _) = discover_go_modules(repo.path());
        assert_eq!(dirs, vec!["live"]);
    }

    #[test]
    fn the_search_finds_a_git_ignored_module_the_listing_cannot() {
        // The scan's own module list comes from `git ls-files
        // --exclude-standard`, which cannot see this one at all.
        let repo = git_repo();
        write(repo.path(), ".gitignore", "generated/\n");
        write(repo.path(), "generated/go.mod", "module generated\n");
        write(repo.path(), "svc/go.mod", "module svc\n");
        let (dirs, partial) = discover_go_modules(repo.path());
        assert_eq!(dirs, vec!["generated", "svc"]);
        assert!(!partial);
    }

    #[test]
    fn the_search_is_bounded_in_depth_and_says_nothing_it_did_not_see() {
        let repo = git_repo();
        // Deeper than MAX_GO_WALK_DEPTH: out of range, and its absence must
        // not be mistaken for "this repository has no such module".
        write(repo.path(), "a/b/c/d/e/go.mod", "module deep\n");
        write(repo.path(), "a/b/go.mod", "module shallow\n");
        let (dirs, partial) = discover_go_modules(repo.path());
        assert_eq!(dirs, vec!["a/b"]);
        assert!(
            partial,
            "skipping deeper directories must make the result partial"
        );
    }

    #[test]
    fn an_oversized_workspace_is_not_replaced_with_an_unlabelled_filesystem_guess() {
        let repo = git_repo();
        write(repo.path(), "svc/go.mod", "module svc\n");
        write(
            repo.path(),
            "go.work",
            &"x".repeat(MAX_GO_WORK_BYTES as usize + 1),
        );
        let (dirs, partial) = discover_go_modules(repo.path());
        assert!(dirs.is_empty(), "go.work is authoritative when present");
        assert!(partial, "an unreadable workspace must not look complete");
    }

    #[cfg(unix)]
    #[test]
    fn the_search_does_not_descend_into_symlinked_directories() {
        // A link to a real module inside the repository. Following it would
        // report one module twice, under its real path and under the alias,
        // and `go -C alias` measures the same code again.
        let repo = git_repo();
        write(repo.path(), "svc/go.mod", "module svc\n");
        std::os::unix::fs::symlink(repo.path().join("svc"), repo.path().join("alias")).unwrap();
        let (dirs, _) = discover_go_modules(repo.path());
        assert_eq!(dirs, vec!["svc"]);
    }

    #[cfg(unix)]
    #[test]
    fn the_search_cannot_reach_a_symlinked_tree_outside_the_repository() {
        // Belt and braces: the link is not descended into because it is a
        // symlink, and `sandbox_join_canonical` would refuse the resolved
        // path anyway.
        let repo = git_repo();
        let outside = TempDir::new().expect("tempdir");
        write(outside.path(), "go.mod", "module outside\n");
        std::os::unix::fs::symlink(outside.path(), repo.path().join("link")).unwrap();
        write(repo.path(), "svc/go.mod", "module svc\n");
        let (dirs, _) = discover_go_modules(repo.path());
        assert_eq!(dirs, vec!["svc"], "a symlinked tree is not this repository");
    }

    #[test]
    fn the_search_reports_itself_partial_when_it_hits_its_module_bound() {
        let repo = git_repo();
        for i in 0..MAX_GO_MODULES + 5 {
            write(repo.path(), &format!("m{i:02}/go.mod"), "module m\n");
        }
        let (dirs, partial) = discover_go_modules(repo.path());
        assert_eq!(dirs.len(), MAX_GO_MODULES);
        assert!(partial, "a capped module list must never look complete");
    }

    #[test]
    fn a_repository_whose_listing_found_modules_pays_nothing_for_the_search() {
        // The report carries module directories only for the case that needs
        // them; every other Go repository gets an empty list.
        let repo = git_repo();
        write(repo.path(), "go.mod", "module root\n");
        write(repo.path(), "main.go", "package main\n\nfunc main() {}\n");
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
        assert!(report.go_modules.is_empty());
        assert!(!report.go_modules_partial);
    }

    #[test]
    fn a_workspace_root_that_is_not_a_module_publishes_its_modules() {
        let repo = git_repo();
        write(repo.path(), "go.work", "go 1.22\n\nuse (\n\t./svc\n)\n");
        write(repo.path(), "svc/go.mod", "module svc\n");
        // Go source at the root, so the family is detected, but no root
        // go.mod — the shape whose root-level command fails.
        write(repo.path(), "doc.go", "package doc\n");
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
        assert_eq!(report.go_modules, vec!["svc"]);
    }

    fn git_repo() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        let status = Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .status()
            .expect("git init");
        assert!(status.success());
        dir
    }

    fn write(dir: &Path, rel: &str, content: &str) {
        let dest = dir.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(dest, content).unwrap();
    }

    /// Regression: a detected family with no plannable generator published
    /// `tool_ready = true` with no command, no reason and no duration — the
    /// exact row shape of a family GitPulse simply had no opinion about. The
    /// user's own report showed it as a bare `- native (expected: lcov)`.
    #[test]
    fn family_without_a_generator_is_never_reported_ready() {
        let repo = git_repo();
        // C sources seed `native`; no CMakeLists.txt and no Makefile exist, so
        // there is nothing to run.
        write(
            repo.path(),
            "src/engine.c",
            "int main(void) { return 0; }\n",
        );
        write(repo.path(), "src/engine.h", "int main(void);\n");
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");

        let native = report
            .families
            .iter()
            .find(|f| f.family == "native")
            .expect("C sources must seed the native family");
        assert!(
            native.suggested_commands.is_empty(),
            "no build file, so no generate command may be planned"
        );
        assert!(
            !native.tool_ready,
            "readiness was never checked; it must not be asserted"
        );
        assert!(
            native.tool_detail.contains("CMakeLists.txt"),
            "the row must say why it is a dead end, got {:?}",
            native.tool_detail
        );
        assert!(
            native.duration_hint.is_empty(),
            "a duration for a command that does not exist is noise"
        );
    }

    /// The invariant is enforced at the single application point, so a family
    /// GitPulse has no plan arm for at all is covered too.
    #[test]
    fn unplannable_families_still_explain_themselves() {
        let repo = git_repo();
        write(repo.path(), "lib/app.ex", "defmodule App do\nend\n");
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");

        let beam = report
            .families
            .iter()
            .find(|f| f.family == "beam")
            .expect("Elixir sources must seed the beam family");
        assert!(beam.suggested_commands.is_empty());
        assert!(!beam.tool_ready);
        assert!(
            !beam.tool_detail.is_empty(),
            "every actionless family row must carry a reason"
        );
    }

    /// Every family the scanner emits must satisfy the invariant, not just the
    /// ones a test happens to name.
    #[test]
    fn no_family_claims_readiness_without_a_command() {
        let repo = git_repo();
        write(
            repo.path(),
            "src/engine.c",
            "int main(void) { return 0; }\n",
        );
        write(repo.path(), "lib/app.ex", "defmodule App do\nend\n");
        write(repo.path(), "app/index.php", "<?php echo 1;\n");
        write(repo.path(), "app/main.rb", "puts 1\n");
        write(repo.path(), "src/main.go", "package main\n");
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
        assert!(
            report.families.len() >= 4,
            "fixture must seed several families"
        );
        for family in &report.families {
            if family.suggested_commands.is_empty() {
                assert!(
                    !family.tool_ready,
                    "{} claims a generator it cannot name",
                    family.family
                );
                assert!(
                    !family.tool_detail.is_empty(),
                    "{} gives the user nothing to act on and no reason",
                    family.family
                );
            }
        }
    }

    /// Regression: the file cap set `truncated` and nothing else, so the
    /// renderer could only headline the count it kept. That reads as a
    /// complete inventory of a small repo.
    #[test]
    fn file_cap_records_exact_retained_and_observed_counts() {
        let repo = git_repo();
        write(repo.path(), "src/a.go", "package main\n");
        write(repo.path(), "src/b.go", "package main\n");
        write(
            repo.path(),
            "coverage.out",
            "mode: set\nsrc/a.go:1.1,2.10 2 1\nsrc/b.go:1.1,2.10 2 1\n",
        );
        let limits = ScanLimits {
            max_files: 1,
            ..ScanLimits::default()
        };
        let (report, _) =
            CoverageScanner::scan_with_limits(repo.path().to_str().unwrap(), limits).expect("scan");

        assert!(report.truncated);
        assert_eq!(report.files.len(), 1);
        let notice = report
            .limit_notices
            .iter()
            .find(|n| n.resource == "covered files")
            .expect("the file cap must publish what it dropped");
        assert_eq!(notice.kept, 1);
        assert_eq!(notice.total, 2);
    }

    #[test]
    fn artifact_cap_records_exact_retained_and_observed_counts() {
        let repo = git_repo();
        write(repo.path(), "src/a.go", "package main\n");
        write(
            repo.path(),
            "coverage.out",
            "mode: set\nsrc/a.go:1.1,2.10 2 1\n",
        );
        write(
            repo.path(),
            "cover.out",
            "mode: set\nsrc/a.go:1.1,2.10 2 1\n",
        );
        let limits = ScanLimits {
            max_artifacts: 1,
            ..ScanLimits::default()
        };
        let (report, _) =
            CoverageScanner::scan_with_limits(repo.path().to_str().unwrap(), limits).expect("scan");

        assert!(report.truncated);
        let notice = report
            .limit_notices
            .iter()
            .find(|n| n.resource == "coverage artifacts")
            .expect("the artifact cap must publish what it dropped");
        assert_eq!(notice.kept, 1);
        assert_eq!(notice.total, 2);
    }

    /// A scan that hit no cap must publish no notices: an empty list is the
    /// signal the renderer uses to print unqualified counts.
    #[test]
    fn complete_scans_publish_no_limit_notices() {
        let repo = git_repo();
        write(repo.path(), "src/a.go", "package main\n");
        write(
            repo.path(),
            "coverage.out",
            "mode: set\nsrc/a.go:1.1,2.10 2 1\n",
        );
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
        assert!(!report.truncated);
        assert!(report.limit_notices.is_empty());
    }

    #[test]
    fn family_mapping_is_language_specific() {
        assert_eq!(LanguageDetector::coverage_family("Rust"), Some("rust"));
        assert_eq!(
            LanguageDetector::coverage_family("TypeScript"),
            Some("javascript")
        );
        assert_eq!(
            LanguageDetector::coverage_family("Svelte"),
            Some("javascript")
        );
        assert_eq!(LanguageDetector::coverage_family("Python"), Some("python"));
        assert_eq!(LanguageDetector::coverage_family("Go"), Some("go"));
        assert_eq!(LanguageDetector::coverage_family("Java"), Some("jvm"));
        assert_eq!(LanguageDetector::coverage_family("Markdown"), None);
    }

    #[test]
    fn rust_candidates_exclude_jacoco() {
        let mut families = BTreeMap::new();
        families.insert(
            "rust".into(),
            CoverageFamilyStatus {
                family: "rust".into(),
                languages: vec!["Rust".into()],
                color_hex: "#dea584".into(),
                expected_formats: vec!["lcov".into()],
                expected_paths: vec!["lcov.info".into()],
                found: false,
                suggested_commands: Vec::new(),
                setup_commands: Vec::new(),
                tool_ready: true,
                tool_detail: String::new(),
                duration_hint: String::new(),
            },
        );
        let cands = collect_candidates(&families, &[], &[]);
        assert!(cands.iter().any(|c| c.rel == "lcov.info"));
        assert!(!cands.iter().any(|c| c.format == CoverageFormat::Jacoco));
        assert!(!cands.iter().any(|c| c.format == CoverageFormat::GoCover));
    }

    #[test]
    fn python_candidates_exclude_go_cover() {
        let mut families = BTreeMap::new();
        families.insert(
            "python".into(),
            CoverageFamilyStatus {
                family: "python".into(),
                languages: vec!["Python".into()],
                color_hex: "#3572a5".into(),
                expected_formats: vec!["cobertura".into()],
                expected_paths: vec!["coverage.xml".into()],
                found: false,
                suggested_commands: Vec::new(),
                setup_commands: Vec::new(),
                tool_ready: true,
                tool_detail: String::new(),
                duration_hint: String::new(),
            },
        );
        let cands = collect_candidates(&families, &[], &[]);
        assert!(cands.iter().any(|c| c.rel == "coverage.xml"));
        assert!(!cands.iter().any(|c| c.format == CoverageFormat::GoCover));
        assert!(!cands.iter().any(|c| c.format == CoverageFormat::Jacoco));
    }

    #[test]
    fn parse_lcov_counts_hits_and_rejects_parent_escape() {
        let repo = git_repo();
        let text = "\
TN:
SF:src/lib.rs
DA:1,1
DA:2,0
DA:3,4
end_of_record
SF:../etc/passwd
DA:1,1
end_of_record
";
        let map = parse_lcov(text, repo.path(), &mut EntryBudget::new(usize::MAX));
        assert!(map.contains_key("src/lib.rs"));
        assert_eq!(map["src/lib.rs"].get(&1), Some(&1));
        assert_eq!(map["src/lib.rs"].get(&2), Some(&0));
        assert_eq!(map["src/lib.rs"].get(&3), Some(&4));
        assert!(!map.keys().any(|k| k.contains("passwd")));
        let totals = CoverageTotals::from_map(&map["src/lib.rs"]);
        assert_eq!(totals.lines_found, 3);
        assert_eq!(totals.lines_hit, 2);
        assert_eq!(totals.percentage, 66.7);
    }

    #[test]
    fn parse_go_cover_marks_line_ranges() {
        let repo = git_repo();
        write(repo.path(), "src/main.go", "package main\n");
        let text = "\
mode: set
src/main.go:1.1,2.10 2 1
src/main.go:4.1,4.8 1 0
";
        let map = parse_go_cover(
            text,
            repo.path(),
            &mut EntryBudget::new(usize::MAX),
            &mut false,
        );
        assert_eq!(map["src/main.go"].get(&1), Some(&1));
        assert_eq!(map["src/main.go"].get(&2), Some(&1));
        assert_eq!(map["src/main.go"].get(&4), Some(&0));
    }

    #[test]
    fn parse_cobertura_and_istanbul() {
        let repo = git_repo();
        let cob = r#"
<coverage>
  <class filename="pkg/mod.py" line-rate="0.5">
    <line number="1" hits="3"/>
    <line number="2" hits="0"/>
  </class>
</coverage>
"#;
        let cob_map = parse_cobertura(cob, repo.path(), &mut EntryBudget::new(usize::MAX));
        assert_eq!(cob_map["pkg/mod.py"].get(&1), Some(&3));
        assert_eq!(cob_map["pkg/mod.py"].get(&2), Some(&0));

        let escaped = r#"<coverage><class filename="../secret.py"><line number="1" hits="9"/></class></coverage>"#;
        let escaped_map = parse_cobertura(escaped, repo.path(), &mut EntryBudget::new(usize::MAX));
        assert!(!escaped_map.keys().any(|k| k.contains("secret")));

        write(repo.path(), "src/app.ts", "x\n");
        let abs = repo.path().join("src/app.ts");
        let istanbul = format!(
            r#"{{
  "{}": {{
    "path": "{}",
    "l": {{ "1": 2, "2": 0 }}
  }}
}}"#,
            abs.display(),
            abs.display()
        );
        let ist = parse_istanbul(&istanbul, repo.path(), &mut EntryBudget::new(usize::MAX))
            .expect("istanbul");
        assert_eq!(ist["src/app.ts"].get(&1), Some(&2));
        assert_eq!(ist["src/app.ts"].get(&2), Some(&0));
    }

    #[test]
    fn rust_repo_scans_lcov_and_ignores_jacoco() {
        let repo = git_repo();
        write(repo.path(), "src/lib.rs", "pub fn x() {}\n");
        write(
            repo.path(),
            "lcov.info",
            "SF:src/lib.rs\nDA:1,1\nDA:2,0\nend_of_record\n",
        );
        write(
            repo.path(),
            "target/site/jacoco/jacoco.xml",
            r#"<report><package name="x"><sourcefile name="Ignored.java"><line nr="1" ci="9" mi="0"/></sourcefile></package></report>"#,
        );
        let path = repo.path().to_str().unwrap();
        let report = CoverageScanner::scan(path).expect("scan");
        assert!(
            report
                .families
                .iter()
                .any(|f| f.family == "rust" && f.found),
            "rust family should be found: {:?}",
            report.families
        );
        assert!(
            !report.artifacts.iter().any(|a| a.format == "jacoco"),
            "jacoco must not be consulted for a rust tree: {:?}",
            report.artifacts
        );
        assert!(report.files.iter().any(|f| f.path == "src/lib.rs"));
        assert_eq!(report.overall.lines_found, 2);
        assert_eq!(report.overall.lines_hit, 1);
        assert_eq!(report.overall.percentage, 50.0);
    }

    #[test]
    fn python_repo_does_not_read_go_cover() {
        let repo = git_repo();
        write(repo.path(), "pkg/mod.py", "def f():\n    return 1\n");
        write(
            repo.path(),
            "coverage.xml",
            r#"<coverage><class filename="pkg/mod.py"><line number="1" hits="1"/><line number="2" hits="0"/></class></coverage>"#,
        );
        write(
            repo.path(),
            "coverage.out",
            "mode: set\npkg/mod.py:1.1,2.1 1 1\n",
        );
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
        assert!(report.families.iter().any(|f| f.family == "python"));
        assert!(!report.artifacts.iter().any(|a| a.format == "go_cover"));
        let file = report
            .files
            .iter()
            .find(|f| f.path == "pkg/mod.py")
            .expect("py file");
        assert_eq!(file.lines_found, 2);
        assert_eq!(file.lines_hit, 1);
    }

    #[test]
    fn jvm_repo_reads_jacoco() {
        let repo = git_repo();
        write(repo.path(), "com/foo/Bar.java", "class Bar {}\n");
        write(
            repo.path(),
            "target/site/jacoco/jacoco.xml",
            r#"<report><package name="com/foo"><sourcefile name="Bar.java"><line nr="1" ci="2" mi="0"/></sourcefile></package></report>"#,
        );
        write(
            repo.path(),
            "coverage.out",
            "mode: set\ncom/foo/Bar.java:1.1,1.8 1 1\n",
        );
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
        assert!(
            report.families.iter().any(|f| f.family == "jvm" && f.found),
            "jvm family: {:?}",
            report.families
        );
        assert!(report
            .artifacts
            .iter()
            .any(|a| a.format == "jacoco" && !a.skipped));
        assert!(!report.artifacts.iter().any(|a| a.format == "go_cover"));
        assert!(report.files.iter().any(|f| f.path.ends_with("Bar.java")));
    }

    #[test]
    fn oversized_artifact_is_skipped_not_parsed() {
        let repo = git_repo();
        write(repo.path(), "src/lib.rs", "fn x() {}\n");
        write(repo.path(), "lcov.info", &"DA:1,1\n".repeat(50));
        let limits = ScanLimits {
            max_artifact_bytes: 20,
            ..ScanLimits::default()
        };
        let (report, _) =
            CoverageScanner::scan_with_limits(repo.path().to_str().unwrap(), limits).unwrap();
        assert!(report.artifacts.iter().any(|a| a.skipped));
        assert!(report.files.is_empty());
    }

    #[test]
    fn file_coverage_returns_line_hits() {
        let repo = git_repo();
        write(repo.path(), "src/lib.rs", "fn a() {}\nfn b() {}\n");
        write(
            repo.path(),
            "lcov.info",
            "SF:src/lib.rs\nDA:1,3\nDA:2,0\nend_of_record\n",
        );
        let detail =
            CoverageScanner::file_coverage(repo.path().to_str().unwrap(), "src/lib.rs").unwrap();
        assert_eq!(detail.language, "Rust");
        assert_eq!(detail.lines.len(), 2);
        assert_eq!(detail.lines[0].hits, 3);
        assert_eq!(detail.lines[1].hits, 0);
    }

    #[test]
    fn empty_markdown_repo_scans_nothing() {
        let repo = git_repo();
        write(repo.path(), "README.md", "# docs\n");
        write(
            repo.path(),
            "lcov.info",
            "SF:README.md\nDA:1,1\nend_of_record\n",
        );
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).unwrap();
        assert!(report.families.is_empty());
        assert!(report.artifacts.is_empty());
    }

    #[test]
    fn javascript_dir_listing_picks_up_custom_lcov_name() {
        let repo = git_repo();
        write(repo.path(), "src/app.ts", "export const x = 1;\n");
        write(
            repo.path(),
            "coverage/custom.info",
            "SF:src/app.ts\nDA:1,1\nend_of_record\n",
        );
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).unwrap();
        assert!(report
            .artifacts
            .iter()
            .any(|a| a.path.ends_with("custom.info") && !a.skipped));
        assert!(report.files.iter().any(|f| f.path == "src/app.ts"));
    }

    #[test]
    fn parse_jacoco_and_clover() {
        let repo = git_repo();
        write(repo.path(), "com/foo/Bar.java", "class Bar {}\n");
        let jacoco = r#"
<report>
  <package name="com/foo">
    <sourcefile name="Bar.java">
      <line nr="1" mi="0" ci="4"/>
      <line nr="2" mi="3" ci="0"/>
    </sourcefile>
  </package>
</report>
"#;
        let map = parse_jacoco(jacoco, repo.path(), &mut EntryBudget::new(usize::MAX));
        assert_eq!(map["com/foo/Bar.java"].get(&1), Some(&4));
        assert_eq!(map["com/foo/Bar.java"].get(&2), Some(&0));

        let minified = r#"<report><package name="com/foo"><sourcefile name="Bar.java"><line nr="1" ci="4" mi="0"/></sourcefile></package></report>"#;
        let compact = parse_jacoco(minified, repo.path(), &mut EntryBudget::new(usize::MAX));
        assert_eq!(compact["com/foo/Bar.java"].get(&1), Some(&4));

        let clover = r#"
<coverage>
  <file name="Foo.php" path="src/Foo.php">
    <line num="3" count="2" type="stmt"/>
    <line num="4" count="0" type="stmt"/>
  </file>
</coverage>
"#;
        let cmap = parse_clover(clover, repo.path(), &mut EntryBudget::new(usize::MAX));
        assert_eq!(cmap["src/Foo.php"].get(&3), Some(&2));
        assert_eq!(cmap["src/Foo.php"].get(&4), Some(&0));
    }

    #[test]
    fn parse_jacoco_extreme_counters_saturate_instead_of_panicking() {
        let repo = git_repo();
        write(repo.path(), "com/foo/Big.java", "class Big {}\n");
        let jacoco = r#"
<report>
  <package name="com/foo">
    <sourcefile name="Big.java">
      <line nr="1" mi="1" ci="18446744073709551615"/>
      <line nr="2" mi="18446744073709551615" ci="18446744073709551615"/>
    </sourcefile>
  </package>
</report>
"#;
        let map = parse_jacoco(jacoco, repo.path(), &mut EntryBudget::new(usize::MAX));
        // ci + mi overflows u64 on both lines; saturating math must treat
        // them as covered instead of panicking in debug builds. Saturation
        // now clamps at u32::MAX (record_hit) because anything above 2^53
        // corrupts over IPC into JS; u64::MAX must never reach the map.
        assert_eq!(map["com/foo/Big.java"].get(&1), Some(&(u32::MAX as u64)));
        assert_eq!(map["com/foo/Big.java"].get(&2), Some(&(u32::MAX as u64)));
    }

    #[test]
    fn relativize_rejects_null_and_parent() {
        let repo = git_repo();
        assert!(relativize(repo.path(), "../secret").is_none());
        assert!(relativize(repo.path(), "a\0b").is_none());
        assert_eq!(
            relativize(repo.path(), "./src/lib.rs").as_deref(),
            Some("src/lib.rs")
        );
    }

    #[test]
    fn language_info_used_for_file_summary() {
        let info: LanguageInfo = LanguageDetector::detect_from_path("src/main.rs");
        assert_eq!(info.name, "Rust");
        assert_eq!(LanguageDetector::coverage_family(info.name), Some("rust"));
    }

    #[test]
    fn cargo_toml_alone_seeds_rust_family() {
        let repo = git_repo();
        write(
            repo.path(),
            "Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).unwrap();
        let rust = report
            .families
            .iter()
            .find(|f| f.family == "rust")
            .expect("rust family from Cargo.toml");
        assert!(rust.languages.iter().any(|l| l == "Rust"));
        assert!(!rust.found);
        assert_eq!(
            rust.suggested_commands,
            vec!["cargo llvm-cov --workspace --lcov --output-path lcov.info".to_string()],
            "root Cargo.toml must not be planned as a nested workspace: {:?}",
            rust.suggested_commands
        );
        assert_rust_llvm_cov_plan(rust);
    }

    #[test]
    fn rust_extra_dirs_find_llvm_cov_under_target() {
        let repo = git_repo();
        write(repo.path(), "src/lib.rs", "pub fn x() {}\n");
        write(
            repo.path(),
            "target/llvm-cov/custom.info",
            "SF:src/lib.rs\nDA:1,1\nend_of_record\n",
        );
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
        assert!(
            report
                .families
                .iter()
                .any(|f| f.family == "rust" && f.found),
            "rust family: {:?}",
            report.families
        );
        assert!(
            report
                .artifacts
                .iter()
                .any(|a| a.path.contains("llvm-cov") && !a.skipped),
            "llvm-cov artifact: {:?}",
            report.artifacts
        );
        assert!(report.files.iter().any(|f| f.path == "src/lib.rs"));
    }

    #[test]
    fn single_quoted_xml_attributes_are_parsed() {
        let repo = git_repo();
        write(repo.path(), "pkg/mod.py", "def f():\n    return 1\n");
        let cob = r#"<coverage><class filename='pkg/mod.py'><line number='1' hits='2'/><line number='2' hits='0'/></class></coverage>"#;
        let map = parse_cobertura(cob, repo.path(), &mut EntryBudget::new(usize::MAX));
        assert_eq!(map["pkg/mod.py"].get(&1), Some(&2));
        assert_eq!(map["pkg/mod.py"].get(&2), Some(&0));
    }

    #[test]
    fn file_uri_percent_encoding_is_decoded() {
        let repo = git_repo();
        write(repo.path(), "src/héllo wörld.rs", "fn x() {}\n");
        let abs = repo.path().join("src/héllo wörld.rs");
        let uri = format!("file://{}", abs.to_string_lossy());
        assert_eq!(
            relativize(repo.path(), &uri).as_deref(),
            Some("src/héllo wörld.rs")
        );
    }

    #[test]
    fn file_coverage_rejects_directory_and_empty_paths() {
        let repo = git_repo();
        write(repo.path(), "src/lib.rs", "fn a() {}\n");
        let root = repo.path().to_str().unwrap();
        assert!(CoverageScanner::file_coverage(root, ".").is_err());
        assert!(CoverageScanner::file_coverage(root, "").is_err());
        assert!(CoverageScanner::file_coverage(root, "../outside").is_err());
    }

    #[test]
    fn total_entry_budget_truncates_report() {
        let repo = git_repo();
        write(repo.path(), "src/lib.rs", "fn a() {}\n");
        let mut text = String::new();
        for i in 1..=200usize {
            text.push_str(&format!("SF:src/gen/file_{i}.rs\nDA:1,1\nend_of_record\n"));
        }
        write(repo.path(), "lcov.info", &text);
        let limits = ScanLimits {
            max_total_entries: 50,
            ..ScanLimits::default()
        };
        let (report, merged) =
            CoverageScanner::scan_with_limits(repo.path().to_str().unwrap(), limits).unwrap();
        assert!(report.truncated);
        let total: usize = merged.values().map(|m| m.len()).sum();
        assert!(total <= 50, "merged entries {total} exceeded budget");
    }

    /// Regression: Tauri apps keep their cargo workspace in a subdirectory
    /// (`src-tauri/`), so `cargo llvm-cov` writes under `<ws>/target/llvm-cov/`,
    /// not `<repo>/target/llvm-cov/`. Those artifacts must be discovered and
    /// their source paths mapped back to repo-relative form.
    #[test]
    fn nested_cargo_workspace_artifacts_are_discovered() {
        let repo = git_repo();
        write(
            repo.path(),
            "src-tauri/Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write(repo.path(), "src-tauri/src/main.rs", "fn main() {}\n");
        write(
            repo.path(),
            "src/app.ts",
            "export function add(a: number, b: number): number {\n  return a + b;\n}\n",
        );
        write(
            repo.path(),
            "coverage/lcov.info",
            "TN:\nSF:src/app.ts\nDA:1,1\nDA:2,1\nDA:3,0\nend_of_record\n",
        );
        write(
            repo.path(),
            "src-tauri/target/llvm-cov/lcov.info",
            "TN:\nSF:src-tauri/src/main.rs\nDA:1,1\nend_of_record\n",
        );
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");

        let rust_file = report
            .files
            .iter()
            .find(|f| f.path == "src-tauri/src/main.rs")
            .expect("nested workspace rust file must appear");
        assert_eq!(rust_file.language, "Rust");
        assert_eq!(rust_file.lines_hit, 1);
        assert!(
            report
                .artifacts
                .iter()
                .any(|a| a.path == "src-tauri/target/llvm-cov/lcov.info" && !a.skipped),
            "nested artifact row missing: {:?}",
            report.artifacts
        );
    }

    /// Regression: `coverage/lcov.info` sits in both the javascript and rust
    /// spec tables. A JS-only artifact must not flip the rust family chip to
    /// "report found" — found means *this family's* languages have data.
    #[test]
    fn family_found_requires_data_for_its_own_languages() {
        let repo = git_repo();
        write(repo.path(), "src/app.ts", "export const x: number = 1;\n");
        // Seeds the rust family without producing any Rust coverage data.
        write(
            repo.path(),
            "Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write(
            repo.path(),
            "coverage/lcov.info",
            "TN:\nSF:src/app.ts\nDA:1,1\nend_of_record\n",
        );
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");

        let js = report
            .families
            .iter()
            .find(|f| f.family == "javascript")
            .unwrap();
        let rust = report.families.iter().find(|f| f.family == "rust").unwrap();
        assert!(
            js.found,
            "javascript has its own data: {:?}",
            report.families
        );
        assert!(
            !rust.found,
            "rust must stay 'no report' when only JS files are covered: {:?}",
            report.families
        );
    }

    fn assert_argv_safe(commands: &[String]) {
        for command in commands {
            assert!(
                command_line_is_argv_safe(command),
                "planned command must be no-shell argv: {command}"
            );
        }
    }

    fn assert_rust_llvm_cov_plan(rust: &CoverageFamilyStatus) {
        assert!(
            rust.duration_hint.contains("several minutes"),
            "rust duration must be honest: {:?}",
            rust.duration_hint
        );
        if rust.tool_ready {
            assert!(
                rust.setup_commands.is_empty(),
                "a ready toolchain must not plan setup: {:?}",
                rust.setup_commands
            );
            assert!(
                rust.tool_detail.is_empty(),
                "ready toolchain detail must be empty: {:?}",
                rust.tool_detail
            );
        } else {
            assert_eq!(
                rust.setup_commands,
                vec![
                    "rustup component add llvm-tools-preview".to_string(),
                    "cargo install cargo-llvm-cov --locked".to_string(),
                ]
            );
            assert!(
                rust.tool_detail.contains("cargo-llvm-cov"),
                "missing tool must be named: {:?}",
                rust.tool_detail
            );
        }
        assert_argv_safe(&rust.suggested_commands);
        assert_argv_safe(&rust.setup_commands);
    }

    #[test]
    fn rust_plan_injects_setup_when_llvm_cov_is_missing() {
        let plan = rust_coverage_plan(Path::new("."), &[], false);
        assert!(!plan.tool_ready);
        assert_eq!(
            plan.setup,
            vec![
                "rustup component add llvm-tools-preview".to_string(),
                "cargo install cargo-llvm-cov --locked".to_string(),
            ]
        );
        assert!(plan.tool_detail.contains("cargo-llvm-cov"));
        assert!(plan.duration_hint.contains("several minutes"));
        assert!(!plan.generate.is_empty());
        assert_argv_safe(&plan.generate);
        assert_argv_safe(&plan.setup);
    }

    #[test]
    fn rust_plan_skips_setup_when_llvm_cov_is_ready() {
        let plan = rust_coverage_plan(Path::new("."), &[], true);
        assert!(plan.tool_ready);
        assert!(plan.setup.is_empty());
        assert!(plan.tool_detail.is_empty());
        assert!(plan.duration_hint.contains("several minutes"));
        assert!(!plan.generate.is_empty());
    }

    /// Replaces `python_plan_is_unavailable_when_pytest_is_missing`, which
    /// asserted the dead end: no generate command, no setup, nothing to press.
    /// pytest is installable, so a missing pytest is a plan, not a verdict.
    /// A virtualenv GitPulse will not execute must not silently become a
    /// fallback to the host interpreter, and must not be planned around
    /// either — that would emit steps the command gate is certain to refuse.
    #[cfg(unix)]
    #[test]
    fn untrusted_project_venv_is_refused_with_its_reason() {
        use std::os::unix::fs::symlink;
        let repo = git_repo();
        write(repo.path(), "app/main.py", "print(1)\n");
        write(repo.path(), ".venv/pyvenv.cfg", "home = /usr/bin\n");
        std::fs::create_dir_all(repo.path().join(".venv/bin")).unwrap();
        symlink("/bin/sh", repo.path().join(".venv/bin/python")).unwrap();

        let plan = python_coverage_plan(repo.path());
        assert!(plan.generate.is_empty(), "no step may be planned around it");
        assert!(plan.setup.is_empty());
        assert!(!plan.tool_ready);
        assert!(
            plan.tool_detail.contains("virtualenv"),
            "the reason must name the virtualenv: {:?}",
            plan.tool_detail
        );
    }

    /// Regression: a project that ships `mvnw` ships its own Maven precisely
    /// so nobody needs a system one, but the plan required `mvn` on PATH and
    /// reported "mvn is not installed" for exactly those repositories.
    #[test]
    fn maven_wrapper_is_used_instead_of_requiring_a_system_maven() {
        let repo = git_repo();
        write(repo.path(), "src/Main.java", "class Main {}\n");
        write(repo.path(), "pom.xml", "<project></project>\n");
        write(repo.path(), "mvnw", "#!/bin/sh\nexit 0\n");

        // `false` = no system maven, the case that used to be a dead end.
        let plan = jvm_coverage_plan(repo.path(), false);
        assert_eq!(plan.generate, vec!["./mvnw verify".to_string()]);
        assert!(plan.tool_ready);
    }

    #[test]
    fn maven_without_wrapper_or_system_maven_says_both_are_missing() {
        let repo = git_repo();
        write(repo.path(), "src/Main.java", "class Main {}\n");
        write(repo.path(), "pom.xml", "<project></project>\n");

        let plan = jvm_coverage_plan(repo.path(), false);
        assert!(plan.generate.is_empty());
        assert!(!plan.tool_ready);
        assert!(
            plan.tool_detail.contains("mvnw"),
            "the wrapper must be named as the alternative: {:?}",
            plan.tool_detail
        );
    }

    /// Regression, found by running the installed app: readiness was decided
    /// by spawning the virtualenv interpreter. That took 0.1s from a shell and
    /// never returned in a Finder-launched bundle, wedging the whole scan on
    /// any repository with a virtualenv. Readiness must be a filesystem
    /// question.
    #[test]
    fn pytest_readiness_is_decided_without_running_anything() {
        let repo = git_repo();
        std::fs::create_dir_all(repo.path().join(".venv/bin")).unwrap();
        assert!(
            !interpreter_has_pytest(repo.path(), ".venv/bin/python"),
            "an empty virtualenv has no pytest"
        );
        // The console script `pip install pytest` writes.
        write(repo.path(), ".venv/bin/pytest", "#!/bin/sh\n");
        assert!(
            interpreter_has_pytest(repo.path(), ".venv/bin/python"),
            "the installed console script is the readiness signal"
        );
    }

    /// The probe must stay inside the repository and never panic on a
    /// malformed interpreter path.
    #[test]
    fn pytest_readiness_probe_is_total_on_hostile_paths() {
        let repo = git_repo();
        for rel in [
            "",
            "python",
            "../outside/bin/python",
            "/abs/bin/python",
            ".venv/bin/",
            "a/b/c/d/e/python",
        ] {
            let _ = interpreter_has_pytest(repo.path(), rel);
        }
    }

    #[test]
    fn python_plan_installs_pytest_into_an_existing_project_venv() {
        let repo = git_repo();
        write(repo.path(), "app/main.py", "print(1)\n");
        // A real virtualenv: it must pass the interpreter trust rule (a stub
        // file does not), and pytest genuinely is not in it yet.
        let built = std::process::Command::new("python3")
            .args(["-m", "venv", ".venv"])
            .current_dir(repo.path())
            .status();
        match built {
            Ok(status) if status.success() => {}
            _ => return, // no usable python3 on this host
        }

        let plan = python_coverage_plan(repo.path());
        assert!(!plan.tool_ready, "a venv without pytest is not ready");
        assert_eq!(
            plan.generate,
            vec![".venv/bin/python -m pytest --cov --cov-report=xml".to_string()],
            "the generate step must run the project interpreter, not a host one"
        );
        assert_eq!(
            plan.setup,
            vec![".venv/bin/python -m pip install pytest pytest-cov".to_string()],
            "installation must target the project virtualenv only"
        );
        assert!(plan.tool_detail.contains("pytest"));
        assert!(!plan.duration_hint.is_empty());
    }

    /// With no virtualenv, GitPulse creates one rather than touching the host
    /// interpreter. Asserted as an invariant rather than an exact command list
    /// because a host that already has pytest legitimately skips the venv.
    #[test]
    fn python_plan_is_never_a_dead_end_when_python_exists() {
        let repo = git_repo();
        write(repo.path(), "app/main.py", "print(1)\n");
        let plan = python_coverage_plan(repo.path());
        if plan.tool_detail.contains("No Python interpreter") {
            // No interpreter to build a venv with: an honest refusal, not a
            // silent one.
            assert!(plan.generate.is_empty());
            assert!(!plan.tool_ready);
            return;
        }
        assert!(
            !plan.generate.is_empty(),
            "a repository with Python sources must always get a generate command"
        );
        if !plan.tool_ready {
            assert!(
                !plan.setup.is_empty(),
                "not-ready must mean 'here are the steps', never 'give up'"
            );
            assert!(
                plan.setup.iter().any(|cmd| cmd.contains("pip install")),
                "the plan must actually install pytest: {:?}",
                plan.setup
            );
            assert!(
                plan.setup
                    .iter()
                    .all(|cmd| !cmd.contains("--user") && !cmd.contains("--break-system-packages")),
                "the host interpreter must never be mutated: {:?}",
                plan.setup
            );
        }
    }

    /// The contract every family now owes the user: if it cannot generate
    /// coverage yet, it either offers the steps that would fix that or states
    /// why no step exists. Silence is not an option.
    #[test]
    fn no_family_is_left_without_steps_or_a_reason() {
        let repo = git_repo();
        write(repo.path(), "app/main.py", "print(1)\n");
        write(repo.path(), "src/lib.rs", "pub fn a() {}\n");
        write(repo.path(), "src/main.go", "package main\n");
        write(repo.path(), "src/engine.c", "int main(void){return 0;}\n");
        write(repo.path(), "app/index.php", "<?php echo 1;\n");
        write(repo.path(), "app/main.rb", "puts 1\n");
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
        assert!(report.families.len() >= 5);
        for family in &report.families {
            if family.found {
                continue;
            }
            let actionable = !family.suggested_commands.is_empty();
            assert!(
                actionable || !family.tool_detail.is_empty(),
                "{} offers neither a command nor a reason",
                family.family
            );
            if !family.tool_ready && actionable {
                assert!(
                    !family.setup_commands.is_empty(),
                    "{} says its toolchain is missing but offers no way to install it",
                    family.family
                );
            }
        }
    }

    #[test]
    fn go_plan_is_unavailable_without_a_module() {
        let plan = go_coverage_plan(Path::new("."), &[], false);
        assert!(!plan.tool_ready);
        assert!(plan.generate.is_empty());
        assert!(plan.tool_detail.contains("go.mod"));
    }

    #[test]
    fn swift_plan_requires_package_manifest() {
        let plan = swift_coverage_plan(false, true);
        assert!(plan.generate.is_empty());
        assert!(plan.tool_detail.contains("Package.swift"));
        let ready = swift_coverage_plan(true, true);
        assert_eq!(
            ready.generate,
            vec!["swift test --enable-code-coverage".to_string()]
        );
        assert!(ready.tool_ready);
    }

    /// GitPulse itself is a Tauri tree: cargo lives under `src-tauri/`, and
    /// `package.json` has a `coverage` script. The chips next to "no report"
    /// must offer those commands, not a root `cargo llvm-cov` (no Cargo.toml)
    /// or a stray `npx jest`.
    #[test]
    fn tauri_layout_plans_manifest_path_llvm_cov_and_npm_coverage() {
        let repo = git_repo();
        write(
            repo.path(),
            "src-tauri/Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write(repo.path(), "src-tauri/src/lib.rs", "pub fn x() {}\n");
        write(repo.path(), "src/app.ts", "export const x = 1;\n");
        write(
            repo.path(),
            "package.json",
            r#"{"name":"app","scripts":{"coverage":"vitest run --coverage"},"devDependencies":{"vitest":"2.0.0","@vitest/coverage-v8":"2.0.0"}}"#,
        );
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");

        let rust = report
            .families
            .iter()
            .find(|f| f.family == "rust")
            .expect("rust family");
        assert!(!rust.found, "no rust artifact was written");
        assert_eq!(
            rust.suggested_commands,
            vec![
                "cargo llvm-cov --manifest-path src-tauri/Cargo.toml --workspace --lcov --output-path src-tauri/lcov.info"
                    .to_string()
            ]
        );
        assert_rust_llvm_cov_plan(rust);

        let js = report
            .families
            .iter()
            .find(|f| f.family == "javascript")
            .expect("javascript family");
        assert_eq!(js.suggested_commands, vec!["npm run coverage".to_string()]);
        assert!(js.tool_ready);
        assert!(js.setup_commands.is_empty());
        assert!(
            js.duration_hint.contains("minute"),
            "js duration must be honest: {:?}",
            js.duration_hint
        );
        assert!(
            !js.suggested_commands.iter().any(|c| c.contains("jest")),
            "a coverage script must win over jest: {:?}",
            js.suggested_commands
        );
        assert_argv_safe(&js.suggested_commands);
    }

    #[test]
    fn javascript_without_coverage_script_prefers_declared_runner() {
        let repo = git_repo();
        write(repo.path(), "src/app.ts", "export const x = 1;\n");
        write(
            repo.path(),
            "package.json",
            r#"{"name":"app","devDependencies":{"vitest":"2.0.0","@vitest/coverage-v8":"2.0.0"}}"#,
        );
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
        let js = report
            .families
            .iter()
            .find(|f| f.family == "javascript")
            .expect("javascript family");
        assert_eq!(
            js.suggested_commands,
            vec!["npx --no-install vitest run --coverage".to_string()]
        );
        assert!(
            !js.suggested_commands.iter().any(|c| c.contains("jest")),
            "jest must not be offered when the manifest has only vitest: {:?}",
            js.suggested_commands
        );
    }

    #[test]
    fn javascript_without_package_json_does_not_invent_npx_runners() {
        let repo = git_repo();
        write(repo.path(), "src/app.ts", "export const x = 1;\n");
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
        let js = report
            .families
            .iter()
            .find(|f| f.family == "javascript")
            .expect("javascript family");
        assert!(
            js.suggested_commands.is_empty(),
            "no package.json must not invent vitest/jest: {:?}",
            js.suggested_commands
        );
        assert!(!js.tool_ready);
        assert!(
            js.tool_detail.contains("package.json"),
            "missing runner must be named: {:?}",
            js.tool_detail
        );
    }

    #[test]
    /// Was `javascript_vitest_without_coverage_provider_is_not_planned`, which
    /// asserted that nothing at all was planned. The guardrail it protected —
    /// never run `vitest --coverage` against a checkout with no provider, which
    /// fails with "Cannot find dependency '@vitest/coverage-v8'" — is now
    /// enforced by ordering instead of by silence: the generate command exists
    /// but sits behind a setup step, and `tool_ready = false` keeps the UI from
    /// offering it as a standalone command chip.
    fn javascript_missing_coverage_provider_is_installed_before_running() {
        let repo = git_repo();
        write(repo.path(), "src/app.ts", "export const x = 1;\n");
        write(
            repo.path(),
            "package.json",
            r#"{"name":"app","devDependencies":{"vitest":"2.0.0"}}"#,
        );
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
        let js = report
            .families
            .iter()
            .find(|f| f.family == "javascript")
            .expect("javascript family");
        assert_eq!(
            js.setup_commands,
            vec!["npm install --save-dev @vitest/coverage-v8".to_string()],
            "the missing provider must be installable"
        );
        assert_eq!(
            js.suggested_commands,
            vec!["npx --no-install vitest run --coverage".to_string()],
        );
        assert!(
            !js.tool_ready,
            "not ready until setup runs — this is what stops the UI offering the bare command"
        );
        assert!(
            js.tool_detail.contains("@vitest/coverage-v8"),
            "missing provider must be named: {:?}",
            js.tool_detail
        );
    }

    /// A checkout with no runner at all is still not a missing package: adding
    /// vitest would leave the user with a runner and no tests.
    #[test]
    fn javascript_without_any_runner_is_not_offered_an_install() {
        let repo = git_repo();
        write(repo.path(), "src/app.ts", "export const x = 1;\n");
        write(repo.path(), "package.json", r#"{"name":"app"}"#);
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
        let js = report
            .families
            .iter()
            .find(|f| f.family == "javascript")
            .expect("javascript family");
        assert!(js.suggested_commands.is_empty());
        assert!(js.setup_commands.is_empty());
        assert!(!js.tool_ready);
        assert!(!js.tool_detail.is_empty());
    }

    #[test]
    fn javascript_test_coverage_script_beats_vitest_fallback() {
        let repo = git_repo();
        write(repo.path(), "src/app.ts", "export const x = 1;\n");
        write(
            repo.path(),
            "package.json",
            r#"{"name":"app","scripts":{"test:coverage":"vitest run --coverage","test:rust:coverage":"cargo llvm-cov"},"devDependencies":{"vitest":"2.0.0"}}"#,
        );
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
        let js = report
            .families
            .iter()
            .find(|f| f.family == "javascript")
            .expect("javascript family");
        assert_eq!(
            js.suggested_commands,
            vec!["npm run test:coverage".to_string()]
        );
        assert!(
            !js.suggested_commands
                .iter()
                .any(|c| c.contains("vitest") || c.contains("test:rust")),
            "declared test:coverage must win over vitest argv and rust scripts: {:?}",
            js.suggested_commands
        );
        assert_argv_safe(&js.suggested_commands);
    }

    #[test]
    fn javascript_coverage_script_still_wins_over_test_coverage() {
        let repo = git_repo();
        write(repo.path(), "src/app.ts", "export const x = 1;\n");
        write(
            repo.path(),
            "package.json",
            r#"{"name":"app","scripts":{"coverage":"vitest run --coverage","test:coverage":"vitest run --coverage --project frontend"},"devDependencies":{"vitest":"2.0.0"}}"#,
        );
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
        let js = report
            .families
            .iter()
            .find(|f| f.family == "javascript")
            .expect("javascript family");
        assert_eq!(js.suggested_commands, vec!["npm run coverage".to_string()]);
    }

    #[test]
    fn source_files_without_manifests_do_not_invent_generators() {
        let repo = git_repo();
        write(repo.path(), "pkg/mod.py", "def f():\n    return 1\n");
        write(repo.path(), "main.go", "package main\nfunc main() {}\n");
        write(repo.path(), "src/Main.java", "class Main {}\n");
        write(repo.path(), "src/lib.c", "int x;\n");
        write(repo.path(), "App.swift", "import Foundation\n");
        write(repo.path(), "lib/hi.dart", "void main() {}\n");
        write(repo.path(), "Program.cs", "class Program {}\n");
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");

        let python = report
            .families
            .iter()
            .find(|f| f.family == "python")
            .expect("python");
        // pytest runs from any directory, so unlike `go test ./...` there is no
        // manifest that must exist first — a command here is planned, not
        // invented. What must hold is that it targets a real interpreter and,
        // when the toolchain is missing, comes with the steps that supply it.
        if python.tool_ready {
            assert_eq!(
                python.suggested_commands,
                vec!["pytest --cov --cov-report=xml".to_string()]
            );
            assert!(python.setup_commands.is_empty());
            assert!(python.duration_hint.contains("few minutes"));
        } else if python.suggested_commands.is_empty() {
            // No interpreter at all: an honest refusal.
            assert!(python.tool_detail.contains("Python"));
        } else {
            assert_eq!(
                python.suggested_commands,
                vec![format!(
                    "{} -m pytest --cov --cov-report=xml",
                    managed_venv_python()
                )],
                "coverage must run the virtualenv interpreter GitPulse creates"
            );
            assert!(
                python.setup_commands.iter().any(|c| c.contains("venv")),
                "the virtualenv must be created first: {:?}",
                python.setup_commands
            );
            assert!(
                python
                    .setup_commands
                    .iter()
                    .any(|c| c.contains("pip install pytest")),
                "pytest must actually be installed: {:?}",
                python.setup_commands
            );
            assert!(python.tool_detail.contains("pytest"));
        }
        let go = report
            .families
            .iter()
            .find(|f| f.family == "go")
            .expect("go");
        assert!(
            go.suggested_commands.is_empty(),
            "bare .go files must not plan root ./...: {:?}",
            go.suggested_commands
        );
        assert!(!go.tool_ready);
        assert!(
            go.tool_detail.contains("go.mod"),
            "missing module must be named: {:?}",
            go.tool_detail
        );
        let jvm = report
            .families
            .iter()
            .find(|f| f.family == "jvm")
            .expect("jvm");
        assert!(
            jvm.suggested_commands.is_empty(),
            "java without wrapper/pom must not invent gradlew/mvn: {:?}",
            jvm.suggested_commands
        );
        assert!(!jvm.tool_ready);
        assert!(
            jvm.tool_detail.contains("pom.xml") || jvm.tool_detail.contains("Gradle"),
            "missing JVM manifest must be named: {:?}",
            jvm.tool_detail
        );
        let native = report
            .families
            .iter()
            .find(|f| f.family == "native")
            .expect("native");
        assert_eq!(native.suggested_commands, Vec::<String>::new());
        assert!(native.setup_commands.is_empty());
        assert!(native.duration_hint.is_empty());
        let swift = report
            .families
            .iter()
            .find(|f| f.family == "swift")
            .expect("swift");
        assert!(
            swift.suggested_commands.is_empty(),
            "swift without Package.swift must not plan swift test: {:?}",
            swift.suggested_commands
        );
        assert!(swift.tool_detail.contains("Package.swift"));
        let dart = report
            .families
            .iter()
            .find(|f| f.family == "dart")
            .expect("dart");
        assert!(
            dart.suggested_commands.is_empty(),
            "dart without pubspec.yaml must not plan dart test: {:?}",
            dart.suggested_commands
        );
        assert!(dart.tool_detail.contains("pubspec.yaml"));
        let dotnet = report
            .families
            .iter()
            .find(|f| f.family == "dotnet")
            .expect("dotnet");
        assert!(
            dotnet.suggested_commands.is_empty(),
            ".cs without a project file must not plan dotnet test: {:?}",
            dotnet.suggested_commands
        );
        for family in &report.families {
            assert_argv_safe(&family.suggested_commands);
            assert_argv_safe(&family.setup_commands);
        }
    }

    #[test]
    fn nested_go_mod_plans_chdir_coverprofile() {
        let repo = git_repo();
        write(
            repo.path(),
            "backend/go_orchestrator/go.mod",
            "module example.com/orch\n\ngo 1.22\n",
        );
        write(
            repo.path(),
            "backend/go_orchestrator/main.go",
            "package main\nfunc main() {}\n",
        );
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
        let go = report
            .families
            .iter()
            .find(|f| f.family == "go")
            .expect("go family");
        assert_eq!(
            go.suggested_commands,
            vec!["go -C backend/go_orchestrator test ./... -coverprofile=coverage.out".to_string()]
        );
        assert!(
            !go.suggested_commands
                .iter()
                .any(|c| c == "go test ./... -coverprofile=coverage.out"),
            "root ./... must not be planned when go.mod is nested: {:?}",
            go.suggested_commands
        );
        assert_argv_safe(&go.suggested_commands);
    }

    #[test]
    fn root_go_mod_keeps_root_cover_command() {
        let repo = git_repo();
        write(repo.path(), "go.mod", "module example.com/app\n\ngo 1.22\n");
        write(repo.path(), "main.go", "package main\nfunc main() {}\n");
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
        let go = report
            .families
            .iter()
            .find(|f| f.family == "go")
            .expect("go family");
        assert_eq!(
            go.suggested_commands,
            vec!["go test ./... -coverprofile=coverage.out".to_string()]
        );
    }

    #[test]
    fn go_work_at_root_uses_workspace_not_nested_chdir() {
        let repo = git_repo();
        write(
            repo.path(),
            "go.work",
            "go 1.22\n\nuse ./backend/go_orchestrator\n",
        );
        write(
            repo.path(),
            "backend/go_orchestrator/go.mod",
            "module example.com/orch\n\ngo 1.22\n",
        );
        write(
            repo.path(),
            "backend/go_orchestrator/main.go",
            "package main\nfunc main() {}\n",
        );
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
        let go = report
            .families
            .iter()
            .find(|f| f.family == "go")
            .expect("go family");
        assert_eq!(
            go.suggested_commands,
            vec!["go test ./... -coverprofile=coverage.out".to_string()]
        );
        assert!(
            !go.suggested_commands.iter().any(|c| c.contains("-C")),
            "workspace root must not emit per-module -C: {:?}",
            go.suggested_commands
        );
    }

    #[test]
    fn two_go_modules_plan_both_cover_commands() {
        let repo = git_repo();
        write(
            repo.path(),
            "api/go.mod",
            "module example.com/api\n\ngo 1.22\n",
        );
        write(repo.path(), "api/main.go", "package main\nfunc main() {}\n");
        write(
            repo.path(),
            "cli/go.mod",
            "module example.com/cli\n\ngo 1.22\n",
        );
        write(repo.path(), "cli/main.go", "package main\nfunc main() {}\n");
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
        let go = report
            .families
            .iter()
            .find(|f| f.family == "go")
            .expect("go family");
        assert_eq!(
            go.suggested_commands,
            vec![
                "go -C api test ./... -coverprofile=coverage.out".to_string(),
                "go -C cli test ./... -coverprofile=coverage.out".to_string(),
            ]
        );
        assert_argv_safe(&go.suggested_commands);
    }

    #[test]
    fn nested_go_cover_artifact_is_found() {
        let repo = git_repo();
        write(
            repo.path(),
            "backend/go_orchestrator/go.mod",
            "module example.com/orch\n\ngo 1.22\n",
        );
        write(
            repo.path(),
            "backend/go_orchestrator/main.go",
            "package main\nfunc main() {}\n",
        );
        write(
            repo.path(),
            "backend/go_orchestrator/coverage.out",
            "mode: set\nbackend/go_orchestrator/main.go:1.1,1.20 1 1\n",
        );
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
        assert!(
            report.families.iter().any(|f| f.family == "go" && f.found),
            "nested coverage.out must mark go found: {:?}",
            report.families
        );
        assert!(
            report
                .artifacts
                .iter()
                .any(|a| a.path == "backend/go_orchestrator/coverage.out" && !a.skipped),
            "nested go cover artifact: {:?}",
            report.artifacts
        );
        assert!(report
            .files
            .iter()
            .any(|f| f.path == "backend/go_orchestrator/main.go"));
    }

    #[test]
    fn jvm_with_gradlew_does_not_offer_maven() {
        let repo = git_repo();
        write(repo.path(), "src/Main.java", "class Main {}\n");
        write(repo.path(), "gradlew", "#!/bin/sh\n");
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
        let jvm = report
            .families
            .iter()
            .find(|f| f.family == "jvm")
            .expect("jvm");
        assert_eq!(
            jvm.suggested_commands,
            vec!["./gradlew test jacocoTestReport".to_string()]
        );
    }

    /// Vitest's json-summary output is totals-only (no line records). Parsing
    /// it as Istanbul JSON yields an empty-but-successful map and a bogus
    /// 0/0/0 artifact row; it must be ignored instead.
    #[test]
    fn coverage_summary_json_is_not_parsed_as_istanbul() {
        let repo = git_repo();
        write(repo.path(), "src/app.ts", "export const x: number = 1;\n");
        write(
            repo.path(),
            "coverage/lcov.info",
            "TN:\nSF:src/app.ts\nDA:1,1\nend_of_record\n",
        );
        write(
            repo.path(),
            "coverage/coverage-summary.json",
            r#"{"total":{"lines":{"total":10,"covered":5,"skipped":0,"pct":50}}}"#,
        );
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
        assert!(
            !report
                .artifacts
                .iter()
                .any(|a| a.path.ends_with("coverage-summary.json")),
            "summary file must not surface as an artifact: {:?}",
            report.artifacts
        );
        assert_eq!(report.files.len(), 1, "only lcov data should be present");
    }

    /// Language split: the report must aggregate per-language totals so the UI
    /// can show e.g. "Rust 80% · TypeScript 90%" instead of one blended number.
    #[test]
    fn report_splits_totals_by_language() {
        let repo = git_repo();
        write(repo.path(), "src/lib.rs", "fn a() {}\nfn b() {}\n");
        write(repo.path(), "src/app.ts", "export const x: number = 1;\n");
        write(
            repo.path(),
            "lcov.info",
            "TN:\nSF:src/lib.rs\nDA:1,1\nDA:2,0\nend_of_record\nTN:\nSF:src/app.ts\nDA:1,1\nend_of_record\n",
        );
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");

        let rust = report
            .languages
            .iter()
            .find(|l| l.language == "Rust")
            .expect("rust split entry");
        let ts = report
            .languages
            .iter()
            .find(|l| l.language == "TypeScript")
            .expect("typescript split entry");
        assert_eq!((rust.files, rust.lines_found, rust.lines_hit), (1, 2, 1));
        assert_eq!((ts.files, ts.lines_found, ts.lines_hit), (1, 1, 1));
        assert_eq!(rust.percentage, 50.0);
        assert_eq!(ts.percentage, 100.0);

        // Split must reconcile with the overall totals.
        let sum_found: usize = report.languages.iter().map(|l| l.lines_found).sum();
        let sum_hit: usize = report.languages.iter().map(|l| l.lines_hit).sum();
        assert_eq!(sum_found, report.overall.lines_found);
        assert_eq!(sum_hit, report.overall.lines_hit);
        assert_eq!(report.languages.len(), 2);
    }

    /// A parse that yields no records (empty file, totals-only JSON) must not
    /// surface as a successful 0/0/0 artifact — that reads as "0% covered".
    #[test]
    fn empty_parse_yields_skipped_artifact_row() {
        let repo = git_repo();
        write(repo.path(), "src/lib.rs", "fn a() {}\n");
        write(repo.path(), "coverage/custom.info", "");
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
        let row = report
            .artifacts
            .iter()
            .find(|a| a.path == "coverage/custom.info")
            .expect("row present for audit");
        assert!(row.skipped, "empty parse must be skipped: {row:?}");
        assert_eq!(
            row.skip_reason.as_deref(),
            Some("parsed but contained no coverage records")
        );
    }

    /// XML comments may legally contain tag-shaped text; a comment must not
    /// be able to forge coverage records.
    #[test]
    fn xml_comments_cannot_forge_records() {
        let repo = git_repo();
        write(repo.path(), "src/real.rs", "fn a() {}\nfn b() {}\n");
        let cob = "<coverage>\
                   <!-- <class filename=\"fake/forged.py\"><line number='1' hits='9'/></class> -->\
                   <class filename=\"src/real.rs\"><line number=\"1\" hits=\"1\"/></class>\
                   </coverage>";
        let map = parse_cobertura(cob, repo.path(), &mut EntryBudget::new(usize::MAX));
        assert!(!map.contains_key("fake/forged.py"), "{:?}", map.keys());
        assert_eq!(map.get("src/real.rs").and_then(|l| l.get(&1)), Some(&1));
    }

    /// `xml_attr` must match attribute names at whitespace boundaries so a
    /// decoy substring inside another attribute's VALUE cannot shadow the
    /// real attribute.
    #[test]
    fn xml_attr_decoy_value_does_not_shadow_real_attribute() {
        // No real filename attr: the decoy inside classname must NOT match.
        let tag = "class classname=\"filename=evil.rs\" name=\"pkg.py\"";
        assert_eq!(xml_attr(tag, "filename"), None);
        // Real filename present after the decoy: still found.
        let tag2 = "class classname=\"filename=evil.rs\" filename=\"good.rs\"";
        assert_eq!(xml_attr(tag2, "filename"), Some("good.rs"));
    }

    /// Filenames arrive entity-encoded in XML; `a&amp;b.ts` must resolve to
    /// the literal file `a&b.ts`.
    #[test]
    fn xml_entities_in_filenames_are_decoded() {
        let repo = git_repo();
        write(repo.path(), "src/a&b.ts", "export const x = 1;\n");
        let cob = "<coverage><class filename=\"src/a&amp;b.ts\">\
                   <line number=\"1\" hits=\"2\"/></class></coverage>";
        let map = parse_cobertura(cob, repo.path(), &mut EntryBudget::new(usize::MAX));
        assert!(
            map.contains_key("src/a&b.ts"),
            "entity-decoded path missing: {:?}",
            map.keys()
        );
        // Malformed entities stay verbatim rather than being dropped.
        assert_eq!(decode_xml_entities("x&nosuchy"), "x&nosuchy");
        assert_eq!(decode_xml_entities("&#65;&#x42;"), "AB");
    }

    /// Detail payloads are capped per file; totals still reflect every line.
    #[test]
    fn detail_lines_are_capped_with_flag() {
        let repo = git_repo();
        write(repo.path(), "src/big.rs", "fn a() {}\n");
        let mut text = String::from("SF:src/big.rs\n");
        for i in 1..=150_000usize {
            text.push_str(&format!("DA:{i},{}\n", i % 2));
        }
        text.push_str("end_of_record\n");
        write(repo.path(), "lcov.info", &text);
        let root = repo.path().to_str().unwrap();
        let detail = CoverageScanner::file_coverage(root, "src/big.rs").expect("detail");
        assert!(detail.lines_truncated);
        assert_eq!(detail.lines.len(), MAX_DETAIL_LINES);
        assert_eq!(detail.totals.lines_found, 150_000, "totals are uncapped");
        assert_eq!(detail.totals.lines_hit, 75_000);
        assert!(!detail.truncated, "scan itself was not truncated");
    }

    /// When the scan was truncated, a missing detail means UNKNOWN — the flag
    /// must say so instead of presenting absence as "uncovered".
    #[test]
    fn detail_flags_scan_truncation() {
        let _sequence = CACHE_SEQUENCE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let repo = git_repo();
        write(repo.path(), "src/a.rs", "fn a() {}\n");
        // Trip the DEFAULT max_files cap (4000) so file_coverage's internal
        // default-limits rescan observes truncation, like a real large repo.
        let mut text = String::new();
        for i in 0..4_001usize {
            let name = format!("src/gen/file_{i:05}.rs");
            write(repo.path(), &name, "fn x() {}\n");
            text.push_str(&format!("SF:{name}\nDA:1,{}\nend_of_record\n", i % 2));
        }
        write(repo.path(), "lcov.info", &text);
        let root = repo.path().to_str().unwrap();
        let (report, maps) =
            CoverageScanner::scan_with_limits(root, ScanLimits::default()).unwrap();
        assert!(report.truncated, "4001 files must trip the default cap");
        assert_eq!(maps.len(), 4_000);

        // Request a file that survived and one that was pruned; both must
        // carry the flag (survivor data may itself be budget-partial).
        let pruned = (0..4_001usize)
            .map(|i| format!("src/gen/file_{i:05}.rs"))
            .find(|p| !maps.contains_key(p))
            .expect("one file was pruned");
        let survivor = maps.keys().next().unwrap().clone();
        for probe in [pruned, survivor] {
            let detail = CoverageScanner::file_coverage(root, &probe).expect("detail");
            assert!(detail.truncated, "{probe} must carry the truncation flag");
        }
    }

    /// Distinct limit sets must not share cache entries even for an identical
    /// fingerprint: the caps shape the survivor set. Serialized behind
    /// CACHE_SEQUENCE_LOCK because parallel tests inserting repos can evict
    /// entries mid-sequence (bounded cache) and turn exact parse counts racy.
    #[test]
    fn cache_respects_limits_tag() {
        let _sequence = CACHE_SEQUENCE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _frozen = FreezeEvictionGuard::new();
        let repo = git_repo();
        write(repo.path(), "src/lib.rs", "fn a() {}\n");
        write(
            repo.path(),
            "lcov.info",
            "SF:src/lib.rs\nDA:1,1\nend_of_record\n",
        );
        let path = repo.path().to_str().unwrap();
        SCAN_PARSE_COUNT.with(|c| c.set(0));
        let first = CoverageScanner::scan_with_limits(path, ScanLimits::default()).unwrap();
        let after_first = SCAN_PARSE_COUNT.with(|c| c.get());
        assert!(after_first >= 1);
        // Same limits → served from cache.
        let cached = CoverageScanner::scan_with_limits(path, ScanLimits::default()).unwrap();
        assert_eq!(SCAN_PARSE_COUNT.with(|c| c.get()), after_first);
        assert_eq!(first.0.overall, cached.0.overall);
        // Different limits → fresh parse, fresh entry.
        let tight = ScanLimits {
            max_files: 10,
            ..ScanLimits::default()
        };
        let second = CoverageScanner::scan_with_limits(path, tight).unwrap();
        assert_eq!(
            SCAN_PARSE_COUNT.with(|c| c.get()),
            after_first + 1,
            "different limits must bypass the default-limits entry"
        );
        assert_eq!(first.0.overall, second.0.overall);
    }

    /// Coarse-mtime filesystems can serve identical (size, mtime) after a
    /// rewrite; the content probe must catch same-size content changes.
    #[test]
    fn fingerprint_content_probe_catches_same_size_rewrite() {
        let _sequence = CACHE_SEQUENCE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _frozen = FreezeEvictionGuard::new();
        let repo = git_repo();
        write(repo.path(), "src/lib.rs", "fn a() {}\n");
        let artifact = repo.path().join("lcov.info");
        std::fs::write(&artifact, "SF:src/lib.rs\nDA:1,1\nend_of_record\n").unwrap();
        let stamp_mtimes = |path: &Path| {
            // Pin both scans' view of mtime to a fixed coarse timestamp via
            // touch -t (macOS + GNU coreutils compatible).
            let status = Command::new("touch")
                .args(["-t", "202601010000"])
                .arg(path)
                .status()
                .expect("touch -t available on test platform");
            assert!(status.success());
        };
        stamp_mtimes(&artifact);

        let path = repo.path().to_str().unwrap();
        SCAN_PARSE_COUNT.with(|c| c.set(0));
        let first = CoverageScanner::scan(path).expect("scan one");
        let after_first = SCAN_PARSE_COUNT.with(|c| c.get());

        // Same byte length, different content, mtime pinned to the same value.
        std::fs::write(&artifact, "SF:src/lib.rs\nDA:1,0\nend_of_record\n").unwrap();
        assert_eq!(
            std::fs::metadata(&artifact).unwrap().len(),
            std::fs::metadata(repo.path().join("lcov.info"))
                .unwrap()
                .len()
        );
        stamp_mtimes(&artifact);

        let second = CoverageScanner::scan(path).expect("scan two");
        assert_eq!(
            SCAN_PARSE_COUNT.with(|c| c.get()),
            after_first + 1,
            "same-size same-mtime rewrite must invalidate via content probe"
        );
        assert_ne!(first.overall.lines_hit, second.overall.lines_hit);
    }

    /// CONTRACT CHANGE (audit M2): the old behavior flagged ANY probe
    /// directory holding more than MAX_DIR_ENTRIES entries, so generated-junk
    /// floods (Python `htmlcov/*.html`) permanently showed "scan capped" and
    /// taught users to ignore the chip. The new two-sided contract:
    ///
    /// 1. junk-only overflow (nothing artifact-shaped beyond the window)
    ///    must NOT flag;
    /// 2. an artifact-shaped name beyond the window must ALWAYS flag —
    ///    readdir order is arbitrary, so we cannot know which names fell
    ///    past the window; a directory whose every overflow entry is
    ///    artifact-shaped makes "at least one dropped artifact-shaped name"
    ///    deterministic.
    #[test]
    fn dir_listing_over_cap_flags_only_artifact_shaped_entries_beyond_window() {
        // Side 1: over-cap probe dir of pure junk → no truncation flag.
        let repo = git_repo();
        write(repo.path(), "src/lib.rs", "fn a() {}\n");
        write(
            repo.path(),
            "lcov.info",
            "SF:src/lib.rs\nDA:1,1\nend_of_record\n",
        );
        for i in 0..MAX_DIR_ENTRIES + 20 {
            write(repo.path(), &format!("coverage/junk_{i}.txt"), "noise\n");
        }
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
        assert!(
            !report.truncated,
            "junk beyond the window must not flag: {:?}",
            report
                .artifacts
                .iter()
                .map(|a| a.path.clone())
                .collect::<Vec<_>>()
        );
        assert!(report.files.iter().any(|f| f.path == "src/lib.rs"));

        // Side 2: over-cap probe dir where EVERY entry is artifact-shaped
        // (.info) → at least MAX_DIR_ENTRIES-window entries were dropped
        // unseen, so the flag must fire. max_artifacts is raised above the
        // candidate count so the artifact cap cannot fake this pass.
        let repo = git_repo();
        write(repo.path(), "src/lib.rs", "fn a() {}\n");
        write(
            repo.path(),
            "lcov.info",
            "SF:src/lib.rs\nDA:1,1\nend_of_record\n",
        );
        for i in 0..MAX_DIR_ENTRIES + 6 {
            write(
                repo.path(),
                &format!("coverage/part_{i:03}.info"),
                "SF:src/lib.rs\nDA:1,1\nend_of_record\n",
            );
        }
        let limits = ScanLimits {
            max_artifacts: MAX_DIR_ENTRIES * 4,
            ..ScanLimits::default()
        };
        let (report, _) =
            CoverageScanner::scan_with_limits(repo.path().to_str().unwrap(), limits).expect("scan");
        assert!(
            report.truncated,
            "artifact-shaped overflow must flag truncation"
        );
        assert!(report.files.iter().any(|f| f.path == "src/lib.rs"));
    }

    /// Percentage rounding contract at the boundaries callers render:
    /// one-decimal rounding, never 100% unless fully covered, and an empty
    /// denominator is 0.0 rather than NaN or 100.
    #[test]
    fn percentage_boundaries_round_to_one_decimal() {
        let cases: [((usize, usize), f64); 6] = [
            ((3, 2), 66.7),
            ((3, 1), 33.3),
            ((1000, 1), 0.1),
            ((10, 10), 100.0),
            ((8, 5), 62.5),
            ((0, 0), 0.0),
        ];
        for ((found, hit), expected) in cases {
            assert_eq!(
                CoverageTotals::from_counts(found, hit).percentage,
                expected,
                "{hit}/{found}"
            );
        }
    }

    /// Fuzz: every byte-prefix of a valid artifact parses without panicking
    /// and never exceeds its budget. Truncated XML/lcov/JSON mid-tag,
    /// mid-attribute, mid-entity — all must yield Ok or Err, never a hang or
    /// a panic.
    #[test]
    fn truncated_artifacts_never_panic_at_any_byte_boundary() {
        let repo = git_repo();
        write(repo.path(), "src/lib.rs", "fn a() {}\nfn b() {}\n");
        let corpora: Vec<(CoverageFormat, &str)> = vec![
            (
                CoverageFormat::Lcov,
                "TN:\nSF:src/lib.rs\nDA:1,1\nDA:2,0\nDA:2,3\ndd:end_of_record\nBRDA:1,0,0,1\nend_of_record\n",
            ),
            (
                CoverageFormat::Cobertura,
                "<coverage line-rate=\"0.5\"><!-- c -->\
                 <class filename=\"src/lib.rs\" complexity=\"4\">\
                 <line number=\"1\" hits=\"2\"/><line number=\"2\" hits=\"&amp;\"/></class></coverage>",
            ),
            (
                CoverageFormat::Istanbul,
                r#"{"/repo/src/lib.rs":{"path":"/repo/src/lib.rs","statementMap":{"0":[1,0,1,20]},"s":{"0":1},"lineMap":{"1":[1,0]}}}"#,
            ),
            (
                CoverageFormat::GoCover,
                "mode: set\nsrc/lib.rs:1.1,2.10 2 1\nsrc/lib.rs:3.5,3.9 0 0\n",
            ),
            (
                CoverageFormat::Jacoco,
                "<report><package name=\"src\"><sourcefile name=\"lib.rs\">\
                 <line nr=\"1\" mi=\"0\" ci=\"9\" mb=\"0\" cb=\"0\"/></sourcefile></package></report>",
            ),
            (
                CoverageFormat::Clover,
                "<coverage><file path=\"src/lib.rs\">\
                 <line type=\"stmt\" num=\"1\" count=\"3\"/></file></coverage>",
            ),
        ];
        for (format, body) in &corpora {
            let mut cut = 0usize;
            while cut <= body.len() {
                let prefix = &body[..cut];
                clear_existence_memo();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut budget = EntryBudget::new(10_000);
                    let mut go_capped = false;
                    parse_artifact(*format, prefix, repo.path(), &mut budget, &mut go_capped)
                }));
                assert!(
                    result.is_ok(),
                    "{format:?} panicked at byte {cut}: {prefix:?}"
                );
                cut += 7;
            }
        }
    }

    /// Deterministic pseudo-garbage corpus: hostile shapes that historically
    /// break parsers (entity bombs, nested comments, decoy attributes,
    /// negative/huge numerics, NULs late in the stream) must parse bounded.
    #[test]
    fn garbage_corpus_parses_bounded_without_panicking() {
        let repo = git_repo();
        write(repo.path(), "src/lib.rs", "fn a() {}\n");
        let cases: [(&str, String); 6] = [
            // Billion-laughs-style entity declarations (no expander: inert).
            ("cob_entity_bomb", "<!DOCTYPE x [<!ENTITY a '&b;'>]><class filename=\"&a;\">".into()),
            // Nested comment openers.
            ("nested_comments", "<!-- <!-- <!-- <class filename=\"x.py\"><line number='1' hits='1'/> --> --> -->".into()),
            // Decoy attribute shadow attempts.
            ("attr_shadow", "<class classname=\"filename=../evil\" filename=\"src/lib.rs\"><line number=\"1\" hits=\"1\"/></class>".into()),
            // Numeric extremes.
            ("numeric_extremes", "SF:src/lib.rs\nDA:-5,99999999999999999999\nDA:999999999,-1\nDA:2000001,1\nend_of_record\n".into()),
            // Late NUL (binary detection).
            ("late_nul", format!("SF:src/lib.rs\nDA:1,1\nend_of_record\n{}", "\0")),
            // Unterminated everything.
            ("unterminated", "<coverage><class filename=\"src/lib.rs\"><line number=\"1\" hits=\"".into()),
        ];
        for (name, body) in cases {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                // Route through every parser; each gets a fresh budget so a
                // poisoned one can't mask unbounded behavior downstream.
                let _ = parse_lcov(&body, repo.path(), &mut EntryBudget::new(50_000));
                let _ = parse_cobertura(
                    &strip_xml_comments(&decode_xml_entities(&body)),
                    repo.path(),
                    &mut EntryBudget::new(50_000),
                );
                let _ = parse_jacoco(
                    &strip_xml_comments(&body),
                    repo.path(),
                    &mut EntryBudget::new(50_000),
                );
                let _ = parse_clover(
                    &strip_xml_comments(&body),
                    repo.path(),
                    &mut EntryBudget::new(50_000),
                );
                let _ = parse_go_cover(
                    &body,
                    repo.path(),
                    &mut EntryBudget::new(50_000),
                    &mut false,
                );
            }));
            assert!(result.is_ok(), "{name} panicked parser pipeline");
        }
    }

    /// ─── Planner ⇄ gate contract ──────────────────────────────────────────
    ///
    /// The planner publishes command text; the gate decides what may be
    /// spawned. Nothing structural forces them to agree, and when they have
    /// disagreed the symptom was not an error but a *dead button*: a command
    /// offered in the UI and refused the instant it was pressed. It shipped
    /// that way twice — `vendor/bin/phpunit` refused as an executable path,
    /// and `bundle install` refused as outside the allowlist — because each
    /// side was internally consistent and nothing compared them.
    ///
    /// These tests are that comparison. They build a fixture repository per
    /// ecosystem, run the real planner over it, and assert the gate accepts
    /// every setup and generate command the planner published.
    mod plan_gate_contract {
        use super::*;
        use crate::terminal::{validate_manvi_action, ManviActionKind};

        /// argv split for planner-emitted command text.
        ///
        /// The frontend's `tokenizeCommand` is the real tokenizer and lives in
        /// TypeScript; reimplementing it here in full would be a third copy of
        /// exactly the kind this module exists to remove. Instead this handles
        /// only the subset the planner can emit, and
        /// [`planned_command_is_unambiguous`] proves the planner stays inside
        /// that subset — where a whitespace-and-double-quote split and the
        /// real tokenizer cannot disagree.
        fn split_planned(command: &str) -> Vec<String> {
            let mut argv = Vec::new();
            let mut current = String::new();
            let mut has_token = false;
            let mut quoted = false;
            for ch in command.chars() {
                match ch {
                    '"' => {
                        quoted = !quoted;
                        has_token = true;
                    }
                    c if c.is_whitespace() && !quoted => {
                        if has_token {
                            argv.push(std::mem::take(&mut current));
                            has_token = false;
                        }
                    }
                    c => {
                        current.push(c);
                        has_token = true;
                    }
                }
            }
            if has_token {
                argv.push(current);
            }
            argv
        }

        /// The planner may only emit command text whose tokenization is
        /// unambiguous: no shell metacharacters (already an invariant), and no
        /// single quotes or backslashes, so the only quoting in play is a
        /// balanced pair of double quotes. Inside that subset the splitter
        /// above and the frontend tokenizer agree by construction.
        fn planned_command_is_unambiguous(command: &str) -> Result<(), String> {
            if !command_line_is_argv_safe(command) {
                return Err("contains a shell metacharacter".into());
            }
            if command.contains('\'') {
                return Err("contains a single quote".into());
            }
            if command.contains('\\') {
                return Err("contains a backslash escape".into());
            }
            if !command.matches('"').count().is_multiple_of(2) {
                return Err("has an unbalanced double quote".into());
            }
            Ok(())
        }

        /// Runs the real scanner and asserts every command it published for
        /// `family` is one the gate will actually run.
        ///
        /// Returns the number of commands checked so a caller can prove the
        /// fixture exercised something — a planner that published nothing
        /// would otherwise pass this vacuously.
        fn assert_published_commands_are_runnable(repo: &TempDir, family: &str) -> usize {
            let path = repo.path().to_str().expect("utf-8 repo path");
            let report = CoverageScanner::scan(path).expect("scan");
            let status = report
                .families
                .iter()
                .find(|f| f.family == family)
                .unwrap_or_else(|| {
                    panic!(
                        "fixture did not seed the {family} family; got {:?}",
                        report
                            .families
                            .iter()
                            .map(|f| &f.family)
                            .collect::<Vec<_>>()
                    )
                });

            let mut checked = 0;
            for (kind, command) in status
                .setup_commands
                .iter()
                .map(|c| ("setup", c))
                .chain(status.suggested_commands.iter().map(|c| ("generate", c)))
            {
                planned_command_is_unambiguous(command)
                    .unwrap_or_else(|why| panic!("{family} {kind} command {command:?} {why}"));
                let argv = split_planned(command);
                assert!(
                    !argv.is_empty(),
                    "{family} {kind} command {command:?} tokenized to nothing"
                );
                validate_manvi_action(&argv, ManviActionKind::CoverageGenerator).unwrap_or_else(
                    |err| {
                        panic!(
                            "the planner published a {family} {kind} command the gate refuses.\n  \
                             command: {command}\n  argv:    {argv:?}\n  refusal: {err}"
                        )
                    },
                );
                checked += 1;
            }
            checked
        }

        /// A family whose plan is a dead end must publish no commands at all —
        /// `apply_language_plan` enforces that — so the contract above is
        /// vacuous for it. Assert the dead end instead, which is the other
        /// half of the same honesty rule: no command, and a stated reason.
        fn assert_dead_end_is_explained(repo: &TempDir, family: &str) {
            let path = repo.path().to_str().expect("utf-8 repo path");
            let report = CoverageScanner::scan(path).expect("scan");
            let Some(status) = report.families.iter().find(|f| f.family == family) else {
                return;
            };
            if !status.suggested_commands.is_empty() {
                return;
            }
            assert!(
                !status.tool_ready,
                "{family} published no command but still claims the tool is ready"
            );
            assert!(
                !status.tool_detail.trim().is_empty(),
                "{family} published no command and no reason"
            );
        }

        /// Python is the case that motivated all of this: the plan installs
        /// into a project-local virtualenv, and the gate's `pip install`
        /// exception is the narrowest arm in the allowlist.
        #[test]
        fn python_plan_is_runnable() {
            let repo = git_repo();
            write(repo.path(), "app.py", "def add(a, b):\n    return a + b\n");
            write(
                repo.path(),
                "test_app.py",
                "def test_add():\n    assert True\n",
            );
            let checked = assert_published_commands_are_runnable(&repo, "python");
            assert!(checked > 0, "the python fixture published no commands");
        }

        /// The vendored PHPUnit step. This is one of the two commands that
        /// shipped refused: the gate rejected `vendor/bin/phpunit` outright as
        /// an executable path.
        #[test]
        fn php_plan_is_runnable() {
            let repo = git_repo();
            write(repo.path(), "src/App.php", "<?php\nclass App {}\n");
            write(repo.path(), "composer.json", r#"{"name":"acme/app"}"#);
            write(repo.path(), "vendor/bin/phpunit", "#!/bin/sh\nexit 0\n");
            let checked = assert_published_commands_are_runnable(&repo, "php");
            assert!(checked > 0, "the php fixture published no commands");
        }

        /// The other command that shipped refused: `bundle install` was
        /// outside the purpose-specific allowlist, so Ruby coverage could
        /// never have run.
        #[test]
        fn ruby_plan_is_runnable() {
            let repo = git_repo();
            write(repo.path(), "lib/app.rb", "class App\nend\n");
            write(repo.path(), "Gemfile", "source 'https://rubygems.org'\n");
            let checked = assert_published_commands_are_runnable(&repo, "ruby");
            if checked == 0 {
                // When bundler is not installed on the host (e.g. Linux CI runners),
                // the plan must explain the dead end rather than claim tool readiness.
                assert_dead_end_is_explained(&repo, "ruby");

                // Still assert that bundle install clears the gate, which is the invariant
                // this test was written to protect.
                let bundle_cmd = ["bundle".to_string(), "install".to_string()];
                validate_manvi_action(&bundle_cmd, ManviActionKind::CoverageGenerator)
                    .expect("bundle install must be accepted by the gate");
            }
        }

        /// The vitest provider install — the only other setup step that writes
        /// to the repository.
        #[test]
        fn javascript_provider_install_is_runnable() {
            let repo = git_repo();
            write(repo.path(), "src/index.ts", "export const one = 1;\n");
            write(
                repo.path(),
                "package.json",
                r#"{"name":"app","devDependencies":{"vitest":"2.0.0"}}"#,
            );
            let checked = assert_published_commands_are_runnable(&repo, "javascript");
            assert!(checked > 0, "the javascript fixture published no commands");
        }

        /// A Maven wrapper repository: the planner emits `./mvnw verify`,
        /// which the gate must accept as a repository-local program.
        #[test]
        fn jvm_wrapper_plan_is_runnable() {
            let repo = git_repo();
            write(repo.path(), "src/main/java/App.java", "class App {}\n");
            write(repo.path(), "pom.xml", "<project></project>\n");
            write(repo.path(), "mvnw", "#!/bin/sh\nexit 0\n");
            let checked = assert_published_commands_are_runnable(&repo, "jvm");
            assert!(checked > 0, "the jvm fixture published no commands");
        }

        /// Go modules, including the `-C <dir>` form for a nested module.
        #[test]
        fn go_plan_is_runnable() {
            let repo = git_repo();
            write(repo.path(), "svc/main.go", "package main\n");
            write(repo.path(), "svc/go.mod", "module svc\n\ngo 1.22\n");
            let checked = assert_published_commands_are_runnable(&repo, "go");
            assert!(checked > 0, "the go fixture published no commands");
        }

        /// C/C++ with a CMake build — the family the user's own report showed
        /// as an unexplained dead end.
        #[test]
        fn native_plan_is_runnable_or_explained() {
            // With a build system and without one: C/C++ has no planned
            // generator either way, and must say so rather than publish a
            // command. See `native_coverage_plan` for why.
            let cmake = git_repo();
            write(cmake.path(), "src/main.c", "int main(void) { return 0; }\n");
            write(cmake.path(), "CMakeLists.txt", "project(app)\n");
            assert_published_commands_are_runnable(&cmake, "native");
            assert_dead_end_is_explained(&cmake, "native");

            let bare = git_repo();
            write(bare.path(), "src/main.c", "int main(void) { return 0; }\n");
            assert_dead_end_is_explained(&bare, "native");
        }

        /// Families whose plan is gated on a host toolchain (`swift`,
        /// `dotnet`, `dart`, `rust`, `jvm`) never reach their `ready` arm on a
        /// machine that lacks the tool, so a scan-driven fixture silently
        /// skips exactly the commands worth checking. Drive the plan functions
        /// directly with their preconditions forced instead, so this holds on
        /// any host.
        /// The commands the frontend's automatic recovery builds must clear the
        /// same gate as a planned one. A recovery the backend refuses is a
        /// third dead button: an offer that cannot run.
        ///
        /// These shapes are pinned in `src/lib/coverage/recovery.ts` and
        /// asserted verbatim by `recovery.test.ts`. Both halves have to move
        /// together; either one drifting fails here or there.
        #[test]
        fn recovery_commands_are_runnable() {
            let repo = git_repo();
            write(repo.path(), "svc/go.mod", "module svc\n");
            write(repo.path(), "bench/stress_test.py", "import sys\n");
            let root = validate_repo(repo.path().to_str().expect("utf-8")).expect("repo");

            // Go: one `-C <module>` command per discovered module.
            let go: Vec<String> = vec![
                "go".into(),
                "-C".into(),
                "svc".into(),
                "test".into(),
                "./...".into(),
                "-coverprofile=coverage.out".into(),
            ];
            validate_manvi_action(&go, ManviActionKind::CoverageGenerator)
                .expect("the Go module recovery must be allowed");
            crate::terminal::validate_manvi_paths(&root, &go, ManviActionKind::CoverageGenerator)
                .expect("and its module directory must validate");

            // Python: the original command plus the exclusion.
            let pytest: Vec<String> = vec![
                "pytest".into(),
                "--cov".into(),
                "--cov-report=xml".into(),
                "--ignore=bench/stress_test.py".into(),
            ];
            validate_manvi_action(&pytest, ManviActionKind::CoverageGenerator)
                .expect("the pytest collection-abort recovery must be allowed");
            crate::terminal::validate_manvi_paths(
                &root,
                &pytest,
                ManviActionKind::CoverageGenerator,
            )
            .expect("and its exclusion path must validate");
        }

        #[test]
        fn every_toolchain_gated_plan_is_runnable() {
            let repo = git_repo();
            write(repo.path(), "Cargo.toml", "[package]\nname = \"app\"\n");
            write(repo.path(), "pom.xml", "<project></project>\n");
            write(repo.path(), "mvnw", "#!/bin/sh\nexit 0\n");
            write(repo.path(), "gradlew", "#!/bin/sh\nexit 0\n");
            write(repo.path(), "composer.json", r#"{"name":"acme/app"}"#);
            write(repo.path(), "vendor/bin/phpunit", "#!/bin/sh\nexit 0\n");
            write(repo.path(), "Gemfile", "source 'x'\n");
            write(repo.path(), "CMakeLists.txt", "project(app)\n");
            let root = repo.path();

            let plans: Vec<(&str, LanguageCoveragePlan)> = vec![
                ("swift", swift_coverage_plan(true, true)),
                ("dotnet", dotnet_coverage_plan(true, true)),
                ("dart", dart_coverage_plan(true, true)),
                ("rust", rust_coverage_plan(root, &["".to_string()], true)),
                (
                    "rust(setup)",
                    rust_coverage_plan(root, &["".to_string()], false),
                ),
                ("jvm", jvm_coverage_plan(root, true)),
                ("php", php_coverage_plan(root)),
                ("ruby", ruby_coverage_plan(root)),
                ("javascript", javascript_coverage_plan(root)),
                ("native", native_coverage_plan(root)),
            ];

            let mut checked = 0;
            for (label, plan) in plans {
                for (kind, command) in plan
                    .setup
                    .iter()
                    .map(|c| ("setup", c))
                    .chain(plan.generate.iter().map(|c| ("generate", c)))
                {
                    planned_command_is_unambiguous(command)
                        .unwrap_or_else(|why| panic!("{label} {kind} command {command:?} {why}"));
                    let argv = split_planned(command);
                    validate_manvi_action(&argv, ManviActionKind::CoverageGenerator)
                        .unwrap_or_else(|err| {
                            panic!(
                                "the planner published a {label} {kind} command the gate refuses.\n  \
                                 command: {command}\n  argv:    {argv:?}\n  refusal: {err}"
                            )
                        });
                    checked += 1;
                }
            }
            assert!(
                checked >= 8,
                "expected the forced-ready plans to publish commands; only {checked} were checked"
            );
        }

        /// Elixir/Erlang: GitPulse parses none of the formats the BEAM tooling
        /// emits, so the contract here is the dead-end half.
        #[test]
        fn beam_dead_end_is_explained() {
            let repo = git_repo();
            write(repo.path(), "lib/app.ex", "defmodule App do\nend\n");
            write(repo.path(), "mix.exs", "defmodule App.MixProject do\nend\n");
            assert_dead_end_is_explained(&repo, "beam");
        }

        /// Falsification for the splitter's precondition: a command carrying
        /// shell syntax must be caught here rather than silently mis-split.
        #[test]
        fn ambiguous_command_text_is_rejected_by_the_precondition() {
            for hostile in [
                "go test ./... | tee out",
                "python -m pytest 'quoted arg'",
                "npm install --save-dev pkg\\name",
                "dotnet test --collect:\"unbalanced",
            ] {
                assert!(
                    planned_command_is_unambiguous(hostile).is_err(),
                    "{hostile:?} must not pass the tokenization precondition"
                );
            }
            // And the shapes the planner really emits do pass.
            for planned in [
                "go -C \"my module\" test ./... -coverprofile=coverage.out",
                "dotnet test --collect:\"XPlat Code Coverage\"",
                ".venv/bin/python -m pip install pytest pytest-cov",
            ] {
                planned_command_is_unambiguous(planned)
                    .unwrap_or_else(|why| panic!("{planned:?} {why}"));
            }
            assert_eq!(
                split_planned("go -C \"my module\" test ./... -coverprofile=coverage.out"),
                vec![
                    "go",
                    "-C",
                    "my module",
                    "test",
                    "./...",
                    "-coverprofile=coverage.out"
                ]
            );
            assert_eq!(
                split_planned("dotnet test --collect:\"XPlat Code Coverage\""),
                vec!["dotnet", "test", "--collect:XPlat Code Coverage"]
            );
        }
    }
}
