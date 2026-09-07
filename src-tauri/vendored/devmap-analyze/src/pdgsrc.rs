//! Source → [`FunctionPdgInput`]: the producer the PDG builder never had.
//!
//! [`crate::pdg`] takes a "grammar-owned statement tree" and refuses to infer
//! control flow from line numbers. That is the right contract, and until now
//! nothing in this workspace satisfied it — `FunctionPdgInput` appeared only in
//! `pdg.rs` and its own tests, so the whole analysis was unreachable from a
//! repository. The Python implementation this port replaces
//! (`indexing/graph/pdg/cfg.py`) built the tree with CPython's `ast` module,
//! which is why it was Python-only and could not move.
//!
//! This walks tree-sitter instead, so the same shape can later be produced for
//! any grammar in the workspace. Only Python is implemented here, because only
//! Python was implemented there: porting a one-language analysis as a
//! one-language analysis keeps the comparison honest.
//!
//! # What it claims, and what it does not
//!
//! Definitions and uses are *syntactic*. A name assigned anywhere in the
//! function is a definition at that statement; a name read is a use. There is no
//! scope analysis, no alias tracking and no interprocedural reasoning — the
//! builder above is intra-procedural by construction.
//!
//! The consequence worth stating: **an empty `taint_sinks` means no call matched
//! a sink pattern, not that the statement is safe.** The patterns are the ones
//! the Python implementation used, transcribed rather than extended, and they
//! are a heuristic list of well-known sinks. A finding is evidence; the absence
//! of one is not.

use crate::pdg::{FunctionPdgInput, PdgStatement, PdgStatementKind, MAX_PDG_NESTING_DEPTH};

/// Calls whose arguments are security-sensitive, by the name written at the call
/// site.
///
/// Transcribed from `indexing/graph/pdg/taint.py::_SINK_PATTERNS`. A suffix
/// match, exactly as there: `.execute` matches `cursor.execute` and
/// `conn.execute` alike, because the receiver's type is not knowable here.
const SINK_PATTERNS: &[&str] = &[
    // command-injection
    "os.system",
    "subprocess.call",
    "subprocess.run",
    "subprocess.Popen",
    "os.popen",
    // path-traversal
    "open",
    "Path",
    "shutil.copy",
    "shutil.move",
    // sql-injection
    ".execute",
    ".raw",
    "executemany",
    // code-injection
    "eval",
    "exec",
    "compile",
    // ssrf
    "urlopen",
    "requests.get",
    "requests.post",
    "httpx.get",
    "httpx.post",
];

/// Whether a call written as `name` is a sink.
fn is_sink(name: &str) -> bool {
    SINK_PATTERNS.iter().any(|pattern| {
        if let Some(method) = pattern.strip_prefix('.') {
            // `.execute` matches any receiver, and also a bare `execute`.
            name == method || name.ends_with(&format!(".{method}"))
        } else {
            name == *pattern || name.ends_with(&format!(".{pattern}"))
        }
    })
}

mod parsing {
    use super::*;
    use tree_sitter::{Node, Parser};

    /// Every function and method in `source`, as PDG inputs.
    ///
    /// Returns an empty vector rather than an error when the source does not
    /// parse: a file the grammar cannot read has no statements to analyse, and
    /// a caller asking "what are the PDGs here" is better served by "none" than
    /// by a failure it must special-case. A file that parses to *nothing* and a
    /// file with no functions are the same answer to this question.
    pub fn python_function_pdgs(
        source: &str,
        qualifier: &str,
        generation_id: u32,
        content_hash: u64,
    ) -> Vec<FunctionPdgInput> {
        let mut parser = Parser::new();
        if parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .is_err()
        {
            return Vec::new();
        }
        let Some(tree) = parser.parse(source, None) else {
            return Vec::new();
        };
        let bytes = source.as_bytes();
        let mut out = Vec::new();
        collect_functions(
            tree.root_node(),
            bytes,
            qualifier,
            &mut Vec::new(),
            generation_id,
            content_hash,
            &mut out,
            0,
        );
        out
    }

    /// Walk for `function_definition`, carrying the enclosing class names so a
    /// method is qualified the way the graph qualifies it.
    #[allow(clippy::too_many_arguments)]
    fn collect_functions(
        node: Node,
        bytes: &[u8],
        qualifier: &str,
        scope: &mut Vec<String>,
        generation_id: u32,
        content_hash: u64,
        out: &mut Vec<FunctionPdgInput>,
        depth: usize,
    ) {
        // The builder refuses a tree deeper than this, so there is nothing to
        // gain by walking past it — and a hostile file should not be able to
        // overflow the stack before the refusal can be reported.
        if depth > MAX_PDG_NESTING_DEPTH {
            return;
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "class_definition" => {
                    let name = field_text(child, "name", bytes).unwrap_or_default();
                    scope.push(name);
                    collect_functions(
                        child,
                        bytes,
                        qualifier,
                        scope,
                        generation_id,
                        content_hash,
                        out,
                        depth + 1,
                    );
                    scope.pop();
                }
                "function_definition" => {
                    if let Some(input) =
                        function_input(child, bytes, qualifier, scope, generation_id, content_hash)
                    {
                        out.push(input);
                    }
                    // Nested functions are their own PDGs: the builder is
                    // intra-procedural, so a closure's body is not part of its
                    // enclosing function's control flow.
                    let name = field_text(child, "name", bytes).unwrap_or_default();
                    scope.push(name);
                    collect_functions(
                        child,
                        bytes,
                        qualifier,
                        scope,
                        generation_id,
                        content_hash,
                        out,
                        depth + 1,
                    );
                    scope.pop();
                }
                _ => collect_functions(
                    child,
                    bytes,
                    qualifier,
                    scope,
                    generation_id,
                    content_hash,
                    out,
                    depth + 1,
                ),
            }
        }
    }

    fn function_input(
        node: Node,
        bytes: &[u8],
        qualifier: &str,
        scope: &[String],
        generation_id: u32,
        content_hash: u64,
    ) -> Option<FunctionPdgInput> {
        let name = field_text(node, "name", bytes)?;
        let mut qualified = String::from(qualifier);
        if !qualified.is_empty() {
            qualified.push_str("::");
        }
        for enclosing in scope {
            qualified.push_str(enclosing);
            qualified.push('.');
        }
        qualified.push_str(&name);

        let params = node
            .child_by_field_name("parameters")
            .map(|p| parameter_names(p, bytes))
            .unwrap_or_default();
        let body = node.child_by_field_name("body")?;

        Some(FunctionPdgInput {
            function_name: qualified,
            generation_id,
            content_hash,
            start_line: node.start_position().row as u32 + 1,
            end_line: node.end_position().row as u32 + 1,
            params,
            body: statements(body, bytes, 0),
        })
    }

    /// Parameter names, including defaulted and splat forms.
    ///
    /// The *name* only: `b=1` contributes `b`, and the `1` is not a use of
    /// anything in the body. Treating a default expression as a use would make
    /// every defaulted parameter look like a read of a value the function never
    /// reads.
    fn parameter_names(node: Node, bytes: &[u8]) -> Vec<String> {
        let mut names = Vec::new();
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "identifier" => push_unique(&mut names, text(child, bytes)),
                "default_parameter" | "typed_parameter" | "typed_default_parameter" => {
                    if let Some(inner) = child
                        .child_by_field_name("name")
                        .or_else(|| first_identifier(child))
                    {
                        push_unique(&mut names, text(inner, bytes));
                    }
                }
                "list_splat_pattern" | "dictionary_splat_pattern" => {
                    if let Some(inner) = first_identifier(child) {
                        push_unique(&mut names, text(inner, bytes));
                    }
                }
                _ => {}
            }
        }
        names
    }

    /// A `block`'s statements, recursively.
    fn statements(block: Node, bytes: &[u8], depth: usize) -> Vec<PdgStatement> {
        if depth > MAX_PDG_NESTING_DEPTH {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut cursor = block.walk();
        for child in block.named_children(&mut cursor) {
            // A nested function's body belongs to its own PDG, not to this one.
            if matches!(child.kind(), "function_definition" | "class_definition") {
                continue;
            }
            if let Some(statement) = statement(child, bytes, depth) {
                out.push(statement);
            }
        }
        out
    }

    /// The block a construct owns, by whichever field name the grammar gives it.
    ///
    /// Not one name: `if_statement` and `elif_clause` call theirs `consequence`,
    /// while `for`, `while`, `with` and `else_clause` call theirs `body`.
    /// Looking only for `body` silently returned an empty `then_body` for every
    /// `if` in the corpus — a branch whose taken arm contains nothing, which the
    /// builder would have faithfully turned into a graph of the wrong program.
    fn body_of(node: Node, bytes: &[u8], depth: usize) -> Vec<PdgStatement> {
        node.child_by_field_name("consequence")
            .or_else(|| node.child_by_field_name("body"))
            .map(|block| statements(block, bytes, depth + 1))
            .unwrap_or_default()
    }

    fn statement(node: Node, bytes: &[u8], depth: usize) -> Option<PdgStatement> {
        let line = node.start_position().row as u32 + 1;
        let mut facts = Facts::default();

        let kind = match node.kind() {
            "return_statement" => {
                facts.read_subtree(node, bytes);
                PdgStatementKind::Return
            }
            "raise_statement" => {
                facts.read_subtree(node, bytes);
                PdgStatementKind::Raise
            }
            "if_statement" => {
                if let Some(condition) = node.child_by_field_name("condition") {
                    facts.read_subtree(condition, bytes);
                }
                let then_body = body_of(node, bytes, depth);
                // `elif` is sugar for a nested `if` in the else branch, and the
                // builder has no elif of its own — flattening it into a nested
                // Branch is what preserves the real control flow.
                let mut else_body = Vec::new();
                let mut cursor = node.walk();
                let mut elifs: Vec<Node> = Vec::new();
                for child in node.named_children(&mut cursor) {
                    match child.kind() {
                        "elif_clause" => elifs.push(child),
                        "else_clause" => {
                            else_body = body_of(child, bytes, depth);
                        }
                        _ => {}
                    }
                }
                debug_assert!(
                    node.child_by_field_name("consequence").is_some(),
                    "an if_statement always has a consequence block"
                );
                for elif in elifs.into_iter().rev() {
                    let mut elif_facts = Facts::default();
                    if let Some(condition) = elif.child_by_field_name("condition") {
                        elif_facts.read_subtree(condition, bytes);
                    }
                    else_body = vec![PdgStatement {
                        line: elif.start_position().row as u32 + 1,
                        definitions: elif_facts.definitions,
                        uses: elif_facts.uses,
                        taint_sinks: elif_facts.sinks,
                        kind: PdgStatementKind::Branch {
                            then_body: body_of(elif, bytes, depth),
                            else_body,
                        },
                    }];
                }
                PdgStatementKind::Branch {
                    then_body,
                    else_body,
                }
            }
            "for_statement" => {
                if let Some(target) = node.child_by_field_name("left") {
                    facts.bind_subtree(target, bytes);
                }
                if let Some(iterable) = node.child_by_field_name("right") {
                    facts.read_subtree(iterable, bytes);
                }
                PdgStatementKind::Loop {
                    body: body_of(node, bytes, depth),
                }
            }
            "while_statement" => {
                if let Some(condition) = node.child_by_field_name("condition") {
                    facts.read_subtree(condition, bytes);
                }
                PdgStatementKind::Loop {
                    body: body_of(node, bytes, depth),
                }
            }
            "try_statement" => {
                let mut handlers = Vec::new();
                let mut finally_body = Vec::new();
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    match child.kind() {
                        "except_clause" | "except_group_clause" => {
                            // `except E as err` binds `err` for the handler, so
                            // the handler's first statement is where it is
                            // defined. Recorded on the try itself would place
                            // the definition outside the block that can see it.
                            let mut handler = body_of_except(child, bytes, depth);
                            if let Some(bound) = except_binding(child, bytes) {
                                if let Some(first) = handler.first_mut() {
                                    push_unique(&mut first.definitions, bound);
                                } else {
                                    handler.push(PdgStatement {
                                        line: child.start_position().row as u32 + 1,
                                        definitions: vec![bound],
                                        uses: Vec::new(),
                                        taint_sinks: Vec::new(),
                                        kind: PdgStatementKind::Basic,
                                    });
                                }
                            }
                            handlers.push(handler);
                        }
                        "finally_clause" => {
                            finally_body = body_of_except(child, bytes, depth);
                        }
                        _ => {}
                    }
                }
                PdgStatementKind::Try {
                    body: body_of(node, bytes, depth),
                    handlers,
                    finally_body,
                }
            }
            "with_statement" => {
                // Not a control-flow construct in this model: the body runs
                // exactly once. The bindings it introduces are real, so they are
                // hoisted onto a Basic statement ahead of the body rather than
                // dropped — otherwise `with open(p) as fh:` leaves every use of
                // `fh` reaching no definition.
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if child.kind() == "with_clause" {
                        facts.bind_with_clause(child, bytes);
                    }
                }
                let mut body = body_of(node, bytes, depth);
                let head = PdgStatement {
                    line,
                    definitions: std::mem::take(&mut facts.definitions),
                    uses: std::mem::take(&mut facts.uses),
                    taint_sinks: std::mem::take(&mut facts.sinks),
                    kind: PdgStatementKind::Basic,
                };
                // The `with` itself, then its body, spliced into the enclosing
                // sequence. Returning only the head would drop the body.
                return Some(splice(head, &mut body));
            }
            _ => {
                facts.read_statement(node, bytes);
                PdgStatementKind::Basic
            }
        };

        Some(PdgStatement {
            line,
            definitions: facts.definitions,
            uses: facts.uses,
            taint_sinks: facts.sinks,
            kind,
        })
    }

    /// A `with` becomes a Loop-of-one? No — a Branch with an empty else, which
    /// is the only shape in this model that carries a nested body without
    /// claiming the body is conditional or repeated.
    ///
    /// The alternative was to flatten the body into the enclosing sequence,
    /// which loses the binding's scope, or to drop it, which loses the body.
    /// A single-armed Branch runs the body exactly once on the taken edge and
    /// falls through on the other, which is what `with` does when nothing
    /// raises.
    fn splice(head: PdgStatement, body: &mut Vec<PdgStatement>) -> PdgStatement {
        PdgStatement {
            kind: PdgStatementKind::Branch {
                then_body: std::mem::take(body),
                else_body: Vec::new(),
            },
            ..head
        }
    }

    fn body_of_except(node: Node, bytes: &[u8], depth: usize) -> Vec<PdgStatement> {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "block" {
                return statements(child, bytes, depth + 1);
            }
        }
        Vec::new()
    }

    /// The name bound by `except E as err`.
    fn except_binding(node: Node, bytes: &[u8]) -> Option<String> {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "as_pattern" {
                let mut inner = child.walk();
                for part in child.named_children(&mut inner) {
                    if part.kind() == "as_pattern_target" {
                        return first_identifier(part).map(|id| text(id, bytes));
                    }
                }
            }
        }
        None
    }

    #[derive(Default)]
    struct Facts {
        definitions: Vec<String>,
        uses: Vec<String>,
        sinks: Vec<String>,
    }

    impl Facts {
        /// A statement's defs and uses, split by assignment form.
        fn read_statement(&mut self, node: Node, bytes: &[u8]) {
            let inner = if node.kind() == "expression_statement" {
                node.named_child(0).unwrap_or(node)
            } else {
                node
            };
            match inner.kind() {
                "assignment" => {
                    if let Some(left) = inner.child_by_field_name("left") {
                        self.bind_subtree(left, bytes);
                    }
                    if let Some(right) = inner.child_by_field_name("right") {
                        self.read_subtree(right, bytes);
                    }
                }
                "augmented_assignment" => {
                    // `total += x` both reads and writes `total`. Recording only
                    // the write breaks the def-use chain that reaches it.
                    if let Some(left) = inner.child_by_field_name("left") {
                        self.bind_subtree(left, bytes);
                        self.read_subtree(left, bytes);
                    }
                    if let Some(right) = inner.child_by_field_name("right") {
                        self.read_subtree(right, bytes);
                    }
                }
                _ => self.read_subtree(inner, bytes),
            }
        }

        /// Names *written* by an assignment target, including tuple unpacking.
        fn bind_subtree(&mut self, node: Node, bytes: &[u8]) {
            if node.kind() == "identifier" {
                push_unique(&mut self.definitions, text(node, bytes));
                return;
            }
            // `obj.field = x` and `items[i] = x` write through a name they also
            // *read*; the name itself is not redefined.
            if matches!(node.kind(), "attribute" | "subscript") {
                self.read_subtree(node, bytes);
                return;
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                self.bind_subtree(child, bytes);
            }
        }

        fn bind_with_clause(&mut self, node: Node, bytes: &[u8]) {
            let mut cursor = node.walk();
            for item in node.named_children(&mut cursor) {
                let mut inner = item.walk();
                for part in item.named_children(&mut inner) {
                    if part.kind() == "as_pattern" {
                        let mut alias = part.walk();
                        for piece in part.named_children(&mut alias) {
                            match piece.kind() {
                                "as_pattern_target" => self.bind_subtree(piece, bytes),
                                // The exception type itself, not a binding.
                                "identifier" => {}
                                _ => self.read_subtree(piece, bytes),
                            }
                        }
                    } else {
                        self.read_subtree(part, bytes);
                    }
                }
            }
        }

        /// Names *read* in a subtree, plus any sink arguments it carries.
        fn read_subtree(&mut self, node: Node, bytes: &[u8]) {
            if node.kind() == "call" {
                if let Some(function) = node.child_by_field_name("function") {
                    let name = dotted_name(function, bytes);
                    if is_sink(&name) {
                        if let Some(arguments) = node.child_by_field_name("arguments") {
                            let mut sink_names = Vec::new();
                            collect_identifiers(arguments, bytes, &mut sink_names);
                            for name in sink_names {
                                // Every sink must also appear in `uses`; the
                                // builder's contract says so, and a sink that is
                                // not a use has no def to flow from.
                                push_unique(&mut self.sinks, name.clone());
                                push_unique(&mut self.uses, name);
                            }
                        }
                    }
                }
            }
            let mut names = Vec::new();
            collect_identifiers(node, bytes, &mut names);
            for name in names {
                push_unique(&mut self.uses, name);
            }
        }
    }

    /// `os.system` from an `attribute`, `eval` from an `identifier`.
    fn dotted_name(node: Node, bytes: &[u8]) -> String {
        match node.kind() {
            "identifier" => text(node, bytes),
            "attribute" => {
                let object = node
                    .child_by_field_name("object")
                    .map(|o| dotted_name(o, bytes))
                    .unwrap_or_default();
                let attribute = node
                    .child_by_field_name("attribute")
                    .map(|a| text(a, bytes))
                    .unwrap_or_default();
                if object.is_empty() {
                    attribute
                } else {
                    format!("{object}.{attribute}")
                }
            }
            _ => String::new(),
        }
    }

    /// Identifiers read in a subtree.
    ///
    /// Skips the `attribute` half of `obj.field` and the `name` of a keyword
    /// argument: neither is a variable, and counting them puts method names into
    /// the def-use graph as though they were locals.
    fn collect_identifiers(node: Node, bytes: &[u8], out: &mut Vec<String>) {
        if node.kind() == "identifier" {
            push_unique(out, text(node, bytes));
            return;
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if node.kind() == "attribute" && Some(child) == node.child_by_field_name("attribute") {
                continue;
            }
            if node.kind() == "keyword_argument" && Some(child) == node.child_by_field_name("name")
            {
                continue;
            }
            collect_identifiers(child, bytes, out);
        }
    }

    fn first_identifier(node: Node) -> Option<Node> {
        if node.kind() == "identifier" {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if let Some(found) = first_identifier(child) {
                return Some(found);
            }
        }
        None
    }

    fn field_text(node: Node, field: &str, bytes: &[u8]) -> Option<String> {
        node.child_by_field_name(field).map(|n| text(n, bytes))
    }

    fn text(node: Node, bytes: &[u8]) -> String {
        node.utf8_text(bytes).unwrap_or("").to_string()
    }
}

fn push_unique(list: &mut Vec<String>, value: String) {
    if value.is_empty() || list.contains(&value) {
        return;
    }
    list.push(value);
}

pub use parsing::python_function_pdgs;

#[cfg(test)]
mod tests {
    use super::*;

    fn only(source: &str) -> FunctionPdgInput {
        let mut all = python_function_pdgs(source, "m.py", 1, 7);
        assert_eq!(all.len(), 1, "expected exactly one function: {all:#?}");
        all.remove(0)
    }

    #[test]
    fn a_function_carries_its_name_params_and_span() {
        let pdg = only("def run(a, b=1, *rest, **kw):\n    return a\n");
        assert_eq!(pdg.function_name, "m.py::run");
        assert_eq!(pdg.params, vec!["a", "b", "rest", "kw"]);
        assert_eq!(pdg.start_line, 1);
        assert_eq!(pdg.end_line, 2);
        assert_eq!(pdg.generation_id, 1);
        assert_eq!(pdg.content_hash, 7);
    }

    #[test]
    fn a_method_is_qualified_by_its_class() {
        let pdg = only("class C:\n    def m(self):\n        return 1\n");
        assert_eq!(pdg.function_name, "m.py::C.m");
    }

    #[test]
    fn a_nested_function_is_its_own_pdg_not_part_of_its_parent() {
        // The builder is intra-procedural: a closure's body is not in its
        // enclosing function's control flow, and splicing it in would invent
        // edges between statements that never run in sequence.
        let all = python_function_pdgs(
            "def outer():\n    def inner():\n        return 1\n    return inner\n",
            "m.py",
            1,
            0,
        );
        let names: Vec<&str> = all.iter().map(|f| f.function_name.as_str()).collect();
        assert_eq!(names, vec!["m.py::outer", "m.py::outer.inner"]);
        let outer = &all[0];
        assert_eq!(outer.body.len(), 1, "only the return: {:#?}", outer.body);
    }

    #[test]
    fn an_assignment_splits_into_a_definition_and_uses() {
        let pdg = only("def f(a, b):\n    total = a + b\n");
        let statement = &pdg.body[0];
        assert_eq!(statement.definitions, vec!["total"]);
        assert_eq!(statement.uses, vec!["a", "b"]);
    }

    #[test]
    fn an_augmented_assignment_both_reads_and_writes_its_target() {
        // Recording only the write breaks the def-use chain that reaches it.
        let pdg = only("def f(x):\n    total = 0\n    total += x\n");
        let statement = &pdg.body[1];
        assert_eq!(statement.definitions, vec!["total"]);
        assert!(
            statement.uses.contains(&"total".to_string()),
            "{statement:?}"
        );
        assert!(statement.uses.contains(&"x".to_string()));
    }

    #[test]
    fn writing_through_an_attribute_does_not_redefine_the_name() {
        // `obj.field = x` reads `obj`; it does not create a new `obj`.
        let pdg = only("def f(obj, x):\n    obj.field = x\n");
        let statement = &pdg.body[0];
        assert!(statement.definitions.is_empty(), "{statement:?}");
        assert!(statement.uses.contains(&"obj".to_string()));
    }

    #[test]
    fn tuple_unpacking_defines_every_name() {
        let pdg = only("def f(pair):\n    a, b = pair\n");
        assert_eq!(pdg.body[0].definitions, vec!["a", "b"]);
        assert_eq!(pdg.body[0].uses, vec!["pair"]);
    }

    #[test]
    fn a_method_name_is_not_a_variable() {
        // Counting the `.read` of `fh.read()` puts method names into the def-use
        // graph as though they were locals.
        let pdg = only("def f(fh):\n    data = fh.read()\n");
        assert_eq!(pdg.body[0].uses, vec!["fh"]);
    }

    #[test]
    fn control_flow_becomes_the_kinds_the_builder_understands() {
        let pdg = only(
            "def f(xs, n):\n\
             \x20   for x in xs:\n\
             \x20       pass\n\
             \x20   while n:\n\
             \x20       pass\n\
             \x20   if n:\n\
             \x20       pass\n\
             \x20   try:\n\
             \x20       pass\n\
             \x20   except ValueError:\n\
             \x20       pass\n\
             \x20   finally:\n\
             \x20       pass\n\
             \x20   raise ValueError(n)\n",
        );
        let kinds: Vec<&str> = pdg
            .body
            .iter()
            .map(|s| match &s.kind {
                PdgStatementKind::Basic => "basic",
                PdgStatementKind::Branch { .. } => "branch",
                PdgStatementKind::Loop { .. } => "loop",
                PdgStatementKind::Try { .. } => "try",
                PdgStatementKind::Return => "return",
                PdgStatementKind::Raise => "raise",
            })
            .collect();
        assert_eq!(kinds, vec!["loop", "loop", "branch", "try", "raise"]);
    }

    #[test]
    fn a_for_loop_defines_its_target_and_reads_its_iterable() {
        let pdg = only("def f(items):\n    for x in items:\n        pass\n");
        assert_eq!(pdg.body[0].definitions, vec!["x"]);
        assert_eq!(pdg.body[0].uses, vec!["items"]);
    }

    #[test]
    fn elif_becomes_a_nested_branch_rather_than_being_dropped() {
        // The builder has no elif. Flattening it into the else branch is what
        // preserves the control flow; dropping it would make the third arm
        // unreachable in the graph while it runs in the program.
        let pdg = only(
            "def f(n):\n\
             \x20   if n > 1:\n\
             \x20       a = 1\n\
             \x20   elif n > 0:\n\
             \x20       b = 2\n\
             \x20   else:\n\
             \x20       c = 3\n",
        );
        let PdgStatementKind::Branch {
            then_body,
            else_body,
        } = &pdg.body[0].kind
        else {
            panic!("expected a branch: {:#?}", pdg.body[0]);
        };
        assert_eq!(then_body[0].definitions, vec!["a"]);
        let PdgStatementKind::Branch {
            then_body: elif_then,
            else_body: elif_else,
        } = &else_body[0].kind
        else {
            panic!("elif must nest as a branch: {else_body:#?}");
        };
        assert_eq!(elif_then[0].definitions, vec!["b"]);
        assert_eq!(elif_else[0].definitions, vec!["c"]);
    }

    #[test]
    fn an_except_binding_is_defined_inside_the_handler_that_can_see_it() {
        let pdg = only(
            "def f():\n\
             \x20   try:\n\
             \x20       risky()\n\
             \x20   except ValueError as err:\n\
             \x20       log(err)\n",
        );
        let PdgStatementKind::Try { handlers, .. } = &pdg.body[0].kind else {
            panic!("expected a try: {:#?}", pdg.body[0]);
        };
        assert_eq!(handlers.len(), 1);
        assert!(
            handlers[0][0].definitions.contains(&"err".to_string()),
            "the handler must define its bound name: {:#?}",
            handlers[0]
        );
    }

    #[test]
    fn a_with_binding_reaches_the_body_that_uses_it() {
        // Dropping the `as` binding leaves every use of `fh` reaching no
        // definition, which reads as an undefined variable.
        let pdg = only(
            "def f(p):\n\
             \x20   with open(p) as fh:\n\
             \x20       data = fh.read()\n",
        );
        assert!(
            pdg.body[0].definitions.contains(&"fh".to_string()),
            "{:#?}",
            pdg.body[0]
        );
        let PdgStatementKind::Branch { then_body, .. } = &pdg.body[0].kind else {
            panic!("expected the with body to be carried: {:#?}", pdg.body[0]);
        };
        assert_eq!(then_body[0].definitions, vec!["data"]);
    }

    #[test]
    fn a_sink_call_records_the_variables_it_consumes() {
        let pdg = only("def f(cmd):\n    os.system(cmd)\n");
        let statement = &pdg.body[0];
        assert!(
            statement.taint_sinks.contains(&"cmd".to_string()),
            "{statement:?}"
        );
        // The builder's contract: every sink must also be a use, or it has no
        // definition to flow from.
        for sink in &statement.taint_sinks {
            assert!(statement.uses.contains(sink), "{statement:?}");
        }
    }

    #[test]
    fn a_receiver_qualified_sink_matches_whatever_the_receiver_is() {
        for source in [
            "def f(q):\n    cursor.execute(q)\n",
            "def f(q):\n    conn.execute(q)\n",
        ] {
            let pdg = only(source);
            assert!(
                pdg.body[0].taint_sinks.contains(&"q".to_string()),
                "{source} -> {:?}",
                pdg.body[0]
            );
        }
    }

    #[test]
    fn an_ordinary_call_records_no_sink() {
        // The absence of a finding is not a safety claim, but it must at least
        // not be a false one.
        let pdg = only("def f(x):\n    helper(x)\n");
        assert!(pdg.body[0].taint_sinks.is_empty(), "{:?}", pdg.body[0]);
    }

    #[test]
    fn source_that_does_not_parse_yields_no_functions_rather_than_an_error() {
        assert!(python_function_pdgs("def (:::", "m.py", 1, 0).is_empty());
        assert!(python_function_pdgs("", "m.py", 1, 0).is_empty());
    }

    #[test]
    fn deep_nesting_is_bounded_rather_than_overflowing_the_stack() {
        // Two separate limits, and it matters which one bites.
        //
        // Measured on tree-sitter-python 0.23: at 40 levels of nested `if` the
        // root is a clean `module` with a `function_definition` child, and the
        // builder accepts the result. At 64 the *grammar* gives up — the root
        // node is `ERROR` and no `function_definition` exists anywhere in the
        // tree — so there is nothing to hand the builder.
        //
        // Both outcomes are safe, and neither is a crash. What this pins is that
        // the pathological case yields *nothing* rather than a partial tree
        // offered as a whole one: a PDG built from part of a function is exactly
        // what `pdg.rs` refuses to produce, and it must not arrive through the
        // producer either.
        let nest = |depth: usize| {
            let mut source = String::from("def f(n):\n");
            for level in 0..depth {
                source.push_str(&"    ".repeat(level + 1));
                source.push_str("if n:\n");
            }
            source.push_str(&"    ".repeat(depth + 1));
            source.push_str("pass\n");
            source
        };

        let deep = python_function_pdgs(&nest(40), "m.py", 1, 0);
        assert_eq!(deep.len(), 1, "40 levels is within the grammar's reach");
        assert!(crate::pdg::build_function_pdg(&deep[0]).is_ok());

        // Far past anything real — CPython's own compiler caps nesting at 20.
        for depth in [64, 256, 1024] {
            let pathological = python_function_pdgs(&nest(depth), "m.py", 1, 0);
            assert!(
                pathological.is_empty(),
                "{depth} levels must yield nothing rather than a partial tree"
            );
        }
    }

    #[test]
    fn the_output_feeds_the_builder_it_was_written_for() {
        // The whole point: this producer exists so `build_function_pdg` can run
        // on a real repository. A shape it cannot consume is not a port.
        let pdg = only(
            "def f(items, cmd):\n\
             \x20   total = 0\n\
             \x20   for x in items:\n\
             \x20       total += x\n\
             \x20   if total:\n\
             \x20       os.system(cmd)\n\
             \x20   return total\n",
        );
        let built = crate::pdg::build_function_pdg(&pdg).expect("the builder must accept this");
        assert_eq!(built.function_name, "m.py::f");
        assert!(!built.nodes.is_empty());
        assert!(!built.edges.is_empty());
    }
}
