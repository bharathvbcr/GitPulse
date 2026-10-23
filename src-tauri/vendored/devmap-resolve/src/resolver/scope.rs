use super::*;

impl Resolver {
    pub(super) fn import_local_name(lang: &str, specifier: &str) -> String {
        if lang == "go" {
            specifier
                .rsplit('/')
                .next()
                .unwrap_or(specifier)
                .to_string()
        } else if lang == "rust" {
            // Rust's path separator is `::`, and splitting on `.` returns the
            // whole specifier — so `use serde_json::*;` used to record its
            // module handle as the literal `"serde_json::*"`, a name no call
            // site can mention. Nothing depended on that before X41 because no
            // Rust `use` reached this function with a real path at all.
            specifier
                .rsplit("::")
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

    /// Whether a bare `name()` written in `caller` can reach the same-file
    /// symbol of that name.
    ///
    /// Two shapes can:
    ///
    /// - the symbol is declared at **file level**, which every language allows
    ///   a bare name to reach; or
    /// - the symbol and the caller are declared by the **same type**, and the
    ///   language supplies the receiver implicitly — `g()` inside
    ///   `class A { void f() { g(); } void g() {} }` is a real call in C++,
    ///   Java and C#.
    ///
    /// The second is refused for the four families where it is demonstrably not
    /// a call: Python and Go need the receiver written (`self.g()`, `r.g()`),
    /// JavaScript needs `this.g()`, and Rust needs `self.g()` or `Type::g()`.
    /// Those are exactly the families this crate resolves with confidence, and
    /// the audited defect — a top-level `invoke()` binding to `C.run` — is
    /// refused for every family by the file-level test above.
    ///
    /// Permissive elsewhere on purpose: allowing the sibling shape is today's
    /// behaviour, so a family whose scoping rules are not stated here keeps
    /// resolving exactly as it did rather than silently losing edges.
    pub(super) fn bare_name_is_in_scope(
        &self,
        file_path: &str,
        family: LangFamily,
        caller: Option<&str>,
        callee_name: &str,
    ) -> bool {
        let parent_of = |qualified: &str| {
            self.symbol_parents
                .get(&(file_path.to_string(), qualified.to_string()))
        };
        let callee_qualified = self.qualified_for(file_path, callee_name);
        let Some(callee_parent) = parent_of(&callee_qualified) else {
            // Not indexed as a symbol of this file, so nothing claims it is in
            // scope. Abstain rather than assume.
            return false;
        };
        if callee_parent == file_path {
            return true;
        }
        if Self::family_needs_explicit_receiver(family) {
            return false;
        }
        caller
            .and_then(parent_of)
            .is_some_and(|caller_parent| caller_parent == callee_parent)
    }

    /// Families in which a method is unreachable without a written receiver.
    ///
    /// Python and Go require it in the signature and at the call site
    /// (`self.g()`, `r.g()`), JavaScript and TypeScript require `this.g()`, and
    /// Rust requires `self.g()` or `Type::g()`. C++, Java, C#, Kotlin, Swift,
    /// Ruby and the rest are *not* listed: they supply the receiver implicitly,
    /// so a bare sibling call is a real call there and must keep resolving.
    pub(super) fn family_needs_explicit_receiver(family: LangFamily) -> bool {
        matches!(
            family,
            LangFamily::Python | LangFamily::JsTs | LangFamily::Go | LangFamily::Rust
        )
    }

    /// Whether `file` declares `bare_name` at file level rather than inside a
    /// type or another callable.
    pub(super) fn declared_at_file_level(&self, file: &str, bare_name: &str) -> bool {
        let qualified = self.qualified_for(file, bare_name);
        self.symbol_parents
            .get(&(file.to_string(), qualified))
            .is_some_and(|parent| parent == file)
    }

    /// Whether a receiver expression denotes the enclosing scope itself.
    ///
    /// These are the spellings across the indexed languages: `self` (Python,
    /// Rust, Swift), `this` (JS/TS, Java, C#, PHP's `$this`), `cls` (Python
    /// classmethods), `me` (VB). A receiver in this set names the object the
    /// current code is already inside, so a sibling declaration in the same
    /// file is a real candidate; any other receiver names something else, and
    /// matching it against this file's symbols by bare name is a guess.
    pub(super) fn receiver_is_self(receiver: &str) -> bool {
        matches!(receiver, "self" | "this" | "cls" | "$this" | "me" | "Self")
    }

    /// Whether a Rust `self` receiver written inside `caller` is the method's
    /// `self` value rather than the `self::` module path.
    ///
    /// The extractor reduces `self.f()` and `self::f()` to the same receiver
    /// text, so the caller decides: only a function owned by a type can have a
    /// `self` parameter, and there `self.f()` is a method call on a value, which
    /// no free function can answer (`self.spawn()` inside
    /// `impl SpawnExt for Command` is std's `Command::spawn`, not this file's
    /// `spawn`). A free-function caller can only have written the path.
    pub(super) fn rust_self_is_a_value(
        &self,
        file: &str,
        caller: Option<&str>,
        receiver: Option<&str>,
    ) -> bool {
        receiver == Some("self")
            && caller
                .and_then(|caller| {
                    self.symbol_parents
                        .get(&(file.to_string(), caller.to_string()))
                })
                .is_some_and(|parent| parent != file)
    }

    /// How far the supertype walk may climb.
    ///
    /// A bound rather than a cycle check alone: `class A(B)` / `class B(A)` is
    /// not the only pathology, and a generated hierarchy thousands deep would
    /// cost a lookup per level per call site. Eight covers every hierarchy this
    /// resolver has been pointed at; past it the rung abstains, which loses an
    /// edge and invents nothing.
    pub(super) const HERITAGE_WALK_MAX_DEPTH: usize = 8;

    /// The type `symbol` is declared by, or `None` when it is declared at file
    /// level.
    ///
    /// Read from `symbol_parents` — the extractor's own answer — and reduced to
    /// the bare name the same way `type_methods` reduces `parent_symbol` when
    /// it is built, so the two cannot key differently. Deliberately **not** a
    /// split of the method's qualified name on `.`: `Outer.Inner.method` and a
    /// module-level `a.b` are the same string to that rule and different facts.
    pub(super) fn declaring_type_of(&self, file: &str, symbol: &str) -> Option<&str> {
        self.symbol_parents
            .get(&(file.to_string(), symbol.to_string()))
            .filter(|parent| *parent != file)
            .and_then(|parent| parent.rsplit("::").next())
            .filter(|type_name| !type_name.is_empty())
    }

    /// What `self` / `Self` denotes at a call attributed to `caller_symbol`.
    ///
    /// Usually the caller is a *member* of a type and the answer is its parent,
    /// which is all `declaring_type_of` gives. But a call can be attributed to
    /// the type itself — a Swift `private static func` body, a property
    /// initializer, a Python class-body statement — and then the parent is the
    /// file, `declaring_type_of` abstains, and X42 never runs even though the
    /// enclosing type is sitting in the caller's own name.
    ///
    /// Measured: 9 Swift targets on a 306-file corpus lost their last
    /// non-structural inbound edge to exactly this, every one a
    /// `Self.staticMethod()` whose caller symbol was the `struct`. The rung
    /// could not see the type because it only ever looked one level *up*.
    ///
    /// The caller must be a type **declared in this file**: the answer is the
    /// key `type_methods` is built with, so an unqualified match elsewhere
    /// would dispatch `self.m()` onto a namesake in another file. Ambiguity
    /// inside the file is left to the caller's own `local.len() > 1` guard,
    /// which already abstains when one name declares the method twice.
    ///
    /// `Module` is in the list because Ruby's `module M; def self.m` is this
    /// exact shape and `self` there *is* `M`. The kind is shared with Go
    /// packages, TypeScript namespaces, C# namespaces and Terraform blocks, so
    /// it was admitted only after checking each: none of them ever reaches here,
    /// because a namespace-level call arrives with no caller symbol at all and
    /// the others have no `self` keyword to spell.
    pub(super) fn self_type_at(
        &self,
        file: &str,
        family: LangFamily,
        caller_symbol: &str,
    ) -> Option<String> {
        if let Some(parent) = self.declaring_type_of(file, caller_symbol) {
            return Some(parent.to_string());
        }
        let declares_a_type_here = self
            .symbol_index
            .get(caller_symbol)
            .into_iter()
            .flatten()
            .any(|(path, kind, candidate_family, _)| {
                path == file
                    && family.admits(*candidate_family)
                    && matches!(
                        kind,
                        SymbolKind::Class
                            | SymbolKind::Struct
                            | SymbolKind::Enum
                            | SymbolKind::Interface
                            | SymbolKind::Trait
                            | SymbolKind::Module
                    )
            });
        if !declares_a_type_here {
            return None;
        }
        // Reduced exactly as `declaring_type_of` reduces a parent, so a caller
        // that *is* a type and a caller that is *inside* one key identically.
        caller_symbol
            .rsplit("::")
            .next()
            .filter(|type_name| !type_name.is_empty())
            .map(str::to_string)
    }

    /// Swift: whether `target`, a nested type, cannot be named by its bare
    /// spelling from where `caller` sits.
    ///
    /// A type nested in `A` is a member of `A`: it is found unqualified inside
    /// `A`, inside a type nested in `A`, in an `extension A` (in any file) and
    /// in a subtype of `A` — and nowhere else. A bare `Inner` written in an
    /// unrelated type or a free function names some other `Inner`, so the one
    /// namesake the global rung finds is a fabricated callee. `false` for a
    /// top-level target, for a target that is not a type, and for a name a
    /// `typealias` somewhere may rebind (the alias, not the spelling, then
    /// decides, and this has no way to read it).
    ///
    /// The nesting is read from the declarations' qualified names, because the
    /// extractor parents a nested type's own members to the bare inner name
    /// (`a_nested_type_is_a_known_extractor_gap_not_a_rung_gap`), which alone
    /// loses the outer type.
    pub(super) fn swift_nested_type_is_out_of_scope(
        &self,
        file: &str,
        caller: Option<&str>,
        target_file: &str,
        target: &str,
    ) -> bool {
        let is_type = |kind: SymbolKind| {
            matches!(
                kind,
                SymbolKind::Class | SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Interface
            )
        };
        let target_is_a_type = self
            .symbol_index
            .get(target)
            .into_iter()
            .flatten()
            .any(|(path, kind, _, identity)| {
                path == target_file && **identity == *target && is_type(*kind)
            });
        let Some(outer) = self
            .declaring_type_of(target_file, target)
            .filter(|_| target_is_a_type)
        else {
            return false;
        };
        let name = target.rsplit(['.', ':']).next().unwrap_or(target);
        if self.swift_type_aliases.contains(name) {
            return false;
        }
        let Some(caller_type) =
            caller.and_then(|caller| self.self_type_at(file, LangFamily::Swift, caller))
        else {
            return true;
        };
        let inside = |written: &str| {
            written == outer
                || written
                    .strip_prefix(outer)
                    .is_some_and(|rest| rest.starts_with('.'))
        };
        if inside(&caller_type) {
            return false;
        }
        // The caller's own type, declared in its file, with the nesting its
        // qualified name carries (`A.Inner` for the type whose members are
        // parented to `Inner`).
        // A nested type's qualified name keeps only its immediate parent
        // (`M.Deep` inside `A.M`), so the outer type is found by climbing:
        // each type's parent, resolved by its bare name within the file, up to
        // the target's outer type.
        let file_prefix = format!("{file}::");
        let type_named = |bare: &str| -> Vec<String> {
            self.symbol_index
                .get(bare)
                .into_iter()
                .flatten()
                .filter(|(path, kind, _, _)| path == file && is_type(*kind))
                .map(|(_, _, _, identity)| identity.to_string())
                .collect()
        };
        let mut climbing = type_named(caller_type.rsplit('.').next().unwrap_or(&caller_type));
        let mut climbed: BTreeSet<String> = BTreeSet::new();
        for _ in 0..Self::HERITAGE_WALK_MAX_DEPTH * 4 {
            let mut parents: Vec<String> = Vec::new();
            for identity in climbing.drain(..) {
                if !climbed.insert(identity.clone()) {
                    continue;
                }
                if identity
                    .strip_prefix(&file_prefix)
                    .is_some_and(|qualified| inside(qualified))
                {
                    return false;
                }
                if let Some(parent) = self
                    .symbol_parents
                    .get(&(file.to_string(), identity))
                    .and_then(|parent| parent.strip_prefix(&file_prefix))
                {
                    if inside(parent) {
                        return false;
                    }
                    parents.extend(type_named(parent.rsplit('.').next().unwrap_or(parent)));
                }
            }
            if parents.is_empty() {
                break;
            }
            climbing = parents;
        }
        // A subtype sees the member types of its supertypes.
        let outer_name = outer.rsplit('.').next().unwrap_or(outer);
        let mut frontier = vec![caller_type];
        let mut visited: BTreeSet<String> = BTreeSet::new();
        for _ in 0..Self::HERITAGE_WALK_MAX_DEPTH {
            let mut next = Vec::new();
            for name in frontier.drain(..) {
                if !visited.insert(name.clone()) {
                    continue;
                }
                for base in self
                    .supertypes
                    .get(&(LangFamily::Swift, name))
                    .into_iter()
                    .flatten()
                {
                    if base == outer_name {
                        return false;
                    }
                    next.push(base.clone());
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        true
    }

    /// Whether exactly one indexed file declares a type of this name.
    ///
    /// The identifiability test for the supertype walk. `type_methods` and
    /// `supertypes` are both flat by bare type name, so a chain that passes
    /// through a name two files declare is a chain this resolver cannot follow
    /// — and following it anyway would dispatch on whichever declaration the
    /// merge happened to produce.
    /// Whether the index declares a type of this name in `family`.
    pub(super) fn declares_type(&self, family: LangFamily, type_name: &str) -> bool {
        self.symbol_index.get(type_name).into_iter().flatten().any(
            |(_, kind, candidate_family, _)| {
                family.admits(*candidate_family)
                    && matches!(
                        kind,
                        SymbolKind::Class
                            | SymbolKind::Struct
                            | SymbolKind::Enum
                            | SymbolKind::Interface
                    )
            },
        )
    }

    pub(super) fn type_name_is_identifiable(&self, family: LangFamily, type_name: &str) -> bool {
        let mut files: BTreeSet<&str> = BTreeSet::new();
        for (path, kind, candidate_family, _) in
            self.symbol_index.get(type_name).into_iter().flatten()
        {
            if family.admits(*candidate_family)
                && matches!(
                    kind,
                    SymbolKind::Class
                        | SymbolKind::Struct
                        | SymbolKind::Enum
                        | SymbolKind::Interface
                        | SymbolKind::Trait
                )
            {
                files.insert(path.as_str());
            }
        }
        files.len() <= 1
    }

    /// The indexed type declares no member of this name on itself, on any
    /// supertype, or — for Rust — on a type it derefs to.
    ///
    /// False when the name is not one indexed declaration (an external
    /// annotation, or two files declaring it): absence is only meaningful for
    /// a type this index can see uniquely. A method that exists but is
    /// ambiguous is present — `contains_key`, not a unique hit — so an
    /// abstention is not relabelled as a missing member.
    pub(super) fn indexed_type_lacks_member(
        &self,
        family: LangFamily,
        type_name: &str,
        method: &str,
    ) -> bool {
        let mut files: BTreeSet<&str> = BTreeSet::new();
        let mut only_interfaces = true;
        for (path, kind, candidate_family, _) in
            self.symbol_index.get(type_name).into_iter().flatten()
        {
            if family.admits(*candidate_family)
                && matches!(
                    kind,
                    SymbolKind::Class
                        | SymbolKind::Struct
                        | SymbolKind::Enum
                        | SymbolKind::Interface
                        | SymbolKind::Trait
                )
            {
                files.insert(path.as_str());
                only_interfaces &= *kind == SymbolKind::Interface;
            }
        }
        // A TypeScript `type X = number` is indexed as an `Interface` and
        // declares no member symbols, as does `interface P { x: number }`.
        // Neither says what the value's members are, so only an interface
        // with at least one method of its own can show a member absent.
        if family == LangFamily::JsTs
            && only_interfaces
            && !self.type_declares_any_method(family, type_name)
        {
            return false;
        }
        files.len() == 1 && !self.member_reachable(family, type_name, method)
    }

    pub(super) fn type_declares_any_method(&self, family: LangFamily, type_name: &str) -> bool {
        self.type_methods
            .range((family, type_name.to_string(), String::new())..)
            .next()
            .is_some_and(|((f, t, _), _)| *f == family && t == type_name)
    }

    pub(super) fn type_has_method(
        &self,
        family: LangFamily,
        type_name: &str,
        method: &str,
    ) -> bool {
        self.type_methods
            .contains_key(&(family, type_name.to_string(), method.to_string()))
    }

    /// `method` on `type_name`, or on a Rust `Deref` target within
    /// [`Self::RUST_DEREF_HOPS`].
    pub(super) fn type_or_deref_has_method(
        &self,
        family: LangFamily,
        type_name: &str,
        method: &str,
    ) -> bool {
        if self.type_has_method(family, type_name, method) {
            return true;
        }
        if family != LangFamily::Rust {
            return false;
        }
        let mut current = type_name.to_string();
        for _ in 0..Self::RUST_DEREF_HOPS {
            let Some(next) = self.rust_deref_targets.get(&current).and_then(Clone::clone) else {
                return false;
            };
            if self.type_has_method(family, &next, method) {
                return true;
            }
            current = next;
        }
        false
    }

    /// Whether `method` is declared on `type_name` or any supertype within
    /// [`Self::HERITAGE_WALK_MAX_DEPTH`], counting an ambiguous declaration as
    /// present.
    pub(super) fn member_reachable(
        &self,
        family: LangFamily,
        type_name: &str,
        method: &str,
    ) -> bool {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut frontier = vec![type_name.to_string()];
        for _ in 0..Self::HERITAGE_WALK_MAX_DEPTH {
            let mut next = Vec::new();
            for name in frontier.drain(..) {
                if !seen.insert(name.clone()) {
                    continue;
                }
                if self.type_or_deref_has_method(family, &name, method) {
                    return true;
                }
                for supertype in self.supertypes.get(&(family, name)).into_iter().flatten() {
                    next.push(supertype.clone());
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        false
    }
}
