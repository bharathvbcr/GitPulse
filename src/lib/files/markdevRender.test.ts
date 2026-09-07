import { describe, expect, it, vi, beforeEach } from "vitest";
import { STRESS_TIMEOUT_MS, expectWithinBudget } from "../__tests__/perfBudget";
import {
  MAX_RENDER_BYTES,
  calculateDocumentStats,
  extractDocumentOutline,
  parseFrontmatter,
  renderMarkDevMarkdown,
  parseMarkdown,
} from "./markdevRender";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
}));

beforeEach(() => {
  invoke.mockReset();
});

describe("markdevRender helpers", () => {
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
});

describe("markdevRender IPC", () => {
  it("routes render through cmd_markdown_render", async () => {
    invoke.mockResolvedValueOnce("<h1>Hello</h1>");
    const html = await renderMarkDevMarkdown("# Hello");
    expect(invoke).toHaveBeenCalledWith("cmd_markdown_render", { text: "# Hello" });
    expect(html).toContain("Hello");
  });

  it("routes parse through cmd_markdown_parse", async () => {
    invoke.mockResolvedValueOnce({
      source: "x",
      spans: [],
      markers: [],
      blocks: [],
      truncated: false,
    });
    await parseMarkdown("x");
    expect(invoke).toHaveBeenCalledWith("cmd_markdown_parse", { text: "x" });
  });

  it("returns empty string for empty markdown without IPC", async () => {
    const html = await renderMarkDevMarkdown("");
    expect(html).toBe("");
    expect(invoke).not.toHaveBeenCalled();
  });
});

describe("markdevRender — document stats stay linear", () => {
  it("does not slow quadratically on unmatched brackets", () => {
    const timings = [20000, 40000, 80000].map((n) => {
      const started = Date.now();
      calculateDocumentStats("[".repeat(n));
      return Date.now() - started;
    });
    expectWithinBudget(timings[2]!, 400, "stats on unmatched brackets");
    // Soft ratio guard: 4× input should not cost ~16× (true quadratic).
    expect(timings[2]! / Math.max(1, timings[0]!)).toBeLessThan(13);
  }, STRESS_TIMEOUT_MS);
});

describe("markdevRender — render cap constant", () => {
  it("keeps the shared ceiling with Rust", () => {
    expect(MAX_RENDER_BYTES).toBe(128 * 1024);
  });
});
