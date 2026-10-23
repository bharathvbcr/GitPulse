use super::*;
use std::time::{Duration, Instant};

/// A C macro body inside [`C_MACRO_BODY_MAX_BYTES`] whose parse is real
/// work: thousands of calls, so the parser is polled many times before it
/// could finish.
fn c_macro_body() -> String {
    let mut body = String::new();
    while body.len() + 32 < 60 * 1024 {
        body.push_str("helper(a, b); other(c); ");
    }
    assert!(
        body.len() <= C_MACRO_BODY_MAX_BYTES,
        "the byte cap stays the bound"
    );
    body
}

#[test]
fn a_c_probe_stops_at_the_file_deadline_and_refuses_the_file() {
    let body = c_macro_body();
    {
        // The file's deadline has already passed when the probe starts: a
        // parse that ran to completion here returned after its deadline.
        let _armed = BudgetGuard::arm(Instant::now());
        let started = Instant::now();
        assert!(
            parse_c_probe("c", &body).is_none(),
            "a probe past the file's deadline must not hand back a tree"
        );
        assert!(walk_overran(), "the probe's stop must refuse the file");
        assert!(started.elapsed() < Duration::from_secs(2));
    }
    // The reused probe parser was reset by the cancel: the next fragment is
    // parsed from its own start, not appended to the cancelled parse.
    let _armed = BudgetGuard::arm(Instant::now() + Duration::from_secs(30));
    let text = "int f(void) { return g(); }";
    let tree = parse_c_probe("c", text).expect("a probe inside its deadline parses");
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    assert_eq!(tree.root_node().end_byte(), text.len());
}

#[test]
fn a_cancelled_parse_leaves_its_parser_ready_for_new_text() {
    let (_, language) = grammar_for("c").expect("the C grammar is linked");
    let mut parser = Parser::new();
    parser.set_language(&language).expect("the C grammar loads");
    let (tree, cancelled) = parse_until(&mut parser, &c_macro_body(), Instant::now());
    assert!(
        cancelled && tree.is_none(),
        "the parse must be cancelled mid-way"
    );
    let text = "int f(void) { return g(); }";
    let (tree, cancelled) =
        parse_until(&mut parser, text, Instant::now() + Duration::from_secs(30));
    let tree = tree.expect("the next parse completes");
    assert!(!cancelled);
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    assert_eq!(tree.root_node().end_byte(), text.len());
}

#[test]
fn a_rust_macro_probe_stops_at_the_file_deadline() {
    let inner = "a.work(), helper(b)";
    let (calls, nested) = {
        let _armed = BudgetGuard::arm(Instant::now() + Duration::from_secs(30));
        probe_macro_body(inner)
    };
    assert!(
        calls.iter().any(|(name, _)| name == "helper"),
        "inside its deadline the probe reads the body: {calls:?}"
    );
    assert!(nested.is_empty());

    let _armed = BudgetGuard::arm(Instant::now());
    let (calls, nested) = probe_macro_body(inner);
    assert!(
        calls.is_empty() && nested.is_empty(),
        "past the deadline the probe neither parses nor falls back to a scan: {calls:?}"
    );
    assert!(walk_overran(), "the probe's stop must refuse the file");
}

fn expire_the_armed_deadline() {
    let deadline = WALK_DEADLINE.with(|slot| slot.get().expect("armed extraction"));
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        std::thread::sleep(remaining);
    }
}

/// Clears the probe hook even when an assertion fails first.
struct ClearProbeHook;
impl Drop for ClearProbeHook {
    fn drop(&mut self) {
        PROBE_START_HOOK.with(|hook| hook.set(None));
    }
}

#[test]
fn a_file_whose_macro_probe_crosses_the_deadline_is_refused_not_published() {
    let _clear = ClearProbeHook;
    let source = format!(
        "#define STAMP(x) {}\nint main(void) {{ return STAMP(1); }}\n",
        c_macro_body()
    );
    PROBE_START_HOOK.with(|hook| hook.set(Some(expire_the_armed_deadline)));
    let started = Instant::now();
    let extraction =
        extract_treesitter_with_budget("stamp.c", "c", &source, Duration::from_millis(300));
    assert!(
        PROBE_START_HOOK.with(|hook| hook.get().is_none()),
        "the macro-body probe must be reached"
    );
    assert!(
        matches!(&extraction.parse_outcome, ParseOutcome::Failed { reason } if reason.contains("budget")),
        "a probe stopped by the deadline must refuse the file, got {:?}",
        extraction.parse_outcome
    );
    assert_eq!(extraction.symbols.len(), 1, "only the File node may remain");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_c_probe_inside_its_deadline_reads_the_macro_body() {
    let extraction = extract_treesitter_with_budget(
        "stamp.c",
        "c",
        "void helper(int);\n#define STAMP(x) helper(x);\nint main(void) { STAMP(1); return 0; }\n",
        Duration::from_secs(5),
    );
    assert_eq!(extraction.parse_outcome, ParseOutcome::Clean);
    assert!(
        extraction
            .calls
            .iter()
            .any(|call| call.callee_name == "helper"
                && call.caller_symbol.as_deref() == Some("stamp.c::STAMP")),
        "{:?}",
        extraction.calls
    );
}

fn implicit_add_receivers(operand: &str) -> (Vec<Option<String>>, Vec<String>) {
    let source = format!("struct V;\nfn f(a: V, b: V) {{ let _ = {operand} + b; }}\n");
    let extraction =
        extract_treesitter_with_budget("ops.rs", "rust", &source, Duration::from_secs(10));
    assert_eq!(extraction.parse_outcome, ParseOutcome::Clean);
    let receivers = extraction
        .rust_implicit_calls
        .iter()
        .filter(|call| call.callee_name == "add")
        .map(|call| call.receiver_expr.clone())
        .collect();
    (receivers, extraction.diagnostics)
}

#[test]
fn shallow_rust_operands_keep_their_receiver_and_say_nothing() {
    for operand in ["a", "(a)", "&a", "(&(a))", "&&a"] {
        let (receivers, diagnostics) = implicit_add_receivers(operand);
        assert_eq!(receivers, vec![Some("a".to_string())], "{operand}");
        assert!(diagnostics.is_empty(), "{operand}: {diagnostics:?}");
    }
}

#[test]
fn a_rust_operand_is_unwrapped_to_the_cap_and_no_further() {
    let at_cap = format!(
        "{}a{}",
        "(".repeat(TYPE_NESTING_MAX_DEPTH),
        ")".repeat(TYPE_NESTING_MAX_DEPTH)
    );
    let (receivers, diagnostics) = implicit_add_receivers(&at_cap);
    assert_eq!(receivers, vec![Some("a".to_string())]);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");

    let past_cap = format!("({at_cap})");
    let (receivers, diagnostics) = implicit_add_receivers(&past_cap);
    assert!(
        receivers.is_empty(),
        "past the cap no receiver is claimed: {receivers:?}"
    );
    assert_eq!(
        diagnostics.len(),
        1,
        "the abstention is recorded once: {diagnostics:?}"
    );
    assert!(
        diagnostics[0].starts_with("rust_typed_operand stopped at 16 levels of nesting 1 time(s)")
    );
}

#[test]
fn a_deep_let_initializer_still_answers_same_name_shadowing() {
    // `let g = ((g()))` calls the item `g`: the binding is not in scope in
    // its own initializer. Asked through `node_contains`, which no longer
    // recurses; shallow and deep answers must agree.
    for depth in [0usize, 3, 40] {
        let source = format!(
            "fn g() -> u8 {{ 1 }}\nfn f() {{ let g = {}g(){}; }}\n",
            "(".repeat(depth),
            ")".repeat(depth)
        );
        let extraction =
            extract_treesitter_with_budget("let.rs", "rust", &source, Duration::from_secs(10));
        assert_eq!(extraction.parse_outcome, ParseOutcome::Clean);
        assert!(
            extraction.calls.iter().any(|call| call.callee_name == "g"
                && call.caller_symbol.as_deref() == Some("let.rs::f")),
            "depth {depth}: {:?}",
            extraction.calls
        );
    }
}
