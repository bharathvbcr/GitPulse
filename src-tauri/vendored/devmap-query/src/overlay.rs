//! Ephemeral Worktree Delta Overlays.
//!
//! Provides support for ephemeral, dirty working tree state overlaid on top of immutable base
//! graph and CAS AST storage. Implements `CAS_TOMBSTONE` marking and Roaring Bitmap chunked
//! bitsets for fast dirty node invalidation fulfilling Milestone 3 (F38).

use devmap_extract::model::Extraction;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub const CAS_TOMBSTONE: &str = "CAS_TOMBSTONE";

// ---------------------------------------------------------------------------
// Roaring Bitmap Implementation (No external dependency)
// ---------------------------------------------------------------------------

const ARRAY_LIMIT: usize = 4096;
const BITSET_WORDS: usize = 1024; // 1024 * 64 = 65536 bits

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum Container {
    Array(Vec<u16>),
    Bitset(Vec<u64>),
}

impl Container {
    fn new_array() -> Self {
        Container::Array(Vec::new())
    }

    fn insert(&mut self, val: u16) -> bool {
        match self {
            Container::Array(arr) => match arr.binary_search(&val) {
                Ok(_) => false,
                Err(idx) => {
                    arr.insert(idx, val);
                    if arr.len() > ARRAY_LIMIT {
                        let mut words = vec![0u64; BITSET_WORDS];
                        for &v in arr.iter() {
                            let w = (v as usize) / 64;
                            let b = (v as usize) % 64;
                            words[w] |= 1u64 << b;
                        }
                        *self = Container::Bitset(words);
                    }
                    true
                }
            },
            Container::Bitset(words) => {
                let w = (val as usize) / 64;
                let b = (val as usize) % 64;
                let mask = 1u64 << b;
                if words[w] & mask == 0 {
                    words[w] |= mask;
                    true
                } else {
                    false
                }
            }
        }
    }

    fn remove(&mut self, val: u16) -> bool {
        match self {
            Container::Array(arr) => {
                if let Ok(idx) = arr.binary_search(&val) {
                    arr.remove(idx);
                    true
                } else {
                    false
                }
            }
            Container::Bitset(words) => {
                let w = (val as usize) / 64;
                let b = (val as usize) % 64;
                let mask = 1u64 << b;
                if words[w] & mask != 0 {
                    words[w] &= !mask;
                    true
                } else {
                    false
                }
            }
        }
    }

    fn contains(&self, val: u16) -> bool {
        match self {
            Container::Array(arr) => arr.binary_search(&val).is_ok(),
            Container::Bitset(words) => {
                let w = (val as usize) / 64;
                let b = (val as usize) % 64;
                (words[w] & (1u64 << b)) != 0
            }
        }
    }

    fn cardinality(&self) -> usize {
        match self {
            Container::Array(arr) => arr.len(),
            Container::Bitset(words) => words.iter().map(|w| w.count_ones() as usize).sum(),
        }
    }

    fn for_each<F>(&self, high: u16, f: &mut F)
    where
        F: FnMut(u32),
    {
        let prefix = (high as u32) << 16;
        match self {
            Container::Array(arr) => {
                for &low in arr {
                    f(prefix | (low as u32));
                }
            }
            Container::Bitset(words) => {
                for (w_idx, &word) in words.iter().enumerate() {
                    if word != 0 {
                        let mut temp = word;
                        let base_low = (w_idx as u32) * 64;
                        while temp != 0 {
                            let tz = temp.trailing_zeros();
                            f(prefix | (base_low + tz));
                            temp &= temp - 1;
                        }
                    }
                }
            }
        }
    }
}

/// Roaring Bitmap for efficient integer set tracking across sparse and dense ID ranges.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RoaringBitmap {
    chunks: Vec<(u16, Container)>,
}

impl RoaringBitmap {
    pub fn new() -> Self {
        Self { chunks: Vec::new() }
    }

    pub fn insert(&mut self, val: u32) -> bool {
        let high = (val >> 16) as u16;
        let low = (val & 0xFFFF) as u16;

        match self.chunks.binary_search_by_key(&high, |(h, _)| *h) {
            Ok(idx) => self.chunks[idx].1.insert(low),
            Err(idx) => {
                let mut container = Container::new_array();
                container.insert(low);
                self.chunks.insert(idx, (high, container));
                true
            }
        }
    }

    pub fn remove(&mut self, val: u32) -> bool {
        let high = (val >> 16) as u16;
        let low = (val & 0xFFFF) as u16;

        if let Ok(idx) = self.chunks.binary_search_by_key(&high, |(h, _)| *h) {
            let removed = self.chunks[idx].1.remove(low);
            if self.chunks[idx].1.cardinality() == 0 {
                self.chunks.remove(idx);
            }
            removed
        } else {
            false
        }
    }

    pub fn contains(&self, val: u32) -> bool {
        let high = (val >> 16) as u16;
        let low = (val & 0xFFFF) as u16;

        if let Ok(idx) = self.chunks.binary_search_by_key(&high, |(h, _)| *h) {
            self.chunks[idx].1.contains(low)
        } else {
            false
        }
    }

    pub fn len(&self) -> usize {
        self.chunks.iter().map(|(_, c)| c.cardinality()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn to_vec(&self) -> Vec<u32> {
        let mut result = Vec::with_capacity(self.len());
        for (high, container) in &self.chunks {
            container.for_each(*high, &mut |val| result.push(val));
        }
        result
    }
}

// ---------------------------------------------------------------------------
// Worktree Delta Overlay Models
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayEdge {
    pub source: String,
    pub target: String,
    pub kind: String,
    pub confidence: u8,
}

/// Ephemeral overlay containing uncommitted worktree changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeOverlay {
    pub worktree_id: String,
    pub generation: u64,
    /// Explicitly deleted files.
    pub tombstoned_files: HashSet<PathBuf>,
    /// Explicitly deleted symbols.
    pub tombstoned_symbols: HashSet<String>,
    /// Roaring bitmap of dirty node IDs needing re-indexing/invalidation.
    pub dirty_nodes: RoaringBitmap,
    /// Map of file path to Blake3 CAS hash or CAS_TOMBSTONE.
    pub modified_files: HashMap<PathBuf, String>,
    /// Modified or newly added AST extractions in the worktree.
    pub added_extractions: HashMap<String, Extraction>,
    /// Added or modified dependency edges in the worktree.
    pub delta_edges: Vec<OverlayEdge>,
}

impl WorktreeOverlay {
    pub fn new(worktree_id: impl Into<String>, generation: u64) -> Self {
        Self {
            worktree_id: worktree_id.into(),
            generation,
            tombstoned_files: HashSet::new(),
            tombstoned_symbols: HashSet::new(),
            dirty_nodes: RoaringBitmap::new(),
            modified_files: HashMap::new(),
            added_extractions: HashMap::new(),
            delta_edges: Vec::new(),
        }
    }

    /// Mark a file as deleted in the ephemeral worktree.
    pub fn mark_file_tombstone(&mut self, path: impl Into<PathBuf>, dirty_node_id: Option<u32>) {
        let pb = path.into();
        self.tombstoned_files.insert(pb.clone());
        self.modified_files.insert(pb, CAS_TOMBSTONE.to_string());
        if let Some(id) = dirty_node_id {
            self.dirty_nodes.insert(id);
        }
    }

    /// Mark a symbol as deleted in the ephemeral worktree.
    pub fn mark_symbol_tombstone(
        &mut self,
        symbol_id: impl Into<String>,
        dirty_node_id: Option<u32>,
    ) {
        let sym = symbol_id.into();
        self.tombstoned_symbols.insert(sym);
        if let Some(id) = dirty_node_id {
            self.dirty_nodes.insert(id);
        }
    }

    /// Record a modified file with its new Blake3 CAS hash.
    pub fn record_modified_file(
        &mut self,
        path: impl Into<PathBuf>,
        cas_hash: impl Into<String>,
        dirty_node_id: Option<u32>,
    ) {
        let pb = path.into();
        let hash = cas_hash.into();
        self.tombstoned_files.remove(&pb);
        self.modified_files.insert(pb, hash);
        if let Some(id) = dirty_node_id {
            self.dirty_nodes.insert(id);
        }
    }

    /// Add an extracted AST for a new or modified symbol.
    pub fn record_extraction(
        &mut self,
        symbol_id: impl Into<String>,
        extraction: Extraction,
        node_id: u32,
    ) {
        let sym = symbol_id.into();
        self.tombstoned_symbols.remove(&sym);
        self.added_extractions.insert(sym, extraction);
        self.dirty_nodes.insert(node_id);
    }

    /// Add an edge into the delta overlay.
    pub fn add_edge(&mut self, edge: OverlayEdge) {
        self.delta_edges.push(edge);
    }

    /// Query whether a file is deleted in this worktree overlay.
    pub fn is_file_tombstoned(&self, path: &Path) -> bool {
        self.tombstoned_files.contains(path)
            || self
                .modified_files
                .get(path)
                .map(|h| h == CAS_TOMBSTONE)
                .unwrap_or(false)
    }

    /// Query whether a symbol is deleted in this worktree overlay.
    pub fn is_symbol_tombstoned(&self, symbol_id: &str) -> bool {
        self.tombstoned_symbols.contains(symbol_id)
    }

    /// Query whether a node ID is flagged dirty in the Roaring Bitmap.
    pub fn is_node_dirty(&self, node_id: u32) -> bool {
        self.dirty_nodes.contains(node_id)
    }

    /// Resolve a file's effective CAS hash, overlaying worktree changes over base storage.
    pub fn resolve_file<'a>(&'a self, path: &Path, base_hash: Option<&'a str>) -> Option<&'a str> {
        if let Some(overlay_hash) = self.modified_files.get(path) {
            if overlay_hash == CAS_TOMBSTONE {
                None
            } else {
                Some(overlay_hash.as_str())
            }
        } else if self.tombstoned_files.contains(path) {
            None
        } else {
            base_hash
        }
    }
}

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roaring_bitmap_sparse_and_dense() {
        let mut bitmap = RoaringBitmap::new();
        assert!(bitmap.is_empty());

        // Insert sparse elements
        assert!(bitmap.insert(10));
        assert!(bitmap.insert(42));
        assert!(bitmap.insert(70_000)); // Second chunk (high=1)
        assert!(!bitmap.insert(10)); // Duplicate

        assert_eq!(bitmap.len(), 3);
        assert!(bitmap.contains(10));
        assert!(bitmap.contains(42));
        assert!(bitmap.contains(70_000));
        assert!(!bitmap.contains(100));

        // Density threshold transition (> 4096 entries in a chunk)
        for i in 1000..6000 {
            bitmap.insert(i);
        }
        assert!(bitmap.len() >= 5000);
        assert!(bitmap.contains(1500));
        assert!(bitmap.contains(5999));
        assert!(!bitmap.contains(6001));

        // Removal
        assert!(bitmap.remove(1500));
        assert!(!bitmap.contains(1500));
        assert!(!bitmap.remove(1500));

        let vec = bitmap.to_vec();
        assert!(vec.contains(&10));
        assert!(vec.contains(&70_000));
        assert!(!vec.contains(&1500));
    }

    #[test]
    fn test_worktree_overlay_tombstone_precedence() {
        let mut overlay = WorktreeOverlay::new("wt-test-1", 1);
        let path = PathBuf::from("src/foo.rs");
        let base_hash = "b3_0123456789abcdef";

        // Baseline: file not in overlay, resolves to base hash
        assert_eq!(
            overlay.resolve_file(&path, Some(base_hash)),
            Some(base_hash)
        );

        // Mark file with tombstone
        overlay.mark_file_tombstone(&path, Some(101));
        assert!(overlay.is_file_tombstoned(&path));
        assert!(overlay.is_node_dirty(101));
        assert_eq!(overlay.resolve_file(&path, Some(base_hash)), None);

        // Modify file with new hash
        let new_hash = "b3_fedcba9876543210";
        overlay.record_modified_file(&path, new_hash, Some(101));
        assert!(!overlay.is_file_tombstoned(&path));
        assert_eq!(overlay.resolve_file(&path, Some(base_hash)), Some(new_hash));
    }

    #[test]
    fn test_worktree_overlay_symbols_and_edges() {
        let mut overlay = WorktreeOverlay::new("wt-test-2", 2);
        overlay.mark_symbol_tombstone("sym_deleted", Some(202));
        assert!(overlay.is_symbol_tombstoned("sym_deleted"));
        assert!(overlay.is_node_dirty(202));

        overlay.add_edge(OverlayEdge {
            source: "sym_a".to_string(),
            target: "sym_b".to_string(),
            kind: "calls".to_string(),
            confidence: 95,
        });
        assert_eq!(overlay.delta_edges.len(), 1);
        assert_eq!(overlay.delta_edges[0].kind, "calls");
    }
}
