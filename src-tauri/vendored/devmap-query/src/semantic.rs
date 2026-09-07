//! TF-IDF ranking over symbol names, computed at query time.
//!
//! Nothing is stored. The vocabulary, the document frequencies and the vectors
//! are all derivable from `generation_nodes`, which the store already holds, so
//! precomputing them would put a second copy of a derived fact in the database —
//! one that has to be rebuilt in step with the symbols, and that is stale
//! whenever it was not. The Python implementation this replaces carried exactly
//! that machinery: an `symbol_embedding_idf` table, a build step, a generation
//! stamp, and a `stale_rows_skipped` counter for when they disagreed.
//!
//! The cost of computing instead of storing is one pass over the symbol names
//! per query. On this repository that is ~14,500 names, and it is not close to
//! being the expensive part of a query.
//!
//! This ranks *names*, not source text. It finds `LLMCache` for "llm cache" and
//! `compute_freshness` for "freshness computation"; it will not find a function
//! whose body is about caching but whose name says nothing about it. That limit
//! is a property of the corpus being indexed, and it is stated rather than
//! implied by calling the feature "semantic".

use std::collections::HashMap;

use crate::cancel::{Cancel, QueryCancelled};

/// Split an identifier into lowercase terms on camelCase and non-alphanumerics.
///
/// `parseHTTPResponse` yields `parse`, `http`, `response`: the run of capitals
/// is one term, not three, because `HTTP` is a word and `H`, `T`, `T`, `P` are
/// not. Digits attach to the term they sit in, so `utf8` stays whole.
pub fn tokenize(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut terms: Vec<String> = Vec::new();
    let mut current = String::new();

    for index in 0..chars.len() {
        let ch = chars[index];
        if !ch.is_alphanumeric() {
            if !current.is_empty() {
                terms.push(std::mem::take(&mut current));
            }
            continue;
        }
        let previous_was_upper = index > 0 && chars[index - 1].is_uppercase();
        let next_is_lower = chars.get(index + 1).is_some_and(|next| next.is_lowercase());

        // Two boundaries, and the second is the one that matters. A
        // lower-to-upper transition ends a word (`computeFreshness`). Inside a
        // run of capitals, the boundary is *before the last one* when a
        // lowercase follows: `LLMCache` is `llm` + `cache`, and
        // `parseHTTPResponse` is `parse` + `http` + `response`. Without that
        // second rule an acronym swallows the word after it, which is how
        // `LLMCache` became the single term `llmcache` and stopped matching a
        // query for "llm cache".
        let starts_new_word =
            ch.is_uppercase() && !current.is_empty() && (!previous_was_upper || next_is_lower);
        if starts_new_word {
            terms.push(std::mem::take(&mut current));
        }
        current.extend(ch.to_lowercase());
    }
    if !current.is_empty() {
        terms.push(current);
    }
    // Single characters are dropped: `a`, `x`, `i` appear everywhere and
    // separate nothing.
    terms.retain(|term| term.chars().count() > 1);
    terms
}

/// Sub-linear term frequency: `1 + ln(count)`.
///
/// A name that repeats a term twice is not twice as much about it, and raw
/// counts let a long qualified name dominate on repetition alone.
fn term_frequencies(terms: &[String]) -> HashMap<&str, f32> {
    let mut counts: HashMap<&str, u32> = HashMap::new();
    for term in terms {
        *counts.entry(term.as_str()).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(term, count)| (term, 1.0 + (count as f32).ln()))
        .collect()
}

/// A scored corpus of symbol names.
pub struct SemanticIndex {
    /// Per document, its unit-normalised sparse weight vector.
    documents: Vec<HashMap<String, f32>>,
    /// Smoothed inverse document frequency per term.
    idf: HashMap<String, f32>,
}

impl SemanticIndex {
    /// Build over one text per symbol. Two passes: document frequency, then
    /// weights, because IDF is a property of the corpus and cannot be known
    /// while the first document is still being read.
    ///
    /// `cancel` is consulted in every pass. This is the longest loop in the
    /// query engine — it tokenizes and vectorises the *whole* corpus, tens of
    /// thousands of names on a real repository — so a caller that has already
    /// been answered with a timeout must be able to stop it here rather than
    /// pay for it to finish on a blocking-pool thread nobody is reading.
    pub fn build(texts: &[String], cancel: &Cancel) -> Result<Self, QueryCancelled> {
        let mut tokenized: Vec<Vec<String>> = Vec::with_capacity(texts.len());
        for (index, text) in texts.iter().enumerate() {
            cancel.check_every(index)?;
            tokenized.push(tokenize(text));
        }

        let mut document_frequency: HashMap<&str, u32> = HashMap::new();
        for (index, terms) in tokenized.iter().enumerate() {
            cancel.check_every(index)?;
            let mut seen: Vec<&str> = terms.iter().map(String::as_str).collect();
            seen.sort_unstable();
            seen.dedup();
            for term in seen {
                *document_frequency.entry(term).or_default() += 1;
            }
        }

        let total = tokenized.len() as f32;
        // Smoothed, so a term appearing in every document gets a small positive
        // weight rather than exactly zero. An unsmoothed IDF makes a query made
        // entirely of common terms score every document at zero, and a ranking
        // where everything ties is indistinguishable from no ranking at all.
        let idf: HashMap<String, f32> = document_frequency
            .iter()
            .map(|(term, count)| {
                let value = ((total + 1.0) / (*count as f32 + 1.0)).ln() + 1.0;
                ((*term).to_string(), value)
            })
            .collect();

        let mut documents = Vec::with_capacity(tokenized.len());
        for (index, terms) in tokenized.iter().enumerate() {
            cancel.check_every(index)?;
            documents.push(Self::unit_vector(terms, &idf));
        }

        Ok(Self { documents, idf })
    }

    fn unit_vector(terms: &[String], idf: &HashMap<String, f32>) -> HashMap<String, f32> {
        let mut vector: HashMap<String, f32> = term_frequencies(terms)
            .into_iter()
            .map(|(term, tf)| {
                let weight = tf * idf.get(term).copied().unwrap_or(1.0);
                (term.to_string(), weight)
            })
            .collect();
        let norm: f32 = vector.values().map(|w| w * w).sum::<f32>().sqrt();
        if norm > 0.0 {
            for weight in vector.values_mut() {
                *weight /= norm;
            }
        }
        vector
    }

    /// Cosine similarity of `query` against every document, as
    /// `(document index, score)` for documents that score above zero.
    ///
    /// Zero-scoring documents are omitted rather than returned with a score of
    /// zero: a symbol sharing no term with the query is not a weak match, it is
    /// not a match, and padding a ranked list with them turns "nothing matched"
    /// into a page of results.
    pub fn score(&self, query: &str, cancel: &Cancel) -> Result<Vec<(usize, f32)>, QueryCancelled> {
        let terms = tokenize(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let query_vector = Self::unit_vector(&terms, &self.idf);
        let mut scored: Vec<(usize, f32)> = Vec::new();
        for (index, document) in self.documents.iter().enumerate() {
            cancel.check_every(index)?;
            // Iterate the smaller side; a query has a handful of terms and a
            // document rarely more.
            let score: f32 = query_vector
                .iter()
                .filter_map(|(term, weight)| document.get(term).map(|w| w * weight))
                .sum();
            if score > 0.0 {
                scored.push((index, score));
            }
        }
        // Descending score, then ascending index, so equal scores keep corpus
        // order and the ranking is identical across runs.
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        Ok(scored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_split_on_case_and_separators() {
        assert_eq!(tokenize("computeFreshness"), vec!["compute", "freshness"]);
        assert_eq!(tokenize("compute_freshness"), vec!["compute", "freshness"]);
        assert_eq!(tokenize("ComputeFreshness"), vec!["compute", "freshness"]);
        assert_eq!(
            tokenize("src/devcouncil/llm/cache.py::LLMCache"),
            vec!["src", "devcouncil", "llm", "cache", "py", "llm", "cache"]
        );
    }

    /// A run of capitals is one word. `parseHTTPResponse` is three terms, not
    /// six — splitting every capital turns acronyms into single letters, which
    /// the length filter then discards entirely.
    #[test]
    fn acronyms_survive_tokenization() {
        assert_eq!(
            tokenize("parseHTTPResponse"),
            vec!["parse", "http", "response"]
        );
        assert_eq!(tokenize("utf8Decoder"), vec!["utf8", "decoder"]);
    }

    #[test]
    fn single_characters_are_dropped() {
        // `a`, `x`, `i` carry no signal and appear everywhere.
        assert_eq!(tokenize("a_b_compute"), vec!["compute"]);
    }

    /// The property that makes this worth having: a match on terms the name
    /// contains in a different form or order than the query.
    #[test]
    fn ranks_a_name_the_query_does_not_literally_contain() {
        let corpus: Vec<String> = vec![
            "computeFreshness compute freshness".into(),
            "unrelatedHelper parse tokens".into(),
            "writeGraph serialize output".into(),
        ];
        let index = SemanticIndex::build(&corpus, &Cancel::new()).unwrap();
        let scored = index
            .score("freshness computation", &Cancel::new())
            .unwrap();
        assert!(!scored.is_empty(), "the query matched nothing");
        assert_eq!(
            scored[0].0, 0,
            "the freshness symbol did not rank first: {scored:?}"
        );
    }

    /// "Nothing matched" must be an answer, not a page of zeroes.
    #[test]
    fn a_query_sharing_no_term_scores_nothing() {
        let corpus: Vec<String> = vec!["alpha beta".into(), "gamma delta".into()];
        let index = SemanticIndex::build(&corpus, &Cancel::new()).unwrap();
        assert!(index
            .score("zebra quokka", &Cancel::new())
            .unwrap()
            .is_empty());
        assert!(index.score("", &Cancel::new()).unwrap().is_empty());
    }

    /// A term in every document still contributes. Unsmoothed IDF would make
    /// this query score every document at exactly zero, and a ranking where
    /// everything ties cannot be told from no ranking at all.
    #[test]
    fn a_term_common_to_every_document_still_ranks() {
        let corpus: Vec<String> = vec![
            "cache read".into(),
            "cache write".into(),
            "cache evict".into(),
        ];
        let index = SemanticIndex::build(&corpus, &Cancel::new()).unwrap();
        let scored = index.score("cache", &Cancel::new()).unwrap();
        assert_eq!(
            scored.len(),
            3,
            "a universal term scored nothing: {scored:?}"
        );
        assert!(scored.iter().all(|(_, score)| *score > 0.0));
    }

    #[test]
    fn ranking_is_stable_across_runs() {
        let corpus: Vec<String> = (0..50)
            .map(|i| format!("symbol{i} cache handler"))
            .collect();
        let index = SemanticIndex::build(&corpus, &Cancel::new()).unwrap();
        let first = index.score("cache handler", &Cancel::new()).unwrap();
        let second = index.score("cache handler", &Cancel::new()).unwrap();
        assert_eq!(first, second);
        // Ties break on corpus order, so the sequence is total.
        let positions: Vec<usize> = first.iter().map(|(i, _)| *i).collect();
        let mut sorted = positions.clone();
        sorted.sort_unstable();
        assert_eq!(positions, sorted, "equal scores did not keep corpus order");
    }

    #[test]
    fn scores_are_bounded_by_cosine() {
        let corpus: Vec<String> = vec!["exact match here".into(), "something else".into()];
        let index = SemanticIndex::build(&corpus, &Cancel::new()).unwrap();
        for (_, score) in index.score("exact match here", &Cancel::new()).unwrap() {
            assert!(
                (0.0..=1.0001).contains(&score),
                "cosine similarity out of range: {score}"
            );
        }
    }
}
