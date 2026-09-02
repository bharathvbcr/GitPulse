use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use devmap_extract::model::*;

use crate::model::*;
use devmap_extract::GoModule;

pub struct Resolver {
    symbol_index: BTreeMap<String, Vec<(String, SymbolKind, LangFamily)>>,
    file_symbols: BTreeMap<String, Vec<String>>, // file_path -> symbol_names
    receiver_types: BTreeMap<String, String>,    // (file_path:var_name) -> ClassType
    /// (file_path:enclosing_symbol:var_name) -> ClassType.
    ///
    /// A receiver variable belongs to one method, not to a whole file. Keying
    /// only by file let `func (s *A)` and `func (s *B)` in one file collide
    /// (SC9). Both maps refuse to answer once a key is claimed by two types.
    scoped_receiver_types: BTreeMap<String, String>,
    /// Keys claimed by more than one type, in either map. Never resolved:
    /// abstaining is correct, answering with the winner of a race is not.
    poisoned_receiver_keys: BTreeSet<String>,
    type_methods: BTreeMap<(LangFamily, String, String), Vec<(String, String)>>,
    /// Per-file local import name → (target file, exported symbol) for import-scoped calls (G6).
    import_bindings: BTreeMap<String, BTreeMap<String, (String, String)>>,
    /// `file:scope:var` and `file:var` → the *declared* type name of a value,
    /// whether or not that type is indexed (SC25).
    ///
    /// Distinct from `receiver_types`, which only records a binding when the
    /// type resolves to exactly one indexed `Class`/`Struct` — it exists to
    /// dispatch a method onto a known symbol, so an unindexed type is correctly
    /// no use to it. This map answers the opposite question: *what did the
    /// author write*, so a receiver typed by an external package can be
    /// recognised even though nothing about it is indexable.
    declared_types: BTreeMap<String, String>,
    /// Per-file local import name → module specifier, for imports whose module
    /// resolved to **no indexed file** (SC18).
    ///
    /// This is the evidence that separates "comes from outside the corpus" from
    /// "we failed to resolve it". `strings.TrimSpace` and `useState` are not
    /// resolution defects — the `strings` and `react` imports prove the corpus
    /// never contained them. Populated alongside `import_bindings` from the same
    /// walk, so a specifier lands in exactly one of the two maps.
    external_imports: BTreeMap<String, BTreeMap<String, String>>,
    /// (file, bare symbol name) → qualified name. Edge endpoints are graph
    /// identities, not bare words: emitting `open` instead of `app.py::open`
    /// makes an edge unjoinable to the node it names.
    qualified_names: BTreeMap<(String, String), String>,
    go_modules: Vec<GoModule>,
    /// file_path → Go package identifier (`pkg` in `package pkg`).
    go_package_by_file: BTreeMap<String, String>,
    /// `(file, scope, name)` for every value a callable binds itself.
    ///
    /// `declared_types` can only answer for a binding that carries a *written
    /// type* — a Rust or Go signature — so the `LocalBinding` tier saw four
    /// calls on a repository that has roughly fifty. An untyped Python `cls`,
    /// a `let handler = |…|` invoked below its own definition, a locally bound
    /// helper: all genuinely local, all landing in the tier that means "possible
    /// defect". The extractor already computes the set per scope; this is the
    /// same fact, indexed for lookup.
    ///
    /// A set rather than a map, because the question is membership: this scope
    /// binds this name. Nothing here claims to know the value's *type*, so it
    /// can never feed dispatch, and no confidently-wrong edge can come out of it.
    scope_locals: BTreeSet<(String, String, String)>,
}

impl Default for Resolver {
    fn default() -> Self {
        Self::new()
    }
}

impl Resolver {
    pub fn new() -> Self {
        Self {
            symbol_index: BTreeMap::new(),
            file_symbols: BTreeMap::new(),
            receiver_types: BTreeMap::new(),
            scoped_receiver_types: BTreeMap::new(),
            poisoned_receiver_keys: BTreeSet::new(),
            type_methods: BTreeMap::new(),
            import_bindings: BTreeMap::new(),
            declared_types: BTreeMap::new(),
            external_imports: BTreeMap::new(),
            qualified_names: BTreeMap::new(),
            go_modules: Vec::new(),
            go_package_by_file: BTreeMap::new(),
            scope_locals: BTreeSet::new(),
        }
    }

    /// Record `key -> type_name`, poisoning the key if a second type claims it.
    ///
    /// Last-write-wins here is what made SC9 a *confidently wrong* answer rather
    /// than a missing one: the loser of the race still got resolved, at
    /// DETERMINISTIC confidence, to the wrong type.
    fn bind_receiver(
        map: &mut BTreeMap<String, String>,
        poisoned: &mut BTreeSet<String>,
        key: String,
        type_name: &str,
    ) {
        if poisoned.contains(&key) {
            return;
        }
        match map.get(&key) {
            Some(existing) if existing != type_name => {
                map.remove(&key);
                poisoned.insert(key);
            }
            _ => {
                map.insert(key, type_name.to_string());
            }
        }
    }

    pub fn index_go_modules(&mut self, modules: &[GoModule]) {
        self.go_modules = modules.to_vec();
    }

    /// Resolve a (file, bare name) pair to its graph identity, falling back to
    /// the bare name only when the file genuinely has no such symbol.
    fn qualified_for(&self, file: &str, name: &str) -> String {
        self.qualified_names
            .get(&(file.to_string(), name.to_string()))
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }

    /// Whether `name` is a value the symbol `enclosing_symbol` declares itself.
    ///
    /// Three binding tables answer this, all keyed by the *scope*:
    ///
    /// - `declared_types` `@type` slots, written from `param_type_bindings` and
    ///   the Go method receiver — so the key exists exactly when this symbol's
    ///   own parameter list (or receiver) declares that name;
    /// - `scoped_receiver_types`, written when this symbol constructs a value
    ///   and binds it to a name;
    /// - `scope_locals`, the extractor's per-scope set of every value the
    ///   callable binds — parameters, `let`/`:=`/`=` targets, loop variables,
    ///   `with … as` handles — with no type attached. The first two can only
    ///   speak for a binding that carries a written type or a resolvable
    ///   constructor, which is why this tier fired on 4 calls out of the ~50
    ///   that are local. It is membership-only by construction: it answers
    ///   *that* the scope binds the name and never *what to*, so it cannot
    ///   contribute a dispatch target and cannot produce a wrong edge.
    ///
    /// **The file-wide maps are deliberately not consulted.** A binding belongs
    /// to one scope. `receiver_types["file:handler"]` may have been written by
    /// a *different* function in the same file, and reading it here would
    /// declare an unrelated bare call "a local, not a defect" on the strength
    /// of someone else's local — which is the SC9 collision and the SC25 leak
    /// in a third place. Both maps also withdraw a key claimed by two types
    /// (`bind_receiver` poisons it), so a scope that binds one name to two
    /// things abstains rather than answering.
    ///
    /// The empty scope is refused explicitly: a call made at file level reports
    /// the file path as its caller, and matching `"{file}::{file}:{name}"` shapes
    /// is not something to leave to string luck.
    fn scope_declares_local(&self, file_path: &str, enclosing_symbol: &str, name: &str) -> bool {
        if enclosing_symbol.is_empty() || enclosing_symbol == file_path {
            return false;
        }
        self.declared_types
            .contains_key(&format!("{file_path}:{enclosing_symbol}:{name}@type"))
            || self
                .scoped_receiver_types
                .contains_key(&format!("{file_path}:{enclosing_symbol}:{name}"))
            || self.scope_locals.contains(&(
                file_path.to_string(),
                enclosing_symbol.to_string(),
                name.to_string(),
            ))
    }

    /// Why a call that failed the resolution ladder has no edge (SC18).
    ///
    /// Ordered by strength of evidence, and **fail-open toward `Unresolved`**:
    /// every rung must prove its claim, and a call that proves nothing keeps the
    /// label that means "this may be a defect". Mislabelling a real failure as
    /// expected is the only outcome here that loses information.
    fn classify_unresolved(
        &self,
        file_path: &str,
        family: LangFamily,
        callee_name: &str,
        receiver: Option<&str>,
        enclosing_symbol: &str,
    ) -> UnresolvedClass {
        // The enclosing scope's own binding beats every wider authority, so it
        // is asked first. A parameter named `len` shadows Go's builtin, and a
        // parameter named `useState` shadows the import: in both cases the call
        // goes to the local value, and answering from the wider table would be
        // the right label for the wrong reason.
        //
        // Only *bare* callees: `x.handler()` is a method on some `x`, and the
        // fact that this scope also binds a `handler` says nothing about it.
        if receiver.is_none() && self.scope_declares_local(file_path, enclosing_symbol, callee_name)
        {
            return UnresolvedClass::LocalBinding;
        }

        // A bare callee that the language itself declares. Checked only without
        // a receiver: `strings.TrimSpace` is library API, and treating a
        // matching method name as a builtin would exempt real calls.
        if receiver.is_none() && crate::builtins::is_builtin(family, callee_name) {
            return UnresolvedClass::Builtin;
        }
        let external = self.external_imports.get(file_path);

        let Some(receiver) = receiver else {
            // `useState()`: the bare name is itself an imported binding.
            if let Some(module) = external.and_then(|imports| imports.get(callee_name)) {
                return UnresolvedClass::External {
                    module: module.clone(),
                };
            }
            // `setTimeout()` / `fetch()`: no import binds it because the
            // runtime puts it on the global object. Checked *after* the import
            // rung on purpose — an explicit `import { fetch } from 'node-fetch'`
            // is file-specific evidence and outranks a global name list.
            return match crate::builtins::host_global_environment(family, callee_name) {
                Some(environment) => UnresolvedClass::HostGlobal {
                    environment: environment.to_string(),
                },
                None => UnresolvedClass::Unresolved,
            };
        };

        // A path receiver is rooted at its leftmost segment, so
        // `metrics.counters.Inc()` is evidence about `metrics` and
        // `std::fs::write()` is evidence about `std`. Both separators are
        // handled because Rust's `scoped_identifier` receivers use `::`.
        let root = receiver
            .split("::")
            .next()
            .unwrap_or(receiver)
            .split('.')
            .next()
            .unwrap_or(receiver);

        if let Some(imports) = external {
            // `strings.TrimSpace()` / `assert.Equal()`: the receiver is the
            // local handle for a module that resolved to no indexed file.
            if let Some(module) = imports.get(root) {
                return UnresolvedClass::External {
                    module: module.clone(),
                };
            }

            // SC25. `t.Fatalf()` where `t` is a `*testing.T`: the receiver is a
            // value, not a module handle, so the lookup above cannot see it.
            // Its *declared type* is the evidence — if that type name is itself
            // an imported binding whose module is outside the corpus, then the
            // method belongs to that module and could never have resolved.
            //
            // The scoped binding is authoritative *as a unit*, across both
            // slots. Falling back per-slot is the SC9 defect wearing a new
            // shape: given `TestOne(t *testing.T)` and `useTracker(t *Tracker)`
            // in one file, `t.RecordMissing()` finds no scoped qualifier —
            // `Tracker` is unqualified — and a per-slot fallback then answers
            // from the file-wide `t -> testing` left by the *other* function,
            // declaring a local type's method external at full confidence.
            //
            // So: if this scope says anything at all about the receiver, only
            // this scope may speak for it.
            let scoped = |slot: &str| {
                self.declared_types
                    .get(&format!("{file_path}:{enclosing_symbol}:{root}{slot}"))
            };
            let scope_knows_receiver = scoped("@mod").is_some() || scoped("@type").is_some();
            let declared = |slot: &str| {
                if scope_knows_receiver {
                    scoped(slot)
                } else {
                    self.declared_types
                        .get(&format!("{file_path}:{root}{slot}"))
                }
            };

            // `t *testing.T`: the type is written with its package, and that
            // package is the import that resolved to nothing.
            if let Some(module) = declared("@mod").and_then(|q| imports.get(q.as_str())) {
                return UnresolvedClass::External {
                    module: module.clone(),
                };
            }
            // `use reqwest::Client; c: &Client`: no qualifier survives at the
            // use site, but the bare type name is itself an imported binding.
            if let Some(module) = declared("@type").and_then(|t| imports.get(t.as_str())) {
                return UnresolvedClass::External {
                    module: module.clone(),
                };
            }
        }

        // A receiver we could not type. Not a defect — naming its owner needs
        // real type inference — but distinct from a bare-name failure, and by
        // far the larger group.
        UnresolvedClass::UninferredReceiver
    }

    /// Return the module path that should be resolved for one imported name.
    /// Python's `from . import module` names the child module in the import
    /// list, while named imports in the other supported languages keep the
    /// module specifier unchanged.
    fn import_spec_for_name(module_specifier: &str, imported_name: &str) -> String {
        if !imported_name.is_empty()
            && imported_name != "*"
            && module_specifier.chars().all(|character| character == '.')
        {
            format!("{module_specifier}{imported_name}")
        } else {
            module_specifier.to_string()
        }
    }

    pub fn index_extractions(&mut self, extractions: &[Extraction]) {
        // The resolver indexes a complete snapshot. Resetting prevents stale
        // candidates and duplicate bindings when a caller reuses the object
        // for a rebuild.
        self.symbol_index.clear();
        self.file_symbols.clear();
        self.receiver_types.clear();
        self.scoped_receiver_types.clear();
        self.poisoned_receiver_keys.clear();
        self.type_methods.clear();
        self.import_bindings.clear();
        self.declared_types.clear();
        self.external_imports.clear();
        self.qualified_names.clear();
        self.go_package_by_file.clear();
        self.scope_locals.clear();

        // Pass one establishes the complete file/symbol universe. Import
        // binding resolution must not depend on whether the importer happens
        // to precede its target in the input slice.
        for ext in extractions {
            let family = LangFamily::from_lang(&ext.language);
            let mut file_syms = Vec::new();
            for sym in &ext.symbols {
                self.symbol_index
                    .entry(sym.name.clone())
                    .or_default()
                    .push((ext.file_path.clone(), sym.kind, family));
                if sym.kind == SymbolKind::Method {
                    if let Some(type_name) = sym
                        .parent_symbol
                        .as_deref()
                        .and_then(|parent| parent.rsplit("::").next())
                    {
                        self.type_methods
                            .entry((family, type_name.to_string(), sym.name.clone()))
                            .or_default()
                            .push((ext.file_path.clone(), sym.qualified_name.clone()));
                    }
                }
                self.symbol_index
                    .entry(sym.qualified_name.clone())
                    .or_default()
                    .push((ext.file_path.clone(), sym.kind, family));
                // First declaration wins, so a duplicate name in one file cannot
                // silently retarget an already-recorded identity.
                self.qualified_names
                    .entry((ext.file_path.clone(), sym.name.clone()))
                    .or_insert_with(|| sym.qualified_name.clone());
                file_syms.push(sym.name.clone());
            }
            self.file_symbols.insert(ext.file_path.clone(), file_syms);
            for (scope, local) in &ext.scope_locals {
                self.scope_locals
                    .insert((ext.file_path.clone(), scope.clone(), local.clone()));
            }
            if let Some(pkg) = ext.go_package.as_deref().filter(|pkg| !pkg.is_empty()) {
                self.go_package_by_file
                    .insert(ext.file_path.clone(), pkg.to_string());
            }
        }

        // Pass two resolves aliases and receiver hints against the complete
        // universe built above.
        for ext in extractions {
            let mut file_bindings = BTreeMap::new();
            // SC18: the mirror of `file_bindings` — every local name whose
            // module resolved to no indexed file. Recorded from the same walk so
            // the two maps cannot disagree about what an import specifier means.
            let mut file_external: BTreeMap<String, String> = BTreeMap::new();
            for imp in &ext.imports {
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
                        let direct = self.resolve_import_path(&ext.file_path, &ext.language, &spec);
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
                        let declares_name = direct
                            .as_deref()
                            .and_then(|file| self.file_symbols.get(file))
                            .is_some_and(|symbols| symbols.iter().any(|symbol| symbol == name));
                        let resolved = if declares_name {
                            direct
                        } else {
                            let submodule = (ext.language == "python").then(|| {
                                self.resolve_import_path(
                                    &ext.file_path,
                                    &ext.language,
                                    &format!("{}.{}", imp.module_specifier, name),
                                )
                            });
                            submodule.flatten().or(direct)
                        };
                        if let Some(target_f) = resolved {
                            file_bindings.insert(local.to_string(), (target_f, name.clone()));
                        } else {
                            file_external.insert(local.to_string(), imp.module_specifier.clone());
                        }
                    }
                } else {
                    let alias = imp.alias.as_deref();
                    if alias == Some("_") {
                        continue;
                    }
                    let targets = self.resolve_import_targets(
                        &ext.file_path,
                        &ext.language,
                        &imp.module_specifier,
                    );
                    let Some(target_f) = targets.first().cloned() else {
                        // A whole-module import that named no indexed file:
                        // `import "strings"`, `import react from "react"`. The
                        // local name is the package handle, so a later
                        // `strings.TrimSpace` can be recognised by its receiver.
                        let local = alias.map(str::to_string).unwrap_or_else(|| {
                            Self::import_local_name(&ext.language, &imp.module_specifier)
                        });
                        file_external.insert(local, imp.module_specifier.clone());
                        continue;
                    };
                    if alias == Some(".") {
                        for file in &targets {
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
                    let local = alias.map(str::to_string).unwrap_or_else(|| {
                        Self::import_local_name(&ext.language, &imp.module_specifier)
                    });
                    file_bindings.insert(local.clone(), (target_f, local));
                }
            }
            if !file_external.is_empty() {
                self.external_imports
                    .insert(ext.file_path.clone(), file_external);
            }
            if !file_bindings.is_empty() {
                self.import_bindings
                    .insert(ext.file_path.clone(), file_bindings);
            }

            let family = LangFamily::from_lang(&ext.language);
            for reference in &ext.references {
                let Some(receiver) = &reference.assigned_to else {
                    continue;
                };
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
                    if let Some(scope) = reference.enclosing_symbol.as_deref() {
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
                    .filter(|(_, kind, candidate_family)| {
                        *candidate_family == family
                            && matches!(kind, SymbolKind::Class | SymbolKind::Struct)
                    })
                    .map(|(path, kind, _)| (path, kind))
                    .collect();
                if types.len() == 1 {
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
        }
    }

    /// Resolve every file. Equivalent to `resolve_subset(extractions, None)`.
    pub fn resolve_all(&self, extractions: &[Extraction]) -> ResolutionResult {
        self.resolve_subset(extractions, None)
    }

    /// Resolve only `only`'s files, if given.
    ///
    /// The index must still be built from *every* extraction — resolution reads
    /// a global symbol index, so a subset index would resolve differently. Only
    /// the emission loop is narrowed, which is what makes the result mergeable
    /// with carried-forward edges keyed by source file.
    pub fn resolve_subset(
        &self,
        extractions: &[Extraction],
        only: Option<&BTreeSet<String>>,
    ) -> ResolutionResult {
        let mut edges = Vec::new();
        let mut unresolved: Vec<UnresolvedReference> = Vec::new();
        let mut package_groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

        for ext in extractions {
            if only.is_some_and(|set| !set.contains(&ext.file_path)) {
                continue;
            }
            let family = LangFamily::from_lang(&ext.language);

            // Lexical containment. The file owns every symbol declared in it —
            // including nested methods — and a type additionally owns its own
            // methods, which is exactly the frozen baseline's `contains` shape.
            for sym in &ext.symbols {
                if sym.kind == SymbolKind::File {
                    continue;
                }
                edges.push(ResolvedEdge {
                    source_file: ext.file_path.clone(),
                    target_file: ext.file_path.clone(),
                    source_symbol: ext.file_path.clone(),
                    target_symbol: sym.qualified_name.clone(),
                    edge_kind: EdgeKind::Contains,
                    confidence: Confidence::DETERMINISTIC,
                    resolution: Some(Arc::new(Resolution::SameFile {
                        target_symbol: sym.qualified_name.clone(),
                        target_file: ext.file_path.clone(),
                    })),
                    details: None,
                });
                // A method is contained twice: once by the file, once by the
                // type that declares it. Only emit the second when the parent
                // is a real type rather than the file itself.
                if let Some(parent) = sym
                    .parent_symbol
                    .as_deref()
                    .filter(|parent| *parent != ext.file_path)
                {
                    edges.push(ResolvedEdge {
                        source_file: ext.file_path.clone(),
                        target_file: ext.file_path.clone(),
                        source_symbol: parent.to_string(),
                        target_symbol: sym.qualified_name.clone(),
                        edge_kind: EdgeKind::Contains,
                        confidence: Confidence::DETERMINISTIC,
                        resolution: Some(Arc::new(Resolution::SameFile {
                            target_symbol: sym.qualified_name.clone(),
                            target_file: ext.file_path.clone(),
                        })),
                        details: None,
                    });
                }
            }

            // G20: Group Go package members for star topology
            if family == LangFamily::Go {
                if let Some(pkg_name) = go_package_name_of(ext) {
                    if pkg_name != "main" {
                        let dir = Self::parent_dir(&ext.file_path);
                        let key = format!("package:{dir}/{pkg_name}");
                        package_groups
                            .entry(key)
                            .or_default()
                            .insert(ext.file_path.clone());
                    }
                }
            }

            // Resolve imports
            for imp in &ext.imports {
                let targets = self.resolve_import_targets(
                    &ext.file_path,
                    &ext.language,
                    &imp.module_specifier,
                );
                let edge_targets = if ext.language == "go" {
                    self.go_import_edge_targets(&targets)
                } else {
                    targets
                };
                for target_f in edge_targets {
                    edges.push(ResolvedEdge {
                        source_file: ext.file_path.clone(),
                        target_file: target_f.clone(),
                        source_symbol: ext.file_path.clone(),
                        target_symbol: target_f.clone(),
                        edge_kind: EdgeKind::Imports,
                        confidence: Confidence::DETERMINISTIC,
                        resolution: Some(Arc::new(Resolution::ImportScoped {
                            target_symbol: target_f.clone(),
                            target_file: target_f,
                            imported_from: imp.module_specifier.clone(),
                        })),
                        details: Some(imp.raw_import.clone()),
                    });
                }
            }

            // Resolve calls using Resolution Ladder (SameFile -> ImportScoped -> UniqueGlobal -> AmbiguousGlobal)
            for call in &ext.calls {
                let mut resolved_target = None;
                let mut resolution = None;
                let mut confidence = Confidence::HIGH;

                // 1. Receiver-based resolution (SameFile / Constructor tracking N6)
                if let Some(recv) = &call.receiver_expr {
                    // Prefer the binding scoped to the calling symbol; fall back
                    // to the file-wide map only when it is unambiguous. Poisoned
                    // keys are absent from both maps, so a collision falls
                    // through the ladder instead of resolving to a guess (SC9).
                    let recv_key = format!("{}:{}", ext.file_path, recv);
                    let scoped = call.caller_symbol.as_deref().and_then(|caller| {
                        self.scoped_receiver_types
                            .get(&format!("{}:{}:{}", ext.file_path, caller, recv))
                    });
                    // A receiver that *is* a type names it directly:
                    // `PdgBuilder::new()`, `Config.default()`, `Self::helper()`.
                    // There is no binding to look up because nothing was bound
                    // — the type is written at the call site — so without this
                    // an associated function fell past every receiver-aware
                    // rung to the global tier, where any other type declaring
                    // `new` made it ambiguous.
                    //
                    // It fires only when a type of exactly that name declares
                    // exactly that method, so it cannot invent a target: the
                    // `type_methods` lookup below is the same one a bound
                    // receiver goes through, asked with the name as written.
                    let literal_type = self
                        .type_methods
                        .contains_key(&(family, recv.clone(), call.callee_name.clone()))
                        .then(|| recv.clone());
                    if let Some(class_type) = scoped
                        .or_else(|| self.receiver_types.get(&recv_key))
                        .or(literal_type.as_ref())
                    {
                        let key = (family, class_type.clone(), call.callee_name.clone());
                        if let Some(hits) = self.type_methods.get(&key) {
                            if hits.len() == 1 {
                                let (target_f, target_symbol) = &hits[0];
                                resolved_target = Some((target_f.clone(), target_symbol.clone()));
                                confidence = Confidence::DETERMINISTIC;
                                resolution = Some(Arc::new(Resolution::ReceiverType {
                                    target_symbol: target_symbol.clone(),
                                    target_file: target_f.clone(),
                                    receiver_type: class_type.clone(),
                                }));
                            }
                        }
                    }
                }

                // 2a. Import-scoped named binding (G6 — no silent global widen)
                if resolved_target.is_none() {
                    if let Some(bindings) = self.import_bindings.get(&ext.file_path) {
                        if let Some((target_f, target_sym)) = bindings.get(&call.callee_name) {
                            if let Some((resolved_file, resolved_sym)) =
                                self.lookup_in_package(target_f, target_sym)
                            {
                                resolved_target =
                                    Some((resolved_file.clone(), resolved_sym.clone()));
                                confidence = Confidence::DETERMINISTIC;
                                resolution = Some(Arc::new(Resolution::ImportScoped {
                                    target_symbol: resolved_sym,
                                    target_file: resolved_file,
                                    imported_from: call.callee_name.clone(),
                                }));
                            }
                        }
                    }
                }

                // 2b. Import-scoped module.method (G6 — no silent global widen)
                if resolved_target.is_none() {
                    let (recv, method) = if let Some(r) = &call.receiver_expr {
                        (r.clone(), call.callee_name.clone())
                    } else if let Some((r, m)) = call.callee_name.rsplit_once('.') {
                        (r.to_string(), m.to_string())
                    } else {
                        (String::new(), String::new())
                    };
                    if !recv.is_empty() && !method.is_empty() {
                        if let Some(bindings) = self.import_bindings.get(&ext.file_path) {
                            if let Some((target_f, _)) = bindings.get(&recv) {
                                if let Some((resolved_file, resolved_sym)) =
                                    self.lookup_in_package(target_f, &method)
                                {
                                    resolved_target =
                                        Some((resolved_file.clone(), resolved_sym.clone()));
                                    confidence = Confidence::DETERMINISTIC;
                                    resolution = Some(Arc::new(Resolution::ImportScoped {
                                        target_symbol: resolved_sym,
                                        target_file: resolved_file,
                                        imported_from: recv.clone(),
                                    }));
                                }
                            }
                        }
                    }
                }

                // 2c. Same-file symbol resolution.
                //
                // Runs *after* the import rungs and only for a call this file
                // could actually be the target of. A call with a receiver names
                // something that receiver owns, so matching the bare callee
                // against this file's own symbols is a guess — and a wrong one
                // wherever a module handle shares a name with a local
                // declaration. `ast_lsp_handlers.reset_caches()` inside a file
                // that itself declares `reset_caches` resolved to *itself*,
                // fabricating a self-call edge and leaving the real target with
                // no caller and a confident dead-code finding. The same shape
                // put `a.cfg.capabilityFor(model)` on `Adapter.capabilityFor`
                // and reported `Config.capabilityFor` dead at 0.9.
                //
                // A self-reference is the exception, because there the receiver
                // *is* this scope: `self.helper()` and `this.helper()` name a
                // sibling declaration, which is exactly what this rung finds.
                if resolved_target.is_none()
                    && call
                        .receiver_expr
                        .as_deref()
                        .is_none_or(Self::receiver_is_self)
                {
                    if let Some(file_syms) = self.file_symbols.get(&ext.file_path) {
                        if file_syms
                            .iter()
                            .filter(|symbol| *symbol == &call.callee_name)
                            .count()
                            == 1
                        {
                            resolved_target =
                                Some((ext.file_path.clone(), call.callee_name.clone()));
                            confidence = Confidence::DETERMINISTIC;
                            resolution = Some(Arc::new(Resolution::SameFile {
                                target_symbol: call.callee_name.clone(),
                                target_file: ext.file_path.clone(),
                            }));
                        }
                    }
                }

                // 3. Global lookup (UniqueGlobal vs AmbiguousGlobal - G5, G3)
                if resolved_target.is_none() {
                    if let Some(hits) = self.symbol_index.get(&call.callee_name) {
                        let family_hits: Vec<_> = hits
                            .iter()
                            .filter(|(path, _, candidate_family)| {
                                *candidate_family == family
                                    && (*candidate_family != LangFamily::Go
                                        || Self::go_symbol_visible_from(
                                            &ext.file_path,
                                            path,
                                            &call.callee_name,
                                        ))
                            })
                            .collect();
                        if family_hits.len() == 1 {
                            let (target_f, _, _) = family_hits[0];
                            // G3: Python stdlib-name guard inside UniqueGlobal rung only
                            let is_python_stdlib_guard = family == LangFamily::Python
                                && matches!(
                                    call.callee_name.as_str(),
                                    "open" | "dir" | "print" | "type" | "id" | "len"
                                )
                                && target_f != &ext.file_path;

                            if !is_python_stdlib_guard {
                                resolved_target =
                                    Some((target_f.clone(), call.callee_name.clone()));
                                confidence = Confidence::HIGH;
                                resolution = Some(Arc::new(Resolution::UniqueGlobal {
                                    target_symbol: call.callee_name.clone(),
                                    target_file: target_f.clone(),
                                    family,
                                }));
                            }
                        } else if family_hits.len() > 1 {
                            // G5: Multi-candidate pick MUST NOT emit Extracted / HIGH confidence
                            let candidates: Vec<(String, String)> = family_hits
                                .iter()
                                .map(|(f, _, _)| ((*f).clone(), call.callee_name.clone()))
                                .collect();
                            resolved_target =
                                Some((candidates[0].0.clone(), call.callee_name.clone()));
                            confidence = Confidence::SPECULATIVE;
                            resolution =
                                Some(Arc::new(Resolution::AmbiguousGlobal { candidates, family }));
                        }
                    }
                }

                let caller_sym = call
                    .caller_symbol
                    .clone()
                    .unwrap_or_else(|| ext.file_path.clone());
                if let Some(Resolution::AmbiguousGlobal { candidates, .. }) = resolution.as_deref()
                {
                    for (target_f, target_sym) in candidates {
                        edges.push(ResolvedEdge {
                            source_file: ext.file_path.clone(),
                            target_file: target_f.clone(),
                            source_symbol: caller_sym.clone(),
                            target_symbol: self.qualified_for(target_f, target_sym),
                            edge_kind: EdgeKind::Calls,
                            confidence,
                            resolution: resolution.clone(),
                            details: None,
                        });
                    }
                } else if let Some((target_f, target_sym)) = resolved_target {
                    let target_symbol = self.qualified_for(&target_f, &target_sym);
                    edges.push(ResolvedEdge {
                        source_file: ext.file_path.clone(),
                        target_file: target_f,
                        source_symbol: caller_sym,
                        target_symbol,
                        edge_kind: EdgeKind::Calls,
                        confidence,
                        resolution,
                        details: None,
                    });
                } else {
                    // D17 / R5: a call the ladder could not resolve is recorded,
                    // never dropped. Silence here is indistinguishable from
                    // "there was no call", which is the failure R5 forbids.
                    let class = self.classify_unresolved(
                        &ext.file_path,
                        family,
                        &call.callee_name,
                        call.receiver_expr.as_deref(),
                        &caller_sym,
                    );
                    unresolved.push(UnresolvedReference {
                        source_file: ext.file_path.clone(),
                        source_symbol: caller_sym,
                        callee_name: call.callee_name.clone(),
                        resolution: Resolution::Unresolved {
                            reason: format!(
                                "no resolution ladder rung matched {:?} in {} family {:?}",
                                call.callee_name, ext.file_path, family
                            ),
                        },
                        class,
                        receiver: call.receiver_expr.clone(),
                    });
                }
            }

            for reference in &ext.references {
                if matches!(
                    reference.kind,
                    ReferenceKind::Call | ReferenceKind::Constructor | ReferenceKind::JsxTag
                ) {
                    continue;
                }
                if let Some(edge) = self.resolve_name_reference(ext, family, reference) {
                    edges.push(edge);
                }
            }

            // Resolve routes
            for route in &ext.routes {
                if let Some(hits) = self.symbol_index.get(&route.handler_name) {
                    let same_file: Vec<_> = hits
                        .iter()
                        .filter(|(path, _, _)| path == &ext.file_path)
                        .collect();
                    let route_target = if same_file.len() == 1 {
                        let (target_f, _, _) = same_file[0];
                        Some((
                            target_f.clone(),
                            Confidence::DETERMINISTIC,
                            Resolution::SameFile {
                                target_symbol: route.handler_name.clone(),
                                target_file: target_f.clone(),
                            },
                        ))
                    } else if let Some((target_f, target_symbol)) = self
                        .import_bindings
                        .get(&ext.file_path)
                        .and_then(|bindings| bindings.get(&route.handler_name))
                    {
                        Some((
                            target_f.clone(),
                            Confidence::DETERMINISTIC,
                            Resolution::ImportScoped {
                                target_symbol: target_symbol.clone(),
                                target_file: target_f.clone(),
                                imported_from: route.handler_name.clone(),
                            },
                        ))
                    } else {
                        let family_hits: Vec<_> = hits
                            .iter()
                            .filter(|(path, _, candidate_family)| {
                                *candidate_family == family
                                    && (*candidate_family != LangFamily::Go
                                        || Self::go_symbol_visible_from(
                                            &ext.file_path,
                                            path,
                                            &route.handler_name,
                                        ))
                            })
                            .collect();
                        (family_hits.len() == 1).then(|| {
                            let (target_f, _, _) = family_hits[0];
                            (
                                target_f.clone(),
                                Confidence::HIGH,
                                Resolution::UniqueGlobal {
                                    target_symbol: route.handler_name.clone(),
                                    target_file: target_f.clone(),
                                    family,
                                },
                            )
                        })
                    };
                    if let Some((target_f, confidence, resolution)) = route_target {
                        edges.push(ResolvedEdge {
                            source_file: ext.file_path.clone(),
                            target_file: target_f.clone(),
                            source_symbol: format!("{} {}", route.http_method, route.path_pattern),
                            target_symbol: route.handler_name.clone(),
                            edge_kind: EdgeKind::HandlesRoute,
                            confidence,
                            resolution: Some(Arc::new(resolution)),
                            details: Some(route.framework.clone()),
                        });
                    }
                }
            }
        }

        // G20: Add Go synthetic package star edges
        for (pkg_node, files) in package_groups {
            for file in files {
                edges.push(ResolvedEdge {
                    source_file: file.clone(),
                    target_file: pkg_node.clone(),
                    source_symbol: file,
                    target_symbol: pkg_node.clone(),
                    edge_kind: EdgeKind::MemberOf,
                    confidence: Confidence::DETERMINISTIC,
                    resolution: Some(Arc::new(Resolution::SameFile {
                        target_symbol: pkg_node.clone(),
                        target_file: pkg_node.clone(),
                    })),
                    details: Some("Go package star topology".to_string()),
                });
            }
        }

        // R4: emission order must not depend on input iteration order.
        //
        // Same total order as the tuple form this replaces, evaluated lazily.
        // Building a tuple constructs every element up front, so all four
        // `format!` calls ran on *every* comparison even though tuple
        // comparison short-circuits at the first difference — and the two
        // costly keys are 5th and 7th, reached only when the four string keys
        // ahead of them tie. Formatting a `Resolution` also serializes its
        // whole candidate list. Measured: this sort was 113.5 s of a 130 s
        // build over ~1.06 M pre-dedup edges.
        //
        // `then_with` defers each key until the preceding ones compare equal,
        // which keeps the ordering identical — verified by an edge-ordinal
        // digest over a 4,742-file corpus before and after.
        edges.sort_by(|left, right| {
            left.source_file
                .cmp(&right.source_file)
                .then_with(|| left.source_symbol.cmp(&right.source_symbol))
                .then_with(|| left.target_file.cmp(&right.target_file))
                .then_with(|| left.target_symbol.cmp(&right.target_symbol))
                .then_with(|| {
                    format!("{:?}", left.edge_kind).cmp(&format!("{:?}", right.edge_kind))
                })
                .then_with(|| {
                    left.confidence
                        .0
                        .to_bits()
                        .cmp(&right.confidence.0.to_bits())
                })
                .then_with(|| {
                    format!("{:?}", left.resolution).cmp(&format!("{:?}", right.resolution))
                })
                .then_with(|| left.details.cmp(&right.details))
        });
        edges.dedup_by(|left, right| {
            left.source_file == right.source_file
                && left.source_symbol == right.source_symbol
                && left.target_file == right.target_file
                && left.target_symbol == right.target_symbol
                && left.edge_kind == right.edge_kind
                && left.confidence == right.confidence
                && left.resolution == right.resolution
                && left.details == right.details
        });

        // R4: emission order must not depend on input iteration order.
        unresolved.sort_by(|left, right| {
            (&left.source_file, &left.source_symbol, &left.callee_name).cmp(&(
                &right.source_file,
                &right.source_symbol,
                &right.callee_name,
            ))
        });

        ResolutionResult {
            edges,
            receiver_types: self.receiver_types.clone(),
            reexport_chains: BTreeMap::new(),
            unresolved,
        }
    }

    fn parent_dir(current_file: &str) -> String {
        Path::new(current_file)
            .parent()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| ".".to_string())
    }

    fn go_name_is_exported(name: &str) -> bool {
        name.rsplit('.')
            .next()
            .unwrap_or(name)
            .chars()
            .next()
            .is_some_and(|ch| ch.is_uppercase())
    }

    /// Unexported Go names are package-private. Unique-global must not bind
    /// `segment` in `api/gateway.go` to `adkeval.HallucinationsEvaluator.segment`.
    fn go_symbol_visible_from(source_file: &str, target_file: &str, name: &str) -> bool {
        Self::go_name_is_exported(name)
            || Self::parent_dir(source_file) == Self::parent_dir(target_file)
    }

    fn normalize_rel(base_dir: &str, spec: &str) -> String {
        let joined = if base_dir == "." {
            spec.to_string()
        } else {
            format!("{}/{}", base_dir, spec)
        };
        let norm = joined.replace('\\', "/");
        let mut stack: Vec<&str> = Vec::new();
        for part in norm.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    stack.pop();
                }
                other => stack.push(other),
            }
        }
        stack.join("/")
    }

    fn import_local_name(lang: &str, specifier: &str) -> String {
        if lang == "go" {
            specifier
                .rsplit('/')
                .next()
                .unwrap_or(specifier)
                .to_string()
        } else {
            specifier
                .rsplit('.')
                .next()
                .unwrap_or(specifier)
                .to_string()
        }
    }

    /// Whether a receiver expression denotes the enclosing scope itself.
    ///
    /// These are the spellings across the indexed languages: `self` (Python,
    /// Rust, Swift), `this` (JS/TS, Java, C#, PHP's `$this`), `cls` (Python
    /// classmethods), `me` (VB). A receiver in this set names the object the
    /// current code is already inside, so a sibling declaration in the same
    /// file is a real candidate; any other receiver names something else, and
    /// matching it against this file's symbols by bare name is a guess.
    fn receiver_is_self(receiver: &str) -> bool {
        matches!(receiver, "self" | "this" | "cls" | "$this" | "me" | "Self")
    }

    fn lookup_in_package(&self, file: &str, name: &str) -> Option<(String, String)> {
        if self
            .file_symbols
            .get(file)
            .is_some_and(|syms| syms.iter().any(|symbol| symbol == name))
        {
            return Some((file.to_string(), name.to_string()));
        }
        if !file.ends_with(".go") {
            return None;
        }
        let dir = Self::parent_dir(file);
        let mut hits: Vec<(String, String)> = self
            .file_symbols
            .iter()
            .filter(|(path, syms)| {
                path.ends_with(".go")
                    && !path.ends_with("_test.go")
                    && Self::parent_dir(path) == dir
                    && syms.iter().any(|symbol| symbol == name)
            })
            .map(|(path, _)| (path.clone(), name.to_string()))
            .collect();
        hits.sort();
        hits.dedup();
        (hits.len() == 1).then(|| hits.pop().unwrap())
    }

    fn go_import_edge_targets(&self, files: &[String]) -> Vec<String> {
        let mut nodes = BTreeSet::new();
        for file in files {
            match self.go_package_by_file.get(file) {
                Some(pkg) if pkg != "main" && !pkg.is_empty() => {
                    nodes.insert(format!("package:{}/{}", Self::parent_dir(file), pkg));
                }
                _ => {
                    nodes.insert(file.clone());
                }
            }
        }
        nodes.into_iter().collect()
    }

    fn go_files_in_dir(&self, dir: &str) -> Vec<String> {
        let dir = dir.trim_end_matches('/');
        let mut files: Vec<String> = self
            .file_symbols
            .keys()
            .filter(|path| {
                path.ends_with(".go")
                    && !path.ends_with("_test.go")
                    && Self::parent_dir(path) == dir
            })
            .cloned()
            .collect();
        files.sort();
        files
    }

    fn apply_go_replace(&self, spec: &str) -> (String, Option<String>) {
        let mut best: Option<(&GoModule, &(String, String))> = None;
        for module in &self.go_modules {
            for replace in &module.replaces {
                if spec == replace.0 || spec.starts_with(&format!("{}/", replace.0)) {
                    let better = best.is_none_or(|(_, current)| replace.0.len() > current.0.len());
                    if better {
                        best = Some((module, replace));
                    }
                }
            }
        }
        let Some((module, (from, to))) = best else {
            return (spec.to_string(), None);
        };
        let suffix = spec[from.len()..].trim_start_matches('/');
        let joined = if to.starts_with('.') || to.starts_with('/') {
            let base = if module.dir.is_empty() {
                "."
            } else {
                module.dir.as_str()
            };
            let replaced = Self::normalize_rel(base, to);
            if suffix.is_empty() {
                replaced
            } else {
                format!("{replaced}/{suffix}")
            }
        } else if suffix.is_empty() {
            to.clone()
        } else {
            format!("{to}/{suffix}")
        };
        (spec.to_string(), Some(joined))
    }

    fn resolve_go_import(&self, specifier: &str) -> Vec<String> {
        let spec = specifier.trim_matches('"').trim();
        if spec.is_empty() || spec == "C" {
            return Vec::new();
        }
        let (original, replaced) = self.apply_go_replace(spec);
        if let Some(replaced_dir) = replaced {
            let files = self.go_files_in_dir(&replaced_dir);
            if !files.is_empty() {
                return files;
            }
        }

        let mut best_module: Option<&GoModule> = None;
        for module in &self.go_modules {
            if original == module.prefix || original.starts_with(&format!("{}/", module.prefix)) {
                let better =
                    best_module.is_none_or(|current| module.prefix.len() > current.prefix.len());
                if better {
                    best_module = Some(module);
                }
            }
        }
        if let Some(module) = best_module {
            let rel = original[module.prefix.len()..].trim_start_matches('/');
            let target_dir = if rel.is_empty() {
                module.dir.clone()
            } else if module.dir.is_empty() {
                rel.to_string()
            } else {
                format!("{}/{}", module.dir, rel)
            };
            let files = self.go_files_in_dir(&target_dir);
            if !files.is_empty() {
                return files;
            }
        }

        let vendor = format!("vendor/{original}");
        let vendor_files = self.go_files_in_dir(&vendor);
        if !vendor_files.is_empty() {
            return vendor_files;
        }

        if !original.contains('/') {
            return Vec::new();
        }
        let mut matches: Vec<(usize, String)> = self
            .file_symbols
            .keys()
            .filter(|path| path.ends_with(".go") && !path.ends_with("_test.go"))
            .map(|path| Self::parent_dir(path))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|dir| {
                let components = dir.split('/').filter(|part| !part.is_empty()).count();
                components >= 2
                    && (original == dir.as_str() || original.ends_with(&format!("/{dir}")))
            })
            .map(|dir| (dir.len(), dir))
            .collect();
        matches.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
        let Some(&(best_len, _)) = matches.first() else {
            return Vec::new();
        };
        matches.retain(|(len, _)| *len == best_len);
        if matches.len() != 1 {
            return Vec::new();
        }
        self.go_files_in_dir(&matches[0].1)
    }

    fn resolve_import_targets(
        &self,
        current_file: &str,
        lang: &str,
        specifier: &str,
    ) -> Vec<String> {
        if lang == "go" {
            return self.resolve_go_import(specifier);
        }
        self.resolve_import_path(current_file, lang, specifier)
            .into_iter()
            .collect()
    }

    /// A member reference resolved through its receiver.
    ///
    /// Two rungs, both of which the call ladder already walks, in the same
    /// order and with the same evidence:
    ///
    /// 1. **Typed receiver.** `cfg` was bound by `cfg = GatesConfig()`, so
    ///    `cfg.enabled` names `GatesConfig.enabled`. The scoped binding is
    ///    preferred over the file-wide one for the SC9 reason: a file-wide
    ///    fallback once declared a local type's method external at full
    ///    confidence.
    /// 2. **Imported receiver.** `cmd` was bound by `from pkg import cmd`, so
    ///    `cmd.baseline` names `pkg/cmd.py::baseline`. This is what makes a
    ///    decorator registration — `app.command(...)(cmd.baseline)` — a use.
    ///
    /// Neither rung guesses. A receiver that is neither typed nor imported
    /// yields `None` and the reference stays unresolved, which is the honest
    /// answer: naming the member alone would be the bare-name global lookup
    /// this function refuses on purpose.
    fn resolve_member_reference(
        &self,
        ext: &Extraction,
        family: LangFamily,
        reference: &ExtractedReference,
        receiver: &str,
        name: &str,
    ) -> Option<ResolvedEdge> {
        let scoped = reference.enclosing_symbol.as_deref().and_then(|scope| {
            self.scoped_receiver_types
                .get(&format!("{}:{}:{}", ext.file_path, scope, receiver))
        });
        let receiver_key = format!("{}:{}", ext.file_path, receiver);
        if let Some(class_type) = scoped.or_else(|| self.receiver_types.get(&receiver_key)) {
            let key = (family, class_type.clone(), name.to_string());
            if let Some(hits) = self.type_methods.get(&key) {
                if hits.len() == 1 {
                    let (target_file, target_symbol) = &hits[0];
                    return Some(self.reference_edge(
                        ext,
                        target_file,
                        target_symbol,
                        reference,
                        Resolution::ReceiverType {
                            target_symbol: self.qualified_for(target_file, target_symbol),
                            target_file: target_file.clone(),
                            receiver_type: class_type.clone(),
                        },
                    ));
                }
            }
        }

        let (module_file, _) = self.import_bindings.get(&ext.file_path)?.get(receiver)?;
        let (resolved_file, resolved_symbol) = self.lookup_in_package(module_file, name)?;
        Some(self.reference_edge(
            ext,
            &resolved_file,
            &resolved_symbol,
            reference,
            Resolution::ImportScoped {
                target_symbol: self.qualified_for(&resolved_file, &resolved_symbol),
                target_file: resolved_file.clone(),
                imported_from: receiver.to_string(),
            },
        ))
    }

    fn resolve_name_reference(
        &self,
        ext: &Extraction,
        family: LangFamily,
        reference: &ExtractedReference,
    ) -> Option<ResolvedEdge> {
        let name = reference.name.rsplit('.').next().unwrap_or(&reference.name);
        if name.is_empty() {
            return None;
        }
        let prefer_types = matches!(
            reference.kind,
            ReferenceKind::Type | ReferenceKind::Heritage
        );
        let is_type = |kind: SymbolKind| {
            matches!(
                kind,
                SymbolKind::Class
                    | SymbolKind::Struct
                    | SymbolKind::Enum
                    | SymbolKind::Interface
                    | SymbolKind::Trait
            )
        };

        let same_file = self.file_symbols.get(&ext.file_path).and_then(|syms| {
            let hits: Vec<_> = syms.iter().filter(|symbol| *symbol == name).collect();
            (hits.len() == 1).then(|| ext.file_path.clone())
        });
        if let Some(target_file) = same_file {
            if let Some(kind) = self.symbol_kind_in(&target_file, name) {
                if !prefer_types || is_type(kind) {
                    return Some(self.reference_edge(
                        ext,
                        &target_file,
                        name,
                        reference,
                        Resolution::SameFile {
                            target_symbol: self.qualified_for(&target_file, name),
                            target_file: target_file.clone(),
                        },
                    ));
                }
            }
        }

        if let Some(bindings) = self.import_bindings.get(&ext.file_path) {
            if let Some((target_f, target_sym)) = bindings.get(name) {
                if let Some((resolved_file, resolved_sym)) =
                    self.lookup_in_package(target_f, target_sym)
                {
                    return Some(self.reference_edge(
                        ext,
                        &resolved_file,
                        &resolved_sym,
                        reference,
                        Resolution::ImportScoped {
                            target_symbol: self.qualified_for(&resolved_file, &resolved_sym),
                            target_file: resolved_file.clone(),
                            imported_from: ext.file_path.clone(),
                        },
                    ));
                }
            }
        }

        // A *member* reference names something another symbol owns, so the
        // two rungs that can prove which one apply exactly as they do for a
        // method call: a receiver whose type is known, and a receiver bound by
        // an import. `cfg.enabled` and `cmd.baseline` resolve here.
        //
        // This runs before the bare-name refusal below and never widens it: a
        // reference with no receiver is still a bare name and still refused.
        if let Some(receiver) = reference.receiver_expr.as_deref() {
            if let Some(edge) =
                self.resolve_member_reference(ext, family, reference, receiver, name)
            {
                return Some(edge);
            }
        }

        // Name identifiers unique-global to a unique function in another file.
        // That binds `except Exception as e` / `print(e)` / `for _, segment`
        // to a unique `def e` / `func segment` across the language family.
        // Same-file and import-scoped remain; Calls still unique-global.
        if matches!(reference.kind, ReferenceKind::Name) {
            return None;
        }

        if let Some(hits) = self.symbol_index.get(name) {
            let family_hits: Vec<_> = hits
                .iter()
                .filter(|(path, kind, candidate_family)| {
                    *candidate_family == family
                        && (!prefer_types || is_type(*kind))
                        && (*candidate_family != LangFamily::Go
                            || Self::go_symbol_visible_from(&ext.file_path, path, name))
                })
                .collect();
            if family_hits.len() == 1 {
                let (target_f, _, _) = family_hits[0];
                let stdlib_guard = family == LangFamily::Python
                    && matches!(
                        name,
                        "open" | "dir" | "print" | "type" | "id" | "len" | "str" | "int" | "list"
                    )
                    && target_f != &ext.file_path;
                if !stdlib_guard {
                    return Some(self.reference_edge(
                        ext,
                        target_f,
                        name,
                        reference,
                        Resolution::UniqueGlobal {
                            target_symbol: self.qualified_for(target_f, name),
                            target_file: target_f.clone(),
                            family,
                        },
                    ));
                }
            }
        }
        None
    }

    fn symbol_kind_in(&self, file: &str, name: &str) -> Option<SymbolKind> {
        self.symbol_index.get(name).and_then(|hits| {
            let file_hits: Vec<_> = hits
                .iter()
                .filter(|(path, _, _)| path == file)
                .map(|(_, kind, _)| *kind)
                .collect();
            (file_hits.len() == 1).then_some(file_hits[0])
        })
    }

    fn reference_edge(
        &self,
        ext: &Extraction,
        target_file: &str,
        target_sym: &str,
        reference: &ExtractedReference,
        resolution: Resolution,
    ) -> ResolvedEdge {
        let target_symbol = self.qualified_for(target_file, target_sym);
        let source_symbol = reference
            .enclosing_symbol
            .clone()
            .unwrap_or_else(|| ext.file_path.clone());
        ResolvedEdge {
            source_file: ext.file_path.clone(),
            target_file: target_file.to_string(),
            source_symbol,
            target_symbol,
            edge_kind: EdgeKind::References,
            confidence: Confidence::DETERMINISTIC,
            resolution: Some(Arc::new(resolution)),
            details: Some(format!("{:?}", reference.kind)),
        }
    }

    fn resolve_import_path(
        &self,
        current_file: &str,
        lang: &str,
        specifier: &str,
    ) -> Option<String> {
        let clean_spec = specifier.trim_matches(|c| c == '\'' || c == '"');
        let dir = Self::parent_dir(current_file);

        if matches!(lang, "javascript" | "typescript" | "tsx") && clean_spec.starts_with('.') {
            let base = Self::normalize_rel(&dir, clean_spec);
            let candidates = [
                base.clone(),
                format!("{}.ts", base),
                format!("{}.tsx", base),
                format!("{}.mts", base),
                format!("{}.cts", base),
                format!("{}.js", base),
                format!("{}.jsx", base),
                format!("{}.mjs", base),
                format!("{}.cjs", base),
                format!("{}/index.ts", base),
                format!("{}/index.tsx", base),
                format!("{}/index.js", base),
                format!("{}/index.jsx", base),
            ];
            for cand in candidates {
                if self.file_symbols.contains_key(&cand) {
                    return Some(cand);
                }
            }
        }

        if lang == "python" {
            let mut py_path = clean_spec.to_string();
            let mut dots = 0;
            for c in clean_spec.chars() {
                if c == '.' {
                    dots += 1;
                } else {
                    break;
                }
            }
            if dots > 0 {
                let mut d = dir.clone();
                for _ in 1..dots {
                    if let Some(parent) = std::path::Path::new(&d).parent() {
                        d = parent.to_string_lossy().replace('\\', "/");
                    }
                }
                let suffix = &clean_spec[dots..].replace('.', "/");
                py_path = if suffix.is_empty() {
                    d
                } else {
                    format!("{}/{}", d, suffix)
                };
            } else {
                py_path = py_path.replace('.', "/");
            }

            let candidates = [
                format!("{}.py", py_path),
                format!("{}/__init__.py", py_path),
                format!("src/{}.py", py_path),
                format!("src/{}/__init__.py", py_path),
                format!("{}/{}.py", dir, py_path),
                format!("{}/{}/__init__.py", dir, py_path),
            ];
            for cand in &candidates {
                if self.file_symbols.contains_key(cand) {
                    return Some(cand.clone());
                }
            }
        }

        if lang == "rust" && clean_spec.starts_with("crate::") {
            let crate_tail = clean_spec.strip_prefix("crate::")?;
            let mut parts: Vec<&str> = crate_tail.split("::").collect();
            while !parts.is_empty() {
                let rust_path = parts.join("/");
                let candidates = [
                    format!("src/{}.rs", rust_path),
                    format!("src/{}/mod.rs", rust_path),
                ];
                for cand in &candidates {
                    if self.file_symbols.contains_key(cand) {
                        return Some(cand.clone());
                    }
                }
                parts.pop();
            }
        }

        if lang == "rust" && (clean_spec.starts_with("self::") || clean_spec.starts_with("super::"))
        {
            let mut module_dir = dir;
            let mut tail = clean_spec;
            if let Some(stripped) = tail.strip_prefix("self::") {
                tail = stripped;
            } else {
                while let Some(stripped) = tail.strip_prefix("super::") {
                    module_dir = Self::parent_dir(&module_dir);
                    tail = stripped;
                }
            }
            let mut parts: Vec<&str> = tail.split("::").collect();
            while !parts.is_empty() {
                let module_path = parts.join("/");
                let base = Self::normalize_rel(&module_dir, &module_path);
                for candidate in [format!("{base}.rs"), format!("{base}/mod.rs")] {
                    if self.file_symbols.contains_key(&candidate) {
                        return Some(candidate);
                    }
                }
                parts.pop();
            }
        }

        None
    }
}

fn go_package_name_of(ext: &Extraction) -> Option<String> {
    if let Some(pkg) = ext.go_package.as_deref().filter(|pkg| !pkg.is_empty()) {
        return Some(pkg.to_string());
    }
    ext.source_code.as_deref().and_then(|source| {
        source.lines().find_map(|line| {
            line.trim()
                .strip_prefix("package ")
                .map(|pkg| pkg.trim_matches(';').trim().to_string())
                .filter(|pkg| !pkg.is_empty())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use devmap_extract::extract_file;

    #[test]
    fn relative_js_import_uses_parent_dir_not_filename() {
        let a = extract_file("pkg/a.js", "import './b'\nexport function fromA() {}\n");
        let b = extract_file("pkg/b.js", "export function fromB() {}\n");
        let mut resolver = Resolver::new();
        resolver.index_extractions(&[a.clone(), b.clone()]);
        let result = resolver.resolve_all(&[a, b]);
        assert!(
            result.edges.iter().any(|e| {
                e.edge_kind == EdgeKind::Imports
                    && e.source_file == "pkg/a.js"
                    && e.target_file == "pkg/b.js"
            }),
            "expected import edge pkg/a.js -> pkg/b.js, got {:?}",
            result.edges
        );
    }
}

#[cfg(test)]
mod import_resolution_tests {
    use super::*;
    use devmap_extract::GoModule;

    fn module(dir: &str, prefix: &str, replaces: &[(&str, &str)]) -> GoModule {
        GoModule {
            prefix: prefix.to_string(),
            dir: dir.to_string(),
            replaces: replaces
                .iter()
                .map(|(from, to)| (from.to_string(), to.to_string()))
                .collect(),
        }
    }

    /// A receiver binds once; a conflicting bind poisons the key permanently.
    ///
    /// This is SC9's fix. Two different types bound to the same receiver name
    /// used to be last-write-wins, so the loser still resolved — at
    /// DETERMINISTIC confidence — to the wrong type. That is a *confidently
    /// wrong* answer rather than a missing one, which is the failure a code
    /// map must never produce. Every part of the conflict test was mutable:
    /// forced false it restores last-write-wins, forced true it poisons on a
    /// harmless repeat, and inverted it poisons agreement while accepting
    /// conflict.
    #[test]
    fn a_conflicting_receiver_bind_poisons_the_key_permanently() {
        let mut map = BTreeMap::new();
        let mut poisoned = BTreeSet::new();
        let key = || "file.go:scope:w".to_string();

        // First bind wins.
        Resolver::bind_receiver(&mut map, &mut poisoned, key(), "Worker");
        assert_eq!(map.get(&key()).map(String::as_str), Some("Worker"));
        assert!(poisoned.is_empty());

        // Re-binding the *same* type is agreement, not conflict.
        Resolver::bind_receiver(&mut map, &mut poisoned, key(), "Worker");
        assert_eq!(
            map.get(&key()).map(String::as_str),
            Some("Worker"),
            "binding the same type twice must not poison the key"
        );
        assert!(poisoned.is_empty(), "agreement is not a conflict");

        // A different type is a conflict: the binding is withdrawn, not
        // overwritten, so neither type is reported.
        Resolver::bind_receiver(&mut map, &mut poisoned, key(), "Other");
        assert_eq!(
            map.get(&key()),
            None,
            "a conflicting bind must withdraw the binding, not overwrite it"
        );
        assert!(poisoned.contains(&key()));

        // And the poison is permanent — a later bind cannot resurrect it, in
        // either direction.
        Resolver::bind_receiver(&mut map, &mut poisoned, key(), "Worker");
        assert_eq!(
            map.get(&key()),
            None,
            "a poisoned key must stay unbound even when re-offered the original type"
        );

        // Poisoning is per key: an unrelated receiver is unaffected.
        let other = "file.go:scope:z".to_string();
        Resolver::bind_receiver(&mut map, &mut poisoned, other.clone(), "Thing");
        assert_eq!(map.get(&other).map(String::as_str), Some("Thing"));
    }

    /// Only Python's dotted relative form names its child module in the list.
    ///
    /// `from . import sibling` puts the *module* in the import list, so the
    /// specifier to resolve is `.sibling`; every other language keeps the
    /// specifier as written. All three clauses were mutable, and each failure
    /// mode rewrites an import to a module path that does not exist — the
    /// import silently resolves to nothing.
    #[test]
    fn only_dotted_relative_specifiers_absorb_the_imported_name() {
        // Python relative imports name the child module.
        assert_eq!(Resolver::import_spec_for_name(".", "sibling"), ".sibling");
        assert_eq!(Resolver::import_spec_for_name("..", "parent"), "..parent");

        // A real module specifier is kept as written.
        assert_eq!(
            Resolver::import_spec_for_name("pkg.sub", "Name"),
            "pkg.sub",
            "a named import from a real module keeps its specifier"
        );
        assert_eq!(Resolver::import_spec_for_name("./local", "x"), "./local");

        // A wildcard names no module, and neither does an absent name.
        assert_eq!(
            Resolver::import_spec_for_name(".", "*"),
            ".",
            "`from . import *` names no child module"
        );
        assert_eq!(Resolver::import_spec_for_name(".", ""), ".");
    }

    /// Every language maps to its own resolution family.
    ///
    /// The family is what stops a Python annotation resolving to an
    /// identically-named Go type. Deleting an arm drops that language to
    /// `Generic`, which shares a family with every other unmapped language —
    /// so the guard that prevents cross-language edges silently stops
    /// separating them.
    #[test]
    fn every_language_maps_to_its_own_resolution_family() {
        use devmap_resolve_family_assertions::*;
        assert_family("python", LangFamily::Python);
        for lang in ["javascript", "typescript", "tsx", "jsx"] {
            assert_family(lang, LangFamily::JsTs);
        }
        assert_family("go", LangFamily::Go);
        assert_family("rust", LangFamily::Rust);
        for lang in ["c", "cpp", "csharp", "java", "objc"] {
            assert_family(lang, LangFamily::CStyle);
        }
        // An unmapped language falls back rather than joining someone else.
        assert_family("cobol", LangFamily::Generic);
        assert_family("", LangFamily::Generic);

        // The families must actually be distinct, or none of the above proves
        // anything.
        let families = [
            LangFamily::from_lang("python"),
            LangFamily::from_lang("javascript"),
            LangFamily::from_lang("go"),
            LangFamily::from_lang("rust"),
            LangFamily::from_lang("c"),
            LangFamily::from_lang("cobol"),
        ];
        for (index, left) in families.iter().enumerate() {
            for right in &families[index + 1..] {
                assert_ne!(left, right, "two languages share a resolution family");
            }
        }
    }

    mod devmap_resolve_family_assertions {
        use super::LangFamily;
        pub fn assert_family(lang: &str, expected: LangFamily) {
            assert_eq!(
                LangFamily::from_lang(lang),
                expected,
                "{lang} must map to {expected:?}"
            );
        }
    }

    /// Fixture spanning the per-language import-path resolvers.
    fn path_fixture() -> Resolver {
        use devmap_extract::extract_file;
        let files = [
            ("web/app.ts", "export const a = 1;\n"),
            ("web/sibling.ts", "export const b = 2;\n"),
            ("web/widget.tsx", "export const c = 3;\n"),
            ("web/nested/index.ts", "export const d = 4;\n"),
            ("web/plain.js", "export const e = 5;\n"),
            ("pkg/mod.py", "def f():\n    pass\n"),
            ("pkg/__init__.py", "\n"),
            ("pkg/sub/leaf.py", "def g():\n    pass\n"),
            ("src/lib.rs", "pub mod thing;\n"),
            ("src/thing.rs", "pub fn t() {}\n"),
            ("src/deep/mod.rs", "pub fn d() {}\n"),
        ];
        let extractions: Vec<_> = files
            .iter()
            .map(|(path, source)| extract_file(path, source))
            .collect();
        let mut resolver = Resolver::new();
        resolver.index_extractions(&extractions);
        resolver
    }

    /// Each language resolves import paths by its own rules.
    ///
    /// `resolve_import_path` is the only thing that turns an import specifier
    /// into a file, so every mutant here breaks import edges — and always by
    /// resolving to *nothing*, never by erroring. The language guards were
    /// mutable in both directions: dropped, one language's path rules are
    /// applied to another's specifiers; inverted, the language that owns the
    /// rule stops using it.
    ///
    /// Every expectation was read off the resolver before being pinned.
    #[test]
    fn each_language_resolves_import_paths_by_its_own_rules() {
        let resolver = path_fixture();
        let resolve =
            |file: &str, lang: &str, spec: &str| resolver.resolve_import_path(file, lang, spec);

        // TypeScript: a relative specifier is tried against each extension and
        // then against an index file.
        assert_eq!(
            resolve("web/app.ts", "typescript", "./sibling").as_deref(),
            Some("web/sibling.ts")
        );
        assert_eq!(
            resolve("web/app.ts", "typescript", "./nested").as_deref(),
            Some("web/nested/index.ts"),
            "a directory import resolves to its index file"
        );
        assert_eq!(
            resolve("web/app.ts", "typescript", "./plain").as_deref(),
            Some("web/plain.js"),
            "a TS file may import a JS sibling"
        );

        // The specifier arrives quoted from the grammar, in either quote style,
        // and both must be stripped before the path is built.
        assert_eq!(
            resolve("web/app.ts", "typescript", "\"./sibling\"").as_deref(),
            Some("web/sibling.ts"),
            "a double-quoted specifier resolves"
        );
        assert_eq!(
            resolve("web/app.ts", "typescript", "'./widget'").as_deref(),
            Some("web/widget.tsx"),
            "a single-quoted specifier resolves"
        );

        // A bare package specifier is not a path and must not be resolved
        // against the repository.
        assert_eq!(
            resolve("web/app.ts", "typescript", "react"),
            None,
            "a node package is not a local file"
        );

        // Python: an absolute dotted module, and the relative forms, where the
        // leading dot count decides how far up to walk.
        assert_eq!(
            resolve("pkg/mod.py", "python", "pkg.sub.leaf").as_deref(),
            Some("pkg/sub/leaf.py"),
            "an absolute dotted module maps onto directories"
        );
        assert_eq!(
            resolve("pkg/sub/leaf.py", "python", "..").as_deref(),
            Some("pkg/__init__.py"),
            "`from .. import x` resolves to the parent package"
        );
        assert_eq!(
            resolve("pkg/sub/leaf.py", "python", "..mod").as_deref(),
            Some("pkg/mod.py"),
            "the dots walk up and the tail names the module"
        );

        // Rust: `crate::` maps onto `src/`, trying a file then a module
        // directory, and popping segments until something exists.
        assert_eq!(
            resolve("src/lib.rs", "rust", "crate::thing").as_deref(),
            Some("src/thing.rs")
        );
        assert_eq!(
            resolve("src/lib.rs", "rust", "crate::deep").as_deref(),
            Some("src/deep/mod.rs"),
            "a module directory resolves through its mod.rs"
        );
        assert_eq!(
            resolve("src/lib.rs", "rust", "crate::thing::t").as_deref(),
            Some("src/thing.rs"),
            "a path to an item inside a module resolves to that module's file"
        );

        // The language guards hold: another language's specifier syntax does
        // not resolve through these rules.
        assert_eq!(
            resolve("src/lib.rs", "python", "crate::thing"),
            None,
            "Rust path syntax is not resolved for Python"
        );
        assert_eq!(
            resolve("web/app.ts", "python", "./sibling"),
            None,
            "a relative TS specifier is not a Python module path"
        );
    }

    /// Fixture spanning every tier of the Go import ladder.    /// Fixture spanning every tier of the Go import ladder.
    fn go_fixture() -> Resolver {
        use devmap_extract::extract_file;
        let files = [
            ("internal/svc/a.go", "package svc\nfunc A() {}\n"),
            ("internal/svc/b.go", "package svc\nfunc B() {}\n"),
            // A test file is not part of the importable surface.
            ("internal/svc/a_test.go", "package svc\nfunc T() {}\n"),
            ("vendor/example.com/ext/e.go", "package ext\nfunc E() {}\n"),
            // A vendored directory whose name collides with cgo's pseudo-package.
            ("vendor/C/c.go", "package c\nfunc C() {}\n"),
            ("local/lib/l.go", "package lib\nfunc L() {}\n"),
            ("deep/nested/pkg/n.go", "package pkg\nfunc N() {}\n"),
            ("cmd/tool/main.go", "package main\nfunc main() {}\n"),
            // A single-component directory that the suffix tier is too weak
            // to claim, so only the module tier can resolve it.
            ("x/y.go", "package x\nfunc Y() {}\n"),
            // A nested module whose directory deliberately differs from the
            // path the outer module's prefix would produce.
            ("othersub/inner/i.go", "package inner\nfunc I() {}\n"),
            // A non-Go file whose directory is a longer suffix than the real
            // Go package's.
            ("a/deep/nested/pkg/z.py", "def z():\n    pass\n"),
        ];
        let extractions: Vec<_> = files
            .iter()
            .map(|(path, source)| extract_file(path, source))
            .collect();
        let mut resolver = Resolver::new();
        resolver.index_extractions(&extractions);
        resolver.index_go_modules(&[
            module("", "example.com/app", &[("example.com/lib", "./local/lib")]),
            module("othersub", "example.com/app/sub", &[]),
            // A duplicate prefix of equal length, declared second.
            module("dup", "example.com/app/sub", &[]),
        ]);
        resolver
    }

    #[test]
    fn probe_extended() {
        let r = go_fixture();
        for spec in [
            "example.com/app",
            "example.com/app/x",
            "example.com/app/sub/inner",
            "example.com/app/internal/svc",
            "other.com/a/deep/nested/pkg",
            "other.com/q/x",
        ] {
            println!("spec {spec:?} -> {:?}", r.resolve_go_import(spec));
        }
        println!("dir_root -> {:?}", r.go_files_in_dir(""));
        println!("dir_internal -> {:?}", r.go_files_in_dir("internal"));
    }

    /// The Go import ladder resolves each tier, in order, and abstains at the end.
    ///
    /// `resolve_go_import` carried 23 surviving mutants — the largest single
    /// cluster in the workspace — including whole-body replacements with
    /// `vec![]` and `vec!["xyzzy"]`. It maps an import specifier to the files
    /// that satisfy it, so it is what makes a Go import edge point anywhere at
    /// all. Returning nothing severs every cross-package edge in a Go repo and
    /// leaves the imported package looking uncalled; returning the wrong files
    /// points the edge at a package that merely shares a path suffix, which is
    /// worse because it still looks resolved.
    ///
    /// Every expectation was read off the resolver before being pinned.
    #[test]
    fn the_go_import_ladder_resolves_each_tier_in_order() {
        let resolver = go_fixture();

        // Tier 0 — abstention. `C` is cgo's pseudo-package and has no files.
        assert!(resolver.resolve_go_import("").is_empty());
        assert!(resolver.resolve_go_import("   ").is_empty());
        assert!(
            resolver.resolve_go_import("C").is_empty(),
            "cgo's pseudo-package must resolve to nothing, not to a directory named C"
        );

        // A specifier arrives quoted from the Go grammar and must be trimmed.
        assert_eq!(
            resolver.resolve_go_import("\"example.com/app/internal/svc\""),
            ["internal/svc/a.go", "internal/svc/b.go"],
            "a quoted specifier resolves exactly as its bare form does"
        );

        // Tier 2 — module prefix. Test files are excluded from the surface.
        assert_eq!(
            resolver.resolve_go_import("example.com/app/internal/svc"),
            ["internal/svc/a.go", "internal/svc/b.go"],
            "an import under the module prefix resolves to that directory"
        );

        // Tier 1 — a `replace` directive outranks the module prefix.
        assert_eq!(
            resolver.resolve_go_import("example.com/lib"),
            ["local/lib/l.go"],
            "a replace directive redirects the import"
        );

        // Tier 3 — vendored copy.
        assert_eq!(
            resolver.resolve_go_import("example.com/ext"),
            ["vendor/example.com/ext/e.go"],
            "an import with no module match falls back to vendor/"
        );

        // Tier 4 — directory suffix, for repositories whose module path is not
        // reflected on disk.
        assert_eq!(
            resolver.resolve_go_import("other.com/x/deep/nested/pkg"),
            ["deep/nested/pkg/n.go"],
            "a directory that is a suffix of the import resolves it"
        );

        // Tier 4 requires at least two path components, so a single-segment
        // directory can never be claimed by an unrelated import.
        assert!(
            resolver.resolve_go_import("other.com/x/tool").is_empty(),
            "a one-component directory is too weak a signal to match on"
        );
        assert!(
            resolver
                .resolve_go_import("other.com/x/nested/pkg")
                .is_empty(),
            "a partial suffix must not match"
        );

        // A bare specifier with no slash is a standard-library package.
        assert!(
            resolver.resolve_go_import("fmt").is_empty(),
            "a stdlib import has no files in this repository"
        );
        assert!(resolver.resolve_go_import("pkg").is_empty());

        // The module root itself holds no Go files here.
        assert!(resolver.resolve_go_import("example.com/app").is_empty());
        assert!(resolver
            .resolve_go_import("unknown.com/nope/deep")
            .is_empty());
    }

    /// The module tier resolves what no weaker tier can.
    ///
    /// `x/` is a single-component directory, so the suffix tier is forbidden
    /// from claiming it and vendor holds nothing — the module prefix is the
    /// only tier that can resolve this import. That makes it the case that
    /// separates a working module tier from one that merely looks like it works
    /// because a later tier happens to reach the same files: both halves of the
    /// prefix test and the "did it find anything" check are only observable here.
    #[test]
    fn the_module_tier_resolves_what_no_weaker_tier_reaches() {
        let resolver = go_fixture();
        assert_eq!(
            resolver.resolve_go_import("example.com/app/x"),
            ["x/y.go"],
            "a one-component package under the module prefix resolves only via the \
             module tier"
        );
        assert!(
            resolver.resolve_go_import("other.com/q/x").is_empty(),
            "and without the prefix it must not resolve at all"
        );
    }

    /// The longest matching module prefix wins.
    ///
    /// Go repositories nest modules, and the inner module's directory need not
    /// match the path the outer module's prefix would produce. All three
    /// comparisons in the "is this a better match" test were mutable. If a
    /// shorter prefix wins, every import under the inner module is rewritten
    /// against the outer module's layout and lands on a directory that does not
    /// exist — the import silently resolves to nothing.
    #[test]
    fn the_longest_matching_module_prefix_wins() {
        let resolver = go_fixture();
        assert_eq!(
            resolver.resolve_go_import("example.com/app/sub/inner"),
            ["othersub/inner/i.go"],
            "the nested module maps `sub/` to `othersub/`; the outer module would \
             have produced `sub/inner`, which holds nothing"
        );
    }

    /// Only Go files contribute directories to the suffix tier.
    ///
    /// The suffix tier scans indexed paths for candidate directories, filtered
    /// to non-test `.go` files. Relaxing that filter to `||` lets any file's
    /// directory become a candidate — and because the tier keeps the *longest*
    /// match, a deeper directory holding no Go code at all outranks the real
    /// package and the import resolves to nothing. The fixture places a Python
    /// file at `a/deep/nested/pkg/`, a strictly longer suffix than the Go
    /// package at `deep/nested/pkg/`.
    #[test]
    fn only_go_files_contribute_suffix_tier_candidates() {
        let resolver = go_fixture();
        assert_eq!(
            resolver.resolve_go_import("other.com/a/deep/nested/pkg"),
            ["deep/nested/pkg/n.go"],
            "a longer non-Go directory must not outrank the real Go package"
        );
    }

    /// A file's Go package survives having its source stripped.
    ///
    /// `go_package_name_of` prefers the extracted `go_package` field and falls
    /// back to scanning source text. The whole function was replaceable with
    /// `None`, `Some("")` and `Some("xyzzy")`, and both emptiness filters were
    /// deletable. The fallback hides the important case: `for_durable_store`
    /// strips `source_code`, so after a reload the field is the *only* record of
    /// the package, and losing it breaks every package-level import edge on
    /// exactly the path a restarted daemon takes.
    #[test]
    fn a_go_package_name_survives_source_stripping() {
        use devmap_extract::extract_file;

        let extraction = extract_file("pkg/a.go", "package svc\nfunc A() {}\n");
        assert_eq!(
            go_package_name_of(&extraction).as_deref(),
            Some("svc"),
            "the package is read from the extracted field"
        );

        let durable = extraction.for_durable_store();
        assert!(durable.source_code.is_none(), "fixture precondition");
        assert_eq!(
            go_package_name_of(&durable).as_deref(),
            Some("svc"),
            "the package must survive a reload, where no source text remains"
        );

        let mut blank = extraction.clone();
        blank.go_package = None;
        blank.source_code = None;
        assert_eq!(go_package_name_of(&blank), None);

        let mut empty_clause = extraction.clone();
        empty_clause.go_package = Some(String::new());
        empty_clause.source_code = Some("package \npackage real\n".to_string());
        assert_eq!(
            go_package_name_of(&empty_clause).as_deref(),
            Some("real"),
            "an empty clause is skipped in favour of a real one, not returned as an \
             empty package name"
        );
    }

    /// cgo's pseudo-package never binds to a real directory.
    ///
    /// `import "C"` is not a package — it is the cgo directive, and the Go
    /// toolchain synthesises it. The early return is what guarantees that, and
    /// it is only observable when a directory of that name exists: the fixture
    /// vendors `vendor/C/`, which the vendor tier would otherwise happily
    /// resolve to. Binding every cgo import in a repository to one unrelated
    /// package is a confidently wrong edge, not a missing one.
    #[test]
    fn cgos_pseudo_package_never_binds_to_a_real_directory() {
        let resolver = go_fixture();
        assert_eq!(
            resolver.go_files_in_dir("vendor/C"),
            ["vendor/C/c.go"],
            "fixture precondition: a directory named C really is vendored"
        );
        assert!(
            resolver.resolve_go_import("C").is_empty(),
            "`import \"C\"` must resolve to nothing even when `vendor/C/` exists"
        );
        assert!(resolver.resolve_go_import("\"C\"").is_empty());
    }

    /// Equal-length module prefixes resolve deterministically, first declared.
    ///
    /// The "is this a better match" comparison was mutable from `>` to `>=`,
    /// which hands ties to the last module instead of the first. A monorepo can
    /// declare the same prefix twice with different directories; whichever rule
    /// applies, resolution must not depend on the order `go.mod` files happen
    /// to be walked in. `>` keeps the first declaration, and this pins it.
    #[test]
    fn equal_length_module_prefixes_resolve_deterministically() {
        let resolver = go_fixture();
        assert_eq!(
            resolver.resolve_go_import("example.com/app/sub/inner"),
            ["othersub/inner/i.go"],
            "on a tie the first declared module wins, not the last"
        );
    }

    /// A package's file set is exactly its own directory, minus tests.    /// A package's file set is exactly its own directory, minus tests.
    #[test]
    fn go_files_in_dir_is_exact_and_excludes_tests() {
        let resolver = go_fixture();

        assert_eq!(
            resolver.go_files_in_dir("internal/svc"),
            ["internal/svc/a.go", "internal/svc/b.go"],
            "`_test.go` files are not part of the importable package"
        );
        // A trailing slash names the same directory.
        assert_eq!(
            resolver.go_files_in_dir("internal/svc/"),
            resolver.go_files_in_dir("internal/svc")
        );
        // Matching is on the exact parent directory, not a path prefix, or a
        // parent package would absorb all of its children's files.
        assert!(
            resolver.go_files_in_dir("internal").is_empty(),
            "a parent directory holds no files of its own here"
        );
        assert!(resolver.go_files_in_dir("").is_empty());
    }

    /// An import edge points at the package node, except for `package main`.
    ///
    /// `go_import_edge_targets` was replaceable with `vec![]` and with a
    /// fabricated name. A Go import names a *package*, not a file, so the edge
    /// target is the synthetic package node — collapsing that to per-file edges
    /// multiplies every import edge by the package's file count, which is the
    /// fan-out SC10 removed. `main` is not importable, so it stays a file.
    #[test]
    fn go_import_edges_target_the_package_not_its_files() {
        let resolver = go_fixture();

        assert_eq!(
            resolver.go_import_edge_targets(&[
                "internal/svc/a.go".to_string(),
                "internal/svc/b.go".to_string(),
            ]),
            ["package:internal/svc/svc"],
            "two files of one package collapse to a single package node"
        );
        assert_eq!(
            resolver.go_import_edge_targets(&["cmd/tool/main.go".to_string()]),
            ["cmd/tool/main.go"],
            "`package main` is not importable and stays a file target"
        );
        assert!(resolver.go_import_edge_targets(&[]).is_empty());
    }

    /// Go visibility is decided by the leading character of the name.
    #[test]
    fn go_export_visibility_follows_the_leading_character() {
        assert!(Resolver::go_name_is_exported("A"));
        assert!(Resolver::go_name_is_exported("Exported"));
        assert!(!Resolver::go_name_is_exported("a"));
        assert!(!Resolver::go_name_is_exported("unexported"));
        assert!(!Resolver::go_name_is_exported("_private"));
        assert!(!Resolver::go_name_is_exported(""));
    }

    /// An import's local binding name is taken per-language.
    ///
    /// Go binds the last path segment (`github.com/x/y/pkg` → `pkg`); the
    /// dotted languages bind the last dotted segment. The `lang == "go"` test
    /// was mutable to `!=` without a failure, which swaps the two rules and
    /// makes every Go import bind to the whole specifier.
    #[test]
    fn import_local_names_follow_the_language() {
        assert_eq!(
            Resolver::import_local_name("go", "github.com/org/repo/pkg"),
            "pkg"
        );
        assert_eq!(Resolver::import_local_name("python", "a.b.c"), "c");
        assert_eq!(Resolver::import_local_name("typescript", "x.y"), "y");
        // The two rules must actually differ, or this test proves nothing.
        assert_ne!(
            Resolver::import_local_name("go", "a.b.c"),
            Resolver::import_local_name("python", "a.b.c")
        );
    }

    /// `replace` directives resolve longest-prefix-first.
    ///
    /// All three comparisons in the "is this a better match" test were mutable
    /// without a failure. A shorter prefix winning sends every import under the
    /// longer one to the wrong module — silently, since both targets exist.
    #[test]
    fn go_replace_prefers_the_longest_matching_prefix() {
        let mut resolver = Resolver::new();
        resolver.index_go_modules(&[module(
            "",
            "example.com/main",
            &[
                ("example.com/lib", "./vendor/lib"),
                ("example.com/lib/inner", "./vendor/inner"),
            ],
        )]);

        // `apply_go_replace` returns (original spec, replacement target); the
        // replacement is the second element.
        let (_, specific) = resolver.apply_go_replace("example.com/lib/inner/pkg");
        assert_eq!(
            specific.as_deref(),
            Some("vendor/inner/pkg"),
            "the longer prefix must win"
        );

        let (_, general) = resolver.apply_go_replace("example.com/lib/other");
        assert_eq!(
            general.as_deref(),
            Some("vendor/lib/other"),
            "a path only the shorter prefix matches must still resolve"
        );

        // A specifier matching neither prefix yields no replacement at all.
        let (spec, untouched) = resolver.apply_go_replace("other.com/thing");
        assert_eq!(spec, "other.com/thing");
        assert_eq!(untouched, None, "a non-matching spec must not be rewritten");
    }

    /// Equal-length prefixes resolve deterministically to the first declared.
    ///
    /// The longest-prefix comparison is strict (`>`), so a later replace of the
    /// *same* path does not displace an earlier one. Relaxing it to `>=` made
    /// the winner depend on module iteration order — two `go.mod` files
    /// replacing the same module path would resolve differently between builds,
    /// which R4 forbids outright.
    #[test]
    fn equal_length_replace_prefixes_resolve_to_the_first_declared() {
        let mut resolver = Resolver::new();
        resolver.index_go_modules(&[
            module("", "m1", &[("example.com/dup", "./first")]),
            module("sub", "m2", &[("example.com/dup", "./second")]),
        ]);

        let (_, target) = resolver.apply_go_replace("example.com/dup");
        assert_eq!(
            target.as_deref(),
            Some("first"),
            "the first declared replace of a path must win, not the last"
        );

        // Repeat: the answer must not depend on iteration order across calls.
        for _ in 0..3 {
            let (_, again) = resolver.apply_go_replace("example.com/dup");
            assert_eq!(again.as_deref(), Some("first"), "resolution must be stable");
        }
    }

    /// A local `replace` target is recognised by `./` or `/`, and both matter.
    ///
    /// The `starts_with('.') || starts_with('/')` test was mutable to `&&`,
    /// which requires a target to begin with both characters at once — so every
    /// local replacement would be treated as a remote module path instead.
    #[test]
    fn local_replace_targets_are_recognised_by_either_marker() {
        let mut dot = Resolver::new();
        dot.index_go_modules(&[module("", "m", &[("example.com/a", "./local/a")])]);
        let (_, dot_target) = dot.apply_go_replace("example.com/a");
        assert_eq!(
            dot_target.as_deref(),
            Some("local/a"),
            "a `./` target must resolve to a repo-relative path"
        );

        let mut slash = Resolver::new();
        slash.index_go_modules(&[module("", "m", &[("example.com/b", "/abs/b")])]);
        let (_, slash_target) = slash.apply_go_replace("example.com/b");
        assert!(
            slash_target
                .as_deref()
                .is_some_and(|target| !target.starts_with("example.com")),
            "an absolute target must resolve to a path, got {slash_target:?}"
        );

        // A module-path target (neither marker) stays a module path.
        let mut remote = Resolver::new();
        remote.index_go_modules(&[module("", "m", &[("example.com/c", "other.com/c")])]);
        let (_, remote_target) = remote.apply_go_replace("example.com/c");
        assert_eq!(
            remote_target.as_deref(),
            Some("other.com/c"),
            "a module-path target must remain a module path"
        );
    }
}

#[cfg(test)]
mod ladder_tests {
    use super::*;
    use devmap_extract::extract_file;

    fn indexed(files: &[(&str, &str)]) -> (Resolver, Vec<devmap_extract::model::Extraction>) {
        let extractions: Vec<_> = files
            .iter()
            .map(|(path, source)| extract_file(path, source))
            .collect();
        let mut resolver = Resolver::new();
        resolver.index_extractions(&extractions);
        (resolver, extractions)
    }

    /// Go same-package lookup is scoped to one directory, excludes tests, and
    /// abstains when ambiguous.
    ///
    /// Every clause of the filter was mutable without a failure. Widening any
    /// of them resolves a call to a same-named function in an unrelated package
    /// — a confidently wrong edge, since nothing downstream can tell the
    /// difference.
    #[test]
    fn go_package_lookup_is_directory_scoped_and_abstains_when_ambiguous() {
        let (resolver, _) = indexed(&[
            ("pkg/a.go", "package pkg\nfunc Helper() {}\n"),
            ("pkg/b.go", "package pkg\nfunc Caller() {}\n"),
            ("other/c.go", "package other\nfunc Elsewhere() {}\n"),
            ("pkg/a_test.go", "package pkg\nfunc OnlyInTest() {}\n"),
        ]);

        // A sibling file in the same directory resolves.
        assert_eq!(
            resolver.lookup_in_package("pkg/b.go", "Helper"),
            Some(("pkg/a.go".to_string(), "Helper".to_string())),
            "a same-package sibling must resolve"
        );

        // A different directory is a different package and must not.
        assert_eq!(
            resolver.lookup_in_package("pkg/b.go", "Elsewhere"),
            None,
            "a symbol in another directory is a different package"
        );

        // Test files are not part of the importable package surface.
        assert_eq!(
            resolver.lookup_in_package("pkg/b.go", "OnlyInTest"),
            None,
            "a symbol declared only in a _test.go file must not resolve"
        );

        // Non-Go files get no package lookup at all.
        assert_eq!(
            resolver.lookup_in_package("app.py", "Helper"),
            None,
            "package lookup is Go-only"
        );
    }

    #[test]
    fn go_package_lookup_abstains_when_two_files_declare_the_name() {
        let (resolver, _) = indexed(&[
            ("pkg/a.go", "package pkg\nfunc Dup() {}\n"),
            ("pkg/b.go", "package pkg\nfunc Dup() {}\n"),
            ("pkg/c.go", "package pkg\nfunc Caller() {}\n"),
        ]);
        assert_eq!(
            resolver.lookup_in_package("pkg/c.go", "Dup"),
            None,
            "two candidates in one package must abstain, not pick one"
        );
    }

    /// A symbol declared in the querying file wins outright.
    #[test]
    fn go_package_lookup_prefers_the_querying_file() {
        let (resolver, _) = indexed(&[
            ("pkg/a.go", "package pkg\nfunc Same() {}\n"),
            ("pkg/b.go", "package pkg\nfunc Same() {}\n"),
        ]);
        assert_eq!(
            resolver.lookup_in_package("pkg/a.go", "Same"),
            Some(("pkg/a.go".to_string(), "Same".to_string())),
            "the querying file's own symbol must win before any package scan"
        );
    }

    /// The Python stdlib guard is scoped to Python and to cross-file targets.
    ///
    /// `open`, `len`, `type` and friends are overwhelmingly the builtins, not a
    /// same-named helper in another module, so resolving them across files
    /// manufactures edges. Both halves of the guard were mutable: dropping the
    /// language test applies it to Go and TypeScript too, and dropping the
    /// `target != self` test suppresses a *genuine* same-file definition.
    #[test]
    fn python_stdlib_names_do_not_resolve_across_files_but_do_within_one() {
        // Cross-file: `open` defined in another module must NOT be reached.
        let (cross, cross_exts) = indexed(&[
            (
                "caller.py",
                "import helpers\n\ndef go():\n    return open('x')\n",
            ),
            ("helpers.py", "def open(path):\n    return path\n"),
        ]);
        let result = cross.resolve_all(&cross_exts);
        assert!(
            !result.edges.iter().any(|edge| {
                edge.source_file == "caller.py"
                    && edge.target_file == "helpers.py"
                    && edge.target_symbol.ends_with("open")
                    && edge.confidence == Confidence::DETERMINISTIC
            }),
            "a Python builtin name must not resolve confidently to another \
             module's same-named function: {:?}",
            result
                .edges
                .iter()
                .map(|e| (&e.source_symbol, &e.target_symbol, e.confidence.0))
                .collect::<Vec<_>>()
        );

        // Same file: a real local definition of `open` must still resolve.
        let (same, same_exts) = indexed(&[(
            "local.py",
            "def open(path):\n    return path\n\ndef go():\n    return open('x')\n",
        )]);
        let local = same.resolve_all(&same_exts);
        assert!(
            local.edges.iter().any(|edge| {
                edge.edge_kind == EdgeKind::Calls && edge.target_symbol.ends_with("open")
            }),
            "a same-file definition must still resolve, guard or not: {:?}",
            local
                .edges
                .iter()
                .map(|e| (&e.source_symbol, &e.target_symbol))
                .collect::<Vec<_>>()
        );

        // The guard is Python-only: a Go function named `open` is ordinary.
        let (go, go_exts) = indexed(&[
            (
                "pkg/a.go",
                "package pkg\nfunc open(p string) string { return p }\n",
            ),
            (
                "pkg/b.go",
                "package pkg\nfunc use() string { return open(\"x\") }\n",
            ),
        ]);
        let go_result = go.resolve_all(&go_exts);
        assert!(
            go_result
                .edges
                .iter()
                .any(|edge| edge.edge_kind == EdgeKind::Calls
                    && edge.target_symbol.ends_with("open")),
            "the stdlib guard must not apply outside Python: {:?}",
            go_result
                .edges
                .iter()
                .map(|e| (&e.source_symbol, &e.target_symbol))
                .collect::<Vec<_>>()
        );
    }
}

#[cfg(test)]
mod reference_resolution_tests {
    use super::*;
    use devmap_extract::extract_file;

    fn resolve(files: &[(&str, &str)]) -> ResolutionResult {
        let extractions: Vec<_> = files
            .iter()
            .map(|(path, source)| extract_file(path, source))
            .collect();
        let mut resolver = Resolver::new();
        resolver.index_extractions(&extractions);
        resolver.resolve_all(&extractions)
    }

    fn reference_targets(result: &ResolutionResult, name: &str) -> Vec<String> {
        result
            .edges
            .iter()
            .filter(|edge| {
                edge.edge_kind == EdgeKind::References && edge.target_symbol.ends_with(name)
            })
            .map(|edge| edge.target_file.clone())
            .collect()
    }

    fn edge_rows(result: &ResolutionResult) -> Vec<String> {
        let mut rows: Vec<String> = result
            .edges
            .iter()
            .filter(|edge| {
                edge.edge_kind == EdgeKind::References || edge.edge_kind == EdgeKind::Calls
            })
            .map(|edge| format!("{}->{}", edge.source_symbol, edge.target_symbol))
            .collect();
        rows.sort();
        rows
    }

    fn edges_of(result: &ResolutionResult, kinds: &[EdgeKind]) -> Vec<String> {
        let mut rows: Vec<String> = result
            .edges
            .iter()
            .filter(|edge| kinds.contains(&edge.edge_kind))
            .map(|edge| format!("{}->{}", edge.source_symbol, edge.target_symbol))
            .collect();
        rows.sort();
        rows
    }

    /// Containment names the file for every symbol, and the type for its methods.
    ///
    /// `Contains` edges are the structural skeleton of every graph — the
    /// majority of edges in the frozen baseline. Two guards shape them and both
    /// were mutable: the file symbol must not contain itself, and the second,
    /// type-owned edge must be emitted only when the parent is a real type
    /// rather than the file. Inverting the latter drops every
    /// `type contains method` edge and duplicates the file edge in its place,
    /// so a method stops being reachable through its type.
    #[test]
    fn containment_names_the_file_and_the_declaring_type() {
        let result = resolve(&[(
            "a.py",
            "class C:\n    def m(self):\n        pass\n\ndef f():\n    pass\n",
        )]);
        assert_eq!(
            edges_of(&result, &[EdgeKind::Contains]),
            [
                "a.py->a.py::C",
                "a.py->a.py::C.m",
                "a.py->a.py::f",
                "a.py::C->a.py::C.m",
            ],
            "the file contains every declared symbol, the class additionally \
             contains its method, and the file never contains itself"
        );
    }

    /// Go package membership groups non-main packages only.
    ///
    /// The G20 star topology hangs every file of a package off one synthetic
    /// package node. Both guards were mutable: dropping the family test builds
    /// package groups for languages that have no packages, and inverting the
    /// `main` test groups exactly the one package that is not importable while
    /// leaving the importable ones ungrouped.
    #[test]
    fn go_package_membership_groups_non_main_packages_only() {
        let package = resolve(&[
            ("pkg/a.go", "package pkg\nfunc A() {}\n"),
            ("pkg/b.go", "package pkg\nfunc B() {}\n"),
        ]);
        assert_eq!(
            edges_of(&package, &[EdgeKind::MemberOf]),
            ["pkg/a.go->package:pkg/pkg", "pkg/b.go->package:pkg/pkg",],
            "both files of a package hang off one package node"
        );

        // `package main` is not importable, so it is never grouped.
        let main = resolve(&[("cmd/m.go", "package main\nfunc main() {}\n")]);
        assert!(
            edges_of(&main, &[EdgeKind::MemberOf]).is_empty(),
            "`package main` must not be grouped"
        );

        // A language without packages produces no membership at all.
        let python = resolve(&[("a.py", "def f():\n    pass\n")]);
        assert!(
            edges_of(&python, &[EdgeKind::MemberOf]).is_empty(),
            "package grouping is Go-only"
        );
    }

    /// A Go import edge names the package, not each of its files.
    ///
    /// The Go branch collapses the resolved file list to package nodes. Losing
    /// it emits one import edge per file in the imported package, which is the
    /// fan-out SC10 removed — the edge count grows with the size of the
    /// imported package rather than with the number of imports.
    #[test]
    fn a_go_import_edge_names_the_package_not_each_file() {
        let result = resolve(&[
            (
                "app/main.go",
                "package main\nimport \"example.com/m/internal/svc\"\nfunc main() { svc.Do() }\n",
            ),
            ("internal/svc/s.go", "package svc\nfunc Do() {}\n"),
            ("internal/svc/t.go", "package svc\nfunc Other() {}\n"),
        ]);
        assert_eq!(
            edges_of(&result, &[EdgeKind::Imports]),
            ["app/main.go->package:internal/svc/svc"],
            "a two-file package yields one import edge, not two"
        );
    }

    /// A same-file callee wins outright; an ambiguous global one is speculative.
    ///
    /// The same-file uniqueness test was mutable, which sends a call that has a
    /// local definition out to the global tier instead — where the same name in
    /// another file makes it ambiguous. The confidence tiers are the point: a
    /// call resolved in its own file is DETERMINISTIC, a unique global match is
    /// HIGH, and a multi-candidate guess must stay SPECULATIVE. G5 exists so a
    /// guess is never presented with the confidence of a fact.
    #[test]
    fn call_confidence_reflects_how_the_callee_was_found() {
        let same_file = resolve(&[
            (
                "s.py",
                "def helper():\n    pass\n\ndef use():\n    return helper()\n",
            ),
            ("other.py", "def helper():\n    pass\n"),
        ]);
        let same_file_edges: Vec<_> = same_file
            .edges
            .iter()
            .filter(|edge| edge.edge_kind == EdgeKind::Calls)
            .collect();
        assert_eq!(
            same_file_edges.len(),
            1,
            "a local definition wins outright, even though another file declares \
             the same name: {:?}",
            edges_of(&same_file, &[EdgeKind::Calls])
        );
        assert_eq!(same_file_edges[0].target_file, "s.py");
        assert_eq!(
            same_file_edges[0].confidence,
            Confidence::DETERMINISTIC,
            "a same-file callee is a fact, not an inference"
        );

        // A unique global match is strong but not certain.
        let unique = resolve(&[
            ("u.py", "def use():\n    return only()\n"),
            ("x.py", "def only():\n    pass\n"),
        ]);
        let unique_edges: Vec<_> = unique
            .edges
            .iter()
            .filter(|edge| edge.edge_kind == EdgeKind::Calls)
            .collect();
        assert_eq!(unique_edges.len(), 1);
        assert_eq!(unique_edges[0].confidence, Confidence::HIGH);

        // Several candidates must all stay speculative — never HIGH.
        let ambiguous = resolve(&[
            ("u.py", "def use():\n    return shared()\n"),
            ("x.py", "def shared():\n    pass\n"),
            ("y.py", "def shared():\n    pass\n"),
        ]);
        let ambiguous_edges: Vec<_> = ambiguous
            .edges
            .iter()
            .filter(|edge| edge.edge_kind == EdgeKind::Calls)
            .collect();
        assert_eq!(ambiguous_edges.len(), 2, "both candidates are recorded");
        for edge in &ambiguous_edges {
            assert_eq!(
                edge.confidence,
                Confidence::SPECULATIVE,
                "a multi-candidate guess must never carry HIGH confidence (G5)"
            );
        }
    }

    /// A route binds to its handler only when the handler is unambiguous and
    /// in the same language family.
    ///
    /// A `HandlesRoute` edge is what tells the dead-code pass that a handler is
    /// reached from outside the call graph. Without it the handler has no
    /// caller and reads as dead; bound to the wrong function, a genuinely dead
    /// one is protected instead.
    ///
    /// Every framework whose handler can be named is covered here: axum names
    /// it in the route call, FastAPI/Flask name it by decoration, and Express
    /// names it in the argument list. An Express handler written as an arrow
    /// function is genuinely anonymous and correctly binds to nothing.
    #[test]
    fn a_route_binds_only_to_an_unambiguous_same_family_handler() {
        let route_file = (
            "srv.rs",
            "fn app() -> Router { Router::new().route(\"/items\", get(list_items)) }\n",
        );

        let unique = resolve(&[route_file, ("h.rs", "pub async fn list_items() {}\n")]);
        assert_eq!(
            edges_of(&unique, &[EdgeKind::HandlesRoute]),
            ["GET /items->list_items"],
            "a unique handler binds to its route"
        );

        // Two candidates: binding either one would be a guess.
        let ambiguous = resolve(&[
            route_file,
            ("h.rs", "pub async fn list_items() {}\n"),
            ("h2.rs", "pub async fn list_items() {}\n"),
        ]);
        assert!(
            edges_of(&ambiguous, &[EdgeKind::HandlesRoute]).is_empty(),
            "two candidate handlers must produce no edge rather than a guess: {:?}",
            edges_of(&ambiguous, &[EdgeKind::HandlesRoute])
        );

        // A same-named function in another language is not a candidate at all.
        let cross_family = resolve(&[route_file, ("h.py", "def list_items():\n    pass\n")]);
        assert!(
            edges_of(&cross_family, &[EdgeKind::HandlesRoute]).is_empty(),
            "a Rust route must not bind to a Python function of the same name"
        );

        // FastAPI/Flask: the handler is the decorated function.
        let python = resolve(&[(
            "api.py",
            "@app.get('/items')\ndef read_items():\n    return []\n",
        )]);
        assert_eq!(
            edges_of(&python, &[EdgeKind::HandlesRoute]),
            ["GET /items->read_items"],
            "a decorated Python handler binds to its route"
        );

        // Express: the handler is the last argument, and resolves across files
        // through the import binding.
        let express = resolve(&[(
            "app.js",
            "app.get('/users', handleUsers);\nfunction handleUsers(req, res) {}\n",
        )]);
        assert_eq!(
            edges_of(&express, &[EdgeKind::HandlesRoute]),
            ["GET /users->handleUsers"]
        );
        let imported = resolve(&[
            (
                "app.js",
                "import { handleUsers } from './h';\napp.get('/users', handleUsers);\n",
            ),
            ("h.js", "export function handleUsers(req, res) {}\n"),
        ]);
        assert_eq!(
            edges_of(&imported, &[EdgeKind::HandlesRoute]),
            ["GET /users->handleUsers"],
            "an imported handler resolves through its import binding"
        );

        // An anonymous handler has no name, so it binds to nothing rather than
        // to whatever symbol a placeholder happened to match.
        let anonymous = resolve(&[("app.js", "app.get('/a', (req, res) => {});\n")]);
        assert!(
            edges_of(&anonymous, &[EdgeKind::HandlesRoute]).is_empty(),
            "an arrow-function handler is anonymous and binds to nothing"
        );
    }

    /// A `module.function()` call resolves through the module's import binding.
    ///
    /// The receiver/method split feeds a lookup of the receiver in this file's
    /// import bindings, then of the method inside that module. Both emptiness
    /// guards were mutable. This is how the most common cross-file call shape
    /// in Python, TypeScript and Go resolves at all — losing it leaves every
    /// `mod.fn()` call unresolved, and the callee with no callers.
    #[test]
    fn a_module_qualified_call_resolves_through_its_import_binding() {
        let python = resolve(&[
            (
                "app.py",
                "import helpers\n\ndef run():\n    return helpers.do()\n",
            ),
            ("helpers.py", "def do():\n    return 1\n"),
        ]);
        assert_eq!(
            edges_of(&python, &[EdgeKind::Calls]),
            ["app.py::run->helpers.py::do"],
            "`helpers.do()` resolves through the `import helpers` binding"
        );

        let typescript = resolve(&[
            (
                "u.ts",
                "import * as ns from './h';\nexport function run() { return ns.doIt(); }\n",
            ),
            ("h.ts", "export function doIt() { return 1; }\n"),
        ]);
        assert_eq!(
            edges_of(&typescript, &[EdgeKind::Calls]),
            ["u.ts::run->h.ts::doIt"],
            "a namespace import binds the receiver too"
        );

        let go = resolve(&[
            (
                "app/main.go",
                "package main\nimport \"example.com/m/internal/svc\"\nfunc main() { svc.Do() }\n",
            ),
            ("internal/svc/s.go", "package svc\nfunc Do() {}\n"),
        ]);
        assert_eq!(
            edges_of(&go, &[EdgeKind::Calls]),
            ["app/main.go::main->internal/svc/s.go::Do"],
            "a Go package-qualified call resolves into the imported package"
        );
    }

    /// Rust `self::` and `super::` resolve relative to the module's own file.
    ///
    /// The guard covering this branch was mutable. `self::` stays in the current
    /// module directory and `super::` walks up one per prefix — getting either
    /// wrong sends the import to a directory that does not exist, so it silently
    /// resolves to nothing.
    #[test]
    fn rust_self_and_super_paths_resolve_relative_to_their_module() {
        use devmap_extract::extract_file;
        let files = [
            ("src/deep/mod.rs", "pub mod leaf;\n"),
            ("src/deep/leaf.rs", "pub fn l() {}\n"),
            ("src/sibling.rs", "pub fn s() {}\n"),
        ];
        let extractions: Vec<_> = files
            .iter()
            .map(|(path, source)| extract_file(path, source))
            .collect();
        let mut resolver = Resolver::new();
        resolver.index_extractions(&extractions);

        assert_eq!(
            resolver
                .resolve_import_path("src/deep/mod.rs", "rust", "self::leaf")
                .as_deref(),
            Some("src/deep/leaf.rs"),
            "`self::` stays inside the current module directory"
        );
        assert_eq!(
            resolver
                .resolve_import_path("src/deep/mod.rs", "rust", "super::sibling")
                .as_deref(),
            Some("src/sibling.rs"),
            "`super::` walks up one directory"
        );
        // The guard is language-scoped: this is not Python module syntax.
        assert_eq!(
            resolver.resolve_import_path("src/deep/mod.rs", "python", "self::leaf"),
            None
        );
    }

    /// A Python builtin is never captured by a same-named user function    /// A Python builtin is never captured by a same-named user function    /// A Python builtin is never captured by a same-named user function    /// A Python builtin is never captured by a same-named user function
    /// elsewhere in the repository.
    ///
    /// Every clause of the stdlib guard was mutable. `open`, `print`, `len` and
    /// friends appear in almost every Python file, so without the guard a
    /// single user-defined `open` anywhere in the repository captures *all* of
    /// them — one function acquires thousands of false callers, and it can
    /// never be reported dead again. The guard is deliberately scoped to other
    /// files: a same-file definition really does shadow the builtin.
    #[test]
    fn python_builtins_are_not_captured_by_a_same_named_user_function() {
        let cross_file = resolve(&[
            ("app.py", "def run(p):\n    return open(p)\n"),
            ("helpers.py", "def open(p):\n    return p\n"),
        ]);
        assert!(
            edge_rows(&cross_file).is_empty(),
            "a builtin call must not bind to another file's same-named function: {:?}",
            edge_rows(&cross_file)
        );

        // Positive control: shadowing in the *same* file is real shadowing.
        let same_file = resolve(&[(
            "app.py",
            "def open(p):\n    return p\n\ndef run(p):\n    return open(p)\n",
        )]);
        assert_eq!(
            edge_rows(&same_file),
            ["app.py::run->app.py::open"],
            "a definition in the same file does shadow the builtin"
        );
    }

    /// An ambiguous name in one file resolves to nothing rather than to a guess.
    ///
    /// Both the same-file uniqueness test and `symbol_kind_in`'s were mutable,
    /// and `symbol_kind_in` was replaceable with `None` outright. When a file
    /// declares the same name twice there is no way to tell which one a
    /// reference means; picking either produces a confidently wrong edge, and
    /// the wrong one also makes a genuinely dead symbol look live.
    #[test]
    fn an_ambiguous_same_file_name_resolves_to_nothing() {
        let ambiguous = resolve(&[(
            "a.py",
            "class Dup:\n    pass\n\ndef Dup():\n    pass\n\ndef use(x: Dup):\n    return x\n",
        )]);
        assert!(
            edge_rows(&ambiguous).is_empty(),
            "a name declared twice in one file must not resolve: {:?}",
            edge_rows(&ambiguous)
        );

        // Positive control: with one declaration the same reference resolves,
        // so the abstention above is about ambiguity and not about the fixture.
        let unique = resolve(&[(
            "b.py",
            "class Dup:\n    pass\n\ndef use(x: Dup):\n    return x\n",
        )]);
        assert!(
            !edge_rows(&unique).is_empty(),
            "one declaration must resolve, or the ambiguity test proves nothing"
        );
    }

    /// A type position prefers a type over a same-named value.
    ///
    /// The `prefer_types` guards appear in both the same-file and global tiers
    /// and were mutable in every direction. A type annotation that binds to a
    /// same-named *function* is a confidently wrong edge, and it also leaves
    /// the real type looking unreferenced.
    #[test]
    fn a_type_position_prefers_a_type_over_a_same_named_value() {
        let result = resolve(&[
            ("t.ts", "export class Shape {}\n"),
            ("f.ts", "export function Shape() {}\n"),
            (
                "u.ts",
                "import { Shape } from './t';\nexport function use(s: Shape) { return s; }\n",
            ),
        ]);
        assert_eq!(
            edge_rows(&result),
            ["u.ts::use->t.ts::Shape"],
            "the annotation must bind to the class, never to the same-named function"
        );
    }

    /// A reference resolves only inside its own language family.    /// A reference resolves only inside its own language family.
    ///
    /// The family test was mutable to `!=` without a failure. Dropping it lets
    /// a Python annotation resolve to an identically-named Go type — a
    /// confidently wrong cross-language edge.
    #[test]
    fn references_do_not_resolve_across_language_families() {
        let result = resolve(&[
            (
                "app.py",
                "from models import Record\n\ndef use(r: Record):\n    return r\n",
            ),
            ("models.py", "class Record:\n    pass\n"),
            ("pkg/record.go", "package pkg\ntype Record struct{}\n"),
        ]);
        let targets = reference_targets(&result, "Record");
        assert!(
            !targets.iter().any(|file| file.ends_with(".go")),
            "a Python reference must never resolve to a Go type: {targets:?}"
        );
    }

    /// Go visibility is enforced on references across packages.
    ///
    /// The `family != Go || visible_from(..)` guard was mutable in both
    /// directions. Dropping it resolves an unexported identifier from another
    /// package, which the Go compiler would reject outright.
    #[test]
    fn unexported_go_symbols_are_invisible_from_another_package() {
        let result = resolve(&[
            ("pkg/a.go", "package pkg\ntype hidden struct{}\n"),
            ("other/b.go", "package other\nfunc use(h hidden) {}\n"),
        ]);
        let targets = reference_targets(&result, "hidden");
        assert!(
            targets.is_empty(),
            "an unexported Go type must not be referenced from another package: {targets:?}"
        );

        // Exported and same-package references must still work.
        let visible = resolve(&[
            ("pkg/a.go", "package pkg\ntype Shown struct{}\n"),
            ("pkg/b.go", "package pkg\nfunc use() { var _ Shown }\n"),
        ]);
        assert!(
            !reference_targets(&visible, "Shown").is_empty(),
            "a same-package Go type must resolve: {:?}",
            visible
                .edges
                .iter()
                .map(|e| (&e.source_symbol, &e.target_symbol))
                .collect::<Vec<_>>()
        );
    }
}
