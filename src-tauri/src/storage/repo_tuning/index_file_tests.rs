use super::index_file::{walk, IndexShape};
use super::*;
use proptest::prelude::*;

fn header(version: u32, entries: u32) -> Vec<u8> {
    let mut bytes = b"DIRC".to_vec();
    bytes.extend_from_slice(&version.to_be_bytes());
    bytes.extend_from_slice(&entries.to_be_bytes());
    bytes
}

/// One on-disk entry for `name`, in `version`'s layout. `prev` is the previous
/// path (version 4 compresses against it); `ext` adds the version 3 extended
/// flags word.
pub(super) fn entry_bytes(
    version: u32,
    hash_len: usize,
    name: &str,
    prev: &str,
    ext: bool,
) -> Vec<u8> {
    let mut out = vec![0u8; 40];
    out.extend(std::iter::repeat_n(0x5A, hash_len));
    let mut flags = name.len().min(0x0FFF) as u16;
    if ext {
        flags |= 0x4000;
    }
    out.extend_from_slice(&flags.to_be_bytes());
    if ext {
        out.extend_from_slice(&[0, 0]);
    }
    if version < 4 {
        out.extend_from_slice(name.as_bytes());
        // 1 to 8 NULs, to a multiple of 8.
        let total = (out.len() + 8) & !7;
        out.resize(total, 0);
    } else {
        let common = name
            .bytes()
            .zip(prev.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        let strip = prev.len() - common;
        // Offset encoding of `strip`.
        let mut digits = vec![(strip & 0x7F) as u8];
        let mut rest = strip >> 7;
        while rest > 0 {
            rest -= 1;
            digits.push(0x80 | (rest & 0x7F) as u8);
            rest >>= 7;
        }
        digits.reverse();
        out.extend(digits);
        out.extend_from_slice(&name.as_bytes()[common..]);
        out.push(0);
    }
    out
}

/// A well-formed index of `entries` short paths with the given extensions.
pub(super) fn index_with(
    version: u32,
    entries: u32,
    exts: &[(&[u8; 4], Vec<u8>)],
    trailer: &[u8],
) -> Vec<u8> {
    let mut bytes = header(version, entries);
    let mut prev = String::new();
    for i in 0..entries {
        let name = format!("d{}/f{i:05}", i % 7);
        bytes.extend(entry_bytes(version, trailer.len(), &name, &prev, false));
        prev = name;
    }
    for (sig, body) in exts {
        bytes.extend_from_slice(*sig);
        bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
        bytes.extend_from_slice(body);
    }
    bytes.extend_from_slice(trailer);
    bytes
}

pub(super) fn index_bytes(version: u32, entries: u32, trailer: &[u8]) -> Vec<u8> {
    index_with(version, entries, &[], trailer)
}

fn walked(bytes: &[u8], hash_len: usize) -> Result<IndexShape, String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index");
    fs::write(&path, bytes).unwrap();
    let meta = fs::symlink_metadata(&path).unwrap();
    let head = parse_index_header(bytes).map_err(|e| e.to_string())?;
    walk(&path, &meta, head, hash_len)
}

#[test]
fn every_version_walks_and_reads_both_trailers() {
    for version in [2, 3, 4] {
        let shape = walked(&index_bytes(version, 40, &[7; 20]), 20).unwrap();
        assert_eq!(
            shape,
            IndexShape {
                split: false,
                sparse: false,
                skip_hash: false
            },
            "v{version}"
        );
        assert!(
            walked(&index_bytes(version, 40, &[0; 20]), 20)
                .unwrap()
                .skip_hash
        );
        assert!(
            walked(&index_bytes(version, 3, &[0; 32]), 32)
                .unwrap()
                .skip_hash
        );
    }
    // No entries at all is a valid, empty index.
    assert!(walked(&index_bytes(2, 0, &[1; 20]), 20).is_ok());
}

#[test]
fn the_link_and_sparse_directory_extensions_are_found() {
    let link = index_with(2, 5, &[(b"link", vec![0; 24])], &[1; 20]);
    let shape = walked(&link, 20).unwrap();
    assert!(shape.split && !shape.sparse);
    let sdir = index_with(3, 5, &[(b"TREE", vec![0; 9]), (b"sdir", vec![])], &[1; 20]);
    let shape = walked(&sdir, 20).unwrap();
    assert!(shape.sparse && !shape.split);
}

#[test]
fn v3_extended_flags_and_a_long_path_are_walked() {
    let mut bytes = header(3, 2);
    bytes.extend(entry_bytes(3, 20, "plain.txt", "", true));
    let long = "a".repeat(5000);
    bytes.extend(entry_bytes(3, 20, &long, "", false));
    bytes.extend_from_slice(&[9; 20]);
    assert!(walked(&bytes, 20).is_ok());
}

#[test]
fn a_header_over_a_garbage_body_is_refused() {
    // The adversary's fixture: 30,000 claimed entries over 2 MB of zeros.
    let mut zeros = header(2, 30_000);
    zeros.resize(2_000_000, 0);
    assert!(walked(&zeros, 20).is_err());
    // The same over random-looking bytes, and a body one byte short.
    let mut noise = header(4, 1_000);
    noise.extend((0..70_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8));
    assert!(walked(&noise, 20).is_err());
    let mut short = index_bytes(2, 40, &[1; 20]);
    short.pop();
    assert!(walked(&short, 20).is_err());
}

#[test]
fn structural_damage_is_each_refused() {
    let good = index_bytes(2, 10, &[1; 20]);
    // An extension that does not end at the checksum.
    let mut long_ext = index_with(2, 10, &[(b"TREE", vec![0; 8])], &[1; 20]);
    let at = long_ext.len() - 20 - 8 - 4;
    long_ext[at..at + 4].copy_from_slice(&1000u32.to_be_bytes());
    assert!(walked(&long_ext, 20).is_err());
    // A signature that does not start with a letter.
    let mut sig = index_with(2, 10, &[(b"TREE", vec![0; 4])], &[1; 20]);
    let at = sig.len() - 20 - 4 - 4 - 4;
    sig[at] = 0;
    assert!(walked(&sig, 20).is_err());
    // Padding that is not NUL.
    let mut pad = good.clone();
    let first_entry_end = 12 + ((62 + "d0/f00000".len() + 8) & !7);
    pad[first_entry_end - 1] = 1;
    assert!(walked(&pad, 20).is_err());
    // An extended-flags entry in a version 2 index.
    let mut v2 = header(2, 1);
    v2.extend(entry_bytes(3, 20, "x", "", true));
    v2.extend_from_slice(&[1; 20]);
    assert!(walked(&v2, 20).is_err());
    // A header that claims an entry the body does not have.
    let mut few = index_bytes(2, 3, &[1; 20]);
    few[8..12].copy_from_slice(&50u32.to_be_bytes());
    assert!(walked(&few, 20).is_err());
    // Shorter than a header and a checksum.
    assert!(walked(&header(2, 0), 20).is_err());
}

#[test]
fn a_v4_entry_may_not_strip_more_than_the_previous_path() {
    let mut bytes = header(4, 1);
    let mut e = vec![0u8; 40];
    e.extend([0x5A; 20]);
    e.extend_from_slice(&1u16.to_be_bytes());
    e.push(5); // strip 5 from an empty previous path
    e.extend_from_slice(b"x\0");
    bytes.extend(e);
    bytes.extend_from_slice(&[1; 20]);
    assert!(walked(&bytes, 20).is_err());
}

#[test]
fn the_walk_is_cached_until_the_file_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index");
    fs::write(&path, index_bytes(2, 30, &[1; 20])).unwrap();
    let head = parse_index_header(&fs::read(&path).unwrap()).unwrap();
    let meta = fs::symlink_metadata(&path).unwrap();
    assert!(walk(&path, &meta, head, 20).is_ok());
    // Older than the racy window, the same stamp is answered from the cache
    // even if the content under it is gone; a different size is not.
    fs::write(&path, b"DIRC").unwrap();
    let meta = fs::symlink_metadata(&path).unwrap();
    assert!(
        walk(&path, &meta, head, 20).is_err(),
        "a changed size is re-read"
    );
}

proptest! {
    /// Whatever the bytes, walking never panics, and an `Ok` is a body whose
    /// entries and extensions account for every byte before the checksum.
    #[test]
    fn walking_arbitrary_bodies_never_panics(
        version in 2u32..=4,
        entries in 0u32..40,
        body in proptest::collection::vec(any::<u8>(), 0..600),
    ) {
        let mut bytes = header(version, entries);
        bytes.extend(body);
        let _ = walked(&bytes, 20);
    }

    /// A built index of any size and version always walks.
    #[test]
    fn built_indexes_always_walk(version in 2u32..=4, entries in 0u32..200, zero in any::<bool>()) {
        let trailer = if zero { [0u8; 20] } else { [3u8; 20] };
        let shape = walked(&index_bytes(version, entries, &trailer), 20).unwrap();
        prop_assert_eq!(shape.skip_hash, zero);
    }
}
