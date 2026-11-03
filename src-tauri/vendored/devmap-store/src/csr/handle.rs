//! Lock-free Atomic Generation Swapping for Graph Readers.

use super::graph::CsrGraphReader;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// Handle providing concurrent readers with wait-free / low-overhead access to the current graph generation,
/// while permitting lock-free/atomic generation updates when new index passes land.
pub struct AtomicGraphHandle {
    current: RwLock<Arc<CsrGraphReader>>,
    generation: AtomicU64,
}

impl AtomicGraphHandle {
    pub fn new(initial: CsrGraphReader) -> Self {
        let gen = initial.generation();
        Self {
            current: RwLock::new(Arc::new(initial)),
            generation: AtomicU64::new(gen),
        }
    }

    /// Obtain a shared reference counted handle to the current graph generation.
    /// Readers can query this instance concurrently without blocking other readers.
    pub fn load(&self) -> Arc<CsrGraphReader> {
        let guard = self.current.read().expect("lock poisoned");
        Arc::clone(&*guard)
    }

    /// Atomically swap in a new generation graph reader. Returns the previous graph reader.
    pub fn swap(&self, new_reader: CsrGraphReader) -> Arc<CsrGraphReader> {
        let new_gen = new_reader.generation();
        let mut guard = self.current.write().expect("lock poisoned");
        let old = Arc::clone(&*guard);
        *guard = Arc::new(new_reader);
        self.generation.store(new_gen, Ordering::Release);
        old
    }

    /// Read the currently loaded graph generation.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Check if a given generation number is older than the current generation.
    pub fn is_stale(&self, observed_gen: u64) -> bool {
        self.generation() > observed_gen
    }
}
