//! A bounded structural read of a whole git index (gitformat-index).
//!
//! The twelve byte header says how many entries the file claims; it does not
//! say the file holds them. A header over a truncated or garbage body used to be
//! reported as that many tracked files, and a skipHash index has no checksum to
//! catch it. This walks the entries and the extension chain instead, stream
//! reading, and so also learns what only the body can say: whether the index is
//! a split index (the `link` extension) or a sparse index (`sdir`).
//!
//! What it checks, from the format document: each entry's fixed fields, a
//! non-empty path, the `0x4000` extended-flags bit only from version 3, the NUL
//! padding of versions 2 and 3, the prefix-compressed path of version 4
//! (strip count, then a NUL-terminated suffix that must leave a non-empty
//! path), and that the extensions (four byte signature starting with a letter,
//! four byte size) end exactly where the trailing checksum begins.
//!
//! What it does NOT check: the checksum itself. No SHA-1 implementation is a
//! direct dependency of this crate and adding one is not allowed here, so a
//! structurally sound but bit-rotted index reads as sound. The read is bounded
//! by [`MAX_INDEX_BYTES`]; a larger index is reported as unreadable, never
//! guessed at. Results are cached by path, size, modification time and inode.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use super::IndexHeader;

/// An index larger than this is not walked (about 3.5 million entries).
pub const MAX_INDEX_BYTES: u64 = 256 * 1024 * 1024;
/// A repository config larger than this falls back to git instead of being
/// parsed in process.
const MAX_CONFIG_BYTES: u64 = 256 * 1024;
const INDEX_HEADER_LEN: usize = 12;
const MIN_INDEX_ENTRY_BYTES: u64 = 62;
/// Mask and value for a gitlink (submodule) mode in `.git/index`.
const GITLINK_MODE_MASK: u32 = 0o170_000;
const GITLINK_MODE: u32 = 0o160_000;
/// A path component the format allows is far shorter; a v4 suffix longer than
/// this is a corrupt file.
const MAX_PATH_BYTES: usize = 1 << 16;
/// Extensions walked before the chain is called corrupt.
const MAX_EXTENSIONS: usize = 64;
const CACHE_CAPACITY: usize = 64;

/// What the walk found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexShape {
    /// The `link` extension: a split index.
    pub split: bool,
    /// The `sdir` extension: a sparse index (directories stand for subtrees).
    pub sparse: bool,
    /// The trailing checksum is all zero (`index.skipHash`).
    pub skip_hash: bool,
}

#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    len: u64,
    mtime_ns: u128,
    inode: u64,
}

type Cache = Mutex<HashMap<PathBuf, (Stamp, Result<IndexShape, String>)>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

#[cfg(unix)]
fn inode_of(meta: &std::fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::ino(meta)
}

#[cfg(windows)]
fn inode_of(meta: &std::fs::Metadata) -> u64 {
    use std::os::windows::fs::MetadataExt;
    meta.creation_time()
}

#[cfg(all(not(unix), not(windows)))]
fn inode_of(_meta: &std::fs::Metadata) -> u64 {
    0
}

fn stamp_of(meta: &std::fs::Metadata) -> Stamp {
    Stamp {
        len: meta.len(),
        mtime_ns: meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos()),
        inode: inode_of(meta),
    }
}

/// Opens `path` without following a trailing symlink or blocking on a FIFO,
/// then confirms via the opened descriptor's metadata that it is a regular file.
pub(super) fn open_regular_file_no_follow(path: &Path) -> std::io::Result<(File, fs::Metadata)> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?
    };
    #[cfg(not(unix))]
    let file = File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "expected a regular file",
        ));
    }
    Ok((file, meta))
}

/// Walks the index at `path`, whose header is `header` and whose metadata is
/// `meta`, with the repository's object-id width `hash_len`. Cached.
pub fn walk(
    path: &Path,
    meta: &std::fs::Metadata,
    header: IndexHeader,
    hash_len: usize,
) -> Result<IndexShape, String> {
    let stamp = stamp_of(meta);
    // A file written in the last few seconds may be rewritten in the same
    // timestamp tick, so a young entry is not trusted from the cache.
    let young = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(true, |now| {
            now.as_nanos().saturating_sub(stamp.mtime_ns) < 3_000_000_000
        });
    if !young {
        if let Some((seen, answer)) = cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(path)
        {
            if *seen == stamp {
                return answer.clone();
            }
        }
    }
    let answer = open_regular_file_no_follow(path)
        .map_err(|e| format!("the index could not be opened: {e}"))
        .and_then(|(file, opened_meta)| walk_file(file, opened_meta.len(), header, hash_len));
    let mut cache = cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if cache.len() >= CACHE_CAPACITY {
        cache.clear();
    }
    cache.insert(path.to_path_buf(), (stamp, answer.clone()));
    answer
}

/// A reader that knows how far into the file it is.
struct Cursor {
    inner: BufReader<File>,
    pos: u64,
    end: u64,
}

impl Cursor {
    fn take(&mut self, buf: &mut [u8]) -> Result<(), String> {
        let want = buf.len() as u64;
        if self.pos + want > self.end {
            return Err("an entry or extension runs past the end of the index".to_string());
        }
        self.inner
            .read_exact(buf)
            .map_err(|e| format!("the index could not be read: {e}"))?;
        self.pos += want;
        Ok(())
    }

    fn byte(&mut self) -> Result<u8, String> {
        let mut one = [0u8; 1];
        self.take(&mut one)?;
        Ok(one[0])
    }

    fn skip(&mut self, n: u64) -> Result<(), String> {
        if self.pos + n > self.end {
            return Err("an extension runs past the end of the index".to_string());
        }
        self.inner
            .seek_relative(i64::try_from(n).map_err(|_| "extension too large".to_string())?)
            .map_err(|e| format!("the index could not be read: {e}"))?;
        self.pos += n;
        Ok(())
    }

    /// Reads bytes up to and including a NUL, appending path bytes into `out`
    /// and returning how many came before the NUL.
    fn append_until_nul(&mut self, out: &mut Vec<u8>) -> Result<usize, String> {
        let mut n = 0usize;
        loop {
            let byte = self.byte()?;
            if byte == 0 {
                return Ok(n);
            }
            n += 1;
            if out.len() + 1 > MAX_PATH_BYTES {
                return Err("an entry path is implausibly long".to_string());
            }
            out.push(byte);
        }
    }
}

fn walk_file(
    file: File,
    len: u64,
    header: IndexHeader,
    hash_len: usize,
) -> Result<IndexShape, String> {
    walk_entries(file, len, header, hash_len, |_, _| Ok(()))
}

fn walk_entries(
    file: File,
    len: u64,
    header: IndexHeader,
    hash_len: usize,
    mut on_entry: impl FnMut(u32, &[u8]) -> Result<(), String>,
) -> Result<IndexShape, String> {
    if len > MAX_INDEX_BYTES {
        return Err(format!(
            "the index is larger than the {} MiB read bound",
            MAX_INDEX_BYTES >> 20
        ));
    }
    let end = len
        .checked_sub(hash_len as u64)
        .filter(|end| *end >= INDEX_HEADER_LEN as u64)
        .ok_or_else(|| "the index is shorter than its header and checksum".to_string())?;
    let mut cur = Cursor {
        inner: BufReader::with_capacity(64 * 1024, file),
        pos: 0,
        end,
    };
    let mut head = [0u8; INDEX_HEADER_LEN];
    cur.inner
        .read_exact(&mut head)
        .map_err(|e| format!("the index header could not be read: {e}"))?;
    cur.pos = INDEX_HEADER_LEN as u64;

    let fixed = 40 + hash_len + 2;
    let mut fields = vec![0u8; fixed];
    let mut path_buf: Vec<u8> = Vec::new();
    let mut has_skip_worktree = false;
    for _ in 0..header.entries {
        cur.take(&mut fields)?;
        let mode = u32::from_be_bytes([fields[24], fields[25], fields[26], fields[27]]);
        let flags = u16::from_be_bytes([fields[fixed - 2], fields[fixed - 1]]);
        let mut fixed_total = fixed as u64;
        if flags & 0x4000 != 0 {
            if header.version < 3 {
                return Err("an entry carries extended flags in a version 2 index".to_string());
            }
            let mut extended = [0u8; 2];
            cur.take(&mut extended)?;
            if u16::from_be_bytes(extended) & 0x4000 != 0 {
                has_skip_worktree = true;
            }
            fixed_total += 2;
        }
        let name_len = usize::from(flags & 0x0FFF);
        if header.version < 4 {
            let long = name_len >= 0x0FFF;
            path_buf.clear();
            let path_len = if !long {
                if name_len == 0 {
                    return Err("an entry has an empty path".to_string());
                }
                path_buf.resize(name_len, 0);
                cur.take(&mut path_buf)?;
                name_len
            } else {
                // The 12 bit length saturates; the path runs to its NUL, which
                // `append_until_nul` consumes along with it.
                let n = cur.append_until_nul(&mut path_buf)?;
                if n == 0 {
                    return Err("an entry has an empty path".to_string());
                }
                n
            };
            // Entries are padded with 1 to 8 NULs to a multiple of 8.
            let size = (fixed_total + path_len as u64 + 8) & !7;
            let already = fixed_total + path_len as u64 + u64::from(long);
            let pad = size
                .checked_sub(already)
                .ok_or_else(|| "an entry is mis-sized".to_string())?;
            let mut padding = vec![0u8; usize::try_from(pad).unwrap_or(8)];
            cur.take(&mut padding)?;
            if padding.iter().any(|b| *b != 0) {
                return Err("an entry's padding is not NUL".to_string());
            }
            on_entry(mode, &path_buf)?;
        } else {
            // Version 4: offset-encoded strip count, then a NUL-terminated suffix.
            let mut b = cur.byte()?;
            let mut strip = usize::from(b & 0x7F);
            let mut bytes = 1;
            while b & 0x80 != 0 {
                bytes += 1;
                if bytes > 5 {
                    return Err("a strip count is implausibly long".to_string());
                }
                b = cur.byte()?;
                strip = ((strip + 1) << 7) | usize::from(b & 0x7F);
            }
            if strip > path_buf.len() {
                return Err("an entry strips more than the previous path".to_string());
            }
            path_buf.truncate(path_buf.len() - strip);
            cur.append_until_nul(&mut path_buf)?;
            if path_buf.is_empty() {
                return Err("an entry has an empty path".to_string());
            }
            on_entry(mode, &path_buf)?;
        }
    }

    let mut shape = IndexShape {
        split: false,
        sparse: has_skip_worktree,
        skip_hash: false,
    };
    let mut extensions = 0usize;
    while cur.pos < end {
        extensions += 1;
        if extensions > MAX_EXTENSIONS {
            return Err("the index lists implausibly many extensions".to_string());
        }
        let mut ext = [0u8; 8];
        cur.take(&mut ext)?;
        if !ext[0].is_ascii_alphabetic() {
            return Err("an extension signature does not start with a letter".to_string());
        }
        match &ext[..4] {
            b"link" => shape.split = true,
            b"sdir" => shape.sparse = true,
            _ => {}
        }
        let size = u64::from(u32::from_be_bytes([ext[4], ext[5], ext[6], ext[7]]));
        cur.skip(size)?;
    }
    if cur.pos != end {
        return Err("the extensions do not end where the checksum begins".to_string());
    }
    let mut trailer = vec![0u8; hash_len];
    cur.inner
        .read_exact(&mut trailer)
        .map_err(|e| format!("the index checksum could not be read: {e}"))?;
    shape.skip_hash = trailer.iter().all(|b| *b == 0);
    Ok(shape)
}

/// Result of reading tracked paths from `.git/index` without spawning `git`.
/// Test-only since `GitReader::list_repo_files` moved to `git ls-files`: its
/// parser and fail-closed tests are kept until the index walker is retired.
#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TrackedIndexPaths {
    Paths(Vec<String>),
    OverLimit,
    Unsupported,
}

/// Returns true when the repository configuration or checkout state sets
/// options that `dcgrep` + direct `.git/index` reading do not emulate:
/// worktree-local config, sparse checkout, `[include]` / `[includeIf]`,
/// repo-local `core.excludesfile`, `core.bare`, `core.splitIndex`,
/// `index.sparse`, or macOS `core.precomposeunicode = false`.
fn config_bool_strict(raw: &str) -> Option<bool> {
    if raw.eq_ignore_ascii_case("true")
        || raw.eq_ignore_ascii_case("yes")
        || raw.eq_ignore_ascii_case("on")
        || raw == "1"
    {
        Some(true)
    } else if raw.is_empty()
        || raw.eq_ignore_ascii_case("false")
        || raw.eq_ignore_ascii_case("no")
        || raw.eq_ignore_ascii_case("off")
        || raw == "0"
    {
        Some(false)
    } else {
        None
    }
}

pub(crate) fn repo_has_custom_ignore_or_unicode_config(git_dir: &Path, common_dir: &Path) -> bool {
    if std::fs::symlink_metadata(git_dir.join("config.worktree")).is_ok()
        || std::fs::symlink_metadata(git_dir.join("info").join("sparse-checkout")).is_ok()
        || std::fs::symlink_metadata(common_dir.join("info").join("sparse-checkout")).is_ok()
    {
        return true;
    }
    let mut custom = false;
    let mut bare = false;
    let mut split_index = false;
    let mut sparse_checkout = false;
    let mut index_sparse = false;
    let mut precompose_false = false;
    let parsed = visit_git_config(&common_dir.join("config"), |section, key, value| {
        if section == "include" || section == "includeif" {
            custom = true;
            return false;
        }
        if section == "core" {
            match key {
                "excludesfile" | "worktree" => {
                    custom = true;
                    return false;
                }
                "bare" => bare = value.is_none_or(|v| config_bool_strict(v).unwrap_or(true)),
                "splitindex" => {
                    split_index = value.is_none_or(|v| config_bool_strict(v).unwrap_or(true))
                }
                "sparsecheckout" => {
                    sparse_checkout = value.is_none_or(|v| config_bool_strict(v).unwrap_or(true))
                }
                #[cfg(target_os = "macos")]
                "precomposeunicode" => {
                    precompose_false = value.is_some_and(|v| config_bool_strict(v) != Some(true));
                }
                _ => {}
            }
        } else if section == "index" && key == "sparse" {
            index_sparse = value.is_none_or(|v| config_bool_strict(v).unwrap_or(true));
        }
        true
    });
    let _ = &mut precompose_false;
    parsed.is_err()
        || custom
        || bare
        || split_index
        || sparse_checkout
        || index_sparse
        || precompose_false
}

/// Object-id hash width (`20` for SHA-1, `32` for SHA-256), or `None` when
/// the repository config is unreadable or uses an unknown object format.
pub(crate) fn repo_hash_len(common_dir: &Path) -> Option<usize> {
    let mut hash_len = 20usize;
    let mut valid = true;
    let parsed = visit_git_config(&common_dir.join("config"), |section, key, value| {
        if section == "extensions" && key == "objectformat" {
            match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
                Some("sha256") => hash_len = 32,
                Some("sha1") => hash_len = 20,
                _ => {
                    valid = false;
                    return false;
                }
            }
        }
        true
    });
    (parsed.is_ok() && valid).then_some(hash_len)
}

/// Lightweight check that `git_dir/index` has no split (`link`), sparse
/// (`sdir` / `CE_SKIP_WORKTREE`), gitlink, non-UTF8, or corrupt layout without
/// allocating tracked path strings.
pub(crate) fn index_shape_supported(git_dir: &Path, common_dir: &Path) -> bool {
    let Some(hash_len) = repo_hash_len(common_dir) else {
        return false;
    };
    let index_path = git_dir.join("index");
    let (mut file, meta) = match open_regular_file_no_follow(&index_path) {
        Ok(pair) => pair,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return true,
        Err(_) => return false,
    };
    if meta.len() > MAX_INDEX_BYTES {
        return false;
    }
    let mut head = [0u8; INDEX_HEADER_LEN];
    if file.read_exact(&mut head).is_err() {
        return false;
    }
    let Ok(header) = super::parse_index_header(&head) else {
        return false;
    };
    if u64::from(header.entries).saturating_mul(MIN_INDEX_ENTRY_BYTES) > meta.len() {
        return false;
    }
    if file.rewind().is_err() {
        return false;
    }
    let walked = walk_entries(file, meta.len(), header, hash_len, |mode, raw| {
        if (mode & GITLINK_MODE_MASK) == GITLINK_MODE {
            return Err("submodule gitlink requires git".to_string());
        }
        if std::str::from_utf8(raw).is_err() {
            return Err("non-utf8 path in index requires git".to_string());
        }
        Ok(())
    });
    matches!(walked, Ok(shape) if !shape.split && !shape.sparse)
}

/// Reads unique tracked UTF-8 paths from `git_dir/index` when config and index
/// shape are standard; otherwise returns `Unsupported` (caller falls back to
/// `git ls-files`) or `OverLimit` if unique tracked paths exceed `max_unique`.
#[cfg(test)]
pub(crate) fn read_tracked_paths(
    git_dir: &Path,
    common_dir: &Path,
    max_unique: usize,
) -> TrackedIndexPaths {
    if repo_has_custom_ignore_or_unicode_config(git_dir, common_dir) {
        return TrackedIndexPaths::Unsupported;
    }
    let Some(hash_len) = repo_hash_len(common_dir) else {
        return TrackedIndexPaths::Unsupported;
    };
    let index_path = git_dir.join("index");
    let (mut file, meta) = match open_regular_file_no_follow(&index_path) {
        Ok(pair) => pair,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return TrackedIndexPaths::Paths(Vec::new());
        }
        Err(_) => return TrackedIndexPaths::Unsupported,
    };
    if meta.len() > MAX_INDEX_BYTES {
        return TrackedIndexPaths::Unsupported;
    }
    let mut head = [0u8; INDEX_HEADER_LEN];
    if file.read_exact(&mut head).is_err() {
        return TrackedIndexPaths::Unsupported;
    }
    let Ok(header) = super::parse_index_header(&head) else {
        return TrackedIndexPaths::Unsupported;
    };
    if u64::from(header.entries).saturating_mul(MIN_INDEX_ENTRY_BYTES) > meta.len() {
        return TrackedIndexPaths::Unsupported;
    }
    if file.rewind().is_err() {
        return TrackedIndexPaths::Unsupported;
    }

    let mut unique: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut over_limit = false;
    let walked = walk_entries(file, meta.len(), header, hash_len, |mode, raw| {
        if (mode & GITLINK_MODE_MASK) == GITLINK_MODE {
            return Err("submodule gitlink requires git".to_string());
        }
        let path = std::str::from_utf8(raw)
            .map_err(|_| "non-utf8 path in index requires git".to_string())?;
        if !unique.contains(path) {
            if unique.len() >= max_unique {
                over_limit = true;
                return Err("unique tracked path limit exceeded".to_string());
            }
            unique.insert(path.to_string());
        }
        Ok(())
    });
    if over_limit {
        return TrackedIndexPaths::OverLimit;
    }
    match walked {
        Ok(shape) if !shape.split && !shape.sparse => {
            TrackedIndexPaths::Paths(unique.into_iter().collect())
        }
        _ => TrackedIndexPaths::Unsupported,
    }
}

fn strip_physical_comment<'a>(line: &'a str, in_quotes: &mut bool) -> &'a str {
    let mut escaped = false;
    for (idx, ch) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if *in_quotes {
            match ch {
                '\\' => escaped = true,
                '"' => *in_quotes = false,
                _ => {}
            }
        } else {
            match ch {
                '"' => *in_quotes = true,
                '#' | ';' => return &line[..idx],
                _ => {}
            }
        }
    }
    line
}

fn find_section_close(after_open: &str) -> Option<usize> {
    let mut in_quotes = false;
    let mut escaped = false;
    for (idx, ch) in after_open.char_indices() {
        if in_quotes {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_quotes = false;
            }
        } else if ch == '"' {
            in_quotes = true;
        } else if ch == ']' {
            return Some(idx);
        }
    }
    None
}

fn ends_with_unescaped_backslash(s: &str) -> bool {
    let backslashes = s.bytes().rev().take_while(|&b| b == b'\\').count();
    backslashes % 2 == 1
}

fn unquote_config_value(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Some(String::new());
    }
    let mut out = String::with_capacity(raw.len());
    let mut in_quotes = false;
    let mut escaped = false;
    for ch in raw.chars() {
        if escaped {
            escaped = false;
            out.push(match ch {
                'n' => '\n',
                't' => '\t',
                'b' => '\x08',
                other => other,
            });
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => in_quotes = !in_quotes,
            _ => out.push(ch),
        }
    }
    if in_quotes || escaped {
        return None;
    }
    Some(out)
}

fn visit_git_config(
    path: &Path,
    mut visit: impl FnMut(&str, &str, Option<&str>) -> bool,
) -> Result<(), ()> {
    let (file, meta) = match open_regular_file_no_follow(path) {
        Ok(pair) => pair,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(()),
    };
    if meta.len() > MAX_CONFIG_BYTES {
        return Err(());
    }
    let mut raw = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|_| ())?;
    if raw.len() as u64 > MAX_CONFIG_BYTES {
        return Err(());
    }
    let text = String::from_utf8(raw).map_err(|_| ())?;

    let mut section = String::new();
    let mut lines = text.lines().peekable();
    while let Some(first) = lines.next() {
        let mut in_quotes = false;
        let first_stripped = strip_physical_comment(first, &mut in_quotes);
        let mut combined = first_stripped.to_string();
        while ends_with_unescaped_backslash(&combined) {
            combined.pop();
            let Some(next) = lines.next() else {
                break;
            };
            let next_stripped = strip_physical_comment(next, &mut in_quotes);
            combined.push_str(next_stripped);
        }
        let mut rest = combined.trim();
        while !rest.is_empty() {
            if let Some(after_open) = rest.strip_prefix('[') {
                let Some(close_idx) = find_section_close(after_open) else {
                    return Err(());
                };
                let header = after_open[..close_idx].trim();
                let name_end = header
                    .find(|c: char| c.is_ascii_whitespace() || c == '"')
                    .unwrap_or(header.len());
                let raw_section = header[..name_end].to_ascii_lowercase();
                if raw_section.is_empty() {
                    return Err(());
                }
                let has_subsection = !header[name_end..].trim().is_empty();
                section =
                    if has_subsection && !matches!(raw_section.as_str(), "include" | "includeif") {
                        format!("{raw_section}\0sub")
                    } else {
                        raw_section
                    };
                if !visit(&section, "", None) {
                    return Ok(());
                }
                rest = after_open[close_idx + 1..].trim();
                continue;
            }
            let (key, value) = match rest.split_once('=') {
                Some((k, v)) => {
                    let parsed = unquote_config_value(v).ok_or(())?;
                    (k.trim().to_ascii_lowercase(), Some(parsed))
                }
                None => (rest.trim().to_ascii_lowercase(), None),
            };
            if !key.is_empty() && !visit(&section, &key, value.as_deref()) {
                return Ok(());
            }
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;

    #[test]
    fn custom_config_fails_closed_on_invalid_bools_worktree_and_broken_symlinks_but_ignores_subsections(
    ) {
        let dir = TempDir::new().unwrap();
        let git_dir = dir.path();

        // Subsection-only entries do not trigger [core] / [index] unsupported flags.
        std::fs::write(
            git_dir.join("config"),
            "[core \"sub\"]\n\tbare = true\n\tprecomposeunicode = false\n[core.other]\n\tworktree = ../wt\n[index \"sub\"]\n\tsparse = true\n",
        )
        .unwrap();
        assert!(!repo_has_custom_ignore_or_unicode_config(git_dir, git_dir));

        // Invalid boolean on core.bare fails closed.
        std::fs::write(git_dir.join("config"), "[core]\n\tbare = garbage\n").unwrap();
        assert!(repo_has_custom_ignore_or_unicode_config(git_dir, git_dir));

        // Invalid boolean on core.precomposeunicode fails closed on macOS.
        #[cfg(target_os = "macos")]
        {
            std::fs::write(
                git_dir.join("config"),
                "[core]\n\tprecomposeunicode = garbage\n",
            )
            .unwrap();
            assert!(repo_has_custom_ignore_or_unicode_config(git_dir, git_dir));
        }

        // core.worktree fails closed.
        std::fs::write(git_dir.join("config"), "[core]\n\tworktree = ../other\n").unwrap();
        assert!(repo_has_custom_ignore_or_unicode_config(git_dir, git_dir));

        // Reset valid config, then test broken symlink config.worktree.
        #[cfg(unix)]
        {
            std::fs::write(git_dir.join("config"), "[core]\n\tbare = false\n").unwrap();
            assert!(!repo_has_custom_ignore_or_unicode_config(git_dir, git_dir));
            std::os::unix::fs::symlink(
                "/nonexistent/missing-worktree-cfg",
                git_dir.join("config.worktree"),
            )
            .unwrap();
            assert!(repo_has_custom_ignore_or_unicode_config(git_dir, git_dir));
        }
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let cmd =
            crate::engine::process::build_capture_command("git", args, Some(dir), &[], None, None);
        let run = crate::engine::process::run_bounded_capped(
            cmd,
            "git test helper",
            Duration::from_secs(10),
            None,
            1024 * 1024,
        )
        .expect("spawn git");
        assert!(
            run.success,
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&run.stderr)
        );
        if args.first() == Some(&"init") {
            crate::test_support::trust_repo(dir);
        }
    }

    #[test]
    fn read_tracked_paths_handles_v4_prefix_compression_and_caps() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        run_git(root, &["init", "-b", "main"]);
        run_git(root, &["config", "user.name", "Test"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "index.version", "4"]);

        fs::create_dir_all(root.join("a/dir")).unwrap();
        fs::write(root.join("a/alpha.rs"), "1\n").unwrap();
        fs::write(root.join("a/beta.rs"), "2\n").unwrap();
        fs::write(root.join("a/dir/gamma.rs"), "3\n").unwrap();
        run_git(root, &["add", "a/alpha.rs", "a/beta.rs", "a/dir/gamma.rs"]);

        let git_dir = root.join(".git");
        let TrackedIndexPaths::Paths(mut paths) = read_tracked_paths(&git_dir, &git_dir, 10) else {
            panic!("expected Paths from v4 index");
        };
        paths.sort();
        assert_eq!(paths, vec!["a/alpha.rs", "a/beta.rs", "a/dir/gamma.rs"]);

        assert_eq!(
            read_tracked_paths(&git_dir, &git_dir, 2),
            TrackedIndexPaths::OverLimit
        );
    }

    #[test]
    fn custom_ignore_unicode_and_hash_len_checks_local_config() {
        let dir = TempDir::new().unwrap();
        let git_dir = dir.path().join(".git");
        fs::create_dir_all(git_dir.join("info")).unwrap();
        fs::write(
            git_dir.join("config"),
            "[core]\n\trepositoryformatversion = 0\n\tbare = false\n\tprecomposeunicode = true\n[remote \"origin\"]\n\turl = https://example.com/repo.git\n",
        )
        .unwrap();
        assert!(!repo_has_custom_ignore_or_unicode_config(
            &git_dir, &git_dir
        ));
        assert_eq!(repo_hash_len(&git_dir), Some(20));

        fs::write(
            git_dir.join("config"),
            "[core]\n\texcludesfile = .myignore\n",
        )
        .unwrap();
        assert!(repo_has_custom_ignore_or_unicode_config(&git_dir, &git_dir));

        fs::write(
            git_dir.join("config"),
            "[includeIf \"gitdir:foo/\"]\n\tpath = other\n",
        )
        .unwrap();
        assert!(repo_has_custom_ignore_or_unicode_config(&git_dir, &git_dir));

        fs::write(
            git_dir.join("config"),
            "[extensions]\n\tobjectformat = sha256\n",
        )
        .unwrap();
        assert!(!repo_has_custom_ignore_or_unicode_config(
            &git_dir, &git_dir
        ));
        assert_eq!(repo_hash_len(&git_dir), Some(32));

        #[cfg(target_os = "macos")]
        {
            fs::write(
                git_dir.join("config"),
                "[core]\n\tprecomposeunicode = false\n",
            )
            .unwrap();
            assert!(repo_has_custom_ignore_or_unicode_config(&git_dir, &git_dir));
        }
    }

    #[test]
    fn visit_git_config_strips_comments_before_continuation_handles_quoted_brackets_and_new_gates()
    {
        let dir = TempDir::new().unwrap();
        let git_dir = dir.path().join(".git");
        fs::create_dir_all(&git_dir).unwrap();

        // `# comment \` must NOT swallow the following `[include]` section!
        fs::write(
            git_dir.join("config"),
            "[branch \"fix]1\"]\n\tremote = origin # trailing comment \\\n[include]\n\tpath = extra.cfg\n",
        )
        .unwrap();
        assert!(repo_has_custom_ignore_or_unicode_config(&git_dir, &git_dir));

        // `; comment \` before `[core]\n\tbare = true` must NOT swallow `[core]`.
        fs::write(
            git_dir.join("config"),
            "[branch \"x]y\"]\n\tmerge = refs/heads/main ; comment \\\n[core]\n\tbare = true\n",
        )
        .unwrap();
        assert!(repo_has_custom_ignore_or_unicode_config(&git_dir, &git_dir));

        // Explicit `core.splitindex = true` and `index.sparse = true` must fall back.
        fs::write(git_dir.join("config"), "[core]\n\tsplitindex = true\n").unwrap();
        assert!(repo_has_custom_ignore_or_unicode_config(&git_dir, &git_dir));

        fs::write(git_dir.join("config"), "[index]\n\tsparse = true\n").unwrap();
        assert!(repo_has_custom_ignore_or_unicode_config(&git_dir, &git_dir));

        // Quoted `]` in subsection name does not reject valid non-custom config.
        fs::write(
            git_dir.join("config"),
            "[branch \"fix]1\"]\n\tremote = origin\n[core]\n\tbare = false\n\tsplitindex = false\n[index]\n\tsparse = false\n",
        )
        .unwrap();
        assert!(!repo_has_custom_ignore_or_unicode_config(
            &git_dir, &git_dir
        ));
    }

    #[cfg(unix)]
    #[test]
    fn open_regular_file_no_follow_rejects_symlinks_and_fifos_without_blocking() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let git_dir = dir.path().join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        let outside = dir.path().join("outside");
        fs::write(&outside, "[core]\n\tbare = false\n").unwrap();

        // Symlinked config and index must fail closed (never followed).
        symlink(&outside, git_dir.join("config")).unwrap();
        assert!(repo_has_custom_ignore_or_unicode_config(&git_dir, &git_dir));

        let valid_index = dir.path().join("real_index");
        fs::write(&valid_index, b"DIRC\0\0\0\x02\0\0\0\0").unwrap();
        symlink(&valid_index, git_dir.join("index")).unwrap();
        fs::write(git_dir.join("config"), "[core]\n\tbare = false\n").unwrap();
        assert_eq!(
            read_tracked_paths(&git_dir, &git_dir, 100),
            TrackedIndexPaths::Unsupported
        );

        // FIFO planted at `.git/index` must fail immediately via O_NONBLOCK without hanging.
        fs::remove_file(git_dir.join("index")).unwrap();
        let fifo = git_dir.join("index");
        let cmd = crate::engine::process::build_capture_command(
            "mkfifo",
            &[fifo.to_str().unwrap()],
            None,
            &[],
            None,
            None,
        );
        let run = crate::engine::process::run_bounded_capped(
            cmd,
            "mkfifo test",
            Duration::from_secs(5),
            None,
            4096,
        )
        .expect("run mkfifo");
        assert!(run.success, "mkfifo failed");
        assert_eq!(
            read_tracked_paths(&git_dir, &git_dir, 100),
            TrackedIndexPaths::Unsupported
        );
    }

    #[test]
    fn read_tracked_paths_dedups_conflict_stages_without_overcounting() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        run_git(root, &["init", "-b", "main"]);
        run_git(root, &["config", "user.name", "Test"]);
        run_git(root, &["config", "user.email", "test@example.com"]);

        fs::write(root.join("conflict.txt"), "base\n").unwrap();
        run_git(root, &["add", "conflict.txt"]);
        run_git(root, &["commit", "-m", "base"]);

        run_git(root, &["checkout", "-b", "side"]);
        fs::write(root.join("conflict.txt"), "side\n").unwrap();
        run_git(root, &["commit", "-am", "side"]);

        run_git(root, &["checkout", "main"]);
        fs::write(root.join("conflict.txt"), "main\n").unwrap();
        run_git(root, &["commit", "-am", "main"]);

        // Produce stages 1, 2, 3 in `.git/index` for the same path.
        let cmd = crate::engine::process::build_capture_command(
            "git",
            &["merge", "side"],
            Some(root),
            &[],
            None,
            None,
        );
        let _ = crate::engine::process::run_bounded_capped(
            cmd,
            "git merge conflict",
            Duration::from_secs(10),
            None,
            64 * 1024,
        )
        .unwrap();

        let git_dir = root.join(".git");
        let TrackedIndexPaths::Paths(paths) = read_tracked_paths(&git_dir, &git_dir, 1) else {
            panic!("conflict stages for 1 unique file must fit under max_unique=1");
        };
        assert_eq!(paths, vec!["conflict.txt"]);
    }
}
