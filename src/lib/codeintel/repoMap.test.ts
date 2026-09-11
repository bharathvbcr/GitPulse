import { describe, expect, it } from "vitest";
import {
  coverageGapSummary,
  formatCap,
  preferredDeadLists,
  roleSample,
  unwiredExclusionSummary,
} from "./repoMap";
import type { RepoMapDocument, RepoMapSubsystem } from "./types";

function emptyMeta() {
  return {
    dead_symbol: { shown: 0, total: 0, truncated: false, count: 0 },
    entry_roots: { shown: 0, total: 0, truncated: false },
    subsystems: {
      shown: 0,
      total: 0,
      truncated: false,
      dropped_no_area: 0,
      neighbors_shown: 0,
      neighbors_total: 0,
      neighbors_truncated: false,
      handoff_paths_shown: 0,
      handoff_paths_total: 0,
      handoff_paths_truncated: false,
      role_files_shown: 0,
      role_files_total: 0,
      role_files_truncated: false,
      neighbors_endpoints_unresolved: 0,
    },
    important_files: { shown: 0, total: 0, truncated: false },
    unwired: {
      shown: 0,
      total: 0,
      truncated: false,
      excluded_coverage_loss: 0,
      excluded_import_blind: 0,
    },
    unavailable: {},
  };
}

function baseMap(overrides: Partial<RepoMapDocument> = {}): RepoMapDocument {
  return {
    generated_head: "",
    indexed_hash: "",
    content_fingerprint: "",
    graph_degraded: false,
    graph_degraded_reason: "",
    liveness_unreachable_unreliable: false,
    entry_roots: [],
    subsystems: [],
    unwired_candidates: [],
    dead_symbol_candidates: [],
    unreachable_files: [],
    liveness_meta: emptyMeta(),
    languages: [],
    important_files: [],
    package_managers: [],
    test_commands: [],
    ...overrides,
  };
}

describe("repoMap honesty helpers", () => {
  it("reads explicit totals from the current devmap coverage envelopes", () => {
    expect(
      coverageGapSummary({
        coverage_gaps: {
          call_blind: { paths: [], shown: 0, total: 0, truncated: false },
          discovery_refused: { paths: [], shown: 0, total: 0, truncated: false },
          import_blind: {
            paths: [{ path: "scripts/webkit-regressions.swift", reason: "no extractor" }],
            shown: 1,
            total: 1,
            truncated: false,
          },
          not_parsed: { paths: [], shown: 0, total: 0, truncated: false },
          parse_failed: { paths: [], shown: 0, total: 0, truncated: false },
          pattern_recovered: { paths: [], shown: 0, total: 0, truncated: false },
        },
      }),
    ).toBe("import_blind: 1");
  });

  it("uses the complete total for a truncated current envelope", () => {
    expect(
      coverageGapSummary({
        coverage_gaps: {
          discovery_refused: { paths: [{ path: "shown.rs" }], shown: 1, total: 7, truncated: true },
        },
      }),
    ).toBe("discovery_refused: 7");
  });

  it("keeps supported legacy count, array, and length shapes", () => {
    expect(
      coverageGapSummary({
        coverage_gaps: {
          numeric: 3,
          paths: ["a.rs", "b.rs"],
          counted: { length: 4 },
          zero: 0,
        },
      }),
    ).toBe("numeric: 3 · paths: 2 · counted: 4");
  });

  it("renders malformed categories as unavailable without turning keys into counts", () => {
    expect(
      coverageGapSummary({
        coverage_gaps: {
          malformed_total: { paths: ["a.rs"], shown: 1, total: "one", truncated: false },
          negative_total: { paths: [], shown: 0, total: -1, truncated: false },
          fractional_total: { paths: [], shown: 0, total: 1.5, truncated: false },
          nan_total: { paths: [], shown: 0, total: Number.NaN, truncated: false },
          unsafe_total: {
            paths: [],
            shown: 0,
            total: Number.MAX_SAFE_INTEGER + 1,
            truncated: false,
          },
          negative_legacy: -2,
          fractional_legacy: 2.5,
          malformed_length: { length: "four" },
          arbitrary_object: { reason: "producer drift", code: 7 },
        },
      }),
    ).toBe(
      "malformed_total: unavailable · negative_total: unavailable · " +
        "fractional_total: unavailable · nan_total: unavailable · " +
        "unsafe_total: unavailable · negative_legacy: unavailable · " +
        "fractional_legacy: unavailable · malformed_length: unavailable · " +
        "arbitrary_object: unavailable",
    );
  });

  it("keeps verified zero distinct from malformed-only input", () => {
    expect(
      coverageGapSummary({
        coverage_gaps: {
          zero_envelope: { paths: [], shown: 0, total: 0, truncated: false },
          zero_legacy: 0,
          zero_array: [],
          zero_length: { length: 0 },
        },
      }),
    ).toBeNull();
    expect(
      coverageGapSummary({ coverage_gaps: { malformed: { total: null } } }),
    ).toBe("malformed: unavailable");
  });

  it("formats caps without implying a truncated sample is complete", () => {
    expect(formatCap(2, 7, true)).toBe("2 of 7");
    expect(formatCap(2, 2, false)).toBe("2");
    expect(formatCap(1, 5, false)).toBe("1 of 5");
  });

  it("pairs role_files samples with role_file_counts totals", () => {
    const sub: RepoMapSubsystem = {
      area: "src/lib",
      summary: "",
      entry_points: [],
      critical_files: [],
      neighbors: [],
      handoff_paths: [],
      role_files: { tests: ["a.test.ts"] },
      role_file_counts: { tests: 5 },
    };
    const sample = roleSample(sub, "tests");
    expect(sample.paths).toEqual(["a.test.ts"]);
    expect(sample.total).toBe(5);
    expect(sample.truncated).toBe(true);
  });

  it("ignores unreachable_files when liveness_unreachable_unreliable is set", () => {
    const lists = preferredDeadLists(
      baseMap({
        liveness_unreachable_unreliable: true,
        unwired_candidates: ["a.rs"],
        dead_symbol_candidates: ["a.rs::f"],
        unreachable_files: ["vendor/dead.rs"],
      }),
    );
    expect(lists.unwired).toEqual(["a.rs"]);
    expect(lists.deadSymbols).toEqual(["a.rs::f"]);
    expect(lists.unreachable).toEqual([]);
    expect(lists.unreachableSuppressed).toBe(true);
  });

  it("surfaces unreachable only when the producer says it is reliable", () => {
    const lists = preferredDeadLists(
      baseMap({
        liveness_unreachable_unreliable: false,
        unreachable_files: ["vendor/dead.rs"],
      }),
    );
    expect(lists.unreachable).toEqual(["vendor/dead.rs"]);
    expect(lists.unreachableSuppressed).toBe(false);
  });

  it("dedupes repeated dead-symbol and unwired ids so Map each-keys stay unique", () => {
    const lists = preferredDeadLists(
      baseMap({
        unwired_candidates: ["a.rs", "b.rs", "a.rs"],
        dead_symbol_candidates: ["a.rs::f", "b.rs::g", "a.rs::f", "a.rs::f"],
        unreachable_files: ["x.rs", "x.rs"],
        liveness_unreachable_unreliable: false,
      }),
    );
    expect(lists.unwired).toEqual(["a.rs", "b.rs"]);
    expect(lists.deadSymbols).toEqual(["a.rs::f", "b.rs::g"]);
    expect(lists.unreachable).toEqual(["x.rs"]);
  });
});

describe("unwiredExclusionSummary", () => {
  it("omits a counter the producer never wrote, and one that is zero", () => {
    // A map from a kernel older than the file-liveness rule carries neither
    // `excluded_not_code` nor `excluded_exempt`. Rendering `not code 0` there
    // would claim the producer looked and found none, which is the exact
    // "a check that could not run reports what a passing check reports"
    // failure the honesty helpers in this module exist to prevent.
    expect(
      unwiredExclusionSummary({
        shown: 0,
        total: 0,
        truncated: false,
        excluded_coverage_loss: 0,
        excluded_import_blind: 0,
      }),
    ).toEqual([]);

    expect(
      unwiredExclusionSummary({
        shown: 1,
        total: 1,
        truncated: false,
        excluded_coverage_loss: 0,
        excluded_import_blind: 3,
        excluded_not_code: 0,
        excluded_exempt: 0,
      }),
    ).toEqual(["import-blind 3"]);
  });

  it("renders the directory-unit count inside the exempt phrase, never beside it", () => {
    // It is a *subset* of `excluded_exempt`. Two numbers side by side in one
    // list read as two populations, and a reader adding them up double-counts
    // every Terraform file in the repository.
    const phrases = unwiredExclusionSummary({
      shown: 2,
      total: 2,
      truncated: false,
      excluded_coverage_loss: 1,
      excluded_import_blind: 2,
      excluded_not_code: 40,
      excluded_exempt: 12,
      excluded_directory_unit: 4,
    });
    expect(phrases).toEqual([
      "coverage loss 1",
      "import-blind 2",
      "not code 40",
      "exempt 12 (4 whose unit is a directory)",
    ]);
    expect(phrases.some((phrase) => phrase === "directory unit 4")).toBe(false);
  });

  it("refuses a malformed count rather than rendering it", () => {
    const phrases = unwiredExclusionSummary({
      shown: 0,
      total: 0,
      truncated: false,
      excluded_coverage_loss: 0,
      excluded_import_blind: 0,
      excluded_not_code: Number.NaN as unknown as number,
      excluded_exempt: -1 as unknown as number,
    });
    expect(phrases).toEqual([]);
  });
});
