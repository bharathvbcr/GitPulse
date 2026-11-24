import { describe, expect, it, vi, beforeEach } from "vitest";
import { STRESS_TIMEOUT_MS, expectWithinBudget, fastestCpuOf, fastestOf } from "../__tests__/perfBudget";
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

/** How many times longer the large input is than the small one. */
const SIZE_FACTOR = 8;

/**
 * The large input's CPU time over the same number of characters in small
 * calls (`SIZE_FACTOR` of them). Linear reads 1: measured 1.00-1.13, with and
 * without coverage. A quadratic term worth +50% at the small size reads
 * (8 + 32) / 12 = 3.3: measured 3.1-3.5. 2 sits well clear of both.
 */
const EQUAL_WORK_RATIO_LIMIT = 2;

describe("markdevRender — document stats stay linear", () => {
  it("does not slow quadratically on unmatched brackets", () => {
    // Fastest of three per size. A single sample made this fail under a
    // concurrent build: `expectWithinBudget` calibrates immediately AFTER the
    // measured work, so work that ran during a busy stretch and calibration
    // that ran during an idle one produced a budget nothing could meet. The
    // minimum is the sample least disturbed by other load, and a genuine
    // regression still slows it.
    const small = "[".repeat(80000);
    const large = "[".repeat(80000 * SIZE_FACTOR);
    expectWithinBudget(
      fastestOf(3, () => void calculateDocumentStats(small)),
      400,
      "stats on unmatched brackets",
    );
    // 8x the input must cost about what 8 calls on the small input cost. Both
    // sizes are large because a quadratic term only shows where it dominates,
    // and the small input is timed as a batch of `SIZE_FACTOR` calls so the two
    // samples are the same amount of work. That is what the CPU clock needs:
    // Windows advances process.cpuUsage in 15.6 ms ticks, and a single 60 ms
    // small call could read a tick either side, which pushed the old 4x
    // single-call ratio (linear ~4, limit 5.5) to 5.63 on Windows CI. Each
    // sample is now ~500 ms, about 30 ticks. The 8x size also widens the gap
    // between linear (1) and the quadratic term this guards against (3.3);
    // at 4x it was 4 against 8.
    // The ratio is taken over CPU time, not wall time: at host load 70 a
    // wall ratio of this linear pass read 2x its true value in 4 of 20 runs,
    // because the two sizes waited for the CPU for different lengths of time.
    // CPU time covers only this thread's work, so work moved off-thread would
    // not be seen; this is a synchronous pure function. Rounds alternate the
    // sizes so a drift in clock speed lands on both, and the minimum of each is
    // what the ratio is taken of.
    let cpuSmallBatch = Number.POSITIVE_INFINITY;
    let cpuLarge = Number.POSITIVE_INFINITY;
    for (let round = 0; round < 5; round += 1) {
      cpuSmallBatch = Math.min(
        cpuSmallBatch,
        fastestCpuOf(1, () => {
          for (let call = 0; call < SIZE_FACTOR; call += 1) void calculateDocumentStats(small);
        }),
      );
      cpuLarge = Math.min(cpuLarge, fastestCpuOf(1, () => void calculateDocumentStats(large)));
    }
    expect(cpuLarge / cpuSmallBatch).toBeLessThan(EQUAL_WORK_RATIO_LIMIT);
  }, STRESS_TIMEOUT_MS);
});

describe("markdevRender — render cap constant", () => {
  it("keeps the shared ceiling with Rust", () => {
    expect(MAX_RENDER_BYTES).toBe(128 * 1024);
  });
});
