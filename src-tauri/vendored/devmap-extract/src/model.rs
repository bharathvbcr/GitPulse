use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FileId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SymbolKind {
    File,
    Module,
    Class,
    Struct,
    Enum,
    Interface,
    /// Distinct from `Interface`: the frozen Python baseline reports a Rust
    /// `trait_item` as `trait`, and collapsing it loses the distinction between
    /// a trait and a TS/Java-style interface.
    Trait,
    Function,
    Method,
    Field,
    Variable,
    Route,
    Endpoint,
    EventSubscriber,
    Dependency,
    Subsystem,
    Community,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EdgeKind {
    Imports,
    Calls,
    /// Lexical containment: a file contains every symbol declared in it, and a
    /// type additionally contains its own methods. The frozen Python baseline
    /// emits these as `contains` and they form the majority of every golden
    /// graph, so omitting them leaves the structural skeleton unrepresented.
    Contains,
    Defines,
    Instantiates,
    Extends,
    Implements,
    SubscribesTo,
    HandlesRoute,
    WiredTo,
    MemberOf,
    DependsOn,
    TaintFlow,
    /// A non-call use: type annotation, JSX identifier, composite-literal type,
    /// receiver declaration, or other name occurrence. Distinct from `Calls` so
    /// liveness can see uses that are not invocations, without pretending they
    /// are.
    References,
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Confidence(pub f32);

impl Confidence {
    pub const DETERMINISTIC: Self = Confidence(1.0);
    pub const HIGH: Self = Confidence(0.9);
    pub const MEDIUM: Self = Confidence(0.7);
    pub const LOW: Self = Confidence(0.4);
    pub const SPECULATIVE: Self = Confidence(0.2);

    /// Integer milliconfidence in `[0, 1000]`. SQLite REAL cannot round-trip
    /// `f32` 0.9, so comparisons and persistence go through this discrete space.
    pub fn to_millis(self) -> i64 {
        confidence_millis(self.0)
    }

    /// Persist as an f64 that `ROUND(value * 1000)` recovers exactly.
    pub fn persist_real(self) -> f64 {
        self.to_millis() as f64 / 1000.0
    }
}

/// Discrete milliconfidence for any f32 confidence, including dead-symbol rows
/// that do not wrap `Confidence`.
pub fn confidence_millis(value: f32) -> i64 {
    if !value.is_finite() {
        return 0;
    }
    (f64::from(value) * 1000.0).round().clamp(0.0, 1000.0) as i64
}

/// Per-file SQLite size budget, never below a 60 MiB floor.
///
/// **Single owner of this policy.** `verify.sh` reads `DB_SIZE_GATE_PER_FILE`
/// from here rather than repeating the number, because the two drifted: this
/// function still said 80 KiB after the gate was recalibrated to 160 KiB on
/// measured evidence, and nothing noticed because no production code called it
/// (SC15). A gate constant that lives in two places will disagree eventually,
/// and the copy nobody runs is the one that goes stale.
///
/// 160 KiB is the measured worst case plus headroom: cold-build cost is
/// ~81 KiB/file on DevCouncil and ~113 KiB/file on a Go-heavy external corpus.
///
/// **This is a PER-GENERATION budget (SC27).** A store retains
/// `GENERATION_RETENTION` (2) generations, each carrying its own extraction
/// payloads, so the steady state is roughly double — measured on DevCouncil,
/// 73 MiB cold against **188 MiB at steady state, 173 KiB/file**. That exceeded
/// this constant, and no gate noticed, because the self-build gate built once
/// into a fresh database and compared a *cold* size against it.
///
/// Both states are now gated, each against the budget that describes it: the
/// self-build gate keeps comparing a cold build to this number, and the growth
/// gate compares its plateaued size to `db_size_gate_steady_bytes`. Loosening
/// this constant to cover the steady state would have blunted the cold-build
/// check instead of adding the missing one.
pub const DB_SIZE_GATE_PER_FILE: u64 = 160 * 1024;
/// Generations a store retains. Mirrors `devmap_store::GENERATION_RETENTION`,
/// which this crate cannot depend on; `retention_matches_the_store_constant` in
/// devmap-store asserts the two agree so they cannot drift.
pub const DB_SIZE_GATE_RETAINED_GENERATIONS: u64 = 2;
pub const DB_SIZE_GATE_FLOOR: u64 = 60 * 1024 * 1024;

pub fn db_size_gate_bytes(file_count: u64) -> u64 {
    DB_SIZE_GATE_FLOOR.max(file_count.saturating_mul(DB_SIZE_GATE_PER_FILE))
}

/// Budget for a store that has reached its retention ceiling (SC27).
///
/// This is the size a running repository actually sits at. Nothing gated it
/// before, so a regression that only showed up on the second build — which is
/// every build after the first — had no check to fail.
pub fn db_size_gate_steady_bytes(file_count: u64) -> u64 {
    DB_SIZE_GATE_FLOOR.max(
        file_count
            .saturating_mul(DB_SIZE_GATE_PER_FILE)
            .saturating_mul(DB_SIZE_GATE_RETAINED_GENERATIONS),
    )
}

#[cfg(test)]
mod span_tests {
    use super::Span;

    /// Byte offsets map to one-based inclusive line numbers.
    ///
    /// `line_range` is the query boundary: every `file:line` a caller is shown
    /// comes through here. The whole body was replaceable with `(0, 1)`,
    /// `(1, 0)` and `(1, 1)` without a failure, and the newline comparison was
    /// invertible — each of which reports every symbol at the top of its file,
    /// which reads as a plausible answer rather than as an error.
    #[test]
    fn byte_offsets_map_to_one_based_inclusive_lines() {
        let source = "alpha\nbeta\ngamma\ndelta\n";

        // First line is 1, not 0.
        let first = Span {
            start_byte: 0,
            end_byte: 5,
        };
        assert_eq!(first.line_range(source), (1, 1));

        // A span opening on line 2 and closing on line 4.
        let start = source.find("beta").unwrap();
        let end = source.find("delta").unwrap() + "delta".len();
        assert_eq!(
            Span {
                start_byte: start,
                end_byte: end
            }
            .line_range(source),
            (2, 4)
        );

        // A newline byte itself still belongs to the line it terminates.
        let nl = source.find('\n').unwrap();
        assert_eq!(
            Span {
                start_byte: nl,
                end_byte: nl
            }
            .line_range(source),
            (1, 1)
        );
        assert_eq!(
            Span {
                start_byte: nl + 1,
                end_byte: nl + 1
            }
            .line_range(source),
            (2, 2),
            "the byte after a newline opens the next line"
        );

        // Offsets past the end clamp to the last line rather than panicking.
        let past = Span {
            start_byte: source.len() + 99,
            end_byte: source.len() + 999,
        };
        assert_eq!(past.line_range(source), (5, 5));

        // Multi-byte characters are counted by newline, not by char index.
        let unicode = "\u{e9}\u{e9}\u{e9}\nx";
        let x = unicode.find('x').unwrap();
        assert_eq!(
            Span {
                start_byte: x,
                end_byte: x
            }
            .line_range(unicode),
            (2, 2)
        );

        // An empty source has exactly one line.
        assert_eq!(
            Span {
                start_byte: 0,
                end_byte: 0
            }
            .line_range(""),
            (1, 1)
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    pub start_byte: usize,
    pub end_byte: usize,
}

impl Span {
    /// Convert byte offsets to a one-based inclusive line range at the query
    /// boundary. Extraction and storage remain UTF-8-safe byte based.
    pub fn line_range(&self, source: &str) -> (u32, u32) {
        let start = self.start_byte.min(source.len());
        let end = self.end_byte.min(source.len());
        let line_number = |offset: usize| -> u32 {
            source.as_bytes()[..offset]
                .iter()
                .filter(|&&byte| byte == b'\n')
                .count()
                .saturating_add(1)
                .min(u32::MAX as usize) as u32
        };
        (line_number(start), line_number(end))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextRange {
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscoverySkipReason {
    NonSource,
    Oversized { bytes: u64, limit: u64 },
    NonUtf8Path,
    Unreadable { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DiscoveryReport {
    pub yielded_paths: Vec<String>,
    pub skipped_paths: Vec<(String, DiscoverySkipReason)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParseOutcome {
    Clean,
    Partial { error_ranges: Vec<TextRange> },
    Failed { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExtractionEngine {
    TreeSitter {
        grammar: String,
        grammar_version: u32,
    },
    ConfigScanner,
    Unavailable {
        requested_language: String,
    },
}

fn default_extraction_engine() -> ExtractionEngine {
    ExtractionEngine::Unavailable {
        requested_language: "legacy-cache".to_string(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedSymbol {
    pub name: String,
    pub qualified_name: String,
    pub kind: SymbolKind,
    pub span: Span,
    pub is_exported: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docstring: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_symbol: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedImport {
    pub raw_import: String,
    pub module_specifier: String,
    pub imported_names: Vec<String>,
    /// Local bindings aligned with `imported_names`.
    ///
    /// A single `alias` cannot represent syntax such as
    /// `from mod import a as x, b as y` without assigning the wrong local
    /// name to one of the imported symbols.
    #[serde(default)]
    pub local_names: Vec<String>,
    pub alias: Option<String>,
    pub span: Span,
}

/// Parse `a`, `a as b`, or `{ a as b, c }` import lists into parallel name vectors.
pub fn parse_import_bindings(raw: &str) -> (Vec<String>, Vec<String>) {
    let mut imported_names = Vec::new();
    let mut local_names = Vec::new();
    let raw = raw
        .trim()
        .trim_start_matches(['{', '('])
        .trim_end_matches(['}', ')']);
    for part in raw.split(',') {
        let part = part.trim().trim_matches(['{', '}', '(', ')']).trim();
        if part.is_empty() || part == "*" {
            continue;
        }
        let mut words = part.split_whitespace();
        let name = words.next().unwrap_or(part);
        let name = name.trim_matches(['{', '}', '(', ')']);
        if name.is_empty() {
            continue;
        }
        let local = if words.next() == Some("as") {
            words.next().unwrap_or(name)
        } else {
            name
        };
        imported_names.push(name.to_string());
        local_names.push(local.to_string());
    }
    (imported_names, local_names)
}

#[cfg(test)]
mod import_binding_tests {
    use super::parse_import_bindings;

    /// A wildcard import contributes no named binding.
    ///
    /// `part.is_empty() || part == "*"` was mutable to `&&` without a failure:
    /// the empty case is caught again downstream, but the wildcard is not, so
    /// `*` gets pushed as a literal imported name. The resolver then binds the
    /// local name `*` and every later reference to a real symbol from that
    /// module resolves against a binding that does not exist.
    #[test]
    fn wildcards_and_blanks_contribute_no_bindings() {
        let (names, locals) = parse_import_bindings("{ *, first }");
        assert_eq!(names, ["first"], "`*` must not become an imported name");
        assert_eq!(locals, ["first"]);

        let (names, locals) = parse_import_bindings("*");
        assert!(names.is_empty(), "a bare wildcard binds nothing: {names:?}");
        assert!(locals.is_empty());

        let (names, _) = parse_import_bindings("{ first, , second }");
        assert_eq!(names, ["first", "second"], "an empty part is skipped");
    }

    #[test]
    fn braced_and_parenthesized_aliases_preserve_every_binding() {
        let (names, locals) = parse_import_bindings("{ first as runFirst, second }");
        assert_eq!(names, ["first", "second"]);
        assert_eq!(locals, ["runFirst", "second"]);

        let (names, locals) = parse_import_bindings("(first as runFirst, second)");
        assert_eq!(names, ["first", "second"]);
        assert_eq!(locals, ["runFirst", "second"]);
    }
}

impl ExtractedImport {
    /// Return `(local_name, imported_name)` pairs for resolver binding construction.
    pub fn binding_pairs(&self) -> Vec<(String, String)> {
        if !self.local_names.is_empty() && self.local_names.len() == self.imported_names.len() {
            return self
                .imported_names
                .iter()
                .zip(&self.local_names)
                .map(|(imported, local)| (local.clone(), imported.clone()))
                .collect();
        }
        if self.imported_names.len() == 1 {
            let imported = self.imported_names[0].clone();
            let local = self.alias.clone().unwrap_or_else(|| imported.clone());
            return vec![(local, imported)];
        }
        self.imported_names
            .iter()
            .map(|name| (name.clone(), name.clone()))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedCall {
    pub caller_symbol: Option<String>,
    pub callee_name: String,
    pub receiver_expr: Option<String>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReferenceKind {
    Call,
    Constructor,
    Type,
    /// The module qualifying a declared type: `name` is the qualifier
    /// (`testing`, `reqwest`), `assigned_to` the value it types.
    ///
    /// Emitted alongside the `Type` reference rather than replacing it, because
    /// dispatch needs the bare type name and provenance needs the qualifier
    /// (SC25).
    TypeQualifier,
    Heritage,
    Decorator,
    JsxTag,
    /// Identifier in expression/value position (JSX prop, object shorthand, …).
    Name,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedReference {
    pub name: String,
    pub kind: ReferenceKind,
    pub span: Span,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enclosing_symbol: Option<String>,
    /// Simple local binding receiving this expression's value, when the
    /// grammar proves an assignment such as `worker = Worker()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_to: Option<String>,
    /// The object half of a member access, when this reference *is* one:
    /// `cfg` for `cfg.enabled`, `cmd` for `cmd.baseline`.
    ///
    /// Without it the extractor flattens `obj.attr` into a bare `attr`, and the
    /// resolver — which refuses to resolve a bare name globally, so that
    /// `except Exception as e` cannot bind to some unrelated `def e` — has no
    /// way to tell a member name from a local variable and drops both. Every
    /// property read, every callback passed by attribute, and every decorator
    /// registration therefore left no edge, and the symbol behind it was
    /// reported dead.
    ///
    /// This is the same field `ExtractedCall` carries for the same reason. A
    /// non-call member use is a method call minus the invocation, so it
    /// resolves on the same rungs: a typed receiver, or a receiver bound by an
    /// import.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_expr: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedExport {
    pub exported_name: String,
    pub local_name: Option<String>,
    pub module_specifier: Option<String>,
    pub span: Span,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedRoute {
    pub framework: String,
    pub http_method: String,
    pub path_pattern: String,
    pub handler_name: String,
    pub span: Span,
}

/// Evidence that something outside the resolvable call graph reaches a symbol.
///
/// `target_symbol` carries the scope: a file-level annotation targets the file
/// path, a symbol-level annotation targets a symbol's `qualified_name`. The two
/// are not interchangeable — a file-level exemption clears every symbol in the
/// file, so a symbol-level signal must never be widened into one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WiringAnnotation {
    pub kind: WiringKind,
    pub target_symbol: String,
    pub details: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WiringKind {
    ScriptEntry,
    Launcher,
    ReExportPackage,
    FrameworkDecorator,
    GeneratedFile,
    Vendored,
    TestFile,
    /// The language makes an explicit call site impossible to observe *and*
    /// forbids the symbol from being marked public — a Rust trait-impl method
    /// cannot carry `pub`, so `is_exported` is structurally always false and
    /// says nothing about whether the method is reachable.
    StructuralExempt,
    /// A runtime, framework, or test harness invokes the symbol without an
    /// explicit call anywhere in the corpus (`func init`, `#[test]`,
    /// `componentDidMount`, `pytest_*`, …).
    RuntimeEntryPoint,
}

/// One `m(...)` entry of a `type X interface { ... }` declaration.
///
/// Extraction is per file, but a Go interface is routinely declared in one file
/// of a package and satisfied by a type in another. The spec therefore travels
/// on the `Extraction` so `devmap-analyze`, which holds every file at once, can
/// join it against method declarations elsewhere in the same package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoInterfaceMethod {
    /// Name of the interface that declared this method spec, for the reason string.
    pub interface_name: String,
    /// Method name as written in the spec.
    pub method: String,
    /// Declared parameter count, counting each name in a grouped declaration
    /// (`m(x, y int)` is 2) and a variadic parameter as one.
    pub param_count: usize,
}

/// Declared parameter count of one `func (r T) m(...)` in this file.
///
/// Keyed by the method symbol's `qualified_name` so the join is unambiguous
/// when a file declares the same method name on two different receiver types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoMethodParams {
    pub qualified_name: String,
    pub param_count: usize,
}

/// Canonical wording for "this method is reached through an interface".
///
/// Shared by the extraction-time same-file annotation and the package-scoped
/// join in `devmap-analyze` so one match cannot report two different reasons.
pub fn go_interface_exemption_reason(interface_name: &str) -> String {
    format!("implements interface `{interface_name}`; calls reach it through the interface")
}

/// Method symbols that satisfy one of `specs`, paired with the interface name.
///
/// The match verifies method **name** and **parameter count** only. Go has no
/// `implements` keyword — satisfaction is structural — so without a type checker
/// this is the strongest available signal. It does NOT verify parameter or
/// result *types*, the receiver type, or that any value of the concrete type is
/// ever assigned to the interface. Callers must therefore keep `specs` scoped to
/// the Go package that declared them.
pub fn go_interface_method_matches<'a>(
    symbols: &'a [ExtractedSymbol],
    param_counts: &BTreeMap<&str, usize>,
    specs: impl IntoIterator<Item = &'a GoInterfaceMethod>,
) -> Vec<(&'a ExtractedSymbol, &'a str)> {
    let mut by_shape: BTreeMap<(&str, usize), &str> = BTreeMap::new();
    for spec in specs {
        by_shape
            .entry((spec.method.as_str(), spec.param_count))
            .or_insert(spec.interface_name.as_str());
    }
    if by_shape.is_empty() {
        return Vec::new();
    }
    symbols
        .iter()
        .filter(|symbol| symbol.kind == SymbolKind::Method)
        .filter_map(|symbol| {
            let params = *param_counts.get(symbol.qualified_name.as_str())?;
            let interface_name = *by_shape.get(&(symbol.name.as_str(), params))?;
            Some((symbol, interface_name))
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Extraction {
    pub file_path: String,
    pub language: String,
    pub content_hash: u64,
    #[serde(default = "default_extraction_engine")]
    pub engine: ExtractionEngine,
    pub parse_outcome: ParseOutcome,
    pub symbols: Vec<ExtractedSymbol>,
    pub imports: Vec<ExtractedImport>,
    pub calls: Vec<ExtractedCall>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exports: Vec<ExtractedExport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<ExtractedReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<ExtractedRoute>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wiring: Vec<WiringAnnotation>,
    /// Go `package` clause. Required after durable persist strips `source_code`,
    /// so G20 stars and package-level import edges survive reload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub go_package: Option<String>,
    /// Whether a Go file carries a build constraint — a `//go:build` or
    /// `// +build` directive, or an implicit `_GOOS`/`_GOARCH` filename suffix.
    /// Always false for non-Go files.
    ///
    /// Go forbids two package-level declarations of one name, so a package that
    /// declares the same name in two files can only compile if those files are
    /// mutually exclusive — which is to say, build-constrained. Recording the
    /// constraint is what lets `analyze_liveness` tell a *spurious* ambiguity
    /// between platform variants of one identity from a genuine one between two
    /// unrelated symbols.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub go_build_constrained: bool,
    /// Interface method specs declared in this file. Empty for non-Go files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub go_interface_methods: Vec<GoInterfaceMethod>,
    /// Parameter counts of the Go method declarations in this file, so a
    /// cross-file interface join can compare arity and not just name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub go_method_params: Vec<GoMethodParams>,
    /// `(qualified scope, local name)` for every value a callable binds itself:
    /// parameters, `let`/`:=`/`=` targets, loop variables, `with … as` handles.
    ///
    /// The resolver's `LocalBinding` tier answers "this bare call went to a
    /// value this scope declared, not to a symbol it failed to find". Without
    /// this it could only see typed parameters — Rust and Go signatures — so a
    /// Python `cls(...)` in a classmethod, a `let handler = |…|` invoked below
    /// it, or a locally bound helper all landed in the tier that means "possible
    /// defect".
    ///
    /// The scope string is the callee-side identity of the enclosing callable,
    /// which is exactly what `ExtractedCall::caller_symbol` records, so the two
    /// join without a second naming rule. Sorted and deduplicated at emission:
    /// it is derived from a `HashSet`, and the determinism gate digests this
    /// payload.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope_locals: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_code: Option<String>,
}

impl Extraction {
    /// Method `qualified_name` to declared parameter count, for the Go
    /// interface-satisfaction join.
    pub fn go_method_param_counts(&self) -> BTreeMap<&str, usize> {
        self.go_method_params
            .iter()
            .map(|entry| (entry.qualified_name.as_str(), entry.param_count))
            .collect()
    }

    /// Payload stored in `generation_files.extraction_json` and the extract
    /// cache. Re-resolve needs symbols, imports, calls, and name/type
    /// references. It does not need source text, matcher diagnostics, or the
    /// call-kind references that duplicate `calls`.
    pub fn for_durable_store(&self) -> Self {
        let mut durable = self.clone();
        durable.source_code = None;
        durable.diagnostics.clear();
        durable.references.retain(|reference| {
            // A call-kind reference duplicates an entry in `calls` and carries
            // no extra information — *unless* it records a receiver binding.
            // `worker = Worker()` is a Constructor reference whose
            // `assigned_to` is the only record that `worker` is a `Worker`;
            // `calls` has no field for it. Dropping those bindings made a
            // reloaded extraction resolve differently from a freshly extracted
            // one: receiver resolution lost its type and fell back to
            // speculative fan-out, so an incremental build emitted *more*
            // `Calls` edges than a cold build of the same tree and never
            // converged (SC16).
            reference.assigned_to.is_some()
                || !matches!(
                    reference.kind,
                    ReferenceKind::Call | ReferenceKind::Constructor | ReferenceKind::JsxTag
                )
        });
        durable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Confidence survives the SQLite round-trip exactly.
    ///
    /// This discrete milliconfidence space exists precisely because SQLite REAL
    /// cannot round-trip `f32` 0.9: persisting the raw float and reading it
    /// back yields a value that compares unequal to the constant it came from,
    /// so a `confidence >= 0.9` filter silently drops rows it should keep.
    /// `model.rs` had no tests at all, so nothing checked the conversion the
    /// whole scheme rests on.
    #[test]
    fn every_confidence_constant_round_trips_through_persistence() {
        for confidence in [
            Confidence::DETERMINISTIC,
            Confidence::HIGH,
            Confidence::MEDIUM,
            Confidence::LOW,
            Confidence::SPECULATIVE,
        ] {
            let persisted = confidence.persist_real();
            let recovered = (persisted * 1000.0).round() as i64;
            assert_eq!(
                recovered,
                confidence.to_millis(),
                "{confidence:?} must survive persist -> ROUND(value * 1000)"
            );
        }

        // The specific value that motivated the scheme.
        assert_eq!(Confidence::HIGH.to_millis(), 900);
        assert_eq!(Confidence::SPECULATIVE.to_millis(), 200);
        // And the tiers must stay ordered after conversion, or threshold
        // filters reorder silently.
        assert!(Confidence::DETERMINISTIC.to_millis() > Confidence::HIGH.to_millis());
        assert!(Confidence::HIGH.to_millis() > Confidence::MEDIUM.to_millis());
        assert!(Confidence::MEDIUM.to_millis() > Confidence::LOW.to_millis());
        assert!(Confidence::LOW.to_millis() > Confidence::SPECULATIVE.to_millis());
    }

    /// Non-finite and out-of-range confidences fail closed at zero or the cap.
    ///
    /// A NaN reaching the store as a confidence makes every comparison against
    /// it false, so the row is neither above nor below any threshold and
    /// disappears from every filtered query without an error.
    #[test]
    fn non_finite_and_out_of_range_confidences_are_clamped() {
        assert_eq!(confidence_millis(f32::NAN), 0, "NaN must fail closed at 0");
        assert_eq!(confidence_millis(f32::INFINITY), 0);
        assert_eq!(confidence_millis(f32::NEG_INFINITY), 0);

        assert_eq!(confidence_millis(2.0), 1000, "above 1.0 clamps to the cap");
        assert_eq!(confidence_millis(-1.0), 0, "below 0 clamps to zero");
        assert_eq!(confidence_millis(0.0), 0);
        assert_eq!(confidence_millis(1.0), 1000);
    }
}

#[cfg(test)]
mod gate_and_binding_tests {
    use super::*;

    /// The size-gate constants are their declared magnitudes.
    ///
    /// The existing gate test in `devmap-cli` reads `DB_SIZE_GATE_FLOOR` and
    /// asserts only relations against itself, so `60 * 1024 * 1024` mutated to
    /// `60 + 1024 + 1024` (2 KiB) still passed every relation while making the
    /// gate fire on any non-empty repository. A budget constant needs one
    /// absolute assertion somewhere, or every check of it is self-referential.
    #[test]
    fn size_gate_constants_are_their_declared_magnitudes() {
        assert_eq!(DB_SIZE_GATE_PER_FILE, 163_840, "160 KiB per file");
        // SC27: the steady-state budget is this per-generation figure times the
        // retention count, and is what a running repository is measured against.
        assert_eq!(DB_SIZE_GATE_RETAINED_GENERATIONS, 2, "2 generations kept");
        assert_eq!(
            db_size_gate_steady_bytes(10_000),
            3_276_800_000,
            "steady-state budget doubles the per-generation one"
        );
        assert_eq!(DB_SIZE_GATE_FLOOR, 62_914_560, "60 MiB floor");
        // The floor covers repositories up to 384 files; past that the per-file
        // budget takes over.
        assert_eq!(
            db_size_gate_bytes(100),
            DB_SIZE_GATE_FLOOR,
            "floor dominates small repos"
        );
        assert_eq!(
            db_size_gate_bytes(384),
            DB_SIZE_GATE_FLOOR,
            "the crossover stays on the floor"
        );
        assert_eq!(
            db_size_gate_bytes(385),
            63_078_400,
            "one file past the crossover scales"
        );
        assert_eq!(
            db_size_gate_bytes(10_000),
            1_638_400_000,
            "per-file dominates large repos"
        );
    }

    /// `binding_pairs` reverses each `(imported, local)` into `(local, imported)`
    /// and drops nothing.
    ///
    /// The whole body was replaceable with `vec![]` and with a single blank
    /// pair. Empty means the resolver builds no import bindings at all, so every
    /// cross-module reference falls to the unresolved ladder — which does not
    /// error, it just quietly downgrades confidence and inflates dead-symbol
    /// counts. The blank pair is worse: a binding keyed on the empty string.
    #[test]
    fn binding_pairs_reverse_each_binding_and_drop_none() {
        let blank = || ExtractedImport {
            raw_import: String::new(),
            module_specifier: "mod".into(),
            imported_names: Vec::new(),
            local_names: Vec::new(),
            alias: None,
            span: Span {
                start_byte: 0,
                end_byte: 0,
            },
        };

        let import = ExtractedImport {
            imported_names: vec!["first".into(), "second".into()],
            local_names: vec!["renamed".into(), "second".into()],
            ..blank()
        };
        assert_eq!(
            import.binding_pairs(),
            vec![
                ("renamed".to_string(), "first".to_string()),
                ("second".to_string(), "second".to_string()),
            ],
            "each pair is (local, imported), in order"
        );

        // No local names: each imported name binds to itself.
        let bare = ExtractedImport {
            imported_names: vec!["only".into()],
            ..blank()
        };
        let pairs = bare.binding_pairs();
        assert_eq!(
            pairs.len(),
            1,
            "an unaliased import still yields a binding: {pairs:?}"
        );
        assert!(
            pairs
                .iter()
                .all(|(local, imported)| !local.is_empty() && !imported.is_empty()),
            "no binding may be keyed on an empty name: {pairs:?}"
        );

        // Misaligned vectors fall back to identity bindings rather than
        // zipping. `!is_empty() && len == len` was mutable to `||`, and `zip`
        // silently truncates to the shorter side: with two imported names and
        // one local, the second import would vanish and the first would be
        // bound to the wrong local.
        let misaligned = ExtractedImport {
            imported_names: vec!["first".into(), "second".into()],
            local_names: vec!["only".into()],
            ..blank()
        };
        assert_eq!(
            misaligned.binding_pairs(),
            vec![
                ("first".to_string(), "first".to_string()),
                ("second".to_string(), "second".to_string()),
            ],
            "misaligned locals must not truncate the imported names"
        );

        // Nothing imported yields nothing bound.
        assert!(blank().binding_pairs().is_empty());
    }
}

#[cfg(test)]
mod go_interface_exemption_tests {
    use super::*;

    fn method(name: &str, qualified: &str) -> ExtractedSymbol {
        ExtractedSymbol {
            name: name.into(),
            qualified_name: qualified.into(),
            kind: SymbolKind::Method,
            span: Span {
                start_byte: 0,
                end_byte: 0,
            },
            is_exported: true,
            docstring: None,
            signature: None,
            parent_symbol: None,
        }
    }

    /// A real extraction to mutate, so the fixture cannot drift from the
    /// struct the pipeline actually produces.
    fn base_extraction() -> Extraction {
        crate::extract_file("fixture.go", "package main\n")
    }

    fn spec(interface: &str, method_name: &str, params: usize) -> GoInterfaceMethod {
        GoInterfaceMethod {
            interface_name: interface.into(),
            method: method_name.into(),
            param_count: params,
        }
    }

    /// The exemption reason names the interface it came from.
    ///
    /// This string is what a caller reads to decide whether a
    /// not-obviously-called method is safe to delete. Blanked or replaced with
    /// a constant, every Go interface exemption reports the same unattributable
    /// reason and the claim becomes unauditable — the reader cannot check which
    /// interface supposedly reaches the method.
    #[test]
    fn the_exemption_reason_names_its_interface() {
        let reason = go_interface_exemption_reason("io.Reader");
        assert!(
            reason.contains("io.Reader"),
            "the reason must name the interface: {reason}"
        );
        assert_ne!(
            reason,
            go_interface_exemption_reason("io.Writer"),
            "two interfaces must not produce the same reason"
        );
        assert!(!reason.is_empty());
    }

    /// Satisfaction matches on method name *and* parameter count, over methods
    /// only.
    ///
    /// The whole body was replaceable with `vec![]` — no method is ever
    /// exempted, which only inflates dead-symbol counts and so looks like a
    /// tuning problem rather than a bug. Inverting the `kind == Method` filter
    /// is the dangerous direction: plain functions get exempted on a name
    /// collision with an interface method, and a genuinely dead function is
    /// reported as live-through-an-interface.
    #[test]
    fn satisfaction_matches_on_name_arity_and_method_kind() {
        let mut symbols = vec![
            method("Read", "Buffer.Read"),
            method("Read", "Other.Read"),
            method("Close", "Buffer.Close"),
        ];
        // A plain function sharing an interface method's name and arity.
        let mut decoy = method("Read", "helperRead");
        decoy.kind = SymbolKind::Function;
        symbols.push(decoy);

        let param_counts: BTreeMap<&str, usize> = [
            ("Buffer.Read", 1),
            ("Other.Read", 2),
            ("Buffer.Close", 0),
            ("helperRead", 1),
        ]
        .into_iter()
        .collect();

        let specs = vec![spec("io.Reader", "Read", 1)];
        let matches = go_interface_method_matches(&symbols, &param_counts, &specs);

        assert_eq!(
            matches.len(),
            1,
            "exactly one method satisfies: {matches:?}"
        );
        assert_eq!(matches[0].0.qualified_name, "Buffer.Read");
        assert_eq!(matches[0].1, "io.Reader");

        // Arity is part of the shape: Other.Read takes 2 params, so it does not
        // satisfy a 1-param interface method.
        assert!(
            !matches
                .iter()
                .any(|(s, _)| s.qualified_name == "Other.Read"),
            "a same-named method with different arity must not satisfy"
        );
        // Kind is part of the shape: a function is never an interface method.
        assert!(
            !matches
                .iter()
                .any(|(s, _)| s.qualified_name == "helperRead"),
            "a plain function must never be exempted as an interface method"
        );

        // No specs means no exemptions — not "exempt everything".
        assert!(go_interface_method_matches(&symbols, &param_counts, &[]).is_empty());
    }

    /// The param-count map is keyed by qualified name and carries real counts.
    ///
    /// It was replaceable with an empty map (no method ever matches its arity,
    /// so no exemption is ever granted) and with single-entry maps keyed on
    /// `""` or `"xyzzy"`. The join above looks up `symbol.qualified_name`, so a
    /// wrong key silently drops every method out of the arity check.
    #[test]
    fn param_counts_are_keyed_by_qualified_name() {
        let mut extraction = base_extraction();
        extraction.go_method_params = vec![
            GoMethodParams {
                qualified_name: "Buffer.Read".into(),
                param_count: 1,
            },
            GoMethodParams {
                qualified_name: "Buffer.Close".into(),
                param_count: 0,
            },
        ];

        let counts = extraction.go_method_param_counts();
        assert_eq!(counts.len(), 2, "every entry is carried: {counts:?}");
        assert_eq!(counts.get("Buffer.Read"), Some(&1));
        assert_eq!(
            counts.get("Buffer.Close"),
            Some(&0),
            "a zero-parameter method is a real entry, not an absent one"
        );
        assert_eq!(counts.get("xyzzy"), None);
        assert_eq!(counts.get(""), None, "the map is not keyed on a blank name");
    }

    /// `for_durable_store` drops exactly the call-kind references and keeps the
    /// rest.
    ///
    /// Deleting the negation in the `retain` inverts it: every non-call
    /// reference is dropped and the calls are kept — the opposite of the
    /// deduplication it exists for. Both shapes still round-trip through the
    /// store, so nothing errors; the graph just loses every type annotation and
    /// name use while double-counting calls.
    #[test]
    fn durable_store_drops_call_kind_references_only() {
        let reference = |kind: ReferenceKind, name: &str| ExtractedReference {
            name: name.into(),
            kind,
            span: Span {
                start_byte: 0,
                end_byte: 0,
            },
            enclosing_symbol: None,
            assigned_to: None,
            receiver_expr: None,
        };

        let mut extraction = base_extraction();
        extraction.source_code = Some("package main".into());
        extraction.diagnostics = vec!["a matcher diagnostic".into()];
        extraction.references = vec![
            reference(ReferenceKind::Call, "dropped_call"),
            reference(ReferenceKind::Constructor, "dropped_ctor"),
            reference(ReferenceKind::JsxTag, "dropped_jsx"),
            reference(ReferenceKind::Type, "kept_type"),
            reference(ReferenceKind::Name, "kept_name"),
        ];

        let durable = extraction.for_durable_store();
        let kept: Vec<&str> = durable.references.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            kept,
            ["kept_type", "kept_name"],
            "only call-kind references are dropped"
        );
        assert!(durable.source_code.is_none(), "source text is not durable");
        assert!(
            durable.diagnostics.is_empty(),
            "diagnostics are not durable"
        );
    }

    /// A call-kind reference that carries a receiver binding survives persist.
    ///
    /// Regression for SC16. `assigned_to` is the only record that `worker` is a
    /// `Worker` — `calls` has no field for it. Stripping those references made a
    /// reloaded extraction resolve differently from a freshly extracted one:
    /// the binding was lost, receiver resolution fell back to speculative
    /// fan-out, and an incremental build emitted *more* `Calls` edges than a
    /// cold build of the same tree — permanently, since no later rebuild
    /// recomputed it. Measured on a 1,610-file tree: 172,161 edges against a
    /// cold 172,046, stable across further rebuilds.
    #[test]
    fn a_receiver_binding_survives_durable_persist() {
        let extraction = crate::extract_file(
            "app.py",
            "def run():\n    worker = Worker()\n    worker.go()\n",
        );
        assert!(
            extraction
                .references
                .iter()
                .any(|reference| reference.assigned_to.is_some()),
            "fixture precondition: the constructor must carry a receiver binding"
        );

        let durable = extraction.for_durable_store();
        let bound: Vec<(&str, Option<&str>)> = durable
            .references
            .iter()
            .filter(|reference| reference.assigned_to.is_some())
            .map(|reference| (reference.name.as_str(), reference.assigned_to.as_deref()))
            .collect();
        assert_eq!(
            bound,
            [("Worker", Some("worker"))],
            "a receiver binding is not redundant with `calls` and must survive persist"
        );

        // The redundant ones are still dropped, so this cannot quietly become
        // "keep everything" and give back the payload size SC8 saved.
        assert!(
            !durable.references.iter().any(|reference| {
                reference.assigned_to.is_none()
                    && matches!(
                        reference.kind,
                        ReferenceKind::Call | ReferenceKind::Constructor | ReferenceKind::JsxTag
                    )
            }),
            "call-kind references without a binding are still dropped"
        );
    }
}
