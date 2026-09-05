import { describe, expect, it } from "vitest";
import { expectWithinBudget, STRESS_TIMEOUT_MS } from "../__tests__/perfBudget";
import { annotateRange, parseUnifiedDiff, type AnnotatedDiffLine } from "./wordDiff";
import {
  buildSplitRows,
  isChangeTone,
  lineForSplitRow,
  lineTones,
  nextChangeRow,
  splitRowForLine,
  splitTones,
  type SplitCodeRow,
} from "./rowModel";
import { buildOutline, hunkAt, sectionAt } from "./outline";
import { buildRailRows, disambiguatePaths } from "./railRows";
import { buildTicks, maxScroll, scrollForRatio, scrollForRow, viewportBand } from "./minimap";
import { composeSpans } from "./highlight";
import { findMatches, hasUnboundedNesting, matchLabel } from "../text/lineSearch";
import { detectLanguageFromPath } from "../files/syntaxHighlight";
import type { RailEntry } from "./fileRail";

/** Deterministic PRNG so a failure is reproducible from its seed alone. */
function rng(seed: number): () => number {
  let state = seed >>> 0 || 1;
  return () => {
    state ^= state << 13;
    state >>>= 0;
    state ^= state >> 17;
    state ^= state << 5;
    state >>>= 0;
    return state / 0x1_0000_0000;
  };
}

const NASTY_LINES = [
  "",
  " ",
  "\t\tdeep\tindent",
  "const s = \"unterminated",
  "/* unclosed",
  "emoji 🚀 é 中文 \u0000",
  "-- looks like a deletion",
  "++ looks like an addition",
  "@@ not really a hunk",
  "diff --git a/fake b/fake",
  "\\ No newline at end of file",
  "x".repeat(5_000),
];

/** A random but well-formed multi-file diff. */
function randomDiff(seed: number, files: number, hunksPerFile: number, linesPerHunk: number): string {
  const rand = rng(seed);
  const out: string[] = [];
  for (let f = 0; f < files; f += 1) {
    const path = `src/pkg${f % 7}/mod${f}.ts`;
    out.push(`diff --git a/${path} b/${path}`);
    out.push(`index ${f.toString(16)}..${(f + 1).toString(16)} 100644`);
    out.push(`--- a/${path}`);
    out.push(`+++ b/${path}`);
    let oldNo = 1;
    let newNo = 1;
    for (let h = 0; h < hunksPerFile; h += 1) {
      out.push(`@@ -${oldNo},${linesPerHunk} +${newNo},${linesPerHunk} @@ fn f${h}()`);
      for (let l = 0; l < linesPerHunk; l += 1) {
        const roll = rand();
        const body = rand() < 0.06 ? NASTY_LINES[Math.floor(rand() * NASTY_LINES.length)] : `code ${f}.${h}.${l}`;
        if (roll < 0.35) {
          out.push(`-${body}`);
          oldNo += 1;
        } else if (roll < 0.7) {
          out.push(`+${body}`);
          newNo += 1;
        } else {
          out.push(` ${body}`);
          oldNo += 1;
          newNo += 1;
        }
      }
      oldNo += 3;
      newNo += 3;
    }
  }
  return out.join("\n") + "\n";
}

function countByType(lines: readonly AnnotatedDiffLine[]) {
  let add = 0;
  let del = 0;
  let ctx = 0;
  for (const line of lines) {
    if (line.type === "add") add += 1;
    else if (line.type === "del") del += 1;
    else if (line.type === "ctx") ctx += 1;
  }
  return { add, del, ctx };
}

describe("split rows never lose or duplicate a line", () => {
  it.each([1, 2, 3, 4, 5, 6, 7, 8])("holds for seed %i", (seed) => {
    const lines = parseUnifiedDiff(randomDiff(seed, 6, 4, 24));
    const model = buildSplitRows(lines);
    const seen = new Int32Array(lines.length).fill(0);
    for (const row of model.rows) {
      if (row.kind === "span") {
        seen[row.index] += 1;
        continue;
      }
      if (row.leftIndex >= 0) seen[row.leftIndex] += 1;
      // A context line occupies both columns as the same object; counting it
      // twice would make this assertion meaningless rather than stricter.
      if (row.rightIndex >= 0 && row.rightIndex !== row.leftIndex) seen[row.rightIndex] += 1;
    }
    for (let i = 0; i < lines.length; i += 1) {
      expect(seen[i], `line ${i} (${lines[i].type}) appeared ${seen[i]} times`).toBe(1);
    }
  });

  it("keeps every row's indices pointing at the line it renders", () => {
    const lines = parseUnifiedDiff(randomDiff(11, 4, 3, 30));
    const model = buildSplitRows(lines);
    for (const row of model.rows) {
      if (row.kind === "span") {
        expect(lines[row.index]).toBe(row.line);
        continue;
      }
      if (row.leftIndex >= 0) expect(lines[row.leftIndex]).toBe(row.left);
      if (row.rightIndex >= 0) expect(lines[row.rightIndex]).toBe(row.right);
      if (row.leftIndex < 0) expect(row.left).toBeNull();
      if (row.rightIndex < 0) expect(row.right).toBeNull();
    }
  });

  it("never puts an addition on the old side or a deletion on the new one", () => {
    for (const seed of [21, 22, 23]) {
      const lines = parseUnifiedDiff(randomDiff(seed, 5, 3, 40));
      for (const row of buildSplitRows(lines).rows) {
        if (row.kind !== "code") continue;
        expect(row.left?.type).not.toBe("add");
        expect(row.right?.type).not.toBe("del");
      }
    }
  });

  it("is at most as tall as the unified list and at least half of it", () => {
    for (const seed of [31, 32, 33]) {
      const lines = parseUnifiedDiff(randomDiff(seed, 5, 4, 30));
      const rows = buildSplitRows(lines).rows.length;
      expect(rows).toBeLessThanOrEqual(lines.length);
      expect(rows * 2).toBeGreaterThanOrEqual(countByType(lines).add + countByType(lines).del);
    }
  });

  it("maps every line to the row that shows it, and back again", () => {
    const lines = parseUnifiedDiff(randomDiff(41, 4, 4, 25));
    const model = buildSplitRows(lines);
    for (let i = 0; i < lines.length; i += 1) {
      const row = splitRowForLine(model, i);
      expect(row).toBeGreaterThanOrEqual(0);
      expect(row).toBeLessThan(model.rows.length);
      // The row really does render this line.
      const entry = model.rows[row];
      const shown =
        entry.kind === "span"
          ? [entry.index]
          : [entry.leftIndex, entry.rightIndex].filter((n) => n >= 0);
      expect(shown, `line ${i}`).toContain(i);
      // Round trip: the row's anchor line maps back to the same row.
      expect(splitRowForLine(model, lineForSplitRow(model, row))).toBe(row);
    }
  });

  it("is monotonic within each kind of line, which is what anchors a scroll", () => {
    // Across kinds it cannot be: a block draws del[k] and add[k] on ONE row,
    // so the last deletion sits below the first addition in the line list and
    // above it in the row list. Within a kind the order is the file's order,
    // and that is what "keep my place" is measured against.
    const lines = parseUnifiedDiff(randomDiff(43, 4, 4, 25));
    const model = buildSplitRows(lines);
    const last = new Map<string, number>();
    for (let i = 0; i < lines.length; i += 1) {
      const kind = lines[i].type;
      const row = splitRowForLine(model, i);
      expect(row, `${kind} at ${i}`).toBeGreaterThanOrEqual(last.get(kind) ?? -1);
      last.set(kind, row);
    }
  });
});

describe("both views agree on what a change replaced", () => {
  /**
   * The bug this pins: the two views ran different pairings over the same
   * mutable line objects and each skipped a line that already carried
   * segments, so the intra-line highlight depended on which view had been
   * opened first.
   */
  it.each([51, 52, 53, 54])("holds for seed %i whichever view annotates first", (seed) => {
    const raw = randomDiff(seed, 3, 3, 30);

    const unifiedFirst = parseUnifiedDiff(raw);
    annotateRange(unifiedFirst, 0, unifiedFirst.length);
    const unifiedPairs = buildSplitRows(unifiedFirst)
      .rows.filter((r): r is SplitCodeRow => r.kind === "code")
      .filter((r) => r.left?.segments && r.right?.segments)
      .map((r) => [r.left?.content, r.right?.content]);

    // Annotating through the split rows must produce the same pairs.
    const splitFirst = parseUnifiedDiff(raw);
    const model = buildSplitRows(splitFirst);
    for (const row of model.rows) {
      if (row.kind !== "code") continue;
      if (row.leftIndex >= 0) annotateRange(splitFirst, row.leftIndex, row.leftIndex + 1);
    }
    annotateRange(splitFirst, 0, splitFirst.length);
    const splitPairs = model.rows
      .filter((r): r is SplitCodeRow => r.kind === "code")
      .filter((r) => r.left?.segments && r.right?.segments)
      .map((r) => [r.left?.content, r.right?.content]);

    expect(splitPairs).toEqual(unifiedPairs);
  });

  it("puts every annotated line on a row whose partner is annotated too", () => {
    const lines = parseUnifiedDiff(randomDiff(61, 4, 4, 40));
    annotateRange(lines, 0, lines.length);
    const model = buildSplitRows(lines);
    const rowOf = new Map<AnnotatedDiffLine, SplitCodeRow>();
    for (const row of model.rows) {
      if (row.kind !== "code") continue;
      if (row.left) rowOf.set(row.left, row);
      if (row.right) rowOf.set(row.right, row);
    }
    for (const line of lines) {
      if (!line.segments || line.type === "ctx") continue;
      const row = rowOf.get(line);
      expect(row, `${line.content} has no row`).toBeDefined();
      const partner = row?.left === line ? row?.right : row?.left;
      expect(partner?.segments, `${line.content} partner unannotated`).toBeDefined();
    }
  });
});

describe("the outline partitions the diff it describes", () => {
  it.each([71, 72, 73])("holds for seed %i", (seed) => {
    const lines = parseUnifiedDiff(randomDiff(seed, 9, 3, 20));
    const outline = buildOutline(lines);
    expect(outline.files.length).toBeGreaterThan(0);
    // Sections tile the line list with no gap and no overlap.
    expect(outline.files[0].index).toBe(0);
    for (let i = 1; i < outline.files.length; i += 1) {
      expect(outline.files[i].index).toBe(outline.files[i - 1].end);
    }
    expect(outline.files[outline.files.length - 1].end).toBe(lines.length);
    // Totals are the sum of the parts, and the parts are the real rows.
    const counted = countByType(lines);
    expect(outline.additions).toBe(counted.add);
    expect(outline.deletions).toBe(counted.del);
    expect(outline.files.reduce((n, f) => n + f.additions, 0)).toBe(counted.add);
    expect(outline.files.reduce((n, f) => n + f.deletions, 0)).toBe(counted.del);
  });

  it("answers sectionAt and hunkAt consistently for every line", () => {
    const lines = parseUnifiedDiff(randomDiff(81, 6, 3, 15));
    const outline = buildOutline(lines);
    for (let i = 0; i < lines.length; i += 1) {
      const section = sectionAt(outline, i);
      expect(section).not.toBeNull();
      expect(section!.index).toBeLessThanOrEqual(i);
      expect(section!.end).toBeGreaterThan(i);
      const hunk = hunkAt(section, i);
      if (hunk) {
        expect(hunk.index).toBeLessThanOrEqual(i);
        expect(section!.hunks).toContain(hunk);
      }
    }
  });

  it("survives a diff of pure garbage without inventing files", () => {
    for (const junk of [
      "",
      "\n\n\n",
      "not a diff at all",
      "@@@@@@",
      "diff --git",
      "diff --git \n",
      "+++\n---\n",
      "\u0000\u0000",
      NASTY_LINES.join("\n"),
    ]) {
      const outline = buildOutline(parseUnifiedDiff(junk));
      expect(Number.isFinite(outline.additions)).toBe(true);
      expect(Number.isFinite(outline.deletions)).toBe(true);
      for (const file of outline.files) {
        expect(file.end).toBeGreaterThanOrEqual(file.index);
      }
    }
  });
});

describe("tones and change stepping", () => {
  it("walks every block exactly once in both directions", () => {
    const lines = parseUnifiedDiff(randomDiff(91, 4, 4, 30));
    const tones = lineTones(lines);
    const forward: number[] = [];
    for (let cursor = nextChangeRow(tones, -1, 1); cursor !== null; cursor = nextChangeRow(tones, cursor, 1)) {
      forward.push(cursor);
      expect(isChangeTone(tones[cursor])).toBe(true);
      expect(forward.length).toBeLessThan(tones.length + 1);
    }
    const backward: number[] = [];
    for (
      let cursor = nextChangeRow(tones, tones.length, -1);
      cursor !== null;
      cursor = nextChangeRow(tones, cursor, -1)
    ) {
      backward.push(cursor);
      expect(backward.length).toBeLessThan(tones.length + 1);
    }
    expect(backward.reverse()).toEqual(forward);
    expect(forward.length).toBeGreaterThan(0);
  });

  it("keeps split tones the same length as the split rows", () => {
    for (const seed of [101, 102]) {
      const model = buildSplitRows(parseUnifiedDiff(randomDiff(seed, 3, 3, 25)));
      expect(splitTones(model)).toHaveLength(model.rows.length);
    }
  });
});

describe("the minimap stays inside the list it maps", () => {
  it("never scrolls outside the content, for any ratio and any list", () => {
    const rand = rng(7);
    for (let i = 0; i < 400; i += 1) {
      const rows = Math.floor(rand() * 400_000);
      const height = 1 + Math.floor(rand() * 40);
      const viewport = Math.floor(rand() * 2_000);
      const ratio = rand() * 1.4 - 0.2;
      const scroll = scrollForRatio(ratio, rows, height, viewport);
      expect(scroll).toBeGreaterThanOrEqual(0);
      expect(scroll).toBeLessThanOrEqual(maxScroll(rows, height, viewport));
      const row = scrollForRow(Math.floor(rand() * rows), rows, height, viewport);
      expect(row).toBeGreaterThanOrEqual(0);
      expect(row).toBeLessThanOrEqual(maxScroll(rows, height, viewport));
    }
  });

  it("is monotonic in the ratio, so dragging never jumps backwards", () => {
    let previous = -1;
    for (let i = 0; i <= 100; i += 1) {
      const scroll = scrollForRatio(i / 100, 12_345, 20, 640);
      expect(scroll).toBeGreaterThanOrEqual(previous);
      previous = scroll;
    }
  });

  it("keeps the viewport band inside the strip", () => {
    const rand = rng(9);
    for (let i = 0; i < 300; i += 1) {
      const rows = Math.floor(rand() * 100_000);
      const height = 1 + Math.floor(rand() * 30);
      const viewport = Math.floor(rand() * 1_500);
      const band = viewportBand(rand() * rows * height * 1.3, viewport, rows, height);
      if (!band) continue;
      expect(band.topPct).toBeGreaterThanOrEqual(0);
      expect(band.topPct).toBeLessThanOrEqual(100);
      expect(band.heightPct).toBeGreaterThan(0);
      expect(band.topPct + band.heightPct).toBeLessThanOrEqual(101);
    }
  });

  it("keeps ticks inside the strip and within budget for any list", () => {
    const rand = rng(13);
    for (let i = 0; i < 60; i += 1) {
      const length = Math.floor(rand() * 5_000);
      const tones = new Uint8Array(length);
      for (let j = 0; j < length; j += 1) tones[j] = Math.floor(rand() * 7);
      const ticks = buildTicks(tones, 80);
      expect(ticks.length).toBeLessThanOrEqual(80);
      for (const tick of ticks) {
        expect(tick.topPct).toBeGreaterThanOrEqual(0);
        expect(tick.topPct).toBeLessThan(100);
        expect(tick.heightPct).toBeGreaterThan(0);
      }
    }
  });
});

describe("the rail survives a repository that fights it", () => {
  const entry = (path: string, isStaged = false): RailEntry => ({
    path,
    statusCode: "M",
    additions: 1,
    deletions: 1,
    isStaged,
  });

  it("labels five thousand colliding names distinguishably", () => {
    const paths = Array.from({ length: 5_000 }, (_, i) => `a/b/c/d${i % 500}/e${i % 50}/index.ts`);
    const map = disambiguatePaths(paths);
    const unique = new Set(paths);
    const labels = new Map<string, number>();
    for (const path of unique) {
      const dir = map.get(path);
      expect(dir, path).toBeDefined();
      const label = `${dir}/index.ts`;
      labels.set(label, (labels.get(label) ?? 0) + 1);
    }
    // Every distinct path gets a distinct label: no two rows read the same.
    for (const [label, count] of labels) {
      expect(count, `${label} claimed by ${count} paths`).toBe(1);
    }
  });

  it("renders hostile paths without dropping a row or repeating a key", () => {
    // A repeated key is not a cosmetic problem: Svelte throws on a duplicate
    // key in a keyed each-block, so one malformed status list would take the
    // whole pane down rather than draw one row fewer.
    const hostile = [
      "a.ts",
      "a.ts",
      "../escape.ts",
      "/abs.ts",
      "C:/win.ts",
      "with space/name.ts",
      "unicode/é/中文.ts",
      "trailing/slash/",
      "..",
      ".",
      "",
      "deep/" + "x/".repeat(60) + "leaf.ts",
    ];
    for (const mode of ["list", "tree"] as const) {
      const result = buildRailRows({
        entries: hostile.map((p) => entry(p)),
        mode,
        query: "",
      });
      const files = result.rows.filter((r) => r.kind === "file");
      expect(files.length, mode).toBe(hostile.length);
      expect(result.matched, mode).toBe(hostile.length);
      expect(new Set(result.rows.map((r) => r.key)).size, `${mode} keys`).toBe(result.rows.length);
      for (const row of files) {
        expect(typeof (row as { name: string }).name).toBe("string");
      }
    }
  });

  it("keeps the staged and unstaged sides of every path apart", () => {
    const entries = Array.from({ length: 200 }, (_, i) => [
      entry(`src/f${i % 20}.ts`, true),
      entry(`src/f${i % 20}.ts`, false),
    ]).flat();
    for (const mode of ["list", "tree"] as const) {
      const rows = buildRailRows({ entries, mode, query: "" }).rows;
      const files = rows.filter((r) => r.kind === "file");
      expect(files.length, mode).toBe(entries.length);
    }
  });

  it("never claims more matches than it has entries", () => {
    const entries = Array.from({ length: 300 }, (_, i) => entry(`p/q${i}/file${i}.ts`));
    for (const query of ["", "file", "q1", "zzz", "  ", "P/Q", "../", "\\", "["]) {
      for (const mode of ["list", "tree"] as const) {
        const result = buildRailRows({ entries, mode, query });
        expect(result.matched).toBeLessThanOrEqual(result.total);
        expect(result.rows.filter((r) => r.kind === "file")).toHaveLength(result.matched);
      }
    }
  });
});

describe("search cannot hang or over-report", () => {
  it("terminates on every pattern that can match nothing", () => {
    const lines = Array.from({ length: 500 }, (_, i) => `line ${i} with words`);
    for (const pattern of ["a*", "\\b", "(?:)", "^", "$", "x?", "(|)", "\\B", "[^]*", "(?=l)"]) {
      const started = Date.now();
      const result = findMatches(lines, pattern, { regex: true });
      expect(Date.now() - started, pattern).toBeLessThan(2_000);
      expect(result.matches.length, pattern).toBeLessThanOrEqual(5_000);
    }
  });

  it("refuses an exponential-backtracking pattern instead of running it", () => {
    // Measured before the guard existed: `(a+)+c` against twenty-eight
    // characters took 111 SECONDS in this suite. A JavaScript regex is not
    // interruptible, so no timeout can shorten it once `exec` starts — the
    // only defence is not starting.
    for (const pattern of ["(a+)+c", "([a-z]+\\s*)+$", "(\\d{2,})*x", "((ab)*)*c"]) {
      const started = Date.now();
      const result = findMatches(["a".repeat(64) + "b"], pattern, { regex: true });
      expect(Date.now() - started, pattern).toBeLessThan(1_000);
      expect(result.invalid, pattern).toBe(true);
      expect(result.reason, pattern).toBe("unbounded");
      expect(matchLabel(result, 0)).toBe("pattern may not terminate");
    }
  });

  it("still runs the quantified patterns that are actually safe", () => {
    for (const pattern of ["(foo|bar)+", "(\\d+)", "(?:abc)+", "a*b", "[a-z]+", "x{2,4}"]) {
      const result = findMatches(["foobar abc 123 xxxx aaab"], pattern, { regex: true });
      expect(result.invalid, pattern).toBe(false);
    }
  });

  it("stops at a wall-clock deadline instead of freezing the window", () => {
    // Through the accessor, so two million lines cost no memory: the claim is
    // that the loop is bounded by time, not that the machine can hold them.
    const lines = {
      length: 2_000_000,
      at: (i: number) => `line ${i} of text with several words in it`,
    };
    const started = Date.now();
    const result = findMatches(lines, "\\bwords?\\b", {
      regex: true,
      maxMatches: 10_000_000,
      maxMillis: 5,
    });
    const elapsed = Date.now() - started;
    expect(result.truncated).toBe(true);
    expect(result.matches.length).toBeLessThan(2_000_000);
    // Generous, because the deadline is only checked every 256 lines and the
    // claim is boundedness rather than precision.
    expect(elapsed).toBeLessThan(5_000);
  }, STRESS_TIMEOUT_MS);

  it("reports a cap as a floor rather than as a total", () => {
    const lines = Array.from({ length: 10_000 }, () => "aaaaaaaaaa");
    const result = findMatches(lines, "a", { maxMatches: 100 });
    expect(result.matches).toHaveLength(100);
    expect(result.truncated).toBe(true);
  });
});

describe("span composition reproduces the line, always", () => {
  it.each([201, 202, 203])("holds for seed %i over a random diff", (seed) => {
    const lines = parseUnifiedDiff(randomDiff(seed, 3, 3, 40));
    annotateRange(lines, 0, lines.length);
    const language = detectLanguageFromPath("x.ts");
    for (const line of lines) {
      const text =
        line.type === "add" || line.type === "del"
          ? line.content.slice(1)
          : line.type === "ctx" && line.content.startsWith(" ")
            ? line.content.slice(1)
            : line.content;
      const spans = composeSpans(
        text,
        language,
        line.segments,
        line.type === "del" ? "Removed" : "Added",
        [{ start: 2, end: 7 }],
      );
      expect(spans.map((s) => s.text).join("")).toBe(text);
    }
  });

  it("reproduces the line for every supported language over hostile text", () => {
    const languages = [
      "typescript",
      "rust",
      "python",
      "go",
      "json",
      "yaml",
      "markdown",
      "css",
      "html",
      "shell",
      "sql",
      "toml",
      "svelte",
      "xml",
      "c",
      "cpp",
      "javascript",
      "diff",
      "plaintext",
    ] as const;
    for (const language of languages) {
      for (const text of NASTY_LINES) {
        const spans = composeSpans(text, language, undefined, "Added", [{ start: 0, end: 3 }]);
        expect(spans.map((s) => s.text).join(""), `${language}: ${JSON.stringify(text)}`).toBe(text);
      }
    }
  });

  it("ignores segments that do not reconstruct the line rather than mis-painting it", () => {
    const spans = composeSpans(
      "actual text",
      "typescript",
      [{ kind: "Added", text: "something else entirely" }],
      "Added",
    );
    expect(spans.map((s) => s.text).join("")).toBe("actual text");
    expect(spans.some((s) => s.changed)).toBe(false);
  });
});

describe("performance bounds", () => {
  /**
   * Loose ceilings, and deliberately so.
   *
   * Two stricter designs were tried and both cry wolf. An absolute budget in
   * reference units swung 14x across three consecutive runs of identical code
   * (16.8, 243.8, 90.6 for the same pipeline): the reference loop is
   * integer-only and allocation-free by design, so it reads CPU availability
   * and cannot see the two things that actually move these numbers — garbage
   * collection over hundreds of megabytes, and whether the tokenizer has been
   * JIT-compiled yet. A ratio between two sizes was then tried to cancel
   * both, and reported 15x-to-40x for a 4x step, consistently, across five
   * runs — because at these sizes allocation dominates and it is not linear
   * in the work even when the algorithm is.
   *
   * So these assert what a wall-clock check in this environment honestly can:
   * that the work COMPLETES, within a ceiling wide enough that only a hang or
   * a several-fold regression trips it. The linearity claims they cannot make
   * are carried instead by the invariant cases above, which are deterministic
   * — every row accounted for, every cap respected, every pattern terminating.
   */
  it(
    "parses, aligns and outlines a 200k-line diff",
    () => {
      const raw = randomDiff(301, 200, 20, 50);
      const started = performance.now();
      const lines = parseUnifiedDiff(raw);
      const model = buildSplitRows(lines);
      const tones = lineTones(lines);
      const outline = buildOutline(lines);
      const ticks = buildTicks(splitTones(model));
      const elapsed = performance.now() - started;
      expect(lines.length).toBeGreaterThan(200_000);
      expect(outline.files).toHaveLength(200);
      expect(tones).toHaveLength(lines.length);
      expect(model.rows.length).toBeLessThanOrEqual(lines.length);
      expect(ticks.length).toBeGreaterThan(0);
      expectWithinBudget(elapsed, 600, "diff pipeline: 200k lines");
    },
    STRESS_TIMEOUT_MS,
  );

  it(
    "composes spans for twenty thousand rows",
    () => {
      const lines = parseUnifiedDiff(randomDiff(311, 4, 8, 60));
      annotateRange(lines, 0, lines.length);
      const language = detectLanguageFromPath("x.ts");
      // Warm the tokenizer: the first thousand calls are measuring the JIT.
      for (let i = 0; i < 2_000; i += 1) {
        composeSpans(lines[i % lines.length].content.slice(1), language, undefined, "Added");
      }
      const started = performance.now();
      for (let i = 0; i < 20_000; i += 1) {
        const line = lines[(i * 7) % lines.length];
        composeSpans(line.content.slice(1), language, line.segments, "Added");
      }
      expectWithinBudget(performance.now() - started, 300, "composeSpans: 20k rows");
    },
    STRESS_TIMEOUT_MS,
  );

  it(
    "labels and groups an eight-thousand-file rail",
    () => {
      const entries: RailEntry[] = Array.from({ length: 8_000 }, (_, i) => ({
        path: `src/area${i % 40}/pkg${i % 200}/mod${i}.rs`,
        statusCode: "M",
        additions: i % 17,
        deletions: i % 5,
        isStaged: i % 3 === 0,
      }));
      const started = performance.now();
      const list = buildRailRows({ entries, mode: "list", query: "" });
      const tree = buildRailRows({ entries, mode: "tree", query: "" });
      const filtered = buildRailRows({ entries, mode: "tree", query: "pkg1" });
      const elapsed = performance.now() - started;
      expect(list.rows).toHaveLength(8_000);
      expect(tree.rows.filter((r) => r.kind === "file")).toHaveLength(8_000);
      expect(filtered.matched).toBeLessThan(8_000);
      // Keys stay unique at scale, which is what the keyed each-block needs.
      expect(new Set(list.rows.map((r) => r.key)).size).toBe(8_000);
      expectWithinBudget(elapsed, 600, "rail rows: 8k files");
    },
    STRESS_TIMEOUT_MS,
  );

  it(
    "searches four hundred thousand lines",
    () => {
      const lines = {
        length: 400_000,
        at: (i: number) => `line ${i} of code with several words in it`,
      };
      const started = performance.now();
      const result = findMatches(lines, "words", {
        maxMatches: 10_000_000,
        maxMillis: 60_000,
      });
      expect(result.matches).toHaveLength(400_000);
      expect(result.truncated).toBe(false);
      expectWithinBudget(performance.now() - started, 400, "findMatches: 400k lines");
    },
    STRESS_TIMEOUT_MS,
  );
});

describe("the unbounded-nesting guard flags the right shapes", () => {
  it("flags a quantifier over a group that quantifies", () => {
    for (const pattern of [
      "(a+)+",
      "(a*)*",
      "(a+)*",
      "(a*)+",
      "([a-z]+\\s*)+",
      "(\\d{2,})*",
      "x((ab+)*)y*",
      "(?:(a+)+)",
    ]) {
      expect(hasUnboundedNesting(pattern), pattern).toBe(true);
    }
  });

  it("leaves alone everything a user would reasonably type", () => {
    for (const pattern of [
      "",
      "foo",
      "(foo|bar)+",
      "(\\d+)",
      "(?:abc)+",
      "a*b*c*",
      "[a-z]+",
      "x{2,4}",
      "^\\s*//",
      "\\((\\w+)\\)",
      "([(])+",
      "(a\\+)+",
      "[+*]+",
      "(unclosed",
      "a)b",
      "(a{1,3})+",
    ]) {
      expect(hasUnboundedNesting(pattern), pattern).toBe(false);
    }
  });
});
