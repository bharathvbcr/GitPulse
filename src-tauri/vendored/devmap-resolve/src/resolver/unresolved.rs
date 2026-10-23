use super::*;

impl Resolver {
    /// Bare-name miss: [`NoNamesake`] when the corpus has no symbol of that
    /// name, otherwise [`Unresolved`].
    pub(super) fn bare_name_miss(&self, callee_name: &str) -> UnresolvedClass {
        if self.symbol_index.contains_key(callee_name) {
            UnresolvedClass::Unresolved
        } else {
            UnresolvedClass::NoNamesake
        }
    }

    /// Resolve a spelling in the nearest declaring scope, retaining the full
    /// identity. A sibling function's nested declaration is never in scope.
    pub(super) fn lexical_target(
        &self,
        file: &str,
        family: LangFamily,
        caller: Option<&str>,
        name: &str,
    ) -> Option<String> {
        // Absent everywhere: no scope can declare it.
        self.symbol_index.get(name)?;
        let mut scope = caller.unwrap_or(file);
        for _ in 0..1024 {
            let mut candidates = self
                .scope_children
                .get(&(file.to_string(), scope.to_string(), name.to_string()))
                .into_iter()
                .flatten()
                .filter(|(kind, _)| {
                    !Self::family_needs_explicit_receiver(family) || *kind != SymbolKind::Method
                })
                .map(|(_, identity)| identity.as_ref());
            if let Some(first) = candidates.next() {
                return candidates
                    .all(|other| other == first)
                    .then(|| first.to_string());
            }
            if scope == file {
                return None;
            }
            let next = self
                .symbol_parents
                .get(&(file.to_string(), scope.to_string()))
                .map(String::as_str)
                .unwrap_or(file);
            if next == scope {
                return None;
            }
            scope = next;
        }
        None
    }

    /// A local binding whose initializer this file extracted as a nested
    /// callable: `const walk = (folders) => { walk(folders); }; walk([])`.
    ///
    /// Unlike [`Self::lexical_target`], this does **not** walk up to the file
    /// scope. Walking up would turn `const walk = getWalk(); walk()` into a
    /// call of a module-level `walk`, which is the RA1 defect. Matching only
    /// a symbol whose parent is the caller (or the callee itself, for the
    /// recursive call inside that arrow) is the one case the binding's value
    /// is the extracted function.
    ///
    /// A bare name in Go is never a method: a method is reachable only through
    /// a receiver, so `Rebuild(1)` written inside `func (Holder) Rebuild()`
    /// names the package function, not the method it sits in, and `delete(m,
    /// k)` inside `func (c *cache) delete` is the builtin. The caller's own
    /// identity matches below (a function calling itself), which for a method
    /// caller used to answer a self-edge at DETERMINISTIC.
    pub(super) fn local_extracted_callee(
        &self,
        file: &str,
        family: LangFamily,
        caller: Option<&str>,
        name: &str,
    ) -> Option<String> {
        let caller = caller?;
        let hits = self.symbol_index.get(name)?;
        let mut candidates = hits.iter().filter_map(|(path, kind, _, identity)| {
            if path != file {
                return None;
            }
            if (family == LangFamily::Go && *kind == SymbolKind::Method)
                || Self::never_a_call_target(family, *kind)
            {
                return None;
            }
            let identity = identity.as_ref();
            if identity == caller {
                return Some(identity);
            }
            self.symbol_parents
                .get(&(file.to_string(), identity.to_string()))
                .is_some_and(|parent| parent == caller)
                .then_some(identity)
        });
        let first = candidates.next()?;
        candidates
            .all(|other| other == first)
            .then(|| first.to_string())
    }

    /// The callable `sym` is declared inside, when it is a local function (the
    /// smallest enclosing symbol is a function or method). A local function's
    /// recorded parent is the file, so containment is the only fact that tells
    /// `a`'s `helper` from `b`'s; a member, an extension, a companion member
    /// and a top-level function all answer `None`, so they stay siblings.
    pub(super) fn innermost_container<'a>(
        ext: &'a Extraction,
        sym: &ExtractedSymbol,
    ) -> Option<&'a str> {
        ext.symbols
            .iter()
            .filter(|other| {
                other.kind != SymbolKind::File
                    && other.span.start_byte <= sym.span.start_byte
                    && sym.span.end_byte <= other.span.end_byte
                    && (other.span.start_byte, other.span.end_byte)
                        != (sym.span.start_byte, sym.span.end_byte)
            })
            .min_by_key(|other| other.span.end_byte - other.span.start_byte)
            .filter(|other| matches!(other.kind, SymbolKind::Function | SymbolKind::Method))
            .map(|other| other.qualified_name.as_str())
    }

    /// Whether `resolution` binds a call written inside `caller` back to
    /// `caller` itself where `caller` is one of several same-named callables of
    /// one container (overloads share one id).
    ///
    /// Real recursion binds there too, and nothing recorded about the call (no
    /// arity) separates it from a call to a sibling overload, so the ladder
    /// abstains rather than assert either; the accepted cost is a recursion
    /// inside one overload of a real pair.
    pub(super) fn is_overload_self_call(
        &self,
        file: &str,
        caller: &str,
        resolution: Option<&Resolution>,
    ) -> bool {
        let Some(
            Resolution::SameFile {
                target_symbol,
                target_file,
            }
            | Resolution::ReceiverType {
                target_symbol,
                target_file,
                ..
            },
        ) = resolution
        else {
            return false;
        };
        target_symbol == caller
            && target_file == file
            && self
                .shared_identities
                .contains(&(file.to_string(), caller.to_string()))
    }

    /// The leftmost segment of a dotted, scoped or slashed path.
    ///
    /// One owner for a split that `classify_unresolved` was doing inline and
    /// `index_extractions` now needs too — `metrics.counters.Inc()` is evidence
    /// about `metrics`, `std::fs::write()` about `std`, and a Go specifier
    /// `example.com/pkg/sub` about `example.com`. All three separators, because
    /// the caller does not know which language wrote the string.
    pub(super) fn path_root(path: &str) -> &str {
        path.split("::")
            .next()
            .unwrap_or(path)
            .split('.')
            .next()
            .unwrap_or(path)
            .split('/')
            .next()
            .unwrap_or(path)
    }

    /// Whether any indexed file this family may resolve into declares `name`.
    ///
    /// The corpus's veto over a name table. A rung that says "the language
    /// declares this" must not fire where the *repository* declares it too:
    /// there the ladder either bound the reference already or abstained between
    /// several declarations, and an abstention filed as "expected" is a real
    /// ambiguity hidden behind a label. Any symbol kind counts — a struct, a
    /// trait and a function named `Default` are all reasons to abstain.
    pub(super) fn family_declares(&self, family: LangFamily, name: &str) -> bool {
        self.symbol_index.get(name).is_some_and(|hits| {
            hits.iter()
                .any(|(_, _, candidate_family, _)| family.admits(*candidate_family))
        })
    }

    /// Why a call that failed the resolution ladder has no edge (SC18).
    ///
    /// Ordered by strength of evidence, and **fail-open toward `Unresolved`**:
    /// every rung must prove its claim, and a call that proves nothing keeps the
    /// label that means "this may be a defect". Mislabelling a real failure as
    /// expected is the only outcome here that loses information.
    pub(super) fn classify_unresolved(
        &self,
        file_path: &str,
        family: LangFamily,
        callee_name: &str,
        receiver: Option<&str>,
        enclosing_symbol: &str,
        position: UsePosition<'_>,
    ) -> UnresolvedClass {
        // X40. A name in type position is classified from type-position
        // evidence, and only then from the value-position ladder below.
        //
        // Ordered the way the value rungs are: file-specific evidence (the
        // qualifier the author wrote) outranks a name list, for the same reason
        // an import outranks the host-global table.
        if let UsePosition::Type {
            types: Some(typed),
            scope,
        } = position
        {
            // `t *testing.T`. The extractor splits the written type into a bare
            // name for dispatch and a `TypeQualifier` sibling for provenance
            // (SC25), so by the time the bare `T` fails the ladder the qualifier
            // is the only thing that still knows where it came from. Read
            // through `declared_types`, which is where that sibling was
            // indexed, and scope-only for the SC9 reason: a qualifier this
            // scope wrote may not speak for a same-named binding in another.
            let qualifier = self.qualifier_declared_in(file_path, scope, typed);
            if let Some(qualifier) = qualifier {
                if let Some(module) = self
                    .external_imports
                    .get(file_path)
                    .and_then(|imports| imports.get(qualifier))
                {
                    return UnresolvedClass::External {
                        module: module.clone(),
                    };
                }
                // A repo-relative qualifier that named no indexed file is an
                // index gap, exactly as it is for a call — never `External`.
                if self
                    .unindexed_local_imports
                    .get(file_path)
                    .is_some_and(|imports| imports.contains_key(qualifier))
                {
                    return UnresolvedClass::Unresolved;
                }
            }
        }

        // Swift: an operator no indexed type overloads is the standard
        // library's, whatever its operands are.
        if family == LangFamily::Swift
            && !callee_name.is_empty()
            && !callee_name.chars().any(|c| c.is_alphanumeric() || c == '_')
            && !self.symbol_index.contains_key(callee_name)
        {
            return UnresolvedClass::NoNamesake;
        }

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

        let external = self.external_imports.get(file_path);
        // An import whose specifier is repo-relative and whose target is not
        // indexed. Consulted at every point `external` is, and *ahead* of it in
        // no case — the two maps are disjoint by construction — but ahead of the
        // host-global table for the same reason `external` is: `from .helpers
        // import fetch` is file-specific evidence about what `fetch` is, and it
        // must not be answered from a list of runtime globals.
        let local_gap = self.unindexed_local_imports.get(file_path);

        let Some(receiver) = receiver else {
            // `useState()`: the bare name is itself an imported binding.
            if let Some(module) = external.and_then(|imports| imports.get(callee_name)) {
                return UnresolvedClass::External {
                    module: module.clone(),
                };
            }
            // `from .helpers import thing`: the import proves the name is
            // *inside* this repository, so failing to resolve it is an index
            // gap. That is the tier that means "this may be a defect", never
            // `External`, which is printed as expected and not worth acting on.
            if local_gap.is_some_and(|imports| imports.contains_key(callee_name)) {
                return UnresolvedClass::Unresolved;
            }
            // A module-scope binding of the file itself (`const fetchThing =
            // vi.fn()`). After the two import rungs, not with the enclosing
            // scope's own binding above: a `const { x } = require('m')` is a
            // module-scope declarator too, and the import is what says where
            // it came from.
            if self.file_scope_declares(file_path, callee_name) {
                return UnresolvedClass::LocalBinding;
            }
            // Type names obey the same binding precedence as callees:
            // `use other::String` and a generic `<String>` both shadow the
            // prelude. Qualified types were handled above; only a bare name
            // with no stronger origin may be explained by a name table.
            if matches!(position, UsePosition::Type { .. }) {
                if crate::builtins::is_reserved_module_root(family, callee_name) {
                    return UnresolvedClass::External {
                        module: callee_name.to_string(),
                    };
                }
                if crate::builtins::is_prelude_type(family, callee_name)
                    && !self.family_declares(family, callee_name)
                {
                    return UnresolvedClass::Builtin;
                }
            }
            // Written imports outrank the language/prelude table. In
            // particular, `from .missing import len` is an index gap rather
            // than a builtin merely because its spelling matches one.
            if crate::builtins::is_builtin(family, callee_name) {
                return UnresolvedClass::Builtin;
            }
            if family == LangFamily::Swift && !self.family_declares(family, callee_name) {
                if crate::builtins::is_prelude_type(family, callee_name) {
                    return UnresolvedClass::Builtin;
                }
                let modules = self.imported_swift_modules(file_path);
                if let Some(module) = crate::builtins::swift_sdk_module(
                    modules.iter().map(String::as_str),
                    callee_name,
                ) {
                    return UnresolvedClass::External {
                        module: module.to_string(),
                    };
                }
            }
            // `setTimeout()` / `fetch()`: no import binds it because the
            // runtime puts it on the global object. Checked *after* the import
            // rung on purpose — an explicit `import { fetch } from 'node-fetch'`
            // is file-specific evidence and outranks a global name list.
            if let Some(environment) = crate::builtins::host_global_environment(family, callee_name)
            {
                return UnresolvedClass::HostGlobal {
                    environment: environment.to_string(),
                };
            }
            // `from numpy import *`: nothing binds `zeros` by name, but the
            // star import is where it may have come from, which is the evidence
            // that kept it from a namesake. Last, so that a name an explicit
            // import, a builtin or a host global already accounts for keeps
            // the more specific label.
            if let Some(module) = external.and_then(|imports| imports.get(STAR_IMPORT)) {
                return UnresolvedClass::External {
                    module: module.clone(),
                };
            }
            if local_gap.is_some_and(|imports| imports.contains_key(STAR_IMPORT)) {
                return UnresolvedClass::Unresolved;
            }
            return self.bare_name_miss(callee_name);
        };

        // A path receiver is rooted at its leftmost segment, so
        // `metrics.counters.Inc()` is evidence about `metrics` and
        // `std::fs::write()` is evidence about `std`. Both separators are
        // handled because Rust's `scoped_identifier` receivers use `::`.
        let root = Self::path_root(receiver);
        let receiver_binding = match position {
            UsePosition::Value { receiver_binding } => receiver_binding,
            UsePosition::Type { .. } => None,
        };

        if let Some(imports) = external {
            // `strings.TrimSpace()` / `assert.Equal()`: the receiver is the
            // local handle for a module that resolved to no indexed file.
            // A captured value bearing that name is a different binding.
            if receiver_binding.is_none() {
                if let Some(module) = imports.get(root) {
                    return UnresolvedClass::External {
                        module: module.clone(),
                    };
                }
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
            let declared = |slot: &str| {
                self.declared_receiver_slot(
                    file_path,
                    enclosing_symbol,
                    root,
                    receiver_binding,
                    slot,
                )
            };

            // `t *testing.T`: the type is written with its package, and that
            // package is the import that resolved to nothing.
            if let Some(module) = declared("@mod").and_then(|q| imports.get(q)) {
                return UnresolvedClass::External {
                    module: module.clone(),
                };
            }
            // `use reqwest::Client; c: &Client`: no qualifier survives at the
            // use site, but the bare type name is itself an imported binding.
            if let Some(module) = declared("@type").and_then(|t| imports.get(t)) {
                return UnresolvedClass::External {
                    module: module.clone(),
                };
            }
        }

        // Swift prelude / SDK methods on a typed value. SC25's import-handle
        // lookup cannot see `s.count` where `s` is a `String` — String is a
        // language type, not an imported binding. Prelude answers without an
        // import; SDK types answer only when this file imported that module.
        if family == LangFamily::Swift {
            if let Some(declared_type) = self.declared_receiver_slot(
                file_path,
                enclosing_symbol,
                root,
                receiver_binding,
                "@type",
            ) {
                if crate::builtins::is_prelude_type(family, declared_type)
                    && !self.family_declares(family, declared_type)
                {
                    return UnresolvedClass::Builtin;
                }
                if !self.family_declares(family, declared_type) {
                    let modules = self.imported_swift_modules(file_path);
                    if let Some(module) = crate::builtins::swift_sdk_module(
                        modules.iter().map(String::as_str),
                        declared_type,
                    ) {
                        return UnresolvedClass::External {
                            module: module.to_string(),
                        };
                    }
                }
            }
        }

        // `helpers.thing()` where `helpers` came from `from . import helpers`
        // and that module is not indexed. The receiver *is* typed — by an
        // import — so this is not an uninferred receiver; it is the same index
        // gap as the bare case above.
        if receiver_binding.is_none() && local_gap.is_some_and(|imports| imports.contains_key(root))
        {
            return UnresolvedClass::Unresolved;
        }

        // X43. The receiver is a **module path** rather than a value.
        //
        // `std::fs::write(...)` reached `UninferredReceiver`, the tier that
        // means "the receiver is a value whose type we could not infer", and
        // `std::fs` is not a value at all. 2,128 rows on this repository, plus
        // 373 rooted at `serde_json` — a crate the file's own `use` lines name,
        // whose *root* is a key no handle-keyed map above holds, because Rust
        // makes a crate addressable by path without a `use` of the root.
        //
        // Two shapes, and the test differs because the evidence does:
        //
        // * a receiver containing `::` whose every segment is a plain
        //   identifier is a path *syntactically* — no binding in these
        //   languages can contain `::`, so this cannot mistake a local for a
        //   module. It is also what keeps a chained-call receiver out: the text
        //   `std::fs::write("out.txt", body)` has a segment with parentheses,
        //   so `unwrap()` on its result stays an uninferred receiver, which is
        //   what it is.
        // * a bare root is only a module if the enclosing scope does **not**
        //   bind that name. `serde_json::from_str(x)` reduces to the receiver
        //   `serde_json`, and `let serde_json = build(); serde_json.take()`
        //   reduces to the same string — the scope's own binding tables are the
        //   only thing that separates them, and they are asked in the same
        //   direction the `LocalBinding` rung asks them.
        // A positive use-site fact is authoritative even for an untyped
        // capture. Absence is not proof of no binding: older/hand-built
        // extractions may omit site facts, so retain their wider evidence.
        let root_is_a_value_here =
            self.root_is_a_value(file_path, enclosing_symbol, root, receiver_binding);
        // The bare shape requires the receiver to *be* the root and nothing
        // else. `std::fs::write("out.txt", body)` as the receiver of `unwrap()`
        // is also rooted at `std`, and it is an expression, not a module — the
        // whole point of the tier it belongs in.
        let bare_module_handle =
            receiver == root && Self::receiver_is_module_path(&format!("{root}::x"));
        if Self::receiver_is_module_path(receiver) || (bare_module_handle && !root_is_a_value_here)
        {
            // Repo-relative by construction: `crate::missing::helper()` cannot
            // name anything outside this tree, so a miss is an index gap and
            // keeps the tier that says a human should look.
            // These module roots are Rust evidence. A Python closure's
            // `self.member`, or a Rust `self.field`, is a value access. The
            // reduced receiver "self" cannot distinguish `self::item` from
            // `self.item`, so only a surviving `self::module` path proves a
            // module; keep the bare ambiguous form in the attribution gaps.
            if family == LangFamily::Rust
                && ((matches!(root, "crate" | "super")
                    || (root == "self" && Self::receiver_is_module_path(receiver)))
                    || (root != "self"
                        && self
                            .local_module_roots
                            .get(file_path)
                            .is_some_and(|roots| roots.contains(root))))
            {
                return UnresolvedClass::ModulePath;
            }
            if crate::builtins::is_reserved_module_root(family, root)
                || self
                    .external_module_roots
                    .get(file_path)
                    .is_some_and(|roots| roots.contains(root))
            {
                return UnresolvedClass::External {
                    module: root.to_string(),
                };
            }
            // The receiver is a **prelude type**, not a module and not a value:
            // `Vec::new()`, `String::from(s)`, `Default::default()`,
            // `Option::Some`. These reached `UninferredReceiver`, the tier that
            // means "the receiver is a value whose type we could not infer",
            // and `Vec` is not a value at all — the language's own standard
            // library declares it, so no indexed file can ever own the method.
            // Measured on this repository: 1,532 rows, led by `Vec` (777),
            // `String` (514) and `Default` (169).
            //
            // The same sentence the Swift rung two blocks above already makes
            // for `s.count` where `s` is a `String`, in the language whose
            // spelling for it is a path rather than a value. It carries the
            // same veto, and the veto is the point: a repository that declares
            // its own `Vec` keeps `Vec::new` in the attribution gaps, because
            // there the corpus — not the prelude — is the authority.
            //
            // Placed after the module-root rungs on purpose. A `use
            // other::Vec` made this file's `Vec` something else, and that
            // import is file-specific evidence that outranks a name table —
            // the rule the bare-name ladder states where it checks imports
            // before `is_builtin`.
            if !root_is_a_value_here
                && receiver == root
                && crate::builtins::is_prelude_type(family, root)
                && !self.family_declares(family, root)
            {
                return UnresolvedClass::Builtin;
            }
        }

        // X48. The receiver is rooted at a **host global object**.
        //
        // `console.log(...)`, `JSON.stringify(x)`, `process.env`,
        // `Math.floor(n)`. These reached `UninferredReceiver`, the tier that
        // means "the receiver is a value whose type we could not infer" — and
        // `console` is not a receiver whose type could not be inferred, it is
        // one whose type the runtime states. Same argument X43 made for
        // `std::fs`, in the language whose globals are objects rather than
        // modules. Measured: 126 of the 316 JS `uninferred_receiver` rows on
        // this repository, 7,408 of 58,904 across scholarlm's JS/TS.
        //
        // Three guards, and the table alone is never enough.
        //
        // The receiver must **be** the root and nothing else — the same shape
        // X43's `bare_module_handle` requires, and here it is load-bearing in a
        // way the `::` version is not. X44 reduces a receiver *structurally*,
        // so `JSON.stringify(rows)` as the receiver of `.padStart(…)` is
        // recorded as `JSON.stringify` with the parentheses gone: after that
        // reduction a call result and a property read are the same string, and
        // no test on the text can separate them. `process.env.PWD` is therefore
        // **not** claimed, and that is an abstention rather than an oversight —
        // it costs rows and invents nothing. Separating them needs the
        // extractor to keep "this was a call" in the reduced receiver, which is
        // `ExtractedCall::receiver_expr`'s shape and a change of its own.
        //
        // The enclosing scope must not bind the root, which
        // `root_is_a_value_here` already answers. And the corpus gets the last
        // word: a repository that declares its own `Date` keeps `Date.parse` in
        // the defect tier, which is the veto `is_prelude_type` opens with. The
        // file's own imports needed no test here — an import of the root
        // returned `External` several rungs above.
        if !root_is_a_value_here && receiver == root && Self::receiver_is_property_path(receiver) {
            if let Some(environment) = crate::builtins::host_global_object(family, root) {
                if !self.family_declares(family, root) {
                    return UnresolvedClass::HostGlobal {
                        environment: environment.to_string(),
                    };
                }
            }
        }

        // No symbol of this family carries the member's name, so there is no
        // owner for type inference to find: every edge ends at an indexed
        // symbol, and none could end here. The evidence `bare_name_miss`
        // stands on, asked of a receiver. Measured at generation 4144: 827 of
        // the 837 JavaScript `uninferred_receiver` rows (`push`, `includes`,
        // `length`) named a member no JavaScript or TypeScript file declares.
        // A namesake in another family is no candidate — `family.admits` is
        // the same scoping the binding rungs use.
        if !self.family_declares(family, callee_name) {
            return UnresolvedClass::NoNamesake;
        }

        // A receiver we could not type. Not a defect — naming its owner needs
        // real type inference — but distinct from a bare-name failure, and by
        // far the larger group.
        UnresolvedClass::UninferredReceiver
    }

    /// Whether a receiver expression is a dotted run of plain identifiers.
    ///
    /// The `.` twin of [`Self::receiver_is_module_path`], and it exists for the
    /// same reason: `process` and `process.env` are the objects themselves,
    /// while `JSON.stringify(x)` as the receiver of `.length` is an
    /// *expression* that merely starts at one. A segment carrying parentheses,
    /// brackets, quotes or whitespace disqualifies the whole receiver, so the
    /// chained case keeps the tier it belongs in.
    pub(super) fn receiver_is_property_path(receiver: &str) -> bool {
        !receiver.is_empty()
            && receiver.split('.').all(|segment| {
                !segment.is_empty()
                    && segment
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric() || character == '_')
            })
    }

    /// Whether a receiver expression is a module **path** rather than a value.
    ///
    /// Syntactic on purpose. A binding cannot contain `::` in any language this
    /// resolver types receivers for, so a `::`-joined run of plain identifiers
    /// is a path and nothing else. Requiring *every* segment to be an
    /// identifier is what excludes a chained call whose text happens to contain
    /// a path — `std::fs::write("out.txt", body)` as the receiver of `unwrap()`
    /// — from being read as one.
    pub(super) fn receiver_is_module_path(receiver: &str) -> bool {
        receiver.contains("::")
            && receiver.split("::").all(|segment| {
                !segment.is_empty()
                    && segment
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric() || character == '_')
            })
    }

    /// Whether an import specifier names something inside this repository *by
    /// construction*, whatever the corpus contains.
    ///
    /// A relative specifier is resolved against the importing file's own
    /// directory, so it cannot reach outside the tree. `crate::` and `self::`
    /// are the Rust spellings of the same fact. Everything else — `requests`,
    /// `react`, `example.com/m/pkg` — may or may not be external, and only a
    /// failed lookup plus a non-relative specifier is evidence that it is.
    pub(super) fn specifier_is_repo_relative(specifier: &str) -> bool {
        let specifier = specifier.trim_matches(|c| c == '\'' || c == '"');
        specifier.starts_with('.')
            || matches!(specifier, "self" | "super" | "crate")
            || specifier.starts_with("self::")
            || specifier.starts_with("super::")
            || specifier.starts_with("crate::")
    }

    /// Return the module path that should be resolved for one imported name.
    /// Python's `from . import module` names the child module in the import
    /// list, while named imports in the other supported languages keep the
    /// module specifier unchanged.
    pub(super) fn import_spec_for_name(module_specifier: &str, imported_name: &str) -> String {
        if !imported_name.is_empty()
            && imported_name != "*"
            && module_specifier.chars().all(|character| character == '.')
        {
            Self::python_submodule_spec(module_specifier, imported_name)
        } else {
            module_specifier.to_string()
        }
    }

    /// Record `name` as the crate rooted at `root`, abstaining when another
    /// root already claims it.
    ///
    /// A crate with both a `lib.rs` and a `main.rs` is one crate with one root,
    /// not two claimants. Two *directories* of the same name are the ambiguity
    /// this abstains on.
    pub(super) fn claim_rust_crate_name(
        rust_crate_roots: &mut BTreeMap<String, Option<String>>,
        name: String,
        root: String,
    ) {
        match rust_crate_roots.entry(name) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(Some(root));
            }
            std::collections::btree_map::Entry::Occupied(mut held) => {
                if held.get().as_deref() != Some(root.as_str()) {
                    held.insert(None);
                }
            }
        }
    }
}
