//! Codex code mode: the calls a `custom_tool_call` `exec` program makes.
//!
//! The `input` of that call is a JavaScript program that calls
//! `tools.exec_command({cmd, workdir})` and `tools.apply_patch(patch)`. An earlier
//! hand-written scanner of that text was wrong both ways (a division taken for a
//! regex lost real calls, a regex literal made fake ones), so the program is read
//! by a real grammar, tree-sitter-javascript, which knows division from regex,
//! comments and strings from code, and an escaped name from a different one.
//!
//! The contract is the honesty one: every `tools.exec_command` / `tools.apply_patch`
//! call node the parse finds is either recorded or counted in [`Program::skips`]
//! with a reason, never dropped and never invented. A call is recorded only when
//!
//! * it is *straight-line*: every ancestor up to the program is statement or
//!   expression scaffolding that runs once, unconditionally (an expression
//!   statement, `await`, a declaration, an assignment, the arguments, array or
//!   object it sits in). A call in a function, loop, conditional, `try`, a `&&` /
//!   `||` / `?:` branch, or behind an optional chain's short circuit may run zero
//!   or many times, so it is counted, not recorded. A top-level `await` is
//!   straight-line. What the grammar cannot say is whether an earlier statement
//!   throws or the program is cut short: a recorded call is an attempted one, as
//!   a structured `exec_command` record is;
//! * its arguments are literals: an object with a string or plain-template `cmd`
//!   (and optional `workdir`), or a string or plain template patch, with every
//!   escape decoded here (the grammar hands back the raw source text);
//! * its subtree and ancestors hold no `ERROR` or missing node.
//!
//! `tools` itself is watched: any use of the name other than as the object of a
//! callee (`const t = tools`, `tools = x`, destructuring, passing it on,
//! `globalThis.tools`) adds one marker to the program, because a call through the
//! alias cannot be seen. Calls recorded before it stay recorded.
//!
//! The program is untrusted input to C code, so it is bounded ([`Limits`]): over
//! the byte cap it is not parsed, the parse is aborted at a deadline, and the walk
//! has a node budget. A program that hits any of them is one skip and none of its
//! calls are recorded. Every parse gets a fresh [`Parser`], because an aborted
//! parse leaves the parser's byte offsets out of range for the next source.

use std::borrow::Cow;
use std::collections::HashSet;
use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use tree_sitter::{Node, ParseOptions, ParseState, Parser, Tree};

use super::{patch_paths, Command};

pub(super) const TOO_LARGE: &str = "code-mode too large";
pub(super) const PARSE_TIMEOUT: &str = "code-mode parse timeout";
pub(super) const NODE_BUDGET: &str = "code-mode node budget";
const PARSE_FAILED: &str = "code-mode parse failed";
const NON_STRAIGHT_LINE: &str = "code-mode non-straight-line";
const SYNTAX_ERROR: &str = "code-mode syntax error";
const HIDDEN_BY_SYNTAX: &str = "code-mode syntax error hides calls";
const ALIASED: &str = "code-mode tools aliased";
const COMPUTED_CALLEE: &str = "code-mode computed callee";
const NON_LITERAL: &str = "code-mode non-literal argument";
const BAD_ESCAPE: &str = "code-mode invalid escape";
const EMPTY_PATCH: &str = "code-mode patch names no file";

/// What bounds one program. [`LIMITS`] is what ships; tests pass tighter ones.
pub(super) struct Limits {
    /// Programs longer than this many bytes are not parsed.
    pub bytes: usize,
    /// The wall-clock time a parse may take before it is aborted.
    pub parse: Duration,
    /// The nodes the walk may visit.
    pub nodes: usize,
}

pub(super) const LIMITS: Limits = Limits {
    bytes: 256 << 10,
    parse: Duration::from_millis(250),
    nodes: 200_000,
};

/// How far up from a call the straight-line check looks. Real programs nest a
/// handful of levels; the cap keeps a hostile one from costing depth per call.
const MAX_ANCESTORS: usize = 64;

/// What one program holds.
#[derive(Default, Debug)]
pub(super) struct Program {
    pub commands: Vec<Command>,
    /// Paths named by patch headers, as written.
    pub patched: Vec<String>,
    /// One reason per call (or program) counted and not read.
    pub skips: Vec<&'static str>,
}

/// Reads the calls out of one program, within `limits`.
pub(super) fn scan(source: &str, limits: &Limits) -> Program {
    let mut program = Program::default();
    if source.len() > limits.bytes {
        program.skips.push(TOO_LARGE);
        return program;
    }
    // `tools` can only be spelled in the text or through a `\u` escape.
    if !source.contains("tools") && !source.contains("\\u") {
        return program;
    }
    let tree = match parse(source, limits.parse) {
        Ok(tree) => tree,
        Err(reason) => {
            program.skips.push(reason);
            return program;
        }
    };
    let mut walk = Walk {
        src: source.as_bytes(),
        consumed: HashSet::new(),
        aliased: false,
        candidates: 0,
        program: Program::default(),
    };
    if let Err(reason) = walk.run(&tree, limits.nodes) {
        program.skips.push(reason);
        return program;
    }
    let mut found = walk.program;
    if walk.aliased {
        found.skips.push(ALIASED);
    }
    // A program the grammar could not make whole may name a call it did not
    // parse as one: more mentions than call nodes is counted, once.
    if tree.root_node().has_error() && mentions(source) > walk.candidates {
        found.skips.push(HIDDEN_BY_SYNTAX);
    }
    found
}

fn mentions(source: &str) -> usize {
    source.matches("exec_command").count() + source.matches("apply_patch").count()
}

/// A fresh parser per program, aborted at `limit`.
fn parse(source: &str, limit: Duration) -> Result<Tree, &'static str> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_javascript::LANGUAGE.into())
        .map_err(|_| PARSE_FAILED)?;
    let started = Instant::now();
    let mut timed_out = false;
    let mut deadline = |_: &ParseState| {
        if started.elapsed() >= limit {
            timed_out = true;
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let bytes = source.as_bytes();
    let tree = parser.parse_with_options(
        &mut |offset, _| bytes.get(offset..).unwrap_or_default(),
        None,
        Some(ParseOptions::new().progress_callback(&mut deadline)),
    );
    match tree {
        Some(tree) => Ok(tree),
        None if timed_out => Err(PARSE_TIMEOUT),
        None => Err(PARSE_FAILED),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Exec,
    Patch,
}

/// How a call's callee names its tool, when its object is `tools`.
enum Callee {
    Named(Tool),
    /// A literal name of some other tool: not ours, not counted.
    Other,
    /// A key that is not a literal.
    Computed,
    BadEscape,
}

/// A decoded literal.
enum Lit {
    Text(String),
    BadEscape,
    NotLiteral,
}

struct Walk<'s> {
    src: &'s [u8],
    /// Ids of the `tools` identifiers that are a callee's object, which the call
    /// accounts for itself.
    consumed: HashSet<usize>,
    aliased: bool,
    /// Call nodes whose callee has `tools` as its object and names (or may name)
    /// one of the two tools.
    candidates: usize,
    program: Program,
}

impl Walk<'_> {
    /// An iterative pre-order walk (a hostile program nests 100k deep), keeping
    /// the ancestors of the current node, with a budget of nodes.
    fn run(&mut self, tree: &Tree, budget: usize) -> Result<(), &'static str> {
        let mut cursor = tree.walk();
        let mut ancestors: Vec<Node<'_>> = Vec::new();
        let mut visited = 0usize;
        loop {
            visited += 1;
            if visited > budget {
                return Err(NODE_BUDGET);
            }
            let node = cursor.node();
            self.visit(node, &ancestors);
            if cursor.goto_first_child() {
                ancestors.push(node);
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return Ok(());
                }
                ancestors.pop();
            }
        }
    }

    fn visit(&mut self, node: Node<'_>, ancestors: &[Node<'_>]) {
        match node.kind() {
            "call_expression" => {
                if let Some(callee) = self.callee(node) {
                    self.call(node, ancestors, callee);
                }
            }
            "identifier"
            | "shorthand_property_identifier"
            | "shorthand_property_identifier_pattern" => {
                if self.is_tools(node) && !self.benign_use(node, ancestors.last()) {
                    self.aliased = true;
                }
            }
            // `globalThis.tools`, `self.tools`: a way to `tools` this scan does
            // not follow.
            "property_identifier" if self.is_tools(node) => self.aliased = true,
            _ => {}
        }
    }

    fn text(&self, node: Node<'_>) -> Option<&str> {
        node.utf8_text(self.src).ok()
    }

    /// Whether an identifier-like node spells `tools`, escapes decoded.
    fn is_tools(&self, node: Node<'_>) -> bool {
        self.name(node).as_deref() == Some("tools")
    }

    fn name(&self, node: Node<'_>) -> Option<Cow<'_, str>> {
        let raw = self.text(node)?;
        if raw.contains('\\') {
            decode(raw, Mode::Identifier).map(Cow::Owned)
        } else {
            Some(Cow::Borrowed(raw))
        }
    }

    fn literal(&self, node: Node<'_>) -> Lit {
        let mode = match node.kind() {
            "string" => Mode::String,
            "template_string" => {
                let mut cursor = node.walk();
                let plain = node
                    .named_children(&mut cursor)
                    .all(|child| child.kind() != "template_substitution");
                if !plain {
                    return Lit::NotLiteral;
                }
                Mode::Template
            }
            _ => return Lit::NotLiteral,
        };
        let Some(raw) = self.text(node).filter(|raw| raw.len() >= 2) else {
            return Lit::BadEscape;
        };
        match raw
            .get(1..raw.len() - 1)
            .and_then(|body| decode(body, mode))
        {
            Some(text) => Lit::Text(text),
            None => Lit::BadEscape,
        }
    }

    /// The callee of a call whose object is `tools`, and records which of the
    /// `tools` identifiers it accounts for.
    fn callee(&mut self, call: Node<'_>) -> Option<Callee> {
        let function = unwrap_parens(call.child_by_field_name("function")?);
        let (object, key) = match function.kind() {
            "member_expression" => (
                function.child_by_field_name("object")?,
                function.child_by_field_name("property")?,
            ),
            "subscript_expression" => (
                function.child_by_field_name("object")?,
                function.child_by_field_name("index")?,
            ),
            _ => return None,
        };
        let object = unwrap_parens(object);
        if object.kind() != "identifier" || !self.is_tools(object) {
            return None;
        }
        self.consumed.insert(object.id());
        let name = if function.kind() == "member_expression" {
            match self.name(key) {
                Some(name) => Lit::Text(name.into_owned()),
                None => Lit::BadEscape,
            }
        } else {
            self.literal(key)
        };
        Some(match name {
            Lit::Text(name) => match name.as_str() {
                "exec_command" => Callee::Named(Tool::Exec),
                "apply_patch" => Callee::Named(Tool::Patch),
                _ => Callee::Other,
            },
            Lit::NotLiteral => Callee::Computed,
            Lit::BadEscape => Callee::BadEscape,
        })
    }

    /// A `tools` identifier that is not the object of a callee: harmless when it
    /// only reaches another tool by a literal name (`tools.read_file`), an alias
    /// otherwise.
    fn benign_use(&self, node: Node<'_>, parent: Option<&Node<'_>>) -> bool {
        if self.consumed.contains(&node.id()) {
            return true;
        }
        let Some(parent) = parent else { return false };
        let key = match parent.kind() {
            "member_expression" if same(parent.child_by_field_name("object"), node) => match parent
                .child_by_field_name("property")
                .and_then(|key| self.name(key))
            {
                Some(name) => Lit::Text(name.into_owned()),
                None => Lit::BadEscape,
            },
            "subscript_expression" if same(parent.child_by_field_name("object"), node) => {
                match parent.child_by_field_name("index") {
                    Some(index) => self.literal(index),
                    None => Lit::NotLiteral,
                }
            }
            _ => return false,
        };
        matches!(key, Lit::Text(name) if name != "exec_command" && name != "apply_patch")
    }

    fn skip(&mut self, reason: &'static str) {
        self.program.skips.push(reason);
    }

    fn call(&mut self, call: Node<'_>, ancestors: &[Node<'_>], callee: Callee) {
        let tool = match callee {
            Callee::Other => return,
            Callee::Named(tool) => tool,
            Callee::Computed => {
                self.candidates += 1;
                return self.skip(COMPUTED_CALLEE);
            }
            Callee::BadEscape => {
                self.candidates += 1;
                return self.skip(BAD_ESCAPE);
            }
        };
        self.candidates += 1;
        if call.has_error() {
            return self.skip(SYNTAX_ERROR);
        }
        if let Err(reason) = straight_line(call, ancestors) {
            return self.skip(reason);
        }
        let Some(arguments) = call
            .child_by_field_name("arguments")
            .filter(|node| node.kind() == "arguments")
        else {
            return self.skip(NON_LITERAL);
        };
        let mut cursor = arguments.walk();
        let args: Vec<Node<'_>> = arguments
            .named_children(&mut cursor)
            .filter(|node| node.kind() != "comment")
            .collect();
        let [argument] = args.as_slice() else {
            return self.skip(NON_LITERAL);
        };
        match tool {
            Tool::Exec => match self.command(*argument) {
                Ok(command) => self.program.commands.push(command),
                Err(reason) => self.skip(reason),
            },
            Tool::Patch => match self.literal(*argument) {
                Lit::Text(patch) => {
                    let paths = patch_paths(&patch);
                    if paths.is_empty() {
                        self.skip(EMPTY_PATCH);
                    } else {
                        self.program.patched.extend(paths);
                    }
                }
                Lit::BadEscape => self.skip(BAD_ESCAPE),
                Lit::NotLiteral => self.skip(NON_LITERAL),
            },
        }
    }

    /// `{cmd: <literal>, workdir?: <literal>, ...}`; the last of a repeated key
    /// wins, as in JavaScript. Any key this cannot read the name or value of
    /// (spread, computed, method) could be `cmd`, so the call is counted.
    fn command(&self, object: Node<'_>) -> Result<Command, &'static str> {
        if object.kind() != "object" {
            return Err(NON_LITERAL);
        }
        let (mut cmd, mut workdir) = (None, None);
        let mut cursor = object.walk();
        for member in object.named_children(&mut cursor) {
            let (key, value) = match member.kind() {
                "comment" => continue,
                "pair" => {
                    let key = member.child_by_field_name("key").ok_or(NON_LITERAL)?;
                    let name = match key.kind() {
                        "property_identifier" => self.name(key).map(Cow::into_owned),
                        "string" => match self.literal(key) {
                            Lit::Text(name) => Some(name),
                            _ => None,
                        },
                        _ => return Err(NON_LITERAL),
                    };
                    (name.ok_or(BAD_ESCAPE)?, member.child_by_field_name("value"))
                }
                "shorthand_property_identifier" => {
                    let name = self.name(member).ok_or(BAD_ESCAPE)?.into_owned();
                    if name == "cmd" || name == "workdir" {
                        return Err(NON_LITERAL);
                    }
                    (name, None)
                }
                _ => return Err(NON_LITERAL),
            };
            let slot = match key.as_str() {
                "cmd" => &mut cmd,
                "workdir" => &mut workdir,
                _ => continue,
            };
            let value = value.ok_or(NON_LITERAL)?;
            *slot = Some(match self.literal(value) {
                Lit::Text(text) => text,
                Lit::BadEscape => return Err(BAD_ESCAPE),
                Lit::NotLiteral => return Err(NON_LITERAL),
            });
        }
        Ok(Command {
            cmd: cmd.ok_or(NON_LITERAL)?,
            workdir: workdir.filter(|dir| !dir.is_empty()),
        })
    }
}

fn same(node: Option<Node<'_>>, other: Node<'_>) -> bool {
    node.is_some_and(|node| node.id() == other.id())
}

fn unwrap_parens(mut node: Node<'_>) -> Node<'_> {
    while node.kind() == "parenthesized_expression" {
        let mut cursor = node.walk();
        let inner = node
            .named_children(&mut cursor)
            .find(|child| child.kind() != "comment");
        match inner {
            Some(inner) => node = inner,
            None => break,
        }
    }
    node
}

/// Whether every ancestor of `call` runs once, unconditionally.
fn straight_line(call: Node<'_>, ancestors: &[Node<'_>]) -> Result<(), &'static str> {
    let mut child = call;
    for ancestor in ancestors.iter().rev().take(MAX_ANCESTORS) {
        match ancestor.kind() {
            "program" => return Ok(()),
            "ERROR" => return Err(SYNTAX_ERROR),
            "expression_statement"
            | "await_expression"
            | "parenthesized_expression"
            | "sequence_expression"
            | "lexical_declaration"
            | "variable_declaration"
            | "variable_declarator"
            | "assignment_expression"
            | "unary_expression"
            | "arguments"
            | "array"
            | "object"
            | "pair"
            | "spread_element"
            | "template_string"
            | "template_substitution" => {}
            // Both operands of an arithmetic or comparison operator are
            // evaluated; the right side of `&&`, `||` and `??` may not be.
            "binary_expression" => {
                let logical = ancestor
                    .child_by_field_name("operator")
                    .is_none_or(|op| matches!(op.kind(), "&&" | "||" | "??"));
                if logical {
                    return Err(NON_STRAIGHT_LINE);
                }
            }
            // An optional chain skips what follows it: only the part evaluated
            // before the `?.` always runs.
            "call_expression" | "member_expression" | "subscript_expression" => {
                let first = if ancestor.kind() == "call_expression" {
                    "function"
                } else {
                    "object"
                };
                if has_optional_chain(*ancestor)
                    && !same(ancestor.child_by_field_name(first), child)
                {
                    return Err(NON_STRAIGHT_LINE);
                }
            }
            _ => return Err(NON_STRAIGHT_LINE),
        }
        child = *ancestor;
    }
    Err(NON_STRAIGHT_LINE)
}

fn has_optional_chain(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    let chained = node
        .children(&mut cursor)
        .any(|child| child.kind() == "optional_chain");
    chained
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    String,
    Template,
    /// Only `\u` escapes are legal in a name.
    Identifier,
}

/// The value of the source text between a literal's delimiters (or of a name),
/// escapes decoded as ECMAScript reads them. `None` when an escape is not valid
/// there, or the value is not valid Unicode (a lone surrogate, which a `String`
/// cannot hold).
///
/// Legacy octal escapes (`\101`, and `\0` before a digit) are valid in a string
/// (Annex B) and an error in a template; `\8` and `\9` are the digit itself in a
/// string and an error in a template. A template's line terminators read as `\n`.
fn decode(raw: &str, mode: Mode) -> Option<String> {
    fn push(units: &mut Vec<u16>, c: char) {
        let mut buf = [0u16; 2];
        units.extend_from_slice(c.encode_utf16(&mut buf));
    }
    let mut units: Vec<u16> = Vec::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' if mode == Mode::Template => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                units.push(u16::from(b'\n'));
            }
            '\\' => {
                let escape = chars.next()?;
                if mode == Mode::Identifier && escape != 'u' {
                    return None;
                }
                match escape {
                    'n' => units.push(10),
                    'r' => units.push(13),
                    't' => units.push(9),
                    'b' => units.push(8),
                    'f' => units.push(12),
                    'v' => units.push(11),
                    '0'..='7' => {
                        let digit = escape.to_digit(8)?;
                        let next_is_digit = chars.peek().is_some_and(char::is_ascii_digit);
                        if escape == '0' && !next_is_digit {
                            units.push(0);
                        } else if mode == Mode::Template {
                            return None;
                        } else {
                            let more = if digit <= 3 { 2 } else { 1 };
                            let mut value = digit;
                            for _ in 0..more {
                                match chars.peek().and_then(|c| c.to_digit(8)) {
                                    Some(next) => {
                                        value = value * 8 + next;
                                        chars.next();
                                    }
                                    None => break,
                                }
                            }
                            units.push(u16::try_from(value).ok()?);
                        }
                    }
                    '8' | '9' => {
                        if mode == Mode::Template {
                            return None;
                        }
                        push(&mut units, escape);
                    }
                    'x' => {
                        let high = chars.next()?.to_digit(16)?;
                        let low = chars.next()?.to_digit(16)?;
                        units.push(u16::try_from(high * 16 + low).ok()?);
                    }
                    'u' => decode_unicode(&mut chars, &mut units)?,
                    '\r' => {
                        if chars.peek() == Some(&'\n') {
                            chars.next();
                        }
                    }
                    '\n' | '\u{2028}' | '\u{2029}' => {}
                    other => push(&mut units, other),
                }
            }
            c => push(&mut units, c),
        }
    }
    String::from_utf16(&units).ok()
}

/// `\uHHHH` or `\u{H…}`, after the `\u`.
fn decode_unicode(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    units: &mut Vec<u16>,
) -> Option<()> {
    if chars.peek() == Some(&'{') {
        chars.next();
        let mut value: u32 = 0;
        let mut digits = 0usize;
        loop {
            let c = chars.next()?;
            if c == '}' {
                break;
            }
            value = value.checked_mul(16)?.checked_add(c.to_digit(16)?)?;
            if value > 0x10_FFFF {
                return None;
            }
            digits += 1;
        }
        if digits == 0 {
            return None;
        }
        match char::from_u32(value) {
            Some(c) => {
                let mut buf = [0u16; 2];
                units.extend_from_slice(c.encode_utf16(&mut buf));
            }
            // A surrogate code point: it only survives as half of a pair.
            None => units.push(u16::try_from(value).ok()?),
        }
    } else {
        let mut value = 0u32;
        for _ in 0..4 {
            value = value * 16 + chars.next()?.to_digit(16)?;
        }
        units.push(u16::try_from(value).ok()?);
    }
    Some(())
}

#[cfg(test)]
mod adv;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn read(source: &str) -> Program {
        scan(source, &LIMITS)
    }

    fn cmds(program: &Program) -> Vec<&str> {
        program.commands.iter().map(|c| c.cmd.as_str()).collect()
    }

    #[test]
    fn escapes_decode_as_ecmascript_reads_them() {
        let s = |raw: &str| decode(raw, Mode::String);
        assert_eq!(
            s(r"a\nb\tc\\d\'e\x41B\u{43}\u{1F600}").unwrap(),
            "a\nb\tc\\d'eABC\u{1F600}"
        );
        assert_eq!(s(r"😀").unwrap(), "\u{1F600}");
        assert_eq!(s("a\\\nb\\\r\nc\\\rd\\\u{2028}e").unwrap(), "abcde");
        assert_eq!(s(r"\0|\101|\7|\08|\9|\z").unwrap(), "\0|A|\u{7}|\u{0}8|9|z");
        assert_eq!(s(r"\377\400").unwrap(), "\u{ff}\u{20}0");
        // Invalid: short or non-hex escapes, a lone surrogate, an overlong code point.
        for bad in [
            r"\x4",
            r"\xZZ",
            r"\u12",
            r"\u{}",
            r"\u{110000}",
            r"\u{1F600",
            r"\uD83D",
            r"\uDE00x",
            "\\",
        ] {
            assert!(s(bad).is_none(), "{bad:?}");
        }
        let t = |raw: &str| decode(raw, Mode::Template);
        assert_eq!(t("a\r\nb\rc\nd").unwrap(), "a\nb\nc\nd");
        assert_eq!(t(r"\0\`\$").unwrap(), "\0`$");
        assert!(t(r"\1").is_none() && t(r"\01").is_none() && t(r"\8").is_none());
        assert_eq!(
            decode(r"exec_command", Mode::Identifier).unwrap(),
            "exec_command"
        );
        assert!(decode(r"a\nb", Mode::Identifier).is_none());
    }

    #[test]
    fn a_program_over_the_byte_cap_is_one_skip_and_is_not_parsed() {
        let unit = "await tools.exec_command({cmd:'x'});\n";
        let mut source = unit.repeat(LIMITS.bytes / unit.len() + 1);
        source.truncate(LIMITS.bytes + 1);
        let program = read(&source);
        assert_eq!(program.skips, vec![TOO_LARGE]);
        assert!(program.commands.is_empty());
        source.truncate(LIMITS.bytes);
        // At the cap it is parsed (this dense program may then hit the node
        // budget, but it is not rejected for its size).
        assert!(!read(&source).skips.contains(&TOO_LARGE));
    }

    /// Parse and walk time of a 256 KiB program, min of 5. Run by name:
    /// `cargo test --lib ingest::codex::js::tests::bench -- --ignored --nocapture`.
    #[test]
    #[ignore = "benchmark: run explicitly"]
    fn bench_a_cap_sized_program() {
        let unit = "await tools.exec_command({cmd:'echo hello', workdir:'/tmp'});\n";
        let mut source = unit.repeat(LIMITS.bytes / unit.len());
        source.truncate(LIMITS.bytes);
        let wide = Limits {
            bytes: usize::MAX,
            parse: Duration::from_secs(60),
            nodes: usize::MAX,
        };
        let mut times = Vec::new();
        let mut rows = 0;
        for _ in 0..5 {
            let started = Instant::now();
            rows = scan(&source, &wide).commands.len();
            times.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "JSBENCH bytes={} rows={rows} min_ms={:.1} median_ms={:.1}",
            source.len(),
            times[0],
            times[2]
        );
    }

    #[test]
    fn a_parse_past_its_deadline_is_one_skip_and_the_next_program_reads_correctly() {
        // 200k nested parentheses is slow to parse; a zero deadline aborts it.
        let deep = format!(
            "{}1{}; tools.exec_command({{cmd:'lost'}})",
            "(".repeat(100_000),
            ")".repeat(100_000)
        );
        let limits = Limits {
            bytes: LIMITS.bytes,
            parse: Duration::ZERO,
            nodes: LIMITS.nodes,
        };
        let aborted = scan(&deep, &limits);
        assert_eq!(aborted.skips, vec![PARSE_TIMEOUT]);
        assert!(aborted.commands.is_empty());
        // The program after an aborted one is parsed from byte 0 with correct text.
        let next = read("const x = 'é';\nawait tools.exec_command({cmd: 'echo ☃ é'})");
        assert_eq!(cmds(&next), vec!["echo ☃ é"]);
        assert!(next.skips.is_empty());
    }

    #[test]
    fn a_walk_past_its_node_budget_is_one_skip_and_no_row() {
        let source = "await tools.exec_command({cmd:'a'});\n".repeat(50);
        let limits = Limits {
            bytes: LIMITS.bytes,
            parse: LIMITS.parse,
            nodes: 100,
        };
        let program = scan(&source, &limits);
        assert_eq!(program.skips, vec![NODE_BUDGET]);
        assert!(program.commands.is_empty());
        assert_eq!(read(&source).commands.len(), 50);
    }

    #[test]
    fn two_hundred_thousand_nested_parentheses_do_not_crash_and_the_call_is_read_or_counted() {
        // Balanced: 120k levels is over the node budget (three nodes a level),
        // or slower than the deadline; either way one counted skip, no row.
        let balanced = format!(
            "{}1{}; await tools.exec_command({{cmd:'after'}})",
            "(".repeat(120_000),
            ")".repeat(120_000)
        );
        let program = read(&balanced);
        assert!(program.commands.is_empty(), "{program:?}");
        assert!(
            program.skips == [NODE_BUDGET] || program.skips == [PARSE_TIMEOUT],
            "{:?}",
            program.skips
        );
        // 200k unclosed: whatever the grammar makes of it, nothing is invented.
        let unclosed = format!(
            "{} 1; await tools.exec_command({{cmd:'after'}})",
            "(".repeat(200_000)
        );
        let program = read(&unclosed);
        assert!(
            program.commands.is_empty() && !program.skips.is_empty(),
            "{program:?}"
        );
    }

    #[test]
    fn a_program_without_a_tools_call_is_nothing_and_a_hidden_one_is_counted() {
        assert!(read("console.log(1 + 1)").skips.is_empty());
        let broken = read("tools.exec_command({cmd:'abc");
        assert!(
            broken.commands.is_empty() && !broken.skips.is_empty(),
            "{broken:?}"
        );
    }
}
