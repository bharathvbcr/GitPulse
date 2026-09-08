use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use rayon::prelude::*;

use devmap_extract::model::*;

use crate::model::*;
use devmap_extract::GoModule;

/// One package-level declaration, as [`Resolver::go_package_symbols`] holds it:
/// the file that declares it, its qualified name, and its kind.
type PackageDecl = (String, String, SymbolKind);

/// Candidate file, kind, language family, and stable qualified identity.
type IndexedSymbol = (String, SymbolKind, LangFamily, Arc<str>);

/// Where the name a resolution rung failed on was written.
///
/// The tiers in [`UnresolvedClass`] are stated over evidence, and the evidence
/// available for a name differs by position: a *value* is answered by the
/// scope's bindings, the language's builtins and the file's imports, while a
/// *type* additionally has the qualifier the author wrote beside it and the
/// language's prelude. Passing the position explicitly is what lets
/// [`Resolver::classify_unresolved`] consult only the rungs whose evidence
/// actually exists, instead of a caller pre-deciding which class to file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsePosition<'a> {
    /// A call, or an identifier in expression position.
    Value,
    /// A type annotation. `types` names the value this annotation types — `t`
    /// for `t *testing.T` — when the extractor recorded one, because that is
    /// the key the `TypeQualifier` sibling was indexed under.
    Type { types: Option<&'a str> },
}

pub struct Resolver {
    symbol_index: BTreeMap<String, Vec<IndexedSymbol>>,
    file_symbols: BTreeMap<String, Vec<String>>, // file_path -> symbol_names
    /// `<file>::<exported name>` -> the file that declares it, for
    /// `export { x } from './m'`. See `compute_reexport_chains`.
    reexport_chains: BTreeMap<String, String>,
    receiver_types: BTreeMap<String, String>, // (file_path:var_name) -> ClassType
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
    /// `(family, type name)` -> the type names it declares as supertypes.
    ///
    /// X42. Built from the `Heritage` / `HeritageInterface` references the
    /// extractor already emits — the same rows that produce `Extends` and
    /// `Implements` edges — so nothing new is parsed and no naming convention
    /// is consulted. It exists because `self.m()` where `m` is declared by a
    /// base class is a *receiver-type* fact, and until this map there was no
    /// way to state it: the ladder fell through to the global rung and bound
    /// the call by bare name, at HIGH, to whichever declaration happened to be
    /// unique.
    ///
    /// Flat by bare type name, exactly as `type_methods` is, so it adds no
    /// namespace imprecision that map does not already carry — and the walk
    /// that reads it refuses to continue through a type name two files declare,
    /// where the chain stops being identifiable.
    supertypes: BTreeMap<(LangFamily, String), BTreeSet<String>>,
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
    /// Per-file local import name → module specifier, for imports whose
    /// specifier is **relative** and whose target is not indexed.
    ///
    /// Deliberately not `external_imports`. `.helpers`, `./util`, `super::x`
    /// and `crate::y` are intra-repository by construction: the specifier is
    /// resolved against this file's own directory, so it cannot name anything
    /// outside the corpus. Failing to resolve one means the target was not
    /// indexed — gitignored, over the size cap, generated — which is an index
    /// gap, not evidence that the name comes from outside. Filing it under
    /// `External` launders a gap into the tier printed as not-worth-acting-on.
    ///
    /// Kept as its own map rather than dropped, so the specifier survives as
    /// evidence in the ledger's reason string.
    unindexed_local_imports: BTreeMap<String, BTreeMap<String, String>>,
    /// Per-file set of **module-path roots** this file's imports name, split
    /// into the two halves `external_imports` and `unindexed_local_imports`
    /// already draw: outside the corpus, and repo-relative-but-unindexed.
    ///
    /// X43. A Rust path is addressable without a `use` of its root —
    /// `use serde_json::Value;` makes `Value` a binding but leaves
    /// `serde_json::from_str(...)` written as a path, whose *root* is a key
    /// neither existing map has. These are those roots, derived from the same
    /// walk so the three maps cannot disagree about what a specifier meant.
    ///
    /// Consulted only for a receiver that is syntactically a path (it contains
    /// `::`), which is what keeps a local variable sharing a crate's name out
    /// of reach — a binding cannot contain `::`.
    external_module_roots: BTreeMap<String, BTreeSet<String>>,
    /// The repo-relative half of the above. A path rooted here is an index gap,
    /// never `External`.
    local_module_roots: BTreeMap<String, BTreeSet<String>>,
    /// (file, bare symbol name) → qualified name. Edge endpoints are graph
    /// identities, not bare words: emitting `open` instead of `app.py::open`
    /// makes an edge unjoinable to the node it names.
    qualified_names: BTreeMap<(String, String), Option<Arc<str>>>,
    /// `(file, qualified name)` → the symbol that declares it, with the file
    /// path standing for "declared at file level".
    ///
    /// A bare `run()` can only reach what is in scope at the call site.
    /// `file_symbols` lists every symbol in the file, instance methods
    /// included, so matching a bare callee against it bound
    /// `def invoke(): run()` to `class C: def run(self)` — code that raises
    /// `NameError` — at DETERMINISTIC, and handed `C.run` a fabricated caller
    /// that shields it from the dead-code pass. Answering "what declares this"
    /// is what makes the scope test possible.
    symbol_parents: BTreeMap<(String, String), String>,
    go_modules: Vec<GoModule>,
    /// Whether `go_modules` was supplied for the snapshot currently indexed.
    ///
    /// `index_go_modules` and `index_extractions` are two calls that together
    /// describe one snapshot, and nothing forced them to stay in step: a
    /// `Resolver` reused for a second `index_extractions` kept the first
    /// snapshot's module prefixes and `replace` directives and resolved Go
    /// imports against them — at DETERMINISTIC. Clearing the modules inside
    /// `index_extractions` is not the fix: `devmap-cli` supplies them *first*
    /// (main.rs:1264-1265) and the in-crate Go fixture supplies them second, so
    /// an unconditional clear would break one order or the other.
    ///
    /// This flag makes the reset order-independent instead. Modules survive
    /// exactly one indexing pass; a second pass that was not given a fresh set
    /// resolves without module prefixes, which under-resolves Go imports rather
    /// than resolving them confidently against a stale map.
    go_modules_fresh: bool,
    /// file_path → Go package identifier (`pkg` in `package pkg`).
    go_package_by_file: BTreeMap<String, String>,
    /// X45. `(directory, package clause, bare name)` → the **package-level**
    /// declarations of that name, as `(file, qualified name, kind)`.
    ///
    /// The index behind [`Resolver::same_package_target`]. Keyed on the
    /// directory *and* the package clause because a directory is not a package:
    /// `search/` holds `package search` and its external test package
    /// `package search_test`, and the second is outside the first's package
    /// block. Holds only declarations whose parent is the file — a method is
    /// declared on its type, not in the package block, so a bare name cannot
    /// reach it.
    go_package_symbols: BTreeMap<(String, String, String), Vec<PackageDecl>>,
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
    /// Slash-separated components in the deepest path this resolver indexed.
    ///
    /// The module ladders in [`Self::resolve_import_path`] walk a specifier
    /// from its full length down to one segment, probing a candidate file path
    /// at every rung. Nothing bounded how many rungs there were: a specifier is
    /// limited only by the extractor's `MAX_SOURCE_BYTES`, and each rung
    /// allocates a fresh path, so one `use crate::a::a::…;` line cost time
    /// quadratic in its own length. Measured before the bound: a 40,000-segment
    /// specifier took **124 s** in one file, and the source-size limit permits
    /// roughly eight times that.
    ///
    /// This is the honest ceiling. A rung can only match a file that was
    /// indexed, so a rung with more components than the deepest indexed path
    /// cannot match anything — skipping it removes no answer. Derived from the
    /// corpus rather than picked, so it cannot become a cap on what a real
    /// repository is allowed to contain.
    max_indexed_path_depth: usize,
    /// File basename -> the one indexed file with that basename, or `None`
    /// when two or more share it.
    ///
    /// The last rung of `resolve_import_path` needs "is there exactly one file
    /// called `util.h`". Answering that by scanning `file_symbols` would be one
    /// pass over every file per import — quadratic in a repository's size, on
    /// the hot path of every build. The map is built once, in the same walk
    /// that establishes the file universe, and stores the *answer* rather than
    /// the candidates: `None` is the ambiguous case, so an ambiguous basename
    /// costs one entry rather than a growing list.
    unique_basename: BTreeMap<String, Option<String>>,
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
            reexport_chains: BTreeMap::new(),
            receiver_types: BTreeMap::new(),
            scoped_receiver_types: BTreeMap::new(),
            poisoned_receiver_keys: BTreeSet::new(),
            type_methods: BTreeMap::new(),
            supertypes: BTreeMap::new(),
            import_bindings: BTreeMap::new(),
            declared_types: BTreeMap::new(),
            external_imports: BTreeMap::new(),
            unindexed_local_imports: BTreeMap::new(),
            external_module_roots: BTreeMap::new(),
            local_module_roots: BTreeMap::new(),
            qualified_names: BTreeMap::new(),
            symbol_parents: BTreeMap::new(),
            go_modules: Vec::new(),
            go_modules_fresh: false,
            go_package_by_file: BTreeMap::new(),
            go_package_symbols: BTreeMap::new(),
            scope_locals: BTreeSet::new(),
            max_indexed_path_depth: 0,
            unique_basename: BTreeMap::new(),
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

    /// Supply the Go module set for the snapshot about to be — or just —
    /// indexed. Replaces any previous set, and marks it current for exactly one
    /// `index_extractions` pass (see `go_modules_fresh`).
    pub fn index_go_modules(&mut self, modules: &[GoModule]) {
        self.go_modules = modules.to_vec();
        self.go_modules_fresh = true;
    }

    /// Resolve a (file, bare name) pair to its graph identity, falling back to
    /// the bare name only when the file genuinely has no such symbol.
    fn qualified_for(&self, file: &str, name: &str) -> String {
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

    /// A binding belongs to its lexical scope. An untyped local must veto the
    /// file-wide fallback just as a typed one supplies the scoped answer.
    fn receiver_type_for(
        &self,
        file: &str,
        scope: Option<&str>,
        name: &str,
        binding: Option<&devmap_extract::model::LocalBinding>,
    ) -> Option<&String> {
        if let Some(binding) = binding {
            let declaring_scope = binding.scope.as_deref()?;
            return self
                .scoped_receiver_types
                .get(&format!("{file}:{declaring_scope}:{name}"));
        }
        if let Some(scope) = scope {
            if let Some(typed) = self
                .scoped_receiver_types
                .get(&format!("{file}:{scope}:{name}"))
            {
                return Some(typed);
            }
            if self.scope_declares_local(file, scope, name) {
                return None;
            }
        }
        self.receiver_types.get(&format!("{file}:{name}"))
    }

    /// Resolve a spelling in the nearest declaring scope, retaining the full
    /// identity. A sibling function's nested declaration is never in scope.
    fn lexical_target(
        &self,
        file: &str,
        family: LangFamily,
        caller: Option<&str>,
        name: &str,
    ) -> Option<String> {
        let hits = self.symbol_index.get(name)?;
        let mut scope = caller.unwrap_or(file);
        for _ in 0..1024 {
            let mut candidates = hits
                .iter()
                .filter(|(path, kind, _, identity)| {
                    path == file
                        && (!Self::family_needs_explicit_receiver(family)
                            || *kind != SymbolKind::Method)
                        && self
                            .symbol_parents
                            .get(&(file.to_string(), identity.to_string()))
                            .is_some_and(|parent| parent == scope)
                })
                .map(|(_, _, _, identity)| identity.as_ref());
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

    /// The leftmost segment of a dotted, scoped or slashed path.
    ///
    /// One owner for a split that `classify_unresolved` was doing inline and
    /// `index_extractions` now needs too — `metrics.counters.Inc()` is evidence
    /// about `metrics`, `std::fs::write()` about `std`, and a Go specifier
    /// `example.com/pkg/sub` about `example.com`. All three separators, because
    /// the caller does not know which language wrote the string.
    fn path_root(path: &str) -> &str {
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
    fn family_declares(&self, family: LangFamily, name: &str) -> bool {
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
    fn classify_unresolved(
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
        if let UsePosition::Type { types } = position {
            // `t *testing.T`. The extractor splits the written type into a bare
            // name for dispatch and a `TypeQualifier` sibling for provenance
            // (SC25), so by the time the bare `T` fails the ladder the qualifier
            // is the only thing that still knows where it came from. Read
            // through `declared_types`, which is where that sibling was
            // indexed, and scoped-first for the SC9 reason: a qualifier this
            // scope wrote may not speak for a same-named binding in another.
            if let Some(typed) = types {
                let qualifier = self
                    .declared_types
                    .get(&format!("{file_path}:{enclosing_symbol}:{typed}@mod"))
                    .or_else(|| self.declared_types.get(&format!("{file_path}:{typed}@mod")));
                if let Some(qualifier) = qualifier {
                    if let Some(module) = self
                        .external_imports
                        .get(file_path)
                        .and_then(|imports| imports.get(qualifier.as_str()))
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
                        .is_some_and(|imports| imports.contains_key(qualifier.as_str()))
                    {
                        return UnresolvedClass::Unresolved;
                    }
                }
            }
            // X43. The qualifier itself, when it *is* a reserved standard-library
            // root: `p: std::path::PathBuf` emits a `TypeQualifier` reference
            // named `std`, which is a module and not a type anything declares.
            if crate::builtins::is_reserved_module_root(family, callee_name) {
                return UnresolvedClass::External {
                    module: callee_name.to_string(),
                };
            }
            // A prelude type, and **nothing in this corpus declares the name**.
            // The second half is the whole guard: where a file does declare it,
            // the reference either resolved to that declaration or the resolver
            // abstained between several, and an abstention is not evidence that
            // the language owns the name. See `builtins::RUST_PRELUDE_TYPES`.
            if crate::builtins::is_prelude_type(family, callee_name)
                && !self.family_declares(family, callee_name)
            {
                return UnresolvedClass::Builtin;
            }
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

        // A bare callee that the language itself declares. Checked only without
        // a receiver: `strings.TrimSpace` is library API, and treating a
        // matching method name as a builtin would exempt real calls.
        if receiver.is_none() && crate::builtins::is_builtin(family, callee_name) {
            return UnresolvedClass::Builtin;
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
        let root = Self::path_root(receiver);

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

        // `helpers.thing()` where `helpers` came from `from . import helpers`
        // and that module is not indexed. The receiver *is* typed — by an
        // import — so this is not an uninferred receiver; it is the same index
        // gap as the bare case above.
        if local_gap.is_some_and(|imports| imports.contains_key(root)) {
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
        let root_is_a_value_here = self.scope_declares_local(file_path, enclosing_symbol, root)
            || self
                .declared_types
                .contains_key(&format!("{file_path}:{root}@type"))
            || self
                .receiver_types
                .contains_key(&format!("{file_path}:{root}"));
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
            if matches!(root, "crate" | "self" | "super")
                || self
                    .local_module_roots
                    .get(file_path)
                    .is_some_and(|roots| roots.contains(root))
            {
                return UnresolvedClass::Unresolved;
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
    fn receiver_is_property_path(receiver: &str) -> bool {
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
    fn receiver_is_module_path(receiver: &str) -> bool {
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
    fn specifier_is_repo_relative(specifier: &str) -> bool {
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
        self.reexport_chains.clear();
        self.receiver_types.clear();
        self.scoped_receiver_types.clear();
        self.poisoned_receiver_keys.clear();
        self.type_methods.clear();
        self.supertypes.clear();
        self.import_bindings.clear();
        self.declared_types.clear();
        self.external_imports.clear();
        self.unindexed_local_imports.clear();
        self.external_module_roots.clear();
        self.local_module_roots.clear();
        self.qualified_names.clear();
        self.symbol_parents.clear();
        self.go_package_by_file.clear();
        self.go_package_symbols.clear();
        self.scope_locals.clear();
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

        // Pass one establishes the complete file/symbol universe. Import
        // binding resolution must not depend on whether the importer happens
        // to precede its target in the input slice.
        for ext in extractions {
            let family = LangFamily::from_lang(&ext.language);
            let mut file_syms = Vec::new();
            for sym in &ext.symbols {
                let identity: Arc<str> = Arc::from(sym.qualified_name.as_str());
                self.symbol_index
                    .entry(sym.name.clone())
                    .or_default()
                    .push((
                        ext.file_path.clone(),
                        sym.kind,
                        family,
                        Arc::clone(&identity),
                    ));
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
                self.symbol_parents
                    .entry((ext.file_path.clone(), sym.qualified_name.clone()))
                    .or_insert_with(|| {
                        sym.parent_symbol
                            .clone()
                            .unwrap_or_else(|| ext.file_path.clone())
                    });
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
            }
        }

        // Between the passes, and necessarily so: following a barrel needs the
        // complete symbol universe pass one builds, and pass two's import
        // bindings need the chains.
        self.reexport_chains = self.compute_reexport_chains(extractions);

        // Pass two resolves aliases and receiver hints against the complete
        // universe built above.
        for ext in extractions {
            let mut file_bindings = BTreeMap::new();
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
            let mut unresolved_import = |local: String, specifier: &str| {
                let root = Self::path_root(specifier);
                if Self::specifier_is_repo_relative(specifier) {
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
                                terminal.rsplit_once("::").map(|(file, _)| file.to_string())
                            });
                        let resolved = if declares_name {
                            direct
                        } else if let Some(file) = via_reexport {
                            Some(file)
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
                            unresolved_import(local.to_string(), &imp.module_specifier);
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
                        unresolved_import(local, &imp.module_specifier);
                        continue;
                    };
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
            if !file_bindings.is_empty() {
                self.import_bindings
                    .insert(ext.file_path.clone(), file_bindings);
            }

            let family = LangFamily::from_lang(&ext.language);
            // X42. The supertype table, read from the heritage references the
            // extractor emits. `enclosing_symbol` on one of these is the
            // *declaring* type — that is what makes the `Extends` edge have two
            // endpoints — so its tail is the subtype's name and the reference's
            // own name is the supertype's.
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
                if supertype.is_empty() || supertype == subtype {
                    continue;
                }
                self.supertypes
                    .entry((family, subtype.to_string()))
                    .or_default()
                    .insert(supertype.to_string());
            }
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
                    .filter(|(_, kind, candidate_family, _)| {
                        family.admits(*candidate_family)
                            && matches!(kind, SymbolKind::Class | SymbolKind::Struct)
                    })
                    .map(|(path, kind, _, _)| (path, kind))
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
    const REEXPORT_CHAIN_MAX_DEPTH: usize = 8;

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
    fn compute_reexport_chains(&self, extractions: &[Extraction]) -> BTreeMap<String, String> {
        // One hop per re-export, keyed by the re-exporting file's own name for
        // the symbol.
        let mut hops: BTreeMap<String, (String, String)> = BTreeMap::new();
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
        }

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
        chains
    }

    pub fn resolve_all(&self, extractions: &[Extraction]) -> ResolutionResult {
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
        // Worth doing because this phase does not shrink on an incremental
        // build: resolution deliberately covers the whole tree on every changed
        // build so that liveness and community detection mean the same thing on
        // both paths (see the comment in `devmap-cli`'s build command), which
        // makes it the phase whose cost grows straight-line with repository
        // size while extraction is cached away.
        type FileResolution = (
            Vec<ResolvedEdge>,
            Vec<UnresolvedReference>,
            BTreeMap<String, BTreeSet<String>>,
        );
        let per_file: Vec<FileResolution> = extractions
            .par_iter()
            .map(|ext| {
                let mut edges: Vec<ResolvedEdge> = Vec::new();
                let mut unresolved: Vec<UnresolvedReference> = Vec::new();
                let mut package_groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
                let family = LangFamily::from_lang(&ext.language);

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
                    // R5. A relative specifier that named no indexed file is an
                    // index gap: the `Imports` edge that should exist is
                    // missing, and emitting only on success made that
                    // indistinguishable from "this file imports nothing".
                    // Absolute specifiers are not recorded — `import "strings"`
                    // resolving to nothing is the expected case and carries no
                    // information.
                    if targets.is_empty() && Self::specifier_is_repo_relative(&imp.module_specifier)
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
                        });
                    }
                    let edge_targets = if ext.language == "go" {
                        self.go_import_edge_targets(&targets)
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

                // Resolve calls using Resolution Ladder (SameFile -> ImportScoped -> UniqueGlobal -> AmbiguousGlobal)
                for call in &ext.calls {
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
                    if call.receiver_expr.is_none()
                        && ext.local_binding_at(call.span.start_byte, &call.callee_name).is_some() {
                        resolution = Some(Arc::new(Resolution::Unresolved {
                            reason: "the callee is a local binding whose value is not known".to_string(),
                        }));
                    }

                    // 1. Receiver-based resolution (SameFile / Constructor tracking N6)
                    if let Some(recv) = &call.receiver_expr {
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
                        if let Some(class_type) = self.receiver_type_for(&ext.file_path, call.caller_symbol.as_deref(), recv, ext.local_binding_at(call.span.start_byte, recv))
                            .filter(|_| family.admits(family))
                        {
                            let key = (family, class_type.clone(), call.callee_name.clone());
                            if let Some(hits) = self.type_methods.get(&key) {
                                if hits.len() == 1 {
                                    let (target_f, target_symbol) = &hits[0];
                                    resolution = Some(Arc::new(Resolution::ReceiverType {
                                        target_symbol: target_symbol.clone(),
                                        target_file: target_f.clone(),
                                        receiver_type: class_type.clone(),
                                    }));
                                }
                            }
                        }
                    }

                    // Whether the receiver is the enclosing object itself.
                    // Several rungs below turn on it, and it was previously
                    // recomputed at each of them.
                    let implicit_receiver = call
                        .receiver_expr
                        .as_deref()
                        .is_some_and(Self::receiver_is_self);

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
                            if let Some((target_file, target_symbol, receiver_type)) = self
                                .implicit_receiver_target(
                                    &ext.file_path,
                                    family,
                                    caller,
                                    &call.callee_name,
                                )
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
                        if let Some(bindings) = self.import_bindings.get(&ext.file_path) {
                            if let Some((target_f, target_sym)) = bindings.get(&call.callee_name) {
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
                                        reason: "the named import has no unambiguous declaration in its module".to_string(),
                                    }));
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
                            if let Some(bindings) = self.import_bindings.get(&ext.file_path) {
                                if let Some((target_f, _)) = bindings.get(&recv) {
                                    if let Some((resolved_file, resolved_sym)) =
                                        self.lookup_in_package(target_f, &method)
                                    {
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
                            if let Some(target_symbol) = self.lexical_target(&ext.file_path, family, call.caller_symbol.as_deref(), &call.callee_name) {
                                resolution = Some(Arc::new(Resolution::SameFile {
                                    target_symbol, target_file: ext.file_path.clone(),
                                }));
                            }
                        } else if self.bare_name_is_in_scope(&ext.file_path, family, call.caller_symbol.as_deref(), &call.callee_name)
                            && self.symbol_kind_in(&ext.file_path, &call.callee_name) == Some(SymbolKind::Method) {
                            resolution = Some(Arc::new(Resolution::SameFile {
                                target_symbol: self.qualified_for(&ext.file_path, &call.callee_name), target_file: ext.file_path.clone(),
                            }));
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
                            let key = (family, recv.clone(), call.callee_name.clone());
                            if let Some(hits) = self.type_methods.get(&key) {
                                let corroborated = hits.len() == 1
                                    && (hits[0].0 == ext.file_path
                                        || self
                                            .import_bindings
                                            .get(&ext.file_path)
                                            .is_some_and(|bindings| bindings.contains_key(recv)));
                                if corroborated {
                                    let (target_f, target_symbol) = &hits[0];
                                    resolution = Some(Arc::new(Resolution::ReceiverType {
                                        target_symbol: target_symbol.clone(),
                                        target_file: target_f.clone(),
                                        receiver_type: recv.clone(),
                                    }));
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
                        }
                    }

                    // A superclass receiver excludes the overriding declaration.
                    // Without a proven base target, bare-name widening fabricates
                    // a self-call. Preserve the unresolved site instead.
                    if resolution.is_none() && call.receiver_expr.as_deref().is_some_and(|r| matches!(r, "super" | "super()" | "base")) {
                        resolution = Some(Arc::new(Resolution::Unresolved {
                            reason: "superclass dispatch requires a proven base declaration".to_string(),
                        }));
                    }

                    // 3. Global lookup (UniqueGlobal vs AmbiguousGlobal - G5, G3)
                    if resolution.is_none() {
                        if let Some(hits) = self.symbol_index.get(&call.callee_name) {
                            // The same scope test rung 2c applies, for the same
                            // reason: a bare `run()` cannot reach a method of
                            // some class, and letting the global rung do what
                            // 2c was stopped from doing would move the
                            // fabricated edge rather than remove it — the
                            // fabricated caller still shields the method from
                            // the dead-code pass, only at HIGH instead of
                            // DETERMINISTIC.
                            //
                            // Cross-file, the sibling shape cannot apply at all:
                            // an implicit receiver reaches the enclosing type,
                            // which is a different symbol in a different file.
                            // So the test is a plain "declared at file level",
                            // and it is applied only to the families whose
                            // scoping rules are stated in `bare_name_is_in_scope`
                            // — a C++ method defined in a `.cpp` and declared in
                            // its header is exactly the cross-file sibling this
                            // would otherwise sever.
                            let bare_call = call.receiver_expr.is_none();
                            let family_hits: Vec<_> = hits
                                .iter()
                                .filter(|(path, kind, candidate_family, identity)| {
                                    family.admits(*candidate_family)
                                        // X42. `self.m()` names a *member* of
                                        // the receiver's type. A module-level
                                        // function of the same name is not one,
                                        // so binding to it is a wrong edge in
                                        // both directions: the call gets a
                                        // target it cannot reach, and the free
                                        // function gets a caller it does not
                                        // have — which shields it from the
                                        // dead-code pass. Measured shape:
                                        // `self.run()` fanning out to both
                                        // `Service.run` and an unrelated
                                        // `other.py::run`.
                                        && (!implicit_receiver
                                            || matches!(kind, SymbolKind::Method))
                                        && (*candidate_family != LangFamily::Go
                                            || Self::go_symbol_visible_from(
                                                &ext.file_path,
                                                path,
                                                &call.callee_name,
                                            ))
                                        && (!bare_call
                                            || !Self::family_needs_explicit_receiver(family)
                                            || self.declared_at_file_level(path, identity))
                                })
                                .collect();
                            if family_hits.len() == 1 {
                                let (target_f, _, _, target_identity) = family_hits[0];
                                // G3: Python stdlib-name guard inside UniqueGlobal rung only
                                let is_python_stdlib_guard = family == LangFamily::Python
                                    && matches!(
                                        call.callee_name.as_str(),
                                        "open" | "dir" | "print" | "type" | "id" | "len"
                                    )
                                    && target_f != &ext.file_path;

                                if !is_python_stdlib_guard {
                                    resolution = Some(Arc::new(Resolution::UniqueGlobal {
                                        target_symbol: target_identity.to_string(),
                                        target_file: target_f.clone(),
                                        family,
                                    }));
                                }
                            } else if family_hits.len() > 1 {
                                // G5: Multi-candidate pick MUST NOT emit Extracted / HIGH confidence
                                let mut candidates: Vec<(String, String)> = family_hits
                                    .iter()
                                    .map(|(f, _, _, identity)| ((*f).clone(), identity.to_string()))
                                    .collect();
                                // R4. `symbol_index` values are in input-slice
                                // order, and that order used to flow straight
                                // into `candidates` — a field of every emitted
                                // edge, a key in the sort comparator and a term
                                // in the dedup predicate. Reversing the input
                                // slice reversed every candidate list. Sorting
                                // here is also what makes the fan-out cap below
                                // pick the same subset on every run.
                                candidates.sort();
                                candidates.dedup();
                                resolution = Some(Arc::new(Resolution::AmbiguousGlobal {
                                    candidates,
                                    family,
                                }));
                            }
                        }
                    }

                    let caller_sym = call
                        .caller_symbol
                        .clone()
                        .unwrap_or_else(|| ext.file_path.clone());
                    // Emission is driven entirely by the resolution, and
                    // anything that emits no edge falls through to the ledger —
                    // so a rung that ever produced evidence naming no target
                    // would be *reported*, not silently dropped and not a panic.
                    let mut emitted = false;
                    if let Some(resolution) = &resolution {
                        match resolution.as_ref() {
                            Resolution::AmbiguousGlobal { candidates, .. } => {
                                // R7. One ambiguous call site emits one edge per
                                // candidate. Uncapped, a single call to a name
                                // with 200 same-family declarations became 200
                                // persisted rows from one call site.
                                //
                                // The cap is on *emission* only: `candidates`
                                // still carries the complete list, and every
                                // edge of a truncated site carries both numbers,
                                // so a capped sample is never presented as
                                // complete coverage. The subset is the first
                                // `AMBIGUOUS_FANOUT_CAP` of a sorted list, so it
                                // is the same subset on every run.
                                //
                                // Known consequence, not an oversight: liveness
                                // reads these edges to downgrade a symbol whose
                                // only callers are ambiguous, so a candidate
                                // past the cap loses that downgrade. `details`
                                // is what says so.
                                let total = candidates.len();
                                let details = (total > AMBIGUOUS_FANOUT_CAP).then(|| {
                                    format!(
                                        "ambiguous fan-out truncated: \
                                         {AMBIGUOUS_FANOUT_CAP} of {total} candidates emitted"
                                    )
                                });
                                for (target_f, target_sym) in
                                    candidates.iter().take(AMBIGUOUS_FANOUT_CAP)
                                {
                                    edges.push(ResolvedEdge::resolved(
                                        ext.file_path.clone(),
                                        target_f.clone(),
                                        caller_sym.clone(),
                                        self.qualified_for(target_f, target_sym),
                                        EdgeKind::Calls,
                                        Arc::clone(resolution),
                                        details.clone(),
                                    ));
                                    emitted = true;
                                }
                            }
                            named => {
                                if let Some((target_file, target_symbol)) = named.target() {
                                    edges.push(ResolvedEdge::resolved(
                                        ext.file_path.clone(),
                                        target_file.to_string(),
                                        caller_sym.clone(),
                                        self.qualified_for(target_file, target_symbol),
                                        EdgeKind::Calls,
                                        Arc::clone(resolution),
                                        None,
                                    ));
                                    emitted = true;
                                }
                            }
                        }
                    }
                    if !emitted {
                        // D17 / R5: a call the ladder could not resolve is recorded,
                        // never dropped. Silence here is indistinguishable from
                        // "there was no call", which is the failure R5 forbids.
                        let class = self.classify_unresolved(
                            &ext.file_path,
                            family,
                            &call.callee_name,
                            call.receiver_expr.as_deref(),
                            &caller_sym,
                            UsePosition::Value,
                        );
                        unresolved.push(UnresolvedReference {
                            source_file: ext.file_path.clone(),
                            source_symbol: caller_sym,
                            callee_name: call.callee_name.clone(),
                            kind: UnresolvedKind::Call,
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
                        }
                    } else {
                        UsePosition::Value
                    };
                    let class = self.classify_unresolved(
                        &ext.file_path,
                        family,
                        name,
                        reference.receiver_expr.as_deref(),
                        &source_symbol,
                        position,
                    );
                    unresolved.push(UnresolvedReference {
                        source_file: ext.file_path.clone(),
                        source_symbol,
                        callee_name: name.to_string(),
                        kind: UnresolvedKind::Reference,
                        resolution: Resolution::Unresolved {
                            reason: format!(
                                "no resolution rung matched {:?} reference {:?} in {} family {:?}",
                                reference.kind, name, ext.file_path, family
                            ),
                        },
                        class,
                        receiver: reference.receiver_expr.clone(),
                    });
                }

                // Resolve routes
                for route in &ext.routes {
                    // An anonymous handler — an Express arrow function — has no
                    // name to resolve, so there is nothing to bind and nothing
                    // to report. Every other route names a handler, and either
                    // binds it or fails to.
                    if route.handler_name.is_empty() {
                        continue;
                    }
                    let hits = self.symbol_index.get(&route.handler_name);
                    // The route's node identity, not a bare "VERB /path"
                    // label. `ExtractedRoute::node_id` owns the shape so the
                    // graph export can emit a node under the same id; an edge
                    // whose source names no node leaves every route consumer
                    // reading an empty graph.
                    let route_source = route.node_id(&ext.file_path);
                    let mut candidate_count = 0usize;
                    let route_target = hits.and_then(|hits| {
                        let same_file: Vec<_> = hits
                            .iter()
                            .filter(|(path, _, _, _)| path == &ext.file_path)
                            .collect();
                        if same_file.len() == 1 {
                            let (target_f, _, _, _) = same_file[0];
                            return Some((
                                target_f.clone(),
                                Resolution::SameFile {
                                    target_symbol: route.handler_name.clone(),
                                    target_file: target_f.clone(),
                                },
                            ));
                        }
                        if let Some((target_f, target_symbol)) = self
                            .import_bindings
                            .get(&ext.file_path)
                            .and_then(|bindings| bindings.get(&route.handler_name))
                        {
                            return Some((
                                target_f.clone(),
                                Resolution::ImportScoped {
                                    target_symbol: target_symbol.clone(),
                                    target_file: target_f.clone(),
                                    imported_from: route.handler_name.clone(),
                                },
                            ));
                        }
                        let family_hits: Vec<_> = hits
                            .iter()
                            .filter(|(path, _, candidate_family, _)| {
                                family.admits(*candidate_family)
                                    && (*candidate_family != LangFamily::Go
                                        || Self::go_symbol_visible_from(
                                            &ext.file_path,
                                            path,
                                            &route.handler_name,
                                        ))
                            })
                            .collect();
                        candidate_count = family_hits.len();
                        (family_hits.len() == 1).then(|| {
                            let (target_f, _, _, _) = family_hits[0];
                            (
                                target_f.clone(),
                                Resolution::UniqueGlobal {
                                    target_symbol: route.handler_name.clone(),
                                    target_file: target_f.clone(),
                                    family,
                                },
                            )
                        })
                    });
                    let Some((target_f, resolution)) = route_target else {
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
                        });
                        continue;
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
                (edges, unresolved, package_groups)
            })
            .collect();

        let mut edges: Vec<ResolvedEdge> = Vec::new();
        let mut unresolved: Vec<UnresolvedReference> = Vec::new();
        let mut package_groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (file_edges, file_unresolved, file_packages) in per_file {
            edges.extend(file_edges);
            unresolved.extend(file_unresolved);
            for (package, files) in file_packages {
                package_groups.entry(package).or_default().extend(files);
            }
        }

        // G20: Add Go synthetic package star edges
        for (pkg_node, files) in package_groups {
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
                    Some("Go package star topology".to_string()),
                ));
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
            reexport_chains: self.reexport_chains.clone(),
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

    /// Delegates to `importpath::normalize_rel`, which is the single owner.
    ///
    /// The two implementations were identical when the table-driven ladder
    /// landed, and two identical copies of path normalisation is how a resolver
    /// starts answering two different questions about one `..`.
    fn normalize_rel(base_dir: &str, spec: &str) -> String {
        crate::importpath::normalize_rel(base_dir, spec)
    }

    /// The directories a Rust module's children can live in, best first.
    ///
    /// `src/lib.rs` and `src/deep/mod.rs` keep their children in their own
    /// directory; `src/deep/leaf.rs` keeps them in `src/deep/leaf/`. The second
    /// is the rule the module system states and the one this resolver never
    /// applied — it used the file's directory for both, so `super::sibling`
    /// written in `src/deep/leaf.rs` probed `src/sibling.rs` when the statement
    /// names `src/deep/sibling.rs`.
    ///
    /// Both readings are returned rather than one chosen, because "is this file
    /// a crate root" is not decidable from its path: `lib.rs` and `main.rs` are,
    /// and so is every file directly under `tests/`, `benches/`, `examples/` and
    /// `src/bin/` — a list that goes stale against Cargo's auto-discovery and
    /// against a hand-written `[[test]] path = …`. The module-system reading is
    /// tried first and the indexed file universe decides; neither can invent a
    /// file that is not there, so the worst case is the answer this rung gave
    /// before.
    fn rust_module_dirs(file: &str) -> Vec<String> {
        let dir = Self::parent_dir(file);
        let stem = file
            .rsplit('/')
            .next()
            .unwrap_or(file)
            .strip_suffix(".rs")
            .unwrap_or_default();
        // A directory module's file: its children are its siblings, not its
        // descendants.
        if stem.is_empty() || matches!(stem, "mod" | "lib" | "main") {
            return vec![dir];
        }
        let nested = if dir.is_empty() {
            stem.to_string()
        } else {
            format!("{dir}/{stem}")
        };
        if nested == dir {
            vec![dir]
        } else {
            vec![nested, dir]
        }
    }

    /// The source root of the crate `file` belongs to — what `crate::` is
    /// relative to.
    ///
    /// The longest ancestor path whose last component is `src`. Cargo requires
    /// a crate's root to be `src/lib.rs` or `src/main.rs` (or a path named in
    /// the manifest), so the innermost `src` above a file is its crate's root
    /// in every layout this resolver can be pointed at, single-crate and
    /// workspace alike.
    ///
    /// Falls back to the literal `src`, which is what this rung probed
    /// unconditionally before: a file with no `src` ancestor — `build.rs`, a
    /// `tests/` integration crate, a bare script — keeps exactly the behaviour
    /// it had rather than gaining a guess.
    fn rust_crate_src_root(file: &str) -> String {
        let mut components: Vec<&str> = file.split('/').collect();
        components.pop();
        while let Some(last) = components.last() {
            if *last == "src" {
                return components.join("/");
            }
            components.pop();
        }
        "src".to_string()
    }

    fn import_local_name(lang: &str, specifier: &str) -> String {
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
    fn bare_name_is_in_scope(
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
    fn family_needs_explicit_receiver(family: LangFamily) -> bool {
        matches!(
            family,
            LangFamily::Python | LangFamily::JsTs | LangFamily::Go | LangFamily::Rust
        )
    }

    /// Whether `file` declares `bare_name` at file level rather than inside a
    /// type or another callable.
    fn declared_at_file_level(&self, file: &str, bare_name: &str) -> bool {
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
    fn receiver_is_self(receiver: &str) -> bool {
        matches!(receiver, "self" | "this" | "cls" | "$this" | "me" | "Self")
    }

    /// How far the supertype walk may climb.
    ///
    /// A bound rather than a cycle check alone: `class A(B)` / `class B(A)` is
    /// not the only pathology, and a generated hierarchy thousands deep would
    /// cost a lookup per level per call site. Eight covers every hierarchy this
    /// resolver has been pointed at; past it the rung abstains, which loses an
    /// edge and invents nothing.
    const HERITAGE_WALK_MAX_DEPTH: usize = 8;

    /// The type `symbol` is declared by, or `None` when it is declared at file
    /// level.
    ///
    /// Read from `symbol_parents` — the extractor's own answer — and reduced to
    /// the bare name the same way `type_methods` reduces `parent_symbol` when
    /// it is built, so the two cannot key differently. Deliberately **not** a
    /// split of the method's qualified name on `.`: `Outer.Inner.method` and a
    /// module-level `a.b` are the same string to that rule and different facts.
    fn declaring_type_of(&self, file: &str, symbol: &str) -> Option<&str> {
        self.symbol_parents
            .get(&(file.to_string(), symbol.to_string()))
            .filter(|parent| *parent != file)
            .and_then(|parent| parent.rsplit("::").next())
            .filter(|type_name| !type_name.is_empty())
    }

    /// Whether exactly one indexed file declares a type of this name.
    ///
    /// The identifiability test for the supertype walk. `type_methods` and
    /// `supertypes` are both flat by bare type name, so a chain that passes
    /// through a name two files declare is a chain this resolver cannot follow
    /// — and following it anyway would dispatch on whichever declaration the
    /// merge happened to produce.
    fn type_name_is_identifiable(&self, family: LangFamily, type_name: &str) -> bool {
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
    /// it, and a type name two files declare. Returns
    /// `(target file, target symbol, the type that declared it)`.
    fn implicit_receiver_target(
        &self,
        file: &str,
        family: LangFamily,
        caller_symbol: &str,
        method: &str,
    ) -> Option<(String, String, String)> {
        let enclosing = self.declaring_type_of(file, caller_symbol)?.to_string();
        // The caller's declaring type is an exact identity even when another
        // file declares a namesake. Only inherited lookup needs a global name.
        if let Some(hits) = self
            .type_methods
            .get(&(family, enclosing.clone(), method.to_string()))
        {
            let local: Vec<_> = hits.iter().filter(|(path, _)| path == file).collect();
            if local.len() == 1 {
                return Some((local[0].0.clone(), local[0].1.clone(), enclosing));
            }
            if local.len() > 1 {
                return None;
            }
        }
        let mut frontier = vec![enclosing];
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
                // Two supertypes at one level declare the name. The language's
                // own MRO might pick one; this resolver has no MRO, and a guess
                // at DETERMINISTIC is the one answer it must not give.
                _ => return None,
            }
            if next.is_empty() {
                return None;
            }
            frontier = next;
        }
        None
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
    fn same_package_target(
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

    fn lookup_in_package(&self, file: &str, name: &str) -> Option<(String, String)> {
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
        // A JVM wildcard import names a package, and a package is every file in
        // one directory. `import com.foo.*;` and `import foo.bar._` really do
        // depend on all of them, so all of them get an edge.
        //
        // Not capped, deliberately. `AMBIGUOUS_FANOUT_CAP` bounds an ambiguous
        // *guess* — N candidates of which at most one is right — where emitting
        // all N is how a resolver launders uncertainty into volume. These N are
        // all correct, and the consumer is `unwired_candidates`: truncating the
        // list would leave the files past the cut falsely reported as imported
        // by nothing, turning a bound into a false finding. The bound here is
        // the repository's own size, which already bounds everything else.
        if let Some(package) = specifier.strip_suffix(".*") {
            return self.resolve_package_wildcard(lang, package);
        }
        // A Terraform `module` source names a directory of `.tf` files, for the
        // same reason and with the same answer.
        if lang == "hcl" {
            return self.resolve_terraform_module(current_file, specifier);
        }
        self.resolve_import_path(current_file, lang, specifier)
            .into_iter()
            .collect()
    }

    /// Every indexed file in the directory a JVM package name maps to.
    fn resolve_package_wildcard(&self, lang: &str, package: &str) -> Vec<String> {
        let Some(rule) = crate::importpath::rule_for(lang) else {
            return Vec::new();
        };
        let relative = package.replace(rule.separator, "/");
        if relative.is_empty() {
            return Vec::new();
        }
        let mut directories: Vec<String> = Vec::new();
        for root in rule.roots {
            let dir = crate::importpath::normalize_rel(root, &relative);
            // The repository root is never a package. Without this an
            // unqualified wildcard would link its importer to every file in the
            // corpus — the one shape of this expansion that is not merely large
            // but wrong.
            if dir.is_empty() || directories.contains(&dir) {
                continue;
            }
            directories.push(dir);
        }
        for directory in directories {
            let files = self.files_in_dir_with_extensions(&directory, rule.extensions);
            if !files.is_empty() {
                return files;
            }
        }
        Vec::new()
    }

    /// Every indexed `.tf` file in the directory a Terraform `source` names.
    fn resolve_terraform_module(&self, current_file: &str, specifier: &str) -> Vec<String> {
        // Only a local source is a path. `hashicorp/consul/aws` is a registry
        // address and `git::https://…` is a remote; both name something outside
        // the repository and must not be resolved against a same-named local
        // directory.
        if !(specifier.starts_with("./") || specifier.starts_with("../")) {
            return Vec::new();
        }
        let directory =
            crate::importpath::normalize_rel(&Self::parent_dir(current_file), specifier);
        if directory.is_empty() {
            return Vec::new();
        }
        self.files_in_dir_with_extensions(&directory, &[".tf"])
    }

    /// Indexed files sitting directly in `dir` whose name ends in one of
    /// `extensions`. Sorted, so an expansion is deterministic across runs.
    fn files_in_dir_with_extensions(&self, dir: &str, extensions: &[&str]) -> Vec<String> {
        let dir = dir.trim_end_matches('/');
        let mut files: Vec<String> = self
            .file_symbols
            .keys()
            .filter(|path| {
                Self::parent_dir(path) == dir
                    && extensions
                        .iter()
                        .any(|extension| !extension.is_empty() && path.ends_with(extension))
            })
            .cloned()
            .collect();
        files.sort();
        files
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
        if let Some(class_type) = self.receiver_type_for(
            &ext.file_path,
            reference.enclosing_symbol.as_deref(),
            receiver,
            ext.local_binding_at(reference.span.start_byte, receiver),
        ) {
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
            let (target_file, target_symbol, receiver_type) =
                reference.enclosing_symbol.as_deref().and_then(|caller| {
                    self.implicit_receiver_target(&ext.file_path, family, caller, name)
                })?;
            return Some(self.reference_edge(
                ext,
                &target_file,
                name,
                reference,
                Resolution::ReceiverType {
                    target_symbol,
                    target_file: target_file.clone(),
                    receiver_type,
                },
            ));
        }

        if ext
            .local_binding_at(reference.span.start_byte, receiver)
            .is_some()
        {
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
    fn type_qualifier_of(&self, file: &str, reference: &ExtractedReference) -> Option<&str> {
        if reference.kind != ReferenceKind::Type {
            return None;
        }
        let typed = reference.assigned_to.as_deref()?;
        reference
            .enclosing_symbol
            .as_deref()
            .and_then(|scope| {
                self.declared_types
                    .get(&format!("{file}:{scope}:{typed}@mod"))
            })
            .or_else(|| self.declared_types.get(&format!("{file}:{typed}@mod")))
            .map(String::as_str)
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
    fn rust_self_type(&self, file: &str, scope: Option<&str>) -> Option<&str> {
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
    fn qualifier_is_bound(&self, file: &str, qualifier: &str) -> bool {
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
        // below that reads the bare name alone. The one that still runs is the
        // global tier, which is HIGH or SPECULATIVE and states its uncertainty
        // — and which, for an implicit receiver, admits only members.
        let receiver = reference.receiver_expr.as_deref();
        if let Some(receiver) = receiver {
            if let Some(edge) =
                self.resolve_member_reference(ext, family, reference, receiver, name)
            {
                return Some(edge);
            }
        }
        let implicit_receiver = receiver.is_some_and(Self::receiver_is_self);

        if receiver.is_none() {
            if let Some(identity) = self.lexical_target(
                &ext.file_path,
                family,
                reference.enclosing_symbol.as_deref(),
                name,
            ) {
                if self
                    .symbol_kind_in(&ext.file_path, &identity)
                    .is_some_and(|kind| !prefer_types || is_type(kind))
                {
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
            }
        }

        // Name identifiers unique-global to a unique function in another file.
        // That binds `except Exception as e` / `print(e)` / `for _, segment`
        // to a unique `def e` / `func segment` across the language family.
        // Same-file and import-scoped remain; Calls still unique-global.
        if matches!(reference.kind, ReferenceKind::Name) {
            return None;
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
                            target_symbol: target_identity.to_string(),
                            target_file: target_f.clone(),
                            family,
                        },
                    ));
                }
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
    fn symbol_kind_in(&self, file: &str, name: &str) -> Option<SymbolKind> {
        self.symbol_index.get(name).and_then(|hits| {
            let file_hits: Vec<_> = hits
                .iter()
                .filter(|(path, _, _, _)| path == file)
                .map(|(_, kind, _, _)| *kind)
                .collect();
            (file_hits.len() == 1).then(|| file_hits[0])
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
    fn trim_to_indexed_depth(&self, parts: &mut Vec<&str>) {
        // `+ 1` because `crate::a::b` probes `src/a/b.rs`, which has one more
        // path component than the specifier has segments. Deliberately
        // generous: this must never cut a rung that could have matched.
        let ceiling = self.max_indexed_path_depth.saturating_add(1);
        if parts.len() > ceiling {
            parts.truncate(ceiling);
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

        // The family, not a hand-listed set of grammar keys. The list this
        // replaces omitted `jsx` (which `from_lang` has always treated as
        // JS/TS) and every embedded-script host, so a `.svelte` file's
        // `import { helper } from "./helpers"` was classified as an *external*
        // import — the corpus was told a local file came from outside it.
        if LangFamily::from_lang(lang) == LangFamily::JsTs && clean_spec.starts_with('.') {
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

        // X41. `use` now arrives here as a real module path, so the three
        // module roots it can name have to be answerable **bare** as well as
        // prefixed: `use super::*;` inside `mod tests { … }` is rewritten by
        // the extractor to the bare `self`, naming the file it is written in.
        if lang == "rust" {
            if clean_spec == "self" {
                return self
                    .file_symbols
                    .contains_key(current_file)
                    .then(|| current_file.to_string());
            }
            if clean_spec == "crate" {
                let root = Self::rust_crate_src_root(current_file);
                for candidate in [format!("{root}/lib.rs"), format!("{root}/main.rs")] {
                    if self.file_symbols.contains_key(&candidate) {
                        return Some(candidate);
                    }
                }
            }
            // The parent module *as a file*: `src/deep/leaf.rs` is `deep::leaf`,
            // so its `super` is `deep`, which lives in `src/deep/mod.rs` or
            // `src/deep.rs`.
            if clean_spec == "super" {
                for module_dir in Self::rust_module_dirs(current_file)
                    .iter()
                    .map(|module_dir| Self::parent_dir(module_dir))
                {
                    for candidate in [
                        format!("{module_dir}/mod.rs"),
                        format!("{module_dir}.rs"),
                        format!("{module_dir}/lib.rs"),
                        format!("{module_dir}/main.rs"),
                    ] {
                        if self.file_symbols.contains_key(&candidate) {
                            return Some(candidate);
                        }
                    }
                }
            }
        }

        if lang == "rust" && clean_spec.starts_with("crate::") {
            let crate_tail = clean_spec.strip_prefix("crate::")?;
            // The **crate's** source root, not the repository's. `crate::` is
            // relative to the crate the file belongs to, and a workspace puts
            // that at `crates/<name>/src`, so probing a literal `src/…` from
            // the tree root answered nothing for every workspace member. That
            // is why this repository produced no `Imports` edge at all for the
            // hundreds of `use crate::…` lines in its own kernel.
            let root = Self::rust_crate_src_root(current_file);
            let mut parts: Vec<&str> = crate_tail.split("::").collect();
            self.trim_to_indexed_depth(&mut parts);
            while !parts.is_empty() {
                let rust_path = parts.join("/");
                let candidates = [
                    format!("{root}/{rust_path}.rs"),
                    format!("{root}/{rust_path}/mod.rs"),
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
            let mut tail = clean_spec;
            let mut hops = 0usize;
            if let Some(stripped) = tail.strip_prefix("self::") {
                tail = stripped;
            } else {
                while let Some(stripped) = tail.strip_prefix("super::") {
                    hops += 1;
                    tail = stripped;
                }
            }
            let mut parts: Vec<&str> = tail.split("::").collect();
            self.trim_to_indexed_depth(&mut parts);
            // Both readings of "where does this module keep its children", best
            // first — see `rust_module_dirs`. Each `super::` walks one directory
            // up from whichever base is being tried.
            let bases: Vec<String> = Self::rust_module_dirs(current_file)
                .into_iter()
                .map(|mut module_dir| {
                    for _ in 0..hops {
                        module_dir = Self::parent_dir(&module_dir);
                    }
                    module_dir
                })
                .collect();
            while !parts.is_empty() {
                let module_path = parts.join("/");
                for module_dir in &bases {
                    let base = Self::normalize_rel(module_dir, &module_path);
                    // `base` itself, before the two conventional forms: a
                    // `#[path = "generated/tables.rs"] mod tables;` reaches this
                    // rung as `self::generated/tables.rs`, and the extension is
                    // already on it. Appending `.rs` to a path that has one probes
                    // `tables.rs.rs` and finds nothing — which is precisely the
                    // case the attribute exists to declare, so failing it would
                    // leave the real file reported as imported by nothing.
                    for candidate in [base.clone(), format!("{base}.rs"), format!("{base}/mod.rs")]
                    {
                        if self.file_symbols.contains_key(&candidate) {
                            return Some(candidate);
                        }
                    }
                }
                parts.pop();
            }
        }

        // W0.3 move 2: the thirteen languages whose import extraction landed
        // with this rung. Table-driven rather than thirteen more blocks above,
        // because they differ only in separator, extensions and build roots —
        // see `importpath`, which owns the table and the candidate order.
        if let Some(rule) = crate::importpath::rule_for(lang) {
            let spec = crate::importpath::strip_dart_package_prefix(clean_spec)
                .filter(|_| lang == "dart")
                .unwrap_or(clean_spec);
            let candidates = crate::importpath::candidates(rule, &dir, spec);
            for candidate in &candidates {
                if self.file_symbols.contains_key(candidate) {
                    return Some(candidate.clone());
                }
            }
            // Last rung: a candidate's *basename* naming exactly one indexed
            // file. This is what makes `#include "util.h"` from a target built
            // with `-Isrc/core` resolve without the resolver knowing the build
            // system's include path, and `-include_lib("kernel/include/x.hrl")`
            // resolve without knowing where the application was unpacked.
            //
            // Guarded on uniqueness, not on plausibility. Two files named
            // `util.h` mean this rung abstains — which is the same answer the
            // ambiguity ladder gives elsewhere in this resolver, and the reason
            // it can be trusted at all: it never picks a winner.
            if !crate::importpath::is_relative_specifier(spec) {
                for candidate in &candidates {
                    let basename = candidate.rsplit('/').next().unwrap_or(candidate);
                    if let Some(Some(unique)) = self.unique_basename.get(basename) {
                        return Some(unique.clone());
                    }
                }
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
    #[cfg(feature = "parse")]
    use super::*;
    #[cfg(feature = "parse")]
    use devmap_extract::extract_file;

    #[test]
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
    use super::*;
    #[cfg(feature = "parse")]
    use devmap_extract::extract_file;

    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
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
            Some(("pkg/a.go".to_string(), "pkg/a.go::Helper".to_string())),
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
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
    fn go_package_lookup_prefers_the_querying_file() {
        let (resolver, _) = indexed(&[
            ("pkg/a.go", "package pkg\nfunc Same() {}\n"),
            ("pkg/b.go", "package pkg\nfunc Same() {}\n"),
        ]);
        assert_eq!(
            resolver.lookup_in_package("pkg/a.go", "Same"),
            Some(("pkg/a.go".to_string(), "pkg/a.go::Same".to_string())),
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
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
    use super::*;
    #[cfg(feature = "parse")]
    use devmap_extract::extract_file;

    #[cfg(feature = "parse")]
    fn resolve(files: &[(&str, &str)]) -> ResolutionResult {
        let extractions: Vec<_> = files
            .iter()
            .map(|(path, source)| extract_file(path, source))
            .collect();
        let mut resolver = Resolver::new();
        resolver.index_extractions(&extractions);
        resolver.resolve_all(&extractions)
    }

    #[cfg(feature = "parse")]
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

    #[cfg(feature = "parse")]
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

    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
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
    ///
    /// Both endpoints are node identities — `file::name`, and
    /// `file::VERB path` for the route, from `ExtractedRoute::node_id`. They
    /// used to be a bare `"VERB path"` and a bare handler name, neither of
    /// which names a node, so the graph export emitted this edge dangling at
    /// both ends and every route consumer read an empty graph.
    #[test]
    #[cfg(feature = "parse")]
    fn a_route_binds_only_to_an_unambiguous_same_family_handler() {
        let route_file = (
            "srv.rs",
            "fn app() -> Router { Router::new().route(\"/items\", get(list_items)) }\n",
        );

        let unique = resolve(&[route_file, ("h.rs", "pub async fn list_items() {}\n")]);
        assert_eq!(
            edges_of(&unique, &[EdgeKind::HandlesRoute]),
            ["srv.rs::GET /items->h.rs::list_items"],
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
            ["api.py::GET /items->api.py::read_items"],
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
            ["app.js::GET /users->app.js::handleUsers"]
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
            ["app.js::GET /users->h.js::handleUsers"],
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
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
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
    #[cfg(feature = "parse")]
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
