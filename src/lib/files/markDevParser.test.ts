import { describe, expect, it } from "vitest";
import {
  MAX_RENDER_BYTES,
  calculateDocumentStats,
  extractDocumentOutline,
  parseFrontmatter,
  renderMarkDevMarkdown,
} from "./markDevParser";

describe("markDevParser", () => {
  describe("calculateDocumentStats", () => {
    it("handles empty or nil inputs gracefully", () => {
      const stats = calculateDocumentStats("");
      expect(stats.wordCount).toBe(0);
      expect(stats.charCount).toBe(0);
      expect(stats.lineCount).toBe(0);
      expect(stats.readingTimeMinutes).toBe(0);
      expect(stats.headingCount).toBe(0);
      expect(stats.linkCount).toBe(0);
    });

    it("calculates words, lines, headings and reading time accurately", () => {
      const sample = `# Title
This is a sample markdown document with some text.

## Section 1
Here is a link: [GitPulse](https://github.com) and a [[Wikilink]].

\`\`\`rust
fn hello() {}
\`\`\`
`;
      const stats = calculateDocumentStats(sample);
      expect(stats.lineCount).toBe(10);
      expect(stats.headingCount).toBe(2);
      expect(stats.linkCount).toBe(2);
      expect(stats.wordCount).toBeGreaterThan(10);
      expect(stats.readingTimeMinutes).toBe(1);
    });
  });

  describe("extractDocumentOutline", () => {
    it("extracts headings while ignoring code blocks", () => {
      const text = `# Main Header
Some intro text.
\`\`\`markdown
# Not A Real Header
\`\`\`
## Sub Header
### Deep Header
`;
      const outline = extractDocumentOutline(text);
      expect(outline).toHaveLength(3);
      expect(outline[0]).toEqual({ level: 1, title: "Main Header", id: "main-header" });
      expect(outline[1]).toEqual({ level: 2, title: "Sub Header", id: "sub-header" });
      expect(outline[2]).toEqual({ level: 3, title: "Deep Header", id: "deep-header" });
    });
  });

  describe("parseFrontmatter", () => {
    it("extracts frontmatter fields and leaves body intact", () => {
      const doc = `---
title: MarkDev Notes
author: Bharath
tags: developer, tools
---
# Real Content
Here is the real body.`;
      const { frontmatter, content } = parseFrontmatter(doc);
      expect(frontmatter).toEqual([
        { key: "title", value: "MarkDev Notes" },
        { key: "author", value: "Bharath" },
        { key: "tags", value: "developer, tools" },
      ]);
      expect(content.trim()).toBe("# Real Content\nHere is the real body.");
    });

    it("returns empty fields when no frontmatter exists", () => {
      const doc = "# Just Markdown\nWithout frontmatter";
      const { frontmatter, content } = parseFrontmatter(doc);
      expect(frontmatter).toEqual([]);
      expect(content).toBe(doc);
    });
  });

  describe("renderMarkDevMarkdown", () => {
    it("renders headings with anchors", () => {
      const html = renderMarkDevMarkdown("# Hello World\n## Next Level");
      expect(html).toContain('<h1 id="hello-world"');
      expect(html).toContain("Hello World");
      expect(html).toContain('<h2 id="next-level"');
      expect(html).toContain("Next Level");
    });

    it("renders callout alert blocks for note, tip, warning, etc.", () => {
      const noteSample = `> [!NOTE]
> This is an important note with details.`;
      const noteHtml = renderMarkDevMarkdown(noteSample);
      expect(noteHtml).toContain("Note");
      expect(noteHtml).toContain("border-sky-500/50");

      const warnSample = `> [!WARNING]
> This is a warning.`;
      const warnHtml = renderMarkDevMarkdown(warnSample);
      expect(warnHtml).toContain("Warning");
      expect(warnHtml).toContain("border-amber-500/50");
    });

    it("renders GFM tables with column alignment and styling", () => {
      const table = `| Feature | Status | Speed |
| :--- | :---: | ---: |
| Rust Core | Done | Fast |
| UI | In Progress | 60fps |`;
      const html = renderMarkDevMarkdown(table);
      expect(html).toContain("<table");
      expect(html).toContain("<th class=\"px-3.5 py-2 text-left");
      expect(html).toContain("<th class=\"px-3.5 py-2 text-center");
      expect(html).toContain("<th class=\"px-3.5 py-2 text-right");
      expect(html).toContain("Rust Core");
      expect(html).toContain("60fps");
    });

    it("renders fenced code blocks with syntax tokens and copy button", () => {
      const code = "```rust\nfn main() {\n    println!(\"hello\");\n}\n```";
      const html = renderMarkDevMarkdown(code);
      expect(html).toContain("RUST");
      expect(html).toContain("copy-code-btn");
      expect(html).toContain("println!");
    });

    it("renders mermaid diagram fences with visualizer card", () => {
      const diagram = "```mermaid\ngraph TD;\nA-->B;\n```";
      const html = renderMarkDevMarkdown(diagram);
      expect(html).toContain("MERMAID DIAGRAM");
      expect(html).toContain("A--&gt;B;");
    });

    it("renders display math and inline math chips", () => {
      const math = "$E = mc^2$\n\n$$\\int_0^1 x dx = \\frac{1}{2}$$";
      const html = renderMarkDevMarkdown(math);
      expect(html).toContain("E = mc^2");
      expect(html).toContain("FORMULA");
      expect(html).toContain("\\int_0^1 x dx = \\frac{1}{2}");
    });

    it("renders task lists with interactive checkbox styling", () => {
      const tasks = "- [x] Finished item\n- [ ] Pending item";
      const html = renderMarkDevMarkdown(tasks);
      expect(html).toContain("Finished item");
      expect(html).toContain("Pending item");
      expect(html).toContain("line-through");
      expect(html).toContain("✓");
    });

    it("renders wikilinks and highlights", () => {
      const sample = "Link to [[Architecture]] and [[Docs|Documentation]] plus ==key highlight== and ~~removed~~";
      const html = renderMarkDevMarkdown(sample);
      expect(html).toContain("[[");
      expect(html).toContain("Architecture");
      expect(html).toContain("Documentation");
      expect(html).toContain("<mark class=\"bg-amber-400/25");
      expect(html).toContain("<del class=\"line-through");
    });

    it("escapes malicious HTML tags safely", () => {
      const malicious = '<script>alert("xss")</script><img onerror="attack()" />';
      const html = renderMarkDevMarkdown(malicious);
      expect(html).not.toContain("<script>");
      expect(html).toContain("&lt;script&gt;");
    });
  });
});

describe("markDevParser — link and image URL schemes", () => {
  const tagOf = (markdown: string) =>
    renderMarkDevMarkdown(markdown).match(/<(?:a|img)[^>]*>/)?.[0] ?? null;

  it("refuses schemes that can execute", () => {
    // Repository markdown is untrusted: a README can carry any target. The CSP
    // should also stop these, but it is the second line and is enforced by
    // three different webviews across the supported platforms.
    for (const hostile of [
      "[c](javascript:alert(1))",
      "[c](JaVaScRiPt:alert(1))",
      "[c](vbscript:msgbox(1))",
      "[c](data:text/html,x)",
      "![i](javascript:alert(1))",
    ]) {
      expect(tagOf(hostile), hostile).toBeNull();
    }
  });

  it("keeps the author's text when a link is refused", () => {
    // Dropping the content would hide what was written; linking it would be
    // the thing being prevented.
    expect(renderMarkDevMarkdown("[click me](javascript:alert(1))")).toContain("click me");
  });

  it("keeps the schemes a repository actually uses", () => {
    for (const [markdown, expected] of [
      ["[c](https://example.com)", "https://example.com"],
      ["[c](http://example.com)", "http://example.com"],
      ["[c](mailto:a@b.c)", "mailto:a@b.c"],
      ["[c](./relative.md)", "./relative.md"],
      ["[c](../up.md)", "../up.md"],
      ["[c](#anchor)", "#anchor"],
      ["[c](/absolute/path)", "/absolute/path"],
    ] as const) {
      expect(tagOf(markdown), markdown).toContain(expected);
    }
    expect(tagOf("![i](./pic.png)")).toContain("./pic.png");
  });

  it("is not fooled by control characters inside the scheme", () => {
    // Some parsers strip these before resolving, so "java\tscript:" can become
    // an executable scheme after the check has already passed it.
    for (const hostile of [
      "[c](java\u0009script:alert(1))",
      "[c](java\u000ascript:alert(1))",
      "[c](\u0000javascript:alert(1))",
    ]) {
      const tag = tagOf(hostile);
      expect(tag === null || !/javascript:/i.test(tag.replace(/[\u0000-\u001f]/g, "")), hostile).toBe(
        true,
      );
    }
  });
});

describe("markDevParser — bounded rendering", () => {
  const block = "## Heading\n\nProse with a [link](https://example.com) and `code`.\n\n";
  const docOf = (bytes: number) => block.repeat(Math.ceil(bytes / block.length));

  it("renders a normal document whole, with no notice", () => {
    const out = renderMarkDevMarkdown(docOf(MAX_RENDER_BYTES / 2));
    expect(out).not.toContain("not shown");
    expect(out).toContain("Heading");
  });

  it("caps a document that would take too long, and says so", () => {
    // Rendering is quadratic in length, so an uncapped megabyte-scale file
    // freezes the view. A silently truncated document reads exactly like a
    // complete one that ends abruptly, so the cap has to announce itself.
    const out = renderMarkDevMarkdown(docOf(MAX_RENDER_BYTES * 4));
    expect(out).toContain("not shown");
    expect(out).toMatch(/Rendered the first \d+ KB/);
    // The reader is told how much is missing, not merely that something is.
    expect(out).toMatch(/[\d,]+ more characters/);
  });

  it("stays fast no matter how large the input is", () => {
    for (const size of [MAX_RENDER_BYTES * 2, MAX_RENDER_BYTES * 16, MAX_RENDER_BYTES * 32]) {
      const started = Date.now();
      renderMarkDevMarkdown(docOf(size));
      expect(Date.now() - started, `${size} bytes took too long`).toBeLessThan(2000);
    }
  });

  it("does not choke on a long run of unmatched brackets", () => {
    // The shape that makes the link patterns scan from every '[' to the end of
    // the string: O(n) work at O(n) positions.
    const started = Date.now();
    const out = renderMarkDevMarkdown("[".repeat(MAX_RENDER_BYTES * 4));
    expect(Date.now() - started).toBeLessThan(2000);
    expect(out).toContain("not shown");
  });
});
