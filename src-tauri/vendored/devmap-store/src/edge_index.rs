//! Adjacency over one generation's edges, built once and reused.
//!
//! Every graph question the query engine answers — `impact`, `trace`, `deps`,
//! `explore`, `affected_tests` — used to begin by materialising the whole
//! generation: `latest_edges` cloned all 271,543 rows out of the cache, the
//! engine converted each one into a `ResolvedEdge`, the traversal built an
//! adjacency map over all of them, and only then did the walk look at the
//! target. On the ScholarLM corpus that was 92 ms for a question whose answer
//! touches a few dozen edges, against 3 ms for `status`.
//!
//! The cost is not the walk; it is arriving at it. This index moved that work
//! to once per *generation* instead of once per *question* — which fixed the
//! daemon and left the CLI exactly where it was, because a one-shot process
//! asks one question and never gets a second to amortise the arrival against.
//! Measured on this repository's 102,239 edges, cold: **63.1 ms to arrive,
//! 1.5 ms to walk.**
//!
//! So the generation is not materialised at all. A generation's rows are mostly
//! repetition — 102,239 edges naming 17,869 distinct symbols, 1,602 paths, 8
//! kinds and 7 resolution labels — and the row shape paid for that twice, once
//! copying 13 MB of symbol text into `StoredEdge`s and again hashing four of
//! its columns per edge to build the adjacency. What this holds instead is an
//! [`EdgeText`] — each distinct string stored once, ranked in byte order — and
//! six `u32` columns over it, with the adjacency a counting sort into two
//! integer vectors per direction. A `StoredEdge` is built by
//! [`GenerationEdges::stored_edge`], one row at a time, for the edges an answer
//! actually contains.
//!
//! Freshness is by construction. The index is keyed by generation id, so a
//! build that commits a new generation invalidates it by existing; there is no
//! separate invalidation path for a long-lived `devmap mcp` to forget to call.

use std::collections::HashMap;
use std::sync::Arc;

use devmap_analyze::model::AnalysisDisclosure;
use devmap_analyze::traversal::{EdgeView, GraphIndex};
use devmap_extract::model::{confidence_millis, EdgeKind};

use crate::db::StoredEdge;

/// The stored spelling of an edge kind, as `EdgeKind`.
///
/// The one owner of the mapping. It is a `Debug` rendering on the way in
/// (`save_generation` writes `format!("{kind:?}")`), so a second hand-written
/// table anywhere else is a table that can drift from this one; the query
/// crate's `resolved_edge_from_stored` calls through to here rather than
/// keeping its own copy.
///
/// An unknown spelling is an error, never a default: it means the store was
/// written by a binary that knows an edge kind this one does not, and silently
/// dropping such edges would answer "nothing depends on this" from a graph that
/// was only partly read.
pub fn edge_kind_from_stored(kind: &str) -> Result<EdgeKind, UnknownEdgeKind> {
    Ok(match kind {
        "Imports" => EdgeKind::Imports,
        "Calls" => EdgeKind::Calls,
        "Contains" => EdgeKind::Contains,
        "Defines" => EdgeKind::Defines,
        "Instantiates" => EdgeKind::Instantiates,
        "Extends" => EdgeKind::Extends,
        "Implements" => EdgeKind::Implements,
        "SubscribesTo" => EdgeKind::SubscribesTo,
        "HandlesRoute" => EdgeKind::HandlesRoute,
        "WiredTo" => EdgeKind::WiredTo,
        "MemberOf" => EdgeKind::MemberOf,
        "DependsOn" => EdgeKind::DependsOn,
        "TaintFlow" => EdgeKind::TaintFlow,
        "References" => EdgeKind::References,
        other => return Err(UnknownEdgeKind(other.to_string())),
    })
}

/// An edge's evidence tier, and whether it was read or guessed.
pub use devmap_resolve::model::Evidence as EdgeResolution;
/// The evidence tier an edge was built from, as it is spelled in the store.
///
/// `devmap_resolve::model::ResolutionKind`, under the name this crate has
/// always used for it. The column holds the *kind*, not the payload, because
/// the payload is either already in the row (`SameFile`'s target) or is
/// evidence the row cannot carry (`AmbiguousGlobal`'s candidate list). The
/// kind is what the honesty invariants are stated over — its `confidence()` is
/// a function of it alone — and the spelling, the confidence table and the
/// variant set all have one owner there, so nothing in this crate can drift
/// from the resolver.
pub use devmap_resolve::model::ResolutionKind as StoredResolutionKind;
/// Where an edge's resolution kind came from — `devmap_resolve`'s enum. The
/// store only ever produces `Stored` and `Reconstructed`; `Resolver` is the
/// value an edge carries before it is written.
pub use devmap_resolve::model::ResolutionSource;

/// The stored spelling of a resolution kind — [`StoredResolutionKind::label`],
/// through the resolution's own `kind()`. Kept as a function so the write path
/// reads as it always did; the table it used to hold is the resolver's now.
pub fn resolution_kind_label(resolution: &devmap_resolve::model::Resolution) -> &'static str {
    resolution.kind().label()
}

/// How many candidates an `AmbiguousGlobal` resolution weighed, or `None` for
/// every other rung.
///
/// The one owner of what `generation_edges.candidate_total` means. `None` is
/// written as SQL NULL, and NULL is deliberately *not* zero and not one: a
/// resolution that names a single target has no candidate list at all, and
/// storing `1` there would make a certain edge indistinguishable from a
/// one-candidate ambiguity in every aggregate that reads the column.
///
/// This is the denominator resolver memory actually tracks. Since
/// `AMBIGUOUS_FANOUT_CAP` bounds emitted edges but not the candidate list the
/// `Arc<Resolution>` holds, the two stopped being the same number, and every
/// metric derived from the store was counting the wrong one.
pub fn ambiguous_candidate_total(
    resolution: Option<&devmap_resolve::model::Resolution>,
) -> Option<i64> {
    match resolution? {
        devmap_resolve::model::Resolution::AmbiguousGlobal { candidates, .. } => {
            i64::try_from(candidates.len()).ok()
        }
        _ => None,
    }
}

/// Decode a stored resolution kind.
///
/// An unknown spelling is an error, never a default — the same rule
/// [`edge_kind_from_stored`] states: it means the store was written by a binary
/// that knows a tier this one does not, and quietly rounding it to some
/// neighbouring tier would put a confidence claim on an edge whose evidence
/// this binary cannot read.
pub fn resolution_kind_from_stored(
    kind: &str,
) -> Result<StoredResolutionKind, UnknownResolutionKind> {
    StoredResolutionKind::from_label(kind).ok_or_else(|| UnknownResolutionKind(kind.to_string()))
}

/// The resolution an edge row carries, or the reconstruction that stands in for
/// one it does not.
///
/// The fallback is deliberately the *naive* reading — the only one a row
/// without the column supports — and it is labelled
/// [`ResolutionSource::Reconstructed`] so nothing can mistake it for the
/// resolver's own record. It gets `Structural` wrong on purpose-built edges
/// whose endpoints share a file, and `ImportScoped` wrong on every cross-file
/// edge; that is what "this generation did not store its evidence" looks like
/// when it is said out loud instead of guessed over.
pub fn edge_resolution(edge: &StoredEdge) -> Result<EdgeResolution, UnknownResolutionKind> {
    match edge.resolution.as_deref() {
        Some(kind) => Ok(EdgeResolution {
            kind: resolution_kind_from_stored(kind)?,
            source: ResolutionSource::Stored,
        }),
        None => Ok(reconstructed_resolution(edge)),
    }
}

/// The guess a row without the column supports, always labelled as one.
fn reconstructed_resolution(edge: &StoredEdge) -> EdgeResolution {
    EdgeResolution {
        kind: if edge.source_file == edge.target_file {
            StoredResolutionKind::SameFile
        } else {
            StoredResolutionKind::UniqueGlobal
        },
        source: ResolutionSource::Reconstructed,
    }
}

/// A stored edge kind this binary does not know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownEdgeKind(pub String);

/// A stored resolution kind this binary does not know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownResolutionKind(pub String);

impl std::fmt::Display for UnknownResolutionKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "stored generation has unknown resolution kind {:?}",
            self.0
        )
    }
}

impl std::error::Error for UnknownResolutionKind {}

impl std::fmt::Display for UnknownEdgeKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "stored generation has unknown edge kind {:?}",
            self.0
        )
    }
}

impl std::error::Error for UnknownEdgeKind {}

/// Whether `confidence` clears `min_confidence` under the store's rounding.
///
/// The same comparison `Store::latest_edges` applies, and the same one the SQL
/// applies, so an indexed walk and a filtered read cannot disagree about which
/// edges exist. NaN is refused before it reaches here — see
/// `checked_min_confidence`.
pub(crate) fn admits(confidence: f32, min_confidence: f32) -> bool {
    (confidence * 1000.0).round() as i64 >= (min_confidence * 1000.0).round() as i64
}

/// One generation's distinct text, each string stored once and addressed by a
/// rank whose integer order **is** the byte order of the string it stands for.
///
/// A generation's edge rows are mostly repetition: this repository's 102,239
/// edges name 17,869 distinct symbols, 1,602 distinct file paths, 8 distinct
/// edge kinds and 7 distinct resolution labels. Materialising them per row cost
/// six owned `String`s per edge — 13 MB of symbol text copied to answer a
/// question about a few hundred edges — and then hashed all of it again to
/// build the adjacency.
///
/// Ranking by byte order is what makes the read order (R4) an integer
/// comparison: `edge_read_order` compares two path strings and two symbol
/// strings, and after this those four keys are `u32` compares whose result is
/// the same as comparing the text, because the ranks were assigned in the text's
/// own order.
#[derive(Default)]
struct EdgeText {
    /// Distinct symbol text in ascending byte order; a rank indexes this.
    ///
    /// `Arc<str>` rather than `Box<str>` so the rank table and the lookup map
    /// share one allocation per distinct string instead of holding two copies
    /// of it — and so the byte-order ranking permutes pointers rather than
    /// re-allocating 17,869 symbol names. `Arc<str>: Borrow<str>`, so a lookup
    /// still takes a plain `&str` and allocates nothing.
    symbols: Vec<Arc<str>>,
    /// Symbol text to its rank — the only lookup a query needs, so it is the
    /// only map kept past the build.
    symbol_rank: HashMap<Arc<str>, u32>,
    /// Distinct file-path text in ascending byte order.
    files: Vec<Arc<str>>,
    /// Distinct *stored* edge-kind spellings in ascending byte order, and the
    /// [`EdgeKind`] each parses to, in the same order.
    kind_labels: Vec<Arc<str>>,
    kind_values: Vec<EdgeKind>,
    /// Distinct stored resolution labels in ascending byte order — a sort key
    /// like the others since v18, where the read order's last tie-break is the
    /// resolution rather than the emission ordinal. `None` is not a label: it
    /// is [`NO_RESOLUTION`] in the column, so a row written before the column
    /// existed stays distinguishable from one that stored a tier, and it sorts
    /// where `Option::None` sorts.
    resolution_labels: Vec<Arc<str>>,
}

/// The sentinel for `generation_edges.resolution` being SQL NULL.
///
/// Not a label in [`EdgeText::resolution_labels`], because "this generation did
/// not record its evidence" is a different fact from any tier it could have
/// recorded, and `edge_resolution` answers the two differently — one is read,
/// the other is reconstructed and says so.
const NO_RESOLUTION: u32 = u32::MAX;

/// A run-length adjacency: for each rank, the ids of its edges, contiguous.
///
/// Replaces a `HashMap<Box<str>, Vec<u32>>` per direction. The maps hashed
/// every edge's symbol text four times over and allocated a `Box<str>` and a
/// `Vec` per distinct key; this is two integer vectors built by a counting
/// sort, and it hashes nothing. Ids come out ascending within each run because
/// a counting sort over ascending ids is stable, which is the property the
/// generation's edge order depends on (R4).
#[derive(Default)]
struct Adjacency {
    /// `offsets[rank]..offsets[rank + 1]` is the run for `rank`. One longer
    /// than the rank count.
    offsets: Vec<u32>,
    ids: Vec<u32>,
}

impl Adjacency {
    /// Group `ids` by `key[id]`, where every key is a rank below `ranks`.
    fn build(keys: &[u32], ranks: usize) -> Self {
        let mut offsets = vec![0u32; ranks + 1];
        for key in keys {
            offsets[*key as usize + 1] += 1;
        }
        for rank in 0..ranks {
            offsets[rank + 1] += offsets[rank];
        }
        let mut cursor = offsets.clone();
        let mut ids = vec![0u32; keys.len()];
        for (id, key) in keys.iter().enumerate() {
            let slot = &mut cursor[*key as usize];
            ids[*slot as usize] = id as u32;
            *slot += 1;
        }
        Self { offsets, ids }
    }

    fn run(&self, rank: u32) -> &[u32] {
        let start = self.offsets[rank as usize] as usize;
        let end = self.offsets[rank as usize + 1] as usize;
        &self.ids[start..end]
    }

    fn ranks(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }
}

/// The order every reader of a generation's edges sees, as one comparator.
///
/// `confidence DESC, source path, target path, source symbol, target symbol,
/// edge kind` — the key `latest_edges_uncached`'s SQL used to hand to SQLite —
/// and then `resolution`. This order is the final tie-break of every answer
/// derived from a walk (R4), so it has exactly one owner.
///
/// # Why the last key is `resolution` and not the emission ordinal
///
/// It was `ordinal`, the position the resolver emitted the edge at, which SQL
/// had no equivalent of and which made the tail of the order defined instead of
/// arbitrary. v18 cannot supply that: a row's ordinal belongs to the generation
/// that first inserted it, and a row carried across generations keeps it, so an
/// incremental generation and a cold one would order the same edges differently.
///
/// `resolution` is a strictly better key, not a substitute. It is the last
/// column `StoredEdge` carries, so any pair it still leaves tied is a pair whose
/// every read column agrees — two rows a caller cannot tell apart, in either
/// order. And it makes the read order a pure function of the stored tuples
/// rather than of the order they happened to arrive in, which is what lets a
/// generation assembled from carried rows be byte-identical to a cold one.
///
/// It is not hypothetical that the old key was load-bearing: six rows of this
/// repository tie on all six keys above and differ only here — pairs like
/// `tests/unit/test_local_llm_calibration.py -> src/devcouncil/app/config.py`
/// at confidence 1, emitted once as `ImportScoped` and once as `ReceiverType`.
/// Under `ordinal` their order was whichever the resolver reached first.
///
/// Every key but the confidence is an [`EdgeText`] rank, and a rank comparison
/// *is* the byte comparison SQL made: the ranks were assigned in ascending byte
/// order, and SQLite's default collation is BINARY, which is `str`'s byte
/// order. The confidence is the `f64` SQLite stored rather than the `f32`
/// [`StoredEdge`] narrows it to, so no pair that SQL separated can collapse
/// into a tie here.
///
/// `paths.path` is `UNIQUE`, so no two file ranks can stand for the same text;
/// symbols and kind labels are interned by text, so the same holds for them by
/// construction. A rank tie is therefore a text tie, exactly as SQL saw it.
fn edge_read_order(left: &EdgeSortKey, right: &EdgeSortKey) -> std::cmp::Ordering {
    right
        .confidence
        .total_cmp(&left.confidence)
        .then_with(|| left.source_file.cmp(&right.source_file))
        .then_with(|| left.target_file.cmp(&right.target_file))
        .then_with(|| left.source_symbol.cmp(&right.source_symbol))
        .then_with(|| left.target_symbol.cmp(&right.target_symbol))
        .then_with(|| left.kind.cmp(&right.kind))
        .then_with(|| left.resolution.cmp(&right.resolution))
}

/// One row's keys under [`edge_read_order`], gathered so the sort moves 40
/// bytes per row instead of the whole column set.
///
/// Every key is a rank, and every rank table is in ascending byte order, so
/// each `u32` comparison is the string comparison it stands for. `resolution`
/// is the one that needs saying: it is `NO_RESOLUTION_ORDER` for SQL NULL and
/// `rank + 1` otherwise, which is exactly how `Option<String>` orders — `None`
/// before every `Some`, and `Some`s among themselves by their text.
#[derive(Clone, Copy)]
struct EdgeSortKey {
    confidence: f64,
    source_file: u32,
    target_file: u32,
    source_symbol: u32,
    target_symbol: u32,
    kind: u32,
    resolution: u32,
    /// Where the row currently sits in the builder's columns.
    row: u32,
}

/// Where a row with no stored resolution sorts: before every row that has one,
/// because that is where `None` sorts among `Option<String>`s.
const NO_RESOLUTION_ORDER: u32 = 0;

/// Which ids a [`GenerationEdgesBuilder`] hands out.
///
/// Not a flag on `finish`: the two answers are different claims about who owns
/// the generation's edge order, and a bare `bool` at the call site would say
/// neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeOrder {
    /// Ids follow the push order. The caller already holds its rows in the
    /// order it wants ids assigned in — which is what every hand-built index
    /// in a test means, and what a caller reading rows that are already in
    /// read order means.
    AsPushed,
    /// Ids follow the generation's read order (R4), which `finish` establishes
    /// with [`edge_read_order`]. This is what the store means: its scan reads
    /// `generation_edges` unordered, because ordering ~100k rows in SQLite's
    /// sorter costs more than ordering them here.
    ReadOrder,
}

/// Edges on their way into a [`GenerationEdges`].
///
/// Exists so the store can arrive at the adjacency **without materialising the
/// generation**: rows are pushed straight off the SQLite cursor, borrowed, and
/// what is kept per row is six `u32`s and an `f64` rather than six owned
/// `String`s. On this repository's store that is the difference between 1.25
/// billion instructions for a cold `devmap impact` and one bounded by the
/// answer.
pub struct GenerationEdgesBuilder {
    text: EdgeText,
    /// Interning maps, dropped by `finish` — `EdgeText::symbol_rank` is rebuilt
    /// in byte order there, and nothing looks a file or kind up by name after
    /// the build.
    file_rank: HashMap<Arc<str>, u32>,
    kind_rank: HashMap<Arc<str>, u32>,
    resolution_rank: HashMap<Arc<str>, u32>,
    source_symbol: Vec<u32>,
    target_symbol: Vec<u32>,
    source_file: Vec<u32>,
    target_file: Vec<u32>,
    kind: Vec<u32>,
    resolution: Vec<u32>,
    confidence: Vec<f64>,
}

impl GenerationEdgesBuilder {
    pub fn with_capacity(edges: usize) -> Self {
        Self {
            text: EdgeText::default(),
            file_rank: HashMap::new(),
            kind_rank: HashMap::new(),
            resolution_rank: HashMap::new(),
            source_symbol: Vec::with_capacity(edges),
            target_symbol: Vec::with_capacity(edges),
            source_file: Vec::with_capacity(edges),
            target_file: Vec::with_capacity(edges),
            kind: Vec::with_capacity(edges),
            resolution: Vec::with_capacity(edges),
            confidence: Vec::with_capacity(edges),
        }
    }

    /// Intern one file path, returning the rank to push rows with.
    ///
    /// Public because the store interns the `paths` table once — 1,602 rows —
    /// rather than hashing two path strings per edge. A caller that has only
    /// the text may call this per row; it is the same map either way.
    pub fn intern_file(&mut self, path: &str) -> u32 {
        intern(&mut self.file_rank, &mut self.text.files, path)
    }

    /// Intern one edge kind, refusing a spelling this binary does not know.
    ///
    /// The refusal is [`edge_kind_from_stored`]'s and happens once per distinct
    /// spelling rather than once per row, which is the only thing that changed:
    /// a store written by a binary that knows an edge kind this one does not is
    /// still refused rather than half-read.
    fn intern_kind(&mut self, label: &str) -> Result<u32, UnknownEdgeKind> {
        if let Some(rank) = self.kind_rank.get(label) {
            return Ok(*rank);
        }
        let value = edge_kind_from_stored(label)?;
        let rank = self.text.kind_labels.len() as u32;
        let label: Arc<str> = Arc::from(label);
        self.text.kind_labels.push(Arc::clone(&label));
        self.text.kind_values.push(value);
        self.kind_rank.insert(label, rank);
        Ok(rank)
    }

    /// Push one row, with its file paths already interned.
    #[allow(clippy::too_many_arguments)]
    pub fn push_ranked(
        &mut self,
        source_file: u32,
        target_file: u32,
        source_symbol: &str,
        target_symbol: &str,
        edge_kind: &str,
        confidence: f64,
        resolution: Option<&str>,
    ) -> Result<(), UnknownEdgeKind> {
        let kind = self.intern_kind(edge_kind)?;
        let source_symbol = intern(
            &mut self.text.symbol_rank,
            &mut self.text.symbols,
            source_symbol,
        );
        let target_symbol = intern(
            &mut self.text.symbol_rank,
            &mut self.text.symbols,
            target_symbol,
        );
        let resolution = match resolution {
            Some(label) => intern(
                &mut self.resolution_rank,
                &mut self.text.resolution_labels,
                label,
            ),
            None => NO_RESOLUTION,
        };
        self.source_file.push(source_file);
        self.target_file.push(target_file);
        self.source_symbol.push(source_symbol);
        self.target_symbol.push(target_symbol);
        self.kind.push(kind);
        self.resolution.push(resolution);
        self.confidence.push(confidence);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.kind.len()
    }

    pub fn is_empty(&self) -> bool {
        self.kind.is_empty()
    }

    /// Rank every interned table by byte order and remap the columns onto it.
    ///
    /// Interning assigns ranks in first-seen order; [`edge_read_order`] needs
    /// them in byte order, because that is the order the SQL it replaces got
    /// from SQLite's BINARY collation. Sorting the *tables* — 17,869 symbols,
    /// 1,602 paths, 8 kinds on this repository — and remapping the columns is
    /// one sort of the distinct text instead of ~400,000 string comparisons
    /// inside the row sort.
    fn rank_by_bytes(&mut self) {
        fn reorder(names: &mut Vec<Arc<str>>, columns: [&mut Vec<u32>; 2]) {
            let mut order: Vec<u32> = (0..names.len() as u32).collect();
            order
                .sort_unstable_by(|left, right| names[*left as usize].cmp(&names[*right as usize]));
            let mut new_rank = vec![0u32; names.len()];
            for (rank, old) in order.iter().enumerate() {
                new_rank[*old as usize] = rank as u32;
            }
            let sorted: Vec<Arc<str>> = order
                .iter()
                .map(|old| Arc::clone(&names[*old as usize]))
                .collect();
            *names = sorted;
            for column in columns {
                for slot in column.iter_mut() {
                    *slot = new_rank[*slot as usize];
                }
            }
        }

        reorder(
            &mut self.text.files,
            [&mut self.source_file, &mut self.target_file],
        );
        reorder(
            &mut self.text.symbols,
            [&mut self.source_symbol, &mut self.target_symbol],
        );
        // The `NO_RESOLUTION` sentinel is not a rank and must not be remapped
        // as one, so the resolution column is reordered through a guard rather
        // than through `reorder`'s straight lookup.
        {
            let names = &mut self.text.resolution_labels;
            let mut order: Vec<u32> = (0..names.len() as u32).collect();
            order
                .sort_unstable_by(|left, right| names[*left as usize].cmp(&names[*right as usize]));
            let mut new_rank = vec![0u32; names.len()];
            for (rank, old) in order.iter().enumerate() {
                new_rank[*old as usize] = rank as u32;
            }
            *names = order
                .iter()
                .map(|old| Arc::clone(&names[*old as usize]))
                .collect();
            for slot in self.resolution.iter_mut() {
                if *slot != NO_RESOLUTION {
                    *slot = new_rank[*slot as usize];
                }
            }
        }
        // The kind label table carries a parallel `EdgeKind` column, so it is
        // permuted with its own names rather than through `reorder`.
        let mut order: Vec<u32> = (0..self.text.kind_labels.len() as u32).collect();
        order.sort_unstable_by(|left, right| {
            self.text.kind_labels[*left as usize].cmp(&self.text.kind_labels[*right as usize])
        });
        let mut new_rank = vec![0u32; order.len()];
        for (rank, old) in order.iter().enumerate() {
            new_rank[*old as usize] = rank as u32;
        }
        let mut labels = Vec::with_capacity(order.len());
        let mut values = Vec::with_capacity(order.len());
        for old in &order {
            labels.push(Arc::clone(&self.text.kind_labels[*old as usize]));
            values.push(self.text.kind_values[*old as usize]);
        }
        self.text.kind_labels = labels;
        self.text.kind_values = values;
        for slot in self.kind.iter_mut() {
            *slot = new_rank[*slot as usize];
        }
    }

    /// Assign ids and build the adjacency, reconstructing every edge's
    /// evidence tier from the row.
    ///
    /// The only reading a caller that does not hold the generation's
    /// resolution column can support, and every entry says so of itself — see
    /// [`reconstructed_resolution`]. It gets `Structural` wrong on
    /// purpose-built edges whose endpoints share a file, and `ImportScoped`
    /// wrong on every cross-file edge; that is what "this generation did not
    /// store its evidence" looks like when it is said out loud instead of
    /// guessed over.
    ///
    /// `analysis` must describe the same generation as the rows — see
    /// [`GenerationEdges::analysis`]. A hand-built index in a test passes
    /// `None`, which is the truth for one.
    pub fn finish(self, analysis: Option<AnalysisDisclosure>, order: EdgeOrder) -> GenerationEdges {
        self.finish_with(analysis, order, None)
    }

    /// [`Self::finish`], decoding `generation_edges.resolution` instead of
    /// reconstructing it.
    ///
    /// What the store uses, because the store has the column. An unknown
    /// spelling is an error, never a default: it means the generation was
    /// written by a binary that knows a tier this one does not, and quietly
    /// rounding it to some neighbouring tier would put a confidence claim on an
    /// edge whose evidence this binary cannot read. Decoded once per *distinct*
    /// label — seven of them on this repository — rather than once per row.
    pub fn finish_with_stored_evidence(
        self,
        analysis: Option<AnalysisDisclosure>,
        order: EdgeOrder,
    ) -> Result<GenerationEdges, UnknownResolutionKind> {
        // The whole refusal happens here, before anything is built: a label
        // this binary cannot read is not a partially-decoded index.
        let mut kinds = Vec::with_capacity(self.text.resolution_labels.len());
        for label in &self.text.resolution_labels {
            kinds.push(resolution_kind_from_stored(label)?);
        }
        Ok(self.finish_with(analysis, order, Some(kinds)))
    }

    /// `stored_kinds` is `Some` when the caller wants the row's own label read
    /// back, `None` when every tier is to be reconstructed. One body, because
    /// the two answers differ in exactly one expression and everything else —
    /// the ranking, the ordering, the adjacency, the honesty count — has to be
    /// identical or the two paths are two indexes.
    fn finish_with(
        mut self,
        analysis: Option<AnalysisDisclosure>,
        order: EdgeOrder,
        stored_kinds: Option<Vec<StoredResolutionKind>>,
    ) -> GenerationEdges {
        // Ids are `u32`. A generation with more edges than that cannot be
        // addressed, and answering over a silently truncated prefix would be a
        // wrong answer rather than a bounded one, so it is refused by the
        // caller before it gets here; the assert documents the invariant.
        assert!(
            self.len() <= u32::MAX as usize,
            "a generation with more than u32::MAX edges cannot be indexed"
        );
        self.rank_by_bytes();
        // The symbol map is rebuilt over the byte-ordered table: interning
        // filled it with first-seen ranks, which are no longer the ranks the
        // columns hold.
        self.text.symbol_rank = self
            .text
            .symbols
            .iter()
            .enumerate()
            .map(|(rank, name)| (Arc::clone(name), rank as u32))
            .collect();

        if order == EdgeOrder::ReadOrder {
            let mut keys: Vec<EdgeSortKey> = (0..self.len())
                .map(|row| EdgeSortKey {
                    confidence: self.confidence[row],
                    source_file: self.source_file[row],
                    target_file: self.target_file[row],
                    source_symbol: self.source_symbol[row],
                    target_symbol: self.target_symbol[row],
                    kind: self.kind[row],
                    resolution: match self.resolution[row] {
                        NO_RESOLUTION => NO_RESOLUTION_ORDER,
                        rank => rank + 1,
                    },
                    row: row as u32,
                })
                .collect();
            keys.sort_unstable_by(edge_read_order);
            let permutation: Vec<u32> = keys.iter().map(|key| key.row).collect();
            permute(&mut self.source_symbol, &permutation);
            permute(&mut self.target_symbol, &permutation);
            permute(&mut self.source_file, &permutation);
            permute(&mut self.target_file, &permutation);
            permute(&mut self.kind, &permutation);
            permute(&mut self.resolution, &permutation);
            let confidence: Vec<f64> = permutation
                .iter()
                .map(|row| self.confidence[*row as usize])
                .collect();
            self.confidence = confidence;
        }

        // The guess a row without the column supports is always labelled as
        // one. `source_file == target_file` is a rank comparison because
        // `paths.path` is `UNIQUE` and file paths are interned by text here:
        // equal ranks are equal text.
        let reconstructed = |source_file: u32, target_file: u32| EdgeResolution {
            kind: if source_file == target_file {
                StoredResolutionKind::SameFile
            } else {
                StoredResolutionKind::UniqueGlobal
            },
            source: ResolutionSource::Reconstructed,
        };
        let resolutions: Vec<EdgeResolution> = (0..self.len())
            .map(|id| match (&stored_kinds, self.resolution[id]) {
                (Some(kinds), label) if label != NO_RESOLUTION => EdgeResolution {
                    kind: kinds[label as usize],
                    source: ResolutionSource::Stored,
                },
                _ => reconstructed(self.source_file[id], self.target_file[id]),
            })
            .collect();

        // The read-side half of the honesty invariant. On the way in,
        // `ResolvedEdge::resolved` makes `confidence` a function of the
        // resolution; here the two are read back separately and compared, so a
        // row whose confidence no longer matches the evidence it names — a
        // tampered store, a bug in a writer, a migration that touched one
        // column — is counted rather than trusted. Only a *stored* kind can be
        // judged: a reconstructed one is a guess about the row, and a guess
        // cannot convict the row of disagreeing with it. Compared in
        // milliconfidence for the reason `Confidence::to_millis` exists.
        let confidence: Vec<f32> = self.confidence.iter().map(|value| *value as f32).collect();
        let confidence_mismatches = confidence
            .iter()
            .zip(&resolutions)
            .filter(|(value, resolution)| {
                resolution.source == ResolutionSource::Stored
                    && confidence_millis(**value) != resolution.kind.confidence().to_millis()
            })
            .count();

        let symbol_ranks = self.text.symbols.len();
        let file_ranks = self.text.files.len();
        GenerationEdges {
            by_source_symbol: Adjacency::build(&self.source_symbol, symbol_ranks),
            by_target_symbol: Adjacency::build(&self.target_symbol, symbol_ranks),
            by_source_file: Adjacency::build(&self.source_file, file_ranks),
            by_target_file: Adjacency::build(&self.target_file, file_ranks),
            source_symbol: self.source_symbol,
            target_symbol: self.target_symbol,
            source_file: self.source_file,
            target_file: self.target_file,
            kind: self.kind,
            resolution: self.resolution,
            confidence,
            resolutions,
            confidence_mismatches,
            text: self.text,
            analysis,
            empty: Vec::new(),
        }
    }
}

/// Intern `value` into `names`, returning its rank.
///
/// One allocation per *distinct* string: the rank table and the lookup map
/// share the same `Arc<str>`. Interning it twice — once as the name, once as
/// the key — was three copies of every symbol by the time `finish` rebuilt the
/// map over the byte-ordered table, which on a generation with as many symbols
/// as edges is the per-edge cost this whole shape exists to remove.
fn intern(ranks: &mut HashMap<Arc<str>, u32>, names: &mut Vec<Arc<str>>, value: &str) -> u32 {
    if let Some(rank) = ranks.get(value) {
        return *rank;
    }
    let rank = names.len() as u32;
    let name: Arc<str> = Arc::from(value);
    names.push(Arc::clone(&name));
    ranks.insert(name, rank);
    rank
}

/// Rewrite `column` so that slot `i` holds what slot `permutation[i]` held.
fn permute(column: &mut Vec<u32>, permutation: &[u32]) {
    let reordered: Vec<u32> = permutation
        .iter()
        .map(|row| column[*row as usize])
        .collect();
    *column = reordered;
}

/// One generation's edges, with the adjacency a bounded walk needs.
///
/// Edge ids are `0..len`, and every id list this type hands out is ascending —
/// which is to say, in the generation's own stable edge order (`confidence
/// DESC, source path, target path, symbols, kind`). That order is the final
/// tie-break of every answer derived from a walk, so it is part of the contract
/// and not an accident of the container (R4).
///
/// # Columns, not rows
///
/// The edges are held as parallel rank columns over an [`EdgeText`], not as a
/// `Vec<StoredEdge>`. A generation's rows are mostly repetition — 102,239 edges
/// over 17,869 symbols, 1,602 paths and 8 kinds on this repository — and the
/// row shape paid for that repetition twice: once materialising six owned
/// `String`s per edge, and again hashing four of them per edge to build the
/// adjacency. A one-shot `devmap impact` paid all of it to answer a question
/// whose answer is a few hundred edges, and never asked a second question to
/// amortise it against.
///
/// What a caller needs a *row* for — [`Self::stored_edge`] — it still gets, one
/// row at a time, for the edges its answer actually contains.
pub struct GenerationEdges {
    text: EdgeText,
    /// Per edge, in id order: the ranks of its two symbols, its two files, its
    /// kind, and its stored resolution label ([`NO_RESOLUTION`] for SQL NULL).
    source_symbol: Vec<u32>,
    target_symbol: Vec<u32>,
    source_file: Vec<u32>,
    target_file: Vec<u32>,
    kind: Vec<u32>,
    resolution: Vec<u32>,
    confidence: Vec<f32>,
    /// The evidence tier behind each edge, decoded once, in edge-id order.
    ///
    /// Travels with the adjacency for the same reason `analysis` does: a
    /// consumer holding an edge has to be able to ask what it was resolved by
    /// *and* whether that answer was read or reconstructed, and taking the two
    /// from separate reads lets them describe different generations.
    resolutions: Vec<EdgeResolution>,
    /// How many edges with a *stored* kind carry a confidence that kind does
    /// not entitle. Zero on every generation a correct writer produced; the
    /// number is reported, never repaired, because the row is the evidence.
    confidence_mismatches: usize,
    /// How much of the corpus the generation that produced these edges
    /// actually read, travelling with the adjacency rather than beside it.
    ///
    /// A walk over this index is only as complete as the graph it walks, and
    /// the two facts have to come from one generation or the qualification
    /// describes a different snapshot than the answer. Holding it here is what
    /// makes that structural: there is no way to obtain the adjacency without
    /// it. `None` means the disclosure could not be read, which is a distinct
    /// answer from a disclosure saying coverage was complete.
    analysis: Option<AnalysisDisclosure>,
    by_source_symbol: Adjacency,
    by_target_symbol: Adjacency,
    by_source_file: Adjacency,
    by_target_file: Adjacency,
    empty: Vec<u32>,
}

impl std::fmt::Debug for GenerationEdges {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GenerationEdges")
            .field("edges", &self.len())
            .field("symbols", &self.text.symbols.len())
            .field("files", &self.text.files.len())
            .finish()
    }
}

impl GenerationEdges {
    /// Index `edges`.
    ///
    /// Fails on an unknown edge kind, exactly where the old per-request
    /// conversion failed, so a store from a newer binary is refused rather than
    /// half-read.
    ///
    /// Ids follow the order `edges` is already in — see [`EdgeOrder::AsPushed`].
    /// The store does not come through here: it pushes rows straight off its
    /// cursor into a [`GenerationEdgesBuilder`], because materialising the
    /// `StoredEdge`s this takes is the cost the columns exist to remove.
    ///
    /// `analysis` must describe the same generation as `edges`. It is a
    /// parameter rather than something set afterwards so the production caller
    /// cannot build an index and forget it; a hand-built index in a test passes
    /// `None`, which is the truth for one.
    pub fn build(
        edges: Arc<Vec<StoredEdge>>,
        analysis: Option<AnalysisDisclosure>,
    ) -> Result<Self, UnknownEdgeKind> {
        Self::build_with_resolutions(edges, analysis, None)
    }

    /// [`Self::build`] with the generation's decoded resolution column.
    ///
    /// `resolutions` is one entry per edge, in the same order. `None` means the
    /// generation carries no resolution column at all, and every edge's tier is
    /// then reconstructed — which each entry says of itself. A length mismatch
    /// is refused rather than zipped short: a shifted alignment would attach
    /// one edge's evidence to another's, which is worse than having none.
    pub fn build_with_resolutions(
        edges: Arc<Vec<StoredEdge>>,
        analysis: Option<AnalysisDisclosure>,
        resolutions: Option<Vec<EdgeResolution>>,
    ) -> Result<Self, UnknownEdgeKind> {
        if let Some(resolutions) = &resolutions {
            assert_eq!(
                resolutions.len(),
                edges.len(),
                "a resolution column that does not line up with its edges would \
                 attribute one edge's evidence to another"
            );
        }
        let mut builder = GenerationEdgesBuilder::with_capacity(edges.len());
        for edge in edges.iter() {
            let source_file = builder.intern_file(&edge.source_file);
            let target_file = builder.intern_file(&edge.target_file);
            builder.push_ranked(
                source_file,
                target_file,
                &edge.source_symbol,
                &edge.target_symbol,
                &edge.edge_kind,
                edge.confidence as f64,
                edge.resolution.as_deref(),
            )?;
        }
        let mut index = builder.finish(analysis, EdgeOrder::AsPushed);
        // A caller-supplied resolution column overrides what the rows' own
        // labels decode to. It is the same decoding — `Store` reads it from the
        // same column in the same pass — but the parameter exists so a caller
        // that already holds the generation's tiers does not decode them twice,
        // and the length check above is what keeps the two aligned.
        if let Some(resolutions) = resolutions {
            index.resolutions = resolutions;
            index.confidence_mismatches = index.recount_confidence_mismatches();
        }
        Ok(index)
    }

    fn recount_confidence_mismatches(&self) -> usize {
        self.confidence
            .iter()
            .zip(&self.resolutions)
            .filter(|(value, resolution)| {
                resolution.source == ResolutionSource::Stored
                    && confidence_millis(**value) != resolution.kind.confidence().to_millis()
            })
            .count()
    }

    /// The coverage disclosure of the generation these edges came from.
    ///
    /// `None` is "could not be read", not "complete" — see the field.
    pub fn analysis(&self) -> Option<&AnalysisDisclosure> {
        self.analysis.as_ref()
    }

    pub fn len(&self) -> usize {
        self.kind.len()
    }

    pub fn is_empty(&self) -> bool {
        self.kind.is_empty()
    }

    /// The row behind an id, materialised.
    ///
    /// The one place a `StoredEdge` is built, and it is built per *answer*
    /// edge rather than per generation edge. Everything a walk needs — the two
    /// symbols, the two paths, the kind, the confidence — is available without
    /// this ([`Self::source_symbol`] and friends borrow the interned text), so
    /// the rows a query pays for are the rows it reports.
    pub fn stored_edge(&self, id: u32) -> StoredEdge {
        StoredEdge {
            source_file: self.source_file(id).to_string(),
            target_file: self.target_file(id).to_string(),
            source_symbol: self.source_symbol(id).to_string(),
            target_symbol: self.target_symbol(id).to_string(),
            edge_kind: self.kind_label(id).to_string(),
            confidence: self.confidence(id),
            resolution: self.resolution_label(id).map(str::to_string),
        }
    }

    /// Every row of the generation, in read order.
    ///
    /// The whole-generation materialisation, kept for the two callers that
    /// genuinely need every row — `Store::latest_edges` and the whole-set
    /// conversion behind `trace_between` — and named so that adding a third is
    /// a decision rather than an accident.
    pub fn stored_edges(&self) -> Vec<StoredEdge> {
        (0..self.len() as u32)
            .map(|id| self.stored_edge(id))
            .collect()
    }

    pub fn source_symbol(&self, id: u32) -> &str {
        &self.text.symbols[self.source_symbol[id as usize] as usize]
    }

    pub fn target_symbol(&self, id: u32) -> &str {
        &self.text.symbols[self.target_symbol[id as usize] as usize]
    }

    pub fn source_file(&self, id: u32) -> &str {
        &self.text.files[self.source_file[id as usize] as usize]
    }

    pub fn target_file(&self, id: u32) -> &str {
        &self.text.files[self.target_file[id as usize] as usize]
    }

    pub fn confidence(&self, id: u32) -> f32 {
        self.confidence[id as usize]
    }

    pub fn kind(&self, id: u32) -> EdgeKind {
        self.text.kind_values[self.kind[id as usize] as usize]
    }

    /// The edge kind as the store spells it.
    pub fn kind_label(&self, id: u32) -> &str {
        &self.text.kind_labels[self.kind[id as usize] as usize]
    }

    /// `generation_edges.resolution` as stored, or `None` on a row written
    /// before the column existed.
    pub fn resolution_label(&self, id: u32) -> Option<&str> {
        let label = self.resolution[id as usize];
        if label == NO_RESOLUTION {
            None
        } else {
            Some(&self.text.resolution_labels[label as usize])
        }
    }

    /// The evidence tier behind an edge, and whether it was read or guessed.
    pub fn resolution(&self, id: u32) -> EdgeResolution {
        self.resolutions[id as usize]
    }

    /// Stored edges whose confidence contradicts the resolution kind the store
    /// recorded for them. See the field.
    pub fn confidence_mismatches(&self) -> usize {
        self.confidence_mismatches
    }

    /// Whether the confidence floor admits this edge.
    pub fn admits(&self, id: u32, min_confidence: f32) -> bool {
        admits(self.confidence(id), min_confidence)
    }

    /// Distinct symbols on one side, each with the ids of its edges.
    ///
    /// Iterated by the traversal-start matcher, which has to run a predicate
    /// per *distinct symbol* rather than per edge: the corpus this exists for
    /// has 41,276 symbols and 271,543 edges. Symbols with no edge on the asked
    /// side are skipped, as the per-side maps this replaced never held them.
    pub fn symbols(&self, reverse: bool) -> impl Iterator<Item = (&str, &[u32])> {
        let adjacency = if reverse {
            &self.by_target_symbol
        } else {
            &self.by_source_symbol
        };
        (0..adjacency.ranks() as u32).filter_map(move |rank| {
            let ids = adjacency.run(rank);
            if ids.is_empty() {
                None
            } else {
                Some((&*self.text.symbols[rank as usize], ids))
            }
        })
    }

    /// Distinct file paths on one side, each with the ids of its edges.
    pub fn files(&self, reverse: bool) -> impl Iterator<Item = (&str, &[u32])> {
        let adjacency = if reverse {
            &self.by_target_file
        } else {
            &self.by_source_file
        };
        (0..adjacency.ranks() as u32).filter_map(move |rank| {
            let ids = adjacency.run(rank);
            if ids.is_empty() {
                None
            } else {
                Some((&*self.text.files[rank as usize], ids))
            }
        })
    }

    /// Ids of the edges leaving `symbol`, ascending.
    pub fn from_source_symbol(&self, symbol: &str) -> &[u32] {
        match self.text.symbol_rank.get(symbol) {
            Some(rank) => self.by_source_symbol.run(*rank),
            None => &self.empty,
        }
    }

    /// Ids of the edges entering `symbol`, ascending.
    pub fn into_target_symbol(&self, symbol: &str) -> &[u32] {
        match self.text.symbol_rank.get(symbol) {
            Some(rank) => self.by_target_symbol.run(*rank),
            None => &self.empty,
        }
    }

    /// A one-direction view a bounded walk can consume.
    ///
    /// The direction travels with the view, so a walk cannot be given a
    /// direction that disagrees with the adjacency it reads — see
    /// [`GraphIndex::reverse`]. Three words over the shared index, so a caller
    /// that needs both directions holds both for the cost of two pointers.
    pub fn directed(&self, reverse: bool, min_confidence: f32) -> DirectedEdges<'_> {
        DirectedEdges {
            index: self,
            reverse,
            min_confidence,
        }
    }
}
/// One direction of a [`GenerationEdges`], under one confidence floor.
pub struct DirectedEdges<'a> {
    index: &'a GenerationEdges,
    reverse: bool,
    min_confidence: f32,
}

impl DirectedEdges<'_> {
    /// The confidence floor this view was built with.
    pub fn min_confidence(&self) -> f32 {
        self.min_confidence
    }
}

impl GraphIndex for DirectedEdges<'_> {
    fn reverse(&self) -> bool {
        self.reverse
    }

    fn neighbors(&self, node: &str) -> &[u32] {
        if self.reverse {
            self.index.into_target_symbol(node)
        } else {
            self.index.from_source_symbol(node)
        }
    }

    fn edge(&self, id: u32) -> EdgeView<'_> {
        EdgeView {
            source_symbol: self.index.source_symbol(id),
            target_symbol: self.index.target_symbol(id),
            source_file: self.index.source_file(id),
            target_file: self.index.target_file(id),
            kind: self.index.kind(id),
        }
    }

    fn kind_label(&self, id: u32) -> &str {
        self.index.kind_label(id)
    }

    fn admits(&self, id: u32) -> bool {
        self.index.admits(id, self.min_confidence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(source: &str, target: &str, kind: &str, confidence: f32) -> StoredEdge {
        StoredEdge {
            source_file: format!("{source}.py"),
            target_file: format!("{target}.py"),
            source_symbol: source.to_string(),
            target_symbol: target.to_string(),
            edge_kind: kind.to_string(),
            confidence,
            resolution: None,
        }
    }

    #[test]
    fn every_edge_kind_this_binary_writes_round_trips_through_the_parser() {
        // The stored form is a `Debug` rendering, so the parser is only correct
        // as long as it names every variant the writer can emit. Anything new
        // fails here rather than at a user's `impact` call.
        for kind in [
            EdgeKind::Imports,
            EdgeKind::Calls,
            EdgeKind::Contains,
            EdgeKind::Defines,
            EdgeKind::Instantiates,
            EdgeKind::Extends,
            EdgeKind::Implements,
            EdgeKind::SubscribesTo,
            EdgeKind::HandlesRoute,
            EdgeKind::WiredTo,
            EdgeKind::MemberOf,
            EdgeKind::DependsOn,
            EdgeKind::TaintFlow,
            EdgeKind::References,
        ] {
            let stored = format!("{kind:?}");
            assert_eq!(edge_kind_from_stored(&stored), Ok(kind), "{stored}");
        }
    }

    #[test]
    fn an_unknown_kind_is_refused_rather_than_dropped() {
        let error = edge_kind_from_stored("FromTheFuture").expect_err("must refuse");
        assert!(error.to_string().contains("FromTheFuture"));
        let refused =
            GenerationEdges::build(Arc::new(vec![edge("a", "b", "FromTheFuture", 1.0)]), None);
        assert!(refused.is_err(), "an index must not half-read a generation");
    }

    #[test]
    fn id_lists_are_ascending_so_the_graph_order_survives() {
        let rows = vec![
            edge("a", "b", "Calls", 1.0),
            edge("c", "b", "Calls", 0.9),
            edge("a", "d", "Calls", 0.8),
        ];
        let index = GenerationEdges::build(Arc::new(rows), None).expect("index");
        assert_eq!(index.from_source_symbol("a"), &[0, 2]);
        assert_eq!(index.into_target_symbol("b"), &[0, 1]);
        assert_eq!(index.from_source_symbol("nothing"), &[] as &[u32]);
    }

    #[test]
    fn duplicate_and_self_edges_keep_every_id() {
        let rows = vec![
            edge("a", "a", "Calls", 1.0),
            edge("a", "a", "Calls", 1.0),
            edge("a", "a", "Contains", 1.0),
        ];
        let index = GenerationEdges::build(Arc::new(rows), None).expect("index");
        assert_eq!(index.from_source_symbol("a"), &[0, 1, 2]);
        assert_eq!(index.into_target_symbol("a"), &[0, 1, 2]);
    }

    #[test]
    fn the_confidence_floor_is_the_stores_rounding_rule() {
        let rows = vec![edge("a", "b", "Calls", 0.7495)];
        let index = GenerationEdges::build(Arc::new(rows), None).expect("index");
        // Rounds to 750 >= 750: admitted, exactly as `latest_edges` admits it.
        assert!(index.admits(0, 0.75));
        assert!(!index.admits(0, 0.76));
    }
}
