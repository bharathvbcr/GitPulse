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
    let (sources, report) = devmap_extract::collect_sources_with_report(root)?;
    let extractions: Vec<Extraction> = sources
        .par_iter()
        .map(|(path, src)| extract_one_cached(store, path, src))
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok((extractions, report))
}

fn extract_one_cached(store: &Store, path: &str, src: &str) -> anyhow::Result<Extraction> {
    extract_one_cached_with(store, path, src, extract_file)
}

fn extract_one_cached_with(
    store: &Store,
    path: &str,
    src: &str,
    extractor: impl FnOnce(&str, &str) -> Extraction,
) -> anyhow::Result<Extraction> {
    let language = detect_language(Path::new(path));
    let key = CacheKey::for_source(language, src);
    if let Some(cached) = store.try_get_cached_extraction(&key)? {
        // The current cache payload includes path-derived qualified names,
        // wiring, and file identity. A content key may be shared by many files;
        // never reuse a path-bound payload for a different path.
        if cached.file_path == path {
            return Ok(cached);
        }
    }
    let ext = extractor(path, src);
    if cache_admits(&ext.parse_outcome) {
        store.admit_cached_extraction(&key, &ext)?;
    }
    Ok(ext)
}

#[cfg(test)]
mod tests {
    use super::*;
    use devmap_extract::model::ParseOutcome;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

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

    #[test]
    fn cache_hit_returns_before_invoking_the_parser() {
        let store = Store::open_in_memory().unwrap();
        let source = "def cached():\n    return 1\n";
        let first = extract_one_cached_with(&store, "cached.py", source, extract_file).unwrap();
        assert_eq!(first.file_path, "cached.py");

        let second = extract_one_cached_with(&store, "cached.py", source, |_, _| {
            panic!("parser must not run on a cache hit")
        })
        .unwrap();
        assert_eq!(second.file_path, "cached.py");
    }
}
