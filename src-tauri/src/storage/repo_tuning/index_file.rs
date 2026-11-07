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
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use super::IndexHeader;

/// An index larger than this is not walked (about 3.5 million entries).
pub const MAX_INDEX_BYTES: u64 = 256 * 1024 * 1024;
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

#[cfg(not(unix))]
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
    let answer = File::open(path)
        .map_err(|e| format!("the index could not be opened: {e}"))
        .and_then(|file| walk_file(file, meta.len(), header, hash_len));
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

    /// Reads bytes up to and including a NUL; returns how many came before it.
    fn until_nul(&mut self) -> Result<usize, String> {
        let mut n = 0usize;
        loop {
            if self.byte()? == 0 {
                return Ok(n);
            }
            n += 1;
            if n > MAX_PATH_BYTES {
                return Err("an entry path is implausibly long".to_string());
            }
        }
    }
}

fn walk_file(
    file: File,
    len: u64,
    header: IndexHeader,
    hash_len: usize,
) -> Result<IndexShape, String> {
    if len > MAX_INDEX_BYTES {
        return Err(format!(
            "the index is larger than the {} MiB read bound",
            MAX_INDEX_BYTES >> 20
        ));
    }
    let end = len
        .checked_sub(hash_len as u64)
        .filter(|end| *end >= 12)
        .ok_or_else(|| "the index is shorter than its header and checksum".to_string())?;
    let mut cur = Cursor {
        inner: BufReader::with_capacity(64 * 1024, file),
        pos: 0,
        end,
    };
    let mut head = [0u8; 12];
    cur.inner
        .read_exact(&mut head)
        .map_err(|e| format!("the index header could not be read: {e}"))?;
    cur.pos = 12;

    let fixed = 40 + hash_len + 2;
    let mut fields = vec![0u8; fixed];
    let mut prev_len: usize = 0;
    for _ in 0..header.entries {
        cur.take(&mut fields)?;
        let flags = u16::from_be_bytes([fields[fixed - 2], fields[fixed - 1]]);
        let mut fixed_total = fixed as u64;
        if flags & 0x4000 != 0 {
            if header.version < 3 {
                return Err("an entry carries extended flags in a version 2 index".to_string());
            }
            let mut extended = [0u8; 2];
            cur.take(&mut extended)?;
            fixed_total += 2;
        }
        let name_len = usize::from(flags & 0x0FFF);
        if header.version < 4 {
            let long = name_len >= 0x0FFF;
            let path_len = if !long {
                if name_len == 0 {
                    return Err("an entry has an empty path".to_string());
                }
                let mut name = vec![0u8; name_len];
                cur.take(&mut name)?;
                name_len
            } else {
                // The 12 bit length saturates; the path runs to its NUL, which
                // `until_nul` has consumed along with it.
                cur.until_nul()?
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
            if strip > prev_len {
                return Err("an entry strips more than the previous path".to_string());
            }
            let suffix = cur.until_nul()?;
            let path_len = prev_len - strip + suffix;
            if path_len == 0 {
                return Err("an entry has an empty path".to_string());
            }
            prev_len = path_len;
        }
    }

    let mut shape = IndexShape {
        split: false,
        sparse: false,
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
