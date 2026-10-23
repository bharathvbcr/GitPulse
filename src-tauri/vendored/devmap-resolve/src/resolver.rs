use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rayon::prelude::*;

use devmap_extract::model::*;

use crate::model::*;

mod gotypes;
mod jsobjects;

/// A protocol method's own declaration, and the conformers' declarations of it:
/// `(file, symbol)` pairs.
type ProtocolDispatch = ((String, String), Vec<(String, String)>);
use devmap_extract::tsconfig::TsProject;
use devmap_extract::{GoModule, ProjectManifests};
use gotypes::{GoCall, GoFileIndex, GoModel, DISPATCH_FANOUT_CAP};

mod pytypes;
use pytypes::PythonImportBind;

/// The key a wildcard import (`from m import *`, `export * from 'm'`) takes in
/// `external_imports` and `unindexed_local_imports` when its module is not
/// indexed. Not a spelling any identifier can have, so it never collides with a
/// bound name; it is read by [`Resolver::names_a_non_local_import`], which is
/// why a star import blocks bare-name binding to a namesake.
const STAR_IMPORT: &str = "*";

/// One package-level declaration, as [`Resolver::go_package_symbols`] holds it:
/// the file that declares it, its qualified name, and its kind.
type PackageDecl = (String, String, SymbolKind);

/// Candidate file, kind, language family, and stable qualified identity.
type IndexedSymbol = (String, SymbolKind, LangFamily, Arc<str>);

type CandidateSet = Arc<[(String, String)]>;

/// Outcome of the X42 implicit-receiver lookup (`self.m()` / `this.m()` / …).
///
/// `VirtualOverride` is distinct from “no answer”: a unique declaration on the
/// enclosing type (or a unique inherited one) is still the wrong *deterministic*
/// target when an indexed subtype overrides the same name, because `self` can
/// be that subtype at runtime. Call-site emission must abstain into the
/// unresolved ledger rather than fall through to `SameFile`.
/// Where a Rust path's first segment can be placed.
///
/// `Keyword` is `crate` / `self` / `super`. `Crate` is one indexed crate
/// root of that name. `Child` is a module file under this module and not
/// a crate. `Ambiguous` is a name two crates claim, or a name that is both
/// a child module and a crate. `Unknown` is everything else — `std`, an
/// external crate, a path this file does not contain — and the call ladder
/// leaves it for classification.
enum RustPathRoot {
    Keyword,
    Crate(String),
    Child,
    Ambiguous,
    Unknown,
}

enum RustModulePlace {
    Unique(String),
    Ambiguous,
    Absent,
}

enum ImplicitReceiverAnswer {
    Unique {
        target_file: String,
        target_symbol: String,
        receiver_type: String,
    },
    VirtualOverride,
    /// The Python heritage walk reached a bound before it could answer; the
    /// reason names the bound.
    HeritageCap(String),
}

fn escaped_evidence_bytes(value: &str) -> u64 {
    value.bytes().fold(0u64, |total, byte| {
        // Ordinary printable ASCII is literal in both compact JSON and Rust
        // Debug strings. Keep the six-byte bound for quotes, backslashes,
        // control characters and each UTF-8 byte that may need escaping.
        total.saturating_add(
            if (0x20..=0x7e).contains(&byte) && !matches!(byte, b'"' | b'\\') {
                1
            } else {
                6
            },
        )
    })
}

struct GlobalCandidates {
    rows: CandidateSet,
    declarations: usize,
    evidence_bytes: u64,
}
type CandidateKey = (LangFamily, String, String);

#[derive(Default)]
struct GlobalCandidateCache {
    sets: BTreeMap<CandidateKey, Arc<GlobalCandidates>>,
    visits: u64,
    retained_bytes: u64,
}

fn charge(
    value: &mut u64,
    additional: u64,
    limit: u64,
    resource: &'static str,
) -> Result<(), ResolutionLimitError> {
    let attempted = value.saturating_add(additional);
    if attempted > limit {
        return Err(ResolutionLimitError {
            resource,
            limit,
            attempted,
        });
    }
    *value = attempted;
    Ok(())
}

fn charge_ambiguity(
    counter: &AtomicU64,
    candidates: &GlobalCandidates,
    limit: u64,
) -> Result<(), ResolutionLimitError> {
    // One ledger row per ambiguous site, however many candidates it names.
    let additional = candidates.evidence_bytes;
    // An explicit compare-exchange loop rather than `fetch_update`, which Rust
    // 1.99 deprecates in favour of `try_update` — a name older toolchains this
    // workspace still builds on do not have.
    let mut used = counter.load(Ordering::Relaxed);
    loop {
        let Some(next) = used.checked_add(additional).filter(|next| *next <= limit) else {
            return Err(ResolutionLimitError {
                resource: "ambiguity evidence bytes",
                limit,
                attempted: used.saturating_add(additional),
            });
        };
        match counter.compare_exchange_weak(used, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Ok(()),
            Err(current) => used = current,
        }
    }
}

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
    /// A call or expression-position identifier, with its exact receiver
    /// binding. Graph caller scopes cannot distinguish anonymous callbacks.
    Value {
        receiver_binding: Option<&'a LocalBinding>,
    },
    /// A type annotation. `types` names the value this annotation types — `t`
    /// for `t *testing.T` — when the extractor recorded one, because that is
    /// the key the `TypeQualifier` sibling was indexed under.
    Type {
        types: Option<&'a str>,
        /// The annotation's own enclosing symbol, `None` when unscoped. Not the
        /// substituted source path, which names no `declared_types` scope.
        scope: Option<&'a str>,
    },
}

impl<'a> UsePosition<'a> {
    fn value_at(extraction: &'a Extraction, start_byte: usize, receiver: Option<&str>) -> Self {
        Self::Value {
            receiver_binding: receiver.and_then(|receiver| {
                extraction
                    .local_binding_at(start_byte, Resolver::path_root(receiver))
                    .or_else(|| extraction.local_binding_at(start_byte, receiver))
            }),
        }
    }
}

/// Whether a file's stylesheet applies to the whole document.
///
/// True for a standalone stylesheet and for a plain page; false for every
/// template language, because Svelte, Vue and Astro each scope a component's
/// `<style>` to that component's own elements. Keyed on the language rather than
/// the extension so the one place that decides it is the same place
/// `detect_language` already answered.
///
/// `:global(.x)` in a component is deliberately **not** detected. It is the one
/// way a component legitimately declares a document-wide class, and honouring it
/// would need the extractor to mark the declaration — a field on the payload. So
/// a `:global` class answers its own file and nothing else: a missing edge, which
/// a reader can see, rather than a wrong one, which they cannot.
fn is_global_stylesheet(language: &str) -> bool {
    matches!(language, "css" | "html")
}

/// One file's declaration of a markup or stylesheet identity.
///
/// The flag is what makes cross-file resolution decidable. A component's
/// `<style>` is **scoped**: Svelte, Vue and Astro each compile it to rules that
/// match only that component's own elements, so a `.selected` declared in
/// `TaskBoard.svelte` cannot style `BranchList.svelte`'s markup, and an edge
/// saying otherwise is false however unique the name is. A standalone
/// stylesheet, and a `<style>` in a plain page, are global and can.
///
/// Measured on GitPulse before this flag existed: 672 cross-file selector edges,
/// 668 of them into the global `src/app.css` and correct, and three of the
/// remaining four into another component's scoped stylesheet and wrong.
#[derive(Debug, Clone)]
struct SelectorDeclaration {
    file: String,
    /// Whether the declaring file's stylesheet is document-wide: a `.css`,
    /// `.scss` or `.less` file, or an `.html` page.
    in_global_sheet: bool,
}

/// One Python file's `sys.path`, as far as its own source fixes it.
#[derive(Debug, Clone, Default)]
struct PySearchPath {
    /// Module-level inserts: the byte offset of each, and its repo-relative
    /// directory. An import after the offset resolves through it.
    entries: Vec<(usize, String)>,
    /// Directories inserted inside a function or class body. Such an insert
    /// may run before any import in the file, so it only vetoes: a module it
    /// could supply is not linked through `entries` unless both name the
    /// same file.
    vetoes: Vec<String>,
    /// An insert this file makes somewhere the source does not fix — an
    /// unreadable in-function directory, or one above the repository root —
    /// could supply any module, so no `sys.path`-derived link is made.
    veto_all: bool,
}

pub struct Resolver {
    symbol_index: BTreeMap<String, Vec<IndexedSymbol>>,
    file_symbols: BTreeMap<String, Vec<String>>, // file_path -> symbol_names
    /// Markup and stylesheet declarations, by their selector-form name:
    /// `[data-add-repo]`, `.nav-heading`, `#repo-heading`, `--gap-x` -> the
    /// files that declare them, deduplicated and sorted.
    ///
    /// **A second namespace, on purpose.** These names do not live in the same
    /// space as identifiers, and `symbol_index` is keyed by qualified name for
    /// rungs that reason about imports, receivers and packages — none of which a
    /// CSS class has. Putting them in one index would let the code ladder's
    /// unique-global rung answer a class with a function: a class called `menu`
    /// is not `fn menu`, and an edge between them is a fabrication, not a
    /// coarser answer. Kept apart, the only thing that can answer a selector is
    /// a markup or stylesheet declaration.
    selector_symbols: BTreeMap<String, Vec<SelectorDeclaration>>,
    /// Entry points a runtime dispatches by name — Metal `kernel`, `vertex` and
    /// `fragment` functions, plain or stamped by a macro — by bare name, to the
    /// files that declare them, deduplicated and sorted.
    ///
    /// The only thing that can answer a [`ReferenceKind::EntryName`], and kept
    /// out of `symbol_index` for the reason `selector_symbols` is: a string
    /// naming `"reduce"` is not a reference to every `fn reduce`, and only a
    /// declaration the runtime itself looks up by name is a target a string can
    /// mean.
    entry_names: BTreeMap<String, Vec<String>>,
    /// Every indexed file, grouped by its parent directory, each group sorted.
    ///
    /// Three call sites answered "which files sit directly in this directory"
    /// by scanning *every* key of `file_symbols` and calling `parent_dir` on
    /// each: `go_files_in_dir`, the package-suffix fallback in
    /// `resolve_go_import`, and `files_in_dir_with_extensions`. Go import
    /// resolution calls the first up to four times per import, so the cost was
    /// O(imports x files in the repository) — and `parent_dir` allocates twice
    /// per call, through `Path::parent` and `to_string_lossy().replace()`.
    ///
    /// Measured on scholarlm (1,942 Go files) with `sample`: 500 of the 825
    /// samples inside `index_extractions` were this scan, 267 of them in
    /// `parent_dir` alone.
    ///
    /// Grouping once costs one `parent_dir` per file per build and turns each
    /// of those scans into a lookup. Built from `file_symbols` rather than from
    /// the extractions so it holds exactly the population the scans walked —
    /// `file_symbols` takes an entry per extraction unconditionally, including
    /// files that declare no symbol.
    files_by_dir: BTreeMap<String, Vec<String>>,
    /// Indexed `.py` files grouped by file name, for the suffix rung of a
    /// Python path load (see [`Self::resolve_path_load`]). Built once, so a
    /// file of thousands of loads costs one lookup each rather than a scan of
    /// the corpus each.
    py_files_by_name: BTreeMap<String, Vec<String>>,
    /// Per-file `(scope, local)` → the file a path-loaded Python module
    /// handle names (`module = module_from_spec(spec)` and friends).
    ///
    /// Deliberately not `import_bindings`, which is per file with **no scope
    /// test**: a handle bound inside one function would bind the same name in
    /// every other function of the file. `scope` is the binding's
    /// `LocalBinding::scope` identity, `None` for a module global.
    path_module_bindings: BTreeMap<String, BTreeMap<(Option<String>, String), String>>,
    /// Per-file `sys.path` entries of a Python file, resolved to
    /// repo-relative directories. Read only for that file — see
    /// [`Self::resolve_via_search_dirs`].
    py_search_dirs: BTreeMap<String, PySearchPath>,
    /// Rust crate name, spelled as a `use` spells it, -> that crate's `src`
    /// root. `None` where two indexed crates claim the same name.
    ///
    /// `use devmap_store::CURRENT_SCHEMA_VERSION;` is how one crate names
    /// another, and nothing mapped that name to a file. The crate-relative rung
    /// derives its root from the *importer's* path — the innermost `src` above
    /// it — which is right for `crate::` and answers nothing here: an
    /// integration test lives in `tests/`, has no `src` ancestor at all, and
    /// fell back to a literal `src` that matches no workspace member.
    ///
    /// Built from indexed roots rather than from manifests, so the entry exists
    /// only where the crate's own `lib.rs` is in the corpus. Cargo's name
    /// normalisation is the one assumption: a directory `devmap-store` is the
    /// crate `devmap_store`, which is the convention every crate in this
    /// workspace follows and the only bridge between the two spellings.
    /// Ambiguity abstains rather than picking, the same rule
    /// [`Self::unique_basename`] follows.
    rust_crate_roots: BTreeMap<String, Option<String>>,
    /// The `Cargo.toml` packages the manifest pass read, so a crate is also
    /// known by the names its manifest gives it — `[package] name` and `[lib]
    /// name` — and not only by its directory. Set by
    /// [`Self::index_project_manifests`], which a caller runs before
    /// [`Self::index_extractions`]; that one builds `rust_crate_roots` from it.
    rust_manifest_crates: Vec<devmap_extract::RustCrateManifest>,
    /// Indexed crate root files a manifest places by hand (`[lib] path`,
    /// `[[bin]] path`), keyed by the directory each keeps its modules in.
    /// The path convention (`src/lib.rs`, `src/main.rs`) cannot see them: a
    /// library at `lib/core.rs` has no `src` ancestor to be relative to and a
    /// stem that reads as a leaf module. Built with `rust_crate_roots`.
    rust_declared_roots: BTreeMap<String, BTreeSet<String>>,
    /// The library root file a manifest's `[lib] path` names, by the directory
    /// `rust_crate_roots` records for that crate — what a bare crate name is.
    rust_declared_libs: BTreeMap<String, String>,
    /// The Python source roots each manifest declares, deepest directory
    /// first. Set by [`Self::index_project_manifests`]; read by
    /// [`Self::python_root_candidates`].
    python_manifest_roots: Vec<devmap_extract::pyproject::PythonSourceRoots>,
    /// Top-level Python package name → the directories that hold a package of
    /// that name (`<dir>/<name>/__init__.py` indexed, `<dir>/__init__.py`
    /// not). The no-manifest fallback: a name held by exactly one directory
    /// is importable from it.
    py_package_parents: BTreeMap<String, BTreeSet<String>>,
    /// The Swift targets the manifests declare; see [`Self::swift_modules_of`].
    swift_targets: Vec<devmap_extract::SwiftTarget>,
    /// `(file, qualified name)` → the header a Rust function was declared
    /// with. Read by the rungs that type a binding from a return type or a
    /// closure parameter; see `devmap_extract::rustsig`.
    rust_signatures: BTreeMap<(String, String), String>,
    /// `(file, qualified name)` of every declaration the language hides from
    /// other files: a Swift `private`/`fileprivate`, a non-exported binding of
    /// a TS/JS module, a non-`pub` Rust item. Read by the bare-name rung so
    /// it never binds a name its caller cannot see (S2).
    unexported_symbols: BTreeSet<(String, String)>,
    /// `<file>::<exported name>` -> the file that declares it, for
    /// `export { x } from './m'`. See `compute_reexport_chains`.
    reexport_chains: BTreeMap<String, String>,
    /// Set when the glob expansion behind `reexport_chains` stopped at
    /// `REEXPORT_GLOB_MAX_HOPS`, so the chains are a partial map. See
    /// [`Self::reexport_glob_cutoff`].
    reexport_glob_cutoff: Option<ReexportGlobCutoff>,
    /// Rust: `<file>` -> the module files its `pub use m::*;` globs name. A
    /// module path segment that is no child of a file may be a module one of
    /// these publishes (see `rust_module_through_globs`).
    rust_module_globs: BTreeMap<String, Vec<String>>,
    /// `<file>` -> the local binding its `export default` publishes. What an
    /// importer's `import X from` and a barrel's `export { default } from` ask
    /// for by the name `default`; see `default_binding`.
    default_exports: BTreeMap<String, String>,
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
    /// `(file, subtype)` -> the bases that file's `class` statement writes, in
    /// source order, for Python only. A dotted base (`pkg.Base`) is one entry
    /// spelled with its dots, because it names a module's member and the last
    /// segment alone names no class.
    ///
    /// [`Self::supertypes`] is flat by bare name, so two classes of one name in
    /// two files share one entry. A Python class is identified by the file that
    /// declares it, and its bases by the file's own imports, so this table is
    /// the per-declaration half the `pytypes` walk reads before it falls back
    /// to the flat one.
    python_bases: BTreeMap<(String, String), Vec<String>>,
    /// `file` -> local name -> everything the file's Python imports bind that
    /// name to. A name bound to more than one thing is not a name the file
    /// can be read as meaning one module at a class statement.
    python_import_binds: BTreeMap<String, BTreeMap<String, BTreeSet<PythonImportBind>>>,
    /// `(file, callable qualified name)` for every Python function or method a
    /// decorator may wrap. See [`pytypes::python_decorated_callables`].
    python_decorated: BTreeSet<(String, String)>,
    /// `(file, class name)` -> what each class of that name is written inside:
    /// `(enclosing identity, whether it is a callable)`. Read from the symbols'
    /// spans, because a class in a function and a class in a same-named class
    /// share one identity.
    python_class_enclosers: BTreeMap<(String, String), Vec<(String, bool)>>,
    /// The Python files whose `__all__` is not the literal list the extractor
    /// reads names from, so `from m import *` may take any name they bind. See
    /// [`pytypes::python_all_is_dynamic`].
    python_dynamic_all: BTreeSet<String>,
    /// `(file, function or class qualified name)` -> what each import written
    /// in that scope binds its local name to; `None` when it resolved to no
    /// file or the scope imports the name from two places. The flat
    /// `import_bindings` keeps only module-scope imports for a name the module
    /// binds; this is where the function's own live.
    python_scoped_import_bindings:
        BTreeMap<(String, String), BTreeMap<String, Option<(String, String)>>>,
    /// `(file, name)` that several functions import differently and the module
    /// does not import: the flat table holds one of them, so a read that no
    /// function's own scope answers has no binding.
    python_ambiguous_scoped_imports: BTreeSet<(String, String)>,
    /// Reverse of [`Self::supertypes`]: `(family, type name)` → indexed subtypes.
    ///
    /// Built from the same heritage rows. X42's virtual-`self` guard walks this
    /// to see whether an indexed subtype overrides a method the enclosing type
    /// (or a base) uniquely declares — if so, a `ReceiverType` edge would claim
    /// a runtime target the subtype can steal.
    subtypes: BTreeMap<(LangFamily, String), BTreeSet<String>>,
    /// Per-file local import name → (target file, exported symbol) for import-scoped calls (G6).
    import_bindings: BTreeMap<String, BTreeMap<String, (String, String)>>,
    /// `(importing file, local name)` → the `(file, name)` that **declares** the
    /// value a named import binds, for the subset of `import_bindings` that
    /// name a declared value rather than a module.
    ///
    /// `import { svc } from './service'` and `from pkg import obj` bind a
    /// *value*; `import * as service`, `import pkg` and a Python submodule
    /// bound by `from pkg import cmd` bind a *module*. Both land in
    /// `import_bindings` with the same shape, and the module-member rung (2b)
    /// read every one of them as a module: `svc.zzhelper()` bound to a free
    /// function `zzhelper` that merely shares the module, at DETERMINISTIC.
    /// Recorded at the one place the binding is made, because only there is it
    /// known which branch chose the target — inferring it afterwards from
    /// `file_symbols` cannot tell `service.py` declaring `service` from the
    /// module `service` itself.
    value_imports: BTreeMap<(String, String), (String, String)>,
    /// `(file, name)` → the one indexed class a **module-scope** value is an
    /// instance of: `export const svc = new SvcClass()`, Python
    /// `service = Service()`.
    ///
    /// Kept apart from `receiver_types`, whose `file:var` key is written by
    /// every assignment in the file whatever its scope — so a function-local
    /// `const svc = new Other()` would lend the exported `svc` its type. This
    /// map is written only from the declaration's own initializer, and
    /// withdrawn when anything else in the file assigns the name. It is what a
    /// value import is typed by in another file, which is the whole shape of a
    /// singleton service: declared once, called everywhere else.
    module_value_types: BTreeMap<(String, String), String>,
    /// `(file, name)` → the names a module-scope value's **own declaration**
    /// carries: the shorthand properties of `export const api = { get, post }`
    /// and the methods written inside `export const log = { info() {…} }`.
    ///
    /// The evidence that makes `api.get()` a call to the module's `get`. A
    /// named value import is not a module, so its members are not the
    /// module's free functions in general — `svc.helper()` on a service
    /// instance is never a sibling `function helper` — but an object literal
    /// built from those functions is exactly that. Only a name the literal
    /// itself spells is admitted, so the coincidence stays refused.
    module_value_members: BTreeMap<(String, String), BTreeSet<String>>,
    /// `file` → how each of its module-scope JS/TS const objects is built, in
    /// source order. Read only through [`Self::object_member_target`].
    js_objects: BTreeMap<String, jsobjects::FileObjects>,
    /// `(file, qualified name)` → the return type a callable's declaration
    /// writes, as the extractor recorded it. What types a factory-built value;
    /// read only through [`Self::factory_return_type`].
    return_types: BTreeMap<(String, String), String>,
    /// `(file, name)` → the initializer shape of a module-scope value built by
    /// a call that is not a class: `var r = NewRegistry()`, `export const s =
    /// createService()`. Resolved lazily, because the factory may be declared
    /// in a file indexed after this one.
    module_value_factories: BTreeMap<(String, String), (LangFamily, String)>,
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
    /// `(file, qualified name)` identities that more than one callable of one
    /// container declares in that file: Kotlin, Swift, Java, C# and Scala
    /// overloads (`add(Int)` / `add(String)`), whose ids the extractor mints
    /// from the name alone. A call from inside one declaration of such an
    /// identity to the same identity cannot be told from a call to its sibling
    /// overload without the call's arity.
    shared_identities: BTreeSet<(String, String)>,
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
    /// `(file, parent scope, bare name)` → what that scope declares by that name.
    ///
    /// The inverse of `symbol_parents`, keyed so the lexical walk asks one
    /// question per scope. Scanning every same-named symbol of the corpus per
    /// scope made a bare read cost O(types sharing the name): 2,000 types each
    /// declaring `id` retired 10.8x the instructions of 2,000 distinct names.
    scope_children: BTreeMap<(String, String, String), Vec<(SymbolKind, Arc<str>)>>,
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
    /// TypeScript / JavaScript project configs, deepest directory first: the
    /// `paths` / `baseUrl` mapping a non-relative specifier resolves through.
    ts_projects: Vec<TsProject>,
    /// The `go_modules_fresh` contract, for `ts_projects` and `ts_workspaces`.
    ts_projects_fresh: bool,
    /// Workspace packages by the name an importer spells them, sorted by name.
    ts_workspaces: Vec<devmap_extract::workspaces::WorkspacePackage>,
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
    /// The unexported-selector rung's index: `(directory, package clause,
    /// method name)` → every concrete method of that **unexported** name the
    /// package declares, as `(file, qualified name)`. See
    /// [`Resolver::unexported_selector_target`].
    go_package_methods: BTreeMap<(String, String, String), Vec<(String, String)>>,
    /// `(directory, package clause, name)` for every unexported struct field or
    /// interface method the package declares. A name here vetoes the rung: the
    /// selector may be the field or the interface method, not the concrete one.
    go_package_member_vetoes: BTreeSet<(String, String, String)>,
    /// Directories holding a `.go` file whose extraction cannot vouch for the
    /// package's complete declaration set — not a clean parse, or no
    /// `go_member_names` (a payload cached before they were collected). A
    /// package here gets no unexported-selector answers at all: one unseen file
    /// could declare the second method or the vetoing field.
    go_package_dirs_unvouched: BTreeSet<String>,
    /// The Go type model: declarations, signatures and imports by package.
    go_model: GoModel,
    /// file_path → Swift module name derived from the path.
    ///
    /// A Swift target is one unqualified namespace. Computed from the path
    /// (`Sources/<Name>/`, `app/MarkDevKit/…`) rather than from a build graph
    /// this kernel does not read. See `devmap_extract::languages::swift_module_of`.
    swift_module_by_file: BTreeMap<String, Vec<String>>,
    /// Kotlin/Java `package` header per file: every file of one package shares
    /// an unqualified namespace, test source sets included.
    jvm_package_by_file: BTreeMap<String, String>,
    /// Every name any Swift file declares with `typealias`; see
    /// [`Extraction::swift_type_aliases`].
    swift_type_aliases: BTreeSet<String>,
    /// `(file, enclosing symbol, bare type name)` to how many `Type`
    /// references with no assigned name a Swift declaration makes of it. A
    /// callable that writes `-> Inner` makes one; a generic clause adds another.
    swift_unassigned_type_mentions: BTreeMap<(String, String, String), u32>,
    /// `(bare owner type, bare type name)` for every unassigned `Type`
    /// reference a Swift type declaration makes outside its members: where a
    /// generic parameter of the type (`Box<Element>`) is declared.
    swift_type_header_mentions: BTreeSet<(String, String)>,
    /// `(package, declared name)` to the files of that package declaring it:
    /// what a Kotlin/Java `import a.b.name` is read against, because a
    /// top-level function or an extension is not in a file named for it.
    jvm_package_symbols: BTreeMap<(String, String), BTreeSet<String>>,
    /// `{file}:{type}:{field}` to the call shape that initializes a Kotlin
    /// property (`val clock = Clock.create()`), for a field no type annotation
    /// types: its type is what the factory declares it returns.
    field_initializers: BTreeMap<String, String>,
    /// Gradle module directories named by `settings.gradle(.kts)` `include`
    /// statements, repo-relative: each holds `src/<set>/{java,kotlin}` roots.
    gradle_module_dirs: Vec<String>,
    /// module name → every indexed file that constitutes it.
    swift_module_files: BTreeMap<String, BTreeSet<String>>,
    /// `(module, bare name)` → the **file-level** declarations of that name.
    ///
    /// The index behind [`Resolver::same_swift_module_target`]. Keyed on the
    /// module rather than the directory because a Swift module is the target,
    /// not a folder: `app/MarkDevKit/Editor/Foo.swift` and
    /// `app/MarkDevKit/Core/Foo.swift` share one namespace.
    swift_module_symbols: BTreeMap<(String, String), Vec<PackageDecl>>,
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
    /// Per Rust file, the names of the inline `mod name { … }` blocks it
    /// declares. See `Extraction::rust_inline_modules`.
    rust_inline_modules: BTreeMap<String, BTreeSet<String>>,
    /// Rust type name → the type it `Deref`s to; `None` when two indexed
    /// `impl Deref` blocks disagree about a type of that name.
    rust_deref_targets: BTreeMap<String, Option<String>>,
    /// Method name → every Rust method a blanket impl declares under that name
    /// (`T.describe` for `impl<T: Display> Describe for T`), as `(file,
    /// identity)`. See `Extraction::rust_blanket_impl_params`.
    rust_blanket_methods: BTreeMap<String, Vec<(String, String)>>,
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
            selector_symbols: BTreeMap::new(),
            entry_names: BTreeMap::new(),
            files_by_dir: BTreeMap::new(),
            py_files_by_name: BTreeMap::new(),
            path_module_bindings: BTreeMap::new(),
            py_search_dirs: BTreeMap::new(),
            rust_crate_roots: BTreeMap::new(),
            rust_manifest_crates: Vec::new(),
            rust_declared_roots: BTreeMap::new(),
            rust_declared_libs: BTreeMap::new(),
            python_manifest_roots: Vec::new(),
            py_package_parents: BTreeMap::new(),
            swift_targets: Vec::new(),
            rust_signatures: BTreeMap::new(),
            unexported_symbols: BTreeSet::new(),
            reexport_chains: BTreeMap::new(),
            reexport_glob_cutoff: None,
            rust_module_globs: BTreeMap::new(),
            default_exports: BTreeMap::new(),
            receiver_types: BTreeMap::new(),
            scoped_receiver_types: BTreeMap::new(),
            poisoned_receiver_keys: BTreeSet::new(),
            type_methods: BTreeMap::new(),
            supertypes: BTreeMap::new(),
            python_bases: BTreeMap::new(),
            python_import_binds: BTreeMap::new(),
            python_decorated: BTreeSet::new(),
            python_class_enclosers: BTreeMap::new(),
            python_dynamic_all: BTreeSet::new(),
            python_scoped_import_bindings: BTreeMap::new(),
            python_ambiguous_scoped_imports: BTreeSet::new(),
            subtypes: BTreeMap::new(),
            import_bindings: BTreeMap::new(),
            value_imports: BTreeMap::new(),
            module_value_types: BTreeMap::new(),
            module_value_members: BTreeMap::new(),
            js_objects: BTreeMap::new(),
            return_types: BTreeMap::new(),
            module_value_factories: BTreeMap::new(),
            declared_types: BTreeMap::new(),
            external_imports: BTreeMap::new(),
            unindexed_local_imports: BTreeMap::new(),
            external_module_roots: BTreeMap::new(),
            local_module_roots: BTreeMap::new(),
            qualified_names: BTreeMap::new(),
            shared_identities: BTreeSet::new(),
            symbol_parents: BTreeMap::new(),
            scope_children: BTreeMap::new(),
            go_modules: Vec::new(),
            go_modules_fresh: false,
            ts_projects: Vec::new(),
            ts_projects_fresh: false,
            ts_workspaces: Vec::new(),
            go_package_by_file: BTreeMap::new(),
            go_package_symbols: BTreeMap::new(),
            go_package_methods: BTreeMap::new(),
            go_package_member_vetoes: BTreeSet::new(),
            go_package_dirs_unvouched: BTreeSet::new(),
            go_model: GoModel::default(),
            swift_module_by_file: BTreeMap::new(),
            jvm_package_by_file: BTreeMap::new(),
            swift_type_aliases: BTreeSet::new(),
            swift_unassigned_type_mentions: BTreeMap::new(),
            swift_type_header_mentions: BTreeSet::new(),
            jvm_package_symbols: BTreeMap::new(),
            field_initializers: BTreeMap::new(),
            gradle_module_dirs: Vec::new(),
            swift_module_files: BTreeMap::new(),
            swift_module_symbols: BTreeMap::new(),
            scope_locals: BTreeSet::new(),
            rust_inline_modules: BTreeMap::new(),
            rust_deref_targets: BTreeMap::new(),
            rust_blanket_methods: BTreeMap::new(),
            max_indexed_path_depth: 0,
            unique_basename: BTreeMap::new(),
        }
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

/// Whether `bindings` imports the member a symbol names (`Type.member` or a
/// bare `member`) from `target_file`.
fn hits_import_binding(
    bindings: Option<&BTreeMap<String, (String, String)>>,
    symbol: &str,
    target_file: &str,
) -> bool {
    let member = symbol.rsplit('.').next().unwrap_or(symbol);
    bindings
        .and_then(|bindings| bindings.get(member))
        .is_some_and(|(file, _)| file == target_file)
}

mod value_types;

mod ts_mapping;

mod receiver_types;

mod unresolved;

mod index;

mod resolve_all;

mod candidates;

mod rust_modules;

mod scope;

mod implicit_receiver;

mod dispatch;

mod imports;

mod references;

mod import_path;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod import_resolution_tests;

#[cfg(test)]
mod ladder_tests;

#[cfg(test)]
mod reference_resolution_tests;
