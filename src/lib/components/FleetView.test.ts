import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import FleetView, { applyFilter, scanTargets, SEVERITY_STRIPE } from "./FleetView.svelte";
import FleetCell from "./FleetCell.svelte";
import { UNSCANNED, failedCell, readCell, type FleetRow } from "../fleet/types";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "FleetView.svelte"), "utf8");
const cellSource = readFileSync(join(here, "FleetCell.svelte"), "utf8");

function row(overrides: Partial<FleetRow> = {}): FleetRow {
  return {
    path: "/repo/a",
    label: "a",
    presence: "open",
    branch: "main",
    severity: "clean",
    headline: "clean",
    changes: UNSCANNED,
    sync: UNSCANNED,
    watchWarning: null,
    work: UNSCANNED,
    activity: UNSCANNED,
    loc: UNSCANNED,
    storage: UNSCANNED,
    health: UNSCANNED,
    coverage: UNSCANNED,
    ...overrides,
  };
}

describe("FleetView", () => {
  it("renders with nothing open and says so", () => {
    const { body } = render(FleetView);
    expect(body).toContain("Fleet");
    expect(body).toContain("No repositories are open.");
    expect(body).toContain("No repositories yet");
  });
});

describe("applyFilter", () => {
  it("shows everything on All, including recents", () => {
    const rows = [row(), row({ path: "/old", presence: "recent", severity: "unknown" })];
    expect(applyFilter(rows, "all")).toHaveLength(2);
  });

  it("keeps only open repositories that need something on Attention", () => {
    const rows = [
      row({ severity: "clean" }),
      row({ path: "/b", severity: "conflicts" }),
      // A recents row is unknown by construction; listing it under "needs
      // attention" would fill the list with rows nobody can act on.
      row({ path: "/old", presence: "recent", severity: "unknown" }),
    ];
    expect(applyFilter(rows, "attention").map((r) => r.path)).toEqual(["/b"]);
  });

  it("does not mutate the array it was handed", () => {
    const rows = [row()];
    applyFilter(rows, "all").push(row({ path: "/x" }));
    expect(rows).toHaveLength(1);
  });
});

describe("scanTargets", () => {
  it("never sends a scan at a repository that is not open", () => {
    // A recents path may not exist any more, and scanning it would spawn work
    // against a repository the user has not opened.
    const rows = [row(), row({ path: "/old", presence: "recent" })];
    expect(scanTargets(rows)).toEqual([{ path: "/repo/a", label: "a" }]);
  });
});

describe("SEVERITY_STRIPE", () => {
  it("gives every severity its own tone", () => {
    const severities: FleetRow["severity"][] = [
      "conflicts",
      "operation",
      "unknown",
      "uncommitted",
      "unpushed",
      "stash",
      "clean",
    ];
    for (const severity of severities) {
      expect(SEVERITY_STRIPE[severity], severity).toBeTruthy();
    }
  });
});

describe("FleetCell", () => {
  it("renders a measured value with its own content", () => {
    const { body } = render(FleetCell, {
      props: { cell: readCell(42, Date.now()), label: "Lines of code" },
    });
    expect(body).toContain('data-state="read"');
    expect(body).not.toContain("not scanned");
  });

  it("says not scanned rather than showing a zero or a dash", () => {
    const { body } = render(FleetCell, { props: { cell: UNSCANNED, label: "Storage" } });
    expect(body).toContain('data-state="unscanned"');
    expect(body).toContain("not scanned");
    // An em dash or a 0 here is the whole failure this component exists to
    // prevent: it is indistinguishable from a measurement.
    expect(body).not.toContain(">0<");
  });

  it("says could not read, and carries the reason, when a scan failed", () => {
    const { body } = render(FleetCell, {
      props: { cell: failedCell("npm is not installed"), label: "Vulnerabilities" },
    });
    expect(body).toContain('data-state="failed"');
    expect(body).toContain("could not read");
    expect(body).toContain("npm is not installed");
  });

  it("marks a partial value so a floor cannot read as a total", () => {
    const { body } = render(FleetCell, {
      props: { cell: readCell(42, Date.now(), true), label: "Storage" },
    });
    expect(body).toContain('data-partial="true"');
  });

  it("distinguishes unscanned from failed in the markup, not only in words", () => {
    const unscanned = render(FleetCell, { props: { cell: UNSCANNED, label: "x" } }).body;
    const failed = render(FleetCell, { props: { cell: failedCell("boom"), label: "x" } }).body;
    expect(unscanned).not.toBe(failed);
  });
});

describe("the grid cannot invent a measurement", () => {
  it("routes every measurable column through FleetCell", () => {
    // Nine columns, six of them measurable. A column rendered inline would be
    // one where "no value" gets to pick its own spelling.
    const cells = source.match(/<FleetCell/g) ?? [];
    expect(cells.length).toBeGreaterThanOrEqual(6);
  });

  it("only ever reads a value inside a read branch", () => {
    // Every `.value` access in the template is guarded by its cell's kind, so
    // there is no path where an unscanned cell's absent value is coerced.
    for (const match of source.matchAll(/row\.(\w+)\.value/g)) {
      const field = match[1];
      expect(source, `${field}.value is read without a kind guard`).toContain(
        `row.${field}.kind === "read"`,
      );
    }
  });

  it("never renders a bare total when nothing was counted", () => {
    // The header tiles fall back to an em dash and the coverage clause, rather
    // than printing 0 for a column no repository has been scanned for.
    expect(source).toContain("locTotal.counted > 0");
    expect(source).toContain("storageTotal.counted > 0");
    expect(source).toContain("vulnTotal.counted > 0");
    expect(source).toContain("describeTally");
  });

  it("keeps the three cell states in one component, not spelled per column", () => {
    expect(cellSource).toContain('data-state="read"');
    expect(cellSource).toContain('data-state="unscanned"');
    expect(cellSource).toContain('data-state="failed"');
  });
});

describe("expensive scans stay opt-in", () => {
  it("never starts a family sweep from an effect", () => {
    // Storage walks 250,000 files and the audit spawns a package manager;
    // both run only from the toolbar, like autoRunCoverage elsewhere.
    const effects = source.match(/\$effect\(\(\) => \{[\s\S]*?\n  \}\);/g) ?? [];
    expect(effects.length).toBeGreaterThan(0);
    for (const effect of effects) {
      expect(effect).not.toContain("scanAll");
      expect(effect).not.toContain("scanOne");
    }
  });

  it("refreshes the cheap sweep only when the repository set changes", () => {
    expect(source).toContain("if (key === lastSwept) return;");
  });
});
