import { describe, expect, it } from "vitest";
import {
  buildSplitRows,
  hasContentLines,
  isChangeTone,
  lineForSplitRow,
  lineTones,
  nextChangeRow,
  splitRowForLine,
  splitTones,
  TONE_ADD,
  TONE_CTX,
  TONE_DEL,
  TONE_FILE,
  TONE_HUNK,
  TONE_MOD,
  TONE_NONE,
  type SplitCodeRow,
} from "./rowModel";
import { annotateRange, parseUnifiedDiff } from "./wordDiff";

const diff = (...body: string[]) =>
  parseUnifiedDiff(
    ["diff --git a/x.ts b/x.ts", "--- a/x.ts", "+++ b/x.ts", "@@ -1,4 +1,4 @@", ...body, ""].join(
      "\n",
    ),
  );

const codeRows = (lines: ReturnType<typeof diff>) =>
  buildSplitRows(lines).rows.filter((row): row is SplitCodeRow => row.kind === "code");

describe("buildSplitRows alignment", () => {
  it("puts a replaced line beside its replacement rather than below it", () => {
    // The regression: one pending deletion meant D deletions and A additions
    // came out as D-1 solo-left rows, one paired row, then A-1 solo-right
    // rows — the two sides offset by the size of the block.
    const rows = codeRows(diff("-alpha", "-beta", "-gamma", "+ALPHA", "+BETA", "+GAMMA"));
    expect(rows.map((row) => [row.left?.content, row.right?.content])).toEqual([
      ["-alpha", "+ALPHA"],
      ["-beta", "+BETA"],
      ["-gamma", "+GAMMA"],
    ]);
  });

  it("collapses a balanced block to one row per pair, not two", () => {
    const lines = diff(...Array.from({ length: 400 }, (_, i) => `-old ${i}`), ...Array.from({ length: 400 }, (_, i) => `+new ${i}`));
    expect(codeRows(lines)).toHaveLength(400);
  });

  it("spills the longer side into rows with an empty other column", () => {
    const rows = codeRows(diff("-a", "-b", "-c", "+A"));
    expect(rows.map((row) => [row.left?.content ?? null, row.right?.content ?? null])).toEqual([
      ["-a", "+A"],
      ["-b", null],
      ["-c", null],
    ]);
  });

  it("spills additions the same way when the new side is longer", () => {
    const rows = codeRows(diff("-a", "+A", "+B", "+C"));
    expect(rows.map((row) => [row.left?.content ?? null, row.right?.content ?? null])).toEqual([
      ["-a", "+A"],
      [null, "+B"],
      [null, "+C"],
    ]);
  });

  it("shows a context line as the same object on both sides", () => {
    const rows = codeRows(diff(" same"));
    expect(rows).toHaveLength(1);
    expect(rows[0].left).toBe(rows[0].right);
  });

  it("gives chrome its own full-width rows instead of an empty right column", () => {
    const model = buildSplitRows(diff(" ctx"));
    const spans = model.rows.filter((row) => row.kind === "span");
    // diff --git, ---, +++, @@ — four rows of chrome above one code row.
    expect(spans).toHaveLength(4);
    expect(model.rows[model.rows.length - 1].kind).toBe("code");
  });

  it("handles an addition-only file with no deletions at all", () => {
    const rows = codeRows(diff("+one", "+two"));
    expect(rows.map((row) => [row.left, row.right?.content])).toEqual([
      [null, "+one"],
      [null, "+two"],
    ]);
  });

  it("handles a deletion-only file", () => {
    const rows = codeRows(diff("-one", "-two"));
    expect(rows.map((row) => [row.left?.content, row.right])).toEqual([
      ["-one", null],
      ["-two", null],
    ]);
  });

  it("returns an empty model for an empty line list", () => {
    const model = buildSplitRows([]);
    expect(model.rows).toEqual([]);
    expect(model.lineToRow).toHaveLength(0);
  });
});

describe("buildSplitRows agrees with the unified word diff", () => {
  /**
   * The two views annotated the same shared line objects with different
   * pairings and each skipped a line that already carried segments, so the
   * intra-line highlight depended on which view had been opened first.
   */
  it("pairs the same lines annotateRange pairs", () => {
    const lines = diff("-alpha one", "-beta two", "-gamma three", "+gamma threeX");
    annotateRange(lines, 0, lines.length);
    const rows = codeRows(lines);
    const annotated = rows.find((row) => row.left?.segments && row.right?.segments);
    expect(annotated?.left?.content).toBe("-alpha one");
    expect(annotated?.right?.content).toBe("+gamma threeX");
    // And no OTHER pair got segments from a second, disagreeing pass.
    expect(rows.filter((row) => row.left?.segments && row.right?.segments)).toHaveLength(1);
  });

  it("puts every annotated pair on one row, for any block shape", () => {
    for (const [dels, adds] of [
      [1, 1],
      [1, 3],
      [3, 1],
      [4, 4],
      [7, 2],
    ]) {
      const lines = diff(
        ...Array.from({ length: dels }, (_, i) => `-old ${i}`),
        ...Array.from({ length: adds }, (_, i) => `+new ${i}`),
      );
      annotateRange(lines, 0, lines.length);
      const rows = codeRows(lines);
      for (const line of lines) {
        if (!line.segments) continue;
        const row = rows.find((r) => r.left === line || r.right === line);
        expect(row, `${dels}/${adds}: annotated line has no row`).toBeDefined();
        const partner = row?.left === line ? row?.right : row?.left;
        expect(partner?.segments, `${dels}/${adds}: partner unannotated`).toBeDefined();
      }
    }
  });
});

describe("split/unified position mapping", () => {
  it("maps a line to the row that shows it, in both directions", () => {
    const lines = diff(" ctx", "-a", "-b", "+A", " tail");
    const model = buildSplitRows(lines);
    const delB = lines.findIndex((line) => line.content === "-b");
    const row = splitRowForLine(model, delB);
    expect((model.rows[row] as SplitCodeRow).left?.content).toBe("-b");
    expect(lineForSplitRow(model, row)).toBe(delB);
  });

  it("anchors a row with no left side on its right line", () => {
    const lines = diff("-a", "+A", "+B");
    const model = buildSplitRows(lines);
    const rightOnly = model.rows.findIndex(
      (row) => row.kind === "code" && row.left === null && row.right !== null,
    );
    expect(lines[lineForSplitRow(model, rightOnly)].content).toBe("+B");
  });

  it("clamps out-of-range lookups instead of throwing", () => {
    const model = buildSplitRows(diff(" ctx"));
    expect(splitRowForLine(model, -50)).toBe(0);
    expect(splitRowForLine(model, 9_999)).toBe(model.rows.length - 1);
    expect(lineForSplitRow(model, -3)).toBeGreaterThanOrEqual(0);
    expect(lineForSplitRow(model, 9_999)).toBeLessThan(model.lineToRow.length);
  });

  it("answers 0 for an empty model rather than -1", () => {
    expect(splitRowForLine({ rows: [], lineToRow: new Int32Array(0) }, 4)).toBe(0);
    expect(lineForSplitRow({ rows: [], lineToRow: new Int32Array(0) }, 4)).toBe(0);
  });
});

describe("tones", () => {
  it("classifies every line kind", () => {
    const lines = parseUnifiedDiff(
      [
        "diff --git a/x.ts b/x.ts",
        "index 1..2 100644",
        "--- a/x.ts",
        "+++ b/x.ts",
        "@@ -1,2 +1,2 @@",
        " ctx",
        "-old",
        "+new",
        "",
      ].join("\n"),
    );
    const tones = [...lineTones(lines)];
    expect(tones).toEqual([
      TONE_FILE, // diff --git
      TONE_NONE, // index
      TONE_FILE, // ---
      TONE_FILE, // +++
      TONE_HUNK, // @@
      TONE_CTX,
      TONE_DEL,
      TONE_ADD,
    ]);
  });

  it("marks a binary notice as neither an addition nor a deletion", () => {
    const lines = parseUnifiedDiff("Binary files a/logo.png and b/logo.png differ\n");
    expect([...lineTones(lines)]).toEqual([TONE_NONE]);
  });

  it("calls a replacement row a modification, not growth", () => {
    const model = buildSplitRows(diff("-old", "+new"));
    const tones = splitTones(model);
    expect(tones[tones.length - 1]).toBe(TONE_MOD);
  });

  it("keeps one-sided rows honest about which side they are", () => {
    const addOnly = splitTones(buildSplitRows(diff("+new")));
    const delOnly = splitTones(buildSplitRows(diff("-old")));
    expect(addOnly[addOnly.length - 1]).toBe(TONE_ADD);
    expect(delOnly[delOnly.length - 1]).toBe(TONE_DEL);
  });

  it("gives split chrome the same tones the unified list would", () => {
    const model = buildSplitRows(
      parseUnifiedDiff(
        ["diff --git a/x b/x", "index 1..2", "@@ -1 +1 @@", " ctx", ""].join("\n"),
      ),
    );
    expect([...splitTones(model)]).toEqual([TONE_FILE, TONE_NONE, TONE_HUNK, TONE_CTX]);
  });

  it("counts a modification as a change for stepping", () => {
    expect(isChangeTone(TONE_MOD)).toBe(true);
    expect(isChangeTone(TONE_CTX)).toBe(false);
    expect(isChangeTone(TONE_HUNK)).toBe(false);
  });
});

describe("nextChangeRow", () => {
  const tones = (pattern: string) =>
    new Uint8Array([...pattern].map((c) => (c === "+" ? TONE_ADD : c === "-" ? TONE_DEL : TONE_CTX)));

  it("lands on the first line of the next block, not the next changed line", () => {
    //          0123456789
    const t = tones("..++..-..+");
    expect(nextChangeRow(t, -1, 1)).toBe(2);
    expect(nextChangeRow(t, 2, 1)).toBe(6);
    expect(nextChangeRow(t, 3, 1)).toBe(6);
    expect(nextChangeRow(t, 6, 1)).toBe(9);
    expect(nextChangeRow(t, 9, 1)).toBeNull();
  });

  it("steps backwards to the start of the previous block", () => {
    const t = tones("..++..-..+");
    expect(nextChangeRow(t, 9, -1)).toBe(6);
    expect(nextChangeRow(t, 6, -1)).toBe(2);
    expect(nextChangeRow(t, 3, -1)).toBe(2);
    expect(nextChangeRow(t, 2, -1)).toBeNull();
  });

  it("finds a block that starts at index 0", () => {
    expect(nextChangeRow(tones("++.."), -1, 1)).toBe(0);
    expect(nextChangeRow(tones("++.."), 3, -1)).toBe(0);
  });

  it("returns null for a diff with no changes at all", () => {
    expect(nextChangeRow(tones("...."), -1, 1)).toBeNull();
    expect(nextChangeRow(tones("...."), 3, -1)).toBeNull();
  });

  it("returns null for an empty tone list", () => {
    expect(nextChangeRow(new Uint8Array(0), 0, 1)).toBeNull();
    expect(nextChangeRow(new Uint8Array(0), 0, -1)).toBeNull();
  });

  it("clamps a wild cursor instead of reading out of bounds", () => {
    const t = tones("..++..");
    expect(nextChangeRow(t, -9_999, 1)).toBe(2);
    expect(nextChangeRow(t, 9_999, -1)).toBe(2);
    expect(nextChangeRow(t, 9_999, 1)).toBeNull();
    expect(nextChangeRow(t, -9_999, -1)).toBeNull();
  });
});

describe("hasContentLines", () => {
  it("is false for a diff of nothing but chrome", () => {
    expect(hasContentLines(parseUnifiedDiff("diff --git a/x b/x\nindex 1..2\n"))).toBe(false);
  });

  it("is true as soon as one add, del or context row exists", () => {
    expect(hasContentLines(diff(" ctx"))).toBe(true);
    expect(hasContentLines(diff("+new"))).toBe(true);
    expect(hasContentLines(diff("-old"))).toBe(true);
  });

  it("is false for an empty list", () => {
    expect(hasContentLines([])).toBe(false);
  });
});
