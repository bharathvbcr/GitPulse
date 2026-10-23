use crate::content_hash;
use crate::frameworks::extract_framework_routes;
use crate::model::*;
use crate::wiring::extract_wiring_annotations;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use tree_sitter::{Language, Node, Parser};

/// Grammars compiled from vendored C, bound directly to our tree-sitter.
///
/// The published Rust wrappers for these languages pin tree-sitter 0.20, whose
/// `Language` is a distinct type from ours — they cannot be linked at all. The
/// generated parser is fine (ABI 15, which 0.25 accepts), so the wrapper was
/// the only obstacle. Declaring the entry point here keeps these grammars on
/// exactly the same runtime as every other one, with no second tree-sitter in
/// the dependency graph.
pub mod vendored {
    use tree_sitter::Language;
    use tree_sitter_language::LanguageFn;

    unsafe extern "C" {
        fn tree_sitter_vue() -> *const ();
        fn tree_sitter_liquid() -> *const ();
    }

    /// SAFETY: `tree_sitter_vue` is the entry point generated into the vendored
    /// `parser.c`, compiled into this binary by `build.rs`, and returns a
    /// pointer to a `'static` grammar table.
    pub const VUE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_vue) };

    /// SAFETY: as `VUE`.
    pub const LIQUID: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_liquid) };

    pub fn vue() -> Language {
        VUE.into()
    }

    pub fn liquid() -> Language {
        LIQUID.into()
    }
}

mod entry;
pub use self::entry::*;

mod bindings;
pub(crate) use self::bindings::*;

mod walk;
pub(crate) use self::walk::*;

mod lang_helpers;
pub use self::lang_helpers::*;

mod extract_node;
use self::extract_node::*;

mod cfamily;
pub(crate) use self::cfamily::*;

mod calls;
pub(crate) use self::calls::*;

mod rust_use;
pub(crate) use self::rust_use::*;

mod references;
pub(crate) use self::references::*;

mod budget;
pub(crate) use self::budget::*;

mod scope_locals;
pub(crate) use self::scope_locals::*;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod parent_work_regression;

#[cfg(test)]
mod probe_deadlines_and_nesting_caps;
