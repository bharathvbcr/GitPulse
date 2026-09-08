//! Repository doc vault: MarkDev `Vault` fed from `git ls-files`, not a walk.
//!
//! Git is the authority on which `.md` files belong in the repo (tracked,
//! non-ignored). Notes are parsed with `Note::parse` and indexed through
//! `Vault::build`. Rename uses the pure `rewrite_links_in` helper plus
//! GitPulse's git writer — never MarkDev's `rename_note`.

use crate::engine::git_cli::{
    git_text, sandbox_join, sandbox_join_canonical, sandbox_write, validate_repo,
};
use crate::engine::git_writer::GitWriter;
use markdev::vault::note::{
    has_markdown_extension, stem, strip_markdown_extension, MARKDOWN_EXTENSIONS,
};
use markdev::vault::rename::{rewrite_links_in, ProtectedRanges};
use markdev::vault::{Backlink, Graph, GraphQuery, Note, SearchHit, Vault, DEFAULT_MAX_NOTE_BYTES};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

/// Soft ceiling on markdown files held in one vault. Personal / project docs
/// sit far below this; a monorepo that vendors thousands of READMEs degrades
/// to a partial index rather than OOM.
const MAX_DOC_FILES: usize = 5_000;
const MAX_DOC_BYTES: usize = 32 * 1024 * 1024;
const MAX_CACHED_VAULTS: usize = 8;

/// Default search hit limit (also the UI honesty threshold).
pub const DEFAULT_SEARCH_LIMIT: usize = 50;

/// Cached vault plus the build-time honesty fields (`truncated`, skips).
/// `status()` must not invent `truncated: false` after a capped build.
struct CachedVault {
    vault: Vault,
    status: DocsStatus,
}

type VaultSlot = Arc<Mutex<Option<Arc<CachedVault>>>>;

struct CacheEntry {
    slot: VaultSlot,
    used_at: Instant,
}

fn vault_cache() -> &'static Mutex<HashMap<PathBuf, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A detached/evicted slot can finish for its existing callers but cannot
/// republish itself into the cache after invalidation. Work for a resident
/// repository shares one slot instead of racing duplicate cold builds.
fn vault_slot(repo: &Path) -> VaultSlot {
    let mut cache = vault_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(entry) = cache.get_mut(repo) {
        entry.used_at = Instant::now();
        return Arc::clone(&entry.slot);
    }
    if cache.len() >= MAX_CACHED_VAULTS {
        let oldest = cache
            .iter()
            .min_by_key(|(_, entry)| entry.used_at)
            .map(|(path, _)| path.clone());
        if let Some(path) = oldest {
            cache.remove(&path);
        }
    }
    let slot = Arc::new(Mutex::new(None));
    cache.insert(
        repo.to_path_buf(),
        CacheEntry {
            slot: Arc::clone(&slot),
            used_at: Instant::now(),
        },
    );
    slot
}

fn load_vault(repo_path: &str, force: bool) -> Result<Arc<CachedVault>, String> {
    let repo = validate_repo(repo_path)?;
    let slot = vault_slot(&repo);
    let mut cached = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !force {
        if let Some(value) = cached.as_ref() {
            return Ok(Arc::clone(value));
        }
    }
    let value = build_vault(&repo, cached.as_ref())?;
    *cached = Some(Arc::clone(&value));
    Ok(value)
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
    Ok(load_vault(repo_path, true)?.status.clone())
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
    let cached = load_vault(repo_path, false)?;
    Ok(f(&cached.vault))
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
    // Preserve the actual build's partial/skipped status on a cache hit.
    Ok(load_vault(repo_path, false)?.status.clone())
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

    let built = build_vault(&repo, None)?;
    let vault = &built.vault;
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

fn build_vault(
    repo: &Path,
    previous: Option<&Arc<CachedVault>>,
) -> Result<Arc<CachedVault>, String> {
    let started = Instant::now();
    let previous = previous.filter(|cached| cached.vault.root() == repo);
    let previous_notes: HashMap<&str, &Note> = previous
        .into_iter()
        .flat_map(|cached| cached.vault.notes())
        .map(|note| (note.path.as_str(), note))
        .collect();
    let paths = list_markdown_paths(repo)?;
    let mut truncated = paths.len() > MAX_DOC_FILES;
    let mut remaining = MAX_DOC_BYTES;
    let take = paths.len().min(MAX_DOC_FILES);

    let mut notes = Vec::with_capacity(take);
    let mut skipped_oversized = 0u32;
    let mut skipped_unreadable = 0u32;
    let mut parsed = 0usize;

    for rel in paths.into_iter().take(take) {
        let abs = match sandbox_join_canonical(repo, &rel) {
            Ok(path) => path,
            Err(_) => {
                skipped_unreadable = skipped_unreadable.saturating_add(1);
                continue;
            }
        };
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            #[cfg(target_os = "macos")]
            let nofollow = libc::O_NOFOLLOW_ANY;
            #[cfg(not(target_os = "macos"))]
            let nofollow = libc::O_NOFOLLOW;
            options.custom_flags(nofollow | libc::O_NONBLOCK);
        }
        let file = match options.open(&abs) {
            Ok(file) => file,
            Err(_) => {
                skipped_unreadable = skipped_unreadable.saturating_add(1);
                continue;
            }
        };
        let meta = match file.metadata() {
            Ok(meta) if meta.is_file() => meta,
            _ => {
                skipped_unreadable = skipped_unreadable.saturating_add(1);
                continue;
            }
        };
        if meta.len() > DEFAULT_MAX_NOTE_BYTES as u64 {
            skipped_oversized = skipped_oversized.saturating_add(1);
            continue;
        }
        if meta.len() > remaining as u64 {
            truncated = true;
            continue;
        }
        // Metadata can race a growing file; bound the actual read as well.
        let limit = DEFAULT_MAX_NOTE_BYTES.min(remaining);
        let mut bytes = Vec::new();
        if file.take(limit as u64 + 1).read_to_end(&mut bytes).is_err() {
            skipped_unreadable = skipped_unreadable.saturating_add(1);
            continue;
        }
        if bytes.len() > DEFAULT_MAX_NOTE_BYTES {
            skipped_oversized = skipped_oversized.saturating_add(1);
            continue;
        }
        if bytes.len() > remaining {
            truncated = true;
            continue;
        }
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => {
                skipped_unreadable = skipped_unreadable.saturating_add(1);
                continue;
            }
        };
        remaining -= text.len();
        // Compare exact bytes: size/mtime caches miss in-place edits, restored
        // timestamps and coarse clocks. Borrow unchanged notes until we know
        // whether the entire indexed snapshot can survive.
        if let Some(note) = previous_notes
            .get(rel.as_str())
            .filter(|note| note.text == text)
        {
            notes.push(Cow::Borrowed(*note));
        } else {
            parsed += 1;
            notes.push(Cow::Owned(Note::parse(rel, &text)));
        }
    }

    let status = DocsStatus {
        note_count: notes.len() as u32,
        truncated,
        skipped_oversized,
        skipped_unreadable,
    };
    let unchanged = previous.is_some_and(|cached| {
        parsed == 0 && cached.vault.notes().len() == notes.len() && cached.status == status
    });
    let value = if let Some(cached) = previous.filter(|_| unchanged) {
        Arc::clone(cached)
    } else {
        Arc::new(CachedVault {
            vault: Vault::build(
                repo.to_path_buf(),
                notes.into_iter().map(Cow::into_owned).collect(),
            ),
            status,
        })
    };
    log::debug!(target: "performance", "docs_refresh admitted={} parsed={} reused={} index_reused={} work_ms={}",
        value.status.note_count, parsed, value.status.note_count as usize - parsed,
        unchanged, started.elapsed().as_millis());
    Ok(value)
}

/// Tracked markdown paths from `git ls-files`, never a filesystem walk.
fn list_markdown_paths(repo: &Path) -> Result<Vec<String>, String> {
    let patterns: Vec<String> = MARKDOWN_EXTENSIONS
        .iter()
        .map(|extension| format!(":(icase)*.{extension}"))
        .collect();
    let args: Vec<&str> = ["-c", "core.quotepath=off", "ls-files", "-z", "--"]
        .into_iter()
        .chain(patterns.iter().map(String::as_str))
        .collect();
    let stdout = git_text(repo, &args)?;
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

    #[test]
    #[ignore = "manual before/after document refresh timing workload"]
    fn repeated_document_refresh_workload() {
        let dir = git_repo();
        let path = dir.path().to_str().unwrap();
        let body = "## Section\nText with [[note-000]] and #tag.\n\n".repeat(100);
        for n in 0..100 {
            write(
                dir.path(),
                &format!("note-{n:03}.md"),
                &format!("# Note {n}\n{body}"),
            );
        }
        git_in(dir.path(), &["add", "."]);
        refresh(path).unwrap();
        let mut timings = Vec::new();
        for _ in 0..15 {
            let started = Instant::now();
            assert_eq!(refresh(path).unwrap().note_count, 100);
            timings.push(started.elapsed().as_micros());
        }
        timings.sort_unstable();
        println!(
            "docs_refresh notes=100 unchanged_samples=15 median_us={} p95_us={} max_us={}",
            timings[7], timings[14], timings[14]
        );
    }

    #[test]
    fn unchanged_documents_reuse_the_indexed_snapshot_on_forced_refresh() {
        let dir = fixture_repo();
        let path = dir.path().to_str().unwrap();
        let before = load_vault(path, true).unwrap();
        let after = load_vault(path, true).unwrap();
        assert!(
            Arc::ptr_eq(&before, &after),
            "unchanged source must not allocate/rebuild the vault"
        );
    }

    #[test]
    fn incremental_refresh_detects_same_size_same_timestamp_edits() {
        let dir = fixture_repo();
        let path = dir.path().to_str().unwrap();
        let file_path = dir.path().join("docs/Guide.md");
        std::fs::write(&file_path, "# Guide\noldtoken\n").unwrap();
        let before = load_vault(path, true).unwrap();
        let modified = std::fs::metadata(&file_path).unwrap().modified().unwrap();
        std::fs::write(&file_path, "# Guide\nnewtoken\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&file_path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let after = load_vault(path, true).unwrap();
        assert!(!Arc::ptr_eq(&before, &after));
        let guide = after
            .vault
            .notes()
            .iter()
            .find(|note| note.path == "docs/Guide.md")
            .unwrap();
        assert!(guide.text.contains("newtoken"));
        assert!(!guide.text.contains("oldtoken"));
    }

    #[test]
    fn incremental_refresh_is_scoped_to_the_vault_root() {
        let first = fixture_repo();
        let second = fixture_repo();
        let before = build_vault(first.path(), None).unwrap();
        let after = build_vault(second.path(), Some(&before)).unwrap();
        assert!(!Arc::ptr_eq(&before, &after));
        assert_eq!(after.vault.root(), second.path());
    }

    #[test]
    fn tracked_document_extensions_match_the_parser_including_uppercase() {
        let dir = git_repo();
        let mut expected = Vec::new();
        for (i, extension) in markdev::vault::note::MARKDOWN_EXTENSIONS.iter().enumerate() {
            for (j, extension) in [extension.to_string(), extension.to_uppercase()]
                .iter()
                .enumerate()
            {
                let name = format!("docs/note-{i}-{j}.{extension}");
                write(dir.path(), &name, "# Document\n");
                expected.push(name);
            }
        }
        write(dir.path(), "docs/ordinary.txt", "# Not Markdown\n");
        git_in(dir.path(), &["add", "."]);
        write(dir.path(), "untracked.md", "# Untracked\n");
        expected.sort();
        assert_eq!(list_markdown_paths(dir.path()).unwrap(), expected);
    }

    #[test]
    fn incremental_refresh_matches_full_rebuild_through_file_churn() {
        let dir = git_repo();
        for n in 0..48 {
            write(
                dir.path(),
                &format!("note-{n:03}.md"),
                &format!("# Note {n}\n[[note-000]] #shared\n"),
            );
        }
        git_in(dir.path(), &["add", "."]);
        let mut previous = build_vault(dir.path(), None).unwrap();
        git_in(dir.path(), &["commit", "-qm", "document churn fixture"]);
        for step in 0..24 {
            let changed = format!("note-{step:03}.md");
            match step % 4 {
                0 => write(
                    dir.path(),
                    &changed,
                    &format!("# Changed {step}\n[[absent]] #different\n"),
                ),
                1 => {
                    git_in(dir.path(), &["rm", &changed]);
                }
                2 => {
                    git_in(
                        dir.path(),
                        &["mv", &changed, &format!("moved-{step:03}.md")],
                    );
                }
                _ => {
                    std::fs::write(dir.path().join(&changed), [0xff, 0xfe]).unwrap();
                }
            }
            let incremental = build_vault(dir.path(), Some(&previous)).unwrap();
            let full = build_vault(dir.path(), None).unwrap();
            assert_eq!(incremental.status, full.status, "step {step}");
            assert_eq!(incremental.vault.notes(), full.vault.notes(), "step {step}");
            assert_eq!(
                incremental.vault.broken_links(),
                full.vault.broken_links(),
                "step {step}"
            );
            assert_eq!(
                serde_json::to_value(incremental.vault.search("shared", 100)).unwrap(),
                serde_json::to_value(full.vault.search("shared", 100)).unwrap(),
                "step {step}"
            );
            previous = incremental;
        }
    }

    #[cfg(unix)]
    #[test]
    fn previously_cached_document_replaced_by_outside_symlink_is_removed() {
        let dir = fixture_repo();
        let previous = build_vault(dir.path(), None).unwrap();
        let outside = tempfile::tempdir().unwrap();
        write(outside.path(), "outside.md", "outside private body");
        std::fs::remove_file(dir.path().join("docs/Guide.md")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("outside.md"),
            dir.path().join("docs/Guide.md"),
        )
        .unwrap();
        let next = build_vault(dir.path(), Some(&previous)).unwrap();
        assert!(!next
            .vault
            .notes()
            .iter()
            .any(|note| note.path == "docs/Guide.md"));
        assert_eq!(next.status.skipped_unreadable, 1);
    }

    #[test]
    fn document_queries_do_not_hold_the_cross_repository_cache_lock() {
        let dir = fixture_repo();
        let path = dir.path().to_str().unwrap();
        refresh(path).unwrap();
        with_vault(path, |_| {
            assert!(
                vault_cache().try_lock().is_ok(),
                "search/graph work must not lock out every repository"
            );
        })
        .unwrap();
    }

    #[test]
    fn document_vault_cache_does_not_retain_every_repository_ever_opened() {
        let mut repos = Vec::new();
        for _ in 0..10 {
            let dir = git_repo();
            refresh(dir.path().to_str().unwrap()).unwrap();
            repos.push(dir);
        }
        let cache = vault_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(cache.len() <= 8, "retained {} vaults", cache.len());
    }

    #[test]
    fn a_slow_query_cannot_republish_a_vault_after_invalidation() {
        let dir = fixture_repo();
        let path = dir.path().to_str().unwrap();
        invalidate(path);
        with_vault(path, |_| {
            write(dir.path(), "docs/Guide.md", "# Guide\nnewlyupdatedtoken");
            invalidate(path);
            refresh(path).unwrap();
        })
        .unwrap();
        assert!(!search(path, "newlyupdatedtoken", 10).unwrap().is_empty());
    }

    #[test]
    fn document_vault_has_an_aggregate_byte_budget() {
        let dir = git_repo();
        let body = "a".repeat(1024 * 1024);
        for i in 0..34 {
            write(dir.path(), &format!("{i:02}.md"), &body);
        }
        git_in(dir.path(), &["add", "."]);
        let built = build_vault(dir.path(), None).unwrap();
        let vault = &built.vault;
        let status = &built.status;
        let bytes: usize = vault.notes().iter().map(|note| note.text.len()).sum();
        assert!(bytes <= 32 * 1024 * 1024, "retained {bytes} bytes");
        assert!(
            status.truncated,
            "a byte-capped vault must report incomplete coverage"
        );
    }

    #[cfg(unix)]
    #[test]
    fn document_vault_refuses_a_tracked_link_outside_the_repository() {
        let dir = git_repo();
        let outside = tempfile::tempdir().unwrap();
        write(outside.path(), "secret.md", "# Outside\nprivate-body");
        std::os::unix::fs::symlink(outside.path().join("secret.md"), dir.path().join("link.md"))
            .unwrap();
        git_in(dir.path(), &["add", "link.md"]);
        let built = build_vault(dir.path(), None).unwrap();
        let vault = &built.vault;
        let status = &built.status;
        assert!(vault.notes().is_empty());
        assert_eq!(status.skipped_unreadable, 1);
    }

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
