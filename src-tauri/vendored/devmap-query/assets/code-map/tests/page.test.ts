import { describe, expect, it } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { readView } from "../src/page/graphView";
import { capText, readPreview, testsSampleText, unplacedText } from "../src/page/previewModel";
import { PAGE_CSS } from "../src/page/styles";

const sample = {
  nodes: [{ id: "a" }, { id: "b" }],
  links: [{ source: "a", target: "b" }],
  subsystems: [
    {
      id: "src/cli", area: "src/cli", summary: "src/cli: CLI", entry_points: ["src/cli/main.rs"], critical_files: [],
      neighbors: ["crates/core"], handoff_paths: [], role_files: { tests: ["t1.rs", "t2.rs"], entry: ["m.rs"] },
      languages: [["rust", 3], ["shell", 1]], file_count: 4, documentation: false,
    },
    { not: "a subsystem" },
    { id: "x", area: "" },
  ],
  unresolved_handoffs: [{ from: "src/cli", text: "x -> y" }],
  unresolved_handoffs_total: 7,
  unresolved_handoffs_by_subsystem: { "src/cli": { total: 7, listed: 1 } },
  liveness: {
    counts: { entry_roots: { shown: 1, available: 1, total: 1, truncated: false }, unwired_candidates: { shown: 0, available: 400, total: 900, truncated: true } },
    entry_roots: ["a.rs"], unwired_candidates: [], unreachable_files: [], dead_symbol_candidates: [],
    liveness_unreachable_unreliable: true,
  },
  meta: {
    language_totals: [["rust", 3], ["shell", 1]], palette: { rust: { color: "#dea584", label: "Rust", official: true } },
    neutral_color: "#6e7681", linguist_version: "v9.7.0", coverage: { indexed_total: 10, in_subsystems: 4 },
    generated_head: "abc1234", graph_html: "graph/graph.html", handoff_paths_computed: true, role_files_computed: false,
  },
};

describe("readPreview", () => {
  const model = readPreview(sample);

  it("keeps only well-formed subsystems", () => {
    expect(model.subsystems.map((s) => s.area)).toEqual(["src/cli"]);
    expect(model.subsystems[0].roleFiles.tests).toEqual(["t1.rs", "t2.rs"]);
    expect(model.subsystems[0].languages).toEqual([["rust", 3], ["shell", 1]]);
  });

  it("carries both coverage numbers and the unplaced total", () => {
    expect(model.indexedTotal).toBe(10);
    expect(model.inSubsystems).toBe(4);
    expect(model.unresolvedTotal).toBe(7);
    expect(model.unresolvedBySubsystem["src/cli"]).toEqual({ total: 7, listed: 1 });
    expect(model.roleFilesComputed).toBe(false);
  });

  it("states a capped liveness list as N of M, and an unknown total as a floor", () => {
    expect(capText(model.liveness.entryRoots)).toBe("1");
    expect(capText(model.liveness.unwiredCandidates)).toBe("0 of 900");
    expect(capText({ shown: 256, available: 400, total: null })).toBe("256 of at least 400");
    expect(capText({ shown: 3, available: 3, total: null })).toBe("3 shown, total unknown");
    expect(model.liveness.unreachableUnreliable).toBe(true);
  });

  it("treats an empty or hostile payload as an empty map, not a crash", () => {
    for (const payload of [{}, { subsystems: "no", meta: 3, liveness: [] }, { subsystems: [null, 1, "x"] }]) {
      const empty = readPreview(payload);
      expect(empty.subsystems).toEqual([]);
      expect(empty.indexedTotal).toBe(0);
      expect(empty.neutralColor).toBe("#6e7681");
    }
  });
});

describe("headline and sample wording", () => {
  it("says how many handoffs found no edge, and says nothing when none are missing", () => {
    expect(unplacedText(1)).toBe("1 handoff unplaced");
    expect(unplacedText(1500)).toBe("1,500 handoffs unplaced");
    expect(unplacedText(0)).toBe("");
  });

  it("never prints an N of M for tests it has no M for", () => {
    expect(testsSampleText(24)).toContain("Sample of 24");
    expect(testsSampleText(24)).toContain("no total");
    expect(testsSampleText(24)).not.toMatch(/\d+ of \d+/);
    expect(testsSampleText(0)).toContain("no total");
  });
});

describe("readView", () => {
  it("reads the view chrome and ignores malformed rows", () => {
    const view = readView({ view: { title: "repo", level: "files", detail_fields: [["Path", "path"], ["bad"], 3], flag_filters: [["dead", "Dead candidates only"]] } });
    expect(view.title).toBe("repo");
    expect(view.suffix).toBe("code graph");
    expect(view.detailFields).toEqual([["Path", "path"]]);
    expect(view.flagFilters).toEqual([["dead", "Dead candidates only"]]);
  });

  it("falls back to a title when the view is absent", () => {
    expect(readView({}).title).toBe("devmap");
  });
});

describe("the stylesheet", () => {
  it("declares a dark scheme, honours reduced motion and uses no external resource", () => {
    expect(PAGE_CSS).toContain("prefers-color-scheme: dark");
    expect(PAGE_CSS).toContain("prefers-reduced-motion");
    expect(PAGE_CSS).not.toMatch(/url\(|@import|https?:\/\//);
    expect(PAGE_CSS).not.toMatch(/text-transform:\s*uppercase|letter-spacing/);
  });
});

describe("the checked-in bundle", () => {
  const path = fileURLToPath(new URL("../../code-map.js.bundle", import.meta.url));

  it("is inline-safe: it cannot end its own script block or open a comment", () => {
    expect(existsSync(path)).toBe(true);
    const bundle = readFileSync(path, "utf8");
    expect(bundle.length).toBeGreaterThan(10_000);
    expect(bundle.toLowerCase()).not.toContain("</script");
    expect(bundle).not.toContain("<!--");
  });

  it("makes no network request of its own", () => {
    const bundle = readFileSync(path, "utf8");
    expect(bundle).not.toMatch(/\bfetch\(|XMLHttpRequest|WebSocket|sendBeacon|importScripts|\.src\s*=/);
  });
});
