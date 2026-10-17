//! What the index keeps of a file whose contents are credentials.
//!
//! DevCouncil's write gate refuses every path in `policy.SecretPathPatterns`,
//! and its read gate refuses to show their bytes, because a value that enters
//! a model's context cannot be redacted afterwards. The index used to be a way
//! round both: `devmap literals AKIA` returned the key from `credentials/`, and
//! `devmap ask --evidence` printed `secrets/` source verbatim, and both values
//! sat at rest in the store, which is copied between worktrees.
//!
//! A secret file is still indexed — its symbols, calls and edges are structure,
//! and a caller asking where `load_prod_database_password` lives deserves the
//! answer — but every *value* it writes is replaced here, once, before the
//! extraction reaches the cache, the store or any query. Readers that show
//! source text from disk ask [`is_secret_path`] themselves and mark what they
//! withheld with [`WITHHELD_SECRET_PATH`].

use std::borrow::Cow;

use crate::model::{Extraction, ReferenceKind};

/// The `withheld` marker a query answer carries for an item whose value or
/// source span was not shown because its file is a secret path.
pub const WITHHELD_SECRET_PATH: &str = "secret_path";

/// What a string literal in a secret file is stored and reported as.
///
/// A constant rather than a dropped row, so `devmap literals` can still say
/// that a secret file writes a literal at that line, and nothing about what.
pub const WITHHELD_LITERAL_VALUE: &str = "<withheld: secret_path>";

/// Whether `path` is one of `policy.SecretPathPatterns`. See
/// [`dc_glob::is_secret_path`], which owns the list and its Go parity check.
pub fn is_secret_path(path: &str) -> bool {
    dc_glob::is_secret_path(path)
}

/// The text the index may keep, or show, for a string literal `value` written
/// in `path`.
///
/// A secret file's literal is [`WITHHELD_LITERAL_VALUE`], whatever it says.
/// Anywhere else a literal keeps its text unless it carries a credential shape
/// `dc-redact` recognises — an AWS key id in `config/settings.py` is as much a
/// credential as one under `credentials/` — and then it keeps that shape's
/// identifying prefix and length and nothing more, the rendering every secret
/// gate in the harness reports. One owner, asked by the extractor before a
/// value is stored and by `literals` before one is shown, so a store written
/// before this rule answers the same way a fresh one does.
pub fn literal_value_for<'a>(path: &str, value: &'a str) -> Cow<'a, str> {
    kept_literal_value(is_secret_path(path), value)
}

/// [`literal_value_for`] with the path already classified, so a file's
/// literals ask about its path once rather than once each.
fn kept_literal_value(secret: bool, value: &str) -> Cow<'_, str> {
    if secret {
        Cow::Borrowed(WITHHELD_LITERAL_VALUE)
    } else if dc_redact::contains_secret(value) {
        Cow::Owned(dc_redact::redact_secrets(value))
    } else {
        Cow::Borrowed(value)
    }
}

/// Replaces the credential values an extraction carries.
///
/// Every literal passes through [`literal_value_for`]. The references made
/// from string text — a selector string, an identifier-shaped literal read as
/// a runtime entry name — have the literal as their `name`, so they follow
/// it: dropped from a secret file, and dropped elsewhere when the name carries
/// a credential shape (an AWS key id is identifier-shaped). A secret file also
/// loses its docstrings, which are its own prose. Symbols, spans, calls and
/// the other references are kept: they say where code is, not what a
/// credential is.
pub fn withhold_secret_values(extraction: &mut Extraction) {
    let secret = is_secret_path(&extraction.file_path);
    for literal in &mut extraction.literals {
        let kept = kept_literal_value(secret, &literal.value);
        if kept != literal.value.as_str() {
            let kept = kept.into_owned();
            literal.value = kept;
        }
    }
    extraction.references.retain(|reference| {
        !matches!(
            reference.kind,
            ReferenceKind::EntryName | ReferenceKind::Selector
        ) || !(secret || dc_redact::contains_secret(&reference.name))
    });
    if secret {
        for symbol in &mut extraction.symbols {
            symbol.docstring = None;
        }
    }
}

/// [`withhold_secret_values`] for a secret file's payload as stored JSON.
///
/// For the store's own scrub of payload rows written before values were
/// withheld. Those rows may come from an older payload schema this build
/// cannot deserialize into an [`Extraction`], so the same three rules are
/// applied to the JSON itself — a test holds the two forms to one output.
/// Returns whether anything changed. A file that is not a secret path is left
/// untouched: its literals' credential shapes are redacted when the file is
/// next extracted, and at query time until then.
pub fn withhold_secret_values_json(path: &str, payload: &mut serde_json::Value) -> bool {
    if !is_secret_path(path) {
        return false;
    }
    let mut changed = false;
    if let Some(literals) = payload.get_mut("literals").and_then(|v| v.as_array_mut()) {
        for literal in literals {
            if let Some(value) = literal.get_mut("value") {
                if value.as_str() != Some(WITHHELD_LITERAL_VALUE) {
                    *value = serde_json::Value::from(WITHHELD_LITERAL_VALUE);
                    changed = true;
                }
            }
        }
    }
    if let Some(symbols) = payload.get_mut("symbols").and_then(|v| v.as_array_mut()) {
        for symbol in symbols {
            if let Some(docstring) = symbol.get_mut("docstring") {
                if !docstring.is_null() {
                    *docstring = serde_json::Value::Null;
                    changed = true;
                }
            }
        }
    }
    if let Some(references) = payload.get_mut("references").and_then(|v| v.as_array_mut()) {
        let before = references.len();
        references.retain(|reference| {
            !matches!(
                reference.get("kind").and_then(|kind| kind.as_str()),
                Some("EntryName" | "Selector")
            )
        });
        changed |= references.len() != before;
    }
    changed
}

#[cfg(all(test, feature = "parse"))]
mod tests {
    use super::*;
    use crate::model::ReferenceKind;

    /// Values in the shapes the audit planted: an AWS key id, a GitHub token,
    /// a Slack bot token, and a bare password no vendor shape describes.
    const PLANTED: &[&str] = &[
        "AKIAIOSFODNN7EXAMPLQ",
        "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
        "xoxb-123456789012-abcdefghijkl",
        "hunter2",
    ];

    const SOURCE: &str = r#"AWS_KEY = "AKIAIOSFODNN7EXAMPLQ"
GH_TOKEN = "ghp_abcdefghijklmnopqrstuvwxyz0123456789"


def load_prod_database_password():
    """The production password is hunter2."""
    return "hunter2"


def slack():
    return "xoxb-123456789012-abcdefghijkl"
"#;

    fn carries_planted(text: &str) -> Option<&'static str> {
        PLANTED.iter().copied().find(|value| text.contains(value))
    }

    #[test]
    fn a_secret_file_is_indexed_with_every_value_withheld() {
        for path in [
            "secrets/db.py",
            "credentials/aws.py",
            "deploy/secrets/db.py",
        ] {
            let extraction = crate::extract_file(path, SOURCE);
            assert!(
                extraction
                    .symbols
                    .iter()
                    .any(|symbol| symbol.name == "load_prod_database_password"),
                "{path}: the structure is still indexed"
            );
            assert!(!extraction.literals.is_empty(), "{path}: sites are kept");
            for literal in &extraction.literals {
                assert_eq!(literal.value, WITHHELD_LITERAL_VALUE, "{path}");
            }
            assert!(extraction.symbols.iter().all(|s| s.docstring.is_none()));
            assert!(!extraction.references.iter().any(|reference| matches!(
                reference.kind,
                ReferenceKind::EntryName | ReferenceKind::Selector
            )));
            let mut durable = extraction.for_durable_store();
            durable.source_code = None;
            let stored = serde_json::to_string(&durable).unwrap();
            assert_eq!(carries_planted(&stored), None, "{path}: {stored}");
        }
    }

    #[test]
    fn an_ordinary_file_keeps_its_literals_but_not_credential_shapes() {
        let extraction = crate::extract_file("config/settings.py", SOURCE);
        let values: Vec<&str> = extraction
            .literals
            .iter()
            .map(|l| l.value.as_str())
            .collect();
        assert!(
            values.contains(&"hunter2"),
            "a plain string is kept: {values:?}"
        );
        let mut durable = extraction.for_durable_store();
        durable.source_code = None;
        let stored = serde_json::to_string(&durable).unwrap();
        for shaped in &PLANTED[..3] {
            assert!(!stored.contains(shaped), "{shaped} survived: {stored}");
        }
        assert!(
            values
                .iter()
                .any(|value| value.starts_with("AKIA") && value.contains("chars")),
            "the redaction keeps the identifying prefix: {values:?}"
        );
    }

    /// An extraction that never passed through `extract_file` — a cache row
    /// written before values were withheld — is withheld on its way to disk.
    #[test]
    fn a_durable_copy_is_withheld_whatever_produced_the_extraction() {
        let mut raw = crate::extract_file("src/db.py", SOURCE);
        raw.file_path = "secrets/db.py".to_string();
        assert!(raw.literals.iter().any(|l| l.value == "hunter2"));
        let durable = raw.for_durable_store();
        assert!(durable
            .literals
            .iter()
            .all(|l| l.value == WITHHELD_LITERAL_VALUE));
    }

    /// The JSON form the store's scrub applies to old payload rows says what
    /// the struct form says, for every field either one touches.
    #[test]
    fn the_json_scrub_agrees_with_the_struct_scrub() {
        let mut raw = crate::extract_file("src/db.py", SOURCE);
        raw.file_path = "secrets/db.py".to_string();
        // A stored payload never carries the source text.
        raw.source_code = None;
        let mut json = serde_json::to_value(&raw).unwrap();
        assert!(withhold_secret_values_json(&raw.file_path, &mut json));
        assert!(
            !withhold_secret_values_json(&raw.file_path, &mut json),
            "idempotent"
        );
        withhold_secret_values(&mut raw);
        let structured = serde_json::to_value(&raw).unwrap();
        let empty = serde_json::Value::Array(Vec::new());
        for field in ["literals", "symbols", "references"] {
            assert_eq!(
                json.get(field).unwrap_or(&empty),
                structured.get(field).unwrap_or(&empty),
                "{field}"
            );
        }
        assert_eq!(carries_planted(&json.to_string()), None);

        let mut ordinary = serde_json::to_value(crate::extract_file("src/db.py", SOURCE)).unwrap();
        assert!(!withhold_secret_values_json("src/db.py", &mut ordinary));
    }
}
