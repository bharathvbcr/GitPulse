//! Zero-copy content-addressed compiler cache for parallel agent worktrees.
//!
//! When an anchor build holds `<target>/<profile>/.cargo-lock`, Copy-on-Write
//! `target/` cloning must skip the build directory (`"anchor build in progress"`),
//! which otherwise forces parallel coding agents into cold rebuilds or `.cargo-lock`
//! contention. `build_cache` implements a two-stage Blake3 content-addressed
//! `RUSTC_WRAPPER` cache stored under `<git-common-dir>/gitpulse/build-cache/`:
//!
//! 1. **Stage 1 (Direct Key)**: Hashes the compiler toolchain identity (`rustc -vV`),
//!    normalized `rustc` flags (stripping worktree-local `--out-dir`, `-L`, and
//!    input prefixes while preserving `-C metadata` / `-C extra-filename` for
//!    `StableCrateId` soundness), sorted `CARGO_PKG_*` / `CARGO_CFG_*` /
//!    `CARGO_FEATURE_*` environment variables, the entry `.rs` file's Blake3
//!    digest, and the Blake3 digests of every `--extern` crate artifact.
//! 2. **Stage 2 (`.d` Dep-Info & `# env-dep` Verification)**: On a Stage 1 hit,
//!    verifies every file dependency recorded in `rustc`'s emitted `.d` dep-info
//!    (catching submodules, `include_str!`, `include_bytes!`, and `$OUT_DIR`
//!    build-script outputs) plus every `# env-dep:KEY=VAL` read via `env!` /
//!    `option_env!`. Any mismatch increments `stage2_invalidations` and falls
//!    through to a real `rustc` compile.
//! 3. **Zero-Copy Materialization**: Verified hits materialize `.rlib`, `.rmeta`,
//!    and dynamic library artifacts into Cargo's `--out-dir` via Copy-on-Write
//!    (`clonefile(2)` on macOS, `FICLONE` on Linux, atomic staged fallback),
//!    rewrite `.d` paths for the current worktree, and rewrite JSON `"artifact"`
//!    notifications on stderr so Cargo's pipelined compilation stays coherent.

use devmap_query::{blake3_hash, write_atomic};
use gitpulse_lib::engine::cow_clone::reflink_copy_file;
use gitpulse_lib::engine::git_cli::capture_command;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Environment variable overriding `<git-common-dir>/gitpulse/build-cache`.
pub const ENV_BUILD_CACHE_DIR: &str = "GITPULSE_BUILD_CACHE_DIR";

/// Environment variable naming the active worktree root for path normalization.
pub const ENV_WORKTREE_ROOT: &str = "GITPULSE_WORKTREE_ROOT";

/// Default maximum cache size used by `cache-stats --prune` when `--max-bytes` is omitted (4 GiB).
pub const DEFAULT_MAX_CACHE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Maximum size of a single JSON manifest or state file read from the cache (4 MiB).
const MAX_JSON_STATE_BYTES: u64 = 4 * 1024 * 1024;

/// Maximum size of a single `.rlib` / `.rmeta` / `.so` artifact cached (512 MiB).
const MAX_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;

/// Maximum nesting depth when expanding `@<rspfile>` response files.
const MAX_RSP_DEPTH: usize = 8;

/// Maximum size of a single `@<rspfile>` response file (4 MiB).
const MAX_RSP_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Maximum total expanded `rustc` arguments accepted after `@<rspfile>` expansion.
const MAX_EXPANDED_ARGS: usize = 65_536;

/// Sentinel recorded in `DirectManifest::env_deps["OUT_DIR"]` when every use of
/// `OUT_DIR` in the crate is proven to be a compile-time `include*!(concat!(env!("OUT_DIR"), ...))`
/// whose target file is already Blake3-verified in `DirectManifest::deps`.
const COMPILE_ONLY_OUT_DIR_SENTINEL: &str = "$GITPULSE_COMPILE_ONLY_OUT_DIR";

const BUILD_CACHE_SUBDIR: &str = "gitpulse/build-cache";
const STATS_FILE: &str = "stats.json";
const STATS_LOCK_FILE: &str = ".stats.lock";
const COMPILERS_FILE: &str = "compilers.json";

/// Persistent hit/miss/reflink counters stored in `<cache_dir>/stats.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildCacheCounters {
    pub hits: u64,
    pub zero_copy_hits: u64,
    pub misses: u64,
    pub passthroughs: u64,
    pub probes: u64,
    pub stage2_invalidations: u64,
    pub bytes_saved: u64,
    pub bytes_stored: u64,
}

/// Full build-cache inspection and optional pruning report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildCacheReport {
    pub cache_dir: String,
    pub counters: BuildCacheCounters,
    pub blob_count: usize,
    pub manifest_count: usize,
    pub total_blob_bytes: u64,
    pub pruned_blobs: usize,
    pub pruned_manifests: usize,
    pub pruned_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CompilerIdCache {
    entries: BTreeMap<String, CompilerIdRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompilerIdRecord {
    size: u64,
    mtime_ns: u128,
    vv_blake3: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
enum DepPathScope {
    CargoBuildOutRel(String),
    WorktreeRel(String),
    ExternalAbs(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct RecordedDepFile {
    scope: DepPathScope,
    blake3: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct RecordedArtifact {
    file_name: String,
    blob_blake3: String,
    size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DirectManifest {
    schema_version: u32,
    crate_name: String,
    recorded_out_dir: String,
    #[serde(default)]
    recorded_out_dir_canon: Option<String>,
    recorded_worktree_root: Option<String>,
    #[serde(default)]
    recorded_worktree_root_raw: Option<String>,
    recorded_cargo_out_dir: Option<String>,
    #[serde(default)]
    recorded_cargo_out_dir_canon: Option<String>,
    dep_file_name: String,
    dep_info_text: String,
    deps: Vec<RecordedDepFile>,
    env_deps: BTreeMap<String, Option<String>>,
    artifacts: Vec<RecordedArtifact>,
    stderr_bytes: Vec<u8>,
}

/// Parsed `.d` dep-info dependencies and `# env-dep:` pairs emitted by `rustc`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedDepInfo {
    pub files: Vec<String>,
    pub env_deps: BTreeMap<String, Option<String>>,
}

fn next_temp_seq() -> u64 {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

fn unique_temp_path(target: &Path) -> PathBuf {
    let pid = std::process::id();
    let seq = next_temp_seq();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let file_name = target
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("artifact");
    target.with_file_name(format!(".{file_name}.gp-tmp-{pid}-{nanos}-{seq}"))
}

/// Resolves `<git-common-dir>/gitpulse/build-cache` for a repository or worktree.
pub fn resolve_build_cache_dir(repo_or_worktree: &Path) -> Result<PathBuf, String> {
    if let Ok(explicit) = std::env::var(ENV_BUILD_CACHE_DIR) {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    resolve_repo_build_cache_dir(repo_or_worktree)
}

/// Resolves `<git-common-dir>/gitpulse/build-cache` directly from Git metadata,
/// ignoring process environment overrides.
pub fn resolve_repo_build_cache_dir(repo_or_worktree: &Path) -> Result<PathBuf, String> {
    let out = capture_command(
        "git",
        &["rev-parse", "--git-common-dir"],
        Some(repo_or_worktree),
        Duration::from_secs(5),
        &[],
    )
    .map_err(|e| e.into_message())?;
    if !out.success {
        return Err(format!(
            "cannot resolve git-common-dir for {}: {}",
            repo_or_worktree.display(),
            out.stderr_text().trim()
        ));
    }
    let raw = out.stdout_text();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("git rev-parse --git-common-dir returned empty path".to_string());
    }
    let common_raw = PathBuf::from(trimmed);
    let common_abs = if common_raw.is_absolute() {
        common_raw
    } else {
        repo_or_worktree.join(common_raw)
    };
    let common = fs::canonicalize(&common_abs).unwrap_or(common_abs);
    Ok(BUILD_CACHE_SUBDIR
        .split('/')
        .fold(common, |acc, part| acc.join(part)))
}

fn read_json_bounded<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> Result<T, String> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(e) => return Err(format!("cannot open {}: {e}", path.display())),
    };
    let mut buf = Vec::new();
    file.take(MAX_JSON_STATE_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if buf.len() as u64 > MAX_JSON_STATE_BYTES {
        return Err(format!(
            "{} exceeds {} byte safety cap",
            path.display(),
            MAX_JSON_STATE_BYTES
        ));
    }
    if buf.is_empty() {
        return Ok(T::default());
    }
    serde_json::from_slice(&buf).map_err(|e| format!("corrupt JSON in {}: {e}", path.display()))
}

struct StatsLockGuard {
    lock_path: PathBuf,
}

impl Drop for StatsLockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lock_path);
    }
}

fn acquire_stats_lock(cache_dir: &Path) -> Option<StatsLockGuard> {
    if fs::create_dir_all(cache_dir).is_err() {
        return None;
    }
    let lock_path = cache_dir.join(STATS_LOCK_FILE);
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut sleep_ms = 1u64;
    loop {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(mut f) => {
                let _ = write!(f, "{}", std::process::id());
                return Some(StatsLockGuard { lock_path });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Evict stale lockfiles left by killed processes (>10s old).
                if let Ok(meta) = fs::metadata(&lock_path) {
                    if let Ok(modified) = meta.modified() {
                        if modified.elapsed().unwrap_or_default() > Duration::from_secs(10) {
                            let _ = fs::remove_file(&lock_path);
                            continue;
                        }
                    }
                }
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(sleep_ms));
                sleep_ms = (sleep_ms * 2).min(20);
            }
            Err(_) => return None,
        }
    }
}

fn update_counters(cache_dir: &Path, f: impl FnOnce(&mut BuildCacheCounters)) {
    let Some(_guard) = acquire_stats_lock(cache_dir) else {
        return;
    };
    let stats_path = cache_dir.join(STATS_FILE);
    let mut counters: BuildCacheCounters = read_json_bounded(&stats_path).unwrap_or_default();
    f(&mut counters);
    if let Ok(bytes) = serde_json::to_vec_pretty(&counters) {
        let _ = write_atomic(&stats_path, &bytes);
    }
}

/// Reads current build-cache statistics (and optionally prunes blobs exceeding `max_bytes`).
pub fn inspect_or_prune_cache(
    cache_dir: &Path,
    prune: bool,
    max_bytes: Option<u64>,
) -> Result<BuildCacheReport, String> {
    let _guard = acquire_stats_lock(cache_dir);
    let counters: BuildCacheCounters =
        read_json_bounded(&cache_dir.join(STATS_FILE)).unwrap_or_default();

    let mut blobs: Vec<(PathBuf, String, u64, SystemTime)> = Vec::new();
    let blobs_root = cache_dir.join("blobs");
    if blobs_root.is_dir() {
        for shard in fs::read_dir(&blobs_root).map_err(|e| e.to_string())? {
            let shard = shard.map_err(|e| e.to_string())?;
            if !shard.path().is_dir() {
                continue;
            }
            for entry in fs::read_dir(shard.path()).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                let path = entry.path();
                let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                if name.starts_with('.') {
                    continue;
                }
                if let Ok(meta) = entry.metadata() {
                    if meta.is_file() {
                        let mtime = meta.modified().unwrap_or(UNIX_EPOCH);
                        let name_owned = name.to_string();
                        blobs.push((path, name_owned, meta.len(), mtime));
                    }
                }
            }
        }
    }

    let mut manifests: Vec<PathBuf> = Vec::new();
    let direct_root = cache_dir.join("direct");
    if direct_root.is_dir() {
        for shard in fs::read_dir(&direct_root).map_err(|e| e.to_string())? {
            let shard = shard.map_err(|e| e.to_string())?;
            if !shard.path().is_dir() {
                continue;
            }
            for entry in fs::read_dir(shard.path()).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("json") {
                    manifests.push(path);
                }
            }
        }
    }

    let mut total_blob_bytes: u64 = blobs.iter().map(|(_, _, sz, _)| *sz).sum();
    let mut pruned_blobs = 0usize;
    let mut pruned_manifests = 0usize;
    let mut pruned_bytes = 0u64;

    if prune {
        let limit = max_bytes.unwrap_or(DEFAULT_MAX_CACHE_BYTES);
        if total_blob_bytes > limit {
            blobs.sort_by_key(|(_, _, _, mtime)| *mtime);
            let mut evicted_hashes = BTreeSet::new();
            let mut kept = Vec::with_capacity(blobs.len());
            for (path, digest, sz, mtime) in blobs {
                if total_blob_bytes > limit {
                    if fs::remove_file(&path).is_ok() {
                        total_blob_bytes = total_blob_bytes.saturating_sub(sz);
                        pruned_blobs += 1;
                        pruned_bytes += sz;
                        evicted_hashes.insert(digest);
                    } else {
                        kept.push((path, digest, sz, mtime));
                    }
                } else {
                    kept.push((path, digest, sz, mtime));
                }
            }
            blobs = kept;

            if !evicted_hashes.is_empty() {
                let mut kept_manifests = Vec::with_capacity(manifests.len());
                for mpath in manifests {
                    let remove_manifest = match read_direct_manifest(&mpath) {
                        Ok(Some(m)) => m
                            .artifacts
                            .iter()
                            .any(|a| evicted_hashes.contains(&a.blob_blake3)),
                        Ok(None) => false,
                        Err(_) => true,
                    };
                    if remove_manifest && fs::remove_file(&mpath).is_ok() {
                        pruned_manifests += 1;
                    } else {
                        kept_manifests.push(mpath);
                    }
                }
                manifests = kept_manifests;
            }
        }
    }

    Ok(BuildCacheReport {
        cache_dir: cache_dir.to_string_lossy().into_owned(),
        counters,
        blob_count: blobs.len(),
        manifest_count: manifests.len(),
        total_blob_bytes,
        pruned_blobs,
        pruned_manifests,
        pruned_bytes,
    })
}

fn read_direct_manifest(path: &Path) -> Result<Option<DirectManifest>, String> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let mut buf = Vec::new();
    file.take(MAX_JSON_STATE_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    if buf.len() as u64 > MAX_JSON_STATE_BYTES || buf.is_empty() {
        return Err("invalid manifest size".to_string());
    }
    serde_json::from_slice(&buf)
        .map(Some)
        .map_err(|e| e.to_string())
}

fn blob_path_for(cache_dir: &Path, digest: &str) -> PathBuf {
    let shard = digest.get(0..2).unwrap_or("00");
    cache_dir.join("blobs").join(shard).join(digest)
}

fn manifest_path_for(cache_dir: &Path, direct_key: &str) -> PathBuf {
    let shard = direct_key.get(0..2).unwrap_or("00");
    cache_dir
        .join("direct")
        .join(shard)
        .join(format!("{direct_key}.json"))
}

fn hash_file_blake3(path: &Path) -> Result<(String, u64), String> {
    let meta = fs::metadata(path).map_err(|e| format!("cannot stat {}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let len = meta.len();
    if len > MAX_ARTIFACT_BYTES {
        return Err(format!(
            "{} ({} B) exceeds {} B cache limit",
            path.display(),
            len,
            MAX_ARTIFACT_BYTES
        ));
    }
    let bytes = fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok((blake3_hash(&bytes), len))
}

fn resolve_executable_path(program: &Path) -> PathBuf {
    if program.is_absolute() || program.components().count() > 1 {
        return program.to_path_buf();
    }
    if let Some(path_os) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_os) {
            let candidate = dir.join(program);
            if candidate.is_file() {
                return candidate;
            }
            #[cfg(windows)]
            if program.extension().is_none() {
                let exe = dir.join(format!("{}.exe", program.to_string_lossy()));
                if exe.is_file() {
                    return exe;
                }
            }
        }
    }
    program.to_path_buf()
}

fn compiler_identity_digest(cache_dir: &Path, rustc: &Path) -> Result<String, String> {
    let resolved = resolve_executable_path(rustc);
    let canon = fs::canonicalize(&resolved).unwrap_or(resolved);
    let toolchain_env = std::env::var("RUSTUP_TOOLCHAIN").unwrap_or_default();
    let key = if toolchain_env.is_empty() {
        canon.to_string_lossy().into_owned()
    } else {
        format!("{}#toolchain={toolchain_env}", canon.to_string_lossy())
    };
    let meta = fs::metadata(&canon)
        .map_err(|e| format!("cannot stat rustc {}: {e}", canon.display()))?;
    let size = meta.len();
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    let compilers_path = cache_dir.join(COMPILERS_FILE);
    if let Ok(cache) = read_json_bounded::<CompilerIdCache>(&compilers_path) {
        if let Some(hit) = cache.entries.get(&key) {
            if hit.size == size && hit.mtime_ns == mtime_ns {
                return Ok(hit.vv_blake3.clone());
            }
        }
    }

    let out = Command::new(&canon)
        .arg("-vV")
        .output()
        .map_err(|e| format!("failed to run rustc -vV: {e}"))?;
    if !out.status.success() {
        return Err("rustc -vV exited non-zero".to_string());
    }
    let vv_blake3 = blake3_hash(&out.stdout);

    if let Some(_guard) = acquire_stats_lock(cache_dir) {
        let mut cache: CompilerIdCache =
            read_json_bounded(&compilers_path).unwrap_or_default();
        if cache.entries.len() < 64 {
            cache.entries.insert(
                key,
                CompilerIdRecord {
                    size,
                    mtime_ns,
                    vv_blake3: vv_blake3.clone(),
                },
            );
            if let Ok(bytes) = serde_json::to_vec_pretty(&cache) {
                let _ = write_atomic(&compilers_path, &bytes);
            }
        }
    }

    Ok(vv_blake3)
}

/// Parses a `rustc`-generated Makefile `.d` dep-info file, including `\ `-escaped
/// spaces in paths, line continuations, and `# env-dep:KEY=VAL` annotations.
pub fn parse_dep_info(text: &str) -> ParsedDepInfo {
    let mut env_deps = BTreeMap::new();
    let mut logical_lines = Vec::new();
    let mut current = String::new();

    for raw_line in text.lines() {
        if let Some(rest) = raw_line.strip_prefix("# env-dep:") {
            if let Some((k, v)) = rest.split_once('=') {
                env_deps.insert(k.to_string(), Some(unescape_env_dep(v)));
            } else if !rest.trim().is_empty() {
                env_deps.insert(rest.trim().to_string(), None);
            }
            continue;
        }
        if raw_line.trim_start().starts_with('#') {
            continue;
        }
        let trimmed_end = raw_line.trim_end_matches('\r');
        // Count trailing backslashes: odd count means line continuation.
        let trailing_bs = trimmed_end
            .chars()
            .rev()
            .take_while(|&c| c == '\\')
            .count();
        if trailing_bs % 2 == 1 {
            current.push_str(&trimmed_end[..trimmed_end.len() - 1]);
            current.push(' ');
        } else {
            current.push_str(trimmed_end);
            if !current.trim().is_empty() {
                logical_lines.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }
    if !current.trim().is_empty() {
        logical_lines.push(current);
    }

    let mut seen = BTreeSet::new();
    let mut files = Vec::new();

    for line in logical_lines {
        let Some(rhs) = split_makefile_rule_rhs(&line) else {
            continue;
        };
        for dep in split_makefile_escaped_words(rhs) {
            if !dep.is_empty() && seen.insert(dep.clone()) {
                files.push(dep);
            }
        }
    }

    ParsedDepInfo { files, env_deps }
}

fn unescape_env_dep(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn split_makefile_rule_rhs(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == b':' {
            // Skip Windows drive letter prefix like `C:\...`
            if i == 1 && bytes[0].is_ascii_alphabetic() && bytes.get(2) == Some(&b'\\') {
                i += 1;
                continue;
            }
            let after = &line[i + 1..];
            if after.is_empty() {
                return Some("");
            }
            if after.starts_with(char::is_whitespace) {
                return Some(after.trim_start());
            }
        }
        i += 1;
    }
    None
}

fn split_makefile_escaped_words(rhs: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut chars = rhs.chars().peekable();

    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if c == '\\' {
            match chars.peek().copied() {
                Some(' ') | Some('#') | Some(':') | Some('\\') => {
                    cur.push(chars.next().unwrap());
                }
                _ => cur.push('\\'),
            }
        } else if c == '$' && chars.peek() == Some(&'$') {
            chars.next();
            cur.push('$');
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

#[derive(Debug)]
enum Classification {
    Probe,
    Passthrough,
    Cacheable(CacheableInvocation),
}

#[derive(Debug)]
struct CacheableInvocation {
    crate_name: String,
    extra_filename: String,
    out_dir: PathBuf,
    input_rs: PathBuf,
    normalized_flags: Vec<String>,
    extern_files: Vec<(String, PathBuf)>,
}

fn expand_rustc_response_files(args: &[String], cwd: &Path) -> Result<Vec<String>, ()> {
    let mut out = Vec::with_capacity(args.len());
    expand_rustc_response_files_iter(args.iter().map(String::as_str), cwd, 0, &mut out)?;
    Ok(out)
}

fn expand_rustc_response_files_iter<'a>(
    args: impl Iterator<Item = &'a str>,
    cwd: &Path,
    depth: usize,
    out: &mut Vec<String>,
) -> Result<(), ()> {
    if depth > MAX_RSP_DEPTH {
        return Err(());
    }
    for arg in args {
        if let Some(rsp_rel) = arg.strip_prefix('@') {
            if rsp_rel.is_empty() || depth == MAX_RSP_DEPTH {
                return Err(());
            }
            let rsp_path = {
                let p = Path::new(rsp_rel);
                if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    cwd.join(p)
                }
            };
            let file = fs::File::open(&rsp_path).map_err(|_| ())?;
            let mut raw = Vec::new();
            file.take(MAX_RSP_FILE_BYTES + 1)
                .read_to_end(&mut raw)
                .map_err(|_| ())?;
            if raw.len() as u64 > MAX_RSP_FILE_BYTES {
                return Err(());
            }
            let text = std::str::from_utf8(&raw).map_err(|_| ())?;
            let text = text.strip_prefix('\u{feff}').unwrap_or(text);
            expand_rustc_response_files_iter(text.lines(), cwd, depth + 1, out)?;
        } else {
            if out.len() >= MAX_EXPANDED_ARGS {
                return Err(());
            }
            out.push(arg.to_owned());
        }
    }
    Ok(())
}

fn classify_rustc_args(raw_args: &[String], cwd: &Path) -> Classification {
    if raw_args.is_empty() {
        return Classification::Probe;
    }
    let Ok(expanded) = expand_rustc_response_files(raw_args, cwd) else {
        return Classification::Passthrough;
    };
    let args = &expanded;
    if args.is_empty() {
        return Classification::Probe;
    }
    for arg in args {
        if arg == "-vV"
            || arg == "-V"
            || arg == "--version"
            || arg == "--print"
            || arg.starts_with("--print=")
        {
            return Classification::Probe;
        }
        if arg == "--test" || arg == "-" {
            return Classification::Passthrough;
        }
    }

    let mut crate_name: Option<String> = None;
    let mut crate_types: Vec<String> = Vec::new();
    let mut extra_filename = String::new();
    let mut out_dir: Option<PathBuf> = None;
    let mut input_rs: Option<PathBuf> = None;
    let mut emits: Vec<String> = Vec::new();
    let mut extern_files: Vec<(String, PathBuf)> = Vec::new();
    let mut normalized_flags: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--crate-name" {
            let Some(val) = args.get(i + 1) else {
                return Classification::Passthrough;
            };
            crate_name = Some(val.clone());
            normalized_flags.push("--crate-name".to_string());
            normalized_flags.push(val.clone());
            i += 2;
            continue;
        }
        if let Some(val) = arg.strip_prefix("--crate-name=") {
            crate_name = Some(val.to_string());
            normalized_flags.push(arg.clone());
            i += 1;
            continue;
        }
        if arg == "--crate-type" {
            let Some(val) = args.get(i + 1) else {
                return Classification::Passthrough;
            };
            for part in val.split(',') {
                crate_types.push(part.to_string());
            }
            normalized_flags.push("--crate-type".to_string());
            normalized_flags.push(val.clone());
            i += 2;
            continue;
        }
        if let Some(val) = arg.strip_prefix("--crate-type=") {
            for part in val.split(',') {
                crate_types.push(part.to_string());
            }
            normalized_flags.push(arg.clone());
            i += 1;
            continue;
        }
        if arg == "--out-dir" {
            let Some(val) = args.get(i + 1) else {
                return Classification::Passthrough;
            };
            let p = PathBuf::from(val);
            out_dir = Some(if p.is_absolute() { p } else { cwd.join(p) });
            // Omit raw `--out-dir` path from normalized flags so sibling worktrees share keys.
            i += 2;
            continue;
        }
        if let Some(val) = arg.strip_prefix("--out-dir=") {
            let p = PathBuf::from(val);
            out_dir = Some(if p.is_absolute() { p } else { cwd.join(p) });
            i += 1;
            continue;
        }
        if arg == "-o" || arg.starts_with("-o/") {
            return Classification::Passthrough;
        }
        if arg == "--emit" {
            let Some(val) = args.get(i + 1) else {
                return Classification::Passthrough;
            };
            for item in val.split(',') {
                emits.push(item.split('=').next().unwrap_or(item).to_string());
                if item.contains('=') {
                    return Classification::Passthrough;
                }
            }
            normalized_flags.push("--emit".to_string());
            normalized_flags.push(val.clone());
            i += 2;
            continue;
        }
        if let Some(val) = arg.strip_prefix("--emit=") {
            for item in val.split(',') {
                emits.push(item.split('=').next().unwrap_or(item).to_string());
                if item.contains('=') {
                    return Classification::Passthrough;
                }
            }
            normalized_flags.push(arg.clone());
            i += 1;
            continue;
        }
        if arg == "-L" {
            let Some(_val) = args.get(i + 1) else {
                return Classification::Passthrough;
            };
            // Search paths `-L dependency=...` point into the worktree's `target/`;
            // actual linked crates are hashed explicitly via `--extern` and `.d` files.
            i += 2;
            continue;
        }
        if arg.starts_with("-L") {
            i += 1;
            continue;
        }
        if arg == "--extern" {
            let Some(val) = args.get(i + 1) else {
                return Classification::Passthrough;
            };
            if let Some((name, raw_path)) = val.split_once('=') {
                // Handle `--extern priv:name=path` or `noprelude:name=path`
                let p = PathBuf::from(raw_path);
                let resolved = if p.is_absolute() { p } else { cwd.join(p) };
                extern_files.push((name.to_string(), resolved));
            } else {
                normalized_flags.push("--extern".to_string());
                normalized_flags.push(val.clone());
            }
            i += 2;
            continue;
        }
        if let Some(val) = arg.strip_prefix("--extern=") {
            if let Some((name, raw_path)) = val.split_once('=') {
                let p = PathBuf::from(raw_path);
                let resolved = if p.is_absolute() { p } else { cwd.join(p) };
                extern_files.push((name.to_string(), resolved));
            } else {
                normalized_flags.push(arg.clone());
            }
            i += 1;
            continue;
        }
        if arg == "-C" {
            let Some(val) = args.get(i + 1) else {
                return Classification::Passthrough;
            };
            if is_uncacheable_codegen_flag(val) {
                return Classification::Passthrough;
            }
            if let Some(ef) = val.strip_prefix("extra-filename=") {
                extra_filename = ef.to_string();
            }
            normalized_flags.push("-C".to_string());
            normalized_flags.push(val.clone());
            i += 2;
            continue;
        }
        if let Some(val) = arg.strip_prefix("-C") {
            if !val.is_empty() {
                if is_uncacheable_codegen_flag(val) {
                    return Classification::Passthrough;
                }
                if let Some(ef) = val.strip_prefix("extra-filename=") {
                    extra_filename = ef.to_string();
                }
                normalized_flags.push(arg.clone());
                i += 1;
                continue;
            }
        }
        // Check positional `.rs` input file.
        if !arg.starts_with('-') && Path::new(arg).extension().and_then(|s| s.to_str()) == Some("rs")
        {
            if input_rs.is_some() {
                return Classification::Passthrough;
            }
            let p = PathBuf::from(arg);
            input_rs = Some(if p.is_absolute() { p } else { cwd.join(p) });
            i += 1;
            continue;
        }
        // Flags taking a separate value argument that must stay intact in normalized_flags:
        if matches!(
            arg.as_str(),
            "--edition"
                | "--target"
                | "--cap-lints"
                | "--cfg"
                | "--check-cfg"
                | "--error-format"
                | "--json"
                | "--color"
                | "--remap-path-prefix"
                | "-A"
                | "-W"
                | "-D"
                | "-F"
        ) {
            if let Some(val) = args.get(i + 1) {
                normalized_flags.push(arg.clone());
                normalized_flags.push(val.clone());
                i += 2;
                continue;
            }
        }
        normalized_flags.push(arg.clone());
        i += 1;
    }

    let Some(crate_name) = crate_name else {
        return Classification::Passthrough;
    };
    if crate_name == "build_script_build" {
        return Classification::Passthrough;
    }
    let Some(out_dir) = out_dir else {
        return Classification::Passthrough;
    };
    let Some(input_rs) = input_rs else {
        return Classification::Passthrough;
    };

    if crate_types.is_empty() {
        return Classification::Passthrough;
    }
    for ct in &crate_types {
        if !matches!(
            ct.as_str(),
            "lib" | "rlib" | "proc-macro" | "dylib" | "cdylib" | "staticlib"
        ) {
            return Classification::Passthrough;
        }
    }

    if !emits.is_empty() {
        if !emits.iter().any(|e| e == "dep-info") {
            return Classification::Passthrough;
        }
        for e in &emits {
            if !matches!(e.as_str(), "dep-info" | "metadata" | "link") {
                return Classification::Passthrough;
            }
        }
    }

    Classification::Cacheable(CacheableInvocation {
        crate_name,
        extra_filename,
        out_dir,
        input_rs,
        normalized_flags,
        extern_files,
    })
}

fn is_uncacheable_codegen_flag(val: &str) -> bool {
    val.starts_with("incremental=")
        || val.starts_with("profile-generate")
        || val.starts_with("profile-use")
        || val.starts_with("instrument-coverage")
}

fn compute_stage1_direct_key(
    compiler_vv_blake3: &str,
    inv: &CacheableInvocation,
    worktree_root_canon: Option<&Path>,
    worktree_root_raw: Option<&Path>,
) -> Result<String, String> {
    let mut material = Vec::with_capacity(4096);
    material.extend_from_slice(b"gitpulse-rustc-v1\n");
    material.extend_from_slice(compiler_vv_blake3.as_bytes());
    material.push(b'\n');

    let mut wt_prefixes: Vec<&str> = Vec::with_capacity(2);
    if let Some(wt_c) = worktree_root_canon.and_then(|p| p.to_str()) {
        if !wt_c.is_empty() {
            wt_prefixes.push(wt_c);
        }
    }
    if let Some(wt_r) = worktree_root_raw.and_then(|p| p.to_str()) {
        if !wt_r.is_empty() && !wt_prefixes.contains(&wt_r) {
            wt_prefixes.push(wt_r);
        }
    }
    wt_prefixes.sort_by_key(|b| std::cmp::Reverse(b.len()));

    for flag in &inv.normalized_flags {
        let mut normalized = flag.clone();
        for wt_str in &wt_prefixes {
            if normalized.contains(wt_str) {
                normalized = normalized.replace(wt_str, "$WT_ROOT");
            }
        }
        material.extend_from_slice(normalized.as_bytes());
        material.push(0);
    }

    let canon_input = fs::canonicalize(&inv.input_rs).unwrap_or_else(|_| inv.input_rs.clone());
    let rel_input = worktree_root_canon
        .and_then(|wt| canon_input.strip_prefix(wt).ok())
        .or_else(|| worktree_root_raw.and_then(|wt| inv.input_rs.strip_prefix(wt).ok()))
        .map(|p| format!("$WT/{}", p.to_string_lossy()))
        .unwrap_or_else(|| canon_input.to_string_lossy().into_owned());
    let (input_hash, _) = hash_file_blake3(&canon_input)?;
    material.extend_from_slice(rel_input.as_bytes());
    material.push(b'=');
    material.extend_from_slice(input_hash.as_bytes());
    material.push(b'\n');

    let mut extern_hashes: Vec<(String, String, String)> = Vec::with_capacity(inv.extern_files.len());
    for (name, path) in &inv.extern_files {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let (digest, _) = hash_file_blake3(path)?;
        extern_hashes.push((name.clone(), ext, digest));
    }
    extern_hashes.sort();
    for (name, ext, digest) in extern_hashes {
        material.extend_from_slice(name.as_bytes());
        material.push(b'.');
        material.extend_from_slice(ext.as_bytes());
        material.push(b'=');
        material.extend_from_slice(digest.as_bytes());
        material.push(b'\n');
    }

    // Hash Cargo package/cfg/feature/rustflags environment variables deterministically.
    let mut env_pairs: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| {
            k.starts_with("CARGO_PKG_")
                || k.starts_with("CARGO_CFG_")
                || k.starts_with("CARGO_FEATURE_")
                || matches!(
                    k.as_str(),
                    "RUSTFLAGS"
                        | "CARGO_ENCODED_RUSTFLAGS"
                        | "PROFILE"
                        | "OPT_LEVEL"
                        | "DEBUG"
                        | "TARGET"
                        | "HOST"
                )
        })
        .collect();
    env_pairs.sort_by(|a, b| a.0.cmp(&b.0));
    for (k, v) in env_pairs {
        material.extend_from_slice(k.as_bytes());
        material.push(b'=');
        material.extend_from_slice(v.as_bytes());
        material.push(0);
    }

    Ok(blake3_hash(&material))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RustLexToken {
    Ident(String),
    Punct(char),
    StrLit {
        value: String,
        has_escape: bool,
        is_raw: bool,
    },
}

fn try_lex_raw_string(s: &str) -> Option<Result<(String, usize), ()>> {
    let prefix_len = if s.starts_with("br") || s.starts_with("cr") {
        2
    } else if s.starts_with('r') {
        1
    } else {
        return None;
    };
    let hashes = s[prefix_len..].bytes().take_while(|&b| b == b'#').count();
    if s.as_bytes().get(prefix_len + hashes) != Some(&b'"') {
        return None;
    }
    let body_start = prefix_len + hashes + 1;
    let rest = &s[body_start..];
    let mut search_offset = 0usize;
    while let Some(rel_quote) = rest[search_offset..].find('"') {
        let quote_idx = search_offset + rel_quote;
        let after_quote = &rest[quote_idx + 1..];
        if after_quote.bytes().take(hashes).all(|b| b == b'#') && after_quote.len() >= hashes {
            let value = rest[..quote_idx].to_string();
            let consumed = body_start + quote_idx + 1 + hashes;
            return Some(Ok((value, consumed)));
        }
        search_offset = quote_idx + 1;
    }
    Some(Err(()))
}

fn lex_rust_simple_tokens(src: &str) -> Result<Vec<RustLexToken>, ()> {
    let mut tokens = Vec::new();
    let mut i = 0usize;
    while i < src.len() {
        let s = &src[i..];
        let ch = s.chars().next().ok_or(())?;
        if ch.is_whitespace() {
            i += ch.len_utf8();
            continue;
        }
        if s.starts_with("//") {
            if let Some(nl) = s.find('\n') {
                i += nl + 1;
            } else {
                break;
            }
            continue;
        }
        if s.starts_with("/*") {
            let mut depth = 1usize;
            i += 2;
            while depth > 0 {
                if i >= src.len() {
                    return Err(());
                }
                let rem = &src[i..];
                if rem.starts_with("/*") {
                    depth += 1;
                    i += 2;
                } else if rem.starts_with("*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    let c = rem.chars().next().ok_or(())?;
                    i += c.len_utf8();
                }
            }
            continue;
        }
        if let Some(raw_res) = try_lex_raw_string(s) {
            let (value, consumed) = raw_res?;
            tokens.push(RustLexToken::StrLit {
                value,
                has_escape: false,
                is_raw: true,
            });
            i += consumed;
            continue;
        }
        if s.starts_with('"') || s.starts_with("b\"") || s.starts_with("c\"") {
            let quote_offset = if s.starts_with('"') { 1 } else { 2 };
            let mut cursor = i + quote_offset;
            let mut value = String::new();
            let mut has_escape = false;
            let mut closed = false;
            while cursor < src.len() {
                let c = src[cursor..].chars().next().ok_or(())?;
                cursor += c.len_utf8();
                if c == '"' {
                    closed = true;
                    break;
                }
                if c == '\\' {
                    has_escape = true;
                    let esc = src.get(cursor..).and_then(|r| r.chars().next()).ok_or(())?;
                    cursor += esc.len_utf8();
                    value.push(esc);
                } else {
                    value.push(c);
                }
            }
            if !closed {
                return Err(());
            }
            tokens.push(RustLexToken::StrLit {
                value,
                has_escape,
                is_raw: false,
            });
            i = cursor;
            continue;
        }
        if s.starts_with("b'") {
            let mut cursor = i + 2;
            let mut closed = false;
            while cursor < src.len() {
                let c = src[cursor..].chars().next().ok_or(())?;
                cursor += c.len_utf8();
                if c == '\'' {
                    closed = true;
                    break;
                }
                if c == '\\' {
                    let esc = src.get(cursor..).and_then(|r| r.chars().next()).ok_or(())?;
                    cursor += esc.len_utf8();
                }
            }
            if !closed {
                return Err(());
            }
            tokens.push(RustLexToken::Ident("<char>".to_string()));
            i = cursor;
            continue;
        }
        if ch == '\'' {
            let after = &s[1..];
            if after.starts_with('\\') {
                let mut cursor = i + 1;
                let mut closed = false;
                while cursor < src.len() {
                    let c = src[cursor..].chars().next().ok_or(())?;
                    cursor += c.len_utf8();
                    if c == '\'' {
                        closed = true;
                        break;
                    }
                    if c == '\\' {
                        let esc = src.get(cursor..).and_then(|r| r.chars().next()).ok_or(())?;
                        cursor += esc.len_utf8();
                    }
                }
                if !closed {
                    return Err(());
                }
                tokens.push(RustLexToken::Ident("<char>".to_string()));
                i = cursor;
                continue;
            }
            if let Some(first) = after.chars().next() {
                if first != '\''
                    && first != '\n'
                    && first != '\r'
                    && after[first.len_utf8()..].starts_with('\'')
                {
                    i += 1 + first.len_utf8() + 1;
                    tokens.push(RustLexToken::Ident("<char>".to_string()));
                    continue;
                }
                if first == '_' || first.is_alphabetic() {
                    let mut len = 1 + first.len_utf8();
                    for lc in after[first.len_utf8()..].chars() {
                        if lc == '_' || lc.is_alphanumeric() {
                            len += lc.len_utf8();
                        } else {
                            break;
                        }
                    }
                    tokens.push(RustLexToken::Ident("<lifetime>".to_string()));
                    i += len;
                    continue;
                }
            }
            tokens.push(RustLexToken::Punct('\''));
            i += 1;
            continue;
        }
        if let Some(after_raw_ident) = s.strip_prefix("r#") {
            if let Some(first) = after_raw_ident.chars().next() {
                if first == '_' || first.is_alphabetic() {
                    let mut ident_len = first.len_utf8();
                    for ic in after_raw_ident[first.len_utf8()..].chars() {
                        if ic == '_' || ic.is_alphanumeric() {
                            ident_len += ic.len_utf8();
                        } else {
                            break;
                        }
                    }
                    tokens.push(RustLexToken::Ident(
                        s[..2 + ident_len].to_string(),
                    ));
                    i += 2 + ident_len;
                    continue;
                }
            }
        }
        if ch == '_' || ch.is_alphabetic() {
            let mut ident_len = ch.len_utf8();
            for ic in s[ch.len_utf8()..].chars() {
                if ic == '_' || ic.is_alphanumeric() {
                    ident_len += ic.len_utf8();
                } else {
                    break;
                }
            }
            tokens.push(RustLexToken::Ident(s[..ident_len].to_string()));
            i += ident_len;
            continue;
        }
        if ch.is_ascii_digit() {
            let mut num_len = ch.len_utf8();
            for nc in s[ch.len_utf8()..].chars() {
                if nc.is_ascii_alphanumeric() || nc == '_' || nc == '.' {
                    num_len += nc.len_utf8();
                } else {
                    break;
                }
            }
            tokens.push(RustLexToken::Ident("<num>".to_string()));
            i += num_len;
            continue;
        }
        tokens.push(RustLexToken::Punct(ch));
        i += ch.len_utf8();
    }
    Ok(tokens)
}

fn matching_close_delim(open: char) -> Option<char> {
    match open {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        _ => None,
    }
}

fn strip_std_core_macro_prefix(tokens: &[RustLexToken], ident_idx: usize) -> usize {
    if ident_idx >= 3
        && tokens[ident_idx - 1] == RustLexToken::Punct(':')
        && tokens[ident_idx - 2] == RustLexToken::Punct(':')
        && matches!(
            &tokens[ident_idx - 3],
            RustLexToken::Ident(ns) if ns == "std" || ns == "core"
        )
    {
        let ns_idx = ident_idx - 3;
        if ns_idx >= 2
            && tokens[ns_idx - 1] == RustLexToken::Punct(':')
            && tokens[ns_idx - 2] == RustLexToken::Punct(':')
        {
            ns_idx - 2
        } else {
            ns_idx
        }
    } else {
        ident_idx
    }
}

fn check_tokens_out_dir_compile_only(tokens: &[RustLexToken]) -> (usize, bool) {
    let mut out_dir_count = 0usize;
    for (idx, tok) in tokens.iter().enumerate() {
        // Reject raw strings mentioning OUT_DIR.
        if let RustLexToken::StrLit {
            value,
            is_raw: true,
            ..
        } = tok
        {
            if value.contains("OUT_DIR") {
                return (out_dir_count, false);
            }
        }
        // Reject any `env!` / `option_env!` whose first argument is not a plain unescaped string literal
        // (prevents `env!("OUT\x5fDIR")` or `env!(concat!("OUT", "_DIR"))` from hiding `OUT_DIR`).
        if let RustLexToken::Ident(name) = tok {
            if (name == "env" || name == "option_env")
                && tokens.get(idx + 1) == Some(&RustLexToken::Punct('!'))
            {
                let Some(RustLexToken::Punct(open)) = tokens.get(idx + 2) else {
                    return (out_dir_count, false);
                };
                if matching_close_delim(*open).is_none() {
                    return (out_dir_count, false);
                }
                match tokens.get(idx + 3) {
                    Some(RustLexToken::StrLit {
                        has_escape: false,
                        is_raw: false,
                        ..
                    }) => {}
                    _ => return (out_dir_count, false),
                }
            }
        }
        let RustLexToken::StrLit {
            value,
            has_escape,
            is_raw,
        } = tok
        else {
            continue;
        };
        if value != "OUT_DIR" {
            continue;
        }
        out_dir_count += 1;
        if *has_escape || *is_raw || idx < 3 {
            return (out_dir_count, false);
        }

        // Must be preceded by `<env> ! <open3>`
        let RustLexToken::Punct(open3) = tokens[idx - 1] else {
            return (out_dir_count, false);
        };
        let Some(close3) = matching_close_delim(open3) else {
            return (out_dir_count, false);
        };
        if tokens[idx - 2] != RustLexToken::Punct('!')
            || tokens[idx - 3] != RustLexToken::Ident("env".to_string())
        {
            return (out_dir_count, false);
        }
        let env_start = strip_std_core_macro_prefix(tokens, idx - 3);
        if env_start < 3 {
            return (out_dir_count, false);
        }

        // Must be preceded by `<concat> ! <open2>`
        let RustLexToken::Punct(open2) = tokens[env_start - 1] else {
            return (out_dir_count, false);
        };
        let Some(close2) = matching_close_delim(open2) else {
            return (out_dir_count, false);
        };
        if tokens[env_start - 2] != RustLexToken::Punct('!')
            || tokens[env_start - 3] != RustLexToken::Ident("concat".to_string())
        {
            return (out_dir_count, false);
        }
        let concat_start = strip_std_core_macro_prefix(tokens, env_start - 3);
        if concat_start < 3 {
            return (out_dir_count, false);
        }

        // Must be preceded by `<include | include_str | include_bytes> ! <open1>`
        let RustLexToken::Punct(open1) = tokens[concat_start - 1] else {
            return (out_dir_count, false);
        };
        let Some(close1) = matching_close_delim(open1) else {
            return (out_dir_count, false);
        };
        if tokens[concat_start - 2] != RustLexToken::Punct('!') {
            return (out_dir_count, false);
        }
        let RustLexToken::Ident(ref inc_name) = tokens[concat_start - 3] else {
            return (out_dir_count, false);
        };
        if !matches!(
            inc_name.as_str(),
            "include" | "include_str" | "include_bytes"
        ) {
            return (out_dir_count, false);
        }

        // Walk forward past optional custom error message inside `env!("OUT_DIR", "msg")` to `close3`.
        let mut cur = idx + 1;
        if tokens.get(cur) == Some(&RustLexToken::Punct(',')) {
            cur += 1;
            if let Some(RustLexToken::StrLit { value: msg, .. }) = tokens.get(cur) {
                if msg == "OUT_DIR" {
                    return (out_dir_count, false);
                }
                cur += 1;
            }
            if tokens.get(cur) == Some(&RustLexToken::Punct(',')) {
                cur += 1;
            }
        }
        if tokens.get(cur) != Some(&RustLexToken::Punct(close3)) {
            return (out_dir_count, false);
        }
        cur += 1;

        // Walk remaining arguments of `concat!(env!("OUT_DIR"), "/...")` up to `close2`.
        let mut saw_suffix_str = false;
        while cur < tokens.len() && tokens[cur] != RustLexToken::Punct(close2) {
            if tokens[cur] != RustLexToken::Punct(',') {
                return (out_dir_count, false);
            }
            cur += 1;
            if tokens.get(cur) == Some(&RustLexToken::Punct(close2)) {
                break;
            }
            match tokens.get(cur) {
                Some(RustLexToken::StrLit { value: suffix, .. }) if suffix != "OUT_DIR" => {
                    saw_suffix_str = true;
                    cur += 1;
                }
                _ => return (out_dir_count, false),
            }
        }
        if !saw_suffix_str || tokens.get(cur) != Some(&RustLexToken::Punct(close2)) {
            return (out_dir_count, false);
        }
        cur += 1;

        // Optional trailing comma inside `include*!(concat!(...),)` before `close1`.
        if tokens.get(cur) == Some(&RustLexToken::Punct(',')) {
            cur += 1;
        }
        if tokens.get(cur) != Some(&RustLexToken::Punct(close1)) {
            return (out_dir_count, false);
        }
    }
    (out_dir_count, true)
}

fn is_out_dir_compile_time_include_only(
    input_rs: &Path,
    dep_files: &[String],
    cwd: &Path,
    deps: &[RecordedDepFile],
) -> bool {
    if !deps
        .iter()
        .any(|d| matches!(d.scope, DepPathScope::CargoBuildOutRel(_)))
    {
        return false;
    }

    let mut rs_paths: Vec<PathBuf> = Vec::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    let push_rs = |p: PathBuf, seen: &mut BTreeSet<PathBuf>, rs_paths: &mut Vec<PathBuf>| {
        if p.extension().and_then(|s| s.to_str()) != Some("rs") {
            return;
        }
        let canon = fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
        if seen.insert(canon.clone()) {
            rs_paths.push(canon);
        }
    };

    push_rs(input_rs.to_path_buf(), &mut seen, &mut rs_paths);
    for raw in dep_files {
        let p = PathBuf::from(raw);
        let abs = if p.is_absolute() { p } else { cwd.join(p) };
        push_rs(abs, &mut seen, &mut rs_paths);
    }

    let mut total_out_dir_literals = 0usize;
    for rs_path in rs_paths {
        let Ok(src) = fs::read_to_string(&rs_path) else {
            return false;
        };
        let Ok(tokens) = lex_rust_simple_tokens(&src) else {
            return false;
        };
        let (count, ok) = check_tokens_out_dir_compile_only(&tokens);
        if !ok {
            return false;
        }
        total_out_dir_literals += count;
    }

    total_out_dir_literals >= 1
}

fn classify_dep_scope(
    raw_dep: &str,
    cwd: &Path,
    out_dir: &Path,
    cargo_out_dir: Option<&Path>,
    worktree_root: Option<&Path>,
) -> Option<(PathBuf, DepPathScope)> {
    let dep_path = {
        let p = PathBuf::from(raw_dep);
        if p.is_absolute() {
            p
        } else {
            cwd.join(p)
        }
    };
    let canon = fs::canonicalize(&dep_path).unwrap_or(dep_path);
    let canon_out = fs::canonicalize(out_dir).unwrap_or_else(|_| out_dir.to_path_buf());
    if canon.starts_with(&canon_out) {
        return None;
    }
    if let Some(build_out) = cargo_out_dir {
        let canon_build_out =
            fs::canonicalize(build_out).unwrap_or_else(|_| build_out.to_path_buf());
        if let Ok(rel) = canon.strip_prefix(&canon_build_out) {
            return Some((
                canon.clone(),
                DepPathScope::CargoBuildOutRel(rel.to_string_lossy().into_owned()),
            ));
        }
    }
    if let Some(wt) = worktree_root {
        if let Ok(rel) = canon.strip_prefix(wt) {
            return Some((
                canon.clone(),
                DepPathScope::WorktreeRel(rel.to_string_lossy().into_owned()),
            ));
        }
    }
    Some((
        canon.clone(),
        DepPathScope::ExternalAbs(canon.to_string_lossy().into_owned()),
    ))
}

fn resolve_dep_scope(
    scope: &DepPathScope,
    cargo_out_dir: Option<&Path>,
    worktree_root: Option<&Path>,
) -> Option<PathBuf> {
    match scope {
        DepPathScope::CargoBuildOutRel(rel) => Some(cargo_out_dir?.join(rel)),
        DepPathScope::WorktreeRel(rel) => Some(worktree_root?.join(rel)),
        DepPathScope::ExternalAbs(abs) => Some(PathBuf::from(abs)),
    }
}

fn verify_stage2_manifest(
    manifest: &DirectManifest,
    cargo_out_dir: Option<&Path>,
    worktree_root: Option<&Path>,
) -> bool {
    for (k, expected) in &manifest.env_deps {
        if k == "OUT_DIR" && expected.as_deref() == Some(COMPILE_ONLY_OUT_DIR_SENTINEL) {
            if cargo_out_dir.is_none() {
                return false;
            }
            continue;
        }
        let actual = std::env::var(k).ok();
        if &actual != expected {
            return false;
        }
    }
    for dep in &manifest.deps {
        let Some(path) = resolve_dep_scope(&dep.scope, cargo_out_dir, worktree_root) else {
            return false;
        };
        let Ok((actual_hash, _)) = hash_file_blake3(&path) else {
            return false;
        };
        if actual_hash != dep.blake3 {
            return false;
        }
    }
    true
}

fn rewrite_path_prefixes_in_str(
    input: &str,
    replacements: &[(&str, &str)],
) -> String {
    let mut out = input.to_string();
    for (from, to) in replacements {
        if !from.is_empty() && from != to && out.contains(from) {
            out = out.replace(from, to);
        }
    }
    out
}

fn rewrite_json_strings(value: &mut Value, replacements: &[(&str, &str)]) {
    match value {
        Value::String(s) => {
            *s = rewrite_path_prefixes_in_str(s, replacements);
        }
        Value::Array(arr) => {
            for item in arr {
                rewrite_json_strings(item, replacements);
            }
        }
        Value::Object(map) => {
            for val in map.values_mut() {
                rewrite_json_strings(val, replacements);
            }
        }
        _ => {}
    }
}

fn rewrite_stderr_for_worktree(raw_stderr: &[u8], replacements: &[(&str, &str)]) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(raw_stderr) else {
        return raw_stderr.to_vec();
    };
    let mut out = Vec::with_capacity(raw_stderr.len() + 64);
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
        if trimmed.starts_with('{') && trimmed.ends_with('}') {
            if let Ok(mut json_val) = serde_json::from_str::<Value>(trimmed) {
                rewrite_json_strings(&mut json_val, replacements);
                if let Ok(serialized) = serde_json::to_string(&json_val) {
                    out.extend_from_slice(serialized.as_bytes());
                    if line.ends_with("\r\n") {
                        out.extend_from_slice(b"\r\n");
                    } else if line.ends_with('\n') {
                        out.push(b'\n');
                    }
                    continue;
                }
            }
        }
        let rewritten = rewrite_path_prefixes_in_str(line, replacements);
        out.extend_from_slice(rewritten.as_bytes());
    }
    out
}

fn materialize_blob_into(blob_path: &Path, dst_path: &Path) -> Result<bool, String> {
    if let Some(parent) = dst_path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = unique_temp_path(dst_path);
    let res = reflink_copy_file(blob_path, &tmp).map_err(|e| e.into_message())?;
    if let Err(e) = fs::rename(&tmp, dst_path) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("cannot rename staged artifact into {}: {e}", dst_path.display()));
    }
    Ok(res.is_cow)
}

fn store_blob_from(src_path: &Path, cache_dir: &Path) -> Result<(String, u64, bool), String> {
    let (digest, size) = hash_file_blake3(src_path)?;
    let blob_path = blob_path_for(cache_dir, &digest);
    if blob_path.is_file() {
        return Ok((digest, size, false));
    }
    if let Some(parent) = blob_path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = unique_temp_path(&blob_path);
    reflink_copy_file(src_path, &tmp).map_err(|e| e.into_message())?;
    match fs::rename(&tmp, &blob_path) {
        Ok(()) => Ok((digest, size, true)),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            if blob_path.is_file() {
                Ok((digest, size, false))
            } else {
                Err(format!("cannot store blob {}: {e}", blob_path.display()))
            }
        }
    }
}

type NamedOutputFile = (String, PathBuf);
type CollectedOutputs = (Option<NamedOutputFile>, Vec<NamedOutputFile>);

fn collect_emitted_artifacts(
    out_dir: &Path,
    crate_name: &str,
    extra_filename: &str,
    stderr_bytes: &[u8],
) -> Result<CollectedOutputs, String> {
    let expected_dep = format!("{crate_name}{extra_filename}.d");
    let prefix_plain = format!("{crate_name}{extra_filename}.");
    let prefix_lib = format!("lib{crate_name}{extra_filename}.");
    let canon_out_dir = fs::canonicalize(out_dir).ok();

    let mut dep_file: Option<NamedOutputFile> = None;
    let mut by_name: BTreeMap<String, PathBuf> = BTreeMap::new();

    // First inspect JSON `$message_type == "artifact"` notifications emitted by rustc on stderr.
    if let Ok(stderr_text) = std::str::from_utf8(stderr_bytes) {
        for line in stderr_text.lines() {
            if !line.starts_with('{') {
                continue;
            }
            if let Ok(val) = serde_json::from_str::<Value>(line) {
                if val.get("$message_type").and_then(Value::as_str) == Some("artifact") {
                    if let Some(art_str) = val.get("artifact").and_then(Value::as_str) {
                        let p = PathBuf::from(art_str);
                        let parent_matches = p.parent() == Some(out_dir)
                            || canon_out_dir.as_deref().is_some_and(|co| {
                                p.parent() == Some(co)
                                    || p.parent()
                                        .and_then(|pp| fs::canonicalize(pp).ok())
                                        .as_deref()
                                        == Some(co)
                            });
                        if parent_matches && p.is_file() {
                            if let Some(fname) = p.file_name().and_then(|s| s.to_str()) {
                                if fname == expected_dep {
                                    dep_file = Some((fname.to_string(), p));
                                } else {
                                    by_name.insert(fname.to_string(), p);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Also scan `out_dir` for `<crate><extra>.*` / `lib<crate><extra>.*` in case `--json=artifacts` wasn't requested.
    for entry in fs::read_dir(out_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let Some(fname) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if fname.starts_with('.') {
            continue;
        }
        if fname == expected_dep && path.is_file() {
            dep_file = Some((fname.to_string(), path));
            continue;
        }
        if (fname.starts_with(&prefix_plain) || fname.starts_with(&prefix_lib))
            && !fname.ends_with(".d")
        {
            let meta = entry.metadata().map_err(|e| e.to_string())?;
            if meta.is_dir() {
                // e.g. macOS `.dSYM` bundle directory — skip caching this invocation cleanly.
                return Err("directory artifact bundle present".to_string());
            }
            if meta.is_file() {
                by_name.entry(fname.to_string()).or_insert(path);
            }
        }
    }

    Ok((dep_file, by_name.into_iter().collect()))
}

/// Returns `(raw_worktree_root, canonical_worktree_root)` so symlinked worktrees
/// (and macOS `/var/folders` vs `/private/var/folders` aliases) match cleanly in both
/// Stage 1 normalization and `.d` / stderr path rewriting.
fn detect_worktree_roots(cwd: &Path) -> (Option<PathBuf>, Option<PathBuf>) {
    if let Ok(raw) = std::env::var(ENV_WORKTREE_ROOT) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            let p = PathBuf::from(trimmed);
            let abs = if p.is_absolute() { p } else { cwd.join(p) };
            let canon = fs::canonicalize(&abs).unwrap_or_else(|_| abs.clone());
            return (Some(abs), Some(canon));
        }
    }
    let Some(out) = capture_command(
        "git",
        &["rev-parse", "--show-toplevel"],
        Some(cwd),
        Duration::from_secs(3),
        &[],
    )
    .ok() else {
        return (None, None);
    };
    if !out.success {
        return (None, None);
    }
    let top = out.stdout_text();
    let trimmed = top.trim();
    if trimmed.is_empty() {
        (None, None)
    } else {
        let p = PathBuf::from(trimmed);
        let abs = if p.is_absolute() { p } else { cwd.join(p) };
        let canon = fs::canonicalize(&abs).unwrap_or_else(|_| abs.clone());
        (Some(abs), Some(canon))
    }
}

fn passthrough_rustc(rustc: &Path, args: &[String]) -> i32 {
    match Command::new(rustc).args(args).status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            let _ = writeln!(
                std::io::stderr(),
                "gitpulse-rustc: failed to execute {}: {e}",
                rustc.display()
            );
            1
        }
    }
}

/// Entrypoint logic for `gitpulse-rustc <rustc> [args...]`.
pub fn run_rustc_wrapper(wrapper_args: &[String]) -> i32 {
    let Some((rustc_str, rustc_args)) = wrapper_args.split_first() else {
        let _ = writeln!(
            std::io::stderr(),
            "usage: gitpulse-rustc <path-to-rustc> [rustc-args...]"
        );
        return 2;
    };
    let rustc = Path::new(rustc_str);
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(_) => return passthrough_rustc(rustc, rustc_args),
    };
    let Ok(cache_dir) = resolve_build_cache_dir(&cwd) else {
        return passthrough_rustc(rustc, rustc_args);
    };

    let inv = match classify_rustc_args(rustc_args, &cwd) {
        Classification::Probe => {
            update_counters(&cache_dir, |c| c.probes += 1);
            return passthrough_rustc(rustc, rustc_args);
        }
        Classification::Passthrough => {
            update_counters(&cache_dir, |c| c.passthroughs += 1);
            return passthrough_rustc(rustc, rustc_args);
        }
        Classification::Cacheable(c) => c,
    };

    let (worktree_root_raw, worktree_root_canon) = detect_worktree_roots(&cwd);
    let cargo_out_dir = std::env::var("OUT_DIR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from);

    let compiler_vv = match compiler_identity_digest(&cache_dir, rustc) {
        Ok(v) => v,
        Err(_) => {
            update_counters(&cache_dir, |c| c.passthroughs += 1);
            return passthrough_rustc(rustc, rustc_args);
        }
    };

    let direct_key = match compute_stage1_direct_key(
        &compiler_vv,
        &inv,
        worktree_root_canon.as_deref(),
        worktree_root_raw.as_deref(),
    ) {
        Ok(k) => k,
        Err(_) => {
            update_counters(&cache_dir, |c| c.passthroughs += 1);
            return passthrough_rustc(rustc, rustc_args);
        }
    };

    let manifest_path = manifest_path_for(&cache_dir, &direct_key);
    let mut stage2_invalidated = false;

    if let Ok(Some(manifest)) = read_direct_manifest(&manifest_path) {
        if verify_stage2_manifest(
            &manifest,
            cargo_out_dir.as_deref(),
            worktree_root_canon.as_deref(),
        ) {
            if try_restore_cached_outputs(
                &cache_dir,
                &manifest,
                &inv.out_dir,
                cargo_out_dir.as_deref(),
                worktree_root_canon.as_deref(),
                worktree_root_raw.as_deref(),
            )
            .is_ok()
            {
                return 0;
            }
        } else {
            stage2_invalidated = true;
        }
    }

    // Cache miss: execute real `rustc` while capturing stdout/stderr so we can both
    // replay output verbatim and persist stderr diagnostics/artifact lines on success.
    let output = match Command::new(rustc).args(rustc_args).output() {
        Ok(o) => o,
        Err(e) => {
            let _ = writeln!(
                std::io::stderr(),
                "gitpulse-rustc: failed to execute {}: {e}",
                rustc.display()
            );
            return 1;
        }
    };

    let _ = std::io::stdout().write_all(&output.stdout);
    let _ = std::io::stderr().write_all(&output.stderr);

    let code = output.status.code().unwrap_or(1);
    if code != 0 {
        update_counters(&cache_dir, |c| {
            c.misses += 1;
            if stage2_invalidated {
                c.stage2_invalidations += 1;
            }
        });
        return code;
    }

    let new_blob_bytes = store_successful_compile(
        &cache_dir,
        &manifest_path,
        &inv,
        &cwd,
        cargo_out_dir.as_deref(),
        worktree_root_canon.as_deref(),
        worktree_root_raw.as_deref(),
        &output.stderr,
    )
    .unwrap_or(0);

    update_counters(&cache_dir, |c| {
        c.misses += 1;
        c.bytes_stored += new_blob_bytes;
        if stage2_invalidated {
            c.stage2_invalidations += 1;
        }
    });

    0
}

fn try_restore_cached_outputs(
    cache_dir: &Path,
    manifest: &DirectManifest,
    current_out_dir: &Path,
    current_cargo_out_dir: Option<&Path>,
    current_worktree_root_canon: Option<&Path>,
    current_worktree_root_raw: Option<&Path>,
) -> Result<(), String> {
    if manifest.artifacts.is_empty() {
        return Err("empty artifact list".to_string());
    }
    // Verify all blobs exist before materializing any output file.
    let mut resolved_blobs = Vec::with_capacity(manifest.artifacts.len());
    for art in &manifest.artifacts {
        let bp = blob_path_for(cache_dir, &art.blob_blake3);
        if !bp.is_file() {
            return Err(format!("missing blob {}", art.blob_blake3));
        }
        resolved_blobs.push((art, bp));
    }

    fs::create_dir_all(current_out_dir).map_err(|e| e.to_string())?;

    let mut all_cow = true;
    let mut bytes_restored = 0u64;
    for (art, bp) in resolved_blobs {
        let dst = current_out_dir.join(&art.file_name);
        let is_cow = materialize_blob_into(&bp, &dst)?;
        all_cow &= is_cow;
        bytes_restored += art.size_bytes;
    }

    let cur_out_str = current_out_dir.to_string_lossy().into_owned();
    let cur_cargo_out_str = current_cargo_out_dir
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let cur_wt_preferred = current_worktree_root_raw
        .or(current_worktree_root_canon)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut raw_pairs: Vec<(&str, &str)> = Vec::with_capacity(6);
    raw_pairs.push((manifest.recorded_out_dir.as_str(), cur_out_str.as_str()));
    if let Some(ref rec_out_canon) = manifest.recorded_out_dir_canon {
        raw_pairs.push((rec_out_canon.as_str(), cur_out_str.as_str()));
    }
    if !cur_cargo_out_str.is_empty() {
        if let Some(ref rec_cargo_out) = manifest.recorded_cargo_out_dir {
            raw_pairs.push((rec_cargo_out.as_str(), cur_cargo_out_str.as_str()));
        }
        if let Some(ref rec_cargo_out_canon) = manifest.recorded_cargo_out_dir_canon {
            raw_pairs.push((rec_cargo_out_canon.as_str(), cur_cargo_out_str.as_str()));
        }
    }
    if !cur_wt_preferred.is_empty() {
        if let Some(ref rec_wt) = manifest.recorded_worktree_root {
            raw_pairs.push((rec_wt.as_str(), cur_wt_preferred.as_str()));
        }
        if let Some(ref rec_wt_raw) = manifest.recorded_worktree_root_raw {
            raw_pairs.push((rec_wt_raw.as_str(), cur_wt_preferred.as_str()));
        }
    }
    // Sort longest `from` prefix first so child `out_dir` / `OUT_DIR` paths are always
    // rewritten before parent `worktree_root` prefixes regardless of symlink length.
    raw_pairs.sort_by_key(|a| std::cmp::Reverse(a.0.len()));
    let mut replacements: Vec<(&str, &str)> = Vec::with_capacity(raw_pairs.len());
    for (from, to) in raw_pairs {
        if !from.is_empty() && !replacements.iter().any(|(seen, _)| *seen == from) {
            replacements.push((from, to));
        }
    }

    let rewritten_dep = rewrite_path_prefixes_in_str(&manifest.dep_info_text, &replacements);
    let dep_dst = current_out_dir.join(&manifest.dep_file_name);
    write_atomic(&dep_dst, rewritten_dep.as_bytes()).map_err(|e| e.to_string())?;

    let rewritten_stderr = rewrite_stderr_for_worktree(&manifest.stderr_bytes, &replacements);
    let _ = std::io::stderr().write_all(&rewritten_stderr);

    update_counters(cache_dir, |c| {
        c.hits += 1;
        if all_cow {
            c.zero_copy_hits += 1;
        }
        c.bytes_saved += bytes_restored;
    });

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn store_successful_compile(
    cache_dir: &Path,
    manifest_path: &Path,
    inv: &CacheableInvocation,
    cwd: &Path,
    cargo_out_dir: Option<&Path>,
    worktree_root_canon: Option<&Path>,
    worktree_root_raw: Option<&Path>,
    stderr_bytes: &[u8],
) -> Result<u64, String> {
    let (Some((dep_file_name, dep_path)), emitted) = collect_emitted_artifacts(
        &inv.out_dir,
        &inv.crate_name,
        &inv.extra_filename,
        stderr_bytes,
    )?
    else {
        return Err("missing .d dep-info output".to_string());
    };
    if emitted.is_empty() {
        return Err("no cacheable artifacts emitted".to_string());
    }

    let dep_info_text = fs::read_to_string(&dep_path).map_err(|e| e.to_string())?;
    let mut parsed = parse_dep_info(&dep_info_text);

    let mut deps = Vec::with_capacity(parsed.files.len());
    for raw_dep in &parsed.files {
        if let Some((resolved_path, scope)) = classify_dep_scope(
            raw_dep,
            cwd,
            &inv.out_dir,
            cargo_out_dir,
            worktree_root_canon,
        ) {
            let (digest, _) = hash_file_blake3(&resolved_path)?;
            deps.push(RecordedDepFile {
                scope,
                blake3: digest,
            });
        }
    }

    if cargo_out_dir.is_some()
        && matches!(parsed.env_deps.get("OUT_DIR"), Some(Some(_)))
        && is_out_dir_compile_time_include_only(&inv.input_rs, &parsed.files, cwd, &deps)
    {
        parsed.env_deps.insert(
            "OUT_DIR".to_string(),
            Some(COMPILE_ONLY_OUT_DIR_SENTINEL.to_string()),
        );
    }

    let mut recorded_artifacts = Vec::with_capacity(emitted.len());
    let mut new_blob_bytes = 0u64;
    for (file_name, path) in emitted {
        let (blob_blake3, size_bytes, newly_stored) = store_blob_from(&path, cache_dir)?;
        if newly_stored {
            new_blob_bytes += size_bytes;
        }
        recorded_artifacts.push(RecordedArtifact {
            file_name,
            blob_blake3,
            size_bytes,
        });
    }

    let recorded_out_dir_canon = fs::canonicalize(&inv.out_dir)
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    let recorded_cargo_out_dir_canon = cargo_out_dir
        .and_then(|p| fs::canonicalize(p).ok())
        .map(|p| p.to_string_lossy().into_owned());

    let manifest = DirectManifest {
        schema_version: 1,
        crate_name: inv.crate_name.clone(),
        recorded_out_dir: inv.out_dir.to_string_lossy().into_owned(),
        recorded_out_dir_canon,
        recorded_worktree_root: worktree_root_canon.map(|p| p.to_string_lossy().into_owned()),
        recorded_worktree_root_raw: worktree_root_raw.map(|p| p.to_string_lossy().into_owned()),
        recorded_cargo_out_dir: cargo_out_dir.map(|p| p.to_string_lossy().into_owned()),
        recorded_cargo_out_dir_canon,
        dep_file_name,
        dep_info_text,
        deps,
        env_deps: parsed.env_deps,
        artifacts: recorded_artifacts,
        stderr_bytes: stderr_bytes.to_vec(),
    };

    if let Some(parent) = manifest_path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let encoded = serde_json::to_vec(&manifest).map_err(|e| e.to_string())?;
    write_atomic(manifest_path, &encoded).map_err(|e| e.to_string())?;

    Ok(new_blob_bytes)
}

/// Resolves the path to the sibling `gitpulse-rustc` binary next to `gitpulse-worktree`
/// (or via `GITPULSE_RUSTC_BIN` / `PATH`).
pub fn resolve_gitpulse_rustc_bin() -> Result<PathBuf, String> {
    if let Ok(explicit) = std::env::var("GITPULSE_RUSTC_BIN") {
        let p = PathBuf::from(explicit.trim());
        if p.is_file() {
            return Ok(p);
        }
    }
    let exe_name = if cfg!(windows) {
        "gitpulse-rustc.exe"
    } else {
        "gitpulse-rustc"
    };
    if let Ok(cur_exe) = std::env::current_exe() {
        if let Some(dir) = cur_exe.parent() {
            let direct = dir.join(exe_name);
            if direct.is_file() {
                return Ok(direct);
            }
            // When `cur_exe` is inside `target/debug/deps/<test-bin>`, check parent `target/debug/`.
            if let Some(parent_dir) = dir.parent() {
                let sibling = parent_dir.join(exe_name);
                if sibling.is_file() {
                    return Ok(sibling);
                }
            }
        }
    }
    Err(format!(
        "cannot locate `{exe_name}` binary beside current executable (build `{exe_name}` or set GITPULSE_RUSTC_BIN)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_dep_info_handles_spaces_continuations_phony_rules_and_env_deps() {
        let sample = "\
/tmp/wt/target/debug/deps/libdemo-abc.rmeta: /tmp/wt/src/lib.rs \\
  /tmp/wt/src/my\\ module.rs /tmp/wt/data/hello\\ world.txt

/tmp/wt/src/lib.rs:
/tmp/wt/src/my\\ module.rs:
# env-dep:DEMO_TOKEN=alpha\\nbeta
# comment ignored
";
        let parsed = parse_dep_info(sample);
        assert_eq!(
            parsed.files,
            vec![
                "/tmp/wt/src/lib.rs".to_string(),
                "/tmp/wt/src/my module.rs".to_string(),
                "/tmp/wt/data/hello world.txt".to_string(),
            ]
        );
        assert_eq!(
            parsed.env_deps.get("DEMO_TOKEN"),
            Some(&Some("alpha\nbeta".to_string()))
        );
    }

    #[test]
    fn classify_rustc_args_filters_probes_build_scripts_and_incremental() {
        let cwd = Path::new("/tmp/ws");
        assert!(matches!(
            classify_rustc_args(&["-vV".to_string()], cwd),
            Classification::Probe
        ));
        assert!(matches!(
            classify_rustc_args(
                &[
                    "--crate-name".to_string(),
                    "build_script_build".to_string(),
                    "--crate-type".to_string(),
                    "bin".to_string(),
                    "--out-dir".to_string(),
                    "/tmp/out".to_string(),
                    "build.rs".to_string(),
                ],
                cwd
            ),
            Classification::Passthrough
        ));
        assert!(matches!(
            classify_rustc_args(
                &[
                    "--crate-name".to_string(),
                    "mylib".to_string(),
                    "--crate-type".to_string(),
                    "lib".to_string(),
                    "-C".to_string(),
                    "incremental=/tmp/inc".to_string(),
                    "--out-dir".to_string(),
                    "/tmp/out".to_string(),
                    "src/lib.rs".to_string(),
                ],
                cwd
            ),
            Classification::Passthrough
        ));
        match classify_rustc_args(
            &[
                "--crate-name".to_string(),
                "mylib".to_string(),
                "--crate-type".to_string(),
                "lib".to_string(),
                "--emit=dep-info,metadata,link".to_string(),
                "-C".to_string(),
                "extra-filename=-1234".to_string(),
                "--out-dir".to_string(),
                "/tmp/out".to_string(),
                "src/lib.rs".to_string(),
            ],
            cwd,
        ) {
            Classification::Cacheable(c) => {
                assert_eq!(c.crate_name, "mylib");
                assert_eq!(c.extra_filename, "-1234");
                assert_eq!(c.out_dir, PathBuf::from("/tmp/out"));
                assert_eq!(c.input_rs, PathBuf::from("/tmp/ws/src/lib.rs"));
            }
            other => panic!("expected Cacheable, got {other:?}"),
        }
    }

    #[test]
    fn rewrite_stderr_for_worktree_updates_json_artifact_paths() {
        let stderr = br#"{"$message_type":"artifact","artifact":"/tmp/wt-a/target/debug/deps/libfoo-1.rmeta","emit":"metadata"}
"#;
        let out = rewrite_stderr_for_worktree(
            stderr,
            &[
                ("/tmp/wt-a/target/debug/deps", "/tmp/wt-b/target/debug/deps"),
                ("/tmp/wt-a", "/tmp/wt-b"),
            ],
        );
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("/tmp/wt-b/target/debug/deps/libfoo-1.rmeta"), "{text}");
        assert!(!text.contains("/tmp/wt-a"), "{text}");
    }

    #[test]
    fn out_dir_lexical_whitelist_accepts_compile_time_includes_and_rejects_runtime_embedding() {
        let ok_src = r#"
            // env!("OUT_DIR") in comment is ignored
            /* nested /* env!("OUT_DIR") */ comment */
            const QUOTE: char = '"';
            include!(concat!(env!("OUT_DIR"), "/gen.rs"));
            pub const DATA: &str = include_str!(concat!(std::env!("OUT_DIR"), "/table.txt"));
            pub const BYTES: &[u8] = include_bytes!(concat!(::core::env!("OUT_DIR", "missing"), "/blob.bin",));
            pub const PKG: &str = env!("CARGO_PKG_NAME");
        "#;
        let tokens = lex_rust_simple_tokens(ok_src).expect("lex ok_src");
        assert_eq!(check_tokens_out_dir_compile_only(&tokens), (3, true));

        for bad_src in [
            r#"pub fn baked() -> &'static str { env!("OUT_DIR") }"#,
            r#"pub const PATH: &str = concat!(env!("OUT_DIR"), "/gen.rs");"#,
            r#"pub const ESC: &str = env!("OUT\x5fDIR");"#,
            r#"pub const DYN: &str = env!(concat!("OUT", "_DIR"));"#,
            r##"pub const RAW: &str = r#"OUT_DIR"#;"##,
            r#"include!(concat!(env!("OUT_DIR")));"#,
        ] {
            let toks = lex_rust_simple_tokens(bad_src).expect("lex bad_src");
            let (_, allowed) = check_tokens_out_dir_compile_only(&toks);
            assert!(!allowed, "expected fail-closed rejection for: {bad_src}");
        }
    }
}
