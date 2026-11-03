//! Multi-language scoping and visibility predicates.
//!
//! Provides mathematically rigorous scoping and visibility evaluations across
//! Rust, Go, TypeScript/JavaScript, Python, and Swift, fulfilling Milestone 3 (F23).

use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

/// Three-valued visibility logic: Invariant I3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VisibilityOutcome {
    Visible,
    Hidden,
    Unknown,
}

// ---------------------------------------------------------------------------
// 1. Rust Scoping & Visibility ($V_{\text{rust}}$)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RustVisibility {
    Public,
    PublicCrate,
    PublicSuper,
    PublicInPath(Vec<String>),
    Private,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RustModuleInstance {
    pub crate_root: PathBuf,
    pub module_path: Vec<String>,
    pub file_path: PathBuf,
}

impl RustModuleInstance {
    pub fn new(
        crate_root: impl Into<PathBuf>,
        module_path: Vec<String>,
        file_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            crate_root: crate_root.into(),
            module_path,
            file_path: file_path.into(),
        }
    }

    pub fn parent(&self) -> Option<Vec<String>> {
        if self.module_path.is_empty() {
            None
        } else {
            let mut p = self.module_path.clone();
            p.pop();
            Some(p)
        }
    }
}

/// Evaluates Rust visibility predicate $V_{\text{rust}}(\text{target}, \text{caller})$.
pub fn rust_visibility_predicate(
    target_vis: &RustVisibility,
    target: &RustModuleInstance,
    caller: &RustModuleInstance,
) -> VisibilityOutcome {
    match target_vis {
        RustVisibility::Public => VisibilityOutcome::Visible,
        RustVisibility::PublicCrate => {
            if target.crate_root == caller.crate_root {
                VisibilityOutcome::Visible
            } else {
                VisibilityOutcome::Hidden
            }
        }
        RustVisibility::PublicSuper => {
            if target.crate_root != caller.crate_root {
                VisibilityOutcome::Hidden
            } else if let Some(parent) = target.parent() {
                if caller.module_path.starts_with(&parent) {
                    VisibilityOutcome::Visible
                } else {
                    VisibilityOutcome::Hidden
                }
            } else {
                VisibilityOutcome::Hidden
            }
        }
        RustVisibility::PublicInPath(path) => {
            if target.crate_root != caller.crate_root {
                VisibilityOutcome::Hidden
            } else if caller.module_path.starts_with(path) {
                VisibilityOutcome::Visible
            } else {
                VisibilityOutcome::Hidden
            }
        }
        RustVisibility::Private => {
            if target.crate_root == caller.crate_root && target.module_path == caller.module_path {
                VisibilityOutcome::Visible
            } else {
                VisibilityOutcome::Hidden
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 2. Go Scoping, Struct Embedding & Interface Dispatch ($V_{\text{go}}$)
// ---------------------------------------------------------------------------

pub const DISPATCH_FANOUT_CAP: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GoPackageKey {
    pub dir_path: String,
    pub pkg_name: String,
}

impl GoPackageKey {
    pub fn new(dir_path: impl Into<String>, pkg_name: impl Into<String>) -> Self {
        Self {
            dir_path: dir_path.into(),
            pkg_name: pkg_name.into(),
        }
    }
}

/// Checks whether an identifier is exported in Go (starts with uppercase Unicode).
pub fn go_is_exported(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_uppercase())
}

/// Evaluates Go package-level visibility predicate.
pub fn go_visibility_predicate(
    target_name: &str,
    target_pkg: &GoPackageKey,
    caller_pkg: &GoPackageKey,
) -> VisibilityOutcome {
    if target_pkg == caller_pkg || go_is_exported(target_name) {
        VisibilityOutcome::Visible
    } else {
        VisibilityOutcome::Hidden
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoStructType {
    pub canonical_name: String,
    pub embedded_fields: Vec<String>,
    pub declared_methods: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotedMember {
    pub method_name: String,
    pub via_type: String,
    pub depth: usize,
}

/// Transitive struct embedding flattening via BFS up to `max_depth`.
pub fn flatten_embedded_types(
    base_struct: &GoStructType,
    type_index: &std::collections::HashMap<String, GoStructType>,
    max_depth: usize,
) -> Vec<PromotedMember> {
    let mut promoted = Vec::new();
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();

    visited.insert(base_struct.canonical_name.clone());
    queue.push_back((base_struct, 0));

    while let Some((curr, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        for embedded_type_name in &curr.embedded_fields {
            if let Some(target_type) = type_index.get(embedded_type_name) {
                if visited.insert(target_type.canonical_name.clone()) {
                    for method in &target_type.declared_methods {
                        // Outer struct declarations shadow embedded declarations
                        if !base_struct.declared_methods.contains(method)
                            && !promoted
                                .iter()
                                .any(|p: &PromotedMember| &p.method_name == method)
                        {
                            promoted.push(PromotedMember {
                                method_name: method.clone(),
                                via_type: curr.canonical_name.clone(),
                                depth: depth + 1,
                            });
                        }
                    }
                    queue.push_back((target_type, depth + 1));
                }
            }
        }
    }
    promoted
}

// ---------------------------------------------------------------------------
// 3. TypeScript / JavaScript Scoping & Path Resolvers ($V_{\text{ts}}$)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TsConfigPathResolver {
    pub base_url: PathBuf,
    pub paths: Vec<(String, Vec<String>)>,
}

impl TsConfigPathResolver {
    pub fn new(base_url: impl Into<PathBuf>, paths: Vec<(String, Vec<String>)>) -> Self {
        Self {
            base_url: base_url.into(),
            paths,
        }
    }

    /// Resolves an import specifier according to tsconfig paths.
    pub fn resolve(&self, specifier: &str) -> Option<PathBuf> {
        for (pattern, targets) in &self.paths {
            // Only the first mapped target is used; an empty target list falls
            // through to the next pattern.
            if pattern == specifier {
                if let Some(target) = targets.first() {
                    return Some(self.base_url.join(target));
                }
            } else if pattern.ends_with("/*")
                && specifier.starts_with(&pattern[..pattern.len() - 1])
            {
                let prefix = &pattern[..pattern.len() - 2];
                let suffix = &specifier[prefix.len() + 1..];
                if let Some(target) = targets.first() {
                    let target_prefix = if target.ends_with("/*") {
                        &target[..target.len() - 2]
                    } else {
                        target.as_str()
                    };
                    return Some(self.base_url.join(target_prefix).join(suffix));
                }
            }
        }
        None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsObjectFact {
    pub object_id: String,
    pub spread_sources: Vec<String>,
    pub explicit_fields: Vec<String>,
}

// ---------------------------------------------------------------------------
// 4. Python Scoping & Dynamic Mixin Sentinel ($V_{\text{py}}$)
// ---------------------------------------------------------------------------

pub use devmap_extract::model::OPAQUE_BASE;

pub fn is_opaque_heritage(bases: &[String]) -> bool {
    bases.iter().any(|b| b == OPAQUE_BASE)
}

pub fn is_script_entry_point(path: &str, content: &str) -> bool {
    let p = Path::new(path);
    if let Some(parent) = p.parent() {
        let parent_str = parent.to_string_lossy();
        if parent_str.ends_with("scripts")
            || parent_str.ends_with("bin")
            || parent_str.ends_with("tools")
        {
            return true;
        }
    }
    content.contains("__name__ == '__main__'") || content.contains("__name__ == \"__main__\"")
}

// ---------------------------------------------------------------------------
// 5. Swift Access Control & TSPL Visibility ($V_{\text{swift}}$)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SwiftAccessLevel {
    Private = 0,
    Fileprivate = 1,
    Internal = 2,
    Package = 3,
    Public = 4,
    Open = 5,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwiftDeclFact {
    pub qualified_name: String,
    pub span_start: usize,
    pub owner_path: String,
    pub access: SwiftAccessLevel,
    pub is_static: bool,
    pub in_extension: bool,
}

pub fn swift_effective_access(
    member_access: SwiftAccessLevel,
    container_access: SwiftAccessLevel,
) -> SwiftAccessLevel {
    std::cmp::min(member_access, container_access)
}

/// Evaluates Swift visibility predicate $V_{\text{swift}}(\text{member}, \text{caller})$.
pub fn swift_visibility_predicate(
    member: &SwiftDeclFact,
    container_access: SwiftAccessLevel,
    caller_file: &str,
    declaring_file: &str,
    caller_module: &str,
    declaring_module: &str,
    has_testable_import: bool,
) -> VisibilityOutcome {
    let effective = swift_effective_access(member.access, container_access);
    match effective {
        SwiftAccessLevel::Private => {
            if caller_file == declaring_file {
                VisibilityOutcome::Visible
            } else {
                VisibilityOutcome::Hidden
            }
        }
        SwiftAccessLevel::Fileprivate => {
            if caller_file == declaring_file {
                VisibilityOutcome::Visible
            } else {
                VisibilityOutcome::Hidden
            }
        }
        SwiftAccessLevel::Internal => {
            if caller_module == declaring_module || has_testable_import {
                VisibilityOutcome::Visible
            } else {
                VisibilityOutcome::Hidden
            }
        }
        SwiftAccessLevel::Package => {
            // If in same module or package
            if caller_module == declaring_module {
                VisibilityOutcome::Visible
            } else {
                VisibilityOutcome::Hidden
            }
        }
        SwiftAccessLevel::Public | SwiftAccessLevel::Open => VisibilityOutcome::Visible,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_visibility_predicate() {
        let root = PathBuf::from("/crate");
        let target =
            RustModuleInstance::new(&root, vec!["foo".into(), "bar".into()], "/crate/foo/bar.rs");
        let sibling =
            RustModuleInstance::new(&root, vec!["foo".into(), "baz".into()], "/crate/foo/baz.rs");
        let inside =
            RustModuleInstance::new(&root, vec!["foo".into(), "bar".into()], "/crate/foo/bar.rs");
        let outside =
            RustModuleInstance::new("/other_crate", vec!["foo".into()], "/other_crate/foo.rs");

        assert_eq!(
            rust_visibility_predicate(&RustVisibility::Public, &target, &outside),
            VisibilityOutcome::Visible
        );
        assert_eq!(
            rust_visibility_predicate(&RustVisibility::PublicCrate, &target, &sibling),
            VisibilityOutcome::Visible
        );
        assert_eq!(
            rust_visibility_predicate(&RustVisibility::PublicCrate, &target, &outside),
            VisibilityOutcome::Hidden
        );
        assert_eq!(
            rust_visibility_predicate(&RustVisibility::Private, &target, &inside),
            VisibilityOutcome::Visible
        );
        assert_eq!(
            rust_visibility_predicate(&RustVisibility::Private, &target, &sibling),
            VisibilityOutcome::Hidden
        );
    }

    #[test]
    fn test_go_scoping_and_embedding() {
        let pkg1 = GoPackageKey::new("/repo/pkg1", "pkg1");
        let pkg2 = GoPackageKey::new("/repo/pkg2", "pkg2");

        assert_eq!(
            go_visibility_predicate("ExportedFn", &pkg1, &pkg2),
            VisibilityOutcome::Visible
        );
        assert_eq!(
            go_visibility_predicate("unexportedFn", &pkg1, &pkg2),
            VisibilityOutcome::Hidden
        );
        assert_eq!(
            go_visibility_predicate("unexportedFn", &pkg1, &pkg1),
            VisibilityOutcome::Visible
        );

        let mut type_index = std::collections::HashMap::new();
        type_index.insert(
            "Inner".to_string(),
            GoStructType {
                canonical_name: "Inner".to_string(),
                embedded_fields: vec![],
                declared_methods: vec!["InnerMethod".to_string(), "ShadowedMethod".to_string()],
            },
        );

        let outer = GoStructType {
            canonical_name: "Outer".to_string(),
            embedded_fields: vec!["Inner".to_string()],
            declared_methods: vec!["ShadowedMethod".to_string()],
        };

        let promoted = flatten_embedded_types(&outer, &type_index, 5);
        assert_eq!(promoted.len(), 1);
        assert_eq!(promoted[0].method_name, "InnerMethod");
    }

    #[test]
    fn test_swift_access_levels() {
        let fact = SwiftDeclFact {
            qualified_name: "MyClass.foo".to_string(),
            span_start: 10,
            owner_path: "App::MyClass".to_string(),
            access: SwiftAccessLevel::Internal,
            is_static: false,
            in_extension: false,
        };

        assert_eq!(
            swift_visibility_predicate(
                &fact,
                SwiftAccessLevel::Public,
                "Caller.swift",
                "Decl.swift",
                "App",
                "App",
                false
            ),
            VisibilityOutcome::Visible
        );
        assert_eq!(
            swift_visibility_predicate(
                &fact,
                SwiftAccessLevel::Public,
                "Caller.swift",
                "Decl.swift",
                "OtherModule",
                "App",
                false
            ),
            VisibilityOutcome::Hidden
        );
        assert_eq!(
            swift_visibility_predicate(
                &fact,
                SwiftAccessLevel::Public,
                "Caller.swift",
                "Decl.swift",
                "OtherModule",
                "App",
                true
            ),
            VisibilityOutcome::Visible
        );
    }
}
