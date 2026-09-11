//! The three freshness digests a repo map carries, computed by the kernel.
//!
//! `generated_head`, `indexed_hash` and `content_fingerprint` are the fields
//! `RepoMapper.map_is_stale` compares to decide whether `dev map` needs to run
//! again. They used to be computed in Python and handed to `devmap manifest` as
//! flags, which cost a `git ls-files` pass and a `stat` walk in the interpreter
//! on every hook invocation — the second time the same tree had just been
//! enumerated, because the build in front of it had already walked it.
//!
//! Computing them here is only useful if the answers are *identical*, so this
//! module is a deliberate transcription of
//! `RepoMapper.get_git_files` / `_files_fingerprint` / `_content_fingerprint`
//! and `indexing.graph.build.content_fingerprint`, down to the `\0` separator,
//! the `c2:` scheme prefix and the `(size, mtime_ns, ctime_ns)` memo key.
//! `tests/freshness_parity.rs` runs both implementations over a real tree and
//! compares the strings; anything here that drifts from Python fails there.
//!
//! **Only the git path is implemented.** Python falls back to an `os.walk` when
//! git cannot answer, and that fallback stays the Python side's: a second
//! transcription of a *different* enumeration is where the two would silently
//! disagree, and the case it serves — a project that is not a git repository —
//! has no performance problem to solve. When git cannot answer, this module
//! reports `InventorySource::Unavailable` and computes nothing, and the caller
//! leaves the stamping to Python exactly as before.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use devmap_extract::subprocess::{self, run_bounded, Bounds};
use serde::{Deserialize, Serialize};

use crate::digest::{hex, sha1_hex, Blake2b};

/// Bumped whenever the digest algorithm changes, and identical to
/// `indexing.graph.build._CONTENT_SCHEME`: a fingerprint stamped under an older
/// scheme must never compare equal to one computed under a newer one.
pub const CONTENT_SCHEME: &str = "c2";

/// Read size for file digests, matching `_HASH_CHUNK` in the Python writer.
/// The value does not affect the digest — only how much of a file is resident
/// while it is hashed.
const HASH_CHUNK: usize = 1 << 20;

/// Directory (and file) names that are never part of the indexed inventory.
/// Transcribed from `repo_mapper._GENERATED_DIR_NAMES`.
///
/// Both state directory names are appended by [`is_generated_dir_name`] rather
/// than listed here, so the set can never drift from
/// [`devmap_extract::paths::STATE_DIR_NAMES`].
const GENERATED_DIR_NAMES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".gitnexus",
    ".pytest_cache",
    ".ruff_cache",
    ".mypy_cache",
    ".tox",
    ".nox",
    ".venv",
    "venv",
    ".virtualenv",
    "site-packages",
    "node_modules",
    "bower_components",
    ".pnpm-store",
    ".yarn",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".astro",
    ".parcel-cache",
    ".turbo",
    ".gradle",
    ".idea",
    ".vscode-test",
    ".terraform",
    "coverage",
    ".coverage",
    "htmlcov",
    ".nyc_output",
    "__snapshots__",
    ".cache",
    ".sass-cache",
    ".eggs",
    "vendor",
    "third_party",
    "Pods",
    "DerivedData",
    "Carthage",
];

/// Binary / archive / artifact suffixes, from `repo_mapper._GENERATED_SUFFIXES`.
/// Compared against the lowercased file name, as Python does.
const GENERATED_SUFFIXES: &[&str] = &[
    ".tgz", ".whl", ".tar.gz", ".tar.bz2", ".tar.xz", ".zip", ".jar", ".war", ".so", ".dylib",
    ".dll", ".a", ".o", ".obj", ".lib", ".exe", ".class", ".wasm", ".bin", ".dat", ".db",
    ".sqlite", ".sqlite3", ".pack", ".idx", ".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico",
    ".bmp", ".tiff", ".pdf", ".mp4", ".mov", ".mp3", ".wav", ".woff", ".woff2", ".ttf", ".eot",
    ".pyd", ".pyo", ".min.js", ".min.css", ".map",
];

/// `(include_untracked, max_indexed_files)` — `RepoMapper._inventory_limits`.
///
/// Supplied by the caller rather than read from `.devcouncil/config.yaml`: the
/// two values live in a YAML document with the project's whole configuration in
/// it, and a hand-rolled reader for two scalars would be a second, weaker parser
/// whose disagreements with the real one show up as a silently different file
/// set. The defaults are the same defaults `_inventory_limits` falls back to
/// when the config cannot be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InventoryLimits {
    pub include_untracked: bool,
    pub max_indexed_files: usize,
}

impl Default for InventoryLimits {
    fn default() -> Self {
        Self {
            include_untracked: true,
            max_indexed_files: 50_000,
        }
    }
}

/// Where an inventory came from, so a caller can tell "git said this" from
/// "git could not be asked".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InventorySource {
    Git,
    /// git is absent, failed, or this is not a repository. The reason travels
    /// with it: a caller that cannot enumerate must say why, not answer with an
    /// empty list.
    Unavailable(String),
}

/// The indexed file inventory, plus what it had to drop to stay bounded.
#[derive(Debug, Clone)]
pub struct Inventory {
    /// Sorted, repository-relative, forward-slashed.
    pub files: Vec<String>,
    pub source: InventorySource,
    /// `Some(total_before_cap)` when `max_indexed_files` bit. Carried rather
    /// than logged away: a capped inventory fingerprints a subset of the tree,
    /// and a caller comparing that fingerprint deserves to know it is one.
    pub capped_from: Option<usize>,
}

impl Inventory {
    pub fn is_available(&self) -> bool {
        matches!(self.source, InventorySource::Git)
    }
}

/// The three digests, each `None` when it could not be computed.
///
/// `None` is not `""`: an empty `indexed_hash` written as though it were a
/// result is exactly the confusion the artifacts' `unavailable` markers exist to
/// prevent, and `map_is_stale` treats an empty stamp as "no check performed".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreshnessDigests {
    pub generated_head: Option<String>,
    pub indexed_hash: Option<String>,
    pub content_fingerprint: Option<String>,
    /// Why the digests are absent, when they are. Empty when they were computed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub unavailable_reason: String,
}

/// `git rev-parse HEAD`, or the empty string when it cannot be answered.
///
/// Empty rather than an error, matching `RepoMapper._git_head`: a repository
/// with no commits still gets a usable map, and staleness then rests on the two
/// fingerprints.
pub fn git_head(root: &Path) -> String {
    git_head_with_program(OsStr::new("git"), root)
}

/// Wall clock allowed for `git rev-parse HEAD`: the same constant the store
/// applies to its own `HEAD` sentinel, re-exported so the two cannot drift.
pub use devmap_extract::subprocess::GIT_HEAD_DEADLINE;

/// Wall clock allowed for one `git ls-files` pass. Generous: the listing is
/// the whole tracked tree, and a monorepo's is seconds, not milliseconds.
pub const GIT_LS_FILES_DEADLINE: Duration = Duration::from_secs(30);

/// Bytes of `git ls-files -z` output kept. A listing past this is refused as
/// incomplete rather than hashed as if it were the tree — a fingerprint of a
/// prefix would report a stale map fresh.
pub const GIT_LS_FILES_OUTPUT_CAP: usize = 64 * 1024 * 1024;

/// [`git_head`] with the program named, so a test can stand a script in for it.
#[doc(hidden)]
pub fn git_head_with_program(program: &OsStr, root: &Path) -> String {
    let mut command = subprocess::git_with_program(program, root);
    command.args(["rev-parse", "HEAD"]);
    let bounds = Bounds {
        deadline: GIT_HEAD_DEADLINE,
        stdout_cap: 4096,
        stderr_cap: 4096,
    };
    match run_bounded(&mut command, bounds) {
        Ok(captured) if captured.status.success() && !captured.stdout_truncated => {
            captured.stdout_lossy().trim().to_string()
        }
        // Not a repository, no commits, no git, or a git that stalled past
        // the deadline: all "cannot be answered", per the contract above.
        Ok(_) | Err(_) => String::new(),
    }
}

/// `RepoMapper._is_runtime_or_generated_file`.
///
/// The membership test runs over *every* path segment including the file name,
/// as Python's `set(normalized.split("/"))` does — a file literally named
/// `vendor` is excluded, and that is deliberate rather than incidental, because
/// the two sides have to agree on it.
/// True for a path segment that names a generated directory.
///
/// The state directories are folded in here rather than listed in
/// [`GENERATED_DIR_NAMES`] so that adding or renaming one is a single edit in
/// `devmap_extract::paths`. Both names are excluded at once: a repository
/// mid-migration has both, and fingerprinting either would make the content
/// digest depend on the size of the store it is meant to describe.
fn is_generated_dir_name(part: &str) -> bool {
    GENERATED_DIR_NAMES.contains(&part) || devmap_extract::paths::is_state_dir_name(part)
}

pub fn is_runtime_or_generated_file(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let name = normalized.rsplit('/').next().unwrap_or("");
    let lower_name = name.to_lowercase();

    if normalized.split('/').any(|part| part == "__pycache__") || normalized.ends_with(".pyc") {
        return true;
    }
    if normalized.split('/').any(is_generated_dir_name) {
        return true;
    }
    // `dist`/`build` are only generated at the top level; a source directory
    // literally named `build` (e.g. `src/…/graph/build`) must not be dropped,
    // so these stay prefix checks rather than segment ones.
    if normalized.starts_with("dist/")
        || normalized.starts_with("build/")
        || normalized.starts_with("out/")
        || normalized.starts_with("target/")
    {
        return true;
    }
    if GENERATED_SUFFIXES
        .iter()
        .any(|suffix| lower_name.ends_with(suffix))
    {
        return true;
    }
    if name.starts_with("tmp") || name.starts_with("temp") || name.starts_with(".tmp") {
        return true;
    }
    name.ends_with('~')
}

pub(crate) fn ls_files(
    program: &OsStr,
    root: &Path,
    flags: &[&str],
) -> Result<Vec<String>, String> {
    let mut command = subprocess::git_with_program(program, root);
    command.arg("ls-files").arg("-z").args(flags);
    let bounds = Bounds {
        deadline: GIT_LS_FILES_DEADLINE,
        stdout_cap: GIT_LS_FILES_OUTPUT_CAP,
        stderr_cap: 4096,
    };
    let captured = run_bounded(&mut command, bounds)
        .map_err(|failure| format!("git ls-files {}: {failure}", flags.join(" ")))?;
    if !captured.status.success() {
        return Err(format!(
            "git ls-files {} exited {}: {}",
            flags.join(" "),
            captured.status.code().unwrap_or(-1),
            captured.stderr_trimmed()
        ));
    }
    if captured.stdout_truncated {
        // A prefix of the tree fingerprints as a different tree, and a map
        // compared against it would read stale as fresh. Refused by name.
        return Err(format!(
            "git ls-files {} produced more than {} bytes; the listing is \
             incomplete and was not fingerprinted",
            flags.join(" "),
            GIT_LS_FILES_OUTPUT_CAP
        ));
    }
    // `-z` so non-ASCII paths arrive unquoted; lossy decoding matches the
    // Python reader's `errors="replace"`, so a path neither side can decode
    // still hashes to the same string on both.
    Ok(captured
        .stdout_lossy()
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .map(|entry| entry.replace('\\', "/"))
        .collect())
}

/// `RepoMapper._keep`: the paths git listed that the map can actually cover.
///
/// Three filters, cheapest first, and the order is only about cost — each one
/// excludes, so the set they leave is the same whichever way round they run.
///
/// The middle one is the one that has to be here rather than in
/// `is_runtime_or_generated_file`: that predicate is a pure function of a path
/// string, and whether a directory carries a `CACHEDIR.TAG` can only be answered
/// by opening it. Discovery already prunes tagged directories whole
/// (`devmap_extract::collect_sources_with_report`), so a file inside one is
/// never indexed, never queued, and can never affect an answer the map gives —
/// while counting it here moved `content_fingerprint` on every write a build
/// made into its own cache, and the map then read stale on files it had
/// deliberately declined to index. `--if-stale`, `--watch` and `verify` rebuilt
/// forever without converging.
///
/// A cache that *is* gitignored never reaches this: `git ls-files` does not list
/// it. The gap is a tagged cache no ignore rule happens to cover — measured on
/// this workspace, `rust-port/.gitignore` carries `/target/`, which does not
/// match the `target-lane*` directories beside it, and 8,252 of the 9,654 paths
/// the old rule kept came from them. Dropping them took `inventory()` on this
/// repository from 2.33s to 0.75s, because a path ruled out here never pays for
/// its `stat`.
///
/// [`NotRepoRelative`] excludes too. `git ls-files` run at the root of a work
/// tree cannot emit an absolute path or one carrying `..`, so this is a
/// contract, not a live branch — but a path no ancestor of which could be
/// opened is a path that was *not* evaluated, and letting it through would make
/// it indistinguishable from one that was evaluated and cleared. Discovery
/// cannot index a path outside the root either way.
///
/// [`NotRepoRelative`]: devmap_extract::CacheVerdict::NotRepoRelative
fn keep_indexable(
    root: &Path,
    caches: &mut devmap_extract::CacheDirectoryCache,
    paths: Vec<String>,
) -> Vec<String> {
    paths
        .into_iter()
        .filter(|path| {
            if is_runtime_or_generated_file(path) {
                return false;
            }
            if !matches!(
                caches.tagged_ancestor(root, path),
                devmap_extract::CacheVerdict::Outside
            ) {
                return false;
            }
            // A tracked symlink whose target resolves outside the repository is
            // the second shape of the disagreement this function exists to
            // close. `git ls-files` lists it — it is an ordinary mode-120000
            // entry — and the `is_file()` below follows it, so the inventory
            // counted a path discovery refuses (`DiscoverySkipReason::
            // EscapesRoot`) and hashed bytes the map does not describe.
            // Measured before this line: editing a file *outside* the
            // repository, through an in-tree symlink, moved the repository's
            // content fingerprint from `c2:496ee980…` to `c2:8bcb1a2b…` while
            // `devmap build` reported `files_indexed: 1,
            // discovery_refused_files: 1`.
            //
            // Through `devmap_extract::escapes_root` rather than a second
            // containment test here, for the reason the cache check above is
            // shared: two implementations of one rule is how the two walks come
            // to disagree again.
            if devmap_extract::escapes_root(root, &root.join(path)).is_some() {
                return false;
            }
            // Index entries whose working-tree file was deleted but not staged
            // are skipped, exactly as `_keep` does. Last because it is a `stat`
            // per surviving path, and the filters above have already dropped the
            // bulk of a tree whose build output is not ignored.
            root.join(path).is_file()
        })
        .collect()
}

/// `RepoMapper.get_git_files`, git path only.
pub fn inventory(root: &Path, limits: InventoryLimits) -> Inventory {
    inventory_with_program(OsStr::new("git"), root, limits)
}

/// [`inventory`] with the program named, so a test can stand a script in for it.
#[doc(hidden)]
pub fn inventory_with_program(program: &OsStr, root: &Path, limits: InventoryLimits) -> Inventory {
    // One memo for both `ls-files` passes: the tagged-ancestor lookup costs one
    // `open` per *distinct directory* rather than one per path, and it stops at
    // the first tagged prefix — so a cache directory holding 50,000 files is
    // opened once and its contents are never probed at all.
    let mut caches = devmap_extract::CacheDirectoryCache::default();
    let mut keep = |paths: Vec<String>| keep_indexable(root, &mut caches, paths);

    let tracked = match ls_files(program, root, &["--cached"]) {
        Ok(paths) => keep(paths),
        Err(reason) => {
            return Inventory {
                files: Vec::new(),
                source: InventorySource::Unavailable(reason),
                capped_from: None,
            }
        }
    };
    let untracked = if limits.include_untracked {
        match ls_files(program, root, &["--others", "--exclude-standard"]) {
            Ok(paths) => {
                let tracked_set: std::collections::HashSet<&str> =
                    tracked.iter().map(String::as_str).collect();
                keep(paths)
                    .into_iter()
                    .filter(|path| !tracked_set.contains(path.as_str()))
                    .collect()
            }
            Err(reason) => {
                return Inventory {
                    files: Vec::new(),
                    source: InventorySource::Unavailable(reason),
                    capped_from: None,
                }
            }
        }
    } else {
        Vec::new()
    };

    let (files, capped_from) = cap_inventory(tracked, untracked, limits.max_indexed_files);
    Inventory {
        files,
        source: InventorySource::Git,
        capped_from,
    }
}

/// `RepoMapper._cap_inventory`: bound the inventory, dropping untracked first.
///
/// git lists both sets in path order and UTF-8 byte order agrees with code-point
/// order, so truncating before the sort cuts the same entries Python's
/// `sorted(tracked[:max])` cuts.
fn cap_inventory(
    tracked: Vec<String>,
    untracked: Vec<String>,
    max_indexed_files: usize,
) -> (Vec<String>, Option<usize>) {
    let max_files = max_indexed_files.max(1);
    let total = tracked.len() + untracked.len();
    if total <= max_files {
        let mut files = tracked;
        files.extend(untracked);
        files.sort();
        return (files, None);
    }
    if tracked.len() >= max_files {
        let mut files = tracked;
        files.truncate(max_files);
        files.sort();
        return (files, Some(total));
    }
    let room = max_files - tracked.len();
    let mut files = tracked;
    files.extend(untracked.into_iter().take(room));
    files.sort();
    (files, Some(total))
}

/// `RepoMapper._files_fingerprint`: SHA-1 over the sorted paths, newline joined.
pub fn files_fingerprint(files: &[String]) -> String {
    let mut sorted: Vec<&str> = files.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sha1_hex(sorted.join("\n").as_bytes())
}

/// One memo entry: `[stat key, digest]`, the shape Python persists.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ContentCache {
    scheme: String,
    entries: BTreeMap<String, Vec<String>>,
}

/// Where the content-hash memo lives for `root`.
///
/// Resolved through `devmap_extract::paths` rather than hardcoded, so it lands
/// in the *same* state directory as the store this build writes. Two memos of
/// one computation — one under `.devcouncil/`, one under `.devmap/` — is how the
/// two implementations come to disagree about which digest belongs to which stat
/// key. It stays advisory in both directions: an absent, unreadable or
/// foreign-scheme cache costs a rehash and nothing else.
fn content_cache_path(root: &Path) -> PathBuf {
    devmap_extract::paths::content_cache_path(root)
}

fn load_content_cache(root: &Path) -> BTreeMap<String, Vec<String>> {
    let Ok(text) = std::fs::read_to_string(content_cache_path(root)) else {
        return BTreeMap::new();
    };
    let Ok(cache) = serde_json::from_str::<ContentCache>(&text) else {
        return BTreeMap::new();
    };
    if cache.scheme != CONTENT_SCHEME {
        return BTreeMap::new();
    }
    cache.entries
}

fn save_content_cache(root: &Path, entries: &BTreeMap<String, Vec<String>>) {
    let path = content_cache_path(root);
    let payload = ContentCache {
        scheme: CONTENT_SCHEME.to_string(),
        entries: entries.clone(),
    };
    // Python appends a trailing newline; matching it keeps the file
    // byte-identical whichever side last wrote it, which is one less spurious
    // diff for anyone looking at the tree.
    let Ok(mut text) = serde_json::to_string(&payload) else {
        return;
    };
    text.push('\n');
    if std::fs::create_dir_all(path.parent().unwrap_or(Path::new("."))).is_err() {
        return;
    }
    // Through the artifact writer so a crashed process cannot leave a partial
    // memo for the next run to read as authoritative.
    let _ = crate::artifacts::write_atomic(&path, text.as_bytes());
}

/// A stat key identical to `indexing.graph.build._stat_key`:
/// `"{size}:{mtime_ns}:{ctime_ns}"`.
///
/// `ctime_ns` is what makes the key safe — unlike `mtime_ns` it cannot be
/// back-dated by `utime`, `cp -p`, `rsync --times` or tar extraction, so a
/// rewrite that restores the old mtime still moves ctime and can never present
/// the key of the content it replaced.
#[cfg(unix)]
fn stat_key(meta: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    let mtime_ns = meta.mtime() as i128 * 1_000_000_000 + meta.mtime_nsec() as i128;
    let ctime_ns = meta.ctime() as i128 * 1_000_000_000 + meta.ctime_nsec() as i128;
    format!("{}:{}:{}", meta.len(), mtime_ns, ctime_ns)
}

/// Off unix there is no `st_ctime`, so the key cannot be the one Python writes.
/// A key the other side rejects costs a rehash on each crossing and never a
/// wrong digest, because the digest itself is recomputed from the bytes.
#[cfg(not(unix))]
fn stat_key(meta: &std::fs::Metadata) -> String {
    format!("{}:-:-", meta.len())
}

fn file_digest(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Blake2b::new(16);
    let mut buffer = vec![0u8; HASH_CHUNK];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finish()))
}

/// `indexing.graph.build.content_fingerprint`: `"c2:" + sha1` over sorted
/// `"{rel}\0{digest}"` lines, with per-file digests memoised behind a stat key.
///
/// `persist_cache=false` reads the memo and never writes it, for callers that
/// must not modify the project.
pub fn content_fingerprint(root: &Path, files: &[String], persist_cache: bool) -> String {
    let cache = load_content_cache(root);
    let mut entries: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut lines: Vec<String> = Vec::with_capacity(files.len());
    let mut recomputed = false;

    let mut sorted: Vec<&str> = files.iter().map(String::as_str).collect();
    sorted.sort_unstable();

    for rel in sorted {
        let path = root.join(rel);
        let Ok(meta) = std::fs::metadata(&path) else {
            // Distinct from every real digest, and stable while it stays gone.
            lines.push(format!("{rel}\0-"));
            continue;
        };
        let key = stat_key(&meta);
        let digest = match cache.get(rel) {
            Some(cached) if cached.len() == 2 && cached[0] == key => cached[1].clone(),
            _ => match file_digest(&path) {
                Ok(digest) => {
                    recomputed = true;
                    digest
                }
                Err(_) => {
                    lines.push(format!("{rel}\0-"));
                    continue;
                }
            },
        };
        entries.insert(rel.to_string(), vec![key, digest.clone()]);
        lines.push(format!("{rel}\0{digest}"));
    }

    if persist_cache && (recomputed || entries.keys().ne(cache.keys())) {
        save_content_cache(root, &entries);
    }
    format!("{CONTENT_SCHEME}:{}", sha1_hex(lines.join("\n").as_bytes()))
}

/// All three digests from one snapshot of the tree.
///
/// One snapshot on purpose: a field written by one rule and read by another is
/// worse than none at all, because it can read fresh when it is not.
pub fn compute(root: &Path, limits: InventoryLimits, persist_cache: bool) -> FreshnessDigests {
    let inventory = inventory(root, limits);
    match &inventory.source {
        InventorySource::Unavailable(reason) => FreshnessDigests {
            unavailable_reason: format!("git file inventory unavailable: {reason}"),
            ..Default::default()
        },
        InventorySource::Git => FreshnessDigests {
            generated_head: Some(git_head(root)).filter(|head| !head.is_empty()),
            indexed_hash: Some(files_fingerprint(&inventory.files)),
            content_fingerprint: Some(content_fingerprint(root, &inventory.files, persist_cache)),
            unavailable_reason: String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_paths_are_excluded_the_way_python_excludes_them() {
        for path in [
            "src/__pycache__/x.pyc",
            "a/b.pyc",
            "node_modules/left-pad/index.js",
            ".venv/lib/python3.12/site.py",
            "dist/bundle.js",
            "build/out.o",
            "target/release/devmap",
            "out/x.ts",
            "assets/logo.PNG",
            "web/app.min.js",
            "src/tmpfile.py",
            "src/temp_thing.py",
            "src/backup.py~",
            // The file name is a path segment too: `set(split("/"))` in Python
            // includes it, so a file called `vendor` is excluded.
            "third_party/x.py",
            "src/vendor",
        ] {
            assert!(
                is_runtime_or_generated_file(path),
                "{path} must be excluded"
            );
        }
        for path in [
            "src/devcouncil/indexing/graph/build.py",
            "rust-port/crates/devmap-query/src/freshness.rs",
            // `build`/`dist`/`out`/`target` only count at the top level.
            "src/graph/build/x.py",
            "pkg/dist/index.ts",
            "uv.lock",
            "Cargo.lock",
            "README.md",
        ] {
            assert!(!is_runtime_or_generated_file(path), "{path} must be kept");
        }
    }

    #[test]
    fn the_inventory_cap_drops_untracked_first_and_says_it_capped() {
        let tracked: Vec<String> = (0..5).map(|index| format!("t{index}.py")).collect();
        let untracked: Vec<String> = (0..5).map(|index| format!("u{index}.py")).collect();

        let (all, capped) = cap_inventory(tracked.clone(), untracked.clone(), 10);
        assert_eq!(all.len(), 10);
        assert_eq!(capped, None);

        let (partial, capped) = cap_inventory(tracked.clone(), untracked.clone(), 7);
        assert_eq!(capped, Some(10));
        assert!(partial
            .iter()
            .all(|path| path.starts_with('t') || ["u0.py", "u1.py"].contains(&path.as_str())));
        assert_eq!(partial.len(), 7);

        let (tracked_only, capped) = cap_inventory(tracked, untracked, 3);
        assert_eq!(capped, Some(10));
        assert_eq!(tracked_only, vec!["t0.py", "t1.py", "t2.py"]);

        // `max(1)`, as Python does: a zero cap must still return something
        // rather than fingerprint an empty tree as though it were the whole one.
        let (one, _) = cap_inventory(vec!["a.py".into()], vec!["b.py".into()], 0);
        assert_eq!(one, vec!["a.py"]);
    }

    #[test]
    fn the_files_fingerprint_is_sha1_over_the_sorted_newline_joined_paths() {
        let files = vec!["b.py".to_string(), "a.py".to_string()];
        assert_eq!(files_fingerprint(&files), sha1_hex(b"a.py\nb.py"));
        // Order of the input must not reach the digest.
        let reversed = vec!["a.py".to_string(), "b.py".to_string()];
        assert_eq!(files_fingerprint(&files), files_fingerprint(&reversed));
    }

    #[test]
    fn a_missing_file_fingerprints_as_a_dash_and_stays_stable() {
        let dir = std::env::temp_dir().join(format!(
            "devmap-freshness-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("present.py"), b"x = 1\n").unwrap();
        let files = vec!["present.py".to_string(), "gone.py".to_string()];

        let first = content_fingerprint(&dir, &files, false);
        let second = content_fingerprint(&dir, &files, false);
        assert_eq!(first, second, "an absent file must not move the digest");
        assert!(first.starts_with("c2:"), "{first}");

        // The digest is over bytes, so a byte-identical rewrite must not move
        // it even though it moves mtime and ctime.
        std::fs::write(dir.join("present.py"), b"x = 1\n").unwrap();
        assert_eq!(content_fingerprint(&dir, &files, false), first);
        std::fs::write(dir.join("present.py"), b"x = 2\n").unwrap();
        assert_ne!(content_fingerprint(&dir, &files, false), first);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unavailable_inventory_computes_no_digest_and_says_why() {
        let dir = std::env::temp_dir().join(format!(
            "devmap-freshness-nogit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // A directory that is not a work tree. `git ls-files` exits non-zero
        // there, and the answer must be "could not be computed", never an empty
        // inventory fingerprinted as though the repository held no files.
        let digests = compute(&dir, InventoryLimits::default(), false);
        if digests.unavailable_reason.is_empty() {
            // A developer machine whose temp directory happens to sit inside a
            // repository would make this vacuous; skip rather than assert a
            // condition the environment decides.
            eprintln!("skipped: {} is inside a git work tree", dir.display());
        } else {
            assert_eq!(digests.indexed_hash, None);
            assert_eq!(digests.content_fingerprint, None);
            assert!(
                digests.unavailable_reason.contains("git file inventory"),
                "{}",
                digests.unavailable_reason
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
