//! Flat Compressed Sparse Row (CSR) Zero-Copy Graph Storage Engine.
//!
//! Provides sub-100ns outgoing edge traversal per hop using memory-mapped contiguous buffers.

use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::Write;
use std::path::Path;

pub const DC_GRAPH_MAGIC: &[u8; 8] = b"DCGRAPH1";
pub const DC_GRAPH_VERSION: u32 = 1;

/// Fixed-size 32-byte node metadata record.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CsrNodeRecord {
    pub node_id: u32,
    pub kind: u16,
    pub flags: u16, // bit 0: entrypoint root, bit 1: test, bit 2: dirty, bit 3: exported
    pub out_degree: u32,
    pub _reserved: u32,
    pub name_hash: u64,
    pub file_path_hash: u64,
}

impl CsrNodeRecord {
    pub const FLAG_ENTRYPOINT: u16 = 1 << 0;
    pub const FLAG_TEST: u16 = 1 << 1;
    pub const FLAG_DIRTY: u16 = 1 << 2;
    pub const FLAG_EXPORTED: u16 = 1 << 3;

    pub fn is_entrypoint(&self) -> bool {
        (self.flags & Self::FLAG_ENTRYPOINT) != 0
    }

    pub fn is_test(&self) -> bool {
        (self.flags & Self::FLAG_TEST) != 0
    }

    pub fn is_dirty(&self) -> bool {
        (self.flags & Self::FLAG_DIRTY) != 0
    }
}

/// 80-byte header preceding CSR buffers.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CsrHeader {
    pub magic: [u8; 8],
    pub version: u32,
    pub flags: u32,
    pub generation: u64,
    pub node_count: u64,
    pub edge_count: u64,
    pub nodes_offset: u64,
    pub row_offsets_offset: u64,
    pub col_indices_offset: u64,
    pub attributes_offset: u64,
}

#[cfg(unix)]
struct RawMmap {
    ptr: *const u8,
    len: usize,
}

#[cfg(unix)]
impl RawMmap {
    fn map(file: &File) -> Result<Self> {
        use std::os::unix::io::AsRawFd;
        let len = file.metadata()?.len() as usize;
        if len == 0 {
            bail!("Cannot mmap empty CSR graph file");
        }
        let raw = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                file.as_raw_fd(),
                0,
            )
        };
        if raw == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error()).context("mmap failed");
        }
        Ok(Self {
            ptr: raw as *const u8,
            len,
        })
    }

    fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

#[cfg(unix)]
impl Drop for RawMmap {
    fn drop(&mut self) {
        if !self.ptr.is_null() && self.len > 0 {
            unsafe {
                let _ = libc::munmap(self.ptr as *mut libc::c_void, self.len);
            }
        }
    }
}

enum StorageBackend {
    #[cfg(unix)]
    Mmap(RawMmap),
    Buffer(Vec<u8>),
}

impl StorageBackend {
    fn as_slice(&self) -> &[u8] {
        match self {
            #[cfg(unix)]
            StorageBackend::Mmap(m) => m.as_slice(),
            StorageBackend::Buffer(b) => b.as_slice(),
        }
    }
}

/// Zero-copy CSR graph reader. Provides direct memory slice references with no deserialization.
pub struct CsrGraphReader {
    _storage: StorageBackend,
    header: CsrHeader,
    nodes: *const CsrNodeRecord,
    row_offsets: *const u64,
    col_indices: *const u32,
    attributes: *const u16,
}

// Safety: The memory buffer is immutable once loaded into CsrGraphReader.
unsafe impl Send for CsrGraphReader {}
unsafe impl Sync for CsrGraphReader {}

impl CsrGraphReader {
    /// Open a CSR graph binary from disk using OS memory mapping (mmap).
    pub fn open_mmap(path: &Path) -> Result<Self> {
        #[cfg(unix)]
        {
            let file = File::open(path)
                .with_context(|| format!("Failed to open CSR graph file: {:?}", path))?;
            let mmap = RawMmap::map(&file)?;
            Self::from_backend(StorageBackend::Mmap(mmap))
        }
        #[cfg(not(unix))]
        {
            let bytes = std::fs::read(path)
                .with_context(|| format!("Failed to read CSR graph file: {:?}", path))?;
            Self::from_backend(StorageBackend::Buffer(bytes))
        }
    }

    /// Load a CSR graph directly from an in-memory byte buffer (e.g., test or IPC).
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        Self::from_backend(StorageBackend::Buffer(bytes))
    }

    fn from_backend(storage: StorageBackend) -> Result<Self> {
        let slice = storage.as_slice();
        if slice.len() < std::mem::size_of::<CsrHeader>() {
            bail!("Buffer too small for CSR header: {} bytes", slice.len());
        }

        let header = unsafe { *(slice.as_ptr() as *const CsrHeader) };
        if &header.magic != DC_GRAPH_MAGIC {
            bail!("Invalid CSR graph magic bytes");
        }
        if header.version != DC_GRAPH_VERSION {
            bail!("Unsupported CSR graph version: {}", header.version);
        }

        let n = header.node_count as usize;
        let m = header.edge_count as usize;

        let nodes_end = (header.nodes_offset as usize) + n * std::mem::size_of::<CsrNodeRecord>();
        let rows_end = (header.row_offsets_offset as usize) + (n + 1) * std::mem::size_of::<u64>();
        let cols_end = (header.col_indices_offset as usize) + m * std::mem::size_of::<u32>();
        let attrs_end = (header.attributes_offset as usize) + m * std::mem::size_of::<u16>();

        if nodes_end > slice.len()
            || rows_end > slice.len()
            || cols_end > slice.len()
            || attrs_end > slice.len()
        {
            bail!("CSR data offsets exceed file length: {}", slice.len());
        }

        let nodes =
            unsafe { slice.as_ptr().add(header.nodes_offset as usize) as *const CsrNodeRecord };
        let row_offsets =
            unsafe { slice.as_ptr().add(header.row_offsets_offset as usize) as *const u64 };
        let col_indices =
            unsafe { slice.as_ptr().add(header.col_indices_offset as usize) as *const u32 };
        let attributes =
            unsafe { slice.as_ptr().add(header.attributes_offset as usize) as *const u16 };

        Ok(Self {
            _storage: storage,
            header,
            nodes,
            row_offsets,
            col_indices,
            attributes,
        })
    }

    #[inline(always)]
    pub fn node_count(&self) -> usize {
        self.header.node_count as usize
    }

    #[inline(always)]
    pub fn edge_count(&self) -> usize {
        self.header.edge_count as usize
    }

    #[inline(always)]
    pub fn generation(&self) -> u64 {
        self.header.generation
    }

    /// Retrieve node metadata by ID.
    #[inline(always)]
    pub fn node(&self, node_id: u32) -> Option<&CsrNodeRecord> {
        let idx = node_id as usize;
        if idx < self.node_count() {
            unsafe { Some(&*self.nodes.add(idx)) }
        } else {
            None
        }
    }

    /// Retrieve outgoing edges from node `u`. Sub-100ns zero-allocation slice retrieval.
    #[inline(always)]
    pub fn outgoing_edges(&self, u: u32) -> &[u32] {
        let idx = u as usize;
        if idx >= self.node_count() {
            return &[];
        }
        unsafe {
            let start = *self.row_offsets.add(idx) as usize;
            let end = *self.row_offsets.add(idx + 1) as usize;
            let ptr = self.col_indices.add(start);
            std::slice::from_raw_parts(ptr, end - start)
        }
    }

    /// Retrieve edge attributes for outgoing edges of node `u`.
    #[inline(always)]
    pub fn edge_attributes(&self, u: u32) -> &[u16] {
        let idx = u as usize;
        if idx >= self.node_count() {
            return &[];
        }
        unsafe {
            let start = *self.row_offsets.add(idx) as usize;
            let end = *self.row_offsets.add(idx + 1) as usize;
            let ptr = self.attributes.add(start);
            std::slice::from_raw_parts(ptr, end - start)
        }
    }

    /// Breadth-first search traversal reaching dependent nodes up to `max_depth`.
    pub fn bfs_reachability(&self, start_node: u32, max_depth: usize) -> Vec<u32> {
        let mut visited = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();

        visited.insert(start_node);
        queue.push_back((start_node, 0usize));

        let mut reachable = Vec::new();

        while let Some((curr, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            for &target in self.outgoing_edges(curr) {
                if visited.insert(target) {
                    reachable.push(target);
                    queue.push_back((target, depth + 1));
                }
            }
        }
        reachable
    }
}

/// Builder for constructing flat CSR binary representations.
pub struct CsrGraphWriter {
    generation: u64,
    nodes: Vec<CsrNodeRecord>,
    edges: Vec<Vec<(u32, u16)>>, // node_id -> Vec<(target_id, attribute)>
}

impl CsrGraphWriter {
    pub fn new(generation: u64) -> Self {
        Self {
            generation,
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    pub fn add_node(&mut self, mut record: CsrNodeRecord) -> u32 {
        let id = self.nodes.len() as u32;
        record.node_id = id;
        self.nodes.push(record);
        self.edges.push(Vec::new());
        id
    }

    pub fn add_edge(&mut self, source: u32, target: u32, attribute: u16) {
        let s_idx = source as usize;
        if s_idx >= self.edges.len() {
            self.edges.resize(s_idx + 1, Vec::new());
            while self.nodes.len() <= s_idx {
                self.nodes.push(CsrNodeRecord {
                    node_id: self.nodes.len() as u32,
                    ..Default::default()
                });
            }
        }
        self.edges[s_idx].push((target, attribute));
    }

    /// Serialize into contiguous byte buffer adhering to alignment requirements.
    pub fn build(mut self) -> Vec<u8> {
        let n = self.nodes.len();
        let mut edge_count = 0usize;
        for (i, edge_list) in self.edges.iter_mut().enumerate() {
            edge_list.sort_by_key(|(t, _)| *t);
            self.nodes[i].out_degree = edge_list.len() as u32;
            edge_count += edge_list.len();
        }

        let header_size = 128usize; // padded header
        let nodes_offset = header_size;
        let nodes_size = n * std::mem::size_of::<CsrNodeRecord>();

        let row_offsets_offset = align_8(nodes_offset + nodes_size);
        let row_offsets_size = (n + 1) * std::mem::size_of::<u64>();

        let col_indices_offset = align_8(row_offsets_offset + row_offsets_size);
        let col_indices_size = edge_count * std::mem::size_of::<u32>();

        let attributes_offset = align_8(col_indices_offset + col_indices_size);
        let attributes_size = edge_count * std::mem::size_of::<u16>();

        let total_size = align_8(attributes_offset + attributes_size);
        let mut buffer = vec![0u8; total_size];

        let header = CsrHeader {
            magic: *DC_GRAPH_MAGIC,
            version: DC_GRAPH_VERSION,
            flags: 0,
            generation: self.generation,
            node_count: n as u64,
            edge_count: edge_count as u64,
            nodes_offset: nodes_offset as u64,
            row_offsets_offset: row_offsets_offset as u64,
            col_indices_offset: col_indices_offset as u64,
            attributes_offset: attributes_offset as u64,
        };

        unsafe {
            *(buffer.as_mut_ptr() as *mut CsrHeader) = header;
        }

        // Copy nodes
        if n > 0 {
            unsafe {
                let dest = buffer.as_mut_ptr().add(nodes_offset) as *mut CsrNodeRecord;
                std::ptr::copy_nonoverlapping(self.nodes.as_ptr(), dest, n);
            }
        }

        // Build row offsets, col indices, attributes
        let mut current_offset = 0u64;
        let mut row_offsets = Vec::with_capacity(n + 1);
        let mut col_indices = Vec::with_capacity(edge_count);
        let mut attributes = Vec::with_capacity(edge_count);

        for edge_list in &self.edges {
            row_offsets.push(current_offset);
            for &(target, attr) in edge_list {
                col_indices.push(target);
                attributes.push(attr);
            }
            current_offset += edge_list.len() as u64;
        }
        row_offsets.push(current_offset);

        unsafe {
            let row_dest = buffer.as_mut_ptr().add(row_offsets_offset) as *mut u64;
            std::ptr::copy_nonoverlapping(row_offsets.as_ptr(), row_dest, n + 1);

            if edge_count > 0 {
                let col_dest = buffer.as_mut_ptr().add(col_indices_offset) as *mut u32;
                std::ptr::copy_nonoverlapping(col_indices.as_ptr(), col_dest, edge_count);

                let attr_dest = buffer.as_mut_ptr().add(attributes_offset) as *mut u16;
                std::ptr::copy_nonoverlapping(attributes.as_ptr(), attr_dest, edge_count);
            }
        }

        buffer
    }

    pub fn write_to_file(self, path: &Path) -> Result<()> {
        let bytes = self.build();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = File::create(path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        Ok(())
    }
}

fn align_8(n: usize) -> usize {
    (n + 7) & !7
}
