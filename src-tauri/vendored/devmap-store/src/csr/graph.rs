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

/// The mapped file changed after it was mapped: it was truncated, extended or
/// rewritten in place. A truncated mapping is `SIGBUS` on first touch, and an
/// in-place rewrite makes the boundaries checked at open stale, so the reader
/// refuses to be used rather than answer from either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsrFileChanged {
    pub mapped_len: u64,
    pub found_len: Option<u64>,
}

impl std::fmt::Display for CsrFileChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.found_len {
            Some(found) => write!(
                f,
                "the CSR graph file changed after it was mapped (mapped {} bytes, now {found} \
bytes or a newer modification time); reopen it",
                self.mapped_len
            ),
            None => write!(
                f,
                "the CSR graph file can no longer be examined after it was mapped (mapped {} \
bytes); reopen it",
                self.mapped_len
            ),
        }
    }
}

impl std::error::Error for CsrFileChanged {}

#[cfg(unix)]
struct RawMmap {
    ptr: *const u8,
    len: usize,
    /// The descriptor the mapping was made from, held so a later `fstat`
    /// describes this file and never whatever a name points at now.
    file: File,
    modified: Option<std::time::SystemTime>,
}

#[cfg(unix)]
impl RawMmap {
    fn map(file: File) -> Result<Self> {
        use std::os::unix::io::AsRawFd;
        let metadata = file.metadata()?;
        let len = metadata.len() as usize;
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
            modified: metadata.modified().ok(),
            file,
        })
    }

    fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    /// `fstat` on the held descriptor against what was mapped.
    fn recheck(&self) -> std::result::Result<(), CsrFileChanged> {
        let mapped_len = self.len as u64;
        let now = self.file.metadata().map_err(|_| CsrFileChanged {
            mapped_len,
            found_len: None,
        })?;
        if now.len() != mapped_len || now.modified().ok() != self.modified {
            return Err(CsrFileChanged {
                mapped_len,
                found_len: Some(now.len()),
            });
        }
        Ok(())
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

/// Bytes held in memory at an 8-byte boundary.
///
/// A `Vec<u8>` promises alignment 1, and the sections of a CSR file are read
/// through `u64` and `u32` pointers; the allocator usually happens to align a
/// large `Vec<u8>` but nothing guarantees it. Backing the copy with `u64`s
/// does.
struct AlignedBuffer {
    words: Vec<u64>,
    len: usize,
}

impl AlignedBuffer {
    fn copy_of(bytes: &[u8]) -> Self {
        let mut words = vec![0u64; bytes.len().div_ceil(8)];
        // SAFETY: `words` owns at least `bytes.len()` bytes, `u8` has alignment
        // 1, and the two allocations are distinct.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                words.as_mut_ptr() as *mut u8,
                bytes.len(),
            );
        }
        Self {
            words,
            len: bytes.len(),
        }
    }

    fn as_slice(&self) -> &[u8] {
        // SAFETY: `len` bytes lie inside `words`, which is initialised.
        unsafe { std::slice::from_raw_parts(self.words.as_ptr() as *const u8, self.len) }
    }
}

enum StorageBackend {
    #[cfg(unix)]
    Mmap(RawMmap),
    Buffer(AlignedBuffer),
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
            let mmap = RawMmap::map(file)?;
            Self::from_backend(StorageBackend::Mmap(mmap))
        }
        #[cfg(not(unix))]
        {
            let bytes = std::fs::read(path)
                .with_context(|| format!("Failed to read CSR graph file: {:?}", path))?;
            Self::from_bytes(bytes)
        }
    }

    /// Load a CSR graph directly from an in-memory byte buffer (e.g., test or IPC).
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        Self::from_backend(StorageBackend::Buffer(AlignedBuffer::copy_of(&bytes)))
    }

    /// Validate everything the file says about itself before any pointer into
    /// it is made. After this returns `Ok`, every slice the accessors build is
    /// inside the mapping, aligned for its element type, and no longer than the
    /// edge array.
    fn from_backend(storage: StorageBackend) -> Result<Self> {
        let slice = storage.as_slice();
        let header_len = std::mem::size_of::<CsrHeader>();
        if slice.len() < header_len {
            bail!("Buffer too small for CSR header: {} bytes", slice.len());
        }

        // SAFETY: at least `size_of::<CsrHeader>()` bytes are readable (checked
        // above) and every bit pattern is a valid `CsrHeader` (integers and a
        // byte array); `read_unaligned` makes no alignment assumption.
        let header = unsafe { std::ptr::read_unaligned(slice.as_ptr() as *const CsrHeader) };
        if &header.magic != DC_GRAPH_MAGIC {
            bail!("Invalid CSR graph magic bytes");
        }
        if header.version != DC_GRAPH_VERSION {
            bail!("Unsupported CSR graph version: {}", header.version);
        }

        let n = usize::try_from(header.node_count)
            .context("CSR node count does not fit in this address space")?;
        let m = usize::try_from(header.edge_count)
            .context("CSR edge count does not fit in this address space")?;
        let rows = n
            .checked_add(1)
            .context("CSR node count leaves no room for the final row boundary")?;

        let base = slice.as_ptr() as usize;
        // One section: `count` elements of `size` bytes at `offset`, wholly inside
        // the file, past the header, and aligned for the element.
        let section =
            |name: &str, offset: u64, count: usize, size: usize, align: usize| -> Result<usize> {
                let offset = usize::try_from(offset)
                    .with_context(|| format!("CSR {name} offset {offset} does not fit"))?;
                let bytes = count.checked_mul(size).with_context(|| {
                    format!("CSR {name} section size overflows ({count} x {size})")
                })?;
                let end = offset
                    .checked_add(bytes)
                    .with_context(|| format!("CSR {name} section end overflows"))?;
                if end > slice.len() {
                    bail!(
                        "CSR {name} section ends at {end}, past the file length {}",
                        slice.len()
                    );
                }
                if offset < header_len {
                    bail!("CSR {name} section at {offset} overlaps the {header_len}-byte header");
                }
                if base.wrapping_add(offset) % align != 0 {
                    bail!("CSR {name} section at {offset} is not aligned to {align} bytes");
                }
                Ok(offset)
            };
        let nodes_at = section(
            "nodes",
            header.nodes_offset,
            n,
            std::mem::size_of::<CsrNodeRecord>(),
            std::mem::align_of::<CsrNodeRecord>(),
        )?;
        let rows_at = section(
            "row offsets",
            header.row_offsets_offset,
            rows,
            std::mem::size_of::<u64>(),
            std::mem::align_of::<u64>(),
        )?;
        let cols_at = section(
            "column indices",
            header.col_indices_offset,
            m,
            std::mem::size_of::<u32>(),
            std::mem::align_of::<u32>(),
        )?;
        let attrs_at = section(
            "attributes",
            header.attributes_offset,
            m,
            std::mem::size_of::<u16>(),
            std::mem::align_of::<u16>(),
        )?;

        // The row boundaries are read at query time to slice the edge arrays,
        // so each must be in range and the sequence must not run backwards.
        // SAFETY: the section is in bounds and aligned (checked above) and every
        // bit pattern is a valid `u64`.
        let boundaries =
            unsafe { std::slice::from_raw_parts(slice.as_ptr().add(rows_at) as *const u64, rows) };
        let mut previous = 0u64;
        for (row, &boundary) in boundaries.iter().enumerate() {
            let in_order = if row == 0 {
                boundary == 0
            } else {
                boundary >= previous
            };
            if !in_order || boundary > header.edge_count {
                bail!("CSR row boundary {row} is {boundary}, which is out of order or past the {m} edges");
            }
            previous = boundary;
        }
        if previous != header.edge_count {
            bail!("CSR row boundaries end at {previous}, not at the {m} edges");
        }

        // SAFETY (the four pointers): each section was bounds-checked and
        // alignment-checked above, within the storage this reader owns.
        let nodes = unsafe { slice.as_ptr().add(nodes_at) as *const CsrNodeRecord };
        let row_offsets = unsafe { slice.as_ptr().add(rows_at) as *const u64 };
        let col_indices = unsafe { slice.as_ptr().add(cols_at) as *const u32 };
        let attributes = unsafe { slice.as_ptr().add(attrs_at) as *const u16 };

        Ok(Self {
            _storage: storage,
            header,
            nodes,
            row_offsets,
            col_indices,
            attributes,
        })
    }

    /// Whether the mapped file is still the one that was validated at open.
    ///
    /// The checks in [`Self::open_mmap`] hold for the bytes as they were. A file
    /// truncated afterwards is `SIGBUS` on the next touch, and one rewritten in
    /// place invalidates the checked row boundaries. Call this at the start of
    /// each query-level operation and reopen on `Err`; the accessors do not
    /// call it themselves, because an `fstat` per hop would cost more than the
    /// hop. It narrows the window and does not close it: a truncation between
    /// this check and the access that follows is still a signal, which only a
    /// reader that copies the file out (or never shares it with a writer)
    /// avoids. An in-memory reader never changes.
    pub fn ensure_unchanged(&self) -> std::result::Result<(), CsrFileChanged> {
        match &self._storage {
            #[cfg(unix)]
            StorageBackend::Mmap(mapped) => mapped.recheck(),
            StorageBackend::Buffer(_) => Ok(()),
        }
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

        // The buffer is a `Vec<u8>`, aligned to 1: write through byte pointers so
        // no typed store assumes more.
        unsafe {
            std::ptr::write_unaligned(buffer.as_mut_ptr() as *mut CsrHeader, header);
        }

        // Copy nodes
        if n > 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    self.nodes.as_ptr() as *const u8,
                    buffer.as_mut_ptr().add(nodes_offset),
                    nodes_size,
                );
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
            std::ptr::copy_nonoverlapping(
                row_offsets.as_ptr() as *const u8,
                buffer.as_mut_ptr().add(row_offsets_offset),
                row_offsets_size,
            );

            if edge_count > 0 {
                std::ptr::copy_nonoverlapping(
                    col_indices.as_ptr() as *const u8,
                    buffer.as_mut_ptr().add(col_indices_offset),
                    col_indices_size,
                );
                std::ptr::copy_nonoverlapping(
                    attributes.as_ptr() as *const u8,
                    buffer.as_mut_ptr().add(attributes_offset),
                    attributes_size,
                );
            }
        }

        buffer
    }

    pub fn write_to_file(self, path: &Path) -> Result<()> {
        let bytes = self.build();
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;
        // Written beside the target and renamed over it, never in place: a
        // reader that has `path` mapped keeps the old, complete inode, where
        // `File::create` on the same file would truncate the pages under it
        // (SIGBUS on its next touch) or rewrite them under its checked bounds.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let name = path
            .file_name()
            .context("CSR graph path has no file name")?
            .to_string_lossy();
        let temp = parent.join(format!(
            ".{name}.tmp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let written = (|| -> Result<()> {
            let mut file = File::create(&temp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            // A replaced file keeps its permissions: the rename would
            // otherwise swap in the temp file's default mode.
            if let Ok(existing) = std::fs::metadata(path) {
                std::fs::set_permissions(&temp, existing.permissions())?;
            }
            std::fs::rename(&temp, path).map_err(|source| {
                anyhow::Error::new(CsrPlaceFailed {
                    path: path.to_path_buf(),
                    source,
                })
            })?;
            Ok(())
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&temp);
            return written;
        }
        // The file is in place. Make the rename itself durable. Not all
        // platforms can open a directory for syncing (Windows cannot), and the
        // data file is already synced, so only a Unix failure is reported, and
        // as what it is: written, with its durability unconfirmed.
        #[cfg(unix)]
        {
            if let Err(source) = File::open(parent).and_then(|dir| dir.sync_all()) {
                return Err(anyhow::Error::new(CsrWrittenUnsynced {
                    path: path.to_path_buf(),
                    source,
                }));
            }
        }
        Ok(())
    }
}

/// The temp file was written but could not be renamed over the target (a
/// directory is there, or the target's directory refused it). Nothing was
/// placed, and the temp file is removed.
#[derive(Debug)]
pub(crate) struct CsrPlaceFailed {
    pub(crate) path: std::path::PathBuf,
    pub(crate) source: std::io::Error,
}

impl std::fmt::Display for CsrPlaceFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cannot place CSR file {}: {}",
            self.path.display(),
            self.source
        )
    }
}

impl std::error::Error for CsrPlaceFailed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// A `.csr` file was written and renamed into place, but syncing its directory
/// failed, so the rename may not survive a crash. The file is there and valid;
/// a caller must not report it as not written.
#[derive(Debug)]
pub(crate) struct CsrWrittenUnsynced {
    pub(crate) path: std::path::PathBuf,
    pub(crate) source: std::io::Error,
}

impl std::fmt::Display for CsrWrittenUnsynced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "CSR file {} was written, but syncing its directory failed ({}), so it may not \
             survive a crash",
            self.path.display(),
            self.source
        )
    }
}

impl std::error::Error for CsrWrittenUnsynced {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

fn align_8(n: usize) -> usize {
    (n + 7) & !7
}
