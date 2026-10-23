use super::*;

impl Resolver {
    pub fn index_extractions(&mut self, extractions: &[Extraction]) {
        // The resolver indexes a complete snapshot. Resetting prevents stale
        // candidates and duplicate bindings when a caller reuses the object
        // for a rebuild.
        self.symbol_index.clear();
        self.file_symbols.clear();
        self.selector_symbols.clear();
        self.entry_names.clear();
        self.files_by_dir.clear();
        self.py_files_by_name.clear();
        self.path_module_bindings.clear();
        self.py_search_dirs.clear();
        self.rust_crate_roots.clear();
        self.rust_declared_roots.clear();
        self.rust_declared_libs.clear();
        self.rust_signatures.clear();
        self.unexported_symbols.clear();
        self.reexport_chains.clear();
        self.reexport_glob_cutoff = None;
        self.rust_module_globs.clear();
        self.default_exports.clear();
        self.receiver_types.clear();
        self.scoped_receiver_types.clear();
        self.poisoned_receiver_keys.clear();
        self.type_methods.clear();
        self.supertypes.clear();
        self.python_bases.clear();
        self.python_import_binds.clear();
        self.python_decorated.clear();
        self.python_class_enclosers.clear();
        self.python_dynamic_all.clear();
        self.python_scoped_import_bindings.clear();
        self.python_ambiguous_scoped_imports.clear();
        self.subtypes.clear();
        self.import_bindings.clear();
        self.value_imports.clear();
        self.module_value_types.clear();
        self.module_value_members.clear();
        self.js_objects.clear();
        self.return_types.clear();
        self.module_value_factories.clear();
        self.declared_types.clear();
        self.external_imports.clear();
        self.unindexed_local_imports.clear();
        self.external_module_roots.clear();
        self.local_module_roots.clear();
        self.qualified_names.clear();
        self.shared_identities.clear();
        self.symbol_parents.clear();
        self.scope_children.clear();
        self.go_package_by_file.clear();
        self.go_package_symbols.clear();
        self.go_package_methods.clear();
        self.go_package_member_vetoes.clear();
        self.go_package_dirs_unvouched.clear();
        self.go_model = GoModel::default();
        self.swift_module_by_file.clear();
        self.jvm_package_by_file.clear();
        self.swift_type_aliases.clear();
        self.swift_unassigned_type_mentions.clear();
        self.swift_type_header_mentions.clear();
        self.jvm_package_symbols.clear();
        self.field_initializers.clear();
        self.swift_module_files.clear();
        self.swift_module_symbols.clear();
        self.scope_locals.clear();
        self.rust_inline_modules.clear();
        self.rust_deref_targets.clear();
        self.rust_blanket_methods.clear();
        self.max_indexed_path_depth = extractions
            .iter()
            .map(|ext| ext.file_path.split('/').count())
            .max()
            .unwrap_or(0);
        self.unique_basename.clear();
        for ext in extractions {
            let basename = ext
                .file_path
                .rsplit('/')
                .next()
                .unwrap_or(&ext.file_path)
                .to_string();
            match self.unique_basename.entry(basename) {
                std::collections::btree_map::Entry::Vacant(slot) => {
                    slot.insert(Some(ext.file_path.clone()));
                }
                std::collections::btree_map::Entry::Occupied(mut slot) => {
                    // Seen twice: poisoned, permanently. Not "the later one
                    // wins" — the rung that reads this exists to abstain when
                    // it cannot tell, and a winner picked by input order is
                    // exactly the confidently-wrong edge it must not produce.
                    if slot.get().as_deref() != Some(ext.file_path.as_str()) {
                        slot.insert(None);
                    }
                }
            }
        }
        // `go_modules` is the one input this method does not own: it arrives
        // through `index_go_modules`, which callers may run before *or* after
        // this. Discard it unless it was supplied for this snapshot, so a
        // reused `Resolver` under-resolves Go imports rather than resolving
        // them against the previous snapshot's module map.
        if !std::mem::take(&mut self.go_modules_fresh) {
            self.go_modules.clear();
        }
        if !std::mem::take(&mut self.ts_projects_fresh) {
            self.ts_projects.clear();
            self.ts_workspaces.clear();
        }

        // Pass one establishes the complete file/symbol universe. Import
        // binding resolution must not depend on whether the importer happens
        // to precede its target in the input slice.
        for ext in extractions {
            let family = LangFamily::from_lang(&ext.language);
            let mut file_syms = Vec::new();
            // Overloads: two callables of one name in one file and one
            // container, in a language that overloads by signature.
            let overloads_by_signature = matches!(
                ext.language.as_str(),
                "kotlin" | "swift" | "java" | "csharp" | "scala"
            );
            let mut callable_names: BTreeMap<&str, usize> = BTreeMap::new();
            if overloads_by_signature {
                for sym in &ext.symbols {
                    if matches!(sym.kind, SymbolKind::Function | SymbolKind::Method) {
                        *callable_names
                            .entry(sym.qualified_name.as_str())
                            .or_default() += 1;
                    }
                }
            }
            let mut file_identities: BTreeSet<(&str, Option<&str>)> = BTreeSet::new();
            for sym in &ext.symbols {
                if family == LangFamily::Rust {
                    if let Some(signature) = &sym.signature {
                        self.rust_signatures.insert(
                            (ext.file_path.clone(), sym.qualified_name.clone()),
                            signature.clone(),
                        );
                    }
                }
                if !sym.is_exported
                    && Self::visibility_is_enforced(family)
                    && (family != LangFamily::JsTs || Self::is_js_module(ext))
                {
                    self.unexported_symbols
                        .insert((ext.file_path.clone(), sym.qualified_name.clone()));
                }
                let identity: Arc<str> = Arc::from(sym.qualified_name.as_str());
                // A Python redefinition, a Rust impl of two traits, or a local
                // function of the same name in two different functions are not
                // siblings of one id.
                if overloads_by_signature
                    && matches!(sym.kind, SymbolKind::Function | SymbolKind::Method)
                    && callable_names.get(sym.qualified_name.as_str()) > Some(&1)
                    && !file_identities.insert((
                        sym.qualified_name.as_str(),
                        Self::innermost_container(ext, sym),
                    ))
                {
                    self.shared_identities
                        .insert((ext.file_path.clone(), sym.qualified_name.clone()));
                }
                self.symbol_index
                    .entry(sym.name.clone())
                    .or_default()
                    .push((
                        ext.file_path.clone(),
                        sym.kind,
                        family,
                        Arc::clone(&identity),
                    ));
                // A Swift property lives in the same per-type member index as a
                // method, under its own key (`property_member`), so the one
                // lookup machinery — typed receiver, implicit self, the
                // supertype walk — serves both without a call ever meeting a
                // property or a read ever mistaking one for the other. An enum
                // case shares `Field` but is spelled and called like a method,
                // so it keeps the plain key.
                if sym.kind == SymbolKind::Method
                    || Self::is_swift_member_property(family, sym.kind)
                {
                    if let Some(type_name) = sym
                        .parent_symbol
                        .as_deref()
                        .and_then(|parent| parent.rsplit("::").next())
                    {
                        let member = if sym.is_member_property() {
                            Self::property_member(&sym.name)
                        } else {
                            sym.name.clone()
                        };
                        self.type_methods
                            .entry((family, type_name.to_string(), member))
                            .or_default()
                            .push((ext.file_path.clone(), sym.qualified_name.clone()));
                    }
                }
                if let Some(written) = &sym.return_type {
                    self.return_types.insert(
                        (ext.file_path.clone(), sym.qualified_name.clone()),
                        written.clone(),
                    );
                }
                self.symbol_index
                    .entry(sym.qualified_name.clone())
                    .or_default()
                    .push((
                        ext.file_path.clone(),
                        sym.kind,
                        family,
                        Arc::clone(&identity),
                    ));
                // A bare spelling is only an identity when exactly one declaration
                // owns it. Every global candidate separately retains its full identity.
                self.qualified_names
                    .entry((ext.file_path.clone(), sym.name.clone()))
                    .and_modify(|known| {
                        if known.as_deref() != Some(identity.as_ref()) {
                            *known = None;
                        }
                    })
                    .or_insert_with(|| Some(Arc::clone(&identity)));
                // Parent links are keyed by full graph identity.
                let parent = self
                    .symbol_parents
                    .entry((ext.file_path.clone(), sym.qualified_name.clone()))
                    .or_insert_with(|| {
                        sym.parent_symbol
                            .clone()
                            .unwrap_or_else(|| ext.file_path.clone())
                    })
                    .clone();
                self.scope_children
                    .entry((ext.file_path.clone(), parent, sym.name.clone()))
                    .or_default()
                    .push((sym.kind, Arc::clone(&identity)));
                // The markup/stylesheet namespace, kept out of the rungs above.
                if matches!(sym.kind, SymbolKind::MarkupAnchor | SymbolKind::StyleRule) {
                    let declaring = self.selector_symbols.entry(sym.name.clone()).or_default();
                    if !declaring.iter().any(|known| known.file == ext.file_path) {
                        declaring.push(SelectorDeclaration {
                            file: ext.file_path.clone(),
                            in_global_sheet: is_global_stylesheet(&ext.language),
                        });
                    }
                }
                file_syms.push(sym.name.clone());
            }
            self.file_symbols.insert(ext.file_path.clone(), file_syms);
            self.index_entry_names(ext);
            for (scope, local) in &ext.scope_locals {
                self.scope_locals
                    .insert((ext.file_path.clone(), scope.clone(), local.clone()));
            }
            for (owner, target) in &ext.rust_deref_targets {
                self.rust_deref_targets
                    .entry(owner.clone())
                    .and_modify(|known| {
                        if known.as_deref() != Some(target.as_str()) {
                            *known = None;
                        }
                    })
                    .or_insert_with(|| Some(target.clone()));
            }
            if !ext.rust_blanket_impl_params.is_empty() {
                for sym in ext
                    .symbols
                    .iter()
                    .filter(|sym| sym.kind == SymbolKind::Method)
                {
                    let blanket = sym
                        .parent_symbol
                        .as_deref()
                        .and_then(|parent| parent.rsplit("::").next())
                        .is_some_and(|owner| {
                            ext.rust_blanket_impl_params.iter().any(|p| p == owner)
                        });
                    if blanket {
                        let hit = (ext.file_path.clone(), sym.qualified_name.clone());
                        let hits = self
                            .rust_blanket_methods
                            .entry(sym.name.clone())
                            .or_default();
                        if !hits.contains(&hit) {
                            hits.push(hit);
                        }
                    }
                }
            }
            if !ext.rust_inline_modules.is_empty() {
                self.rust_inline_modules.insert(
                    ext.file_path.clone(),
                    ext.rust_inline_modules.iter().cloned().collect(),
                );
            }
            if let Some(pkg) = ext.go_package.as_deref().filter(|pkg| !pkg.is_empty()) {
                self.go_package_by_file
                    .insert(ext.file_path.clone(), pkg.to_string());
                // X45. The package block, indexed. `parent_symbol` is the
                // extractor's own answer for "what declares this", and a
                // file-level parent is exactly what Go puts in the package
                // block; a method's parent is its type, and no bare name
                // reaches one.
                let dir = Self::parent_dir(&ext.file_path);
                for sym in &ext.symbols {
                    if sym.kind == SymbolKind::File {
                        continue;
                    }
                    let file_level = sym
                        .parent_symbol
                        .as_deref()
                        .is_none_or(|parent| parent == ext.file_path);
                    if !file_level {
                        continue;
                    }
                    self.go_package_symbols
                        .entry((dir.clone(), pkg.to_string(), sym.name.clone()))
                        .or_default()
                        .push((ext.file_path.clone(), sym.qualified_name.clone(), sym.kind));
                }
                for sym in &ext.symbols {
                    if sym.kind == SymbolKind::Method && Self::go_name_is_unexported(&sym.name) {
                        self.go_package_methods
                            .entry((dir.clone(), pkg.to_string(), sym.name.clone()))
                            .or_default()
                            .push((ext.file_path.clone(), sym.qualified_name.clone()));
                    }
                }
                self.go_model.index(ext, &dir, pkg);
                for name in ext.go_member_names.iter().flatten() {
                    self.go_package_member_vetoes.insert((
                        dir.clone(),
                        pkg.to_string(),
                        name.clone(),
                    ));
                }
            }
            // Keyed on the directory, not the package clause: a file that
            // failed to parse has no clause to key on, and it may belong to
            // any package in its directory.
            // A `Partial` parse vouches only because its extraction vetoes every
            // unexported token in the file (see `go_unexported_tokens`); every
            // other non-clean outcome contributed no member list worth trusting.
            if ext.file_path.ends_with(".go")
                && (!matches!(
                    ext.parse_outcome,
                    ParseOutcome::Clean | ParseOutcome::Partial { .. }
                ) || ext.go_member_names.is_none())
            {
                self.go_package_dirs_unvouched
                    .insert(Self::parent_dir(&ext.file_path));
            }
            self.swift_type_aliases
                .extend(ext.swift_type_aliases.iter().cloned());
            if family == LangFamily::Swift {
                // A generic parameter's declaration is a `Type` reference like
                // any other: `id<Item>(_ x: Item) -> Item` has one for the
                // clause, one for the parameter and one for the return, and
                // `Box<Element>` has one on the type itself. What tells a
                // parameter from a named type is therefore how many unassigned
                // mentions a callable makes of the name (a lone one is the
                // return type) and whether the owner type mentions it outside
                // any member. Counted here so the resolver reads one fact.
                let type_identities: BTreeSet<&str> = ext
                    .symbols
                    .iter()
                    .filter(|sym| {
                        matches!(
                            sym.kind,
                            SymbolKind::Class
                                | SymbolKind::Struct
                                | SymbolKind::Enum
                                | SymbolKind::Interface
                        )
                    })
                    .map(|sym| sym.qualified_name.as_str())
                    .collect();
                for reference in &ext.references {
                    if reference.kind != ReferenceKind::Type {
                        continue;
                    }
                    if let (Some(owner), None) = (
                        reference.enclosing_symbol.as_deref(),
                        reference.assigned_to.as_deref(),
                    ) {
                        let written = reference
                            .name
                            .rsplit('.')
                            .next()
                            .unwrap_or(&reference.name)
                            .to_string();
                        if type_identities.contains(owner) {
                            let bare = owner.rsplit("::").next().unwrap_or(owner);
                            let bare = bare.rsplit('.').next().unwrap_or(bare);
                            self.swift_type_header_mentions
                                .insert((bare.to_string(), written.clone()));
                        }
                        *self
                            .swift_unassigned_type_mentions
                            .entry((ext.file_path.clone(), owner.to_string(), written))
                            .or_insert(0) += 1;
                    }
                }
            }
            if let Some(package) = ext.jvm_package.as_deref() {
                self.jvm_package_by_file
                    .insert(ext.file_path.clone(), package.to_string());
                for sym in &ext.symbols {
                    if sym.kind != SymbolKind::File {
                        self.jvm_package_symbols
                            .entry((package.to_string(), sym.name.clone()))
                            .or_default()
                            .insert(ext.file_path.clone());
                    }
                }
            }
            if ext.language == "swift" {
                let modules = self.swift_modules_of(&ext.file_path);
                if !modules.is_empty() {
                    self.swift_module_by_file
                        .insert(ext.file_path.clone(), modules.clone());
                    for module in &modules {
                        self.swift_module_files
                            .entry(module.clone())
                            .or_default()
                            .insert(ext.file_path.clone());
                        for sym in &ext.symbols {
                            if sym.kind == SymbolKind::File {
                                continue;
                            }
                            let file_level = sym
                                .parent_symbol
                                .as_deref()
                                .is_none_or(|parent| parent == ext.file_path);
                            if !file_level {
                                continue;
                            }
                            // `private` and `fileprivate` — the two levels Swift's
                            // `is_exported` answers false for — confine a name to
                            // its own file, so neither a sibling of the module nor
                            // an importer can bind it. Indexing one anyway let
                            // `import ChronosCore` put `DialDrag.swift`'s private
                            // `forwardSpan` in scope for an app file whose struct
                            // declares its own, and the struct's method — the real
                            // target — was published dead (the audit's Swift row).
                            // Same-file references never consult this index.
                            if !sym.is_exported {
                                continue;
                            }
                            self.swift_module_symbols
                                .entry((module.clone(), sym.name.clone()))
                                .or_default()
                                .push((
                                    ext.file_path.clone(),
                                    sym.qualified_name.clone(),
                                    sym.kind,
                                ));
                        }
                    }
                }
            }
        }

        // Group the indexed files by directory once, now that `file_symbols`
        // holds the complete set and before pass two asks the first "what is in
        // this directory" question. Built into a local because the loop borrows
        // `file_symbols` for the whole walk.
        //
        // Each group comes out ascending — `BTreeMap::keys` yields sorted keys
        // and the push preserves that — but the sort is explicit rather than
        // inherited, so the ordering `go_files_in_dir` and
        // `files_in_dir_with_extensions` promise survives a change of source.
        let mut files_by_dir: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for path in self.file_symbols.keys() {
            files_by_dir
                .entry(Self::parent_dir(path))
                .or_default()
                .push(path.clone());
        }
        for files in files_by_dir.values_mut() {
            files.sort();
        }
        self.files_by_dir = files_by_dir;
        let mut py_files_by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for path in self
            .file_symbols
            .keys()
            .filter(|path| path.ends_with(".py"))
        {
            let name = path.rsplit('/').next().unwrap_or(path);
            py_files_by_name
                .entry(name.to_string())
                .or_default()
                .push(path.clone());
        }
        self.py_files_by_name = py_files_by_name;
        let mut py_package_parents: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for path in self.file_symbols.keys() {
            let Some(package_dir) = path.strip_suffix("/__init__.py") else {
                continue;
            };
            let parent = Self::parent_dir(package_dir);
            let is_top_level = !self
                .file_symbols
                .contains_key(&format!("{parent}/__init__.py"))
                && !(parent.is_empty() && self.file_symbols.contains_key("__init__.py"));
            if let (true, Some(name)) = (is_top_level, package_dir.rsplit('/').next()) {
                py_package_parents
                    .entry(name.to_string())
                    .or_default()
                    .insert(parent);
            }
        }
        self.py_package_parents = py_package_parents;

        // Which crate each indexed `src/lib.rs` or `src/main.rs` is the root
        // of, keyed by the name a `use` would spell. Built here for the same
        // reason `files_by_dir` is: `file_symbols` is complete and pass two is
        // about to start asking.
        let mut rust_crate_roots: BTreeMap<String, Option<String>> = BTreeMap::new();
        for path in self.file_symbols.keys() {
            let Some(src_root) = path
                .strip_suffix("/lib.rs")
                .or_else(|| path.strip_suffix("/main.rs"))
            else {
                continue;
            };
            if src_root != "src" && !src_root.ends_with("/src") {
                continue;
            }
            let crate_dir = Self::parent_dir(src_root);
            let Some(name) = crate_dir.rsplit('/').next().filter(|name| !name.is_empty()) else {
                continue;
            };
            let name = name.replace('-', "_");
            let root = src_root.to_string();
            Self::claim_rust_crate_name(&mut rust_crate_roots, name, root);
        }
        // The target roots a manifest places by hand. Only indexed ones: a
        // path the manifest names that is not in the corpus is the manifest
        // pass's refusal row, not a root to resolve against.
        let mut declared_roots: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut declared_libs: BTreeMap<String, String> = BTreeMap::new();
        let mut shared_lib_dirs: BTreeSet<String> = BTreeSet::new();
        for manifest in &self.rust_manifest_crates {
            for file in manifest.lib_path.iter().chain(&manifest.bin_paths) {
                if self.file_symbols.contains_key(file) {
                    declared_roots
                        .entry(Self::parent_dir(file))
                        .or_default()
                        .insert(file.clone());
                }
            }
            if let Some(lib) = manifest
                .lib_path
                .as_ref()
                .filter(|lib| self.file_symbols.contains_key(*lib))
            {
                let dir = Self::parent_dir(lib);
                if declared_libs.get(&dir).is_some_and(|held| held != lib) {
                    shared_lib_dirs.insert(dir);
                } else {
                    declared_libs.insert(dir, lib.clone());
                }
            }
        }
        // The names a manifest gives its roots. A package whose library is
        // `[lib] name = "xlib"` is spelled `use xlib::…` by every caller, and
        // its directory name is no help (GitPulse: `src-tauri`, package
        // `gitpulse`, lib `gitpulse_lib`). Only where the crate's own root
        // file is indexed. Two manifests claiming one name abstain, as two
        // directories do; a manifest's name outranks a directory's guess at
        // one, which is a convention and not a declaration.
        let mut manifest_claims: BTreeMap<String, Option<String>> = BTreeMap::new();
        for manifest in &self.rust_manifest_crates {
            let src_root = match &manifest.lib_path {
                Some(lib) => {
                    if !self.file_symbols.contains_key(lib) {
                        continue;
                    }
                    Self::parent_dir(lib)
                }
                None => {
                    let src_root = Self::rel_join(&manifest.dir, "src");
                    let indexed = ["lib.rs", "main.rs"].iter().any(|file| {
                        self.file_symbols
                            .contains_key(&format!("{src_root}/{file}"))
                    });
                    if !indexed {
                        continue;
                    }
                    src_root
                }
            };
            for name in &manifest.names {
                // Two libraries rooted in one directory: the directory cannot
                // say which root a name means.
                if shared_lib_dirs.contains(&src_root) {
                    manifest_claims.insert(name.clone(), None);
                    continue;
                }
                Self::claim_rust_crate_name(&mut manifest_claims, name.clone(), src_root.clone());
            }
        }
        rust_crate_roots.extend(manifest_claims);
        for dir in &shared_lib_dirs {
            declared_libs.remove(dir);
        }
        self.rust_crate_roots = rust_crate_roots;
        self.rust_declared_roots = declared_roots;
        self.rust_declared_libs = declared_libs;
        // Python packages this corpus indexes, by the name an absolute import
        // would spell: every directory holding an indexed `__init__.py`.
        let python_packages: BTreeSet<String> = self
            .file_symbols
            .keys()
            .filter_map(|path| {
                let dir = path.strip_suffix("/__init__.py")?;
                dir.rsplit('/').next().map(str::to_string)
            })
            .collect();
        let rust_crates: BTreeSet<String> = self.rust_crate_roots.keys().cloned().collect();
        // The directory path of every indexed JVM source file, at every suffix:
        // `core/src/main/java/com/fx/util/A.kt` holds `util`, `fx/util`,
        // `com/fx/util` and so on. A JVM package is its directory path, so a
        // dotted import whose package matches one is an import into this
        // repository, whatever the build layout puts above it.
        // Directory names of indexed JS/TS files: a scoped specifier
        // `@acme/shared` whose last segment names one is a workspace package
        // of this repository, not a registry package.
        let js_dir_names: BTreeSet<String> = self
            .file_symbols
            .keys()
            .filter(|path| {
                matches!(
                    path.rsplit('.').next(),
                    Some("ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs")
                )
            })
            .flat_map(|path| {
                let mut parts: Vec<&str> = path.split('/').collect();
                parts.pop();
                parts.into_iter().map(str::to_string)
            })
            .collect();
        let jvm_package_dirs: BTreeSet<String> = self
            .file_symbols
            .keys()
            .filter(|path| {
                matches!(
                    path.rsplit('.').next(),
                    Some("kt" | "kts" | "java" | "scala")
                )
            })
            .flat_map(|path| {
                let dir = path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
                let mut suffixes = Vec::new();
                let mut rest = dir;
                while !rest.is_empty() {
                    suffixes.push(rest.to_string());
                    match rest.split_once('/') {
                        Some((_, tail)) => rest = tail,
                        None => break,
                    }
                }
                suffixes
            })
            .collect();

        // Between the passes, and necessarily so: following a barrel needs the
        // complete symbol universe pass one builds, and pass two's import
        // bindings need the chains.
        self.default_exports = extractions
            .iter()
            .filter_map(|ext| {
                ext.exports
                    .iter()
                    .find(|export| {
                        export.exported_name == "default" && export.module_specifier.is_none()
                    })
                    .and_then(|export| export.local_name.clone())
                    .filter(|local| !local.is_empty() && local != "default")
                    .map(|local| (ext.file_path.clone(), local))
            })
            .collect();
        (self.reexport_chains, self.reexport_glob_cutoff) =
            self.compute_reexport_chains(extractions);
        self.rust_module_globs = self.compute_rust_module_globs(extractions);

        // Pass two resolves aliases and receiver hints against the complete
        // universe built above.
        for ext in extractions {
            let mut file_bindings = BTreeMap::new();
            // The value-binding subset of `file_bindings`; see `value_imports`.
            let mut file_values: Vec<(String, (String, String))> = Vec::new();
            // SC18: the mirror of `file_bindings` — every local name whose
            // module resolved to no indexed file. Recorded from the same walk so
            // the two maps cannot disagree about what an import specifier means.
            let mut file_external: BTreeMap<String, String> = BTreeMap::new();
            // The half of that mirror whose specifier is repo-relative, and so
            // proves an index gap rather than an outside origin.
            let mut file_local_gap: BTreeMap<String, String> = BTreeMap::new();
            // X43. The specifier's own leading segment, recorded beside the
            // local handle. A path is written from its root — `serde_json::
            // from_str(...)` — and the root of a specifier is a key no
            // handle-keyed map holds.
            let mut file_external_roots: BTreeSet<String> = BTreeSet::new();
            let mut file_local_roots: BTreeSet<String> = BTreeSet::new();
            // Specifiers a governing tsconfig `paths` pattern maps into this
            // tree. Owned, so the closure below holds no borrow of `self`.
            let ts_mapped: BTreeSet<String> = ext
                .imports
                .iter()
                .filter(|imp| {
                    self.specifier_is_ts_mapped(
                        &ext.file_path,
                        &ext.language,
                        &imp.module_specifier,
                    )
                })
                .map(|imp| imp.module_specifier.clone())
                .collect();
            // An absolute specifier whose root names a package this corpus
            // indexes — `from pkg.core import Engine` with `lib/pkg/__init__.py`
            // indexed, `use xlib::engine` with `xlib` a `[lib] name` here — is
            // an index gap however the ladder failed it, exactly as a
            // repo-relative one is. Filing it `External` printed live code's
            // only callers as "expected", and nothing vetoed the dead verdict.
            let names_an_indexed_package = |specifier: &str| {
                let root = Self::path_root(specifier.trim_matches(|c| c == '\'' || c == '"'));
                match ext.language.as_str() {
                    "python" => python_packages.contains(root),
                    "rust" => rust_crates.contains(root),
                    "typescript" | "tsx" | "javascript" | "jsx" => {
                        let spec = specifier.trim_matches(|c| c == '\'' || c == '"');
                        spec.starts_with('@')
                            && spec
                                .rsplit('/')
                                .next()
                                .is_some_and(|name| js_dir_names.contains(name))
                    }
                    "kotlin" | "java" | "scala" => {
                        // `com.fx.util.format` names the package `com.fx.util`
                        // (the last segment is the member) or, for `com.fx.*`
                        // or an import of a package itself, the whole path.
                        let dotted = specifier
                            .trim_matches(|c| c == '\'' || c == '"')
                            .trim_end_matches(".*");
                        let whole = dotted.replace('.', "/");
                        jvm_package_dirs.contains(&whole)
                            || dotted.rsplit_once('.').is_some_and(|(package, _)| {
                                jvm_package_dirs.contains(&package.replace('.', "/"))
                            })
                    }
                    _ => false,
                }
            };
            let mut unresolved_import = |local: String, specifier: &str| {
                let root = Self::path_root(specifier);
                if Self::specifier_is_repo_relative(specifier)
                    || ts_mapped.contains(specifier)
                    || names_an_indexed_package(specifier)
                {
                    if !root.is_empty() {
                        file_local_roots.insert(root.to_string());
                    }
                    file_local_gap.insert(local, specifier.to_string());
                } else {
                    if !root.is_empty() {
                        file_external_roots.insert(root.to_string());
                    }
                    file_external.insert(local, specifier.to_string());
                }
            };
            let mut file_path_modules: BTreeMap<(Option<String>, String), String> = BTreeMap::new();
            // Before this file's imports are read: they consult it.
            let search_path = self.search_dirs_of(ext);
            if !search_path.entries.is_empty() {
                self.py_search_dirs
                    .insert(ext.file_path.clone(), search_path);
            }
            // A Python import written inside a function or class body binds a
            // name in that scope, not the module's. The table is flat, so such
            // an import must not take a name the module scope already binds
            // (a later `import two as o` in a function is not what a class at
            // module level reads as `o`); it still binds one nothing else does.
            let py_scopes = (LangFamily::from_lang(&ext.language) == LangFamily::Python)
                .then(|| pytypes::python_scope_containers(ext));
            // What each such import bound, by the scope that wrote it, and the
            // names the module scope itself imports.
            let mut file_scoped: BTreeMap<String, BTreeMap<String, Option<(String, String)>>> =
                BTreeMap::new();
            let mut module_imported: BTreeSet<String> = BTreeSet::new();
            let mut note_scoped = |scope: &str, local: &str, bound: Option<(String, String)>| {
                use std::collections::btree_map::Entry;
                match file_scoped
                    .entry(scope.to_string())
                    .or_default()
                    .entry(local.to_string())
                {
                    Entry::Vacant(slot) => {
                        slot.insert(bound);
                    }
                    // Two imports of one name in one scope: whichever ran.
                    Entry::Occupied(mut slot) => {
                        if *slot.get() != bound {
                            slot.insert(None);
                        }
                    }
                }
            };
            for imp in &ext.imports {
                let nested_python: Option<String> = py_scopes.as_ref().and_then(|scopes| {
                    pytypes::python_innermost_container(scopes, &imp.span)
                        .map(|(_, scope)| scope.to_string())
                });
                // A Python module loaded by file path binds a *scoped* handle,
                // so it goes to its own table and never to `file_bindings`. A
                // load that binds nothing (`run_path`) contributes only the
                // file edge, which the resolution pass emits.
                if let Some(load) = &imp.path_load {
                    if let Some(local) = imp.alias.as_ref() {
                        if let Some(target) = self.resolve_loaded_module(ext, imp, load) {
                            file_path_modules.insert((load.scope.clone(), local.clone()), target);
                        }
                    }
                    continue;
                }
                if !imp.imported_names.is_empty() {
                    for (idx, name) in imp.imported_names.iter().enumerate() {
                        // A legacy singular alias is safe only for a single
                        // imported symbol. Applying it to every entry would
                        // corrupt multi-alias imports when old serialized
                        // data has no aligned local_names vector.
                        let local = imp
                            .local_names
                            .get(idx)
                            .map(String::as_str)
                            .filter(|local| !local.is_empty())
                            .or_else(|| {
                                (imp.imported_names.len() == 1)
                                    .then_some(imp.alias.as_deref())
                                    .flatten()
                            })
                            .unwrap_or(name.as_str());
                        let spec = Self::import_spec_for_name(&imp.module_specifier, name);
                        // A Swift kinded import (`import struct Foundation.Date`)
                        // names a **module**, not a file. `resolve_import_path`
                        // would look for a file called `Foundation` and either
                        // miss or, worse, bind a coincidental `Foundation.swift`.
                        let direct = if ext.language == "swift" {
                            self.swift_file_declaring(&imp.module_specifier, name)
                        } else {
                            self.resolve_import_path(&ext.file_path, &ext.language, &spec)
                                .or_else(|| {
                                    self.resolve_via_search_dirs(ext, imp.span.start_byte, &spec)
                                })
                        };
                        // `from pkg import cmd` binds an attribute of
                        // `pkg/__init__.py` when that file defines one, and the
                        // *submodule* `pkg/cmd.py` when it does not. Only the
                        // first was ever tried, so every submodule import bound
                        // to the package `__init__` — a file that does not
                        // declare the name — and every use through it resolved
                        // to nothing.
                        //
                        // That is how `from devcouncil.cli.commands import
                        // baseline, boot, …` lost its edges. Each name is a
                        // module; `app.command(...)(baseline.baseline)` is the
                        // only use of the command behind it; with the binding
                        // pointing at `commands/__init__.py`, which declares
                        // none of them, all ten CLI entry points in that one
                        // file read as confidently dead.
                        //
                        // The `__init__` binding still wins when it really does
                        // declare the name, which is what a re-exporting
                        // package means and the order Python itself uses.
                        // `import { default as X }` and the default a mixed
                        // `import X, { y }` carries ask the module for the name
                        // `default`, which no symbol has: the module's own
                        // `export default <local>` says which one it is.
                        let default_local = (name == "default")
                            .then(|| {
                                direct
                                    .as_deref()
                                    .and_then(|file| self.default_exports.get(file))
                            })
                            .flatten();
                        let name = default_local.unwrap_or(name);
                        let declares_name = default_local.is_some()
                            || direct
                                .as_deref()
                                .and_then(|file| self.file_symbols.get(file))
                                .is_some_and(|symbols| symbols.iter().any(|symbol| symbol == name));
                        // The barrel's own statement, used before the
                        // fallbacks. `export { thing } from './impl'` says
                        // where `thing` comes from; without consulting it the
                        // binding points at a file that declares no such
                        // symbol, the ladder falls through to the bare-name
                        // global lookup, and a name two files declare fans out
                        // ambiguously to both — including one the caller
                        // demonstrably does not call.
                        let via_reexport = (!declares_name)
                            .then(|| {
                                direct.as_deref().and_then(|file| {
                                    self.reexport_chains.get(&format!("{file}::{name}"))
                                })
                            })
                            .flatten()
                            .and_then(|terminal| {
                                terminal.rsplit_once("::").map(|(file, declared)| {
                                    (file.to_string(), declared.to_string())
                                })
                            });
                        // A renaming re-export (`export { a as b }`, `from impl
                        // import a as b`) publishes `b` and declares `a`; the
                        // binding must name what the terminal file declares,
                        // or every lookup through it asks for a name that file
                        // does not have.
                        let mut declared_as = name.clone();
                        // Both branches that answer with a file *declaring* the
                        // name bind a value; the submodule fallback below binds
                        // a module. `via_reexport` keeps the terminal's own
                        // name, because `export { svc as questions }` publishes
                        // a name the declaring file never wrote.
                        let declared_value = if declares_name {
                            direct.clone().map(|file| (file, name.clone()))
                        } else {
                            via_reexport.clone()
                        };
                        // Only where an import binds one namespace. A Rust
                        // `use blast::{blast}` binds the function while
                        // `blast::Report` still walks the module of the same
                        // name — values and modules live apart there, so the
                        // binding says nothing about what a path through it
                        // means.
                        let single_namespace = matches!(
                            LangFamily::from_lang(&ext.language),
                            LangFamily::JsTs | LangFamily::Python
                        );
                        if let (true, Some(declared), Some(_)) =
                            (single_namespace, &declared_value, &direct)
                        {
                            file_values.push((local.to_string(), declared.clone()));
                        }
                        let resolved = if declares_name {
                            direct
                        } else if let Some((file, symbol)) = via_reexport {
                            declared_as = symbol;
                            Some(file)
                        } else {
                            let submodule = match ext.language.as_str() {
                                "python" => {
                                    let dotted =
                                        Self::python_submodule_spec(&imp.module_specifier, name);
                                    self.resolve_import_path(&ext.file_path, &ext.language, &dotted)
                                        .or_else(|| {
                                            self.resolve_via_search_dirs(
                                                ext,
                                                imp.span.start_byte,
                                                &dotted,
                                            )
                                        })
                                }
                                // The same shape in Rust: `use devmap_serve::
                                // session_log;` names the module `session_log`,
                                // and the crate root it was bound to declares
                                // no such item — `pub mod session_log;` is a
                                // module declaration, not a symbol — so every
                                // `session_log::read_live()` through it found
                                // nothing. Only a file that *is* that module
                                // counts: a walk that popped the name back off
                                // lands on the parent, which is `direct`.
                                "rust" => self
                                    .resolve_import_path(
                                        &ext.file_path,
                                        &ext.language,
                                        &format!("{}::{}", imp.module_specifier, name),
                                    )
                                    .filter(|file| {
                                        file.ends_with(&format!("/{name}.rs"))
                                            || file.ends_with(&format!("/{name}/mod.rs"))
                                    }),
                                _ => None,
                            };
                            submodule.or(direct)
                        };
                        if let Some(target_f) = resolved {
                            if let Some(scope) = &nested_python {
                                note_scoped(
                                    scope,
                                    local,
                                    Some((target_f.clone(), declared_as.clone())),
                                );
                                file_bindings
                                    .entry(local.to_string())
                                    .or_insert((target_f, declared_as));
                            } else {
                                module_imported.insert(local.to_string());
                                file_bindings.insert(local.to_string(), (target_f, declared_as));
                            }
                        } else {
                            if let Some(scope) = &nested_python {
                                note_scoped(scope, local, None);
                            }
                            unresolved_import(local.to_string(), &imp.module_specifier);
                        }
                    }
                } else {
                    let alias = imp.alias.as_deref();
                    if alias == Some("_") {
                        continue;
                    }
                    let targets = self.resolve_import_targets_or_search_dirs(ext, imp);
                    let Some(target_f) = targets.first().cloned() else {
                        // A whole-module import that named no indexed file:
                        // `import "strings"`, `import react from "react"`. The
                        // local name is the package handle, so a later
                        // `strings.TrimSpace` can be recognised by its receiver.
                        //
                        // `.` is not a handle — it is the marker for "bind
                        // everything this module exports", so a glob whose
                        // module resolved to nothing must fall back to the
                        // specifier's own last segment. Keying `external_imports`
                        // under `"."` would file the evidence under a name no
                        // call site can ever mention.
                        let local = alias
                            .filter(|alias| *alias != ".")
                            .map(str::to_string)
                            .unwrap_or_else(|| {
                                Self::import_local_name(&ext.language, &imp.module_specifier)
                            });
                        if let Some(scope) = &nested_python {
                            note_scoped(scope, &local, None);
                        }
                        unresolved_import(local, &imp.module_specifier);
                        continue;
                    };
                    // `from m import *`, marked by the alias `*` as a Go
                    // dot-import is by `.`. The wildcard names nothing, but it is
                    // evidence about every bare name in the file, and is kept as
                    // such rather than dropped:
                    //
                    // * a source the tree does not index took the early exit
                    //   above and sits under [`STAR_IMPORT`] in the external or
                    //   unindexed half, which every bare-name rung reads as "this
                    //   name may come from there" and so will not bind to a
                    //   namesake;
                    // * an indexed source binds the names it declares at file
                    //   level, as Python does — public ones only, and never over
                    //   an explicit import, which `or_insert_with` keeps.
                    if alias == Some(STAR_IMPORT) {
                        for file in targets.iter().filter(|file| **file != ext.file_path) {
                            for symbol in self.file_symbols.get(file).into_iter().flatten() {
                                if !symbol.starts_with('_')
                                    && self.declared_at_file_level(file, symbol)
                                {
                                    file_bindings
                                        .entry(symbol.clone())
                                        .or_insert_with(|| (file.clone(), symbol.clone()));
                                }
                            }
                        }
                        continue;
                    }
                    if alias == Some(".") {
                        for file in &targets {
                            // X41. A glob of the file's *own* module —
                            // `mod tests { use super::*; }` — is skipped, and
                            // not as an optimisation. `import_bindings` is
                            // per-file and rung 2a consults it with **no scope
                            // test**, so binding every symbol of this file into
                            // it would let a bare `run()` anywhere in the file
                            // reach `class C: def run(self)` at DETERMINISTIC —
                            // the fabricated-caller defect rung 2c's
                            // `bare_name_is_in_scope` exists to stop. The
                            // same-file rungs already reach everything this
                            // binding could, and they apply that test.
                            if file == &ext.file_path {
                                continue;
                            }
                            if let Some(syms) = self.file_symbols.get(file) {
                                for name in syms {
                                    file_bindings
                                        .entry(name.clone())
                                        .or_insert_with(|| (file.clone(), name.clone()));
                                }
                            }
                        }
                        continue;
                    }
                    if ext.language == "swift" {
                        // `import Kit` puts Kit's file-level names in
                        // unqualified scope. Only file-level declarations —
                        // the same population `swift_module_symbols` holds —
                        // so a method of some type cannot become a bare callee.
                        if let Some(module) = self.swift_module_named_by(&imp.module_specifier) {
                            for ((owner, name), decls) in &self.swift_module_symbols {
                                if owner != module {
                                    continue;
                                }
                                let others: Vec<_> = decls
                                    .iter()
                                    .filter(|(path, symbol, _)| {
                                        path != &ext.file_path
                                            && self.candidate_is_visible_from(
                                                LangFamily::Swift,
                                                &ext.file_path,
                                                path,
                                                symbol,
                                            )
                                    })
                                    .collect();
                                if others.len() != 1 {
                                    continue;
                                }
                                let (file, _, _) = others[0];
                                // The importing file's own module shadows what an
                                // import brings in under the same name.
                                if self.own_module_declares(&ext.file_path, name) {
                                    continue;
                                }
                                file_bindings
                                    .entry(name.clone())
                                    .or_insert_with(|| (file.clone(), name.clone()));
                            }
                        }
                    }
                    let local = alias.map(str::to_string).unwrap_or_else(|| {
                        Self::import_local_name(&ext.language, &imp.module_specifier)
                    });
                    // A JS/TS `import X from './m'` binds whatever `./m`
                    // publishes as its default, under whatever name `./m` gave
                    // it — not a symbol called `X`.
                    //
                    // A Kotlin `import a.b.format as fmt` binds `fmt`, but the
                    // declaration it names is `format`: the binding's second
                    // half is what the target file declares.
                    let binding = if LangFamily::from_lang(&ext.language) == LangFamily::JsTs {
                        self.default_binding(&target_f, &local)
                    } else if ext.language == "kotlin" && alias.is_some() {
                        (
                            target_f,
                            Self::import_local_name(&ext.language, &imp.module_specifier),
                        )
                    } else {
                        (target_f, local.clone())
                    };
                    if let Some(scope) = &nested_python {
                        // `import a.b` binds the package `a`, the same package a
                        // module-level `import a` binds, not the module the flat
                        // table files under `b`. The scope's `a` is that package
                        // (none when it names no indexed file), and the flat
                        // table gains nothing for `b`.
                        let root = imp.module_specifier.split('.').next().unwrap_or("");
                        if alias.is_none() && root != local {
                            let package = self
                                .resolve_import_path(&ext.file_path, &ext.language, root)
                                .map(|file| (file, root.to_string()));
                            note_scoped(scope, root, package);
                        } else {
                            note_scoped(scope, &local, Some(binding.clone()));
                            file_bindings.entry(local).or_insert(binding);
                        }
                    } else {
                        module_imported.insert(local.clone());
                        file_bindings.insert(local, binding);
                    }
                }
            }
            // A name only functions bind, and bind to different things, has no
            // one binding in the flat table. A name the module also binds is
            // not a conflict: a module-scope read is the module's, and a read
            // in a function asks the scope walk first.
            let mut targets_by_name: BTreeMap<&str, BTreeSet<&Option<(String, String)>>> =
                BTreeMap::new();
            for names in file_scoped.values() {
                for (name, bound) in names {
                    targets_by_name.entry(name).or_default().insert(bound);
                }
            }
            for (name, targets) in targets_by_name {
                if targets.len() > 1 && !module_imported.contains(name) {
                    self.python_ambiguous_scoped_imports
                        .insert((ext.file_path.clone(), name.to_string()));
                }
            }
            for (scope, names) in file_scoped {
                self.python_scoped_import_bindings
                    .insert((ext.file_path.clone(), scope), names);
            }
            if !file_external.is_empty() {
                self.external_imports
                    .insert(ext.file_path.clone(), file_external);
            }
            if !file_external_roots.is_empty() {
                self.external_module_roots
                    .insert(ext.file_path.clone(), file_external_roots);
            }
            if !file_local_roots.is_empty() {
                self.local_module_roots
                    .insert(ext.file_path.clone(), file_local_roots);
            }
            if !file_local_gap.is_empty() {
                self.unindexed_local_imports
                    .insert(ext.file_path.clone(), file_local_gap);
            }
            for (local, declared) in file_values {
                // A later whole-module import of the same local name replaces
                // the binding, and with it the claim that the name is a value.
                let still_value = file_bindings
                    .get(&local)
                    .is_some_and(|(target, _): &(String, String)| target == &declared.0);
                if still_value {
                    self.value_imports
                        .insert((ext.file_path.clone(), local), declared);
                }
            }
            if !file_bindings.is_empty() {
                self.import_bindings
                    .insert(ext.file_path.clone(), file_bindings);
            }
            if !file_path_modules.is_empty() {
                self.path_module_bindings
                    .insert(ext.file_path.clone(), file_path_modules);
            }

            let family = LangFamily::from_lang(&ext.language);
            if family == LangFamily::Python {
                let binds = pytypes::python_import_binds(ext);
                if !binds.is_empty() {
                    self.python_import_binds
                        .insert(ext.file_path.clone(), binds);
                }
                if pytypes::python_all_is_dynamic(ext) {
                    self.python_dynamic_all.insert(ext.file_path.clone());
                }
                for callable in pytypes::python_decorated_callables(ext) {
                    self.python_decorated
                        .insert((ext.file_path.clone(), callable));
                }
                for (key, encloser) in pytypes::python_class_enclosers(ext) {
                    self.python_class_enclosers
                        .entry(key)
                        .or_default()
                        .push(encloser);
                }
            }
            // X42. The supertype table, read from the heritage references the
            // extractor emits. `enclosing_symbol` on one of these is the
            // *declaring* type — that is what makes the `Extends` edge have two
            // endpoints — so its tail is the subtype's name and the reference's
            // own name is the supertype's.
            // The span of the previous Python base written for a subtype: the
            // extractor emits one reference per segment of a dotted base
            // (`pkg.Base` is `pkg`, then `Base`) over one span.
            let mut last_python_base: Option<(String, usize, usize)> = None;
            for reference in &ext.references {
                if !matches!(
                    reference.kind,
                    ReferenceKind::Heritage | ReferenceKind::HeritageInterface
                ) {
                    continue;
                }
                let Some(subtype) = reference
                    .enclosing_symbol
                    .as_deref()
                    .filter(|symbol| *symbol != ext.file_path)
                    .and_then(|symbol| symbol.rsplit("::").next())
                    .filter(|subtype| !subtype.is_empty())
                else {
                    continue;
                };
                // A qualified base (`base.Widget`, `crate::m::Widget`) reduces
                // to the bare name, which is the key `type_methods` uses.
                let supertype = reference
                    .name
                    .rsplit("::")
                    .next()
                    .unwrap_or(&reference.name)
                    .rsplit('.')
                    .next()
                    .unwrap_or(&reference.name);
                if family == LangFamily::Python && !reference.name.is_empty() {
                    let bases = self
                        .python_bases
                        .entry((ext.file_path.clone(), subtype.to_string()))
                        .or_default();
                    let span = (
                        subtype.to_string(),
                        reference.span.start_byte,
                        reference.span.end_byte,
                    );
                    match bases.last_mut() {
                        // The member comes first, then the qualifier segments
                        // from the root down (`C`, `a`, `b` for `a.b.C`).
                        Some(last) if last_python_base.as_ref() == Some(&span) => {
                            *last = match last.rsplit_once('.') {
                                Some((qualifier, member)) => {
                                    format!("{qualifier}.{}.{member}", reference.name)
                                }
                                None => format!("{}.{last}", reference.name),
                            };
                        }
                        _ => bases.push(reference.name.clone()),
                    }
                    last_python_base = Some(span);
                }
                // An opaque base names no class: it has no supertype row.
                if supertype.is_empty()
                    || supertype == subtype
                    || (family == LangFamily::Python && reference.name == OPAQUE_BASE)
                {
                    continue;
                }
                self.supertypes
                    .entry((family, subtype.to_string()))
                    .or_default()
                    .insert(supertype.to_string());
                self.subtypes
                    .entry((family, supertype.to_string()))
                    .or_default()
                    .insert(subtype.to_string());
            }
            for reference in &ext.references {
                let Some(receiver) = &reference.assigned_to else {
                    continue;
                };
                // `self.engine = Engine()`: the extractor spells a Python field
                // target `self.<name>`, which is no local's name, so it never
                // reaches the receiver maps below. It is the owning class's
                // initializer for that field, read by `field_initializer_type`.
                if family == LangFamily::Python {
                    if let Some(field) = receiver.strip_prefix("self.") {
                        self.record_python_field_initializer(ext, reference, field);
                        continue;
                    }
                }
                // A Kotlin property initialized by a factory call names no type,
                // but the factory's declaration does. `lazy` itself is the
                // delegate, not the value.
                if family == LangFamily::Kotlin
                    && reference.kind == ReferenceKind::Call
                    && reference.name != "lazy"
                {
                    if let (Some(scope), Some(shape)) = (
                        reference.enclosing_symbol.as_deref(),
                        reference.initializer_shape(&ext.calls),
                    ) {
                        self.field_initializers
                            .entry(format!("{}:{}:{}", ext.file_path, scope, receiver))
                            .or_insert(shape);
                    }
                }
                // SC25: record what the author declared *before* asking whether
                // it is indexed. A `*testing.T` parameter names a type this
                // corpus will never contain, which is exactly what makes it
                // recognisable as external — the dispatch map below drops it
                // for the same reason it is useful here.
                if matches!(
                    reference.kind,
                    ReferenceKind::Type | ReferenceKind::TypeQualifier
                ) && !reference.name.is_empty()
                {
                    // Qualifiers are namespaced apart from bare type names so a
                    // package called `Foo` and a type called `Foo` cannot
                    // overwrite one another.
                    let slot = if reference.kind == ReferenceKind::TypeQualifier {
                        "@mod"
                    } else {
                        "@type"
                    };
                    let scope = reference.enclosing_symbol.as_deref();
                    if let Some(scope) = scope {
                        Self::bind_receiver(
                            &mut self.declared_types,
                            &mut self.poisoned_receiver_keys,
                            format!("{}:{}:{}{}", ext.file_path, scope, receiver, slot),
                            &reference.name,
                        );
                    }
                    Self::bind_receiver(
                        &mut self.declared_types,
                        &mut self.poisoned_receiver_keys,
                        format!("{}:{}{}", ext.file_path, receiver, slot),
                        &reference.name,
                    );
                }
                let Some(candidates) = self.symbol_index.get(&reference.name) else {
                    continue;
                };
                let types: Vec<_> = candidates
                    .iter()
                    .filter(|(path, kind, candidate_family, identity)| {
                        family.admits(*candidate_family)
                            && matches!(kind, SymbolKind::Class | SymbolKind::Struct)
                            // Swift: a `private`/`fileprivate` type of another
                            // file is not a type this file can write, so it
                            // does not make the name ambiguous here.
                            && (family != LangFamily::Swift
                                || self.candidate_is_visible_from(
                                    family,
                                    &ext.file_path,
                                    path,
                                    identity,
                                ))
                    })
                    .map(|(path, kind, _, _)| (path, kind))
                    .collect();
                // Python says which class a name is — this file's own `class`
                // statement, else the import that bound it — so a name more
                // than one file declares still types the receiver; the lookup
                // at the call restricts to that class's file.
                // `K.A()` with `K` a class this file declares reads `A` off the
                // class: a class attribute, not the class `A` the name alone
                // would name.
                //
                // `K` is that class only where the reading scope does not
                // rebind it (`import m as K` inside the function is a module),
                // and when `K` declares a nested `class A` the attribute is
                // that class, which the unique-name rule below may still bind.
                let local_class_qualifier = family == LangFamily::Python
                    && reference
                        .receiver_expr
                        .clone()
                        .or_else(|| {
                            reference.initializer_shape(&ext.calls).and_then(|shape| {
                                shape
                                    .rsplit_once('.')
                                    .map(|(qualifier, _)| qualifier.to_string())
                            })
                        })
                        .as_deref()
                        .is_some_and(|qualifier| {
                            let root = qualifier.split('.').next().unwrap_or(qualifier);
                            let declares_class = |name: &str| {
                                self.symbol_index.get(name).into_iter().flatten().any(
                                    |(path, kind, candidate_family, _)| {
                                        *path == ext.file_path
                                            && *kind == SymbolKind::Class
                                            && *candidate_family == LangFamily::Python
                                    },
                                )
                            };
                            declares_class(root)
                                && !reference.enclosing_symbol.as_deref().is_some_and(|scope| {
                                    self.python_scope_lookup(&ext.file_path, scope, root, 0)
                                        .is_some()
                                })
                        });
                let nested_class_attribute = local_class_qualifier
                    && ext.symbols.iter().any(|nested| {
                        nested.name == reference.name
                            && nested.kind == SymbolKind::Class
                            // The extractor files a class written in a class under
                            // the file; its span says which class it sits in.
                            && ext.symbols.iter().any(|owner| {
                                owner.kind == SymbolKind::Class
                                    && owner.name != nested.name
                                    && owner.span.start_byte <= nested.span.start_byte
                                    && nested.span.end_byte <= owner.span.end_byte
                            })
                    });
                let class_attribute = local_class_qualifier && !nested_class_attribute;
                let python_names_one_class = family == LangFamily::Python
                    && !local_class_qualifier
                    && self
                        .python_class_named(
                            &ext.file_path,
                            reference.enclosing_symbol.as_deref(),
                            &reference.name,
                        )
                        .is_some();
                if (types.len() == 1 && !class_attribute) || python_names_one_class {
                    if let Some(scope) = reference.enclosing_symbol.as_deref() {
                        Self::bind_receiver(
                            &mut self.scoped_receiver_types,
                            &mut self.poisoned_receiver_keys,
                            format!("{}:{}:{}", ext.file_path, scope, receiver),
                            &reference.name,
                        );
                    }
                    Self::bind_receiver(
                        &mut self.receiver_types,
                        &mut self.poisoned_receiver_keys,
                        format!("{}:{}", ext.file_path, receiver),
                        &reference.name,
                    );
                }
            }
            self.index_module_value_types(ext, family);
            self.index_js_objects(ext);
            // A `let x = self.field` (including through `if` / `&`) takes the
            // field's type, so `x.run()` can dispatch. Field `Type` references
            // are indexed just above; this pass is a second walk so a field
            // declared later in the file still types an earlier `let`.
            for reference in &ext.references {
                if reference.kind != ReferenceKind::Name {
                    continue;
                }
                let Some(local) = reference
                    .assigned_to
                    .as_deref()
                    .filter(|name| !name.is_empty())
                else {
                    continue;
                };
                if !reference
                    .receiver_expr
                    .as_deref()
                    .is_some_and(Self::receiver_is_self)
                {
                    continue;
                }
                let Some(caller) = reference.enclosing_symbol.as_deref() else {
                    continue;
                };
                let Some(type_name) = self
                    .declaring_type_of(&ext.file_path, caller)
                    .map(str::to_string)
                else {
                    continue;
                };
                let field = reference.name.as_str();
                let file = ext.file_path.as_str();
                let field_type = [
                    format!("{file}:{file}::{type_name}:{field}"),
                    format!("{file}:{type_name}:{field}"),
                ]
                .into_iter()
                .find_map(|key| self.scoped_receiver_types.get(&key).cloned());
                let Some(field_type) = field_type else {
                    continue;
                };
                Self::bind_receiver(
                    &mut self.scoped_receiver_types,
                    &mut self.poisoned_receiver_keys,
                    format!("{file}:{caller}:{local}"),
                    &field_type,
                );
            }
        }
    }

    /// Resolve every file.
    ///
    /// There is deliberately **no subset entry point**. One existed — a
    /// `resolve_subset(extractions, only)` that narrowed the emission loop to a
    /// changed-file closure — and it was unsound for a reason that is not
    /// fixable by narrowing more carefully: `analyze` consumes whatever this
    /// produces, and liveness and community detection are global by nature.
    /// They answer "does anything call this symbol" and "what clusters with
    /// what", questions no subset of the edges can answer. Measured on a
    /// 155-file fixture, one edited file handed the analyser 63 edges instead
    /// of 15,017, and the generation committed 433 dead-code candidates instead
    /// of 14 — plainly-called functions recorded as callerless.
    ///
    /// `devmap-cli` reverted to a whole-tree resolve on every build and the
    /// parameter has had no caller since. It is gone rather than left
    /// unreachable, because an unused narrowing hook reads as a supported
    /// option and the next caller would reintroduce the same bug.
    /// How deep a barrel may nest before this stops following it.
    ///
    /// A `packages/*/index.ts` re-exporting a `src/index.ts` re-exporting a
    /// feature barrel is three; anything past this is either generated or a
    /// mistake, and following it without a bound turns a malformed tree into a
    /// hang. Refusing is the honest outcome — the chain is recorded only when
    /// it terminates.
    pub(super) const REEXPORT_CHAIN_MAX_DEPTH: usize = 8;

    /// Most hops the whole glob expansion may hold.
    ///
    /// One glob over a file declaring N symbols adds N hops, and a crate root
    /// globbing a dozen modules multiplies that — so the work is a product of
    /// two corpus dimensions rather than a walk over one, and a generated tree
    /// is exactly where it gets large. Past this the expansion stops and the
    /// chains it would have built are simply absent, which is the answer the
    /// map gave before globs were expanded at all: a lost chain costs
    /// precision, never a wrong edge. A lost chain is still a loss, so the stop
    /// is recorded: [`Self::reexport_glob_cutoff`] reports it.
    pub(super) const REEXPORT_GLOB_MAX_HOPS: usize = 200_000;

    /// Turn each glob re-export into one hop per name the target publishes.
    ///
    /// What a target publishes is what it *declares* plus what it already
    /// re-exports, and the second is why this runs to a fixpoint: a crate root
    /// globbing a module that itself globs only learns the inner names once the
    /// inner hops exist. The round count is [`REEXPORT_CHAIN_MAX_DEPTH`], the
    /// same bound the chain walk uses and for the same reason.
    ///
    /// **Two names it refuses to invent.** An explicit re-export already states
    /// where a name comes from and is never overwritten — which is also the
    /// language's own rule, since a named `pub use` shadows a glob covering it.
    /// And a name that *two* globs bring in from two different files has no
    /// single true source — Rust rejects such a program outright — so it is
    /// recorded from neither, on the same rule the cycle follows: picking one
    /// endpoint would manufacture the certainty the map exists to supply. That
    /// holds across rounds too: a hop a glob swept in on an earlier round is
    /// reconsidered on every later one, and withdrawn — with every swept hop
    /// built on it — when a second glob's target comes to publish the name.
    ///
    /// Returns the cutoff when the expansion stopped at `max_hops`, so the
    /// partial map is never mistaken for a complete one.
    pub(super) fn expand_glob_hops(
        hops: &mut BTreeMap<String, (String, String)>,
        globs: &BTreeSet<(String, String)>,
        file_symbols: &BTreeMap<String, Vec<String>>,
        max_hops: usize,
    ) -> Option<ReexportGlobCutoff> {
        if globs.is_empty() {
            return None;
        }

        // Keys this expansion inserted. Only these are ever reconsidered: an
        // explicit re-export is the author's statement and outranks every glob.
        let mut swept: BTreeSet<String> = BTreeSet::new();
        let mut poisoned: BTreeSet<String> = BTreeSet::new();
        let mut cutoff = None;
        for _ in 0..Self::REEXPORT_CHAIN_MAX_DEPTH {
            let (additions, conflicts) = {
                // What each file publishes as of this round.
                let mut published: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
                for (file, names) in file_symbols {
                    published
                        .entry(file.as_str())
                        .or_default()
                        .extend(names.iter().map(String::as_str));
                }
                for key in hops.keys() {
                    if let Some((file, name)) = key.rsplit_once("::") {
                        if name != "*" {
                            published.entry(file).or_default().insert(name);
                        }
                    }
                }

                let mut proposals: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
                for (from, target) in globs {
                    let Some(names) = published.get(target.as_str()) else {
                        continue;
                    };
                    // Every file carries a `SymbolKind::File` container symbol
                    // named after its own basename, and it is marked exported
                    // like any other. A glob republishes the module's *names*,
                    // not the file object, so sweeping it in produced the
                    // nonsense chain `a.rs::b.rs` -> `b.rs::b.rs`. Identified by
                    // the basename because that is exactly how the extractor
                    // names it, and no language whose files carry an extension
                    // can declare a real symbol with a dot in it.
                    let container = target.rsplit('/').next().unwrap_or(target.as_str());
                    for name in names.iter().filter(|name| **name != container) {
                        let key = format!("{from}::{name}");
                        if poisoned.contains(&key) {
                            continue;
                        }
                        match hops.get(&key) {
                            Some(_) if !swept.contains(&key) => continue,
                            // Taken on an earlier round. It stays a candidate
                            // beside this one, because a glob whose target only
                            // publishes the name after its *own* expansion is a
                            // second source the earlier round could not see.
                            Some((recorded, _)) => {
                                let sources = proposals.entry(key).or_default();
                                sources.insert(recorded.as_str());
                                sources.insert(target.as_str());
                            }
                            None => {
                                proposals.entry(key).or_default().insert(target.as_str());
                            }
                        }
                    }
                }

                let mut additions: Vec<(String, (String, String))> = Vec::new();
                let mut conflicts: Vec<String> = Vec::new();
                for (key, targets) in proposals {
                    if targets.len() > 1 {
                        conflicts.push(key);
                        continue;
                    }
                    if hops.contains_key(&key) {
                        // Already recorded, from this same source.
                        continue;
                    }
                    let (Some((_, name)), Some(first)) =
                        (key.rsplit_once("::"), targets.into_iter().next())
                    else {
                        continue;
                    };
                    let hop = (first.to_string(), name.to_string());
                    additions.push((key, hop));
                }
                (additions, conflicts)
            };

            if additions.is_empty() && conflicts.is_empty() {
                break;
            }
            let total = additions.len();
            for (inserted, (key, hop)) in additions.into_iter().enumerate() {
                if hops.len() >= max_hops {
                    cutoff = Some(ReexportGlobCutoff {
                        limit: max_hops,
                        dropped_at_least: total - inserted,
                    });
                    break;
                }
                swept.insert(key.clone());
                hops.insert(key, hop);
            }
            // Withdrawn after this round's additions, not before, so a hop
            // swept in *through* a conflicting key on this same round is found
            // and withdrawn with it.
            Self::poison_glob_hops(hops, &mut swept, &mut poisoned, conflicts);
            if cutoff.is_some() {
                break;
            }
        }
        cutoff
    }

    /// Withdraw each conflicting key and every swept hop built on it.
    ///
    /// A name two globs publish has no source, so the file holding it does not
    /// publish it either — and a later round's glob over that file swept in a
    /// hop naming a source that no longer exists. Those are withdrawn and
    /// poisoned too: the language rejects a use of the name through them for
    /// the same ambiguity, and leaving them would let them keep publishing it.
    pub(super) fn poison_glob_hops(
        hops: &mut BTreeMap<String, (String, String)>,
        swept: &mut BTreeSet<String>,
        poisoned: &mut BTreeSet<String>,
        conflicts: Vec<String>,
    ) {
        if conflicts.is_empty() {
            return;
        }
        // `"<file>::<name>"` a swept hop asks for -> the swept keys asking.
        // Built once per call, so the cascade is linear in the swept hops
        // rather than a scan of them per withdrawn key.
        let mut dependents: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for key in swept.iter() {
            if let Some((file, name)) = hops.get(key) {
                dependents
                    .entry(format!("{file}::{name}"))
                    .or_default()
                    .push(key.clone());
            }
        }
        let mut queue = conflicts;
        while let Some(key) = queue.pop() {
            if !poisoned.insert(key.clone()) {
                continue;
            }
            swept.remove(&key);
            hops.remove(&key);
            if let Some(downstream) = dependents.remove(&key) {
                queue.extend(downstream);
            }
        }
    }

    /// `<file>::<exported name>` for every `export { x } from './m'`, followed
    /// to the file that actually declares the name.
    ///
    /// **What this is evidence of.** A barrel does not merely suggest where a
    /// name comes from; it states it. `export { thing } from './impl'` is a
    /// fact about `thing` that the resolver had in hand and threw away: the
    /// import binding pointed at `index.ts`, `index.ts` declares no `thing`, so
    /// the ladder fell through to the bare-name global lookup — and in a
    /// repository where two files declare `thing`, that produced an
    /// `AmbiguousGlobal` fan-out to both at confidence 0.2, one of which is an
    /// edge to a function the caller demonstrably does not call.
    ///
    /// Measured on the three-file fixture in `reexport_chains.rs`: two `Calls`
    /// edges where one is correct, and the barrel names which.
    ///
    /// **Terminal, not one hop.** The value is the file that declares the name,
    /// after following nested barrels, so a consumer reads one entry rather
    /// than walking the map itself and re-deriving the depth cap.
    ///
    /// A cycle — `a.ts` re-exporting from `b.ts` re-exporting from `a.ts` —
    /// yields no entry at all. There is no terminal file, so there is nothing
    /// true to record, and recording either endpoint would invent one.
    /// A Python module's own `from m import name` is a re-export hop.
    ///
    /// Python has no export statement: every name a module binds at module
    /// scope is an attribute of that module, and `from barrel import name`
    /// elsewhere reads it. So `barrel.py: from impl import normalise` states
    /// where `barrel.normalise` comes from as plainly as `export { normalise }
    /// from './impl'` does — and the chain never saw it, because only
    /// `ext.exports` fed it. The importer's binding pointed at `barrel.py`,
    /// which declares nothing, and `impl.normalise` was reported dead at 0.4
    /// with its caller one file away. The same shape is every package
    /// `__init__.py` that lifts a name out of a submodule.
    ///
    /// Module scope only: an import inside a function or class body binds a
    /// local or a class attribute, never a module attribute. An explicit hop
    /// for the same name — there is none in Python today — is never displaced.
    pub(super) fn python_import_hops(
        &self,
        ext: &Extraction,
        hops: &mut BTreeMap<String, (String, String)>,
    ) {
        let enclosed = |span: &Span| {
            ext.symbols.iter().any(|symbol| {
                matches!(
                    symbol.kind,
                    SymbolKind::Function | SymbolKind::Method | SymbolKind::Class
                ) && symbol.span.start_byte <= span.start_byte
                    && span.end_byte <= symbol.span.end_byte
            })
        };
        for imp in &ext.imports {
            if imp.path_load.is_some() || imp.imported_names.is_empty() || enclosed(&imp.span) {
                continue;
            }
            for (index, name) in imp.imported_names.iter().enumerate() {
                if name.is_empty() || name == "*" {
                    continue;
                }
                let local = imp
                    .local_names
                    .get(index)
                    .filter(|local| !local.is_empty())
                    .unwrap_or(name);
                let spec = Self::import_spec_for_name(&imp.module_specifier, name);
                let Some(target) = self.resolve_import_path(&ext.file_path, &ext.language, &spec)
                else {
                    continue;
                };
                hops.entry(format!("{}::{local}", ext.file_path))
                    .or_insert((target, name.clone()));
            }
        }
    }

    /// Whether `name` is bound in `file` by an import whose module is **not in
    /// this tree**: a package (`import { formatTitle } from 'some-npm-lib'`) or
    /// a relative specifier that names no indexed file.
    ///
    /// The file states where the name comes from, so a repository declaration
    /// that merely shares the spelling is no evidence about it. Letting the
    /// global tier answer anyway bound an npm import to a local namesake at 0.9
    /// confidence — 658 of 1,042 JS/TS global-tier edges on the audit corpus
    /// were of exactly this shape, and none of the 16 sampled was right. The
    /// site is left to `classify_unresolved`, which files it `external` or
    /// `unresolved` by the same two tables.
    ///
    /// Every family, because both ladders read it through one owner,
    /// [`Self::bare_name_has_other_evidence`]. It was JS/TS only in the reference
    /// ladder while the call ladder applied it to all, which let `from typing
    /// import Literal` bind a type reference to an in-repo `class Literal`.
    ///
    /// A wildcard import is evidence about *every* bare name the file uses: a
    /// non-indexed `from numpy import *` is where a bare `zeros` may come from,
    /// so it counts under [`STAR_IMPORT`] exactly as a named import does.
    pub(super) fn names_a_non_local_import(&self, file: &str, name: &str) -> bool {
        let names = |imports: &BTreeMap<String, BTreeMap<String, String>>| {
            imports.get(file).is_some_and(|imports| {
                imports.contains_key(name) || imports.contains_key(STAR_IMPORT)
            })
        };
        names(&self.external_imports) || names(&self.unindexed_local_imports)
    }

    /// The `(file, declared name)` a default import of `target` binds.
    ///
    /// The module's own `export default <local>` when it has one; else the
    /// barrel's `export { default } from` / `export { default as X } from`
    /// chain to the file that declares it; else the importer's own spelling,
    /// which is right whenever the default export is named after its binding
    /// and is what this answered before default exports were recorded.
    pub(super) fn default_binding(&self, target: &str, local: &str) -> (String, String) {
        if let Some(declared) = self.default_exports.get(target) {
            return (target.to_string(), declared.clone());
        }
        if let Some((file, declared)) = self
            .reexport_chains
            .get(&format!("{target}::default"))
            .and_then(|terminal| terminal.rsplit_once("::"))
        {
            return (file.to_string(), declared.to_string());
        }
        (target.to_string(), local.to_string())
    }

    /// One hop for each JS/TS local export that publishes an imported binding:
    /// `import { x } from './m'; export { x };` and `import X from './m';
    /// export { X };`. The export names the importing file's own binding and
    /// the import says where it came from, which is all a barrel's
    /// `export { x } from './m'` states in one statement.
    pub(super) fn js_import_reexport_hops(
        &self,
        ext: &Extraction,
        hops: &mut BTreeMap<String, (String, String)>,
    ) {
        for export in &ext.exports {
            if export.module_specifier.is_some() || export.exported_name.is_empty() {
                continue;
            }
            let Some(local) = export
                .local_name
                .as_deref()
                .filter(|local| !local.is_empty() && *local != "default")
            else {
                continue;
            };
            for imp in &ext.imports {
                let imported = if imp.imported_names.is_empty() {
                    // A default import: `import X from './m'`.
                    (imp.alias.as_deref() == Some(local)).then(|| "default".to_string())
                } else {
                    imp.imported_names
                        .iter()
                        .zip(&imp.local_names)
                        .find(|(_, bound)| bound.as_str() == local)
                        .map(|(name, _)| name.clone())
                };
                let Some(imported) = imported.filter(|name| name != "*") else {
                    continue;
                };
                if let Some(target) =
                    self.resolve_import_path(&ext.file_path, &ext.language, &imp.module_specifier)
                {
                    hops.entry(format!("{}::{}", ext.file_path, export.exported_name))
                        .or_insert((target, imported));
                    break;
                }
            }
        }
    }

    /// Every Rust `pub use m::*;` as `(file, the one module file m names)`.
    pub(super) fn compute_rust_module_globs(
        &self,
        extractions: &[Extraction],
    ) -> BTreeMap<String, Vec<String>> {
        let mut globs: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for ext in extractions
            .iter()
            .filter(|ext| LangFamily::from_lang(&ext.language) == LangFamily::Rust)
        {
            for export in ext
                .exports
                .iter()
                .filter(|export| export.exported_name == "*")
            {
                let Some(specifier) = export.module_specifier.as_deref() else {
                    continue;
                };
                if let Some(target) =
                    self.resolve_import_path(&ext.file_path, &ext.language, specifier)
                {
                    globs.entry(ext.file_path.clone()).or_default().push(target);
                }
            }
        }
        globs
    }

    pub(super) fn compute_reexport_chains(
        &self,
        extractions: &[Extraction],
    ) -> (BTreeMap<String, String>, Option<ReexportGlobCutoff>) {
        // One hop per re-export, keyed by the re-exporting file's own name for
        // the symbol.
        let mut hops: BTreeMap<String, (String, String)> = BTreeMap::new();
        // `(globbing file, target file)` for every `export * from` /
        // `pub use m::*`. Kept apart from `hops` because a glob names no symbol:
        // keyed `<file>::*` there, a file's second glob overwrote its first and
        // only the last one was ever expanded.
        let mut globs: BTreeSet<(String, String)> = BTreeSet::new();
        for ext in extractions {
            for export in &ext.exports {
                let Some(specifier) = export.module_specifier.as_deref() else {
                    continue;
                };
                if export.exported_name.is_empty() {
                    continue;
                }
                let Some(target) =
                    self.resolve_import_path(&ext.file_path, &ext.language, specifier)
                else {
                    // The specifier named no indexed file. Recorded nowhere:
                    // this is the same index gap `UnresolvedKind::Import`
                    // already reports, and inventing a chain endpoint for it
                    // would manufacture graph structure.
                    continue;
                };
                if export.exported_name == "*" {
                    globs.insert((ext.file_path.clone(), target));
                    continue;
                }
                // `export { a as b } from './m'` publishes `b` and asks `./m`
                // for `a`. Defaulting to the exported name covers the common
                // `export { a } from './m'`, where the two are equal.
                let source_name = export
                    .local_name
                    .clone()
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| export.exported_name.clone());
                hops.insert(
                    format!("{}::{}", ext.file_path, export.exported_name),
                    (target, source_name),
                );
            }
            if ext.language == "python" {
                self.python_import_hops(ext, &mut hops);
            }
            if LangFamily::from_lang(&ext.language) == LangFamily::JsTs {
                self.js_import_reexport_hops(ext, &mut hops);
            }
        }

        // A glob names a *module*, not a symbol, so the hop it states —
        // `index.js::*` pointing at `("impl.js", "*")` — can never terminate:
        // no file declares a symbol called `*`. Every `export * from` chain was
        // dropped in silence for exactly that reason, and Rust's `pub use m::*`
        // arrives with the same shape.
        //
        // The expansion cannot happen in the extractor, which sees one file at
        // a time while a glob names whatever *another* file declares. Here the
        // whole-corpus symbol table is in hand.
        let cutoff = Self::expand_glob_hops(
            &mut hops,
            &globs,
            &self.file_symbols,
            Self::REEXPORT_GLOB_MAX_HOPS,
        );

        let mut chains = BTreeMap::new();
        for key in hops.keys() {
            let mut seen: BTreeSet<String> = BTreeSet::new();
            let mut cursor = key.clone();
            let mut terminal = None;
            for _ in 0..Self::REEXPORT_CHAIN_MAX_DEPTH {
                if !seen.insert(cursor.clone()) {
                    // A cycle. Abandon the whole chain rather than recording
                    // the last node visited, which would be an answer the tree
                    // does not support.
                    terminal = None;
                    break;
                }
                let Some((file, name)) = hops.get(&cursor) else {
                    break;
                };
                // A hop asking for `default` asks for what the file's own
                // `export default` publishes.
                let name = if name == "default" {
                    self.default_exports.get(file).unwrap_or(name)
                } else {
                    name
                };
                let next = format!("{file}::{name}");
                // The file this hop names declares the symbol itself, so the
                // walk is over and this is the answer.
                if self
                    .file_symbols
                    .get(file)
                    .is_some_and(|symbols| symbols.iter().any(|symbol| symbol == name))
                {
                    terminal = Some(next);
                    break;
                }
                cursor = next;
            }
            if let Some(terminal) = terminal {
                if terminal != *key {
                    chains.insert(key.clone(), terminal);
                }
            }
        }
        (chains, cutoff)
    }
}
