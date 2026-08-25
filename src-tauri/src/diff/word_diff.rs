use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffChunkKind {
    Equal,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffSegment {
    pub kind: DiffChunkKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntraLineDiff {
    pub original_segments: Vec<DiffSegment>,
    pub modified_segments: Vec<DiffSegment>,
}

/// Tokenizes text into word-like chunks and delimiters for intra-line diffing.
/// Safely bounds maximum token count to prevent quadratic memory explosion on minified files.
fn tokenize_line(line: &str, max_tokens: usize) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut chars = line.char_indices().peekable();

    while let Some((start, ch)) = chars.next() {
        if tokens.len() >= max_tokens {
            // Remainder becomes a single token
            tokens.push(&line[start..]);
            break;
        }

        let is_alphanumeric = ch.is_alphanumeric() || ch == '_';
        let mut end = start + ch.len_utf8();

        while let Some(&(_, next_ch)) = chars.peek() {
            let next_is_alnum = next_ch.is_alphanumeric() || next_ch == '_';
            if next_is_alnum == is_alphanumeric {
                chars.next();
                end += next_ch.len_utf8();
            } else {
                break;
            }
        }
        tokens.push(&line[start..end]);
    }

    tokens
}

/// Computes intra-line word-level diffs with safeguards against massive minified files.
pub fn compute_word_diff(old_line: &str, new_line: &str) -> IntraLineDiff {
    // Fast path: Identical lines
    if old_line == new_line {
        return IntraLineDiff {
            original_segments: vec![DiffSegment {
                kind: DiffChunkKind::Equal,
                text: old_line.to_string(),
            }],
            modified_segments: vec![DiffSegment {
                kind: DiffChunkKind::Equal,
                text: new_line.to_string(),
            }],
        };
    }

    // Safety guard for massive minified lines (> 50,000 characters)
    if old_line.len() > 50_000 || new_line.len() > 50_000 {
        return IntraLineDiff {
            original_segments: vec![DiffSegment {
                kind: DiffChunkKind::Removed,
                text: old_line.to_string(),
            }],
            modified_segments: vec![DiffSegment {
                kind: DiffChunkKind::Added,
                text: new_line.to_string(),
            }],
        };
    }

    const MAX_TOKENS: usize = 500;
    let old_tokens = tokenize_line(old_line, MAX_TOKENS);
    let new_tokens = tokenize_line(new_line, MAX_TOKENS);

    let lcs_matrix = compute_lcs_table(&old_tokens, &new_tokens);

    let mut i = old_tokens.len();
    let mut j = new_tokens.len();

    let mut raw_orig_reversed = Vec::new();
    let mut raw_mod_reversed = Vec::new();

    while i > 0 || j > 0 {
        if i > 0 && j > 0 && old_tokens[i - 1] == new_tokens[j - 1] {
            raw_orig_reversed.push(DiffSegment {
                kind: DiffChunkKind::Equal,
                text: old_tokens[i - 1].to_string(),
            });
            raw_mod_reversed.push(DiffSegment {
                kind: DiffChunkKind::Equal,
                text: new_tokens[j - 1].to_string(),
            });
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || lcs_matrix[i][j - 1] >= lcs_matrix[i - 1][j]) {
            raw_mod_reversed.push(DiffSegment {
                kind: DiffChunkKind::Added,
                text: new_tokens[j - 1].to_string(),
            });
            j -= 1;
        } else if i > 0 && (j == 0 || lcs_matrix[i][j - 1] < lcs_matrix[i - 1][j]) {
            raw_orig_reversed.push(DiffSegment {
                kind: DiffChunkKind::Removed,
                text: old_tokens[i - 1].to_string(),
            });
            i -= 1;
        }
    }

    raw_orig_reversed.reverse();
    raw_mod_reversed.reverse();

    let orig_segments = merge_consecutive_segments(raw_orig_reversed);
    let mod_segments = merge_consecutive_segments(raw_mod_reversed);

    IntraLineDiff {
        original_segments: orig_segments,
        modified_segments: mod_segments,
    }
}

fn compute_lcs_table(a: &[&str], b: &[&str]) -> Vec<Vec<usize>> {
    let m = a.len();
    let n = b.len();
    let mut table = vec![vec![0; n + 1]; m + 1];

    for i in 1..=m {
        for j in 1..=n {
            if a[i - 1] == b[j - 1] {
                table[i][j] = table[i - 1][j - 1] + 1;
            } else {
                table[i][j] = table[i - 1][j].max(table[i][j - 1]);
            }
        }
    }

    table
}

fn merge_consecutive_segments(segments: Vec<DiffSegment>) -> Vec<DiffSegment> {
    let mut merged = Vec::new();
    for seg in segments {
        if let Some(last) = merged.last_mut() {
            let last_mut: &mut DiffSegment = last;
            if last_mut.kind == seg.kind {
                last_mut.text.push_str(&seg.text);
                continue;
            }
        }
        merged.push(seg);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_word_replacement() {
        let old_l = "let count = 42;";
        let new_l = "let count = 100;";

        let diff = compute_word_diff(old_l, new_l);

        assert_eq!(diff.original_segments.len(), 3);
        assert_eq!(diff.original_segments[0].kind, DiffChunkKind::Equal);
        assert_eq!(diff.original_segments[0].text, "let count = ");
        assert_eq!(diff.original_segments[1].kind, DiffChunkKind::Removed);
        assert_eq!(diff.original_segments[1].text, "42");
        assert_eq!(diff.original_segments[2].kind, DiffChunkKind::Equal);
        assert_eq!(diff.original_segments[2].text, ";");

        assert_eq!(diff.modified_segments.len(), 3);
        assert_eq!(diff.modified_segments[0].kind, DiffChunkKind::Equal);
        assert_eq!(diff.modified_segments[0].text, "let count = ");
        assert_eq!(diff.modified_segments[1].kind, DiffChunkKind::Added);
        assert_eq!(diff.modified_segments[1].text, "100");
        assert_eq!(diff.modified_segments[2].kind, DiffChunkKind::Equal);
        assert_eq!(diff.modified_segments[2].text, ";");
    }

    #[test]
    fn test_huge_line_safeguard() {
        let huge_old = "a".repeat(60_000);
        let huge_new = "b".repeat(60_000);
        let diff = compute_word_diff(&huge_old, &huge_new);

        assert_eq!(diff.original_segments.len(), 1);
        assert_eq!(diff.original_segments[0].kind, DiffChunkKind::Removed);
        assert_eq!(diff.modified_segments.len(), 1);
        assert_eq!(diff.modified_segments[0].kind, DiffChunkKind::Added);
    }
}
