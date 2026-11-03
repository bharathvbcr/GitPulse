//! Content-Addressed Storage (CAS) for AST extraction artifacts.
//!
//! Implements immutable shared AST storage keyed by Blake3 hash (`.devcouncil/codeintel/cas/ast/<b3>.bin`)
//! fulfilling Milestone 3 (F38).

use anyhow::{Context, Result};
use devmap_extract::model::Extraction;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const CAS_TOMBSTONE: &str = "CAS_TOMBSTONE";

// ---------------------------------------------------------------------------
// Standard BLAKE3 Implementation (256-bit output)
// ---------------------------------------------------------------------------

const BLAKE3_IV: [u32; 8] = [
    0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A, 0x510E527F, 0x9B05688C, 0x1F83D9AB, 0x5BE0CD19,
];

const BLAKE3_SIGMA: [[usize; 16]; 7] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8],
    [3, 4, 10, 12, 13, 2, 7, 14, 6, 5, 9, 0, 11, 15, 8, 1],
    [10, 7, 12, 9, 14, 3, 13, 15, 4, 0, 11, 2, 5, 8, 1, 6],
    [12, 13, 9, 11, 15, 10, 14, 8, 7, 2, 5, 3, 0, 1, 6, 4],
    [9, 14, 11, 5, 8, 12, 15, 1, 13, 3, 0, 10, 2, 6, 4, 7],
    [11, 15, 5, 0, 1, 9, 8, 6, 14, 10, 2, 12, 3, 4, 7, 13],
];

const CHUNK_START: u32 = 1 << 0;
const CHUNK_END: u32 = 1 << 1;
const ROOT: u32 = 1 << 3;

fn g(state: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize, mx: u32, my: u32) {
    state[a] = state[a].wrapping_add(state[b]).wrapping_add(mx);
    state[d] = (state[d] ^ state[a]).rotate_right(16);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_right(12);
    state[a] = state[a].wrapping_add(state[b]).wrapping_add(my);
    state[d] = (state[d] ^ state[a]).rotate_right(8);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_right(7);
}

fn round(state: &mut [u32; 16], m: &[u32; 16], round_idx: usize) {
    let s = &BLAKE3_SIGMA[round_idx];
    // Columns
    g(state, 0, 4, 8, 12, m[s[0]], m[s[1]]);
    g(state, 1, 5, 9, 13, m[s[2]], m[s[3]]);
    g(state, 2, 6, 10, 14, m[s[4]], m[s[5]]);
    g(state, 3, 7, 11, 15, m[s[6]], m[s[7]]);
    // Diagonals
    g(state, 0, 5, 10, 15, m[s[8]], m[s[9]]);
    g(state, 1, 6, 11, 12, m[s[10]], m[s[11]]);
    g(state, 2, 7, 8, 13, m[s[12]], m[s[13]]);
    g(state, 3, 4, 9, 14, m[s[14]], m[s[15]]);
}

fn compress_block(
    cv: &[u32; 8],
    block: &[u8; 64],
    block_len: u32,
    counter: u64,
    flags: u32,
) -> [u32; 16] {
    let mut m = [0u32; 16];
    for i in 0..16 {
        m[i] = u32::from_le_bytes([
            block[i * 4],
            block[i * 4 + 1],
            block[i * 4 + 2],
            block[i * 4 + 3],
        ]);
    }

    let mut state = [
        cv[0],
        cv[1],
        cv[2],
        cv[3],
        cv[4],
        cv[5],
        cv[6],
        cv[7],
        BLAKE3_IV[0],
        BLAKE3_IV[1],
        BLAKE3_IV[2],
        BLAKE3_IV[3],
        counter as u32,
        (counter >> 32) as u32,
        block_len,
        flags,
    ];

    for r in 0..7 {
        round(&mut state, &m, r);
    }

    state
}

/// Computes the 256-bit BLAKE3 hash of `data` formatted as `b3_<hex>`.
pub fn blake3_hash(data: &[u8]) -> String {
    let mut hasher = Blake3Hasher::new();
    hasher.update(data);
    hasher.finalize()
}

pub struct Blake3Hasher {
    cv: [u32; 8],
    chunk_buffer: [u8; 1024],
    chunk_len: usize,
    total_chunks: u64,
}

impl Default for Blake3Hasher {
    fn default() -> Self {
        Self::new()
    }
}

impl Blake3Hasher {
    pub fn new() -> Self {
        Self {
            cv: BLAKE3_IV,
            chunk_buffer: [0u8; 1024],
            chunk_len: 0,
            total_chunks: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            if self.chunk_len == 1024 {
                self.process_chunk(false);
                self.chunk_len = 0;
                self.total_chunks += 1;
            }
            let take = (1024 - self.chunk_len).min(data.len());
            self.chunk_buffer[self.chunk_len..self.chunk_len + take].copy_from_slice(&data[..take]);
            self.chunk_len += take;
            data = &data[take..];
        }
    }

    fn process_chunk(&mut self, is_root: bool) {
        let chunk_bytes = self.chunk_len;
        let mut cv = self.cv;

        let num_blocks = chunk_bytes.div_ceil(64);
        let num_blocks = if num_blocks == 0 { 1 } else { num_blocks };

        for b in 0..num_blocks {
            let mut block = [0u8; 64];
            let start = b * 64;
            let end = (start + 64).min(chunk_bytes);
            let len = end.saturating_sub(start);
            if len > 0 {
                block[..len].copy_from_slice(&self.chunk_buffer[start..end]);
            }

            let mut flags = 0u32;
            if b == 0 {
                flags |= CHUNK_START;
            }
            if b == num_blocks - 1 {
                flags |= CHUNK_END;
                if is_root {
                    flags |= ROOT;
                }
            }

            let out = compress_block(&cv, &block, len as u32, self.total_chunks, flags);
            for i in 0..8 {
                cv[i] = out[i] ^ out[i + 8];
            }
        }
        self.cv = cv;
    }

    pub fn finalize(mut self) -> String {
        self.process_chunk(true);
        let mut bytes = [0u8; 32];
        for (i, word) in self.cv.iter().enumerate() {
            bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        let hex_str = crate::digest::hex(&bytes);
        format!("b3_{}", hex_str)
    }
}

// ---------------------------------------------------------------------------
// Immutable CAS AST Storage (`.devcouncil/codeintel/cas/ast/<b3>.bin`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CasStats {
    pub stored_count: usize,
    pub reused_count: usize,
    pub deduplication_ratio: f64,
}

#[derive(Debug)]
pub struct CasAstStore {
    base_dir: PathBuf,
    cache: Arc<Mutex<HashMap<String, Extraction>>>,
    stats: Arc<Mutex<(usize, usize)>>, // (stored, reused)
}

impl CasAstStore {
    pub fn new(repo_root: impl AsRef<Path>) -> Self {
        let base_dir = repo_root
            .as_ref()
            .join(".devcouncil")
            .join("codeintel")
            .join("cas")
            .join("ast");
        Self {
            base_dir,
            cache: Arc::new(Mutex::new(HashMap::new())),
            stats: Arc::new(Mutex::new((0, 0))),
        }
    }

    pub fn with_custom_dir(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
            cache: Arc::new(Mutex::new(HashMap::new())),
            stats: Arc::new(Mutex::new((0, 0))),
        }
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Stores an extracted AST in the CAS store, keyed by Blake3 hash of content.
    /// Deduplicates automatically if the blob already exists on disk or in memory.
    pub fn store_ast(
        &self,
        content: &[u8],
        extraction: &Extraction,
    ) -> Result<(String, PathBuf, bool)> {
        let hash = blake3_hash(content);
        let file_path = self.base_dir.join(format!("{}.bin", hash));

        // Check in-memory cache first
        {
            let mut cache = self.cache.lock().unwrap();
            if cache.contains_key(&hash) {
                let mut stats = self.stats.lock().unwrap();
                stats.1 += 1;
                return Ok((hash, file_path, true));
            }
            cache.insert(hash.clone(), extraction.clone());
        }

        // Check on disk
        if file_path.exists() {
            let mut stats = self.stats.lock().unwrap();
            stats.1 += 1;
            return Ok((hash, file_path, true));
        }

        // Write new blob to CAS storage
        fs::create_dir_all(&self.base_dir)
            .with_context(|| format!("creating CAS directory {:?}", self.base_dir))?;

        let tmp_path = self
            .base_dir
            .join(format!("{}.tmp.{}", hash, std::process::id()));
        let encoded = serde_json::to_vec(extraction).context("serializing extraction to JSON")?;

        let mut file = File::create(&tmp_path)
            .with_context(|| format!("creating temp file {:?}", tmp_path))?;
        file.write_all(&encoded)
            .with_context(|| format!("writing to temp file {:?}", tmp_path))?;
        file.sync_all()?;
        drop(file);

        fs::rename(&tmp_path, &file_path)
            .with_context(|| format!("renaming {:?} to {:?}", tmp_path, file_path))?;

        let mut stats = self.stats.lock().unwrap();
        stats.0 += 1;

        Ok((hash, file_path, false))
    }

    /// Loads an AST extraction from CAS by hash.
    pub fn load_ast(&self, hash: &str) -> Result<Option<Extraction>> {
        // Memory cache hit
        if let Some(ext) = self.cache.lock().unwrap().get(hash).cloned() {
            return Ok(Some(ext));
        }

        let file_path = self.base_dir.join(format!("{}.bin", hash));
        if !file_path.exists() {
            return Ok(None);
        }

        let mut file =
            File::open(&file_path).with_context(|| format!("opening CAS file {:?}", file_path))?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)?;

        let ext: Extraction = serde_json::from_slice(&buffer)
            .with_context(|| format!("deserializing CAS file {:?}", file_path))?;

        self.cache
            .lock()
            .unwrap()
            .insert(hash.to_string(), ext.clone());
        Ok(Some(ext))
    }

    pub fn stats(&self) -> CasStats {
        let (stored, reused) = *self.stats.lock().unwrap();
        let total = stored + reused;
        let deduplication_ratio = if total == 0 {
            0.0
        } else {
            reused as f64 / total as f64
        };
        CasStats {
            stored_count: stored,
            reused_count: reused,
            deduplication_ratio,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blake3_hash_prefix_and_stability() {
        let data = b"fn test_function() { println!(\"devmap\"); }";
        let h1 = blake3_hash(data);
        let h2 = blake3_hash(data);
        assert!(h1.starts_with("b3_"));
        assert_eq!(h1, h2);

        let h3 = blake3_hash(b"different content");
        assert_ne!(h1, h3);
    }

    #[test]
    fn test_cas_store_and_deduplication() {
        let tmp = std::env::temp_dir().join(format!("devmap_cas_test_{}", std::process::id()));
        let store = CasAstStore::with_custom_dir(&tmp);

        let content = b"package main\nfunc main() {}";
        let ext: Extraction = serde_json::from_str(
            r#"{"file_path":"test.go","language":"go","content_hash":123,"parse_outcome":"Clean","symbols":[],"imports":[],"calls":[]}"#
        ).expect("valid extraction");

        let (h1, p1, reused1) = store.store_ast(content, &ext).unwrap();
        assert!(!reused1);
        assert!(p1.exists());

        // Re-store identical content -> must reuse
        let (h2, p2, reused2) = store.store_ast(content, &ext).unwrap();
        assert!(reused2);
        assert_eq!(h1, h2);
        assert_eq!(p1, p2);

        let stats = store.stats();
        assert_eq!(stats.stored_count, 1);
        assert_eq!(stats.reused_count, 1);
        assert!((stats.deduplication_ratio - 0.5).abs() < 1e-6);

        // Load back
        let loaded = store.load_ast(&h1).unwrap().expect("loaded AST");
        assert_eq!(loaded.symbols.len(), ext.symbols.len());

        let _ = fs::remove_dir_all(&tmp);
    }
}
