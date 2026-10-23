use super::*;

impl Resolver {
    /// X42. Where `self.m()` / `cls.m()` / `this.m()` / a Go receiver's `s.M()`
    /// goes, given the type the call is written inside.
    ///
    /// The receiver of such a call *is* the enclosing type — that is what the
    /// keyword means — so this is `ReceiverType` evidence and not a new rung.
    /// The type's own methods answer first; failing that, its declared
    /// supertypes do, breadth-first, because an inherited method is still a
    /// method of the receiver's type.
    ///
    /// Abstains, rather than choosing, on every ambiguity: a type that declares
    /// the name twice, a level of the hierarchy where two supertypes declare
    /// it, and a type name two files declare.
    ///
    /// Also abstains ([`ImplicitReceiverAnswer::VirtualOverride`]) when an
    /// indexed subtype of the enclosing type overrides the same method: a
    /// unique declaration on the enclosing type is still not a unique *runtime*
    /// target for `self`, and labelling it `ReceiverType` would be a
    /// confidence-1.0 edge the subtype can steal.
    pub(super) fn implicit_receiver_target(
        &self,
        file: &str,
        family: LangFamily,
        caller_symbol: &str,
        method: &str,
    ) -> Option<ImplicitReceiverAnswer> {
        let enclosing = self.self_type_at(file, family, caller_symbol)?;
        // The caller's declaring type is an exact identity even when another
        // file declares a namesake. Only inherited lookup needs a global name.
        if let Some(hits) = self
            .type_methods
            .get(&(family, enclosing.clone(), method.to_string()))
        {
            let local: Vec<_> = hits.iter().filter(|(path, _)| path == file).collect();
            if local.len() == 1 {
                return Some(self.implicit_receiver_unique(
                    family,
                    &enclosing,
                    method,
                    local[0].0.clone(),
                    local[0].1.clone(),
                    enclosing.clone(),
                ));
            }
            if local.len() > 1 {
                return None;
            }
        }
        let flat = self.supertype_method(family, vec![enclosing.clone()], method);
        // The breadth-first walk has no unknown or ambiguous bases and no
        // order. For Python the class's own MRO walk is the authority: an
        // opaque or ambiguous base ahead of the declaring class, or a different
        // class first in the MRO, means the flat answer would be a guess. A
        // class the file-scoped walk cannot speak for (a base declared inside a
        // function, say) is read from the flat table only when no class in its
        // heritage has several bases, because only then are the two orders the
        // same.
        if family == LangFamily::Python {
            match self.python_mro_method(file, &enclosing, method, true) {
                pytypes::MroAnswer::Found(found) => {
                    if flat
                        .as_ref()
                        .is_some_and(|(f, s, _)| *f != found.file || *s != found.symbol)
                    {
                        return None;
                    }
                    return Some(self.implicit_receiver_unique(
                        family,
                        &enclosing,
                        method,
                        found.file,
                        found.symbol,
                        found.declaring_type,
                    ));
                }
                pytypes::MroAnswer::Capped(reason) => {
                    return Some(ImplicitReceiverAnswer::HeritageCap(reason));
                }
                pytypes::MroAnswer::NotScoped
                    if self.python_flat_heritage_is_linear(file, &enclosing) => {}
                _ => return None,
            }
        }
        let (target_file, target_symbol, receiver_type) = flat?;
        Some(self.implicit_receiver_unique(
            family,
            &enclosing,
            method,
            target_file,
            target_symbol,
            receiver_type,
        ))
    }

    /// The one method `method` that `frontier`'s types, or the nearest of their
    /// declared supertypes, declare — breadth-first, so an inherited method is
    /// found at the level it is first declared.
    ///
    /// Abstains on every ambiguity: a type that declares the name twice, a level
    /// where two supertypes declare it, and a type name two files declare. The
    /// language's own MRO might pick one; this resolver has no MRO, and a guess
    /// at DETERMINISTIC is the one answer it must not give. Returns `(file,
    /// symbol, declaring type)`.
    pub(super) fn supertype_method(
        &self,
        family: LangFamily,
        mut frontier: Vec<String>,
        method: &str,
    ) -> Option<(String, String, String)> {
        let mut visited: BTreeSet<String> = BTreeSet::new();
        for _ in 0..Self::HERITAGE_WALK_MAX_DEPTH {
            let mut found: BTreeSet<(String, String, String)> = BTreeSet::new();
            let mut next: Vec<String> = Vec::new();
            for type_name in &frontier {
                if !visited.insert(type_name.clone()) {
                    continue;
                }
                if !self.type_name_is_identifiable(family, type_name) {
                    return None;
                }
                if let Some(hits) =
                    self.type_methods
                        .get(&(family, type_name.clone(), method.to_string()))
                {
                    // One type declaring the same method twice is an ambiguity
                    // inside that type, and nothing here can choose.
                    if hits.len() != 1 {
                        return None;
                    }
                    found.insert((hits[0].0.clone(), hits[0].1.clone(), type_name.clone()));
                }
                if let Some(bases) = self.supertypes.get(&(family, type_name.clone())) {
                    next.extend(bases.iter().cloned());
                }
            }
            match found.len() {
                1 => return found.into_iter().next(),
                0 => {}
                _ => return None,
            }
            if next.is_empty() {
                return None;
            }
            frontier = next;
        }
        None
    }

    /// A unique hit on the receiver's type, or [`ImplicitReceiverAnswer::VirtualOverride`]
    /// when an indexed subtype of `enclosing` also declares `method`.
    pub(super) fn implicit_receiver_unique(
        &self,
        family: LangFamily,
        enclosing: &str,
        method: &str,
        target_file: String,
        target_symbol: String,
        receiver_type: String,
    ) -> ImplicitReceiverAnswer {
        if self.method_overridden_by_indexed_subtype(family, enclosing, method) {
            ImplicitReceiverAnswer::VirtualOverride
        } else {
            ImplicitReceiverAnswer::Unique {
                target_file,
                target_symbol,
                receiver_type,
            }
        }
    }

    /// True when some indexed subtype of `enclosing` (transitively) declares
    /// `method` — evidence that `self.method()` inside `enclosing` is virtual.
    /// The `(file, symbol)` of every indexed subtype's declaration of `method`:
    /// the runtime targets of a call written against `receiver_type`. A test
    /// source set's subtypes are targets of test code only.
    ///
    /// Breadth-first over the recorded subtype edges, bounded by the same
    /// heritage depth as every other walk here. The receiver type's own
    /// declaration is not a candidate (it is the direct edge).
    pub(super) fn overriding_methods(
        &self,
        family: LangFamily,
        receiver_type: &str,
        method: &str,
        caller_file: &str,
    ) -> Vec<(String, String)> {
        let mut frontier: Vec<String> = self
            .subtypes
            .get(&(family, receiver_type.to_string()))
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        let mut visited: BTreeSet<String> = BTreeSet::new();
        let mut found: BTreeSet<(String, String)> = BTreeSet::new();
        for _ in 0..Self::HERITAGE_WALK_MAX_DEPTH {
            if frontier.is_empty() {
                break;
            }
            let mut next: Vec<String> = Vec::new();
            for type_name in frontier.drain(..) {
                if !visited.insert(type_name.clone()) {
                    continue;
                }
                if let Some(hits) =
                    self.type_methods
                        .get(&(family, type_name.clone(), method.to_string()))
                {
                    found.extend(hits.iter().cloned());
                }
                if let Some(subs) = self.subtypes.get(&(family, type_name)) {
                    next.extend(subs.iter().cloned());
                }
            }
            frontier = next;
        }
        // A fake or stub declared in a test source set is a runtime target of a
        // test's call and never of production code's: shipping code cannot
        // reach a class that only exists in the test build.
        let from_test = devmap_extract::wiring::is_test_path(caller_file);
        found
            .into_iter()
            .filter(|(file, _)| from_test || !devmap_extract::wiring::is_test_path(file))
            .collect()
    }

    /// The declaration `super.method()` reaches from inside `caller_symbol`'s
    /// type: the nearest supertype level that declares it, exactly once.
    ///
    /// Abstains on a level where two supertypes declare it (an MRO this
    /// resolver does not model) and on any supertype it cannot identify.
    pub(super) fn super_method_target(
        &self,
        file: &str,
        family: LangFamily,
        caller_symbol: &str,
        method: &str,
    ) -> Option<(String, String, String)> {
        let enclosing = self.self_type_at(file, family, caller_symbol)?;
        let mut frontier: Vec<String> = self
            .supertypes
            .get(&(family, enclosing))
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        let mut visited: BTreeSet<String> = BTreeSet::new();
        for _ in 0..Self::HERITAGE_WALK_MAX_DEPTH {
            if frontier.is_empty() {
                return None;
            }
            let mut found: BTreeSet<(String, String, String)> = BTreeSet::new();
            let mut next: Vec<String> = Vec::new();
            for type_name in frontier.drain(..) {
                if !visited.insert(type_name.clone()) {
                    continue;
                }
                if let Some(hits) =
                    self.type_methods
                        .get(&(family, type_name.clone(), method.to_string()))
                {
                    if hits.len() != 1 {
                        return None;
                    }
                    found.insert((hits[0].0.clone(), hits[0].1.clone(), type_name.clone()));
                }
                if let Some(bases) = self.supertypes.get(&(family, type_name)) {
                    next.extend(bases.iter().cloned());
                }
            }
            match found.len() {
                0 => frontier = next,
                1 => return found.into_iter().next(),
                _ => return None,
            }
        }
        None
    }

    pub(super) fn method_overridden_by_indexed_subtype(
        &self,
        family: LangFamily,
        enclosing: &str,
        method: &str,
    ) -> bool {
        let mut frontier: Vec<String> = self
            .subtypes
            .get(&(family, enclosing.to_string()))
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        let mut visited: BTreeSet<String> = BTreeSet::new();
        for _ in 0..Self::HERITAGE_WALK_MAX_DEPTH {
            if frontier.is_empty() {
                return false;
            }
            let mut next: Vec<String> = Vec::new();
            for type_name in frontier.drain(..) {
                if !visited.insert(type_name.clone()) {
                    continue;
                }
                if self
                    .type_methods
                    .get(&(family, type_name.clone(), method.to_string()))
                    .is_some_and(|hits| !hits.is_empty())
                {
                    return true;
                }
                if let Some(subs) = self.subtypes.get(&(family, type_name)) {
                    next.extend(subs.iter().cloned());
                }
            }
            frontier = next;
        }
        false
    }

    /// X45. Where a bare `name` written in `file` goes by Go's package-block
    /// scope rule, or `None` where the rule cannot answer.
    ///
    /// Go's spec puts every package-level identifier in scope, unqualified,
    /// throughout the package — which spans the files of one directory that
    /// share a package clause. That is deterministic evidence and the resolver
    /// had no rung for it: the answer came from the global tier, which counts
    /// matches across the whole language family and so said `UniqueGlobal`
    /// where the family held one and fanned out at `AmbiguousGlobal` where it
    /// held several. Measured on scholarlm: 31,587 and 24,635 same-directory
    /// cross-file Go edges respectively, plus 1,770 defect-tier rows for type
    /// references the package itself declares.
    ///
    /// The declaring file must not be `file`: the same-file rungs own that, and
    /// they apply scope tests this one has no way to repeat.
    ///
    /// `accept` is the caller's kind filter — a type annotation admits only
    /// type declarations — applied *before* the uniqueness count, so a rejected
    /// candidate can neither win nor veto.
    ///
    /// **Test files are a build tag, not a name.** A `_test.go` declaration is
    /// absent from the ordinary build, so a non-test file must not reach one;
    /// a `_test.go` file reaches both, because that is what its build sees.
    /// The external test package (`package search_test`) needs no rule here —
    /// its package clause differs, so it keys elsewhere.
    ///
    /// Abstains on a package that declares the name twice. That does not
    /// compile, but this resolver indexes whatever it is pointed at — a
    /// generated file beside its source, a half-applied merge — and a
    /// DETERMINISTIC rung that picked one would be picking by input order.
    pub(super) fn same_package_target(
        &self,
        file: &str,
        name: &str,
        accept: impl Fn(SymbolKind) -> bool,
    ) -> Option<(String, String, String)> {
        let package = self.go_package_by_file.get(file)?;
        let source_is_test = file.ends_with("_test.go");
        let hits = self.go_package_symbols.get(&(
            Self::parent_dir(file),
            package.clone(),
            name.to_string(),
        ))?;
        let mut visible = hits.iter().filter(|(path, _, kind)| {
            path != file && accept(*kind) && (source_is_test || !path.ends_with("_test.go"))
        });
        let (target_file, target_symbol, _) = visible.next()?;
        visible
            .next()
            .is_none()
            .then(|| (target_file.clone(), target_symbol.clone(), package.clone()))
    }

    /// Where `x.name()` goes when `name` is an **unexported** Go method and the
    /// type of `x` could not be inferred.
    ///
    /// Go's own visibility rule makes this a proof rather than a guess. An
    /// unexported selector is resolvable only inside the package that declares
    /// it — the compiler refuses `x.m` for another package's unexported `m`
    /// even when `x`'s type embeds that package's type — so whatever `x` is,
    /// `.name` names something *this* package declares: a concrete method, a
    /// struct field, or an interface method. With exactly one concrete method
    /// of that name visible and no field or interface method sharing it, that
    /// method is the callee.
    ///
    /// Measured on scholarlm: 13,143 of 104,696 Go `uninferred_receiver` rows
    /// have an unexported callee, and `job.recordReplayEvent(cancelled)` in
    /// `WisDevJobCancelHandler` — `job` bound by `yoloJobStore.get(id)`, a
    /// return type the receiver rungs cannot read — was one of them.
    ///
    /// Abstains, never guesses:
    /// * the package is keyed on `(directory, package clause)`, so an external
    ///   `package foo_test` sees nothing of `package foo`;
    /// * a non-test file does not see a method a `_test.go` file declares;
    /// * two concrete methods of the name (two types, or two build-constrained
    ///   variants of one) — the receiver's type decides and is unknown;
    /// * any field or interface method of the name, from any struct or
    ///   interface literal in the package;
    /// * any `.go` file in the directory whose extraction cannot vouch for the
    ///   whole declaration set (a failed or pattern-recovered parse, or cached
    ///   before `go_member_names` existed) — the missing namesake could be
    ///   there. A `Partial` parse is not one of them: its extraction vetoes
    ///   every unexported token in the file, so it blocks exactly the names it
    ///   could be hiding;
    /// * cgo's `C.name()`, which names a C function, not a Go method.
    pub(super) fn unexported_selector_target(
        &self,
        file: &str,
        receiver: &str,
        name: &str,
    ) -> Option<(String, String, String)> {
        if !Self::go_name_is_unexported(name) || Self::path_root(receiver) == "C" {
            return None;
        }
        let dir = Self::parent_dir(file);
        if self.go_package_dirs_unvouched.contains(&dir) {
            return None;
        }
        let package = self.go_package_by_file.get(file)?;
        let key = (dir, package.clone(), name.to_string());
        if self.go_package_member_vetoes.contains(&key) {
            return None;
        }
        let source_is_test = file.ends_with("_test.go");
        let mut visible = self
            .go_package_methods
            .get(&key)?
            .iter()
            .filter(|(path, _)| source_is_test || !path.ends_with("_test.go"));
        let (target_file, target_symbol) = visible.next()?;
        visible
            .next()
            .is_none()
            .then(|| (target_file.clone(), target_symbol.clone(), package.clone()))
    }

    /// Where a bare `name` written in `file` goes by Swift's module-scope rule.
    ///
    /// A Swift target is one unqualified namespace spanning every file that
    /// constitutes it — the Go package-block shape, keyed on the module the
    /// path names rather than on `(directory, package clause)`. Two files under
    /// `app/MarkDevKit/` share a module even when they sit in different
    /// folders; two files that merely share a folder do not, if they belong to
    /// different targets.
    ///
    /// The declaring file must not be `file`: the same-file rungs own that.
    /// Abstains when the module declares the name twice, for the same
    /// input-order reason [`Self::same_package_target`] does.
    /// The unique `type_methods` target that is evidence at this call site.
    ///
    /// Same-file and an import binding of the type were already enough.
    /// Swift siblings need a third: no import is written between files of
    /// one module, so `Normalizer.normalize()` in `Renderer.swift` must
    /// still bind when `Other.normalize` exists elsewhere and would make
    /// the global rung AmbiguousGlobal — the MarkDev Kit enum-helper shape.
    ///
    /// Overloads of one method on one type are several hits in the same
    /// file (`present(_:Int)` and `present(_:String)`). `len == 1` treated
    /// that as "no type", so `Viewer.shared.present(...)` fell to
    /// AmbiguousGlobal and the file looked unwired. Dedup, then require
    /// every remaining hit to name that one file.
    pub(super) fn type_method_target<'a>(
        &self,
        call_file: &str,
        type_name: &str,
        hits: &'a [(String, String)],
    ) -> Option<(&'a str, &'a str)> {
        let mut uniq: Vec<&(String, String)> = hits.iter().collect();
        uniq.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
        uniq.dedup();
        // Swift: a type the caller's own module declares shadows the same name
        // an import brings in, and a module is where `Theme` is looked up first.
        // When some hits sit in the caller's modules, only those are candidates.
        if let Some(own) = self.swift_module_by_file.get(call_file) {
            let in_own: Vec<&(String, String)> = uniq
                .iter()
                .copied()
                .filter(|hit| {
                    self.swift_module_by_file
                        .get(&hit.0)
                        .is_some_and(|modules| modules.iter().any(|m| own.contains(m)))
                })
                .collect();
            if !in_own.is_empty() {
                uniq = in_own;
            }
        }
        if uniq.is_empty() || !uniq.iter().all(|hit| hit.0 == uniq[0].0) {
            return None;
        }
        let (target_file, target_symbol) = uniq[0];
        let corroborated = target_file == call_file
            || self
                .import_bindings
                .get(call_file)
                .is_some_and(|bindings| bindings.contains_key(type_name))
            || match (
                self.swift_module_by_file.get(call_file),
                self.swift_module_by_file.get(target_file.as_str()),
            ) {
                (Some(caller), Some(target)) => caller.iter().any(|module| target.contains(module)),
                _ => false,
            }
            // An extension function reaches its receiver type through the
            // function's own import: `import p.slugify` is the evidence that
            // `"x".slugify()` names it, and the receiver type is not imported.
            || hits_import_binding(
                self.import_bindings.get(call_file),
                target_symbol,
                target_file,
            )
            // A Kotlin type of the caller's own package needs no import.
            || match (
                self.jvm_package_by_file.get(call_file),
                self.jvm_package_by_file.get(target_file.as_str()),
            ) {
                (Some(caller_package), Some(target_package)) => caller_package == target_package,
                _ => false,
            };
        corroborated.then_some((target_file.as_str(), target_symbol.as_str()))
    }

    /// Every `type_methods` hit a Swift type spelling reaches from `call_file`
    /// when the file is compiled into more than one target: one per target.
    ///
    /// `ios/Shared/Formatter.swift` is built into the app and the widget, and
    /// its `Theme.accent()` is each target's own `Theme`. Both are right — in
    /// different builds — so both are edges. Abstains (empty) unless every
    /// target of the file resolves the type to exactly one file.
    pub(super) fn type_method_targets_per_module<'a>(
        &self,
        call_file: &str,
        hits: &'a [(String, String)],
    ) -> Vec<(&'a str, &'a str)> {
        let Some(own) = self
            .swift_module_by_file
            .get(call_file)
            .filter(|m| m.len() > 1)
        else {
            return Vec::new();
        };
        let mut found: Vec<(&str, &str)> = Vec::new();
        for module in own {
            let in_module: Vec<&(String, String)> = hits
                .iter()
                .filter(|hit| {
                    self.swift_module_by_file
                        .get(&hit.0)
                        .is_some_and(|modules| modules.contains(module))
                })
                .collect();
            let files: BTreeSet<&str> = in_module.iter().map(|hit| hit.0.as_str()).collect();
            if files.len() != 1 {
                return Vec::new();
            }
            let hit = in_module[0];
            if !found.contains(&(hit.0.as_str(), hit.1.as_str())) {
                found.push((hit.0.as_str(), hit.1.as_str()));
            }
        }
        found
    }
}
