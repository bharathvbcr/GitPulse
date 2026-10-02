import { describe, expect, it, vi, beforeEach } from "vitest";
import { STRESS_TIMEOUT_MS, expectWithinBudget, fastestOf } from "../__tests__/perfBudget";
import {
  MAX_RENDER_BYTES,
  EMPTY_RENDER,
  calculateDocumentStats,
  renderMarkDevMarkdown,
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
});

describe("markdevRender IPC", () => {
  it("routes render through cmd_markdown_render, with no location by default", async () => {
    const answer = { html: "<h1 id=\"hello\">Hello</h1>", headings: [], frontmatter: [], omittedBytes: 0 };
    invoke.mockResolvedValueOnce(answer);
    const rendered = await renderMarkDevMarkdown("# Hello");
    // Both location fields go across, as null: Rust refuses one without the
    // other, so they must never be sent half-filled.
    expect(invoke).toHaveBeenCalledWith("cmd_markdown_render", { text: "# Hello", repoPath: null, filePath: null });
    expect(rendered).toEqual(answer);
  });

  it("sends a note's repository and path together", async () => {
    invoke.mockResolvedValueOnce(EMPTY_RENDER);
    await renderMarkDevMarkdown("x", { repoPath: "/r", filePath: "docs/a.md" });
    expect(invoke).toHaveBeenCalledWith("cmd_markdown_render", { text: "x", repoPath: "/r", filePath: "docs/a.md" });
  });

  it("returns the empty render for empty markdown without IPC", async () => {
    const rendered = await renderMarkDevMarkdown("");
    expect(rendered).toEqual({ html: "", headings: [], frontmatter: [], omittedBytes: 0 });
    expect(invoke).not.toHaveBeenCalled();
  });
});

describe("markdevRender — document stats stay linear", () => {
  it("does not slow quadratically on unmatched brackets", () => {
    // Fastest of three per size. A single sample made this fail under a
    // concurrent build: `expectWithinBudget` calibrates immediately AFTER the
    // measured work, so work that ran during a busy stretch and calibration
    // that ran during an idle one produced a budget nothing could meet. The
    // minimum is the sample least disturbed by other load, and a genuine
    // regression still slows it.
    const timings = [20000, 40000, 80000].map((n) => {
      const text = "[".repeat(n);
      return fastestOf(3, () => void calculateDocumentStats(text));
    });
    expectWithinBudget(timings[2]!, 400, "stats on unmatched brackets");
    // 4x the input must not cost ~16x. Measured on this tree the ratio is a
    // flat 3.9-4.0, so 8 sits midway between linear and quadratic: tight enough
    // to catch the regression, loose enough never to depend on the weather.
    // No `Math.max` floor is needed — `fastestOf` reports sub-millisecond time,
    // where the old `Date.now()` truncated ~13.6ms work toward a whole number
    // and a genuinely fast case to 0.
    expect(timings[2]! / timings[0]!).toBeLessThan(8);
  }, STRESS_TIMEOUT_MS);
});

describe("markdevRender — render cap constant", () => {
  it("keeps the shared ceiling with Rust", () => {
    expect(MAX_RENDER_BYTES).toBe(128 * 1024);
  });
});
