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
