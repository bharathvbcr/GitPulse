//! How a change and a symptom relate through the code graph — both ways
//! round.
//!
//! Two questions, one machinery:
//!
//! - [`suspects`] runs **backwards**: a symptom names a symbol, and the
//!   commits that could have caused it are the ones that touched what that
//!   symbol transitively *calls*.
//! - [`blast`] runs **forwards**: a change names lines, and what it affects is
//!   everything that transitively *calls* the symbols those lines sit in.
//!
//! They share the blob identity, the span-to-line arithmetic and the rule that
//! a check which could not run never reports what a check that passed reports.
//! They differ in exactly one thing — the direction of the walk — which is why
//! [`CodeGraph`] exposes it as two named methods rather than one with a flag.
//!
//! # The backwards direction
//!
//! The naive shape of this question is "blame the file and read the names off
//! the gutter". That answers a different question — *who last touched these
//! lines* — and blame is a poor witness for it: a reformat, a rename, a lint
//! sweep or a merge re-attributes whole regions to whoever performed them. The
//! last toucher is very often a bystander.
//!
//! So the query runs the other way round. The symptom names a symbol; the code
//! graph names everything that symbol transitively *depends on* (the *cone*);
//! and only the byte spans of the symbols in that cone are blamed. Most of a
//! file is irrelevant to any given failure, and blaming it is noise that
//! outweighs the signal.
//!
//! The direction matters and is easy to get backwards. A symptom is caused by
//! its own body or by something it calls — never by its callers, which sit
//! *downstream* of the failure. Walking the inbound blast radius instead
//! produces a cone containing the symptom and nothing else, which is a silent
//! empty answer rather than a loud wrong one.
//!
//! # The join is the hard part
//!
//! A graph records symbol extents as **byte offsets**. `git blame` answers in
//! **line numbers**. Converting between them is only valid against *the exact
//! bytes the spans were recorded from*, and nothing in either representation
//! says which bytes those were. Hand a line index built from blob B to a span
//! recorded against blob A and the arithmetic does not fail — it returns a
//! plausible, ordered, wrong line range, and every symbol attribution built on
//! it is wrong in a way no consumer can detect.
//!
//! [`BlobIdentity`] exists so that cannot happen quietly. Every span set and
//! every blame result carries the blob it was computed against, and
//! [`join::attribute`] refuses a mismatch instead of clamping past it.
//!
//! # A check that could not run is not a check that passed
//!
//! There are several distinct ways this analysis can fail to produce an
//! answer — the graph is unbuilt, blame was refused, the window overran its
//! cap, the blob moved underneath it. If they all collapse into an empty
//! suspect list, the tool reports "nothing to see here" when it means "I did
//! not look". [`Unavailable`] keeps them apart and [`SuspectReport::complete`]
//! is false whenever any of them fired.

use serde::{Deserialize, Serialize};

pub mod blame;
pub mod blast;
pub mod change;
pub mod history;
pub mod join;
pub mod rank;
pub mod suspects;

pub use blame::{BlameLine, BlameRefusal, FileBlame};
pub use blast::{
    blast, AffectedTestFile, BlastReport, ImpactedFile, ImpactedModule, ImpactedSymbol, Owner,
    SeedSymbol, TestSignal, UnattributedChange, UnattributedReason, DEFAULT_BLAST_DEPTH,
};
pub use change::{ChangeRefusal, ChangeSet, ChangeStatus, ChangedRange, FileChange};
pub use join::{attribute, Attribution, SymbolBlame};
pub use rank::{rank, EvidenceClass, RankInputs, Suspect, TouchedSymbol};
pub use suspects::{suspects, DEFAULT_CONE_DEPTH};

/// The identity of the bytes a span set or a blame result was computed against.
///
/// Two spans are comparable only when their identities are equal. This is the
/// whole reason the type exists: an offset is meaningless without the content
/// it indexes into, and every silent misattribution this crate can produce
/// begins with someone assuming two offset sets share a basis when they do not.
///
/// `Worktree` is deliberately *not* equal to itself across reads. A dirty file
/// can change between the graph build and the blame, so a worktree basis is
/// only ever trusted within one read, and [`BlobIdentity::comparable_with`]
/// refuses to pair two of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlobIdentity {
    /// A git object id — the only identity that is stable enough to compare
    /// across processes and across time.
    Blob(String),
    /// Content read from the working tree, with a digest of what was read.
    /// Comparable with a `Blob` of the same digest, never with another
    /// `Worktree`.
    Worktree { digest: String },
    /// The basis was not recorded. Never comparable with anything, including
    /// another `Unknown` — an unrecorded basis is not evidence that two things
    /// share one.
    Unknown,
}

impl BlobIdentity {
    /// Whether spans taken against `self` may be read with a line index built
    /// from `other`.
    ///
    /// Conservative by construction: anything not provably the same content is
    /// refused. Losing an answer costs a re-index; accepting a wrong one costs
    /// a confident, unfalsifiable attribution.
    pub fn comparable_with(&self, other: &BlobIdentity) -> bool {
        match (self, other) {
            (BlobIdentity::Blob(left), BlobIdentity::Blob(right)) => left == right,
            (BlobIdentity::Blob(blob), BlobIdentity::Worktree { digest })
            | (BlobIdentity::Worktree { digest }, BlobIdentity::Blob(blob)) => blob == digest,
            // Two worktree reads are two different instants. Nothing says the
            // file did not change between them, so nothing here may assume it.
            (BlobIdentity::Worktree { .. }, BlobIdentity::Worktree { .. }) => false,
            (BlobIdentity::Unknown, _) | (_, BlobIdentity::Unknown) => false,
        }
    }

    /// How to name this basis in a refusal.
    pub fn describe(&self) -> String {
        match self {
            BlobIdentity::Blob(oid) => format!("blob {oid}"),
            BlobIdentity::Worktree { digest } => format!("worktree digest {digest}"),
            BlobIdentity::Unknown => "an unrecorded basis".to_string(),
        }
    }
}

/// One reason a part of the analysis could not run.
///
/// Every variant names what was being attempted and why it stopped, because
/// the whole point of the type is that a caller can tell these apart from
/// "there are no suspects".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Unavailable {
    /// The graph answered, but said it holds nothing — an unbuilt or empty
    /// index answers every question with silence.
    GraphEmpty { reason: String },
    /// The symptom named no symbol the graph knows.
    SymptomNotFound { symptom: String },
    /// `git` could not be run at all.
    GitUnavailable { reason: String },
    /// Blame was refused for one file, with the reason it was refused for.
    BlameRefused { path: String, reason: String },
    /// The commit window hit its cap, so the commits considered are a suffix
    /// of the window rather than the whole of it.
    WindowCapped { considered: usize, cap: usize },
    /// The cone spanned more files than one analysis will blame, so some of
    /// its symbols were never examined.
    ///
    /// Distinct from `WindowCapped`, which it was briefly folded into. Both
    /// are "a cap was hit", but they bound different things and a reader
    /// deciding whether to rerun with a narrower question needs to know which:
    /// one is fixed by moving `--since` forward, the other by narrowing the
    /// symptom.
    FilesCapped {
        blamed: usize,
        in_cone: usize,
        cap: usize,
    },
    /// A span set and a line index did not share a basis, so the symbols in
    /// that file could not be attributed to lines.
    BlobMismatch {
        path: String,
        spans_taken_against: String,
        lines_built_from: String,
    },
    /// The cone walk stopped at its depth cap, so the set of symbols that can
    /// reach the symptom is a lower bound.
    ConeIncomplete { depth: u32, reached: usize },

    // ---- the forward direction ([`blast`]) ----
    /// The change itself could not be read, so there is nothing to start from.
    ChangeUnreadable { reason: String },
    /// Some of the change's lines landed in no symbol, so the walk started
    /// from fewer places than the change touched.
    ///
    /// This is the forward direction's most important refusal and the easiest
    /// one to have omitted. A change to a module-level constant, an import or
    /// a macro invocation lands here; without this the report would show an
    /// empty blast radius and read as "this change affects nothing", which is
    /// a confident wrong answer rather than a cautious one. Each range is
    /// listed individually in [`blast::BlastReport::unattributed`] with its
    /// own reason; this is the one-line summary that flips `complete`.
    ChangeUnattributed { ranges: usize, lines: u64 },
    /// The change spanned more files than one analysis reads.
    ChangedFilesCapped { considered: usize, cap: usize },
    /// The change touched more symbols than one walk is seeded with, so the
    /// radius is that of the most-changed symbols rather than of all of them.
    SeedsCapped {
        seeded: usize,
        touched: usize,
        cap: usize,
    },
    /// The inbound walk stopped at a cap, so the impacted set is a lower
    /// bound.
    ImpactIncomplete { depth: u32, reached: usize },
    /// The affected-test walk was trimmed, so the test list is a lower bound.
    AffectedTestsIncomplete { found: usize },
    /// Recent owners of the changed paths could not be read — git missing,
    /// not a repository, a deadline, a commit cap, or an empty author. The
    /// owners list is then a lower bound (possibly empty), never evidence that
    /// nobody owns the change.
    OwnersUnavailable { reason: String },
}

impl Unavailable {
    /// A one-line rendering for a human-facing report.
    pub fn describe(&self) -> String {
        match self {
            Unavailable::GraphEmpty { reason } => {
                format!("the code graph answered nothing: {reason}")
            }
            Unavailable::SymptomNotFound { symptom } => {
                format!("`{symptom}` matched no symbol in the graph")
            }
            Unavailable::GitUnavailable { reason } => format!("git could not be run: {reason}"),
            Unavailable::BlameRefused { path, reason } => {
                format!("blame refused for {path}: {reason}")
            }
            Unavailable::WindowCapped { considered, cap } => {
                format!("the commit window hit its cap: {considered} commits considered, cap {cap}")
            }
            Unavailable::FilesCapped {
                blamed,
                in_cone,
                cap,
            } => format!(
                "the cone spans {in_cone} file(s) but only {blamed} were blamed (cap {cap}); \
                 narrow the symptom to examine the rest"
            ),
            Unavailable::BlobMismatch {
                path,
                spans_taken_against,
                lines_built_from,
            } => format!(
                "{path}: spans were taken against {spans_taken_against} but the lines came from \
                 {lines_built_from}, so no line can be attributed to a symbol"
            ),
            Unavailable::ConeIncomplete { depth, reached } => format!(
                "the cone walk stopped at depth {depth} having reached {reached} symbols; the \
                 suspect list is a lower bound"
            ),
            Unavailable::ChangeUnreadable { reason } => {
                format!("the change could not be read: {reason}")
            }
            Unavailable::ChangeUnattributed { ranges, lines } => format!(
                "{lines} changed line(s) in {ranges} range(s) landed in no symbol the graph \
                 declares — module-level code has no inbound edges to walk, so the impact \
                 below is a lower bound"
            ),
            Unavailable::ChangedFilesCapped { considered, cap } => format!(
                "the change spans more than {cap} files ({considered} seen); only the first \
                 {cap} were read"
            ),
            Unavailable::SeedsCapped {
                seeded,
                touched,
                cap,
            } => format!(
                "the change touched {touched} symbol(s) but only the {seeded} most-changed \
                 seeded the walk (cap {cap}); narrow the revision range to cover the rest"
            ),
            Unavailable::ImpactIncomplete { depth, reached } => format!(
                "the inbound walk stopped at depth {depth} having reached {reached} symbols; \
                 the impacted set is a lower bound"
            ),
            Unavailable::AffectedTestsIncomplete { found } => format!(
                "the affected-test walk was trimmed at {found} test file(s); the test list is \
                 a lower bound"
            ),
            Unavailable::OwnersUnavailable { reason } => {
                format!("owners of the changed paths could not be fully read: {reason}")
            }
        }
    }
}

/// A symbol as the code graph knows it.
///
/// `span_start`/`span_end` are byte offsets into `basis` — never line numbers,
/// and never offsets into anything else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphSymbol {
    pub qualified_name: String,
    pub file_path: String,
    pub span_start: usize,
    pub span_end: usize,
    /// The body signature the graph recorded, when it recorded one. Two
    /// generations whose signatures agree had the same body, whatever moved
    /// around it — which is what separates a real edit from a reformat.
    pub body_exact: Option<i64>,
}

/// What this crate needs from a code graph, and nothing more.
///
/// A trait rather than a dependency on the store for three reasons: this crate
/// then needs no parsing frontend and cannot rot in the `parse`-off
/// configuration; the ranking can be tested against a graph built by hand in a
/// test rather than against a fixture repository; and an embedder that already
/// has its own index can answer these three questions without adopting a
/// second one.
pub trait CodeGraph {
    /// Symbols declared in `file`, with the basis their spans were taken
    /// against.
    ///
    /// **Symbols, not the file.** An index that also holds a node for the file
    /// itself must not return it here. Such a node spans the whole file, so it
    /// contains every line of every change — and [`blast`] decides whether a
    /// changed line landed in *any* symbol by exactly this overlap. Include
    /// the file node and that question can only ever be answered yes: a
    /// changed import, a changed top-level constant and a changed license
    /// header are all credited to "the file", and the report claims to have
    /// placed lines it has not placed. The whole
    /// [`Unavailable::ChangeUnattributed`] mechanism is disarmed by one extra
    /// row.
    fn symbols_in(&self, file: &str) -> (Vec<GraphSymbol>, BlobIdentity);

    /// Symbols `symbol` can reach, nearest first, walking **outbound** call
    /// edges to `depth` — its transitive dependencies, plus itself at distance
    /// zero.
    ///
    /// Outbound, because a symptom is caused by what it calls. An
    /// implementation that walks inbound edges here answers "what would break
    /// if this changed", which is a different and useless question for this
    /// purpose: it returns the symptom alone and the report reads as "no
    /// commit touched anything relevant".
    ///
    /// The returned flag is `true` when the walk was stopped by a cap — a
    /// lower bound, not a complete answer.
    fn cone(&self, symbol: &str, depth: u32) -> (Vec<ConeEntry>, bool);

    /// Symbols that can reach `seeds`, walking **inbound** call edges to
    /// `depth` — everything that would be affected if the seeds changed.
    ///
    /// The exact opposite of [`CodeGraph::cone`], and deliberately a separate
    /// method rather than a direction flag on one. The two questions look
    /// symmetric and are routinely confused — this crate shipped `cone` with
    /// the inbound walk once, which returns the seed and nothing else and
    /// reads as a confident "no commit touched anything relevant". A boolean
    /// parameter would have made that a one-character mistake at every call
    /// site instead of a one-time one in an implementation; two named methods,
    /// each with its direction in its contract, cannot be passed the wrong
    /// way round.
    ///
    /// Seeds are included at distance zero. The returned flag is `true` when
    /// the walk was stopped by a cap or a budget, which makes the answer a
    /// lower bound.
    ///
    /// Required rather than defaulted: a default returning an empty list would
    /// let a graph that cannot answer this produce a report that reads exactly
    /// like a change affecting nothing.
    fn impacted(&self, seeds: &[String], depth: u32) -> (Vec<ConeEntry>, bool);

    /// Test files reachable from `seeds` through the same **inbound** walk.
    ///
    /// Separate from [`CodeGraph::impacted`] rather than filtered out of it,
    /// because a store that already computes this applies its own ranking and
    /// its own budget to it, and re-deriving the list from a budget-trimmed
    /// symbol set would silently drop the tests whose band the budget cut.
    ///
    /// The returned flag is `true` when the list is a lower bound.
    fn affected_tests(&self, seeds: &[String], depth: u32) -> (Vec<blast::AffectedTestFile>, bool);

    /// Resolve a symptom — a symbol name, a qualified name, or a file path —
    /// to the qualified names it could mean.
    fn resolve_symptom(&self, symptom: &str) -> Vec<String>;

    /// Whether `commit` changed `symbol`'s body, as opposed to moving it.
    ///
    /// `None` — the default — means *the question was not asked*, and is the
    /// honest answer for a graph that retains too few generations to compare
    /// body signatures across a commit. It is deliberately not `Some(false)`:
    /// "this commit did not change the logic" and "we cannot tell whether this
    /// commit changed the logic" support opposite conclusions, and a graph that
    /// cannot answer must not be able to exonerate a commit by silence.
    ///
    /// Blame is already run with `-w -M`, so whitespace and intra-file moves
    /// do not re-attribute lines at all. This method is the stronger,
    /// structural version of the same question, for a graph that can answer it.
    fn body_changed(&self, symbol: &str, commit: &str) -> Option<bool> {
        let _ = (symbol, commit);
        None
    }
}

/// One symbol in the cone, with how far it sits from the symptom.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConeEntry {
    pub qualified_name: String,
    pub file_path: String,
    /// Call edges from the symptom down to this symbol. Zero is the symptom
    /// itself, and a change to its own body is the most suspicious thing
    /// there is.
    pub distance: u32,
}

/// The answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SuspectReport {
    pub symptom: String,
    /// Ranked most-suspect first.
    pub suspects: Vec<Suspect>,
    /// How many symbols the cone held, and how many of them were blamed.
    pub cone_size: usize,
    pub blamed_symbols: usize,
    /// Everything that could not be done. Non-empty means the suspect list is
    /// a lower bound.
    pub unavailable: Vec<Unavailable>,
    /// False whenever anything at all could not be done.
    pub complete: bool,
}

impl SuspectReport {
    /// Build a report, deriving `complete` from `unavailable` so the flag and
    /// the list cannot disagree.
    ///
    /// Taking `complete` as a parameter is how a report comes to claim a
    /// completeness its own contents contradict; there is deliberately no way
    /// to set it independently.
    pub fn new(
        symptom: String,
        suspects: Vec<Suspect>,
        cone_size: usize,
        blamed_symbols: usize,
        unavailable: Vec<Unavailable>,
    ) -> Self {
        let complete = unavailable.is_empty();
        Self {
            symptom,
            suspects,
            cone_size,
            blamed_symbols,
            unavailable,
            complete,
        }
    }

    /// An empty answer that says why it is empty, for the cases where the
    /// analysis cannot start at all.
    pub fn refused(symptom: String, unavailable: Vec<Unavailable>) -> Self {
        Self::new(symptom, Vec::new(), 0, 0, unavailable)
    }
}

#[cfg(test)]
mod blob_identity_tests {
    use super::BlobIdentity;

    #[test]
    fn the_same_blob_is_comparable_with_itself() {
        let left = BlobIdentity::Blob("abc123".into());
        let right = BlobIdentity::Blob("abc123".into());
        assert!(left.comparable_with(&right));
    }

    #[test]
    fn different_blobs_are_not_comparable() {
        let left = BlobIdentity::Blob("abc123".into());
        let right = BlobIdentity::Blob("def456".into());
        assert!(!left.comparable_with(&right));
    }

    #[test]
    fn an_unknown_basis_is_comparable_with_nothing_including_itself() {
        assert!(!BlobIdentity::Unknown.comparable_with(&BlobIdentity::Unknown));
        assert!(!BlobIdentity::Unknown.comparable_with(&BlobIdentity::Blob("abc".into())));
        assert!(!BlobIdentity::Blob("abc".into()).comparable_with(&BlobIdentity::Unknown));
    }

    /// The case that motivates the type. Two reads of a dirty file are two
    /// instants, and nothing proves the file held still between them.
    #[test]
    fn two_worktree_reads_are_never_assumed_to_be_the_same_bytes() {
        let left = BlobIdentity::Worktree {
            digest: "same".into(),
        };
        let right = BlobIdentity::Worktree {
            digest: "same".into(),
        };
        assert!(
            !left.comparable_with(&right),
            "equal digests taken at two instants still do not prove the file \
             held still; only a git object id is stable enough to pair"
        );
    }

    #[test]
    fn a_worktree_read_matching_a_blob_digest_is_comparable() {
        let blob = BlobIdentity::Blob("deadbeef".into());
        let worktree = BlobIdentity::Worktree {
            digest: "deadbeef".into(),
        };
        assert!(blob.comparable_with(&worktree));
        assert!(worktree.comparable_with(&blob));
    }
}

#[cfg(test)]
mod report_tests {
    use super::{SuspectReport, Unavailable};

    #[test]
    fn a_report_with_nothing_missing_is_complete() {
        let report = SuspectReport::new("x".into(), Vec::new(), 3, 3, Vec::new());
        assert!(report.complete);
    }

    /// The distinction the whole module exists for: no suspects because there
    /// are none, versus no suspects because nothing could be examined.
    #[test]
    fn an_empty_answer_that_could_not_look_is_not_complete() {
        let report = SuspectReport::refused(
            "x".into(),
            vec![Unavailable::GraphEmpty {
                reason: "index not built".into(),
            }],
        );
        assert!(report.suspects.is_empty());
        assert!(
            !report.complete,
            "an empty list from an unbuilt index must never read as a clean bill of health"
        );
    }
}
