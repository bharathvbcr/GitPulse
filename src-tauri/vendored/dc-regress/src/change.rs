//! What a change touched, as line ranges in the post-image.
//!
//! This is the seed of the forward question — "these lines changed, what is
//! affected" — and it is the half that has nothing to do with the graph. It
//! runs `git diff` and reads the hunk headers, and its entire job is to be
//! honest about the cases where a hunk header does not mean what it appears
//! to.
//!
//! # Why the post-image, and why it is pinned
//!
//! A hunk header carries both a pre-image and a post-image range. Only the
//! post-image can be intersected with the graph's symbol spans, because the
//! graph indexed the post-image content and nothing else. So every range here
//! is a post-image range, and the caller is responsible for making the
//! post-image the revision the graph was built at — which
//! [`crate::blast`] does by construction rather than by asking.
//!
//! # Three headers that lie if read literally
//!
//! 1. **A pure deletion.** `@@ -40,12 +39,0 @@` has a post-image count of
//!    zero, so read literally it touches no lines at all and the change
//!    disappears. It is not nothing: twelve lines were removed from between
//!    post-image lines 39 and 40, and whichever symbol spans that boundary was
//!    changed. [`ChangedRange`] widens a zero-count hunk to the pair of lines
//!    it sits between, and flags it, because over-inclusion here costs a
//!    reader one extra symbol and under-inclusion costs them the answer.
//!
//! 2. **A merge.** `git show` on a merge commit prints *nothing* by default,
//!    and with `-m` prints a combined diff whose `@@@` headers have one range
//!    per parent. Both are silent failures — the first reads as "this commit
//!    changed nothing", the second parses as garbage. Merges are refused by
//!    name, before either can happen.
//!
//! 3. **A quoted path.** Git C-escapes paths containing control characters,
//!    and a path containing a newline would split across what this parser
//!    reads as two lines. `core.quotePath=false` handles the common non-ASCII
//!    case; anything still quoted after that is refused by name rather than
//!    unescaped by hand into a path that may not be the one on disk.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::history::{run_git, HistoryRefusal, LOG_OUTPUT_CAP};

/// Bytes of `git diff --unified=0` output kept.
///
/// With no context lines this is roughly the size of the change itself, so a
/// cap this size corresponds to a diff far larger than any change a human
/// reviews. Past it the read is refused rather than truncated, because a
/// prefix of a diff is a change set missing exactly the files it does not
/// reach.
pub const DIFF_OUTPUT_CAP: usize = LOG_OUTPUT_CAP;

/// Most files one change will be read as touching.
///
/// A change spanning more files than this is a merge, a vendor drop or a
/// formatting sweep, and its blast radius is "the repository". The cap is
/// reported rather than applied silently.
pub const MAX_CHANGED_FILES: usize = 1_000;

/// Why a change could not be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChangeRefusal {
    /// The underlying git read failed.
    Git { reason: String },
    /// The commit has more than one parent. A merge's "what changed" is
    /// ambiguous — against which parent? — and both of git's answers are
    /// silent failures, so it is refused instead of guessed at.
    MergeCommit { commit: String, parents: usize },
    /// A path arrived still C-quoted, so its real bytes are not recoverable
    /// without re-implementing git's unescaping.
    QuotedPath { raw: String },
    /// The output contained a combined (`@@@`) hunk header, which this parser
    /// does not read and must not read as a two-way one.
    CombinedDiff,
    /// A hunk header did not parse.
    UnparsableHunk { header: String },
    /// The diff itself was larger than [`DIFF_OUTPUT_CAP`].
    ///
    /// Its own variant rather than a [`Self::Git`] carrying a byte count,
    /// because it is the one refusal a caller can act on and the action is not
    /// obvious from "produced more than 4194304 bytes".
    DiffTooLarge { cap: usize },
}

impl ChangeRefusal {
    pub fn describe(&self) -> String {
        match self {
            ChangeRefusal::Git { reason } => reason.clone(),
            ChangeRefusal::MergeCommit { commit, parents } => format!(
                "{commit} is a merge with {parents} parents; a merge has no single \
                 \"what changed\", and diffing it against its first parent would \
                 attribute the whole merged branch to it — ask about the branch's own \
                 commits instead"
            ),
            ChangeRefusal::QuotedPath { raw } => format!(
                "git reported the path {raw} in quoted form, so its real bytes are not \
                 recoverable here; this change cannot be mapped to an indexed path"
            ),
            ChangeRefusal::CombinedDiff => {
                "the diff is a combined (merge) diff, whose hunk headers carry one range \
                 per parent; reading them as two-way ranges would produce line numbers \
                 belonging to no revision"
                    .to_string()
            }
            ChangeRefusal::UnparsableHunk { header } => {
                format!("a hunk header could not be read: {header}")
            }
            ChangeRefusal::DiffTooLarge { cap } => format!(
                "the diff between these revisions is larger than {cap} bytes and was refused \
                 rather than truncated — a prefix of a diff is a change set missing exactly \
                 the files it did not reach. Ask about a narrower range; a range this wide \
                 would in any case seed the walk from a trimmed symbol set, so it buys a less \
                 precise answer rather than a fuller one"
            ),
        }
    }
}

/// A contiguous run of post-image lines a change touched.
///
/// One-based and inclusive, matching [`crate::join`] and `git blame`, so the
/// three never need translating between each other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedRange {
    pub start_line: u32,
    pub end_line: u32,
    /// True when the hunk removed lines and added none, so this range is the
    /// *boundary* the removal left behind rather than surviving changed
    /// content.
    ///
    /// Kept as a flag rather than folded away because a reader looking at the
    /// post-image will find nothing remarkable on these lines — the evidence
    /// is what is no longer there — and a report that does not say so invites
    /// them to conclude the tool is wrong.
    pub deletion_only: bool,
}

/// How a file participated in the change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChangeStatus {
    /// The file exists in the post-image and has line ranges.
    Modified,
    /// The file is new in the post-image.
    Added,
    /// The file is gone in the post-image. It has no post-image lines, so no
    /// range can be intersected with a span — the graph built at the
    /// post-image holds nothing for it either.
    Deleted,
    /// Git reported the content as binary, so it has no lines at all.
    Binary,
}

/// One file's participation in a change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    /// The **post-image** path, which is what a graph built at the post-image
    /// knows the file as. For a rename this is the new name.
    pub path: String,
    /// The pre-image path, when it differs — a rename. Carried so a reader can
    /// see why a file with no history under this name has a blast radius.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renamed_from: Option<String>,
    pub status: ChangeStatus,
    /// Post-image ranges, in file order, non-overlapping.
    pub ranges: Vec<ChangedRange>,
}

/// Everything a change touched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSet {
    pub files: Vec<FileChange>,
    /// True when [`MAX_CHANGED_FILES`] trimmed the list, so `files` is a
    /// prefix rather than the whole change.
    pub capped: bool,
}

impl ChangeSet {
    /// Total post-image lines named by every range, counting each line once.
    ///
    /// Ranges within a file are non-overlapping by construction, so this is a
    /// plain sum rather than a union.
    pub fn touched_lines(&self) -> u64 {
        self.files
            .iter()
            .flat_map(|file| file.ranges.iter())
            // Widened *before* the arithmetic, not after. `end - start + 1` in
            // `u32` panics in a debug build whenever the difference is the
            // whole range, which a hand-built `ChangeSet` can hold — the
            // fields are public and `--at` builds one. A library that panics
            // on its own input is neither closed nor loud.
            .map(|range| u64::from(range.end_line).saturating_sub(u64::from(range.start_line)) + 1)
            .sum()
    }
}

impl ChangeSet {
    /// A change set naming one explicit line range, for "what depends on the
    /// line I am looking at" — a question with no revision range to speak of.
    ///
    /// Marked [`ChangeStatus::Modified`] because that is what it is being
    /// treated as: a region of the post-image whose dependents are wanted.
    /// Not `deletion_only` — the lines are there and the caller can read them.
    pub fn at(path: &str, start_line: u32, end_line: u32) -> Self {
        Self {
            files: vec![FileChange {
                path: path.to_string(),
                renamed_from: None,
                status: ChangeStatus::Modified,
                ranges: vec![ChangedRange {
                    start_line,
                    end_line,
                    deletion_only: false,
                }],
            }],
            capped: false,
        }
    }
}

/// Parse `path:start-end`, `path:line`, or a bare `path`, into a line range.
///
/// Lives here rather than in a host because there are two hosts — the CLI and
/// the daemon — and a location spelling that means one thing to one of them
/// and another to the other is a silent disagreement about which lines were
/// asked about. One parser, one meaning.
///
/// Splits on the **last** colon: a path may contain one, a line range never
/// does. A bare path means the whole file.
pub fn parse_location(at: &str) -> Result<(String, u32, u32), String> {
    let at = at.trim();
    if at.is_empty() {
        return Err("a location needs a path, optionally followed by `:start-end`".to_string());
    }
    let Some((path, range)) = at.rsplit_once(':') else {
        return Ok((at.to_string(), 1, u32::MAX));
    };
    let parse_line = |field: &str| -> Result<u32, String> {
        let value: u32 = field.trim().parse().map_err(|_| {
            format!(
                "`{field}` is not a line number; a location is `path`, `path:line` or \
                 `path:start-end`"
            )
        })?;
        if value == 0 {
            return Err("line numbers are one-based; there is no line 0".to_string());
        }
        Ok(value)
    };
    let (start, end) = match range.split_once('-') {
        Some((start, end)) => (parse_line(start)?, parse_line(end)?),
        None => {
            let line = parse_line(range)?;
            (line, line)
        }
    };
    if end < start {
        return Err(format!(
            "the range {start}-{end} runs backwards; a range that ends before it starts \
             names no lines"
        ));
    }
    if path.is_empty() {
        return Err("a location needs a path before the line range".to_string());
    }
    Ok((path.to_string(), start, end))
}

/// How many parents `commit` has.
///
/// Read before any diff, because the two ways git answers "what did this merge
/// change" are both silent failures and neither is detectable afterwards.
pub fn parent_count(
    program: &std::ffi::OsStr,
    repo: &Path,
    commit: &str,
) -> Result<usize, HistoryRefusal> {
    let captured = run_git(
        program,
        repo,
        &["rev-list", "--parents", "--max-count=1", commit],
        "git rev-list --parents",
        8 * 1024,
    )?;
    // `<commit> <parent>...` — the first field is the commit itself.
    Ok(captured
        .stdout_lossy()
        .split_whitespace()
        .count()
        .saturating_sub(1))
}

/// The lines `since..until` changed, as post-image ranges.
pub fn changes_between(repo: &Path, since: &str, until: &str) -> Result<ChangeSet, ChangeRefusal> {
    changes_between_with_program(std::ffi::OsStr::new("git"), repo, since, until)
}

#[doc(hidden)]
pub fn changes_between_with_program(
    program: &std::ffi::OsStr,
    repo: &Path,
    since: &str,
    until: &str,
) -> Result<ChangeSet, ChangeRefusal> {
    let captured = run_git(
        program,
        repo,
        &[
            // Raw bytes for non-ASCII paths. A path that is *still* quoted
            // after this contains control characters and is refused below
            // rather than unescaped by hand.
            "-c",
            "core.quotePath=false",
            "diff",
            // No context. Context lines would widen every hunk by three lines
            // in each direction and pull in neighbouring symbols that the
            // change did not touch.
            "--unified=0",
            "--no-color",
            // Rename detection on, so an edited-and-renamed file is one entry
            // under its post-image name rather than a delete plus an add whose
            // added half looks like a wholly new file.
            "--find-renames",
            since,
            until,
        ],
        "git diff",
        DIFF_OUTPUT_CAP,
    )
    .map_err(|refusal| match refusal {
        // The commonest way this command is refused, and the one whose
        // generic wording helps least: a caller who asked about a release-
        // sized range gets told a byte count. The cap is deliberate — a
        // prefix of a diff is a change set missing exactly the files it did
        // not reach — but a refusal that does not name the remedy reads as a
        // defect rather than as a bound.
        //
        // Widening the cap is not the remedy either: past
        // [`crate::blast::MAX_SEED_SYMBOLS`] the walk is already running from
        // a trimmed seed set, so a larger range buys a *less* precise answer,
        // not a fuller one.
        HistoryRefusal::OutputCapped { cap, .. } => ChangeRefusal::DiffTooLarge { cap },
        other => ChangeRefusal::Git {
            reason: other.describe(),
        },
    })?;
    parse_diff(&captured.stdout_lossy())
}

/// Parse `git diff --unified=0` output into per-file post-image ranges.
pub fn parse_diff(diff: &str) -> Result<ChangeSet, ChangeRefusal> {
    let mut files: Vec<FileChange> = Vec::new();
    let mut current: Option<FileChange> = None;
    // Set by the `---`/`+++` pair, which is the only place a rename's two
    // names both appear in a form this parser reads.
    let mut pre_image: Option<String> = None;
    // False until this file's first `@@`. Everything after that point is hunk
    // *content*, and content is not a header however much it looks like one.
    //
    // This is not defensive padding. `--unified=0` drops context lines but
    // still prints every changed line with a `+` or `-` prefix, so a removed
    // line reading `-- sql comment` is emitted as `--- sql comment` and an
    // added line reading `++ b/evil.rs` is emitted as `+++ b/evil.rs` —
    // byte-for-byte a file header. Read positionally, that added line opened a
    // new file and **discarded the ranges already collected for the real
    // one**: a whole file's changes vanishing from the report with nothing to
    // say they had. Verified against git rather than reasoned about.
    let mut in_hunks = false;

    for raw in diff.lines() {
        // A combined diff's header is `@@@`, and its ranges belong one to each
        // parent. Checked before the two-way header so it can never be read as
        // one with a strange first field. Content cannot reach here: a changed
        // line always carries a `+` or `-` prefix, so it can never begin `@`.
        if raw.starts_with("@@@") {
            return Err(ChangeRefusal::CombinedDiff);
        }

        if raw.starts_with("diff --git ") {
            if let Some(done) = current.take() {
                files.push(done);
            }
            pre_image = None;
            in_hunks = false;
            // The path is not taken from this line: `diff --git a/x b/y` is
            // ambiguous when a name contains a space, and the `---`/`+++` pair
            // below states each side on its own line.
            continue;
        }

        if !in_hunks && (raw.starts_with("Binary files ") || raw.starts_with("GIT binary patch")) {
            if let Some(file) = current.as_mut() {
                file.status = ChangeStatus::Binary;
                file.ranges.clear();
            }
            // Binary payload lines follow and are not diff syntax. Treated as
            // hunk content so nothing in them is read as a header.
            in_hunks = true;
            continue;
        }

        if !in_hunks {
            if let Some(rest) = raw.strip_prefix("--- ") {
                pre_image = strip_side(rest)?;
                continue;
            }
        }

        if let Some(rest) = raw.strip_prefix("+++ ").filter(|_| !in_hunks) {
            let post = strip_side(rest)?;
            match post {
                // `+++ /dev/null` — the file is gone in the post-image.
                None => {
                    let path = pre_image.clone().unwrap_or_default();
                    if !path.is_empty() {
                        current = Some(FileChange {
                            path,
                            renamed_from: None,
                            status: ChangeStatus::Deleted,
                            ranges: Vec::new(),
                        });
                    }
                }
                Some(path) => {
                    // `--- /dev/null` — the file is new in the post-image.
                    let status = if pre_image.is_none() {
                        ChangeStatus::Added
                    } else {
                        ChangeStatus::Modified
                    };
                    let renamed_from = pre_image
                        .as_ref()
                        .filter(|before| *before != &path)
                        .cloned();
                    current = Some(FileChange {
                        path,
                        renamed_from,
                        status,
                        ranges: Vec::new(),
                    });
                }
            }
            continue;
        }

        if raw.starts_with("@@ ") {
            in_hunks = true;
            let range = parse_hunk(raw)?;
            if let Some(file) = current.as_mut() {
                // Only a file that *has* a post-image has post-image ranges.
                // A deleted file still emits a hunk — `@@ -1,9 +0,0 @@` — and
                // reading its `+0,0` as a deletion boundary would manufacture
                // a range at line 1 of a file that no longer exists, which
                // then matches whatever the graph still holds under that path.
                // A binary file has no lines at all.
                if matches!(file.status, ChangeStatus::Modified | ChangeStatus::Added) {
                    file.ranges.push(range);
                }
            }
            continue;
        }
    }
    if let Some(done) = current.take() {
        files.push(done);
    }

    // A file may appear once per `diff --git` block; ranges inside one arrive
    // in ascending order from git. Merging touching or overlapping ranges
    // keeps the later line-count arithmetic a plain sum.
    for file in &mut files {
        file.ranges = merge_ranges(std::mem::take(&mut file.ranges));
    }

    let capped = files.len() > MAX_CHANGED_FILES;
    files.truncate(MAX_CHANGED_FILES);
    Ok(ChangeSet { files, capped })
}

/// The path from a `---`/`+++` line, or `None` for `/dev/null`.
///
/// Refuses a quoted path rather than unescaping it: git quotes exactly when
/// the name contains bytes that would not survive this line-oriented format,
/// and a hand-rolled unescape that gets one case wrong produces a path that
/// silently matches nothing in the graph.
fn strip_side(rest: &str) -> Result<Option<String>, ChangeRefusal> {
    let rest = rest.trim_end();
    if rest == "/dev/null" {
        return Ok(None);
    }
    if rest.starts_with('"') {
        return Err(ChangeRefusal::QuotedPath {
            raw: rest.to_string(),
        });
    }
    // `a/` and `b/` are git's diff prefixes, not part of the path.
    let path = rest
        .strip_prefix("a/")
        .or_else(|| rest.strip_prefix("b/"))
        .unwrap_or(rest);
    if path.is_empty() {
        return Ok(None);
    }
    Ok(Some(path.to_string()))
}

/// Read the post-image range out of `@@ -a,b +c,d @@`.
fn parse_hunk(header: &str) -> Result<ChangedRange, ChangeRefusal> {
    let unparsable = || ChangeRefusal::UnparsableHunk {
        header: header.to_string(),
    };
    // Everything between the first `@@ ` and the next ` @@`. A section heading
    // may follow the second `@@` and may itself contain `@@`, so the *first*
    // closing marker is the right one.
    let body = header.strip_prefix("@@ ").ok_or_else(unparsable)?;
    let body = body.split(" @@").next().ok_or_else(unparsable)?;
    let post = body
        .split_whitespace()
        .find_map(|field| field.strip_prefix('+'))
        .ok_or_else(unparsable)?;

    let (start, count) = match post.split_once(',') {
        Some((start, count)) => (start, count),
        // An omitted count means exactly one line.
        None => (post, "1"),
    };
    let start: u32 = start.parse().map_err(|_| unparsable())?;
    let count: u32 = count.parse().map_err(|_| unparsable())?;

    if count == 0 {
        // A pure deletion. Git names the line *before* the removed region, and
        // the removal sits between it and the next one. Both are candidates
        // for having contained the removed code, so both are in range.
        //
        // `start` is 0 when the removal was at the very top of the file, and
        // there is no line 0 — the boundary is then above line 1.
        let start_line = start.max(1);
        let end_line = start.saturating_add(1).max(1);
        return Ok(ChangedRange {
            start_line,
            end_line,
            deletion_only: true,
        });
    }

    // A non-zero count always names real post-image lines, so a start of 0
    // would be git contradicting itself. Clamped to 1 rather than refused:
    // the range is still the right region, and losing the whole file's answer
    // over an off-by-one in the header helps nobody.
    let start_line = start.max(1);
    Ok(ChangedRange {
        start_line,
        // `start + (count - 1)`, not `start + count - 1`. The two agree on
        // every ordinary input and differ on the one that matters: with
        // `start` at the type maximum, `saturating_add` pins the sum there and
        // the trailing `- 1` then drags it *below* `start`, yielding a range
        // that ends before it begins. Saturating arithmetic composed in the
        // wrong order is still invalid arithmetic; subtracting inside the
        // parenthesis cannot produce one.
        end_line: start_line.saturating_add(count.saturating_sub(1)),
        deletion_only: false,
    })
}

/// Merge overlapping and adjacent ranges, preserving order.
///
/// `deletion_only` survives only when *every* merged part carried it: a range
/// that absorbed real added lines is no longer a bare boundary, and labelling
/// it one would tell a reader to go looking for absent code when the code is
/// right there.
fn merge_ranges(mut ranges: Vec<ChangedRange>) -> Vec<ChangedRange> {
    if ranges.len() < 2 {
        return ranges;
    }
    ranges.sort_by(|a, b| {
        a.start_line
            .cmp(&b.start_line)
            .then(a.end_line.cmp(&b.end_line))
    });
    let mut merged: Vec<ChangedRange> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            // `+ 1` so ranges that merely touch also merge: lines 1-3 and 4-6
            // describe one region of six lines, not two.
            Some(last) if range.start_line <= last.end_line.saturating_add(1) => {
                last.end_line = last.end_line.max(range.end_line);
                last.deletion_only = last.deletion_only && range.deletion_only;
            }
            _ => merged.push(range),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_hunk_names_its_post_image_lines() {
        let range = parse_hunk("@@ -10,3 +12,4 @@").expect("parses");
        assert_eq!(range.start_line, 12);
        assert_eq!(range.end_line, 15, "four lines beginning at 12");
        assert!(!range.deletion_only);
    }

    #[test]
    fn an_omitted_count_means_one_line() {
        let range = parse_hunk("@@ -10 +12 @@").expect("parses");
        assert_eq!((range.start_line, range.end_line), (12, 12));
    }

    /// The header that disappears if read literally. A post-image count of
    /// zero is not "no lines changed"; it is "lines were removed from here".
    #[test]
    fn a_pure_deletion_is_the_boundary_it_left_behind() {
        let range = parse_hunk("@@ -40,12 +39,0 @@").expect("parses");
        assert!(
            range.deletion_only,
            "the range must say the evidence is what is no longer there"
        );
        assert_eq!(
            (range.start_line, range.end_line),
            (39, 40),
            "the removal sat between post-image lines 39 and 40, so a symbol \
             spanning either one lost code"
        );
    }

    #[test]
    fn a_deletion_at_the_top_of_a_file_does_not_name_line_zero() {
        let range = parse_hunk("@@ -1,5 +0,0 @@").expect("parses");
        assert_eq!(range.start_line, 1, "there is no line 0");
        assert!(range.end_line >= range.start_line);
    }

    /// A section heading is free text and may contain anything, including the
    /// marker that ends the range.
    #[test]
    fn a_section_heading_containing_the_marker_does_not_confuse_the_range() {
        let range = parse_hunk("@@ -1,2 +3,4 @@ fn f() { /* @@ */ }").expect("parses");
        assert_eq!((range.start_line, range.end_line), (3, 6));
    }

    #[test]
    fn a_malformed_header_is_refused_rather_than_guessed_at() {
        assert!(matches!(
            parse_hunk("@@ not a hunk @@"),
            Err(ChangeRefusal::UnparsableHunk { .. })
        ));
        assert!(matches!(
            parse_hunk("@@ -1,2 +x,4 @@"),
            Err(ChangeRefusal::UnparsableHunk { .. })
        ));
    }

    #[test]
    fn a_combined_diff_is_refused_not_misread() {
        let diff = "diff --cc src/lib.rs\n@@@ -1,2 -1,2 +1,3 @@@\n";
        assert!(matches!(parse_diff(diff), Err(ChangeRefusal::CombinedDiff)));
    }

    #[test]
    fn a_quoted_path_is_refused_rather_than_unescaped_by_hand() {
        let diff = "diff --git a/x b/x\n--- a/x\n+++ \"b/od\\ted\"\n@@ -1 +1 @@\n";
        assert!(matches!(
            parse_diff(diff),
            Err(ChangeRefusal::QuotedPath { .. })
        ));
    }

    #[test]
    fn a_modified_file_carries_its_post_image_path_and_ranges() {
        let diff = "diff --git a/src/lib.rs b/src/lib.rs\n\
                    --- a/src/lib.rs\n\
                    +++ b/src/lib.rs\n\
                    @@ -1,2 +1,3 @@\n\
                    @@ -20,0 +30,2 @@\n";
        let set = parse_diff(diff).expect("parses");
        assert_eq!(set.files.len(), 1);
        assert_eq!(set.files[0].path, "src/lib.rs");
        assert_eq!(set.files[0].status, ChangeStatus::Modified);
        assert_eq!(set.files[0].ranges.len(), 2);
        assert_eq!(set.files[0].ranges[0].start_line, 1);
        assert_eq!(set.files[0].ranges[1].start_line, 30);
    }

    #[test]
    fn a_new_file_is_added_not_modified() {
        let diff = "diff --git a/n.rs b/n.rs\n--- /dev/null\n+++ b/n.rs\n@@ -0,0 +1,5 @@\n";
        let set = parse_diff(diff).expect("parses");
        assert_eq!(set.files[0].status, ChangeStatus::Added);
        assert_eq!(
            (
                set.files[0].ranges[0].start_line,
                set.files[0].ranges[0].end_line
            ),
            (1, 5)
        );
    }

    /// A deleted file has no post-image content, so it has no range that could
    /// be intersected with a span. Recorded as deleted rather than dropped, so
    /// a caller can say why it has no symbols instead of omitting it.
    #[test]
    fn a_deleted_file_is_recorded_with_no_ranges() {
        let diff = "diff --git a/g.rs b/g.rs\n--- a/g.rs\n+++ /dev/null\n@@ -1,9 +0,0 @@\n";
        let set = parse_diff(diff).expect("parses");
        assert_eq!(set.files[0].status, ChangeStatus::Deleted);
        assert_eq!(set.files[0].path, "g.rs");
        assert!(set.files[0].ranges.is_empty());
    }

    #[test]
    fn a_rename_reports_the_post_image_name_and_where_it_came_from() {
        let diff = "diff --git a/old.rs b/new.rs\n--- a/old.rs\n+++ b/new.rs\n@@ -1 +1 @@\n";
        let set = parse_diff(diff).expect("parses");
        assert_eq!(
            set.files[0].path, "new.rs",
            "the graph knows the file by its post-image name"
        );
        assert_eq!(set.files[0].renamed_from.as_deref(), Some("old.rs"));
    }

    /// Real `git diff --unified=0` output, byte for byte, for a commit whose
    /// added line reads `++ b/evil.rs`. `--unified=0` drops *context* lines
    /// but still prefixes every changed line, so that added line is emitted as
    /// `+++ b/evil.rs` — indistinguishable from a file header by position.
    ///
    /// Read as one, it opened a second file and discarded every range already
    /// collected for `f.rs`. The whole file's changes disappeared from the
    /// report, and nothing in the report said they had.
    #[test]
    fn a_changed_line_that_looks_like_a_file_header_is_content() {
        let diff = "diff --git a/f.rs b/f.rs\n\
                    index ffbd050..c7f305d 100644\n\
                    --- a/f.rs\n\
                    +++ b/f.rs\n\
                    @@ -2 +2 @@ fn a() {}\n\
                    -old line\n\
                    +++ b/evil.rs\n\
                    @@ -3,0 +4 @@ fn b() {}\n\
                    +-- sql comment\n";
        let set = parse_diff(diff).expect("parses");
        assert_eq!(
            set.files.len(),
            1,
            "an added line beginning `++ ` is content, not a second file: {:?}",
            set.files.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
        assert_eq!(set.files[0].path, "f.rs");
        assert_eq!(
            set.files[0].ranges.len(),
            2,
            "both hunks survive; reading the content line as a header dropped the first"
        );
    }

    /// The `---` half of the same defect, and the more damaging one. A removed
    /// line beginning `-- "` — an SQL comment introducing a quoted identifier,
    /// a signature delimiter — is emitted as `--- "…`. Read as a pre-image
    /// header it is a *quoted path*, which refuses the **whole change**: every
    /// other file in the commit is lost because one line of one file's content
    /// began with two dashes and a quote.
    #[test]
    fn a_removed_line_beginning_with_dashes_does_not_refuse_the_whole_change() {
        let diff = "diff --git a/q.sql b/q.sql\n\
                    --- a/q.sql\n\
                    +++ b/q.sql\n\
                    @@ -5 +5 @@\n\
                    --- \"quoted identifier\" is not a path\n\
                    +SELECT 1;\n\
                    diff --git a/other.rs b/other.rs\n\
                    --- a/other.rs\n\
                    +++ b/other.rs\n\
                    @@ -1 +1 @@\n\
                    +fn f() {}\n";
        let set =
            parse_diff(diff).expect("a content line beginning `-- ` must not refuse the change");
        assert_eq!(
            set.files.len(),
            2,
            "both files survive; reading the content line as a quoted path lost both"
        );
        assert_eq!(set.files[1].path, "other.rs");
        assert_eq!(
            set.files[0].renamed_from, None,
            "the content line must not be read as a rename's pre-image name"
        );
    }

    /// A quoted path is refused — but only when it is actually a header. An
    /// added line that happens to begin with a quote is ordinary content, and
    /// refusing the whole change over it would lose every other file in it.
    #[test]
    fn a_quoted_looking_content_line_does_not_refuse_the_change() {
        let diff = "diff --git a/j.rs b/j.rs\n\
                    --- a/j.rs\n\
                    +++ b/j.rs\n\
                    @@ -1 +1 @@\n\
                    +++ \"b/not a header\"\n";
        let set = parse_diff(diff).expect("content is not a header, so nothing is refused");
        assert_eq!(set.files.len(), 1);
        assert_eq!(set.files[0].path, "j.rs");
    }

    #[test]
    fn a_binary_file_has_no_lines_and_says_so() {
        let diff = "diff --git a/i.png b/i.png\n\
                    --- a/i.png\n+++ b/i.png\n\
                    Binary files a/i.png and b/i.png differ\n";
        let set = parse_diff(diff).expect("parses");
        assert_eq!(set.files[0].status, ChangeStatus::Binary);
        assert!(set.files[0].ranges.is_empty());
    }

    #[test]
    fn touching_ranges_merge_into_one_region() {
        let merged = merge_ranges(vec![
            ChangedRange {
                start_line: 1,
                end_line: 3,
                deletion_only: false,
            },
            ChangedRange {
                start_line: 4,
                end_line: 6,
                deletion_only: false,
            },
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!((merged[0].start_line, merged[0].end_line), (1, 6));
    }

    /// A merged range that absorbed a real edit is not a bare deletion
    /// boundary any more, and must not keep telling the reader to go looking
    /// for code that is not there.
    #[test]
    fn a_merged_range_is_deletion_only_only_when_every_part_was() {
        let merged = merge_ranges(vec![
            ChangedRange {
                start_line: 1,
                end_line: 2,
                deletion_only: true,
            },
            ChangedRange {
                start_line: 2,
                end_line: 4,
                deletion_only: false,
            },
        ]);
        assert_eq!(merged.len(), 1);
        assert!(!merged[0].deletion_only);

        let both = merge_ranges(vec![
            ChangedRange {
                start_line: 1,
                end_line: 2,
                deletion_only: true,
            },
            ChangedRange {
                start_line: 3,
                end_line: 4,
                deletion_only: true,
            },
        ]);
        assert!(both[0].deletion_only);
    }

    #[test]
    fn separate_regions_do_not_merge() {
        let merged = merge_ranges(vec![
            ChangedRange {
                start_line: 1,
                end_line: 3,
                deletion_only: false,
            },
            ChangedRange {
                start_line: 100,
                end_line: 101,
                deletion_only: false,
            },
        ]);
        assert_eq!(merged.len(), 2);
    }

    /// The refusal a caller is most likely to meet, and the only one they can
    /// act on. It must name the action.
    #[test]
    fn an_oversized_diff_says_what_to_do_about_it() {
        let described = ChangeRefusal::DiffTooLarge {
            cap: DIFF_OUTPUT_CAP,
        }
        .describe();
        assert!(
            described.contains("narrower range"),
            "the remedy is named, not just the limit: {described}"
        );
        assert!(
            described.contains("rather than truncated"),
            "and why a prefix was not returned instead: {described}"
        );
    }

    #[test]
    fn a_location_reads_a_range_a_single_line_and_a_bare_path() {
        assert_eq!(
            parse_location("src/f.rs:10-20").expect("range"),
            ("src/f.rs".to_string(), 10, 20)
        );
        assert_eq!(
            parse_location("src/f.rs:7").expect("one line"),
            ("src/f.rs".to_string(), 7, 7)
        );
        let (path, start, end) = parse_location("src/f.rs").expect("bare path");
        assert_eq!(path, "src/f.rs");
        assert_eq!((start, end), (1, u32::MAX), "a bare path is the whole file");
    }

    /// Split from the right, so a path containing a colon keeps it. Splitting
    /// from the left would cut `C:/x.rs:4` into a path of `C` and a range of
    /// `/x.rs:4`, which names nothing.
    #[test]
    fn a_path_containing_a_colon_keeps_it() {
        assert_eq!(
            parse_location("C:/src/f.rs:4").expect("parses"),
            ("C:/src/f.rs".to_string(), 4, 4)
        );
    }

    #[test]
    fn a_backwards_or_zero_location_is_refused() {
        assert!(parse_location("f.rs:9-2")
            .expect_err("refused")
            .contains("backwards"));
        assert!(parse_location("f.rs:0-4")
            .expect_err("refused")
            .contains("one-based"));
        assert!(parse_location("f.rs:x-4").is_err());
        assert!(parse_location("   ").is_err());
    }

    #[test]
    fn touched_lines_counts_every_named_line_once() {
        let set = ChangeSet {
            files: vec![FileChange {
                path: "a.rs".into(),
                renamed_from: None,
                status: ChangeStatus::Modified,
                ranges: vec![
                    ChangedRange {
                        start_line: 1,
                        end_line: 3,
                        deletion_only: false,
                    },
                    ChangedRange {
                        start_line: 10,
                        end_line: 10,
                        deletion_only: false,
                    },
                ],
            }],
            capped: false,
        };
        assert_eq!(set.touched_lines(), 4);
    }
}
