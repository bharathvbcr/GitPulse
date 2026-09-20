//! The substance gate: how much of a diff is new work.
//!
//! Every other gate here answers a question of the form "is there something
//! wrong with this change". None of them answers "is there anything *in* this
//! change", and the two are not the same: a diff of two hundred closing braces,
//! relocated functions and a regenerated lockfile has no stub marker, no
//! credential, and — because moved code carries its moved tests — no coverage
//! gap either. It passes cleanly. `findings: []` then means what it always
//! means, "these gates ran and found nothing", and a caller reasonably reads
//! that as work having been done.
//!
//! This is the gate borrowed from reverify's information weighting, which makes
//! the same argument about a different subject: "every claim verified" is
//! trivially reachable by asserting that a PE file starts with `MZ`, so it
//! weighs each verified claim by how much it says about the binary rather than
//! counting it. The subject here is a diff rather than an artifact, so the
//! measure is different, but the failure it closes is identical — a verdict
//! that is cheap to satisfy without doing the thing the verdict is about.
//!
//! **This gate does not decide whether a change is good.** A pure refactor is
//! almost entirely moved lines and *should* score near zero; that is the
//! correct reading of a pure refactor, and it is useful precisely because it
//! tells a reviewer to review it as a relocation. So this produces a
//! measurement and never a blocking finding. What a host does with the number
//! is the host's policy, and [`SubstanceReport::is_low`] is offered as one
//! answer rather than imposed as the answer.
//!
//! # What is deliberately not here
//!
//! reverify weighs by Shannon entropy and occurrence frequency in the artifact.
//! The entropy half does not transfer: source code is low-entropy by design,
//! legitimately repetitive code (a match arm per variant, a table of test
//! cases) would score as padding, and a gate with that false-positive rate gets
//! turned off — which is the outcome this file's sibling gates are written to
//! avoid. Every classification below is instead a decision about text that a
//! reader can check by eye against the line it fired on.

use crate::FileDiff;
use std::collections::HashSet;

/// Which bucket one added line falls into.
///
/// Exactly one, and the order of the checks in [`classify_line`] is what makes
/// that true. The buckets sum to the added-line count, which is the property
/// that makes the report arithmetic rather than opinion — and which
/// `buckets_partition_the_added_lines` holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineClass {
    /// New text, in a hand-written file, that this diff has not already added.
    Substantive,
    /// Structure rather than content: a brace, a bracket, a blank line.
    Trivial,
    /// Identical to a line the same diff removed. Relocated, not written.
    Moved,
    /// Identical to a line this diff already added somewhere.
    Repeated,
    /// In a file whose contents a tool produces. See [`is_generated_path`].
    Generated,
}

/// One file's share of the measurement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSubstance {
    pub path: String,
    pub added_lines: usize,
    pub substantive_lines: usize,
}

/// What the diff is made of.
///
/// Counts only. No ratio is computed here and none is put on the wire: a
/// consumer that is handed `0.11` cannot tell 1-of-9 from 111-of-999, and the
/// two deserve completely different responses. Handing over both numbers costs
/// one field and removes the question — and it keeps a float out of a
/// hand-rolled JSON encoder, where formatting and locale are a real hazard for
/// no gain.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubstanceReport {
    pub added_lines: usize,
    pub substantive_lines: usize,
    pub trivial: usize,
    pub moved: usize,
    pub repeated: usize,
    pub generated: usize,
    /// Per file, sorted by path, and only for files that added a line.
    pub files: Vec<FileSubstance>,
}

/// The smallest diff this gate will judge.
///
/// Below it the ratio is noise rather than signal: a three-line diff that
/// closes a block scores zero, which says nothing about the change, and a gate
/// that fires there teaches a reader to ignore it.
///
/// Twenty excludes 39 of the 200 commits in the calibration set (see
/// [`LOW_SUBSTANCE_NUMERATOR`]) — about a fifth of real history is too small
/// for this measurement to describe, and [`SubstanceReport::judged`] says so
/// rather than reporting a ratio nobody should act on.
pub const MIN_LINES_TO_JUDGE: usize = 20;

/// The ratio below which [`SubstanceReport::is_low`] answers true, as an exact
/// fraction — `substantive * DENOMINATOR < added * NUMERATOR`.
///
/// # Where this number comes from
///
/// Measured on the last 200 non-merge commits of this repository at 0944ef51,
/// one run of the real `dcverify` binary per commit over its full diff. 161 of
/// the 200 added at least [`MIN_LINES_TO_JUDGE`] lines and were therefore
/// judged. The substantive ratio over that set:
///
/// | percentile | ratio |
/// |---|---|
/// | minimum | 0.17 |
/// | 5th | 0.40 |
/// | 25th | 0.61 |
/// | median | 0.68 |
/// | 75th | 0.77 |
/// | maximum | 0.97 |
///
/// Ordinary work on this codebase sits between 0.61 and 0.77, and no judged
/// commit reached 1.00 — a real change always carries some structure. One
/// quarter is below the 5th percentile and above the observed floor, so it
/// separates the shape it is for from the tail of normal development rather
/// than cutting into it.
///
/// Exactly one commit of the 161 falls under it, and it was inspected rather
/// than counted: `89a1d676`, "Prepare DevCouncil v0.4.5 and DevMap v0.1.1 for
/// integrated hosts" — 31 substantive lines of 182, of which 141 are the
/// `Cargo.lock` a version bump regenerates. That is a true statement about that
/// commit and the one the measurement exists to make.
///
/// Reproduce with `tools/substance_calibration.sh`.
pub const LOW_SUBSTANCE_NUMERATOR: usize = 1;
/// Denominator of [`LOW_SUBSTANCE_NUMERATOR`].
pub const LOW_SUBSTANCE_DENOMINATOR: usize = 4;

impl SubstanceReport {
    /// Whether the diff is large enough for the ratio to carry information.
    ///
    /// Separate from [`is_low`](Self::is_low) on purpose. "Too small to judge"
    /// and "judged and fine" are different answers, and collapsing them into
    /// one boolean is the shape of mistake the rest of this crate exists to
    /// refuse.
    pub fn judged(&self) -> bool {
        self.added_lines >= MIN_LINES_TO_JUDGE
    }

    /// Whether a judged diff falls below [`LOW_SUBSTANCE_NUMERATOR`].
    ///
    /// False for a diff too small to judge — not because such a diff is known
    /// to be fine, but because this function has nothing to say about it. Ask
    /// [`judged`](Self::judged) to tell the two apart.
    pub fn is_low(&self) -> bool {
        self.judged()
            && self.substantive_lines * LOW_SUBSTANCE_DENOMINATOR
                < self.added_lines * LOW_SUBSTANCE_NUMERATOR
    }
}

/// Measures a parsed diff.
///
/// Both passes are over the whole diff rather than per file, and that is the
/// point of each: a function moved from one file to another is moved, not
/// written, and a block pasted into six files is written once. A per-file
/// measurement would score both as entirely new.
pub fn measure(files: &[FileDiff]) -> SubstanceReport {
    // Removed text, keyed by its trimmed content. Trimmed because a relocation
    // almost always re-indents — a function lifted into a new `impl` block or
    // out of one shifts by a level — and a comparison that counted that as new
    // work would answer "moved" only for the rarest kind of move.
    let mut removed: HashSet<&str> = HashSet::new();
    for file in files {
        for line in &file.removed_lines {
            removed.insert(line.trim());
        }
    }

    let mut seen_added: HashSet<&str> = HashSet::new();
    let mut report = SubstanceReport::default();

    for file in files {
        if file.added_lines.is_empty() {
            continue;
        }
        let generated = is_generated_path(&file.path);
        let mut file_substantive = 0usize;

        for (_, content) in &file.added_lines {
            let trimmed = content.trim();
            // `insert` returns false when the line was already present, which
            // is the repeat test — and it must run for every line, including
            // the ones classified before it reaches the check, or the *first*
            // substantive occurrence of a line whose earlier twin sat in a
            // generated file would be scored as a repeat.
            let first_occurrence = seen_added.insert(trimmed);
            match classify_line(trimmed, generated, &removed, first_occurrence) {
                LineClass::Substantive => {
                    report.substantive_lines += 1;
                    file_substantive += 1;
                }
                LineClass::Trivial => report.trivial += 1,
                LineClass::Moved => report.moved += 1,
                LineClass::Repeated => report.repeated += 1,
                LineClass::Generated => report.generated += 1,
            }
            report.added_lines += 1;
        }

        report.files.push(FileSubstance {
            path: file.path.clone(),
            added_lines: file.added_lines.len(),
            substantive_lines: file_substantive,
        });
    }

    report.files.sort_by(|a, b| a.path.cmp(&b.path));
    report
}

/// Puts one added line in exactly one bucket.
///
/// The order is the specification:
///
/// 1. **Generated** first, because nothing else about a lockfile line is worth
///    computing — its triviality and its novelty are equally beside the point.
/// 2. **Trivial** next, before the move and repeat tests. A closing brace is
///    identical to thousands of others; letting it reach those tests would file
///    the diff's entire punctuation under `moved` or `repeated` and make those
///    two counts meaningless as a signal about code.
/// 3. **Moved** before **repeated**, because a line that is both — relocated
///    twice — is more usefully described as relocated.
fn classify_line(
    trimmed: &str,
    generated: bool,
    removed: &HashSet<&str>,
    first_occurrence: bool,
) -> LineClass {
    if generated {
        return LineClass::Generated;
    }
    if is_trivial(trimmed) {
        return LineClass::Trivial;
    }
    if removed.contains(trimmed) {
        return LineClass::Moved;
    }
    if !first_occurrence {
        return LineClass::Repeated;
    }
    LineClass::Substantive
}

/// Block delimiters that carry no content despite spelling a word.
///
/// [`is_trivial`]'s alphanumeric-run rule catches punctuation and would let
/// these through on the strength of four letters. They are here because they
/// are frequent enough to move the ratio on a large refactor and because each
/// is unambiguously structure — unlike `break` or `return`, which are
/// statements and are deliberately absent.
const STRUCTURAL_WORDS: &[&str] = &[
    "else",
    "end",
    "end;",
    "endif",
    "#endif",
    "fi",
    "done",
    "esac",
    "endfunction",
    "endmodule",
    "endwhile",
    "endfor",
    "then",
    "do",
    "begin",
    "loop",
    "}else{",
];

/// Whether a line is structure rather than content.
///
/// The rule is one sentence: a line is trivial when it contains no run of three
/// or more alphanumeric characters, or when what remains after its punctuation
/// is a bare block keyword.
///
/// Three is the threshold because two-character identifiers are punctuation's
/// neighbours in practice (`ok`, `id`, `fi`) while three-character ones are
/// ordinarily real (`let`, `for`, `key`). It is a definition rather than a
/// guess — it is applied exactly, and a reader can check any line against it —
/// which is why a finding resting on it can be called measured.
fn is_trivial(trimmed: &str) -> bool {
    if trimmed.is_empty() {
        return true;
    }
    // Compare against the structural list with every non-alphanumeric character
    // dropped, so `} else {`, `}else{` and `else` are one case rather than
    // three, and a trailing `;` or `,` does not smuggle a keyword past the
    // list.
    let squeezed: String = trimmed
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    if !squeezed.is_empty()
        && STRUCTURAL_WORDS.iter().any(|word| {
            let target: String = word
                .chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect();
            target == squeezed
        })
    {
        return true;
    }
    // No alphanumeric run of three. `char_indices` over the trimmed line rather
    // than the squeezed one: the run must be contiguous in the source text, or
    // `a.b.c` would read as the three-character run `abc`.
    let mut run = 0usize;
    for c in trimmed.chars() {
        if c.is_alphanumeric() {
            run += 1;
            if run >= 3 {
                return false;
            }
        } else {
            run = 0;
        }
    }
    true
}

/// Whether a path's contents are produced by a tool rather than written.
///
/// This is the one heuristic in the module, and it is reported as its own count
/// so a reader can see how much of a verdict rests on it. It is a claim about
/// what a path *means*, and a repository that keeps hand-written code under
/// `vendor/` would be misread — which is a reason to report the number
/// separately, not a reason to leave the lines uncounted, because the
/// alternative is scoring a regenerated lockfile as three thousand lines of new
/// work.
pub fn is_generated_path(path: &str) -> bool {
    /// Whole directory segments whose contents are vendored or produced.
    const GENERATED_SEGMENTS: &[&str] = &[
        "vendor",
        "node_modules",
        "third_party",
        "thirdparty",
        "__pycache__",
        "__snapshots__",
    ];
    /// Complete file names that are always tool output.
    const GENERATED_NAMES: &[&str] = &[
        "Cargo.lock",
        "package-lock.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "go.sum",
        "poetry.lock",
        "Gemfile.lock",
        "composer.lock",
        "uv.lock",
        "Pipfile.lock",
    ];
    /// Suffixes that mark a generated file whatever it is called.
    const GENERATED_SUFFIXES: &[&str] = &[
        ".pb.go",
        ".pb.cc",
        ".pb.h",
        "_pb2.py",
        "_pb2_grpc.py",
        "_generated.go",
        "_gen.go",
        ".generated.ts",
        ".generated.js",
        ".g.dart",
        ".freezed.dart",
        ".min.js",
        ".min.css",
        ".snap",
    ];

    // Split on both separators: a diff carries forward slashes, but a caller
    // may hand this crate paths straight from a Windows working tree, and a
    // classifier that silently stopped recognising `vendor\` there would report
    // a vendored refresh as hand-written work on exactly one platform.
    if path
        .split(['/', '\\'])
        .any(|segment| GENERATED_SEGMENTS.contains(&segment))
    {
        return true;
    }
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    if GENERATED_NAMES.contains(&name) {
        return true;
    }
    GENERATED_SUFFIXES
        .iter()
        .any(|suffix| name.ends_with(suffix))
}
