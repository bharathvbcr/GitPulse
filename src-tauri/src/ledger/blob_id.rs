//! Git blob object ids computed in process.
//!
//! `git hash-object --stdin` of a byte string is the hash of
//! `"blob " + decimal length + NUL + content`: SHA-1 in a SHA-1 repository,
//! SHA-256 in a SHA-256 one. The family ledger names each sibling source by
//! that id of its path, and the agent hook's read-only lookup needs the name of
//! every sibling that holds rows on every tool call. Asking Git cost one child
//! per sibling against the shared per-user spawn budget, so the id is computed
//! here; the stored `ledger_family_imports.source_key` rows are unchanged.
//!
//! The SHA-1 below exists only to reproduce Git's name for a string. It is a
//! content key for a path, never a security primitive: nothing here
//! authenticates, signs or compares secrets, and a collision could do no more
//! than make two paths share an import row. The dependency tree carries no
//! SHA-1 crate, so it is written out; the tests pin it to the published vectors
//! and to a real `git hash-object --stdin` in both object formats.

use crate::storage::repo_tuning::index_file::repo_hash_len;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The two names one string has: its blob id in a SHA-1 repository and in a
/// SHA-256 repository. They differ in length (40 and 64 hex digits), so one
/// can never be mistaken for the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BlobIds {
    pub(super) sha1: String,
    pub(super) sha256: String,
}

impl BlobIds {
    pub(super) fn of(content: &[u8]) -> Self {
        let mut framed = Vec::with_capacity(content.len() + 24);
        framed.extend_from_slice(format!("blob {}", content.len()).as_bytes());
        framed.push(0);
        framed.extend_from_slice(content);
        Self {
            sha1: hex(&sha1(&framed)),
            sha256: hex(&Sha256::digest(&framed)),
        }
    }
}

/// The blob id `git hash-object --stdin` would print for `content` in the
/// repository checked out at `repo`, or `None` when this process cannot tell
/// which object format that repository uses.
///
/// The object format is read, not guessed: `extensions.objectFormat` from the
/// config of the repository's common directory, the one place Git itself reads
/// it (a linked worktree reaches it through its `commondir` file; no key means
/// SHA-1). The reader is [`repo_hash_len`], shared with the repository-tuning
/// scan. Anything short of a definite answer is `None`: no `.git`, a
/// `gitdir:` or `commondir` pointer that is unreadable, empty or points at
/// nothing, an unreadable config or an unknown format value. The caller then
/// asks Git, so a doubt falls toward the authority and never toward a key of
/// the wrong length.
pub(super) fn blob_id_in_repo(repo: &Path, content: &[u8]) -> Option<String> {
    let common = common_dir_of(repo)?;
    let ids = BlobIds::of(content);
    match repo_hash_len(&common)? {
        20 => Some(ids.sha1),
        32 => Some(ids.sha256),
        _ => None,
    }
}

/// The common directory of the checkout at `repo`: its `.git` directory, or
/// for a linked worktree (whose `.git` is a `gitdir:` file) the directory its
/// `commondir` file names.
fn common_dir_of(repo: &Path) -> Option<PathBuf> {
    let dot_git = repo.join(".git");
    let git_dir = if std::fs::symlink_metadata(&dot_git).ok()?.is_dir() {
        dot_git
    } else {
        let pointer = std::fs::read_to_string(&dot_git).ok()?;
        let target = pointer.trim().strip_prefix("gitdir:")?.trim();
        if target.is_empty() {
            return None;
        }
        repo.join(target)
    };
    if !git_dir.is_dir() {
        return None;
    }
    let pointer = git_dir.join("commondir");
    let common = match std::fs::read_to_string(&pointer) {
        Ok(text) => {
            let text = text.trim();
            if text.is_empty() {
                return None;
            }
            git_dir.join(text)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => git_dir,
        Err(_) => return None,
    };
    common.is_dir().then_some(common)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    out
}

/// SHA-1 (FIPS 180-4) of `message`.
fn sha1(message: &[u8]) -> [u8; 20] {
    let mut state: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let bit_length = (message.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity(message.len() + 72);
    padded.extend_from_slice(message);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_length.to_be_bytes());

    for block in padded.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            let at = i * 4;
            *word = u32::from_be_bytes([block[at], block[at + 1], block[at + 2], block[at + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = state;
        for (i, word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let next = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = next;
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e]) {
            *slot = slot.wrapping_add(value);
        }
    }

    let mut out = [0u8; 20];
    for (i, word) in state.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::git_cli::git_with_stdin;
    use crate::test_support::git_in;

    fn sha1_hex(message: &[u8]) -> String {
        hex(&sha1(message))
    }

    #[test]
    fn sha1_matches_the_published_vectors() {
        assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            sha1_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
        assert_eq!(
            sha1_hex(&vec![b'a'; 1_000_000]),
            "34aa973cd4c4daa4f61eeb2bdbad27316534016f"
        );
    }

    /// The padding boundaries: 55 bytes still fit one block with the length,
    /// 56 spill into a second, 63/64/65 straddle a block edge.
    #[test]
    fn sha1_agrees_with_the_system_sha1_at_every_padding_boundary() {
        // Reference digests of `a` repeated n times, from `shasum -a 1`.
        let reference = [
            (55, "c1c8bbdc22796e28c0e15163d20899b65621d65a"),
            (56, "c2db330f6083854c99d4b5bfb6e8f29f201be699"),
            (63, "03f09f5b158a7a8cdad920bddc29b81c18a551f5"),
            (64, "0098ba824b5c16427bd7a1122a5a442a25ec644d"),
            (65, "11655326c708d70319be2610e8a57d9a5b959d3b"),
        ];
        for (n, expected) in reference {
            assert_eq!(sha1_hex(&vec![b'a'; n]), expected, "{n} bytes");
        }
    }

    /// Git's own answer, in a repository of each object format, for the inputs
    /// that break a hand-written hash: empty, one block, the padding edges,
    /// several blocks, and bytes that are not ASCII.
    #[test]
    fn blob_ids_match_git_hash_object_in_both_object_formats() {
        let inputs: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"abc".to_vec(),
            "/Users/ünï/Code/wörktree-日本語".as_bytes().to_vec(),
            vec![b'p'; 40],
            vec![b'p'; 55],
            vec![b'p'; 56],
            vec![b'p'; 63],
            vec![b'p'; 64],
            vec![b'p'; 65],
            vec![b'x'; 1000],
            vec![0u8, 255, 0, 128, 7],
        ];
        for format in ["sha1", "sha256"] {
            let dir = tempfile::tempdir().expect("repository");
            git_in(
                dir.path(),
                &["init", "-b", "main", &format!("--object-format={format}")],
            );
            for input in &inputs {
                let git = git_with_stdin(dir.path(), &["hash-object", "--stdin"], input)
                    .expect("git hash-object");
                let git = String::from_utf8(git).expect("hex").trim().to_string();
                let ours = BlobIds::of(input);
                let mine = if format == "sha1" {
                    &ours.sha1
                } else {
                    &ours.sha256
                };
                assert_eq!(
                    mine,
                    &git,
                    "{format}: {} byte input differs from git",
                    input.len()
                );
            }
        }
    }
}
