// The parsing frontend. Everything below the `parse` gate needs tree-sitter
// and its grammars; everything above it — the model types, language detection,
// go.mod parsing, the ignore rules — does not, and is what a *query* consumer
// actually uses.
#[cfg(feature = "parse")]
pub mod cache;
pub mod frameworks;
pub mod gomod;
#[cfg(feature = "parse")]
pub mod langcalls;
#[cfg(feature = "parse")]
pub(crate) mod langdecl;
pub mod languages;
pub mod model;
#[cfg(feature = "parse")]
pub mod treesitter;
pub mod wiring;

use std::fs;
use std::path::{Path, PathBuf};

#[cfg(feature = "parse")]
use rayon::prelude::*;

pub use gomod::{collect_go_modules, git_worktree_root, parse_go_mod, GoModule};
pub use languages::{detect_language, is_ignored_path, is_indexable_source};
pub use model::*;
#[cfg(feature = "parse")]
pub use treesitter::extract_treesitter;

pub struct FileRef<'a> {
    pub path: &'a str,
    pub source: &'a str,
}

pub const MAX_SOURCE_BYTES: u64 = 1024 * 1024;

/// Canonical source-content identity shared by extraction, cache, and
/// connect-time freshness checks.
pub fn content_hash(source: &str) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    source
        .as_bytes()
        .iter()
        .fold(FNV_OFFSET_BASIS, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
        })
}

/// Evaluate Git ignore rules the same way `WalkBuilder` does for a cold build.
///
/// `WalkBuilder.git_ignore(true)` reads `.gitignore` from the git worktree
/// root down, including parents of `root` when the build is rooted in a
/// subdirectory. The watcher must use this same stack or incremental
/// generations admit files the next cold build drops.
pub fn is_gitignored(root: &Path, path: &Path, is_dir: bool) -> anyhow::Result<bool> {
    let path_abs = path_under_root(root, path)?;
    let matchers = ignore_matchers_for(root, &path_abs, is_dir)?;
    Ok(matches_ignore(&matchers, &path_abs, is_dir).unwrap_or(false))
}

/// Ignore-rule files that affect `path`, from the git worktree root (or `root`
/// when there is no git metadata) down to the path. Watcher cache stamps use
/// this list so a parent `.gitignore` edit invalidates verdicts.
pub fn ignore_rule_files(root: &Path, path: &Path, is_dir: bool) -> anyhow::Result<Vec<PathBuf>> {
    let path_abs = path_under_root(root, path)?;
    Ok(ignore_rule_bases(root, &path_abs, is_dir)?
        .into_iter()
        .map(|(_, rules)| rules)
        .collect())
}

fn path_under_root(root: &Path, path: &Path) -> anyhow::Result<PathBuf> {
    let root_abs = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let path_abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let path_abs = path_abs.canonicalize().unwrap_or(path_abs);
    path_abs
        .strip_prefix(&root_abs)
        .map_err(|_| anyhow::anyhow!("watch path {path_abs:?} is outside root {root_abs:?}"))?;
    Ok(path_abs)
}

fn ignore_rule_bases(
    root: &Path,
    path: &Path,
    is_dir: bool,
) -> anyhow::Result<Vec<(PathBuf, PathBuf)>> {
    let git_root = git_worktree_root(root)
        .or_else(|| root.canonicalize().ok())
        .unwrap_or_else(|| root.to_path_buf());
    let path_abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let path_abs = path_abs.canonicalize().unwrap_or(path_abs);
    let git_root = git_root.canonicalize().unwrap_or(git_root);
    let rel = path_abs
        .strip_prefix(&git_root)
        .unwrap_or(path_abs.as_path());
    let parent = if is_dir {
        rel
    } else {
        rel.parent().unwrap_or_else(|| Path::new(""))
    };

    let mut rules = vec![
        (git_root.clone(), git_root.join(".git/info/exclude")),
        (git_root.clone(), git_root.join(".gitignore")),
    ];
    let mut current = git_root;
    for component in parent.components() {
        current.push(component.as_os_str());
        rules.push((current.clone(), current.join(".gitignore")));
    }
    Ok(rules)
}

fn ignore_matchers_for(
    root: &Path,
    path: &Path,
    is_dir: bool,
) -> anyhow::Result<Vec<ignore::gitignore::Gitignore>> {
    let bases = ignore_rule_bases(root, path, is_dir)?;
    let mut matchers = Vec::new();
    for (base, rules) in &bases {
        add_ignore_rules(&mut matchers, base, rules)?;
    }
    Ok(matchers)
}

fn add_ignore_rules(
    matchers: &mut Vec<ignore::gitignore::Gitignore>,
    base: &Path,
    rules: &Path,
) -> anyhow::Result<()> {
    if !rules.is_file() {
        return Ok(());
    }
    let mut builder = ignore::gitignore::GitignoreBuilder::new(base);
    if let Some(error) = builder.add(rules) {
        anyhow::bail!("cannot parse ignore rules {rules:?}: {error}");
    }
    matchers.push(builder.build()?);
    Ok(())
}

fn matches_ignore(
    matchers: &[ignore::gitignore::Gitignore],
    path: &Path,
    is_dir: bool,
) -> Option<bool> {
    let mut ignored = None;
    for matcher in matchers {
        let relative = path.strip_prefix(matcher.path()).unwrap_or(path);
        let matched = matcher.matched_path_or_any_parents(relative, is_dir);
        if matched.is_ignore() {
            ignored = Some(true);
        } else if matched.is_whitelist() {
            ignored = Some(false);
        }
    }
    ignored
}

#[cfg(feature = "parse")]
pub fn extract_file(path: &str, source: &str) -> Extraction {
    let lang = detect_language(Path::new(path));
    extract_treesitter(path, lang, source)
}

#[cfg(feature = "parse")]
pub fn extract_all(files: &[FileRef]) -> Vec<Extraction> {
    files
        .par_iter()
        .map(|f| extract_file(f.path, f.source))
        .collect()
}

#[cfg(test)]
mod content_hash_tests {
    use super::content_hash;

    #[test]
    fn content_hash_is_pinned_fnv1a64() {
        assert_eq!(content_hash(""), 0xcbf29ce484222325);
        assert_eq!(content_hash("hello"), 0xa430d84680aabd0b);
    }
}

/// Collect indexable source files under `root` as owned `(relative_path, source)` pairs.
/// Skips non-source paths and unreadable / non-UTF8 files (fail-closed: omit, do not invent).
pub fn collect_sources(root: &Path) -> anyhow::Result<Vec<(String, String)>> {
    let (sources, _) = collect_sources_with_report(root)?;
    Ok(sources)
}

/// Collect source files and report each admitted or rejected candidate.
/// Gitignored paths are rejected by the walker before they become candidates.
pub fn collect_sources_with_report(
    root: &Path,
) -> anyhow::Result<(Vec<(String, String)>, DiscoveryReport)> {
    let mut out = Vec::new();
    let mut report = DiscoveryReport::default();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .build();

    for result in walker {
        let entry = result?;
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let Ok(rel) = p.strip_prefix(root) else {
            continue;
        };
        let Some(rel_str) = rel.to_str() else {
            report.skipped_paths.push((
                rel.to_string_lossy().into_owned(),
                DiscoverySkipReason::NonUtf8Path,
            ));
            continue;
        };
        let rel_str = rel_str.replace('\\', "/");
        if !is_indexable_source(&rel_str) {
            report
                .skipped_paths
                .push((rel_str, DiscoverySkipReason::NonSource));
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                report.skipped_paths.push((
                    rel_str,
                    DiscoverySkipReason::Unreadable {
                        reason: error.to_string(),
                    },
                ));
                continue;
            }
        };
        if metadata.len() > MAX_SOURCE_BYTES {
            report.skipped_paths.push((
                rel_str,
                DiscoverySkipReason::Oversized {
                    bytes: metadata.len(),
                    limit: MAX_SOURCE_BYTES,
                },
            ));
            continue;
        }
        match fs::read_to_string(p) {
            Ok(src) => {
                report.yielded_paths.push(rel_str.clone());
                out.push((rel_str, src));
            }
            Err(error) => report.skipped_paths.push((
                rel_str,
                DiscoverySkipReason::Unreadable {
                    reason: error.to_string(),
                },
            )),
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    report.yielded_paths.sort();
    report
        .skipped_paths
        .sort_by(|left, right| left.0.cmp(&right.0));
    Ok((out, report))
}

/// Extract every indexable file under `root` (owned paths — no leaks).
#[cfg(feature = "parse")]
pub fn extract_tree(root: &Path) -> anyhow::Result<Vec<Extraction>> {
    let sources = collect_sources(root)?;
    let refs: Vec<FileRef> = sources
        .iter()
        .map(|(path, src)| FileRef {
            path: path.as_str(),
            source: src.as_str(),
        })
        .collect();
    Ok(extract_all(&refs))
}

#[cfg(all(test, feature = "parse"))]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn extract_tree_skips_non_source_and_finds_py() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("devmap-extract-{}", stamp));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("pkg")).unwrap();
        fs::write(root.join("pkg/mod.py"), "def foo():\n    return 1\n").unwrap();
        fs::write(root.join("pkg/notes.bin"), b"\x00\x01\x02\xff").unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::write(root.join("target/debug/x.rs"), "fn ignored() {}\n").unwrap();

        let exts = extract_tree(&root).unwrap();
        assert_eq!(exts.len(), 1);
        assert_eq!(exts[0].file_path, "pkg/mod.py");
        assert!(exts[0].symbols.iter().any(|s| s.name == "foo"));

        let _ = fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod discovery_bound_tests {
    use super::*;

    /// The source size limit is exclusive and is its declared value.
    ///
    /// `metadata.len() > MAX_SOURCE_BYTES` was mutable to `>=`, and the
    /// constant `1024 * 1024` to `1024 + 1024`. The limit is what stops a
    /// generated multi-megabyte file from being parsed and stored; shrunk to
    /// 2 KiB it silently skips most real sources, and every skip is recorded as
    /// `Oversized` rather than failing, so the map just gets quietly smaller.
    #[test]
    fn the_source_size_limit_is_its_declared_value_and_exclusive() {
        assert_eq!(MAX_SOURCE_BYTES, 1_048_576, "1 MiB source ceiling");

        let dir = std::env::temp_dir().join(format!(
            "devmap-size-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        // Exactly at the limit is admitted.
        let at = dir.join("at.py");
        std::fs::write(&at, "#".repeat(MAX_SOURCE_BYTES as usize)).unwrap();
        // One byte past is skipped as oversized.
        let over = dir.join("over.py");
        std::fs::write(&over, "#".repeat(MAX_SOURCE_BYTES as usize + 1)).unwrap();

        let (sources, report) = collect_sources_with_report(&dir).unwrap();
        assert!(
            sources.iter().any(|(path, _)| path.ends_with("at.py")),
            "a source exactly at the limit must be admitted"
        );
        assert!(
            !sources.iter().any(|(path, _)| path.ends_with("over.py")),
            "a source past the limit must not be admitted"
        );
        assert!(
            report.skipped_paths.iter().any(|(path, reason)| {
                path.ends_with("over.py") && matches!(reason, DiscoverySkipReason::Oversized { .. })
            }),
            "the skip must be recorded as Oversized, not silently dropped: {:?}",
            report.skipped_paths
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
