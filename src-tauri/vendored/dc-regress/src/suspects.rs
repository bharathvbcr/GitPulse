//! The analysis itself: symptom to ranked suspects.
//!
//! The order of operations is the argument. A blame-first tool reads a file's
//! gutter and ranks by recency; this reads the *graph* first, so that the only
//! lines ever blamed are the ones belonging to symbols the symptom actually
//! depends on. Everything else in the file is noise by construction, and not
//! blaming it is both cheaper and more accurate.
//!
//! Every step can fail on its own, and each failure is recorded rather than
//! folded into an empty result. A report with an empty suspect list and an
//! empty `unavailable` means "nothing in the window touched the cone". A
//! report with an empty suspect list and a populated `unavailable` means
//! something could not be examined. Those are different answers and the type
//! keeps them different.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::blame::blame_file_with_program;
use crate::history::{commit_window_with_program, resolve_blob_with_program, HistoryRefusal};
use crate::join::{attribute, Attribution};
use crate::rank::{rank, RankInputs, TouchedSymbol};
use crate::{CodeGraph, SuspectReport, Unavailable};

/// How deep the dependency walk goes by default.
///
/// Three edges covers the overwhelming majority of real "this broke that"
/// relationships while keeping the blamed-file set small. A walk stopped by
/// this cap reports `ConeIncomplete` rather than presenting a lower bound as
/// the whole cone.
pub const DEFAULT_CONE_DEPTH: u32 = 3;

/// Most files that will be blamed for one question.
///
/// A cone that spans more files than this is not a useful answer anyway — the
/// point of the cone is that it is small — and blaming them all is the
/// unbounded work this cap exists to prevent.
pub const MAX_BLAMED_FILES: usize = 200;

/// The files a cone occupies, nearest first.
///
/// A file's rank is the smallest distance of any cone symbol it holds, and ties
/// break on the path so the order is total. Pulled out as its own function
/// because of [`MAX_BLAMED_FILES`]: when the cap trims, *which* files survive
/// is the difference between a trimmed answer made of the most likely causes
/// and one made of whatever sorted first alphabetically.
fn cone_files_nearest_first(cone: &BTreeMap<String, (String, u32)>) -> Vec<&str> {
    let mut nearest: BTreeMap<&str, u32> = BTreeMap::new();
    for (file, distance) in cone.values() {
        nearest
            .entry(file.as_str())
            .and_modify(|held| *held = (*held).min(*distance))
            .or_insert(*distance);
    }
    let mut files: Vec<&str> = nearest.keys().copied().collect();
    files.sort_by(|left, right| nearest[left].cmp(&nearest[right]).then(left.cmp(right)));
    files
}

/// Run the analysis.
pub fn suspects<G: CodeGraph>(
    repo: &Path,
    graph: &G,
    symptom: &str,
    since: &str,
    until: &str,
    depth: u32,
) -> SuspectReport {
    suspects_with_program(
        std::ffi::OsStr::new("git"),
        repo,
        graph,
        symptom,
        since,
        until,
        depth,
    )
}

#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn suspects_with_program<G: CodeGraph>(
    program: &std::ffi::OsStr,
    repo: &Path,
    graph: &G,
    symptom: &str,
    since: &str,
    until: &str,
    depth: u32,
) -> SuspectReport {
    let mut unavailable: Vec<Unavailable> = Vec::new();

    // 1. What does the symptom name?
    let targets = graph.resolve_symptom(symptom);
    if targets.is_empty() {
        return SuspectReport::refused(
            symptom.to_string(),
            vec![Unavailable::SymptomNotFound {
                symptom: symptom.to_string(),
            }],
        );
    }

    // 2. What does it depend on? Cones from several targets are unioned by
    //    qualified name, keeping the *shortest* distance — a symbol reachable
    //    from two targets is as close as its closest route.
    let mut cone: BTreeMap<String, (String, u32)> = BTreeMap::new();
    let mut cone_incomplete = false;
    for target in &targets {
        let (entries, incomplete) = graph.cone(target, depth);
        cone_incomplete |= incomplete;
        for entry in entries {
            cone.entry(entry.qualified_name)
                .and_modify(|held| {
                    if entry.distance < held.1 {
                        *held = (entry.file_path.clone(), entry.distance);
                    }
                })
                .or_insert((entry.file_path, entry.distance));
        }
    }
    if cone.is_empty() {
        return SuspectReport::refused(
            symptom.to_string(),
            vec![Unavailable::GraphEmpty {
                reason: format!(
                    "`{symptom}` resolved to {} symbol(s) but the graph gives them no \
                     dependencies and not even themselves, which an unbuilt or empty index \
                     also reports",
                    targets.len()
                ),
            }],
        );
    }
    if cone_incomplete {
        unavailable.push(Unavailable::ConeIncomplete {
            depth,
            reached: cone.len(),
        });
    }
    let cone_size = cone.len();

    // 3. Which commits are candidates?
    let window = match commit_window_with_program(program, repo, since, until) {
        Ok(window) => window,
        Err(HistoryRefusal::GitUnavailable { reason }) => {
            return SuspectReport::refused(
                symptom.to_string(),
                vec![Unavailable::GitUnavailable { reason }],
            )
        }
        Err(other) => {
            return SuspectReport::refused(
                symptom.to_string(),
                vec![Unavailable::GitUnavailable {
                    reason: other.describe(),
                }],
            )
        }
    };
    if window.capped {
        unavailable.push(Unavailable::WindowCapped {
            considered: window.commits.len(),
            cap: crate::history::COMMIT_CAP,
        });
    }
    let in_window: BTreeSet<&str> = window.commits.iter().map(String::as_str).collect();

    // 4. Blame only the files the cone actually occupies, **nearest first**.
    //
    // The order matters because of the cap below. Iterating a set of paths
    // would blame whichever 200 files sort first alphabetically, so a cone
    // spanning 300 files would drop the ones closest to the symptom in favour
    // of whatever happens to begin with `a`. Ranking by each file's nearest
    // cone symbol keeps a trimmed answer made of the most likely causes, which
    // is the same rule `affected_tests` applies to a budget-trimmed test list.
    let files = cone_files_nearest_first(&cone);

    let mut blamed_symbols = 0usize;
    // commit -> (author, time, touched symbols)
    let mut per_commit: BTreeMap<String, (String, i64, Vec<TouchedSymbol>)> = BTreeMap::new();

    for file in files.iter().take(MAX_BLAMED_FILES) {
        let (symbols, spans_basis) = graph.symbols_in(file);
        // Only the cone's symbols. A file usually holds many more, and blaming
        // lines for symbols that cannot reach the symptom is the noise this
        // whole design exists to avoid.
        let wanted: Vec<crate::GraphSymbol> = symbols
            .into_iter()
            .filter(|symbol| cone.contains_key(&symbol.qualified_name))
            .collect();
        if wanted.is_empty() {
            continue;
        }

        let resolved = match resolve_blob_with_program(program, repo, until, file) {
            Ok(resolved) => resolved,
            Err(refusal) => {
                unavailable.push(Unavailable::BlameRefused {
                    path: (*file).to_string(),
                    reason: refusal.describe(),
                });
                continue;
            }
        };

        let mut file_blame = match blame_file_with_program(program, repo, until, file) {
            Ok(blame) => blame,
            Err(refusal) => {
                unavailable.push(Unavailable::BlameRefused {
                    path: (*file).to_string(),
                    reason: refusal.describe(),
                });
                continue;
            }
        };
        // The blame and the content were resolved from the same rev and path,
        // so they are the same object. Stamping the identity here — where both
        // halves are in hand — is what lets `attribute` check rather than
        // assume.
        file_blame.basis = resolved.identity.clone();

        match attribute(&wanted, &spans_basis, &file_blame, &resolved.content) {
            Attribution::BasisMismatch {
                path,
                spans_taken_against,
                lines_built_from,
            } => {
                unavailable.push(Unavailable::BlobMismatch {
                    path,
                    spans_taken_against,
                    lines_built_from,
                });
                continue;
            }
            Attribution::Attributed(rows) => {
                blamed_symbols += rows.len();
                for row in rows {
                    let Some((_, distance)) = cone.get(&row.qualified_name) else {
                        continue;
                    };
                    for touch in row.commits {
                        if !in_window.contains(touch.commit.as_str()) {
                            continue;
                        }
                        let entry = per_commit.entry(touch.commit.clone()).or_insert_with(|| {
                            (touch.author.clone(), touch.author_time, Vec::new())
                        });
                        entry.2.push(TouchedSymbol {
                            qualified_name: row.qualified_name.clone(),
                            file_path: row.file_path.clone(),
                            distance: *distance,
                            lines: touch.lines,
                            body_changed: graph.body_changed(&row.qualified_name, &touch.commit),
                        });
                    }
                }
            }
        }
    }
    if files.len() > MAX_BLAMED_FILES {
        unavailable.push(Unavailable::FilesCapped {
            blamed: MAX_BLAMED_FILES,
            in_cone: files.len(),
            cap: MAX_BLAMED_FILES,
        });
    }

    let ranked = rank(
        per_commit
            .into_iter()
            .map(|(commit, (author, author_time, touched))| RankInputs {
                commit,
                author,
                author_time,
                touched,
            })
            .collect(),
    );

    SuspectReport::new(
        symptom.to_string(),
        ranked,
        cone_size,
        blamed_symbols,
        unavailable,
    )
}

#[cfg(test)]
mod tests {
    use super::{cone_files_nearest_first, MAX_BLAMED_FILES};
    use std::collections::BTreeMap;

    fn cone(entries: &[(&str, &str, u32)]) -> BTreeMap<String, (String, u32)> {
        entries
            .iter()
            .map(|(symbol, file, distance)| (symbol.to_string(), (file.to_string(), *distance)))
            .collect()
    }

    #[test]
    fn a_file_ranks_by_its_nearest_cone_symbol() {
        let cone = cone(&[("zzz.rs::near", "zzz.rs", 0), ("aaa.rs::far", "aaa.rs", 5)]);
        assert_eq!(
            cone_files_nearest_first(&cone),
            vec!["zzz.rs", "aaa.rs"],
            "distance decides before the path does; sorting by path alone would \
             put the far file first"
        );
    }

    #[test]
    fn a_file_holding_both_near_and_far_symbols_ranks_by_the_near_one() {
        let cone = cone(&[
            ("mixed.rs::far", "mixed.rs", 9),
            ("mixed.rs::near", "mixed.rs", 1),
            ("other.rs::mid", "other.rs", 4),
        ]);
        assert_eq!(
            cone_files_nearest_first(&cone),
            vec!["mixed.rs", "other.rs"]
        );
    }

    #[test]
    fn ties_break_on_the_path_so_the_order_is_total() {
        let cone = cone(&[
            ("b.rs::x", "b.rs", 2),
            ("a.rs::y", "a.rs", 2),
            ("c.rs::z", "c.rs", 2),
        ]);
        assert_eq!(
            cone_files_nearest_first(&cone),
            vec!["a.rs", "b.rs", "c.rs"]
        );
    }

    /// The property the ordering exists for. Past the cap the analysis blames a
    /// prefix, and that prefix must be the nearest files rather than an
    /// alphabetical accident.
    #[test]
    fn the_cap_keeps_the_nearest_files() {
        let mut built: BTreeMap<String, (String, u32)> = BTreeMap::new();
        // Distant files whose names sort first.
        for index in 0..MAX_BLAMED_FILES {
            built.insert(
                format!("aaa{index:04}.rs::far"),
                (format!("aaa{index:04}.rs"), 9u32),
            );
        }
        // One adjacent file whose name sorts last.
        built.insert("zzz.rs::near".to_string(), ("zzz.rs".to_string(), 0));

        let ordered = cone_files_nearest_first(&built);
        let kept: Vec<&str> = ordered.into_iter().take(MAX_BLAMED_FILES).collect();
        assert_eq!(
            kept[0], "zzz.rs",
            "the file adjacent to the symptom is examined first"
        );
        assert!(
            kept.contains(&"zzz.rs"),
            "and it survives a cap that drops {MAX_BLAMED_FILES} distant ones"
        );
    }
}
