use super::*;

impl Resolver {
    /// The modules a Swift file is compiled into.
    ///
    /// A `Package.swift` target owns the folders it names exclusively (the
    /// deepest claim wins); otherwise every XcodeGen target listing a folder
    /// above the file compiles it, so `Shared/` belongs to both targets that
    /// list it. A file no manifest claims falls back to the path heuristic
    /// (`swift_module_of`), which is right for the conventional layouts.
    pub(super) fn swift_modules_of(&self, file: &str) -> Vec<String> {
        let claim = |target: &devmap_extract::SwiftTarget| -> Option<usize> {
            target
                .paths
                .iter()
                .filter(|path| {
                    file == path.as_str()
                        || file
                            .strip_prefix(path.as_str())
                            .is_some_and(|rest| rest.starts_with('/'))
                })
                .map(String::len)
                .max()
        };
        let mut package: Vec<(usize, &str)> = Vec::new();
        let mut project: BTreeSet<&str> = BTreeSet::new();
        for target in &self.swift_targets {
            let Some(depth) = claim(target) else {
                continue;
            };
            if target.package {
                package.push((depth, target.name.as_str()));
            } else {
                project.insert(target.name.as_str());
            }
        }
        if let Some(deepest) = package.iter().map(|(depth, _)| *depth).max() {
            let names: BTreeSet<String> = package
                .iter()
                .filter(|(depth, _)| *depth == deepest)
                .map(|(_, name)| name.to_string())
                .collect();
            return names.into_iter().collect();
        }
        if !project.is_empty() {
            return project.into_iter().map(str::to_string).collect();
        }
        devmap_extract::languages::swift_module_of(file)
            .into_iter()
            .collect()
    }

    /// Whether a module `file` is compiled into declares a top-level `name`
    /// anywhere — in which case that declaration, and not an import's, is what
    /// the name means in `file`.
    pub(super) fn own_module_declares(&self, file: &str, name: &str) -> bool {
        self.swift_module_by_file.get(file).is_some_and(|modules| {
            modules.iter().any(|module| {
                self.swift_module_symbols
                    .contains_key(&(module.clone(), name.to_string()))
            })
        })
    }

    pub(super) fn same_swift_module_target(
        &self,
        file: &str,
        name: &str,
        accept: impl Fn(SymbolKind) -> bool,
    ) -> Option<(String, String, String)> {
        let mut answers: BTreeSet<(String, String, String)> = BTreeSet::new();
        for module in self.swift_module_by_file.get(file)? {
            let Some(hits) = self
                .swift_module_symbols
                .get(&(module.clone(), name.to_string()))
            else {
                continue;
            };
            let mut visible = hits.iter().filter(|(path, symbol, kind)| {
                path != file
                    && accept(*kind)
                    && self.candidate_is_visible_from(LangFamily::Swift, file, path, symbol)
            });
            let Some((target_file, target_symbol, _)) = visible.next() else {
                continue;
            };
            if visible.next().is_some() {
                return None;
            }
            answers.insert((target_file.clone(), target_symbol.clone(), module.clone()));
        }
        let mut answers = answers.into_iter();
        let first = answers.next()?;
        // A target per module that resolves the name to different files has no
        // single answer for a file compiled into both.
        answers
            .all(|other| other.0 == first.0 && other.1 == first.1)
            .then_some(first)
    }

    /// The Swift module a specifier names, if this corpus indexed one.
    pub(super) fn swift_module_named_by<'a>(&'a self, specifier: &'a str) -> Option<&'a str> {
        let first = specifier
            .split('.')
            .next()
            .filter(|part| !part.is_empty())?;
        if self.swift_module_files.contains_key(first) {
            return Some(first);
        }
        let last = specifier.rsplit('.').next()?;
        self.swift_module_files.contains_key(last).then_some(last)
    }

    /// The file in an imported Swift module that uniquely declares `name`.
    pub(super) fn swift_file_declaring(&self, specifier: &str, name: &str) -> Option<String> {
        let module = self.swift_module_named_by(specifier)?;
        let hits = self
            .swift_module_symbols
            .get(&(module.to_string(), name.to_string()))?;
        let mut visible = hits
            .iter()
            .filter(|(_, _, kind)| *kind != SymbolKind::Method);
        let (file, _, _) = visible.next()?;
        visible.next().is_none().then(|| file.clone())
    }

    pub(super) fn resolve_swift_import(&self, specifier: &str) -> Vec<String> {
        let Some(module) = self.swift_module_named_by(specifier) else {
            return Vec::new();
        };
        self.swift_module_files
            .get(module)
            .map(|files| files.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub(super) fn swift_import_edge_targets(&self, specifier: &str) -> Vec<String> {
        self.swift_module_named_by(specifier)
            .map(|module| vec![format!("module:{module}")])
            .unwrap_or_default()
    }

    /// Module names this Swift file imported that resolved to no indexed file.
    ///
    /// Both the local handle and the specifier are collected: a kinded
    /// `import struct Foundation.Date` records `Date → Foundation`, and
    /// classification needs `Foundation` as the module that was imported.
    pub(super) fn imported_swift_modules(&self, file: &str) -> Vec<String> {
        let Some(map) = self.external_imports.get(file) else {
            return Vec::new();
        };
        let mut modules = BTreeSet::new();
        for (local, specifier) in map {
            for name in [local.as_str(), specifier.as_str()] {
                if let Some(root) = name.split('.').next().filter(|part| !part.is_empty()) {
                    modules.insert(root.to_string());
                }
            }
        }
        modules.into_iter().collect()
    }

    pub(super) fn declared_receiver_slot<'a>(
        &'a self,
        file_path: &str,
        enclosing_symbol: &str,
        root: &str,
        binding: Option<&LocalBinding>,
        slot: &str,
    ) -> Option<&'a str> {
        let scope = match binding {
            Some(binding) => binding.scope.as_deref(),
            None => Some(enclosing_symbol),
        };
        let scoped = |slot: &str| {
            scope.and_then(|scope| {
                self.declared_types
                    .get(&format!("{file_path}:{scope}:{root}{slot}"))
            })
        };
        // A known capture belongs only to its recorded declaring scope. A
        // missing annotation there cannot inherit a sibling's imported type.
        let scope_knows =
            binding.is_some() || scoped("@mod").is_some() || scoped("@type").is_some();
        if scope_knows {
            scoped(slot).map(String::as_str)
        } else {
            self.declared_types
                .get(&format!("{file_path}:{root}{slot}"))
                .map(String::as_str)
        }
    }

    pub(super) fn lookup_in_package(&self, file: &str, name: &str) -> Option<(String, String)> {
        let hits = self.symbol_index.get(name)?;
        let eligible = |path: &str, identity: &str| self.declared_at_file_level(path, identity);
        let local: Vec<_> = hits
            .iter()
            .filter(|(path, _, _, identity)| path == file && eligible(path, identity))
            .collect();
        if local.len() == 1 {
            return Some((file.to_string(), local[0].3.to_string()));
        }
        if !local.is_empty() || !file.ends_with(".go") {
            return None;
        }
        let dir = Self::parent_dir(file);
        let package = self.go_package_by_file.get(file)?;
        let mut candidates: Vec<_> = hits
            .iter()
            .filter(|(path, _, _, identity)| {
                path.ends_with(".go")
                    && !path.ends_with("_test.go")
                    && Self::parent_dir(path) == dir
                    && self.go_package_by_file.get(path) == Some(package)
                    && eligible(path, identity)
            })
            .map(|(path, _, _, identity)| (path.clone(), identity.to_string()))
            .collect();
        candidates.sort();
        candidates.dedup();
        (candidates.len() == 1).then(|| candidates.pop().unwrap())
    }

    /// A call written against an interface (a Go interface, a Swift protocol):
    /// the interface method is a symbol, so its direct callers are every call
    /// written against it, and each in-repo implementer is a possible runtime
    /// target. The one place that shape is built, whichever language found it.
    ///
    /// With no implementer the direct edge is the whole answer, returned as an
    /// ordinary `ReceiverType` for the generic emission to write. Otherwise the
    /// direct edge is pushed here and the returned [`Resolution::Dispatch`] is
    /// emitted per candidate — or, past [`DISPATCH_FANOUT_CAP`], as one ledger
    /// row — by the generic emission.
    pub(super) fn dispatch_through_interface(
        &self,
        ext: &Extraction,
        call: &ExtractedCall,
        edges: &mut Vec<ResolvedEdge>,
        interface: &str,
        (iface_file, iface_symbol): (String, String),
        targets: Vec<(String, String)>,
    ) -> Arc<Resolution> {
        let direct = Arc::new(Resolution::ReceiverType {
            target_symbol: iface_symbol.clone(),
            target_file: iface_file.clone(),
            receiver_type: interface.to_string(),
        });
        if targets.is_empty() {
            return direct;
        }
        let caller = call
            .caller_symbol
            .clone()
            .unwrap_or_else(|| ext.file_path.clone());
        edges.push(ResolvedEdge::resolved(
            ext.file_path.clone(),
            iface_file.clone(),
            caller,
            self.qualified_for(&iface_file, &iface_symbol),
            EdgeKind::Calls,
            direct,
            None,
        ));
        Arc::new(Resolution::Dispatch {
            candidates: targets.into(),
            via_file: iface_file,
            via_symbol: iface_symbol,
        })
    }

    /// Swift: `Meter(v: 1).bump()` — a receiver that is itself a construction
    /// is a value of the constructed type. The text must be exactly one call of
    /// a capitalised name (generic arguments allowed) and the name must be an
    /// indexed class, struct or enum; a capitalised *function* returning
    /// something else, or a chain (`Meter(v: 1).scaled().bump()`), is not typed.
    pub(super) fn swift_constructed_receiver(&self, file: &str, receiver: &str) -> Option<String> {
        if !file.ends_with(".swift") || !receiver.ends_with(')') {
            return None;
        }
        let open = receiver.find('(')?;
        let head = receiver[..open].trim();
        let head = head.split('<').next().unwrap_or(head);
        if !head.chars().next().is_some_and(char::is_uppercase) || !Self::is_plain_ident(head) {
            return None;
        }
        // One call spanning the whole text: the parentheses balance only at the end.
        let mut depth = 0usize;
        for (index, ch) in receiver.char_indices().skip_while(|(i, _)| *i < open) {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 && index + 1 != receiver.len() {
                        return None;
                    }
                }
                _ => {}
            }
        }
        let declared = self
            .symbol_index
            .get(head)?
            .iter()
            .any(|(_, kind, family, _)| {
                *family == LangFamily::Swift
                    && matches!(
                        kind,
                        SymbolKind::Class | SymbolKind::Struct | SymbolKind::Enum
                    )
            });
        declared.then(|| head.to_string())
    }

    /// Swift: `base.padding().badge()` — a receiver that is one method call on a
    /// receiver this resolver can type is a value of that method's written
    /// return type.
    ///
    /// The method is the one `type_methods` holds under the receiver's type, or
    /// the one level of its supertypes that declares it once (a protocol
    /// extension's default included); an overload set, a generic, an optional,
    /// a collection or a tuple result types nothing, because the written text
    /// does not say which value reaches the next call. An opaque result
    /// (`some View`, `any View`) is the protocol it names — a call on it is a
    /// call through that protocol. The result must be one declared type: a name
    /// two files declare is a type this resolver cannot dispatch on.
    /// `(prefix, method)` of a receiver that is exactly one method call on a
    /// plain receiver: `base.padding()` is `("base", "padding")`. The `(` read
    /// is the one that closes with the final `)`; a string or character literal
    /// in the arguments can hold an unbalanced paren, so none is read. A call on
    /// an optional (`a?.m()`, `a!.m()`) is not one: the value is wrapped.
    pub(super) fn swift_call_chain_split(receiver: &str) -> Option<(&str, &str)> {
        if !receiver.ends_with(')') {
            return None;
        }
        let mut depth = 0usize;
        let mut open = None;
        for (index, ch) in receiver.char_indices().rev() {
            match ch {
                '"' | '\'' | '\\' => return None,
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
        let (prefix, method) = receiver[..open?].rsplit_once('.')?;
        (Self::is_plain_ident(method) && !prefix.is_empty() && !prefix.ends_with(['?', '!']))
            .then_some((prefix, method))
    }

    pub(super) fn swift_chained_receiver(
        &self,
        file: &str,
        scope: Option<&str>,
        receiver: &str,
        binding: Option<&LocalBinding>,
    ) -> Option<String> {
        if !file.ends_with(".swift") {
            return None;
        }
        let (prefix, method) = Self::swift_call_chain_split(receiver)?;
        let root = Self::path_root(prefix);
        let base = self.receiver_type_for(
            file,
            scope,
            prefix,
            binding.filter(|binding| binding.name == root),
        )?;
        let key = (LangFamily::Swift, base.clone(), method.to_string());
        let (callee_file, callee) = match self.type_methods.get(&key) {
            Some(hits) => {
                let visible: Vec<&(String, String)> = hits
                    .iter()
                    .filter(|hit| self.swift_member_hit_is_visible_from(file, hit, &base))
                    .collect();
                match visible.as_slice() {
                    [only] if hits.len() == 1 => (only.0.clone(), only.1.clone()),
                    _ => return None,
                }
            }
            None => {
                let (found_file, found, _) =
                    self.inherited_method(LangFamily::Swift, &base, method)?;
                (found_file, found)
            }
        };
        let written = self
            .return_types
            .get(&(callee_file.clone(), callee.clone()))?;
        let written = written
            .trim()
            .strip_prefix("some ")
            .or_else(|| written.trim().strip_prefix("any "))
            .unwrap_or_else(|| written.trim());
        let mut nominal = Self::admissible_nominal_type(written)?;
        if nominal == "Self" {
            // `-> Self` is the receiver's own concrete type, whichever
            // supertype's extension declared the method: the next call is then
            // resolved against that type, an override included.
            nominal = base.clone();
        } else {
            // A return type that is a generic parameter of the function
            // (`id<Item>(_:) -> Item`: a second unassigned mention, the clause)
            // or of its type (`Box<Element>`: a mention on the type's own
            // header) is not the same-named declared type.
            let mentions = self
                .swift_unassigned_type_mentions
                .get(&(callee_file.clone(), callee.clone(), nominal.clone()))
                .copied()
                .unwrap_or(0);
            let owner = self
                .declaring_type_of(&callee_file, &callee)
                .map(|owner| owner.rsplit('.').next().unwrap_or(owner).to_string());
            if mentions != 1
                || owner.is_some_and(|owner| {
                    self.swift_type_header_mentions
                        .contains(&(owner, nominal.clone()))
                })
            {
                return None;
            }
        }
        if self.swift_type_aliases.contains(nominal.as_str()) {
            return None;
        }
        let mut declarations: Vec<&(String, SymbolKind, LangFamily, Arc<str>)> = self
            .symbol_index
            .get(nominal.as_str())?
            .iter()
            .filter(|(path, kind, family, identity)| {
                *family == LangFamily::Swift
                    && matches!(
                        kind,
                        SymbolKind::Class
                            | SymbolKind::Struct
                            | SymbolKind::Enum
                            | SymbolKind::Interface
                    )
                    && self.candidate_is_visible_from(LangFamily::Swift, file, path, identity)
            })
            .collect();
        declarations.dedup_by(|a, b| a.0 == b.0 && a.3 == b.3);
        (declarations.len() == 1).then_some(nominal)
    }

    /// Swift: the method `type_name` inherits from a supertype it conforms to —
    /// a protocol-extension default or a base class's method — when exactly one
    /// supertype level declares it once. Abstains on every ambiguity, as
    /// [`Self::implicit_receiver_target`] does.
    pub(super) fn inherited_method(
        &self,
        family: LangFamily,
        type_name: &str,
        method: &str,
    ) -> Option<(String, String, String)> {
        let mut frontier: Vec<String> = self
            .supertypes
            .get(&(family, type_name.to_string()))
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        let mut visited: BTreeSet<String> = BTreeSet::new();
        visited.insert(type_name.to_string());
        for _ in 0..Self::HERITAGE_WALK_MAX_DEPTH {
            let mut found: BTreeSet<(String, String, String)> = BTreeSet::new();
            let mut next: Vec<String> = Vec::new();
            for name in frontier.drain(..) {
                if !visited.insert(name.clone()) {
                    continue;
                }
                if !self.type_name_is_identifiable(family, &name) {
                    return None;
                }
                if let Some(hits) =
                    self.type_methods
                        .get(&(family, name.clone(), method.to_string()))
                {
                    let [hit] = hits.as_slice() else {
                        return None;
                    };
                    found.insert((hit.0.clone(), hit.1.clone(), name.clone()));
                }
                next.extend(
                    self.supertypes
                        .get(&(family, name))
                        .into_iter()
                        .flatten()
                        .cloned(),
                );
            }
            match found.len() {
                0 => {}
                1 => return found.into_iter().next(),
                _ => return None,
            }
            if next.is_empty() {
                return None;
            }
            frontier = next;
        }
        None
    }

    /// Swift: the protocol method `method` of `protocol`, and the in-repo
    /// conformers' declarations of it. Rust: the same for a trait and its
    /// `impl Trait for Type` implementors.
    ///
    /// `None` unless `protocol` is one indexed protocol declaration that
    /// declares `method` exactly once. The conformers are the protocol's
    /// transitive subtypes (`subtypes` is built from the `: Greeter`
    /// conformance lists, extensions included); a conformer that does not write
    /// the method inherits a protocol-extension default, which is the direct
    /// target already. A protocol that refines another contributes its own
    /// conformers and is not itself a candidate.
    pub(super) fn protocol_dispatch(
        &self,
        family: LangFamily,
        protocol: &str,
        method: &str,
    ) -> Option<ProtocolDispatch> {
        let is_protocol = self.symbol_index.get(protocol).is_some_and(|hits| {
            let mut kinds = hits
                .iter()
                .filter(|(_, _, candidate, _)| family.admits(*candidate));
            kinds.all(|(_, kind, _, _)| matches!(kind, SymbolKind::Interface | SymbolKind::Trait))
        });
        if !is_protocol || !self.type_name_is_identifiable(family, protocol) {
            return None;
        }
        let mut declared = self
            .type_methods
            .get(&(family, protocol.to_string(), method.to_string()))?
            .clone();
        declared.sort();
        declared.dedup();
        let [direct] = declared.as_slice() else {
            return None;
        };
        let mut candidates: BTreeSet<(String, String)> = BTreeSet::new();
        let mut visited: BTreeSet<String> = BTreeSet::new();
        let mut frontier = vec![protocol.to_string()];
        for _ in 0..Self::HERITAGE_WALK_MAX_DEPTH {
            let mut next = Vec::new();
            for name in frontier.drain(..) {
                for sub in self.subtypes.get(&(family, name)).into_iter().flatten() {
                    if !visited.insert(sub.clone()) {
                        continue;
                    }
                    if let Some(hits) =
                        self.type_methods
                            .get(&(family, sub.clone(), method.to_string()))
                    {
                        candidates.extend(hits.iter().cloned());
                    }
                    next.push(sub.clone());
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        candidates.remove(direct);
        Some((direct.clone(), candidates.into_iter().collect()))
    }

    /// The one `type_methods` hit a typed receiver dispatches to.
    ///
    /// `type_methods` is keyed by the *bare* type name, so `(Go, "Client",
    /// "Zzembed")` holds both `rpc.Client.Zzembed` and `llm.Client.Zzembed`
    /// when two packages declare a `Client`. One hit is the answer, as it always
    /// was. Several are refused — except in the one case Go's scoping settles:
    /// a type spelled **unqualified, in the caller's own file**, means the type
    /// the caller's package block declares ([`Self::go_own_package_method`]).
    ///
    /// `class_type` cannot say which case it is: the binding facts record
    /// `&rpc.Client{}` as `Client{..}` and `c *rpc.Client` as `Client`, and a
    /// type reached through a field or a call is spelled relative to whichever
    /// package declared *that*. So the spelling is read again, from the
    /// references the extractor emitted for this variable
    /// ([`Self::go_receiver_spelled_bare`]).
    pub(super) fn receiver_type_method<'a>(
        &self,
        ext: &Extraction,
        scope: Option<&str>,
        receiver: &str,
        binding: Option<&LocalBinding>,
        class_type: &str,
        hits: &'a [(String, String)],
    ) -> Option<&'a (String, String)> {
        // Swift: a `private`/`fileprivate` type, or member, of another file is
        // not a candidate for a name written here. Two files can each declare
        // an `Rng` and only the one this file can see is the receiver's type;
        // a lone hit that this file cannot see is no answer at all.
        if ext.language == "swift" {
            let visible: Vec<&(String, String)> = hits
                .iter()
                .filter(|hit| {
                    self.swift_member_hit_is_visible_from(&ext.file_path, hit, class_type)
                })
                .collect();
            return visible
                .first()
                .copied()
                .filter(|first| visible.iter().all(|hit| hit == first));
        }
        // Overloads of one method are several hits that name one identity
        // (`add(Int)` / `add(String)` both mint `Calc.add`): one target.
        if let Some(first) = hits
            .first()
            .filter(|first| hits.iter().all(|hit| hit == *first))
        {
            return Some(first);
        }
        let scope = binding.and_then(|b| b.scope.as_deref()).or(scope);
        if ext.language != "go" || !Self::go_receiver_spelled_bare(ext, scope, receiver, class_type)
        {
            return None;
        }
        self.go_own_package_method(ext, class_type, hits)
    }

    /// Whether this file wrote `receiver`'s type as a bare `class_type`, and
    /// never as a qualified one.
    ///
    /// The Go extractor ties each typed name to the variable it types through
    /// `assigned_to`: a parameter or method receiver `c *Client` is a `Type`
    /// reference, `c := &Client{}` a `Constructor` reference, and a qualified
    /// parameter type `c *rpc.Client` adds a `TypeQualifier` reference naming
    /// `rpc`. A composite literal carries no qualifier reference, but its span
    /// is the written type, so `rpc.Client{}` is the `Client` whose span is
    /// longer than its name.
    ///
    /// Refuses any qualified spelling of the variable in the scope, and a type
    /// nothing here wrote for it: one learned from a call's return, or a
    /// field's (`h.Inner` names no variable, so no reference is assigned to
    /// it, and its type is spelled in the package that declared the field).
    pub(super) fn go_receiver_spelled_bare(
        ext: &Extraction,
        scope: Option<&str>,
        receiver: &str,
        class_type: &str,
    ) -> bool {
        let mut bare = false;
        for reference in ext.references.iter().filter(|reference| {
            reference.assigned_to.as_deref() == Some(receiver)
                && reference.enclosing_symbol.as_deref() == scope
        }) {
            match reference.kind {
                ReferenceKind::TypeQualifier => return false,
                ReferenceKind::Type if reference.name == class_type => bare = true,
                ReferenceKind::Constructor if reference.name == class_type => {
                    let written = reference.span.end_byte - reference.span.start_byte;
                    if written != reference.name.len() {
                        return false;
                    }
                    bare = true;
                }
                _ => {}
            }
        }
        bare
    }

    /// The method `class_type` declares in `file`'s own Go package, when the
    /// package block is where a bare `class_type` written in `file` points.
    ///
    /// The type's declaration is [`Self::lookup_in_package`]'s answer — this
    /// file, then the one non-test file of its `(directory, package clause)`
    /// — and the method must sit in that same package, under the build-tag
    /// rule [`Self::same_package_target`] states: a non-test file does not see
    /// a `_test.go` declaration. A file with a dot-import refuses outright: it
    /// puts another package's names in this file's scope, so even a bare
    /// `Client` can mean someone else's.
    pub(super) fn go_own_package_method<'a>(
        &self,
        ext: &Extraction,
        class_type: &str,
        hits: &'a [(String, String)],
    ) -> Option<&'a (String, String)> {
        if ext
            .imports
            .iter()
            .any(|imp| imp.alias.as_deref() == Some("."))
        {
            return None;
        }
        let file = ext.file_path.as_str();
        let (declaring_file, _) = self.lookup_in_package(file, class_type)?;
        let dir = Self::parent_dir(&declaring_file);
        let package = self.go_package_by_file.get(&declaring_file)?;
        let source_is_test = file.ends_with("_test.go");
        let mut own: Vec<&(String, String)> = hits
            .iter()
            .filter(|(path, _)| {
                path.ends_with(".go")
                    && Self::parent_dir(path) == dir
                    && self.go_package_by_file.get(path) == Some(package)
                    && (source_is_test || !path.ends_with("_test.go"))
            })
            .collect();
        own.sort();
        own.dedup();
        match own.as_slice() {
            [only] => Some(*only),
            _ => None,
        }
    }
}
