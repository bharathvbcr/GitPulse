import { describe, expect, it } from "vitest";
import { filterCoverageFiles, missedLineBlocks, moveMissedBlock, coverageScanReasons, coverageScanStatus } from "./explorer";
import type { CoverageArtifact, CoverageFamilyStatus, FileCoverageSummary, CoverageReport } from "./types";

const artifact = (skipped: boolean): CoverageArtifact =>
  ({ path: "coverage/lcov.info", format: "lcov", family: "javascript", skipped, totals: { lines_found: 0, lines_hit: 0, percentage: 0 } });

const family = (name: string, found: boolean): CoverageFamilyStatus =>
  ({ family: name, languages: [name], color_hex: "#000", expected_formats: [], expected_paths: [], found, suggested_commands: [], setup_commands: [], tool_ready: true, tool_detail: "", duration_hint: "" });

const file = (path: string, hit: number, found: number, language = "TypeScript"): FileCoverageSummary =>
  ({ path, language, lines_hit: hit, lines_found: found, percentage: found ? hit / found * 100 : 0, color_hex: "#000" });
const files = [file("src/Good.ts", 10, 10), file("src/empty.ts", 0, 0), file("lib/a.rs", 1, 10, "Rust"), file("src/b.ts", 0, 2)];

describe("coverage exploration", () => {
  it("combines case-insensitive path, language and missed filters without mutating the report", () => {
    expect(filterCoverageFiles(files, " SRC/ ", "TypeScript", "missed", "missed").map(f => f.path)).toEqual(["src/b.ts"]);
    expect(files[0].path).toBe("src/Good.ts");
    expect(filterCoverageFiles(files, "", "", "below80", "coverage").map(f => f.path)).toEqual(["src/b.ts", "lib/a.rs"]);
  });
  it("sorts missed counts descending, keeps unmeasured last, and has stable path ties", () => {
    expect(filterCoverageFiles(files, "", "", "all", "missed").map(f => f.path)).toEqual(["lib/a.rs", "src/b.ts", "src/Good.ts", "src/empty.ts"]);
    expect(filterCoverageFiles(files, "nomatch", "", "all", "path")).toEqual([]);
    expect(filterCoverageFiles([file("b", 0, 1), file("a", 0, 1)], "", "", "all", "coverage").map(f => f.path)).toEqual(["a", "b"]);
  });
  it("groups only explicit zero-hit source lines and bounds navigation to real source", () => {
    const blocks = missedLineBlocks(new Map([[9, 0], [3, 0], [2, 0], [4, 1], [0, 0], [12, 0], [5, -1]]), 10);
    expect(blocks).toEqual([{ start: 2, end: 3 }, { start: 9, end: 9 }]);
    expect(moveMissedBlock(blocks, null, 1)).toBe(2);
    expect(moveMissedBlock(blocks, 2, 1)).toBe(9);
    expect(moveMissedBlock(blocks, 9, 1)).toBe(2);
    expect(moveMissedBlock(blocks, null, -1)).toBe(9);
    expect(moveMissedBlock(blocks, 2, -1)).toBe(9);
    expect(moveMissedBlock([], null, 1)).toBeNull();
  });
  it("distinguishes unmeasured, measured zero, partial and failed scans", () => {
    const report: CoverageReport = { files: [], families: [], languages: [], artifacts: [], overall: { lines_found: 1, lines_hit: 0, percentage: 0 }, truncated: false };
    expect(coverageScanStatus(null, false, false, false)).toBe("Not scanned");
    expect(coverageScanStatus(report, false, false, false)).toBe("Measured");
    expect(coverageScanStatus({ ...report, truncated: true }, false, false, false)).toBe("Partial coverage");
    expect(coverageScanStatus(report, false, true, false)).toBe("Stale · scan failed");
    expect(coverageScanStatus(null, false, true, false)).toBe("Scan failed");
    expect(coverageScanStatus(report, true, false, false)).toBe("Scanning…");
    expect(coverageScanStatus(report, false, false, true)).toBe("Partial coverage");
    expect(coverageScanStatus({ ...report, overall: { lines_found: 0, lines_hit: 0, percentage: 0 } }, false, false, false)).toBe("Unmeasured");
  });

  it("names every reason a scan is partial, so the header word has a recoverable cause", () => {
    const report: CoverageReport = { files: [], families: [], languages: [], artifacts: [], overall: { lines_found: 1, lines_hit: 0, percentage: 0 }, truncated: false };
    expect(coverageScanReasons(null, false)).toEqual([]);
    expect(coverageScanReasons(report, false)).toEqual([]);
    expect(coverageScanReasons({ ...report, truncated: true }, false)).toEqual(["the scan did not read every artifact"]);
    expect(coverageScanReasons({ ...report, limit_notices: [{ resource: "covered files", kept: 4000, total: 12873 }] }, false))
      .toEqual(["only 4000 of 12873 covered files were kept"]);
    expect(coverageScanReasons({ ...report, go_modules_partial: true }, false)).toEqual(["the Go module search was cut short"]);
    expect(coverageScanReasons({ ...report, artifacts: [artifact(true), artifact(false)] }, false)).toEqual(["1 coverage artifact was skipped"]);
    expect(coverageScanReasons({ ...report, artifacts: [artifact(true), artifact(true)] }, false)).toEqual(["2 coverage artifacts were skipped"]);
    expect(coverageScanReasons({ ...report, families: [family("go", false), family("rust", true), family("python", false)] }, false))
      .toEqual(["no report found for go, python"]);
    expect(coverageScanReasons(report, true)).toEqual(["a command only passed with files excluded from measurement"]);
    // Every cause is reported, not just the first one the old predicate chain hit.
    expect(coverageScanReasons({ ...report, truncated: true, go_modules_partial: true }, true)).toHaveLength(3);
  });

  it("does not call a scan partial for a limit notice that dropped nothing", () => {
    // `limit_notices?.length` alone made a cap that kept everything read as
    // partial, while the detail chip — which filters on total > kept — printed
    // nothing, leaving a warning with no recoverable cause.
    const report: CoverageReport = {
      files: [], families: [], languages: [], artifacts: [], truncated: false,
      overall: { lines_found: 1, lines_hit: 0, percentage: 0 },
      limit_notices: [{ resource: "covered files", kept: 12, total: 12 }],
    };
    expect(coverageScanReasons(report, false)).toEqual([]);
    expect(coverageScanStatus(report, false, false, false)).toBe("Measured");
  });
});
