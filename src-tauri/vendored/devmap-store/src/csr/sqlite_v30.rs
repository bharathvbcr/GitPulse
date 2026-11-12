//! Topologically Scoped Incremental Digest Engine.

use std::collections::{HashMap, HashSet};

use super::bitset::FastBitSet;
use super::graph::CsrGraphReader;

/// 1-Hop Topologically Scoped Incremental Digest Engine.
///
/// Limits digest recalculation to modified nodes and their direct 1-hop neighborhood,
/// achieving sub-millisecond incremental update performance.
pub struct IncrementalDigestEngine;

impl IncrementalDigestEngine {
    /// Deterministic 64-bit FNV-1a hash of a byte slice (symbol name or file path).
    pub fn hash_bytes(bytes: &[u8]) -> u64 {
        let mut state: u64 = 0xcbf29ce484222325;
        for &b in bytes {
            state ^= u64::from(b);
            state = state.wrapping_mul(0x100000001b3);
        }
        state
    }

    /// Recalculate node digests strictly scoped to the 1-hop neighborhood of dirty nodes.
    pub fn compute_1hop_digests(
        reader: &CsrGraphReader,
        dirty_nodes: &FastBitSet,
        previous_digests: &HashMap<u32, u64>,
    ) -> HashMap<u32, u64> {
        let dirty_vec = dirty_nodes.to_vec();
        if dirty_vec.is_empty() {
            return previous_digests.clone();
        }

        // 1. Collect 1-hop outward neighborhood
        let mut affected = HashSet::new();
        for &u in &dirty_vec {
            affected.insert(u);
            for &target in reader.outgoing_edges(u) {
                affected.insert(target);
            }
        }

        // 2. Collect 1-hop inward neighborhood (nodes pointing into dirty nodes)
        let n = reader.node_count();
        for candidate in 0..n as u32 {
            if affected.contains(&candidate) {
                continue;
            }
            for &target in reader.outgoing_edges(candidate) {
                if dirty_nodes.contains(target) {
                    affected.insert(candidate);
                    break;
                }
            }
        }

        // 3. Incrementally update digests for affected nodes only
        let mut updated_digests = previous_digests.clone();

        for &node_id in &affected {
            if let Some(record) = reader.node(node_id) {
                let mut hasher = Fnv1aHasher::new();
                hasher.write_u64(record.name_hash);
                hasher.write_u64(record.file_path_hash);
                hasher.write_u16(record.kind);
                hasher.write_u16(record.flags);

                let edges = reader.outgoing_edges(node_id);
                let attrs = reader.edge_attributes(node_id);
                for (&target, &attr) in edges.iter().zip(attrs.iter()) {
                    hasher.write_u32(target);
                    hasher.write_u16(attr);
                }

                updated_digests.insert(node_id, hasher.finish());
            }
        }

        updated_digests
    }
}

// Minimal fast deterministic 64-bit hasher for incremental digests
struct Fnv1aHasher {
    state: u64,
}

impl Fnv1aHasher {
    fn new() -> Self {
        Self {
            state: 0xcbf29ce484222325,
        }
    }

    fn write_u64(&mut self, val: u64) {
        for b in val.to_le_bytes() {
            self.state ^= b as u64;
            self.state = self.state.wrapping_mul(0x100000001b3);
        }
    }

    fn write_u32(&mut self, val: u32) {
        for b in val.to_le_bytes() {
            self.state ^= b as u64;
            self.state = self.state.wrapping_mul(0x100000001b3);
        }
    }

    fn write_u16(&mut self, val: u16) {
        for b in val.to_le_bytes() {
            self.state ^= b as u64;
            self.state = self.state.wrapping_mul(0x100000001b3);
        }
    }

    fn finish(&self) -> u64 {
        self.state
    }
}
