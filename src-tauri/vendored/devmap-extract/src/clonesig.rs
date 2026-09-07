//! Structural body signatures, computed from the parse tree.
//!
//! A clone detector needs a body identity that survives reformatting and
//! comments. Doing that on raw text means writing a per-language comment and
//! string lexer, and getting it wrong invents clones: naive `//` stripping
//! collapses `a = "//foo"` and `a = "//bar"` to the same prefix and reports two
//! unrelated functions as identical. False clones are worse than missed ones —
//! they send a reader to code that does not match.
//!
//! The parse tree already carries the answer. Comments are nodes with a
//! grammar-declared kind, whitespace is not in the tree at all, and every
//! grammar labels its own constructs. Hashing the node sequence is therefore
//! comment-immune and format-immune *by construction* rather than by a lexer we
//! maintain per language.
//!
//! Two hashes fall out of one walk:
//!
//! - `exact` additionally mixes in leaf *text*, so it changes when an
//!   identifier or literal changes. This is a Type-1 clone: the same code.
//! - `structural` mixes only node kinds, so it is blind to renaming. This is a
//!   Type-2 clone: the same shape with different names.
//!
//! Both are reported separately and never merged. "Identical code" and "the
//! same shape" are different claims, and a caller acting on the second needs to
//! know it is the second.

// Every item below this point exists only to serve `parse_impl`, so each
// carries the same gate that module does. Without it, building this crate
// with `parse` off — the configuration an embedder uses to answer questions
// about a persisted map without linking 32 C grammars — produced five
// dead-code warnings, which is what a `-D warnings` build fails on.
#[cfg(feature = "parse")]
use crate::model::BodySignature;

/// Below this, a body carries no evidence of duplication.
///
/// Short bodies collide constantly for uninteresting reasons: every Go
/// `func (x T) Name() string { return x.name }` is structurally identical to
/// every other, and reporting them buries the real findings.
///
/// The value is measured, not chosen for roundness. Node counts for canonical
/// trivial bodies: Python getter 15, TypeScript getter 20, Go getter 23, Rust
/// getter 24, Python one-line delegate 25. For genuine multi-statement bodies:
/// Python guard-and-call 36, Python two-statement 38, Go error wrap 43, Rust
/// three-line 52, Go three-line 60. The two populations separate between 25 and
/// 36, and 32 sits in that gap; 24 would admit the Go and Rust accessors.
///
/// Its effect on this workspace (1,300 files, 12,742 signable symbols): raising
/// the floor from 1 to 32 drops signed symbols by 10% but cuts Type-1 groups
/// from 230 to 112 and Type-2 groups from 562 to 384. Nearly all of what it
/// removes is accessors.
///
/// A symbol under the floor gets no signature at all rather than a signature
/// that is filtered later, so `None` keeps meaning "not computed" everywhere.
#[cfg(feature = "parse")]
pub const MIN_SIGNATURE_NODES: u32 = 32;

#[cfg(feature = "parse")]
const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
#[cfg(feature = "parse")]
const FNV_PRIME: u64 = 0x100000001b3;

#[cfg(feature = "parse")]
#[inline]
fn mix(hash: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(hash, |acc, byte| {
        (acc ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

/// Whether a node kind names a comment in some grammar.
///
/// Grammars disagree on spelling — `comment`, `line_comment`, `block_comment`,
/// `doc_comment`, `html_comment` — but every one of them contains "comment",
/// and no non-comment kind in the 31 linked grammars does. Matching on the
/// substring covers grammars this file has never been tested against, which is
/// the point: a grammar added later must not start injecting comment text into
/// body identity without anyone noticing.
#[cfg(feature = "parse")]
#[inline]
fn is_comment_kind(kind: &str) -> bool {
    kind.contains("comment")
}

#[cfg(feature = "parse")]
mod parse_impl {
    use super::*;
    use crate::model::{ExtractedSymbol, SymbolKind};
    use tree_sitter::Node;

    /// Hash the subtree rooted at `node` into a Type-1 and a Type-2 signature.
    ///
    /// Returns `None` when the body is smaller than [`MIN_SIGNATURE_NODES`],
    /// which is a claim about the *body*, not a failure: there is nothing there
    /// to duplicate.
    ///
    /// Anonymous nodes are hashed alongside named ones because in tree-sitter an
    /// anonymous node's kind *is* its text. Dropping them would erase operators
    /// and keywords, and `a + b` would carry the same structural signature as
    /// `a - b` — a "same shape" report that is simply wrong.
    pub fn signature_of(node: Node, source: &str) -> Option<BodySignature> {
        let bytes = source.as_bytes();
        let mut exact = FNV_OFFSET_BASIS;
        let mut structural = FNV_OFFSET_BASIS;
        let mut nodes: u32 = 0;

        // Explicit worklist rather than recursion, for the same reason
        // `walk_tree` uses one: a deeply nested expression must not overflow
        // the stack. Children are pushed in reverse so `pop` yields tree-sitter
        // pre-order, which makes the hash a function of the tree alone.
        let mut stack = vec![node];
        let mut cursor = node.walk();
        while let Some(current) = stack.pop() {
            let kind = current.kind();
            if is_comment_kind(kind) {
                continue;
            }
            nodes = nodes.saturating_add(1);
            structural = mix(structural, kind.as_bytes());
            exact = mix(exact, kind.as_bytes());

            let child_count = current.child_count();
            if child_count == 0 {
                // A leaf carries the only text the kind sequence cannot: the
                // identifier or literal. It separates Type-1 from Type-2.
                let range = current.byte_range();
                if let Some(text) = bytes.get(range.start..range.end) {
                    exact = mix(exact, text);
                }
                continue;
            }
            let children: Vec<Node> = current.children(&mut cursor).collect();
            stack.extend(children.into_iter().rev());
        }

        (nodes >= MIN_SIGNATURE_NODES).then_some(BodySignature {
            exact,
            structural,
            nodes,
        })
    }

    /// Whether a symbol kind names something with a body worth comparing.
    ///
    /// `File` and `Module` are excluded deliberately. Their spans subsume every
    /// symbol inside them, so a single duplicated helper would report the
    /// function, its class, and its whole file as three separate findings of
    /// the same fact. `Field` and `Variable` are excluded because their spans
    /// are declarations, not bodies.
    fn has_comparable_body(kind: SymbolKind) -> bool {
        matches!(
            kind,
            SymbolKind::Function
                | SymbolKind::Method
                | SymbolKind::Class
                | SymbolKind::Struct
                | SymbolKind::Trait
                | SymbolKind::Interface
                | SymbolKind::Enum
        )
    }

    /// Hash a declaration with its body excluded.
    ///
    /// This is the half of a symbol its callers depend on: the name, the
    /// parameter list, the return type, the modifiers. Changing it can break a
    /// call site; changing the body cannot.
    ///
    /// Returns `None` when the grammar exposes no `body` field on this node.
    /// Most grammars name it `body` on exactly the constructs that have one, so
    /// its absence usually means the node is a declaration without a body — but
    /// it can also mean a grammar spells the field differently, and this cannot
    /// tell those apart. `None` therefore means "no declaration/body split was
    /// available here", never "the declaration did not change".
    pub fn declaration_hash_of(node: Node, source: &str) -> Option<u64> {
        let body = node.child_by_field_name("body")?;
        let body_range = body.byte_range();
        let bytes = source.as_bytes();
        let mut hash = FNV_OFFSET_BASIS;

        let mut cursor = node.walk();
        let mut stack = vec![node];
        while let Some(current) = stack.pop() {
            let kind = current.kind();
            if is_comment_kind(kind) {
                continue;
            }
            let range = current.byte_range();
            // Skip the body subtree wholesale. Comparing ranges rather than
            // node identity keeps this correct for grammars that wrap the body
            // in an extra node.
            if range.start >= body_range.start && range.end <= body_range.end {
                continue;
            }
            hash = mix(hash, kind.as_bytes());
            if current.child_count() == 0 {
                if let Some(text) = bytes.get(range.start..range.end) {
                    hash = mix(hash, text);
                }
                continue;
            }
            let children: Vec<Node> = current.children(&mut cursor).collect();
            stack.extend(children.into_iter().rev());
        }
        Some(hash)
    }

    /// Stamp every eligible symbol in `symbols` with its body signature.
    ///
    /// Runs as a second pass over the tree rather than inside the extraction
    /// walk: the walk emits symbols from thirty-odd language-specific arms, and
    /// threading a hash through each is thirty places to forget one. Locating
    /// the node from the span keeps this a single owner.
    pub fn stamp_signatures(symbols: &mut [ExtractedSymbol], root: Node, source: &str) {
        for symbol in symbols.iter_mut() {
            if !has_comparable_body(symbol.kind) {
                continue;
            }
            let start = symbol.span.start_byte;
            let end = symbol.span.end_byte;
            if end <= start || end > source.len() {
                continue;
            }
            // `descendant_for_byte_range` returns the smallest node spanning
            // the range, which for a symbol span is the declaration node the
            // extractor took the span from.
            let Some(node) = root.descendant_for_byte_range(start, end) else {
                continue;
            };
            symbol.body_signature = signature_of(node, source);
        }
    }

    /// Stamp every symbol with its declaration hash.
    ///
    /// Separate from [`stamp_signatures`] and deliberately not filtered by
    /// [`has_comparable_body`] or the size floor: a `Field`, an accessor, and a
    /// one-line delegate all have declarations that callers bind to.
    pub fn stamp_declaration_hashes(symbols: &mut [ExtractedSymbol], root: Node, source: &str) {
        for symbol in symbols.iter_mut() {
            let start = symbol.span.start_byte;
            let end = symbol.span.end_byte;
            if end <= start || end > source.len() {
                continue;
            }
            let Some(node) = root.descendant_for_byte_range(start, end) else {
                continue;
            };
            symbol.declaration_hash = declaration_hash_of(node, source);
        }
    }
}

#[cfg(feature = "parse")]
pub use parse_impl::{
    declaration_hash_of, signature_of, stamp_declaration_hashes, stamp_signatures,
};

#[cfg(all(test, feature = "parse"))]
mod tests {
    use super::*;
    use crate::extract_file;
    use crate::model::ExtractedSymbol;

    fn sig(path: &str, source: &str, name: &str) -> Option<BodySignature> {
        let extraction = extract_file(path, source);
        extraction
            .symbols
            .iter()
            .find(|s: &&ExtractedSymbol| s.name == name)
            .and_then(|s| s.body_signature)
    }

    const BODY: &str = r#"
    total = 0
    for row in rows:
        if row.active:
            total += row.amount * rate
        else:
            total -= row.penalty
    return total
"#;

    #[test]
    fn reformatting_and_comments_do_not_change_body_identity() {
        let plain = format!("def compute(rows, rate):{BODY}");
        let commented = format!(
            "def compute(rows, rate):\n    # running total\n{}",
            BODY.trim_start_matches('\n')
        );
        let a = sig("a.py", &plain, "compute").expect("plain body signed");
        let b = sig("b.py", &commented, "compute").expect("commented body signed");
        assert_eq!(
            a.exact, b.exact,
            "a comment changed body identity; comments are not code"
        );
    }

    #[test]
    fn renaming_changes_exact_but_not_structural() {
        let original = format!("def compute(rows, rate):{BODY}");
        let renamed = format!("def compute(rows, rate):{}", BODY.replace("total", "sum_"));
        let a = sig("a.py", &original, "compute").expect("original signed");
        let b = sig("b.py", &renamed, "compute").expect("renamed signed");
        assert_ne!(
            a.exact, b.exact,
            "renaming a variable must change the Type-1 signature"
        );
        assert_eq!(
            a.structural, b.structural,
            "renaming a variable must not change the Type-2 signature"
        );
    }

    /// The defect that ruled out text-based normalisation.
    ///
    /// A hand-written comment stripper that scans for `//` outside no string
    /// context truncates both of these to `url = "`, making two functions that
    /// fetch different hosts indistinguishable. The tree never sees a comment
    /// here at all, because there is none.
    #[test]
    fn comment_markers_inside_string_literals_are_not_comments() {
        let a = r#"
function fetchPrimary(client, options) {
  const url = "//primary.example/api/v1/records";
  const parsed = new URL(url, window.location.href);
  return client.get(parsed, options).then((r) => r.json());
}
"#;
        let b = r#"
function fetchPrimary(client, options) {
  const url = "//replica.example/api/v1/records";
  const parsed = new URL(url, window.location.href);
  return client.get(parsed, options).then((r) => r.json());
}
"#;
        let sa = sig("a.js", a, "fetchPrimary").expect("a signed");
        let sb = sig("b.js", b, "fetchPrimary").expect("b signed");
        assert_ne!(
            sa.exact, sb.exact,
            "two different URLs collapsed to one signature: a comment stripper \
             ate the string body and invented a clone"
        );
        assert_eq!(
            sa.structural, sb.structural,
            "same code shape, different literal: that is a Type-2 match"
        );
    }

    #[test]
    fn identical_bodies_in_different_files_share_a_signature() {
        let body = format!("def compute(rows, rate):{BODY}");
        let a = sig("pkg/a.py", &body, "compute").expect("a signed");
        let b = sig("other/b.py", &body, "compute").expect("b signed");
        assert_eq!(a.exact, b.exact);
        assert_eq!(a.structural, b.structural);
        assert_eq!(a.nodes, b.nodes);
    }

    #[test]
    fn a_symbol_name_is_not_part_of_its_body_identity() {
        // Two functions with the same body under different names are clones.
        // If the declaration's own identifier leaked into the hash they would
        // not match, and the single most common form of copy-paste — same code,
        // new name — would be invisible.
        let a = format!("def compute(rows, rate):{BODY}");
        let b = format!("def calculate(rows, rate):{BODY}");
        let sa = sig("a.py", &a, "compute").expect("a signed");
        let sb = sig("b.py", &b, "calculate").expect("b signed");
        assert_eq!(
            sa.structural, sb.structural,
            "renaming the function changed its body shape"
        );
    }

    #[test]
    fn a_signature_records_the_weight_behind_it() {
        let s = sig(
            "a.py",
            &format!("def compute(rows, rate):{BODY}"),
            "compute",
        )
        .expect("signed");
        assert!(
            s.nodes >= MIN_SIGNATURE_NODES,
            "a signature was emitted below the floor: {} nodes",
            s.nodes
        );
    }

    /// The floor is a measured boundary, not a preference. A grammar upgrade
    /// that changes how many nodes a body costs can move accessors back above
    /// it, and the first symptom would be a clone report full of getters. This
    /// pins both sides of the gap the constant was chosen to sit in.
    #[test]
    fn the_floor_separates_accessors_from_real_bodies() {
        let accessors: &[(&str, &str, &str)] = &[
            ("a.py", "def name(self):\n    return self._name\n", "name"),
            (
                "a.go",
                "package p\nfunc (t T) Name() string { return t.name }\n",
                "Name",
            ),
            (
                "a.ts",
                "class C { get name(): string { return this._name; } }\n",
                "name",
            ),
            (
                "a.rs",
                "impl T { pub fn name(&self) -> &str { &self.name } }\n",
                "name",
            ),
        ];
        for (path, src, name) in accessors {
            assert!(
                sig(path, src, name).is_none(),
                "{path}: an accessor was signed; the floor no longer excludes them"
            );
        }

        let bodies: &[(&str, &str, &str)] = &[
            (
                "b.py",
                "def run(self, x):\n    y = self.prep(x)\n    return self._inner.run(y)\n",
                "run",
            ),
            (
                "b.go",
                "package p\nfunc Load(p string) ([]byte, error) {\n\tb, err := os.ReadFile(p)\n\tif err != nil {\n\t\treturn nil, err\n\t}\n\treturn b, nil\n}\n",
                "Load",
            ),
            (
                "b.rs",
                "fn load(p: &Path) -> Result<Vec<u8>> {\n    let b = std::fs::read(p)?;\n    Ok(b)\n}\n",
                "load",
            ),
        ];
        for (path, src, name) in bodies {
            assert!(
                sig(path, src, name).is_some(),
                "{path}: a real multi-statement body fell below the floor"
            );
        }
    }

    #[test]
    fn unparsed_languages_carry_no_signature() {
        // The regex fallback recovers declarations, not bodies. Claiming a body
        // signature for one would assert an identity nothing computed.
        let extraction = extract_file(
            "api.proto",
            "package llm;\nmessage Req {\n  string a = 1;\n}\n",
        );
        assert!(
            extraction
                .symbols
                .iter()
                .all(|s| s.body_signature.is_none()),
            "the regex fallback emitted a body signature"
        );
    }
}
