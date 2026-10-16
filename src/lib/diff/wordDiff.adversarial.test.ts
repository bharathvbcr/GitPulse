import { describe, expect, it } from "vitest";
import {
  annotateAround,
  annotateRange,
  parseUnifiedDiff,
  replacementBlock,
  type AnnotatedDiffLine,
} from "./wordDiff";

/** Small seeded PRNG (mulberry32) so failures reproduce. */
function rng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const WORDS = ["alpha", "beta", "gamma", "delta", "let", "x", "=", "42", "foo()", "bar"];

function randomHunk(next: () => number, runs: number): string {
  const out = ["@@ -1,100 +1,100 @@"];
  for (let r = 0; r < runs; r += 1) {
    const kind = Math.floor(next() * 3); // 0 ctx, 1 del, 2 add
    const n = 1 + Math.floor(next() * 6);
    for (let i = 0; i < n; i += 1) {
      const text = Array.from({ length: 1 + Math.floor(next() * 5) }, () =>
        WORDS[Math.floor(next() * WORDS.length)]
      ).join(" ");
      out.push((kind === 0 ? " " : kind === 1 ? "-" : "+") + text);
    }
  }
  return out.join("\n") + "\n";
}

const plain = (lines: AnnotatedDiffLine[]) => lines.map((l) => l.segments ?? null);

function order(kind: string, n: number, next: () => number): number[] {
  const idx = Array.from({ length: n }, (_, i) => i);
  if (kind === "backward") return idx.reverse();
  if (kind === "random") return idx.sort(() => next() - 0.5);
  if (kind === "partial") return idx.slice(Math.floor(n / 3), Math.floor((2 * n) / 3));
  return idx;
}

describe("annotateAround equals annotateRange over the whole block (seeded oracle)", () => {
  for (const kind of ["forward", "backward", "random", "partial"]) {
    it(`any hunk shape, ${kind} render order, with and without a cached block`, () => {
      for (let seed = 1; seed <= 150; seed += 1) {
        const next = rng(seed);
        const raw = randomHunk(next, 3 + Math.floor(next() * 8));
        const oracle = parseUnifiedDiff(raw);
        annotateRange(oracle, 0, oracle.length);
        const viewed = order(kind, oracle.length, next);
        for (const cache of [false, true]) {
          const lines = parseUnifiedDiff(raw);
          for (const i of viewed) {
            annotateAround(lines, i, cache ? replacementBlock(lines, i) : undefined, 24);
          }
          // Every row that was rendered must carry what the old whole-block
          // pass gave it.
          for (const i of viewed) {
            expect(lines[i].segments ?? null, `seed ${seed} row ${i}\n${raw}`).toEqual(
              oracle[i].segments ?? null
            );
          }
        }
      }
    }, 60_000);
  }

  it("the oracle can fail: a mutated pairing is caught", () => {
    const raw = "@@ -1,2 +1,2 @@\n-a b\n-c d\n+a x\n+c y\n";
    const oracle = parseUnifiedDiff(raw);
    annotateRange(oracle, 0, oracle.length);
    const mutated = parseUnifiedDiff(raw);
    // Wrong pairing: del[0] against add[1].
    annotateRange(mutated, 0, 0);
    expect(plain(mutated)).not.toEqual(plain(oracle));
  });

  it("rows with a pair in the old behaviour all get segments (fixed shape: add run then del run)", () => {
    const raw = "@@ -1,2 +1,2 @@\n-a b\n+a x\n-c d\n+c y\n";
    const oracle = parseUnifiedDiff(raw);
    annotateRange(oracle, 0, oracle.length);
    const lines = parseUnifiedDiff(raw);
    for (let i = 0; i < lines.length; i += 1) annotateAround(lines, i);
    expect(plain(lines)).toEqual(plain(oracle));
  });
});

describe("a del row that directly follows an add row", () => {
  const raw = "@@ -1,2 +1,2 @@\n-a b\n+a x\n-c d\n+c y\n";

  it("is annotated when it is the only row rendered", () => {
    const oracle = parseUnifiedDiff(raw);
    annotateRange(oracle, 0, oracle.length);
    const lines = parseUnifiedDiff(raw);
    annotateAround(lines, 3);
    expect(lines[3].segments).toEqual(oracle[3].segments);
    expect(lines[4].segments).toEqual(oracle[4].segments);
  });

  it("belongs to its own block, never the previous one", () => {
    const lines = parseUnifiedDiff(raw);
    for (let i = 1; i < lines.length; i += 1) {
      const b = replacementBlock(lines, i);
      expect(b && b.start <= i && i < b.end, `row ${i}`).toBe(true);
    }
    expect(replacementBlock(lines, 3)).toEqual({ start: 3, addStart: 4, end: 5 });
  });
});

describe("pairs that bail out of computeWordDiff", () => {
  it("still get segments, so a re-render never recomputes them", () => {
    const huge = (c: string) => c.repeat(60_000);
    const lines = parseUnifiedDiff(`@@ -1 +1 @@\n-${huge("a")}\n+${huge("b")}\n`);
    annotateAround(lines, 1);
    const first = lines[1].segments;
    expect(first).toBeDefined();
    for (let i = 0; i < 50; i += 1) annotateAround(lines, 1);
    expect(lines[1].segments).toBe(first);
  });

  it("an all-bail block costs only its forced pair plus cheap neighbours", () => {
    const row = (c: string, i: number) => `${c}${i} ${"q".repeat(60_000)}`;
    const raw = ["@@ -1,60 +1,60 @@"];
    for (let i = 0; i < 60; i += 1) raw.push("-" + row("a", i));
    for (let i = 0; i < 60; i += 1) raw.push("+" + row("b", i));
    const lines = parseUnifiedDiff(raw.join("\n") + "\n");
    annotateAround(lines, 30);
    expect(lines[30].segments).toBeDefined();
    // The budget charges a bail-out the full table (it is a size upper bound),
    // so the halo stops early; the row's own pair is the guarantee.
    expect(lines.filter((l) => l.segments).length).toBeLessThanOrEqual(2 * 49);
  });
});

describe("annotateAround budget edges", () => {
  it("halo 0 annotates only the requested pair", () => {
    const lines = parseUnifiedDiff("@@ -1,3 +1,3 @@\n-a\n-b\n-c\n+d\n+e\n+f\n");
    annotateAround(lines, 2, undefined, 0);
    expect(lines.filter((l) => l.segments).length).toBe(2);
  });

  it("only dels, only adds and a negative index do nothing", () => {
    const d = parseUnifiedDiff("@@ -1,2 +1 @@\n-a\n-b\n");
    const a = parseUnifiedDiff("@@ -1 +1,2 @@\n+a\n+b\n");
    for (const l of [d, a]) {
      for (const i of [-5, 0, 1, 2, 9]) annotateAround(l, i);
      expect(l.some((x) => x.segments)).toBe(false);
    }
  });
});
