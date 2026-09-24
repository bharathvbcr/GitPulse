//! The forward question: these lines changed — what is affected?
//!
//! [`crate::suspects`] runs from a symptom back to the commits that could have
//! caused it. This runs the other way: from a change forward to the symbols,
//! files, modules and tests that depend on what it touched. The two are
//! mirror images and share everything that makes either sound — the blob
//! identity, the span-to-line arithmetic, and the rule that a check which
//! could not run never reports what a check that passed reports.
//!
//! # The direction, again
//!
//! `suspects` walks **outbound** call edges, because a symptom is caused by
//! what it calls. This walks **inbound** edges, because a change breaks what
//! calls *it*. Getting these two backwards is the defect that has already cost
//! this codebase a full debug cycle, and the reason [`crate::CodeGraph`] now
//! has two separate methods with the direction written into each one's
//! contract rather than one method with a flag.
//!
//! # Why a changed line that hits no symbol is a finding, not a silence
//!
//! Most of the interesting part of this analysis is the lines it *cannot*
//! attribute. A change to an import, a top-level constant, a macro
//! invocation, a build attribute or a module-level `static` lands in no
//! symbol's span, so a seed-based walk finds nothing to walk from. Dropping
//! those lines would let a commit that changed a module's central constant
//! report "nothing affected" — a confident, wrong, unfalsifiable answer of
//! exactly the shape this crate exists to refuse.
//!
//! So every changed line is accounted for: it is either inside a symbol, or it
//! is in [`BlastReport::unattributed`] with the reason it could not be placed,
//! and the report is not complete while any remain.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

use devmap_extract::model::{LineIndex, Span};
use serde::{Deserialize, Serialize};

use crate::change::{
    changes_between_with_program, ChangeSet, ChangeStatus, ChangedRange, FileChange,
};
use crate::history::{self, resolve_blob_with_program};
use crate::{CodeGraph, ConeEntry, GraphSymbol, Unavailable};

/// How far the inbound walk goes by default.
///
/// Matched to [`crate::DEFAULT_CONE_DEPTH`] so the two directions answer over
/// the same horizon: a reader comparing "what could have caused this" against
/// "what does this affect" should not have to account for two different
/// walk depths.
pub const DEFAULT_BLAST_DEPTH: u32 = crate::DEFAULT_CONE_DEPTH;

/// Most symbols one change will seed the walk with.
///
/// A change touching more symbols than this is a sweep, and its blast radius
/// is the repository. The cap keeps the walk bounded; which seeds survive it
/// is decided by how much of each symbol changed, so a trimmed answer is made
/// of the most-changed symbols rather than of whatever sorted first.
pub const MAX_SEED_SYMBOLS: usize = 256;

/// Most commits examined per path when collecting owners.
pub const OWNER_COMMIT_CAP: usize = 32;

/// Wall clock for the whole owner pass across every changed path.
pub const OWNER_DEADLINE: Duration = Duration::from_secs(5);

/// Bytes of one path's `git log` kept while collecting owners.
pub const OWNER_OUTPUT_CAP: usize = 256 * 1024;

/// A symbol the change landed inside.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeedSymbol {
    pub qualified_name: String,
    pub file_path: String,
    /// One-based inclusive, the symbol's own extent in the post-image.
    pub start_line: u32,
    pub end_line: u32,
    /// How many of the change's lines fell inside this symbol.
    pub changed_lines: u32,
    /// True when every changed line that reached this symbol came from a
    /// deletion boundary, so the post-image shows nothing unusual and the
    /// evidence is what was removed.
    pub deletion_only: bool,
}

/// Why some changed lines could not be placed in a symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UnattributedReason {
    /// The file is indexed and has symbols, but none span these lines. This is
    /// module-level code: imports, top-level constants, attributes, macro
    /// invocations, or the gaps between declarations.
    ///
    /// `nearest_symbol` names the closest symbol in the file and how far away
    /// it is, which is diagnosis and **not** attribution. The commonest range
    /// that lands here is a doc comment or an attribute sitting just above the
    /// symbol it documents — extractors record a span from the declaration,
    /// not from its documentation — and a reader who can see that resolves it
    /// in a glance. Crediting it to that symbol automatically is the thing
    /// this crate refuses to do: a top-level constant declared immediately
    /// above a function is indistinguishable by position from that function's
    /// attributes, and one of those two guesses is a fabricated edge.
    OutsideEverySymbol {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nearest_symbol: Option<String>,
        /// Lines between the range and `nearest_symbol`. Zero means adjacent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lines_away: Option<u32>,
    },
    /// The graph holds no symbols for this file — it is not a language the
    /// index parses, it is excluded, or the index predates it.
    FileNotIndexed,
    /// The file has no post-image lines to intersect: it was deleted, or its
    /// content is binary.
    NoPostImageLines { status: String },
    /// The spans and the content did not share a basis, so no line could be
    /// placed without inventing the arithmetic.
    BasisMismatch {
        spans_taken_against: String,
        lines_built_from: String,
    },
    /// The range names lines past the end of the post-image content. The diff
    /// and the blob disagree, which means one of them is not describing the
    /// revision this analysis was told to run against.
    BeyondEndOfFile { last_line: u32 },
    /// The post-image content could not be read at all.
    ContentUnavailable { reason: String },
}

impl UnattributedReason {
    pub fn describe(&self) -> String {
        match self {
            UnattributedReason::OutsideEverySymbol {
                nearest_symbol,
                lines_away,
            } => {
                let base = "module-level: inside no symbol the graph declares, so the inbound \
                            walk has nothing to start from — imports, top-level constants, \
                            attributes and macro invocations all land here";
                match (nearest_symbol, lines_away) {
                    (Some(symbol), Some(0)) => {
                        format!("{base}; immediately adjacent to {symbol}")
                    }
                    (Some(symbol), Some(away)) => {
                        format!("{base}; nearest symbol {symbol}, {away} line(s) away")
                    }
                    (Some(symbol), None) => format!("{base}; nearest symbol {symbol}"),
                    _ => base.to_string(),
                }
            }
            UnattributedReason::FileNotIndexed => {
                "the graph holds no symbols for this file, so nothing in it can seed a walk"
                    .to_string()
            }
            UnattributedReason::NoPostImageLines { status } => {
                format!("the file is {status} in the post-image, so it has no lines to place")
            }
            UnattributedReason::BasisMismatch {
                spans_taken_against,
                lines_built_from,
            } => format!(
                "spans were taken against {spans_taken_against} but the content came from \
                 {lines_built_from}, so no line can be placed in a symbol"
            ),
            UnattributedReason::BeyondEndOfFile { last_line } => format!(
                "the change names lines past the end of the post-image content, which ends \
                 at line {last_line}; the diff and the blob describe different revisions"
            ),
            UnattributedReason::ContentUnavailable { reason } => {
                format!("the post-image content could not be read: {reason}")
            }
        }
    }
}

/// Changed lines that reached no symbol, with why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnattributedChange {
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub reason: UnattributedReason,
}

/// A symbol the walk reached from the seeds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImpactedSymbol {
    pub qualified_name: String,
    pub file_path: String,
    /// Inbound call edges from the nearest seed. Zero is a seed itself.
    pub distance: u32,
}

/// A file the walk reached, rolled up from its symbols.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImpactedFile {
    pub path: String,
    pub symbols: u32,
    pub nearest_distance: u32,
    /// True when this file is one the change itself touched.
    pub changed: bool,
}

/// A directory the walk reached, rolled up from its files.
///
/// A module is the file's directory. That is the unit `repo_map` subsystems
/// use and the unit a reader navigates by; deriving it from a language's own
/// module system would give a different answer per language and none at all
/// for the files that have no module system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImpactedModule {
    pub path: String,
    pub files: u32,
    pub symbols: u32,
    pub nearest_distance: u32,
    /// True when the change itself touched a file in this directory.
    pub changed: bool,
    /// Whether tests covering this module moved with the change.
    pub test_signal: TestSignal,
}

/// Whether the tests that reach an impacted module moved with the change.
///
/// `unavailable` is a separate state from `none`: a walk that could not finish
/// must never read as "there are no tests".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestSignal {
    /// A reached test file is in the diff.
    Changed,
    /// Tests reach the area and none of those files changed.
    Stale,
    /// No test file reached.
    None,
    /// No behavioral seed — nothing to ask tests about.
    Na,
    /// The test walk itself could not finish, or a renamed test could not be
    /// reconciled with the reached list.
    Unavailable,
}

/// A recent author of a changed path, name and email only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    pub name: String,
    pub email: String,
}

/// A test file the inbound walk reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AffectedTestFile {
    pub path: String,
    /// Shortest distance from any seed.
    pub distance: u32,
}

/// The answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlastReport {
    /// What was asked, as a revision range or an explicit location.
    pub change: String,
    /// Files the change touched, with their post-image ranges.
    pub changed_files: Vec<FileChange>,
    /// Symbols the change landed inside, most-changed first.
    pub seeds: Vec<SeedSymbol>,
    /// Changed lines that reached no symbol. Never empty because something was
    /// skipped — every entry names why.
    pub unattributed: Vec<UnattributedChange>,
    /// Symbols the inbound walk reached, nearest first. Excludes the seeds.
    pub impacted: Vec<ImpactedSymbol>,
    pub files: Vec<ImpactedFile>,
    pub modules: Vec<ImpactedModule>,
    pub tests: Vec<AffectedTestFile>,
    /// Recent authors of the changed paths, name and email only. Empty when
    /// there were no paths to ask about, or when every lookup failed — and in
    /// the latter case [`Self::unavailable`] names why, so emptiness is not
    /// read as "nobody owns this".
    pub owners: Vec<Owner>,
    /// Everything that could not be done. Non-empty means every list above is
    /// a lower bound.
    pub unavailable: Vec<Unavailable>,
    /// False whenever anything at all could not be done.
    pub complete: bool,
}

impl BlastReport {
    /// Build a report, deriving `complete` from `unavailable` so the flag and
    /// the list cannot disagree.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        change: String,
        changed_files: Vec<FileChange>,
        seeds: Vec<SeedSymbol>,
        unattributed: Vec<UnattributedChange>,
        impacted: Vec<ImpactedSymbol>,
        files: Vec<ImpactedFile>,
        modules: Vec<ImpactedModule>,
        tests: Vec<AffectedTestFile>,
        owners: Vec<Owner>,
        unavailable: Vec<Unavailable>,
    ) -> Self {
        let complete = unavailable.is_empty();
        Self {
            change,
            changed_files,
            seeds,
            unattributed,
            impacted,
            files,
            modules,
            tests,
            owners,
            unavailable,
            complete,
        }
    }

    /// An empty answer that says why it is empty.
    pub fn refused(change: String, unavailable: Vec<Unavailable>) -> Self {
        Self::new(
            change,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            unavailable,
        )
    }
}

/// The directory a file sits in, as the module key.
///
/// A file at the repository root has no directory; it is its own module, named
/// `.` rather than the empty string so the key is never mistaken for missing.
fn module_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) => "/",
        Some(index) => &path[..index],
        None => ".",
    }
}

/// Run the analysis over `since..until`.
///
/// `until` must be the revision the graph's spans were taken against.
/// Anything else makes every file's basis check fail — correctly, because the
/// spans then describe content the diff is not about — so the caller pins it
/// rather than being allowed to choose.
pub fn blast<G: CodeGraph>(
    repo: &Path,
    graph: &G,
    since: &str,
    until: &str,
    depth: u32,
) -> BlastReport {
    blast_with_program(
        std::ffi::OsStr::new("git"),
        repo,
        graph,
        since,
        until,
        depth,
    )
}

#[doc(hidden)]
pub fn blast_with_program<G: CodeGraph>(
    program: &std::ffi::OsStr,
    repo: &Path,
    graph: &G,
    since: &str,
    until: &str,
    depth: u32,
) -> BlastReport {
    let label = format!("{since}..{until}");
    let change = match changes_between_with_program(program, repo, since, until) {
        Ok(change) => change,
        Err(refusal) => {
            return BlastReport::refused(
                label,
                vec![Unavailable::ChangeUnreadable {
                    reason: refusal.describe(),
                }],
            )
        }
    };
    blast_change_with_program(program, repo, graph, &change, &label, until, depth)
}

/// The analysis proper, over a change set already in hand.
///
/// Split from [`blast`] so the whole of it can be driven from a change set
/// built in a test — the git half and the graph half fail in different ways
/// and testing them through one entry point tests neither thoroughly.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn blast_change_with_program<G: CodeGraph>(
    program: &std::ffi::OsStr,
    repo: &Path,
    graph: &G,
    change: &ChangeSet,
    label: &str,
    until: &str,
    depth: u32,
) -> BlastReport {
    let mut unavailable: Vec<Unavailable> = Vec::new();
    let mut unattributed: Vec<UnattributedChange> = Vec::new();
    // qualified name -> (file, span lines, changed lines, deletion-only so far)
    let mut seeds: BTreeMap<String, SeedSymbol> = BTreeMap::new();

    if change.capped {
        unavailable.push(Unavailable::ChangedFilesCapped {
            considered: change.files.len(),
            cap: crate::change::MAX_CHANGED_FILES,
        });
    }

    for file in &change.files {
        place_file(
            program,
            repo,
            graph,
            file,
            until,
            &mut seeds,
            &mut unattributed,
        );
    }

    // Every unattributed range means the walk started from fewer places than
    // the change touched, so everything downstream is a lower bound. Recorded
    // once with the total rather than once per range, because a formatting
    // sweep would otherwise fill the ledger with a thousand identical rows.
    if !unattributed.is_empty() {
        let lines: u64 = unattributed
            .iter()
            // Widened before the arithmetic — see `ChangeSet::touched_lines`
            // for why `end - start + 1` in `u32` is a debug panic waiting on
            // one wide range.
            .map(|entry| u64::from(entry.end_line).saturating_sub(u64::from(entry.start_line)) + 1)
            .sum();
        unavailable.push(Unavailable::ChangeUnattributed {
            ranges: unattributed.len(),
            lines,
        });
    }

    // Most-changed first, so the seed cap keeps the symbols the change did the
    // most to rather than the ones whose names sort first.
    let mut ordered: Vec<SeedSymbol> = seeds.into_values().collect();
    ordered.sort_by(|a, b| {
        b.changed_lines
            .cmp(&a.changed_lines)
            .then(a.file_path.cmp(&b.file_path))
            .then(a.qualified_name.cmp(&b.qualified_name))
    });
    if ordered.len() > MAX_SEED_SYMBOLS {
        unavailable.push(Unavailable::SeedsCapped {
            seeded: MAX_SEED_SYMBOLS,
            touched: ordered.len(),
            cap: MAX_SEED_SYMBOLS,
        });
        ordered.truncate(MAX_SEED_SYMBOLS);
    }

    let seed_names: Vec<String> = ordered
        .iter()
        .map(|seed| seed.qualified_name.clone())
        .collect();
    let seed_set: BTreeSet<&str> = seed_names.iter().map(String::as_str).collect();
    let changed_paths: BTreeSet<&str> =
        change.files.iter().map(|file| file.path.as_str()).collect();

    // No seeds is a legitimate finding — a docs-only or config-only change has
    // no symbols to walk from — so the walks are skipped rather than the
    // report being cut short. They used to be cut short: an early return here
    // skipped the roll-up too, and a change to an unindexed file came back
    // with an empty `files` list, reporting that a change which demonstrably
    // touched a file had touched none. The roll-up is below every branch now,
    // so there is no second path for it to be forgotten on.
    let (reached, walk_incomplete) = if seed_names.is_empty() {
        (Vec::new(), false)
    } else {
        graph.impacted(&seed_names, depth)
    };
    if walk_incomplete {
        unavailable.push(Unavailable::ImpactIncomplete {
            depth,
            reached: reached.len(),
        });
    }

    let impacted = roll_up_symbols(reached, &seed_set);
    let files = roll_up_files(&impacted, &ordered, &changed_paths);
    let mut modules = roll_up_modules(&files);

    let (tests, tests_incomplete) = if seed_names.is_empty() {
        (Vec::new(), false)
    } else {
        graph.affected_tests(&seed_names, depth)
    };
    if tests_incomplete {
        unavailable.push(Unavailable::AffectedTestsIncomplete { found: tests.len() });
    }

    annotate_test_signals(
        &mut modules,
        &ordered,
        &tests,
        &change.files,
        tests_incomplete,
    );

    let (owners, owner_gaps) = owners_for_paths_with_program(
        program,
        repo,
        &owner_paths(&change.files),
        OWNER_COMMIT_CAP,
        OWNER_DEADLINE,
    );
    unavailable.extend(owner_gaps);

    BlastReport::new(
        label.to_string(),
        change.files.clone(),
        ordered,
        unattributed,
        impacted,
        files,
        modules,
        tests,
        owners,
        unavailable,
    )
}

/// Place one file's changed ranges into the symbols that span them.
fn place_file<G: CodeGraph>(
    program: &std::ffi::OsStr,
    repo: &Path,
    graph: &G,
    file: &FileChange,
    until: &str,
    seeds: &mut BTreeMap<String, SeedSymbol>,
    unattributed: &mut Vec<UnattributedChange>,
) {
    // A file with no post-image lines cannot be intersected with anything. Its
    // ranges are empty by construction, so there is nothing to mark
    // unattributed either — but the file is still reported as changed, and
    // saying why it contributed no seeds is the point.
    if matches!(file.status, ChangeStatus::Deleted | ChangeStatus::Binary) {
        unattributed.push(UnattributedChange {
            path: file.path.clone(),
            start_line: 0,
            end_line: 0,
            reason: UnattributedReason::NoPostImageLines {
                status: match file.status {
                    ChangeStatus::Deleted => "deleted".to_string(),
                    _ => "binary".to_string(),
                },
            },
        });
        return;
    }
    if file.ranges.is_empty() {
        // A mode-only change, or a rename with no edits. Nothing changed
        // inside the content, so there is nothing to place and nothing
        // missing.
        return;
    }

    let (symbols, spans_basis) = graph.symbols_in(&file.path);
    if symbols.is_empty() {
        push_all(
            unattributed,
            &file.path,
            &file.ranges,
            UnattributedReason::FileNotIndexed,
        );
        return;
    }

    let resolved = match resolve_blob_with_program(program, repo, until, &file.path) {
        Ok(resolved) => resolved,
        Err(refusal) => {
            push_all(
                unattributed,
                &file.path,
                &file.ranges,
                UnattributedReason::ContentUnavailable {
                    reason: refusal.describe(),
                },
            );
            return;
        }
    };
    if !spans_basis.comparable_with(&resolved.identity) {
        push_all(
            unattributed,
            &file.path,
            &file.ranges,
            UnattributedReason::BasisMismatch {
                spans_taken_against: spans_basis.describe(),
                lines_built_from: resolved.identity.describe(),
            },
        );
        return;
    }

    let index = LineIndex::new(&resolved.content);
    // One past the newline count is the phantom final line a span ending at
    // EOF lands on — the same convention `join` documents, so a symbol that
    // ends at EOF is not judged to be past the end of its own file.
    let last_line = resolved.content.matches('\n').count() as u32 + 1;

    let placed: Vec<(u32, u32, &GraphSymbol)> = symbols
        .iter()
        .map(|symbol| {
            let span = Span {
                start_byte: symbol.span_start,
                end_byte: symbol.span_end,
            };
            let (start, end) = index.line_range(&span);
            // The same degeneracy rule `join::attribute` applies: a stored
            // span whose end precedes its start describes one line, not a
            // backwards range. Written the same way in both places on purpose
            // — a range that is inverted in one and degenerate in the other
            // would make the two directions disagree about which symbol a line
            // belongs to.
            let end = if end < start { start } else { end };
            (start, end, symbol)
        })
        .collect();

    for range in &file.ranges {
        if range.start_line > last_line {
            unattributed.push(UnattributedChange {
                path: file.path.clone(),
                start_line: range.start_line,
                end_line: range.end_line,
                reason: UnattributedReason::BeyondEndOfFile { last_line },
            });
            continue;
        }
        let mut hit = false;
        for (start, end, symbol) in &placed {
            let overlap_start = (*start).max(range.start_line);
            let overlap_end = (*end).min(range.end_line);
            if overlap_start > overlap_end {
                continue;
            }
            hit = true;
            let lines = overlap_end - overlap_start + 1;
            seeds
                .entry(symbol.qualified_name.clone())
                .and_modify(|held| {
                    held.changed_lines = held.changed_lines.saturating_add(lines);
                    // One real edit anywhere in the symbol means it is not a
                    // bare deletion boundary, however many deletions
                    // accompanied it.
                    held.deletion_only = held.deletion_only && range.deletion_only;
                })
                .or_insert_with(|| SeedSymbol {
                    qualified_name: symbol.qualified_name.clone(),
                    file_path: symbol.file_path.clone(),
                    start_line: *start,
                    end_line: *end,
                    changed_lines: lines,
                    deletion_only: range.deletion_only,
                });
        }
        if !hit {
            let (nearest_symbol, lines_away) = nearest_to(&placed, range);
            unattributed.push(UnattributedChange {
                path: file.path.clone(),
                start_line: range.start_line,
                end_line: range.end_line,
                reason: UnattributedReason::OutsideEverySymbol {
                    nearest_symbol,
                    lines_away,
                },
            });
        }
    }
}

/// The symbol nearest a range that landed in none, and the gap between them.
///
/// Diagnosis only — nothing downstream treats this as a seed. Ties resolve to
/// the symbol that *follows* the range, because the range that most often
/// lands here is a doc comment or attribute block sitting immediately above
/// the declaration it belongs to.
fn nearest_to(
    placed: &[(u32, u32, &GraphSymbol)],
    range: &ChangedRange,
) -> (Option<String>, Option<u32>) {
    let mut best: Option<(u32, bool, &str)> = None;
    for (start, end, symbol) in placed {
        // The range is outside every symbol, so it is wholly before this one
        // or wholly after it.
        let (gap, follows) = if *start > range.end_line {
            (start - range.end_line - 1, true)
        } else {
            (
                range.start_line.saturating_sub(*end).saturating_sub(1),
                false,
            )
        };
        let candidate = (gap, follows, symbol.qualified_name.as_str());
        best = match best {
            // Smaller gap wins; at an equal gap the following symbol wins.
            Some((held_gap, held_follows, _))
                if held_gap < gap || (held_gap == gap && held_follows) =>
            {
                best
            }
            _ => Some(candidate),
        };
    }
    match best {
        Some((gap, _, name)) => (Some(name.to_string()), Some(gap)),
        None => (None, None),
    }
}

fn push_all(
    unattributed: &mut Vec<UnattributedChange>,
    path: &str,
    ranges: &[ChangedRange],
    reason: UnattributedReason,
) {
    for range in ranges {
        unattributed.push(UnattributedChange {
            path: path.to_string(),
            start_line: range.start_line,
            end_line: range.end_line,
            reason: reason.clone(),
        });
    }
}

/// Reached symbols, nearest first, with the seeds themselves removed.
///
/// A seed is at distance zero and is already reported in `seeds`; repeating it
/// here would double every count a reader takes from `impacted`.
fn roll_up_symbols(reached: Vec<ConeEntry>, seeds: &BTreeSet<&str>) -> Vec<ImpactedSymbol> {
    let mut nearest: BTreeMap<String, (String, u32)> = BTreeMap::new();
    for entry in reached {
        if seeds.contains(entry.qualified_name.as_str()) {
            continue;
        }
        nearest
            .entry(entry.qualified_name)
            .and_modify(|held| {
                if entry.distance < held.1 {
                    held.1 = entry.distance;
                }
            })
            .or_insert((entry.file_path, entry.distance));
    }
    let mut out: Vec<ImpactedSymbol> = nearest
        .into_iter()
        .map(|(qualified_name, (file_path, distance))| ImpactedSymbol {
            qualified_name,
            file_path,
            distance,
        })
        .collect();
    out.sort_by(|a, b| {
        a.distance
            .cmp(&b.distance)
            .then(a.file_path.cmp(&b.file_path))
            .then(a.qualified_name.cmp(&b.qualified_name))
    });
    out
}

/// Files the walk reached, plus the files the change itself touched.
///
/// The changed files are included at distance zero even when no symbol in them
/// was reached: a reader asking "which files are affected" means the changed
/// ones too, and a list that omits them is answering a narrower question than
/// the one asked.
fn roll_up_files(
    impacted: &[ImpactedSymbol],
    seeds: &[SeedSymbol],
    changed_paths: &BTreeSet<&str>,
) -> Vec<ImpactedFile> {
    let mut by_path: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    for seed in seeds {
        let entry = by_path.entry(seed.file_path.clone()).or_insert((0, 0));
        entry.0 = entry.0.saturating_add(1);
    }
    for symbol in impacted {
        let entry = by_path
            .entry(symbol.file_path.clone())
            .or_insert((0, u32::MAX));
        entry.0 = entry.0.saturating_add(1);
        entry.1 = entry.1.min(symbol.distance);
    }
    // A changed file with no symbols at all still belongs in the list.
    for path in changed_paths {
        by_path.entry((*path).to_string()).or_insert((0, 0));
    }

    let mut out: Vec<ImpactedFile> = by_path
        .into_iter()
        .map(|(path, (symbols, distance))| {
            let changed = changed_paths.contains(path.as_str());
            ImpactedFile {
                // Both cases are genuinely distance zero, so they are one
                // condition rather than two arms returning the same number: a
                // file the change touched is at the change, and a file no
                // impacted symbol reached (`u32::MAX`, the fold's identity)
                // holds only seeds, which are also at the change.
                nearest_distance: if changed || distance == u32::MAX {
                    0
                } else {
                    distance
                },
                changed,
                path,
                symbols,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        a.nearest_distance
            .cmp(&b.nearest_distance)
            .then(b.symbols.cmp(&a.symbols))
            .then(a.path.cmp(&b.path))
    });
    out
}

fn roll_up_modules(files: &[ImpactedFile]) -> Vec<ImpactedModule> {
    let mut by_module: BTreeMap<String, (u32, u32, u32, bool)> = BTreeMap::new();
    for file in files {
        let entry = by_module
            .entry(module_of(&file.path).to_string())
            .or_insert((0, 0, u32::MAX, false));
        entry.0 = entry.0.saturating_add(1);
        entry.1 = entry.1.saturating_add(file.symbols);
        entry.2 = entry.2.min(file.nearest_distance);
        entry.3 |= file.changed;
    }
    let mut out: Vec<ImpactedModule> = by_module
        .into_iter()
        .map(
            |(path, (files, symbols, distance, changed))| ImpactedModule {
                path,
                files,
                symbols,
                nearest_distance: if distance == u32::MAX { 0 } else { distance },
                changed,
                // Filled by [`annotate_test_signals`] once the test walk has
                // answered. Defaulting to `Na` here would collapse a missing
                // annotation into "no behavioral seed"; `None` would collapse
                // it into "no tests". The placeholder is overwritten before
                // the report is built.
                test_signal: TestSignal::Unavailable,
            },
        )
        .collect();
    out.sort_by(|a, b| {
        a.nearest_distance
            .cmp(&b.nearest_distance)
            .then(b.symbols.cmp(&a.symbols))
            .then(a.path.cmp(&b.path))
    });
    out
}

/// Paths to ask `git log` about: every post-image path and every pre-image
/// rename source. Asking only the new name after a rename would miss the
/// authors who wrote under the old one.
fn owner_paths(changed: &[FileChange]) -> Vec<String> {
    let mut paths: BTreeSet<String> = BTreeSet::new();
    for file in changed {
        paths.insert(file.path.clone());
        if let Some(from) = &file.renamed_from {
            paths.insert(from.clone());
        }
    }
    paths.into_iter().collect()
}

/// Per-module test signal from the reached tests and the change's files.
///
/// `unavailable` when the test walk itself could not finish — never collapsed
/// into `none`. A renamed test file that the walk did not reconcile is also
/// `unavailable`, not a silent "no tests".
fn annotate_test_signals(
    modules: &mut [ImpactedModule],
    seeds: &[SeedSymbol],
    tests: &[AffectedTestFile],
    changed_files: &[FileChange],
    tests_incomplete: bool,
) {
    for module in modules.iter_mut() {
        module.test_signal =
            test_signal_for_module(module, seeds, tests, changed_files, tests_incomplete);
    }
}

fn test_signal_for_module(
    module: &ImpactedModule,
    seeds: &[SeedSymbol],
    tests: &[AffectedTestFile],
    changed_files: &[FileChange],
    tests_incomplete: bool,
) -> TestSignal {
    let module_has_seed = seeds
        .iter()
        .any(|seed| module_of(&seed.file_path) == module.path);
    // A module that holds neither a seed nor any reached symbol is present
    // only because an unindexed or docs-only file changed there — there is
    // nothing behavioral to ask tests about.
    if seeds.is_empty() || (!module_has_seed && module.symbols == 0) {
        return TestSignal::Na;
    }
    if tests_incomplete {
        return TestSignal::Unavailable;
    }

    let change_touches_test = changed_files.iter().any(|file| {
        looks_like_test_path(&file.path)
            || file
                .renamed_from
                .as_deref()
                .is_some_and(looks_like_test_path)
    });

    if tests.is_empty() {
        // A renamed, deleted or added test in the diff with an empty reached
        // list is not "no tests" — the walk and the change disagree.
        if change_touches_test {
            return TestSignal::Unavailable;
        }
        return TestSignal::None;
    }

    let test_in_diff = tests.iter().any(|test| {
        changed_files.iter().any(|file| {
            file.path == test.path || file.renamed_from.as_deref() == Some(test.path.as_str())
        })
    });
    if test_in_diff {
        TestSignal::Changed
    } else {
        TestSignal::Stale
    }
}

fn looks_like_test_path(path: &str) -> bool {
    // Same owner as the rest of the workspace's test-path rule, available
    // without the parse frontend that this crate deliberately does not link.
    devmap_extract::wiring::is_test_path(path)
}

/// Recent authors of `paths`, name and email only.
///
/// Paths are examined one at a time under a shared deadline so a timeout after
/// the first of many still returns the owners already found and an
/// [`Unavailable`] entry — never a silently short list.
#[doc(hidden)]
pub fn owners_for_paths_with_program(
    program: &std::ffi::OsStr,
    repo: &Path,
    paths: &[String],
    commit_cap: usize,
    deadline: Duration,
) -> (Vec<Owner>, Vec<Unavailable>) {
    if paths.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let started = Instant::now();
    let mut by_identity: BTreeMap<(String, String), ()> = BTreeMap::new();
    let mut unavailable: Vec<Unavailable> = Vec::new();
    let mut empty_author = false;
    let mut examined = 0usize;

    for path in paths {
        let remaining = deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            unavailable.push(Unavailable::OwnersUnavailable {
                reason: format!(
                    "owner lookup exceeded {deadline:?} after {examined} of {} path(s)",
                    paths.len()
                ),
            });
            break;
        }

        let max_count = format!("--max-count={commit_cap}");
        let what = format!("git log -- {path}");
        match history::run_git_with_deadline(
            program,
            repo,
            &["log", &max_count, "--format=%an%x00%ae", "--", path],
            &what,
            OWNER_OUTPUT_CAP,
            remaining,
        ) {
            Ok(captured) => {
                examined += 1;
                for line in captured.stdout_lossy().lines() {
                    let line = line.trim_end_matches('\r');
                    if line.is_empty() {
                        continue;
                    }
                    let (name, email) = match line.split_once('\0') {
                        Some((name, email)) => (name.trim(), email.trim()),
                        None => (line.trim(), ""),
                    };
                    if name.is_empty() && email.is_empty() {
                        empty_author = true;
                        continue;
                    }
                    by_identity.insert((name.to_string(), email.to_string()), ());
                }
            }
            Err(history::HistoryRefusal::Deadline { what }) => {
                unavailable.push(Unavailable::OwnersUnavailable {
                    reason: format!(
                        "{what} exceeded the remaining owner budget after {examined} of {} path(s)",
                        paths.len()
                    ),
                });
                break;
            }
            Err(refusal) => {
                unavailable.push(Unavailable::OwnersUnavailable {
                    reason: refusal.describe(),
                });
                // A spawn failure means every later path will fail the same
                // way; stop rather than fill the ledger with duplicates.
                if matches!(refusal, history::HistoryRefusal::GitUnavailable { .. }) {
                    break;
                }
                examined += 1;
            }
        }
    }

    if empty_author {
        unavailable.push(Unavailable::OwnersUnavailable {
            reason: "at least one commit has an empty author name and email".into(),
        });
    }

    let owners = by_identity
        .into_iter()
        .map(|((name, email), _)| Owner { name, email })
        .collect();
    (owners, unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_module_is_the_files_directory() {
        assert_eq!(
            module_of("rust/dc-regress/src/blast.rs"),
            "rust/dc-regress/src"
        );
    }

    /// A root-level file is its own module. Named `.` rather than the empty
    /// string so a grouping key is never mistaken for an absent one.
    #[test]
    fn a_root_level_file_has_a_named_module() {
        assert_eq!(module_of("README.md"), ".");
        assert_eq!(module_of("/abs.rs"), "/");
    }

    fn seed(name: &str, file: &str, lines: u32) -> SeedSymbol {
        SeedSymbol {
            qualified_name: name.into(),
            file_path: file.into(),
            start_line: 1,
            end_line: 10,
            changed_lines: lines,
            deletion_only: false,
        }
    }

    fn reached(name: &str, file: &str, distance: u32) -> ConeEntry {
        ConeEntry {
            qualified_name: name.into(),
            file_path: file.into(),
            distance,
        }
    }

    /// A seed is already reported as a seed. Counting it again among the
    /// reached symbols would double every total a reader takes from the
    /// report.
    #[test]
    fn a_seed_is_not_repeated_among_the_symbols_it_reached() {
        let seeds: BTreeSet<&str> = ["a.rs::seed"].into_iter().collect();
        let rolled = roll_up_symbols(
            vec![
                reached("a.rs::seed", "a.rs", 0),
                reached("b.rs::caller", "b.rs", 1),
            ],
            &seeds,
        );
        assert_eq!(rolled.len(), 1);
        assert_eq!(rolled[0].qualified_name, "b.rs::caller");
    }

    #[test]
    fn a_symbol_reached_by_two_routes_keeps_its_shortest_distance() {
        let rolled = roll_up_symbols(
            vec![reached("b.rs::x", "b.rs", 3), reached("b.rs::x", "b.rs", 1)],
            &BTreeSet::new(),
        );
        assert_eq!(rolled.len(), 1);
        assert_eq!(rolled[0].distance, 1);
    }

    /// The question is "which files are affected", and the file you edited is
    /// affected. A list that starts at the callers answers something narrower.
    #[test]
    fn a_changed_file_is_in_the_file_list_even_with_nothing_reached() {
        let changed: BTreeSet<&str> = ["a.rs"].into_iter().collect();
        let files = roll_up_files(&[], &[seed("a.rs::f", "a.rs", 3)], &changed);
        let entry = files.iter().find(|f| f.path == "a.rs").expect("present");
        assert!(entry.changed);
        assert_eq!(entry.nearest_distance, 0);
    }

    #[test]
    fn a_changed_file_with_no_symbols_at_all_is_still_listed() {
        let changed: BTreeSet<&str> = ["docs/README.md"].into_iter().collect();
        let files = roll_up_files(&[], &[], &changed);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "docs/README.md");
        assert_eq!(files[0].symbols, 0);
    }

    #[test]
    fn modules_roll_up_their_files_and_keep_the_nearest_distance() {
        let files = vec![
            ImpactedFile {
                path: "src/a.rs".into(),
                symbols: 2,
                nearest_distance: 3,
                changed: false,
            },
            ImpactedFile {
                path: "src/b.rs".into(),
                symbols: 1,
                nearest_distance: 1,
                changed: false,
            },
            ImpactedFile {
                path: "other/c.rs".into(),
                symbols: 5,
                nearest_distance: 2,
                changed: true,
            },
        ];
        let modules = roll_up_modules(&files);
        let src = modules.iter().find(|m| m.path == "src").expect("present");
        assert_eq!(src.files, 2);
        assert_eq!(src.symbols, 3);
        assert_eq!(src.nearest_distance, 1);
        assert!(!src.changed);
        let other = modules.iter().find(|m| m.path == "other").expect("present");
        assert!(other.changed, "a module holding a changed file is changed");
    }

    #[test]
    fn a_report_with_nothing_missing_is_complete() {
        let report = BlastReport::new(
            "a..b".into(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        assert!(report.complete);
    }

    #[test]
    fn a_refused_report_is_never_complete() {
        let report = BlastReport::refused(
            "a..b".into(),
            vec![Unavailable::ChangeUnreadable {
                reason: "git exploded".into(),
            }],
        );
        assert!(!report.complete);
        assert!(report.seeds.is_empty());
        assert!(report.owners.is_empty());
    }

    fn module(path: &str, symbols: u32) -> ImpactedModule {
        ImpactedModule {
            path: path.into(),
            files: 1,
            symbols,
            nearest_distance: 0,
            changed: true,
            test_signal: TestSignal::Unavailable,
        }
    }

    /// An incomplete test walk must never collapse into `none`. That is how
    /// "we stopped looking" comes to read as "there are no tests".
    #[test]
    fn an_incomplete_test_walk_is_unavailable_not_none() {
        let mut modules = vec![module("src", 1)];
        annotate_test_signals(
            &mut modules,
            &[seed("src/lib.rs::f", "src/lib.rs", 1)],
            &[],
            &[],
            true,
        );
        assert_eq!(modules[0].test_signal, TestSignal::Unavailable);
    }

    #[test]
    fn no_behavioral_seed_is_na() {
        let mut modules = vec![module("docs", 0)];
        annotate_test_signals(&mut modules, &[], &[], &[], false);
        assert_eq!(modules[0].test_signal, TestSignal::Na);
    }

    #[test]
    fn reached_tests_that_did_not_change_are_stale() {
        let mut modules = vec![module("src", 1)];
        annotate_test_signals(
            &mut modules,
            &[seed("src/lib.rs::f", "src/lib.rs", 1)],
            &[AffectedTestFile {
                path: "tests/f.rs".into(),
                distance: 1,
            }],
            &[FileChange {
                path: "src/lib.rs".into(),
                renamed_from: None,
                status: ChangeStatus::Modified,
                ranges: vec![],
            }],
            false,
        );
        assert_eq!(modules[0].test_signal, TestSignal::Stale);
    }

    #[test]
    fn a_reached_test_in_the_diff_is_changed() {
        let mut modules = vec![module("src", 1)];
        annotate_test_signals(
            &mut modules,
            &[seed("src/lib.rs::f", "src/lib.rs", 1)],
            &[AffectedTestFile {
                path: "tests/f.rs".into(),
                distance: 1,
            }],
            &[FileChange {
                path: "tests/f.rs".into(),
                renamed_from: None,
                status: ChangeStatus::Modified,
                ranges: vec![],
            }],
            false,
        );
        assert_eq!(modules[0].test_signal, TestSignal::Changed);
    }

    /// A renamed test file that the walk did not reconcile must not read as
    /// "no tests" — that is the silent wrong answer this signal exists to
    /// refuse.
    #[test]
    fn a_renamed_test_with_no_reached_list_is_unavailable_not_none() {
        let mut modules = vec![module("src", 1)];
        annotate_test_signals(
            &mut modules,
            &[seed("src/lib.rs::f", "src/lib.rs", 1)],
            &[],
            &[FileChange {
                path: "tests/new.rs".into(),
                renamed_from: Some("tests/old.rs".into()),
                status: ChangeStatus::Modified,
                ranges: vec![],
            }],
            false,
        );
        assert_eq!(modules[0].test_signal, TestSignal::Unavailable);
        assert_ne!(modules[0].test_signal, TestSignal::None);
    }

    #[test]
    fn no_tests_reached_and_no_test_in_the_diff_is_none() {
        let mut modules = vec![module("src", 1)];
        annotate_test_signals(
            &mut modules,
            &[seed("src/lib.rs::f", "src/lib.rs", 1)],
            &[],
            &[FileChange {
                path: "src/lib.rs".into(),
                renamed_from: None,
                status: ChangeStatus::Modified,
                ranges: vec![],
            }],
            false,
        );
        assert_eq!(modules[0].test_signal, TestSignal::None);
    }

    #[test]
    fn an_owners_unavailable_entry_makes_the_report_incomplete() {
        let report = BlastReport::new(
            "a..b".into(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![Unavailable::OwnersUnavailable {
                reason: "not a git repository".into(),
            }],
        );
        assert!(!report.complete);
        assert!(report.owners.is_empty());
    }
}
