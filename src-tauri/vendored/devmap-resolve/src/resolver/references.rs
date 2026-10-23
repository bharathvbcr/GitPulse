use super::*;

impl Resolver {
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
    pub(super) fn resolve_member_reference(
        &self,
        ext: &Extraction,
        family: LangFamily,
        reference: &ExtractedReference,
        receiver: &str,
        name: &str,
    ) -> Option<ResolvedEdge> {
        let binding = {
            let root = Self::path_root(receiver);
            ext.local_binding_at(reference.span.start_byte, root)
                .or_else(|| ext.local_binding_at(reference.span.start_byte, receiver))
        };
        let receiver_type = self.receiver_type_for(
            &ext.file_path,
            reference.enclosing_symbol.as_deref(),
            receiver,
            binding,
        );
        // Python: a property read (`Engine(1).size`) or attribute of a class
        // written at the reference, or of a typed receiver, through the file's
        // own class identity and the declared bases — the call ladder's rule.
        let python_class = (family == LangFamily::Python)
            .then(|| {
                receiver_type.clone().or_else(|| {
                    (binding.is_none() && Self::is_plain_ident(receiver))
                        .then(|| receiver.to_string())
                })
            })
            .flatten()
            .and_then(|written| {
                self.python_method_on(
                    &ext.file_path,
                    reference.enclosing_symbol.as_deref(),
                    &written,
                    name,
                )
                .ok()
            });
        let class_is_known = python_class.is_some();
        if let Some(Some(found)) = python_class {
            return Some(self.reference_edge(
                ext,
                &found.file,
                &found.symbol,
                reference,
                Resolution::ReceiverType {
                    target_symbol: found.symbol.clone(),
                    target_file: found.file.clone(),
                    receiver_type: found.declaring_type,
                },
            ));
        }
        if let Some(class_type) = receiver_type.filter(|_| !class_is_known) {
            for member in Self::read_members(family, name) {
                if let Some(((target_file, target_symbol), receiver_type)) = self
                    .receiver_method_through_deref(
                        ext,
                        family,
                        reference.enclosing_symbol.as_deref(),
                        receiver,
                        binding,
                        &class_type,
                        &member,
                    )
                {
                    return Some(self.reference_edge(
                        ext,
                        target_file,
                        target_symbol,
                        reference,
                        Resolution::ReceiverType {
                            target_symbol: self.qualified_for(target_file, target_symbol),
                            target_file: target_file.clone(),
                            receiver_type,
                        },
                    ));
                }
                // Swift: a member the type does not write is the one a
                // superclass supplies, as it is for a call.
                if family == LangFamily::Swift
                    && !self.type_methods.contains_key(&(
                        family,
                        class_type.clone(),
                        member.clone(),
                    ))
                {
                    if let Some((target_file, target_symbol, receiver_type)) =
                        self.inherited_method(family, &class_type, &member)
                    {
                        return Some(self.reference_edge(
                            ext,
                            &target_file,
                            &target_symbol,
                            reference,
                            Resolution::ReceiverType {
                                target_symbol: self.qualified_for(&target_file, &target_symbol),
                                target_file: target_file.clone(),
                                receiver_type,
                            },
                        ));
                    }
                }
            }
        }

        // A Rust associated function or method named as a value by its type:
        // `.map(Item::score)`. The receiver is the type itself, written at the
        // use site, so there is no binding to type — the call ladder's rung 2d
        // reads it the same way. Corroborated by this file (declared here or
        // imported here), never by the name existing somewhere in the corpus.
        if family == LangFamily::Rust
            && Self::is_plain_ident(receiver)
            && !Self::receiver_is_self(receiver)
            && receiver.starts_with(|ch: char| ch.is_ascii_uppercase())
            && binding.is_none()
        {
            let key = (family, receiver.to_string(), name.to_string());
            if let Some((target_file, target_symbol)) = self
                .type_methods
                .get(&key)
                .and_then(|hits| self.type_method_target(&ext.file_path, receiver, hits))
            {
                return Some(self.reference_edge(
                    ext,
                    target_file,
                    target_symbol,
                    reference,
                    Resolution::ReceiverType {
                        target_symbol: self.qualified_for(target_file, target_symbol),
                        target_file: target_file.to_string(),
                        receiver_type: receiver.to_string(),
                    },
                ));
            }
        }

        // Swift: `Meter.privDouble` passed by name is a member of the type the
        // receiver spells — the reference half of call rung 2d. The type must be
        // corroborated by this file, as `type_method_target` requires.
        if family == LangFamily::Swift
            && Self::is_plain_ident(receiver)
            && receiver.chars().next().is_some_and(char::is_uppercase)
        {
            for member in Self::read_members(family, name) {
                let key = (family, receiver.to_string(), member);
                if let Some((target_file, target_symbol)) = self
                    .type_methods
                    .get(&key)
                    .and_then(|hits| self.type_method_target(&ext.file_path, receiver, hits))
                {
                    return Some(self.reference_edge(
                        ext,
                        target_file,
                        target_symbol,
                        reference,
                        Resolution::ReceiverType {
                            target_symbol: self.qualified_for(target_file, target_symbol),
                            target_file: target_file.to_string(),
                            receiver_type: receiver.to_string(),
                        },
                    ));
                }
            }
        }

        // A Rust function named as a value through a module path:
        // `commands::run` in `generate_handler![..]`, `.map(util::helper)`. The
        // call ladder's rung 2d' places the module the same way and binds only
        // a function in the one file that path names. A bare first segment is a
        // module only when nothing here binds that name.
        if family == LangFamily::Rust && !Self::receiver_is_self(receiver) {
            let bare_root_is_free = !Self::is_plain_ident(receiver)
                || (binding.is_none()
                    && !self.root_is_a_value(
                        &ext.file_path,
                        reference
                            .enclosing_symbol
                            .as_deref()
                            .unwrap_or(&ext.file_path),
                        receiver,
                        None,
                    )
                    && !self
                        .import_bindings
                        .get(&ext.file_path)
                        .is_some_and(|bindings| bindings.contains_key(receiver)));
            if bare_root_is_free {
                if let Some(found) = self.rust_qualified_call(&ext.file_path, receiver, name, true)
                {
                    if let Some((target_file, target_symbol)) = found.target() {
                        return Some(self.reference_edge(
                            ext,
                            target_file,
                            target_symbol,
                            reference,
                            found.clone(),
                        ));
                    }
                }
            }
        }

        // X47. 1b, the reference half of X42: an implicit receiver dispatches
        // on the type the reference is written inside. `bus.subscribe(
        // self.on_done)` names `Service.on_done` — the method used as a value —
        // and the call ladder has known that since X42 while this one did not.
        //
        // After the typed-receiver rung above, for X42's reason: a scope that
        // writes `this = Other()` has said what `this` is. Before the import
        // rung below, for the opposite one — `self.run` cannot mean an imported
        // free function in any language here, so the import rung is refused for
        // an implicit receiver outright.
        let implicit = Self::receiver_is_self(receiver);
        if implicit {
            let answer = reference.enclosing_symbol.as_deref().and_then(|caller| {
                Self::read_members(family, name)
                    .into_iter()
                    .find_map(|member| {
                        self.implicit_receiver_target(&ext.file_path, family, caller, &member)
                    })
            })?;
            match answer {
                ImplicitReceiverAnswer::Unique {
                    target_file,
                    target_symbol,
                    receiver_type,
                } => {
                    // The declaration's own identity, not the bare spelling:
                    // `qualified_for` answers a bare name only when the file
                    // declares it once, and a second type spelling it left the
                    // edge pointing at `x`, which is no symbol anywhere.
                    return Some(self.reference_edge(
                        ext,
                        &target_file,
                        &target_symbol.clone(),
                        reference,
                        Resolution::ReceiverType {
                            target_symbol,
                            target_file: target_file.clone(),
                            receiver_type,
                        },
                    ));
                }
                // Same rule as the call ladder: an overridden `self.m` is not a
                // deterministic reference target. Returning `None` here refuses
                // the import fallthrough for an implicit receiver.
                ImplicitReceiverAnswer::VirtualOverride
                | ImplicitReceiverAnswer::HeritageCap(_) => return None,
            }
        }

        // A Python module handle loaded by file path. Before the local-binding
        // refusal below, because a function-scoped handle *is* a local
        // binding — the one local whose value is known.
        if let Some(module_file) = self.path_module_for(ext, reference.span.start_byte, receiver) {
            let (resolved_file, resolved_symbol) = self.lookup_in_package(module_file, name)?;
            return Some(self.reference_edge(
                ext,
                &resolved_file,
                &resolved_symbol,
                reference,
                Resolution::ImportScoped {
                    target_symbol: self.qualified_for(&resolved_file, &resolved_symbol),
                    target_file: resolved_file.clone(),
                    imported_from: receiver.to_string(),
                },
            ));
        }
        if ext
            .local_binding_at(reference.span.start_byte, receiver)
            .is_some()
        {
            return None;
        }
        // The call ladder's rung-2b rule, for the same reason.
        if !self.module_member_admitted(&ext.file_path, receiver, name) {
            return None;
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

    /// The module a written type was qualified by, for a reference in type
    /// position: `search` for `paper search.Paper`.
    ///
    /// The one reader of the `@mod` half of `declared_types`, so the resolution
    /// ladder and [`Self::classify_unresolved`] cannot key that index two ways.
    /// Scoped-first for the SC9 reason: a qualifier this scope wrote does not
    /// speak for a same-named binding in another.
    ///
    /// `None` for a bare type, and for a `TypeQualifier` reference itself —
    /// that row *is* the qualifier, and asking what qualifies it would key on
    /// its own binding and answer with itself.
    pub(super) fn type_qualifier_of(
        &self,
        file: &str,
        reference: &ExtractedReference,
    ) -> Option<&str> {
        if reference.kind != ReferenceKind::Type {
            return None;
        }
        let typed = reference.assigned_to.as_deref()?;
        self.qualifier_declared_in(file, reference.enclosing_symbol.as_deref(), typed)
    }

    /// The `@mod` slot for `typed` as written in `scope`.
    ///
    /// A binding written inside a scope is read from that scope's slot alone.
    /// The annotation and its `TypeQualifier` sibling share an enclosing symbol,
    /// so a qualified type is always in its own scope's slot; a miss there
    /// means the binding is bare. Falling back to the file-wide slot on a miss
    /// let `params json.RawMessage` in one function qualify a bare
    /// `var params t` in another. The file-wide slot answers only for a
    /// reference with no enclosing scope (empty `scope` counts as none).
    pub(super) fn qualifier_declared_in(
        &self,
        file: &str,
        scope: Option<&str>,
        typed: &str,
    ) -> Option<&str> {
        let key = match scope.filter(|scope| !scope.is_empty()) {
            Some(scope) => format!("{file}:{scope}:{typed}@mod"),
            None => format!("{file}:{typed}@mod"),
        };
        self.declared_types.get(&key).map(String::as_str)
    }

    /// X46. The type `Self` names in a Rust type annotation, or `None` where
    /// the keyword cannot be given a concrete answer.
    ///
    /// `fn with_god_nodes(self, …) -> Self` returns the type written at the top
    /// of its `impl` block, and that type is `symbol_parents`' answer for the
    /// method — the extractor's own record, reduced the same way
    /// [`Self::declaring_type_of`] reduces it, never a split of the qualified
    /// name on `.`. `impl Render for Widget` puts `Widget` there and not
    /// `Render`, which is what makes this the implementor and not the trait.
    ///
    /// Abstains inside a `trait`. There `Self` is whatever type implements it —
    /// not the trait, and not a type this resolver can name — so answering with
    /// the trait would emit a DETERMINISTIC edge asserting a return type no
    /// implementation has. That is the opposite of X42's answer for
    /// `Self::blank()` in the same position, and deliberately: a *method* named
    /// there is one the trait really does declare.
    ///
    /// Rust only. `Self` is a type keyword in Swift too, but inside a `class`
    /// it means the dynamic type — a subclass this rung would silently name the
    /// base of — so Swift keeps the honest abstention until someone measures it.
    pub(super) fn rust_self_type(&self, file: &str, scope: Option<&str>) -> Option<&str> {
        let enclosing = self.declaring_type_of(file, scope?)?;
        matches!(
            self.symbol_kind_in(file, enclosing)?,
            SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Class
        )
        .then_some(enclosing)
    }

    /// Whether one of `file`'s own import tables binds `qualifier` — the three
    /// halves the import walk splits every specifier into: resolved to an
    /// indexed file, external to the corpus, or repo-relative and unindexed.
    ///
    /// The same three maps [`Self::classify_unresolved`] consults, asked here
    /// as one question: does this file state where that module comes from?
    pub(super) fn qualifier_is_bound(&self, file: &str, qualifier: &str) -> bool {
        self.import_bindings
            .get(file)
            .is_some_and(|bindings| bindings.contains_key(qualifier))
            || self
                .external_imports
                .get(file)
                .is_some_and(|imports| imports.contains_key(qualifier))
            || self
                .unindexed_local_imports
                .get(file)
                .is_some_and(|imports| imports.contains_key(qualifier))
    }

    pub(super) fn resolve_name_reference(
        &self,
        ext: &Extraction,
        family: LangFamily,
        reference: &ExtractedReference,
    ) -> Option<ResolvedEdge> {
        let name = reference.name.rsplit('.').next().unwrap_or(&reference.name);
        if name.is_empty() {
            return None;
        }
        if matches!(
            reference.kind,
            ReferenceKind::Name | ReferenceKind::Call | ReferenceKind::Constructor
        ) && reference.receiver_expr.is_none()
            && ext
                .local_binding_at(reference.span.start_byte, name)
                .is_some()
        {
            return None;
        }
        // X46. `Self` in type position is read as the name of the type the item
        // is written inside, and then answered by the ordinary rungs — so this
        // is a substitution, not a rung. The unresolved ledger keeps `Self`
        // where the substitution finds no type: what failed is the keyword the
        // author wrote, and renaming a failure is not reporting it.
        let name = (family == LangFamily::Rust && reference.kind == ReferenceKind::Type)
            .then(|| {
                (name == "Self")
                    .then(|| {
                        self.rust_self_type(&ext.file_path, reference.enclosing_symbol.as_deref())
                    })
                    .flatten()
            })
            .flatten()
            .unwrap_or(name);
        let prefer_types = matches!(
            reference.kind,
            ReferenceKind::Type | ReferenceKind::Heritage | ReferenceKind::HeritageInterface
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
                // A Ruby module is what `include` mixes in, and the only thing
                // its heritage position can name besides a class.
                || (family == LangFamily::Ruby && kind == SymbolKind::Module)
        };
        // What the same-file rung accepts in type and heritage position.
        //
        // TypeScript and Rust keep types and values in separate namespaces, so
        // a `const Foo` and a `type Foo` coexist and an annotation means the
        // type — the kind filter is what chooses between them. Python has one
        // namespace: a type is whatever a module-scope name is bound to, and
        // the aliases a typed codebase writes most are assignments —
        // `Purpose = Literal[…]`, `T = TypeVar("T")`, `UserId = NewType(…)` —
        // as is a base class built at runtime (`Base = declarative_base()`).
        // The binding lexical scope found is the only candidate there is, so
        // refusing it left every one of them with no reader and reported dead.
        //
        // JavaScript and TypeScript keep the filter for annotations, but a
        // class's `extends` is not one: it takes an expression, evaluated at
        // runtime, and `class Admin extends Mixed` with
        // `const Mixed = mixin(Base)` is the mixin idiom. The extractor files
        // heritage for a `class` only — an interface's `extends` never arrives
        // as `Heritage` — and `implements`, which does name a type, is
        // `HeritageInterface` and stays filtered.
        //
        // Same-file only. The global rung below has no scope proof, and the
        // import rung never filtered by kind.
        let is_lexical_type = |kind: SymbolKind| {
            is_type(kind)
                || (kind == SymbolKind::Variable
                    && (family == LangFamily::Python
                        || (family == LangFamily::JsTs
                            && reference.kind == ReferenceKind::Heritage)))
        };

        // X45. A *qualified* type is resolved by its qualifier, before any
        // rung that reads the bare name.
        //
        // `paper search.Paper` is split by the extractor into a `Type`
        // reference carrying the bare `Paper` — which is what dispatch needs —
        // and a `TypeQualifier` sibling carrying `search`, paired by the
        // binding they annotate (SC25). Until now only `classify_unresolved`
        // read that sibling, and only to *label* the failure; the ladder itself
        // reduced the written type to `Paper` and then answered as though the
        // author had written a bare name. On scholarlm that costs every
        // `search.Paper` outside the package: `Paper` is declared by two
        // packages, so the global tier abstains — correctly — and the one piece
        // of evidence that says which is discarded.
        //
        // First, not last, because the failure it prevents is a wrong answer
        // rather than a missing one: in a file that itself declares a `T`,
        // `t *testing.T` matched the same-file rung and bound a foreign type to
        // a local one at DETERMINISTIC. A qualifier the ladder cannot follow
        // still falls through — an unindexed module is a gap, not a veto.
        if let Some(qualifier) = self.type_qualifier_of(&ext.file_path, reference) {
            if let Some(edge) =
                self.resolve_member_reference(ext, family, reference, qualifier, name)
            {
                return Some(edge);
            }
            // The qualifier could not be followed. Where the file's own import
            // tables *bind* it, that is an answer and not an absence: the
            // module is external to the corpus, or repo-relative and unindexed,
            // and either way no declaration reachable by the bare name can be
            // the one written. Falling through would hand the reference to the
            // rungs that read the name alone — which is how `t *testing.T`
            // bound to a local `type T struct`. A qualifier no import table
            // names is a different case (Rust makes a crate addressable by path
            // with no `use` of its root) and still falls through.
            if self.qualifier_is_bound(&ext.file_path, qualifier) {
                return None;
            }
        }

        // X47. A reference *with a receiver* names something that receiver
        // owns, and the two rungs that can prove which one are the member
        // rungs. They ran last, after the bare-name rungs below, so the ones
        // that cannot see a receiver answered first: `self.on_done` in a file
        // with a module-level `def on_done` resolved to that free function at
        // `SameFile` and `DETERMINISTIC`, handing it a caller it does not have
        // and shielding it from the dead-code pass. That is the same
        // fabricated-caller defect the call ladder's rung 2c was written to
        // stop, in the ladder it was never applied to.
        //
        // So a receiver is asked first and then **disqualifies** every rung
        // below that reads the bare name alone — including the global tier.
        // Falling through to UniqueGlobal here for `obj.attr` is the same
        // fabricated-caller defect the call ladder's receiver gate stops.
        let receiver = reference.receiver_expr.as_deref();
        if let Some(receiver) = receiver {
            if let Some(edge) =
                self.resolve_member_reference(ext, family, reference, receiver, name)
            {
                return Some(edge);
            }
            return None;
        }
        let implicit_receiver = false;

        if receiver.is_none() {
            if let Some(identity) = self.lexical_target(
                &ext.file_path,
                family,
                reference.enclosing_symbol.as_deref(),
                name,
            ) {
                if self.file_declares_as(&ext.file_path, &identity, |kind| {
                    !prefer_types || is_lexical_type(kind)
                }) {
                    return Some(self.reference_edge(
                        ext,
                        &ext.file_path,
                        &identity,
                        reference,
                        Resolution::SameFile {
                            target_symbol: identity.clone(),
                            target_file: ext.file_path.clone(),
                        },
                    ));
                }
            }
        }

        // Swift: a bare name inside a type is `self.name`, and the member may be
        // one the lexical walk cannot see — a superclass's, a protocol
        // extension's, or the type's own declared in another file. Without this
        // such a read was dropped silently, and the property it names reported
        // dead.
        if family == LangFamily::Swift
            && receiver.is_none()
            && reference.kind == ReferenceKind::Name
        {
            if let Some(caller) = reference.enclosing_symbol.as_deref() {
                for member in Self::read_members(family, name) {
                    if let Some(ImplicitReceiverAnswer::Unique {
                        target_file,
                        target_symbol,
                        receiver_type,
                    }) = self.implicit_receiver_target(&ext.file_path, family, caller, &member)
                    {
                        return Some(self.reference_edge(
                            ext,
                            &target_file,
                            &target_symbol,
                            reference,
                            Resolution::ReceiverType {
                                target_symbol: target_symbol.clone(),
                                target_file: target_file.clone(),
                                receiver_type,
                            },
                        ));
                    }
                }
            }
        }

        let mut bound_by_unfollowable_import = false;
        if let Some(bindings) = self
            .import_bindings
            .get(&ext.file_path)
            .filter(|_| receiver.is_none())
        {
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
                // Bound by an import, and the module it names holds no
                // unambiguous declaration of that name — a barrel that
                // `export *`s a package, a submodule handle. The file said where
                // the name comes from, so no namesake elsewhere in the tree can
                // answer for it. The call ladder abstains at this same point
                // (rung 2a); falling through to the global tier here bound the
                // reference to whichever repository symbol shared the spelling.
                // Only the global tier is closed: the package rungs below read
                // a block the language itself defines, not a spelling.
                bound_by_unfollowable_import = true;
            }
        }

        // X45. The package block, for the reference half of the ladder.
        //
        // `func score(paper Paper)` in `search/rank.go` names the `Paper` its
        // own package declares in `search/provider.go`. Measured on scholarlm,
        // 1,770 rows of the defect tier were exactly this — `Paper` 455,
        // `Hypothesis` 449, `AgentSession` 338 — because two packages of that
        // corpus declare `Paper` and the global tier below abstains between
        // them, as it should. The package block is the evidence that says
        // which, and it is written at the top of both files.
        //
        // After the `Name` refusal above, deliberately: a bare identifier
        // mention is the one shape this function declines rather than fails,
        // and a package-scope rung must not be the thing that widens it.
        //
        // Bare names only, for the X47 reason: `search.Paper` written inside
        // package `api` names `search`'s type, and asking `api`'s own package
        // block about the bare `Paper` would answer a question nobody asked.
        if family == LangFamily::Go && receiver.is_none() {
            if let Some((target_file, target_symbol, package_name)) =
                self.same_package_target(&ext.file_path, name, |kind| {
                    !prefer_types || is_type(kind)
                })
            {
                return Some(self.reference_edge(
                    ext,
                    &target_file,
                    name,
                    reference,
                    Resolution::SamePackage {
                        target_symbol,
                        target_file: target_file.clone(),
                        package_name,
                    },
                ));
            }
        }
        if family == LangFamily::Swift && receiver.is_none() {
            if let Some((target_file, target_symbol, package_name)) =
                self.same_swift_module_target(&ext.file_path, name, |kind| {
                    !prefer_types || is_type(kind)
                })
            {
                return Some(self.reference_edge(
                    ext,
                    &target_file,
                    &target_symbol.clone(),
                    reference,
                    Resolution::SamePackage {
                        target_symbol,
                        target_file: target_file.clone(),
                        package_name,
                    },
                ));
            }
        }

        // Name identifiers unique-global to a unique function in another file.
        // That binds `except Exception as e` / `print(e)` / `for _, segment`
        // to a unique `def e` / `func segment` across the language family.
        // Same-file and import-scoped remain; Calls still unique-global.
        //
        // This guards the **global** tier below, and it used to sit above the
        // two scope rungs as well — which caught them by position rather than
        // by the argument. The argument is about matching a bare name *across
        // the family*, where two packages can each declare `Version` and the
        // name alone cannot say which was meant. Inside one package that is not
        // an inference at all: Go's spec puts every package-level identifier in
        // the package block, and `same_package_target` abstains outright when
        // the package declares the name twice
        // (`a_name_the_package_declares_twice_is_an_abstention_not_a_choice`).
        // The same-file and import-scoped rungs have already run by here, so a
        // local variable or an imported name has bound above and cannot reach
        // the package block by mistake.
        //
        // What the refusal cost while it stood above them: a package-level
        // *constant* read as a bare name produced no edge at all, so
        // `cmd/devcouncil/version.go` — whose `Version` `main.go` prints on
        // three lines — was reported by `unwired_candidates` as a file nothing
        // depends on. Types and calls were unaffected, which is why it survived:
        // they do not arrive as `ReferenceKind::Name`.
        //
        // `a_bare_name_still_does_not_reach_another_package` holds the other
        // half, so moving this cannot quietly become deleting it.
        if matches!(reference.kind, ReferenceKind::Name) {
            return None;
        }

        // The call ladder's own classification, not a second copy of it: a name
        // the file imports from outside the tree (or may take from a star
        // import of one), a builtin and a host global all say what the name is,
        // and a repository declaration that merely shares the spelling is no
        // evidence about it. This replaced a JS/TS-only import test and a
        // nine-name Python list.
        if bound_by_unfollowable_import
            || self.bare_name_has_other_evidence(&ext.file_path, family, name)
        {
            return None;
        }
        if let Some(hits) = self.symbol_index.get(name) {
            let family_hits: Vec<_> = hits
                .iter()
                .filter(|(path, kind, candidate_family, _)| {
                    family.admits(*candidate_family)
                        && (!prefer_types || is_type(*kind))
                        // X47, the same restriction X42 put on the call
                        // ladder's global rung: `self.on_done` names a member
                        // of the enclosing type, and a module-level function is
                        // not one.
                        && (!implicit_receiver || matches!(kind, SymbolKind::Method))
                        && (*candidate_family != LangFamily::Go
                            || Self::go_symbol_visible_from(&ext.file_path, path, name))
                })
                .collect();
            if family_hits.len() == 1 {
                let (target_f, _, _, target_identity) = family_hits[0];
                // A Swift type nested in another type is named bare only from
                // inside that type (see `swift_nested_type_is_out_of_scope`).
                if family == LangFamily::Swift
                    && reference.name == name
                    && self.type_qualifier_of(&ext.file_path, reference).is_none()
                    // `A.Inner` arrives as a `Type` reference for `A` and one for
                    // `Inner`, the second starting one byte (the dot) after the
                    // first ends. A qualified spelling names its outer type, so
                    // only a bare `Inner` can be out of scope.
                    && !ext.references.iter().any(|written| {
                        written.kind == ReferenceKind::Type
                            && written.enclosing_symbol == reference.enclosing_symbol
                            && written.span.end_byte + 1 == reference.span.start_byte
                    })
                    && self.swift_nested_type_is_out_of_scope(
                        &ext.file_path,
                        reference.enclosing_symbol.as_deref(),
                        target_f,
                        target_identity,
                    )
                {
                    return None;
                }
                return Some(self.reference_edge(
                    ext,
                    target_f,
                    target_identity,
                    reference,
                    Resolution::UniqueGlobal {
                        target_symbol: target_identity.to_string(),
                        target_file: target_f.clone(),
                        family,
                    },
                ));
            }
        }
        None
    }

    /// The kind `file` declares `name` as, when it declares it exactly once.
    ///
    /// `then_some` **evaluates its argument**, so `(len == 1).then_some(v[0])`
    /// indexes the vector before the length test can guard it: a name the
    /// symbol index holds for *other* files and not for this one panicked with
    /// "the len is 0 but the index is 0". It stood because every caller had
    /// already proved the name was in this file — X46 added one that had not,
    /// and the crash was a build abort rather than a wrong answer. `then` takes
    /// a closure and is evaluated only on the true branch.
    pub(super) fn symbol_kind_in(&self, file: &str, name: &str) -> Option<SymbolKind> {
        self.symbol_index.get(name).and_then(|hits| {
            let file_hits: Vec<_> = hits
                .iter()
                .filter(|(path, _, _, _)| path == file)
                .map(|(_, kind, _, _)| *kind)
                .collect();
            // Two `#[cfg]` variants of one fn share an identity. `len == 1`
            // treated that as "no kind", so a Name reference that
            // `lexical_target` had already uniquely identified — a fn item
            // passed as a value — produced no edge and the fn was
            // confident-dead. Agreeing kinds are one kind.
            let first = *file_hits.first()?;
            file_hits.iter().all(|kind| *kind == first).then_some(first)
        })
    }

    /// Whether `file` declares `identity` as at least one kind `admits`.
    ///
    /// `identity` is the qualified name [`Self::lexical_target`] answered
    /// with, and `symbol_index` holds every declaration under its qualified
    /// name as well as its bare one, so the hits are the declarations of that
    /// one identity and never a same-named member elsewhere in the file.
    ///
    /// The same-file rung's question, which is not [`Self::symbol_kind_in`]'s.
    /// That one answers "what kind is this?" and abstains when the file's
    /// declarations disagree — right for a caller that dispatches on the kind.
    /// The rung only asks whether a declaration the reference may name is
    /// there, and lexical scope has already proved which identity it is. A
    /// name declared twice with two kinds on purpose is TypeScript's companion
    /// idiom — `const User = z.object(…)` beside
    /// `type User = z.infer<typeof User>` — and the abstention left every
    /// annotation of `User` in its own file, and every value read of it, with
    /// no target.
    pub(super) fn file_declares_as(
        &self,
        file: &str,
        identity: &str,
        admits: impl Fn(SymbolKind) -> bool,
    ) -> bool {
        self.symbol_index.get(identity).is_some_and(|hits| {
            hits.iter()
                .any(|(path, kind, _, _)| path == file && admits(*kind))
        })
    }

    /// Resolve one [`ReferenceKind::Selector`] against markup and stylesheet
    /// declarations only.
    ///
    /// Two rungs, and deliberately no third:
    ///
    /// 1. **This file.** A component's `<style>` is scoped to it by every
    ///    framework that has one, and a `data-*` hook is queried by the script
    ///    beside it, so a declaration in the same file is the answer whenever
    ///    there is one — deterministic, exactly as `SameFile` is for code.
    /// 2. **Exactly one file in the repository.** A global stylesheet declares
    ///    `.btn` once and every component that writes `class="btn"` means that
    ///    one. Rated `UniqueGlobal`, the same rung and the same confidence the
    ///    code ladder gives a name with one declaration.
    ///
    /// Several files and no same-file match is **no edge**. Two components that
    /// each scope a `.card` are two different `.card`s, and picking one would
    /// assert a relationship between unrelated components; the honest answer is
    /// that this index cannot say which, and `AmbiguousGlobal` would still put a
    /// speculative edge in the graph.
    pub(super) fn resolve_selector_reference(
        &self,
        ext: &Extraction,
        reference: &ExtractedReference,
    ) -> Option<ResolvedEdge> {
        let declaring = self.selector_symbols.get(&reference.name)?;
        let (target_file, resolution) = if declaring
            .iter()
            .any(|declaration| declaration.file == ext.file_path)
        {
            (
                ext.file_path.clone(),
                Resolution::SameFile {
                    target_symbol: reference.name.clone(),
                    target_file: ext.file_path.clone(),
                },
            )
        } else if declaring.len() == 1 && self.may_cross_files(reference, &declaring[0]) {
            let target = declaring[0].file.clone();
            (
                target.clone(),
                Resolution::UniqueNamespaced {
                    target_symbol: reference.name.clone(),
                    target_file: target,
                },
            )
        } else {
            return None;
        };
        Some(self.reference_edge(ext, &target_file, &reference.name, reference, resolution))
    }

    /// Every Metal shader entry point `ext` declares, into `entry_names`. See
    /// [`Extraction::metal_entry_points`] — a shader's private helper, which the
    /// host can never look up, is never a target.
    pub(super) fn index_entry_names(&mut self, ext: &Extraction) {
        for symbol in ext.metal_entry_points() {
            let declaring = self.entry_names.entry(symbol.name.clone()).or_default();
            if let Err(at) = declaring.binary_search(&ext.file_path) {
                declaring.insert(at, ext.file_path.clone());
            }
        }
    }

    /// Resolve one [`ReferenceKind::EntryName`] against runtime entry points
    /// only.
    ///
    /// One rung: **exactly one file declares an entry point of that name.** A
    /// runtime library is built from every shader source together, and two
    /// files declaring one name would not link, so two candidates mean the
    /// index holds two libraries — a tuning copy beside the real kernels, say —
    /// and which one this string loads is not in the source. That is no edge,
    /// as it is for a selector two components scope.
    ///
    /// **Who names it.** A string inside a function is that function's. One in
    /// a `const` or `static` initializer (`assigned_to` names the item) is
    /// dispatched by whatever reads the item: when the item is itself a symbol
    /// — a `pub const` — it is the source, and the reads already edge to it;
    /// when it is not — a private const, which the extractor does not emit —
    /// each function in this file that reads it is a source, since a private
    /// item has no reader anywhere else. A string nothing narrower owns stays
    /// with the file.
    pub(super) fn resolve_entry_name_reference(
        &self,
        ext: &Extraction,
        reference: &ExtractedReference,
    ) -> Vec<ResolvedEdge> {
        let Some([target_file]) = self.entry_names.get(&reference.name).map(Vec::as_slice) else {
            return Vec::new();
        };
        let edge = |source: &ExtractedReference| {
            let resolution = Resolution::UniqueNamespaced {
                target_symbol: reference.name.clone(),
                target_file: target_file.clone(),
            };
            self.reference_edge(ext, target_file, &reference.name, source, resolution)
        };
        let item = reference
            .assigned_to
            .as_deref()
            .filter(|_| reference.enclosing_symbol.is_none());
        let Some(item) = item else {
            return vec![edge(reference)];
        };
        let item_symbol = format!("{}::{item}", ext.file_path);
        if ext
            .symbols
            .iter()
            .any(|symbol| symbol.qualified_name == item_symbol)
        {
            let mut owned = reference.clone();
            owned.enclosing_symbol = Some(item_symbol);
            return vec![edge(&owned)];
        }
        let mut readers: Vec<&str> = ext
            .references
            .iter()
            .filter(|read| {
                read.kind == ReferenceKind::Name
                    && read.name == item
                    && matches!(read.receiver_expr.as_deref(), None | Some("Self"))
            })
            .filter_map(|read| read.enclosing_symbol.as_deref())
            .collect();
        readers.sort_unstable();
        readers.dedup();
        if readers.is_empty() {
            return vec![edge(reference)];
        }
        readers
            .into_iter()
            .map(|reader| {
                let mut owned = reference.clone();
                owned.enclosing_symbol = Some(reader.to_string());
                edge(&owned)
            })
            .collect()
    }

    /// Whether a selector may reach a declaration in *another* file at all.
    ///
    /// Uniqueness is not sufficient here, which is what separates this rung from
    /// the code ladder's. A name declared exactly once is still unreachable when
    /// the two ends are scoped to different components, and an edge between them
    /// is false rather than uncertain. Two ways for it to be reachable, and
    /// nothing else:
    ///
    /// 1. **The declaration is global.** A `.css`/`.scss`/`.less` file, or a
    ///    `<style>` in a plain `.html` page, applies to the whole document, so
    ///    every component that names one of its classes means that one.
    /// 2. **The name is an `id`.** An id is unique *per document*, not per
    ///    component, so one component's `aria-controls="task-archive-dock"`
    ///    naming another's `id` is the ordinary correct case — it is how an ARIA
    ///    relationship is written across a page.
    ///
    /// Everything else abstains. Measured on GitPulse, this is the difference
    /// between 672 cross-file edges with three wrong ones and 669 with none: the
    /// three were `.selected`, `.sheet-body` and `.status`, each declared in one
    /// component's scoped `<style>` and named by a different component's markup,
    /// where the real declaration is either global CSS this index never saw or
    /// absent altogether.
    ///
    /// **A third clause was considered and removed**, which is worth recording
    /// because it reads as obviously right: *the reference is global*, on the
    /// grounds that a rule in a global stylesheet selects across the whole
    /// document. It has no correct producer. A stylesheet's selectors are read as
    /// *declarations*, not uses, so a global sheet emits almost no references to
    /// admit — and the one shape the clause did admit was wrong: a plain page's
    /// `class="x"` reaching a `.x` that only a component scopes, which that
    /// component's compiled CSS cannot match.
    ///
    /// The residual, stated rather than implied: clause 2 assumes the two
    /// components render into the same document, which is true of a single-page
    /// application and is not something this index proves. Two separately-mounted
    /// pages could each carry an element with that id and the edge would join the
    /// wrong pair. That is why an id crossing files is rated `HIGH` and never
    /// `DETERMINISTIC`.
    pub(super) fn may_cross_files(
        &self,
        reference: &ExtractedReference,
        declaration: &SelectorDeclaration,
    ) -> bool {
        declaration.in_global_sheet || reference.name.starts_with('#')
    }

    /// Whether `symbol` in `file` is an interface (protocol, trait) declaration.
    pub(super) fn declares_interface(&self, file: &str, symbol: &str) -> bool {
        let bare = symbol.rsplit("::").next().unwrap_or(symbol);
        self.symbol_index.get(bare).is_some_and(|hits| {
            hits.iter().any(|(path, kind, _, identity)| {
                path == file
                    && *kind == SymbolKind::Interface
                    && (identity.as_ref() == symbol || identity.rsplit("::").next() == Some(bare))
            })
        })
    }

    pub(super) fn reference_edge(
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
        // The confidence comes from the rung that found the target, exactly as
        // it does for a call. This function used to stamp every reference
        // `DETERMINISTIC`, including the bare-name `UniqueGlobal` rung, so a
        // reference resolved on evidence the call ladder rates `HIGH` outranked
        // that call — and a `min_confidence = 1.0` query kept the weaker one.
        ResolvedEdge::resolved(
            ext.file_path.clone(),
            target_file.to_string(),
            source_symbol,
            target_symbol,
            // The edge kind follows the reference kind, so `Extends` and
            // `Implements` come out of the ladder that already exists — same
            // rungs, same confidences, an ambiguous supertype resolving to
            // nothing as usual. Both variants were declared with no producer;
            // every reference to them was a label map or a string parser.
            match reference.kind {
                // Swift, Kotlin and Python write a base class and an interface
                // in one syntax, so the extractor can only say `Extends`. The
                // resolver knows what the supertype is: an indexed interface is
                // implemented, not extended.
                ReferenceKind::Heritage if self.declares_interface(target_file, target_sym) => {
                    EdgeKind::Implements
                }
                ReferenceKind::Heritage => EdgeKind::Extends,
                ReferenceKind::HeritageInterface => EdgeKind::Implements,
                _ => EdgeKind::References,
            },
            Arc::new(resolution),
            Some(format!("{:?}", reference.kind)),
        )
    }

    /// Start the module ladder at the deepest rung that could match an indexed
    /// file, instead of at the specifier's full length.
    ///
    /// The ladder builds a candidate path from `parts`, probes it, pops one
    /// segment and repeats — so it visits every prefix of `parts` from longest
    /// to shortest, and its cost is quadratic in `parts.len()`. That length is
    /// bounded only by the extractor's source-size limit, which makes a single
    /// `use crate::a::a::…;` line a build-length stall.
    ///
    /// Lossless, because the rungs it skips are exactly the ones that could not
    /// have matched: a candidate path with more `/`-separated components than
    /// the deepest path this resolver indexed is absent from `file_symbols` by
    /// construction, so `contains_key` on it is `false` without being asked.
    /// Every shorter prefix is still visited, in the same order, because the
    /// ladder continues to pop from the truncated vector.
    ///
    /// See [`Self::max_indexed_path_depth`] for the measurement.
    pub(super) fn trim_to_indexed_depth(&self, parts: &mut Vec<&str>) {
        // `+ 1` because `crate::a::b` probes `src/a/b.rs`, which has one more
        // path component than the specifier has segments. Deliberately
        // generous: this must never cut a rung that could have matched.
        let ceiling = self.max_indexed_path_depth.saturating_add(1);
        if parts.len() > ceiling {
            parts.truncate(ceiling);
        }
    }

    /// Where an absolute Python import `py_path` (`pkg/util`) may live, beyond
    /// the repository root and `src/`: the source roots of every manifest
    /// governing `current_file`, nearest manifest first, and failing those the
    /// one directory that holds a top-level package of that name. Each root
    /// yields the module file and the package `__init__.py`.
    pub(super) fn python_root_candidates(&self, current_file: &str, py_path: &str) -> Vec<String> {
        let mut roots: Vec<&str> = self
            .python_manifest_roots
            .iter()
            .filter(|set| set.dir.is_empty() || current_file.starts_with(&format!("{}/", set.dir)))
            .flat_map(|set| set.roots.iter().map(String::as_str))
            .collect();
        let first = py_path.split('/').next().unwrap_or(py_path);
        // The standard library owns its names: a repository directory called
        // `logging` is not evidence about `import logging`, so the fallback
        // abstains for them like it does for a name two directories hold.
        if let Some(parents) = self
            .py_package_parents
            .get(first)
            .filter(|_| !crate::builtins::is_python_stdlib_module(first))
        {
            // Held by two directories, it names neither: abstain.
            if parents.len() == 1 {
                roots.extend(parents.iter().map(String::as_str));
            }
        }
        roots
            .into_iter()
            .filter(|root| !root.is_empty())
            .flat_map(|root| {
                [
                    format!("{root}/{py_path}.py"),
                    format!("{root}/{py_path}/__init__.py"),
                ]
            })
            .collect()
    }
}
