//! The return type a callable's declaration writes.
//!
//! One owner for the per-grammar question "what does this function say it
//! returns", stamped after the walk the way `clonesig` stamps body signatures:
//! the walk emits symbols from dozens of grammar branches, and teaching each of
//! them a field is that many chances to forget one.
//!
//! The text is recorded as written. Whether it names a single nominal type —
//! and not a generic, a tuple, a union or an optional — is the resolver's
//! decision (`Resolver::admissible_nominal_type`), so the rule has one home.

use tree_sitter::Node;

use crate::model::{ExtractedSymbol, SymbolKind};

/// Longest return-type text kept. A real annotation is a few words; anything
/// longer is an inline object or function type, which names no nominal type
/// and would only inflate every payload that carries it.
const MAX_RETURN_TYPE_BYTES: usize = 200;

/// Stamp `return_type` on every function and method in `symbols`.
pub fn stamp_return_types(symbols: &mut [ExtractedSymbol], root: Node, source: &str) {
    for symbol in symbols.iter_mut() {
        if !matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method) {
            continue;
        }
        let start = symbol.span.start_byte;
        let end = symbol.span.end_byte;
        if end <= start || end > source.len() {
            continue;
        }
        let Some(node) = root.descendant_for_byte_range(start, end) else {
            continue;
        };
        symbol.return_type =
            callable_of(node).and_then(|callable| written_return(callable, source));
    }
}

/// The callable node a symbol's span names.
///
/// Usually the span *is* the declaration. Two wrappers are seen through: a
/// declarator whose value is a function (`const make = (): Svc => …`), and an
/// export or decorator around the declaration.
fn callable_of(node: Node) -> Option<Node> {
    if is_callable(node.kind()) {
        return Some(node);
    }
    for field in ["value", "declaration", "definition"] {
        if let Some(inner) = node.child_by_field_name(field) {
            if is_callable(inner.kind()) {
                return Some(inner);
            }
        }
    }
    None
}

fn is_callable(kind: &str) -> bool {
    matches!(
        kind,
        // Go
        "function_declaration"
            | "method_declaration"
            // TypeScript / JavaScript
            | "method_definition"
            | "arrow_function"
            | "function_expression"
            | "function"
            | "generator_function_declaration"
            // Python
            | "function_definition"
            // Rust
            | "function_item"
            | "function_signature_item"
            // TypeScript interface/abstract members
            | "method_signature"
            | "abstract_method_signature"
    )
}

/// The written return type of `callable`, trimmed, or `None`.
///
/// Grammar fields, as declared by each grammar's `node-types.json`: Go
/// `result`, Java `type`, and `return_type` for TypeScript, Python and Rust.
fn written_return(callable: Node, source: &str) -> Option<String> {
    let node = callable
        .child_by_field_name("return_type")
        .or_else(|| callable.child_by_field_name("result"))
        .or_else(|| {
            (callable.kind() == "method_declaration")
                .then(|| callable.child_by_field_name("type"))
                .flatten()
        })?;
    let node = if node.kind() == "parameter_list" {
        // Go's parenthesised result list. One unnamed result is a type in
        // parentheses; anything more — `(T, error)` — is a tuple the caller
        // destructures, which names no single value.
        let mut cursor = node.walk();
        let declarations: Vec<Node> = node
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "parameter_declaration")
            .collect();
        // A named single result, `(r *Registry)`, is still one value.
        let [only] = declarations.as_slice() else {
            return None;
        };
        only.child_by_field_name("type")?
    } else {
        node
    };
    let text = node.utf8_text(source.as_bytes()).ok()?;
    // TypeScript's `type_annotation` keeps its colon; Python string
    // annotations keep their quotes.
    let text = text
        .trim()
        .trim_start_matches(':')
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .trim();
    (!text.is_empty() && text.len() <= MAX_RETURN_TYPE_BYTES).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use crate::extract_file;

    fn return_of(path: &str, source: &str, name: &str) -> Option<String> {
        extract_file(path, source)
            .symbols
            .into_iter()
            .find(|symbol| symbol.name == name)
            .unwrap_or_else(|| panic!("{name} not extracted from {path}"))
            .return_type
    }

    #[test]
    fn go_records_a_single_result_and_refuses_a_tuple() {
        let source = "package reg\n\ntype Registry struct{}\n\n\
                      func NewRegistry() *Registry { return nil }\n\
                      func Wrapped() (*Registry) { return nil }\n\
                      func Open() (*Registry, error) { return nil, nil }\n\
                      func Nothing() {}\n\
                      func (r *Registry) Clone() Registry { return *r }\n";
        assert_eq!(
            return_of("r.go", source, "NewRegistry").as_deref(),
            Some("*Registry")
        );
        assert_eq!(
            return_of("r.go", source, "Wrapped").as_deref(),
            Some("*Registry")
        );
        assert_eq!(return_of("r.go", source, "Open"), None);
        assert_eq!(return_of("r.go", source, "Nothing"), None);
        assert_eq!(
            return_of("r.go", source, "Clone").as_deref(),
            Some("Registry")
        );
    }

    #[test]
    fn typescript_reads_functions_methods_and_arrows() {
        let source = "export class Svc { clone(): Svc { return this; } }\n\
                      export function make(a: number): Svc { return new Svc(); }\n\
                      export const arrow = (): Svc => new Svc();\n\
                      export async function load(): Promise<Svc> { return new Svc(); }\n\
                      export function bare() { return 1; }\n";
        assert_eq!(return_of("f.ts", source, "make").as_deref(), Some("Svc"));
        assert_eq!(return_of("f.ts", source, "clone").as_deref(), Some("Svc"));
        assert_eq!(return_of("f.ts", source, "arrow").as_deref(), Some("Svc"));
        assert_eq!(
            return_of("f.ts", source, "load").as_deref(),
            Some("Promise<Svc>")
        );
        assert_eq!(return_of("f.ts", source, "bare"), None);
    }

    #[test]
    fn python_reads_the_arrow_and_unquotes_a_forward_reference() {
        let source = "class Svc:\n    pass\n\n\
                      def make() -> Svc:\n    return Svc()\n\n\
                      def later() -> 'Svc':\n    return Svc()\n\n\
                      def bare():\n    return 1\n";
        assert_eq!(return_of("f.py", source, "make").as_deref(), Some("Svc"));
        assert_eq!(return_of("f.py", source, "later").as_deref(), Some("Svc"));
        assert_eq!(return_of("f.py", source, "bare"), None);
    }

    #[test]
    fn rust_reads_self_and_named_types() {
        let source = "pub struct Svc;\n\
                      impl Svc { pub fn new() -> Self { Svc } }\n\
                      pub fn make() -> Svc { Svc }\n\
                      pub fn unit() {}\n";
        assert_eq!(return_of("f.rs", source, "new").as_deref(), Some("Self"));
        assert_eq!(return_of("f.rs", source, "make").as_deref(), Some("Svc"));
        assert_eq!(return_of("f.rs", source, "unit"), None);
    }
}
