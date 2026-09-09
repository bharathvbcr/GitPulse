//! End-to-end extraction cache integration for the build path (RP2.5).

use std::path::Path;

use devmap_extract::cache::{cache_admits, CacheKey};
use devmap_extract::{detect_language, extract_file, Extraction};
use rayon::prelude::*;

use crate::Store;

/// Extract all indexable sources under `root`, consulting `store` for cache hits
/// and admitting clean/partial outcomes after parse (closes X7 via `cache_admits`).
pub fn extract_tree_cached(store: &Store, root: &Path) -> anyhow::Result<Vec<Extraction>> {
    Ok(extract_tree_cached_with_report(store, root)?.0)
}

/// Extract a tree and return what discovery **refused**, alongside the results.
///
/// `extract_tree_cached` calls `collect_sources`, which throws the discovery
/// report away — so an oversized or unreadable file vanished from the build with
/// no record anywhere a consumer could reach. Measured on a fixture: a 60,000
/// function source and a binary file were both dropped, and `repo_map.json`
/// reported five files with nothing saying two more existed. A capped sample
/// that reads as complete coverage is the one outcome this codebase treats as
/// worse than a visible failure (PHASE1_CONTRACT.md), so the report is carried
/// out rather than discarded.
pub fn extract_tree_cached_with_report(
    store: &Store,
    root: &Path,
) -> anyhow::Result<(Vec<Extraction>, devmap_extract::model::DiscoveryReport)> {
    let scanned = devmap_extract::scan_tree(root)?;
    let extractions = extract_scanned_cached(store, &scanned)?;
    Ok((extractions, scanned.report))
}

/// Extract a tree that has already been scanned, consulting `store` for hits.
///
/// The split exists so a caller can decide *whether* to extract. Every lookup
/// here goes through the store's single guarded connection, so this loop is
/// serial on the SQLite mutex however many rayon threads enter it, and it
/// deserializes one full extraction payload per file: measured on this
/// repository, 213–254 ms for 1,311 unchanged files. A build that only needs to
/// know whether the tree moved gets that from
/// [`devmap_extract::ScannedTree::matches_file_hashes`] instead and never calls
/// this at all.
pub fn extract_scanned_cached(
    store: &Store,
    scanned: &devmap_extract::ScannedTree,
) -> anyhow::Result<Vec<Extraction>> {
    extract_scanned_cached_with_progress(store, scanned, None)
}

pub fn extract_scanned_cached_with_progress(
    store: &Store,
    scanned: &devmap_extract::ScannedTree,
    progress: Option<&devmap_extract::progress::FileProgress>,
) -> anyhow::Result<Vec<Extraction>> {
    extract_scanned_with_progress(store, scanned, progress, CacheAdmission::Immediate)
}

/// Reuse committed extraction payloads while preparing the next generation.
///
/// `save_generation` durably records every new payload in its transaction, and
/// `try_get_cached_extraction` already reads those rows. Writing misses into
/// `extraction_cache` first duplicated the entire corpus only to delete it at
/// commit, forcing extra WAL writes and vacuum work on every cold build.
/// An interrupted generation may repeat uncommitted extraction; it cannot lose
/// a published generation or acknowledge pending work before commit.
pub fn extract_scanned_for_generation(
    store: &Store,
    scanned: &devmap_extract::ScannedTree,
    progress: Option<&devmap_extract::progress::FileProgress>,
) -> anyhow::Result<Vec<Extraction>> {
    extract_scanned_with_progress(store, scanned, progress, CacheAdmission::AtGenerationCommit)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CacheAdmission {
    Immediate,
    AtGenerationCommit,
}

fn extract_scanned_with_progress(
    store: &Store,
    scanned: &devmap_extract::ScannedTree,
    progress: Option<&devmap_extract::progress::FileProgress>,
    admission: CacheAdmission,
) -> anyhow::Result<Vec<Extraction>> {
    if let Some(progress) = progress {
        progress.start(scanned.sources.len());
    }
    scanned
        .sources
        .par_iter()
        .map(|(path, src)| {
            let result = extract_one_cached_with(store, path, src, admission, extract_file);
            if let Some(progress) = progress {
                finish_extraction_progress(progress, &result);
            }
            result.map(|(extraction, _)| extraction)
        })
        .collect::<anyhow::Result<Vec<_>>>()
}

fn finish_extraction_progress(
    progress: &devmap_extract::progress::FileProgress,
    result: &anyhow::Result<(Extraction, bool)>,
) {
    let cached = result.as_ref().is_ok_and(|(_, cached)| *cached);
    let failed = result
        .as_ref()
        .map_or(true, |(extraction, _)| extraction.is_parse_failure());
    progress.finish_file(cached, failed);
}

fn extract_one_cached_with(
    store: &Store,
    path: &str,
    src: &str,
    admission: CacheAdmission,
    extractor: impl FnOnce(&str, &str) -> Extraction,
) -> anyhow::Result<(Extraction, bool)> {
    let language = detect_language(Path::new(path));
    let key = CacheKey::for_source(language, src);
    if let Some(cached) = store.try_get_cached_extraction(&key)? {
        // The current cache payload includes path-derived qualified names,
        // wiring, and file identity. A content key may be shared by many files;
        // never reuse a path-bound payload for a different path.
        if cached.file_path == path {
            return Ok((cached, true));
        }
    }
    let ext = extractor(path, src);
    if admission == CacheAdmission::Immediate && cache_admits(&ext.parse_outcome) {
        store.admit_cached_extraction(&key, &ext)?;
    }
    Ok((ext, false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use devmap_extract::model::ParseOutcome;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn progress_preserves_extractions_across_cache_collisions_and_parse_failures() {
        let mut scanned = devmap_extract::ScannedTree::default();
        for index in 0..64 {
            scanned.sources.push((
                format!("copy_{index}.py"),
                "def shared(): return 42\n".into(),
            ));
        }
        scanned
            .sources
            .push(("invalid.py".into(), "def ((( invalid\n".into()));
        scanned.sources.push(("empty.py".into(), String::new()));
        scanned
            .sources
            .push(("failed.ipynb".into(), "not json".into()));
        let refs: Vec<_> = scanned
            .sources
            .iter()
            .map(|(path, source)| devmap_extract::FileRef { path, source })
            .collect();
        let expected = devmap_extract::extract_all(&refs);
        let store = Store::open_in_memory().unwrap();
        for _ in 0..3 {
            let progress = devmap_extract::progress::FileProgress::default();
            let observed =
                extract_scanned_cached_with_progress(&store, &scanned, Some(&progress)).unwrap();
            let snapshot = progress.snapshot();
            assert!(snapshot.valid);
            assert_eq!(snapshot.total, Some(67));
            assert_eq!(snapshot.completed, 67);
            assert!(snapshot.failed > 0, "fixture must exercise a failed parse");
            assert_eq!(observed.len(), expected.len());
            let mut stripped_sources = 0;
            for (actual, expected) in observed.iter().zip(&expected) {
                // The cache's canonical admission path intentionally strips
                // source_code. Every graph-bearing field must still agree.
                let mut expected = expected.clone();
                if actual.source_code.is_none() && expected.source_code.is_some() {
                    stripped_sources += 1;
                    expected.source_code = None;
                }
                assert_eq!(
                    serde_json::to_value(actual).unwrap(),
                    serde_json::to_value(expected).unwrap(),
                    "{}",
                    actual.file_path
                );
            }
            assert_eq!(stripped_sources, snapshot.cache_hits);
            assert!(snapshot.cache_hits <= snapshot.completed - snapshot.failed);
            assert_eq!(
                snapshot.failed,
                observed
                    .iter()
                    .filter(|extraction| extraction.is_parse_failure())
                    .count()
            );
        }
    }

    #[test]
    fn progress_counts_only_genuine_parse_failures_for_cold_files() {
        let mut scanned = devmap_extract::ScannedTree::default();
        scanned
            .sources
            .push(("guide.md".into(), "# prose\n".into()));
        scanned
            .sources
            .push(("broken.ipynb".into(), "not json".into()));

        let store = Store::open_in_memory().unwrap();
        let cold_progress = devmap_extract::progress::FileProgress::default();
        let cold =
            extract_scanned_cached_with_progress(&store, &scanned, Some(&cold_progress)).unwrap();
        assert!(!cold[0].is_parse_failure());
        assert!(cold[1].is_parse_failure());
        let cold_snapshot = cold_progress.snapshot();
        assert!(cold_snapshot.valid);
        assert_eq!(cold_snapshot.completed, 2);
        assert_eq!(cold_snapshot.cache_hits, 0);
        assert_eq!(cold_snapshot.failed, 1);
    }

    #[test]
    fn progress_counts_a_cached_not_applicable_file_as_successful_work() {
        // Old stores may contain a NotApplicable payload admitted by an older
        // cache policy. A hit is still successful work and must not invalidate
        // progress merely because its wire outcome is the legacy `Failed`.
        let prose = devmap_extract::extract_file("guide.md", "# prose\n");
        let cached_progress = devmap_extract::progress::FileProgress::default();
        cached_progress.start(2);
        finish_extraction_progress(&cached_progress, &Ok((prose.clone(), true)));
        let broken = devmap_extract::extract_file("broken.ipynb", "not json");
        finish_extraction_progress(&cached_progress, &Ok((broken.clone(), false)));
        assert!(!prose.is_parse_failure());
        assert!(broken.is_parse_failure());
        let cached_snapshot = cached_progress.snapshot();
        assert!(cached_snapshot.valid);
        assert_eq!(cached_snapshot.completed, 2);
        assert_eq!(cached_snapshot.cache_hits, 1);
        assert_eq!(cached_snapshot.failed, 1);
    }

    #[test]
    fn test_cache_admission_end_to_end() {
        // closes X7 — Failed never cached; second pass hits cache for Clean
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("devmap-cache-e2e-{stamp}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("pkg")).unwrap();
        fs::write(root.join("pkg/good.py"), "def good(): pass\n").unwrap();
        fs::write(root.join("pkg/bad.py"), "def ((( invalid\n").unwrap();

        let store = Store::open_in_memory().unwrap();
        let first = extract_tree_cached(&store, &root).unwrap();
        assert_eq!(first.len(), 2);

        let bad = first
            .iter()
            .find(|e| e.file_path.ends_with("bad.py"))
            .unwrap();
        if matches!(bad.parse_outcome, ParseOutcome::Failed { .. }) {
            let key = CacheKey::for_extraction(bad);
            assert!(store.try_get_cached_extraction(&key).unwrap().is_none());
        }

        let second = extract_tree_cached(&store, &root).unwrap();
        assert_eq!(second.len(), 2);

        let good = second
            .iter()
            .find(|e| e.file_path.ends_with("good.py"))
            .unwrap();
        let key = CacheKey::for_extraction(good);
        assert!(
            store.try_get_cached_extraction(&key).unwrap().is_some(),
            "clean extraction must be cache-admitted"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn identical_content_never_aliases_file_identity_through_cache() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("devmap-cache-identity-{stamp}"));
        fs::create_dir_all(&root).unwrap();
        for index in 0..64 {
            fs::write(
                root.join(format!("module_{index}.py")),
                "def identical():\n    return 1\n",
            )
            .unwrap();
        }
        let store = Store::open_in_memory().unwrap();
        for _ in 0..2 {
            let extractions = extract_tree_cached(&store, &root).unwrap();
            let paths: std::collections::BTreeSet<_> = extractions
                .iter()
                .map(|extraction| extraction.file_path.as_str())
                .collect();
            assert_eq!(extractions.len(), 64);
            assert_eq!(paths.len(), 64, "cache must preserve every file identity");
        }
        let _ = fs::remove_dir_all(&root);
    }

    /// The unchanged verdict a scan reaches is the one extraction would reach.
    ///
    /// This is the equivalence the no-op fast path rests on: the build now
    /// compares `ScannedTree`'s `(path, content_hash)` pairs against the stored
    /// generation instead of extracting every file and comparing the
    /// extractions' own `file_path`/`content_hash`. The two must agree over
    /// every case the corpus can produce — a clean parse, a parse that FAILED
    /// (never cache-admitted, so it takes the re-parse arm every time), a file
    /// whose content is shared with another path (a cache key collision the
    /// path guard rejects), and an empty file.
    #[test]
    fn scan_hashes_agree_with_extraction_hashes_over_every_outcome() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("devmap-scan-agree-{stamp}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("clean.py"), "def clean():\n    return 1\n").unwrap();
        fs::write(root.join("broken.py"), "def ((( invalid\n").unwrap();
        // Byte-identical to clean.py: one content hash, two file identities.
        fs::write(root.join("twin.py"), "def clean():\n    return 1\n").unwrap();
        fs::write(root.join("empty.py"), "").unwrap();
        fs::write(root.join("notes.md"), "# not a source file\n").unwrap();

        let store = Store::open_in_memory().unwrap();
        // Twice: once cold (every file re-parsed) and once warm (cache hits),
        // because the two arms of `extract_one_cached` build `content_hash`
        // differently — one from the parser, one from a stored payload.
        for pass in 0..2 {
            let scanned = devmap_extract::scan_tree(&root).unwrap();
            let extractions = extract_scanned_cached(&store, &scanned).unwrap();

            let from_scan: std::collections::BTreeMap<&str, u64> =
                scanned.file_hashes().into_iter().collect();
            let from_extraction: std::collections::BTreeMap<&str, u64> = extractions
                .iter()
                .map(|e| (e.file_path.as_str(), e.content_hash))
                .collect();
            assert_eq!(
                from_scan, from_extraction,
                "pass {pass}: scan and extraction must agree on every (path, hash)"
            );

            // And the verdict built on them agrees too, in both directions.
            let previous: std::collections::BTreeMap<String, u64> = from_extraction
                .iter()
                .map(|(path, hash)| ((*path).to_string(), *hash))
                .collect();
            assert!(
                scanned.matches_file_hashes(&previous),
                "pass {pass}: an identical tree must match"
            );
            let mut moved = previous.clone();
            moved.insert("clean.py".to_string(), 0);
            assert!(
                !scanned.matches_file_hashes(&moved),
                "pass {pass}: one differing hash must not match"
            );
            let mut renamed = previous.clone();
            let hash = renamed.remove("twin.py").unwrap();
            renamed.insert("renamed.py".to_string(), hash);
            assert!(
                !scanned.matches_file_hashes(&renamed),
                "pass {pass}: a rename with identical content must not match"
            );
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cache_hit_returns_before_invoking_the_parser() {
        let store = Store::open_in_memory().unwrap();
        let source = "def cached():\n    return 1\n";
        let first = extract_one_cached_with(
            &store,
            "cached.py",
            source,
            CacheAdmission::Immediate,
            extract_file,
        )
        .unwrap();
        assert_eq!(first.0.file_path, "cached.py");
        assert!(!first.1);

        let second = extract_one_cached_with(
            &store,
            "cached.py",
            source,
            CacheAdmission::Immediate,
            |_, _| panic!("parser must not run on a cache hit"),
        )
        .unwrap();
        assert_eq!(second.0.file_path, "cached.py");
        assert!(second.1);
    }
}
