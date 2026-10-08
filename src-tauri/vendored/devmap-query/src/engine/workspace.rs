use super::{budget_take, search_hit_tokens, StoreQueryEngine};
use crate::model::{Request, ResolutionAvailability};

/// Search every repository in a workspace, labelling each hit with its origin.
///
/// Repositories are queried in registry order and the budget is spent across
/// the union, so a large first repository can exhaust it before a later one is
/// reached. That is reported — `truncated` and `hidden` cover the whole
/// workspace, not one repository — rather than papered over by giving each
/// repository an equal slice, which would silently drop the best matches in a
/// large repository to make room for weak ones in a small one.
pub fn workspace_search(
    workspace: &crate::workspace::Workspace,
    query: &str,
    token_budget: u32,
    semantic: bool,
) -> anyhow::Result<crate::workspace::FederatedSearch> {
    use crate::workspace::{FederatedHit, FederatedSearch, RepoUnavailable};

    let mut all: Vec<FederatedHit> = Vec::new();
    let mut unavailable: Vec<RepoUnavailable> = Vec::new();
    let mut queried = 0usize;
    // Matches across the workspace *before* any budget was applied. Counting
    // the union of the returned items instead — which this did — counts what
    // each repository could afford to send, and every repository has already
    // spent its budget by then. A repository with 500 matches that fitted four
    // contributed four, and the federated answer called that the total and set
    // `truncated: false`.
    let mut matched_total = 0u32;

    for repo in &workspace.repos {
        let db = repo.db_path();
        let store = match devmap_store::Store::open_existing(&db) {
            Ok(Some(store)) => store,
            Ok(None) => {
                unavailable.push(RepoUnavailable {
                    repo: repo.name.clone(),
                    reason: format!("no store at {}", db.display()),
                });
                continue;
            }
            Err(error) => {
                unavailable.push(RepoUnavailable {
                    repo: repo.name.clone(),
                    reason: format!("store at {} could not be opened: {error}", db.display()),
                });
                continue;
            }
        };
        let engine = StoreQueryEngine::new(&store);
        // Each repository is asked for the *whole* budget's worth of hits; the
        // union is trimmed once at the end. Asking each for a slice would rank
        // within repositories instead of across them.
        let response = if semantic {
            engine.search_semantic(query, token_budget)?
        } else {
            engine.search(Request {
                query: query.to_string(),
                token_budget,
                min_confidence: 0.0,
                max_depth: 1,
            })?
        };
        if let ResolutionAvailability::Unavailable { reason } = &response.resolution {
            unavailable.push(RepoUnavailable {
                repo: repo.name.clone(),
                reason: reason.clone(),
            });
            continue;
        }
        queried += 1;
        matched_total = matched_total.saturating_add(response.total);
        for hit in response.items {
            all.push(FederatedHit {
                repo: repo.name.clone(),
                hit,
            });
        }
    }

    // One ranking across the workspace. Ties break on repository then path so
    // the order is total and identical on every run.
    all.sort_by(|a, b| {
        b.hit
            .score
            .total_cmp(&a.hit.score)
            .then_with(|| a.repo.cmp(&b.repo))
            .then_with(|| a.hit.file_path.cmp(&b.hit.file_path))
            .then_with(|| a.hit.symbol_name.cmp(&b.hit.symbol_name))
    });

    let budgeted = budget_take(all, token_budget, |entry| search_hit_tokens(&entry.hit));
    // `matched_total` counts every match each repository found, so it is never
    // below what was shown; the saturating subtraction is belt-and-braces
    // against a store that miscounts rather than a state this can reach.
    let hidden = matched_total.saturating_sub(budgeted.shown);
    Ok(FederatedSearch {
        items: budgeted.items,
        repos_queried: queried,
        unavailable,
        total: matched_total,
        shown: budgeted.shown,
        hidden,
        truncated: hidden > 0,
    })
}

/// Modules each repository *provides*, as `(specifier prefix, evidence)`.
///
/// Two sources, both declarations rather than inferences:
///
/// - Go: every `module` line in every `go.mod`. `import "manvi/dc/store"`
///   resolving to the repository whose `go.mod` says `module manvi` is not a
///   guess, it is how the toolchain resolves it.
/// - Python and JavaScript: top-level package directories — a directory
///   directly under the root containing `__init__.py`, or a `package.json`
///   `name`. Weaker than Go's, and labelled as the directory it came from.
///
/// Deliberately not included: matching on symbol names. Two repositories both
/// declaring `Client` is not a link, and asserting one would produce edges at a
/// rate that buries the real ones.
pub(super) fn provided_modules(root: &std::path::Path) -> Vec<(String, String)> {
    let mut provided: Vec<(String, String)> = Vec::new();

    if let Ok(modules) = devmap_extract::collect_go_modules(root) {
        for module in modules {
            if module.prefix.is_empty() {
                continue;
            }
            let where_from = if module.dir.is_empty() {
                "go.mod".to_string()
            } else {
                format!("{}/go.mod", module.dir)
            };
            provided.push((
                module.prefix.clone(),
                format!("{where_from} declares `module {}`", module.prefix),
            ));
        }
    }

    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name == "node_modules" || name == "target" {
                continue;
            }
            if entry.path().join("__init__.py").is_file() {
                provided.push((
                    name.clone(),
                    format!("{name}/__init__.py declares a Python package"),
                ));
            }
        }
    }
    // A `src/` layout puts the package one level down, which is where this
    // repository's own `devcouncil` package lives.
    if let Ok(entries) = std::fs::read_dir(root.join("src")) {
        for entry in entries.flatten() {
            if entry.path().join("__init__.py").is_file() {
                let name = entry.file_name().to_string_lossy().into_owned();
                provided.push((
                    name.clone(),
                    format!("src/{name}/__init__.py declares a Python package"),
                ));
            }
        }
    }

    provided.sort();
    provided.dedup();
    provided
}

/// Whether `specifier` is satisfied by a module named `prefix`.
///
/// Exact, or a path segment beneath it. `manvi/dc/store` is provided by
/// `manvi`; `manvibench` is not, and matching on a bare `starts_with` would
/// claim it is.
pub(super) fn specifier_matches(specifier: &str, prefix: &str) -> bool {
    if specifier == prefix {
        return true;
    }
    specifier
        .strip_prefix(prefix)
        .is_some_and(|rest| rest.starts_with('/') || rest.starts_with('.'))
}

/// Imports in one repository that another repository declares the module for.
///
/// Reported as *candidates*. A matching module path is strong evidence — for Go
/// it is how the compiler resolves the import — but this does not verify that
/// the imported symbol exists in the target, and it cannot tell a local
/// checkout from a published copy at a different version. Calling these
/// resolved edges would put an unverified claim in the graph beside verified
/// ones.
pub fn link_candidates(
    workspace: &crate::workspace::Workspace,
) -> anyhow::Result<Vec<crate::workspace::LinkCandidate>> {
    use crate::workspace::LinkCandidate;

    // What each repository provides.
    let mut providers: Vec<(&str, Vec<(String, String)>)> = Vec::new();
    for repo in &workspace.repos {
        providers.push((repo.name.as_str(), provided_modules(&repo.root)));
    }

    let mut candidates: Vec<LinkCandidate> = Vec::new();
    let mut loaded: Vec<(&str, Vec<devmap_extract::model::Extraction>)> = Vec::new();
    for repo in &workspace.repos {
        let Ok(Some(store)) = devmap_store::Store::open_existing(repo.db_path()) else {
            continue;
        };
        loaded.push((repo.name.as_str(), store.latest_extractions()?));
    }
    // Every Metal entry point in the workspace, by name: `(repo, file, symbol)`.
    let mut entry_points: std::collections::BTreeMap<&str, Vec<(&str, &str, &str)>> =
        std::collections::BTreeMap::new();
    for (repo_name, extractions) in &loaded {
        for extraction in extractions {
            for symbol in extraction.metal_entry_points() {
                entry_points.entry(symbol.name.as_str()).or_default().push((
                    repo_name,
                    extraction.file_path.as_str(),
                    symbol.qualified_name.as_str(),
                ));
            }
        }
    }
    for (repo_name, extractions) in &loaded {
        for extraction in extractions {
            candidates.extend(entry_name_links(repo_name, extraction, &entry_points));
        }
    }
    for repo in &workspace.repos {
        let Some((_, extractions)) = loaded.iter().find(|(name, _)| *name == repo.name) else {
            continue;
        };
        for extraction in extractions {
            for import in &extraction.imports {
                // A Python module loaded by file path names a file beside its
                // loader, never a module another repository provides — and
                // `scripts/x.py` would otherwise match a provider of `scripts`.
                if import.path_load.is_some() {
                    continue;
                }
                let specifier = import.module_specifier.trim();
                if specifier.is_empty() || specifier.starts_with('.') {
                    continue;
                }
                for (provider_name, provided) in &providers {
                    // A repository importing its own module is not a
                    // cross-repository link.
                    if *provider_name == repo.name {
                        continue;
                    }
                    for (prefix, evidence) in provided {
                        if specifier_matches(specifier, prefix) {
                            candidates.push(LinkCandidate {
                                kind: crate::workspace::LinkKind::Import,
                                from_repo: repo.name.clone(),
                                from_file: extraction.file_path.clone(),
                                module_specifier: specifier.to_string(),
                                to_repo: (*provider_name).to_string(),
                                evidence: evidence.clone(),
                                from_symbol: None,
                                to_symbol: None,
                            });
                        }
                    }
                }
            }
        }
    }
    candidates.sort_by(|a, b| {
        (
            a.kind,
            &a.from_repo,
            &a.from_file,
            &a.module_specifier,
            &a.to_repo,
            &a.from_symbol,
        )
            .cmp(&(
                b.kind,
                &b.from_repo,
                &b.from_file,
                &b.module_specifier,
                &b.to_repo,
                &b.from_symbol,
            ))
    });
    candidates.dedup_by(|a, b| {
        a.kind == b.kind
            && a.from_repo == b.from_repo
            && a.from_file == b.from_file
            && a.module_specifier == b.module_specifier
            && a.to_repo == b.to_repo
            && a.from_symbol == b.from_symbol
    });
    Ok(candidates)
}

/// Strings in `extraction` that name a Metal entry point another repository
/// declares.
///
/// The resolver's rule, across repositories: the name must be declared by
/// exactly one file in the whole workspace, and not by the naming repository
/// itself — a repository that declares the name resolves it inside its own
/// graph, and two declaring files mean the index cannot say which library the
/// string loads. A string nothing declares is no candidate, as it is no edge.
pub(super) fn entry_name_links(
    repo_name: &str,
    extraction: &devmap_extract::model::Extraction,
    entry_points: &std::collections::BTreeMap<&str, Vec<(&str, &str, &str)>>,
) -> Vec<crate::workspace::LinkCandidate> {
    use devmap_extract::model::ReferenceKind;
    let mut links = Vec::new();
    for reference in &extraction.references {
        if reference.kind != ReferenceKind::EntryName {
            continue;
        }
        let Some([(to_repo, to_file, to_symbol)]) =
            entry_points.get(reference.name.as_str()).map(Vec::as_slice)
        else {
            continue;
        };
        if *to_repo == repo_name {
            continue;
        }
        let from_symbol = reference.enclosing_symbol.clone().or_else(|| {
            reference
                .assigned_to
                .as_ref()
                .map(|binding| format!("{}::{binding}", extraction.file_path))
        });
        links.push(crate::workspace::LinkCandidate {
            kind: crate::workspace::LinkKind::EntryName,
            from_repo: repo_name.to_string(),
            from_file: extraction.file_path.clone(),
            module_specifier: reference.name.clone(),
            to_repo: (*to_repo).to_string(),
            evidence: format!(
                "{to_file} declares the Metal entry point `{}`, and no other indexed file does",
                reference.name
            ),
            from_symbol,
            to_symbol: Some((*to_symbol).to_string()),
        });
    }
    links
}
