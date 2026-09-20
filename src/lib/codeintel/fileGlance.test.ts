import { describe, expect, it } from "vitest";
import { fileGlance, fileHonesty, markerForHonesty } from "./previewSummary";
import type { DevmapPreviewFileResult, DevmapPreviewReport } from "./types";

function report(over: Partial<DevmapPreviewReport> = {}): DevmapPreviewReport {
  return {
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
  } as DevmapPreviewReport;
}

const file = (over: Partial<DevmapPreviewReport> = {}): DevmapPreviewFileResult => ({
  file_path: over.file_path ?? "src/a.ts",
  available: true,
  reason: null,
  report: report(over),
});

const glanceOf = (over: Partial<DevmapPreviewReport> = {}) => fileGlance(fileHonesty(file(over)));

describe("fileGlance says what was found, not what the struct holds", () => {
  it("a clean, reliable preview reads as one short phrase", () => {
    expect(glanceOf()).toBe("no callers break");
  });

  it("counts broken callers and hedges a truncated count", () => {
    const broken = (total: number, truncated = false) =>
      glanceOf({
        broken_callers: {
          source_freshness: { fresh: true },
          available: true,
          reason: null,
          items: [],
          total,
          shown: truncated ? 5 : total,
          truncated,
        },
      } as Partial<DevmapPreviewReport>);
    expect(broken(1)).toBe("1 caller would break");
    expect(broken(9)).toBe("9 callers would break");
    expect(broken(9, true)).toBe("at least 9 callers would break");
  });

  it("names an unindexed file rather than reporting zero breakage", () => {
    expect(glanceOf({ file_is_indexed: false })).toBe("not in the index — callers unknown");
  });

  it("names an unparseable file", () => {
    expect(glanceOf({ parse_status: "Fallback" })).toBe("could not be parsed (fallback)");
  });

  it("names a missing delta", () => {
    expect(glanceOf({ delta_available: false })).toBe("no before/after to compare");
  });

  it("prefers an explicit degraded reason, folded to one clause", () => {
    expect(
      glanceOf({ degraded_reason: "index older than the file; rebuild to compare bodies" }),
    ).toBe("index older than the file");
  });

  it("an unavailable preview is never silent about being unavailable", () => {
    const g = fileGlance(
      fileHonesty({
        file_path: "package-lock.json",
        available: false,
        reason: "path is not indexable source",
        report: null,
      }),
    );
    expect(g).toContain("not previewed");
    expect(g).toContain("path is not indexable source");
  });

  it("an unfinished search cannot claim clean", () => {
    const g = glanceOf({
      broken_callers: {
        source_freshness: { fresh: true },
        available: true,
        reason: null,
        items: [],
        total: 0,
        shown: 0,
        truncated: false,
        walk_incomplete: "the walk did not complete: stopped at depth 10",
      },
    } as Partial<DevmapPreviewReport>);
    expect(g).toBe("search did not finish — cannot say it is clean");
  });

  it("surfaces uncompared bodies and ambiguous callers instead of a bare zero", () => {
    expect(glanceOf({ bodies_not_compared: 3 })).toBe("3 bodies not compared");
    expect(glanceOf({ bodies_not_compared: 1 })).toBe("1 body not compared");
    expect(glanceOf({ ambiguous_callers: 2 })).toBe("2 ambiguous callers");
  });
});

describe("fileGlance honesty invariant", () => {
  /**
   * Derived rather than hand-listed: every way a report can fail to support a
   * "nothing breaks" claim must produce a phrase that does not read as clean.
   */
  const DEGRADATIONS: Array<[string, Partial<DevmapPreviewReport>]> = [
    ["unindexed", { file_is_indexed: false }],
    ["fallback parse", { parse_status: "Fallback" }],
    ["unreadable parse", { parse_status: "Unreadable" }],
    ["error parse", { parse_status: "Error" }],
    ["unknown parse", { parse_status: "Unknown" }],
    ["no delta", { delta_available: false }],
    ["degraded", { degraded_reason: "stale index" }],
    [
      "walk incomplete",
      {
        broken_callers: {
          source_freshness: { fresh: true },
          available: true,
          reason: null,
          items: [],
          total: 0,
          shown: 0,
          truncated: false,
          walk_incomplete: "the walk did not complete",
        },
      } as Partial<DevmapPreviewReport>,
    ],
    [
      "broken callers unavailable",
      {
        broken_callers: {
          source_freshness: { fresh: false },
          available: false,
          reason: "no index",
          items: [],
          total: 0,
          shown: 0,
          truncated: false,
        },
      } as Partial<DevmapPreviewReport>,
    ],
  ];

  it("no degraded report ever reads as 'no callers break'", () => {
    let checked = 0;
    for (const [name, over] of DEGRADATIONS) {
      const h = fileHonesty(file(over));
      const g = fileGlance(h);
      expect(h.claim_clean, `${name} must not be claim_clean`).toBe(false);
      expect(g, `${name} read as clean: ${g}`).not.toBe("no callers break");
      expect(g.length, `${name} produced an empty glance`).toBeGreaterThan(0);
      checked++;
    }
    expect(checked).toBe(DEGRADATIONS.length);
    expect(checked).toBeGreaterThan(0);
  });

  it("agrees with the rail marker about whether a file is clean", () => {
    for (const [name, over] of [...DEGRADATIONS, ["clean", {}] as const]) {
      const h = fileHonesty(file(over as Partial<DevmapPreviewReport>));
      const marker = markerForHonesty(h);
      const isCleanPhrase = fileGlance(h) === "no callers break";
      expect(marker.kind === "clean", `${name}: marker and glance disagree`).toBe(isCleanPhrase);
    }
  });

  it("a clean parse whose caller search never finished is not marked clean", () => {
    // Regression: `unreliable` asks whether the PARSE was trustworthy and says
    // nothing about whether the walk finished, so this file used to render in
    // the diff rail as a bare "0" titled "No broken callers detected" — a
    // finished-search claim for a search that stopped at depth 10.
    const h = fileHonesty(
      file({
        broken_callers: {
          source_freshness: { fresh: true },
          available: true,
          reason: null,
          items: [],
          total: 0,
          shown: 0,
          truncated: false,
          walk_incomplete: "the walk did not complete: stopped at depth 10",
        },
      } as Partial<DevmapPreviewReport>),
    );
    expect(h.unreliable).toBe(false); // the parse really was fine
    expect(h.claim_clean).toBe(false); // but the search was not
    const marker = markerForHonesty(h);
    expect(marker.kind).not.toBe("clean");
    expect(marker.label).not.toBe("0");
    expect(marker.title).toContain('not "nothing breaks"');
  });

  it("zero broken callers with uncompared bodies is also not clean", () => {
    const h = fileHonesty(file({ bodies_not_compared: 4 }));
    expect(h.claim_clean).toBe(false);
    expect(markerForHonesty(h).kind).not.toBe("clean");
  });

  it("never returns a phrase long enough to wrap a sidebar row", () => {
    for (const [, over] of DEGRADATIONS) {
      expect(fileGlance(fileHonesty(file(over))).length).toBeLessThanOrEqual(60);
    }
  });
});
