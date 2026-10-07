//! Narrowing a ranking to part of the repository.
//!
//! `ask`, `ask_evidence` and semantic `search` rank the whole indexed corpus.
//! On a polyglot repository that answers a question about one subtree from
//! all of them: auditing a React `frontend/`, a question no React name
//! answers was filled by Go backend names. A [`SymbolScope`] restricts the
//! corpus *before* it is ranked, so IDF is computed over the scoped corpus
//! and the call graph the ranking walks is the scoped subgraph — a filter
//! applied to the answer afterwards would rank by term weights the scope
//! never had.
//!
//! A scope that names nothing is refused, never answered. An empty answer
//! from a prefix with a typo in it reads exactly like "nothing here answers
//! the question", which is the reading that sends an agent to conclude the
//! code does not exist.

use std::collections::{BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

/// Most path prefixes one scope accepts. Each is compared against every
/// indexed file, so the bound is on that product.
pub const MAX_SCOPE_PATHS: usize = 32;
/// Most languages one scope accepts. More than the extractor knows.
pub const MAX_SCOPE_LANGUAGES: usize = 16;
/// Most entries a refusal lists as alternatives, so a repository with a
/// thousand top-level directories cannot turn an error into a page.
const MAX_LISTED_ALTERNATIVES: usize = 20;

/// Path prefixes and languages a ranking is restricted to, as the caller gave
/// them after normalisation.
///
/// Within a field the entries are alternatives (any prefix, any language);
/// across fields they are both required. A path prefix matches at a segment
/// boundary: `frontend` admits `frontend/app.tsx` and a file named exactly
/// `frontend`, never `frontend2/x.ts`. Languages are compared ignoring case
/// against the label the extractor stored (`typescript` and `tsx` are
/// distinct labels).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolScope {
    paths: Vec<String>,
    languages: Vec<String>,
}

impl SymbolScope {
    /// Build a scope from caller input, or `None` when both lists are empty —
    /// no scope, the whole repository.
    ///
    /// Refuses a prefix that cannot name a repository path (`..`, empty after
    /// normalisation) and lists over their bounds. An absolute path is kept as
    /// given and made relative against the indexed repository root in
    /// [`Self::resolve`], which is the first place the root is known.
    pub fn new(paths: &[String], languages: &[String]) -> anyhow::Result<Option<Self>> {
        if paths.is_empty() && languages.is_empty() {
            return Ok(None);
        }
        if paths.len() > MAX_SCOPE_PATHS {
            anyhow::bail!(
                "paths accepts at most {MAX_SCOPE_PATHS} prefixes, got {}",
                paths.len()
            );
        }
        if languages.len() > MAX_SCOPE_LANGUAGES {
            anyhow::bail!(
                "languages accepts at most {MAX_SCOPE_LANGUAGES} entries, got {}",
                languages.len()
            );
        }
        let mut normalised = Vec::with_capacity(paths.len());
        for raw in paths {
            let path = normalise_prefix(raw)?;
            if !normalised.contains(&path) {
                normalised.push(path);
            }
        }
        let mut langs = Vec::with_capacity(languages.len());
        for raw in languages {
            let language = raw.trim().to_ascii_lowercase();
            if language.is_empty() {
                anyhow::bail!(
                    "languages entry {raw:?} is empty; omit `languages` to admit every language"
                );
            }
            if !langs.contains(&language) {
                langs.push(language);
            }
        }
        Ok(Some(Self {
            paths: normalised,
            languages: langs,
        }))
    }

    /// Check this scope against the files one generation indexed, and return
    /// the set of in-scope paths.
    ///
    /// `files` is `(path, language)` for every indexed file, symbols or not,
    /// from the generation being ranked. Refused when a path prefix matches no
    /// indexed file, when a language labels no indexed file, or when every
    /// prefix and every language match something but no file is both. Each
    /// refusal says what *is* there, so the caller can correct it.
    pub fn resolve(
        &self,
        files: &[(String, String)],
        repo_root: Option<&str>,
    ) -> anyhow::Result<ResolvedScope> {
        let paths = self
            .paths
            .iter()
            .map(|prefix| relative_to_root(prefix, repo_root))
            .collect::<anyhow::Result<Vec<String>>>()?;
        // Again after relativising: `frontend` and `/repo/frontend` are one prefix.
        let paths: Vec<String> = paths.into_iter().fold(Vec::new(), |mut kept, path| {
            if !kept.contains(&path) {
                kept.push(path);
            }
            kept
        });
        for prefix in &paths {
            if !files.iter().any(|(path, _)| under_prefix(path, prefix)) {
                anyhow::bail!(
                    "paths entry {prefix:?} matches no indexed file ({} file(s) indexed); \
                     prefixes are repository-relative and match at a path segment. \
                     Top-level entries: {}",
                    files.len(),
                    listed(top_level_entries(files))
                );
            }
        }
        for language in &self.languages {
            if !files
                .iter()
                .any(|(_, label)| label.eq_ignore_ascii_case(language))
            {
                anyhow::bail!(
                    "languages entry {language:?} labels no indexed file. Indexed languages: {}",
                    listed(
                        files
                            .iter()
                            .map(|(_, label)| label.to_ascii_lowercase())
                            .collect()
                    )
                );
            }
        }
        let admitted: HashSet<String> = files
            .iter()
            .filter(|(path, language)| admits(&paths, &self.languages, path, language))
            .map(|(path, _)| path.clone())
            .collect();
        if admitted.is_empty() {
            let in_prefixes: BTreeSet<String> = files
                .iter()
                .filter(|(path, _)| paths.iter().any(|prefix| under_prefix(path, prefix)))
                .map(|(_, label)| label.to_ascii_lowercase())
                .collect();
            anyhow::bail!(
                "no indexed file is both under paths {paths:?} and in languages {:?}; \
                 the languages under those paths are: {}",
                self.languages,
                listed(in_prefixes)
            );
        }
        Ok(ResolvedScope {
            report: ScopeReport {
                paths,
                languages: self.languages.clone(),
                files: u32::try_from(admitted.len()).unwrap_or(u32::MAX),
                symbols: 0,
                corpus_files: u32::try_from(files.len()).unwrap_or(u32::MAX),
                corpus_symbols: 0,
                related_tests_outside_scope: 0,
            },
            files: admitted,
        })
    }
}

/// A [`SymbolScope`] checked against one generation: the in-scope paths, and
/// the report the answer carries.
#[derive(Debug, Clone)]
pub struct ResolvedScope {
    files: HashSet<String>,
    pub report: ScopeReport,
}

impl ResolvedScope {
    /// Whether `path` is one of the generation's in-scope files.
    pub fn contains(&self, path: &str) -> bool {
        self.files.contains(path)
    }
}

/// What a scoped answer was ranked over, beside the whole corpus it was cut
/// from.
///
/// Carried on the answer so a short list is attributable: three hits out of
/// a 40-symbol scope is a different finding from three out of 15,000. Absent
/// from an unscoped answer, so nothing changes for callers that never scope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScopeReport {
    /// The path prefixes applied, repository-relative after normalisation.
    /// Empty when only languages were given.
    pub paths: Vec<String>,
    /// The languages applied, lowercased. Empty when only paths were given.
    pub languages: Vec<String>,
    /// Indexed files in scope, including files that declare no symbol.
    pub files: u32,
    /// Symbols in scope — the corpus the ranking ran over.
    pub symbols: u32,
    /// Files the generation indexed in total.
    pub corpus_files: u32,
    /// Symbols the generation indexed in total.
    pub corpus_symbols: u32,
    /// Test files the evidence pack's related-test walk reached but left out
    /// because they sit outside the scope. Zero, and omitted, everywhere else.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub related_tests_outside_scope: u32,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

fn admits(paths: &[String], languages: &[String], path: &str, language: &str) -> bool {
    (paths.is_empty() || paths.iter().any(|prefix| under_prefix(path, prefix)))
        && (languages.is_empty()
            || languages
                .iter()
                .any(|wanted| wanted.eq_ignore_ascii_case(language)))
}

/// `path` is `prefix` or lies beneath it — a segment-boundary match, so
/// `frontend` does not admit `frontend2/x.ts`.
fn under_prefix(path: &str, prefix: &str) -> bool {
    path == prefix
        || (path.len() > prefix.len()
            && path.starts_with(prefix)
            && path.as_bytes()[prefix.len()] == b'/')
}

/// Strip `./` and trailing separators, refuse what cannot name a repository
/// path. An absolute path is kept absolute for [`relative_to_root`].
fn normalise_prefix(raw: &str) -> anyhow::Result<String> {
    let mut path = raw.trim().replace('\\', "/");
    while let Some(rest) = path.strip_prefix("./") {
        path = rest.to_string();
    }
    let absolute = path.starts_with('/');
    let trimmed = path.trim_end_matches('/');
    let path = if trimmed.is_empty() && absolute {
        "/".to_string()
    } else {
        trimmed.to_string()
    };
    if path.is_empty() || path == "." {
        anyhow::bail!(
            "paths entry {raw:?} names the whole repository; omit `paths` instead of passing it"
        );
    }
    if path.split('/').any(|segment| segment == "..") {
        anyhow::bail!("paths entry {raw:?} contains `..`; prefixes are repository-relative");
    }
    Ok(path)
}

/// An absolute prefix made relative to the indexed root; a relative one as is.
fn relative_to_root(prefix: &str, repo_root: Option<&str>) -> anyhow::Result<String> {
    if !prefix.starts_with('/') {
        return Ok(prefix.to_string());
    }
    let Some(root) = repo_root.map(|root| root.trim_end_matches('/')) else {
        anyhow::bail!(
            "paths entry {prefix:?} is absolute and this index recorded no repository root to \
             make it relative to; pass it repository-relative"
        );
    };
    if prefix == root {
        anyhow::bail!(
            "paths entry {prefix:?} is the repository root itself; omit `paths` instead of passing it"
        );
    }
    match prefix
        .strip_prefix(root)
        .and_then(|rest| rest.strip_prefix('/'))
    {
        Some(relative) if !relative.is_empty() => Ok(relative.to_string()),
        _ => anyhow::bail!(
            "paths entry {prefix:?} is outside the indexed repository {root:?}; pass a prefix \
             inside it, repository-relative"
        ),
    }
}

fn top_level_entries(files: &[(String, String)]) -> BTreeSet<String> {
    files
        .iter()
        .map(|(path, _)| match path.split_once('/') {
            Some((head, _)) => format!("{head}/"),
            None => path.clone(),
        })
        .collect()
}

fn listed(entries: BTreeSet<String>) -> String {
    if entries.is_empty() {
        return "(none)".to_string();
    }
    let total = entries.len();
    let mut shown: Vec<String> = entries.into_iter().take(MAX_LISTED_ALTERNATIVES).collect();
    if total > shown.len() {
        shown.push(format!("… and {} more", total - MAX_LISTED_ALTERNATIVES));
    }
    shown.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> Vec<(String, String)> {
        vec![
            ("backend/server.go".to_string(), "go".to_string()),
            ("frontend/app.tsx".to_string(), "tsx".to_string()),
            ("frontend/util.ts".to_string(), "typescript".to_string()),
            ("frontend2/other.ts".to_string(), "typescript".to_string()),
            ("README.md".to_string(), "markdown".to_string()),
        ]
    }

    fn scope(paths: &[&str], languages: &[&str]) -> SymbolScope {
        let paths: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
        let languages: Vec<String> = languages.iter().map(|l| l.to_string()).collect();
        SymbolScope::new(&paths, &languages).unwrap().unwrap()
    }

    #[test]
    fn no_paths_and_no_languages_is_no_scope() {
        assert_eq!(SymbolScope::new(&[], &[]).unwrap(), None);
    }

    #[test]
    fn a_prefix_matches_at_a_segment_boundary() {
        let resolved = scope(&["frontend/"], &[]).resolve(&files(), None).unwrap();
        assert!(resolved.contains("frontend/app.tsx"));
        assert!(resolved.contains("frontend/util.ts"));
        assert!(!resolved.contains("frontend2/other.ts"));
        assert_eq!(resolved.report.files, 2);
        assert_eq!(resolved.report.corpus_files, 5);
        assert_eq!(resolved.report.paths, vec!["frontend".to_string()]);
    }

    #[test]
    fn a_prefix_can_name_one_file() {
        let resolved = scope(&["./README.md"], &[])
            .resolve(&files(), None)
            .unwrap();
        assert_eq!(resolved.report.files, 1);
    }

    #[test]
    fn an_absolute_prefix_is_made_relative_to_the_indexed_root() {
        let resolved = scope(&["/repo/frontend"], &[])
            .resolve(&files(), Some("/repo/"))
            .unwrap();
        assert_eq!(resolved.report.paths, vec!["frontend".to_string()]);
        let both = scope(&["frontend", "/repo/frontend"], &[])
            .resolve(&files(), Some("/repo"))
            .unwrap();
        assert_eq!(both.report.paths, vec!["frontend".to_string()]);
        let outside = scope(&["/elsewhere/frontend"], &[])
            .resolve(&files(), Some("/repo"))
            .unwrap_err()
            .to_string();
        assert!(
            outside.contains("outside the indexed repository"),
            "{outside}"
        );
        let unrooted = scope(&["/repo/frontend"], &[])
            .resolve(&files(), None)
            .unwrap_err()
            .to_string();
        assert!(unrooted.contains("no repository root"), "{unrooted}");
    }

    #[test]
    fn a_prefix_matching_no_file_is_refused_and_names_what_is_there() {
        let err = scope(&["frontnd"], &[])
            .resolve(&files(), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("\"frontnd\" matches no indexed file"), "{err}");
        assert!(err.contains("frontend/"), "{err}");
        assert!(err.contains("backend/"), "{err}");
    }

    #[test]
    fn one_bad_prefix_among_good_ones_is_still_refused() {
        // Admitting the union would answer from `frontend/` and say nothing
        // about the typo beside it.
        let err = scope(&["frontend", "bakend"], &[])
            .resolve(&files(), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("\"bakend\""), "{err}");
    }

    #[test]
    fn a_language_is_matched_ignoring_case_and_refused_when_absent() {
        let resolved = scope(&[], &["TSX"]).resolve(&files(), None).unwrap();
        assert_eq!(resolved.report.files, 1);
        assert_eq!(resolved.report.languages, vec!["tsx".to_string()]);
        let err = scope(&[], &["rust"])
            .resolve(&files(), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("\"rust\" labels no indexed file"), "{err}");
        assert!(err.contains("typescript"), "{err}");
    }

    #[test]
    fn paths_and_languages_with_no_file_in_common_are_refused() {
        let err = scope(&["frontend"], &["go"])
            .resolve(&files(), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no indexed file is both"), "{err}");
        assert!(err.contains("tsx, typescript"), "{err}");
    }

    #[test]
    fn prefixes_that_cannot_name_a_repository_path_are_refused() {
        for bad in ["", ".", "./", "/", "frontend/../backend", ".."] {
            let paths = vec![bad.to_string()];
            assert!(
                SymbolScope::new(&paths, &[]).is_err()
                    || SymbolScope::new(&paths, &[])
                        .unwrap()
                        .unwrap()
                        .resolve(&files(), Some("/"))
                        .is_err(),
                "{bad:?} was accepted"
            );
        }
        let too_many: Vec<String> = (0..=MAX_SCOPE_PATHS).map(|i| format!("d{i}")).collect();
        assert!(SymbolScope::new(&too_many, &[]).is_err());
        assert!(SymbolScope::new(&[], &[" ".to_string()]).is_err());
    }
}
