use super::*;

impl Resolver {
    /// Resolve a (file, bare name) pair to its graph identity, falling back to
    /// the bare name only when the file genuinely has no such symbol.
    pub(super) fn qualified_for(&self, file: &str, name: &str) -> String {
        self.qualified_names
            .get(&(file.to_string(), name.to_string()))
            .and_then(|value| value.as_deref())
            .unwrap_or(name)
            .to_string()
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
    pub(super) fn scope_declares_local(
        &self,
        file_path: &str,
        enclosing_symbol: &str,
        name: &str,
    ) -> bool {
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

    /// The `(file, declared name)` an import binds `name` to where it is read
    /// in `scope`: Python asks its scope walk ([`Self::python_import_binding`]),
    /// every other family its one flat table.
    pub(super) fn import_binding_at(
        &self,
        family: LangFamily,
        file: &str,
        scope: Option<&str>,
        name: &str,
    ) -> Option<&(String, String)> {
        if family == LangFamily::Python {
            self.python_import_binding(file, scope, name)
        } else {
            self.import_bindings.get(file)?.get(name)
        }
    }

    /// Whether `file` itself binds `name` at module scope without declaring a
    /// symbol for it — a JS/TS private `const fetchThing = vi.fn()`, which the
    /// extractor files under the file's own name in `scope_locals`.
    ///
    /// Deliberately not folded into [`Self::scope_declares_local`]: that one is
    /// asked about receivers too, and a module-scope singleton is a receiver
    /// the member rungs already type; this answers the bare-name question only.
    pub(super) fn file_scope_declares(&self, file: &str, name: &str) -> bool {
        self.scope_locals
            .contains(&(file.to_string(), file.to_string(), name.to_string()))
    }

    /// Whether `root`, written as a receiver's first segment, is a **value**
    /// at this site rather than a module or crate name.
    ///
    /// `serde_json::from_str(x)` and `let serde_json = build(); serde_json.
    /// take()` reduce to the same receiver string, and only the scope's own
    /// binding tables separate them. One owner, because the classifier and the
    /// Rust path rung must agree: a root the classifier calls a value must
    /// never be bound as a crate by the rung.
    ///
    /// A positive use-site fact is authoritative even for an untyped capture.
    /// Absence is not proof of no binding — older or hand-built extractions
    /// may omit site facts — so the file-wide maps are kept as wider evidence.
    pub(super) fn root_is_a_value(
        &self,
        file_path: &str,
        enclosing_symbol: &str,
        root: &str,
        receiver_binding: Option<&LocalBinding>,
    ) -> bool {
        receiver_binding.is_some()
            || self.scope_declares_local(file_path, enclosing_symbol, root)
            || self
                .declared_types
                .contains_key(&format!("{file_path}:{root}@type"))
            || self
                .receiver_types
                .contains_key(&format!("{file_path}:{root}"))
    }

    /// A binding belongs to its lexical scope. An untyped local must veto the
    /// file-wide fallback just as a typed one supplies the scoped answer.
    ///
    /// Also reads [`LocalBinding::declared_type`] / [`LocalBinding::initializer`]
    /// for one-hop typing (`let engine = Engine::new(); engine.tick()`), and
    /// one-hop field receivers (`w.Priority.valid()`, `h.engine.tick()`) via
    /// the field's Type ref keyed by owning type.
    pub(super) fn receiver_type_for(
        &self,
        file: &str,
        scope: Option<&str>,
        name: &str,
        binding: Option<&devmap_extract::model::LocalBinding>,
    ) -> Option<String> {
        let ty = self.receiver_type_unchecked(file, scope, name, binding)?;
        // A Swift file that writes `Entry` cannot tell a top-level `Entry` from
        // a member alias another file introduces (`extension Host { typealias
        // Entry = Item }`); only this pass sees every file. A name some file
        // declares with `typealias` types nothing, as the in-file case already
        // does in the extractor.
        let aliased = self.family_of_file(file) == Some(LangFamily::Swift)
            && self.swift_type_aliases.contains(ty.as_str());
        (!aliased).then_some(ty)
    }

    pub(super) fn receiver_type_unchecked(
        &self,
        file: &str,
        scope: Option<&str>,
        name: &str,
        binding: Option<&devmap_extract::model::LocalBinding>,
    ) -> Option<String> {
        // `rc.borrow_mut().m()`, `m.lock().unwrap().m()`: the method the
        // wrapper's guard hands out is the wrapped value's own, so the guard
        // chain is typed by what the root holds.
        if let Some(root) = Self::rust_wrapper_chain_root(name) {
            if self.family_of_file(file) == Some(LangFamily::Rust) {
                return self.receiver_type_for(
                    file,
                    scope,
                    root,
                    binding.filter(|b| b.name == root),
                );
            }
        }
        if let Some(constructed) = self.swift_constructed_receiver(file, name) {
            return Some(constructed);
        }
        if let Some(chained) = self.swift_chained_receiver(file, scope, name, binding) {
            return Some(chained);
        }
        if let Some((root, field)) = Self::one_hop_field(name) {
            let root_binding = binding.filter(|b| b.name == root);
            let owner = self
                .receiver_type_for(file, scope, root, root_binding)
                .or_else(|| {
                    // The file-wide declared slot holds whichever scope in the
                    // file wrote that name with a type. A Python root the
                    // reading scope binds itself — a local assigned from a
                    // call — is not that scope's parameter of the same name.
                    // Any binding site of the root in the scope or the functions
                    // around it — a parameter (typed or not), a `for`, `with`
                    // or `except` target, a comprehension variable, a walrus,
                    // a nested `def` — makes the name that scope's own, whose
                    // type is the scope-keyed slot or nothing.
                    let shadowed = scope.is_some_and(|scope| {
                        self.family_of_file(file) == Some(LangFamily::Python)
                            && scope != file
                            && self.python_scope_lookup(file, scope, root, 0).is_some()
                            && !self
                                .declared_types
                                .contains_key(&format!("{file}:{scope}:{root}@type"))
                    });
                    (!shadowed)
                        .then(|| self.lookup_declared_type_name(file, scope, root))
                        .flatten()
                })
                .or_else(|| {
                    // `self.lease.run()`. No map records a type under the
                    // keyword, because `self` is not a binding any declaration
                    // wrote — it is the type the call is written inside, which
                    // is what X42 already reads it as one rung down. Without
                    // this the explicit spelling resolved nothing while the bare
                    // `lease.run()` beside it resolved, which is the wrong way
                    // round: the explicit one says strictly more.
                    Self::receiver_is_self(root)
                        .then(|| scope.and_then(|scope| self.declaring_type_of(file, scope)))
                        .flatten()
                        .map(str::to_string)
                });
            return match owner {
                Some(owner) => self.field_type_through(file, scope, &owner, field),
                // `ns.svc.run()`: the root is no typed value, but it may be an
                // imported module whose member is a module-scope instance. A
                // root the scope binds itself shadows the import.
                None if root_binding.is_none()
                    && !scope.is_some_and(|scope| self.scope_declares_local(file, scope, root)) =>
                {
                    self.imported_receiver_type(file, name)
                }
                None => None,
            };
        }

        if let Some(binding) = binding {
            if let Some(from_facts) = Self::type_from_binding_facts(binding) {
                return Some(from_facts);
            }
            if let Some(from_header) = self.type_from_rust_header(file, scope, binding) {
                return Some(from_header);
            }
            // `w := NewWorker()`: the binding's initializer is a factory, and
            // the factory's declaration says what it returns.
            if let Some(from_factory) = binding.initializer.as_deref().and_then(|shape| {
                let family = self.family_of_file(file)?;
                self.factory_return_type(family, file, binding.scope.as_deref(), shape)
            }) {
                return Some(from_factory);
            }
            // A Kotlin local whose initializer states no type (`val vm =
            // createViewModel()`) is still the name the file types everywhere
            // else it is constructed (`val vm = ViewModel(..)`): the file-wide
            // maps below are asked instead of ending the answer here. Every
            // other family keeps the binding's word as final (SC9).
            let scoped = binding.scope.as_deref().and_then(|declaring_scope| {
                self.scoped_receiver_types
                    .get(&format!("{file}:{declaring_scope}:{name}"))
                    .cloned()
            });
            if scoped.is_some() || self.family_of_file(file) != Some(LangFamily::Kotlin) {
                return scoped;
            }
            return self
                .receiver_types
                .get(&format!("{file}:{name}"))
                .cloned()
                .or_else(|| self.lookup_declared_type_name(file, None, name))
                .or_else(|| self.imported_receiver_type(file, name));
        }
        if let Some(scope) = scope {
            if let Some(typed) = self
                .scoped_receiver_types
                .get(&format!("{file}:{scope}:{name}"))
            {
                return Some(typed.clone());
            }
            // This scope's own declaration only: the owner-less `file:name@type`
            // slot is the file-wide rung's, asked last, because it holds
            // whichever type in the file wrote that name.
            if let Some(typed) = self
                .declared_types
                .get(&format!("{file}:{scope}:{name}@type"))
            {
                return Self::admissible_nominal_type(typed);
            }
            if self.scope_declares_local(file, scope, name) {
                return None;
            }
            // An implicit `self`: a bare name inside a method can be the
            // enclosing type's own member. Asked after the scope's own bindings
            // — a local named `lease` shadows the field `lease`, in every
            // language that allows both — and before the file-wide map, which
            // knows nothing about which type a name belongs to.
            if let Some(member) = self.member_declared_type(file, Some(scope), name) {
                return Some(member);
            }
            // A member the enclosing type constructs (`var engine = Other()`)
            // is typed under that type's own scope. Asked before the
            // file-wide maps below, which hold one slot per name and would
            // answer with a sibling type's same-named member.
            if let Some(owner) = self.declaring_type_of(file, scope) {
                if let Some(typed) = [
                    format!("{file}:{file}::{owner}:{name}"),
                    format!("{file}:{owner}:{name}"),
                ]
                .iter()
                .find_map(|key| self.scoped_receiver_types.get(key))
                {
                    return Some(typed.clone());
                }
            }
        }
        self.receiver_types
            .get(&format!("{file}:{name}"))
            .cloned()
            .or_else(|| self.lookup_declared_type_name(file, None, name))
            // Last, because every rung above is evidence written in this file
            // and an import is evidence about another one.
            .or_else(|| self.imported_receiver_type(file, name))
            // A Go package-level value is in scope in every file of its
            // package, with no import between them.
            .or_else(|| self.module_value_type(file, name))
            // A Rust call's own value, used as a receiver: `make().run()`,
            // `open(p)?.read()`. Last, because a name this file binds is a
            // value and every rung above has already looked for it.
            .or_else(|| self.rust_call_value_type(file, scope, name))
    }

    /// The method `method` of `class_type`, or — in Rust, when the type
    /// declares no such method — of the type it `Deref`s to, through at most
    /// [`Self::RUST_DEREF_HOPS`] hops. The type that declared it comes back
    /// with the declaration.
    ///
    /// A type that declares the name itself, even ambiguously, ends the walk:
    /// auto-deref only reaches the target when the receiver's own type has no
    /// such method, and an ambiguity is the answer, not a reason to look
    /// further.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn receiver_method_through_deref(
        &self,
        ext: &Extraction,
        family: LangFamily,
        scope: Option<&str>,
        receiver: &str,
        binding: Option<&LocalBinding>,
        class_type: &str,
        method: &str,
    ) -> Option<(&(String, String), String)> {
        let mut class_type = class_type.to_string();
        for _ in 0..=Self::RUST_DEREF_HOPS {
            let key = (family, class_type.clone(), method.to_string());
            if let Some(hits) = self.type_methods.get(&key) {
                return self
                    .receiver_type_method(ext, scope, receiver, binding, &class_type, hits)
                    .map(|hit| (hit, class_type));
            }
            if family != LangFamily::Rust {
                return None;
            }
            class_type = self.rust_deref_targets.get(&class_type)?.clone()?;
        }
        None
    }

    /// How many `Deref` hops a method lookup follows.
    pub(super) const RUST_DEREF_HOPS: usize = 4;

    /// The one method a Rust blanket impl declares under `method`, for a
    /// receiver whose own type (and every type it derefs to) declares none.
    ///
    /// The impl's bound is not checked — that needs the trait solver — so two
    /// blanket impls declaring the name abstain rather than pick one. One is
    /// bound as `UniqueGlobal` (HIGH), not `ReceiverType`: nothing about the
    /// receiver's type names it, only being the one blanket declaration of the
    /// name in the family. It does not see a method the standard library
    /// supplies under the same name.
    pub(super) fn rust_blanket_method(&self, method: &str) -> Option<&(String, String)> {
        match self.rust_blanket_methods.get(method)?.as_slice() {
            [one] => Some(one),
            _ => None,
        }
    }

    /// `x` for the guard chains `x.borrow`, `x.borrow_mut`, `x.lock.unwrap`,
    /// `x.lock.expect`, `x.read.unwrap`, `x.write.unwrap` (and `expect`) the
    /// extractor spells a receiver `x.lock().unwrap()` as. A field that happens
    /// to share one of those names would be read the same way, which is why the
    /// root's type must still declare the method called on the result.
    pub(super) fn rust_wrapper_chain_root(receiver: &str) -> Option<&str> {
        let (root, chain) = receiver.split_once('.')?;
        matches!(
            chain,
            "borrow"
                | "borrow_mut"
                | "lock.unwrap"
                | "lock.expect"
                | "read.unwrap"
                | "read.expect"
                | "write.unwrap"
                | "write.expect"
        )
        .then_some(root)
        .filter(|root| Self::is_plain_ident(root))
    }

    /// The types a Rust literal can have, as its receiver text spells it:
    /// a string literal is `str` (or `String` for a method both answer), a
    /// suffixed number is its suffix, an unsuffixed one any integer (or float)
    /// type, `true`/`false` a `bool`, a quoted character a `char`. Anything else
    /// is no literal.
    pub(super) fn rust_literal_types(receiver: &str) -> &'static [&'static str] {
        const INTS: &[&str] = &[
            "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
        ];
        const FLOATS: &[&str] = &["f32", "f64"];
        let text = receiver.trim();
        if text.starts_with('"') || text.starts_with("r\"") || text.starts_with("r#") {
            return &["str", "String"];
        }
        if text.starts_with('\'') {
            return &["char"];
        }
        if matches!(text, "true" | "false") {
            return &["bool"];
        }
        if !text.starts_with(|ch: char| ch.is_ascii_digit()) {
            return &[];
        }
        for ty in INTS.iter().chain(FLOATS) {
            if text.ends_with(ty) {
                return std::slice::from_ref(ty);
            }
        }
        if text.contains('.') || (text.contains('e') && !text.starts_with("0x")) {
            FLOATS
        } else {
            INTS
        }
    }

    /// The type of a Rust call's value spelled as a receiver.
    ///
    /// The extractor names a call receiver by the call's callee (`make().run()`
    /// → `make`, `m::make().run()` → `m.make`) and keeps the text of anything
    /// it cannot reduce (`open(p)?.read()` → `open(p)?`). None of those can be
    /// a binding — the caller has already established that — so the written
    /// name is a function and its declared return type, unwrapped as far as a
    /// `?` unwraps it, is the receiver's type.
    ///
    /// Refuses a scope that binds the name itself, and any callee that does not
    /// resolve to exactly one function.
    pub(super) fn rust_call_value_type(
        &self,
        file: &str,
        scope: Option<&str>,
        receiver: &str,
    ) -> Option<String> {
        if self.family_of_file(file)? != LangFamily::Rust {
            return None;
        }
        let (callee, unwrapped) = Self::rust_call_receiver_shape(receiver)?;
        if !callee.contains("::")
            && scope.is_some_and(|scope| self.scope_declares_local(file, scope, &callee))
        {
            return None;
        }
        let (callee_file, callee_identity) = self.rust_called_function(file, scope, &callee)?;
        self.rust_function_value_type(file, &callee_file, &callee_identity, unwrapped)
    }

    /// `(callee path, unwrapped)` for a receiver that can only be a call's
    /// value: `f`, `m.f` or `a::b.f` (the extractor's callee identity, with the
    /// module joined by `.`), or `f(..)?` / `m::f(..)?` (raw text under a `?`).
    /// The callee comes back as a `::` path.
    pub(super) fn rust_call_receiver_shape(receiver: &str) -> Option<(String, bool)> {
        let (body, unwrapped) = match receiver.strip_suffix('?') {
            Some(body) => (body, true),
            None => (receiver, false),
        };
        let callee = if body.ends_with(')') {
            let mut depth = 0usize;
            let mut open = None;
            for (index, ch) in body.char_indices().rev() {
                match ch {
                    ')' => depth += 1,
                    '(' => {
                        depth = depth.checked_sub(1)?;
                        if depth == 0 {
                            open = Some(index);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            &body[..open?]
        } else if unwrapped {
            // `x?.m()` on a value: nothing here says it is a call.
            return None;
        } else {
            body
        };
        let path = match callee.rsplit_once('.') {
            Some((module, name)) if !module.contains('.') => format!("{module}::{name}"),
            Some(_) => return None,
            None => callee.to_string(),
        };
        (!path.is_empty() && path.split("::").all(Self::is_plain_ident))
            .then_some((path, unwrapped))
    }

    /// The one Rust function a call path names: a bare name this file declares
    /// or imports, `module::f` for a module the file's own path rules place, or
    /// `Type::f` for the one associated function a type declares.
    pub(super) fn rust_called_function(
        &self,
        file: &str,
        scope: Option<&str>,
        path: &str,
    ) -> Option<(String, String)> {
        let Some((module, name)) = path.rsplit_once("::") else {
            let found = self
                .lookup_in_package(file, path)
                .filter(|(found_file, found)| {
                    self.symbol_kind_in(found_file, found) == Some(SymbolKind::Function)
                })
                .or_else(|| {
                    let (target, declared) = self.import_bindings.get(file)?.get(path)?;
                    let (target, declared) = self.declaring_site(target, declared);
                    self.lookup_in_package(&target, &declared)
                })?;
            return Some(found);
        };
        let _ = scope;
        if self.names_inline_modules_only(file, module) {
            return self
                .lookup_in_package(file, name)
                .filter(|(found_file, found)| {
                    self.symbol_kind_in(found_file, found) == Some(SymbolKind::Function)
                });
        }
        if let RustModulePlace::Unique(module_file) = self.rust_exact_module(file, module) {
            let (decl_file, decl_name) = self.declaring_site(&module_file, name);
            return self
                .lookup_in_package(&decl_file, &decl_name)
                .filter(|(found_file, found)| {
                    self.symbol_kind_in(found_file, found) == Some(SymbolKind::Function)
                });
        }
        let owner = module.rsplit("::").next()?;
        if owner.starts_with(|ch: char| ch.is_ascii_uppercase()) {
            return self.unique_type_method(LangFamily::Rust, owner, name);
        }
        None
    }

    /// The nominal type a call to the function `(callee_file, callee)` yields
    /// at a call site in `file`, or `None` unless it names one indexed type.
    pub(super) fn rust_function_value_type(
        &self,
        file: &str,
        callee_file: &str,
        callee: &str,
        unwrapped: bool,
    ) -> Option<String> {
        let written = self
            .return_types
            .get(&(callee_file.to_string(), callee.to_string()))?;
        let value = devmap_extract::rustsig::value_type(written, unwrapped)?;
        let mut nominal = Self::admissible_nominal_type(value)?;
        if nominal == "Self" {
            nominal = self.declaring_type_of(callee_file, callee)?.to_string();
        }
        (self.unique_indexed_type(LangFamily::Rust, &nominal)
            || self.unique_indexed_trait(&nominal)
            || self.names_one_type_in_both(file, callee_file, &nominal))
        .then_some(nominal)
    }

    /// A Rust receiver typed `T`, where `T` is a generic parameter of the
    /// calling fn: the one trait that bounds it (`fn f<T: Render>(t: &T)`), so
    /// the call reaches that trait's declaration of the method. A generic with
    /// no usable bound, or several, is no type at all — `None`, rather than
    /// letting a struct elsewhere that happens to be called `T` answer.
    /// Any other type, and every other family, passes through unchanged.
    pub(super) fn rust_generic_receiver_type(
        &self,
        file: &str,
        family: LangFamily,
        caller: Option<&str>,
        ty: String,
    ) -> Option<String> {
        use devmap_extract::rustsig::{generic_param, parse_header, GenericParam};
        if family != LangFamily::Rust {
            return Some(ty);
        }
        let Some(header) = caller
            .and_then(|caller| {
                self.rust_signatures
                    .get(&(file.to_string(), caller.to_string()))
            })
            .and_then(|signature| parse_header(signature))
        else {
            return Some(ty);
        };
        match generic_param(&header, &ty) {
            GenericParam::NotDeclared => Some(ty),
            GenericParam::OneTrait(bound) => Some(bound.to_string()),
            GenericParam::Other => None,
        }
    }

    /// Whether exactly one indexed Rust declaration of this name exists and it
    /// is a trait: what `-> impl Render` / `-> Box<dyn Render>` names. A name
    /// that is a trait in one crate and a struct in another is not one.
    pub(super) fn unique_indexed_trait(&self, name: &str) -> bool {
        let mut rust = self.symbol_index.get(name).into_iter().flatten().filter(
            |(_, kind, candidate_family, _)| {
                LangFamily::Rust.admits(*candidate_family)
                    && matches!(
                        kind,
                        SymbolKind::Class
                            | SymbolKind::Struct
                            | SymbolKind::Enum
                            | SymbolKind::Interface
                            | SymbolKind::Trait
                    )
            },
        );
        matches!(
            (rust.next(), rust.next()),
            (Some((_, SymbolKind::Trait, _, _)), None)
        )
    }

    /// The type a Rust binding has according to a **function header** in the
    /// corpus — the half of binder typing one file cannot do alone, because
    /// the function is usually declared in another file. The extractor states
    /// the shape (see `devmap_extract::rustlocal`); this reads the header.
    ///
    /// * `T::f()` / `T::f()?`: `T` when every indexed `T::f` is declared to
    ///   return `Self` or `T` — through a `Result`/`Option` only when the call
    ///   was unwrapped, because a `Result<T>` is not a `T`. With no `T::new`
    ///   indexed, an un-unwrapped `T::new()` keeps the reading the
    ///   initializer shape always gave it.
    /// * `|f|k|p`: parameter `p` of the closure type `f` declares for its
    ///   argument `k`. `f` is bound only as this file binds it — its own
    ///   lexical scope or a `use` — never by a corpus-wide name match, which
    ///   would type the closure by some other crate's `f`.
    pub(super) fn type_from_rust_header(
        &self,
        file: &str,
        scope: Option<&str>,
        binding: &devmap_extract::model::LocalBinding,
    ) -> Option<String> {
        use devmap_extract::rustsig;
        let shape = binding.initializer.as_deref()?;
        if let Some(closure) = shape.strip_prefix('|') {
            let mut parts = closure.split('|');
            let (callee, arg, param) = (parts.next()?, parts.next()?, parts.next()?);
            let (arg, param): (usize, usize) = (arg.parse().ok()?, param.parse().ok()?);
            let (callee_file, callee_identity) = self
                .lexical_target(file, LangFamily::Rust, scope, callee)
                .map(|identity| (file.to_string(), identity))
                .or_else(|| {
                    let (target_file, target_symbol) =
                        self.import_bindings.get(file)?.get(callee)?;
                    let (target_file, target_symbol) =
                        self.declaring_site(target_file, target_symbol);
                    self.lookup_in_package(&target_file, &target_symbol)
                })?;
            let header = self.rust_signatures.get(&(callee_file, callee_identity))?;
            let header = rustsig::parse_header(header)?;
            return Self::admissible_nominal_type(rustsig::closure_parameter(&header, arg, param)?);
        }
        let (call, unwrapped) = match shape.strip_suffix("()?") {
            Some(call) => (call, true),
            None => (shape.strip_suffix("()")?, false),
        };
        // `f()?` / `m::f()?` — a free function's value, not an associated
        // function's. The last path segment before the name is a module (or
        // absent) rather than a capitalised type.
        let names_a_type = call.rsplit_once("::").is_some_and(|(owner, _)| {
            owner
                .rsplit("::")
                .next()
                .is_some_and(|last| last.starts_with(|ch: char| ch.is_ascii_uppercase()))
        });
        if !names_a_type {
            if !call.contains("::")
                && scope.is_some_and(|scope| self.scope_declares_local(file, scope, call))
            {
                return None;
            }
            let (callee_file, callee) = self.rust_called_function(file, scope, call)?;
            return self.rust_function_value_type(file, &callee_file, &callee, unwrapped);
        }
        let (written_type, function) = call.split_once("::")?;
        if !Self::is_plain_ident(written_type) || !Self::is_plain_ident(function) {
            return None;
        }
        let type_name = if written_type == "Self" {
            self.declaring_type_of(file, scope?)?.to_string()
        } else {
            written_type.to_string()
        };
        let Some(hits) =
            self.type_methods
                .get(&(LangFamily::Rust, type_name.clone(), function.to_string()))
        else {
            return (function == "new" && !unwrapped).then_some(type_name);
        };
        let every_hit_returns_it = hits.iter().all(|(hit_file, hit_identity)| {
            self.rust_signatures
                .get(&(hit_file.clone(), hit_identity.clone()))
                .and_then(|header| rustsig::parse_header(header))
                .is_some_and(|header| rustsig::returns_type(&header, &type_name, unwrapped))
        });
        (!hits.is_empty() && every_hit_returns_it).then_some(type_name)
    }

    /// `root.field` with exactly one hop of plain identifiers — the shape
    /// `w.Priority.valid()` and `h.engine.tick()` need. Deeper chains and
    /// `::` paths are refused: those need inference this resolver does not do.
    pub(super) fn one_hop_field(receiver: &str) -> Option<(&str, &str)> {
        if receiver.contains("::") {
            return None;
        }
        let (root, field) = receiver.split_once('.')?;
        if root.is_empty()
            || field.is_empty()
            || field.contains('.')
            || !Self::is_plain_ident(root)
            || !Self::is_plain_ident(field)
        {
            return None;
        }
        Some((root, field))
    }

    pub(super) fn is_plain_ident(name: &str) -> bool {
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    }

    /// Nominal type from a binding's written annotation or simple initializer.
    ///
    /// Refuses generics, trait objects, and anything that is not a single
    /// path-ending identifier — those need real inference.
    pub(super) fn type_from_binding_facts(
        binding: &devmap_extract::model::LocalBinding,
    ) -> Option<String> {
        if let Some(declared) = binding.declared_type.as_deref() {
            if let Some(ty) = Self::admissible_nominal_type(declared) {
                return Some(ty);
            }
        }
        binding
            .initializer
            .as_deref()
            .and_then(Self::type_from_initializer_shape)
    }

    pub(super) fn type_from_initializer_shape(init: &str) -> Option<String> {
        let init = init.trim();
        if let Some(ty) = init.strip_suffix("::new") {
            return Self::admissible_nominal_type(ty);
        }
        if let Some(ty) = init.strip_suffix("{..}") {
            return Self::admissible_nominal_type(ty);
        }
        None
    }

    pub(super) fn admissible_nominal_type(raw: &str) -> Option<String> {
        let mut s = raw.trim();
        loop {
            let next = s
                .strip_prefix('&')
                .or_else(|| s.strip_prefix('*'))
                .or_else(|| s.strip_prefix("mut "));
            match next {
                Some(rest) => s = rest.trim_start(),
                None => break,
            }
        }
        // A deref-transparent wrapper is not a generic this needs to refuse:
        // `shared_ptr<Service>` *is* a `Service` as far as member access goes.
        //
        // This is the second of the two places that rule is enforced, and it is
        // not redundant with the extractor's. The extractor unwraps a parse
        // *node*, which covers a parameter's written type; a C++ local binding
        // reaches here as the raw text `shared_ptr<Service>` from a different
        // producer, and hit the `contains('<')` refusal below. Measured: the
        // type reference `probe -> Service` resolved while the call
        // `probe -> Service.wait_tick` did not, because the two facts came down
        // different paths and only one had been taught the rule.
        //
        // The *list* is shared — `crate::deref` is the single owner — so the
        // two enforcement points cannot come to disagree about which wrappers
        // are transparent. `Vec<Service>` is still refused here, exactly as
        // before, because `Vec` is not in that list.
        s = devmap_extract::deref::strip_deref_transparent(s);
        // A Rust trait object or opaque type, `dyn Render` / `impl Render`: the
        // value is "some Render", so the trait is the type a method on it is
        // looked up in — which reaches the trait's declaration of the method,
        // never an impl's (see `protocol_dispatch`). Only one named bound
        // qualifies; auto traits and lifetimes add nothing a method can come
        // from, but `dyn A + B` names two traits and is refused.
        let s = match s.strip_prefix("dyn ").or_else(|| s.strip_prefix("impl ")) {
            Some(bounds) => devmap_extract::rustsig::single_trait_bound(bounds)?,
            None => s,
        };
        if s.is_empty()
            || s.contains('<')
            || s.contains('>')
            || s.contains(',')
            || s.starts_with("dyn ")
            || s.starts_with("impl ")
        {
            return None;
        }
        let bare = s.rsplit("::").next()?.rsplit('.').next()?.trim();
        Self::is_plain_ident(bare).then(|| bare.to_string())
    }

    pub(super) fn lookup_declared_type_name(
        &self,
        file: &str,
        scope: Option<&str>,
        name: &str,
    ) -> Option<String> {
        if let Some(scope) = scope {
            if let Some(typed) = self
                .declared_types
                .get(&format!("{file}:{scope}:{name}@type"))
            {
                return Self::admissible_nominal_type(typed);
            }
        }
        self.declared_types
            .get(&format!("{file}:{name}@type"))
            .and_then(|typed| Self::admissible_nominal_type(typed))
    }

    /// The declared type of a member named `name` on the type `scope` sits in.
    ///
    /// What a bare receiver means in a language with an implicit `self`:
    /// `lease.revalidate()` written inside `Holder.run` is `self.lease`, and
    /// `Holder`'s own declaration of `lease` is the file's written answer.
    ///
    /// Reads the *owner-scoped* key, which is the only one a member writes, so
    /// a namesake member on a sibling type in the same file cannot answer here.
    /// Swift, Kotlin, Scala, C#, Java, Python, Ruby and PHP all spell field
    /// access this way; Rust and Go cannot, and reach fields through
    /// [`Self::field_type_on`] instead.
    pub(super) fn member_declared_type(
        &self,
        file: &str,
        scope: Option<&str>,
        name: &str,
    ) -> Option<String> {
        let enclosing = self.declaring_type_of(file, scope?)?;
        // `declaring_type_of` reduces to the bare name; the key is written with
        // the qualified spelling the extractor recorded.
        let qualified = format!("{file}::{enclosing}");
        self.declared_types
            .get(&format!("{file}:{qualified}:{name}@type"))
            .and_then(|typed| Self::admissible_nominal_type(typed))
            // A Kotlin extension is written beside neither its receiver type
            // nor that type's properties: `val Task.label get() = title..`
            // reads `Task.title` as an implicit member, declared where `Task`
            // is. Asked of the one file that declares the type.
            .or_else(|| {
                let family = self.family_of_file(file)?;
                if family != LangFamily::Kotlin {
                    return None;
                }
                let mut owners = self
                    .symbol_index
                    .get(enclosing)?
                    .iter()
                    .filter(|(path, kind, owner_family, _)| {
                        path != file
                            && *owner_family == family
                            && matches!(kind, SymbolKind::Class | SymbolKind::Struct)
                    })
                    .map(|(path, _, _, _)| path.as_str());
                let owner = owners.next()?;
                if owners.next().is_some() {
                    return None;
                }
                self.declared_types
                    .get(&format!("{owner}:{owner}::{enclosing}:{name}@type"))
                    .and_then(|typed| Self::admissible_nominal_type(typed))
            })
            .or_else(|| self.field_initializer_type(file, enclosing, name))
    }

    /// The type a field's own initializer gives it: what the factory
    /// declares it returns, or, for Python's `self.engine = Engine()`, the
    /// class the call names.
    ///
    /// The one reader of `field_initializers`, so the implicit-`self` rung
    /// ([`Self::member_declared_type`]) and the explicit `self.field` rung
    /// ([`Self::field_type_on`]) answer from the same fact. Keyed by the owning
    /// class like every declared slot: class A's `self.engine = Engine()`
    /// never types class B's `engine` in the same file.
    pub(super) fn field_initializer_type(
        &self,
        file: &str,
        owner: &str,
        field: &str,
    ) -> Option<String> {
        if self
            .poisoned_receiver_keys
            .contains(&format!("field-initializer-unowned:{file}:{field}"))
        {
            return None;
        }
        let shape = self
            .field_initializers
            .get(&format!("{file}:{file}::{owner}:{field}"))?;
        let family = self.family_of_file(file)?;
        if family == LangFamily::Python
            && self.python_class_named(file, Some(file), shape).is_some()
        {
            return Some(shape.clone());
        }
        self.factory_return_type(family, file, None, shape)
    }

    /// Record `self.<field> = callee(..)` as `field`'s initializer on the class
    /// that owns the method it is written in, however deeply a closure inside
    /// that method nests it.
    ///
    /// A field is not one type when the file says two things about it, and the
    /// entry is then dropped for good, because a winner picked by extraction
    /// order is the confidently-wrong answer. That covers two different shapes
    /// (in `__init__` and again in a reset method), a callee name the writing
    /// function binds itself (a parameter or a rebound local is not the
    /// imported class), and an assignment whose owning class cannot be found,
    /// which poisons the field name for the whole file. The same shape written
    /// twice is one fact.
    pub(super) fn record_python_field_initializer(
        &mut self,
        ext: &Extraction,
        reference: &ExtractedReference,
        field: &str,
    ) {
        if !matches!(
            reference.kind,
            ReferenceKind::Call | ReferenceKind::Constructor
        ) {
            return;
        }
        let file = ext.file_path.as_str();
        let (Some(scope), Some(shape)) = (
            reference.enclosing_symbol.as_deref(),
            reference.initializer_shape(&ext.calls),
        ) else {
            return;
        };
        let Some(owner) = self.python_owning_class(file, scope) else {
            self.poisoned_receiver_keys
                .insert(format!("field-initializer-unowned:{file}:{field}"));
            return;
        };
        let key = format!("{file}:{file}::{owner}:{field}");
        let poison = format!("field-initializer:{key}");
        if self.poisoned_receiver_keys.contains(&poison) {
            return;
        }
        let callee_root = shape.split('.').next().unwrap_or(&shape);
        let shadowed = self.python_name_is_rebound(file, scope, callee_root);
        match self.field_initializers.get(&key) {
            _ if shadowed => {
                self.field_initializers.remove(&key);
                self.poisoned_receiver_keys.insert(poison);
            }
            Some(existing) if *existing != shape => {
                self.field_initializers.remove(&key);
                self.poisoned_receiver_keys.insert(poison);
            }
            Some(_) => {}
            None => {
                self.field_initializers.insert(key, shape);
            }
        }
    }

    /// The class a Python symbol is written inside, walking out through the
    /// functions and lambdas between: a closure in `C.__init__` writes `C`'s
    /// `self`. `None` when no enclosing class is found.
    pub(super) fn python_owning_class(&self, file: &str, symbol: &str) -> Option<String> {
        let mut current = symbol.to_string();
        for _ in 0..64 {
            let parent = self
                .symbol_parents
                .get(&(file.to_string(), current.clone()))
                .filter(|parent| *parent != file)?
                .clone();
            // The nearest class wins: a class written inside a method owns the
            // `self` its own methods write, and never the outer class's.
            if let Some(class) = self.python_class_scope(file, &parent) {
                return Some(class);
            }
            current = parent;
        }
        None
    }

    /// Type of `field` on the type `owner` names where it is read in `file`.
    ///
    /// Every family but Python writes the owner's field slots under the file the
    /// reader is in ([`Self::field_type_on`]). A Python class is named by its
    /// declaring file, which is not the reading file when the class was
    /// imported or came out of a factory: `result.loss_log` in `run.py`, with
    /// `loss_log: ZqLog` written in `class Holder` in `trainer.py`. So the
    /// owner's own file is asked, and the field's written type — a name in
    /// *that* file's scope — is handed on only when it denotes one class from
    /// the reader's scope as well: the corpus declares it once, or both files
    /// bind the name to the same declaration. The `factory_return_type` rule.
    pub(super) fn field_type_through(
        &self,
        file: &str,
        scope: Option<&str>,
        owner: &str,
        field: &str,
    ) -> Option<String> {
        if self.family_of_file(file) != Some(LangFamily::Python) {
            return self.field_type_on(file, owner, field);
        }
        let owner = Self::admissible_nominal_type(owner)?;
        let Some((owner_file, identity)) = self
            .python_class_named(file, scope, &owner)
            .or_else(|| self.python_unique_class(&owner))
        else {
            // A class this file declares inside a callable or a class names no
            // file-level class; its slots are the reading file's own.
            return self.field_type_on(file, &owner, field);
        };
        let type_key = identity.rsplit("::").next().unwrap_or(&identity);
        let mut nominal = self.field_type_on(&owner_file, type_key, field)?;
        // The annotation is a name in the owner's file: an alias there is the
        // class it aliases, and an import this walk cannot follow names nothing.
        if owner_file != file {
            nominal = self.python_annotation_class(&owner_file, &nominal)?;
        }
        (owner_file == file
            || self.unique_indexed_type(LangFamily::Python, &nominal)
            || self.names_one_type_in_both(file, &owner_file, &nominal))
        .then_some(nominal)
    }

    /// The one Python class called `name` in the corpus, as `(declaring file,
    /// identity)`; `None` when none or several declare it.
    fn python_unique_class(&self, name: &str) -> Option<(String, String)> {
        let mut classes = self
            .symbol_index
            .get(name)?
            .iter()
            .filter(|(_, kind, family, _)| {
                *kind == SymbolKind::Class && *family == LangFamily::Python
            });
        let (path, _, _, identity) = classes.next()?;
        classes
            .next()
            .is_none()
            .then(|| (path.clone(), identity.to_string()))
    }

    /// Type of `field` on `owner`, from the field Type ref indexed at build.
    pub(super) fn field_type_on(&self, file: &str, owner: &str, field: &str) -> Option<String> {
        let owner = Self::admissible_nominal_type(owner)?;
        let keys = [
            format!("{file}:{file}::{owner}:{field}@type"),
            format!("{file}:{owner}:{field}@type"),
        ];
        for key in &keys {
            if let Some(typed) = self
                .declared_types
                .get(key)
                .and_then(|t| Self::admissible_nominal_type(t))
            {
                return Some(typed);
            }
        }
        // Unique Class/Struct hit for the field when the Type ref only
        // populated the scoped receiver map. Keyed by the owning class like
        // the declared slots above: the owner-less `file:field` key is one
        // slot per file, so another class's same-named field would answer.
        [
            format!("{file}:{file}::{owner}:{field}"),
            format!("{file}:{owner}:{field}"),
        ]
        .iter()
        .find_map(|key| self.scoped_receiver_types.get(key))
        .and_then(|t| Self::admissible_nominal_type(t))
        // No annotation and no field Type ref: the initializer is the file's
        // only statement of the type, the same fallback the implicit-`self`
        // rung takes. A declaration always outranks it.
        .or_else(|| self.field_initializer_type(file, &owner, field))
    }
}
