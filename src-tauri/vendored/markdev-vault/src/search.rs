//! Full-text search over the vault.
//!
//! A hand-rolled inverted index rather than a search-engine dependency. A
//! personal vault is thousands of notes, not millions of documents; at that
//! scale this is instant, and it keeps the crate free of a large dependency
//! whose features would go unused.

use std::collections::{HashMap, HashSet};

use super::index::SearchHit;
use super::note::Note;

/// Token to the notes containing it.
#[derive(Debug, Default)]
pub struct SearchIndex {
    postings: HashMap<String, HashSet<usize>>,
}

impl SearchIndex {
    pub fn build(notes: &[Note]) -> SearchIndex {
        let mut postings: HashMap<String, HashSet<usize>> = HashMap::new();
        for (index, note) in notes.iter().enumerate() {
            for token in tokenize(&note.text) {
                postings.entry(token).or_default().insert(index);
            }
            // Titles are indexed separately so a note is findable by name
            // even when the name never appears in its body.
            for token in tokenize(&note.title) {
                postings.entry(token).or_default().insert(index);
            }
        }
        SearchIndex { postings }
    }

    /// Notes matching every token in `query`, best first.
    ///
    /// Terms are ANDed: searching `rust parser` should mean notes about a
    /// Rust parser, not every note mentioning either word. The last token is
    /// matched as a prefix so results narrow while still being typed.
    pub fn query(&self, query: &str, notes: &[Note], limit: usize) -> Vec<SearchHit> {
        let tokens = tokenize(query);
        if tokens.is_empty() {
            return Vec::new();
        }

        let mut candidates: Option<HashSet<usize>> = None;
        for (position, token) in tokens.iter().enumerate() {
            let is_last = position == tokens.len() - 1;
            let matches = if is_last {
                self.prefix_matches(token)
            } else {
                self.postings.get(token).cloned().unwrap_or_default()
            };

            candidates = Some(match candidates {
                None => matches,
                Some(existing) => existing.intersection(&matches).copied().collect(),
            });

            if candidates.as_ref().is_some_and(HashSet::is_empty) {
                return Vec::new();
            }
        }

        let mut hits: Vec<SearchHit> = candidates
            .unwrap_or_default()
            .into_iter()
            .filter_map(|index| {
                let note = notes.get(index)?;
                let (line, context) = snippet(&note.text, &tokens)?;
                let mut score = 0u32;
                // A title match is a much stronger signal than a body match.
                // The body is lowered once here, not once per token: this ran
                // on the palette's keystroke path, and a long note paid a
                // full-case-fold per term for nothing.
                let title = note.title.to_lowercase();
                let body = note.text.to_lowercase();
                for token in &tokens {
                    if title.contains(token.as_str()) {
                        score += 40;
                    }
                    score += body.matches(token.as_str()).count().min(20) as u32;
                }
                Some(SearchHit {
                    path: note.path.clone(),
                    title: note.title.clone(),
                    context,
                    line,
                    score,
                })
            })
            .collect();

        hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
        hits.truncate(limit);
        hits
    }

    fn prefix_matches(&self, prefix: &str) -> HashSet<usize> {
        // Exact *and* prefix: returning only the exact postings — which is
        // what this used to do once the word grew past three letters — made
        // a note containing only "notes" invisible to the query "note". The
        // vocabulary scan is cheap at personal-vault scale; a miss that
        // should have been a hit is not.
        let mut matches = HashSet::new();
        for (token, notes) in &self.postings {
            if token == prefix || token.starts_with(prefix) {
                matches.extend(notes.iter().copied());
            }
        }
        matches
    }
}

/// Splits text into lowercase tokens, with CJK runs as overlapping bigrams.
///
/// Chinese, Japanese, and Korean carry no spaces, so the old splitter saw one
/// whole paragraph as a single token and a query for any word inside it
/// matched nothing — the index answered only queries that quoted the entire
/// paragraph back. Bigrams are the standard fix: every adjacent pair becomes
/// a token, so a two-character query hits its own pair exactly. Latin and
/// CJK runs are flushed separately, so `中文note` indexes both spellings.
fn tokenize(text: &str) -> Vec<String> {
    #[derive(Clone, Copy, PartialEq)]
    enum Script {
        Word,
        Cjk,
    }

    fn flush(run: &mut String, script: Script, out: &mut Vec<String>) {
        if !run.is_empty() {
            match script {
                Script::Word => out.push(run.to_lowercase()),
                // One character alone still becomes a token, so single-kanji
                // queries have something exact to land on.
                Script::Cjk if run.chars().count() == 1 => out.push(run.to_lowercase()),
                Script::Cjk => {
                    let chars: Vec<char> = run.chars().collect();
                    for pair in chars.windows(2) {
                        let bigram: String = pair.iter().collect();
                        out.push(bigram.to_lowercase());
                    }
                }
            }
        }
        run.clear();
    }

    let mut tokens = Vec::new();
    let mut run = String::new();
    let mut script: Option<Script> = None;

    for ch in text.chars() {
        let next = if is_cjk(ch) {
            Some(Script::Cjk)
        } else if ch.is_alphanumeric() || ch == '_' || ch == '-' {
            Some(Script::Word)
        } else {
            None
        };

        match (script, next) {
            (_, None) => {
                if let Some(seen) = script {
                    flush(&mut run, seen, &mut tokens);
                    script = None;
                }
            }
            (Some(seen), Some(incoming)) if seen != incoming => {
                flush(&mut run, seen, &mut tokens);
                run.push(ch);
                script = Some(incoming);
            }
            (_, Some(incoming)) => {
                run.push(ch);
                script = Some(incoming);
            }
        }
    }
    if let Some(last) = script {
        flush(&mut run, last, &mut tokens);
    }
    tokens
}

/// Whether `ch` belongs to a script written without spaces between words.
fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x1100..=0x11FF      // Hangul Jamo
            | 0x2E80..=0x9FFF    // Han, kana, CJK punctuation blocks
            | 0xAC00..=0xD7AF    // Hangul syllables
            | 0xF900..=0xFAFF    // CJK compatibility ideographs
            | 0xFF66..=0xFF9D    // Halfwidth katakana
    )
}

/// The line that mentions the most query terms, with its 1-based number.
///
/// The first line mentioning *anything* used to win, which put a passing
/// mention of one term where the reader wanted the paragraph that tied the
/// terms together. Distinct terms matched decides; total occurrences breaks
/// ties within a line; earliest line breaks the rest — all deterministic.
fn snippet(text: &str, tokens: &[String]) -> Option<(u32, String)> {
    let mut best: Option<(usize, usize, usize)> = None; // (distinct, occurrences, line index)
    let mut best_line = "";

    for (index, line) in text.lines().enumerate() {
        let lower = line.to_lowercase();
        let mut distinct = 0;
        let mut occurrences = 0;
        for token in tokens {
            let hits = lower.matches(token.as_str()).count();
            if hits > 0 {
                distinct += 1;
                occurrences += hits;
            }
        }
        if distinct == 0 {
            continue;
        }
        let better = match best {
            None => true,
            Some((bd, bo, bi)) => {
                (distinct, occurrences) > (bd, bo)
                    || ((distinct, occurrences) == (bd, bo) && index < bi)
            }
        };
        if better {
            best = Some((distinct, occurrences, index));
            best_line = line;
        }
    }

    // No line matched any term: a title-only hit still deserves a result,
    // just without a body line.
    let chosen = if best_line.is_empty() {
        text.lines().next().unwrap_or("")
    } else {
        best_line
    };
    let trimmed = chosen.trim();
    let context = if trimmed.chars().count() > 160 {
        trimmed.chars().take(160).collect::<String>() + "…"
    } else {
        trimmed.to_string()
    };
    let line_number = best
        .map(|(_, _, index)| index)
        .map(|i| i as u32 + 1)
        .unwrap_or(1);
    Some((line_number, context))
}
