import { describe, expect, it } from "vitest";
import { STRESS_TIMEOUT_MS, expectWithinBudget, fastestOf } from "../__tests__/perfBudget";
import { blastGlance, confidenceLabel, hedgedCount } from "./blastGlance";
import { nodeLabel, nodeLabelText, nodeFilePath, looksLikePath } from "./nodeLabel";
import { firstClause, summarizeWalkIncomplete, tooltipWalkIncomplete } from "./walkIncomplete";
import { fileGlance, fileHonesty, markerForHonesty } from "./previewSummary";
import type { ComposedBlastRadius, ComposedBlastLayer } from "./blastCompose";
import { QUERY_CANCELLED_REASON } from "./blastCompose";
import type { DevmapPreviewFileResult } from "./types";

const KERNEL_ESSAY =
  "the walk did not complete: stopped at depth 10; the result is a lower bound, not the full " +
  "blast radius; 16977 of 54269 unresolved attribution site(s) have no indexed target after " +
  "excluding 37292 site(s) classified as builtin, runtime-global, external-import, no-namesake, " +
  "or module-path; classification does not prove complete source coverage";

function layer(depth: number, nodes: string[] = []): ComposedBlastLayer {
  return { depth, nodes, node_count: nodes.length, nodes_omitted: 0, lowest_confidence: 1 };
}

function radius(over: Partial<ComposedBlastRadius> = {}): ComposedBlastRadius {
  return {
    available: true,
    reason: null,
    seeds: [],
    unmatched_targets: [],
    total_impacted: 0,
    overlap_possible: false,
    layers: [],
    layers_truncated: false,
    walk_incomplete: null,
    unavailable_seeds: [],
    cancelled_seeds: 0,
    ...over,
  };
}

/**
 * Whitespace is the hostile shape here, not length.
 *
 * `CLAUSE_SPLIT` begins with `\s*`, so a long run of whitespace makes the
 * engine rescan the run from every position in it — quadratic, and invisible
 * to an audit that only looks for catastrophic backtracking. Measured before
 * the fix: 10k chars 45ms, 100k chars 3.9s, 200k chars 15.2s.
 *
 * The run must contain NO clause joiner. With a `·` after it the engine finds
 * a match on the first attempt and stops scanning, which reads as linear and
 * tests nothing — the first version of this test made exactly that mistake.
 */
const whitespaceBomb = (n: number) => `lead${" ".repeat(n)}tail`;

describe("engine prose folding survives hostile shapes", () => {
  it(
    "folds a whitespace-heavy qualification in linear time",
    () => {
      const bomb = whitespaceBomb(200_000);
      const ms = fastestOf(3, () => {
        summarizeWalkIncomplete([bomb]);
      });
      expectWithinBudget(ms, 40, "summarizeWalkIncomplete/whitespace-bomb");
    },
    STRESS_TIMEOUT_MS,
  );

  it(
    "takes a first clause from a whitespace-heavy string in linear time",
    () => {
      const bomb = whitespaceBomb(200_000);
      const ms = fastestOf(3, () => {
        firstClause(bomb);
      });
      expectWithinBudget(ms, 40, "firstClause/whitespace-bomb");
    },
    STRESS_TIMEOUT_MS,
  );

  it(
    "scales sub-quadratically as the whitespace run doubles",
    () => {
      // The structural claim, independent of any machine: doubling the input
      // must not quadruple the cost. A quadratic scan reads ~4x here.
      const small = fastestOf(3, () => summarizeWalkIncomplete([whitespaceBomb(60_000)]));
      const large = fastestOf(3, () => summarizeWalkIncomplete([whitespaceBomb(120_000)]));
      // Guard against a zero denominator on a fast machine.
      const ratio = large / Math.max(small, 0.05);
      expect(ratio, `doubling cost ratio ${ratio.toFixed(1)}x looks quadratic`).toBeLessThan(3);
    },
    STRESS_TIMEOUT_MS,
  );

  it(
    "folds 200 per-seed essays without dumping 200 copies",
    () => {
      const parts = Array.from(
        { length: 200 },
        (_, i) => `${KERNEL_ESSAY}; ${i} traversed edges unrecorded`,
      );
      let folded = "";
      const ms = fastestOf(3, () => {
        folded = summarizeWalkIncomplete(parts) ?? "";
      });
      expectWithinBudget(ms, 60, "summarizeWalkIncomplete/200-seeds");
      expect(folded.length).toBeLessThanOrEqual(720);
      // One copy of the corpus clause, not two hundred.
      expect(folded.split("unresolved attribution").length - 1).toBeLessThanOrEqual(1);
    },
    STRESS_TIMEOUT_MS,
  );

  it("never throws on adversarial prose", () => {
    const hostile = [
      "",
      "   ",
      ";;;;;;",
      "·".repeat(5_000),
      "\u0000\u0000 null bytes",
      "\ud800 lone surrogate",
      "🙂".repeat(5_000),
      "a".repeat(100_000),
      "\n".repeat(50_000),
      "; ".repeat(20_000),
    ];
    for (const text of hostile) {
      expect(() => summarizeWalkIncomplete([text]), text.slice(0, 20)).not.toThrow();
      expect(() => firstClause(text), text.slice(0, 20)).not.toThrow();
      expect(() => tooltipWalkIncomplete([text]), text.slice(0, 20)).not.toThrow();
      const clause = firstClause(text);
      if (clause !== null) expect(Array.from(clause).length).toBeLessThanOrEqual(96);
    }
  });
});

describe("blastGlance under load and abuse", () => {
  it(
    "summarizes a 5,000-hop radius within budget",
    () => {
      const layers = Array.from({ length: 5_000 }, (_, i) =>
        layer(i + 1, [`src/f${i}.ts::T${i}.m`]),
      );
      const big = radius({
        layers,
        total_impacted: 5_000_000,
        walk_incomplete: KERNEL_ESSAY,
        overlap_possible: true,
      });
      let out = blastGlance(big);
      const ms = fastestOf(3, () => {
        out = blastGlance(big);
      });
      expectWithinBudget(ms, 30, "blastGlance/5000-hops");
      expect(out.hops).toBe(5_000);
      expect(out.confidence).toBe("approximate");
      expect(out.headline).toContain("roughly");
    },
    STRESS_TIMEOUT_MS,
  );

  it("holds the honesty invariant across randomized radii", () => {
    // Deterministic LCG: a fuzz case that cannot be reproduced is not a test.
    let seed = 0x2f6e2b1;
    const next = () => (seed = (seed * 1103515245 + 12345) & 0x7fffffff);
    const pick = (n: number) => next() % n;

    let qualifiedSeen = 0;
    let exactSeen = 0;
    for (let i = 0; i < 2_000; i++) {
      // Six independent coin-flips almost never land all-clean, so a quarter
      // of the cases are forced clean. Without this the sweep only ever
      // exercises the qualified branch, and the exact-claim guard below —
      // the half that actually matters — never runs.
      const clean = pick(4) === 0;
      const cancelled = !clean && pick(4) === 0 ? pick(3) : 0;
      const unavailable = clean
        ? []
        : Array.from({ length: cancelled + pick(3) }, (_, k) => ({
            seed: `s${k}`,
            reason: k < cancelled ? QUERY_CANCELLED_REASON : "not indexed",
          }));
      const r = radius({
        available: clean || pick(8) !== 0,
        reason: !clean && pick(2) ? KERNEL_ESSAY : null,
        total_impacted: pick(1_000_000),
        overlap_possible: !clean && pick(2) === 0,
        layers: Array.from({ length: pick(6) }, (_, d) => layer(d + 1)),
        layers_truncated: !clean && pick(3) === 0,
        walk_incomplete: !clean && pick(2) === 0 ? KERNEL_ESSAY : null,
        unmatched_targets: clean ? [] : Array.from({ length: pick(4) }, (_, k) => `u${k}.ts`),
        unavailable_seeds: unavailable,
        cancelled_seeds: cancelled,
      });

      const g = blastGlance(r);
      expect(typeof g.headline).toBe("string");
      expect(g.headline.length).toBeGreaterThan(0);
      // The invariant: an exact claim is made ONLY when nothing qualifies it.
      if (g.confidence === "exact") {
        exactSeen++;
        expect(r.available).toBe(true);
        expect(r.walk_incomplete).toBeNull();
        expect(r.layers_truncated).toBe(false);
        expect(r.overlap_possible).toBe(false);
        expect(r.unmatched_targets).toHaveLength(0);
        expect(r.unavailable_seeds).toHaveLength(0);
        expect(g.caveats).toHaveLength(0);
      } else {
        qualifiedSeen++;
        expect(g.qualified).toBe(true);
      }
      // A caveat count can never go negative, whatever the bookkeeping says.
      for (const c of g.caveats) expect(c.startsWith("-")).toBe(false);
      expect(confidenceLabel(g.confidence)).toBeTruthy();
    }
    // Proof the sweep exercised both sides rather than one branch 2000 times.
    expect(exactSeen).toBeGreaterThan(0);
    expect(qualifiedSeen).toBeGreaterThan(0);
    expect(exactSeen + qualifiedSeen).toBe(2_000);
  });

  it("tolerates impossible bookkeeping without inventing a number", () => {
    const broken = radius({
      total_impacted: -5,
      cancelled_seeds: 99,
      unavailable_seeds: [],
      layers: [layer(1)],
    });
    expect(() => blastGlance(broken)).not.toThrow();
    const g = blastGlance(broken);
    expect(g.caveats.every((c) => !c.startsWith("-"))).toBe(true);
    expect(hedgedCount(0, true, "caller")).toBe("at least 0 callers");
  });
});

describe("nodeLabel under abuse", () => {
  it(
    "labels 50,000 node ids within budget",
    () => {
      const ids = Array.from(
        { length: 50_000 },
        (_, i) => `Sources/Deep/Nested/Path/File${i}.swift::Type${i}.member${i}`,
      );
      const ms = fastestOf(3, () => {
        for (const id of ids) nodeLabel(id);
      });
      expectWithinBudget(ms, 60, "nodeLabel/50k");
    },
    STRESS_TIMEOUT_MS,
  );

  it("never throws, and never grows the string it was given", () => {
    const hostile = [
      "",
      "::",
      ":::::",
      "::::::::x",
      "a".repeat(100_000),
      `${"/".repeat(50_000)}::x`,
      `.${"a".repeat(100_000)}!`,
      `${"a".repeat(50_000)}::${"b".repeat(50_000)}`,
      "🙂/🙂.ts::🙂.🙂",
      "\ud800::x",
      "C:\\Windows\\path.ts::Thing.run",
    ];
    for (const id of hostile) {
      expect(() => nodeLabel(id), id.slice(0, 20)).not.toThrow();
      expect(() => nodeLabelText(id), id.slice(0, 20)).not.toThrow();
      expect(() => nodeFilePath(id), id.slice(0, 20)).not.toThrow();
      expect(() => looksLikePath(id), id.slice(0, 20)).not.toThrow();
      const label = nodeLabel(id);
      expect(label.symbol.length).toBeLessThanOrEqual(id.length);
      if (label.file) expect(label.file.length).toBeLessThanOrEqual(id.length);
    }
  });

  it(
    "resists a path-shaped backtracking bomb",
    () => {
      const bomb = `.${"a".repeat(200_000)}!`;
      const ms = fastestOf(3, () => {
        looksLikePath(bomb);
        nodeLabel(`${bomb}::x`);
      });
      expectWithinBudget(ms, 30, "looksLikePath/backtrack-bomb");
    },
    STRESS_TIMEOUT_MS,
  );
});

describe("fileGlance under abuse", () => {
  const result = (over: Record<string, unknown>): DevmapPreviewFileResult =>
    ({
      file_path: "src/a.ts",
      available: true,
      reason: null,
      report: {
        file_path: "src/a.ts",
        parse_status: "Full",
        delta_available: true,
        file_is_indexed: true,
        compared_against: "HEAD",
        degraded_reason: null,
        symbols: [],
        bodies_not_compared: 0,
        ambiguous_callers: 0,
        broken_callers: {
          source_freshness: { fresh: true },
          available: true,
          reason: null,
          items: [],
          total: 0,
          shown: 0,
          truncated: false,
        },
        ...over,
      },
    }) as DevmapPreviewFileResult;

  it("never emits an unbounded phrase, whatever the engine says", () => {
    const hostile = [
      { degraded_reason: "x".repeat(100_000) },
      { degraded_reason: whitespaceBomb(50_000) },
      { parse_status: "Fallback".repeat(5_000) },
      { bodies_not_compared: Number.MAX_SAFE_INTEGER },
      { ambiguous_callers: Number.MAX_SAFE_INTEGER },
    ];
    for (const over of hostile) {
      const h = fileHonesty(result(over));
      expect(() => fileGlance(h)).not.toThrow();
      const g = fileGlance(h);
      expect(g.length).toBeGreaterThan(0);
      // A phrase that wraps the sidebar is the defect this replaced.
      expect(g.length, `runaway phrase: ${g.slice(0, 60)}`).toBeLessThanOrEqual(120);
      expect(markerForHonesty(h).title.length).toBeLessThanOrEqual(480);
    }
  });

  it(
    "glances 20,000 files within budget",
    () => {
      const files = Array.from({ length: 20_000 }, (_, i) =>
        fileHonesty(result({ file_path: `src/f${i}.ts`, bodies_not_compared: i % 3 })),
      );
      const ms = fastestOf(3, () => {
        for (const h of files) fileGlance(h);
      });
      expectWithinBudget(ms, 60, "fileGlance/20k");
    },
    STRESS_TIMEOUT_MS,
  );
});
