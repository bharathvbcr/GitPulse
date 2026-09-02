/// Single HTML/text escape sink for generated artifacts (closes V1 / R8).
pub fn html_escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Escape JSON for an HTML raw-text `<script>` element without changing its
/// JSON meaning. HTML entities are not decoded inside script elements, so
/// `html_escape` would produce invalid JSON here. JSON unicode escapes keep
/// the HTML parser and `JSON.parse` on the same representation.
pub fn json_script_escape(raw: &str) -> String {
    raw.replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

/// Render a minimal symbol label safe for HTML embedding.
pub fn render_symbol_label(file_path: &str, symbol_name: &str) -> String {
    format!(
        "<span class=\"sym\" data-file=\"{}\" data-name=\"{}\">{}</span>",
        html_escape(file_path),
        html_escape(symbol_name),
        html_escape(symbol_name)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_v1_hostile_filename_renders_inert() {
        // closes V1
        let hostile = "x<img src=x onerror=alert(1)>.ts";
        let html = render_symbol_label(hostile, "handler");
        assert!(!html.contains("<img"));
        assert!(html.contains("data-file=\"x&lt;img src=x onerror=alert(1)&gt;.ts\""));
        assert!(html.contains("&lt;img"));
        assert!(html.contains("&gt;"));
    }

    #[test]
    fn test_html_escape_quotes_and_ampersand() {
        assert_eq!(html_escape("a&b"), "a&amp;b");
        assert_eq!(html_escape("\"x\""), "&quot;x&quot;");
    }

    #[test]
    fn json_script_escape_preserves_json_and_blocks_raw_script_end() {
        let raw = r#"{"name":"</script>","value":"a&b"}"#;
        let escaped = json_script_escape(raw);
        assert!(!escaped.contains("</script>"));
        let value: serde_json::Value = serde_json::from_str(&escaped).unwrap();
        assert_eq!(value["name"], "</script>");
        assert_eq!(value["value"], "a&b");
    }
}
