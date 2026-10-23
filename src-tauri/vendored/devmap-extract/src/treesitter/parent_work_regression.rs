use super::*;

#[test]
fn repeated_ancestor_questions_do_not_reconstruct_the_root_walk() {
    let source: String = (0..500)
        .map(|i| format!("fn f{i}(a: i32) -> i32 {{ external(a) + {i} }}\n"))
        .collect();
    NATIVE_PARENT_READS.with(|count| count.set(0));
    let extraction = extract_treesitter("wide.rs", "rust", &source);
    assert_eq!(extraction.parse_outcome, ParseOutcome::Clean);
    assert_eq!(
        extraction
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Function)
            .count(),
        500
    );
    assert_eq!(
        extraction
            .calls
            .iter()
            .filter(|c| c.callee_name == "external")
            .count(),
        500
    );
    let reads = NATIVE_PARENT_READS.with(Cell::get);
    // This bounds actual expensive native work, independently of CPU speed
    // and debug/release profiles. Every ancestor is in this small tree.
    assert!(
        reads < 500,
        "reconstructed {reads} root walks for 500 functions"
    );
}
