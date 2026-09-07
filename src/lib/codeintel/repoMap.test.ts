import { describe, expect, it } from "vitest";
import { formatCap, preferredDeadLists, roleSample } from "./repoMap";
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
});
