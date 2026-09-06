import { describe, expect, it } from "vitest";
import {
  DEFAULT_SORT,
  HIDEABLE_COLUMNS,
  hiddenFailures,
  nextSort,
  searchRows,
  sortRows,
  type SortKey,
} from "./sort";
import { UNSCANNED, failedCell, readCell, type FleetLanguageStat, type FleetRow } from "./types";

function language(name: string): FleetLanguageStat {
  return {
    language: name,
    color_hex: "#123456",
    category: "programming",
    code_lines: 10,
    file_count: 1,
    percentage: 100,
  };
}

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
    commits: UNSCANNED,
    loc: UNSCANNED,
    storage: UNSCANNED,
    health: UNSCANNED,
    coverage: UNSCANNED,
    ...overrides,
  };
}

const loc = (lines: number) => readCell({ lines, language: null, languages: [] }, 1);

describe("nextSort", () => {
  it("opens each column in the direction its reader wants", () => {
    // Clicking "Vulnerabilities" means "show me the worst", and clicking
    // "Repository" means "A first". Getting this backwards costs a second
    // click every single time.
    expect(nextSort(DEFAULT_SORT, "health").direction).toBe("desc");
    expect(nextSort(DEFAULT_SORT, "repository").direction).toBe("asc");
    expect(nextSort(DEFAULT_SORT, "coverage").direction).toBe("asc");
  });

  it("flips direction when the same column is clicked again", () => {
    const first = nextSort(DEFAULT_SORT, "loc");
    expect(first.direction).toBe("desc");
    expect(nextSort(first, "loc").direction).toBe("asc");
  });
});

describe("sortRows keeps absences out of the comparison", () => {
  const measured = row({ path: "/measured", label: "measured", loc: loc(10) });
  const bigger = row({ path: "/bigger", label: "bigger", loc: loc(9000) });
  const never = row({ path: "/never", label: "never", loc: UNSCANNED });
  const broken = row({ path: "/broken", label: "broken", loc: failedCell("no git") });
  const rows = [never, bigger, broken, measured];

  it("sinks unscanned and failed below every measured value, descending", () => {
    const order = sortRows(rows, { key: "loc", direction: "desc" }).map((r) => r.label);
    expect(order).toEqual(["bigger", "measured", "never", "broken"]);
  });

  it("keeps them at the bottom ascending too — 'not scanned' is not a low score", () => {
    // The bug this file exists to prevent: `value ?? 0` would file every
    // unaudited repository right next to the ones audited and found clean.
    const order = sortRows(rows, { key: "loc", direction: "asc" }).map((r) => r.label);
    expect(order).toEqual(["measured", "bigger", "never", "broken"]);
  });

  it("orders never-scanned before could-not-read, in both directions", () => {
    const only = [broken, never];
    for (const direction of ["asc", "desc"] as const) {
      expect(sortRows(only, { key: "loc", direction }).map((r) => r.label)).toEqual([
        "never",
        "broken",
      ]);
    }
  });

  it("does not mutate the array it was handed", () => {
    const input = [bigger, measured];
    sortRows(input, { key: "loc", direction: "asc" });
    expect(input.map((r) => r.label)).toEqual(["bigger", "measured"]);
  });
});

describe("sortRows orders every column", () => {
  it("ranks by severity, worst first, with open above recents at a tie", () => {
    const rows = [
      row({ path: "/clean", label: "clean", severity: "clean" }),
      row({ path: "/bad", label: "bad", severity: "conflicts" }),
      row({ path: "/old", label: "old", presence: "recent", severity: "conflicts" }),
    ];
    expect(sortRows(rows, { key: "severity", direction: "asc" }).map((r) => r.label)).toEqual([
      "bad",
      "old",
      "clean",
    ]);
  });

  it("reads one number out of the two-number columns", () => {
    const rows = [
      row({ path: "/a", label: "a", sync: readCell({ ahead: 1, behind: 1, stash: 0 }, null) }),
      row({ path: "/b", label: "b", sync: readCell({ ahead: 9, behind: 0, stash: 0 }, null) }),
    ];
    expect(sortRows(rows, { key: "sync", direction: "desc" }).map((r) => r.label)).toEqual([
      "b",
      "a",
    ]);
  });

  it("breaks every tie by label, so the order is total and stable", () => {
    const rows = [
      row({ path: "/z", label: "z", loc: loc(5) }),
      row({ path: "/a", label: "a", loc: loc(5) }),
    ];
    for (const key of ["loc", "changes", "commits"] as SortKey[]) {
      expect(sortRows(rows, { key, direction: "desc" }).map((r) => r.label)).toEqual(["a", "z"]);
    }
  });
});

describe("searchRows", () => {
  const rows = [
    row({
      path: "/work/api-server",
      label: "api-server",
      branch: "feat/login",
      loc: readCell({ lines: 10, language: "Rust", languages: [language("TypeScript")] }, 1),
    }),
    row({ path: "/work/docs", label: "docs", branch: "main" }),
  ];

  it("returns everything for a blank query", () => {
    expect(searchRows(rows, "   ")).toHaveLength(2);
  });

  it("matches the label, the path and the branch, case-insensitively", () => {
    expect(searchRows(rows, "API").map((r) => r.label)).toEqual(["api-server"]);
    expect(searchRows(rows, "/work/docs").map((r) => r.label)).toEqual(["docs"]);
    expect(searchRows(rows, "feat/").map((r) => r.label)).toEqual(["api-server"]);
  });

  it("finds a repository by any language in its mix, not only the dominant one", () => {
    // Searching "typescript" should find the mostly-Rust service that has a
    // TypeScript front end inside it.
    expect(searchRows(rows, "typescript").map((r) => r.label)).toEqual(["api-server"]);
    expect(searchRows(rows, "rust").map((r) => r.label)).toEqual(["api-server"]);
  });

  it("treats regex metacharacters as literal text", () => {
    // Plain substring, never a compiled pattern: an unanchored user-supplied
    // regex over every row on every keystroke is a backtracking hazard.
    expect(() => searchRows(rows, "(((((a+)+)+)+$")).not.toThrow();
    expect(searchRows(rows, ".*")).toHaveLength(0);
  });

  it("does not mutate the array it was handed", () => {
    const input = [...rows];
    searchRows(input, "api").push(row({ path: "/x", label: "x" }));
    expect(input).toHaveLength(2);
  });
});

describe("hiddenFailures", () => {
  const failing = row({
    path: "/a",
    label: "a",
    storage: failedCell("permission denied"),
    health: failedCell("npm is missing"),
  });
  const fine = row({ path: "/b", label: "b", storage: loc(1) as never });

  it("reports nothing while every column is visible", () => {
    expect(hiddenFailures([failing], new Set())).toEqual([]);
  });

  it("names the column and counts the repositories it is hiding", () => {
    // A hidden column is still a column that could not be read. Letting the
    // failure vanish with it is the same lie as a blank cell, one level up.
    const found = hiddenFailures([failing, fine], new Set(["storage", "health"]));
    expect(found).toEqual([
      { key: "storage", count: 1 },
      { key: "health", count: 1 },
    ]);
  });

  it("says nothing about a hidden column whose cells are merely unscanned", () => {
    // "Nobody has looked" is not a failure being concealed; surfacing it would
    // make the notice fire constantly and stop meaning anything.
    const unscanned = row({ path: "/c", label: "c", storage: UNSCANNED });
    expect(hiddenFailures([unscanned], new Set(["storage"]))).toEqual([]);
  });

  it("ignores recents rows, like every other fleet total", () => {
    const recent = row({
      path: "/old",
      label: "old",
      presence: "recent",
      storage: failedCell("gone"),
    });
    expect(hiddenFailures([recent], new Set(["storage"]))).toEqual([]);
  });

  it("offers every measurable column for hiding, and no structural one", () => {
    // Hiding Repository would leave a grid of measurements with no way to tell
    // whose they are.
    expect(HIDEABLE_COLUMNS).not.toContain("repository");
    expect(HIDEABLE_COLUMNS).not.toContain("severity");
    for (const key of ["changes", "sync", "work", "commits", "activity", "loc", "storage", "health", "coverage"] as SortKey[]) {
      expect(HIDEABLE_COLUMNS, key).toContain(key);
    }
  });
});
