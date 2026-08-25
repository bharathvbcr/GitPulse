//! Language-aware test-coverage scanning.
//!
//! GitPulse does not run tests. It finds coverage artifacts the opened
//! repository's languages actually produce, parses them, and folds hit maps
//! into one report. A Rust tree is not searched for JaCoCo; a Python tree is
//! not searched for `coverage.out`.

use crate::analyzer::language::LanguageDetector;
use crate::engine::git_cli::{git_text, sandbox_join, validate_repo};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

type HitMap = BTreeMap<usize, u64>;
type FileHitMaps = HashMap<String, HitMap>;

const MAX_LINE_NO: usize = 2_000_000;
const MAX_DIR_ENTRIES: usize = 64;
const DEFAULT_MAX_ARTIFACT_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_MAX_ARTIFACTS: usize = 48;
const DEFAULT_MAX_FILES: usize = 4_000;
const DEFAULT_MAX_TOTAL_ENTRIES: usize = 4_000_000;

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
}

impl EntryBudget {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            remaining: capacity,
        }
    }

    fn spend(&mut self) -> bool {
        if self.remaining == 0 {
            return false;
        }
        self.remaining -= 1;
        true
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoverageReport {
    pub families: Vec<CoverageFamilyStatus>,
    pub artifacts: Vec<CoverageArtifact>,
    pub files: Vec<FileCoverageSummary>,
    pub overall: CoverageTotals,
    pub truncated: bool,
}

pub struct CoverageScanner;

impl CoverageScanner {
    pub fn scan(repo_path: &str) -> Result<CoverageReport, String> {
        Self::scan_with_limits(repo_path, ScanLimits::default()).map(|(report, _)| report)
    }

    pub fn file_coverage(repo_path: &str, file_path: &str) -> Result<FileCoverage, String> {
        let repo = validate_repo(repo_path)?;
        let joined = sandbox_join(&repo, file_path)?;
        let rel = joined
            .strip_prefix(&repo)
            .map_err(|_| "File path escapes the repository".to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        let rel = LanguageDetector::normalize_rel_path(&rel);
        if rel.is_empty() {
            return Err("Invalid file path".into());
        }
        let (_, maps) = Self::scan_with_limits(repo_path, ScanLimits::default())?;
        let lang = LanguageDetector::detect_from_path(&rel);
        let lines_map = maps.get(&rel).cloned().unwrap_or_default();
        let totals = CoverageTotals::from_map(&lines_map);
        let lines = lines_map
            .into_iter()
            .map(|(line_no, hits)| CoveredLine { line_no, hits })
            .collect();
        Ok(FileCoverage {
            path: rel,
            language: lang.name.to_string(),
            color_hex: lang.color_hex.to_string(),
            lines,
            totals,
        })
    }

    pub fn scan_with_limits(
        repo_path: &str,
        limits: ScanLimits,
    ) -> Result<(CoverageReport, FileHitMaps), String> {
        let repo = validate_repo(repo_path)?;
        let mut families = detect_families(&repo)?;
        if families.is_empty() {
            return Ok((
                CoverageReport {
                    families: Vec::new(),
                    artifacts: Vec::new(),
                    files: Vec::new(),
                    overall: CoverageTotals::default(),
                    truncated: false,
                },
                HashMap::new(),
            ));
        }

        let mut candidates = collect_candidates(&families);
        extend_directory_candidates(&repo, &families, &mut candidates);

        let mut artifacts = Vec::new();
        let mut merged: FileHitMaps = HashMap::new();
        let mut truncated = false;
        let mut budget = EntryBudget::new(limits.max_total_entries);

        for (considered, cand) in candidates.into_iter().enumerate() {
            if considered >= limits.max_artifacts {
                truncated = true;
                break;
            }
            match read_artifact(&repo, &cand.rel, limits.max_artifact_bytes) {
                ArtifactRead::Missing => continue,
                ArtifactRead::Skipped { reason } => {
                    mark_families_for_artifact(&mut families, &cand.rel, &cand.family);
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
                    mark_families_for_artifact(&mut families, &cand.rel, &cand.family);
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
                    match parse_artifact(cand.format, &text, &repo, &mut budget) {
                        Ok(map) => {
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

        if merged.len() > limits.max_files {
            truncated = true;
            let mut keys: Vec<String> = merged.keys().cloned().collect();
            keys.sort();
            for extra in keys.into_iter().skip(limits.max_files) {
                merged.remove(&extra);
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

        let overall = {
            let found = files.iter().map(|f| f.lines_found).sum();
            let hit = files.iter().map(|f| f.lines_hit).sum();
            CoverageTotals::from_counts(found, hit)
        };

        let mut family_list: Vec<CoverageFamilyStatus> = families.into_values().collect();
        family_list.sort_by(|a, b| a.family.cmp(&b.family));

        Ok((
            CoverageReport {
                families: family_list,
                artifacts,
                files,
                overall,
                truncated,
            },
            merged,
        ))
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
        ],
        "swift" => &[
            ("cobertura.xml", CoverageFormat::Cobertura),
            ("coverage/cobertura.xml", CoverageFormat::Cobertura),
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
        _ => &[],
    }
}

fn detect_families(repo: &Path) -> Result<BTreeMap<String, CoverageFamilyStatus>, String> {
    let stdout = git_text(
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
    for rel in stdout.split('\0') {
        let rel = LanguageDetector::normalize_rel_path(rel);
        if rel.is_empty() || skip_source(&rel) {
            continue;
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
                    "#dea584".to_string()
                } else {
                    info.color_hex.to_string()
                },
                expected_formats: formats.into_iter().collect(),
                expected_paths: paths,
                found: false,
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
    Ok(families)
}

fn collect_candidates(families: &BTreeMap<String, CoverageFamilyStatus>) -> Vec<Candidate> {
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
    out
}

/// Marks every family that claims artifact `rel` as having found coverage:
/// the family whose spec produced the candidate (`owner`), plus any other
/// family whose expected paths include the same file, so a shared artifact
/// credits all of its claimants exactly once.
fn mark_families_for_artifact(
    families: &mut BTreeMap<String, CoverageFamilyStatus>,
    rel: &str,
    owner: &str,
) {
    for status in families.values_mut() {
        if status.family == owner || status.expected_paths.iter().any(|p| p == rel) {
            status.found = true;
        }
    }
}

fn extend_directory_candidates(
    repo: &Path,
    families: &BTreeMap<String, CoverageFamilyStatus>,
    out: &mut Vec<Candidate>,
) {
    let mut seen: BTreeSet<String> = out.iter().map(|c| c.rel.clone()).collect();
    for family in families.keys() {
        for dir in extra_dirs_for(family) {
            let Ok(joined) = sandbox_join(repo, dir) else {
                continue;
            };
            let Ok(entries) = std::fs::read_dir(&joined) else {
                continue;
            };
            for entry in entries.filter_map(|e| e.ok()).take(MAX_DIR_ENTRIES) {
                let name = entry.file_name();
                let name = name.to_string_lossy();
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
                    family: family.clone(),
                });
            }
        }
    }
}

fn format_from_filename(name: &str) -> Option<CoverageFormat> {
    let lower = name.to_ascii_lowercase();
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
        Some(CoverageFormat::Istanbul)
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
        Err(_) => return ArtifactRead::Missing,
    };
    if meta.is_dir() {
        return ArtifactRead::Missing;
    }
    if meta.len() > max_bytes {
        return ArtifactRead::Skipped {
            reason: format!("artifact exceeds {} byte limit ({})", max_bytes, meta.len()),
        };
    }
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
        Err(_) => ArtifactRead::Missing,
    }
}

fn budget_remaining(budget: &EntryBudget) -> usize {
    budget.remaining
}

fn parse_artifact(
    format: CoverageFormat,
    text: &str,
    repo: &Path,
    budget: &mut EntryBudget,
) -> Result<HashMap<String, BTreeMap<usize, u64>>, String> {
    let text = text.trim_start_matches('\u{feff}');
    match format {
        CoverageFormat::Lcov => Ok(parse_lcov(text, repo, budget)),
        CoverageFormat::Cobertura => Ok(parse_cobertura(text, repo, budget)),
        CoverageFormat::GoCover => Ok(parse_go_cover(text, repo, budget)),
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
            current = relativize(repo, sf.trim());
        } else if raw.trim() == "end_of_record" {
            flush_record(&mut out, &mut current, &mut lines, budget);
        } else if let Some(da) = raw.strip_prefix("DA:") {
            // DA:<line>,<hits>[,<checksum>] — the optional checksum must be
            // ignored rather than glued onto the hits field.
            let mut parts = da.split(',');
            if let (Some(ln), Some(hits)) = (parts.next(), parts.next()) {
                if let (Ok(ln), Ok(hits)) = (ln.trim().parse::<usize>(), hits.trim().parse::<u64>())
                {
                    record_hit(&mut lines, ln, hits, budget);
                }
            }
        }
    }
    flush_record(&mut out, &mut current, &mut lines, budget);
    out
}

fn parse_cobertura(
    text: &str,
    repo: &Path,
    budget: &mut EntryBudget,
) -> HashMap<String, BTreeMap<usize, u64>> {
    let mut out = HashMap::new();
    let mut current: Option<String> = None;
    let mut lines = BTreeMap::new();
    for piece in text.split('<') {
        let trimmed = piece.trim();
        if trimmed.starts_with("class ") || trimmed.starts_with("file ") {
            if let Some(path) = current.take() {
                merge_file(&mut out, path, std::mem::take(&mut lines), budget);
            }
            let reported = xml_attr(trimmed, "filename")
                .or_else(|| xml_attr(trimmed, "name"))
                .unwrap_or("");
            current = relativize(repo, reported);
        } else if current.is_some() {
            if let Some(rest) = trimmed.strip_prefix("line ") {
                let ln = xml_attr(rest, "number").or_else(|| xml_attr(rest, "nr"));
                let hits = xml_attr(rest, "hits").or_else(|| xml_attr(rest, "count"));
                if let (Some(ln), Some(hits)) = (ln, hits) {
                    if let (Ok(ln), Ok(hits)) = (ln.parse::<usize>(), hits.parse::<u64>()) {
                        record_hit(&mut lines, ln, hits, budget);
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
) -> HashMap<String, BTreeMap<usize, u64>> {
    // A few KB of go-cover text can describe tens of millions of covered
    // lines via block ranges. Expanding them all would explode memory and
    // CPU, so each file gets a hard expansion budget; once spent, further
    // ranges for that file are ignored (coverage stays representative, the
    // scan stays bounded).
    const MAX_EXPANDED_LINES_PER_FILE: usize = 200_000;
    let mut out = HashMap::new();
    let mut expansion: HashMap<String, usize> = HashMap::new();
    for raw in text.lines() {
        if raw.starts_with("mode:") || raw.trim().is_empty() {
            continue;
        }
        let Some((path_part, rest)) = raw.rsplit_once(':') else {
            continue;
        };
        let Some(path) = relativize_or_suffix(repo, path_part.trim()) else {
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
            break;
        }
        let allowance = expansion
            .entry(path.clone())
            .or_insert(MAX_EXPANDED_LINES_PER_FILE);
        if *allowance == 0 {
            continue;
        }
        let file = out.entry(path.clone()).or_default();
        let end = end_line
            .max(start_line)
            .min(start_line.saturating_add(10_000))
            .min(MAX_LINE_NO);
        // Clamp the span to whatever budget remains; a range that straddles
        // the boundary still records its reachable head.
        let span_end = start_line
            .saturating_add((*allowance).saturating_sub(1))
            .min(end);
        if span_end < start_line {
            *allowance = 0;
            continue;
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
        let Some(path) = relativize(repo, reported) else {
            continue;
        };
        let dest = out.entry(path).or_default();
        if let Some(lines) = file.get("l").and_then(|v| v.as_object()) {
            for (ln, hits) in lines {
                if let (Ok(ln), Some(hits)) = (ln.parse::<usize>(), hits.as_u64()) {
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
            let hits = hits.as_u64().unwrap_or(0);
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
    for piece in text.split('<') {
        let trimmed = piece.trim();
        if trimmed.starts_with("package ") {
            package = xml_attr(trimmed, "name").unwrap_or("").replace('.', "/");
        } else if trimmed.starts_with("sourcefile ") {
            if let Some(path) = current.take() {
                merge_file(&mut out, path, std::mem::take(&mut lines), budget);
            }
            let name = xml_attr(trimmed, "name").unwrap_or("");
            let reported = if package.is_empty() {
                name.to_string()
            } else {
                format!("{package}/{name}")
            };
            current = relativize_or_suffix(repo, &reported);
        } else if current.is_some() && trimmed.starts_with("line ") {
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
                record_hit(&mut lines, ln, ci, budget);
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
    for piece in text.split('<') {
        let trimmed = piece.trim();
        if trimmed.starts_with("file ") {
            if let Some(path) = current.take() {
                merge_file(&mut out, path, std::mem::take(&mut lines), budget);
            }
            let reported = xml_attr(trimmed, "path").or_else(|| xml_attr(trimmed, "name"));
            current = reported.and_then(|r| relativize(repo, r));
        } else if current.is_some() && trimmed.starts_with("line ") {
            let ln = xml_attr(trimmed, "num");
            let hits = xml_attr(trimmed, "count");
            if let (Some(ln), Some(hits)) = (ln, hits) {
                if let (Ok(ln), Ok(hits)) = (ln.parse::<usize>(), hits.parse::<u64>()) {
                    record_hit(&mut lines, ln, hits, budget);
                }
            }
        }
    }
    if let Some(path) = current.take() {
        merge_file(&mut out, path, lines, budget);
    }
    out
}

/// Extracts `name="value"` or `name='value'` from a tag fragment. Some
/// coverage generators emit single-quoted XML attributes.
fn xml_attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    for quote in ['"', '\''] {
        let prefix = format!("{name}={quote}");
        if let Some(start) = tag.find(&prefix) {
            let rest = tag.get(start + prefix.len()..)?;
            if let Some(end) = rest.find(quote) {
                return Some(&rest[..end]);
            }
        }
    }
    None
}

fn record_hit(lines: &mut BTreeMap<usize, u64>, ln: usize, hits: u64, budget: &mut EntryBudget) {
    if ln == 0 || ln > MAX_LINE_NO {
        return;
    }
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

fn merge_file(
    out: &mut HashMap<String, BTreeMap<usize, u64>>,
    path: String,
    lines: BTreeMap<usize, u64>,
    budget: &mut EntryBudget,
) {
    if skip_source(&path) || lines.is_empty() || budget_remaining(budget) == 0 {
        return;
    }
    let dest = out.entry(path).or_default();
    for (ln, hits) in lines {
        record_hit(dest, ln, hits, budget);
        if budget_remaining(budget) == 0 {
            break;
        }
    }
}

fn merge_into(
    dest: &mut HashMap<String, BTreeMap<usize, u64>>,
    src: HashMap<String, BTreeMap<usize, u64>>,
    budget: &mut EntryBudget,
) -> bool {
    let mut exhausted = false;
    for (path, lines) in src {
        merge_file(dest, path, lines, budget);
        if budget_remaining(budget) == 0 {
            exhausted = true;
            break;
        }
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

fn relativize_or_suffix(repo: &Path, reported: &str) -> Option<String> {
    let rel = relativize(repo, reported);
    if let Some(ref path) = rel {
        if repo.join(path).is_file() {
            return rel;
        }
    }
    let cleaned = reported.replace('\\', "/");
    let parts: Vec<&str> = cleaned
        .split('/')
        .filter(|p| !p.is_empty() && *p != "..")
        .collect();
    for i in 0..parts.len() {
        let candidate = parts[i..].join("/");
        if is_safe_rel(&candidate) && !skip_source(&candidate) && repo.join(&candidate).is_file() {
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
            },
        );
        let cands = collect_candidates(&families);
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
            },
        );
        let cands = collect_candidates(&families);
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
        let map = parse_go_cover(text, repo.path(), &mut EntryBudget::new(usize::MAX));
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
        // them as covered instead of panicking in debug builds.
        assert_eq!(map["com/foo/Big.java"].get(&1), Some(&u64::MAX));
        assert_eq!(map["com/foo/Big.java"].get(&2), Some(&u64::MAX));
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
}
