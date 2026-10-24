//! Adversarial: `alerts::text` against the renderer's `clean` over EVERY code
//! point. `scripts/fixtures/hidden-codepoints.json` is generated from the
//! Unicode data behind `\p{Cf}\p{Co}` / `\p{Cc}\s` (ranges of code points that
//! clean drops, and that it turns into a space); everything else is kept.

use super::text::{is_hidden, printable, sanitize};
use serde_json::Value;

fn ranges(table: &Value, key: &str) -> Vec<(u32, u32)> {
    table[key]
        .as_array()
        .expect("ranges")
        .iter()
        .map(|r| (r[0].as_u64().unwrap() as u32, r[1].as_u64().unwrap() as u32))
        .collect()
}

fn table() -> Value {
    serde_json::from_str(include_str!(
        "../../../scripts/fixtures/hidden-codepoints.json"
    ))
    .expect("fixture JSON")
}

#[test]
fn sanitize_matches_the_unicode_derived_table_for_every_code_point() {
    let table = table();
    let drop = ranges(&table, "drop");
    let space = ranges(&table, "space");
    let within = |rs: &[(u32, u32)], cp: u32| rs.iter().any(|(lo, hi)| cp >= *lo && cp <= *hi);
    let mut bad = Vec::new();
    for cp in 0..=0x10ffffu32 {
        let Some(ch) = char::from_u32(cp) else { continue };
        let got = sanitize(&format!("a{ch}b"));
        let kind = if got == "ab" {
            "drop"
        } else if got == "a b" {
            "space"
        } else if got == format!("a{ch}b") {
            "keep"
        } else {
            "other"
        };
        let want = if within(&drop, cp) {
            "drop"
        } else if within(&space, cp) {
            "space"
        } else {
            "keep"
        };
        if kind != want {
            bad.push(format!("U+{cp:04X} {kind} want {want}"));
        }
    }
    assert!(
        bad.is_empty(),
        "{} mismatches (first 40): {:?}",
        bad.len(),
        &bad[..bad.len().min(40)]
    );
}

#[test]
fn printable_and_is_hidden_agree_with_sanitize_on_every_code_point() {
    for cp in 0..=0x10ffffu32 {
        let Some(ch) = char::from_u32(cp) else { continue };
        let s = ch.to_string();
        let removed = printable(&s, 8).is_empty();
        let collapsed = sanitize(&s).is_empty();
        assert_eq!(removed, collapsed, "U+{cp:04X}");
        if is_hidden(ch) {
            assert!(collapsed, "U+{cp:04X} hidden but kept");
        }
    }
}
