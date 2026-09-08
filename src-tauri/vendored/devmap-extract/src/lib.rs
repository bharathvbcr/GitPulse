// The parsing frontend. Everything below the `parse` gate needs tree-sitter
// and its grammars; everything above it — the model types, language detection,
// go.mod parsing, the ignore rules — does not, and is what a *query* consumer
// actually uses.
pub mod cache;
pub mod clonesig;
#[cfg(feature = "parse")]
pub mod embedded;
pub mod fallback;
pub mod frameworks;
pub mod gomod;
#[cfg(feature = "parse")]
pub mod heritage;
#[cfg(feature = "parse")]
pub mod langcalls;
#[cfg(feature = "parse")]
pub(crate) mod langdecl;
// Every extractor here takes a `tree_sitter::Node`, and the only caller is
// `treesitter::extract_treesitter`, which is itself behind the gate.
#[cfg(feature = "parse")]
pub mod langimports;
pub mod languages;
pub mod model;
// Where state lives. Below the `parse` gate on purpose: a query-only consumer
// needs to find the store and the artifacts without linking a single grammar.
pub mod paths;
pub mod progress;
pub mod subprocess;
// Needs the grammars: a notebook's cells are reconstructed and then handed to
// the real extractor, so this module is only meaningful with `parse` on.
#[cfg(feature = "parse")]
pub mod notebook;
#[cfg(feature = "parse")]
pub mod treesitter;
pub mod wiring;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

// Ungated: discovery reads the tree in parallel whether or not the parsing
// frontend is compiled in, so `scan_tree` needs rayon in both shapes.
use rayon::prelude::*;

pub use gomod::{collect_go_modules, git_worktree_root, parse_go_mod, GoModule};
pub use languages::{declared_language_ids, detect_language, is_ignored_path, is_indexable_source};
pub use model::*;
pub use paths::{
    code_graph_path, compact_code_graph_path, content_cache_path, plugin_dir, repo_map_path,
    state_dir, store_path, workspace_path,
};
#[cfg(feature = "parse")]
pub use treesitter::{extract_treesitter, linked_grammar_count, linked_grammar_keys};

pub struct FileRef<'a> {
    pub path: &'a str,
    pub source: &'a str,
}

pub const MAX_SOURCE_BYTES: u64 = 1024 * 1024;

/// Marker file of the Cache Directory Tagging Standard.
pub const CACHEDIR_TAG_FILE: &str = "CACHEDIR.TAG";

/// The standard's mandatory first 43 bytes.
///
/// A directory is a cache directory if and only if it holds a `CACHEDIR.TAG`
/// whose content *begins* with exactly this. The signature is checked rather
/// than the filename alone so a source file that happens to be called
/// `CACHEDIR.TAG` cannot silently delete a subtree from the index.
pub const CACHEDIR_TAG_SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// Whether `dir` is tagged as a cache directory.
///
/// K7: the ignore rules matched a fixed list of directory *names* — `target`,
/// `node_modules`, `dist`, `build` — so a cargo output directory named anything
/// else was walked as source. Measured in generation 779 of this repository's
/// store: 1,041 of 2,363 indexed files were `.fingerprint/*.json` and
/// `.rustc_info.json` under `rust-port/target-serve` and `target-store`, and
/// the daemon had queued 47,000 pending rows from them. Neither was gitignored;
/// neither was named `target`.
///
/// Both carried a `CACHEDIR.TAG`. That is the point of the standard: the tool
/// that created the cache says so, so nothing downstream has to guess a name.
/// cargo, pip, uv, ccache, tox, ruff and pytest all write one.
///
/// A directory this returns true for is skipped whole — not walked, not
/// indexed, not queued. That is safe because the tag is an explicit,
/// machine-written declaration by the tool that owns the directory, and it is
/// checked by signature rather than by filename.
///
/// This is **not** a replacement for [`is_ignored_path`]: many caches carry no
/// tag (this workspace's own long-lived `target/` has none), so the two are
/// complementary. Absence of a tag says nothing.
pub fn is_cache_directory(dir: &Path) -> bool {
    use std::io::Read;

    let Ok(mut file) = fs::File::open(dir.join(CACHEDIR_TAG_FILE)) else {
        return false;
    };
    let mut head = vec![0u8; CACHEDIR_TAG_SIGNATURE.len()];
    // `read_exact`: a file shorter than the signature cannot carry it, and the
    // error path is the same "not a cache directory" answer.
    if file.read_exact(&mut head).is_err() {
        return false;
    }
    head == CACHEDIR_TAG_SIGNATURE
}

/// What [`CacheDirectoryCache::tagged_ancestor`] was able to determine.
///
/// Three states, not two, because "I opened every ancestor and none carried a
/// tag" and "I was handed something that is not a repo-relative path, so I
/// never opened anything" are different answers. Collapsing them into `None`
/// let a path the walk could not evaluate read as one it evaluated and
/// cleared — and the caller acts on that by indexing the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheVerdict {
    /// The path lies inside this repo-relative tagged cache directory.
    Inside(String),
    /// Every ancestor was opened; none carried a `CACHEDIR.TAG`.
    Outside,
    /// The path is not repo-relative, so no ancestor was opened. The payload
    /// says which rule it broke, for the caller's refusal message.
    NotRepoRelative(&'static str),
}

/// Memoised ancestor lookup for [`is_cache_directory`].
///
/// One `open` per directory rather than per path. Reconciling the pending queue
/// asks this of every row — 51,136 of them on the live store — and a repository
/// is a few thousand directories deep in total, so the memo turns
/// O(rows x depth) syscalls into O(distinct directories).
///
/// The memo is scoped to one root at a time. It was keyed on the repo-relative
/// prefix alone while the root arrived as a per-call parameter, so a single
/// instance asked about two roots answered the second from the first's cache:
/// a `pkg/` that is a cargo output tree in one checkout made an ordinary
/// `pkg/` in another checkout read as a build cache, and a directory this
/// answers `Inside` for is skipped whole — not walked, not indexed. Every
/// construction site today pairs one instance with one root, so this was
/// latent rather than live; it is a property of the type now instead of an
/// unwritten rule its callers happened to keep.
#[derive(Debug, Default)]
pub struct CacheDirectoryCache {
    root: Option<std::path::PathBuf>,
    verdict: std::collections::HashMap<String, bool>,
}

impl CacheDirectoryCache {
    /// The repo-relative tagged cache directory containing `relative`, if any.
    ///
    /// `relative` itself is checked too, so passing a directory answers for the
    /// directory. The repository root is deliberately **not** checked: a user
    /// who points `devmap build` at a tagged directory has asked for it, and
    /// refusing the whole tree would be a worse answer than indexing it.
    ///
    /// `relative` must be repo-relative and canonical. An absolute path or one
    /// carrying a `..` component is refused rather than walked, because
    /// `root.join(part)` is not a containment operation: `src/..` *is* the
    /// root, which defeats the exemption above, and `../sibling` leaves the
    /// repository entirely — both were reachable, and both returned the
    /// escaping string to the caller labelled as a repo-relative cache
    /// directory. `--affected` hands this raw command-line input, so the
    /// refusal is load-bearing rather than defensive.
    ///
    /// An empty component (`a//b`) is skipped and `.` is ignored, which leaves
    /// `""` and `"."` meaning the root itself: exempt, hence [`Outside`].
    ///
    /// [`Outside`]: CacheVerdict::Outside
    pub fn tagged_ancestor(&mut self, root: &Path, relative: &str) -> CacheVerdict {
        if relative.starts_with('/') {
            return CacheVerdict::NotRepoRelative("is an absolute path");
        }
        if self.root.as_deref() != Some(root) {
            self.verdict.clear();
            self.root = Some(root.to_path_buf());
        }
        let mut prefix = String::new();
        for part in relative.split('/') {
            if part.is_empty() || part == "." {
                continue;
            }
            if part == ".." {
                return CacheVerdict::NotRepoRelative("contains a `..` component");
            }
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            let tagged = match self.verdict.get(&prefix) {
                Some(known) => *known,
                None => {
                    let known = is_cache_directory(&root.join(&prefix));
                    self.verdict.insert(prefix.clone(), known);
                    known
                }
            };
            if tagged {
                return CacheVerdict::Inside(prefix);
            }
        }
        CacheVerdict::Outside
    }
}

/// The contents of a lock, whatever a panic elsewhere did to it.
///
/// E-8: the prune ledger below was read and written through
/// `if let Ok(guard) = lock.lock()`, which silently does *nothing* once the
/// mutex is poisoned — so a panic anywhere under the walk would erase every
/// pruned `CACHEDIR.TAG` subtree from `skipped_paths` and the report would then
/// describe a tree it had not walked, with nothing saying so. A ledger that
/// could not be read must not read as an empty ledger.
///
/// Recovering is right here rather than propagating: poisoning says a *writer*
/// panicked, not that the data is torn. `BTreeSet::insert` has no intermediate
/// state a panic can leave behind, so the set holds every directory recorded
/// before the panic, and reporting those is strictly better than reporting
/// none. The panic itself is not swallowed — it unwinds its own thread as
/// usual.
fn recover_lock<T>(lock: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod prune_ledger_tests {
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex};

    /// E-8: a poisoned ledger must not read as an empty one.
    ///
    /// The pruned-directory set is shared with `filter_entry`, which the
    /// `ignore` crate requires to be `Fn + Send + Sync`, so a mutex is the
    /// honest way to get an answer back out of it. Both ends of that mutex were
    /// spelled `if let Ok(guard) = lock.lock()`, which does *nothing at all*
    /// once a panic has poisoned it: every pruned `CACHEDIR.TAG` subtree would
    /// vanish from `skipped_paths`, and the report would describe a tree it had
    /// not walked with nothing saying so. On this repository that is 1,041 of
    /// 2,363 candidate paths.
    ///
    /// No end-to-end reproduction is possible today and that is deliberate
    /// rather than an omission: `collect_sources_with_report` builds the serial
    /// `Walk`, so a panic inside the closure unwinds out of the function that
    /// owns the mutex and the report is never read. The trap is one edit away —
    /// `build_parallel` is the obvious next move for discovery, and it runs the
    /// same closure on worker threads where one panic poisons the ledger the
    /// survivors keep writing to. This pins the policy at the only level where
    /// it is observable: the guard, and both branches of it.
    #[test]
    fn a_poisoned_prune_ledger_is_recovered_rather_than_silently_dropped() {
        let ledger: Arc<Mutex<BTreeSet<String>>> = Arc::new(Mutex::new(BTreeSet::new()));
        super::recover_lock(&ledger).insert("rust-port/target-serve".to_string());

        let writer = Arc::clone(&ledger);
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = writer.lock().unwrap();
            panic!("a walk callback panicked while holding the ledger");
        }));
        assert!(panicked.is_err(), "the fixture must actually panic");
        assert!(
            ledger.lock().is_err(),
            "precondition: the ledger is poisoned, which is the case the old \
             `if let Ok(..)` silently skipped"
        );

        let recovered = super::recover_lock(&ledger);
        assert_eq!(
            recovered.len(),
            1,
            "a directory recorded before the panic is still a directory that was \
             pruned, and the report has to say so"
        );
        assert!(recovered.contains("rust-port/target-serve"));
    }
}

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
///
/// "The same way" includes **tolerating the same broken files**. A rule file
/// is not all-or-nothing: `GitignoreBuilder::add` compiles every line it can
/// and reports the rest as a *partial* error (`ignore-0.4.33`,
/// `src/gitignore.rs:405-434` — the loop never breaks for a bad glob), which is
/// how `WalkBuilder` walks a tree whose `.gitignore` contains a typo like
/// `[z-a]` without raising anything. Treating that return as fatal made every
/// verdict under such a tree an `Err`, the watcher read the `Err` as
/// "ignored", and the incremental index froze while `status` went on reporting
/// `is_fresh: true` — the divergence this comment claims to prevent, in the
/// opposite direction and silent. The unusable lines are reported through
/// [`is_gitignored_reporting`] instead of being thrown away.
pub fn is_gitignored(root: &Path, path: &Path, is_dir: bool) -> anyhow::Result<bool> {
    Ok(is_gitignored_reporting(root, path, is_dir)?.0)
}

/// [`is_gitignored`], plus one diagnostic per rule line that could not be
/// compiled.
///
/// The verdict is computed from the lines that *did* compile, exactly as the
/// cold walker does. The diagnostics exist so a watcher can say which line of
/// which file it is not applying: a rule the developer wrote and the kernel
/// silently drops is precisely the kind of divergence that is invisible from
/// either side. Empty for a well-formed tree, so a caller pays nothing to
/// carry it.
pub fn is_gitignored_reporting(
    root: &Path,
    path: &Path,
    is_dir: bool,
) -> anyhow::Result<(bool, Vec<String>)> {
    let path_abs = path_under_root(root, path)?;
    let (matchers, problems) = ignore_matchers_for(root, &path_abs, is_dir)?;
    Ok((
        matches_ignore(&matchers, &path_abs, is_dir).unwrap_or(false),
        problems,
    ))
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
) -> anyhow::Result<(Vec<ignore::gitignore::Gitignore>, Vec<String>)> {
    let bases = ignore_rule_bases(root, path, is_dir)?;
    let mut matchers = Vec::new();
    let mut problems = Vec::new();
    for (base, rules) in &bases {
        add_ignore_rules(&mut matchers, &mut problems, base, rules)?;
    }
    Ok((matchers, problems))
}

/// Compile one rule file into a matcher, keeping every line that is valid.
///
/// The `Option<Error>` from `GitignoreBuilder::add` is a *partial* result, not
/// a verdict on the file: it holds one entry per line that failed to compile
/// while the builder retains all the others. It is also how "the file could not
/// be opened at all" is reported — in which case the builder is simply empty
/// and no rule applies, which is again what the cold walker does. Neither case
/// may abort the evaluation: an ignore verdict that fails is a verdict the
/// watcher reads as "ignored", so one typo would freeze the whole index.
fn add_ignore_rules(
    matchers: &mut Vec<ignore::gitignore::Gitignore>,
    problems: &mut Vec<String>,
    base: &Path,
    rules: &Path,
) -> anyhow::Result<()> {
    if !rules.is_file() {
        return Ok(());
    }
    let mut builder = ignore::gitignore::GitignoreBuilder::new(base);
    if let Some(error) = builder.add(rules) {
        problems.push(format!(
            "ignore rules {rules:?} are partly unusable and those lines are not \
             being applied: {error}"
        ));
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
    // Notebooks divert here rather than inside `extract_treesitter`, because
    // what they need is not a different grammar but a different *source*: the
    // code has to be reconstructed out of the JSON before any grammar sees it,
    // and the resulting spans relocated back into the raw file. Handing the
    // reconstructed buffer straight to the extractor would produce symbols
    // whose spans index a string that exists only in memory.
    if notebook::is_notebook(path) {
        return notebook::extract_notebook(path, source, extract_treesitter);
    }
    let lang = detect_language(Path::new(path));
    extract_treesitter(path, lang, source)
}

#[cfg(feature = "parse")]
pub fn extract_all(files: &[FileRef]) -> Vec<Extraction> {
    extract_all_with_progress(files, None)
}

#[cfg(feature = "parse")]
pub fn extract_all_with_progress(
    files: &[FileRef],
    progress: Option<&progress::FileProgress>,
) -> Vec<Extraction> {
    if let Some(progress) = progress {
        progress.start(files.len());
    }
    files
        .par_iter()
        .map(|f| {
            let extraction = extract_file(f.path, f.source);
            if let Some(progress) = progress {
                progress.finish_file(
                    false,
                    matches!(extraction.parse_outcome, ParseOutcome::Failed { .. }),
                );
            }
            extraction
        })
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

/// The path a walker error is *about*, when it is about one.
///
/// `ignore::Error` wraps the underlying failure in `WithPath`/`WithDepth`/
/// `WithLineNumber` layers and exposes no accessor for the path, so this
/// unwraps them. `Loop` names two paths — the ancestor and the child that
/// points back at it — and the child is the entry the walk actually stopped
/// on, which is the one to record.
///
/// `None` means the failure names no path at all, and the caller must not
/// treat it as an entry it can step over.
pub(crate) fn walk_error_path(error: &ignore::Error) -> Option<&Path> {
    match error {
        ignore::Error::WithPath { path, .. } => Some(path),
        ignore::Error::WithDepth { err, .. } | ignore::Error::WithLineNumber { err, .. } => {
            walk_error_path(err)
        }
        ignore::Error::Loop { child, .. } => Some(child),
        ignore::Error::Partial(errors) => errors.iter().find_map(walk_error_path),
        _ => None,
    }
}

/// Where a symlinked candidate actually points, when that is outside `root`.
///
/// Public because the *freshness inventory* has to ask the same question and
/// get the same answer. `freshness::keep_indexable` and this walk each decide
/// what the map covers; two implementations of "does this path leave the
/// repository" is how they come to disagree, and a disagreement there makes a
/// map permanently stale on a file it deliberately never indexed.
///
/// `None` for anything that is not a symlink — the ordinary case, and one stat
/// — and for a symlink whose target resolves inside the repository, which is
/// the monorepo's shared config or vendored header and whose bytes the walk
/// reaches under their real name regardless.
///
/// Both roots are canonicalised before the comparison. A lexical prefix test
/// answers "inside" for `../../elsewhere/secret.py` and for any root reached
/// through a symlink of its own, which on macOS is every path under
/// `std::env::temp_dir()`.
///
/// A dangling link answers `Some`: its target is definitely not there, which is
/// a fact about the link. A link that will not resolve for any *other* reason —
/// a loop, a parent that lost `+x`, a stale mount — answers `None` here,
/// because containment is then unknown rather than disproven, and [`candidate_kind`]
/// reports it as `Undecidable` so the caller can decide what unknown costs it.
/// A caller that needs "not indexable" rather than "proven outside" must still
/// test the path itself; `freshness::keep_indexable`'s trailing `is_file()`
/// does exactly that.
pub fn escapes_root(root: &Path, path: &Path) -> Option<String> {
    match candidate_kind(root, path) {
        CandidateKind::Refused(DiscoverySkipReason::EscapesRoot { target }) => Some(target),
        _ => None,
    }
}

/// What discovery makes of one path, before anything reads its bytes.
///
/// The variants a caller must not collapse into each other are the point:
/// "refused", "absent" and "the stat could not run" have three different
/// consequences, and every place that answered them with one `bool` got at
/// least one of them wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateKind {
    /// A regular file, or a symlink to one that stays inside the repository.
    /// An ordinary source candidate under its own name.
    ///
    /// `bytes` is the size of the bytes that would actually be read — the
    /// *target's* through a link. `ignore::DirEntry::metadata()` reports the
    /// link's own size instead (a path length, tens of bytes), which is how a
    /// 30 MB file reached `read_to_string` under a symlink whose stat said it
    /// was nowhere near `MAX_SOURCE_BYTES`.
    File { bytes: u64 },
    /// A real directory. The walk descends it; the drain expands it.
    Directory,
    /// A symlink to a directory inside the repository. Neither path descends a
    /// link, and the files under the target are reached under their real names,
    /// so this path is not itself work.
    LinkedDirectory,
    /// A socket, fifo or device — or a link to one. Never a source.
    Other,
    /// The path leaves the repository, or will not resolve to say either way.
    /// Carries the reason discovery records, so both callers report one thing.
    Refused(DiscoverySkipReason),
    /// Nothing is there. The *only* verdict that may be read as a deletion.
    Absent,
    /// The filesystem could not answer. Containment is unknown, and unknown is
    /// not absence: ELOOP, EACCES on a parent, ESTALE on a mount all land here.
    Undecidable(String),
}

/// The one owner of "is this path something this repository contains, and what
/// is it?" — for the cold walk in [`collect_sources_with_report`] and for the
/// drain's `classify_pending_entry` alike.
///
/// The two used to answer it separately and disagreed about exactly one shape.
/// The cold walk kept a symlinked file whose target resolves inside the root
/// (a monorepo's shared config, a vendored header: the bytes are the
/// repository's either way) and refused only one that escapes it. The drain
/// refused **every** symlink, as "not a regular file or directory". Measured:
/// a cold build indexed `src/util.py -> shared/util.py`, and the next drain of
/// that path deleted the queued row as structurally unprocessable — so the
/// edit was dropped, the stored extraction went stale, and `status` went on
/// reporting fresh. The cold walk's rule is the one that survives, and it is
/// stated here once.
///
/// One `symlink_metadata` for the ordinary case, which is what `is_file()`
/// cost before — and unlike `is_file()` it does not collapse a dangling link,
/// a link loop and a link to a directory into the same silent `false`.
pub fn candidate_kind(root: &Path, path: &Path) -> CandidateKind {
    let link = match fs::symlink_metadata(path) {
        Ok(link) => link,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return CandidateKind::Absent,
        Err(error) => return CandidateKind::Undecidable(error.to_string()),
    };
    if !link.file_type().is_symlink() {
        return kind_of(&link);
    }
    // Both roots are canonicalised before the comparison. A lexical prefix test
    // answers "inside" for `../../elsewhere/secret.py` and for any root reached
    // through a symlink of its own, which on macOS is every path under
    // `std::env::temp_dir()`.
    //
    // Fail-closed when the target will not resolve. Containment is then
    // *unknown*, and unknown must not be recorded as proven-inside — but which
    // kind of unknown decides what the drain may do about it, so the two are
    // kept apart here rather than collapsed into one refusal.
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let target = match path.canonicalize() {
        Ok(target) => target,
        // The link dangles. That is a fact about the link, not about this
        // attempt: the target is not there, a full build writes no rows for the
        // path, and the drain may remove any it already wrote.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return CandidateKind::Refused(DiscoverySkipReason::EscapesRoot {
                target: format!("target could not be resolved: {error}"),
            })
        }
        // Every other resolution failure — a loop, a parent that lost `+x`, a
        // stale handle on a mount — is a question that could not be answered.
        // The walk records it as a refusal (below) because it read nothing; the
        // drain retries it, because *its* verdict deletes the file's rows and a
        // wrongly-dropped row is a symbol the dead-code pass is then free to
        // call unreferenced.
        Err(error) => return CandidateKind::Undecidable(error.to_string()),
    };
    if !target.starts_with(&canonical_root) {
        return CandidateKind::Refused(DiscoverySkipReason::EscapesRoot {
            target: target.to_string_lossy().into_owned(),
        });
    }
    match fs::metadata(&target) {
        // Inside the repository, so the link is transparent — except for a
        // directory, which neither path descends through a link.
        Ok(metadata) if metadata.is_dir() => CandidateKind::LinkedDirectory,
        Ok(metadata) => kind_of(&metadata),
        // `canonicalize` resolved it a syscall ago, so this is a race or a mode
        // change, not proof the target was never there.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => CandidateKind::Absent,
        Err(error) => CandidateKind::Undecidable(error.to_string()),
    }
}

fn kind_of(metadata: &fs::Metadata) -> CandidateKind {
    if metadata.is_dir() {
        CandidateKind::Directory
    } else if metadata.is_file() {
        CandidateKind::File {
            bytes: metadata.len(),
        }
    } else {
        CandidateKind::Other
    }
}

/// A discovered tree: every source discovery admitted, and what it refused.
///
/// Discovery and extraction used to be a single step, so the only way to learn
/// what a tree *contains* was to extract it — and the unchanged check, which
/// needs nothing but `(path, content_hash)`, paid a full extraction round-trip
/// per file to get it. Measured on this repository (1,311 files): 213–254 ms of
/// a ~300 ms no-op scan went into cache lookups and JSON deserialization whose
/// entire result was then discarded. Splitting discovery from extraction lets a
/// caller reach the same verdict from hashes alone.
#[derive(Debug, Default)]
pub struct ScannedTree {
    /// `(repo-relative path, source)` for every admitted file, sorted by path.
    pub sources: Vec<(String, String)>,
    /// Every candidate discovery admitted or refused.
    pub report: DiscoveryReport,
}

impl ScannedTree {
    /// `(path, content_hash)` for every admitted file.
    ///
    /// This is exactly the identity an `Extraction` carries. Every construction
    /// site sets `content_hash: content_hash(source)` over the same bytes
    /// discovery read (`treesitter.rs` — the parsed, refused and unavailable
    /// arms alike; `notebook.rs` builds on the same base), and `file_path` is
    /// the discovery path. A cached payload is only ever returned for a key
    /// built from those same bytes, and only when its `file_path` matches. So a
    /// caller comparing these pairs against a stored generation reaches the
    /// verdict extraction would have produced, without extracting.
    pub fn file_hashes(&self) -> Vec<(&str, u64)> {
        self.sources
            .par_iter()
            .map(|(path, source)| (path.as_str(), content_hash(source)))
            .collect()
    }

    /// Whether this tree is, file for file, the one `previous` describes.
    ///
    /// Fails closed on any disagreement in either direction: a differing file
    /// count, a path only one side holds, or a single differing hash. The
    /// comparison is per path rather than over a multiset of hashes because a
    /// rename with byte-identical content leaves the count and every hash
    /// intact while changing the graph.
    pub fn matches_file_hashes(&self, previous: &BTreeMap<String, u64>) -> bool {
        previous.len() == self.sources.len() && self.file_delta(previous).is_unchanged()
    }

    pub fn file_delta(&self, previous: &BTreeMap<String, u64>) -> progress::FileDelta {
        let mut delta = progress::FileDelta::default();
        for (path, hash) in self.file_hashes() {
            match previous.get(path) {
                None => delta.added += 1,
                Some(stored) if *stored == hash => delta.unchanged += 1,
                Some(_) => delta.changed += 1,
            }
        }
        delta.removed = previous
            .len()
            .saturating_sub(delta.changed + delta.unchanged);
        delta
    }
}

/// Collect source files and report each admitted or rejected candidate.
/// Gitignored paths are rejected by the walker before they become candidates.
pub fn collect_sources_with_report(
    root: &Path,
) -> anyhow::Result<(Vec<(String, String)>, DiscoveryReport)> {
    let scanned = scan_tree(root)?;
    Ok((scanned.sources, scanned.report))
}

/// Walk `root` and read every admitted source.
///
/// The walk is sequential — it is one `readdir` chain, and the gitignore
/// matcher it drives is stateful — but the reads are not, and doing them inline
/// in the walk loop made discovery single-threaded over the whole corpus.
/// Splitting the two costs one `PathBuf` per candidate and buys the read
/// parallelism. Ordering is not at stake: both outputs are sorted by path
/// before this returns, exactly as they were when the reads were inline.
pub fn scan_tree(root: &Path) -> anyhow::Result<ScannedTree> {
    scan_tree_with_progress(root, None)
}

pub fn scan_tree_with_progress(
    root: &Path,
    progress: Option<&progress::FileProgress>,
) -> anyhow::Result<ScannedTree> {
    let (candidates, mut report) = walk_candidates(root)?;
    if let Some(progress) = progress {
        progress.start(candidates.len());
    }

    let read: Vec<Result<(String, String), (String, DiscoverySkipReason)>> = candidates
        .into_par_iter()
        .map(|(relative, absolute)| {
            let result = match fs::read_to_string(&absolute) {
                Ok(source) => Ok((relative, source)),
                Err(error) => Err((
                    relative,
                    DiscoverySkipReason::Unreadable {
                        reason: error.to_string(),
                    },
                )),
            };
            if let Some(progress) = progress {
                progress.finish_file(false, result.is_err());
            }
            result
        })
        .collect();

    let mut sources = Vec::with_capacity(read.len());
    for outcome in read {
        match outcome {
            Ok((relative, source)) => {
                report.yielded_paths.push(relative.clone());
                sources.push((relative, source));
            }
            Err(skip) => report.skipped_paths.push(skip),
        }
    }

    sources.sort_by(|left, right| left.0.cmp(&right.0));
    report.yielded_paths.sort();
    report
        .skipped_paths
        .sort_by(|left, right| left.0.cmp(&right.0));
    Ok(ScannedTree { sources, report })
}

/// The walk half of [`scan_tree`]: every candidate that survived the ignore
/// rules, the extension test and the size ceiling, as `(relative, absolute)`.
fn walk_candidates(root: &Path) -> anyhow::Result<(Vec<(String, PathBuf)>, DiscoveryReport)> {
    // A root that exists and is not a directory was never examined. `ignore`
    // walks a file root by yielding the file itself, so a build pointed at a
    // regular file "ran" with zero candidates and wrote a generation that read
    // as an empty repository — the check-that-could-not-run reporting as one
    // that ran, in the exact shape the fatal-root rule below refuses for a root
    // it cannot open. Measured through the release binary: `devmap build
    // <file>` exited 0 with `files_indexed: 0`; `devmap build <missing>` exited
    // 1. A missing root is left to the walker, whose error already names it;
    // `metadata` follows links, so a link to a directory is a directory.
    if let Ok(metadata) = fs::metadata(root) {
        if !metadata.is_dir() {
            anyhow::bail!(
                "{}: not a directory; a build root must be a directory",
                root.display()
            );
        }
    }

    let mut out = Vec::new();
    let mut report = DiscoveryReport::default();

    // K7: prune tagged cache directories at the directory, not per file.
    //
    // `filter_entry` returning false for a directory stops the walk descending
    // into it, so a cargo output tree costs one `open` instead of a stat and an
    // extension test for each of its tens of thousands of files. Doing it here
    // rather than in `is_indexable_source` is deliberate: that predicate is a
    // pure function of a path string, and this question can only be answered by
    // reading the filesystem.
    //
    // The pruned directories are recorded so the report can say what was
    // skipped wholesale. `Arc<Mutex<_>>` because `filter_entry` takes a
    // `Fn + Send + Sync + 'static`, and this is the honest way to get an answer
    // back out of it.
    let pruned: std::sync::Arc<std::sync::Mutex<std::collections::BTreeSet<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeSet::new()));
    let walk_root = root.to_path_buf();
    let pruned_writer = std::sync::Arc::clone(&pruned);
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .filter_entry(move |entry| {
            if !entry.file_type().is_some_and(|kind| kind.is_dir()) {
                return true;
            }
            // Never prune the root the caller asked for: pointing devmap at a
            // tagged directory is a request, not an accident.
            if entry.path() == walk_root {
                return true;
            }
            if !is_cache_directory(entry.path()) {
                return true;
            }
            if let Ok(relative) = entry.path().strip_prefix(&walk_root) {
                recover_lock(&pruned_writer).insert(relative.to_string_lossy().replace('\\', "/"));
            }
            false
        })
        .build();

    for result in walker {
        let entry = match result {
            Ok(entry) => entry,
            // A path below the root that the walker could not descend into or
            // stat. Recorded and stepped over, not fatal.
            //
            // The `?` that used to be here made one `chmod 000` directory cost
            // the entire map: measured on a tree of eight ordinary sources plus
            // one unreadable directory, `devmap build` exited 1 with a single
            // JSON error and `status` then reported `node_count: 0`, "nothing
            // has been indexed yet". An unreadable *file* three lines below is
            // recorded as `Unreadable` and skipped, and the build describes the
            // rest of the repository — two spellings of "this indexer could not
            // read that path", one a hole and one a dead build. A root-owned
            // build directory, an object pack with odd modes or a mount that
            // lost `+x` are ordinary; none of them is a reason to refuse to
            // describe everything else.
            //
            // The root itself stays fatal, below: a root that could not be
            // opened was never examined, and answering "no sources here" for
            // one is a check that could not run reporting as a check that ran.
            Err(error) => {
                let named = walk_error_path(&error)
                    .and_then(|path| path.strip_prefix(root).ok().map(Path::to_path_buf))
                    .map(|relative| relative.to_string_lossy().replace('\\', "/"))
                    .filter(|relative| !relative.is_empty());
                match named {
                    Some(relative) => report.skipped_paths.push((
                        relative,
                        DiscoverySkipReason::Unreadable {
                            reason: error.to_string(),
                        },
                    )),
                    // Either the error names the root, or it names nothing at
                    // all — a `.gitignore` this walk could not read, say. Both
                    // are about the pass as a whole rather than about one entry
                    // under it, and neither can be stepped over.
                    None => return Err(error.into()),
                }
                continue;
            }
        };
        let p = entry.path();
        // What this path is, at the one owner the drain also asks
        // ([`candidate_kind`]) — and asked *before* anything follows the link.
        // `is_file()` follows it and answers `false` for a dangling link, a
        // link loop and a link to a directory alike, so the report said nothing
        // at all about a `src/x.py -> /nowhere`: not indexed, not refused, not
        // mentioned.
        let candidate = match candidate_kind(root, p) {
            // The candidate, sized by the bytes that would actually be read.
            CandidateKind::File { bytes } => Ok(bytes),
            // Recorded under the link's own name, once past the source gate.
            CandidateKind::Refused(reason) => Err(reason),
            // A path this walk was pointed at and could not read is the same
            // hole as one it was refused, whatever the errno: `Unreadable` is
            // what the walker's own errors are already recorded as.
            CandidateKind::Undecidable(reason) => Err(DiscoverySkipReason::Unreadable { reason }),
            // Stepped over without a word, exactly as `!is_file()` did: a real
            // directory (the walk descends it itself), a link to one (neither
            // path descends a link, and its files are reached under their real
            // names), a socket, and a path that vanished between the walker
            // naming it and this stat.
            CandidateKind::Directory
            | CandidateKind::LinkedDirectory
            | CandidateKind::Other
            | CandidateKind::Absent => continue,
        };
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
        // Containment, from the verdict already taken above.
        //
        // `preview` refuses a `--file` that "resolves outside the indexed
        // repository root". This walk enforced nothing: `WalkBuilder` is
        // configured not to *descend* through symlinks, so a symlinked
        // directory was never a way out — but `Path::is_file` and
        // `fs::read_to_string` both follow a link, so a single symlinked file
        // was read through without anyone asking where it pointed.
        //
        // Measured: a repository holding `src/creds.py -> <outside>/credentials.py`
        // indexed that file's symbols, reported `discovery_refused_files: 0`,
        // and `preview --file src/creds.py` then refused to show the very
        // symbols the build had just written — the two halves of one tool
        // disagreeing about where the repository ends, with the half that reads
        // the bytes being the permissive one.
        //
        // Recorded after the `is_indexable_source` gate above, deliberately: a
        // `node_modules -> /shared/node_modules` is not a source file, and
        // charging every repository that has one with permanent coverage loss
        // would make the marker useless.
        let bytes = match candidate {
            Ok(bytes) => bytes,
            Err(reason) => {
                report.skipped_paths.push((rel_str, reason));
                continue;
            }
        };
        if bytes > MAX_SOURCE_BYTES {
            report.skipped_paths.push((
                rel_str,
                DiscoverySkipReason::Oversized {
                    bytes,
                    limit: MAX_SOURCE_BYTES,
                },
            ));
            continue;
        }
        out.push((rel_str, p.to_path_buf()));
    }
    // Record each pruned cache directory once, as `NonSource`: a build cache is
    // the ordinary case, like a README beside the code, not a gap in coverage.
    // Recording it at all is what keeps the report honest about the subtree it
    // did not walk.
    for directory in recover_lock(&pruned).iter() {
        report
            .skipped_paths
            .push((directory.clone(), DiscoverySkipReason::NonSource));
    }

    // Not sorted here: `scan_tree` sorts both outputs once the reads are in,
    // and the candidate order does not reach a caller.
    Ok((out, report))
}

/// Extract every indexable file under `root` (owned paths — no leaks).
#[cfg(feature = "parse")]
pub fn extract_tree(root: &Path) -> anyhow::Result<Vec<Extraction>> {
    Ok(extract_tree_with_report(root)?.0)
}

/// [`extract_tree`], keeping the discovery report it walked.
///
/// The one owner of the uncached whole-tree extraction. `devmap build --full`
/// wrote the walk, the `FileRef` mapping and the `extract_all` call out for
/// itself only because `extract_tree` dropped the report and the build needs it
/// to say what discovery refused — so the same three steps existed twice, and
/// the copy that was not this one was the one running in production. There is
/// now one, and the CLI calls it.
///
/// The report is returned rather than folded in: which skips are coverage loss
/// is [`crate::model::DiscoverySkipReason::is_refusal`]'s decision and the
/// caller's to act on, and this function has no business deciding what a
/// refusal costs a particular build.
#[cfg(feature = "parse")]
pub fn extract_tree_with_report(
    root: &Path,
) -> anyhow::Result<(Vec<Extraction>, crate::model::DiscoveryReport)> {
    let (sources, report) = collect_sources_with_report(root)?;
    let refs: Vec<FileRef> = sources
        .iter()
        .map(|(path, src)| FileRef {
            path: path.as_str(),
            source: src.as_str(),
        })
        .collect();
    Ok((extract_all(&refs), report))
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
