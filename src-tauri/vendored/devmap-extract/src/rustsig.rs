//! Reading a Rust function header as text: its parameter types, its return
//! type, and the argument types a closure parameter is declared to receive.
//!
//! The extractor records a Rust function's header — everything from the item
//! start to its body, whitespace collapsed — as [`ExtractedSymbol::signature`].
//! The resolver needs two facts out of it that the extraction of one file
//! cannot supply, because the function is usually declared in another file:
//!
//! * what `T::f(..)` returns, so `let paths = PathRanks::read(&c)?;` types
//!   `paths`;
//! * what a closure passed as the `k`th argument receives, so
//!   `note(shared, |c| c.merge())` types `c` from `note`'s
//!   `tally: impl FnOnce(&mut Collected)`.
//!
//! Text rather than a tree, because the reader is the resolver and the
//! resolver has no parse tree — the same reason [`crate::deref`] works on
//! strings. Every split here tracks bracket depth, so `impl Fn(u8) -> u8`
//! inside a parameter list is not mistaken for the function's own `->`, and a
//! header whose brackets do not balance answers nothing rather than a guess.
//!
//! [`ExtractedSymbol::signature`]: crate::model::ExtractedSymbol::signature

/// The longest header this records. A header past it is not recorded at all —
/// a truncated header would parse as a different, wrong one.
pub const MAX_SIGNATURE_BYTES: usize = 1024;

/// The parts of a header this module reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header<'a> {
    /// The text inside `<..>` after the name, if any.
    pub generics: Option<&'a str>,
    /// Each parameter as written, `self` receivers included.
    pub params: Vec<&'a str>,
    /// The type after the top-level `->`, if any.
    pub returns: Option<&'a str>,
    /// The text after a top-level `where`, if any.
    pub where_clause: Option<&'a str>,
}

/// Split a recorded header. `None` for anything that is not `… fn name(…)`
/// with balanced brackets.
pub fn parse_header(signature: &str) -> Option<Header<'_>> {
    let fn_at = find_fn_keyword(signature)?;
    let after_fn = &signature[fn_at + 3..];
    let name_end = after_fn
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ' '))
        .unwrap_or(after_fn.len());
    let mut rest = &after_fn[name_end..];
    let mut generics = None;
    if rest.starts_with('<') {
        let close = matching_close(rest, 0)?;
        generics = Some(rest[1..close].trim());
        rest = rest[close + 1..].trim_start();
    }
    if !rest.starts_with('(') {
        return None;
    }
    let close = matching_close(rest, 0)?;
    let params = split_top(&rest[1..close], ',')
        .into_iter()
        .map(str::trim)
        .filter(|param| !param.is_empty())
        .collect();
    let mut tail = rest[close + 1..].trim();
    let mut where_clause = None;
    if let Some(at) = find_top_word(tail, "where") {
        where_clause = Some(tail[at + 5..].trim().trim_end_matches(',').trim());
        tail = tail[..at].trim();
    }
    let returns = tail
        .strip_prefix("->")
        .map(str::trim)
        .filter(|ty| !ty.is_empty());
    Some(Header {
        generics,
        params,
        returns,
        where_clause,
    })
}

/// The type `T::f(..)` gives a binding, when the header says it is `T`.
///
/// `Self` and `type_name` (the bare name, any path before it ignored) are `T`.
/// `Arc`/`Rc`/`Box` of either are too, because a method call sees through
/// them. A `Result` or `Option` of either is `T` only when the caller
/// `unwrapped` it — `?`, `.unwrap()` or `.expect(..)` — since a `Result<T>`
/// has its own methods and a binding of it is not a `T`.
pub fn returns_type(header: &Header<'_>, type_name: &str, unwrapped: bool) -> bool {
    let Some(returns) = header.returns else {
        return false;
    };
    let mut ty = returns.trim();
    let mut peeled_wrapper = false;
    for _ in 0..8 {
        if names_type(ty, type_name) {
            return !peeled_wrapper || unwrapped;
        }
        let Some((outer, args)) = generic_parts(ty) else {
            return false;
        };
        let first = split_top(args, ',').into_iter().next().unwrap_or("").trim();
        if crate::deref::RUST_DEREF_TRANSPARENT.contains(&outer) {
            ty = first;
        } else if !peeled_wrapper && (outer == "Result" || outer == "Option") {
            peeled_wrapper = true;
            ty = first;
        } else {
            return false;
        }
    }
    false
}

/// The type a closure passed as argument `arg` receives as its parameter
/// `param`, when the header declares that argument as a closure type.
///
/// Read from `impl Fn*(A, B)`, `&dyn Fn*(..)`, `Box<dyn Fn*(..)>`, and a
/// generic `F` whose bound — in the angle brackets or the `where` clause — is a
/// `Fn*(..)`. `self` receivers are not arguments of a free-function call and
/// are skipped, so `arg` counts what the caller wrote.
pub fn closure_parameter<'a>(header: &Header<'a>, arg: usize, param: usize) -> Option<&'a str> {
    let declared = header
        .params
        .iter()
        .filter(|written| !is_self_param(written))
        .nth(arg)?;
    let (_, ty) = split_once_top(declared, ':')?;
    let ty = ty.trim();
    let callable = fn_trait_args(ty).or_else(|| {
        let generic = ty.trim();
        if !is_ident(generic) {
            return None;
        }
        bound_of(header, generic)
    })?;
    split_top(callable, ',')
        .into_iter()
        .map(str::trim)
        .filter(|arg| !arg.is_empty())
        .nth(param)
}

/// `"(A, B)"`'s inside for `impl Fn(A, B)`, `impl FnMut(A) -> R`,
/// `&dyn FnOnce(A)`, `&mut dyn FnMut(A)`, `Box<dyn Fn(A)>`.
fn fn_trait_args(ty: &str) -> Option<&str> {
    let mut ty = ty.trim();
    for _ in 0..8 {
        ty = ty
            .strip_prefix("&mut ")
            .or_else(|| ty.strip_prefix('&'))
            .map(str::trim_start)
            .unwrap_or(ty);
        if let Some((outer, args)) = generic_parts(ty) {
            if outer == "Box" {
                ty = args.trim();
                continue;
            }
            return None;
        }
        break;
    }
    let ty = ty
        .strip_prefix("impl ")
        .or_else(|| ty.strip_prefix("dyn "))?
        .trim_start();
    let mut bounds = split_top(ty, '+').into_iter();
    let first = bounds.next()?;
    fn_bound_args(first)
}

/// `"A, B"` for the bound `FnMut(A, B) -> R`; `None` for any other bound.
fn fn_bound_args(bound: &str) -> Option<&str> {
    let bound = bound.trim();
    let rest = ["FnOnce", "FnMut", "Fn"]
        .iter()
        .find_map(|name| bound.strip_prefix(name))?;
    if !rest.starts_with('(') {
        return None;
    }
    let close = matching_close(rest, 0)?;
    let after = rest[close + 1..].trim();
    if !(after.is_empty() || after.starts_with("->")) {
        return None;
    }
    Some(&rest[1..close])
}

/// The `Fn*(..)` argument list a generic parameter `name` is bounded by, in
/// the angle brackets or the `where` clause.
fn bound_of<'a>(header: &Header<'a>, name: &str) -> Option<&'a str> {
    let clauses = header
        .generics
        .into_iter()
        .chain(header.where_clause)
        .flat_map(|list| split_top(list, ','));
    for clause in clauses {
        let Some((bounded, bounds)) = split_once_top(clause, ':') else {
            continue;
        };
        if bounded.trim() != name {
            continue;
        }
        if let Some(args) = split_top(bounds, '+').into_iter().find_map(fn_bound_args) {
            return Some(args);
        }
    }
    None
}

/// `self`, `mut self`, `&self`, `&mut self`, `&'a self`, `self: Box<Self>`.
fn is_self_param(param: &str) -> bool {
    let param = param.trim();
    if param.starts_with("self:") || param.starts_with("mut self:") {
        return true;
    }
    let Some(last) = param.split_whitespace().last() else {
        return false;
    };
    let last = last.trim_start_matches('&');
    last == "self"
        && param
            .split_whitespace()
            .rev()
            .skip(1)
            .all(|word| word == "mut" || word.starts_with('&'))
}

/// Whether `ty` is exactly `Self` or `type_name`, with any leading path.
fn names_type(ty: &str, type_name: &str) -> bool {
    let bare = ty.trim().rsplit("::").next().unwrap_or(ty).trim();
    bare == "Self" || bare == type_name
}

/// `("Result", "Self, E")` for `std::io::Result<Self, E>`.
fn generic_parts(ty: &str) -> Option<(&str, &str)> {
    let ty = ty.trim();
    let open = ty.find('<')?;
    if matching_close(ty, open)? != ty.len() - 1 {
        return None;
    }
    let outer = ty[..open].trim().rsplit("::").next()?.trim();
    Some((outer, &ty[open + 1..ty.len() - 1]))
}

fn is_ident(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The byte offset of the `fn` keyword that starts the header.
fn find_fn_keyword(signature: &str) -> Option<usize> {
    let bytes = signature.as_bytes();
    let mut at = 0;
    while let Some(found) = signature[at..].find("fn ") {
        let index = at + found;
        if index == 0 || !(bytes[index - 1].is_ascii_alphanumeric() || bytes[index - 1] == b'_') {
            return Some(index);
        }
        at = index + 3;
    }
    None
}

/// The offset of the bracket closing the one at `open`, tracking `<>`, `()`
/// and `[]` together. `->` is an arrow, not a closing angle.
fn matching_close(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut index = open;
    while index < bytes.len() {
        match bytes[index] {
            b'<' | b'(' | b'[' => depth += 1,
            b'>' if index > 0 && bytes[index - 1] == b'-' => {}
            b'>' | b')' | b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
                if depth < 0 {
                    return None;
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

/// `text` split on `sep` wherever no bracket is open.
fn split_top(text: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let bytes = text.as_bytes();
    for (index, ch) in text.char_indices() {
        match ch {
            '<' | '(' | '[' => depth += 1,
            '>' if index > 0 && bytes[index - 1] == b'-' => {}
            '>' | ')' | ']' => depth -= 1,
            _ if ch == sep && depth == 0 => {
                parts.push(&text[start..index]);
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// `text` split at the first top-level `sep`. A `::` is a path, not a `:`.
fn split_once_top(text: &str, sep: char) -> Option<(&str, &str)> {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    for (index, ch) in text.char_indices() {
        match ch {
            '<' | '(' | '[' => depth += 1,
            '>' if index > 0 && bytes[index - 1] == b'-' => {}
            '>' | ')' | ']' => depth -= 1,
            ':' if sep == ':'
                && (bytes.get(index + 1) == Some(&b':')
                    || (index > 0 && bytes[index - 1] == b':')) => {}
            _ if ch == sep && depth == 0 => {
                return Some((&text[..index], &text[index + ch.len_utf8()..]));
            }
            _ => {}
        }
    }
    None
}

/// The offset of `word` standing alone at bracket depth zero.
fn find_top_word(text: &str, word: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    for (index, ch) in text.char_indices() {
        match ch {
            '<' | '(' | '[' => depth += 1,
            '>' if index > 0 && bytes[index - 1] == b'-' => {}
            '>' | ')' | ']' => depth -= 1,
            _ => {}
        }
        if depth == 0
            && text[index..].starts_with(word)
            && (index == 0 || bytes[index - 1] == b' ')
            && text[index + word.len()..]
                .chars()
                .next()
                .is_none_or(|next| next == ' ')
        {
            return Some(index);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_splits_into_its_parts() {
        let header = parse_header(
            "pub(crate) fn note<F: FnOnce(&mut Collected) -> u8>(shared: &Mutex<Collected>, \
             tally: F) -> WalkState where F: Send",
        )
        .unwrap();
        assert_eq!(header.generics, Some("F: FnOnce(&mut Collected) -> u8"));
        assert_eq!(header.params, vec!["shared: &Mutex<Collected>", "tally: F"]);
        assert_eq!(header.returns, Some("WalkState"));
        assert_eq!(header.where_clause, Some("F: Send"));
    }

    #[test]
    fn an_arrow_inside_a_parameter_is_not_the_return_type() {
        let header = parse_header("fn map(f: impl Fn(u8) -> u8)").unwrap();
        assert_eq!(header.returns, None);
        assert_eq!(header.params, vec!["f: impl Fn(u8) -> u8"]);
    }

    #[test]
    fn an_unbalanced_header_answers_nothing() {
        assert_eq!(parse_header("fn broken(a: Vec<u8)"), None);
        assert_eq!(parse_header("struct NotAFunction"), None);
    }

    #[test]
    fn a_constructor_returns_its_type_only_once_unwrapped_through_a_result() {
        let read = parse_header("fn read(conn: &Connection) -> Result<Self>").unwrap();
        assert!(returns_type(&read, "PathRanks", true));
        assert!(
            !returns_type(&read, "PathRanks", false),
            "a Result is not a PathRanks"
        );

        let open =
            parse_header("pub fn open(path: &Path) -> anyhow::Result<Store, Error>").unwrap();
        assert!(returns_type(&open, "Store", true));
        assert!(!returns_type(&open, "Other", true));

        let plain = parse_header("fn new() -> Self").unwrap();
        assert!(returns_type(&plain, "Anything", false));
        let shared = parse_header("fn shared() -> Arc<Self>").unwrap();
        assert!(returns_type(&shared, "Anything", false));

        for refused in [
            "fn list() -> Vec<Self>",
            "fn pair() -> (Self, Self)",
            "fn nested() -> Result<Option<Self>>",
            "fn nothing()",
            "fn other() -> Result<Other>",
        ] {
            let header = parse_header(refused).unwrap();
            assert!(!returns_type(&header, "Thing", true), "{refused}");
        }
    }

    #[test]
    fn a_closure_parameter_is_read_from_every_closure_spelling() {
        for (signature, expected) in [
            (
                "fn note(shared: &M, tally: impl FnOnce(&mut Collected)) -> W",
                "&mut Collected",
            ),
            (
                "fn note(shared: &M, tally: &dyn Fn(&Collected))",
                "&Collected",
            ),
            (
                "fn note(shared: &M, tally: Box<dyn FnMut(Collected) + Send>)",
                "Collected",
            ),
            (
                "fn note<F: FnOnce(&mut Collected)>(shared: &M, tally: F)",
                "&mut Collected",
            ),
            (
                "fn note<F>(shared: &M, tally: F) where F: Send + FnMut(&mut Collected) -> u8",
                "&mut Collected",
            ),
            (
                "fn note(&self, shared: &M, tally: impl Fn(&Collected))",
                "&Collected",
            ),
        ] {
            let header = parse_header(signature).unwrap();
            assert_eq!(
                closure_parameter(&header, 1, 0),
                Some(expected),
                "{signature}"
            );
        }
    }

    #[test]
    fn a_non_closure_argument_types_no_closure_parameter() {
        for signature in [
            "fn note(shared: &M, tally: Collected)",
            "fn note(shared: &M, tally: Vec<Collected>)",
            "fn note<F: Clone>(shared: &M, tally: F)",
            "fn note(shared: &M)",
        ] {
            let header = parse_header(signature).unwrap();
            assert_eq!(closure_parameter(&header, 1, 0), None, "{signature}");
        }
        let header = parse_header("fn note(tally: impl Fn(&A, &B))").unwrap();
        assert_eq!(closure_parameter(&header, 0, 1), Some("&B"));
        assert_eq!(closure_parameter(&header, 0, 2), None);
    }
}
