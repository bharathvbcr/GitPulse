//! Markdown for GitPulse surfaces, through MarkDev's renderer.
//!
//! Rendering is MarkDev's own (`markdev-html`) — the pipeline its export
//! uses — so a README reads here the way it does in MarkDev, nested lists,
//! tables, callouts, footnotes and all. GitPulse used to rebuild HTML from
//! the editor's live-preview model, slicing raw source ranges; every `**`,
//! `# `, `> ` and `](url)` leaked into the page and nested structure was
//! flattened. What this module adds is GitPulse's boundary: how much one
//! render takes, and which files a render may read.
//!
//! Repository Markdown is untrusted. The renderer escapes raw HTML as text
//! and refuses active URL schemes; reads are confined to the repository by
//! [`FileAccess::Vault`], through the same validation as `cmd_get_file_blob`.

use std::path::Path;

use markdev_html::{render_fragment, ExportOptions, FileAccess, Fragment, HTMLExportError};
use serde::{Deserialize, Serialize};

use crate::engine::git_cli::{sandbox_join_entry, validate_repo};

/// Ceiling shared with the TypeScript client. Enforced before IPC so a
/// megabyte CHANGELOG cannot freeze the UI or the backend.
///
/// Rendering is linear in MarkDev's own code, but pulldown-cmark itself is
/// super-linear on some adversarial inputs (alternating `*a_` quadruples
/// when it doubles): at this cap that is about a second, off-thread.
pub const MAX_RENDER_BYTES: usize = 128 * 1024;

/// Most picture bytes one render embeds. The viewer re-renders as a note is
/// edited, and every embedded byte is read, base64-encoded and sent over IPC
/// each time; MarkDev's own ceiling (32 MiB) is sized for a one-off export.
pub const MAX_EMBEDDED_IMAGE_BYTES: usize = 8 * 1024 * 1024;

/// Most frontmatter fields returned, so a pathological block cannot grow the
/// header without bound.
const MAX_FRONTMATTER_FIELDS: usize = 64;

/// A rendered note, as the frontend embeds it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RenderedMarkdown {
    /// Sanitized body HTML. Styling belongs to the page (`.gp-markdown`).
    pub html: String,
    /// The note's headings in order, each with the `id` its element carries —
    /// the outline. Derived by the same pass that wrote the ids, so an outline
    /// entry can never name an id the page lacks.
    pub headings: Vec<MarkdownHeading>,
    /// `key: value` (or TOML `key = value`) lines of the frontmatter block,
    /// which is not part of `html`.
    pub frontmatter: Vec<FrontmatterField>,
    /// UTF-8 bytes of the source past [`MAX_RENDER_BYTES`] that were not
    /// rendered; 0 when the whole note was.
    pub omitted_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownHeading {
    pub level: u8,
    pub title: String,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FrontmatterField {
    pub key: String,
    pub value: String,
}

/// Renders `text` for embedding.
///
/// `note` is where it lives — a repository and a repository-relative path —
/// when it is a file: relative pictures are then read from the repository
/// and embedded, never from outside it. Without one (a commit message, a PR
/// body) nothing is read from disk.
pub fn render(text: &str, note: Option<(&str, &str)>) -> Result<RenderedMarkdown, String> {
    let slice = cap(text);
    let omitted_bytes = text.len() - slice.len();
    let fragment = match note {
        Some((repo_path, file_path)) => {
            let repo = validate_repo(repo_path)?;
            let entry = sandbox_join_entry(&repo, file_path)?;
            let folder = entry.parent().unwrap_or(&repo).to_path_buf();
            render_with(slice, Some((&repo, &folder)))
        }
        None => render_with(slice, None),
    }
    .map_err(describe_render_error)?;
    Ok(RenderedMarkdown {
        html: fragment.html,
        headings: fragment
            .headings
            .into_iter()
            .map(|h| MarkdownHeading {
                level: h.level,
                title: h.text,
                id: h.id,
            })
            .collect(),
        frontmatter: fragment
            .frontmatter
            .as_deref()
            .map(frontmatter_fields)
            .unwrap_or_default(),
        omitted_bytes,
    })
}

/// `files` is the repository root and the note's folder inside it.
fn render_with(text: &str, files: Option<(&Path, &Path)>) -> Result<Fragment, HTMLExportError> {
    let (vault_root, asset_base, file_access) = match files {
        Some((repo, folder)) => (Some(repo), Some(folder), FileAccess::Vault),
        None => (None, None, FileAccess::None),
    };
    render_fragment(
        text,
        &ExportOptions {
            asset_base,
            vault_root,
            file_access,
            // Remote pictures load — the app's CSP admits https images — and
            // go without a referrer. Remote audio and video stay blocked by
            // the CSP's default-src.
            remote_media: true,
            max_embedded_bytes: Some(MAX_EMBEDDED_IMAGE_BYTES),
            ..Default::default()
        },
    )
}

/// The first [`MAX_RENDER_BYTES`] of `text`, cut after a line when one ends
/// in range so the last rendered line is whole.
fn cap(text: &str) -> &str {
    if text.len() <= MAX_RENDER_BYTES {
        return text;
    }
    let mut end = MAX_RENDER_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let end = text[..end].rfind('\n').map_or(end, |newline| newline + 1);
    &text[..end]
}

/// `key: value` and TOML `key = value` lines; nested YAML is not unfolded.
fn frontmatter_fields(block: &str) -> Vec<FrontmatterField> {
    block
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let cut = line.find([':', '='])?;
            let key = line[..cut].trim();
            if key.is_empty() {
                return None;
            }
            let value = line[cut + 1..].trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(value);
            Some(FrontmatterField {
                key: key.to_owned(),
                value: value.to_owned(),
            })
        })
        .take(MAX_FRONTMATTER_FIELDS)
        .collect()
}

fn describe_render_error(err: HTMLExportError) -> String {
    match err {
        HTMLExportError::SourceTooLarge { actual, maximum } => {
            format!("markdown is {actual} bytes, over the {maximum} byte render limit")
        }
        HTMLExportError::TitleTooLarge { .. } => "markdown title is too large".into(),
        HTMLExportError::OutputTooLarge { maximum, .. } => {
            format!("rendered markdown exceeds the {maximum} byte output limit")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn html(source: &str) -> String {
        render(source, None).unwrap().html
    }

    // The defects the old renderer had, one per construct. Each assertion
    // failed against it: it sliced raw source ranges out of the editor model.

    #[test]
    fn syntax_is_consumed_not_shown() {
        for (source, leaked) in [
            ("# Hello", "# Hello"),
            ("Some **bold** text", "**"),
            ("`code`", "`code`"),
            ("> quoted", "&gt; quoted"),
            ("- item", "- item"),
            ("[a](https://x.y)", "](https"),
            ("~~gone~~", "~~"),
            ("> [!NOTE]\n> body", "[!NOTE]"),
            ("$$\nx^2\n$$", "$$"),
        ] {
            let rendered = html(source);
            assert!(!rendered.contains(leaked), "{source:?} leaked {leaked:?}: {rendered}");
        }
    }

    #[test]
    fn nested_structure_survives() {
        assert!(html("**a *b* c**").contains("<strong>a <em>b</em> c</strong>"));
        assert!(html("[a **b**](https://x.y)").contains("<a href=\"https://x.y\">a <strong>b</strong></a>"));
        let list = html("- one\n  - nested\n- two");
        assert!(list.contains("<li>one\n<ul>\n<li>nested</li>\n</ul>\n</li>"), "{list}");
        assert!(html("3. three\n4. four").contains("<ol start=\"3\">"));
        let table = html("| a | b |\n|-|-:|\n| 1 | 2 |");
        assert!(table.contains("<thead><tr><th>a</th><th style=\"text-align: right\">b</th></tr></thead>"), "{table}");
        assert!(html("a  \nb").contains("a<br />"));
        assert!(html("a &amp; b").contains("a &amp; b") && !html("a &amp; b").contains("&amp;amp;"));
    }

    #[test]
    fn the_outline_names_the_ids_the_page_carries() {
        let rendered = render("# Intro\n\n## Intro\n\nSetext\n===\n\n```\n# not a heading\n```", None).unwrap();
        let outline: Vec<(u8, &str, &str)> = rendered
            .headings
            .iter()
            .map(|h| (h.level, h.title.as_str(), h.id.as_str()))
            .collect();
        assert_eq!(outline, [(1, "Intro", "intro"), (2, "Intro", "intro-1"), (1, "Setext", "setext")]);
        for heading in &rendered.headings {
            assert!(rendered.html.contains(&format!("id=\"{}\"", heading.id)), "{}", heading.id);
        }
    }

    #[test]
    fn frontmatter_is_returned_as_fields_not_rendered() {
        let rendered = render("---\ntitle: \"Hello\"\ntags: [a, b]\n---\n# Body", None).unwrap();
        assert_eq!(
            rendered.frontmatter,
            [
                FrontmatterField { key: "title".into(), value: "Hello".into() },
                FrontmatterField { key: "tags".into(), value: "[a, b]".into() },
            ]
        );
        assert!(!rendered.html.contains("title"), "{}", rendered.html);
        let toml = render("+++\ntitle = \"T\"\n+++\nx", None).unwrap();
        assert_eq!(toml.frontmatter, [FrontmatterField { key: "title".into(), value: "T".into() }]);
    }

    #[test]
    fn hostile_markup_stays_inert() {
        for source in [
            "<script>alert(1)</script>",
            "<img src=x onerror=alert(1)>",
            "[c](javascript:alert(1))",
            "[c](JaVaScRiPt:alert(1))",
            "![i](javascript:alert(1))",
            "[[javascript:alert(1)]]",
        ] {
            // Author HTML may appear as escaped text; it must never be a tag,
            // and no attribute may carry the active scheme.
            let rendered = html(source).to_ascii_lowercase();
            assert!(!rendered.contains("<script"), "{source}: {rendered}");
            assert!(!rendered.contains("<svg"), "{source}: {rendered}");
            for tag in rendered.split('<').skip(1).filter_map(|rest| rest.split_once('>')) {
                assert!(!tag.0.contains("onerror"), "{source}: {rendered}");
                assert!(!tag.0.contains("javascript:"), "{source}: {rendered}");
                assert!(!tag.0.contains("src=\"x\""), "{source}: {rendered}");
            }
        }
    }

    #[test]
    fn remote_pictures_load_without_a_referrer() {
        let rendered = html("![shot](https://example.com/s.png)");
        assert!(rendered.contains("src=\"https://example.com/s.png\""), "{rendered}");
        assert!(rendered.contains("referrerpolicy=\"no-referrer\""), "{rendered}");
    }

    #[test]
    fn oversized_input_is_cut_at_a_line_and_the_rest_is_counted() {
        let line = "word ".repeat(19) + "\n";
        let big = line.repeat(MAX_RENDER_BYTES / line.len() + 50);
        let rendered = render(&big, None).unwrap();
        assert!(rendered.omitted_bytes > 0);
        let kept = big.len() - rendered.omitted_bytes;
        assert!(kept <= MAX_RENDER_BYTES);
        assert_eq!(&big[kept - 1..kept], "\n", "cut mid-line");
        assert_eq!(render("small", None).unwrap().omitted_bytes, 0);
        // A single line longer than the cap is cut at a character boundary.
        let wide = "é".repeat(MAX_RENDER_BYTES);
        let rendered = render(&wide, None).unwrap();
        assert_eq!((wide.len() - rendered.omitted_bytes) % 2, 0);
    }

    #[test]
    fn a_note_reads_pictures_from_its_repository_and_nowhere_else() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(repo.join("docs")).unwrap();
        crate::test_support::git_in(&repo, &["init", "-b", "main"]);
        let png: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89";
        std::fs::write(repo.join("docs/inside.png"), png).unwrap();
        std::fs::write(root.path().join("secret.png"), png).unwrap();
        let repo_path = repo.to_str().unwrap();
        let rendered = render(
            "![in](inside.png) ![out](../../secret.png) ![abs](/etc/hosts)",
            Some((repo_path, "docs/README.md")),
        )
        .unwrap();
        assert_eq!(rendered.html.matches("src=\"data:image/png").count(), 1, "{}", rendered.html);
        // No location: nothing is read at all.
        let unplaced = render("![in](inside.png)", None).unwrap();
        assert!(!unplaced.html.contains("data:image"), "{}", unplaced.html);
        // A location outside the repository is refused, not rendered unconfined.
        assert!(render("x", Some((repo_path, "../outside.md"))).is_err());
    }

    /// `harness/markdown.html` drives the viewer with this command's answer
    /// for a kitchen-sink note. A hand-written copy of that answer would drift
    /// from the renderer silently, and the harness would then pass against
    /// markup GitPulse no longer produces; so the copy is the renderer's own
    /// output, and this fails the moment the two differ.
    /// `GITPULSE_BLESS_FIXTURES=1` rewrites it.
    #[test]
    fn the_harness_fixture_is_what_render_returns() {
        let harness = Path::new(env!("CARGO_MANIFEST_DIR")).join("../harness");
        let source = std::fs::read_to_string(harness.join("markdown-kitchen-sink.md")).unwrap();
        let rendered = render(&source, None).unwrap();
        let actual = format!("{}\n", serde_json::to_string_pretty(&rendered).unwrap());
        let fixture = harness.join("markdownRender.fixture.json");
        if std::env::var_os("GITPULSE_BLESS_FIXTURES").is_some() {
            std::fs::write(&fixture, &actual).unwrap();
        }
        let expected = std::fs::read_to_string(&fixture).unwrap_or_default();
        assert!(
            expected == actual,
            "harness/markdownRender.fixture.json is not what render() returns for \
             harness/markdown-kitchen-sink.md; rerun this test with GITPULSE_BLESS_FIXTURES=1"
        );
    }
}
