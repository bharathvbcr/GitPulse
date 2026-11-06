//! Later copies of one Rust function or method get their own qualified name.
//!
//! Two `#[cfg]` bodies are both real declarations. They used to share one
//! qualified name, so every call inside either body was stored on that one
//! identity and the later span showed the other body's calls. The earliest
//! span keeps the name. Each later span becomes `name@<start_byte>`.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::{ExtractedCall, ExtractedReference, ExtractedSymbol, Extraction, SymbolKind};

use super::{walk_deadline_passed, DEADLINE_CHECK_STRIDE};

struct Rename {
    old: String,
    new_name: String,
    start: usize,
    end: usize,
    index: usize,
}

/// Split later Rust copies onto their own qualified names.
///
/// Returns false when the extraction deadline passes. The caller refuses the
/// file: a half-applied split is not a complete read. No IO. The clock is the
/// one the walk already armed.
pub(super) fn split_rust_cfg_copies(extraction: &mut Extraction) -> bool {
    if extraction.language != "rust" {
        return true;
    }
    if walk_deadline_passed() {
        return false;
    }
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, symbol) in extraction.symbols.iter().enumerate() {
        if index.is_multiple_of(DEADLINE_CHECK_STRIDE as usize) && walk_deadline_passed() {
            return false;
        }
        if matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method) {
            groups
                .entry(symbol.qualified_name.clone())
                .or_default()
                .push(index);
        }
    }
    let mut taken: BTreeSet<String> = extraction
        .symbols
        .iter()
        .map(|symbol| symbol.qualified_name.clone())
        .collect();
    let mut renames: Vec<Rename> = Vec::new();
    for indexes in groups.into_values() {
        if walk_deadline_passed() {
            return false;
        }
        renames.extend(plan_group(&extraction.symbols, &indexes, &mut taken));
    }
    if renames.is_empty() {
        return !walk_deadline_passed();
    }
    for rename in &renames {
        extraction.symbols[rename.index]
            .qualified_name
            .clone_from(&rename.new_name);
    }
    if !retarget_symbols(extraction, &renames) {
        return false;
    }
    if !retarget_calls(&mut extraction.calls, &renames) {
        return false;
    }
    if !retarget_calls(&mut extraction.rust_implicit_calls, &renames) {
        return false;
    }
    if !retarget_calls(&mut extraction.rust_try_conversions, &renames) {
        return false;
    }
    if !retarget_references(&mut extraction.references, &renames) {
        return false;
    }
    if !retarget_bindings(extraction, &renames) {
        return false;
    }
    duplicate_scope_locals(extraction, &renames)
}

fn plan_group(
    symbols: &[ExtractedSymbol],
    indexes: &[usize],
    taken: &mut BTreeSet<String>,
) -> Vec<Rename> {
    if indexes.len() < 2 {
        return Vec::new();
    }
    let kind = symbols[indexes[0]].kind;
    if indexes.iter().any(|&index| symbols[index].kind != kind) {
        return Vec::new();
    }
    let mut ordered = indexes.to_vec();
    ordered.sort_by(|&left, &right| {
        let a = &symbols[left].span;
        let b = &symbols[right].span;
        (a.start_byte, a.end_byte).cmp(&(b.start_byte, b.end_byte))
    });
    for pair in ordered.windows(2) {
        let earlier = &symbols[pair[0]].span;
        let later = &symbols[pair[1]].span;
        if earlier.end_byte > later.start_byte {
            return Vec::new();
        }
    }
    let old = symbols[ordered[0]].qualified_name.clone();
    let mut renames = Vec::new();
    for &index in ordered.iter().skip(1) {
        let start = symbols[index].span.start_byte;
        let end = symbols[index].span.end_byte;
        let Some(new_name) = allocate(&old, start, end, taken) else {
            continue;
        };
        renames.push(Rename {
            old: old.clone(),
            new_name,
            start,
            end,
            index,
        });
    }
    renames
}

/// `@<start>` when that string is free, otherwise `@` plus the digits of the
/// start and the end with no second `@`. A dash would stop
/// `cfg_copy_base` from seeing the base. `None` when both spellings are
/// already another symbol's full name; that copy stays on the shared name
/// rather than colliding with it.
fn allocate(old: &str, start: usize, end: usize, taken: &mut BTreeSet<String>) -> Option<String> {
    let primary = format!("{old}@{start}");
    if taken.insert(primary.clone()) {
        return Some(primary);
    }
    let secondary = format!("{old}@{start}{end}");
    if taken.insert(secondary.clone()) {
        return Some(secondary);
    }
    None
}

fn retarget_symbols(extraction: &mut Extraction, renames: &[Rename]) -> bool {
    let renamed: BTreeSet<usize> = renames.iter().map(|rename| rename.index).collect();
    for (index, symbol) in extraction.symbols.iter_mut().enumerate() {
        if index.is_multiple_of(DEADLINE_CHECK_STRIDE as usize) && walk_deadline_passed() {
            return false;
        }
        let skip = renamed.contains(&index).then_some(index);
        let Some(container) =
            innermost(renames, symbol.span.start_byte, symbol.span.end_byte, skip)
        else {
            continue;
        };
        if !renamed.contains(&index) {
            rewrite_owned(
                &mut symbol.qualified_name,
                &container.old,
                &container.new_name,
            );
        }
        if let Some(parent) = symbol.parent_symbol.as_mut() {
            rewrite_owned(parent, &container.old, &container.new_name);
        }
    }
    true
}

fn retarget_calls(calls: &mut [ExtractedCall], renames: &[Rename]) -> bool {
    for (index, call) in calls.iter_mut().enumerate() {
        if index.is_multiple_of(DEADLINE_CHECK_STRIDE as usize) && walk_deadline_passed() {
            return false;
        }
        let Some(container) = innermost(renames, call.span.start_byte, call.span.end_byte, None)
        else {
            continue;
        };
        if let Some(caller) = call.caller_symbol.as_mut() {
            rewrite_owned(caller, &container.old, &container.new_name);
        }
    }
    true
}

fn retarget_references(references: &mut [ExtractedReference], renames: &[Rename]) -> bool {
    for (index, reference) in references.iter_mut().enumerate() {
        if index.is_multiple_of(DEADLINE_CHECK_STRIDE as usize) && walk_deadline_passed() {
            return false;
        }
        let Some(container) = innermost(
            renames,
            reference.span.start_byte,
            reference.span.end_byte,
            None,
        ) else {
            continue;
        };
        if let Some(enclosing) = reference.enclosing_symbol.as_mut() {
            rewrite_owned(enclosing, &container.old, &container.new_name);
        }
    }
    true
}

fn retarget_bindings(extraction: &mut Extraction, renames: &[Rename]) -> bool {
    for (index, binding) in extraction.local_bindings.iter_mut().enumerate() {
        if index.is_multiple_of(DEADLINE_CHECK_STRIDE as usize) && walk_deadline_passed() {
            return false;
        }
        let Some(container) = innermost_point(renames, binding.start_byte) else {
            continue;
        };
        if let Some(scope) = binding.scope.as_mut() {
            rewrite_owned(scope, &container.old, &container.new_name);
        }
    }
    true
}

fn duplicate_scope_locals(extraction: &mut Extraction, renames: &[Rename]) -> bool {
    let mut extra = Vec::new();
    for (index, (scope, local)) in extraction.scope_locals.iter().enumerate() {
        if index.is_multiple_of(DEADLINE_CHECK_STRIDE as usize) && walk_deadline_passed() {
            return false;
        }
        for rename in renames {
            if let Some(rewritten) = rewritten_if_prefix(scope, &rename.old, &rename.new_name) {
                extra.push((rewritten, local.clone()));
            }
        }
    }
    if extra.is_empty() {
        return !walk_deadline_passed();
    }
    extraction.scope_locals.extend(extra);
    extraction.scope_locals.sort();
    extraction.scope_locals.dedup();
    !walk_deadline_passed()
}

fn innermost(renames: &[Rename], start: usize, end: usize, skip: Option<usize>) -> Option<&Rename> {
    renames
        .iter()
        .filter(|rename| skip != Some(rename.index) && rename.start <= start && end <= rename.end)
        .min_by_key(|rename| (rename.end.saturating_sub(rename.start), rename.start))
}

fn innermost_point(renames: &[Rename], byte: usize) -> Option<&Rename> {
    renames
        .iter()
        .filter(|rename| rename.start <= byte && byte < rename.end)
        .min_by_key(|rename| (rename.end.saturating_sub(rename.start), rename.start))
}

fn rewrite_owned(value: &mut String, old: &str, new: &str) {
    if let Some(rewritten) = rewritten_if_prefix(value, old, new) {
        *value = rewritten;
    }
}

/// Exact match, or `old` followed by `::` or `.`. `old_other` and a name that
/// merely contains `old` stay as written.
fn rewritten_if_prefix(value: &str, old: &str, new: &str) -> Option<String> {
    if old.is_empty() {
        return None;
    }
    if value == old {
        return Some(new.to_string());
    }
    let rest = value.strip_prefix(old)?;
    if rest.starts_with("::") || rest.starts_with('.') {
        return Some(format!("{new}{rest}"));
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::model::{cfg_canonical, cfg_copy_base, SymbolKind};

    #[test]
    fn the_marker_is_only_a_trailing_run_of_digits() {
        assert_eq!(cfg_copy_base("src/a.rs::func"), None);
        assert_eq!(cfg_copy_base("src/a.rs::func@"), None);
        assert_eq!(cfg_copy_base("src/a.rs::func@12"), Some("src/a.rs::func"));
        assert_eq!(cfg_copy_base("src/a.rs::func@12-40"), None);
        assert_eq!(cfg_copy_base("crates/foo@2/src/lib.rs::func"), None);
        assert_eq!(cfg_copy_base("src/odd 'quote' 名前.rs::名前"), None);
        let names = BTreeSet::from(["src/a.rs::func"]);
        assert_eq!(cfg_canonical("src/a.rs::func@12", &names), "src/a.rs::func");
        assert_eq!(
            cfg_canonical("src/a.rs::func@12", &BTreeSet::new()),
            "src/a.rs::func@12"
        );
        assert_eq!(
            cfg_canonical("src/a.rs::other@12", &names),
            "src/a.rs::other@12"
        );
    }

    #[cfg(feature = "parse")]
    fn rust_fn(source: &str) -> crate::model::Extraction {
        crate::extract_file("src/a.rs", source)
    }

    #[cfg(feature = "parse")]
    fn host_names(extraction: &crate::model::Extraction) -> Vec<String> {
        extraction
            .symbols
            .iter()
            .filter(|symbol| symbol.name == "host")
            .map(|symbol| symbol.qualified_name.clone())
            .collect()
    }

    /// Overlapping spans are not two bodies. Splitting them would invent a
    /// second function the parser did not separate.
    #[cfg(feature = "parse")]
    #[test]
    fn overlapping_spans_stay_on_the_shared_name() {
        let mut extraction = rust_fn("fn host() { live(); }\nfn live() {}\n");
        let host = extraction
            .symbols
            .iter()
            .find(|symbol| symbol.name == "host")
            .unwrap()
            .clone();
        let mut overlap = host.clone();
        overlap.span.start_byte = host.span.start_byte + 1;
        overlap.span.end_byte = host.span.end_byte + 1;
        extraction.symbols.push(overlap);
        assert!(super::split_rust_cfg_copies(&mut extraction));
        let names = host_names(&extraction);
        assert_eq!(names.len(), 2, "{names:?}");
        assert_eq!(names[0], names[1], "{names:?}");
        assert!(!names[0].contains('@'), "{names:?}");
    }

    /// A function and a method that happen to share a name are not cfg copies.
    #[cfg(feature = "parse")]
    #[test]
    fn mixed_kinds_stay_on_the_shared_name() {
        let mut extraction = rust_fn("fn host() {}\n");
        let host = extraction
            .symbols
            .iter()
            .find(|symbol| symbol.name == "host")
            .unwrap()
            .clone();
        let mut method = host.clone();
        method.kind = SymbolKind::Method;
        method.span.start_byte = host.span.end_byte + 10;
        method.span.end_byte = method.span.start_byte + 4;
        extraction.symbols.push(method);
        assert!(super::split_rust_cfg_copies(&mut extraction));
        let names = host_names(&extraction);
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.iter().all(|name| !name.contains('@')), "{names:?}");
    }

    /// `name@<start>` already belongs to another symbol, so the copy takes
    /// `name@<start><end>`, which is still one run of digits.
    #[cfg(feature = "parse")]
    #[test]
    fn a_taken_primary_suffix_uses_both_ends() {
        let source = "\
#[cfg(unix)]
fn host() { live(); }
#[cfg(not(unix))]
fn host() {}
fn live() {}
";
        let mut extraction = rust_fn(source);
        let names = host_names(&extraction);
        let canonical = names
            .iter()
            .find(|name| !name.contains('@'))
            .unwrap()
            .clone();
        let later = extraction
            .symbols
            .iter()
            .find(|symbol| symbol.name == "host" && symbol.qualified_name.contains('@'))
            .unwrap()
            .clone();
        let start = later.span.start_byte;
        let end = later.span.end_byte;
        for symbol in &mut extraction.symbols {
            if symbol.name == "host" {
                symbol.qualified_name.clone_from(&canonical);
            }
        }
        let mut decoy = later.clone();
        decoy.name = "decoy".to_string();
        decoy.qualified_name = format!("{canonical}@{start}");
        decoy.span.start_byte = end + 50;
        decoy.span.end_byte = decoy.span.start_byte + 3;
        extraction.symbols.push(decoy);
        assert!(super::split_rust_cfg_copies(&mut extraction));
        let names = host_names(&extraction);
        let secondary = format!("{canonical}@{start}{end}");
        assert!(names.iter().any(|name| name == &canonical), "{names:?}");
        assert!(names.iter().any(|name| name == &secondary), "{names:?}");
        assert!(
            !names
                .iter()
                .any(|name| name == &format!("{canonical}@{start}")),
            "{names:?}"
        );
        assert!(extraction
            .symbols
            .iter()
            .any(|symbol| symbol.name == "decoy"
                && symbol.qualified_name == format!("{canonical}@{start}")));
    }

    /// A renamed copy gains scope rows. The original rows stay, and a name
    /// that only shares a prefix (`host_other`) is not rewritten.
    #[cfg(feature = "parse")]
    #[test]
    fn scope_locals_of_a_renamed_copy_are_duplicated_not_removed() {
        let source = "\
#[cfg(unix)]
fn host() { let x = 1; }
#[cfg(not(unix))]
fn host() { let x = 1; }
";
        let mut extraction = rust_fn(source);
        let canonical = host_names(&extraction)
            .into_iter()
            .find(|name| !name.contains('@'))
            .unwrap();
        for symbol in &mut extraction.symbols {
            if symbol.name == "host" {
                symbol.qualified_name.clone_from(&canonical);
            }
        }
        let inner = format!("{canonical}::inner");
        let other = format!("{canonical}_other");
        extraction.scope_locals = vec![
            (canonical.clone(), "x".to_string()),
            (inner.clone(), "y".to_string()),
            (other.clone(), "z".to_string()),
        ];
        assert!(super::split_rust_cfg_copies(&mut extraction));
        let scopes = extraction.scope_locals.clone();
        assert!(
            scopes
                .iter()
                .any(|(scope, local)| scope == &canonical && local == "x"),
            "{scopes:?}"
        );
        assert!(
            scopes
                .iter()
                .any(|(scope, local)| scope == &inner && local == "y"),
            "{scopes:?}"
        );
        assert!(
            scopes
                .iter()
                .any(|(scope, local)| scope == &other && local == "z"),
            "{scopes:?}"
        );
        assert_eq!(
            scopes.iter().filter(|(scope, _)| scope == &other).count(),
            1,
            "{scopes:?}"
        );
        assert!(
            scopes.iter().any(|(scope, local)| {
                scope.starts_with(&format!("{canonical}@"))
                    && scope.ends_with("::inner")
                    && local == "y"
            }),
            "{scopes:?}"
        );
        assert!(
            scopes.iter().any(|(scope, local)| {
                let Some(rest) = scope.strip_prefix(&format!("{canonical}@")) else {
                    return false;
                };
                rest.bytes().all(|byte| byte.is_ascii_digit()) && local == "x"
            }),
            "{scopes:?}"
        );
    }

    /// The walk clock is already past, so the split refuses and leaves both
    /// bodies on the shared name. The clock is thread-local and cleared
    /// before the test returns, including on panic.
    #[cfg(feature = "parse")]
    #[test]
    fn a_passed_deadline_refuses_the_split_and_renames_nothing() {
        let source = "\
#[cfg(unix)]
fn host() { live(); }
#[cfg(not(unix))]
fn host() {}
fn live() {}
";
        let mut extraction = rust_fn(source);
        let canonical = host_names(&extraction)
            .into_iter()
            .find(|name| !name.contains('@'))
            .unwrap();
        for symbol in &mut extraction.symbols {
            if symbol.name == "host" {
                symbol.qualified_name.clone_from(&canonical);
            }
        }
        struct Clear;
        impl Drop for Clear {
            fn drop(&mut self) {
                super::super::budget::WALK_DEADLINE.with(|slot| slot.set(None));
                super::super::budget::WALK_OVERRAN.with(|slot| slot.set(false));
            }
        }
        let _clear = Clear;
        super::super::budget::WALK_DEADLINE.with(|slot| slot.set(Some(std::time::Instant::now())));
        assert!(!super::split_rust_cfg_copies(&mut extraction));
        let names = host_names(&extraction);
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.iter().all(|name| name == &canonical), "{names:?}");
    }

    /// The split is Rust-only. Another language keeps the shared name.
    #[cfg(feature = "parse")]
    #[test]
    fn a_non_rust_file_is_not_split() {
        let mut extraction = rust_fn("fn host() {}\n");
        extraction.language = "go".to_string();
        let host = extraction
            .symbols
            .iter()
            .find(|symbol| symbol.name == "host")
            .unwrap()
            .clone();
        let mut later = host.clone();
        later.span.start_byte = host.span.end_byte + 10;
        later.span.end_byte = later.span.start_byte + 4;
        extraction.symbols.push(later);
        assert!(super::split_rust_cfg_copies(&mut extraction));
        let names = host_names(&extraction);
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.iter().all(|name| !name.contains('@')), "{names:?}");
    }
}
