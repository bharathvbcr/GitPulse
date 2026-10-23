use super::*;

impl Resolver {
    pub(super) fn global_candidates(
        &self,
        cache: &Mutex<GlobalCandidateCache>,
        file: &str,
        family: LangFamily,
        name: &str,
        caller_in_type: bool,
        limits: ResolutionLimits,
    ) -> Result<Arc<GlobalCandidates>, ResolutionLimitError> {
        // Visibility is part of the cache key: unexported Go names are visible
        // from their directory, and a private Swift declaration, a non-exported
        // TS binding or a non-`pub` Rust item from another crate from nowhere
        // else. Exported names and the other families share one view.
        let hits = self
            .symbol_index
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let hidden_somewhere = hits.iter().any(|(path, _, _, identity)| {
            self.unexported_symbols
                .contains(&(path.clone(), identity.to_string()))
        });
        let view = if family == LangFamily::Go && !Self::go_name_is_exported(name) {
            Self::parent_dir(file)
        } else if hidden_somewhere && Self::visibility_is_enforced(family) {
            match family {
                LangFamily::Rust => self.rust_crate_src_root(file),
                _ => file.to_string(),
            }
        } else {
            String::new()
        };
        let key = (
            family,
            name.to_string(),
            format!("{view}|{}", u8::from(caller_in_type)),
        );
        let mut cache = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(known) = cache.sets.get(&key) {
            return Ok(Arc::clone(known));
        }
        charge(
            &mut cache.visits,
            hits.len() as u64,
            limits.candidate_visits,
            "candidate visits",
        )?;
        let mut candidates = Vec::new();
        for (path, kind, candidate_family, identity) in hits {
            if family.admits(*candidate_family)
                && !Self::never_a_call_target(family, *kind)
                && (*candidate_family != LangFamily::Go
                    || Self::go_symbol_visible_from(file, path, name))
                && (!Self::family_needs_explicit_receiver(family)
                    || self.declared_at_file_level(path, identity))
                && self.candidate_is_visible_from(family, file, path, identity)
                && (caller_in_type
                    || *kind != SymbolKind::Method
                    || !Self::method_needs_a_type_scope(family))
            {
                let bytes =
                    (std::mem::size_of::<(String, String)>() + path.len() + identity.len()) as u64;
                charge(
                    &mut cache.retained_bytes,
                    bytes,
                    limits.retained_candidate_bytes,
                    "retained candidate bytes",
                )?;
                candidates.push((path.clone(), identity.to_string()));
            }
        }
        let declarations = candidates.len();
        candidates.sort();
        candidates.dedup();
        // Conservative JSON/Debug upper bound, computed once per shared set.
        let evidence_bytes = candidates.iter().fold(64u64, |total, (file, symbol)| {
            total.saturating_add(16 + escaped_evidence_bytes(file) + escaped_evidence_bytes(symbol))
        });
        let candidates = Arc::new(GlobalCandidates {
            rows: candidates.into(),
            declarations,
            evidence_bytes,
        });
        cache.sets.insert(key, Arc::clone(&candidates));
        Ok(candidates)
    }

    /// Families whose declarations carry a visibility the extractor reads into
    /// `is_exported`, and the bare-name rung must honour (S2): a Swift
    /// `private`/`fileprivate` declaration is invisible outside its file, a
    /// non-exported TS/JS module binding outside its module, and a non-`pub`
    /// Rust item outside its crate.
    pub(super) fn visibility_is_enforced(family: LangFamily) -> bool {
        matches!(
            family,
            LangFamily::Swift | LangFamily::JsTs | LangFamily::Rust
        )
    }

    /// Whether a JS/TS file is a module, so that an un-exported top-level
    /// binding is private to it. A classic script shares its top-level names
    /// with every other script on the page, so for one the absence of `export`
    /// proves nothing.
    pub(super) fn is_js_module(ext: &Extraction) -> bool {
        // The extractor files one synthetic export for every file, so the
        // export list says nothing; a real `export` shows as an exported
        // non-file symbol.
        matches!(ext.language.as_str(), "typescript" | "tsx")
            || !ext.imports.is_empty()
            || ext
                .symbols
                .iter()
                .any(|sym| sym.is_exported && sym.kind != SymbolKind::File)
    }

    /// Whether `candidate` (declared in `path`) can be named from `file`.
    pub(super) fn candidate_is_visible_from(
        &self,
        family: LangFamily,
        file: &str,
        path: &str,
        identity: &str,
    ) -> bool {
        if path == file
            || !Self::visibility_is_enforced(family)
            || !self
                .unexported_symbols
                .contains(&(path.to_string(), identity.to_string()))
        {
            return true;
        }
        family == LangFamily::Rust
            && self.rust_crate_src_root(file) == self.rust_crate_src_root(path)
    }

    /// Swift: whether the member `hit` (a `type_methods` entry) of the type
    /// spelled `type_name` can be reached from `file`.
    ///
    /// `type_methods` is keyed by the bare type name, so a `private struct Rng`
    /// nested in one file and a top-level `Rng` in another share a key. The
    /// member itself may be `private`, and so may the type that declares it —
    /// the type declarations named `type_name` in the hit's own file are the
    /// evidence, because the member's parent identity can lack the nesting
    /// (`Wrapper.Rng` declares `Rng.next`). A file that declares no type of that
    /// name (an `extension` of a type declared elsewhere) says nothing about
    /// the type's visibility, so the member alone decides.
    pub(super) fn swift_member_hit_is_visible_from(
        &self,
        file: &str,
        hit: &(String, String),
        type_name: &str,
    ) -> bool {
        if !self.candidate_is_visible_from(LangFamily::Swift, file, &hit.0, &hit.1) {
            return false;
        }
        let mut declared = self
            .symbol_index
            .get(type_name)
            .into_iter()
            .flatten()
            .filter(|(path, kind, family, _)| {
                *path == hit.0
                    && *family == LangFamily::Swift
                    && matches!(
                        kind,
                        SymbolKind::Class
                            | SymbolKind::Struct
                            | SymbolKind::Enum
                            | SymbolKind::Interface
                    )
            })
            .peekable();
        declared.peek().is_none()
            || declared.any(|(path, _, _, identity)| {
                self.candidate_is_visible_from(LangFamily::Swift, file, path, identity)
            })
    }

    /// Languages whose method is reachable by a bare name only from inside a
    /// type: a Kotlin or Swift function outside every type has no implicit
    /// receiver to dispatch on, so a method namesake is not what it calls.
    pub(super) fn method_needs_a_type_scope(family: LangFamily) -> bool {
        matches!(family, LangFamily::Kotlin | LangFamily::Swift)
    }

    /// Whether the call's enclosing symbol sits (at any depth) inside a type.
    /// Computed only for the families that ask; it is `false` elsewhere.
    pub(super) fn call_is_inside_a_type(
        &self,
        file: &str,
        family: LangFamily,
        caller: Option<&str>,
    ) -> bool {
        if !Self::method_needs_a_type_scope(family) {
            return false;
        }
        let mut scope = caller.unwrap_or(file).to_string();
        for _ in 0..1024 {
            if scope == file {
                return false;
            }
            let is_type = self.symbol_index.get(&scope).is_some_and(|hits| {
                hits.iter().any(|(path, kind, _, identity)| {
                    path == file
                        && identity.as_ref() == scope
                        && matches!(
                            kind,
                            SymbolKind::Class
                                | SymbolKind::Struct
                                | SymbolKind::Enum
                                | SymbolKind::Interface
                                | SymbolKind::Trait
                                | SymbolKind::Module
                        )
                })
            });
            if is_type {
                return true;
            }
            match self.symbol_parents.get(&(file.to_string(), scope.clone())) {
                Some(parent) => scope = parent.clone(),
                None => return false,
            }
        }
        false
    }

    /// Evidence about a bare callee name that outranks its spelling matching a
    /// repository declaration (S2): this file imports the name from a package
    /// or from a repo-relative path that is not indexed, or it is a builtin or
    /// a host global of the language. Each such name is classified by
    /// `classify_unresolved` instead of being bound to a namesake.
    pub(super) fn bare_name_has_other_evidence(
        &self,
        file: &str,
        family: LangFamily,
        name: &str,
    ) -> bool {
        self.file_scope_declares(file, name)
            || self.names_a_non_local_import(file, name)
            || crate::builtins::is_builtin(family, name)
            || crate::builtins::host_global_environment(family, name).is_some()
    }

    /// Whether a declaration of `kind` can never be what a bare call in
    /// `family` names, so the global rung must not offer it.
    ///
    /// `Interface` is an interface or a type alias. In Rust a type alias is not
    /// a constructor (`type HWND = isize; HWND(0)` is E0423) and a trait is its
    /// own kind; a Python `type V = …` alias raises when called; a TypeScript
    /// interface or alias has no runtime value. So for these families an
    /// `Interface` namesake is never the callee — yet once type aliases became
    /// symbols, `HWND(ptr)` building the external `windows` crate's tuple
    /// struct bound to a framework's `pub type HWND = isize` at `UniqueGlobal`,
    /// thirteen fabricated callers on one corpus. Kotlin (`fun interface`
    /// SAM conversions) and Java (`new Listener() { … }`) can name an interface
    /// in call position, so they keep the candidate.
    pub(super) fn never_a_call_target(family: LangFamily, kind: SymbolKind) -> bool {
        // A macro is invoked as `name!(..)`, which its own reference records;
        // `name()` is a function call, so a macro is never its target.
        kind == SymbolKind::Macro
            // A Swift property (or enum case) is a member its owner's receiver
            // names, and `name()` never reaches one through a bare spelling: the
            // bare-name rungs would otherwise bind any unique namesake in the
            // module, which is the unique-global defect (ft-0e6cc90f) on the
            // property surface. A typed or implicit `self` receiver reaches it
            // through `type_methods`, which this does not touch.
            || Self::is_swift_member_property(family, kind)
            || (kind == SymbolKind::Interface
                && matches!(
                    family,
                    LangFamily::Rust | LangFamily::Python | LangFamily::JsTs
                ))
    }

    /// Whether a symbol of `kind` in `family` is a member the language reaches
    /// only through a receiver: a Swift stored or computed property, or an enum
    /// case (the two share `Field`).
    pub(super) fn is_swift_member_property(family: LangFamily, kind: SymbolKind) -> bool {
        family == LangFamily::Swift && kind == SymbolKind::Field
    }

    /// Swift use edges whose target is not the identity of a symbol the target
    /// file declares — `source -> x` where `x` is a bare member spelling.
    ///
    /// The invariant every use edge must keep: its target names a declaration.
    /// `qualified_for` answers a bare spelling only when the file declares it
    /// once, so a rung that passes the spelling instead of the declaration's own
    /// identity emits a dangling edge exactly when a second type reuses the name.
    /// Checked by `resolve_all` in debug builds and by the resolve tests through
    /// [`Self::dangling_targets`].
    pub(super) fn dangling_swift_targets(&self, edges: &[ResolvedEdge]) -> Vec<String> {
        edges
            .iter()
            .filter(|edge| {
                matches!(
                    edge.edge_kind,
                    EdgeKind::Calls
                        | EdgeKind::References
                        | EdgeKind::Extends
                        | EdgeKind::Implements
                ) && edge.target_file.ends_with(".swift")
            })
            .filter(|edge| {
                !self
                    .symbol_index
                    .get(&edge.target_symbol)
                    .is_some_and(|hits| {
                        hits.iter().any(|(path, _, _, identity)| {
                            *path == edge.target_file && **identity == *edge.target_symbol
                        })
                    })
            })
            .map(|edge| format!("{} -> {}", edge.source_symbol, edge.target_symbol))
            .collect()
    }

    /// The edges of `result` that [`Self::dangling_swift_targets`] rejects, for
    /// tests that assert the invariant on a resolution's output.
    pub fn dangling_targets(&self, result: &ResolutionResult) -> Vec<String> {
        self.dangling_swift_targets(&result.edges)
    }

    /// The `type_methods` member key a property is filed under. A property and
    /// a method of one base name (`var size` beside `func size(_:)`) are two
    /// members of one type, and a call may reach only the method while a read
    /// reaches the property first. `\u{1}` is no character of a Swift
    /// identifier, so no method key can equal one.
    pub(super) fn property_member(name: &str) -> String {
        format!("\u{1}{name}")
    }

    /// The member keys a *read* of `name` consults, in order: a Swift property
    /// first, then a method or enum case of that name used as a value
    /// (`map(self.helper)`). A call consults only the plain key.
    pub(super) fn read_members(family: LangFamily, name: &str) -> Vec<String> {
        if family == LangFamily::Swift {
            vec![Self::property_member(name), name.to_string()]
        } else {
            vec![name.to_string()]
        }
    }

    pub(super) fn parent_dir(current_file: &str) -> String {
        // `Path` splits on `/` only on Unix, so `a\b.go` would have no parent
        // (`.`). A repo-relative path may be spelled with either separator, and
        // the answer is always `/`-separated.
        let normalized = current_file.replace('\\', "/");
        Path::new(&normalized)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| ".".to_string())
    }

    /// Mark Go package directories whose files on disk the index does not
    /// fully hold, so the unexported-selector rung abstains there.
    ///
    /// The extractions can vouch for the files they are; they cannot say that
    /// no other `.go` file shares their directory — a gitignored generated
    /// file is compiled into the package and read by nobody. The builder
    /// supplies that fact from disk (`devmap_extract::go_dirs_with_unindexed_files`).
    /// Call it **after** [`Self::index_extractions`], which resets the set.
    pub fn mark_go_dirs_incomplete(&mut self, dirs: impl IntoIterator<Item = String>) {
        self.go_package_dirs_unvouched.extend(dirs);
    }

    /// `Some` when the last [`Self::index_extractions`] stopped expanding glob
    /// re-exports at `REEXPORT_GLOB_MAX_HOPS`, so every `reexport_chains`
    /// answer derived from it — and every edge resolved through one — rests on
    /// a partial map. `None` means the expansion ran to its fixpoint.
    pub fn reexport_glob_cutoff(&self) -> Option<ReexportGlobCutoff> {
        self.reexport_glob_cutoff
    }

    pub(super) fn go_name_is_unexported(name: &str) -> bool {
        !name.is_empty() && !Self::go_name_is_exported(name)
    }

    pub(super) fn go_name_is_exported(name: &str) -> bool {
        name.rsplit('.')
            .next()
            .unwrap_or(name)
            .chars()
            .next()
            .is_some_and(|ch| ch.is_uppercase())
    }

    /// Unexported Go names are package-private. Unique-global must not bind
    /// `segment` in `api/gateway.go` to `adkeval.HallucinationsEvaluator.segment`.
    pub(super) fn go_symbol_visible_from(source_file: &str, target_file: &str, name: &str) -> bool {
        Self::go_name_is_exported(name)
            || Self::parent_dir(source_file) == Self::parent_dir(target_file)
    }

    /// Bind a name a route registration wrote — its handler, or a piece of its
    /// middleware — to the declaration it names.
    ///
    /// The one ladder for both, so a handler and a middleware written the same
    /// way bind the same way: a single same-file declaration, then this file's
    /// import binding, then a single same-family declaration anywhere (a Go
    /// one only if it is visible from here). `Err` carries how many
    /// same-family candidates the last rung saw, so ambiguity (2) and absence
    /// (0) stay distinguishable in the ledger.
    ///
    /// `qualifier` is the binding a qualified reference went through —
    /// `middleware` in `middleware.Logger`. A qualified name is never a
    /// same-file or import-bound bare name, so those rungs are skipped, and the
    /// global rung admits only a declaration whose directory or file stem *is*
    /// the qualifier: Go names a package after its directory and a namespace
    /// import after its module. Without that narrowing, chi's
    /// `middleware.Logger` would bind to whichever one `Logger` the repository
    /// happens to declare.
    pub(super) fn bind_route_reference(
        &self,
        ext: &Extraction,
        family: LangFamily,
        name: &str,
        qualifier: Option<&str>,
    ) -> Result<(String, Resolution), usize> {
        let Some(hits) = self.symbol_index.get(name) else {
            return Err(0);
        };
        if qualifier.is_none() {
            let mut same_file = hits.iter().filter(|(path, ..)| path == &ext.file_path);
            if let (Some((target_f, ..)), None) = (same_file.next(), same_file.next()) {
                return Ok((
                    target_f.clone(),
                    Resolution::SameFile {
                        target_symbol: name.to_string(),
                        target_file: target_f.clone(),
                    },
                ));
            }
            if let Some((target_f, target_symbol)) = self
                .import_bindings
                .get(&ext.file_path)
                .and_then(|bindings| bindings.get(name))
            {
                return Ok((
                    target_f.clone(),
                    Resolution::ImportScoped {
                        target_symbol: target_symbol.clone(),
                        target_file: target_f.clone(),
                        imported_from: name.to_string(),
                    },
                ));
            }
        }
        let family_hits: Vec<_> = hits
            .iter()
            .filter(|(path, _, candidate_family, _)| {
                family.admits(*candidate_family)
                    && (*candidate_family != LangFamily::Go
                        || Self::go_symbol_visible_from(&ext.file_path, path, name))
                    && qualifier.is_none_or(|qualifier| {
                        let dir = Self::parent_dir(path);
                        let stem = path
                            .rsplit('/')
                            .next()
                            .and_then(|file| file.split('.').next())
                            .unwrap_or_default();
                        dir.rsplit('/').next() == Some(qualifier) || stem == qualifier
                    })
            })
            .collect();
        match family_hits.as_slice() {
            [(target_f, ..)] => Ok((
                target_f.clone(),
                Resolution::UniqueGlobal {
                    target_symbol: name.to_string(),
                    target_file: target_f.clone(),
                    family,
                },
            )),
            many => Err(many.len()),
        }
    }
}
