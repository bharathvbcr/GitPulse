use super::*;

impl Resolver {
    /// Supply the Go module set for the snapshot about to be — or just —
    /// indexed. Replaces any previous set, and marks it current for exactly one
    /// `index_extractions` pass (see `go_modules_fresh`).
    pub fn index_go_modules(&mut self, modules: &[GoModule]) {
        self.go_modules = modules.to_vec();
        self.go_modules_fresh = true;
    }

    /// Supply every project manifest collected for the snapshot: Go modules
    /// and TypeScript / JavaScript project configs, under the one-pass
    /// freshness contract `index_go_modules` states.
    pub fn index_project_manifests(&mut self, manifests: &ProjectManifests) {
        self.index_go_modules(&manifests.go_modules);
        self.rust_manifest_crates = manifests.rust_crates.clone();
        let mut python_roots = manifests.python_roots.clone();
        // Deepest first: the nearest manifest's roots are probed first.
        python_roots.sort_by(|left, right| {
            Self::dir_depth(&right.dir)
                .cmp(&Self::dir_depth(&left.dir))
                .then_with(|| left.dir.cmp(&right.dir))
        });
        self.python_manifest_roots = python_roots;
        self.swift_targets = manifests.swift_targets.clone();
        self.gradle_module_dirs = manifests
            .gradle_modules
            .iter()
            .flat_map(|settings| settings.modules.iter().cloned())
            .collect();
        self.gradle_module_dirs.sort();
        self.gradle_module_dirs.dedup();
        let mut projects = manifests.ts_projects.clone();
        // Deepest first, whatever order arrived: the nearest config governs.
        projects.sort_by(|left, right| {
            Self::dir_depth(&right.dir)
                .cmp(&Self::dir_depth(&left.dir))
                .then_with(|| right.dir.len().cmp(&left.dir.len()))
                .then_with(|| left.dir.cmp(&right.dir))
        });
        self.ts_projects = projects;
        self.ts_workspaces = manifests.ts_workspaces.clone();
        self.ts_projects_fresh = true;
    }

    pub(super) fn dir_depth(dir: &str) -> usize {
        if dir.is_empty() {
            0
        } else {
            dir.split('/').count()
        }
    }

    /// The config governing `file`: the deepest whose directory contains it.
    pub(super) fn governing_ts_project(&self, file: &str) -> Option<&TsProject> {
        self.ts_projects.iter().find(|project| {
            project.dir.is_empty() || file.starts_with(&format!("{}/", project.dir))
        })
    }

    /// Where a non-relative specifier points under the governing config's
    /// `paths`, as the substituted base paths in TypeScript's order: for each
    /// mapping (own config, then references), the best-matching pattern —
    /// exact, else the longest prefix before its `*` — and its targets.
    ///
    /// A lone `*` pattern is skipped here: it matches every specifier, so it
    /// is no evidence that a given one names a file in this tree.
    pub(super) fn ts_paths_bases(&self, file: &str, specifier: &str) -> Vec<String> {
        let Some(project) = self.governing_ts_project(file) else {
            return Vec::new();
        };
        let mut bases = Vec::new();
        for mapping in &project.mappings {
            let mut best: Option<(usize, bool, &str, &Vec<String>)> = None;
            for (pattern, targets) in &mapping.paths {
                let matched = match pattern.split_once('*') {
                    None if pattern == specifier => Some((pattern.len(), true, "")),
                    None => None,
                    Some(("", "")) => None,
                    Some((prefix, suffix)) => specifier
                        .strip_prefix(prefix)
                        .and_then(|rest| rest.strip_suffix(suffix))
                        .map(|star| (prefix.len(), false, star)),
                };
                let Some((rank, exact, star)) = matched else {
                    continue;
                };
                let better = match best {
                    None => true,
                    Some((best_rank, best_exact, _, _)) => {
                        (exact && !best_exact) || (exact == best_exact && rank > best_rank)
                    }
                };
                if better {
                    best = Some((rank, exact, star, targets));
                }
            }
            if let Some((_, _, star, targets)) = best {
                bases.extend(targets.iter().map(|target| target.replacen('*', star, 1)));
            }
        }
        bases
    }

    /// The JS/TS module candidates for a base path: the file itself, each
    /// extension, then a directory's `index`.
    pub(super) fn js_module_candidates(base: &str) -> [String; 19] {
        [
            base.to_string(),
            format!("{}.ts", base),
            format!("{}.tsx", base),
            format!("{}.mts", base),
            format!("{}.cts", base),
            format!("{}.js", base),
            format!("{}.jsx", base),
            format!("{}.mjs", base),
            format!("{}.cjs", base),
            format!("{}.svelte", base),
            format!("{}.vue", base),
            format!("{}.astro", base),
            format!("{}/index.ts", base),
            format!("{}/index.tsx", base),
            format!("{}/index.js", base),
            format!("{}/index.jsx", base),
            format!("{}/index.svelte", base),
            format!("{}/index.vue", base),
            format!("{}/index.astro", base),
        ]
    }

    /// The TypeScript sources a NodeNext/Node16 specifier ending in a JavaScript
    /// extension can mean: `./a.js` is `a.ts` or `a.tsx`, `./a.jsx` is `a.tsx`,
    /// `./a.mjs` is `a.mts`, `./a.cjs` is `a.cts`. Empty for any other ending.
    pub(super) fn nodenext_source_siblings(base: &str) -> Vec<String> {
        let (stem, extensions): (&str, &[&str]) = if let Some(stem) = base.strip_suffix(".js") {
            (stem, &[".ts", ".tsx"])
        } else if let Some(stem) = base.strip_suffix(".jsx") {
            (stem, &[".tsx"])
        } else if let Some(stem) = base.strip_suffix(".mjs") {
            (stem, &[".mts"])
        } else if let Some(stem) = base.strip_suffix(".cjs") {
            (stem, &[".cts"])
        } else {
            return Vec::new();
        };
        extensions
            .iter()
            .map(|extension| format!("{stem}{extension}"))
            .collect()
    }

    /// A non-relative JS/TS specifier resolved through the governing config:
    /// its `paths` first, then each mapping's `baseUrl`.
    pub(super) fn resolve_ts_mapped(&self, file: &str, specifier: &str) -> Option<String> {
        let indexed = |base: &str| {
            Self::js_module_candidates(base)
                .into_iter()
                .find(|candidate| self.file_symbols.contains_key(candidate))
        };
        if let Some(hit) = self
            .ts_paths_bases(file, specifier)
            .iter()
            .find_map(|base| indexed(base))
        {
            return Some(hit);
        }
        let project = self.governing_ts_project(file)?;
        project.mappings.iter().find_map(|mapping| {
            let base_url = mapping.base_url.as_deref()?;
            let base = if base_url.is_empty() {
                specifier.to_string()
            } else {
                format!("{base_url}/{specifier}")
            };
            indexed(&base)
        })
    }

    /// Whether a JS/TS specifier is a `paths` pattern or a workspace package
    /// into this tree: a miss through one is an index gap, not an outside
    /// module.
    pub(super) fn specifier_is_ts_mapped(&self, file: &str, lang: &str, specifier: &str) -> bool {
        let specifier = specifier.trim_matches(|c| c == '\'' || c == '"');
        LangFamily::from_lang(lang) == LangFamily::JsTs
            && !specifier.starts_with('.')
            && (!self.ts_paths_bases(file, specifier).is_empty()
                || self.workspace_package_for(specifier).is_some())
    }

    /// The workspace package a bare specifier names, and the subpath written
    /// after the name: `@acme/shared/utils` is `@acme/shared` + `utils`.
    /// The longest name wins, so a package `@acme/shared-ui` is not mistaken
    /// for `@acme/shared` plus a subpath.
    pub(super) fn workspace_package_for<'a>(
        &'a self,
        specifier: &'a str,
    ) -> Option<(&'a devmap_extract::workspaces::WorkspacePackage, &'a str)> {
        self.ts_workspaces
            .iter()
            .filter_map(|package| {
                let rest = specifier.strip_prefix(package.name.as_str())?;
                (rest.is_empty() || rest.starts_with('/'))
                    .then(|| (package, rest.trim_start_matches('/')))
            })
            .max_by_key(|(package, _)| package.name.len())
    }

    /// The indexed file a workspace package specifier names.
    ///
    /// The package root asks the manifest's own entries first (`types`, `module`,
    /// `main`, `exports["."]`), then the conventional source entries — a
    /// `main` of `dist/index.js` is the build output of `src/index.ts`, and
    /// the tree holds the source. A subpath is tried under the package
    /// directory and under its `src/`.
    pub(super) fn resolve_workspace_package(&self, specifier: &str) -> Option<String> {
        let (package, subpath) = self.workspace_package_for(specifier)?;
        let join = |relative: &str| {
            let relative = relative.trim_start_matches("./");
            if package.dir.is_empty() {
                Self::normalize_rel("", relative)
            } else {
                Self::normalize_rel(&package.dir, relative)
            }
        };
        let mut bases: Vec<String> = Vec::new();
        if subpath.is_empty() {
            bases.extend(package.entries.iter().filter_map(|entry| join(entry)));
            bases.extend(
                ["src/index", "index"]
                    .iter()
                    .filter_map(|entry| join(entry)),
            );
        } else {
            bases.extend(
                [subpath.to_string(), format!("src/{subpath}")]
                    .iter()
                    .filter_map(|entry| join(entry)),
            );
        }
        for base in bases {
            if let Some(found) = Self::nodenext_source_siblings(&base)
                .into_iter()
                .find(|candidate| self.file_symbols.contains_key(candidate))
            {
                return Some(found);
            }
            if let Some(found) = Self::js_module_candidates(&base)
                .into_iter()
                .find(|candidate| self.file_symbols.contains_key(candidate))
            {
                return Some(found);
            }
        }
        None
    }
}
