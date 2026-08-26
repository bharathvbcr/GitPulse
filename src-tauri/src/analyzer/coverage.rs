//! Language-aware test-coverage scanning.
//!
//! GitPulse does not run tests. It finds coverage artifacts the opened
//! repository's languages actually produce, parses them, and folds hit maps
//! into one report. A Rust tree is not searched for JaCoCo; a Python tree is
//! not searched for `coverage.out`.

use crate::analyzer::language::LanguageDetector;
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoverageReport {
    pub families: Vec<CoverageFamilyStatus>,
    /// Per-language rollup of the merged file data (the language split).
    pub languages: Vec<CoverageLanguageSplit>,
    pub artifacts: Vec<CoverageArtifact>,
    pub files: Vec<FileCoverageSummary>,
    pub overall: CoverageTotals,
    pub truncated: bool,
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
        let detected = detect_families(&repo)?;
        let mut families = detected.families;
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
                },
                HashMap::new(),
            ));
        }

        let mut truncated = detected.listing_partial;
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
                &detected.cargo_dirs,
                &detected.go_mod_dirs,
                detected.go_work_at_root,
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

        for (considered, cand) in present.into_iter().enumerate() {
            if considered >= limits.max_artifacts {
                truncated = true;
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
            &detected.cargo_dirs,
            &detected.go_mod_dirs,
            detected.go_work_at_root,
        );

        let report = CoverageReport {
            families: family_list,
            languages,
            artifacts,
            files,
            overall,
            truncated,
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
/// multi-megabyte manifest is skipped and we fall back to generic runners.
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

fn javascript_coverage_commands(repo: &Path) -> Vec<String> {
    let mut commands = Vec::new();
    if let Some(pkg) = read_root_package_json(repo) {
        // Prefer a declared coverage script over a generic vitest/jest argv.
        // `coverage` is the historical name; `test:coverage` is the npm
        // convention ScholarLM and others actually ship. Do not pick every
        // script whose name contains "coverage" (`test:rust:coverage` is not
        // a JavaScript generator).
        if package_script(&pkg, "coverage").is_some() {
            commands.push("npm run coverage".into());
        } else if package_script(&pkg, "test:coverage").is_some() {
            commands.push("npm run test:coverage".into());
        } else {
            if package_has_dep(&pkg, "vitest") || package_has_dep(&pkg, "@vitest/coverage-v8") {
                commands.push("npx --no-install vitest run --coverage".into());
            }
            if package_has_dep(&pkg, "jest") || package_has_dep(&pkg, "@jest/core") {
                commands.push("npx --no-install jest --coverage".into());
            }
        }
    }
    if commands.is_empty() {
        commands.push("npx --no-install vitest run --coverage".into());
        commands.push("npx --no-install jest --coverage".into());
    }
    commands
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

fn python_coverage_commands() -> Vec<String> {
    vec!["pytest --cov --cov-report=xml".into()]
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
        // Listing missed every go.mod (ignored, or only `.go` files). A root
        // go.work still lets `./...` resolve; otherwise fall back to CWD.
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
    if commands.is_empty() {
        commands.push("go test ./... -coverprofile=coverage.out".into());
    }
    commands
}

fn jvm_coverage_commands(repo: &Path) -> Vec<String> {
    let mut commands = Vec::new();
    if manifest_is_file(repo, "gradlew") || manifest_is_file(repo, "gradlew.bat") {
        commands.push("./gradlew test jacocoTestReport".into());
    }
    if manifest_is_file(repo, "pom.xml") {
        commands.push("mvn verify".into());
    }
    if commands.is_empty() {
        commands.push("./gradlew test jacocoTestReport".into());
        commands.push("mvn verify".into());
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

fn javascript_coverage_plan(repo: &Path) -> LanguageCoveragePlan {
    LanguageCoveragePlan::ready(
        javascript_coverage_commands(repo),
        "Frontend coverage usually finishes in about a minute.",
    )
}

fn python_coverage_plan() -> LanguageCoveragePlan {
    LanguageCoveragePlan::ready(
        python_coverage_commands(),
        "Python coverage can take a few minutes.",
    )
}

fn go_coverage_plan(
    repo: &Path,
    go_mod_dirs: &[String],
    go_work_at_root: bool,
) -> LanguageCoveragePlan {
    LanguageCoveragePlan::ready(
        go_coverage_commands(repo, go_mod_dirs, go_work_at_root),
        "Go coverage can take a few minutes.",
    )
}

fn jvm_coverage_plan(repo: &Path) -> LanguageCoveragePlan {
    LanguageCoveragePlan::ready(
        jvm_coverage_commands(repo),
        "JVM coverage can take a few minutes.",
    )
}

fn native_coverage_commands(repo: &Path) -> Vec<String> {
    let mut commands = Vec::new();
    if manifest_is_file(repo, "CMakeLists.txt") {
        commands.push("ctest --output-on-failure".into());
    }
    if manifest_is_file(repo, "Makefile") || manifest_is_file(repo, "GNUmakefile") {
        commands.push("make test".into());
    }
    commands
}

fn native_coverage_plan(repo: &Path) -> LanguageCoveragePlan {
    LanguageCoveragePlan::ready(
        native_coverage_commands(repo),
        "Native C/C++ coverage usually finishes in a few minutes.",
    )
}

fn swift_coverage_commands() -> Vec<String> {
    vec!["swift test --enable-code-coverage".into()]
}

fn swift_coverage_plan() -> LanguageCoveragePlan {
    LanguageCoveragePlan::ready(
        swift_coverage_commands(),
        "Swift test coverage usually finishes in a few minutes.",
    )
}

fn dotnet_coverage_commands() -> Vec<String> {
    vec!["dotnet test --collect:\"XPlat Code Coverage\"".into()]
}

fn dotnet_coverage_plan() -> LanguageCoveragePlan {
    LanguageCoveragePlan::ready(
        dotnet_coverage_commands(),
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
    LanguageCoveragePlan::ready(
        php_coverage_commands(repo),
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
    LanguageCoveragePlan::ready(
        ruby_coverage_commands(repo),
        "Ruby test coverage usually finishes in about a minute.",
    )
}

fn dart_coverage_commands() -> Vec<String> {
    vec!["dart test --coverage=coverage".into()]
}

fn dart_coverage_plan() -> LanguageCoveragePlan {
    LanguageCoveragePlan::ready(
        dart_coverage_commands(),
        "Dart test coverage usually finishes in about a minute.",
    )
}

fn apply_language_plan(status: &mut CoverageFamilyStatus, plan: LanguageCoveragePlan) {
    status.suggested_commands = plan.generate;
    status.setup_commands = plan.setup;
    status.tool_ready = plan.tool_ready;
    status.tool_detail = plan.tool_detail;
    status.duration_hint = plan.duration_hint;
}

/// Fills generate/setup commands from the checkout's actual manifests and a
/// live toolchain probe so the UI never offers `cargo llvm-cov` at a repo
/// root that has no Cargo.toml, `go test ./...` at a root with no go.mod, or
/// pretends Rust generation is ready when cargo-llvm-cov is missing.
fn fill_suggested_commands(
    repo: &Path,
    families: &mut [CoverageFamilyStatus],
    cargo_dirs: &[String],
    go_mod_dirs: &[String],
    go_work_at_root: bool,
) {
    let rust_present = families.iter().any(|status| status.family == "rust");
    let llvm_cov_ready = if rust_present {
        cargo_llvm_cov_available()
    } else {
        true
    };
    let javascript = javascript_coverage_plan(repo);
    let rust = rust_coverage_plan(repo, cargo_dirs, llvm_cov_ready);
    let python = python_coverage_plan();
    let go = go_coverage_plan(repo, go_mod_dirs, go_work_at_root);
    let jvm = jvm_coverage_plan(repo);
    let native = native_coverage_plan(repo);
    let swift = swift_coverage_plan();
    let dotnet = dotnet_coverage_plan();
    let php = php_coverage_plan(repo);
    let ruby = ruby_coverage_plan(repo);
    let dart = dart_coverage_plan();
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
            _ => LanguageCoveragePlan::ready(Vec::new(), ""),
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
            r#"{"name":"app","devDependencies":{"vitest":"2.0.0"}}"#,
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
    fn javascript_without_package_json_falls_back_to_both_runners() {
        let repo = git_repo();
        write(repo.path(), "src/app.ts", "export const x = 1;\n");
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
        let js = report
            .families
            .iter()
            .find(|f| f.family == "javascript")
            .expect("javascript family");
        assert_eq!(
            js.suggested_commands,
            vec![
                "npx --no-install vitest run --coverage".to_string(),
                "npx --no-install jest --coverage".to_string()
            ]
        );
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
    fn python_go_and_jvm_keep_curated_commands_native_has_none() {
        let repo = git_repo();
        write(repo.path(), "pkg/mod.py", "def f():\n    return 1\n");
        write(repo.path(), "main.go", "package main\nfunc main() {}\n");
        write(repo.path(), "src/Main.java", "class Main {}\n");
        write(repo.path(), "src/lib.c", "int x;\n");
        let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");

        let python = report
            .families
            .iter()
            .find(|f| f.family == "python")
            .expect("python");
        assert_eq!(
            python.suggested_commands,
            vec!["pytest --cov --cov-report=xml".to_string()]
        );
        assert!(python.tool_ready);
        assert!(python.setup_commands.is_empty());
        assert!(python.duration_hint.contains("few minutes"));
        let go = report
            .families
            .iter()
            .find(|f| f.family == "go")
            .expect("go");
        assert_eq!(
            go.suggested_commands,
            vec!["go test ./... -coverprofile=coverage.out".to_string()]
        );
        assert!(go.tool_ready);
        assert!(go.duration_hint.contains("few minutes"));
        let jvm = report
            .families
            .iter()
            .find(|f| f.family == "jvm")
            .expect("jvm");
        assert_eq!(
            jvm.suggested_commands,
            vec![
                "./gradlew test jacocoTestReport".to_string(),
                "mvn verify".to_string()
            ]
        );
        assert!(jvm.tool_ready);
        assert!(jvm.duration_hint.contains("few minutes"));
        let native = report
            .families
            .iter()
            .find(|f| f.family == "native")
            .expect("native");
        assert_eq!(native.suggested_commands, Vec::<String>::new());
        assert!(native.setup_commands.is_empty());
        assert!(native.duration_hint.is_empty());
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
}
