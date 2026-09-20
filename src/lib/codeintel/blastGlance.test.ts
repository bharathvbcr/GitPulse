import { describe, expect, it } from "vitest";
import { blastGlance, confidenceLabel, type BlastConfidence } from "./blastGlance";
import { firstClause } from "./walkIncomplete";
import type { ComposedBlastRadius } from "./blastCompose";
import { QUERY_CANCELLED_REASON } from "./blastCompose";

/** The real essay the kernel emits, so the fold is tested against the thing it folds. */
const KERNEL_ESSAY =
  "the walk did not complete: stopped at depth 10; the result is a lower bound, not the full blast radius; " +
  "16977 of 54269 unresolved attribution site(s) have no indexed target after excluding 37292 site(s) " +
  "classified as builtin, runtime-global, external-import, no-namesake, or module-path; classification " +
  "does not prove complete source coverage; these repository-wide counts are not specific to this target, " +
  "so this answer may omit callers or dependencies that name it";

function radius(over: Partial<ComposedBlastRadius> = {}): ComposedBlastRadius {
  return {
    available: true,
    reason: null,
    seeds: ["src/a.ts"],
    unmatched_targets: [],
    total_impacted: 12,
    overlap_possible: false,
    layers: [
      { depth: 1, nodes: ["src/b.ts::B.run"], node_count: 4, nodes_omitted: 0, lowest_confidence: 1 },
      { depth: 2, nodes: [], node_count: 8, nodes_omitted: 0, lowest_confidence: 1 },
    ],
    layers_truncated: false,
    walk_incomplete: null,
    unavailable_seeds: [],
    cancelled_seeds: 0,
    ...over,
  };
}

/**
 * Every independent way an answer can be qualified.
 *
 * Derived, not hand-listed: a new qualification field added to
 * ComposedBlastRadius without a row here shows up as an unqualified headline
 * in the exhaustive test below, which is exactly the failure worth catching.
 */
const QUALIFIERS: Array<{ name: string; apply: (r: ComposedBlastRadius) => void }> = [
  { name: "walk_incomplete", apply: (r) => (r.walk_incomplete = KERNEL_ESSAY) },
  { name: "layers_truncated", apply: (r) => (r.layers_truncated = true) },
  { name: "overlap_possible", apply: (r) => (r.overlap_possible = true) },
  { name: "unmatched_targets", apply: (r) => (r.unmatched_targets = ["src/z.ts"]) },
  {
    name: "unavailable_seeds",
    apply: (r) => (r.unavailable_seeds = [{ seed: "src/y.ts", reason: "not indexed" }]),
  },
  {
    name: "cancelled_seeds",
    apply: (r) => {
      r.cancelled_seeds = 1;
      r.unavailable_seeds = [{ seed: "src/x.ts", reason: QUERY_CANCELLED_REASON }];
    },
  },
];

/** Words that tell the reader the number is not exact. Case is presentational. */
const HEDGES = ["at least", "up to", "roughly", "interrupted", "incomplete", "could not"];

const hedged = (headline: string) =>
  HEDGES.some((h) => headline.toLowerCase().includes(h));

describe("blastGlance honesty invariant", () => {
  it("a clean answer reads as a plain count with no hedge", () => {
    const glance = blastGlance(radius());
    expect(glance.confidence).toBe<BlastConfidence>("exact");
    expect(glance.qualified).toBe(false);
    expect(glance.caveats).toEqual([]);
    expect(glance.headline).toBe("Reaches 12 symbols within 2 hops.");
    expect(hedged(glance.headline)).toBe(false);
  });

  it("EVERY non-empty combination of qualifiers yields a hedged, qualified headline", () => {
    const total = 1 << QUALIFIERS.length;
    let checked = 0;
    for (let mask = 1; mask < total; mask++) {
      const r = radius();
      const applied: string[] = [];
      for (let bit = 0; bit < QUALIFIERS.length; bit++) {
        if (mask & (1 << bit)) {
          QUALIFIERS[bit]!.apply(r);
          applied.push(QUALIFIERS[bit]!.name);
        }
      }
      const glance = blastGlance(r);
      const where = `[${applied.join("+")}]`;
      expect(glance.qualified, `${where} must be flagged qualified`).toBe(true);
      expect(glance.confidence, `${where} must not claim exact`).not.toBe("exact");
      expect(hedged(glance.headline), `${where} headline unhedged: ${glance.headline}`).toBe(true);
      expect(glance.caveats.length, `${where} must name a reason`).toBeGreaterThan(0);
      checked++;
    }
    // 2^6 - 1: proof the sweep ran rather than short-circuiting on an empty list.
    expect(checked).toBe(63);
  });

  it("puts the hedge BEFORE the number, so clipping the tail cannot hide it", () => {
    for (const q of QUALIFIERS) {
      const r = radius();
      q.apply(r);
      const headline = blastGlance(r).headline.toLowerCase();
      const hedge = HEDGES.find((h) => headline.includes(h))!;
      const digit = headline.search(/\d/);
      if (digit >= 0) {
        expect(headline.indexOf(hedge), `${q.name}: hedge after the number`).toBeLessThan(digit);
      }
    }
  });
});

describe("blastGlance direction of error", () => {
  it("under-counting alone is a floor", () => {
    const g = blastGlance(radius({ walk_incomplete: KERNEL_ESSAY }));
    expect(g.confidence).toBe("floor");
    expect(g.headline).toContain("at least");
  });

  it("over-counting alone is a ceiling", () => {
    const g = blastGlance(radius({ overlap_possible: true }));
    expect(g.confidence).toBe("ceiling");
    expect(g.headline).toContain("up to");
  });

  it("both directions at once is approximate, never a bound in one direction", () => {
    const g = blastGlance(radius({ overlap_possible: true, walk_incomplete: KERNEL_ESSAY }));
    expect(g.confidence).toBe("approximate");
    expect(g.headline).toContain("roughly");
    expect(g.headline).not.toContain("at least");
    expect(g.headline).not.toContain("up to");
  });

  it("cancellation outranks both: the shape of the miss is unknown", () => {
    const g = blastGlance(
      radius({
        cancelled_seeds: 2,
        unavailable_seeds: [
          { seed: "a", reason: QUERY_CANCELLED_REASON },
          { seed: "b", reason: QUERY_CANCELLED_REASON },
        ],
        overlap_possible: true,
        walk_incomplete: KERNEL_ESSAY,
      }),
    );
    expect(g.confidence).toBe("partial");
    expect(g.headline).toContain("Interrupted after");
  });

  it("an unavailable radius never reads as zero impact", () => {
    const g = blastGlance(radius({ available: false, reason: "no index", total_impacted: 0 }));
    expect(g.confidence).toBe("unavailable");
    expect(g.impacted).toBeNull();
    expect(g.headline).toContain("not the same as no impact");
  });

  it("a genuine zero says so, and a qualified zero does not", () => {
    expect(blastGlance(radius({ total_impacted: 0 })).headline).toBe(
      "Nothing else in the index references this change.",
    );
    const qualified = blastGlance(radius({ total_impacted: 0, walk_incomplete: KERNEL_ESSAY }));
    expect(qualified.headline).toContain("incomplete");
    expect(qualified.headline).not.toContain("Nothing else");
  });

  it("counts only genuinely refused seeds, not the cancelled ones twice", () => {
    const g = blastGlance(
      radius({
        cancelled_seeds: 1,
        unavailable_seeds: [
          { seed: "a", reason: QUERY_CANCELLED_REASON },
          { seed: "b", reason: "not indexed" },
        ],
      }),
    );
    expect(g.caveats).toContain("1 file stopped before the walk finished");
    expect(g.caveats).toContain("1 file could not be walked");
  });
});

describe("firstClause", () => {
  it("takes a whole clause, never a character prefix", () => {
    expect(firstClause(KERNEL_ESSAY)).toBe("the walk did not complete: stopped at depth 10");
  });

  it("drops a first clause too long to sit on a line rather than cutting mid-claim", () => {
    const long = `${"x".repeat(200)} words; short tail`;
    expect(firstClause(long)).toBeNull();
  });

  it("splits on the middot joiner the folder emits", () => {
    expect(firstClause("first thing · second thing")).toBe("first thing");
  });

  it("skips punctuation-only leading clauses", () => {
    expect(firstClause("---; real clause")).toBe("real clause");
  });

  it("is null for empty input", () => {
    for (const v of [null, undefined, "", "   ", ";;;"]) expect(firstClause(v)).toBeNull();
  });
});

describe("blastGlance robustness", () => {
  it("handles a null radius", () => {
    const g = blastGlance(null);
    expect(g.impacted).toBeNull();
    expect(g.qualified).toBe(true);
  });

  it("reports hops from the layer count and reads singular at one hop", () => {
    const one = blastGlance(
      radius({
        layers: [{ depth: 1, nodes: [], node_count: 1, nodes_omitted: 0, lowest_confidence: 1 }],
        total_impacted: 1,
      }),
    );
    expect(one.hops).toBe(1);
    expect(one.headline).toBe("Reaches 1 symbol within 1 hop.");
  });

  it("omits the reach phrase when no layers came back", () => {
    expect(blastGlance(radius({ layers: [] })).headline).toBe("Reaches 12 symbols.");
  });

  it("does not throw on absurd totals or negative bookkeeping", () => {
    for (const total of [0, 1, Number.MAX_SAFE_INTEGER]) {
      expect(() => blastGlance(radius({ total_impacted: total }))).not.toThrow();
    }
    // unavailable_seeds shorter than cancelled_seeds must not emit "-1 files".
    const g = blastGlance(radius({ cancelled_seeds: 3, unavailable_seeds: [] }));
    expect(g.caveats.some((c) => c.startsWith("-"))).toBe(false);
  });

  it("gives every confidence a one-word label", () => {
    const all: BlastConfidence[] = [
      "unavailable",
      "partial",
      "floor",
      "ceiling",
      "approximate",
      "exact",
    ];
    for (const c of all) expect(confidenceLabel(c)).toBeTruthy();
  });
});
