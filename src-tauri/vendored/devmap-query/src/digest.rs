//! SHA-1 and BLAKE2b, in-tree, because the freshness digests must be
//! byte-identical to the ones Python computes.
//!
//! `RepoMapper.map_is_stale` compares a map's `indexed_hash` (SHA-1 over the
//! sorted git file list) and `content_fingerprint` (SHA-1 over
//! `path\0blake2b-128-of-bytes` lines) against values it recomputes itself. As
//! long as any Python reader does that comparison, a kernel that stamps the
//! artifacts has to produce the same two strings — not an equivalent digest, the
//! same one — or every map it writes reads permanently stale.
//!
//! No hashing crate is linked in this workspace and none may be added, so both
//! algorithms live here. Both are pinned to published vectors: FIPS 180-4 for
//! SHA-1, RFC 7693 Appendix A for BLAKE2b-512, and CPython `hashlib` output for
//! the BLAKE2b-128 shape Python actually calls (`blake2b(digest_size=16)`),
//! which is the implementation this code has to agree with.

/// SHA-1 of `bytes`, lowercase hex — `hashlib.sha1(...).hexdigest()`.
pub fn sha1_hex(bytes: &[u8]) -> String {
    let mut state = Sha1::new();
    state.update(bytes);
    hex(&state.finish())
}

/// BLAKE2b-128 of `bytes`, lowercase hex —
/// `hashlib.blake2b(..., digest_size=16).hexdigest()`.
pub fn blake2b128_hex(bytes: &[u8]) -> String {
    let mut state = Blake2b::new(16);
    state.update(bytes);
    hex(&state.finish())
}

/// Lowercase hex, the rendering both `hexdigest()` callers expect.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

// ---- SHA-1 (FIPS 180-4 §6.1) -------------------------------------------------

/// Streaming SHA-1. Streaming rather than one-shot because
/// `content_fingerprint` hashes whole files, and a repository holds files
/// larger than it is reasonable to bring into memory to digest.
pub struct Sha1 {
    h: [u32; 5],
    buffer: [u8; 64],
    buffered: usize,
    length_bits: u64,
}

impl Default for Sha1 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha1 {
    pub fn new() -> Self {
        Self {
            h: [
                0x6745_2301,
                0xefcd_ab89,
                0x98ba_dcfe,
                0x1032_5476,
                0xc3d2_e1f0,
            ],
            buffer: [0u8; 64],
            buffered: 0,
            length_bits: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.length_bits = self.length_bits.wrapping_add((data.len() as u64) * 8);
        if self.buffered > 0 {
            let want = 64 - self.buffered;
            let take = want.min(data.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&data[..take]);
            self.buffered += take;
            data = &data[take..];
            if self.buffered < 64 {
                // The buffer is still short of a block. Falling through here
                // would run the remainder path below over an empty `data` and
                // reset `buffered` to 0, silently dropping everything just
                // copied in — the failure a chunked-by-1 stream shows and a
                // one-shot call never can.
                return;
            }
            let block = self.buffer;
            self.compress(&block);
            self.buffered = 0;
        }
        let (blocks, rest) = data.as_chunks::<64>();
        for block in blocks {
            self.compress(block);
        }
        self.buffer[..rest.len()].copy_from_slice(rest);
        self.buffered = rest.len();
    }

    pub fn finish(mut self) -> [u8; 20] {
        let length_bits = self.length_bits;
        // 0x80, then zeroes, then the 64-bit big-endian bit length.
        self.update_no_count(&[0x80]);
        while self.buffered != 56 {
            self.update_no_count(&[0x00]);
        }
        self.update_no_count(&length_bits.to_be_bytes());
        debug_assert_eq!(self.buffered, 0);

        let mut out = [0u8; 20];
        for (index, word) in self.h.iter().enumerate() {
            out[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    /// Padding bytes are not message bytes: the length field they encode was
    /// fixed before padding began.
    fn update_no_count(&mut self, data: &[u8]) {
        for byte in data {
            self.buffer[self.buffered] = *byte;
            self.buffered += 1;
            if self.buffered == 64 {
                let block = self.buffer;
                self.compress(&block);
                self.buffered = 0;
            }
        }
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 80];
        for (index, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([
                block[index * 4],
                block[index * 4 + 1],
                block[index * 4 + 2],
                block[index * 4 + 3],
            ]);
        }
        for index in 16..80 {
            w[index] = (w[index - 3] ^ w[index - 8] ^ w[index - 14] ^ w[index - 16]).rotate_left(1);
        }

        let [mut a, mut b, mut c, mut d, mut e] = self.h;
        for (index, word) in w.iter().enumerate() {
            let (f, k) = match index {
                0..=19 => ((b & c) | ((!b) & d), 0x5a82_7999u32),
                20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        self.h[0] = self.h[0].wrapping_add(a);
        self.h[1] = self.h[1].wrapping_add(b);
        self.h[2] = self.h[2].wrapping_add(c);
        self.h[3] = self.h[3].wrapping_add(d);
        self.h[4] = self.h[4].wrapping_add(e);
    }
}

// ---- BLAKE2b (RFC 7693 §3.1-3.3) --------------------------------------------

const BLAKE2B_IV: [u64; 8] = [
    0x6a09_e667_f3bc_c908,
    0xbb67_ae85_84ca_a73b,
    0x3c6e_f372_fe94_f82b,
    0xa54f_f53a_5f1d_36f1,
    0x510e_527f_ade6_82d1,
    0x9b05_688c_2b3e_6c1f,
    0x1f83_d9ab_fb41_bd6b,
    0x5be0_cd19_137e_2179,
];

const BLAKE2B_SIGMA: [[usize; 16]; 12] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
];

/// Streaming unkeyed BLAKE2b with a caller-chosen digest length.
///
/// A block is compressed only once the *next* block has arrived, because the
/// final block is compressed with the finalization flag set and BLAKE2b gives
/// no way to take that back.
pub struct Blake2b {
    h: [u64; 8],
    buffer: [u8; 128],
    buffered: usize,
    counter: u128,
    digest_len: usize,
}

impl Blake2b {
    /// `digest_len` is in bytes, 1..=64 (`hashlib.blake2b(digest_size=...)`).
    pub fn new(digest_len: usize) -> Self {
        assert!(
            (1..=64).contains(&digest_len),
            "BLAKE2b digest length must be 1..=64 bytes, got {digest_len}"
        );
        let mut h = BLAKE2B_IV;
        // Parameter block, unkeyed: depth 1, fanout 1, no salt, no personal.
        h[0] ^= 0x0101_0000 ^ (digest_len as u64);
        Self {
            h,
            buffer: [0u8; 128],
            buffered: 0,
            counter: 0,
            digest_len,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            if self.buffered == 128 {
                let block = self.buffer;
                self.counter += 128;
                let counter = self.counter;
                self.compress(&block, counter, false);
                self.buffered = 0;
            }
            let take = (128 - self.buffered).min(data.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&data[..take]);
            self.buffered += take;
            data = &data[take..];
        }
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.counter += self.buffered as u128;
        for byte in self.buffer.iter_mut().skip(self.buffered) {
            *byte = 0;
        }
        let block = self.buffer;
        let counter = self.counter;
        self.compress(&block, counter, true);

        let mut out = Vec::with_capacity(self.digest_len);
        for word in self.h.iter() {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out.truncate(self.digest_len);
        out
    }

    fn compress(&mut self, block: &[u8; 128], counter: u128, last: bool) {
        let mut m = [0u64; 16];
        for (index, word) in m.iter_mut().enumerate() {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&block[index * 8..index * 8 + 8]);
            *word = u64::from_le_bytes(bytes);
        }

        let mut v = [0u64; 16];
        v[..8].copy_from_slice(&self.h);
        v[8..].copy_from_slice(&BLAKE2B_IV);
        v[12] ^= counter as u64;
        v[13] ^= (counter >> 64) as u64;
        if last {
            v[14] = !v[14];
        }

        for round in BLAKE2B_SIGMA.iter() {
            mix(&mut v, 0, 4, 8, 12, m[round[0]], m[round[1]]);
            mix(&mut v, 1, 5, 9, 13, m[round[2]], m[round[3]]);
            mix(&mut v, 2, 6, 10, 14, m[round[4]], m[round[5]]);
            mix(&mut v, 3, 7, 11, 15, m[round[6]], m[round[7]]);
            mix(&mut v, 0, 5, 10, 15, m[round[8]], m[round[9]]);
            mix(&mut v, 1, 6, 11, 12, m[round[10]], m[round[11]]);
            mix(&mut v, 2, 7, 8, 13, m[round[12]], m[round[13]]);
            mix(&mut v, 3, 4, 9, 14, m[round[14]], m[round[15]]);
        }

        for index in 0..8 {
            self.h[index] ^= v[index] ^ v[index + 8];
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn mix(v: &mut [u64; 16], a: usize, b: usize, c: usize, d: usize, x: u64, y: u64) {
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
    v[d] = (v[d] ^ v[a]).rotate_right(32);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(24);
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
    v[d] = (v[d] ^ v[a]).rotate_right(16);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(63);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FIPS 180-4 test vectors, plus the block-boundary cases padding gets
    /// wrong: 55 bytes (one padding byte fits), 56 (the length field forces a
    /// second block) and 64 (an exactly full block).
    #[test]
    fn sha1_matches_the_fips_180_vectors() {
        for (input, expected) in [
            (b"".to_vec(), "da39a3ee5e6b4b0d3255bfef95601890afd80709"),
            (b"abc".to_vec(), "a9993e364706816aba3e25717850c26c9cd0d89d"),
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq".to_vec(),
                "84983e441c3bd26ebaae4aa1f95129e5e54670f1",
            ),
            (
                b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu".to_vec(),
                "a49b2446a02c645bf419f995b67091253a04a259",
            ),
            (vec![b'a'; 1_000_000], "34aa973cd4c4daa4f61eeb2bdbad27316534016f"),
            (vec![b'x'; 55], "cef734ba81a024479e09eb5a75b6ddae62e6abf1"),
            (vec![b'x'; 56], "901305367c259952f4e7af8323f480d59f81335b"),
            (vec![b'x'; 64], "bb2fa3ee7afb9f54c6dfb5d021f14b1ffe40c163"),
        ] {
            assert_eq!(
                sha1_hex(&input),
                expected,
                "sha1 of a {}-byte input",
                input.len()
            );
        }
    }

    /// The same message hashed in arbitrary slices must give the same digest as
    /// hashing it whole — the streaming path is the one `content_fingerprint`
    /// uses on every file.
    #[test]
    fn sha1_streaming_equals_one_shot() {
        let message: Vec<u8> = (0..1000u32).map(|index| (index % 251) as u8).collect();
        let one_shot = sha1_hex(&message);
        for chunk in [1usize, 7, 63, 64, 65, 128, 999] {
            let mut state = Sha1::new();
            for piece in message.chunks(chunk) {
                state.update(piece);
            }
            assert_eq!(hex(&state.finish()), one_shot, "chunked by {chunk}");
        }
    }

    /// RFC 7693 Appendix A pins BLAKE2b-512("abc"); the 128-bit vectors come
    /// from CPython `hashlib.blake2b(..., digest_size=16)`, which is the
    /// implementation `content_fingerprint` has to agree with byte for byte.
    #[test]
    fn blake2b_matches_rfc_7693_and_cpython_hashlib() {
        let mut state = Blake2b::new(64);
        state.update(b"abc");
        assert_eq!(
            hex(&state.finish()),
            "ba80a53f981c4d0d6a2797b69f12f6e94c212f14685ac4b74b12bb6fdbffa2d1\
             7d87c5392aab792dc252d5de4533cc9518d38aa8dbf1925ab92386edd4009923",
            "RFC 7693 Appendix A"
        );

        for (input, expected) in [
            (b"".to_vec(), "cae66941d9efbd404e4d88758ea67670"),
            (b"abc".to_vec(), "cf4ab791c62b8d2b2109c90275287816"),
            // 128 bytes is exactly one block: the boundary where a naive
            // implementation compresses the final block twice, or sets the
            // finalization flag on a block that is not the last.
            (vec![b'x'; 128], "874acca82a22239ec64a3e70c3ae494e"),
            (vec![b'x'; 129], "b9ee96f69e8210c624fee55d71d5a54f"),
            (
                vec![b'y'; 1024 * 1024 + 1],
                "be1371f9957e50c38edce8402b9ae007",
            ),
            (
                (0..256u32).map(|b| b as u8).collect::<Vec<u8>>(),
                "c2472c0ac37a8dbdb25f05ada0d82643",
            ),
        ] {
            assert_eq!(
                blake2b128_hex(&input),
                expected,
                "blake2b-128 of a {}-byte input",
                input.len()
            );
        }
    }

    #[test]
    fn blake2b_streaming_equals_one_shot() {
        let message: Vec<u8> = (0..5000u32).map(|index| (index % 253) as u8).collect();
        let one_shot = blake2b128_hex(&message);
        for chunk in [1usize, 17, 127, 128, 129, 1024, 4999] {
            let mut state = Blake2b::new(16);
            for piece in message.chunks(chunk) {
                state.update(piece);
            }
            assert_eq!(hex(&state.finish()), one_shot, "chunked by {chunk}");
        }
    }
}
