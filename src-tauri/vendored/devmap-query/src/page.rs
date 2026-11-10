//! The one HTML shell both DevMap pages are rendered through.
//!
//! [`crate::viz::render_html`] (the code graph) and
//! [`crate::map_preview::render_map_preview_html`] (the repo map) used to carry
//! two templates, two copies of a force-graph bundle and two ways to embed their
//! payload. They now differ only in the JSON they hand to [`render_shell`]: the
//! page itself is the TypeScript package under `assets/code-map`, built by Bun
//! into one bundle that is inlined here, and it decides which page to draw from
//! the payload (a `view` block is the code graph, a `subsystems` list the repo
//! map).
//!
//! # What is and is not in the markup
//!
//! Nothing from the repository is interpolated into HTML except through
//! [`html_escape`] (the `<title>`, the fingerprint comment) or as JSON inside a
//! raw-text script element, escaped by [`json_script_escape`] so `</script>` and
//! `<!--` cannot occur in it. The page puts every name into the DOM as a text
//! node, so a hostile file name is inert structurally rather than by escaping
//! at each sink.
//!
//! The bundle is inlined raw, so it must never contain `</script` or `<!--`;
//! `the_bundle_cannot_end_its_own_script_block` pins that.

use serde_json::Value;

use crate::escape::{html_escape, json_script_escape};

/// The Bun-built page bundle (`npm run build:code-map`), inlined so a page
/// opens from a `file://` URL with no network.
///
/// The `.bundle` suffix keeps it out of the DevMap index: as a `.js` this
/// minified file is `is_indexable_source`, and a single long line exhausts the
/// 5 s per-file parse budget, which makes two extractions of identical bytes
/// disagree (an R4 determinism violation; see the history of the force-graph
/// bundle this replaces).
const CODE_MAP_BUNDLE: &str = include_str!("../assets/code-map.js.bundle");

/// `id` of the JSON element the page reads its payload from. The TypeScript
/// entry (`page/main.ts`, `PAYLOAD_ELEMENT_ID`) reads the same id.
pub(crate) const PAYLOAD_ELEMENT_ID: &str = "devmap-payload";

/// Render a page: `title` is the document title, `fingerprint` an optional
/// freshness stamp kept as an HTML comment, `payload` what the page draws.
pub(crate) fn render_shell(title: &str, fingerprint: Option<&str>, payload: &Value) -> String {
    let raw = serde_json::to_string(payload).unwrap_or_else(|_| "null".to_string());
    let json = json_script_escape(&raw);
    let stamp = fingerprint
        .map(|value| format!("<!-- fingerprint:{} -->\n", html_escape(value)))
        .unwrap_or_default();
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\"/>\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"/>\n\
<title>{title}</title>\n</head>\n<body>\n{stamp}<div id=\"app\"></div>\n\
<script type=\"application/json\" id=\"{PAYLOAD_ELEMENT_ID}\">{json}</script>\n\
<script>{CODE_MAP_BUNDLE}</script>\n</body>\n</html>\n",
        title = html_escape(title),
    )
}

/// The payload a rendered page carries, parsed back out of its markup.
///
/// Test support: the hostile-name tests assert both that the markup has no raw
/// tag and that the value survives the round trip, and both need the element's
/// text exactly as a browser would hand it to `JSON.parse`.
#[cfg(test)]
pub(crate) fn embedded_payload(html: &str) -> Value {
    let open = format!("id=\"{PAYLOAD_ELEMENT_ID}\">");
    let start = html
        .find(&open)
        .unwrap_or_else(|| panic!("the page has no #{PAYLOAD_ELEMENT_ID} element"))
        + open.len();
    let end = html[start..]
        .find("</script>")
        .expect("the payload element is closed");
    serde_json::from_str(&html[start..start + end]).expect("the embedded payload is valid JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_bundle_cannot_end_its_own_script_block() {
        // It is inlined raw. `</script` ends the element whatever JavaScript
        // thinks of it, and `<!--` can open an escaped state that swallows the
        // closing tag.
        assert!(
            CODE_MAP_BUNDLE.len() > 10_000,
            "the bundle is the real build"
        );
        assert!(!CODE_MAP_BUNDLE.to_ascii_lowercase().contains("</script"));
        assert!(!CODE_MAP_BUNDLE.contains("<!--"));
    }

    #[test]
    fn the_payload_is_escaped_for_a_raw_text_script_and_survives_the_round_trip() {
        let payload =
            json!({"name": "</script><script>alert(1)</script><!-- &", "sep": "\u{2028}"});
        let html = render_shell("t", None, &payload);
        let between = html
            .split(&format!("id=\"{PAYLOAD_ELEMENT_ID}\">"))
            .nth(1)
            .unwrap()
            .split("</script>")
            .next()
            .unwrap();
        for raw in ["<", ">", "&", "\u{2028}"] {
            assert!(!between.contains(raw), "{raw:?} must be a unicode escape");
        }
        assert_eq!(embedded_payload(&html), payload);
    }

    #[test]
    fn the_title_and_fingerprint_are_escaped_markup_not_markup() {
        let html = render_shell("<b>x</b>", Some("a-->b<script>"), &json!({}));
        assert!(html.contains("<title>&lt;b&gt;x&lt;/b&gt;</title>"));
        assert!(html.contains("<!-- fingerprint:a--&gt;b&lt;script&gt; -->"));
        assert_eq!(html.matches("id=\"app\"").count(), 1);
        assert_eq!(
            html.matches(&format!("id=\"{PAYLOAD_ELEMENT_ID}\""))
                .count(),
            1
        );
    }
}
