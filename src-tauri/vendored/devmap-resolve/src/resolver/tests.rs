#[test]
fn evidence_size_keeps_an_escape_bound_for_hostile_names() {
    assert_eq!(super::escaped_evidence_bytes("defs/ordinary.rs"), 16);
    for value in (0u8..=127)
        .map(|byte| char::from(byte).to_string())
        .chain(["é", "\u{85}", "\u{200d}", "\u{2028}", "😀"].map(str::to_owned))
    {
        let debug_bytes = format!("{value:?}").len() - 2;
        assert!(super::escaped_evidence_bytes(&value) >= debug_bytes as u64);
    }
    assert_eq!(super::escaped_evidence_bytes("\"\\\n\0"), 24);
}

#[test]
fn parent_dir_splits_on_either_separator() {
    use super::Resolver;
    assert_eq!(Resolver::parent_dir("a/b.go"), "a");
    assert_eq!(Resolver::parent_dir("a\\b.go"), "a");
    assert_eq!(Resolver::parent_dir("a\\b\\c.go"), "a/b");
    assert_eq!(Resolver::parent_dir("a/b\\c.go"), "a/b");
    assert_eq!(Resolver::parent_dir("b.go"), ".");
    assert_eq!(Resolver::parent_dir("\\b.go"), "/");
}

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
    let result = resolver.resolve_all(&[a, b]).unwrap();
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

#[cfg(feature = "parse")]
fn resolve_one(path: &str, source: &str) -> (devmap_extract::model::Extraction, ResolutionResult) {
    let extraction = extract_file(path, source);
    let mut resolver = Resolver::new();
    resolver.index_extractions(std::slice::from_ref(&extraction));
    let result = resolver
        .resolve_all(std::slice::from_ref(&extraction))
        .expect("resolution budget");
    (extraction, result)
}

#[cfg(feature = "parse")]
fn call_targets(result: &ResolutionResult, source_suffix: &str) -> Vec<(String, String)> {
    result
        .edges
        .iter()
        .filter(|edge| {
            edge.edge_kind == EdgeKind::Calls && edge.source_symbol.ends_with(source_suffix)
        })
        .filter_map(|edge| {
            let kind = edge.resolution.as_ref()?.kind().label().to_string();
            Some((edge.target_symbol.clone(), kind))
        })
        .collect()
}

/// `self::helper()` inside an impl is the module path. `self.helper()` is the
/// method. Both names exist, so binding the path as ReceiverType is the bug.
#[test]
#[cfg(feature = "parse")]
fn self_path_inside_an_impl_binds_the_free_function() {
    let source = "\
fn helper() {}\n\
struct S;\n\
impl S {\n\
    fn helper(&self) {}\n\
    fn run(&self) {\n\
        self::helper();\n\
        self.helper();\n\
        let named = self::helper;\n\
    }\n\
}\n\
fn outside() { self::helper(); }\n\
";
    let (extraction, result) = resolve_one("src/s.rs", source);
    let path_receivers: Vec<_> = extraction
        .calls
        .iter()
        .filter(|call| call.callee_name == "helper")
        .map(|call| call.receiver_expr.as_deref())
        .collect();
    assert!(
        path_receivers.contains(&Some("self::")),
        "the path call must not be stored as the value self; got {path_receivers:?}"
    );
    assert!(
        path_receivers.contains(&Some("self")),
        "the field call must stay the value self; got {path_receivers:?}"
    );

    let from_run = call_targets(&result, "::S.run");
    assert!(
        from_run.iter().any(|(target, kind)| {
            target == "src/s.rs::helper" && kind == "SameFile"
        }),
        "self::helper() must name the free function; got {from_run:?}"
    );
    assert!(
        from_run.iter().any(|(target, kind)| {
            target == "src/s.rs::S.helper" && kind == "ReceiverType"
        }),
        "self.helper() must stay the method; got {from_run:?}"
    );
    assert!(
        !from_run.iter().any(|(target, kind)| {
            target == "src/s.rs::S.helper" && kind != "ReceiverType"
                || target == "src/s.rs::helper" && kind == "ReceiverType"
        }),
        "the path must not be a ReceiverType edge onto the method; got {from_run:?}"
    );

    let named = result.edges.iter().any(|edge| {
        edge.edge_kind == EdgeKind::References
            && edge.source_symbol.ends_with("::S.run")
            && edge.target_symbol == "src/s.rs::helper"
            && edge.resolution.as_ref().is_some_and(|resolution| {
                resolution.kind().label() == "SameFile"
            })
    });
    assert!(
        named,
        "a self::helper value must reference the free function; got {:?}",
        result
            .edges
            .iter()
            .filter(|edge| edge.edge_kind == EdgeKind::References)
            .map(|edge| format!("{} -> {}", edge.source_symbol, edge.target_symbol))
            .collect::<Vec<_>>()
    );

    let from_outside = call_targets(&result, "::outside");
    assert!(
        from_outside.iter().any(|(target, kind)| {
            target == "src/s.rs::helper" && kind == "SameFile"
        }),
        "self::helper() outside an impl must keep naming the free function; got {from_outside:?}"
    );
}

/// No method of the same name. The old value-self guard skipped the path and
/// left the free function with no caller.
#[test]
#[cfg(feature = "parse")]
fn self_path_inside_an_impl_binds_the_free_function_when_no_method_exists() {
    let source = "\
fn helper() {}\n\
struct S;\n\
impl S {\n\
    fn run(&self) { self::helper(); }\n\
}\n\
";
    let (_, result) = resolve_one("src/s.rs", source);
    let from_run = call_targets(&result, "::S.run");
    assert!(
        from_run.iter().any(|(target, kind)| {
            target == "src/s.rs::helper" && kind == "SameFile"
        }),
        "a path with no method homonym must still name the free function; got {from_run:?}"
    );
}

/// A method and no free function. The path must not invent a ReceiverType edge.
#[test]
#[cfg(feature = "parse")]
fn self_path_does_not_bind_a_method_when_the_module_has_no_function() {
    let source = "\
struct S;\n\
impl S {\n\
    fn helper(&self) {}\n\
    fn run(&self) { self::helper(); }\n\
}\n\
";
    let (_, result) = resolve_one("src/s.rs", source);
    let from_run = call_targets(&result, "::S.run");
    assert!(
        from_run.iter().all(|(target, kind)| {
            !(target.ends_with("helper") && kind == "ReceiverType")
        }),
        "self::helper() is not the method; got {from_run:?}"
    );
}

/// The same four facts on a non-ascii path and a non-ascii free function, and
/// on a trait impl, which is still inside a type.
#[test]
#[cfg(feature = "parse")]
fn self_path_on_a_unicode_file_and_a_trait_impl_stays_the_free_function() {
    let source = "\
fn 帮助() {}\n\
fn helper() {}\n\
trait T { fn run(&self); }\n\
struct S;\n\
impl T for S {\n\
    fn run(&self) {\n\
        self::帮助();\n\
        self::helper();\n\
        let named = self::helper;\n\
    }\n\
}\n\
";
    let (extraction, result) = resolve_one("src/配置.rs", source);
    assert!(
        extraction.calls.iter().any(|call| {
            call.callee_name == "帮助" && call.receiver_expr.as_deref() == Some("self::")
        }),
        "unicode path call must keep the module spelling; calls: {:?}",
        extraction
            .calls
            .iter()
            .map(|call| (call.callee_name.as_str(), call.receiver_expr.as_deref()))
            .collect::<Vec<_>>()
    );
    let from_run = call_targets(&result, "::S.run");
    assert!(
        from_run.iter().any(|(target, kind)| {
            target == "src/配置.rs::帮助" && kind == "SameFile"
        }),
        "trait-impl self:: path must name the free function; got {from_run:?}"
    );
    assert!(
        result.edges.iter().any(|edge| {
            edge.edge_kind == EdgeKind::References
                && edge.target_symbol == "src/配置.rs::helper"
                && edge.resolution.as_ref().is_some_and(|resolution| {
                    resolution.kind().label() == "SameFile"
                })
        }),
        "a self::helper value in a unicode path must name the free function; edges {:?}",
        result
            .edges
            .iter()
            .filter(|edge| edge.edge_kind == EdgeKind::References)
            .map(|edge| format!("{} -> {}", edge.source_symbol, edge.target_symbol))
            .collect::<Vec<_>>()
    );
}

/// `self::helper()` written in an inline module is still a path. The module
/// is flattened into the file, and the only `helper` here is a method, so the
/// path must stay unresolved rather than becoming that method.
#[test]
#[cfg(feature = "parse")]
fn self_path_inside_an_inline_module_does_not_bind_a_method() {
    let source = "\
struct S;\n\
impl S {\n\
    fn helper(&self) {}\n\
}\n\
mod child {\n\
    fn run() { self::helper(); }\n\
}\n\
";
    let (extraction, result) = resolve_one("src/s.rs", source);
    assert!(
        extraction.calls.iter().any(|call| {
            call.callee_name == "helper" && call.receiver_expr.as_deref() == Some("self::")
        }),
        "inline-module path must keep the module spelling; calls {:?}",
        extraction
            .calls
            .iter()
            .map(|call| (call.callee_name.as_str(), call.receiver_expr.as_deref()))
            .collect::<Vec<_>>()
    );
    let bound: Vec<_> = result
        .edges
        .iter()
        .filter(|edge| edge.edge_kind == EdgeKind::Calls && edge.target_symbol.ends_with("helper"))
        .map(|edge| {
            format!(
                "{} -> {} {}",
                edge.source_symbol,
                edge.target_symbol,
                edge.resolution
                    .as_ref()
                    .map(|resolution| resolution.kind().label())
                    .unwrap_or("none")
            )
        })
        .collect();
    assert!(
        bound.is_empty(),
        "self::helper() in a module is not the method; got {bound:?}"
    );
}

/// `super::helper()` inside a method that is itself inside `mod tests` is
/// absorbed to this file's module. That must name the free function, not the
/// method of the same name on the enclosing type.
#[test]
#[cfg(feature = "parse")]
fn absorbed_super_inside_a_nested_method_binds_the_free_function() {
    let source = "\
fn helper() {}\n\
struct S;\n\
impl S { fn helper(&self) {} }\n\
mod tests {\n\
    impl S {\n\
        fn run(&self) { super::helper(); }\n\
    }\n\
}\n\
";
    let (extraction, result) = resolve_one("src/s.rs", source);
    assert!(
        extraction.calls.iter().any(|call| {
            call.callee_name == "helper" && call.receiver_expr.as_deref() == Some("self::")
        }),
        "absorbed super must be the module spelling; calls {:?}",
        extraction
            .calls
            .iter()
            .map(|call| (call.callee_name.as_str(), call.receiver_expr.as_deref()))
            .collect::<Vec<_>>()
    );
    let from_run = call_targets(&result, "::S.run");
    assert!(
        from_run.iter().any(|(target, kind)| {
            target == "src/s.rs::helper" && kind == "SameFile"
        }),
        "super::helper() inside the nested method must name the free function; got {from_run:?}"
    );
    assert!(
        from_run.iter().all(|(target, kind)| {
            !(target.ends_with("S.helper") && kind == "ReceiverType")
        }),
        "super::helper() must not bind the method; got {from_run:?}"
    );
}
