//! Repository doc vault: MarkDev `Vault` fed from `git ls-files`, not a walk.
//!
//! Git is the authority on which `.md` files belong in the repo (tracked,
//! non-ignored). Notes are parsed with `Note::parse` and indexed through
//! `Vault::build`. Rename uses the pure `rewrite_links_in` helper plus
//! GitPulse's git writer — never MarkDev's `rename_note`.

use crate::engine::git_cli::{git_text, sandbox_join, sandbox_write, validate_repo};
use crate::engine::git_writer::GitWriter;
use markdev::vault::note::{has_markdown_extension, stem, strip_markdown_extension};
use markdev::vault::rename::{rewrite_links_in, ProtectedRanges};
use markdev::vault::{Backlink, Graph, GraphQuery, Note, SearchHit, Vault, DEFAULT_MAX_NOTE_BYTES};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Soft ceiling on markdown files held in one vault. Personal / project docs
/// sit far below this; a monorepo that vendors thousands of READMEs degrades
/// to a partial index rather than OOM.
const MAX_DOC_FILES: usize = 5_000;

/// Default search hit limit (also the UI honesty threshold).
pub const DEFAULT_SEARCH_LIMIT: usize = 50;

/// Cached vault plus the build-time honesty fields (`truncated`, skips).
/// `status()` must not invent `truncated: false` after a capped build.
struct CachedVault {
    vault: Vault,
    status: DocsStatus,
}

fn vault_cache() -> &'static Mutex<HashMap<PathBuf, CachedVault>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedVault>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// One broken link: source note path and the unresolved target as written.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BrokenLink {
    pub source: String,
    pub target: String,
}

/// Summary of the vault currently cached for a repo.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DocsStatus {
    pub note_count: u32,
    pub truncated: bool,
    pub skipped_oversized: u32,
    pub skipped_unreadable: u32,
}

/// Outcome of a link-preserving markdown rename.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DocRenameOutcome {
    pub from: String,
    pub to: String,
    /// Paths whose link text was rewritten (not including `from`/`to`).
    pub rewritten_paths: Vec<String>,
    pub links_rewritten: u32,
}

/// Builds (or rebuilds) the vault for `repo_path` from tracked markdown files.
pub fn refresh(repo_path: &str) -> Result<DocsStatus, String> {
    let repo = validate_repo(repo_path)?;
    let (vault, status) = build_vault(&repo)?;
    let mut cache = vault_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.insert(
        repo,
        CachedVault {
            vault,
            status: status.clone(),
        },
    );
    Ok(status)
}

/// Drops a cached vault so the next query rebuilds.
pub fn invalidate(repo_path: &str) {
    let Ok(repo) = validate_repo(repo_path) else {
        return;
    };
    let mut cache = vault_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.remove(&repo);
}

fn with_vault<T>(repo_path: &str, f: impl FnOnce(&Vault) -> T) -> Result<T, String> {
    let repo = validate_repo(repo_path)?;
    {
        let cache = vault_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cached) = cache.get(&repo) {
            return Ok(f(&cached.vault));
        }
    }
    let (vault, status) = build_vault(&repo)?;
    let result = f(&vault);
    let mut cache = vault_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.insert(repo, CachedVault { vault, status });
    Ok(result)
}

/// Repo-wide markdown full-text search.
pub fn search(repo_path: &str, query: &str, limit: usize) -> Result<Vec<SearchHit>, String> {
    let limit = limit.clamp(1, 200);
    with_vault(repo_path, |vault| vault.search(query, limit))
}

/// Every unresolved note link in the vault.
pub fn broken_links(repo_path: &str) -> Result<Vec<BrokenLink>, String> {
    with_vault(repo_path, |vault| {
        vault
            .broken_links()
            .into_iter()
            .map(|(source, target)| BrokenLink { source, target })
            .collect()
    })
}

/// Notes that link at `path`.
pub fn backlinks(repo_path: &str, path: &str) -> Result<Vec<Backlink>, String> {
    with_vault(repo_path, |vault| vault.backlinks(path))
}

/// Force-directed doc graph payload (coords in a 1000×1000 extent).
pub fn graph(
    repo_path: &str,
    focus: Option<&str>,
    depth: u32,
    tag: Option<&str>,
    folder: Option<&str>,
) -> Result<Graph, String> {
    with_vault(repo_path, |vault| {
        let query = GraphQuery {
            focus,
            depth,
            tag,
            folder,
        };
        Graph::build(vault, &query)
    })
}

pub fn status(repo_path: &str) -> Result<DocsStatus, String> {
    // Prefer the cached build status (including truncated / skips); rebuild
    // only when cold. Never invent `truncated: false` after a capped index.
    let repo = validate_repo(repo_path)?;
    {
        let cache = vault_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cached) = cache.get(&repo) {
            return Ok(DocsStatus {
                note_count: cached.vault.notes().len() as u32,
                truncated: cached.status.truncated,
                skipped_oversized: cached.status.skipped_oversized,
                skipped_unreadable: cached.status.skipped_unreadable,
            });
        }
    }
    let (vault, status) = build_vault(&repo)?;
    let out = DocsStatus {
        note_count: vault.notes().len() as u32,
        truncated: status.truncated,
        skipped_oversized: status.skipped_oversized,
        skipped_unreadable: status.skipped_unreadable,
    };
    let mut cache = vault_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.insert(
        repo,
        CachedVault {
            vault,
            status: out.clone(),
        },
    );
    Ok(out)
}

/// `git mv` plus staged link rewrites via `rewrite_links_in`.
///
/// Reviewable in the diff viewer before commit. Does **not** call
/// `Vault::rename_note`.
pub fn rename_doc(repo_path: &str, from: &str, to: &str) -> Result<DocRenameOutcome, String> {
    let repo = validate_repo(repo_path)?;
    let from = normalize_rel(from)?;
    let to = normalize_rel(to)?;
    if from == to {
        return Err("source and destination paths are the same".into());
    }
    if !has_markdown_extension(&from) || !has_markdown_extension(&to) {
        return Err("doc rename only moves markdown notes".into());
    }

    let (vault, _status) = build_vault(&repo)?;
    let Some(source_index) = vault.notes().iter().position(|n| n.path == from) else {
        return Err(format!("note not in vault: {from}"));
    };
    if vault.notes().iter().any(|n| n.path == to) {
        return Err(format!("destination already exists in vault: {to}"));
    }
    // Filesystem authority: refuse overwriting an on-disk file the index
    // somehow missed.
    let to_abs = sandbox_join(&repo, &to)?;
    if to_abs.exists() {
        return Err(format!("destination already exists on disk: {to}"));
    }

    let old_stem = stem(&from);
    let new_stem = stem(&to);
    let new_path = strip_markdown_extension(&to).to_string();
    let stem_ambushed = vault.notes().iter().enumerate().any(|(index, note)| {
        index != source_index && stem(&note.path).eq_ignore_ascii_case(&new_stem)
    });

    let mut edits: Vec<(String, String, u32)> = Vec::new();
    for (index, note) in vault.notes().iter().enumerate() {
        if index == source_index {
            continue;
        }
        let source_note_path = note.path.clone();
        let mut resolve_wiki = |written: &str| -> Option<String> {
            // Same rule as MarkDev's rename_note: rewrite only when the
            // written target resolves to the note being moved.
            let resolved = vault.resolve(written, None)?;
            if resolved.path != from {
                return None;
            }
            if written.eq_ignore_ascii_case(&old_stem) {
                (!stem_ambushed)
                    .then(|| new_stem.clone())
                    .or(Some(new_path.clone()))
            } else {
                Some(new_path.clone())
            }
        };
        let mut resolve_markdown = |written: &str| -> Option<String> {
            let resolved = vault.resolve_from(&source_note_path, written, None)?;
            if resolved.path != from {
                return None;
            }
            Some(new_path.clone())
        };

        let protected = ProtectedRanges::for_document(&note.text);
        let (text, links) = rewrite_links_in(
            &note.text,
            &protected,
            &mut resolve_wiki,
            &mut resolve_markdown,
        );
        if links > 0 {
            if text.len() > DEFAULT_MAX_NOTE_BYTES {
                return Err(format!(
                    "rewrite of {} exceeds the note size limit",
                    note.path
                ));
            }
            edits.push((note.path.clone(), text, links as u32));
        }
    }

    // Move first so a failed mv leaves rewrites unapplied.
    GitWriter::mv_file(repo_path, &from, &to)?;

    let mut links_rewritten = 0u32;
    let mut rewritten_paths = Vec::with_capacity(edits.len());
    for (path, text, links) in edits {
        sandbox_write(repo_path, &path, &text)?;
        GitWriter::stage_file(repo_path, &path)?;
        links_rewritten = links_rewritten.saturating_add(links);
        rewritten_paths.push(path);
    }

    invalidate(repo_path);
    let _ = refresh(repo_path);

    Ok(DocRenameOutcome {
        from,
        to,
        rewritten_paths,
        links_rewritten,
    })
}

fn normalize_rel(path: &str) -> Result<String, String> {
    let trimmed = path.trim().replace('\\', "/");
    if trimmed.is_empty() || trimmed.contains('\0') || trimmed.starts_with('/') {
        return Err("path must be a non-empty repository-relative path".into());
    }
    if trimmed
        .split('/')
        .any(|c| c.is_empty() || c == "." || c == "..")
    {
        return Err("path must not contain '.' or '..' components".into());
    }
    Ok(trimmed)
}

fn build_vault(repo: &Path) -> Result<(Vault, DocsStatus), String> {
    let paths = list_markdown_paths(repo)?;
    let truncated = paths.len() > MAX_DOC_FILES;
    let take = paths.len().min(MAX_DOC_FILES);

    let mut notes = Vec::with_capacity(take);
    let mut skipped_oversized = 0u32;
    let mut skipped_unreadable = 0u32;

    for rel in paths.into_iter().take(take) {
        let abs = repo.join(&rel);
        let meta = match std::fs::metadata(&abs) {
            Ok(m) => m,
            Err(_) => {
                skipped_unreadable = skipped_unreadable.saturating_add(1);
                continue;
            }
        };
        if meta.len() as usize > DEFAULT_MAX_NOTE_BYTES {
            skipped_oversized = skipped_oversized.saturating_add(1);
            continue;
        }
        let text = match std::fs::read_to_string(&abs) {
            Ok(t) => t,
            Err(_) => {
                skipped_unreadable = skipped_unreadable.saturating_add(1);
                continue;
            }
        };
        notes.push(Note::parse(rel, &text));
    }

    let status = DocsStatus {
        note_count: notes.len() as u32,
        truncated,
        skipped_oversized,
        skipped_unreadable,
    };
    Ok((Vault::build(repo.to_path_buf(), notes), status))
}

/// Tracked markdown paths from `git ls-files`, never a filesystem walk.
fn list_markdown_paths(repo: &Path) -> Result<Vec<String>, String> {
    let stdout = git_text(
        repo,
        &[
            "-c",
            "core.quotepath=off",
            "ls-files",
            "-z",
            "--",
            "*.md",
            "*.mdx",
            "*.markdown",
        ],
    )?;
    let mut paths: Vec<String> = stdout
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(|s| s.replace('\\', "/"))
        .filter(|s| has_markdown_extension(s))
        .collect();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{git_in, git_repo, write};

    fn fixture_repo() -> tempfile::TempDir {
        let dir = git_repo();
        write(
            dir.path(),
            "docs/Guide.md",
            "# Guide\n\nSee [[Roadmap]] and [intro](Intro.md).\n",
        );
        write(
            dir.path(),
            "docs/Intro.md",
            "# Intro\n\nBack to [Guide](Guide.md).\n\n```md\n[[Roadmap]]\n```\n",
        );
        write(dir.path(), "docs/Roadmap.md", "# Roadmap\n\nPlan items.\n");
        write(
            dir.path(),
            "README.md",
            "# Repo\n\nDocs in [Guide](docs/Guide.md).\n",
        );
        // Ignored markdown must not enter the vault.
        write(dir.path(), ".gitignore", "vendor/\n");
        write(dir.path(), "vendor/Secret.md", "# Secret\n[[Guide]]\n");
        git_in(dir.path(), &["add", "docs", "README.md", ".gitignore"]);
        git_in(dir.path(), &["commit", "-m", "docs fixture"]);
        dir
    }

    #[test]
    fn vault_comes_from_git_ls_files_not_the_filesystem_walk() {
        let dir = fixture_repo();
        let path = dir.path().to_str().unwrap();
        let status = refresh(path).expect("refresh");
        // Four tracked notes; vendor/Secret.md is ignored.
        assert_eq!(status.note_count, 4, "{status:?}");
        assert_eq!(status.skipped_unreadable, 0);

        let hits = search(path, "Plan", 10).expect("search");
        assert!(hits.iter().any(|h| h.path == "docs/Roadmap.md"), "{hits:?}");

        let broken = broken_links(path).expect("broken");
        assert!(
            broken.is_empty(),
            "fixture links should resolve: {broken:?}"
        );

        let backs = backlinks(path, "docs/Roadmap.md").expect("backlinks");
        assert!(backs.iter().any(|b| b.path == "docs/Guide.md"), "{backs:?}");

        let g = graph(path, None, 0, None, None).expect("graph");
        assert_eq!(g.total_notes, 4);
        assert_eq!(g.nodes.len(), 4);
        assert!(!g.edges.is_empty());
        // Deterministic layout: same vault → same coords.
        let g2 = graph(path, None, 0, None, None).expect("graph again");
        assert_eq!(g.nodes, g2.nodes);
    }

    #[test]
    fn ignored_markdown_never_enters_the_vault() {
        let dir = fixture_repo();
        let path = dir.path().to_str().unwrap();
        let _ = refresh(path).expect("refresh");
        let hits = search(path, "Secret", 10).expect("search");
        assert!(hits.iter().all(|h| !h.path.contains("vendor")), "{hits:?}");
    }

    #[test]
    fn rename_rewrites_links_and_protects_fenced_samples() {
        let dir = fixture_repo();
        let path = dir.path().to_str().unwrap();
        let outcome = rename_doc(path, "docs/Roadmap.md", "docs/Plan.md").expect("rename");
        assert_eq!(outcome.from, "docs/Roadmap.md");
        assert_eq!(outcome.to, "docs/Plan.md");
        assert!(outcome.links_rewritten >= 1, "{outcome:?}");
        assert!(
            outcome.rewritten_paths.iter().any(|p| p == "docs/Guide.md"),
            "{outcome:?}"
        );

        let guide = std::fs::read_to_string(dir.path().join("docs/Guide.md")).unwrap();
        assert!(
            guide.contains("[[Plan]]") || guide.contains("[[docs/Plan]]"),
            "wikilink should move: {guide}"
        );
        let intro = std::fs::read_to_string(dir.path().join("docs/Intro.md")).unwrap();
        // Fenced sample must keep the old spelling.
        assert!(
            intro.contains("```md\n[[Roadmap]]\n```"),
            "code fence must be protected: {intro}"
        );

        // Staged for review — not committed.
        let status = git_text(dir.path(), &["status", "--porcelain"]).unwrap();
        assert!(
            status.contains("Plan.md") || status.contains("Roadmap.md"),
            "rename should be staged/visible: {status}"
        );
    }

    #[test]
    fn refresh_invalidates_and_rebuilds() {
        let dir = fixture_repo();
        let path = dir.path().to_str().unwrap();
        assert_eq!(refresh(path).unwrap().note_count, 4);
        write(dir.path(), "docs/Extra.md", "# Extra\n");
        git_in(dir.path(), &["add", "docs/Extra.md"]);
        // Cached vault is stale until refresh.
        invalidate(path);
        assert_eq!(refresh(path).unwrap().note_count, 5);
    }

    #[test]
    fn status_retains_honesty_fields_from_refresh() {
        let dir = fixture_repo();
        let path = dir.path().to_str().unwrap();
        let refreshed = refresh(path).expect("refresh");
        let reported = status(path).expect("status");
        assert_eq!(reported.note_count, refreshed.note_count);
        assert_eq!(reported.truncated, refreshed.truncated);
        assert_eq!(reported.skipped_oversized, refreshed.skipped_oversized);
        assert_eq!(reported.skipped_unreadable, refreshed.skipped_unreadable);
    }
}
