//! Fast bitset implementation for node ID sets, roots, and dirty flags.

use serde::{Deserialize, Serialize};

const WORDS_PER_CHUNK: usize = 1024; // 1024 * 64 = 65,536 bits per chunk

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum Chunk {
    Sparse(Vec<u16>),
    Dense(Vec<u64>),
}

const SPARSE_CUTOFF: usize = 4096;

impl Chunk {
    fn new_sparse() -> Self {
        Chunk::Sparse(Vec::new())
    }

    fn insert(&mut self, val: u16) -> bool {
        match self {
            Chunk::Sparse(v) => match v.binary_search(&val) {
                Ok(_) => false,
                Err(idx) => {
                    v.insert(idx, val);
                    if v.len() > SPARSE_CUTOFF {
                        let mut words = vec![0u64; WORDS_PER_CHUNK];
                        for &x in v.iter() {
                            let w = (x as usize) / 64;
                            let b = (x as usize) % 64;
                            words[w] |= 1u64 << b;
                        }
                        *self = Chunk::Dense(words);
                    }
                    true
                }
            },
            Chunk::Dense(words) => {
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
            Chunk::Sparse(v) => match v.binary_search(&val) {
                Ok(idx) => {
                    v.remove(idx);
                    true
                }
                Err(_) => false,
            },
            Chunk::Dense(words) => {
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
            Chunk::Sparse(v) => v.binary_search(&val).is_ok(),
            Chunk::Dense(words) => {
                let w = (val as usize) / 64;
                let b = (val as usize) % 64;
                (words[w] & (1u64 << b)) != 0
            }
        }
    }

    fn cardinality(&self) -> usize {
        match self {
            Chunk::Sparse(v) => v.len(),
            Chunk::Dense(words) => words.iter().map(|w| w.count_ones() as usize).sum(),
        }
    }

    fn for_each<F>(&self, high: u16, f: &mut F)
    where
        F: FnMut(u32),
    {
        let prefix = (high as u32) << 16;
        match self {
            Chunk::Sparse(v) => {
                for &low in v {
                    f(prefix | (low as u32));
                }
            }
            Chunk::Dense(words) => {
                for (w_idx, &word) in words.iter().enumerate() {
                    if word != 0 {
                        let mut temp = word;
                        let base = (w_idx as u32) * 64;
                        while temp != 0 {
                            let tz = temp.trailing_zeros();
                            f(prefix | (base + tz));
                            temp &= temp - 1;
                        }
                    }
                }
            }
        }
    }
}

/// Chunked FastBitSet / Roaring Bitset for tracking dirty nodes, roots, and visited sets.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FastBitSet {
    chunks: Vec<(u16, Chunk)>,
}

impl FastBitSet {
    pub fn new() -> Self {
        Self { chunks: Vec::new() }
    }

    pub fn insert(&mut self, val: u32) -> bool {
        let high = (val >> 16) as u16;
        let low = (val & 0xFFFF) as u16;

        match self.chunks.binary_search_by_key(&high, |(h, _)| *h) {
            Ok(idx) => self.chunks[idx].1.insert(low),
            Err(idx) => {
                let mut chunk = Chunk::new_sparse();
                chunk.insert(low);
                self.chunks.insert(idx, (high, chunk));
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

    pub fn clear(&mut self) {
        self.chunks.clear();
    }

    pub fn to_vec(&self) -> Vec<u32> {
        let mut result = Vec::with_capacity(self.len());
        for (high, chunk) in &self.chunks {
            chunk.for_each(*high, &mut |v| result.push(v));
        }
        result
    }
}
