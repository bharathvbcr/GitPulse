//! Symbol classification for one already-reported overlapping path.
//!
//! The porcelain collision scan stays path-only and cheap. This module runs
//! *after* that scan names a path, and only then: each worktree's old-side
//! diff line ranges are joined to DevMap symbol spans when that worktree's
//! index `head_sha` equals its `HEAD`. Disjoint symbols are a distinct notice,
//! not a merge promise. A mismatch, missing store, or untracked file stays
//! today's file-level notice plus why.

use crate::codeintel::{self, CodeintelFileSymbol};
use crate::engine::git_cli::git_text;
use crate::engine::git_reader::GitReader;
use crate::engine::validate_repo;
use crate::insights::{CollisionItem, CollisionParty};
use crate::ipc_error::CommandError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

/// How the symbol join classified one overlapping path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityCollisionKind {
    /// At least one symbol appears on both sides' old-side ranges.
    SharedSymbol,
    /// Both sides resolved symbols, none overlap, and no two parties' edits
    /// touch (adjacent or overlapping old-side lines, which git conflicts) —
    /// file overlap only. The touch rule compares old-side line numbers, so it
    /// is meaningful only when the parties share a HEAD.
    DisjointSymbols,
    /// Index / diff / trackability could not support a symbol join.
    FileLevel,
}

/// Verdict for one overlapping path the porcelain scan already named.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EntityCollisionVerdict {
    pub path: String,
    pub kind: EntityCollisionKind,
    /// Human-readable explanation. Always set — including for shared symbols.
    pub reason: String,
    /// Symbol names shared across parties when `kind` is [`SharedSymbol`].
    pub shared_symbols: Vec<String>,
    /// Set when the head_sha check failed for every party that failed. Never
    /// inferred from `reason`, which carries user-chosen worktree names and
    /// source text. Not part of the wire shape.
    #[serde(skip)]
    pub stale: bool,
}

impl EntityCollisionVerdict {
    /// The symbol join was stale rather than absent: every party that could not
    /// be joined had an index built for another HEAD than the one being read.
    /// The overlap is real but its symbols could not be compared, so the
    /// finding is inconclusive, not a conflict. A verdict with any other party
    /// failure is not stale, whatever its text says.
    pub fn index_stale(&self) -> bool {
        self.kind == EntityCollisionKind::FileLevel && self.stale
    }
}

/// Why one party could not be joined, and whether that was only a stale index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartyFailure {
    pub reason: String,
    /// The index was built for another HEAD than this worktree's.
    pub stale: bool,
}

impl PartyFailure {
    fn other(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            stale: false,
        }
    }
}

/// What one party's hunks touched: join keys plus the old-side footprint each
/// hunk occupies, so two parties' edits can be compared for adjacency.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PartyEdits {
    pub keys: BTreeSet<String>,
    /// Closed intervals in line-boundary coordinates: modified lines `s..=e`
    /// occupy `[s, e + 1]`, an insertion after line `N` is the point `N + 1`.
    /// Git conflicts two edits whose intervals intersect, touching included.
    pub footprint: Vec<(u64, u64)>,
}

#[cfg(test)]
impl From<BTreeSet<String>> for PartyEdits {
    fn from(keys: BTreeSet<String>) -> Self {
        Self {
            keys,
            footprint: Vec::new(),
        }
    }
}

/// Inclusive line range on the old (HEAD) side of a unified diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

/// One hunk of a `--unified=0` diff, located on the old (HEAD) side.
///
/// A hunk that is not a modification must still be a hunk: an addition-only
/// edit has no old-side lines but does land inside some symbol, and a header
/// the parser cannot read is a hunk nobody has located. Both used to be
/// dropped, which made a party that only inserted code look like it touched
/// nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OldSideHunk {
    /// Old lines `start..=end` were changed or removed.
    Modified(LineRange),
    /// Lines were inserted after old line `after` (`-N,0`); `0` is before line 1.
    Insertion { after: u32 },
    /// An `@@` header that is malformed, overflows, or is a combined-diff header.
    Unparsed,
}

/// Most hunks one party's diff may carry before the join gives up. Bounds the
/// containment work, which is hunks times indexed symbols.
const MAX_HUNKS_PER_PARTY: usize = 4096;

/// Kind of the whole-file node every indexed file carries. It spans every
/// line, so keeping it would make any two edits to one file "share" it.
const FILE_KIND: &str = "File";

/// One indexed symbol with a usable span, in line terms.
struct Span<'a> {
    /// Position in the filtered span list, which `join_keys` indexes by.
    index: usize,
    start: u32,
    end: u32,
    symbol: &'a CodeintelFileSymbol,
}

impl Span<'_> {
    fn size(&self) -> u32 {
        self.end - self.start
    }

    fn contains_lines(&self, first: u32, last: u32) -> bool {
        self.start <= first && last <= self.end
    }

    /// `self` lies inside `outer` and is not the same span.
    fn strictly_inside(&self, outer: &Span<'_>) -> bool {
        outer.start <= self.start
            && self.end <= outer.end
            && (outer.start, outer.end) != (self.start, self.end)
    }
}

/// The spans among `candidates` that no other candidate nests inside.
fn innermost<'a, 'b>(candidates: &[&'b Span<'a>]) -> Vec<&'b Span<'a>> {
    candidates
        .iter()
        .filter(|s| !candidates.iter().any(|t| t.strictly_inside(s)))
        .copied()
        .collect()
}

/// The identity both parties' edits are joined on, one per span: the qualified
/// name with its `{path}::` prefix removed. Bare names collide across unrelated
/// impls and classes (`A::new` against `B::new`); qualified names do not. An
/// empty qualified name falls back to `kind:enclosing-key/name`.
fn join_keys(path: &str, spans: &[Span<'_>]) -> Vec<String> {
    let prefix = format!("{path}::");
    // Containers before their members, so a member can name its enclosing key.
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(spans[i].size()));
    let mut keys = vec![String::new(); spans.len()];
    for (rank, &i) in order.iter().enumerate() {
        let symbol = spans[i].symbol;
        keys[i] = if symbol.qualified_name.is_empty() {
            let enclosing = order[..rank]
                .iter()
                .rev()
                .find(|&&j| spans[i].strictly_inside(&spans[j]))
                .map_or("", |&j| keys[j].as_str());
            format!("{}:{enclosing}/{}", symbol.kind, symbol.symbol_name)
        } else {
            symbol
                .qualified_name
                .strip_prefix(&prefix)
                .unwrap_or(&symbol.qualified_name)
                .to_string()
        };
    }
    keys
}

/// Why one hunk could not be placed in a symbol, worded for the reason string.
/// Never quotes the raw header: its trailing function context is arbitrary
/// source text, and [`EntityCollisionVerdict::index_stale`] string-matches
/// reasons.
fn unplaced(hunk: &OldSideHunk) -> String {
    match hunk {
        OldSideHunk::Modified(r) => format!(
            "hunk @@ -{},{} @@ (old lines {}-{}) is not inside indexed symbols",
            r.start,
            r.end - r.start + 1,
            r.start,
            r.end
        ),
        OldSideHunk::Insertion { after } => {
            format!("hunk @@ -{after},0 @@ (insertion after line {after}) maps to no symbol")
        }
        OldSideHunk::Unparsed => "a diff hunk header could not be parsed".into(),
    }
}

/// Join keys of the symbols a party's hunks touch, or why a hunk could not be
/// placed. Only the innermost symbol counts: editing a method does not touch
/// its class, but editing the class's own field or header lines does.
///
/// Every hunk must be placed. A modification must lie wholly inside indexed
/// symbols; an insertion after old line `N` must sit between two lines of one
/// symbol (`start <= N < end`). Anything else (imports, gaps between items, an
/// unreadable header) is an edit this join cannot attribute, so it fails the
/// party instead of reporting a clean split.
#[cfg(test)]
pub fn symbols_touching_ranges(
    path: &str,
    symbols: &[CodeintelFileSymbol],
    hunks: &[OldSideHunk],
) -> Result<BTreeSet<String>, String> {
    party_edits(path, symbols, hunks).map(|edits| edits.keys)
}

/// [`symbols_touching_ranges`] plus where each hunk sits, for the cross-party
/// adjacency check.
pub fn party_edits(
    path: &str,
    symbols: &[CodeintelFileSymbol],
    hunks: &[OldSideHunk],
) -> Result<PartyEdits, String> {
    let spans: Vec<Span<'_>> = symbols
        .iter()
        .filter(|s| s.kind != FILE_KIND && s.span_start_line != 0 && s.span_end_line != 0)
        .enumerate()
        .map(|(index, symbol)| Span {
            index,
            start: symbol.span_start_line,
            end: symbol.span_end_line.max(symbol.span_start_line),
            symbol,
        })
        .collect();
    let keys = join_keys(path, &spans);
    let key_of = |span: &Span<'_>| keys[span.index].clone();

    // Every line inside one segment is covered by the same symbols, so a
    // modified hunk is resolved against segments rather than line by line.
    let mut bounds: Vec<u32> = spans
        .iter()
        .flat_map(|s| [s.start, s.end.saturating_add(1)])
        .collect();
    bounds.sort_unstable();
    bounds.dedup();
    let segments: Vec<(u32, u32, Vec<&Span<'_>>)> = bounds
        .windows(2)
        .map(|w| {
            let (first, last) = (w[0], w[1] - 1);
            let cover: Vec<&Span<'_>> = spans
                .iter()
                .filter(|s| s.contains_lines(first, last))
                .collect();
            (first, last, innermost(&cover))
        })
        .collect();

    let mut out = BTreeSet::new();
    let mut footprint = Vec::with_capacity(hunks.len());
    for hunk in hunks {
        match *hunk {
            OldSideHunk::Modified(range) => {
                footprint.push((u64::from(range.start), u64::from(range.end) + 1));
                let mut covered = 0u64;
                let mut touched = Vec::new();
                for (first, last, owners) in &segments {
                    if *last < range.start || range.end < *first || owners.is_empty() {
                        continue;
                    }
                    covered += u64::from(range.end.min(*last) - range.start.max(*first)) + 1;
                    touched.extend(owners.iter().copied());
                }
                if covered != u64::from(range.end - range.start) + 1 {
                    return Err(unplaced(hunk));
                }
                out.extend(touched.into_iter().map(key_of));
            }
            OldSideHunk::Insertion { after } => {
                footprint.push((u64::from(after) + 1, u64::from(after) + 1));
                let candidates: Vec<&Span<'_>> = spans
                    .iter()
                    .filter(|s| after >= 1 && s.contains_lines(after, after.saturating_add(1)))
                    .collect();
                let owners = innermost(&candidates);
                if owners.is_empty() {
                    return Err(unplaced(hunk));
                }
                out.extend(owners.into_iter().map(key_of));
            }
            OldSideHunk::Unparsed => return Err(unplaced(hunk)),
        }
    }
    footprint.sort_unstable();
    Ok(PartyEdits {
        keys: out,
        footprint,
    })
}

/// The first line where two parties' footprints touch or overlap, if any.
/// `a` and `b` are sorted by start; one pass over `b` with a running maximum
/// end over `a`, so the cost is a sort, not hunks times hunks.
fn first_contact(a: &[(u64, u64)], b: &[(u64, u64)]) -> Option<u64> {
    let mut reach: Option<(u64, u64)> = None;
    let mut next = 0;
    for &(lo, hi) in b {
        while next < a.len() && a[next].0 <= hi {
            let end = reach.map_or(a[next].1, |(_, e)| e.max(a[next].1));
            reach = Some((a[next].0, end));
            next += 1;
        }
        if let Some((_, end)) = reach {
            if end >= lo {
                // Some earlier-starting interval of `a` reaches `lo`; the
                // contact is at the later of the two starts.
                let start = a[..next]
                    .iter()
                    .filter(|&&(_, e)| e >= lo)
                    .map(|&(s, _)| s)
                    .next()
                    .unwrap_or(lo);
                return Some(lo.max(start));
            }
        }
    }
    None
}

/// Pure classifier over already-gathered per-party symbol sets.
///
/// `failures` are reasons a party could not be joined; any failure forces
/// [`EntityCollisionKind::FileLevel`] so a stale index never borrows the
/// disjoint-symbol wording.
///
/// Plain string failures are never stale: staleness is a typed fact about a
/// party, see [`classify_parties`].
#[cfg(test)]
pub fn classify_party_symbols(
    path: &str,
    party_symbols: &[BTreeSet<String>],
    failures: &[String],
) -> EntityCollisionVerdict {
    let edits: Vec<PartyEdits> = party_symbols
        .iter()
        .cloned()
        .map(PartyEdits::from)
        .collect();
    let failures: Vec<PartyFailure> = failures.iter().cloned().map(PartyFailure::other).collect();
    classify_parties(path, &edits, &failures)
}

/// The classifier proper. Any failed party gives FileLevel, and the verdict is
/// stale only when every failed party failed for staleness. Otherwise the
/// parties' symbol sets are intersected, and a Disjoint split is downgraded to
/// FileLevel when two parties' edits touch on the old side, because git
/// conflicts touching edits whichever symbols own them.
pub fn classify_parties(
    path: &str,
    parties: &[PartyEdits],
    failures: &[PartyFailure],
) -> EntityCollisionVerdict {
    let file_level = |reason: String, stale: bool| EntityCollisionVerdict {
        path: path.to_string(),
        kind: EntityCollisionKind::FileLevel,
        reason,
        shared_symbols: Vec::new(),
        stale,
    };
    if !failures.is_empty() {
        let reasons: Vec<&str> = failures.iter().map(|f| f.reason.as_str()).collect();
        return file_level(reasons.join("; "), failures.iter().all(|f| f.stale));
    }
    if parties.len() < 2 {
        return file_level(
            "fewer than two worktrees produced symbol sets for this path".into(),
            false,
        );
    }

    // A symbol two parties both edited is a collision whatever the others did,
    // so this is the union of the pairwise intersections, not the symbols every
    // party touched.
    let mut shared = BTreeSet::new();
    for (i, a) in parties.iter().enumerate() {
        for b in &parties[i + 1..] {
            shared.extend(a.keys.intersection(&b.keys).cloned());
        }
    }
    if !shared.is_empty() {
        let names: Vec<String> = shared.into_iter().collect();
        return EntityCollisionVerdict {
            path: path.to_string(),
            kind: EntityCollisionKind::SharedSymbol,
            reason: format!(
                "{path} shares symbol(s) {} across worktrees; editing it here will conflict when these branches meet",
                names.join(", ")
            ),
            shared_symbols: names,
            stale: false,
        };
    }
    let contact = parties.iter().enumerate().find_map(|(i, a)| {
        parties[i + 1..]
            .iter()
            .find_map(|b| first_contact(&a.footprint, &b.footprint))
    });
    if let Some(line) = contact {
        return file_level(
            format!(
                "edits touch at line {line} of {path}: the symbols differ but git merges adjacent changes as a conflict"
            ),
            false,
        );
    }
    EntityCollisionVerdict {
        path: path.to_string(),
        kind: EntityCollisionKind::DisjointSymbols,
        reason: format!(
            "{path} is dirty in multiple worktrees, but no shared symbol was seen on the old-side ranges — file overlap, not a merge promise"
        ),
        shared_symbols: Vec::new(),
        stale: false,
    }
}

/// Classify one overlapping path already reported by the porcelain scan.
///
/// Opens each party's own DevMap store and diffs that worktree against its
/// own HEAD. Does not run inside [`super::collision_from_list`].
pub fn classify_overlapping_path(item: &CollisionItem) -> EntityCollisionVerdict {
    let mut parties = Vec::new();
    let mut failures = Vec::new();

    for party in &item.worktrees {
        match party_old_side_symbols(&item.path, party, None) {
            Ok(edits) => parties.push(edits),
            Err(failure) => failures.push(PartyFailure {
                reason: format!("{}: {}", short_party(party), failure.reason),
                stale: failure.stale,
            }),
        }
    }

    classify_parties(&item.path, &parties, &failures)
}

fn short_party(party: &CollisionParty) -> String {
    Path::new(&party.path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(party.path.as_str())
        .to_string()
}

/// One party's edits to `file_path`, joined to its symbols. With `incoming`,
/// the text the tool call is about to replace is folded into the party's
/// hunks first: the edit is this party's too, and a party whose dirty edits
/// are elsewhere would otherwise read as disjoint from a symbol it is about to
/// change.
fn party_old_side_symbols(
    file_path: &str,
    party: &CollisionParty,
    incoming: Option<&[String]>,
) -> Result<PartyEdits, PartyFailure> {
    let repo = validate_repo(&party.path).map_err(|e| PartyFailure::other(e.into_message()))?;
    let repo_str = repo.to_string_lossy();

    if !GitReader::is_tracked(&repo_str, file_path).unwrap_or(false) {
        return Err(PartyFailure::other(
            "file is untracked (no HEAD blob for an old-side join)",
        ));
    }

    let head = GitReader::head_id(&repo_str).map_err(|e| PartyFailure::other(e.into_message()))?;
    let spans = codeintel::symbols_for_file(&repo_str, file_path, &head);
    if !spans.available {
        return Err(PartyFailure::other(
            spans
                .reason
                .unwrap_or_else(|| "DevMap store unavailable".into()),
        ));
    }
    if !spans.head_matches {
        // The one typed staleness fact: the index was built for another HEAD.
        return Err(PartyFailure {
            reason: spans
                .reason
                .unwrap_or_else(|| format!("index head_sha does not equal HEAD ({head})")),
            stale: true,
        });
    }
    if spans.items.iter().any(|s| s.span_start_line == 0) {
        return Err(PartyFailure::other(spans.reason.unwrap_or_else(|| {
            "symbol line spans could not be computed from the indexed blob".into()
        })));
    }
    if spans.truncated {
        // A capped page is a prefix of the file's symbols; an edit outside it
        // would read as touching nothing.
        return Err(PartyFailure::other(format!(
            "symbol list truncated ({} of {} symbols shown)",
            spans.shown, spans.total
        )));
    }
    if spans.items.iter().all(|s| s.kind == FILE_KIND) {
        // An unindexed path, or the new path of a rename: no rows is not a
        // disjoint set.
        return Err(PartyFailure::other("no indexed symbols for path at HEAD"));
    }

    let (mut hunks, head_diff) =
        old_side_diff(&repo, file_path).map_err(|e| PartyFailure::other(e.into_message()))?;
    if let Some(replaced) = incoming {
        let content = read_working_text(&repo, file_path).map_err(PartyFailure::other)?;
        for old in replaced {
            hunks.push(incoming_hunk(&content, old, &head_diff).map_err(PartyFailure::other)?);
        }
        hunks.sort();
        hunks.dedup();
    }
    if hunks.is_empty() {
        return Err(PartyFailure::other(
            "no hunks in the worktree diff (binary, mode-only or empty)",
        ));
    }
    party_edits(file_path, &spans.items, &hunks).map_err(PartyFailure::other)
}

/// Old-side hunks of the worktree against HEAD: the union of the staged and
/// the working-tree edits.
///
/// Both diffs are taken against `HEAD`, so every hunk is numbered against the
/// same HEAD blob the index map describes. A plain `git diff` would number the
/// unstaged hunks against the index, which a staged line-count change has
/// already shifted. The union is needed because each half alone misses edits:
/// `git diff HEAD` cannot see a change staged and then reverted in the working
/// tree, though the index still carries it into the next commit. A hunk both
/// show is counted once. HEAD missing (an unborn branch) makes git fail, which
/// fails the party.
///
/// The hunk cap applies to the union. The caller decides what no hunks at all
/// (a binary or mode-only change, or an empty diff) means: with no incoming
/// edit there is nothing to place, so no clean split to report.
///
/// The text of the `git diff HEAD` half comes back with the hunks: it is the
/// only one that also numbers the working-tree side, which is what places an
/// incoming edit in HEAD's coordinates.
fn old_side_diff(repo: &Path, file_path: &str) -> Result<(Vec<OldSideHunk>, String), CommandError> {
    // `:(literal)` stops `*?[` in a filename from widening the pathspec.
    // `--no-textconv` keeps a configured textconv filter from rewriting the
    // diff into something whose line numbers are not the file's.
    let spec = format!(":(literal){file_path}");
    let mut hunks = Vec::new();
    let mut head_diff = String::new();
    for staged in [true, false] {
        let mut args = vec!["-c", "core.quotepath=off", "diff"];
        if staged {
            args.push("--cached");
        }
        args.extend(["--no-textconv", "--unified=0", "HEAD", "--", spec.as_str()]);
        let text = git_text(repo, &args)
            .map_err(|e| e.wrap(|e| format!("git diff against HEAD failed: {e}")))?;
        // A diff that says the file differs but prints no hunk header (binary,
        // or anything this parser was not written for) is not "no edits": the
        // working lines cannot be numbered, so no edit can be placed against
        // HEAD. It fails the party, which asks.
        if !text.trim().is_empty() && !text.lines().any(|line| line.starts_with("@@")) {
            return Err(
                "the diff against HEAD has no hunks to number lines by (binary or unparsed), so the edit cannot be placed"
                    .into(),
            );
        }
        hunks.extend(parse_old_side_ranges(&text));
        if !staged {
            head_diff = text;
        }
    }
    hunks.sort();
    hunks.dedup();
    if hunks.len() > MAX_HUNKS_PER_PARTY {
        return Err(format!("too many hunks (more than {MAX_HUNKS_PER_PARTY})").into());
    }
    Ok((hunks, head_diff))
}

/// The largest working file an incoming edit is located in. A source file past
/// this is not read, and an edit that cannot be located is asked about.
const MAX_INCOMING_FILE_BYTES: usize = 4 * 1024 * 1024;

fn read_working_text(repo: &Path, file_path: &str) -> Result<String, String> {
    let bytes =
        crate::fs_entry::read_bounded_nofollow(&repo.join(file_path), MAX_INCOMING_FILE_BYTES)
            .map_err(|error| format!("the file could not be read to place the edit ({error:?})"))?;
    String::from_utf8(bytes)
        .map_err(|_| "the file is not UTF-8, so the edit cannot be placed".into())
}

/// One `@@ -old +new @@` header of `git diff HEAD --unified=0`, both sides.
#[derive(Clone, Copy)]
struct DiffHeader {
    old_start: u32,
    old_lines: u32,
    new_start: u32,
    new_lines: u32,
}

fn parse_diff_headers(diff: &str) -> Result<Vec<DiffHeader>, String> {
    let side = |token: &str, mark: char| -> Option<(u32, u32)> {
        let body = token.strip_prefix(mark)?;
        match body.split_once(',') {
            Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
            None => Some((body.parse().ok()?, 1)),
        }
    };
    diff.lines()
        .filter(|line| line.starts_with("@@"))
        .map(|line| {
            let mut tokens = line
                .strip_prefix("@@ ")
                .unwrap_or_default()
                .split_whitespace();
            let old = tokens.next().and_then(|t| side(t, '-'));
            let new = tokens.next().and_then(|t| side(t, '+'));
            match (old, new) {
                (Some((old_start, old_lines)), Some((new_start, new_lines))) => Ok(DiffHeader {
                    old_start,
                    old_lines,
                    new_start,
                    new_lines,
                }),
                _ => Err(
                    "a diff hunk header could not be parsed, so the edit cannot be placed"
                        .to_string(),
                ),
            }
        })
        .collect()
}

/// Where a working-tree line sits against HEAD.
enum Placed {
    /// An unchanged line, at this HEAD line.
    Unchanged(u32),
    /// A line inside one of the party's own hunks.
    Inside(DiffHeader),
}

fn place_line(line: u32, headers: &[DiffHeader]) -> Placed {
    let mut delta: i64 = 0;
    for header in headers {
        // A pure deletion's `new_start` is the line before it.
        let first = if header.new_lines == 0 {
            header.new_start + 1
        } else {
            header.new_start
        };
        if line < first {
            break;
        }
        if header.new_lines > 0 && line < header.new_start + header.new_lines {
            return Placed::Inside(*header);
        }
        delta += i64::from(header.new_lines) - i64::from(header.old_lines);
    }
    Placed::Unchanged(u32::try_from(i64::from(line) - delta).unwrap_or(0))
}

/// The HEAD-side hunk an incoming `old_string` occupies, found in the
/// working-tree file `content`.
///
/// The text must be found exactly once: none means the edit will not apply as
/// described, and several means which one it hits is a guess. Either is
/// reported as an error, which fails the party and so asks the user, rather
/// than guessing a range that could read as disjoint.
fn incoming_hunk(content: &str, old: &str, head_diff: &str) -> Result<OldSideHunk, String> {
    if old.is_empty() {
        return Err("the edit names no text to replace, so it cannot be placed".into());
    }
    let mut found = content.match_indices(old).map(|(at, _)| at);
    let (Some(at), None) = (found.next(), found.next()) else {
        return Err(
            "the edit's old_string is not found exactly once in the file, so it cannot be placed"
                .into(),
        );
    };
    let line_of = |offset: usize| -> Result<u32, String> {
        u32::try_from(content[..offset].matches('\n').count() + 1)
            .map_err(|_| "the file has too many lines to place the edit".to_string())
    };
    let first = line_of(at)?;
    // A trailing newline ends the last line rather than starting another.
    let body = old.strip_suffix('\n').unwrap_or(old);
    let last = line_of(at + body.len())?.max(first);

    let headers = parse_diff_headers(head_diff)?;
    let lower = match place_line(first, &headers) {
        Placed::Unchanged(line) => line,
        Placed::Inside(h) if h.old_lines > 0 => h.old_start,
        Placed::Inside(h) => h.old_start + 1,
    };
    let upper = match place_line(last, &headers) {
        Placed::Unchanged(line) => line,
        Placed::Inside(h) if h.old_lines > 0 => h.old_start + h.old_lines - 1,
        Placed::Inside(h) => h.old_start,
    };
    if lower == 0 || upper == 0 {
        return Err("the edit could not be placed against HEAD".into());
    }
    Ok(if lower <= upper {
        OldSideHunk::Modified(LineRange {
            start: lower,
            end: upper,
        })
    } else {
        // Wholly inside lines this party has already added: no old line of
        // its own, an insertion after the one above.
        OldSideHunk::Insertion { after: upper }
    })
}

/// The tool call a hook is judging, as far as its payload says what it does to
/// the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncomingEdit {
    /// `Write` or `NotebookEdit`: the file, or a cell of it, is replaced, so
    /// there is no narrower range to compare.
    WholeFile,
    /// `Edit` or `MultiEdit`: each `old_string` is located in the file.
    Replacing(Vec<String>),
}

/// The verdict for an edit of `path` that `here` is about to make, against the
/// edits `others` already hold.
///
/// `here` is a party whether or not it is dirty: the incoming edit is folded
/// into its hunks, so an edit into a symbol another party changed is a shared
/// symbol even when everything `here` has changed so far is elsewhere. A whole
/// file write is file-level without reading anything. An edit that cannot be
/// located fails `here`, which is file-level and so asks, never disjoint.
pub fn classify_incoming(
    path: &str,
    others: &[CollisionParty],
    here: &CollisionParty,
    incoming: &IncomingEdit,
) -> EntityCollisionVerdict {
    let replaced = match incoming {
        IncomingEdit::WholeFile => {
            return EntityCollisionVerdict {
                path: path.to_string(),
                kind: EntityCollisionKind::FileLevel,
                reason: "this write replaces the whole file".into(),
                shared_symbols: Vec::new(),
                stale: false,
            }
        }
        IncomingEdit::Replacing(replaced) => replaced.as_slice(),
    };
    let mut parties = Vec::new();
    let mut failures = Vec::new();
    for (party, edit) in others
        .iter()
        .map(|party| (party, None))
        .chain(std::iter::once((here, Some(replaced))))
    {
        match party_old_side_symbols(path, party, edit) {
            Ok(edits) => parties.push(edits),
            Err(failure) => failures.push(PartyFailure {
                reason: format!("{}: {}", short_party(party), failure.reason),
                stale: failure.stale,
            }),
        }
    }
    classify_parties(path, &parties, &failures)
}

/// Parse every `@@ -old_start[,old_lines] +… @@` header into one old-side hunk.
///
/// Exactly one hunk per `@@` line: `-N,0` is an insertion after old line `N`, a
/// missing count is 1, and a header that is malformed, overflows `u32`, or is a
/// combined-diff (`@@@`) header is [`OldSideHunk::Unparsed`] rather than
/// skipped. `str::lines` already strips the `\r` of a CRLF diff.
pub fn parse_old_side_ranges(diff: &str) -> Vec<OldSideHunk> {
    diff.lines()
        .filter(|line| line.starts_with("@@"))
        .map(|line| parse_hunk_header(line).unwrap_or(OldSideHunk::Unparsed))
        .collect()
}

fn parse_hunk_header(line: &str) -> Option<OldSideHunk> {
    let mut tokens = line.strip_prefix("@@ ")?.split_whitespace();
    let old = tokens.next()?.strip_prefix('-')?;
    // The new side must be present, or this is not a two-sided header.
    tokens.next()?.strip_prefix('+')?;
    let (start, count) = match old.split_once(',') {
        Some((start, count)) => (start.parse::<u32>().ok()?, count.parse::<u32>().ok()?),
        None => (old.parse::<u32>().ok()?, 1),
    };
    match count {
        0 => Some(OldSideHunk::Insertion { after: start }),
        _ if start == 0 => None,
        _ => Some(OldSideHunk::Modified(LineRange {
            start,
            end: start.checked_add(count - 1)?,
        })),
    }
}

/// Attach a symbol verdict to the row `focus` names, and to no other.
///
/// One path only, because classifying costs git diffs and a DevMap open per
/// party. A `focus` that names no row, or no `focus` at all, classifies nothing:
/// falling back to the first row (sorted by path) spent that cost on a file
/// nobody asked about and attached its verdict to the wrong answer. A row held
/// by one worktree has nothing to compare, so it is left unclassified too.
pub fn enrich_item(
    mut risk: crate::insights::CollisionRisk,
    focus: Option<&str>,
) -> crate::insights::CollisionRisk {
    let index = focus
        .and_then(|path| risk.items.iter().position(|item| item.path == path))
        .filter(|&index| risk.items[index].worktrees.len() > 1);
    if let Some(index) = index {
        let verdict = classify_overlapping_path(&risk.items[index]);
        risk.items[index].entity = Some(verdict);
    }
    risk
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codeintel::tests as codeintel_tests;

    fn sym_q(name: &str, qualified: &str, kind: &str, start: u32, end: u32) -> CodeintelFileSymbol {
        CodeintelFileSymbol {
            symbol_name: name.into(),
            qualified_name: qualified.into(),
            kind: kind.into(),
            span_start_line: start,
            span_end_line: end,
        }
    }

    fn sym(name: &str, start: u32, end: u32) -> CodeintelFileSymbol {
        sym_q(name, name, "Function", start, end)
    }

    fn modified(start: u32, end: u32) -> OldSideHunk {
        OldSideHunk::Modified(LineRange { start, end })
    }

    /// The keys `hunks` touch in `symbols`, for the path `"src/x.rs"`.
    fn touching(
        symbols: &[CodeintelFileSymbol],
        hunks: &[OldSideHunk],
    ) -> Result<BTreeSet<String>, String> {
        symbols_touching_ranges("src/x.rs", symbols, hunks)
    }

    #[test]
    fn non_overlapping_spans_do_not_share_symbols() {
        let left = touching(&[sym("alpha", 1, 10)], &[modified(1, 5)]).unwrap();
        let right = touching(&[sym("beta", 20, 30)], &[modified(20, 25)]).unwrap();
        let verdict = classify_party_symbols("src/x.rs", &[left, right], &[]);
        assert_eq!(verdict.kind, EntityCollisionKind::DisjointSymbols);
        assert!(
            !verdict
                .reason
                .contains("will conflict when these branches meet"),
            "disjoint must not use the shared-symbol collision wording: {}",
            verdict.reason
        );
        assert!(
            verdict.reason.contains("not a merge promise"),
            "{}",
            verdict.reason
        );
    }

    #[test]
    fn overlapping_spans_share_a_symbol() {
        let left = touching(&[sym("shared", 1, 20)], &[modified(5, 8)]).unwrap();
        let right = touching(&[sym("shared", 1, 20)], &[modified(10, 12)]).unwrap();
        let verdict = classify_party_symbols("src/x.rs", &[left, right], &[]);
        assert_eq!(verdict.kind, EntityCollisionKind::SharedSymbol);
        assert_eq!(verdict.shared_symbols, vec!["shared".to_string()]);
        assert!(
            verdict
                .reason
                .contains("will conflict when these branches meet"),
            "{}",
            verdict.reason
        );
    }

    #[test]
    fn head_sha_mismatch_failure_stays_file_level_not_disjoint() {
        let left = BTreeSet::from(["alpha".into()]);
        let right = BTreeSet::from(["beta".into()]);
        let verdict = classify_party_symbols(
            "src/x.rs",
            &[left, right],
            &["wt-a: indexed generation head_sha abc is not the requested def".into()],
        );
        assert_eq!(verdict.kind, EntityCollisionKind::FileLevel);
        assert!(
            !verdict.reason.contains("not a merge promise"),
            "a failed join must not use the disjoint-symbol wording: {}",
            verdict.reason
        );
        assert!(
            verdict.reason.contains("head_sha"),
            "file-level reason should carry the mismatch: {}",
            verdict.reason
        );
    }

    #[test]
    fn parse_old_side_ranges_keeps_pure_additions_as_insertions() {
        // The old test asserted that `-0,0` (and any `-N,0`) vanished. That
        // encoded the bug: an addition-only edit has no old-side lines but it
        // is still an edit, so dropping it let a party that only inserted code
        // read as touching nothing and join as "disjoint".
        let diff = "\
@@ -10,2 +10,3 @@
-old
 context
+new
@@ -0,0 +1,2 @@
+brand
+new
@@ -20,0 +23 @@
+more
";
        assert_eq!(
            parse_old_side_ranges(diff),
            vec![
                modified(10, 11),
                OldSideHunk::Insertion { after: 0 },
                OldSideHunk::Insertion { after: 20 },
            ]
        );
    }

    #[test]
    fn a_missing_count_is_one_and_crlf_headers_parse() {
        let diff = "@@ -5 +5 @@ fn head_sha() {\r\n-x\r\n+y\r\n@@ -7,0 +8 @@\r\n+z\r\n";
        assert_eq!(
            parse_old_side_ranges(diff),
            vec![modified(5, 5), OldSideHunk::Insertion { after: 7 }]
        );
    }

    #[test]
    fn malformed_and_combined_headers_are_unparsed_never_skipped() {
        for header in [
            "@@@ -1,2 -1,2 +1,3 @@@",
            "@@ -a,2 +1 @@",
            "@@ -1,99999999999 +1 @@",
            "@@ -4294967295,2 +1 @@",
            "@@ -0,3 +1 @@",
            "@@ -1,2 @@",
            "@@-1 +1 @@",
            "@@",
        ] {
            assert_eq!(
                parse_old_side_ranges(header),
                vec![OldSideHunk::Unparsed],
                "{header}"
            );
        }
    }

    #[test]
    fn an_unparsed_hunk_fails_the_party_without_quoting_the_header() {
        let reason = touching(&[sym("a", 1, 9)], &[OldSideHunk::Unparsed]).unwrap_err();
        assert!(reason.contains("could not be parsed"), "{reason}");
        // The reason must not trip `index_stale`'s string match.
        let verdict = classify_party_symbols("x.rs", &[], &[reason]);
        assert!(!verdict.index_stale(), "{}", verdict.reason);
    }

    mod hunk_parser_properties {
        use super::super::*;
        use proptest::prelude::*;

        proptest! {
            /// Random `@@`-ish lines, including oversized numbers, missing
            /// counts, combined headers and CRLF: never a panic, exactly one
            /// hunk per `@@` line, and the Unparsed ones are the ones that
            /// cannot be located.
            #[test]
            fn every_header_classifies_and_nothing_panics(
                lines in proptest::collection::vec(
                    "(@@|@@@)? ?[-+ ]?[0-9a-z,]{0,14} ?[-+]?[0-9,]{0,12} ?(@@|@@@)?[ a-z_]{0,8}\r?",
                    0..24,
                ),
            ) {
                let diff = lines.join("\n");
                let headers = diff.lines().filter(|l| l.starts_with("@@")).count();
                let hunks = parse_old_side_ranges(&diff);
                prop_assert_eq!(hunks.len(), headers);
                for hunk in hunks {
                    if let OldSideHunk::Modified(r) = hunk {
                        prop_assert!(r.start >= 1 && r.start <= r.end);
                    }
                }
            }
        }
    }

    #[test]
    fn symbols_nest_to_the_innermost_and_the_file_symbol_is_ignored() {
        let symbols = [
            sym_q("x.rs", "src/x.rs::", "File", 1, 30),
            sym_q("C", "src/x.rs::C", "Class", 1, 12),
            sym_q("one", "src/x.rs::C.one", "Method", 3, 5),
            sym_q("two", "src/x.rs::C.two", "Method", 7, 9),
        ];
        let keys = |hunks: &[OldSideHunk]| touching(&symbols, hunks).unwrap();
        // A method edit does not touch its class.
        assert_eq!(
            keys(&[modified(4, 4)]),
            BTreeSet::from(["C.one".to_string()])
        );
        // The class's own field line does, and not the methods.
        assert_eq!(keys(&[modified(2, 2)]), BTreeSet::from(["C".to_string()]));
        // Across a method and the class lines between: both, fully covered.
        assert_eq!(
            keys(&[modified(5, 7)]),
            BTreeSet::from(["C.one".to_string(), "C".to_string(), "C.two".to_string()])
        );
        // Lines in no non-File symbol are unmapped even though File spans them.
        assert!(touching(&symbols, &[modified(20, 21)]).is_err());
        // Identical spans add both keys.
        let twins = [
            sym_q("a", "src/x.rs::a", "Function", 1, 3),
            sym_q("b", "src/x.rs::b", "Function", 1, 3),
        ];
        assert_eq!(
            touching(&twins, &[OldSideHunk::Insertion { after: 1 }]).unwrap(),
            BTreeSet::from(["a".to_string(), "b".to_string()])
        );
    }

    #[test]
    fn an_empty_qualified_name_falls_back_to_kind_enclosing_key_and_name() {
        let symbols = [
            sym_q("C", "src/x.rs::C", "Class", 1, 9),
            sym_q("m", "", "Method", 3, 5),
        ];
        assert_eq!(
            touching(&symbols, &[modified(4, 4)]).unwrap(),
            BTreeSet::from(["Method:C/m".to_string()])
        );
    }

    fn risk_with(paths: &[&str]) -> crate::insights::CollisionRisk {
        crate::insights::CollisionRisk {
            ok: true,
            error: String::new(),
            overlapping_files: paths.len() as u32,
            worktrees_involved: 2,
            scanned_worktrees: 2,
            unscanned_worktrees: 0,
            failed_worktrees: 0,
            truncated: false,
            items: paths
                .iter()
                .map(|path| crate::insights::CollisionItem {
                    path: (*path).to_string(),
                    worktrees: ["/nonexistent/gp-a", "/nonexistent/gp-b"]
                        .iter()
                        .map(|path| crate::insights::CollisionParty {
                            path: (*path).to_string(),
                            branch: None,
                            agent_kind: String::new(),
                        })
                        .collect(),
                    entity: None,
                })
                .collect(),
            shared_worktree_files: 0,
            shared_worktrees: Vec::new(),
            sessions_ok: true,
            sessions_error: String::new(),
        }
    }

    /// The hook asks about the file being edited. Rows are sorted by path, so
    /// enriching the first one spent two git diffs and a DevMap open on an
    /// unrelated file and left the edited path with no verdict.
    #[test]
    fn the_verdict_goes_to_the_focused_path_not_the_first_row() {
        let enriched = enrich_item(risk_with(&["a.rs", "b.rs", "c.rs"]), Some("b.rs"));
        let entities: Vec<bool> = enriched.items.iter().map(|i| i.entity.is_some()).collect();
        assert_eq!(entities, vec![false, true, false]);
        // No focus, or a focus naming no row, classifies nothing: it never
        // falls back to row 0.
        for focus in [None, Some("zzz.rs"), Some("")] {
            let absent = enrich_item(risk_with(&["a.rs", "b.rs"]), focus);
            assert!(
                absent.items.iter().all(|item| item.entity.is_none()),
                "focus {focus:?} classified a row it did not name"
            );
        }
        assert!(enrich_item(risk_with(&[]), Some("a.rs")).items.is_empty());
    }

    /// The row the caller asked about is far past the cap's worth of rows, and
    /// a row held by one worktree has nothing to compare: neither is
    /// classified by falling back to another row.
    #[test]
    fn an_absent_focus_over_the_row_cap_and_a_lone_holder_classify_nothing() {
        let names: Vec<String> = (0..40).map(|n| format!("f{n:02}.rs")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let absent = enrich_item(risk_with(&refs), Some("f39.rs-not-kept"));
        assert!(absent.items.iter().all(|item| item.entity.is_none()));
        let mut lone = risk_with(&["only.rs"]);
        lone.items[0].worktrees.truncate(1);
        let lone = enrich_item(lone, Some("only.rs"));
        assert!(
            lone.items[0].entity.is_none(),
            "one holder, nothing to compare"
        );
    }

    /* ── Real git worktrees + seeded DevMap stores ────────────────────────── */

    const FILE: &str = "lib.rs";

    /// Two impls whose methods share a bare name, then two free functions.
    /// Lines 5-9 are `impl A`, 10-14 are `impl B`; `impl` blocks are not
    /// symbols, so lines 5, 9, 10 and 14 belong to no symbol.
    const RUST_TEXT: &str = "\
use std::fmt;

struct A;
struct B;
impl A {
    fn new() -> A {
        A
    }
}
impl B {
    fn new() -> B {
        B
    }
}

fn foo() {
    let x = 1;
}

fn bar() {
    let y = 2;
}
";

    /// A class whose own field line sits between its methods' headers.
    const CLASS_TEXT: &str = "\
class C:
    field = 1
    def one(self):
        pass
    def two(self):
        pass
";

    fn rust_symbols() -> Vec<codeintel_tests::SeedSymbol<'static>> {
        vec![
            ("lib.rs", "lib.rs", "File", 1, 22),
            ("A", "lib.rs::A", "Struct", 3, 3),
            ("B", "lib.rs::B", "Struct", 4, 4),
            ("new", "lib.rs::A.new", "Method", 6, 8),
            ("new", "lib.rs::B.new", "Method", 11, 13),
            ("foo", "lib.rs::foo", "Function", 16, 18),
            ("bar", "lib.rs::bar", "Function", 20, 22),
        ]
    }

    fn class_symbols() -> Vec<codeintel_tests::SeedSymbol<'static>> {
        vec![
            ("lib.rs", "lib.rs", "File", 1, 6),
            ("C", "lib.rs::C", "Class", 1, 6),
            ("one", "lib.rs::C.one", "Method", 3, 4),
            ("two", "lib.rs::C.two", "Method", 5, 6),
        ]
    }

    /// One worktree: a git repo whose HEAD holds `text`, a store seeded for
    /// that HEAD, and the working tree then edited to `edited`.
    fn worktree(
        text: &str,
        symbols: &[codeintel_tests::SeedSymbol<'_>],
        edited: &str,
    ) -> tempfile::TempDir {
        use crate::test_support::{git_in, write};
        let dir = tempfile::TempDir::new().expect("tempdir");
        git_in(dir.path(), &["init", "-b", "main"]);
        write(dir.path(), FILE, text);
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-m", "base"]);
        let head = git_text(dir.path(), &["rev-parse", "HEAD"]).expect("rev-parse");
        codeintel_tests::seed_file_symbols(dir.path(), head.trim(), FILE, text, symbols);
        write(dir.path(), FILE, edited);
        dir
    }

    /// `text` with 1-based line `n` replaced by `new`.
    fn replace_line(text: &str, n: usize, new: &str) -> String {
        let mut lines: Vec<&str> = text.lines().collect();
        lines[n - 1] = new;
        lines.join("\n") + "\n"
    }

    /// `text` with `new` inserted after 1-based line `n` (`0` inserts first).
    fn insert_after(text: &str, n: usize, new: &str) -> String {
        let mut lines: Vec<&str> = text.lines().collect();
        lines.insert(n, new);
        lines.join("\n") + "\n"
    }

    fn verdict(left: &tempfile::TempDir, right: &tempfile::TempDir) -> EntityCollisionVerdict {
        let party = |dir: &tempfile::TempDir| CollisionParty {
            path: dir.path().to_string_lossy().into_owned(),
            branch: None,
            agent_kind: "claude".into(),
        };
        classify_overlapping_path(&CollisionItem {
            path: FILE.into(),
            worktrees: vec![party(left), party(right)],
            entity: None,
        })
    }

    fn rust_edit(symbols: &[codeintel_tests::SeedSymbol<'_>], edited: &str) -> tempfile::TempDir {
        worktree(RUST_TEXT, symbols, edited)
    }

    fn party_of(dir: &tempfile::TempDir) -> CollisionParty {
        CollisionParty {
            path: dir.path().to_string_lossy().into_owned(),
            branch: None,
            agent_kind: "claude".into(),
        }
    }

    fn replacing(old: &str) -> IncomingEdit {
        IncomingEdit::Replacing(vec![old.to_string()])
    }

    /// The folded 2026-10-14 finding. The other party changed `foo`; this one
    /// has only changed `bar`, and its incoming edit is into `foo`. Judged on
    /// the edits already dirty the two are disjoint and the first edit into
    /// `foo` is a notice; with the incoming edit folded in, it is the shared
    /// symbol, which asks.
    #[test]
    fn an_incoming_edit_into_another_partys_symbol_is_shared_though_this_partys_edits_are_elsewhere(
    ) {
        let syms = rust_symbols();
        let other = rust_edit(&syms, &replace_line(RUST_TEXT, 17, "    let x = 9;"));
        let here = rust_edit(&syms, &replace_line(RUST_TEXT, 21, "    let y = 9;"));
        assert_eq!(
            verdict(&other, &here).kind,
            EntityCollisionKind::DisjointSymbols,
            "without the incoming edit the parties read as disjoint"
        );
        let v = classify_incoming(
            FILE,
            &[party_of(&other)],
            &party_of(&here),
            &replacing("    let x = 1;"),
        );
        assert_eq!(v.kind, EntityCollisionKind::SharedSymbol, "{v:?}");
        assert_eq!(v.shared_symbols, vec!["foo".to_string()]);
    }

    /// The incoming edit is located in the working file and placed against
    /// HEAD: this party has inserted a line above, so `bar` sits one line
    /// lower than at HEAD, where the other party edited it.
    #[test]
    fn an_incoming_edit_is_placed_against_head_through_this_partys_own_shift() {
        let syms = rust_symbols();
        let other = rust_edit(&syms, &replace_line(RUST_TEXT, 21, "    let y = 9;"));
        let here = rust_edit(&syms, &insert_after(RUST_TEXT, 17, "    let extra = 0;"));
        assert_eq!(
            verdict(&other, &here).kind,
            EntityCollisionKind::DisjointSymbols
        );
        let v = classify_incoming(
            FILE,
            &[party_of(&other)],
            &party_of(&here),
            &replacing("    let y = 2;"),
        );
        assert_eq!(v.kind, EntityCollisionKind::SharedSymbol, "{v:?}");
        assert_eq!(v.shared_symbols, vec!["bar".to_string()]);
    }

    #[test]
    fn an_incoming_edit_into_a_different_symbol_stays_disjoint() {
        let syms = rust_symbols();
        let other = rust_edit(&syms, &replace_line(RUST_TEXT, 17, "    let x = 9;"));
        let here = rust_edit(&syms, &replace_line(RUST_TEXT, 7, "        A // x"));
        let v = classify_incoming(
            FILE,
            &[party_of(&other)],
            &party_of(&here),
            &replacing("    let y = 2;"),
        );
        assert_eq!(v.kind, EntityCollisionKind::DisjointSymbols, "{v:?}");
    }

    /// An edit whose target cannot be located is never disjoint: it asks.
    #[test]
    fn an_incoming_edit_that_cannot_be_located_fails_closed() {
        let syms = rust_symbols();
        let other = rust_edit(&syms, &replace_line(RUST_TEXT, 17, "    let x = 9;"));
        let here = rust_edit(&syms, &replace_line(RUST_TEXT, 21, "    let y = 9;"));
        for (what, old) in [
            ("absent", "    let nowhere = 1;"),
            ("ambiguous", "    let "),
            ("empty", ""),
        ] {
            let v = classify_incoming(FILE, &[party_of(&other)], &party_of(&here), &replacing(old));
            assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{what}: {v:?}");
            assert!(!v.index_stale(), "{what}: {v:?}");
            assert!(
                v.reason.contains("cannot be placed"),
                "{what}: {}",
                v.reason
            );
        }
        // A file that is not there to be read fails the same way.
        let missing = tempfile::TempDir::new().unwrap();
        let v = classify_incoming(
            FILE,
            &[party_of(&other)],
            &party_of(&missing),
            &replacing("    let x = 1;"),
        );
        assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{v:?}");
    }

    /// A `Write` replaces the file, so there is no narrower range to compare,
    /// and nothing is read to say so.
    #[test]
    fn an_incoming_whole_file_write_is_file_level_without_reading_anything() {
        let nowhere = CollisionParty {
            path: "/nonexistent/gp-write".into(),
            branch: None,
            agent_kind: String::new(),
        };
        let v = classify_incoming(
            FILE,
            std::slice::from_ref(&nowhere),
            &nowhere,
            &IncomingEdit::WholeFile,
        );
        assert_eq!(v.kind, EntityCollisionKind::FileLevel);
        assert!(!v.index_stale());
        assert!(v.reason.contains("whole file"), "{}", v.reason);
    }

    fn lines(n: u32) -> String {
        (1..=n).map(|i| format!("l{i}\n")).collect()
    }

    #[test]
    fn an_incoming_range_is_numbered_in_head_lines() {
        let text = lines(12);
        // No edits of this party's own: the working lines are HEAD's.
        assert_eq!(incoming_hunk(&text, "l10\n", "").unwrap(), modified(10, 10));
        assert_eq!(
            incoming_hunk(&text, "l3\nl4\nl5", "").unwrap(),
            modified(3, 5)
        );
        // Two HEAD lines (5-6) deleted above: working line 10 is HEAD's 12.
        let deleted = "@@ -5,2 +4,0 @@\n-l5\n-l6\n";
        assert_eq!(
            incoming_hunk(&text, "l10\n", deleted).unwrap(),
            modified(12, 12)
        );
        // A line of this party's own insertion has no HEAD line: an insertion.
        let inserted = "@@ -3,0 +4,2 @@\n+l4\n+l5\n";
        assert_eq!(
            incoming_hunk(&text, "l4\n", inserted).unwrap(),
            OldSideHunk::Insertion { after: 3 }
        );
        // Starting in this party's own change and running past it.
        let changed = "@@ -4,2 +4,2 @@\n-a\n-b\n+l4\n+l5\n";
        assert_eq!(
            incoming_hunk(&text, "l5\nl6\nl7", changed).unwrap(),
            modified(4, 7)
        );
        assert!(incoming_hunk(&text, "l1\n", "@@ nonsense @@\n").is_err());
    }

    #[test]
    fn methods_of_different_impls_sharing_a_bare_name_are_disjoint() {
        let syms = rust_symbols();
        let left = rust_edit(&syms, &replace_line(RUST_TEXT, 7, "        A // x"));
        let right = rust_edit(&syms, &replace_line(RUST_TEXT, 12, "        B // y"));
        let v = verdict(&left, &right);
        assert_eq!(v.kind, EntityCollisionKind::DisjointSymbols, "{v:?}");
    }

    #[test]
    fn two_insertions_into_the_same_function_share_it() {
        let syms = rust_symbols();
        // Left edits foo and inserts into bar; right only inserts into bar.
        let left = insert_after(
            &replace_line(RUST_TEXT, 17, "    let x = 9;"),
            21,
            "    let z = 3;",
        );
        let right = insert_after(RUST_TEXT, 21, "    let w = 4;");
        let v = verdict(&rust_edit(&syms, &left), &rust_edit(&syms, &right));
        assert_eq!(v.kind, EntityCollisionKind::SharedSymbol, "{v:?}");
        assert_eq!(v.shared_symbols, vec!["bar".to_string()]);
    }

    #[test]
    fn an_insertion_is_joined_to_its_symbol_not_dropped() {
        let syms = rust_symbols();
        let modifies_foo = rust_edit(&syms, &replace_line(RUST_TEXT, 17, "    let x = 9;"));
        let inserts_into_bar = rust_edit(&syms, &insert_after(RUST_TEXT, 21, "    let z = 3;"));
        let v = verdict(&modifies_foo, &inserts_into_bar);
        assert_eq!(v.kind, EntityCollisionKind::DisjointSymbols, "{v:?}");
    }

    #[test]
    fn a_header_edit_belongs_to_no_symbol_and_names_the_party() {
        let syms = rust_symbols();
        let imports = rust_edit(&syms, &replace_line(RUST_TEXT, 1, "use std::io;"));
        let in_foo = rust_edit(&syms, &replace_line(RUST_TEXT, 17, "    let x = 9;"));
        let v = verdict(&imports, &in_foo);
        assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{v:?}");
        let short = imports.path().file_name().unwrap().to_string_lossy();
        assert!(v.reason.contains(short.as_ref()), "{}", v.reason);
        assert!(v.reason.contains("@@ -1,1 @@"), "{}", v.reason);
        // The hook downgrades a stale-index FileLevel to a notice; this one is
        // an unattributable edit and must keep asking.
        assert!(!v.index_stale(), "{}", v.reason);
    }

    #[test]
    fn class_members_are_told_apart_and_the_file_symbol_is_not_shared() {
        let syms = class_symbols();
        let edit =
            |n: usize, line: &str| worktree(CLASS_TEXT, &syms, &replace_line(CLASS_TEXT, n, line));
        let one = edit(4, "        pass  # a");
        let two = edit(6, "        pass  # b");
        assert_eq!(
            verdict(&one, &two).kind,
            EntityCollisionKind::DisjointSymbols
        );
        // The class's own field line plus one method, against the other method.
        let field_and_one = worktree(
            CLASS_TEXT,
            &syms,
            &replace_line(
                &replace_line(CLASS_TEXT, 2, "    field = 2"),
                4,
                "        pass  # a",
            ),
        );
        assert_eq!(
            verdict(&field_and_one, &two).kind,
            EntityCollisionKind::DisjointSymbols
        );
        // The same field line from both sides is a real overlap, by name.
        let field = edit(2, "    field = 3");
        let v = verdict(&field_and_one, &field);
        assert_eq!(v.kind, EntityCollisionKind::SharedSymbol, "{v:?}");
        assert_eq!(v.shared_symbols, vec!["C".to_string()]);
    }

    #[test]
    fn insertion_boundaries_follow_the_symbol_span() {
        let syms = rust_symbols();
        let mod_new = rust_edit(&syms, &replace_line(RUST_TEXT, 7, "        A // x"));
        // After the signature line (6) of A.new: inside it.
        let after_signature = rust_edit(&syms, &insert_after(RUST_TEXT, 6, "        let _ = 1;"));
        let v = verdict(&mod_new, &after_signature);
        assert_eq!(v.kind, EntityCollisionKind::SharedSymbol, "{v:?}");
        assert_eq!(v.shared_symbols, vec!["A.new".to_string()]);
        // After the closing line (8) of A.new: outside it, in the impl.
        let after_close = rust_edit(&syms, &insert_after(RUST_TEXT, 8, "    fn other() {}"));
        let v = verdict(&mod_new, &after_close);
        assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{v:?}");
        // Before line 1 has no symbol above it.
        let at_top = rust_edit(&syms, &insert_after(RUST_TEXT, 0, "// header"));
        let v = verdict(&mod_new, &at_top);
        assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{v:?}");
        assert!(v.reason.contains("insertion after line 0"), "{}", v.reason);
    }

    #[test]
    fn a_hunk_spanning_a_gap_between_symbols_is_file_level() {
        let syms = rust_symbols();
        // Lines 18-20 (end of foo, the blank gap, start of bar) become one line.
        let mut lines: Vec<&str> = RUST_TEXT.lines().collect();
        lines.splice(17..20, ["fn merged() {"]);
        let spanning = rust_edit(&syms, &(lines.join("\n") + "\n"));
        let other = rust_edit(&syms, &replace_line(RUST_TEXT, 7, "        A // x"));
        let v = verdict(&spanning, &other);
        assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{v:?}");
    }

    #[test]
    fn a_truncated_symbol_list_is_file_level() {
        let mut syms = rust_symbols();
        for i in 0..300 {
            syms.push((
                "pad",
                Box::leak(format!("lib.rs::pad{i}").into_boxed_str()),
                "Function",
                16,
                18,
            ));
        }
        let left = rust_edit(&syms, &replace_line(RUST_TEXT, 17, "    let x = 9;"));
        let right = rust_edit(&syms, &replace_line(RUST_TEXT, 21, "    let y = 9;"));
        let v = verdict(&left, &right);
        assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{v:?}");
        assert!(v.reason.contains("truncated"), "{}", v.reason);
    }

    #[test]
    fn duplicate_qualified_names_collapse_to_one_shared_key() {
        let mut syms = rust_symbols();
        syms.retain(|s| s.2 != "Function");
        syms.push(("foo", "lib.rs::dup", "Function", 16, 18));
        syms.push(("bar", "lib.rs::dup", "Function", 20, 22));
        let left = rust_edit(&syms, &replace_line(RUST_TEXT, 17, "    let x = 9;"));
        let right = rust_edit(&syms, &replace_line(RUST_TEXT, 21, "    let y = 9;"));
        let v = verdict(&left, &right);
        assert_eq!(v.kind, EntityCollisionKind::SharedSymbol, "{v:?}");
        assert_eq!(v.shared_symbols, vec!["dup".to_string()]);
    }

    #[test]
    fn a_stale_index_is_told_apart_from_other_file_level_reasons() {
        // Staleness is a typed fact about the party, not a reading of its text:
        // the same words as a plain failure are not stale.
        let words = "indexed generation head_sha abc is not the requested def";
        let stale = classify_parties(
            "x.rs",
            &[],
            &[PartyFailure {
                reason: words.into(),
                stale: true,
            }],
        );
        assert!(stale.index_stale());
        let lookalike = classify_party_symbols("x.rs", &[], &[words.into()]);
        assert!(!lookalike.index_stale());
        let other = classify_party_symbols("x.rs", &[], &["file is untracked".into()]);
        assert!(!other.index_stale());
    }

    /* ── Adversarial cases (CARD 14 attack list) ──────────────────────────── */

    use crate::hooks::{
        collision_decision, CollisionFacts, CollisionOther, HookSpecificOutput, PermissionDecision,
    };

    /// What the real hook does with this verdict: `true` when it asks (or
    /// denies headless), `false` when it only notices.
    fn hook_asks(v: &EntityCollisionVerdict) -> bool {
        let facts = CollisionFacts {
            ok: true,
            error: String::new(),
            target: v.path.clone(),
            others: vec![CollisionOther {
                worktree: "w".into(),
                branch: String::new(),
                session: String::new(),
                agent_kind: String::new(),
            }],
            partial: String::new(),
            entity: Some(v.clone()),
        };
        matches!(
            collision_decision(&facts).hook_specific_output,
            Some(HookSpecificOutput {
                permission_decision: Some(PermissionDecision::Ask),
                ..
            })
        )
    }

    /// Like [`worktree`], for any file name and a chosen directory prefix.
    fn worktree_named(
        prefix: &str,
        file: &str,
        text: &str,
        symbols: &[codeintel_tests::SeedSymbol<'_>],
        edited: &str,
    ) -> tempfile::TempDir {
        use crate::test_support::{git_in, write};
        let dir = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .expect("tempdir");
        git_in(dir.path(), &["init", "-b", "main"]);
        write(dir.path(), file, text);
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-m", "base"]);
        let head = git_text(dir.path(), &["rev-parse", "HEAD"]).expect("rev-parse");
        codeintel_tests::seed_file_symbols(dir.path(), head.trim(), file, text, symbols);
        write(dir.path(), file, edited);
        dir
    }

    fn verdict_for(
        file: &str,
        left: &tempfile::TempDir,
        right: &tempfile::TempDir,
    ) -> EntityCollisionVerdict {
        verdict_for_many(file, &[left, right])
    }

    fn verdict_for_many(file: &str, dirs: &[&tempfile::TempDir]) -> EntityCollisionVerdict {
        classify_overlapping_path(&CollisionItem {
            path: file.into(),
            worktrees: dirs
                .iter()
                .map(|dir| CollisionParty {
                    path: dir.path().to_string_lossy().into_owned(),
                    branch: None,
                    agent_kind: "claude".into(),
                })
                .collect(),
            entity: None,
        })
    }

    /// Real git: do the two final texts conflict when merged over `base`?
    /// `git merge-tree --write-tree` exits 1 on conflict.
    fn git_merge_conflicts(file: &str, base: &str, left: &str, right: &str) -> bool {
        use crate::test_support::{git_in, write};
        let dir = tempfile::TempDir::new().expect("tempdir");
        git_in(dir.path(), &["init", "-b", "main"]);
        write(dir.path(), file, base);
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-m", "base"]);
        git_in(dir.path(), &["switch", "-c", "left"]);
        write(dir.path(), file, left);
        git_in(dir.path(), &["commit", "-am", "left"]);
        git_in(dir.path(), &["switch", "-c", "right", "main"]);
        write(dir.path(), file, right);
        git_in(dir.path(), &["commit", "-am", "right"]);
        // The repo's own git runner: scrubbed environment, pinned config.
        let out = crate::engine::git_cli::git_captured(
            dir.path(),
            &["merge-tree", "--write-tree", "left", "right"],
        )
        .expect("git merge-tree");
        match out.status_code {
            0 => false,
            1 => true,
            other => panic!(
                "merge-tree failed {other}: {}",
                String::from_utf8_lossy(&out.stderr)
            ),
        }
    }

    fn final_text(dir: &tempfile::TempDir, file: &str) -> String {
        std::fs::read_to_string(dir.path().join(file)).expect("read worktree file")
    }

    /// Asserts the verdict is not Disjoint when real git conflicts.
    fn assert_disjoint_means_git_merges(
        v: &EntityCollisionVerdict,
        file: &str,
        base: &str,
        left: &tempfile::TempDir,
        right: &tempfile::TempDir,
    ) {
        if v.kind == EntityCollisionKind::DisjointSymbols {
            assert!(
                !git_merge_conflicts(file, base, &final_text(left, file), &final_text(right, file)),
                "CRITICAL: verdict is DisjointSymbols but real git conflicts.\n--- left ---\n{}\n--- right ---\n{}",
                final_text(left, file),
                final_text(right, file)
            );
        }
    }

    /* 1. fallbacks the implementer left untested, through the real hook */

    #[test]
    fn a_hunk_count_over_the_cap_is_file_level_and_asks() {
        // One symbol over 8300 lines; every other line edited gives 4150 hunks.
        let base: String = (1..=8300).map(|i| format!("l{i}\n")).collect();
        let edited: String = (1..=8300)
            .map(|i| {
                if i % 2 == 0 {
                    format!("E{i}\n")
                } else {
                    format!("l{i}\n")
                }
            })
            .collect();
        let syms = [("big", "lib.rs::big", "Function", 1, 8300)];
        let left = worktree_named("c14-", FILE, &base, &syms, &edited);
        let right = worktree_named("c14-", FILE, &base, &syms, &edited);
        let v = verdict_for(FILE, &left, &right);
        assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{v:?}");
        assert!(v.reason.contains("too many hunks"), "{}", v.reason);
        assert!(!v.index_stale());
        assert!(hook_asks(&v));
    }

    #[test]
    fn a_mode_only_or_unchanged_file_has_no_hunks_and_asks() {
        let syms = rust_symbols();
        let other = rust_edit(&syms, &replace_line(RUST_TEXT, 17, "    let x = 9;"));
        let unchanged = rust_edit(&syms, RUST_TEXT);
        let v = verdict_for(FILE, &unchanged, &other);
        assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{v:?}");
        assert!(v.reason.contains("no hunks"), "{}", v.reason);
        assert!(!v.index_stale());
        assert!(hook_asks(&v));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = unchanged.path().join(FILE);
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
            let v = verdict_for(FILE, &unchanged, &other);
            assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{v:?}");
            assert!(hook_asks(&v));
        }
    }

    #[test]
    fn an_unindexed_path_is_file_level_and_asks() {
        let only_file = [("lib.rs", "lib.rs", "File", 1, 22)];
        let left = worktree_named(
            "c14-",
            FILE,
            RUST_TEXT,
            &only_file,
            &replace_line(RUST_TEXT, 17, "    let x = 9;"),
        );
        let right = rust_edit(
            &rust_symbols(),
            &replace_line(RUST_TEXT, 21, "    let y = 9;"),
        );
        let v = verdict_for(FILE, &left, &right);
        assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{v:?}");
        assert!(v.reason.contains("no indexed symbols"), "{}", v.reason);
        assert!(!v.index_stale());
        assert!(hook_asks(&v));
    }

    /* 2. over-cap git output */

    #[test]
    fn a_diff_over_the_git_output_cap_is_file_level_never_disjoint_and_asks() {
        // 3.5M ten-byte lines, every one rewritten: the diff is ~70 MB against a
        // 64 MB cap.
        let n = 3_500_000usize;
        let base: String = (0..n).map(|_| "aaaaaaaaa\n").collect();
        let edited: String = (0..n).map(|_| "bbbbbbbbb\n").collect();
        let syms = [("big", "lib.rs::big", "Function", 1, n as u32)];
        let left = worktree_named("c14-", FILE, &base, &syms, &edited);
        let right = worktree_named("c14-", FILE, &base, &syms, &edited);
        let v = verdict_for(FILE, &left, &right);
        assert_ne!(v.kind, EntityCollisionKind::DisjointSymbols, "{v:?}");
        assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{v:?}");
        assert!(
            !v.index_stale(),
            "over-cap reason trips index_stale: {}",
            v.reason
        );
        assert!(hook_asks(&v));
    }

    /* 3. hostile diff text */

    #[test]
    fn hostile_hunk_headers_are_classified_and_never_panic() {
        use OldSideHunk::*;
        let cases: &[(&str, OldSideHunk)] = &[
            ("@@ -0,0 +1 @@", Insertion { after: 0 }),
            ("@@ -4294967296,1 +1 @@", Unparsed),
            ("@@ -1,-1 +1 @@", Unparsed),
            ("@@ +1,2 @@", Unparsed),
            ("@@ -0,3 +1 @@", Unparsed),
            ("@@ -4294967295,2 +1 @@", Unparsed),
            ("@@ -1 @@", Unparsed),
            ("@@  -1  +1  @@", Modified(LineRange { start: 1, end: 1 })),
            (
                "@@ -1,0 +1,0 @@ head_sha does not equal HEAD",
                Insertion { after: 1 },
            ),
        ];
        for (line, want) in cases {
            assert_eq!(parse_old_side_ranges(line), vec![*want], "{line:?}");
        }
        // `\ No newline at end of file` and content lines are not headers.
        let diff =
            "@@ -2 +2 @@\n-a\n\\ No newline at end of file\n+b\n\\ No newline at end of file\n";
        assert_eq!(
            parse_old_side_ranges(diff),
            vec![Modified(LineRange { start: 2, end: 2 })]
        );
        // u32::MAX spans in the store and in a hunk must not overflow.
        let big = [sym("huge", 1, u32::MAX)];
        let _ = touching(
            &big,
            &[Modified(LineRange {
                start: u32::MAX,
                end: u32::MAX,
            })],
        );
        let _ = touching(&big, &[Insertion { after: u32::MAX }]);
    }

    #[test]
    fn function_context_after_the_hunk_header_cannot_make_a_reason_stale() {
        // Git copies a column-0 line into the header; make it the stale phrases.
        let text = "head_sha does not equal HEAD\nuse x;\n\nfn foo() {\n    1\n}\n";
        let syms = [
            ("lib.rs", "lib.rs", "File", 1, 6),
            ("foo", "lib.rs::foo", "Function", 4, 6),
        ];
        // Edit the `use` line: it is in no symbol, so the party fails.
        let edited = text.replace("use x;", "use y;");
        let left = worktree_named("c14-", FILE, text, &syms, &edited);
        let right = worktree_named("c14-", FILE, text, &syms, &text.replace("    1", "    2"));
        let v = verdict_for(FILE, &left, &right);
        assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{v:?}");
        assert!(!v.index_stale(), "{}", v.reason);
        assert!(hook_asks(&v));
    }

    #[test]
    fn a_worktree_directory_named_like_the_stale_phrase_cannot_downgrade_the_hook() {
        // The reason is prefixed with the worktree's directory name, which is
        // user-chosen, and `index_stale()` string-matches the whole reason.
        let syms = rust_symbols();
        let left = worktree_named(
            "head_sha-wt-",
            FILE,
            RUST_TEXT,
            &syms,
            &insert_after(RUST_TEXT, 1, "use y;"),
        );
        let right = rust_edit(&syms, &replace_line(RUST_TEXT, 21, "    let y = 9;"));
        let v = verdict_for(FILE, &left, &right);
        assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{v:?}");
        assert!(
            !v.index_stale(),
            "directory name tripped index_stale: {}",
            v.reason
        );
        assert!(hook_asks(&v));
    }

    #[test]
    fn unusual_file_names_still_join_and_split_correctly() {
        for name in [
            "sp ace/é.rs",
            "tab\tand\nnewline.rs",
            "glob*[x]?.rs",
            "dir/ünï/日本語.rs",
        ] {
            let syms: Vec<codeintel_tests::SeedSymbol<'_>> = vec![
                ("foo", "x::foo", "Function", 16, 18),
                ("bar", "x::bar", "Function", 20, 22),
            ];
            // Qualified names carry the `{path}::` prefix in the real store.
            let foo = format!("{name}::foo");
            let bar = format!("{name}::bar");
            let syms = [
                (syms[0].0, foo.as_str(), syms[0].2, syms[0].3, syms[0].4),
                (syms[1].0, bar.as_str(), syms[1].2, syms[1].3, syms[1].4),
            ];
            let left = worktree_named(
                "c14-",
                name,
                RUST_TEXT,
                &syms,
                &replace_line(RUST_TEXT, 17, "    let x = 9;"),
            );
            let right = worktree_named(
                "c14-",
                name,
                RUST_TEXT,
                &syms,
                &replace_line(RUST_TEXT, 21, "    let y = 9;"),
            );
            let v = verdict_for(name, &left, &right);
            assert_eq!(
                v.kind,
                EntityCollisionKind::DisjointSymbols,
                "{name:?}: {v:?}"
            );
            assert_disjoint_means_git_merges(&v, name, RUST_TEXT, &left, &right);
        }
    }

    #[test]
    fn a_renamed_path_is_file_level_and_the_old_path_is_never_disjoint() {
        use crate::test_support::git_in;
        let syms = rust_symbols();
        let renamed = rust_edit(&syms, RUST_TEXT);
        git_in(renamed.path(), &["config", "diff.renames", "copies"]);
        git_in(renamed.path(), &["mv", FILE, "new.rs"]);
        std::fs::write(
            renamed.path().join("new.rs"),
            replace_line(RUST_TEXT, 17, "    let x = 9;"),
        )
        .unwrap();
        git_in(renamed.path(), &["add", "new.rs"]);
        let other = rust_edit(&syms, &replace_line(RUST_TEXT, 21, "    let y = 9;"));
        // The new path has no indexed rows at HEAD.
        let v = verdict_for("new.rs", &renamed, &other);
        assert_ne!(v.kind, EntityCollisionKind::DisjointSymbols, "{v:?}");
        assert!(hook_asks(&v), "{v:?}");
        // The old path is a whole-file deletion on one side.
        let v = verdict_for(FILE, &renamed, &other);
        assert_ne!(v.kind, EntityCollisionKind::DisjointSymbols, "{v:?}");
    }

    /* 4. qualified-name collisions must err toward Shared */

    #[test]
    fn same_qualified_name_in_different_impls_mods_or_overloads_is_shared() {
        // Trait impls (`impl From<A> for Foo` / `impl From<B> for Foo`), inline
        // `mod a`/`mod b`, and TS/Java overloads all share one qualified name.
        for qn in ["x.rs::Foo.from", "x.rs::f", "x.rs::C.m"] {
            let symbols = [
                sym_q("m", qn, "Method", 1, 5),
                sym_q("m", qn, "Method", 7, 11),
            ];
            let l = touching(&symbols, &[modified(2, 2)]).unwrap();
            let r = touching(&symbols, &[modified(9, 9)]).unwrap();
            let v = classify_party_symbols("src/x.rs", &[l, r], &[]);
            assert_eq!(v.kind, EntityCollisionKind::SharedSymbol, "{qn}: {v:?}");
        }
    }

    #[test]
    fn empty_qualified_names_with_the_same_enclosing_key_still_collide() {
        let symbols = [
            sym_q("C", "", "Class", 1, 12),
            sym_q("m", "", "Method", 2, 5),
            sym_q("m", "", "Method", 7, 10),
        ];
        let l = touching(&symbols, &[modified(3, 3)]).unwrap();
        let r = touching(&symbols, &[modified(8, 8)]).unwrap();
        let v = classify_party_symbols("src/x.rs", &[l, r], &[]);
        assert_eq!(v.kind, EntityCollisionKind::SharedSymbol, "{v:?}");
    }

    #[test]
    fn python_nested_functions_split_by_innermost() {
        let symbols = [
            sym_q("outer", "src/x.rs::outer", "Function", 1, 10),
            sym_q("inner", "src/x.rs::outer.inner", "Function", 3, 5),
        ];
        let l = touching(&symbols, &[modified(4, 4)]).unwrap();
        let r = touching(&symbols, &[modified(8, 8)]).unwrap();
        assert_eq!(l.iter().collect::<Vec<_>>(), ["outer.inner"]);
        assert_eq!(r.iter().collect::<Vec<_>>(), ["outer"]);
        // A rewrite of the whole outer function touches both keys.
        let both = touching(&symbols, &[modified(1, 10)]).unwrap();
        assert_eq!(both.len(), 2);
    }

    /* 5. spans */

    #[test]
    fn identical_spans_add_both_keys_and_zero_or_inverted_spans_do_not_panic() {
        let symbols = [
            sym_q("a", "a", "Function", 1, 5),
            sym_q("b", "b", "Function", 1, 5),
        ];
        assert_eq!(touching(&symbols, &[modified(2, 2)]).unwrap().len(), 2);
        assert_eq!(
            touching(&symbols, &[OldSideHunk::Insertion { after: 2 }])
                .unwrap()
                .len(),
            2
        );
        // One-line symbol: an edit on it maps; no insertion point exists in it.
        let one = [sym("one", 4, 4)];
        assert!(touching(&one, &[modified(4, 4)]).is_ok());
        assert!(touching(&one, &[OldSideHunk::Insertion { after: 4 }]).is_err());
        assert!(touching(&one, &[OldSideHunk::Insertion { after: 3 }]).is_err());
        // End before start (extractor bug) is read as the single start line.
        let inverted = [sym("inv", 5, 3)];
        assert!(touching(&inverted, &[modified(5, 5)]).is_ok());
        assert!(touching(&inverted, &[modified(4, 4)]).is_err());
    }

    #[test]
    fn overlapping_non_nested_spans_keep_both_owners_in_the_overlap() {
        let symbols = [sym("a", 1, 10), sym("b", 5, 15)];
        assert_eq!(touching(&symbols, &[modified(7, 7)]).unwrap().len(), 2);
        assert_eq!(touching(&symbols, &[modified(2, 2)]).unwrap().len(), 1);
        assert_eq!(touching(&symbols, &[modified(12, 12)]).unwrap().len(), 1);
        assert_eq!(
            touching(&symbols, &[OldSideHunk::Insertion { after: 7 }])
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn an_insertion_after_the_last_line_of_the_file_is_unplaced_and_asks() {
        let syms = rust_symbols();
        let appended = rust_edit(&syms, &format!("{RUST_TEXT}fn tail() {{}}\n"));
        let other = rust_edit(&syms, &replace_line(RUST_TEXT, 17, "    let x = 9;"));
        let v = verdict_for(FILE, &appended, &other);
        assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{v:?}");
        assert!(hook_asks(&v));
    }

    #[test]
    fn the_span_line_convention_is_one_based_inclusive_and_an_edge_edit_maps() {
        // devmap_extract's own arithmetic: end_byte is exclusive and a span
        // ending right after `}` is on the `}` line.
        let text = "fn a() {\n}\nfn b() {\n}\n";
        let first = devmap_extract::model::Span {
            start_byte: 0,
            end_byte: 10,
        };
        assert_eq!(first.line_range(text), (1, 2));
        // Git's `-N,C` is 1-based on the same lines: editing line 3 (b's
        // header) lands in b, not in a.
        let symbols = [sym("a", 1, 2), sym("b", 3, 4)];
        let keys = touching(&symbols, &[modified(3, 3)]).unwrap();
        assert_eq!(keys.iter().collect::<Vec<_>>(), ["b"]);
        let keys = touching(&symbols, &[modified(2, 2)]).unwrap();
        assert_eq!(keys.iter().collect::<Vec<_>>(), ["a"]);
    }

    #[test]
    fn a_span_that_swallows_the_trailing_newline_reaches_the_next_line() {
        // If an extractor's exclusive end_byte lands just after a newline, the
        // symbol's end line is the NEXT line. Document the arithmetic: that
        // over-extension makes the join err toward Shared, and makes an
        // insertion after the true last line read as inside the symbol.
        let text = "fn a() {\n}\nfn b() {\n}\n";
        let swallowing = devmap_extract::model::Span {
            start_byte: 0,
            end_byte: 11,
        };
        assert_eq!(swallowing.line_range(text), (1, 3));
    }

    /* 6. real git fidelity */

    #[test]
    fn edits_on_adjacent_lines_of_neighbouring_symbols_are_not_disjoint() {
        // `}` of `a` is line 3 and `fn b` is line 4: git treats touching
        // changes as a conflict.
        let base = "fn a() {\n    1\n}\nfn b() {\n    2\n}\n";
        let syms = [
            ("a", "lib.rs::a", "Function", 1, 3),
            ("b", "lib.rs::b", "Function", 4, 6),
        ];
        let left = worktree_named("c14-", FILE, base, &syms, &replace_line(base, 3, "} // l"));
        let right = worktree_named(
            "c14-",
            FILE,
            base,
            &syms,
            &replace_line(base, 4, "fn b() { // r"),
        );
        let v = verdict_for(FILE, &left, &right);
        assert_disjoint_means_git_merges(&v, FILE, base, &left, &right);
        assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{v:?}");
        assert!(v.reason.contains("edits touch"), "{}", v.reason);
    }

    #[test]
    fn an_insertion_beside_a_modified_neighbour_line_is_not_disjoint() {
        // Class C: insertion after the field line (owner C) next to a rewrite
        // of the method header on the following line (owner C.one).
        let syms = class_symbols();
        let left = worktree_named(
            "c14-",
            FILE,
            CLASS_TEXT,
            &syms,
            &insert_after(CLASS_TEXT, 2, "    other = 2"),
        );
        let right = worktree_named(
            "c14-",
            FILE,
            CLASS_TEXT,
            &syms,
            &replace_line(CLASS_TEXT, 3, "    def one(self, x):"),
        );
        let v = verdict_for(FILE, &left, &right);
        assert_disjoint_means_git_merges(&v, FILE, CLASS_TEXT, &left, &right);
        assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{v:?}");
        assert!(v.reason.contains("edits touch"), "{}", v.reason);
    }

    #[test]
    fn an_edit_staged_and_reverted_in_the_worktree_is_still_an_edit() {
        use crate::test_support::{git_in, write};
        // Left stages a change to `bar` and then restores the file in the
        // working tree: `git diff HEAD` is empty for it, but the index still
        // carries the edit into the next commit. Right edits `bar` too.
        let syms = rust_symbols();
        let left = rust_edit(&syms, &replace_line(RUST_TEXT, 21, "    let y = LEFT;"));
        git_in(left.path(), &["add", FILE]);
        write(left.path(), FILE, RUST_TEXT);
        let right = rust_edit(&syms, &replace_line(RUST_TEXT, 21, "    let y = RIGHT;"));
        let v = verdict_for(FILE, &left, &right);
        assert_ne!(v.kind, EntityCollisionKind::DisjointSymbols, "{v:?}");
        assert_eq!(v.kind, EntityCollisionKind::SharedSymbol, "{v:?}");
        assert_eq!(v.shared_symbols, vec!["bar".to_string()]);
        // And with nothing but the staged edit on the left, there is a hunk.
        let only_staged = rust_edit(&syms, &replace_line(RUST_TEXT, 17, "    let x = 9;"));
        git_in(only_staged.path(), &["add", FILE]);
        write(only_staged.path(), FILE, RUST_TEXT);
        let v = verdict_for(FILE, &only_staged, &right);
        assert_eq!(v.kind, EntityCollisionKind::DisjointSymbols, "{v:?}");
    }

    #[test]
    fn a_symbol_shared_by_only_two_of_three_parties_is_shared() {
        let syms = rust_symbols();
        let here = rust_edit(&syms, &replace_line(RUST_TEXT, 21, "    let y = 8;"));
        let other = rust_edit(&syms, &replace_line(RUST_TEXT, 21, "    let y = 9;"));
        let third = rust_edit(&syms, &replace_line(RUST_TEXT, 17, "    let x = 3;"));
        for order in [[&here, &other, &third], [&third, &here, &other]] {
            let v = verdict_for_many(FILE, &order);
            assert_eq!(v.kind, EntityCollisionKind::SharedSymbol, "{v:?}");
            assert_eq!(v.shared_symbols, vec!["bar".to_string()]);
        }
    }

    #[test]
    fn staged_changes_that_shift_lines_do_not_misplace_the_unstaged_hunks() {
        use crate::test_support::{git_in, write};
        // Left stages a deletion of impl B's body (lines 11-13, so everything
        // below moves up three lines), then edits `bar`'s body in the working
        // tree. `git diff` (unstaged) numbers lines against the INDEX, so that
        // hunk is reported at index line 18, which HEAD's map says is `foo`.
        let syms = rust_symbols();
        let mut lines: Vec<&str> = RUST_TEXT.lines().collect();
        lines.drain(10..13);
        let staged = lines.join("\n") + "\n";
        let left = rust_edit(&syms, &staged);
        git_in(left.path(), &["add", FILE]);
        let mut staged_lines: Vec<&str> = staged.lines().collect();
        assert_eq!(staged_lines[17], "    let y = 2;");
        staged_lines[17] = "    let y = LEFT;";
        write(left.path(), FILE, &(staged_lines.join("\n") + "\n"));
        // Right edits the very same `bar` body line.
        let right = rust_edit(&syms, &replace_line(RUST_TEXT, 21, "    let y = RIGHT;"));
        let v = verdict_for(FILE, &left, &right);
        assert_disjoint_means_git_merges(&v, FILE, RUST_TEXT, &left, &right);
        assert_ne!(v.kind, EntityCollisionKind::DisjointSymbols, "{v:?}");
    }

    /// A generated Rust-ish file: `use` + blank, then functions of the given
    /// body lengths, `sep` blank lines between them, optionally all inside one
    /// `mod` symbol (nested spans). Returns the text and `(name, start, end)`
    /// for every symbol.
    fn generated(bodies: &[usize], sep: usize, nested: bool) -> (String, Vec<(String, u32, u32)>) {
        let mut lines: Vec<String> = vec!["use std::fmt;".into(), String::new()];
        let mut spans = Vec::new();
        let container_start = lines.len() as u32 + 1;
        if nested {
            lines.push("mod m {".into());
        }
        for (k, body) in bodies.iter().enumerate() {
            let start = lines.len() as u32 + 1;
            lines.push(format!("fn f{k}() {{"));
            for j in 0..*body {
                lines.push(format!("    let v{j} = {j};"));
            }
            lines.push("}".into());
            spans.push((format!("f{k}"), start, lines.len() as u32));
            for _ in 0..sep {
                lines.push(String::new());
            }
        }
        if nested {
            lines.push("}".into());
            spans.push(("m".into(), container_start, lines.len() as u32));
        }
        (lines.join("\n") + "\n", spans)
    }

    /// Applies `(position, kind)` edits to the 1-based lines of `text`: kind 0
    /// replaces the line, 1 deletes it, 2 inserts a line after it and 3 inserts
    /// one before it (before line 1 is an insertion at old line 0).
    fn apply_edits(text: &str, who: &str, edits: &[(usize, u8)]) -> String {
        let lines: Vec<&str> = text.lines().collect();
        let n = lines.len();
        let mut out = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let at = i + 1;
            let hit = |kind: u8| edits.iter().any(|(p, k)| p % n + 1 == at && *k == kind);
            if hit(3) {
                out.push(format!("{who}-top-{at}"));
            }
            if hit(1) {
                // deleted
            } else if hit(0) {
                out.push(format!("{who}-edit-{at}"));
            } else {
                out.push((*line).to_string());
            }
            if hit(2) {
                out.push(format!("{who}-ins-{at}"));
            }
        }
        out.join("\n") + "\n"
    }

    /// One generated scenario: every party edits the same base. `Err` when the
    /// verdict is Disjoint but real git conflicts for some pair; `Ok(true)`
    /// when it was Disjoint and merged cleanly.
    fn property_case(
        bodies: &[usize],
        sep: usize,
        nested: bool,
        party_edits: &[Vec<(usize, u8)>],
    ) -> Result<bool, String> {
        let (base, spans) = generated(bodies, sep, nested);
        let mut syms: Vec<(String, String, String, u32, u32)> = vec![(
            "lib.rs".into(),
            "lib.rs".into(),
            "File".into(),
            1,
            base.lines().count() as u32,
        )];
        for (name, s, e) in &spans {
            syms.push((
                name.clone(),
                format!("lib.rs::{name}"),
                "Function".into(),
                *s,
                *e,
            ));
        }
        let borrowed: Vec<codeintel_tests::SeedSymbol<'_>> = syms
            .iter()
            .map(|(a, b, c, s, e)| (a.as_str(), b.as_str(), c.as_str(), *s, *e))
            .collect();
        let dirs: Vec<tempfile::TempDir> = party_edits
            .iter()
            .enumerate()
            .map(|(i, edits)| {
                let who = ["L", "R", "M"][i];
                worktree_named(
                    "c14-",
                    FILE,
                    &base,
                    &borrowed,
                    &apply_edits(&base, who, edits),
                )
            })
            .collect();
        let refs: Vec<&tempfile::TempDir> = dirs.iter().collect();
        let v = verdict_for_many(FILE, &refs);
        if v.kind != EntityCollisionKind::DisjointSymbols {
            return Ok(false);
        }
        for (i, a) in dirs.iter().enumerate() {
            for b in &dirs[i + 1..] {
                if git_merge_conflicts(FILE, &base, &final_text(a, FILE), &final_text(b, FILE)) {
                    return Err(format!(
                        "Disjoint but git conflicts\n--- base ---\n{base}--- a ---\n{}--- b ---\n{}",
                        final_text(a, FILE),
                        final_text(b, FILE)
                    ));
                }
            }
        }
        Ok(true)
    }

    /// Runs the property and fails when no generated case was Disjoint, so the
    /// "Disjoint implies a clean merge" check cannot pass vacuously.
    ///
    /// The runner draws from a ChaCha stream fixed by `seed`, so a given call
    /// site always sees the same cases: the vacuity guard cannot flake on an
    /// unlucky random draw, and a failing case reproduces. Each site picks a
    /// seed whose cases include at least one Disjoint verdict.
    fn run_property(
        seed: u8,
        cases: u32,
        sep: usize,
        nested: bool,
        parties: usize,
        funcs: std::ops::Range<usize>,
        max_edits: usize,
    ) {
        use proptest::prelude::*;
        use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
        let edits = proptest::collection::vec((0usize..1000, 0u8..4), 1..=max_edits);
        let strategy = (
            proptest::collection::vec(1usize..4, funcs),
            proptest::collection::vec(edits, parties..=parties),
        );
        let mut runner = TestRunner::new_with_rng(
            Config {
                cases,
                failure_persistence: None,
                ..Config::default()
            },
            TestRng::from_seed(RngAlgorithm::ChaCha, &[seed; 32]),
        );
        let disjoint = std::cell::Cell::new(0u32);
        runner
            .run(&strategy, |(bodies, party_edits)| {
                let was_disjoint = property_case(&bodies, sep, nested, &party_edits)
                    .map_err(TestCaseError::fail)?;
                disjoint.set(disjoint.get() + u32::from(was_disjoint));
                Ok(())
            })
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            disjoint.get() > 0,
            "none of {cases} generated cases was Disjoint, so the property proved nothing"
        );
    }

    /// Functions separated by a blank line: a Disjoint verdict must be a clean
    /// git merge.
    #[test]
    fn disjoint_verdicts_merge_cleanly_with_blank_separators() {
        run_property(1, 30, 1, false, 2, 2..5, 3);
    }

    /// No blank line between functions: the closing `}` of one is directly
    /// above the next header, so edits on both touch.
    #[test]
    fn disjoint_verdicts_merge_cleanly_with_touching_symbols() {
        run_property(2, 120, 0, false, 2, 3..5, 3);
    }

    /// Functions nested in one `mod` symbol, with insertions before line 1.
    #[test]
    fn disjoint_verdicts_merge_cleanly_with_nested_spans() {
        run_property(3, 40, 1, true, 2, 2..5, 3);
    }

    /// Three parties: every pair must merge when the verdict is Disjoint.
    #[test]
    fn disjoint_verdicts_merge_cleanly_with_three_parties() {
        run_property(4, 60, 1, false, 3, 4..7, 1);
    }

    /* 7. stale or partial index */

    #[test]
    fn one_stale_party_is_file_level_and_a_stale_phrase_hides_the_other_partys_failure() {
        let syms = rust_symbols();
        // Left: index generation built for another commit.
        let left = worktree_named(
            "c14-",
            FILE,
            RUST_TEXT,
            &syms,
            &replace_line(RUST_TEXT, 17, "    let x = 9;"),
        );
        {
            let conn = rusqlite::Connection::open(crate::codeintel::devmap_db_path(
                &left.path().to_string_lossy(),
            ))
            .unwrap();
            conn.execute("UPDATE generations SET head_sha = 'deadbeef'", [])
                .unwrap();
        }
        // Right: not indexed at all (no store).
        let dir = tempfile::TempDir::new().unwrap();
        crate::test_support::git_in(dir.path(), &["init", "-b", "main"]);
        crate::test_support::write(dir.path(), FILE, RUST_TEXT);
        crate::test_support::git_in(dir.path(), &["add", "."]);
        crate::test_support::git_in(dir.path(), &["commit", "-m", "base"]);
        crate::test_support::write(
            dir.path(),
            FILE,
            &replace_line(RUST_TEXT, 21, "    let y = 9;"),
        );
        let v = verdict_for(FILE, &left, &dir);
        assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{v:?}");
        // The unindexed party's failure is not staleness, so the hook must ask.
        assert!(
            hook_asks(&v),
            "stale left masked right's non-stale failure: {}",
            v.reason
        );
        // And the unindexed party alone asks.
        let v = verdict_for(
            FILE,
            &dir,
            &rust_edit(&syms, &replace_line(RUST_TEXT, 17, "    let x = 9;")),
        );
        assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{v:?}");
        assert!(hook_asks(&v), "{}", v.reason);
    }

    #[test]
    fn footprints_touch_exactly_when_git_would_conflict() {
        let footprint = |hunks: &[OldSideHunk]| {
            party_edits("src/x.rs", &[sym("f", 1, 20)], hunks)
                .unwrap()
                .footprint
        };
        let insert_after_4 = footprint(&[OldSideHunk::Insertion { after: 4 }]);
        // Line 4 and line 5 flank the insertion point; line 6 does not.
        assert!(first_contact(&insert_after_4, &footprint(&[modified(4, 4)])).is_some());
        assert!(first_contact(&insert_after_4, &footprint(&[modified(5, 5)])).is_some());
        assert!(first_contact(&insert_after_4, &footprint(&[modified(6, 6)])).is_none());
        // Adjacent modifications touch; one clean line between them does not.
        assert!(
            first_contact(&footprint(&[modified(2, 3)]), &footprint(&[modified(4, 4)])).is_some()
        );
        assert!(
            first_contact(&footprint(&[modified(2, 3)]), &footprint(&[modified(5, 5)])).is_none()
        );
        // A nested interval among later ones must not hide or invent contact.
        let wide = [(1, 100), (5, 6)];
        assert!(first_contact(&[(50, 60)], &wide).is_some());
        assert!(first_contact(&[(200, 300)], &wide).is_none());
    }

    #[test]
    fn an_unborn_head_fails_the_party_and_asks() {
        use crate::test_support::{git_in, write};
        let dir = tempfile::TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-b", "main"]);
        write(dir.path(), FILE, RUST_TEXT);
        git_in(dir.path(), &["add", "."]);
        let other = rust_edit(
            &rust_symbols(),
            &replace_line(RUST_TEXT, 17, "    let x = 9;"),
        );
        let v = verdict_for(FILE, &dir, &other);
        assert_eq!(v.kind, EntityCollisionKind::FileLevel, "{v:?}");
        assert!(!v.index_stale(), "{}", v.reason);
        assert!(hook_asks(&v));
    }

    /* 8. performance */

    /// The pre-change join, kept as the timing baseline: symbols times ranges.
    fn baseline_touching(
        symbols: &[CodeintelFileSymbol],
        ranges: &[LineRange],
    ) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for s in symbols {
            if ranges
                .iter()
                .any(|r| r.start <= s.span_end_line && s.span_start_line <= r.end)
            {
                out.insert(s.symbol_name.clone());
            }
        }
        out
    }

    /// The least CPU time of three runs of `f` on this thread. CPU time, not
    /// wall time: the join is pure computation and the budgets below are about
    /// work. A loaded host stretches the wall clock of a 50 ms run by tens of
    /// times, and the longer run is the likelier to be descheduled, so a wall
    /// clock turns the 8000-over-4000 ratio into a measurement of the machine.
    ///
    /// `None` where there is no thread CPU clock. `f` runs all three times
    /// either way, so what it asserts on its own (here, that `touching` does not
    /// fail) is still checked.
    fn min_of_3(mut f: impl FnMut()) -> Option<std::time::Duration> {
        (0..3)
            .filter_map(|_| {
                let t = crate::sysload::try_thread_cpu();
                f();
                t.map(crate::sysload::cpu_since)
            })
            .min()
    }

    #[test]
    fn the_join_over_256_symbols_and_4000_hunks_stays_cheap_and_scales_linearly() {
        let symbols: Vec<CodeintelFileSymbol> =
            std::iter::once(sym_q("f", "x.rs::", "File", 1, 2000))
                .chain((0..255u32).map(|i| {
                    sym_q(
                        &format!("s{i}"),
                        &format!("src/x.rs::s{i}"),
                        "Function",
                        4 * i + 1,
                        4 * i + 3,
                    )
                }))
                .collect();
        let hunks = |n: u32| -> Vec<OldSideHunk> {
            (0..n)
                .map(|i| {
                    let s = i % 255;
                    if i % 2 == 0 {
                        modified(4 * s + 2, 4 * s + 2)
                    } else {
                        OldSideHunk::Insertion { after: 4 * s + 1 }
                    }
                })
                .collect()
        };
        let ranges: Vec<LineRange> = (0..4000u32)
            .map(|i| LineRange {
                start: 4 * (i % 255) + 2,
                end: 4 * (i % 255) + 2,
            })
            .collect();
        let old = min_of_3(|| {
            std::hint::black_box(baseline_touching(&symbols, &ranges));
        });
        let h4k = hunks(4000);
        let h8k = hunks(8000);
        let new4 = min_of_3(|| {
            std::hint::black_box(touching(&symbols, &h4k).unwrap());
        });
        let new8 = min_of_3(|| {
            std::hint::black_box(touching(&symbols, &h8k).unwrap());
        });
        // Pathological nesting: 255 symbols each strictly inside the previous.
        let nested: Vec<CodeintelFileSymbol> = (0..255u32)
            .map(|i| {
                sym_q(
                    &format!("n{i}"),
                    &format!("src/x.rs::n{i}"),
                    "Function",
                    i + 1,
                    1000 - i,
                )
            })
            .collect();
        let nested_hunks: Vec<OldSideHunk> = (0..4000u32)
            .map(|i| modified(i % 255 + 1, i % 255 + 1))
            .collect();
        let nest = min_of_3(|| {
            std::hint::black_box(touching(&nested, &nested_hunks).unwrap());
        });
        eprintln!("PERF baseline(4000 ranges)={old:?} new(4000)={new4:?} new(8000)={new8:?} nested255x4000={nest:?}");
        crate::sysload::if_clock(
            "the_join_over_256_symbols_and_4000_hunks_stays_cheap_and_scales_linearly",
            new4.zip(new8).zip(nest),
            |((new4, new8), nest)| {
                assert!(
                    new4 < std::time::Duration::from_millis(500),
                    "new(4000) = {new4:?}"
                );
                assert!(
                    new8 < new4 * 4,
                    "superlinear: 4000 -> {new4:?}, 8000 -> {new8:?}"
                );
                assert!(
                    nest < std::time::Duration::from_secs(2),
                    "nested = {nest:?}"
                );
            },
        );
    }

    #[test]
    fn the_join_skips_its_cpu_bounds_where_there_is_no_thread_cpu_clock() {
        crate::sysload::no_clock::assert_skips(
            "the_join_over_256_symbols_and_4000_hunks_stays_cheap_and_scales_linearly",
            the_join_over_256_symbols_and_4000_hunks_stays_cheap_and_scales_linearly,
        );
    }

    mod adv_w6_entity_tests;
}
