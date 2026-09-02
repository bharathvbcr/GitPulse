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
pub(crate) mod java;
pub(crate) mod jvm_dotnet;
pub(crate) mod kotlin;
pub mod lua;
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
        "java" => java::extract_java_call(node, source, file_symbol_name, calls, references),
        "kotlin" => kotlin::extract_kotlin_call(node, source, file_symbol_name, calls, references),
        // Luau is a Lua superset and shares its node kinds; verified identical
        // by the agent that wrote the module against both grammars.
        "lua" | "luau" => lua::extract_lua_call(node, source, file_symbol_name, calls, references),
        "php" => php::extract_php_calls(node, source, file_symbol_name, calls, references),
        "r" => r::extract_r_call(node, source, file_symbol_name, calls, references),
        "ruby" => ruby::extract_ruby_calls(node, source, file_symbol_name, calls, references),
        "scala" => scala::extract_scala_call(node, source, file_symbol_name, calls, references),
        "swift" => swift::extract_swift_call(node, source, file_symbol_name, calls, references),
        _ => {}
    }
}

/// Languages whose calls this module extracts.
///
/// Read by the coverage report so "this language has no call graph" is a stated
/// fact rather than an indistinguishable zero. Kept sorted; the test pins it.
pub const CALL_EXTRACTION_LANGUAGES: &[&str] = &[
    "csharp", "dart", "java", "kotlin", "lua", "luau", "php", "r", "ruby", "scala", "swift",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_covered_language_list_is_sorted_and_unique() {
        let mut sorted = CALL_EXTRACTION_LANGUAGES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.as_slice(),
            CALL_EXTRACTION_LANGUAGES,
            "the list is binary-searched and reported to consumers; keep it sorted and unique"
        );
    }
}
