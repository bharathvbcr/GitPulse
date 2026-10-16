//! Read-only commit-graph detector.
//!
//! History walks (topo pages, `log -- <path>`, ahead/behind, `--merged`) are
//! much faster when git has a commit-graph, and path-limited ones again when it
//! carries changed-path Bloom filters. A repository that was cloned fresh or
//! has auto-maintenance disabled may have neither. This module says which.
//!
//! # Zero spawns, by choice
//!
//! The detector reads the files git itself would read (`objects/info/commit-graph`
//! or the `commit-graphs/commit-graph-chain` split chain, plus `HEAD` and the
//! ref it names) and spawns nothing. The alternative, `git commit-graph verify`
//! or `rev-parse HEAD`, costs a process on every storage scan and `verify`
//! rereads the whole graph. Reading files has the opposite cost: it must parse
//! the on-disk format (gitformat-commit-graph), so every layout it does not
//! understand is reported as [`CommitGraphState::Unknown`] rather than guessed.
//!
//! # What it does NOT do
//!
//! It never writes, and it holds no write command: the Health hint
//! (`src/lib/health/commitGraph.ts`) shows the command for the person to run,
//! because writing or refreshing a commit-graph from GitPulse is an undecided
//! owner decision (OD 3).
//! It validates the header, chunk table, required chunk sizes, layer order and
//! hash version, and spot-checks the OID table under its fanout. It does not
//! read the whole file or the trailing checksum, so a graph that is
//! structurally sound but bit-rotted in the commit data reads as present;
//! `git commit-graph verify` is the only full check.
//!
//! It asks the shared config snapshot (`git_reader::config_snapshot`, git's own
//! effective config, cached) for `core.commitGraph` and
//! `extensions.objectFormat`; when that cannot be read the answer is unknown.
//! `fetch.writeCommitGraph` only governs whether fetches refresh a graph, so it
//! does not change what a graph means.

use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Layers read from a split chain before the detector gives up.
const MAX_CHAIN_LAYERS: usize = 64;
/// Chunks a graph file may declare (the format defines under ten).
const MAX_CHUNKS: usize = 32;
/// `packed-refs` is scanned for HEAD's branch; a larger file is not scanned.
const MAX_PACKED_REFS_BYTES: u64 = 64 * 1024 * 1024;
/// `HEAD`, a loose ref and the chain file are tiny; anything bigger is wrong.
const MAX_SMALL_FILE_BYTES: u64 = 64 * 1024;

const CHUNK_OID_FANOUT: [u8; 4] = *b"OIDF";
const CHUNK_OID_LOOKUP: [u8; 4] = *b"OIDL";
const CHUNK_COMMIT_DATA: [u8; 4] = *b"CDAT";
const CHUNK_BASE: [u8; 4] = *b"BASE";
const CHUNK_BLOOM_INDEXES: [u8; 4] = *b"BIDX";
const CHUNK_BLOOM_DATA: [u8; 4] = *b"BDAT";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CommitGraphState {
    /// A graph covers HEAD and carries changed-path Bloom filters.
    Present,
    /// A graph covers HEAD but has no Bloom filters: path-limited history
    /// walks still scan trees.
    NoBloom,
    /// A graph exists and does not contain HEAD, so the newest history is
    /// walked without it.
    Stale,
    /// No graph file or chain exists and nothing suggests one lives elsewhere.
    Missing,
    /// The repository has no commits, so there is nothing to accelerate.
    NoHistory,
    /// The check could not establish an answer. `note` says why; this is never
    /// a claim that the graph is missing.
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CommitGraphLayout {
    #[default]
    None,
    /// `objects/info/commit-graph`.
    Single,
    /// `objects/info/commit-graphs/commit-graph-chain` and its layers.
    Split,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommitGraphReport {
    pub state: CommitGraphState,
    pub layout: CommitGraphLayout,
    /// Files read (1 for a single graph, one per layer for a chain).
    pub layers: u32,
    /// Commits recorded across all layers, when the graph was readable.
    pub commits: Option<u64>,
    /// Whether every layer carries changed-path Bloom filters.
    pub bloom: Option<bool>,
    /// Whether HEAD's commit is in the graph; `None` when HEAD could not be
    /// resolved from files (reftable, unreadable ref).
    pub head_covered: Option<bool>,
    /// Why the state is what it is, in one sentence. Always set for `Unknown`.
    pub note: Option<String>,
}

/// Longest note shown. Notes quote ref names and paths, which a repository
/// controls, so they are bounded and stripped of control characters here, once.
const MAX_NOTE_CHARS: usize = 240;

fn tidy_note(note: &str) -> String {
    note.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_NOTE_CHARS)
        .collect()
}

impl CommitGraphReport {
    fn unknown(layout: CommitGraphLayout, note: impl Into<String>) -> Self {
        Self {
            state: CommitGraphState::Unknown,
            layout,
            note: Some(tidy_note(&note.into())),
            ..Self::default()
        }
    }
}

/// What one graph file told us.
struct GraphFile {
    commits: u64,
    bloom: bool,
    hash_len: usize,
    /// Header byte 7: how many graphs this layer sits on (0 for a base).
    base_count: u8,
    /// The BASE chunk: those graphs' checksums, base first.
    bases: Vec<u8>,
    file: File,
    oid_lookup_offset: u64,
    fanout: [u32; 256],
}

impl GraphFile {
    fn contains(&mut self, oid: &[u8]) -> Result<bool, String> {
        if oid.len() != self.hash_len {
            return Ok(false);
        }
        let first = oid[0] as usize;
        let mut hi = self.fanout[first] as u64;
        let mut lo = if first == 0 {
            0
        } else {
            self.fanout[first - 1] as u64
        };
        let mut buf = vec![0u8; self.hash_len];
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            self.file
                .seek(SeekFrom::Start(
                    self.oid_lookup_offset + mid * self.hash_len as u64,
                ))
                .map_err(|e| e.to_string())?;
            self.file.read_exact(&mut buf).map_err(|e| e.to_string())?;
            // The fanout promises every entry in this range starts with the
            // probed byte; one that does not means the table is not sorted, and
            // a search over it proves nothing.
            if buf[0] as usize != first {
                return Err("the OID table is not sorted under its fanout".to_string());
            }
            match buf.as_slice().cmp(oid) {
                std::cmp::Ordering::Equal => return Ok(true),
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        Ok(false)
    }
}

fn read_exact_at(file: &mut File, offset: u64, len: usize) -> Result<Vec<u8>, String> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

/// Opens one graph file and validates header, chunk table and table bounds.
fn open_graph(path: &Path) -> Result<GraphFile, String> {
    let meta = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let len = meta.len();
    let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let header = read_exact_at(&mut file, 0, 8).map_err(|e| format!("header unreadable: {e}"))?;
    if &header[0..4] != b"CGPH" {
        return Err("not a commit-graph file (bad signature)".to_string());
    }
    if header[4] != 1 {
        return Err(format!("unsupported commit-graph version {}", header[4]));
    }
    let hash_len = match header[5] {
        1 => 20usize,
        2 => 32usize,
        other => return Err(format!("unsupported commit-graph hash version {other}")),
    };
    let chunk_count = header[6] as usize;
    if chunk_count == 0 || chunk_count > MAX_CHUNKS {
        return Err(format!("implausible chunk count {chunk_count}"));
    }
    let table = read_exact_at(&mut file, 8, (chunk_count + 1) * 12)
        .map_err(|e| format!("chunk table unreadable: {e}"))?;
    let mut chunks: Vec<([u8; 4], u64)> = Vec::with_capacity(chunk_count + 1);
    for i in 0..=chunk_count {
        let row = &table[i * 12..(i + 1) * 12];
        let id = [row[0], row[1], row[2], row[3]];
        let offset = u64::from_be_bytes(row[4..12].try_into().expect("8-byte slice"));
        chunks.push((id, offset));
    }
    // Chunk i spans [offset_i, offset_{i+1}); offsets must be monotonic and
    // within the file or the sizes below are meaningless.
    let trailer = hash_len as u64;
    for pair in chunks.windows(2) {
        if pair[1].1 < pair[0].1 {
            return Err("chunk offsets are not ascending".to_string());
        }
    }
    if chunks.last().map(|c| c.1).unwrap_or(0) > len.saturating_sub(trailer) {
        return Err("chunk table points past the end of the file".to_string());
    }
    // The table is `chunk_count` real rows closed by a row whose id is zero;
    // git rejects a zero id anywhere else, a repeated id, and a first chunk
    // that starts inside the header or the table itself.
    let (terminator, real) = chunks.split_last().expect("chunk_count + 1 rows");
    if terminator.0 != [0; 4] {
        return Err("chunk table has no terminating row".to_string());
    }
    for (i, (id, _)) in real.iter().enumerate() {
        if *id == [0; 4] {
            return Err("chunk table has a zero chunk id".to_string());
        }
        if real[..i].iter().any(|(other, _)| other == id) {
            return Err("chunk table repeats a chunk id".to_string());
        }
    }
    if real[0].1 < 8 + (chunk_count as u64 + 1) * 12 {
        return Err("first chunk starts inside the chunk table".to_string());
    }
    let span = |id: [u8; 4]| -> Option<(u64, u64)> {
        chunks
            .windows(2)
            .find(|w| w[0].0 == id)
            .map(|w| (w[0].1, w[1].1 - w[0].1))
    };
    let (fanout_off, fanout_len) =
        span(CHUNK_OID_FANOUT).ok_or_else(|| "missing OID fanout chunk".to_string())?;
    if fanout_len != 1024 {
        return Err("OID fanout chunk has the wrong size".to_string());
    }
    let (lookup_off, lookup_len) =
        span(CHUNK_OID_LOOKUP).ok_or_else(|| "missing OID lookup chunk".to_string())?;
    let raw = read_exact_at(&mut file, fanout_off, 1024)?;
    let mut fanout = [0u32; 256];
    for (i, slot) in fanout.iter_mut().enumerate() {
        *slot = u32::from_be_bytes([raw[4 * i], raw[4 * i + 1], raw[4 * i + 2], raw[4 * i + 3]]);
    }
    if fanout.windows(2).any(|w| w[1] < w[0]) {
        return Err("OID fanout is not monotonic".to_string());
    }
    let commits = u64::from(fanout[255]);
    if commits.saturating_mul(hash_len as u64) != lookup_len {
        return Err("OID lookup size disagrees with the fanout".to_string());
    }
    let (_, cdat_len) =
        span(CHUNK_COMMIT_DATA).ok_or_else(|| "missing commit data chunk".to_string())?;
    if commits.saturating_mul(hash_len as u64 + 16) != cdat_len {
        return Err("commit data size disagrees with the fanout".to_string());
    }
    let (bidx, bdat) = (span(CHUNK_BLOOM_INDEXES), span(CHUNK_BLOOM_DATA));
    if bidx.is_some() != bdat.is_some() {
        return Err("only one of the two Bloom filter chunks is present".to_string());
    }
    if let (Some((idx_off, idx_len)), Some((_, data_len))) = (bidx, bdat) {
        // BIDX is a cumulative end offset per commit and BDAT is a 12 byte
        // header plus exactly that many bytes, so the last entry pins both
        // sizes and shows the two chunks really are what their ids claim.
        if idx_len != commits * 4 || data_len < 12 {
            return Err("Bloom filter chunks have the wrong size".to_string());
        }
        if commits > 0 {
            let last = read_exact_at(&mut file, idx_off + idx_len - 4, 4)?;
            let end = u64::from(u32::from_be_bytes([last[0], last[1], last[2], last[3]]));
            if end + 12 != data_len {
                return Err("Bloom filter index disagrees with the filter data".to_string());
            }
        }
    }
    let base_count = header[7];
    let bases = match span(CHUNK_BASE) {
        Some((off, len)) if len == u64::from(base_count) * hash_len as u64 => {
            read_exact_at(&mut file, off, len as usize)?
        }
        Some(_) => return Err("base graph list has the wrong size".to_string()),
        None if base_count == 0 => Vec::new(),
        None => return Err("a layered graph has no base graph list".to_string()),
    };
    Ok(GraphFile {
        commits,
        bloom: bidx.is_some(),
        hash_len,
        base_count,
        bases,
        file,
        oid_lookup_offset: lookup_off,
        fanout,
    })
}

fn is_hex_hash(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn read_small(path: &Path) -> Result<Option<String>, String> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_file() && m.len() <= MAX_SMALL_FILE_BYTES => fs::read_to_string(path)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display())),
        Ok(m) if m.is_file() => Err(format!("{} is implausibly large", path.display())),
        Ok(_) => Err(format!("{} is not a regular file", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn valid_ref_name(name: &str) -> bool {
    name.starts_with("refs/")
        && !name.contains('\0')
        && !name.contains('\\')
        && name
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..")
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len() / 2)
        .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok())
        .collect()
}

/// Outcome of resolving HEAD from files.
enum Head {
    Commit(Vec<u8>),
    Unborn,
    Unresolved(String),
}

fn resolve_head(git_dir: &Path, common_dir: &Path) -> Head {
    if common_dir.join("reftable").exists() {
        return Head::Unresolved("the repository uses reftable refs".to_string());
    }
    let head = match read_small(&git_dir.join("HEAD")) {
        Ok(Some(text)) => text,
        Ok(None) => return Head::Unresolved("HEAD is missing".to_string()),
        Err(e) => return Head::Unresolved(e),
    };
    let head = head.trim();
    let mut name = match head.strip_prefix("ref:") {
        Some(rest) => rest.trim().to_string(),
        None => {
            return match decode_hex(head) {
                Some(oid) if is_hex_hash(head) => Head::Commit(oid),
                _ => Head::Unresolved("HEAD is neither a ref nor an object id".to_string()),
            }
        }
    };
    // Symbolic refs may chain; git itself stops at five hops.
    for _ in 0..5 {
        if !valid_ref_name(&name) {
            return Head::Unresolved(format!("HEAD names an unusable ref `{name}`"));
        }
        // Per-worktree refs live in the private dir, the rest in the common one.
        let loose = [git_dir, common_dir]
            .iter()
            .map(|dir| dir.join(&name))
            .find(|p| p.exists());
        if let Some(path) = loose {
            match read_small(&path) {
                Ok(Some(text)) => {
                    let text = text.trim();
                    if let Some(next) = text.strip_prefix("ref:") {
                        name = next.trim().to_string();
                        continue;
                    }
                    return match decode_hex(text) {
                        Some(oid) if is_hex_hash(text) => Head::Commit(oid),
                        _ => Head::Unresolved(format!("ref `{name}` is not an object id")),
                    };
                }
                Ok(None) => {}
                Err(e) => return Head::Unresolved(e),
            }
        }
        return match packed_ref(common_dir, &name) {
            Ok(Some(oid)) => Head::Commit(oid),
            Ok(None) => Head::Unborn,
            Err(e) => Head::Unresolved(e),
        };
    }
    Head::Unresolved("HEAD's symbolic ref chain is too long".to_string())
}

fn packed_ref(common_dir: &Path, name: &str) -> Result<Option<Vec<u8>>, String> {
    let path = common_dir.join("packed-refs");
    let meta = match fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("packed-refs: {e}")),
    };
    if !meta.is_file() || meta.len() > MAX_PACKED_REFS_BYTES {
        return Err("packed-refs is not a scannable regular file".to_string());
    }
    let reader = BufReader::new(File::open(&path).map_err(|e| format!("packed-refs: {e}"))?);
    for line in reader.lines() {
        let line = line.map_err(|e| format!("packed-refs: {e}"))?;
        if let Some((oid, refname)) = line.split_once(' ') {
            if refname == name && is_hex_hash(oid) {
                return Ok(decode_hex(oid));
            }
        }
    }
    Ok(None)
}

/// Layout, the files to read in chain order, and the chain's own checksums
/// (empty for a single graph).
type GraphPaths = (CommitGraphLayout, Vec<PathBuf>, Vec<Vec<u8>>);

/// Graph files to read, in chain order, or `None` when no graph exists.
fn graph_paths(info: &Path) -> Result<Option<GraphPaths>, (CommitGraphLayout, String)> {
    let chain_dir = info.join("commit-graphs");
    let chain_file = chain_dir.join("commit-graph-chain");
    match read_small(&chain_file) {
        Ok(Some(text)) => {
            // Git reads one hash and one newline per layer: a carriage return,
            // a blank line or padding is a chain it rejects.
            let hashes: Vec<&str> = text
                .strip_suffix('\n')
                .unwrap_or(&text)
                .split('\n')
                .collect();
            if text.is_empty() {
                return Err((
                    CommitGraphLayout::Split,
                    "the commit-graph chain file is empty".to_string(),
                ));
            }
            if hashes.len() > MAX_CHAIN_LAYERS {
                return Err((
                    CommitGraphLayout::Split,
                    format!("the commit-graph chain has more than {MAX_CHAIN_LAYERS} layers"),
                ));
            }
            let mut paths = Vec::with_capacity(hashes.len());
            let mut raw = Vec::with_capacity(hashes.len());
            for hash in hashes {
                let decoded = match decode_hex(hash) {
                    Some(bytes) if is_hex_hash(hash) => bytes,
                    _ => {
                        return Err((
                            CommitGraphLayout::Split,
                            "the commit-graph chain file lists a malformed hash".to_string(),
                        ))
                    }
                };
                if raw.contains(&decoded) {
                    return Err((
                        CommitGraphLayout::Split,
                        "the commit-graph chain file lists a layer twice".to_string(),
                    ));
                }
                raw.push(decoded);
                paths.push(chain_dir.join(format!("graph-{hash}.graph")));
            }
            return Ok(Some((CommitGraphLayout::Split, paths, raw)));
        }
        Ok(None) => {}
        Err(e) => return Err((CommitGraphLayout::Split, e)),
    }
    let single = info.join("commit-graph");
    match fs::symlink_metadata(&single) {
        Ok(_) => Ok(Some((CommitGraphLayout::Single, vec![single], Vec::new()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err((CommitGraphLayout::Single, format!("commit-graph: {e}"))),
    }
}

/// The layers must be one hash version matching the repository's object
/// format, and a chain must stack in the order its file lists: layer `i` sits
/// on exactly the `i` layers before it. Anything else is a graph git rejects.
fn check_layers(graphs: &[GraphFile], chain: &[Vec<u8>], sha256: bool) -> Result<(), String> {
    let want = if sha256 { 32 } else { 20 };
    for (i, g) in graphs.iter().enumerate() {
        if g.hash_len != want {
            return Err("the graph's hash version differs from the repository's".to_string());
        }
        if usize::from(g.base_count) != i {
            return Err("a layer does not sit on the layers before it".to_string());
        }
        if chain.is_empty() {
            continue;
        }
        if chain[i].len() != g.hash_len {
            return Err("a chain hash is the wrong width for its layer".to_string());
        }
        if g.bases != chain[..i].concat() {
            return Err("a layer's base list disagrees with the chain order".to_string());
        }
    }
    Ok(())
}

/// Git's boolean false spellings. An empty value is read as false too: the
/// snapshot prints a key with no `=` as empty, so the two cannot be told apart,
/// and "could not tell" must land on unknown, not on a verdict.
fn config_value_is_false(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "false" | "no" | "off" | "0" | ""
    )
}

/// Inspect the commit-graph of the repository at `repo` (the path git config is
/// read from) whose private git dir is `git_dir` and whose common dir (objects,
/// packed-refs) is `common_dir`. For a linked worktree the two differ and the
/// graph lives in the common dir.
pub fn inspect(repo: &Path, git_dir: &Path, common_dir: &Path) -> CommitGraphReport {
    let objects = common_dir.join("objects");
    let info = objects.join("info");
    let has_alternates = objects.join("info").join("alternates").exists();
    // Git ignores the commit-graph in a shallow repository, so neither the
    // absence nor the presence of one says anything useful about speed.
    if common_dir.join("shallow").exists() {
        return CommitGraphReport::unknown(
            CommitGraphLayout::None,
            "this is a shallow repository; git does not use a commit-graph there",
        );
    }
    // Grafts and replace refs rewrite history under the graph, so git turns it
    // off rather than serve a parent list that no longer matches.
    let has_replace_refs = fs::read_dir(common_dir.join("refs").join("replace"))
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false);
    if common_dir.join("info").join("grafts").exists() || has_replace_refs {
        return CommitGraphReport::unknown(
            CommitGraphLayout::None,
            "this repository has grafts or replace refs; git does not use a commit-graph there",
        );
    }

    // `core.commitGraph=false` makes git ignore the files below, so neither a
    // verdict on them nor a "write one" hint is true.
    let config = match crate::engine::git_reader::config_snapshot(repo) {
        Ok(config) => config,
        Err(why) => {
            return CommitGraphReport::unknown(
                CommitGraphLayout::None,
                format!("could not read git config, which may switch the graph off: {why}"),
            )
        }
    };
    if config
        .get("core.commitgraph")
        .is_some_and(|v| config_value_is_false(v))
    {
        return CommitGraphReport::unknown(
            CommitGraphLayout::None,
            "core.commitGraph is false, so git ignores the commit-graph",
        );
    }

    let (layout, paths, chain_hashes) = match graph_paths(&info) {
        Ok(Some(found)) => found,
        Ok(None) => {
            if has_alternates {
                return CommitGraphReport::unknown(
                    CommitGraphLayout::None,
                    "the repository borrows objects through alternates, which may hold the commit-graph",
                );
            }
            return match resolve_head(git_dir, common_dir) {
                Head::Unborn => CommitGraphReport {
                    state: CommitGraphState::NoHistory,
                    note: Some("the repository has no commits yet".to_string()),
                    ..CommitGraphReport::default()
                },
                _ => CommitGraphReport {
                    state: CommitGraphState::Missing,
                    note: Some(
                        "neither objects/info/commit-graph nor a commit-graph chain exists"
                            .to_string(),
                    ),
                    ..CommitGraphReport::default()
                },
            };
        }
        Err((layout, reason)) => {
            return CommitGraphReport::unknown(layout, format!("could not read it: {reason}"))
        }
    };

    let mut graphs = Vec::with_capacity(paths.len());
    for path in &paths {
        match open_graph(path) {
            Ok(g) => graphs.push(g),
            Err(reason) => {
                return CommitGraphReport::unknown(layout, format!("could not read it: {reason}"))
            }
        }
    }
    let sha256 = config
        .get("extensions.objectformat")
        .is_some_and(|v| v.eq_ignore_ascii_case("sha256"));
    if let Err(why) = check_layers(&graphs, &chain_hashes, sha256) {
        return CommitGraphReport::unknown(layout, format!("could not read it: {why}"));
    }
    let commits: u64 = graphs.iter().map(|g| g.commits).sum();
    let bloom = graphs.iter().all(|g| g.bloom);
    let mut report = CommitGraphReport {
        layout,
        layers: graphs.len() as u32,
        commits: Some(commits),
        bloom: Some(bloom),
        ..CommitGraphReport::default()
    };

    let head = resolve_head(git_dir, common_dir);
    let head_covered = match &head {
        Head::Commit(oid) => {
            if graphs.iter().any(|g| g.hash_len != oid.len()) {
                return CommitGraphReport::unknown(
                    layout,
                    "HEAD's object id is a different hash version than the graph's",
                );
            }
            let mut covered = false;
            for g in &mut graphs {
                match g.contains(oid) {
                    Ok(true) => {
                        covered = true;
                        break;
                    }
                    Ok(false) => {}
                    Err(e) => {
                        return CommitGraphReport::unknown(
                            layout,
                            format!("could not search it: {e}"),
                        )
                    }
                }
            }
            Some(covered)
        }
        Head::Unborn => {
            report.state = CommitGraphState::NoHistory;
            report.note = Some("the repository has no commits yet".to_string());
            return report;
        }
        Head::Unresolved(_) => None,
    };
    report.head_covered = head_covered;

    match (head_covered, bloom) {
        (Some(false), _) if has_alternates => {
            return CommitGraphReport::unknown(
                layout,
                "HEAD is not in the local commit-graph, but alternates may hold the rest",
            )
        }
        (Some(false), _) => {
            report.state = CommitGraphState::Stale;
            report.note = Some("HEAD is newer than the commit-graph".to_string());
        }
        (Some(true), true) => report.state = CommitGraphState::Present,
        (Some(true), false) => {
            report.state = CommitGraphState::NoBloom;
            report.note = Some("the commit-graph has no changed-path Bloom filters".to_string());
        }
        (None, false) => {
            report.state = CommitGraphState::NoBloom;
            report.note = Some(
                "the commit-graph has no changed-path Bloom filters; HEAD could not be checked"
                    .to_string(),
            );
        }
        (None, true) => {
            let why = match head {
                Head::Unresolved(why) => why,
                _ => String::new(),
            };
            return CommitGraphReport {
                note: Some(tidy_note(&format!(
                    "HEAD could not be resolved without git: {why}"
                ))),
                state: CommitGraphState::Unknown,
                ..report
            };
        }
    }
    report
}
