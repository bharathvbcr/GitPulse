//! Official GitHub Linguist colours for the language ids this kernel emits.
//!
//! Single owner for "what colour is this language". The map preview
//! ([`crate::map_preview`]) reads it; nothing else carries a palette.
//!
//! Values are transcribed from `lib/linguist/languages.yml` in
//! github-linguist/linguist **v9.7.0** (released 2026-08-26) — the `color:`
//! field, which is what GitHub paints its repository language bar with. The
//! table is a literal on purpose: the preview is an offline artifact that must
//! not reach the network, and a pinned table is auditable against a pinned
//! upstream tag.
//!
//! Four ids resolve to [`NEUTRAL_COLOR`] and must never be dressed up as if
//! Linguist had an opinion about them:
//!
//! * `cobol` and `protobuf` are Linguist languages that declare no `color`.
//! * `config` and `generic` are ours, not Linguist's — the buckets
//!   [`devmap_extract::detect_language`] returns for manifests and for anything
//!   it could not classify.
//!
//! [`Swatch::official`] carries that distinction so a legend can draw "grey
//! because Linguist says so" differently from "grey because nobody knows".
//! `every_declared_language_has_a_swatch` keeps the table honest against
//! [`devmap_extract::declared_language_ids`], so adding a grammar without a
//! colour fails the build's tests rather than shipping a silently grey node.

use std::collections::BTreeMap;

/// Linguist tag the table below was transcribed from. Bump it with the table.
pub const LINGUIST_VERSION: &str = "v9.7.0";

/// Slate grey for ids with no official colour. Deliberately outside the
/// Linguist palette so an uncoloured language never passes for a coloured one.
pub const NEUTRAL_COLOR: &str = "#6e7681";

/// A language's presentation: what to paint it, what to call it, and whether
/// the colour is Linguist's or our fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Swatch {
    pub color: &'static str,
    pub label: &'static str,
    pub official: bool,
}

/// `(devmap language id, Linguist colour or None, display label)`.
///
/// Sorted by id; `swatch` binary-searches it.
const TABLE: &[(&str, Option<&str>, &str)] = &[
    ("astro", Some("#ff5a03"), "Astro"),
    ("c", Some("#555555"), "C"),
    ("cfml", Some("#ed2cd6"), "CFML"),
    ("cobol", None, "COBOL"),   // Linguist declares no colour
    ("config", None, "Config"), // not a Linguist language
    ("cpp", Some("#f34b7d"), "C++"),
    ("csharp", Some("#7355dd"), "C#"),
    ("css", Some("#663399"), "CSS"),
    ("cuda", Some("#3A4E3A"), "Cuda"),
    ("dart", Some("#00B4AB"), "Dart"),
    ("erlang", Some("#B83998"), "Erlang"),
    ("generic", None, "Other"), // not a Linguist language
    ("go", Some("#00ADD8"), "Go"),
    ("hcl", Some("#844FBA"), "HCL"),
    ("html", Some("#e34c26"), "HTML"),
    ("java", Some("#b07219"), "Java"),
    ("javascript", Some("#f1e05a"), "JavaScript"),
    ("json", Some("#292929"), "JSON"),
    ("kotlin", Some("#A97BFF"), "Kotlin"),
    ("liquid", Some("#67b8de"), "Liquid"),
    ("lua", Some("#000080"), "Lua"),
    ("luau", Some("#00A2FF"), "Luau"),
    ("markdown", Some("#083fa1"), "Markdown"),
    ("nix", Some("#7e7eff"), "Nix"),
    ("notebook", Some("#DA5B0B"), "Notebook"),
    ("objc", Some("#438eff"), "Objective-C"),
    ("pascal", Some("#E3F171"), "Pascal"),
    ("php", Some("#4F5D95"), "PHP"),
    ("powershell", Some("#012456"), "PowerShell"),
    ("protobuf", None, "Protocol Buffer"), // Linguist declares no colour
    ("python", Some("#3572A5"), "Python"),
    ("r", Some("#198CE7"), "R"),
    ("ruby", Some("#701516"), "Ruby"),
    ("rust", Some("#dea584"), "Rust"),
    ("scala", Some("#c22d40"), "Scala"),
    ("shell", Some("#89e051"), "Shell"),
    ("solidity", Some("#AA6746"), "Solidity"),
    ("sql", Some("#e38c00"), "SQL"),
    ("svelte", Some("#ff3e00"), "Svelte"),
    ("swift", Some("#F05138"), "Swift"),
    ("toml", Some("#9c4221"), "TOML"),
    ("tsx", Some("#3178c6"), "TSX"),
    ("typescript", Some("#3178c6"), "TypeScript"),
    ("vb", Some("#945db7"), "VB.NET"),
    ("vue", Some("#41b883"), "Vue"),
    ("yaml", Some("#cb171e"), "YAML"),
];

/// Colour, label and provenance for a language id.
///
/// An id with no row is neutral and labelled as itself rather than folded into
/// "Other": an unrecognised id is a table gap worth seeing on the page, and
/// merging it into the unknown bucket is how the gap stays invisible.
pub fn swatch(language_id: &str) -> Swatch {
    let key = language_id.trim().to_ascii_lowercase();
    match TABLE.binary_search_by(|(id, _, _)| (*id).cmp(key.as_str())) {
        Ok(index) => {
            let (_, color, label) = TABLE[index];
            match color {
                Some(color) => Swatch {
                    color,
                    label,
                    official: true,
                },
                None => Swatch {
                    color: NEUTRAL_COLOR,
                    label,
                    official: false,
                },
            }
        }
        Err(_) => Swatch {
            color: NEUTRAL_COLOR,
            label: "unknown",
            official: false,
        },
    }
}

/// Every id the table knows, coloured or explicitly not.
pub fn known_language_ids() -> Vec<&'static str> {
    TABLE.iter().map(|(id, _, _)| *id).collect()
}

/// `{id: {color, label, official}}` for the ids given, ready to embed.
///
/// A page ships swatches for the languages it actually shows rather than all
/// of them; unknown ids still get an entry so the page never has to invent one.
pub fn palette<'a, I>(language_ids: I) -> BTreeMap<String, Swatch>
where
    I: IntoIterator<Item = &'a str>,
{
    language_ids
        .into_iter()
        .map(|id| {
            let key = id.trim().to_ascii_lowercase();
            let value = swatch(&key);
            (key, value)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_unique() {
        // `swatch` binary-searches, so an out-of-order row would not merely be
        // ugly — it would be unfindable.
        let ids = known_language_ids();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(ids, sorted, "TABLE must be sorted by id and free of dupes");
    }

    #[test]
    fn every_declared_language_has_a_swatch() {
        // The drift guard. `declared_language_ids` is the exact set
        // `detect_language` can return, so a grammar added to the extractor
        // without a colour row fails here instead of shipping a grey node that
        // looks like a deliberate choice.
        let missing: Vec<&str> = devmap_extract::declared_language_ids()
            .into_iter()
            .filter(|id| {
                TABLE
                    .binary_search_by(|(known, _, _)| (*known).cmp(id))
                    .is_err()
            })
            .collect();
        assert!(
            missing.is_empty(),
            "language ids declared by devmap-extract with no Linguist row: {missing:?}"
        );
    }

    #[test]
    fn official_colours_are_six_digit_hex() {
        for (id, color, _) in TABLE {
            let Some(color) = color else { continue };
            assert!(
                color.len() == 7
                    && color.starts_with('#')
                    && color[1..].chars().all(|c| c.is_ascii_hexdigit()),
                "{id} has a malformed colour {color}"
            );
        }
    }

    #[test]
    fn uncoloured_ids_are_neutral_and_say_so() {
        // The honesty contract: a language Linguist has no colour for must be
        // distinguishable from one it does, or the legend lies about which
        // greys were a choice.
        for id in ["cobol", "protobuf", "config", "generic"] {
            let s = swatch(id);
            assert_eq!(s.color, NEUTRAL_COLOR, "{id} should be neutral");
            assert!(!s.official, "{id} must not claim an official colour");
        }
        let python = swatch("python");
        assert_eq!(python.color, "#3572A5");
        assert!(python.official);
    }

    #[test]
    fn unknown_ids_are_neutral_but_keep_their_identity() {
        let s = swatch("brainfuck");
        assert_eq!(s.color, NEUTRAL_COLOR);
        assert!(!s.official);
        assert_eq!(s.label, "unknown");
    }

    #[test]
    fn lookup_is_case_and_space_insensitive() {
        assert_eq!(swatch("  Python ").color, swatch("python").color);
        assert_eq!(swatch("RUST").label, "Rust");
    }

    #[test]
    fn palette_covers_exactly_what_it_is_asked_for() {
        let p = palette(["python", "rust", "cobol"]);
        assert_eq!(p.len(), 3);
        assert!(p["python"].official);
        assert!(!p["cobol"].official);
        assert!(!p.contains_key("go"));
    }
}
