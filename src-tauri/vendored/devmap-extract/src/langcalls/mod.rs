//! Per-language call extraction for languages served by the generic arm.
//!
//! `extract_node` dispatches declarations for every language, but until now only
//! Python, JS/TS, Go, Rust and the C family had a `calls.push` site — so
//! `impact`, `trace`, dead-code and the PDG answered for every other language
//! from an empty call graph, with no signal separating "no callers" from
//! "callers were never extracted" (SC34).
//!
//! Measured against the Python implementation this port replaces, on a fixture
//! where every file contains exactly one real call: Python recovered calls for
//! Ruby, Swift, PHP, Scala and Lua where this port recovered none. Those five
//! were outright regressions of the migration, not shared gaps.
//!
//! Each language lives in its own module rather than another arm in
//! `treesitter.rs`, which is already 5.6k lines and was the single hottest file
//! in this workspace. The dispatcher below is the only shared surface.

use crate::model::{ExtractedCall, ExtractedReference};
use tree_sitter::Node;

pub(crate) mod csharp;
pub(crate) mod dart;
pub mod erlang;
pub(crate) mod java;
pub(crate) mod jvm_dotnet;
pub(crate) mod kotlin;
pub mod lua;
pub mod nix;
pub mod pascal;
pub(crate) mod php;
pub mod r;
pub(crate) mod ruby;
pub mod scala;
/// Mirrors the declaration emitter's own scope walk so a call's
/// `caller_symbol` is the identity the emitter actually produced.
///
/// Deliberately not `enclosing_callable_qualified`: that helper answers through
/// `enclosing_type_name`, which matches only `class_*`/`trait_item`/`impl_item`,
/// while declarations are named through `generic_symbol_kind`, which also covers
/// interfaces, enums, structs and namespaces. Three agents measured the same
/// divergence independently — a Java method in an `interface` emits `F::I.d`
/// while that helper answers `F::d`, and every such row is an orphaned edge.
pub(crate) mod scope;
pub mod shell;
pub mod solidity;
pub mod sql;
pub mod swift;

/// Route one node to its language's call extractor.
///
/// Returns without doing anything for a language that has no module yet, which
/// is the honest state: a missing arm means the call graph for that language is
/// empty, and `dev map`'s consumers are told so through the language coverage
/// report rather than by silently returning zero.
pub(crate) fn extract_calls(
    lang: &str,
    node: Node,
    source: &str,
    file_symbol_name: &str,
    calls: &mut Vec<ExtractedCall>,
    references: &mut Vec<ExtractedReference>,
) {
    match lang {
        "csharp" => csharp::extract_csharp_call(node, source, file_symbol_name, calls, references),
        "dart" => dart::extract_dart_call(node, source, file_symbol_name, calls, references),
        "erlang" => erlang::extract_erlang_call(node, source, file_symbol_name, calls, references),
        "java" => java::extract_java_call(node, source, file_symbol_name, calls, references),
        "kotlin" => kotlin::extract_kotlin_call(node, source, file_symbol_name, calls, references),
        // Luau is a Lua superset and shares its node kinds; verified identical
        // by the agent that wrote the module against both grammars.
        "lua" | "luau" => lua::extract_lua_call(node, source, file_symbol_name, calls, references),
        "nix" => nix::extract_nix_call(node, source, file_symbol_name, calls, references),
        "pascal" => pascal::extract_pascal_call(node, source, file_symbol_name, calls, references),
        "php" => php::extract_php_calls(node, source, file_symbol_name, calls, references),
        "r" => r::extract_r_call(node, source, file_symbol_name, calls, references),
        "ruby" => ruby::extract_ruby_calls(node, source, file_symbol_name, calls, references),
        "scala" => scala::extract_scala_call(node, source, file_symbol_name, calls, references),
        // `detect_language` maps `.sh`, `.bash` and `.zsh` to one key; all three
        // are parsed by `tree-sitter-bash`, so one arm serves the family.
        "shell" => shell::extract_shell_call(node, source, file_symbol_name, calls, references),
        "solidity" => {
            solidity::extract_solidity_call(node, source, file_symbol_name, calls, references)
        }
        "sql" => sql::extract_sql_call(node, source, file_symbol_name, calls, references),
        "swift" => swift::extract_swift_call(node, source, file_symbol_name, calls, references),
        _ => {}
    }
}
