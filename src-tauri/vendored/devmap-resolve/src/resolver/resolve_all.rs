use serde::{Deserialize, Serialize};

use super::*;

/// Cache identity for per-file resolution reuse.
///
/// The blobs live in the repository state directory, not in sqlite. A resolver
/// change that alters a file's edges without changing its content hash bumps
/// this; a mismatch is a miss, and the file is resolved again.
pub const RESOLUTION_REUSE_EPOCH: &str = "1";

/// One file's resolution before the whole-tree post-pass.
///
/// Package `MemberOf` edges and the inert-value rewrite read every file
/// together, so a reused file is stored as this tuple and the pass runs again
/// on the union. `UnresolvedReference::site` is not serialized; a
/// language-server enrich must not read these blobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReusedFileResolution {
    pub content_hash: u64,
    pub edges: Vec<ResolvedEdge>,
    pub unresolved: Vec<UnresolvedReference>,
    pub package_groups: BTreeMap<String, BTreeSet<String>>,
}

/// What the global candidate cache charged during one pass.
///
/// A reuse pass does not charge files it returned from the cache. The caller
/// adds this to the last full resolve's usage and resolves every file when the
/// sum can exceed the budget a cold resolve would have charged.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolutionBudgetUsage {
    pub visits: u64,
    pub retained_bytes: u64,
    pub evidence_bytes: u64,
}

/// Limits and charges of the last full resolve, stored beside the per-file blobs.
///
/// A reuse pass does not charge the files it skipped. The next build adds this
/// receipt's charges to the files it does resolve and, when the sum can exceed
/// a limit, resolves every file instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReuseBudgetReceipt {
    pub candidate_visits: u64,
    pub retained_candidate_bytes: u64,
    pub ambiguity_evidence_bytes: u64,
    pub degraded_candidates: usize,
    pub refuse: bool,
    pub usage: ResolutionBudgetUsage,
}

/// A resolution plus the per-file tuples this pass computed itself.
///
/// `produced` omits files taken from `reused`, so the caller persists only the
/// work it just did.
pub struct ResolveReuse {
    pub result: ResolutionResult,
    pub produced: BTreeMap<String, ReusedFileResolution>,
    pub budget: ResolutionBudgetUsage,
}

impl Resolver {
    pub fn resolve_all(
        &self,
        extractions: &[Extraction],
    ) -> Result<ResolutionResult, ResolutionLimitError> {
        self.resolve_all_with_limits(extractions, ResolutionLimits::default())
    }

    /// Resolve every file under `limits`.
    ///
    /// A budget that is exceeded does not abort the build under
    /// [`LimitPolicy::Degrade`] (the default). The first pass runs in parallel
    /// and treats a budget as an error, which costs nothing on every tree that
    /// stays inside its budgets; if one trips, the whole resolution runs again
    /// on a single thread, with the budgets degrading the offending candidate
    /// sets instead — one thread because *which* sets a shared budget cuts is
    /// decided by the order they are charged in, and a degraded graph must be
    /// the same graph on every run. Under [`LimitPolicy::Refuse`] the first
    /// pass's error is returned as it always was.
    pub fn resolve_all_with_limits(
        &self,
        extractions: &[Extraction],
        limits: ResolutionLimits,
    ) -> Result<ResolutionResult, ResolutionLimitError> {
        self.resolve_all_with_limits_reusing(extractions, limits, &BTreeMap::new())
            .map(|reuse| reuse.result)
    }

    /// Resolve `extractions`, returning a cached file unchanged when `reused`
    /// holds that path and the same content hash.
    ///
    /// An empty `reused` is [`Self::resolve_all_with_limits`]. A hash mismatch
    /// is a miss and the file is resolved. The post-pass still runs over every
    /// file, reused or not, so analysis receives the full edge set. The serial
    /// retry after a budget refusal sees the same `reused` map: which files
    /// were cached must not depend on the thread count.
    pub fn resolve_all_with_limits_reusing(
        &self,
        extractions: &[Extraction],
        limits: ResolutionLimits,
        reused: &BTreeMap<String, ReusedFileResolution>,
    ) -> Result<ResolveReuse, ResolutionLimitError> {
        match self.resolve_pass(extractions, limits, false, reused) {
            Err(refusal) if limits.on_exceed == LimitPolicy::Degrade => {
                // A pool that cannot be built cannot give the deterministic
                // order, so the budget's own refusal stands rather than a
                // degraded graph that depends on scheduling.
                let Ok(serial) = rayon::ThreadPoolBuilder::new().num_threads(1).build() else {
                    return Err(refusal);
                };
                serial.install(|| self.resolve_pass(extractions, limits, true, reused))
            }
            outcome => outcome,
        }
    }

    fn resolve_pass(
        &self,
        extractions: &[Extraction],
        limits: ResolutionLimits,
        degrade: bool,
        reused: &BTreeMap<String, ReusedFileResolution>,
    ) -> Result<ResolveReuse, ResolutionLimitError> {
        let global_candidates = Mutex::new(GlobalCandidateCache {
            degrade,
            ..Default::default()
        });
        // Per-file resolution runs in parallel.
        //
        // Sound because the loop body below reads only `self` — the symbol and
        // type indexes, both immutable and fully built before this point — and
        // writes only its own file's output. No iteration observes another's
        // edges, and `self` is never mutated. That was checked, not assumed.
        //
        // Determinism survives because emission order was never load-bearing:
        // `edges` is totally ordered by the sort below (R4), which exists
        // precisely so the result cannot depend on iteration order.
        // `par_iter().map().collect()` into a `Vec` preserves input order
        // anyway, so the merge below is the same sequence the serial loop
        // produced, and `package_groups` is merged into a `BTreeMap` whose
        // ordering is by key.
        //
        // A changed build used to resolve every file. Analysis still receives
        // every edge — a subset is the 433-versus-14 dead-code failure — but
        // a file whose content hash matches a `reused` entry is not walked
        // again. Indexing still covers the whole tree; this loop is the part
        // that grew with repository size while extraction was cached.
        type FileResolution = (
            Vec<ResolvedEdge>,
            Vec<UnresolvedReference>,
            BTreeMap<String, BTreeSet<String>>,
        );
        let per_file: Vec<FileResolution> = extractions
            .par_iter()
            .map(|ext| -> Result<FileResolution, ResolutionLimitError> {
                if let Some(cached) = reused.get(&ext.file_path) {
                    if cached.content_hash == ext.content_hash {
                        return Ok((
                            cached.edges.clone(),
                            cached.unresolved.clone(),
                            cached.package_groups.clone(),
                        ));
                    }
                }
                let mut local_candidates: BTreeMap<(&str, bool), Arc<GlobalCandidates>> = BTreeMap::new();
                let mut edges: Vec<ResolvedEdge> = Vec::new();
                let mut unresolved: Vec<UnresolvedReference> = Vec::new();
                let mut package_groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
                let family = LangFamily::from_lang(&ext.language);
                // The scope a Python statement is read in. A call in a function
                // names it; one in a class body is recorded with no caller, and
                // its span says which class body it is written in; one outside
                // every function and class is read at module scope.
                let py_call_scopes = (family == LangFamily::Python)
                    .then(|| pytypes::python_scope_containers(ext));
                let read_scope = |caller: Option<&'_ str>, span: &Span| -> Option<String> {
                    caller.map(str::to_string).or_else(|| {
                        py_call_scopes.as_ref().and_then(|scopes| {
                            pytypes::python_innermost_container(scopes, span)
                                .map(|(_, scope)| scope.to_string())
                        })
                    })
                };

                // Lexical containment. The file owns every symbol declared in it —
                // including nested methods — and a type additionally owns its own
                // methods, which is exactly the frozen baseline's `contains` shape.
                for sym in &ext.symbols {
                    if sym.kind == SymbolKind::File {
                        continue;
                    }
                    edges.push(ResolvedEdge::resolved(
                        ext.file_path.clone(),
                        ext.file_path.clone(),
                        ext.file_path.clone(),
                        sym.qualified_name.clone(),
                        EdgeKind::Contains,
                        Arc::new(Resolution::SameFile {
                            target_symbol: sym.qualified_name.clone(),
                            target_file: ext.file_path.clone(),
                        }),
                        None,
                    ));
                    // A method is contained twice: once by the file, once by the
                    // type that declares it. Only emit the second when the parent
                    // is a real type rather than the file itself.
                    if let Some(parent) = sym
                        .parent_symbol
                        .as_deref()
                        .filter(|parent| *parent != ext.file_path)
                    {
                        edges.push(ResolvedEdge::resolved(
                            ext.file_path.clone(),
                            ext.file_path.clone(),
                            parent.to_string(),
                            sym.qualified_name.clone(),
                            EdgeKind::Contains,
                            Arc::new(Resolution::SameFile {
                                target_symbol: sym.qualified_name.clone(),
                                target_file: ext.file_path.clone(),
                            }),
                            None,
                        ));
                    }
                }

                // G20: Group Go package / Swift module members for star topology
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
                if family == LangFamily::Swift {
                    for module in self
                        .swift_module_by_file
                        .get(&ext.file_path)
                        .into_iter()
                        .flatten()
                    {
                        let key = format!("module:{module}");
                        package_groups
                            .entry(key)
                            .or_default()
                            .insert(ext.file_path.clone());
                    }
                }

                // Resolve imports
                //
                // One path-loaded file is often named by several imports in the
                // same loader — the handle, and the loader function that
                // returns it — but it is one dependency, so one edge.
                let mut path_loaded: BTreeSet<String> = BTreeSet::new();
                for imp in &ext.imports {
                    let targets = match &imp.path_load {
                        // A `sys.path` entry is not an import of anything.
                        Some(load) if load.kind == PathLoadKind::SearchDirectory => continue,
                        Some(load) => match self.resolve_loaded_module(ext, imp, load) {
                            // Already linked: neither a second edge nor a
                            // spurious "resolved to nothing" row.
                            Some(target) if !path_loaded.insert(target.clone()) => continue,
                            resolved => resolved.into_iter().collect(),
                        },
                        None => self.resolve_import_targets_or_search_dirs(ext, imp),
                    };
                    // R5. A relative specifier that named no indexed file is an
                    // index gap: the `Imports` edge that should exist is
                    // missing, and emitting only on success made that
                    // indistinguishable from "this file imports nothing".
                    // Absolute specifiers are not recorded — `import "strings"`
                    // resolving to nothing is the expected case and carries no
                    // information.
                    if targets.is_empty()
                        && (Self::specifier_is_repo_relative(&imp.module_specifier)
                            || self.specifier_is_ts_mapped(
                                &ext.file_path,
                                &ext.language,
                                &imp.module_specifier,
                            ))
                    {
                        unresolved.push(UnresolvedReference {
                            source_file: ext.file_path.clone(),
                            source_symbol: ext.file_path.clone(),
                            callee_name: imp.module_specifier.clone(),
                            kind: UnresolvedKind::Import,
                            resolution: Resolution::Unresolved {
                                reason: format!(
                                    "relative import {:?} in {} resolved to no indexed file",
                                    imp.module_specifier, ext.file_path
                                ),
                            },
                            class: UnresolvedClass::Unresolved,
                            receiver: None,
                            site: None,
                        });
                    }
                    let edge_targets = if ext.language == "go" {
                        self.go_import_edge_targets(&targets)
                    } else if ext.language == "swift" {
                        self.swift_import_edge_targets(&imp.module_specifier)
                    } else {
                        targets
                    };
                    for target_f in edge_targets {
                        // X41. An import that names the file it is written in
                        // is a real statement about the module tree — `mod
                        // tests { use super::*; }` — and not a dependency
                        // between files. `langimports/rust.rs` already declines
                        // to emit one for the `mod` half of the same fact, for
                        // the same reason: "emitting an import for it would be
                        // an edge from a file to itself". The import is still
                        // *resolved*, so it stops being recorded as an
                        // unresolved relative import; only the self-loop is
                        // withheld.
                        if target_f == ext.file_path {
                            continue;
                        }
                        edges.push(ResolvedEdge::resolved(
                            ext.file_path.clone(),
                            target_f.clone(),
                            ext.file_path.clone(),
                            target_f.clone(),
                            EdgeKind::Imports,
                            Arc::new(Resolution::ImportScoped {
                                target_symbol: target_f.clone(),
                                target_file: target_f,
                                imported_from: imp.module_specifier.clone(),
                            }),
                            Some(imp.raw_import.clone()),
                        ));
                    }
                }

                // Python calls by start offset: the one way to tell `f(x).m()`
                // from `f.m()`, whose extracted receivers are both `f`, is the
                // inner call `f(x)` that begins where the outer call begins.
                let (py_calls_at, py_field_reads) = if family == LangFamily::Python {
                    (
                        pytypes::python_calls_by_start(ext),
                        pytypes::python_field_reads(ext),
                    )
                } else {
                    Default::default()
                };

                // The Go type evaluator's per-file tables; `None` for a file
                // with no extracted Go facts, which leaves its calls to the
                // name-based ladder below.
                let go_index = if family == LangFamily::Go {
                    GoFileIndex::new(ext)
                } else {
                    None
                };

                // Resolve calls using Resolution Ladder (SameFile -> ImportScoped -> UniqueGlobal -> AmbiguousGlobal)
                let explicit_calls = ext.calls.len();
                // A language with no call extractor still records the sites it
                // recognized. They must not enter the ladder: a unique global
                // named `file` would become a Calls edge. The row says the
                // language extracts no calls, and the walk stops.
                let extracts_calls = ext
                    .capabilities()
                    .contains(devmap_extract::languages::Capability::Calls);
                for (call_index, call) in ext.calls.iter().chain(&ext.rust_implicit_calls).enumerate() {
                    // An implicit call is a candidate: when no rung binds it
                    // it is dropped, not recorded (see
                    // `Extraction::rust_implicit_calls`).
                    let implicit_candidate = call_index >= explicit_calls;
                    if !extracts_calls {
                        if !implicit_candidate {
                            unresolved.push(UnresolvedReference {
                                source_file: ext.file_path.clone(),
                                source_symbol: call
                                    .caller_symbol
                                    .clone()
                                    .unwrap_or_else(|| ext.file_path.clone()),
                                callee_name: call.callee_name.clone(),
                                kind: UnresolvedKind::Call,
                                resolution: Resolution::Unresolved {
                                    reason: format!("`{}` extracts no calls", ext.language),
                                },
                                class: UnresolvedClass::Unresolved,
                                receiver: call.receiver_expr.clone(),
                                site: Some(SiteBytes::of(&call.span)),
                            });
                        }
                        continue;
                    }
                    // The resolution is the *only* record of what was found:
                    // it names the target file and symbol, and
                    // `Resolution::confidence` scores it. A parallel
                    // `resolved_target` used to hold the same two strings a
                    // second time, which is the same shape as the confidence
                    // drift this crate was audited for — two fields that must
                    // agree, with nothing making them.
                    let mut resolution: Option<Arc<Resolution>> = None;

                    // RA1: local binding evidence must be consulted before a
                    // same-file/import/global rung can invent a target for it.
                    // A nested `const walk = () => { … }; walk()` is the one
                    // local whose value *is* known: this file extracted that
                    // arrow as a symbol whose parent is the caller. That join
                    // is tried first and does not depend on `local_bindings`
                    // covering nested const arrows (they are function
                    // symbols, not site bindings).
                    // Kotlin `op(2)` where `op` is a typed value: the call is
                    // `op.invoke(2)`. Asked first because a local is otherwise
                    // refused below as "a local binding whose value is not
                    // known" — here its *type* is known and names the target.
                    if family == LangFamily::Kotlin && call.receiver_expr.is_none() {
                        let binding = ext.local_binding_at(call.span.start_byte, &call.callee_name);
                        if let Some(class_type) = self.receiver_type_for(
                            &ext.file_path,
                            call.caller_symbol.as_deref(),
                            &call.callee_name,
                            binding,
                        ) {
                            let key = (family, class_type.clone(), "invoke".to_string());
                            if let Some((target_f, target_symbol)) = self
                                .type_methods
                                .get(&key)
                                .and_then(|hits| self.type_method_target(&ext.file_path, &class_type, hits))
                            {
                                resolution = Some(Arc::new(Resolution::ReceiverType {
                                    target_symbol: target_symbol.to_string(),
                                    target_file: target_f.to_string(),
                                    receiver_type: class_type,
                                }));
                            }
                        }
                    }
                    // A bare call inside `x.apply { .. }` / `with(x) { .. }` is
                    // first a member of `x`'s type (Kotlin tries the implicit
                    // receiver before any function in scope). A member of an
                    // indexed type is that type's; a type the corpus does not
                    // declare may declare it, so a project namesake is not
                    // evidence about this call and binding it would be a guess.
                    if family == LangFamily::Kotlin
                        && call.receiver_expr.is_none()
                        && resolution.is_none()
                        && ext
                            .local_binding_at(call.span.start_byte, &call.callee_name)
                            .is_none()
                    {
                        if let Some(receiver) = ext.scope_receiver_at(call.span.start_byte) {
                            let root = Self::path_root(receiver);
                            let binding = ext.local_binding_at(call.span.start_byte, root);
                            if let Some(class_type) = self.receiver_type_for(
                                &ext.file_path,
                                call.caller_symbol.as_deref(),
                                receiver,
                                binding,
                            ) {
                                let key = (family, class_type.clone(), call.callee_name.clone());
                                if let Some((target_f, target_symbol)) =
                                    self.type_methods.get(&key).and_then(|hits| {
                                        self.type_method_target(&ext.file_path, &class_type, hits)
                                    })
                                {
                                    resolution = Some(Arc::new(Resolution::ReceiverType {
                                        target_symbol: target_symbol.to_string(),
                                        target_file: target_f.to_string(),
                                        receiver_type: class_type,
                                    }));
                                } else if !self.declares_type(family, &class_type) {
                                    resolution = Some(Arc::new(Resolution::Unresolved {
                                        reason: format!(
                                            "the implicit receiver is a {class_type}, which the index does not declare, so a member of it may be meant"
                                        ),
                                    }));
                                }
                            }
                        }
                    }
                    if call.receiver_expr.is_none() && resolution.is_none() {
                        if let Some(target_symbol) = self.local_extracted_callee(
                            &ext.file_path,
                            family,
                            call.caller_symbol.as_deref(),
                            &call.callee_name,
                        ) {
                            resolution = Some(Arc::new(Resolution::SameFile {
                                target_symbol,
                                target_file: ext.file_path.clone(),
                            }));
                        } else if let Some((class_file, identity)) = (family == LangFamily::Python
                            && call.callee_name == "cls")
                            .then_some(call.caller_symbol.as_deref())
                            .flatten()
                            .and_then(|caller| self.python_cls_target(&ext.file_path, caller))
                        {
                            // `cls(...)` inside a method constructs the class
                            // the method is written in. `cls` is a parameter,
                            // so the local-binding arm below would otherwise
                            // claim it.
                            resolution = Some(Arc::new(Resolution::ReceiverType {
                                receiver_type: identity
                                    .rsplit("::")
                                    .next()
                                    .unwrap_or(&identity)
                                    .to_string(),
                                target_symbol: identity,
                                target_file: class_file,
                            }));
                        } else if let Some(binding) =
                            ext.local_binding_at(call.span.start_byte, &call.callee_name)
                        {
                            // A Kotlin value of a declared, non-function type is
                            // not what `name(..)` calls unless the type declares
                            // `invoke` (asked above): `privacyMode: PrivacyMode =
                            // privacyMode()` and `text(context, text)` both call
                            // the *function* beside a parameter of that name.
                            // Only a value of unknown or function type shadows.
                            let typed_non_invokable =
                                family == LangFamily::Kotlin && binding.declared_type.is_some();
                            if !typed_non_invokable {
                                resolution = Some(Arc::new(Resolution::Unresolved {
                                    reason: "the callee is a local binding whose value is not known".to_string(),
                                }));
                            }
                        }
                    }

                    // 0. A Go receiver whose type the evaluator can state: the
                    // type is identified (package and name, not a bare name), so
                    // the method is the one Go's selector rules pick — and a
                    // type that lives outside the index binds nothing in it.
                    // What it cannot tell falls through to the rungs below.
                    let mut go_class: Option<UnresolvedClass> = None;
                    // A Rust receiver typed to an indexed struct or enum that
                    // declares, derefs to, inherits and is blanket-supplied no
                    // member of this name (set by the typed-receiver rung).
                    let mut rust_member_miss = false;
                    let mut typed_member_absent = false;
                    if resolution.is_none() {
                        if let Some(index) = &go_index {
                            match self.go_call(ext, index, call) {
                                GoCall::Concrete {
                                    file,
                                    symbol,
                                    receiver_type,
                                } => {
                                    resolution = Some(Arc::new(Resolution::ReceiverType {
                                        target_symbol: symbol,
                                        target_file: file,
                                        receiver_type,
                                    }));
                                }
                                GoCall::External(module) => {
                                    resolution = Some(Arc::new(Resolution::Unresolved {
                                        reason: format!(
                                            "the receiver's type is declared in {module}, outside the index"
                                        ),
                                    }));
                                    go_class = Some(UnresolvedClass::External { module });
                                }
                                GoCall::Absent { receiver_type } => {
                                    resolution = Some(Arc::new(Resolution::Unresolved {
                                        reason: format!(
                                            "the receiver is a {receiver_type}, which declares no member {:?}",
                                            call.callee_name
                                        ),
                                    }));
                                    go_class = Some(UnresolvedClass::UnresolvedMember);
                                }
                                GoCall::Interface {
                                    iface,
                                    iface_file,
                                    iface_symbol,
                                    targets,
                                } => {
                                    resolution = Some(self.dispatch_through_interface(
                                        ext,
                                        call,
                                        &mut edges,
                                        iface.name(),
                                        (iface_file, iface_symbol),
                                        targets.unwrap_or_default(),
                                    ));
                                }
                                GoCall::Pass => {}
                            }
                        }
                    }

                    // 1. Receiver-based resolution (SameFile / Constructor tracking N6)
                    //
                    // Not after rung 0: a receiver the Go evaluator typed — as an
                    // external type, or an indexed one with no such member — has
                    // its answer, and a bare-name rung overwriting it is the
                    // false edge rung 0 exists to prevent.
                    if let Some(recv) = call.receiver_expr.as_ref().filter(|_| resolution.is_none()) {
                        // Prefer the binding scoped to the calling symbol; fall back
                        // to the file-wide map only when it is unambiguous. Poisoned
                        // keys are absent from both maps, so a collision falls
                        // through the ladder instead of resolving to a guess (SC9).
                        // `admits` gates this rung as well as the global one.
                        // `type_methods` is keyed by `(family, type, method)`,
                        // so two languages sharing `Generic` could dispatch a
                        // method onto each other's type at DETERMINISTIC
                        // confidence — a worse version of the same defect the
                        // global rung had.
                        //
                        // For `w.Priority.valid()` the site binding is on `w`
                        // (the root), not on the full receiver path.
                        let root = Self::path_root(recv);
                        let site_binding = ext
                            .local_binding_at(call.span.start_byte, root)
                            .or_else(|| ext.local_binding_at(call.span.start_byte, recv))
                            .or_else(|| {
                                let (root, field) = Self::one_hop_field(recv)?;
                                (family == LangFamily::Python)
                                    .then(|| {
                                        pytypes::python_field_root_binding(
                                            ext, &py_field_reads, call, root, field,
                                        )
                                    })
                                    .flatten()
                            });
                        // `f(x).m()`: the receiver is the call's result, typed by
                        // `f`'s written return type — never by a value named
                        // `f`, which the extracted receiver text cannot tell
                        // from the call's result.
                        let call_result = py_calls_at
                            .get(&call.span.start_byte)
                            .and_then(|inner| {
                                self.python_call_result_type(
                                    &ext.file_path,
                                    call.caller_symbol.as_deref(),
                                    call,
                                    recv,
                                    inner,
                                )
                            });
                        if let Some(class_type) = match call_result {
                            Some(result_type) => result_type,
                            None => self.receiver_type_for(
                                &ext.file_path,
                                call.caller_symbol.as_deref(),
                                recv,
                                site_binding,
                            ),
                        }
                            .filter(|_| family.admits(family))
                            .and_then(|ty| {
                                self.rust_generic_receiver_type(
                                    &ext.file_path,
                                    family,
                                    call.caller_symbol.as_deref(),
                                    ty,
                                )
                            })
                        {
                            // Python names its class: the file's own `class`
                            // statement, else the import that bound it. A flat
                            // `type_methods` hit in another file's namesake is
                            // not that class, and an alias is not its key.
                            let python_class = (family == LangFamily::Python)
                                .then(|| {
                                    self.python_method_on(
                                        &ext.file_path,
                                        read_scope(call.caller_symbol.as_deref(), &call.span).as_deref(),
                                        &class_type,
                                        &call.callee_name,
                                    )
                                })
                                .and_then(Result::ok);
                            let class_is_known = python_class.is_some();
                            if let Some(Some(found)) = python_class {
                                resolution = Some(Arc::new(Resolution::ReceiverType {
                                    target_symbol: found.symbol,
                                    target_file: found.file,
                                    receiver_type: found.declaring_type,
                                }));
                            }
                            let key = (family, class_type.clone(), call.callee_name.clone());
                            let own = self
                                .receiver_method_through_deref(
                                    ext,
                                    family,
                                    call.caller_symbol.as_deref(),
                                    recv,
                                    site_binding,
                                    &class_type,
                                    &call.callee_name,
                                )
                                .filter(|_| !class_is_known);
                            // Swift: a method the type does not write is the one
                            // a protocol extension or base class supplies.
                            // Rust: a method the impl does not write is the
                            // default body of a trait the type implements.
                            let inherited = (own.is_none()
                                && matches!(family, LangFamily::Swift | LangFamily::Rust)
                                && !self.type_methods.contains_key(&key))
                            .then(|| {
                                self.inherited_method(family, &class_type, &call.callee_name)
                                    .map(|(file, symbol, _)| (file, symbol))
                            })
                            .flatten();
                            // Failing that, the one a Rust blanket impl
                            // supplies — see `rust_blanket_method` for its tier.
                            let blanket = (family == LangFamily::Rust
                                && own.is_none()
                                && inherited.is_none()
                                && !self.type_methods.contains_key(&key))
                            .then(|| self.rust_blanket_method(&call.callee_name))
                            .flatten();
                            // The type is known and declares nothing by that
                            // name: a miss about the member, not the receiver.
                            if family == LangFamily::Rust
                                && own.is_none()
                                && inherited.is_none()
                                && blanket.is_none()
                                && !self.type_methods.contains_key(&key)
                                && self.declares_type(family, &class_type)
                            {
                                rust_member_miss = true;
                            }
                            if let Some((target_f, target_symbol)) = blanket {
                                resolution = Some(Arc::new(Resolution::UniqueGlobal {
                                    target_symbol: target_symbol.clone(),
                                    target_file: target_f.clone(),
                                    family,
                                }));
                            }
                            if let Some(((target_f, target_symbol), receiver_type)) = own
                                .map(|(target, receiver_type)| (target.clone(), receiver_type))
                                .or_else(|| {
                                    inherited.map(|(file, symbol)| ((file, symbol), class_type.clone()))
                                })
                            {
                                resolution = Some(match family {
                                    // A receiver typed by a protocol (Swift), a
                                    // trait (Rust: `dyn T`, `impl T`, a `T`
                                    // bound) or an interface (TypeScript) is a
                                    // call through it: see `protocol_dispatch`.
                                    // A class of the same name stays
                                    // `ReceiverType`, because `protocol_dispatch`
                                    // returns `None` unless every indexed hit is
                                    // an interface or a trait.
                                    // A Swift receiver typed by a call chain
                                    // (`a.padding().badge()`) is a protocol named
                                    // by one return type's text. Conformers are
                                    // joined to a protocol by its bare name, so
                                    // fanning out from that text would reach a
                                    // namesake protocol's conformer (SwiftUI's
                                    // `View`): the chain gets the direct edge only.
                                    LangFamily::Swift | LangFamily::Rust | LangFamily::JsTs => (!(family
                                        == LangFamily::Swift
                                        && Self::swift_call_chain_split(recv).is_some()))
                                    .then(|| self.protocol_dispatch(family, &class_type, &call.callee_name))
                                    .flatten()
                                    .map(|(direct, targets)| {
                                            self.dispatch_through_interface(
                                                ext,
                                                call,
                                                &mut edges,
                                                &class_type,
                                                direct,
                                                targets,
                                            )
                                        }),
                                    // A Kotlin receiver typed by an interface, an
                                    // abstract or an open class may run any indexed
                                    // subtype's override: the declaration is a symbol
                                    // and each override is a possible runtime target.
                                    LangFamily::Kotlin => {
                                        let overriders = self.overriding_methods(
                                            family,
                                            &class_type,
                                            &call.callee_name,
                                            &ext.file_path,
                                        );
                                        (!overriders.is_empty()).then(|| {
                                            self.dispatch_through_interface(
                                                ext,
                                                call,
                                                &mut edges,
                                                &class_type,
                                                (target_f.clone(), target_symbol.clone()),
                                                overriders,
                                            )
                                        })
                                    }
                                    _ => None,
                                }
                                .unwrap_or_else(|| {
                                    Arc::new(Resolution::ReceiverType {
                                        target_symbol: target_symbol.clone(),
                                        target_file: target_f.clone(),
                                        receiver_type: receiver_type.clone(),
                                    })
                                }));
                            }
                            // The method is not on this type, on a supertype, or
                            // (Rust) on a type it derefs to. A later bare-name
                            // rung would bind a namesake the type does not have.
                            // Go has its own answer (`GoCall::Absent`), including
                            // embeddings the flat tables do not see.
                            if resolution.is_none()
                                && family != LangFamily::Go
                                && (family != LangFamily::Python || class_is_known)
                                && self.indexed_type_lacks_member(
                                    family,
                                    &class_type,
                                    &call.callee_name,
                                )
                            {
                                resolution = Some(Arc::new(Resolution::Unresolved {
                                    reason: format!(
                                        "the receiver is a {class_type}, which declares no member {:?}",
                                        call.callee_name
                                    ),
                                }));
                                typed_member_absent = true;
                            }
                        }
                    }

                    // 1a. A Rust path through a generic parameter: `T::create()`
                    // inside `fn f<T: Factory>()` names `Factory::create`, the
                    // one declaration the bound states. Which impl runs is the
                    // caller's choice of `T`, so each one is a dispatch candidate.
                    if resolution.is_none() && family == LangFamily::Rust {
                        if let Some(bound) = call
                            .receiver_expr
                            .as_deref()
                            .filter(|recv| Self::is_plain_ident(recv))
                            .and_then(|recv| {
                                self.rust_generic_receiver_type(
                                    &ext.file_path,
                                    family,
                                    call.caller_symbol.as_deref(),
                                    recv.to_string(),
                                )
                                .filter(|bound| bound != recv)
                            })
                        {
                            if let Some((direct, targets)) =
                                self.protocol_dispatch(family, &bound, &call.callee_name)
                            {
                                resolution = Some(self.dispatch_through_interface(
                                    ext, call, &mut edges, &bound, direct, targets,
                                ));
                            }
                        }
                    }

                    // 1a. A Rust literal receiver: `"abc".ext()`, `5u8.double()`.
                    // The literal states its type, so the method is the one an
                    // extension trait implements for that primitive — and only
                    // when exactly one declaration answers across every type the
                    // literal can be.
                    if resolution.is_none() && family == LangFamily::Rust {
                        if let Some(recv) = call.receiver_expr.as_deref() {
                            let mut answers: Vec<(&(String, String), &str)> = Vec::new();
                            for candidate in Self::rust_literal_types(recv) {
                                let key = (family, candidate.to_string(), call.callee_name.clone());
                                for hit in self.type_methods.get(&key).into_iter().flatten() {
                                    answers.push((hit, candidate));
                                }
                            }
                            answers.sort();
                            answers.dedup_by(|a, b| a.0 == b.0);
                            if let [((target_f, target_symbol), receiver_type)] = answers.as_slice() {
                                resolution = Some(Arc::new(Resolution::ReceiverType {
                                    target_symbol: target_symbol.clone(),
                                    target_file: target_f.clone(),
                                    receiver_type: receiver_type.to_string(),
                                }));
                            } else if answers.is_empty() && !Self::rust_literal_types(recv).is_empty() {
                                // No impl for the primitive itself: a blanket
                                // impl (`impl<T: Display> Describe for T`) is
                                // the one that can supply it.
                                if let Some((target_f, target_symbol)) =
                                    self.rust_blanket_method(&call.callee_name)
                                {
                                    resolution = Some(Arc::new(Resolution::UniqueGlobal {
                                        target_symbol: target_symbol.clone(),
                                        target_file: target_f.clone(),
                                        family,
                                    }));
                                }
                            }
                        }
                    }

                    // Whether the receiver is the enclosing object itself.
                    // Several rungs below turn on it, and it was previously
                    // recomputed at each of them.
                    let implicit_receiver = call.receiver_expr.as_deref().is_some_and(|receiver| {
                        Self::receiver_is_self(receiver) && !Self::rust_module_self_path(receiver)
                    });

                    // `self::helper()` names this module's free function, including
                    // when `helper` is also a method of the enclosing type. The
                    // value-self rung below would bind that method at
                    // deterministic confidence. A miss stays unresolved: the
                    // path does not fall through to the method.
                    if resolution.is_none()
                        && family == LangFamily::Rust
                        && call
                            .receiver_expr
                            .as_deref()
                            .is_some_and(Self::rust_module_self_path)
                    {
                        if let Some(target_symbol) =
                            self.rust_file_level_function(&ext.file_path, &call.callee_name)
                        {
                            resolution = Some(Arc::new(Resolution::SameFile {
                                target_symbol,
                                target_file: ext.file_path.clone(),
                            }));
                        } else if let Some(found) = self.rust_qualified_call(
                            &ext.file_path,
                            "self",
                            &call.callee_name,
                            true,
                        ) {
                            resolution = Some(Arc::new(found));
                        } else {
                            resolution = Some(Arc::new(Resolution::Unresolved {
                                reason: "self:: names a free item of this module, and this module declares none"
                                    .to_string(),
                            }));
                        }
                    }

                    // 1b. X42. An implicit receiver dispatches on the type the
                    // call is written inside, and on that type's supertypes.
                    //
                    // Runs after rung 1 on purpose: a scope that writes
                    // `self = Other()` has stated what `self` is, and written
                    // evidence in this very scope outranks the enclosing type's
                    // default. It runs *before* the import rungs for the
                    // opposite reason — `self.run()` cannot mean an imported
                    // free function, in any language here, so an import binding
                    // of that bare name is not evidence about this call.
                    if resolution.is_none() && implicit_receiver {
                        if let Some(caller) = call.caller_symbol.as_deref() {
                            match self.implicit_receiver_target(
                                &ext.file_path,
                                family,
                                caller,
                                &call.callee_name,
                            ) {
                                Some(ImplicitReceiverAnswer::Unique {
                                    target_file,
                                    target_symbol,
                                    receiver_type,
                                }) => {
                                    resolution = Some(Arc::new(Resolution::ReceiverType {
                                        target_symbol,
                                        target_file,
                                        receiver_type,
                                    }));
                                }
                                Some(ImplicitReceiverAnswer::VirtualOverride) => {
                                    // Inside a protocol extension `self` is any
                                    // conformer: `greet()` written in `extension
                                    // Greeter` is a call through the protocol.
                                    // A Rust trait's default body is the same:
                                    // `self.hook()` there is any implementor's.
                                    let through_protocol = matches!(family, LangFamily::Swift | LangFamily::Rust)
                                        .then(|| self.self_type_at(&ext.file_path, family, caller))
                                        .flatten()
                                        .and_then(|own| {
                                            self.protocol_dispatch(family, &own, &call.callee_name)
                                                .map(|found| (own, found))
                                        });
                                    resolution = Some(match through_protocol {
                                        Some((own, (direct, targets))) => self
                                            .dispatch_through_interface(
                                                ext, call, &mut edges, &own, direct, targets,
                                            ),
                                        None => Arc::new(Resolution::Unresolved {
                                            reason: "implicit receiver method is overridden by an indexed subtype".to_string(),
                                        }),
                                    });
                                }
                                Some(ImplicitReceiverAnswer::HeritageCap(reason)) => {
                                    resolution = Some(Arc::new(Resolution::Unresolved { reason }));
                                }
                                None => {}
                            }
                        }
                    }

                    // 1c. `super.m()` names the nearest supertype that declares
                    // `m` — static dispatch, so an override in a subtype is not a
                    // candidate. Kotlin records the receiver as `super`; every
                    // other family keeps the refusal below.
                    if resolution.is_none()
                        && family == LangFamily::Kotlin
                        && call.receiver_expr.as_deref() == Some("super")
                    {
                        if let Some(caller) = call.caller_symbol.as_deref() {
                            if let Some((target_file, target_symbol, receiver_type)) =
                                self.super_method_target(&ext.file_path, family, caller, &call.callee_name)
                            {
                                resolution = Some(Arc::new(Resolution::ReceiverType {
                                    target_symbol,
                                    target_file,
                                    receiver_type,
                                }));
                            }
                        }
                    }

                    // 2a. Import-scoped named binding (G6 — no silent global widen)
                    //
                    // Refused for an implicit receiver. This rung reads
                    // `import_bindings` by the **bare callee name** and never
                    // looked at the receiver, so `self.run()` in a file carrying
                    // `from helpers import run` bound to `helpers.run` at
                    // DETERMINISTIC — a confident edge to a function the code
                    // demonstrably does not call, and one that also hands the
                    // real method one fewer caller than it has.
                    if resolution.is_none() && call.receiver_expr.is_none() {
                        if let Some((target_f, target_sym)) = self.import_binding_at(
                            family,
                            &ext.file_path,
                            read_scope(call.caller_symbol.as_deref(), &call.span).as_deref(),
                            &call.callee_name,
                        ) {
                            // A whole-module handle called as a function is the
                            // module's own value, which is what its default
                            // export names: `const run = require('./util');
                            // run()` against `module.exports = main`. A
                            // namespace object is not callable, so the call can
                            // only mean that value.
                            let target_sym = if target_sym == "*" && family == LangFamily::JsTs {
                                self.default_exports
                                    .get(target_f)
                                    .unwrap_or(target_sym)
                            } else {
                                target_sym
                            };
                            if let Some((resolved_file, resolved_sym)) =
                                self.lookup_in_package(target_f, target_sym)
                            {
                                resolution = Some(Arc::new(Resolution::ImportScoped {
                                    target_symbol: resolved_sym,
                                    target_file: resolved_file,
                                    imported_from: call.callee_name.clone(),
                                }));
                            } else {
                                resolution = Some(Arc::new(Resolution::Unresolved {
                                    reason: self
                                        .abstention_reason(
                                            family,
                                            &ext.file_path,
                                            read_scope(call.caller_symbol.as_deref(), &call.span)
                                                .as_deref(),
                                            None,
                                            &call.callee_name,
                                            ext.imports.iter().any(|imp| {
                                                imp.imported_names.is_empty()
                                                    && imp.alias.as_deref()
                                                        == Some(call.callee_name.as_str())
                                            }),
                                        )
                                        .unwrap_or_else(|| {
                                            "the named import has no unambiguous declaration in its module"
                                                .to_string()
                                        }),
                                }));
                            }
                        }
                    }

                    // 2a'. Kotlin and Java name each other's members through an
                    // imported or fully-qualified type: `LegacyMath.twice(2)` from
                    // Kotlin, `com.fx.app.Registry.INSTANCE.register(..)` from
                    // Java. The two languages sit in different families, so the
                    // typed-receiver map cannot join them; the package headers
                    // and the member's own declaration can.
                    if resolution.is_none() && (family == LangFamily::Kotlin || ext.language == "java") {
                        if let Some(recv) = call.receiver_expr.as_deref() {
                            if let Some((target_file, target_symbol)) =
                                self.jvm_qualified_member(&ext.file_path, recv, &call.callee_name)
                            {
                                resolution = Some(Arc::new(Resolution::ImportScoped {
                                    target_symbol,
                                    target_file,
                                    imported_from: recv.to_string(),
                                }));
                            }
                        }
                    }

                    // 2a''. A member of a module-scope JS/TS const object built
                    // by spread, `Object.assign`, an alias or a forwarding
                    // property (`debugLogger.warn()` for `{ ...core }`): the
                    // member's own declaration, by JavaScript's last-write-wins
                    // order. A receiver the call's scope rebinds is not the
                    // module's object, and a member no step can be proven to
                    // define falls through to the rungs below unchanged.
                    if resolution.is_none() && family == LangFamily::JsTs {
                        if let Some(recv) = call.receiver_expr.as_deref() {
                            let first = recv.split('.').next().unwrap_or(recv);
                            let shadowed = ext.local_binding_at(call.span.start_byte, first).is_some()
                                || call.caller_symbol.as_deref().is_some_and(|scope| {
                                    self.scope_declares_local(&ext.file_path, scope, first)
                                });
                            if !shadowed {
                                let use_at = call
                                    .caller_symbol
                                    .is_none()
                                    .then_some(call.span.start_byte);
                                match self.object_member_target(
                                    &ext.file_path,
                                    recv,
                                    &call.callee_name,
                                    use_at,
                                ) {
                                    jsobjects::ObjectMember::Bound(target_file, target_symbol) => {
                                        resolution = Some(Arc::new(if target_file == ext.file_path {
                                            Resolution::SameFile { target_symbol, target_file }
                                        } else {
                                            Resolution::ImportScoped {
                                                target_symbol,
                                                target_file,
                                                imported_from: recv.to_string(),
                                            }
                                        }));
                                    }
                                    // The object is known and some step that
                                    // may define this member is not readable:
                                    // a later rung would answer with a
                                    // namesake the object does not have.
                                    jsobjects::ObjectMember::Abstain => {
                                        resolution = Some(Arc::new(Resolution::Unresolved {
                                            reason: format!(
                                                "the receiver is an object whose member {:?} is written by a step that cannot be read",
                                                call.callee_name
                                            ),
                                        }));
                                    }
                                    jsobjects::ObjectMember::Absent => {}
                                }
                            }
                        }
                    }

                    // 2b. Import-scoped module.method (G6 — no silent global widen)
                    if resolution.is_none() {
                        let (recv, method) = if let Some(r) = &call.receiver_expr {
                            (r.clone(), call.callee_name.clone())
                        } else if let Some((r, m)) = call.callee_name.rsplit_once('.') {
                            (r.to_string(), m.to_string())
                        } else {
                            (String::new(), String::new())
                        };
                        if !recv.is_empty() && !method.is_empty()
                            && ext.local_binding_at(call.span.start_byte, &recv).is_none() {
                            {
                                if let Some((target_f, imported)) = self.import_binding_at(
                                    family,
                                    &ext.file_path,
                                    read_scope(call.caller_symbol.as_deref(), &call.span).as_deref(),
                                    &recv,
                                ) {
                                    // A module member: a member of a module
                                    // handle, of an imported type, or of an
                                    // object literal that spells it — never
                                    // an arbitrary value's method that shares
                                    // a name with a free function beside it.
                                    if self.module_member_admitted(&ext.file_path, &recv, &method) {
                                        // Go: the imported package declares the
                                        // name once per build variant. Asked
                                        // first, because `lookup_in_package`
                                        // abstains on the pair, or binds the one
                                        // that happens to sit in `target_f`.
                                        if family == LangFamily::Go {
                                            resolution = self.go_build_variants(
                                                &Self::parent_dir(target_f),
                                                self.go_package_by_file.get(target_f),
                                                &method,
                                                false,
                                            );
                                        }
                                        // A module handle's member may be one the module
                                        // only re-exports; a type's may not (an enum's
                                        // variant `Query` is not the `Query` struct a
                                        // re-export beside the enum happens to publish).
                                        // The import names the type, wherever the path
                                        // to it ran through.
                                        // Go keeps the declared-here lookup it had (the
                                        // variant set above, then its own package
                                        // siblings): a Go package has no re-exports, so
                                        // the chain fallback has nothing to add and a
                                        // chain entry must never outrank a sibling file.
                                        //
                                        // Rust takes the split: `pub use` publishes module
                                        // members as a JS barrel does. A Rust module-handle
                                        // member (`m::run()` after `use crate::m;`) now
                                        // reaches `lookup_module_member`: `lookup_in_package`
                                        // first (a declaration in `m` wins), then, on a miss,
                                        // the re-export chain `m` states for the name
                                        // (`reexport_chains`, which follows an explicit
                                        // `pub use a::run;` and a `pub use a::*;` the corpus
                                        // expansion resolved to one terminal; an ambiguous
                                        // glob yields no chain). Residual risk: an explicit
                                        // `pub use` whose terminal is a namesake of the item
                                        // the caller's own `use` tree denotes (`m` re-exports
                                        // `a::run` while the caller meant `b::run`). Watched
                                        // by the corpus-diff bucket "Rust ImportScoped
                                        // module-handle Calls edges new vs dbd0193a".
                                        let member = resolution.is_none().then(|| {
                                            if family == LangFamily::Go
                                                || self.imports_a_type(&ext.file_path, &recv)
                                                || self.declares_type_at(None, imported)
                                            {
                                                self.lookup_in_package(target_f, &method)
                                            } else {
                                                self.lookup_module_member(target_f, &method)
                                            }
                                        }).flatten();
                                        if let Some((resolved_file, resolved_sym)) = member {
                                            resolution = Some(Arc::new(Resolution::ImportScoped {
                                                target_symbol: resolved_sym,
                                                target_file: resolved_file,
                                                imported_from: recv.clone(),
                                            }));
                                        }
                                    }
                                    if resolution.is_none()
                                        && self.is_value_import(&ext.file_path, &recv)
                                        && !self.imports_a_type(&ext.file_path, &recv)
                                        && !self.imported_declaration_owns(
                                            family,
                                            &ext.file_path,
                                            &recv,
                                            &method,
                                        )
                                    {
                                        // Stop here. The import is this file's
                                        // evidence about `recv`, so a later
                                        // rung matching a same-spelled type
                                        // elsewhere would be a coincidence
                                        // outranking it (R-2).
                                        resolution = Some(Arc::new(Resolution::Unresolved {
                                            reason: "the receiver is an imported value whose type is not known".to_string(),
                                        }));
                                    }
                                }
                            }
                        }
                    }

                    // 2b'. A Python module handle loaded by file path —
                    // `module = module_from_spec(spec)`. Scoped: the handle
                    // is read through the use site's own binding, so a
                    // same-named local in another function never matches.
                    if resolution.is_none() {
                        if let Some(recv) = call.receiver_expr.as_deref() {
                            if let Some(target_f) =
                                self.path_module_for(ext, call.span.start_byte, recv)
                            {
                                if let Some((resolved_file, resolved_sym)) =
                                    self.lookup_in_package(target_f, &call.callee_name)
                                {
                                    resolution = Some(Arc::new(Resolution::ImportScoped {
                                        target_symbol: resolved_sym,
                                        target_file: resolved_file,
                                        imported_from: recv.to_string(),
                                    }));
                                }
                            }
                        }
                    }

                    // 2b''. A Python dotted module path: `import a.b.c` then
                    // `a.b.c.f()`. The flat import table files the module under
                    // its leaf, so the whole path is no key there; the import
                    // statement and the module resolution are the evidence, and
                    // a root the call's own scope rebinds is not the package.
                    if resolution.is_none()
                        && family == LangFamily::Python
                        && ext
                            .local_binding_at(
                                call.span.start_byte,
                                call.receiver_expr.as_deref().and_then(|r| r.split('.').next()).unwrap_or_default(),
                            )
                            .is_none()
                    {
                        if let Some(recv) = call.receiver_expr.as_deref().filter(|r| r.contains('.')) {
                            let scope = read_scope(call.caller_symbol.as_deref(), &call.span);
                            if let Some((resolved_file, resolved_sym)) = self.python_module_path_member(
                                ext,
                                py_call_scopes.as_ref(),
                                scope.as_deref(),
                                recv,
                                &call.callee_name,
                            ) {
                                resolution = Some(Arc::new(Resolution::ImportScoped {
                                    target_symbol: resolved_sym,
                                    target_file: resolved_file,
                                    imported_from: recv.to_string(),
                                }));
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
                    //
                    // A *bare* callee is additionally restricted to what is in
                    // scope at file level. `file_symbols` lists every symbol the
                    // file declares, instance methods included, so matching a
                    // bare name against it bound `def invoke(): run()` to
                    // `class C: def run(self)` — code that raises `NameError` —
                    // at DETERMINISTIC. `C.run` gained a caller that does not
                    // exist and was thereby shielded from the dead-code pass.
                    // The existing duplicate-method guard could not catch it:
                    // with one class the name occurs once, so the count test
                    // passes. A `self.`/`this.` receiver keeps reaching methods,
                    // because there the receiver *is* the declaring type.
                    if resolution.is_none()
                        && call
                            .receiver_expr
                            .as_deref()
                            .is_none_or(Self::receiver_is_self)
                    {
                        if call.receiver_expr.is_none() {
                            if let Some(target_symbol) = self
                                .lexical_target(&ext.file_path, family, call.caller_symbol.as_deref(), &call.callee_name)
                                .filter(|identity| {
                                    !self.file_declares_as(&ext.file_path, identity, |kind| {
                                        Self::never_a_call_target(family, kind)
                                    })
                                })
                            {
                                resolution = Some(Arc::new(Resolution::SameFile {
                                    target_symbol, target_file: ext.file_path.clone(),
                                }));
                            }
                        } else if self.bare_name_is_in_scope(&ext.file_path, family, call.caller_symbol.as_deref(), &call.callee_name)
                            && self.symbol_kind_in(&ext.file_path, &call.callee_name) == Some(SymbolKind::Method) {
                            resolution = Some(Arc::new(Resolution::SameFile {
                                target_symbol: self.qualified_for(&ext.file_path, &call.callee_name), target_file: ext.file_path.clone(),
                            }));
                        } else if family == LangFamily::Rust
                            && self.symbol_kind_in(&ext.file_path, &call.callee_name) == Some(SymbolKind::Function)
                            && !self.rust_self_is_a_value(&ext.file_path, call.caller_symbol.as_deref(), call.receiver_expr.as_deref()) {
                            // A value `self.helper()` whose caller is not inside a
                            // type. An absorbed `super::helper()` and a written
                            // `self::helper()` are stored as `self::` and bound
                            // by the module-path rung above, including inside a
                            // method. This arm keeps the field spelling.
                            resolution = Some(Arc::new(Resolution::SameFile {
                                target_symbol: self.qualified_for(&ext.file_path, &call.callee_name), target_file: ext.file_path.clone(),
                            }));
                        }
                    }

                    // 2c'. Swift: a bare call that bound to a method of a protocol is
                    // a call through it — `greet()` written in `extension Greeter`
                    // runs whichever conformer `self` is.
                    if family == LangFamily::Swift && call.receiver_expr.is_none() {
                        // An overload calling its sibling cannot say whether the
                        // protocol's own declaration is the target, so that one
                        // edge is withheld — but the conformers it fans out to
                        // are still reached, and are kept.
                        let caller_identity =
                            call.caller_symbol.as_deref().unwrap_or(&ext.file_path);
                        let withhold_self = self.is_overload_self_call(
                            &ext.file_path,
                            caller_identity,
                            resolution.as_deref(),
                        );
                        let through_protocol = match resolution.as_deref() {
                            Some(Resolution::SameFile {
                                target_symbol,
                                target_file,
                            }) => self
                                .declaring_type_of(target_file, target_symbol)
                                .and_then(|own| {
                                    self.protocol_dispatch(family, own, &call.callee_name)
                                        .map(|found| (own.to_string(), found))
                                })
                                .filter(|(_, (_, targets))| !targets.is_empty()),
                            _ => None,
                        };
                        if let Some((own, (direct, mut targets))) = through_protocol {
                            if withhold_self {
                                targets.retain(|(file, symbol)| {
                                    !(*file == ext.file_path && symbol == caller_identity)
                                });
                            }
                            if !(withhold_self && targets.is_empty()) {
                                // The dispatch pushes the direct edge into the
                                // vector it is given; the withheld self-edge goes
                                // to a scratch one instead.
                                let mut scratch = Vec::new();
                                let sink = if withhold_self {
                                    &mut scratch
                                } else {
                                    &mut edges
                                };
                                resolution = Some(self.dispatch_through_interface(
                                    ext, call, sink, &own, direct, targets,
                                ));
                            }
                        }
                    }

                    // 2d. A receiver that *is* a type names it directly:
                    // `PdgBuilder::new()`, `Config.default()`. There is no
                    // binding to look up because nothing was bound — the type is
                    // written at the call site — so without this an associated
                    // function falls to the global tier, where any other type
                    // declaring `new` makes it ambiguous.
                    //
                    // It sat in rung 1, *ahead* of the import rungs, where a
                    // spelling coincidence outranked explicit import evidence:
                    // `from real import parser; parser.parse()` bound to an
                    // unrelated `class parser` in some other file, at
                    // DETERMINISTIC. Go and lowercase-class Python make that
                    // collision ordinary — a package handle and a struct share a
                    // lowercase namespace.
                    //
                    // Two things fix it. It runs after 2a/2b, so an import in
                    // *this* file always wins. And the type must be corroborated
                    // by this file: declared here, or bound by an import here.
                    // A type of that name existing somewhere in the repository is
                    // not evidence about what `recv` means at this call site.
                    if resolution.is_none() {
                        if let Some(recv) = &call.receiver_expr {
                            // `Renderer.shared.isScalable()` writes the type at
                            // the call site and then a member; the receiver
                            // text is `Renderer.shared`, which is not itself a
                            // type. The leading capitalised segment is, when
                            // this file (or an import here) corroborates it.
                            let type_spellings = std::iter::once(recv.as_str()).chain(
                                recv.split(['.', ':'])
                                    .next()
                                    .filter(|head| {
                                        *head != recv.as_str()
                                            && head
                                                .chars()
                                                .next()
                                                .is_some_and(|ch| ch.is_uppercase())
                                    }),
                            );
                            for type_name in type_spellings {
                                let key =
                                    (family, type_name.to_string(), call.callee_name.clone());
                                if let Some(hits) = self.type_methods.get(&key) {
                                    if let Some((target_f, target_symbol)) = self
                                        .type_method_target(
                                            &ext.file_path,
                                            type_name,
                                            hits,
                                        )
                                    {
                                        resolution = Some(Arc::new(Resolution::ReceiverType {
                                            target_symbol: target_symbol.to_string(),
                                            target_file: target_f.to_string(),
                                            receiver_type: type_name.to_string(),
                                        }));
                                        break;
                                    }
                                    // A Swift file compiled into several targets
                                    // reaches each target's own type: one edge
                                    // per target, none of them a guess.
                                    let per_target = if family == LangFamily::Swift {
                                        self.type_method_targets_per_module(&ext.file_path, hits)
                                    } else {
                                        Vec::new()
                                    };
                                    if let Some(((first_file, first_symbol), rest)) =
                                        per_target.split_first().filter(|_| per_target.len() > 1)
                                    {
                                        let caller = call
                                            .caller_symbol
                                            .clone()
                                            .unwrap_or_else(|| ext.file_path.clone());
                                        for (target_f, target_symbol) in rest {
                                            edges.push(ResolvedEdge::resolved(
                                                ext.file_path.clone(),
                                                target_f.to_string(),
                                                caller.clone(),
                                                self.qualified_for(target_f, target_symbol),
                                                EdgeKind::Calls,
                                                Arc::new(Resolution::ReceiverType {
                                                    target_symbol: target_symbol.to_string(),
                                                    target_file: target_f.to_string(),
                                                    receiver_type: type_name.to_string(),
                                                }),
                                                None,
                                            ));
                                        }
                                        resolution = Some(Arc::new(Resolution::ReceiverType {
                                            target_symbol: first_symbol.to_string(),
                                            target_file: first_file.to_string(),
                                            receiver_type: type_name.to_string(),
                                        }));
                                        break;
                                    }
                                }
                            }
                        }
                    }

                    // 2d''. Python: a class written at the call — `Engine(1).quack()`,
                    // `E2.create()` — through the alias or inheritance 2d cannot
                    // see — and `super().m()`, which names the enclosing class's
                    // bases. A local of the same name shadows the class.
                    if resolution.is_none() && family == LangFamily::Python {
                        let found = match call.receiver_expr.as_deref() {
                            Some("super" | "super()") => call.caller_symbol.as_deref().and_then(
                                |caller| {
                                    self.python_super_method(ext, call, caller, &call.callee_name)
                                },
                            ),
                            Some(recv)
                                if Self::is_plain_ident(recv)
                                    && ext.local_binding_at(call.span.start_byte, recv).is_none()
                                    && !call.caller_symbol.as_deref().is_some_and(|scope| {
                                        self.scope_declares_local(&ext.file_path, scope, recv)
                                    }) =>
                            {
                                self.python_method_on(
                                    &ext.file_path,
                                    read_scope(call.caller_symbol.as_deref(), &call.span).as_deref(),
                                    recv,
                                    &call.callee_name,
                                )
                                    .ok()
                                    .flatten()
                            }
                            _ => None,
                        };
                        if let Some(found) = found {
                            resolution = Some(Arc::new(Resolution::ReceiverType {
                                target_symbol: found.symbol,
                                target_file: found.file,
                                receiver_type: found.declaring_type,
                            }));
                        }
                    }

                    // 2d''. `Outer.Inner(..)`: a nested Kotlin class named through
                    // its enclosing type is a constructor call on it. Like 2d, the
                    // type must be corroborated by this file — declared here,
                    // imported here, or in this file's package.
                    if resolution.is_none() && family == LangFamily::Kotlin {
                        if let Some(recv) = call.receiver_expr.as_deref() {
                            if let Some((target_file, target_symbol)) =
                                self.nested_type_target(&ext.file_path, recv, &call.callee_name)
                            {
                                resolution = Some(Arc::new(Resolution::ReceiverType {
                                    target_symbol,
                                    target_file,
                                    receiver_type: recv.to_string(),
                                }));
                            }
                        }
                    }

                    // 2d'. A Rust path written at the call.
                    //
                    // `crate::resolver::Resolver::resolve_all()` does not name
                    // an import and it does not name a bare type, so every
                    // rung above leaves it for `ModulePath`. The path is the
                    // evidence: it places one module, and either a free
                    // function in that file or a method of a type that file
                    // declares. After 2d so an import in this file still wins,
                    // and only for Rust — `work::helper()` in C++ is not this
                    // module system.
                    //
                    // A bare root — `dc_glob::matches()` reduces to the
                    // receiver `dc_glob` — is a crate only when nothing in
                    // this scope binds that name and no `use` here does: a
                    // local or an import alias shadows the extern crate.
                    if resolution.is_none() && family == LangFamily::Rust {
                        if let Some(recv) = call.receiver_expr.as_deref() {
                            let bare_root_is_free = Self::is_plain_ident(recv)
                                && !self.root_is_a_value(
                                    &ext.file_path,
                                    call.caller_symbol.as_deref().unwrap_or(&ext.file_path),
                                    recv,
                                    ext.local_binding_at(call.span.start_byte, recv),
                                )
                                && !self
                                    .import_bindings
                                    .get(&ext.file_path)
                                    .is_some_and(|bindings| bindings.contains_key(recv));
                            if !self.rust_self_is_a_value(&ext.file_path, call.caller_symbol.as_deref(), Some(recv))
                                && !Self::rust_module_self_path(recv)
                            {
                            if let Some(found) = self.rust_qualified_call(
                                &ext.file_path,
                                recv,
                                &call.callee_name,
                                bare_root_is_free,
                            ) {
                                resolution = Some(Arc::new(found));
                            }
                            }
                        }
                    }

                    // 2e. X45. The package block. A bare `Trim(raw)` in
                    // `search/rank.go` names `search/provider.go`'s `Trim`
                    // because Go's package-level scope spans the package's
                    // files — no import says so and none needs to.
                    //
                    // Placed last of the pre-global rungs, so it takes only
                    // what the global tier was answering and no rung above it
                    // loses a call. A *bare* callee only: `x.Trim()` names
                    // something `x` owns, and a package-level function is not
                    // one, which is the same rule rung 2c applies within a file.
                    if resolution.is_none()
                        && family == LangFamily::Go
                        && call.receiver_expr.is_none()
                    {
                        if let Some((target_file, target_symbol, package_name)) = self
                            .same_package_target(&ext.file_path, &call.callee_name, |kind| {
                                kind != SymbolKind::Method
                            })
                        {
                            resolution = Some(Arc::new(Resolution::SamePackage {
                                target_symbol,
                                target_file,
                                package_name,
                            }));
                        } else if let Some(variants) = self.go_build_variants(
                            &Self::parent_dir(&ext.file_path),
                            self.go_package_by_file.get(&ext.file_path),
                            &call.callee_name,
                            ext.file_path.ends_with("_test.go"),
                        ) {
                            // The name is declared once per build variant.
                            resolution = Some(variants);
                        }
                    }
                    // 2f. An unexported Go selector on a receiver no rung above
                    // could type. `SamePackage` because the evidence is the
                    // package block again — this time Go's rule that an
                    // unexported selector cannot leave it. See
                    // `unexported_selector_target` for every abstention.
                    if resolution.is_none() && family == LangFamily::Go {
                        if let Some(recv) = call.receiver_expr.as_deref() {
                            if let Some((target_file, target_symbol, package_name)) = self
                                .unexported_selector_target(
                                    &ext.file_path,
                                    recv,
                                    &call.callee_name,
                                )
                            {
                                resolution = Some(Arc::new(Resolution::SamePackage {
                                    target_symbol,
                                    target_file,
                                    package_name,
                                }));
                            }
                        }
                    }
                    // Swift: a bare call inside a type is first a member of that
                    // type — `self.name(...)` — and a member this file cannot see
                    // is still one: the type's own, declared in an extension in
                    // another file, or a supertype's. Asked before the module's
                    // free functions, because a member shadows a function of the
                    // same name.
                    if resolution.is_none()
                        && family == LangFamily::Swift
                        && call.receiver_expr.is_none()
                        && ext
                            .local_binding_at(call.span.start_byte, &call.callee_name)
                            .is_none()
                    {
                        if let Some(caller) = call
                            .caller_symbol
                            .as_deref()
                            .filter(|caller| {
                                !self.scope_declares_local(&ext.file_path, caller, &call.callee_name)
                            })
                        {
                            if let Some(ImplicitReceiverAnswer::Unique {
                                target_file,
                                target_symbol,
                                receiver_type,
                            }) = self.implicit_receiver_target(
                                &ext.file_path,
                                family,
                                caller,
                                &call.callee_name,
                            ) {
                                resolution = Some(Arc::new(Resolution::ReceiverType {
                                    target_symbol,
                                    target_file,
                                    receiver_type,
                                }));
                            }
                        }
                    }
                    // A Swift module is the same scope rule as a Go package:
                    // every file-level name is unqualified throughout the
                    // target. Reuses `SamePackage` rather than a second kind —
                    // the evidence is the same, and the `package_name` slot
                    // carries the module.
                    if resolution.is_none()
                        && family == LangFamily::Swift
                        && call.receiver_expr.is_none()
                    {
                        if let Some((target_file, target_symbol, package_name)) = self
                            .same_swift_module_target(&ext.file_path, &call.callee_name, |kind| {
                                kind != SymbolKind::Method
                            })
                        {
                            resolution = Some(Arc::new(Resolution::SamePackage {
                                target_symbol,
                                target_file,
                                package_name,
                            }));
                        }
                    }

                    // A superclass receiver excludes the overriding declaration.
                    // Without a proven base target, bare-name widening fabricates
                    // a self-call. Preserve the unresolved site instead.
                    //
                    // Not in Rust, where `super` is not superclass dispatch at
                    // all — the language has no inheritance and `super::f()` is
                    // a *module path* naming the parent module. Claiming
                    // "superclass dispatch requires a proven base declaration"
                    // for it stated a reason that cannot apply, and did so on
                    // every file-level `super::` call in the corpus. Letting
                    // Rust fall through leaves the site to `classify_unresolved`,
                    // which has a tier for exactly this shape — `ModulePath`,
                    // whose own documentation names `super::` — so the row still
                    // records that nothing was bound, under a reason that is
                    // true. The inline-module spelling never arrives here: the
                    // extractor has already rewritten it to `self`, and rung 2c
                    // above binds it.
                    if resolution.is_none()
                        && family != LangFamily::Rust
                        && call.receiver_expr.as_deref().is_some_and(|r| matches!(r, "super" | "super()" | "base")) {
                        resolution = Some(Arc::new(Resolution::Unresolved {
                            reason: "superclass dispatch requires a proven base declaration".to_string(),
                        }));
                    }

                    // A Ruby bare send is an implicit-`self` send: first a method
                    // of the class it is written in or of an ancestor (a superclass
                    // or an `include`d module), and only then a function in scope.
                    // Asked before the global tier, which would otherwise bind an
                    // inherited attribute to whichever class elsewhere declares the
                    // same name.
                    if resolution.is_none()
                        && family == LangFamily::Ruby
                        && call.receiver_expr.is_none()
                    {
                        if let Some(caller) = call.caller_symbol.as_deref() {
                            if let Some(ImplicitReceiverAnswer::Unique {
                                target_file,
                                target_symbol,
                                receiver_type,
                            }) = self.implicit_receiver_target(
                                &ext.file_path,
                                family,
                                caller,
                                &call.callee_name,
                            ) {
                                resolution = Some(Arc::new(Resolution::ReceiverType {
                                    target_symbol,
                                    target_file,
                                    receiver_type,
                                }));
                            }
                        }
                    }

                    // Bare global names share one immutable set per visibility
                    // scope. Receiver and earlier proven rungs stay unchanged.
                    //
                    // S2. This rung answers only when nothing else is known
                    // about the name: an import, a builtin or a host global
                    // already says what it is, and a name that is merely the
                    // same *spelling* as a repository declaration is not
                    // evidence of that declaration. Declining leaves the site
                    // to `classify_unresolved`, which names the evidence.
                    if resolution.is_none()
                        && call.receiver_expr.is_none()
                        && !self.bare_name_has_other_evidence(&ext.file_path, family, &call.callee_name)
                    {
                        let in_type = self.call_is_inside_a_type(
                            &ext.file_path,
                            family,
                            call.caller_symbol.as_deref(),
                        );
                        let candidates = match local_candidates.get(&(call.callee_name.as_str(), in_type)) {
                            Some(candidates) => Arc::clone(candidates),
                            None => {
                                let candidates = self.global_candidates(
                                    &global_candidates,
                                    &ext.file_path,
                                    family,
                                    &call.callee_name,
                                    in_type,
                                    limits,
                                )?;
                                local_candidates.insert((&call.callee_name, in_type), Arc::clone(&candidates));
                                candidates
                            }
                        };
                        if let Some(cap) = candidates.cap.as_ref().filter(|cap| !cap.looked_up) {
                            // The lookup itself was skipped: no claim either
                            // way, and the row says why.
                            resolution = Some(Arc::new(Resolution::CappedGlobal {
                                candidates: Arc::clone(&candidates.rows),
                                total: None,
                                limit: Arc::clone(&cap.limit),
                                family,
                            }));
                        } else if candidates.declarations == 1 {
                            let (target_f, target_identity) = &candidates.rows[0];
                            // A Swift type nested in another type is named
                            // bare only from inside that type; from anywhere
                            // else the one namesake this rung found is not it.
                            let out_of_scope = family == LangFamily::Swift
                                && self.swift_nested_type_is_out_of_scope(
                                    &ext.file_path,
                                    call.caller_symbol.as_deref(),
                                    target_f,
                                    target_identity,
                                );
                            if !out_of_scope {
                                resolution = Some(Arc::new(Resolution::UniqueGlobal {
                                    target_symbol: target_identity.clone(), target_file: target_f.clone(), family,
                                }));
                            }
                        } else if candidates.declarations > 1 {
                            resolution = Some(Arc::new(match &candidates.cap {
                                None => Resolution::AmbiguousGlobal { candidates: Arc::clone(&candidates.rows), family },
                                // The list was cut by a budget: say so on the
                                // row, with the true count.
                                Some(cap) => Resolution::CappedGlobal {
                                    candidates: Arc::clone(&candidates.rows),
                                    total: Some(cap.distinct_total),
                                    limit: Arc::clone(&cap.limit),
                                    family,
                                },
                            }));
                        }
                    }

                    let caller_sym = call
                        .caller_symbol
                        .clone()
                        .unwrap_or_else(|| ext.file_path.clone());
                    // Overloads share one id, so a call out of one overload to
                    // its sibling (`fun add(a: String) = add(a.length)`) binds
                    // to the caller's own identity. Real recursion binds there
                    // too, and nothing recorded about the call (no arity)
                    // separates the two: abstain, with the reason, rather than
                    // assert a self-edge a sibling call would make false.
                    if self.is_overload_self_call(&ext.file_path, &caller_sym, resolution.as_deref()) {
                        resolution = Some(Arc::new(Resolution::Unresolved {
                            reason: format!(
                                "{caller_sym} is declared more than once in {} (overloads share one id), so a call to {:?} from inside it cannot be told apart from a call to a sibling overload",
                                ext.file_path, call.callee_name
                            ),
                        }));
                    }
                    // Emission is driven entirely by the resolution, and
                    // anything that emits no edge falls through to the ledger —
                    // so a rung that ever produced evidence naming no target
                    // would be *reported*, not silently dropped and not a panic.
                    let mut emitted = false;
                    if let Some(resolution) = &resolution {
                        match resolution.as_ref() {
                            Resolution::AmbiguousGlobal { .. } | Resolution::CappedGlobal { .. } => {
                                // S2. A bare name with several in-repo namesakes
                                // and no other evidence is **not** a call to any
                                // of them: measured 0/8 (TS), 1/26 (Swift), 4/12
                                // (Kotlin) correct, so a fan-out was three wrong
                                // callers in four standing beside edges that
                                // were checked. The site keeps one ledger row
                                // carrying the complete candidate list, classed
                                // `Unresolved` — it is by definition a bare name
                                // with namesakes — and liveness reads the
                                // ambiguity from that row, not from edges.
                                unresolved.push(UnresolvedReference {
                                    source_file: ext.file_path.clone(),
                                    source_symbol: caller_sym.clone(),
                                    callee_name: call.callee_name.clone(),
                                    kind: UnresolvedKind::Call,
                                    resolution: (**resolution).clone(),
                                    class: UnresolvedClass::Unresolved,
                                    receiver: call.receiver_expr.clone(),
                                    site: Some(SiteBytes::of(&call.span)),
                                });
                                emitted = true;
                            }
                            Resolution::Dispatch {
                                candidates,
                                via_symbol,
                                ..
                            } => {
                                // One edge per implementing method, each carrying
                                // the interface method it was reached through. Past
                                // the cap: no edges, and one ledger row holding the
                                // whole candidate list — the interface method's own
                                // direct edge stays.
                                let via = via_symbol.rsplit("::").next().unwrap_or(via_symbol);
                                if candidates.len() > DISPATCH_FANOUT_CAP {
                                    unresolved.push(UnresolvedReference {
                                        source_file: ext.file_path.clone(),
                                        source_symbol: caller_sym.clone(),
                                        callee_name: call.callee_name.clone(),
                                        kind: UnresolvedKind::Call,
                                        resolution: (**resolution).clone(),
                                        class: UnresolvedClass::Unresolved,
                                        receiver: call.receiver_expr.clone(),
                                        site: Some(SiteBytes::of(&call.span)),
                                    });
                                } else {
                                    for (target_f, target_sym) in candidates.iter() {
                                        edges.push(ResolvedEdge::resolved(
                                            ext.file_path.clone(),
                                            target_f.clone(),
                                            caller_sym.clone(),
                                            self.qualified_for(target_f, target_sym),
                                            EdgeKind::Calls,
                                            Arc::clone(resolution),
                                            Some(format!("via {via}")),
                                        ));
                                    }
                                }
                                emitted = true;
                            }
                            named => {
                                if let Some((target_file, target_symbol)) = named.target() {
                                    // A composite literal or conversion names a
                                    // type, which nothing runs: a use, not a call.
                                    let (edge_kind, details) =
                                        self.call_site_edge_kind(family, target_file, target_symbol);
                                    edges.push(ResolvedEdge::resolved(
                                        ext.file_path.clone(),
                                        target_file.to_string(),
                                        caller_sym.clone(),
                                        self.qualified_for(target_file, target_symbol),
                                        edge_kind,
                                        Arc::clone(resolution),
                                        details,
                                    ));
                                    emitted = true;
                                }
                            }
                        }
                    }
                    if !emitted && !implicit_candidate {
                        // D17 / R5: a call the ladder could not resolve is recorded,
                        // never dropped. Silence here is indistinguishable from
                        // "there was no call", which is the failure R5 forbids.
                        let class = if typed_member_absent {
                            UnresolvedClass::UnresolvedMember
                        } else {
                            go_class.unwrap_or_else(|| {
                                match self.classify_unresolved(
                                    &ext.file_path,
                                    family,
                                    &call.callee_name,
                                    call.receiver_expr.as_deref(),
                                    &caller_sym,
                                    UsePosition::value_at(
                                        ext,
                                        call.span.start_byte,
                                        call.receiver_expr.as_deref(),
                                    ),
                                ) {
                                    // The receiver was typed; only the member was
                                    // not found. A class that explains the miss
                                    // (no namesake anywhere, an external) stands.
                                    UnresolvedClass::UninferredReceiver if rust_member_miss => {
                                        UnresolvedClass::UnresolvedMember
                                    }
                                    class => class,
                                }
                            })
                        };
                        unresolved.push(UnresolvedReference {
                            source_file: ext.file_path.clone(),
                            source_symbol: caller_sym,
                            callee_name: call.callee_name.clone(),
                            kind: UnresolvedKind::Call,
                            resolution: match resolution {
                                Some(r) => (*r).clone(),
                                None => Resolution::Unresolved {
                                    // A JS/TS import that names something the module
                                    // withholds says so, instead of the bare ladder text.
                                    reason: self
                                        .abstention_reason(
                                            family,
                                            &ext.file_path,
                                            read_scope(call.caller_symbol.as_deref(), &call.span)
                                                .as_deref(),
                                            call.receiver_expr.as_deref(),
                                            &call.callee_name,
                                            false,
                                        )
                                        .unwrap_or_else(|| {
                                            format!(
                                                "no resolution ladder rung matched {:?} in {} family {:?}",
                                                call.callee_name, ext.file_path, family
                                            )
                                        }),
                                },
                            },
                            class,
                            receiver: call.receiver_expr.clone(),
                            site: Some(SiteBytes::of(&call.span)),
                        });
                    }
                }

                // `?` into a function's `Result<_, E>`: a possible call to the
                // `From::from` impl `E` declares, at the dispatch tier — the
                // operand's error type is not known, and when it is already `E`
                // the identity conversion runs instead (see
                // `Extraction::rust_try_conversions`). A candidate that names no
                // indexed impl is dropped, like an operator candidate.
                for conversion in &ext.rust_try_conversions {
                    let Some(error_type) = conversion.receiver_expr.as_deref() else {
                        continue;
                    };
                    let key = (family, error_type.to_string(), conversion.callee_name.clone());
                    let Some((target_f, target_sym)) = self
                        .type_methods
                        .get(&key)
                        .and_then(|hits| self.type_method_target(&ext.file_path, error_type, hits))
                    else {
                        continue;
                    };
                    let resolution = Arc::new(Resolution::Dispatch {
                        candidates: vec![(target_f.to_string(), target_sym.to_string())].into(),
                        via_file: target_f.to_string(),
                        via_symbol: "core::convert::From.from".to_string(),
                    });
                    edges.push(ResolvedEdge::resolved(
                        ext.file_path.clone(),
                        target_f.to_string(),
                        conversion
                            .caller_symbol
                            .clone()
                            .unwrap_or_else(|| ext.file_path.clone()),
                        self.qualified_for(target_f, target_sym),
                        EdgeKind::Calls,
                        resolution,
                        Some("via From.from (`?`)".to_string()),
                    ));
                }

                for reference in &ext.references {
                    if matches!(
                        reference.kind,
                        ReferenceKind::Call | ReferenceKind::Constructor | ReferenceKind::JsxTag
                    ) {
                        // These kinds duplicate `calls` wherever a grammar
                        // extracted one, and the ladder already recorded that
                        // edge. A pattern-recovered file has no such edge: the
                        // markup reader left the site on this reference so a
                        // Fallback file can name it without publishing a call
                        // edge. The row is the same one a parsed call-blind
                        // file gets from `calls`, and the walk stops.
                        if !extracts_calls
                            && ext.calls.is_empty()
                            && matches!(
                                ext.engine,
                                devmap_extract::ExtractionEngine::RegexFallback { .. }
                            )
                            && reference.kind == ReferenceKind::Call
                            && !reference.name.is_empty()
                        {
                            unresolved.push(UnresolvedReference {
                                source_file: ext.file_path.clone(),
                                source_symbol: reference
                                    .enclosing_symbol
                                    .clone()
                                    .unwrap_or_else(|| ext.file_path.clone()),
                                callee_name: reference.name.clone(),
                                kind: UnresolvedKind::Call,
                                resolution: Resolution::Unresolved {
                                    reason: format!("`{}` extracts no calls", ext.language),
                                },
                                class: UnresolvedClass::Unresolved,
                                receiver: reference.receiver_expr.clone(),
                                site: Some(SiteBytes::of(&reference.span)),
                            });
                        }
                        continue;
                    }
                    // A Python base no name denotes is a fact about the MRO,
                    // not a reference to attribute: it is neither an edge nor
                    // a failed attribution.
                    if reference.kind == ReferenceKind::Heritage
                        && reference.name == OPAQUE_BASE
                    {
                        continue;
                    }
                    // A DOM or stylesheet identity, answered by its own two-rung
                    // ladder and by nothing else. It never reaches the code
                    // rungs below, and a name they cannot find is not recorded
                    // as an unresolved *code* reference: a class this index never
                    // saw declared is the ordinary case — it lives in a global
                    // stylesheet outside the tree, in a framework, or behind a
                    // CDN — and filing 40,000 of those as failed attributions
                    // would bury the tier whose whole purpose is to name
                    // defects. The limit is stated in `markup`'s documentation
                    // instead of implied by an empty ledger.
                    if reference.kind == ReferenceKind::Selector {
                        if let Some(edge) = self.resolve_selector_reference(ext, reference) {
                            edges.push(edge);
                        }
                        continue;
                    }
                    // A string naming a runtime entry point, answered from
                    // `entry_names` and by nothing else — and silent when that
                    // finds nothing, because an identifier-shaped string is
                    // almost always a key, a label or a message. Recording each
                    // as a failed code attribution would bury the defect tier
                    // under literals no declaration was ever meant to answer.
                    if reference.kind == ReferenceKind::EntryName {
                        edges.extend(self.resolve_entry_name_reference(ext, reference));
                        continue;
                    }
                    if let Some(edge) = self.resolve_name_reference(ext, family, reference) {
                        edges.push(edge);
                        continue;
                    }
                    // R5, the same rule the call ladder above obeys. A reference
                    // the ladder *ran* and could not attribute used to be
                    // dropped, so the ledger `devmap build` prints covered one
                    // edge family and was read as covering both — a partial
                    // denominator presented as a total.
                    //
                    // A reference whose name is empty is not a failed
                    // attribution: there is nothing to attribute.
                    let name = reference.name.rsplit('.').next().unwrap_or(&reference.name);
                    if name.is_empty() {
                        continue;
                    }
                    // A **bare** `Name` is the one rung the resolver declines
                    // rather than fails: `resolve_name_reference` deliberately
                    // stops before the global lookup, because `except Exception
                    // as e` must not bind to some unrelated `def e`. Recording a
                    // declined check as a failed one is the same Class A error
                    // in the other direction, and it is not a small one —
                    // measured over 148 files of this repository it puts 33,243
                    // local-variable mentions into the tier documented as "the
                    // only tier that indicates a defect", which today holds 9
                    // rows. Saying "not attempted" needs an `UnresolvedClass`
                    // variant, and that enum is matched exhaustively in
                    // `devmap-cli`; until it has one, the honest ledger is of
                    // the rungs that ran.
                    //
                    // A `Name` *with a receiver* is not declined: the member
                    // rungs run for it in full, so its failure is recorded.
                    if reference.kind == ReferenceKind::Name && reference.receiver_expr.is_none() {
                        continue;
                    }
                    let source_symbol = reference
                        .enclosing_symbol
                        .clone()
                        .unwrap_or_else(|| ext.file_path.clone());
                    // A `Type` or `TypeQualifier` reference is a type
                    // annotation; every other surviving kind — `Name`,
                    // `Heritage`, `HeritageInterface`, `Decorator` — names a
                    // value or a supertype and is answered by the value rungs.
                    // Heritage is deliberately *not* a type position here: a
                    // base class is a real declaration the corpus is expected
                    // to contain, and exempting an unfound one as "the language
                    // declares it" would hide a missing supertype.
                    let position = if matches!(
                        reference.kind,
                        ReferenceKind::Type | ReferenceKind::TypeQualifier
                    ) {
                        UsePosition::Type {
                            types: reference.assigned_to.as_deref(),
                            scope: reference.enclosing_symbol.as_deref(),
                        }
                    } else {
                        UsePosition::value_at(
                            ext,
                            reference.span.start_byte,
                            reference.receiver_expr.as_deref(),
                        )
                    };
                    let class = self.classify_unresolved(
                        &ext.file_path,
                        family,
                        name,
                        reference.receiver_expr.as_deref(),
                        &source_symbol,
                        position,
                    );
                    // A `java.lang` spelling this file may bind as a type
                    // parameter is left unexplained, and says so.
                    let type_parameter_doubt = matches!(class, UnresolvedClass::Unresolved)
                        && matches!(
                            reference.kind,
                            ReferenceKind::Type | ReferenceKind::TypeQualifier
                        )
                        && self.is_possibly_a_type_parameter(&ext.file_path, name);
                    unresolved.push(UnresolvedReference {
                        source_file: ext.file_path.clone(),
                        source_symbol,
                        callee_name: name.to_string(),
                        kind: UnresolvedKind::Reference,
                        resolution: Resolution::Unresolved {
                            reason: format!(
                                "no resolution rung matched {:?} reference {:?} in {} family {:?}{}",
                                reference.kind,
                                name,
                                ext.file_path,
                                family,
                                if type_parameter_doubt {
                                    ": possibly a type parameter"
                                } else {
                                    ""
                                }
                            ),
                        },
                        class,
                        receiver: reference.receiver_expr.clone(),
                        site: Some(SiteBytes::of(&reference.span)),
                    });
                }

                // Resolve routes
                for route in &ext.routes {
                    // The route's node identity, not a bare "VERB /path"
                    // label. `ExtractedRoute::node_id` owns the shape so the
                    // graph export can emit a node under the same id; an edge
                    // whose source names no node leaves every route consumer
                    // reading an empty graph.
                    let route_source = route.node_id(&ext.file_path);

                    // Middleware first in the loop body only because the
                    // handler arm ends in `continue`s; neither depends on the
                    // other. Each named entry binds through the handler's
                    // ladder to a `Registers` edge, or is recorded as not
                    // binding — the same Class A rule: an unbound middleware
                    // must not look like a route that registers none.
                    for entry in route.middleware.iter().flatten() {
                        // An anonymous entry — an arrow function, a `func`
                        // literal — names nothing. The route node lists it by
                        // expression; there is no symbol to bind or miss.
                        if entry.name.is_empty() {
                            continue;
                        }
                        // A package's middleware — `cors()`, chi's
                        // `middleware.Logger`, `express.json()` — is outside
                        // the repository, and the file's own import says so.
                        // Classified `External`, which the dead-code namesake
                        // veto ignores, rather than left to a global name
                        // lookup that would bind it to whatever repository
                        // symbol shares its name.
                        let binding = entry.qualifier.as_deref().unwrap_or(&entry.name);
                        if let Some(module) = self
                            .external_imports
                            .get(&ext.file_path)
                            .and_then(|imports| imports.get(binding))
                        {
                            unresolved.push(UnresolvedReference {
                                source_file: ext.file_path.clone(),
                                source_symbol: route_source.clone(),
                                callee_name: entry.name.clone(),
                                kind: UnresolvedKind::Route,
                                resolution: Resolution::Unresolved {
                                    reason: format!(
                                        "route middleware {:?} in {} comes from external \
                                         module {module:?}",
                                        entry.expression, ext.file_path
                                    ),
                                },
                                class: UnresolvedClass::External {
                                    module: module.clone(),
                                },
                                receiver: entry.qualifier.clone(),
                                site: None,
                            });
                            continue;
                        }
                        match self.bind_route_reference(
                            ext,
                            family,
                            &entry.name,
                            entry.qualifier.as_deref(),
                        ) {
                            Ok((target_f, resolution)) => {
                                let target_symbol = self.qualified_for(&target_f, &entry.name);
                                edges.push(ResolvedEdge::resolved(
                                    ext.file_path.clone(),
                                    target_f,
                                    route_source.clone(),
                                    target_symbol,
                                    EdgeKind::Registers,
                                    Arc::new(resolution),
                                    Some(format!("{}:{}", route.framework, entry.scope.label())),
                                ));
                            }
                            Err(candidate_count) => unresolved.push(UnresolvedReference {
                                source_file: ext.file_path.clone(),
                                source_symbol: route_source.clone(),
                                callee_name: entry.name.clone(),
                                kind: UnresolvedKind::Route,
                                resolution: Resolution::Unresolved {
                                    reason: format!(
                                        "route middleware {:?} in {} bound to none of {} \
                                         same-family candidates",
                                        entry.expression, ext.file_path, candidate_count
                                    ),
                                },
                                class: UnresolvedClass::Unresolved,
                                receiver: entry.qualifier.clone(),
                                site: None,
                            }),
                        }
                    }

                    // An anonymous handler — an Express arrow function — has no
                    // name to resolve, so there is nothing to bind and nothing
                    // to report. Every other route names a handler, and either
                    // binds it or fails to.
                    if route.handler_name.is_empty() {
                        continue;
                    }
                    let route_target =
                        self.bind_route_reference(ext, family, &route.handler_name, None);
                    let (target_f, resolution) = match route_target {
                        Ok(bound) => bound,
                        Err(candidate_count) => {
                        // Class A. A route whose handler did not bind used to
                        // produce no edge and no record, byte-identical to a
                        // route with no named handler at all. `HandlesRoute` is
                        // what tells liveness a handler is reached from outside
                        // the call graph, so both candidates of an ambiguous
                        // bind were then reported dead with nothing saying the
                        // route had been checked and had failed.
                        //
                        // The candidate count is carried so ambiguity is
                        // distinguishable from absence: 0 means no file declares
                        // the name, 2 means the resolver refused to guess.
                        unresolved.push(UnresolvedReference {
                            source_file: ext.file_path.clone(),
                            source_symbol: route_source,
                            callee_name: route.handler_name.clone(),
                            kind: UnresolvedKind::Route,
                            resolution: Resolution::Unresolved {
                                reason: format!(
                                    "route handler {:?} in {} bound to none of {} \
                                     same-family candidates",
                                    route.handler_name, ext.file_path, candidate_count
                                ),
                            },
                            class: UnresolvedClass::Unresolved,
                            receiver: None,
                            site: None,
                        });
                        continue;
                        }
                    };
                    // The handler by its graph identity, the way every
                    // other edge kind names its target. `route.handler_name`
                    // is the bare name the source wrote, which matches no node
                    // and left this edge dangling at both ends.
                    //
                    // Liveness is unaffected, and that is checked rather than
                    // assumed: `called_symbols` inserts both the full target
                    // and its `rsplit("::")` tail, so a routed handler stays
                    // reached under either spelling.
                    let target_symbol = self.qualified_for(&target_f, &route.handler_name);
                    edges.push(ResolvedEdge::resolved(
                        ext.file_path.clone(),
                        target_f,
                        route_source,
                        target_symbol,
                        EdgeKind::HandlesRoute,
                        Arc::new(resolution),
                        Some(route.framework.clone()),
                    ));
                }
                Ok((edges, unresolved, package_groups))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let budget = {
            let cache = global_candidates
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            ResolutionBudgetUsage {
                visits: cache.visits,
                retained_bytes: cache.retained_bytes,
                evidence_bytes: cache.evidence_bytes,
            }
        };

        // Snapshot freshly resolved files before the post-pass mutates the
        // merged edge list. `Arc` payloads are shared; the owned strings are
        // copied. Reused files are not copied into `produced`.
        let mut produced: BTreeMap<String, ReusedFileResolution> = BTreeMap::new();
        let mut edges: Vec<ResolvedEdge> = Vec::new();
        let mut unresolved: Vec<UnresolvedReference> = Vec::new();
        let mut package_groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (ext, (file_edges, file_unresolved, file_packages)) in extractions.iter().zip(per_file)
        {
            let hit = reused
                .get(&ext.file_path)
                .is_some_and(|cached| cached.content_hash == ext.content_hash);
            if !hit {
                produced.insert(
                    ext.file_path.clone(),
                    ReusedFileResolution {
                        content_hash: ext.content_hash,
                        edges: file_edges.clone(),
                        unresolved: file_unresolved.clone(),
                        package_groups: file_packages.clone(),
                    },
                );
            }
            edges.extend(file_edges);
            unresolved.extend(file_unresolved);
            for (package, files) in file_packages {
                package_groups.entry(package).or_default().extend(files);
            }
        }

        // G20: synthetic package / module star edges
        for (pkg_node, files) in package_groups {
            let reason = if pkg_node.starts_with("module:") {
                "Swift module star topology"
            } else {
                "Go package star topology"
            };
            for file in files {
                edges.push(ResolvedEdge::resolved(
                    file.clone(),
                    pkg_node.clone(),
                    file,
                    pkg_node.clone(),
                    EdgeKind::MemberOf,
                    Arc::new(Resolution::Structural {
                        target_symbol: pkg_node.clone(),
                        target_file: pkg_node.clone(),
                    }),
                    Some(reason.to_string()),
                ));
            }
        }

        // A call written at a value nothing can run is not a call of it: a
        // template's `maxlength={MAX_QUERY_LENGTH}` and a `LIMIT()` both *use*
        // the constant, so the edge stays, as the reference it is, and the
        // constant keeps its users.
        if !self.inert_values.is_empty() {
            for edge in edges.iter_mut() {
                if edge.edge_kind == EdgeKind::Calls
                    && self.inert_values.contains(edge.target_symbol.as_str())
                {
                    edge.edge_kind = EdgeKind::References;
                }
            }
        }

        // R4: emission order must not depend on input iteration order. See
        // `edge_order` for the comparator and why the sort must stay stable.
        sort_edges(&mut edges);
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
        sort_unresolved(&mut unresolved);

        debug_assert!(
            self.dangling_swift_targets(&edges).is_empty(),
            "an edge targets a spelling that is no indexed symbol: {:?}",
            self.dangling_swift_targets(&edges)
        );
        Ok(ResolveReuse {
            result: ResolutionResult {
                edges,
                receiver_types: self.receiver_types.clone(),
                reexport_chains: self.reexport_chains.clone(),
                unresolved,
            },
            produced,
            budget,
        })
    }
}

/// Sorts `resolve_pass`'s edges into emission order. The one call production
/// makes, so the tests in `parallel_sort_tests` exercise it rather than a copy:
/// a switch to an unstable sort here is what they catch.
pub(super) fn sort_edges(edges: &mut [ResolvedEdge]) {
    edges.par_sort_by(edge_order);
}

/// Sorts `resolve_pass`'s unresolved ledger into emission order; see
/// [`sort_edges`].
pub(super) fn sort_unresolved(unresolved: &mut [UnresolvedReference]) {
    unresolved.par_sort_by(unresolved_order);
}

/// The emission order of `resolve_pass`'s edges (R4).
///
/// Named so the tie-order pin in `parallel_sort_tests` sorts with the very function
/// production does. The caller uses `par_sort_by`, which is a *stable* merge
/// sort, so rows this comparator calls equal keep their input order exactly as
/// `sort_by` kept them; never switch the caller to an unstable sort.
///
/// Same total order as the tuple form this replaces, evaluated lazily.
/// Building a tuple constructs every element up front, so all four `format!`
/// calls ran on *every* comparison even though tuple comparison short-circuits
/// at the first difference — and the two costly keys are 5th and 7th, reached
/// only when the four string keys ahead of them tie. Formatting a `Resolution`
/// also serializes its whole candidate list. Measured: this sort was 113.5 s of
/// a 130 s build over ~1.06 M pre-dedup edges. `then_with` defers each key
/// until the preceding ones compare equal, which keeps the ordering identical
/// — verified by an edge-ordinal digest over a 4,742-file corpus before and
/// after.
pub(super) fn edge_order(left: &ResolvedEdge, right: &ResolvedEdge) -> std::cmp::Ordering {
    left.source_file
        .cmp(&right.source_file)
        .then_with(|| left.source_symbol.cmp(&right.source_symbol))
        .then_with(|| left.target_file.cmp(&right.target_file))
        .then_with(|| left.target_symbol.cmp(&right.target_symbol))
        .then_with(|| format!("{:?}", left.edge_kind).cmp(&format!("{:?}", right.edge_kind)))
        .then_with(|| {
            left.confidence
                .0
                .to_bits()
                .cmp(&right.confidence.0.to_bits())
        })
        .then_with(|| format!("{:?}", left.resolution).cmp(&format!("{:?}", right.resolution)))
        .then_with(|| left.details.cmp(&right.details))
}

/// The emission order of `resolve_pass`'s unresolved ledger (R4).
///
/// Deliberately not total: rows equal on `(source_file, source_symbol,
/// callee_name)` but differing in kind, receiver or reason tie, and their
/// relative order is output. That is why the caller must stay on a stable sort.
pub(super) fn unresolved_order(
    left: &UnresolvedReference,
    right: &UnresolvedReference,
) -> std::cmp::Ordering {
    (&left.source_file, &left.source_symbol, &left.callee_name).cmp(&(
        &right.source_file,
        &right.source_symbol,
        &right.callee_name,
    ))
}
