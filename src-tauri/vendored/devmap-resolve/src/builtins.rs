//! Language-declared names, used to classify calls that can never resolve.
//!
//! SC18. Membership here must be a *fact about the language*, not a guess about
//! what a name looks like. Every set below is closed and specification-derived,
//! so a name is either in the language or it is not.
//!
//! The bias is deliberate and one-directional: when in doubt, leave a name out.
//! An omitted builtin is merely recorded as `Unresolved`, which is where it sits
//! today; a *wrongly included* one would silently hide a genuine resolution
//! defect behind an "expected" label. Under-classifying costs noise, and
//! over-classifying costs correctness.
//!
//! Notably excluded, and why:
//!
//! - **Standard-library methods** (`unwrap`, `to_string`, `join` in Rust;
//!   `Fatalf` on a `*testing.T`). These are library API, not language
//!   declarations, and no import binds them — an inherent method needs no `use`.
//!   There is no evidence to classify them with, so they stay `Unresolved`.
//! - **Host environment globals** (`setTimeout`, `fetch`, `structuredClone`).
//!   These are supplied by a browser or by Node, not by ECMAScript. They are
//!   *not* omitted any more, but they are deliberately kept out of `is_builtin`
//!   and answered by `host_global_environment` instead, because the authority
//!   behind the claim is different — see `HOST_GLOBALS`.
//!
//! Package-qualified library calls (`strings.TrimSpace`, `assert.Equal`,
//! `useState`) are *not* handled here. They are classified as `External` from
//! import evidence instead, which is stronger than any name list.

use crate::model::LangFamily;

/// Go's predeclared functions and types (Go specification, "Predeclared
/// identifiers"). The types appear as callees through conversions — `string(b)`
/// parses as a call whose callee is `string`.
/// Sorted, not grouped by category: `is_builtin` uses `binary_search`.
const GO_BUILTINS: &[&str] = &[
    "any",
    "append",
    "bool",
    "byte",
    "cap",
    "clear",
    "close",
    "comparable",
    "complex",
    "complex128",
    "complex64",
    "copy",
    "delete",
    "error",
    "float32",
    "float64",
    "imag",
    "int",
    "int16",
    "int32",
    "int64",
    "int8",
    "len",
    "make",
    "max",
    "min",
    "new",
    "panic",
    "print",
    "println",
    "real",
    "recover",
    "rune",
    "string",
    "uint",
    "uint16",
    "uint32",
    "uint64",
    "uint8",
    "uintptr",
];

/// Python's `builtins` module — callable names available without any import.
///
/// Includes the built-in *exception* types, which are as much a part of the
/// module as its functions and are called constantly (`raise ValueError(...)`
/// parses as a call). Omitting them left ~800 rows in the tier reserved for
/// probable defects.
///
/// Audited entry-by-entry against the `builtins` module of CPython 3.13; the
/// gaps that audit found were `BaseExceptionGroup`/`ExceptionGroup` (3.11),
/// `EncodingWarning` (3.10) and `PythonFinalizationError` (3.13).
///
/// `exit`, `quit`, `copyright`, `credits` and `license` are deliberately
/// **excluded**: the `site` module injects them at interpreter start-up, so
/// they are a property of a deployment rather than of the language, and a
/// module run with `-S` does not have them.
const PYTHON_BUILTINS: &[&str] = &[
    "ArithmeticError",
    "AssertionError",
    "AttributeError",
    "BaseException",
    "BaseExceptionGroup",
    "BlockingIOError",
    "BrokenPipeError",
    "BufferError",
    "BytesWarning",
    "ChildProcessError",
    "ConnectionAbortedError",
    "ConnectionError",
    "ConnectionRefusedError",
    "ConnectionResetError",
    "DeprecationWarning",
    "EOFError",
    "EncodingWarning",
    "EnvironmentError",
    "Exception",
    "ExceptionGroup",
    "FileExistsError",
    "FileNotFoundError",
    "FloatingPointError",
    "FutureWarning",
    "GeneratorExit",
    "IOError",
    "ImportError",
    "ImportWarning",
    "IndentationError",
    "IndexError",
    "InterruptedError",
    "IsADirectoryError",
    "KeyError",
    "KeyboardInterrupt",
    "LookupError",
    "MemoryError",
    "ModuleNotFoundError",
    "NameError",
    "NotADirectoryError",
    "NotImplementedError",
    "OSError",
    "OverflowError",
    "PendingDeprecationWarning",
    "PermissionError",
    "ProcessLookupError",
    "PythonFinalizationError",
    "RecursionError",
    "ReferenceError",
    "ResourceWarning",
    "RuntimeError",
    "RuntimeWarning",
    "StopAsyncIteration",
    "StopIteration",
    "SyntaxError",
    "SyntaxWarning",
    "SystemError",
    "SystemExit",
    "TabError",
    "TimeoutError",
    "TypeError",
    "UnboundLocalError",
    "UnicodeDecodeError",
    "UnicodeEncodeError",
    "UnicodeError",
    "UnicodeTranslateError",
    "UnicodeWarning",
    "UserWarning",
    "ValueError",
    "Warning",
    "ZeroDivisionError",
    "__import__",
    "abs",
    "aiter",
    "all",
    "anext",
    "any",
    "ascii",
    "bin",
    "bool",
    "breakpoint",
    "bytearray",
    "bytes",
    "callable",
    "chr",
    "classmethod",
    "compile",
    "complex",
    "delattr",
    "dict",
    "dir",
    "divmod",
    "enumerate",
    "eval",
    "exec",
    "filter",
    "float",
    "format",
    "frozenset",
    "getattr",
    "globals",
    "hasattr",
    "hash",
    "help",
    "hex",
    "id",
    "input",
    "int",
    "isinstance",
    "issubclass",
    "iter",
    "len",
    "list",
    "locals",
    "map",
    "max",
    "memoryview",
    "min",
    "next",
    "object",
    "oct",
    "open",
    "ord",
    "pow",
    "print",
    "property",
    "range",
    "repr",
    "reversed",
    "round",
    "set",
    "setattr",
    "slice",
    "sorted",
    "staticmethod",
    "str",
    "sum",
    "super",
    "tuple",
    "type",
    "vars",
    "zip",
];

/// Rust's standard prelude: the macros it exports and the enum variants it
/// brings into scope. Deliberately *not* std methods — see the module comment.
///
/// Macro names arrive without their `!`, which is how the extractor records
/// them (`rust_macro_calls` strips the bang before emitting the call).
/// Sorted, not grouped by category: `is_builtin` uses `binary_search`. Contains
/// the prelude's enum variants (`Some`/`Ok`, which parse as calls when
/// constructed), its macros, and `drop`.
///
/// Audited against the macros `std` re-exports at its crate root, which are the
/// ones usable with no `use` at all. The gaps that audit found are the
/// compile-time macros — `cfg`, `concat`, `stringify`, `include*`,
/// `module_path`, `option_env`, `compile_error` — and `thread_local`.
///
/// Four std macros are deliberately **excluded**: `env`, `file`, `line` and
/// `column`. The extractor records `env!(...)` and a plain `env()` call under
/// the same callee name, and those four are all plausible user function names,
/// so including them would risk labelling a genuine unresolved call as
/// expected. Per the module comment, an omission costs noise and an
/// over-inclusion costs correctness. `is_x86_feature_detected` is excluded as
/// architecture-gated, and `assert_matches`/`concat_bytes` as unstable.
const RUST_BUILTINS: &[&str] = &[
    "Err",
    "None",
    "Ok",
    "Some",
    "assert",
    "assert_eq",
    "assert_ne",
    "cfg",
    "compile_error",
    "concat",
    "dbg",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "drop",
    "eprint",
    "eprintln",
    "format",
    "format_args",
    "include",
    "include_bytes",
    "include_str",
    "matches",
    "module_path",
    "option_env",
    "panic",
    "print",
    "println",
    "stringify",
    "thread_local",
    "todo",
    "unimplemented",
    "unreachable",
    "vec",
    "write",
    "writeln",
];

/// ECMAScript global *functions* (ECMA-262, "Function Properties of the Global
/// Object") plus the standard constructors, which appear as callees through
/// `new X()` and through conversion calls like `String(v)`.
/// Sorted, not grouped by category: `is_builtin` uses `binary_search`. Uppercase
/// sorts before lowercase, so the constructors lead.
///
/// Audited clause-by-clause against ECMA-262 (19 "The Global Object" through 28
/// "Reflection"). The original table held only 16 of the standard constructors
/// and was missing every native error type but `Error` and every typed array:
/// `TypeError` and `Float64Array` were both sitting in the tier reserved for
/// probable defects.
///
/// Excluded on purpose:
/// - `Math`, `JSON`, `Reflect`, `Atomics`, `globalThis` — namespace objects,
///   not callable. A call whose *callee* is one of these is evidence of a bad
///   extraction, and labelling it expected would hide that.
/// - `Intl` — ECMA-402, a different specification, and also not callable.
/// - `Iterator` — a global constructor only since ES2025, and by far the most
///   plausible user-declared name in this list.
/// - `escape` / `unescape` — Annex B legacy, and `escape` is a common name for
///   a hand-written HTML/SQL escaper.
const JS_BUILTINS: &[&str] = &[
    "AggregateError",
    "Array",
    "ArrayBuffer",
    "BigInt",
    "BigInt64Array",
    "BigUint64Array",
    "Boolean",
    "DataView",
    "Date",
    "Error",
    "EvalError",
    "FinalizationRegistry",
    "Float16Array",
    "Float32Array",
    "Float64Array",
    "Function",
    "Int16Array",
    "Int32Array",
    "Int8Array",
    "Map",
    "Number",
    "Object",
    "Promise",
    "Proxy",
    "RangeError",
    "ReferenceError",
    "RegExp",
    "Set",
    "SharedArrayBuffer",
    "String",
    "Symbol",
    "SyntaxError",
    "TypeError",
    "URIError",
    "Uint16Array",
    "Uint32Array",
    "Uint8Array",
    "Uint8ClampedArray",
    "WeakMap",
    "WeakRef",
    "WeakSet",
    "decodeURI",
    "decodeURIComponent",
    "encodeURI",
    "encodeURIComponent",
    "eval",
    "isFinite",
    "isNaN",
    "parseFloat",
    "parseInt",
];

/// Names a *runtime* supplies to every JavaScript program it hosts, with the
/// runtime that supplies them.
///
/// Kept out of `is_builtin` deliberately. `Builtin` claims "the language
/// declares this", and no edition of ECMA-262 declares `setTimeout` or `fetch`
/// — a browser and Node do. Folding these into `Builtin` would be a lie about
/// the authority behind the claim, and the authority is the whole reason the
/// classification is trustworthy. They get their own tier instead, carrying the
/// environment as the evidence, exactly as `External` carries its module.
///
/// Sorted by name: `host_global_environment` uses `binary_search_by_key`.
///
/// Two admission rules, both narrowing:
///
/// 1. **Callable only.** `console`, `document`, `window`, `process`,
///    `navigator`, `location`, `localStorage` and `globalThis` are host
///    objects, not host functions. A call whose *callee* is one of them cannot
///    be a real call, so it is evidence of an extraction defect and must stay
///    in the tier that means "possible defect". Their *methods*
///    (`console.log()`) are receiver-bearing and are answered by
///    `UninferredReceiver`, which already means "expected, not a defect".
/// 2. **Unmistakably platform API.** `open`, `close`, `print`, `stop`, `focus`,
///    `blur`, `scroll`, `confirm`, `prompt`, `Event`, `Request`, `Response`,
///    `Headers`, `File`, `Image`, `Audio`, `Notification` and `Worker` are all
///    genuine `Window` members and all omitted, because each is at least as
///    likely to be a name the repository declares itself. (`Worker` is the
///    fixture name in this crate's own tests.) The bias is the module's: an
///    omission costs noise, an over-inclusion hides a defect.
///
/// `fetch` and `URL` are kept despite being plausible identifiers, because they
/// are the dominant members of this class in the corpus and are only ever
/// reached here *after* the whole resolution ladder, including this file's own
/// imports, has failed to find a declaration.
///
/// Sources: WHATWG HTML ("Web application APIs" — `Window`,
/// `WindowOrWorkerGlobalScope`), WHATWG DOM, URL, Fetch, Encoding, Streams,
/// File API, XHR, CSSOM/CSSOM View, W3C Background Tasks and the observer
/// specifications; and the Node.js "Global objects" documentation.
const HOST_GLOBALS: &[(&str, &str)] = &[
    ("AbortController", "web+node"),
    ("AbortSignal", "web+node"),
    ("Blob", "web+node"),
    ("BroadcastChannel", "web+node"),
    ("Buffer", "node"),
    ("CompressionStream", "web+node"),
    ("CustomEvent", "web+node"),
    ("DOMException", "web+node"),
    ("DOMParser", "web"),
    ("DecompressionStream", "web+node"),
    ("EventSource", "web"),
    ("FileReader", "web"),
    ("FormData", "web+node"),
    ("IntersectionObserver", "web"),
    ("MessageChannel", "web+node"),
    ("MutationObserver", "web"),
    ("PerformanceObserver", "web+node"),
    ("ReadableStream", "web+node"),
    ("ResizeObserver", "web"),
    ("TextDecoder", "web+node"),
    ("TextEncoder", "web+node"),
    ("TransformStream", "web+node"),
    ("URL", "web+node"),
    ("URLSearchParams", "web+node"),
    ("WebSocket", "web+node"),
    ("WritableStream", "web+node"),
    ("XMLHttpRequest", "web"),
    ("XMLSerializer", "web"),
    ("alert", "web"),
    ("atob", "web+node"),
    ("btoa", "web+node"),
    ("cancelAnimationFrame", "web"),
    ("cancelIdleCallback", "web"),
    ("clearImmediate", "node"),
    ("clearInterval", "web+node"),
    ("clearTimeout", "web+node"),
    ("createImageBitmap", "web"),
    ("fetch", "web+node"),
    ("getComputedStyle", "web"),
    ("getSelection", "web"),
    ("matchMedia", "web"),
    ("queueMicrotask", "web+node"),
    ("reportError", "web+node"),
    ("requestAnimationFrame", "web"),
    ("requestIdleCallback", "web"),
    ("require", "node"),
    ("setImmediate", "node"),
    ("setInterval", "web+node"),
    ("setTimeout", "web+node"),
    ("structuredClone", "web+node"),
];

/// X48. The JavaScript-family global **objects**, as opposed to the global
/// *functions* in [`HOST_GLOBALS`].
///
/// A separate table because it answers a different question, and answering it
/// from the other one would break that one's first admission rule. `HOST_GLOBALS`
/// is "callable only" on purpose: a call whose *callee* is `console` cannot be a
/// real call, so it must stay in the tier that means "possible defect". What
/// this table is for is the *receiver*: `console.log(...)`, `JSON.stringify(x)`,
/// `process.env`, `Math.floor(n)`. There the name is not being called — it is
/// the object the runtime injected, and the member is platform API.
///
/// Those rows reached `UninferredReceiver`, the tier documented as "the receiver
/// is a value whose type we could not infer". `console` is not a receiver whose
/// type could not be inferred; it is one whose type the runtime states. That is
/// the same argument X43 made for `std::fs`, in the language whose globals are
/// objects rather than modules. Measured before this table existed: **126 of the
/// 316** JS `uninferred_receiver` rows on this repository were rooted at one of
/// these, and 7,408 of 58,904 across scholarlm's JS/TS.
///
/// Two admission rules:
///
/// 1. **Named by a specification as a property of the global object** — ECMA-262
///    §19-28 for the standard objects, WHATWG HTML "Web application APIs" for
///    the browser ones, and the Node.js "Global objects" page for `process`.
/// 2. **Never sufficient on its own.** Every use of this table is guarded by the
///    corpus: the enclosing scope must not bind the name, the file's imports
///    must not, and no indexed file of the family may declare it. A repository
///    with its own `class Date` keeps its `Date.parse` in the defect tier, which
///    is the same veto `is_prelude_type` opens with and for the same reason.
///
/// Sorted by name: `host_global_object` uses `binary_search_by_key`.
const HOST_GLOBAL_OBJECTS: &[(&str, &str)] = &[
    ("Array", "ecma"),
    ("Atomics", "ecma"),
    ("BigInt", "ecma"),
    ("Boolean", "ecma"),
    ("Date", "ecma"),
    ("Intl", "ecma"),
    ("JSON", "ecma"),
    ("Map", "ecma"),
    ("Math", "ecma"),
    ("Number", "ecma"),
    ("Object", "ecma"),
    ("Promise", "ecma"),
    ("Proxy", "ecma"),
    ("Reflect", "ecma"),
    ("RegExp", "ecma"),
    ("Set", "ecma"),
    ("String", "ecma"),
    ("Symbol", "ecma"),
    ("WeakMap", "ecma"),
    ("WeakSet", "ecma"),
    ("console", "web+node"),
    ("crypto", "web+node"),
    ("document", "web"),
    ("globalThis", "web+node"),
    ("history", "web"),
    ("localStorage", "web"),
    ("location", "web"),
    ("navigator", "web+node"),
    ("performance", "web+node"),
    ("process", "node"),
    ("sessionStorage", "web"),
    ("window", "web"),
];

/// The environment that injects `name` as a global **object**, or `None`.
///
/// Only ever consulted for the *root of a receiver* in a JavaScript-family
/// file, and only behind the three corpus guards [`HOST_GLOBAL_OBJECTS`]
/// documents. A bare callee is not asked here: `console()` is not a call the
/// runtime can answer, and letting this table say otherwise would hide the
/// extraction defect that produced it.
pub fn host_global_object(family: LangFamily, name: &str) -> Option<&'static str> {
    if family != LangFamily::JsTs {
        return None;
    }
    HOST_GLOBAL_OBJECTS
        .binary_search_by_key(&name, |(global, _)| *global)
        .ok()
        .map(|index| HOST_GLOBAL_OBJECTS[index].1)
}

/// Whether `name` is declared by the language itself for this family.
///
/// Only ever consulted for a *bare* callee. A call with a receiver is library
/// API — `strings.TrimSpace` is not a builtin just because `TrimSpace` is short
/// — and is classified from import evidence instead.
pub fn is_builtin(family: LangFamily, name: &str) -> bool {
    let table = match family {
        LangFamily::Go => GO_BUILTINS,
        LangFamily::Python => PYTHON_BUILTINS,
        LangFamily::Rust => RUST_BUILTINS,
        LangFamily::JsTs => JS_BUILTINS,
        LangFamily::Swift => SWIFT_BUILTINS,
        LangFamily::Kotlin => KOTLIN_BUILTINS,
        LangFamily::Ruby => RUBY_BUILTINS,
        LangFamily::Php => PHP_BUILTINS,
        LangFamily::Shell => SHELL_BUILTINS,
        // C/C++/C#/Java have no free-function builtins that reach the resolver
        // this way, and `Generic` spans languages with no curated set at all.
        //
        // The six added when embedded-script extraction landed
        // (`Erlang`..`Sql`) are here for the honest reason rather than the
        // convenient one: no curated builtin table has been written for them.
        // Listing them explicitly rather than adding a `_` arm keeps this match
        // exhaustive, so the next language forces the same decision instead of
        // silently inheriting "no builtins" — which would classify every
        // `length/1` or `echo` as a possible defect and bury the real ones,
        // the exact signal loss SC18 closed.
        LangFamily::CStyle
        | LangFamily::Scala
        | LangFamily::Lua
        | LangFamily::R
        | LangFamily::Dart
        | LangFamily::Erlang
        | LangFamily::Nix
        | LangFamily::Pascal
        | LangFamily::Solidity
        | LangFamily::Sql
        | LangFamily::Generic => return false,
    };
    table.binary_search(&name).is_ok()
}

/// Rust's standard prelude, restricted to the names that appear in **type
/// position**: the types and traits `std::prelude::rust_2021` brings into scope
/// with no `use` at all, plus the derive macros usable unqualified.
///
/// Kept apart from [`RUST_BUILTINS`] rather than folded into it, because the
/// two are consulted with different evidence. `RUST_BUILTINS` answers a *bare
/// callee* — `println!`, `Some(x)` — and a Rust source file cannot write
/// `String(x)` as a call at all, so nothing in this table belongs there. These
/// names reach the resolver as `ReferenceKind::Type`, and the rung that reads
/// them (`Resolver::classify_unresolved`) additionally requires that **no
/// indexed file of the same family declares the name**. That second condition
/// is what makes the table safe where the module comment says a name list is
/// not: `Default`, `Debug`, `Clone`, `Iterator` and `Error`-adjacent names are
/// all plausible things a Rust repository declares itself, and where one does,
/// the reference either resolved to it or the resolver *abstained on
/// ambiguity*. Labelling an abstention "the language declares this" is exactly
/// the over-classification the module comment forbids, so the corpus gets the
/// last word.
///
/// Audited against the Rust reference's "Standard library prelude" section for
/// the 2021 edition: `std::prelude::v1` (which re-exports `core::prelude::v1`)
/// plus the 2021 additions `TryFrom`, `TryInto` and `FromIterator`. The
/// prelude's enum *variants* — `Some`, `None`, `Ok`, `Err` — are deliberately
/// absent here and present in [`RUST_BUILTINS`] instead: they are written in
/// value position, where that table is the one consulted.
///
/// Sorted, not grouped by category: [`is_prelude_type`] uses `binary_search`.
pub const RUST_PRELUDE_TYPES: &[&str] = &[
    "AsMut",
    "AsRef",
    "Box",
    "Clone",
    "Copy",
    "Debug",
    "Default",
    "DoubleEndedIterator",
    "Drop",
    "Eq",
    "ExactSizeIterator",
    "Extend",
    "Fn",
    "FnMut",
    "FnOnce",
    "From",
    "FromIterator",
    "Hash",
    "Into",
    "IntoIterator",
    "Iterator",
    "Option",
    "Ord",
    "PartialEq",
    "PartialOrd",
    "Result",
    "Send",
    "Sized",
    "String",
    "Sync",
    "ToOwned",
    "ToString",
    "TryFrom",
    "TryInto",
    "Unpin",
    "Vec",
];

/// The shell's own builtins: POSIX.1-2017 XCU §2.14 ("Special Built-In
/// Utilities") and §1.6 ("Built-In Utilities"), plus the builtins `bash` adds,
/// which is the interpreter every `#!` line in this repository names.
///
/// X43. `LangFamily::Shell` fell to `is_builtin`'s "no curated table" arm,
/// which the arm's own comment predicted the cost of: "which would classify
/// every `length/1` or `echo` as a possible defect and bury the real ones".
/// Measured on this repository: **398 of the 521 shell rows** were `echo`,
/// `exit`, `printf`, `return`, `set`, `cd`, `command`, `pwd`, `break`, `trap`
/// and friends, sitting in the tier documented as the only one that indicates a
/// defect, and shell reported a net resolution of 38 permille against python's
/// 492.
///
/// External *programs* are deliberately absent — `grep`, `awk`, `cargo`, `git`,
/// `python3`. The shell does not declare them, no import names them, and what
/// is on `PATH` is a property of a machine rather than of the language. Under
/// this module's rule that leaves them in `Unresolved`, which costs 123 rows of
/// noise on this repository and hides nothing.
///
/// `[` and `[[` are included as written: they are utilities whose *name* is the
/// bracket, and a script cannot declare a function called `[`.
/// Sorted, not grouped by category: `is_builtin` uses `binary_search`.
pub const SHELL_BUILTINS: &[&str] = &[
    ".",
    ":",
    "[",
    "[[",
    "alias",
    "bg",
    "bind",
    "break",
    "builtin",
    "caller",
    "cd",
    "command",
    "compgen",
    "complete",
    "compopt",
    "continue",
    "declare",
    "dirs",
    "disown",
    "echo",
    "enable",
    "eval",
    "exec",
    "exit",
    "export",
    "false",
    "fc",
    "fg",
    "getopts",
    "hash",
    "help",
    "history",
    "jobs",
    "kill",
    "let",
    "local",
    "logout",
    "mapfile",
    "popd",
    "printf",
    "pushd",
    "pwd",
    "read",
    "readarray",
    "readonly",
    "return",
    "set",
    "shift",
    "shopt",
    "source",
    "suspend",
    "test",
    "times",
    "trap",
    "true",
    "type",
    "typeset",
    "ulimit",
    "umask",
    "unalias",
    "unset",
    "wait",
];

/// Module-path roots the language reserves for its own standard library.
///
/// X43. `std::fs::write(...)` reached the classifier as a *receiver* — `std::fs`
/// — and fell to `UninferredReceiver`, the tier that means "the receiver is a
/// value whose type we could not infer". `std::fs` is not a value. Rust injects
/// `std`, `core` and `alloc` into every crate's extern prelude, so a path rooted
/// at one of them is addressable with no `use` at all and can never name
/// anything an indexed file declares. 2,128 rows on this repository.
///
/// Only Rust has an entry, and the arm is exhaustive so the next language has
/// to decide rather than inherit a silence. Python's `os.path.join` needs no
/// entry: `import os` is a statement in the file, and the import rungs already
/// read it — a table would be a second, weaker answer to a question already
/// answered by evidence.
pub fn is_reserved_module_root(family: LangFamily, root: &str) -> bool {
    match family {
        // `proc_macro` and `test` are compiler-provided too, but only under a
        // crate type / feature that the source does not state, so they are left
        // out on this module's "when in doubt, leave it out" rule.
        LangFamily::Rust => matches!(root, "std" | "core" | "alloc"),
        _ => false,
    }
}

/// Whether `name`, written in **type position**, is one the language itself
/// puts in scope with no import.
///
/// Rust only. Every other family either has its type names in [`is_builtin`]
/// already (Go's predeclared types, JavaScript's constructors, Python's
/// `str`/`int`/`list`) or has no prelude to speak of, and inventing a table for
/// one that does not exist would classify a real defect as expected — the one
/// direction this module refuses to fail in.
pub fn is_prelude_type(family: LangFamily, name: &str) -> bool {
    match family {
        LangFamily::Rust => RUST_PRELUDE_TYPES.binary_search(&name).is_ok(),
        LangFamily::Swift => SWIFT_PRELUDE_TYPES.binary_search(&name).is_ok(),
        _ => false,
    }
}

/// Swift standard-library **free functions**, from the Swift Standard Library
/// reference. Swift is a primary language for this repository, so a bare call
/// to one of these is expected-unresolvable rather than a defect.
///
/// Deliberately omits `abs`, `max`, `min`, `swap`, `dump`, `zip`, `stride` and
/// `sequence`. Each is genuinely in the stdlib, and each is at least as likely
/// to be a function the repository declares itself — the SC30 rule: a name that
/// a repository plausibly owns stays in `Unresolved`, because over-reporting a
/// defect is recoverable and exempting a real one is not. Type initialisers
/// (`String(...)`, `Int(...)`) are absent for the same reason and because a
/// constructor call resolves through the type, not through this table.
pub const SWIFT_BUILTINS: &[&str] = &[
    "assert",
    "assertionFailure",
    "debugPrint",
    "fatalError",
    "getVaList",
    "isKnownUniquelyReferenced",
    "numericCast",
    "precondition",
    "preconditionFailure",
    "print",
    "readLine",
    "repeatElement",
    "transcode",
    "unsafeBitCast",
    "unsafeDowncast",
    "withExtendedLifetime",
    "withUnsafeBytes",
    "withUnsafeMutableBytes",
    "withUnsafeMutablePointer",
    "withUnsafePointer",
    "withVaList",
];

/// Swift standard-library types available without any `import`.
///
/// Constructors (`String("x")`, `Int(n)`) parse as calls whose callee is the
/// type. They are not in [`SWIFT_BUILTINS`] — that table is free functions —
/// and without this list every `String` annotation and every `Int(...)` in a
/// Swift file landed in `Unresolved`, the tier that means "possible defect".
///
/// `View` is **not** here: it belongs to SwiftUI and is only in scope after
/// `import SwiftUI`. `URL` / `Data` / `UUID` belong to Foundation the same way.
/// A name the repository might declare (`Result`, `Task`, `Error`) stays
/// behind [`Resolver::family_declares`] at the call site, the SC30 rule.
///
/// Sorted for [`is_prelude_type`]'s `binary_search`.
pub const SWIFT_PRELUDE_TYPES: &[&str] = &[
    "Any",
    "AnyHashable",
    "AnyObject",
    "Array",
    "AsyncStream",
    "AsyncThrowingStream",
    "Bool",
    "CancellationError",
    "CaseIterable",
    "Character",
    "ClosedRange",
    "Codable",
    "Collection",
    "CommandLine",
    "Comparable",
    "CustomStringConvertible",
    "Decodable",
    "Dictionary",
    "Double",
    "Duration",
    "Encodable",
    "Equatable",
    "Error",
    "Float",
    "Hashable",
    "Identifiable",
    "Int",
    "Int16",
    "Int32",
    "Int64",
    "Int8",
    "IteratorProtocol",
    "KeyPath",
    "MainActor",
    "Never",
    "ObjectIdentifier",
    "OpaquePointer",
    "Optional",
    "Range",
    "RawRepresentable",
    "Result",
    "Sendable",
    "Sequence",
    "Set",
    "String",
    "Task",
    "TaskGroup",
    "TaskPriority",
    "UInt",
    "UInt16",
    "UInt32",
    "UInt64",
    "UInt8",
    "UnsafeMutablePointer",
    "UnsafeMutableRawPointer",
    "UnsafePointer",
    "UnsafeRawPointer",
    "Void",
];

/// Unprefixed types that belong to a specific Apple / Swift SDK module.
///
/// Consulted only when the file imported that module, and only when the
/// corpus does not declare the name. Prefix rules (`XCT*`, `NS*`, `CG*`,
/// `UI*`) live beside this table in [`swift_sdk_module`] so a local type
/// named `NSCache` in a file that never imported Foundation stays unresolved.
///
/// Sorted by name for `binary_search_by_key`.
pub const SWIFT_SDK_TYPES: &[(&str, &str)] = &[
    ("AnyCancellable", "Combine"),
    ("AnyPublisher", "Combine"),
    ("AppKit", "AppKit"),
    ("AttributedString", "Foundation"),
    ("Binding", "SwiftUI"),
    ("Bundle", "Foundation"),
    ("Button", "SwiftUI"),
    ("CGAffineTransform", "CoreGraphics"),
    ("CGFloat", "CoreGraphics"),
    ("CGPoint", "CoreGraphics"),
    ("CGRect", "CoreGraphics"),
    ("CGSize", "CoreGraphics"),
    ("CGVector", "CoreGraphics"),
    ("Calendar", "Foundation"),
    ("Color", "SwiftUI"),
    ("Combine", "Combine"),
    ("CurrentValueSubject", "Combine"),
    ("Data", "Foundation"),
    ("Date", "Foundation"),
    ("DateFormatter", "Foundation"),
    ("Decimal", "Foundation"),
    ("DispatchGroup", "Dispatch"),
    ("DispatchQueue", "Dispatch"),
    ("DispatchTime", "Dispatch"),
    ("Environment", "SwiftUI"),
    ("EnvironmentObject", "SwiftUI"),
    ("FileManager", "Foundation"),
    ("Font", "SwiftUI"),
    ("ForEach", "SwiftUI"),
    ("Form", "SwiftUI"),
    ("Foundation", "Foundation"),
    ("HStack", "SwiftUI"),
    ("HTTPURLResponse", "Foundation"),
    ("Image", "SwiftUI"),
    ("IndexPath", "Foundation"),
    ("IndexSet", "Foundation"),
    ("JSONDecoder", "Foundation"),
    ("JSONEncoder", "Foundation"),
    ("LazyHStack", "SwiftUI"),
    ("LazyVStack", "SwiftUI"),
    ("List", "SwiftUI"),
    ("Locale", "Foundation"),
    ("Measurement", "Foundation"),
    ("NSRange", "Foundation"),
    ("NavigationLink", "SwiftUI"),
    ("NavigationStack", "SwiftUI"),
    ("Notification", "Foundation"),
    ("NotificationCenter", "Foundation"),
    ("ObservedObject", "SwiftUI"),
    ("PassthroughSubject", "Combine"),
    ("PersonNameComponents", "Foundation"),
    ("ProcessInfo", "Foundation"),
    ("Published", "Combine"),
    ("ScrollView", "SwiftUI"),
    ("Spacer", "SwiftUI"),
    ("State", "SwiftUI"),
    ("StateObject", "SwiftUI"),
    ("SwiftUI", "SwiftUI"),
    ("Text", "SwiftUI"),
    ("TimeZone", "Foundation"),
    ("URL", "Foundation"),
    ("URLQueryItem", "Foundation"),
    ("URLRequest", "Foundation"),
    ("URLSession", "Foundation"),
    ("UUID", "Foundation"),
    ("UserDefaults", "Foundation"),
    ("VStack", "SwiftUI"),
    ("View", "SwiftUI"),
    ("WindowGroup", "SwiftUI"),
    ("ZStack", "SwiftUI"),
];

/// XCTest types that do not start with `XCT`.
///
/// `XCTAssertEqual` and friends match the prefix rule; `XCTestCase` does not.
pub const SWIFT_XCTEST_TYPES: &[&str] = &[
    "XCTest",
    "XCTestCase",
    "XCTestExpectation",
    "XCTestObservationCenter",
];

/// The SDK module that owns `name` in this file, if the file imported it.
///
/// File-specific: `View` is SwiftUI only in a file that imported SwiftUI, and
/// a repository that declares its own `View` is not classified here — the
/// caller applies the corpus veto (SC30) before treating the answer as final.
pub fn swift_sdk_module<'a>(
    imported_modules: impl IntoIterator<Item = &'a str>,
    name: &str,
) -> Option<&'static str> {
    let imported: Vec<&str> = imported_modules.into_iter().collect();
    let imported = |module: &str| imported.iter().any(|item| *item == module);

    if name.starts_with("XCT") || SWIFT_XCTEST_TYPES.binary_search(&name).is_ok() {
        return imported("XCTest").then_some("XCTest");
    }
    if let Ok(index) = SWIFT_SDK_TYPES.binary_search_by_key(&name, |(n, _)| *n) {
        let module = SWIFT_SDK_TYPES[index].1;
        return imported(module).then_some(module);
    }
    if name.starts_with("NS") && name.len() > 2 {
        for module in ["Foundation", "AppKit", "UIKit"] {
            if imported(module) {
                return Some(module);
            }
        }
    }
    if name.starts_with("CG") && name.len() > 2 {
        for module in ["CoreGraphics", "Foundation", "AppKit", "UIKit", "SwiftUI"] {
            if imported(module) {
                return Some(module);
            }
        }
    }
    if name.starts_with("UI")
        && name.len() > 2
        && name.as_bytes()[2].is_ascii_uppercase()
        && imported("UIKit")
    {
        return Some("UIKit");
    }
    None
}

/// Kotlin top-level functions from the auto-imported `kotlin` package. Kotlin
/// is a primary language for this repository.
///
/// Deliberately omits `error`, `check`, `require`, `repeat`, `maxOf`, `minOf`,
/// `synchronized` and `lazy` — all real stdlib entries, all plausible names for
/// a repository to declare, so they stay in `Unresolved` under the SC30 rule.
/// The scope functions (`let`, `run`, `apply`, `also`, `with`, `use`, `takeIf`)
/// are absent for a structural reason instead: they are extension functions
/// invoked as `x.let { }`, and the builtin rung is only ever consulted for a
/// *bare* callee, so an entry here could never match. `checkNotNull` and
/// `requireNotNull` are kept where their bare stems are not — the suffixed
/// forms are not names a repository reaches for.
pub const KOTLIN_BUILTINS: &[&str] = &[
    "TODO",
    "arrayOf",
    "arrayOfNulls",
    "buildList",
    "buildMap",
    "buildSet",
    "checkNotNull",
    "emptyArray",
    "emptyList",
    "emptyMap",
    "emptySet",
    "hashMapOf",
    "hashSetOf",
    "linkedMapOf",
    "listOf",
    "listOfNotNull",
    "mapOf",
    "mutableListOf",
    "mutableMapOf",
    "mutableSetOf",
    "print",
    "println",
    "readLine",
    "readln",
    "readlnOrNull",
    "requireNotNull",
    "setOf",
    "sortedMapOf",
    "sortedSetOf",
];

/// Ruby `Kernel` methods callable with no receiver.
///
/// Every one is a private instance method of `Kernel`, mixed into `Object`, so
/// a bare `puts` in any scope reaches it — no indexed file can declare them.
/// Measured on this corpus, `puts` alone was 50 rows of the defect tier.
///
/// Withheld under the SC30 rule as plausibly repository-declared: `loop`,
/// `open`, `system`, `exec`, `spawn`, `warn`, `load`, `format`, `exit`, `abort`,
/// `sleep`, `rand`, `gets`, `lambda`, `proc`, `freeze`. Enumerable methods
/// (`each`, `map`, `select`) are absent for a structural reason instead: they
/// are called on a receiver, and the builtin rung only ever sees a bare callee.
pub const RUBY_BUILTINS: &[&str] = &[
    "__method__",
    "at_exit",
    "autoload",
    "binding",
    "block_given?",
    "caller",
    "catch",
    "fail",
    "p",
    "pp",
    "print",
    "printf",
    "puts",
    "raise",
    "require",
    "require_relative",
    "sprintf",
    "srand",
    "throw",
];

/// PHP standard-library functions, core subset.
///
/// **Deliberately partial.** PHP's standard library runs to thousands of
/// functions across optional extensions, and this table carries the core that
/// is always present. That is safe in exactly one direction: an unlisted
/// builtin stays `Unresolved`, which over-reports a possible defect, whereas a
/// wrong entry would exempt a real one. Growing it is cheap; the fail-open
/// direction is the reason it can ship incomplete.
///
/// Withheld as plausibly repository-declared: `compact`, `extract`, `key`,
/// `current`, `next`, `prev`, `reset`, `end`, `list`, `range`, `min`, `max`,
/// `abs`, `round`, `sort`, `join`, `implode`, `explode`, `print`, `echo`.
pub const PHP_BUILTINS: &[&str] = &[
    "array_column",
    "array_combine",
    "array_diff",
    "array_fill",
    "array_filter",
    "array_flip",
    "array_intersect",
    "array_key_exists",
    "array_keys",
    "array_map",
    "array_merge",
    "array_pop",
    "array_push",
    "array_reduce",
    "array_reverse",
    "array_search",
    "array_shift",
    "array_slice",
    "array_splice",
    "array_unique",
    "array_unshift",
    "array_values",
    "call_user_func",
    "call_user_func_array",
    "count",
    "func_get_args",
    "function_exists",
    "get_class",
    "gettype",
    "in_array",
    "is_array",
    "is_bool",
    "is_callable",
    "is_float",
    "is_int",
    "is_null",
    "is_numeric",
    "is_object",
    "is_scalar",
    "is_string",
    "iterator_to_array",
    "json_decode",
    "json_encode",
    "preg_match",
    "preg_match_all",
    "preg_quote",
    "preg_replace",
    "preg_split",
    "sprintf",
    "str_contains",
    "str_ends_with",
    "str_pad",
    "str_repeat",
    "str_replace",
    "str_split",
    "str_starts_with",
    "strlen",
    "strpos",
    "strtolower",
    "strtoupper",
    "substr",
    "trim",
    "var_dump",
    "var_export",
];

/// The runtime that supplies `name` as a global, if one does.
///
/// Only ever consulted for a *bare* callee in a JavaScript-family file. A host
/// global is a property of the global object, so `x.setTimeout()` is a method
/// on some object `x` and proves nothing about the runtime; and no other
/// language family has a global object for these names to live on.
pub fn host_global_environment(family: LangFamily, name: &str) -> Option<&'static str> {
    if family != LangFamily::JsTs {
        return None;
    }
    HOST_GLOBALS
        .binary_search_by_key(&name, |(global, _)| *global)
        .ok()
        .map(|index| HOST_GLOBALS[index].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `binary_search` is a silent liar on an unsorted slice — it returns `Err`
    /// for names that are present, so builtins would quietly fall through to
    /// `Unresolved` and this whole module would do nothing. Sorting is not
    /// visually obvious in a 70-entry list, so it is asserted.
    #[test]
    fn every_builtin_table_is_sorted_and_unique() {
        for (label, table) in [
            ("go", GO_BUILTINS),
            ("python", PYTHON_BUILTINS),
            ("rust", RUST_BUILTINS),
            ("js", JS_BUILTINS),
            ("swift", SWIFT_BUILTINS),
            ("swift prelude types", SWIFT_PRELUDE_TYPES),
            ("swift xctest types", SWIFT_XCTEST_TYPES),
            ("kotlin", KOTLIN_BUILTINS),
            ("ruby", RUBY_BUILTINS),
            ("php", PHP_BUILTINS),
            ("rust prelude types", RUST_PRELUDE_TYPES),
            ("shell", SHELL_BUILTINS),
        ] {
            let mut sorted = table.to_vec();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(
                sorted.as_slice(),
                table,
                "{label} builtin table must be sorted and duplicate-free for \
                 binary_search to find its entries"
            );
        }
        let sdk_names: Vec<&str> = SWIFT_SDK_TYPES.iter().map(|(name, _)| *name).collect();
        let mut sdk_sorted = sdk_names.clone();
        sdk_sorted.sort_unstable();
        sdk_sorted.dedup();
        assert_eq!(
            sdk_sorted, sdk_names,
            "SWIFT_SDK_TYPES must be sorted by name and duplicate-free"
        );
    }

    /// The same guard as above, for the table `host_global_environment`
    /// searches. `binary_search_by_key` is exactly as silently wrong on an
    /// unsorted slice, and this table is hand-maintained from several
    /// specifications, so a new entry is easy to drop in the wrong place.
    #[test]
    fn the_host_global_table_is_sorted_and_unique() {
        let names: Vec<&str> = HOST_GLOBALS.iter().map(|(name, _)| *name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted, names,
            "the host-global table must be sorted and duplicate-free for \
             binary_search_by_key to find its entries"
        );
        for (name, environment) in HOST_GLOBALS {
            assert!(
                matches!(*environment, "web" | "node" | "web+node"),
                "{name:?} names an environment {environment:?} that is not one \
                 of the three this crate can cite"
            );
        }
    }

    /// X48. The object table, held to the same two properties, plus the one
    /// that keeps the two tables from answering each other's question: a
    /// *function* the runtime injects and an *object* it injects are asked
    /// about in different positions — callee and receiver root — and a name in
    /// both would make the answer depend on which position it was written in.
    #[test]
    fn the_host_global_object_table_is_sorted_unique_and_disjoint() {
        let names: Vec<&str> = HOST_GLOBAL_OBJECTS.iter().map(|(name, _)| *name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted, names,
            "the host-global-object table must be sorted and duplicate-free for \
             binary_search_by_key to find its entries"
        );
        for (name, environment) in HOST_GLOBAL_OBJECTS {
            assert!(
                matches!(*environment, "web" | "node" | "web+node" | "ecma"),
                "{name:?} names an environment {environment:?} that is not one \
                 of the four this crate can cite"
            );
            assert!(
                HOST_GLOBALS
                    .binary_search_by_key(name, |(global, _)| *global)
                    .is_err(),
                "{name:?} is claimed as both a host global function and a host \
                 global object; the tier would then depend on the position the \
                 name was written in rather than on evidence"
            );
        }
    }

    /// Two tiers that answer the same question with different authorities must
    /// not both claim a name: the classification would then depend on which
    /// rung the ladder happens to check first, which is not evidence.
    ///
    /// `Builtin` means ECMA-262 declares it. `HostGlobal` means a browser or
    /// Node does. Nothing is both, and this asserts it rather than trusting the
    /// two lists to have been curated consistently.
    #[test]
    fn no_name_is_both_a_language_builtin_and_a_host_global() {
        for (name, environment) in HOST_GLOBALS {
            assert!(
                !is_builtin(LangFamily::JsTs, name),
                "{name:?} is claimed by both JS_BUILTINS and HOST_GLOBALS \
                 ({environment}); exactly one specification must own it"
            );
        }
    }

    /// The host tier is *only* about names the runtime supplies as callables.
    ///
    /// A host **object** — `console`, `window`, `process` — cannot legitimately
    /// appear as a callee, so a row saying it did is evidence of an extraction
    /// defect and must survive in the tier that means "possible defect".
    /// Admitting them would have converted that signal into an "expected"
    /// label, which is the one failure mode this whole module exists to avoid.
    #[test]
    fn host_objects_are_not_admitted_as_host_globals() {
        for object in [
            "console",
            "document",
            "window",
            "globalThis",
            "process",
            "navigator",
            "location",
            "localStorage",
            "sessionStorage",
            "history",
            "performance",
            "crypto",
            "self",
            "module",
            "exports",
            "__dirname",
        ] {
            assert_eq!(
                host_global_environment(LangFamily::JsTs, object),
                None,
                "{object:?} is a host object, not a host function: a call whose \
                 callee is {object:?} is an extraction defect, not an expected \
                 runtime call"
            );
        }
    }

    #[test]
    fn host_globals_are_recognised_only_for_the_javascript_family() {
        assert_eq!(
            host_global_environment(LangFamily::JsTs, "setTimeout"),
            Some("web+node")
        );
        assert_eq!(
            host_global_environment(LangFamily::JsTs, "requestAnimationFrame"),
            Some("web")
        );
        assert_eq!(
            host_global_environment(LangFamily::JsTs, "setImmediate"),
            Some("node")
        );

        // No other family has a global object for these to live on, and a Go
        // or Python function called `require` is the repository's own.
        for family in [
            LangFamily::Go,
            LangFamily::Python,
            LangFamily::Rust,
            LangFamily::CStyle,
            LangFamily::Generic,
        ] {
            assert_eq!(
                host_global_environment(family, "setTimeout"),
                None,
                "{family:?} has no host global object"
            );
        }

        // Names deliberately left out because the repository is at least as
        // likely to declare them as the platform is.
        for ambiguous in [
            "open",
            "close",
            "print",
            "stop",
            "focus",
            "confirm",
            "prompt",
            "Event",
            "Request",
            "Response",
            "Headers",
            "File",
            "Image",
            "Worker",
            "Notification",
        ] {
            assert_eq!(
                host_global_environment(LangFamily::JsTs, ambiguous),
                None,
                "{ambiguous:?} is a real Window member but too plausible a \
                 user-declared name to claim as expected"
            );
        }
    }

    /// The audit of the four language tables against their specifications
    /// (class 2 of the residue read). Every name here was measured in the
    /// `unresolved` tier of a real build *before* it was added, or is in the
    /// same specification clause as one that was.
    #[test]
    fn the_language_tables_cover_their_specifications() {
        // ECMA-262 native error types — `TypeError` alone was 18 rows.
        for error in [
            "TypeError",
            "RangeError",
            "SyntaxError",
            "ReferenceError",
            "EvalError",
            "URIError",
            "AggregateError",
        ] {
            assert!(
                is_builtin(LangFamily::JsTs, error),
                "{error:?} is an ECMA-262 standard constructor"
            );
        }
        // ECMA-262 typed arrays and buffers — `Float64Array` was 4 rows.
        for typed in [
            "Float64Array",
            "Float32Array",
            "Float16Array",
            "Int8Array",
            "Uint8Array",
            "Uint8ClampedArray",
            "Int16Array",
            "Uint16Array",
            "Int32Array",
            "Uint32Array",
            "BigInt64Array",
            "BigUint64Array",
            "ArrayBuffer",
            "SharedArrayBuffer",
            "DataView",
        ] {
            assert!(
                is_builtin(LangFamily::JsTs, typed),
                "{typed:?} is an ECMA-262 standard constructor"
            );
        }
        for other in ["Function", "WeakRef", "FinalizationRegistry"] {
            assert!(is_builtin(LangFamily::JsTs, other));
        }

        // Namespace objects are not callable and must not be claimed.
        for namespace in ["Math", "JSON", "Reflect", "Atomics", "Intl", "globalThis"] {
            assert!(
                !is_builtin(LangFamily::JsTs, namespace),
                "{namespace:?} is a namespace object, not a constructor"
            );
        }

        // CPython 3.10–3.13 additions to `builtins`.
        for name in [
            "ExceptionGroup",
            "BaseExceptionGroup",
            "EncodingWarning",
            "PythonFinalizationError",
        ] {
            assert!(is_builtin(LangFamily::Python, name));
        }
        // Injected by `site`, not by the language.
        for name in ["exit", "quit", "copyright", "credits", "license"] {
            assert!(
                !is_builtin(LangFamily::Python, name),
                "{name:?} comes from the site module, not from builtins"
            );
        }

        // std macros usable with no `use` at all.
        for macro_name in [
            "cfg",
            "concat",
            "stringify",
            "include_str",
            "include_bytes",
            "include",
            "module_path",
            "option_env",
            "compile_error",
            "thread_local",
        ] {
            assert!(
                is_builtin(LangFamily::Rust, macro_name),
                "{macro_name:?} is a std macro available without any import"
            );
        }
        // Excluded because a plain function of the same name is plausible and
        // the extractor records both spellings identically.
        for ambiguous in ["env", "file", "line", "column"] {
            assert!(
                !is_builtin(LangFamily::Rust, ambiguous),
                "{ambiguous:?} is too plausible a function name to claim"
            );
        }

        // Go's predeclared identifier list is closed and already complete;
        // this pins that, including the 1.21 additions.
        for name in ["min", "max", "clear", "any", "comparable"] {
            assert!(is_builtin(LangFamily::Go, name));
        }
        // Predeclared but not callable: a call to one of these is a defect.
        for constant in ["true", "false", "iota", "nil"] {
            assert!(
                !is_builtin(LangFamily::Go, constant),
                "{constant:?} is a predeclared constant, not a callable"
            );
        }
    }

    #[test]
    fn builtins_are_recognised_per_family_and_do_not_leak_across_families() {
        assert!(is_builtin(LangFamily::Go, "len"));
        assert!(is_builtin(LangFamily::Go, "append"));
        assert!(is_builtin(LangFamily::Go, "string"));
        assert!(is_builtin(LangFamily::Python, "print"));
        assert!(is_builtin(LangFamily::Rust, "Some"));
        assert!(is_builtin(LangFamily::Rust, "println"));
        assert!(is_builtin(LangFamily::JsTs, "parseInt"));

        // `append` is Go's and `len` is not JS's — a single shared table would
        // silently exempt the wrong language's real calls.
        assert!(!is_builtin(LangFamily::Python, "append"));
        assert!(!is_builtin(LangFamily::JsTs, "len"));
        assert!(!is_builtin(LangFamily::Go, "Some"));

        // Families with no curated set must classify nothing.
        assert!(!is_builtin(LangFamily::CStyle, "len"));
        assert!(!is_builtin(LangFamily::Generic, "print"));

        // Library API is never a builtin: these must stay visible as genuine
        // unresolved calls rather than being labelled expected.
        for method in ["unwrap", "to_string", "Fatalf", "TrimSpace", "useState"] {
            for family in [
                LangFamily::Go,
                LangFamily::Python,
                LangFamily::Rust,
                LangFamily::JsTs,
            ] {
                assert!(
                    !is_builtin(family, method),
                    "{method:?} is library API, not a language builtin"
                );
            }
        }
    }

    #[test]
    fn swift_sdk_module_requires_the_file_to_have_imported_it() {
        assert_eq!(swift_sdk_module(["Foundation"], "URL"), Some("Foundation"));
        assert_eq!(swift_sdk_module(["Foundation"], "String"), None);
        assert_eq!(swift_sdk_module(["SwiftUI"], "URL"), None);
        assert_eq!(
            swift_sdk_module(["XCTest"], "XCTAssertEqual"),
            Some("XCTest")
        );
        assert_eq!(swift_sdk_module(["XCTest"], "XCTestCase"), Some("XCTest"));
        assert_eq!(swift_sdk_module(["Foundation"], "XCTAssertEqual"), None);
        assert_eq!(swift_sdk_module(["AppKit"], "NSView"), Some("AppKit"));
        assert_eq!(swift_sdk_module(["UIKit"], "UIView"), Some("UIKit"));
        assert_eq!(swift_sdk_module(["SwiftUI"], "View"), Some("SwiftUI"));
        assert_eq!(swift_sdk_module(std::iter::empty::<&str>(), "URL"), None);
    }
}
