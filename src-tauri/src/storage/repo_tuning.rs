//! Repository tuning advisor: a read-only detector, measured thresholds, and
//! the plan for the one user-clicked action that may write git config.
//!
//! This module is in three parts, in the order they are used:
//!
//! 1. the detector ([`inspect`]): what the repository's maintenance, object
//!    store, index and tuning keys look like right now;
//! 2. the recommendations ([`recommend`]): which tuning is worth offering, each
//!    behind a threshold taken from a committed measurement
//!    (docs/PERFORMANCE.md, "2026-11-06: repository tuning thresholds");
//! 3. the plan ([`plan_apply`]): the exact argv, scope and undo of each change.
//!    `GitWriter::tuning_apply` is the only code that runs one.
//!
//! # Zero spawns, by choice (the detector)
//!
//! Like `storage/commit_graph.rs`, the detector spawns nothing. It reads the
//! files git would read (`objects/pack`, `objects/17`, the index header) and
//! takes git's effective config, with each value's scope, from
//! `git_reader::config_entries`, the cached snapshot every other reader shares.
//! It never parses a config file itself: a hand-rolled parser disagrees with git
//! about includes, conditional includes and quoting. Whatever it cannot read is
//! reported as unknown (`None`, with a note), never as absent or as zero.

mod advice;
pub use advice::*;

use crate::engine::git_reader::{ConfigEntry, ConfigScope};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Directory entries read from `objects/pack` or `objects/17` before the count
/// is given up as unknown rather than reported as a sample.
const MAX_DIR_ENTRIES: usize = 1_000_000;
/// `maintenance.repo` lines read from one config before the answer is unknown.
const MAX_MAINTENANCE_REPOS: usize = 100_000;
/// git's default for `gc.auto` (`git help config`).
pub const GC_AUTO_DEFAULT: u64 = 6700;
/// The index's fixed leading bytes: `DIRC`, version, entry count.
pub const INDEX_HEADER_LEN: usize = 12;
/// Smallest on-disk size of one index entry (the fixed fields of a SHA-1 entry
/// are 62 bytes); a count the file cannot hold is a corrupt header.
const MIN_INDEX_ENTRY_BYTES: u64 = 62;

/// The tuning keys the detector reports, lowercase as git's config list prints
/// them, with the spelling a person types.
pub const TUNING_KEYS: [(&str, &str); 7] = [
    ("feature.manyfiles", "feature.manyFiles"),
    ("core.fsmonitor", "core.fsmonitor"),
    ("core.untrackedcache", "core.untrackedCache"),
    ("core.splitindex", "core.splitIndex"),
    ("index.version", "index.version"),
    ("core.commitgraph", "core.commitGraph"),
    ("gc.writecommitgraph", "gc.writeCommitGraph"),
];

// ---------------------------------------------------------------------------
// Detector types
// ---------------------------------------------------------------------------

/// Global-config maintenance registration (`git maintenance register`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintenanceState {
    /// A global or system `maintenance.repo` names this repository. `None`
    /// when the config could not be read.
    pub registered: Option<bool>,
    pub strategy: Option<String>,
    pub auto: Option<String>,
}

/// Pack and loose-object pressure against `gc.auto`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectPressure {
    /// `.pack` files in `objects/pack`; `None` when that could not be counted.
    pub packs: Option<u64>,
    /// Loose objects; `None` when unknown.
    pub loose_objects: Option<u64>,
    /// `loose_objects` came from `git count-objects -v` (the storage scan's
    /// count); otherwise it is git's own estimate, 256 times the objects in
    /// `objects/17`.
    pub loose_exact: bool,
    /// `gc.auto` in effect; `None` when the config could not be read.
    pub gc_auto: Option<u64>,
    /// `loose_objects` is past `gc.auto` (always false when `gc.auto` is 0,
    /// which turns auto-gc off). `None` when either side is unknown.
    pub loose_over_gc_auto: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IndexReadState {
    /// The header was read and is a version this reader knows.
    Present,
    /// No index file (a repository with nothing checked out yet).
    Missing,
    /// The file exists and could not be understood. `IndexInfo::note` says why.
    #[default]
    Unknown,
}

/// What the index header says, for the git dir that owns it (a linked
/// worktree has its own).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexInfo {
    pub state: IndexReadState,
    pub version: Option<u32>,
    /// Entry count from the header: the tracked-file count.
    pub entries: Option<u64>,
    /// `entries` is the whole tree. False under split-index or a sparse index,
    /// where the header counts only part of it.
    pub entries_exact: bool,
    /// The trailing checksum is all zero (`index.skipHash`). `None` when the
    /// trailer could not be read.
    pub skip_hash: Option<bool>,
    pub note: Option<String>,
}

/// One tuning key's value and the config layer that supplied it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TuningKeyValue {
    /// The key as a person types it (`core.fsmonitor`).
    pub key: String,
    /// `None`: unset (config readable) or unknown (see `RepoTuningReport::config_note`).
    pub value: Option<String>,
    pub origin: Option<ConfigScope>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoTuningReport {
    pub maintenance: MaintenanceState,
    /// `objects/pack/multi-pack-index` (or an incremental chain) exists.
    /// `None` when `objects/pack` could not be read.
    pub midx: Option<bool>,
    pub objects: ObjectPressure,
    pub index: IndexInfo,
    pub partial_clone: Option<bool>,
    pub sparse_checkout: Option<bool>,
    pub keys: Vec<TuningKeyValue>,
    /// Why every config-derived field is unknown, when the config could not be
    /// read.
    pub config_note: Option<String>,
    /// Facts the numbers above cannot carry (alternates, an unreadable pack
    /// directory), in sentences.
    pub notes: Vec<String>,
}

impl RepoTuningReport {
    /// A key's current value, by its lowercase name.
    pub fn key(&self, lower: &str) -> Option<&TuningKeyValue> {
        let display = TUNING_KEYS.iter().find(|(l, _)| *l == lower)?.1;
        self.keys.iter().find(|k| k.key == display)
    }

    fn value_of(&self, lower: &str) -> Option<&str> {
        self.key(lower).and_then(|k| k.value.as_deref())
    }

    /// A key reads as true: git's true spellings, or a bare key.
    fn is_true(&self, lower: &str) -> bool {
        self.value_of(lower).is_some_and(config_true)
    }
}

/// Git's boolean true spellings; a key with no `=` prints as empty and git
/// reads that as true for a boolean.
pub fn config_true(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "yes" | "on" | "1" | ""
    )
}

/// A non-negative integer as git reads one: decimal with an optional `k`, `m`
/// or `g` suffix. `None` for anything else (including a negative or overflow).
fn parse_git_unsigned(value: &str) -> Option<u64> {
    let value = value.trim();
    let (digits, factor) = match value.chars().last()? {
        'k' | 'K' => (&value[..value.len() - 1], 1u64 << 10),
        'm' | 'M' => (&value[..value.len() - 1], 1 << 20),
        'g' | 'G' => (&value[..value.len() - 1], 1 << 30),
        _ => (value, 1),
    };
    digits.trim().parse::<u64>().ok()?.checked_mul(factor)
}

// ---------------------------------------------------------------------------
// Index header
// ---------------------------------------------------------------------------

/// The twelve leading bytes of `.git/index` (gitformat-index).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexHeader {
    pub version: u32,
    pub entries: u32,
}

/// Parses the index header. Accepts versions 2, 3 and 4 (the ones git writes);
/// a longer slice is fine, only the first twelve bytes are read. Total over
/// arbitrary input: it returns an error, never panics.
pub fn parse_index_header(bytes: &[u8]) -> Result<IndexHeader, String> {
    let head = bytes
        .get(..INDEX_HEADER_LEN)
        .ok_or_else(|| format!("only {} bytes, an index header is 12", bytes.len()))?;
    if &head[0..4] != b"DIRC" {
        return Err("not a git index (bad signature)".to_string());
    }
    let version = u32::from_be_bytes([head[4], head[5], head[6], head[7]]);
    if !(2..=4).contains(&version) {
        return Err(format!("unsupported index version {version}"));
    }
    let entries = u32::from_be_bytes([head[8], head[9], head[10], head[11]]);
    Ok(IndexHeader { version, entries })
}

/// Reads the index of `git_dir` without a process. `hash_len` is the
/// repository's object-id width, which is the trailer's width.
fn read_index(git_dir: &Path, hash_len: usize, entries_exact: bool) -> IndexInfo {
    let path = git_dir.join("index");
    let unknown = |note: String| IndexInfo {
        state: IndexReadState::Unknown,
        note: Some(note),
        ..IndexInfo::default()
    };
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return IndexInfo {
                state: IndexReadState::Missing,
                ..IndexInfo::default()
            }
        }
        Err(e) => return unknown(format!("the index could not be examined: {e}")),
    };
    if !meta.is_file() {
        return unknown("the index is not a regular file".to_string());
    }
    let len = meta.len();
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(e) => return unknown(format!("the index could not be opened: {e}")),
    };
    let mut head = [0u8; INDEX_HEADER_LEN];
    if let Err(e) = file.read_exact(&mut head) {
        return unknown(format!("the index header could not be read: {e}"));
    }
    let header = match parse_index_header(&head) {
        Ok(header) => header,
        Err(why) => return unknown(why),
    };
    let entries = u64::from(header.entries);
    if entries.saturating_mul(MIN_INDEX_ENTRY_BYTES) > len {
        return unknown(format!(
            "the index header claims {entries} entries, more than a {len} byte file can hold"
        ));
    }
    // `index.skipHash` writes an all-zero checksum in place of the hash of the
    // file; the zero trailer is what tells it apart.
    let skip_hash = if len >= (INDEX_HEADER_LEN + hash_len) as u64 {
        let mut trailer = vec![0u8; hash_len];
        file.seek(SeekFrom::End(-(hash_len as i64)))
            .and_then(|_| file.read_exact(&mut trailer))
            .ok()
            .map(|()| trailer.iter().all(|b| *b == 0))
    } else {
        None
    };
    IndexInfo {
        state: IndexReadState::Present,
        version: Some(header.version),
        entries: Some(entries),
        entries_exact,
        skip_hash,
        note: (!entries_exact).then(|| {
            "split-index or a sparse index is in use, so the header counts only part of the tree"
                .to_string()
        }),
    }
}

// ---------------------------------------------------------------------------
// Object store
// ---------------------------------------------------------------------------

/// Counts supplied by a caller that already ran `git count-objects -v` (the
/// storage scan). They replace the file estimate for the same fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectCounts {
    pub loose_objects: u64,
    pub packs: u64,
}

fn count_dir(dir: &Path, keep: impl Fn(&str) -> bool) -> Result<Option<u64>, String> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Some(0)),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    let mut count = 0u64;
    for (seen, entry) in entries.enumerate() {
        if seen >= MAX_DIR_ENTRIES {
            return Ok(None);
        }
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        if entry.file_name().to_str().is_some_and(&keep) {
            count += 1;
        }
    }
    Ok(Some(count))
}

fn object_pressure(
    objects: &Path,
    hash_len: usize,
    counts: Option<ObjectCounts>,
    gc_auto: Option<u64>,
    notes: &mut Vec<String>,
) -> (ObjectPressure, Option<bool>) {
    let pack_dir = objects.join("pack");
    let (packs, midx) = match count_dir(&pack_dir, |n| n.ends_with(".pack")) {
        Ok(Some(n)) => {
            let midx = pack_dir.join("multi-pack-index").is_file()
                || pack_dir
                    .join("multi-pack-index.d")
                    .join("multi-pack-index-chain")
                    .is_file();
            (Some(n), Some(midx))
        }
        Ok(None) => {
            notes.push("objects/pack holds too many entries to count".to_string());
            (None, None)
        }
        Err(why) => {
            notes.push(format!("objects/pack could not be read: {why}"));
            (None, None)
        }
    };
    let (loose, exact) = match counts {
        Some(counts) => (Some(counts.loose_objects), true),
        None => {
            let sample = objects.join("17");
            let hex = hash_len * 2 - 2;
            let one_dir = count_dir(&sample, |n| {
                n.len() == hex && n.bytes().all(|b| b.is_ascii_hexdigit())
            });
            match one_dir {
                Ok(Some(n)) => (Some(n.saturating_mul(256)), false),
                Ok(None) => (None, false),
                Err(why) => {
                    notes.push(format!("loose objects could not be sampled: {why}"));
                    (None, false)
                }
            }
        }
    };
    if objects.join("info").join("alternates").exists() {
        notes.push(
            "this repository borrows objects through alternates; the counts cover only its own"
                .to_string(),
        );
    }
    let packs = counts.map(|c| c.packs).or(packs);
    let over = match (loose, gc_auto) {
        (Some(_), Some(0)) => Some(false),
        (Some(loose), Some(limit)) => Some(loose > limit),
        _ => None,
    };
    (
        ObjectPressure {
            packs,
            loose_objects: loose,
            loose_exact: exact,
            gc_auto,
            loose_over_gc_auto: over,
        },
        midx,
    )
}

// ---------------------------------------------------------------------------
// Config-derived facts
// ---------------------------------------------------------------------------

fn last<'a>(config: &'a [ConfigEntry], key: &str) -> Option<&'a ConfigEntry> {
    config.iter().rev().find(|entry| entry.key == key)
}

/// A path as git would compare it: canonical when it exists, else the text
/// without trailing separators.
fn comparable(path: &str) -> PathBuf {
    let trimmed = path.trim();
    fs::canonicalize(trimmed)
        .unwrap_or_else(|_| PathBuf::from(trimmed.trim_end_matches(['/', '\\'])))
}

fn maintenance_state(
    config: &[ConfigEntry],
    repo: &Path,
    common_dir: &Path,
    notes: &mut Vec<String>,
) -> MaintenanceState {
    let mut mine: Vec<PathBuf> = [Some(repo), Some(common_dir), common_dir.parent()]
        .into_iter()
        .flatten()
        .map(|p| comparable(&p.to_string_lossy()))
        .collect();
    mine.dedup();
    let registered: Vec<&ConfigEntry> = config
        .iter()
        .filter(|e| e.key == "maintenance.repo")
        // `git maintenance run` reads this key from the system and global
        // files only; a repository's own list is ignored.
        .filter(|e| {
            matches!(
                e.scope,
                ConfigScope::Global | ConfigScope::System | ConfigScope::Unknown
            )
        })
        .collect();
    let is_registered = if registered.len() > MAX_MAINTENANCE_REPOS {
        notes.push("maintenance.repo lists too many repositories to search".to_string());
        None
    } else {
        Some(
            registered
                .iter()
                .any(|e| mine.contains(&comparable(&e.value))),
        )
    };
    MaintenanceState {
        registered: is_registered,
        strategy: last(config, "maintenance.strategy").map(|e| e.value.clone()),
        auto: last(config, "maintenance.auto").map(|e| e.value.clone()),
    }
}

fn is_partial_clone(config: &[ConfigEntry]) -> bool {
    let last_wins: std::collections::HashMap<String, String> = config
        .iter()
        .map(|e| (e.key.clone(), e.value.clone()))
        .collect();
    super::commit_graph::config_is_partial_clone(&last_wins)
}

fn is_sparse(git_dir: &Path, common_dir: &Path, config: &[ConfigEntry]) -> bool {
    last(config, "core.sparsecheckout").is_some_and(|e| config_true(&e.value))
        // A linked worktree keeps its cone in its own git dir.
        || git_dir.join("info").join("sparse-checkout").is_file()
        || common_dir.join("info").join("sparse-checkout").is_file()
}

// ---------------------------------------------------------------------------
// inspect
// ---------------------------------------------------------------------------

/// Inspects the repository at `repo` whose private git dir is `git_dir` and
/// whose common dir (objects, config) is `common_dir`; for a linked worktree
/// the two differ and each owns different facts. `counts` are the storage
/// scan's `count-objects` numbers when it has them.
///
/// The one process this may cause is the shared `config --list`, on a cold
/// cache; everything else is a file read.
pub fn inspect(
    repo: &Path,
    git_dir: &Path,
    common_dir: &Path,
    counts: Option<ObjectCounts>,
) -> RepoTuningReport {
    let config =
        crate::engine::git_reader::config_entries(repo).map_err(|e| e.message().to_string());
    inspect_with(
        repo,
        git_dir,
        common_dir,
        counts,
        config.as_deref().map_err(String::as_str),
    )
}

/// [`inspect`] with the config read straight from git (one process), for the
/// apply, which must not plan or report from a cache that cannot see the global
/// config.
pub fn inspect_fresh(
    repo: &Path,
    git_dir: &Path,
    common_dir: &Path,
    counts: Option<ObjectCounts>,
) -> RepoTuningReport {
    let config =
        crate::engine::git_reader::config_entries_fresh(repo).map_err(|e| e.message().to_string());
    inspect_with(
        repo,
        git_dir,
        common_dir,
        counts,
        config.as_deref().map_err(String::as_str),
    )
}

/// [`inspect`] with the config given, so a test can stand in any config and
/// prove nothing here reaches for git.
pub fn inspect_with(
    repo: &Path,
    git_dir: &Path,
    common_dir: &Path,
    counts: Option<ObjectCounts>,
    config: Result<&[ConfigEntry], &str>,
) -> RepoTuningReport {
    let mut notes = Vec::new();
    let objects = common_dir.join("objects");
    let (entries, config_note): (&[ConfigEntry], Option<String>) = match config {
        Ok(entries) => (entries, None),
        Err(why) => (
            &[],
            Some(crate::alerts::text::sanitize(&format!(
                "git config could not be read, so config-derived facts are unknown: {why}"
            ))),
        ),
    };
    let readable = config_note.is_none();
    let hash_len = if last(entries, "extensions.objectformat")
        .is_some_and(|e| e.value.eq_ignore_ascii_case("sha256"))
    {
        32
    } else {
        20
    };
    // An unset key is git's default; a value that is set and unreadable is
    // unknown, never the default.
    let gc_auto = match (readable, last(entries, "gc.auto")) {
        (false, _) => None,
        (true, None) => Some(GC_AUTO_DEFAULT),
        (true, Some(entry)) => parse_git_unsigned(&entry.value),
    };
    let (object_pressure, midx) = object_pressure(&objects, hash_len, counts, gc_auto, &mut notes);

    let split_or_sparse = readable
        && (last(entries, "core.splitindex").is_some_and(|e| config_true(&e.value))
            || last(entries, "index.sparse").is_some_and(|e| config_true(&e.value)));
    let mut index = read_index(git_dir, hash_len, !split_or_sparse);
    if !readable && index.state == IndexReadState::Present {
        // Whether the header counts the whole tree depends on config.
        index.entries_exact = false;
        index.note =
            Some("config unreadable: the entry count may cover only part of the tree".into());
    }

    let keys = TUNING_KEYS
        .iter()
        .map(|(lower, display)| {
            let found = last(entries, lower);
            TuningKeyValue {
                key: (*display).to_string(),
                value: found.map(|e| e.value.clone()),
                origin: found.map(|e| e.scope),
            }
        })
        .collect();

    RepoTuningReport {
        maintenance: if readable {
            maintenance_state(entries, repo, common_dir, &mut notes)
        } else {
            MaintenanceState::default()
        },
        midx,
        objects: object_pressure,
        index,
        partial_clone: readable.then(|| is_partial_clone(entries)),
        sparse_checkout: readable.then(|| is_sparse(git_dir, common_dir, entries)),
        keys,
        config_note,
        notes,
    }
}

#[cfg(test)]
mod advice_tests;
#[cfg(test)]
mod detector_tests;
