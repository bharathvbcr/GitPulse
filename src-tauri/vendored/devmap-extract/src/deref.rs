//! Which wrapper types a method call sees *through*.
//!
//! `Arc<Service>` is `Deref<Target = Service>` and `std::shared_ptr<Service>`
//! forwards member access through `operator->`, so in both languages
//! `receiver.method()` on one of these really does call the **inner** type's
//! method. That is a language rule, not an inference, which is why unwrapping
//! these is sound where unwrapping a generic in general is not.
//!
//! This module exists because the same question is asked at two places that
//! cannot share a traversal:
//!
//! * the extractor reads a **parse tree**, and answers it in
//!   `rust_type_name` / `c_type_name` by descending into the argument node;
//! * the resolver reads a **string** — the `declared_type` recorded on a local
//!   binding, which for C++ arrives as the raw text `shared_ptr<Service>` —
//!   and answers it in `admissible_nominal_type`.
//!
//! Two traversals are unavoidable. Two *lists* would not be: they would drift,
//! and the drift would be silent, because each side has its own tests and each
//! would keep passing. So the lists live here, once, and both sides consult
//! them.
//!
//! **What must never be added here** is as important as what is. `Vec<T>`,
//! `Option<T>`, `Result<T, E>`, `Mutex<T>`, `RwLock<T>`, `RefCell<T>` and
//! `weak_ptr<T>` all own their own methods — a caller must index, match,
//! `.lock()` or `.borrow()` before any `T` method is reachable. Unwrapping one
//! of those would not recover a missing edge; it would fabricate an edge to a
//! method the receiver does not have, and a graph that invents edges is worse
//! than one that misses them: a missing edge makes something look dead, an
//! invented edge makes something dead look live.
//!
//! The nesting falls out of that without a special case. `Arc<Mutex<Service>>`
//! unwraps once to `Mutex<Service>` and stops, because `Mutex` is not in the
//! list — which is the correct answer, not a partial one.

/// Rust wrappers that are `Deref<Target = T>`.
pub const RUST_DEREF_TRANSPARENT: &[&str] = &["Arc", "Rc", "Box"];

/// C++ wrappers that forward member access through `operator->`.
///
/// `weak_ptr` is deliberately absent: it has none, and must be `.lock()`ed into
/// a `shared_ptr` first.
pub const CPP_DEREF_TRANSPARENT: &[&str] = &["shared_ptr", "unique_ptr"];

/// Whether `name` — a bare type name, already stripped of any path — is a
/// wrapper a method call sees through.
pub fn is_deref_transparent(name: &str) -> bool {
    RUST_DEREF_TRANSPARENT.contains(&name) || CPP_DEREF_TRANSPARENT.contains(&name)
}

/// How deep a chain of wrappers this will unwrap before giving up.
///
/// Bounded for the reason [`crate::treesitter`]'s type walks are bounded: a
/// ~10 KB file of nested generics is four orders of magnitude under
/// `MAX_SOURCE_BYTES` and must not be able to turn into unbounded work. Past
/// the bound the type is simply not recovered — a lost qualification, never a
/// guessed one.
const MAX_UNWRAP_DEPTH: usize = 16;

/// The single type argument of a deref-transparent wrapper, written as text.
///
/// `"Arc<Service>"` and `"std::shared_ptr<Service>"` both answer `"Service"`;
/// `"Vec<Service>"` answers `None`, and so does anything carrying two type
/// arguments — a wrapper written with two is not the shape this rule is about,
/// and guessing which argument was meant would manufacture a receiver type.
fn unwrap_once(text: &str) -> Option<&str> {
    let text = text.trim();
    let open = text.find('<')?;
    let inner = text.strip_suffix('>')?.get(open + 1..)?;
    let outer = text.get(..open)?.trim().rsplit("::").next()?.trim();
    if !is_deref_transparent(outer) {
        return None;
    }
    // A comma at nesting depth zero means two type arguments. `Arc<Mutex<A>>`
    // has an interior comma at no depth and must not be confused with
    // `Pair<A, B>`, so the scan tracks depth rather than searching for ','.
    let mut depth = 0i32;
    for ch in inner.chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => return None,
            _ => {}
        }
    }
    Some(inner.trim())
}

/// Strip every deref-transparent wrapper from a written type, innermost first.
///
/// Returns the input unchanged when nothing is strippable, so a caller can use
/// it unconditionally. `Arc<Mutex<Service>>` returns `Mutex<Service>`: one
/// wrapper removed, and then a stop, because the remaining one is not
/// transparent.
pub fn strip_deref_transparent(text: &str) -> &str {
    let mut current = text.trim();
    for _ in 0..MAX_UNWRAP_DEPTH {
        match unwrap_once(current) {
            Some(inner) => current = inner,
            None => break,
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_wrappers_are_stripped_and_others_are_not() {
        assert_eq!(strip_deref_transparent("Arc<Service>"), "Service");
        assert_eq!(strip_deref_transparent("Rc<Service>"), "Service");
        assert_eq!(strip_deref_transparent("Box<Service>"), "Service");
        assert_eq!(
            strip_deref_transparent("std::sync::Arc<Service>"),
            "Service"
        );
        assert_eq!(strip_deref_transparent("shared_ptr<Service>"), "Service");
        assert_eq!(
            strip_deref_transparent("std::shared_ptr<Service>"),
            "Service"
        );
        assert_eq!(strip_deref_transparent("unique_ptr<Service>"), "Service");
        assert_eq!(strip_deref_transparent("Arc<Box<Service>>"), "Service");

        // The negative half, which is the one that matters.
        assert_eq!(strip_deref_transparent("Vec<Service>"), "Vec<Service>");
        assert_eq!(
            strip_deref_transparent("Option<Service>"),
            "Option<Service>"
        );
        assert_eq!(strip_deref_transparent("Mutex<Service>"), "Mutex<Service>");
        assert_eq!(
            strip_deref_transparent("weak_ptr<Service>"),
            "weak_ptr<Service>"
        );
        assert_eq!(
            strip_deref_transparent("Arc<Mutex<Service>>"),
            "Mutex<Service>",
            "one wrapper removed, then a stop — a caller must `.lock()` before \
             any `Service` method is reachable"
        );
        assert_eq!(
            strip_deref_transparent("Pair<A, B>"),
            "Pair<A, B>",
            "two type arguments is not this shape"
        );
        assert_eq!(strip_deref_transparent("Service"), "Service");
        assert_eq!(strip_deref_transparent(""), "");
    }

    /// Malformed and adversarial text must terminate and never panic.
    #[test]
    fn malformed_type_text_is_survivable() {
        for text in [
            "Arc<",
            "Arc>",
            "<>",
            "<",
            ">",
            "Arc<<<<>>>>",
            "Arc<Service",
            "Arc Service>",
            "::<>",
            "Arc<Arc<Arc<Arc<Arc<Arc<Arc<Arc<Arc<Arc<Service>>>>>>>>>>",
            "💥<Service>",
            "Arc<💥>",
        ] {
            let out = strip_deref_transparent(text);
            assert!(
                out.len() <= text.len(),
                "{text:?} produced something longer than itself: {out:?}"
            );
        }
    }

    /// The bound is a bound, not a suggestion.
    #[test]
    fn deep_nesting_terminates() {
        let deep = format!("{}Service{}", "Arc<".repeat(5_000), ">".repeat(5_000));
        let started = std::time::Instant::now();
        let out = strip_deref_transparent(&deep);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "5,000 levels did not terminate promptly"
        );
        assert!(
            out.len() < deep.len(),
            "some wrappers should have come off before the bound stopped it"
        );
    }
}
