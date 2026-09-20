//! Attributing blamed lines to graph symbols — and refusing to when the two
//! do not describe the same bytes.
//!
//! This is the module the crate exists to get right. A graph records extents
//! as byte offsets; blame answers in line numbers; converting between them
//! needs the content, and *which* content is the entire question. The
//! conversion never fails loudly on the wrong content — `LineIndex` clamps an
//! out-of-range offset to the last line, by design, so that one emoji cannot
//! abort an export. That clamp is correct for its own purpose and actively
//! dangerous for this one: fed a line index built from a different revision it
//! returns an ordered, plausible, wrong range, and every symbol attributed
//! through it is wrong with no signal that anything happened.
//!
//! So the basis travels with the data and is checked before any arithmetic
//! runs. [`attribute`] returns [`Attribution::BasisMismatch`] rather than a
//! clamped answer.

use devmap_extract::model::{LineIndex, Span};
use serde::{Deserialize, Serialize};

use crate::{BlameLine, BlobIdentity, FileBlame, GraphSymbol};

/// A symbol with the commits that last touched its own lines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolBlame {
    pub qualified_name: String,
    pub file_path: String,
    /// One-based, inclusive — the same convention `LineIndex` produces and
    /// `git blame` reports, so the two never need translating again.
    pub start_line: u32,
    pub end_line: u32,
    /// Distinct commits touching this symbol's lines, most lines first.
    pub commits: Vec<CommitTouch>,
    /// Lines inside the symbol that are not committed. A symbol made entirely
    /// of these has no commit to suspect, and saying so is different from
    /// saying nothing touched it.
    pub uncommitted_lines: u32,
}

/// One commit's footprint inside one symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitTouch {
    pub commit: String,
    pub author: String,
    pub author_time: i64,
    /// How many of the symbol's lines this commit last touched.
    pub lines: u32,
}

/// The outcome of attributing one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Attribution {
    /// Symbols attributed. May be empty when the file declares none.
    Attributed(Vec<SymbolBlame>),
    /// The spans and the lines do not describe the same bytes. No arithmetic
    /// was performed, because any result would be plausible and unfalsifiable.
    BasisMismatch {
        path: String,
        spans_taken_against: String,
        lines_built_from: String,
    },
}

/// Attribute a file's blamed lines to the symbols the graph declares in it.
///
/// `content` must be the bytes `blame.basis` names; `spans_basis` is what the
/// graph's offsets were taken against. Both are required and both are checked
/// — passing the content without the basis is how a caller would reintroduce
/// exactly the bug this signature exists to prevent.
pub fn attribute(
    symbols: &[GraphSymbol],
    spans_basis: &BlobIdentity,
    blame: &FileBlame,
    content: &str,
) -> Attribution {
    if !spans_basis.comparable_with(&blame.basis) {
        return Attribution::BasisMismatch {
            path: blame.path.clone(),
            spans_taken_against: spans_basis.describe(),
            lines_built_from: blame.basis.describe(),
        };
    }

    let index = LineIndex::new(content);
    // Blame is keyed by line for an O(1) lookup per line of each span, rather
    // than a scan of the whole file per symbol. A file with k symbols and n
    // lines would otherwise be O(k·n), and the files this runs on are the
    // large ones by construction — that is why they have regressions.
    let mut by_line: std::collections::HashMap<u32, &BlameLine> =
        std::collections::HashMap::with_capacity(blame.lines.len());
    for line in &blame.lines {
        by_line.insert(line.line_no, line);
    }

    let mut out = Vec::with_capacity(symbols.len());
    for symbol in symbols {
        // The clamp is `LineIndex`'s own, applied to a span we have already
        // established indexes this very content — which is the only condition
        // under which clamping means "the last line" rather than "some line".
        let span = Span {
            start_byte: symbol.span_start,
            end_byte: symbol.span_end,
        };
        let (start_line, end_line) = index.line_range(&span);
        let (start_line, end_line) = if end_line < start_line {
            // A stored span whose end precedes its start cannot describe a
            // range. Degenerate rather than inverted: one line, the one the
            // symbol starts on.
            (start_line, start_line)
        } else {
            (start_line, end_line)
        };

        let mut tally: std::collections::HashMap<&str, (u32, &BlameLine)> =
            std::collections::HashMap::new();
        let mut uncommitted_lines = 0u32;
        for line_no in start_line..=end_line {
            let Some(line) = by_line.get(&line_no) else {
                continue;
            };
            if line.uncommitted {
                uncommitted_lines += 1;
                continue;
            }
            let entry = tally.entry(line.commit.as_str()).or_insert((0, line));
            entry.0 += 1;
        }

        let mut commits: Vec<CommitTouch> = tally
            .into_iter()
            .map(|(commit, (lines, line))| CommitTouch {
                commit: commit.to_string(),
                author: line.author.clone(),
                author_time: line.author_time,
                lines,
            })
            .collect();
        // Most lines first, then newest, then by id — a total order, so two
        // runs over the same input produce the same report.
        commits.sort_by(|a, b| {
            b.lines
                .cmp(&a.lines)
                .then(b.author_time.cmp(&a.author_time))
                .then(a.commit.cmp(&b.commit))
        });

        out.push(SymbolBlame {
            qualified_name: symbol.qualified_name.clone(),
            file_path: symbol.file_path.clone(),
            start_line,
            end_line,
            commits,
            uncommitted_lines,
        });
    }
    Attribution::Attributed(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blame::BlameLine;

    fn line(no: u32, commit: &str, author: &str, time: i64) -> BlameLine {
        BlameLine {
            line_no: no,
            commit: commit.to_string(),
            author: author.to_string(),
            author_mail: format!("{author}@example.com"),
            author_time: time,
            uncommitted: commit.chars().all(|c| c == '0'),
        }
    }

    const SOURCE: &str = "fn a() {\n    1\n}\n\nfn b() {\n    2\n}\n";

    fn symbols() -> Vec<GraphSymbol> {
        // `fn a` spans bytes 0..16 (lines 1-3); `fn b` starts after the blank.
        let a_start = 0;
        let a_end = SOURCE.find("}\n").unwrap() + 1;
        let b_start = SOURCE.find("fn b").unwrap();
        vec![
            GraphSymbol {
                qualified_name: "f.rs::a".into(),
                file_path: "f.rs".into(),
                span_start: a_start,
                span_end: a_end,
                body_exact: Some(1),
            },
            GraphSymbol {
                qualified_name: "f.rs::b".into(),
                file_path: "f.rs".into(),
                span_start: b_start,
                span_end: SOURCE.len(),
                body_exact: Some(2),
            },
        ]
    }

    fn blame_of(lines: Vec<BlameLine>, basis: BlobIdentity) -> FileBlame {
        FileBlame {
            path: "f.rs".into(),
            lines,
            basis,
        }
    }

    #[test]
    fn a_symbols_lines_are_attributed_to_the_commit_that_touched_them() {
        let basis = BlobIdentity::Blob("same".into());
        let blame = blame_of(
            vec![
                line(1, "c1", "ada", 10),
                line(2, "c1", "ada", 10),
                line(3, "c1", "ada", 10),
                line(4, "c1", "ada", 10),
                line(5, "c2", "grace", 20),
                line(6, "c2", "grace", 20),
                line(7, "c2", "grace", 20),
            ],
            basis.clone(),
        );
        let Attribution::Attributed(rows) = attribute(&symbols(), &basis, &blame, SOURCE) else {
            panic!("bases match, so this must attribute");
        };
        let a = rows.iter().find(|r| r.qualified_name == "f.rs::a").unwrap();
        assert_eq!(a.commits.len(), 1);
        assert_eq!(a.commits[0].commit, "c1");
        let b = rows.iter().find(|r| r.qualified_name == "f.rs::b").unwrap();
        assert_eq!(b.commits[0].commit, "c2");
        assert_eq!(
            b.commits[0].author, "grace",
            "the blame for `b`'s lines names `b`'s author, not the file's first"
        );
    }

    /// The defect this module exists to prevent. Same content length, same
    /// arithmetic, different revision — and without the check the answer looks
    /// perfectly reasonable.
    #[test]
    fn spans_from_another_revision_are_refused_rather_than_clamped() {
        let outcome = attribute(
            &symbols(),
            &BlobIdentity::Blob("revision-a".into()),
            &blame_of(
                vec![line(1, "c1", "ada", 10)],
                BlobIdentity::Blob("revision-b".into()),
            ),
            SOURCE,
        );
        match outcome {
            Attribution::BasisMismatch {
                spans_taken_against,
                lines_built_from,
                ..
            } => {
                assert!(spans_taken_against.contains("revision-a"));
                assert!(lines_built_from.contains("revision-b"));
            }
            Attribution::Attributed(rows) => panic!(
                "a span set from another revision must never be read against these lines; \
                 got {rows:?}"
            ),
        }
    }

    #[test]
    fn an_unknown_basis_is_refused() {
        let outcome = attribute(
            &symbols(),
            &BlobIdentity::Unknown,
            &blame_of(vec![line(1, "c1", "ada", 10)], BlobIdentity::Unknown),
            SOURCE,
        );
        assert!(matches!(outcome, Attribution::BasisMismatch { .. }));
    }

    #[test]
    fn uncommitted_lines_are_counted_not_credited_to_a_commit() {
        let basis = BlobIdentity::Blob("same".into());
        let zero = "0".repeat(40);
        let blame = blame_of(
            vec![
                line(1, &zero, "you", 0),
                line(2, &zero, "you", 0),
                line(3, "c1", "ada", 10),
            ],
            basis.clone(),
        );
        let Attribution::Attributed(rows) = attribute(&symbols(), &basis, &blame, SOURCE) else {
            panic!("bases match");
        };
        let a = rows.iter().find(|r| r.qualified_name == "f.rs::a").unwrap();
        assert_eq!(a.uncommitted_lines, 2);
        assert!(
            a.commits.iter().all(|c| c.commit != zero),
            "an all-zero id is not a commit and must not be offered as a suspect"
        );
    }

    /// Multi-byte content is where the naive implementation of this used to
    /// panic outright. The canonical `LineIndex` handles it; this asserts the
    /// join keeps using it rather than re-deriving the arithmetic.
    #[test]
    fn multibyte_content_does_not_shift_line_attribution() {
        let source = "fn a() {\n    // 🎉🎉🎉\n}\n";
        let basis = BlobIdentity::Blob("same".into());
        // Ends on the closing brace, which is what an extractor records — not
        // at EOF. A span ending past the final newline lands on the phantom
        // empty line after it, which is correct arithmetic and a different
        // fact from the one under test here.
        let symbols = vec![GraphSymbol {
            qualified_name: "f.rs::a".into(),
            file_path: "f.rs".into(),
            span_start: 0,
            span_end: source.rfind('}').unwrap() + 1,
            body_exact: None,
        }];
        let blame = blame_of(
            vec![
                line(1, "c1", "ada", 10),
                line(2, "c2", "grace", 20),
                line(3, "c1", "ada", 10),
            ],
            basis.clone(),
        );
        let Attribution::Attributed(rows) = attribute(&symbols, &basis, &blame, source) else {
            panic!("bases match");
        };
        assert_eq!(rows[0].start_line, 1);
        assert_eq!(
            rows[0].end_line, 3,
            "three four-byte characters on line 2 must not push the closing \
             brace onto a different line than it is on"
        );
        // The emoji line belongs to c2, and one line of it. If the byte
        // offsets were being counted as characters (or the reverse) this
        // attribution is where it would show.
        let c2 = rows[0]
            .commits
            .iter()
            .find(|c| c.commit == "c2")
            .expect("line 2 is attributed");
        assert_eq!(c2.lines, 1);
        let c1 = rows[0]
            .commits
            .iter()
            .find(|c| c.commit == "c1")
            .expect("lines 1 and 3 are attributed");
        assert_eq!(c1.lines, 2);
    }

    /// A span that really does end at EOF lands on the empty line after the
    /// final newline. Asserted rather than avoided, because a reader comparing
    /// this crate's line numbers against an editor's needs to know which
    /// convention is in force.
    #[test]
    fn a_span_ending_at_eof_lands_on_the_line_after_the_final_newline() {
        let basis = BlobIdentity::Blob("same".into());
        let symbols = vec![GraphSymbol {
            qualified_name: "f.rs::whole".into(),
            file_path: "f.rs".into(),
            span_start: 0,
            span_end: SOURCE.len(),
            body_exact: None,
        }];
        let Attribution::Attributed(rows) = attribute(
            &symbols,
            &basis,
            &blame_of(vec![line(1, "c1", "ada", 10)], basis.clone()),
            SOURCE,
        ) else {
            panic!("bases match");
        };
        assert_eq!(
            rows[0].end_line,
            SOURCE.matches('\n').count() as u32 + 1,
            "one past the newline count: the offset sits after the last \
             newline, which is the start of an empty final line"
        );
    }

    /// A span past the end of the content is the graph disagreeing with the
    /// file. It is clamped — but only because the basis check has already
    /// established they are the same bytes, so "past the end" can only mean a
    /// degenerate stored span rather than the wrong file.
    #[test]
    fn a_span_past_the_end_lands_on_the_last_line() {
        let basis = BlobIdentity::Blob("same".into());
        let symbols = vec![GraphSymbol {
            qualified_name: "f.rs::huge".into(),
            file_path: "f.rs".into(),
            span_start: SOURCE.len() + 1_000,
            span_end: SOURCE.len() + 2_000,
            body_exact: None,
        }];
        let Attribution::Attributed(rows) = attribute(
            &symbols,
            &basis,
            &blame_of(vec![line(1, "c1", "ada", 10)], basis.clone()),
            SOURCE,
        ) else {
            panic!("bases match");
        };
        assert!(rows[0].start_line >= 1);
        assert!(rows[0].end_line >= rows[0].start_line, "never inverted");
    }

    #[test]
    fn an_inverted_span_is_degenerate_not_inverted() {
        let basis = BlobIdentity::Blob("same".into());
        let symbols = vec![GraphSymbol {
            qualified_name: "f.rs::bad".into(),
            file_path: "f.rs".into(),
            span_start: 20,
            span_end: 2,
            body_exact: None,
        }];
        let Attribution::Attributed(rows) = attribute(
            &symbols,
            &basis,
            &blame_of(vec![line(1, "c1", "ada", 10)], basis.clone()),
            SOURCE,
        ) else {
            panic!("bases match");
        };
        assert_eq!(
            rows[0].start_line, rows[0].end_line,
            "an inverted stored span describes one line, not a backwards range"
        );
    }

    #[test]
    fn commit_order_is_total_and_stable() {
        let basis = BlobIdentity::Blob("same".into());
        // Two commits with equal line counts and equal times: only the id
        // breaks the tie, and it must break it the same way every run.
        let blame = blame_of(
            vec![
                line(1, "bbb", "x", 5),
                line(2, "aaa", "y", 5),
                line(3, "bbb", "x", 5),
                line(4, "aaa", "y", 5),
            ],
            basis.clone(),
        );
        let first = attribute(&symbols(), &basis, &blame, SOURCE);
        let second = attribute(&symbols(), &basis, &blame, SOURCE);
        assert_eq!(first, second, "the same input must produce the same report");
    }
}
