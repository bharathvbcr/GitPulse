import { describe, expect, it } from "vitest";
import { formatCoverageReport } from "./report";
import type {
  CoverageArtifact,
  CoverageFamilyStatus,
  CoverageLanguageSplit,
  CoverageReport,
  FileCoverageSummary,
} from "./types";

function totals(hit: number, found: number, percentage: number) {
  return { lines_hit: hit, lines_found: found, percentage };
}

function language(overrides: Partial<CoverageLanguageSplit> = {}): CoverageLanguageSplit {
  return {
    language: "Rust",
    color_hex: "#f74c00",
    files: 12,
    lines_found: 500,
    lines_hit: 410,
    percentage: 82.0,
    ...overrides,
  };
}

function file(overrides: Partial<FileCoverageSummary> = {}): FileCoverageSummary {
  return {
    path: "src/lib.rs",
    language: "Rust",
    color_hex: "#f74c00",
    lines_found: 100,
    lines_hit: 40,
    percentage: 40.0,
    ...overrides,
  };
}

function artifact(overrides: Partial<CoverageArtifact> = {}): CoverageArtifact {
  return {
    path: "target/lcov.info",
    format: "lcov",
    family: "rust",
    skipped: false,
    skip_reason: null,
    totals: totals(410, 500, 82.0),
    ...overrides,
  };
}

function family(overrides: Partial<CoverageFamilyStatus> = {}): CoverageFamilyStatus {
  return {
    family: "rust",
    languages: ["Rust"],
    color_hex: "#f74c00",
    expected_formats: ["lcov.info", "cobertura.xml"],
    expected_paths: ["target/lcov.info"],
    found: true,
    ...overrides,
  };
}

function representativeReport(): CoverageReport {
  return {
    families: [
      family(),
      family({
        family: "python",
        languages: ["Python"],
        color_hex: "#3572A5",
        expected_formats: ["coverage.xml"],
        expected_paths: ["coverage.xml"],
        found: false,
      }),
    ],
    languages: [language()],
    artifacts: [
      artifact(),
      artifact({
        path: "target/old-lcov.info",
        format: "lcov",
        skipped: true,
        skip_reason: "parse failed",
      }),
    ],
    files: [
      file({ path: "src/worst.rs", percentage: 12.5, lines_hit: 10, lines_found: 80 }),
      file({ path: "src/middle.rs", percentage: 55.0, lines_hit: 110, lines_found: 200 }),
      file({ path: "src/best.rs", percentage: 95.0, lines_hit: 190, lines_found: 200 }),
    ],
    overall: totals(410, 500, 82.0),
    truncated: false,
  };
}

describe("formatCoverageReport", () => {
  it("renders the golden shape of a representative report", () => {
    const text = formatCoverageReport(representativeReport(), "/repos/acme");
    const lines = text.split("\n");

    expect(lines[0]).toBe("Coverage report — /repos/acme");

    expect(text).toContain("OVERALL");
    expect(text).toContain("82.0% (410/500 lines)");

    expect(text).toContain("PER-LANGUAGE");
    expect(text).toContain("Rust: 82.0% (410/500 lines, 12 files)");

    expect(text).toContain("LOWEST-COVERED FILES (worst first, showing 3 of 3)");
    // Already-sorted order is preserved, worst first.
    const fileLines = lines.filter((l) => l.startsWith("- src/"));
    expect(fileLines).toEqual([
      "- src/worst.rs: 12.5% (10/80 lines)",
      "- src/middle.rs: 55.0% (110/200 lines)",
      "- src/best.rs: 95.0% (190/200 lines)",
    ]);

    expect(text).toContain("ARTIFACTS (showing 2 of 2)");
    expect(text).toContain("- target/lcov.info (lcov)");
    expect(text).toContain("- target/old-lcov.info (lcov) — skipped: parse failed");

    expect(text).toContain("FAMILIES WITHOUT REPORT");
    expect(text).toContain("- python (expected: coverage.xml)");
    // Found families stay out of the missing section.
    expect(text).not.toContain("- rust (expected:");
  });

  it("omits the truncation flag on complete scans and appends it on truncated ones", () => {
    const clean = formatCoverageReport(representativeReport(), "/repo");
    expect(clean).not.toContain("SCAN TRUNCATED");

    const capped = representativeReport();
    capped.truncated = true;
    const text = formatCoverageReport(capped, "/repo");
    expect(text).toContain("82.0% (410/500 lines) [SCAN TRUNCATED — results partial]");
  });

  it("renders hostile percentages as 0.0%", () => {
    for (const bad of [Number.NaN, -1, Number.NEGATIVE_INFINITY, Number.POSITIVE_INFINITY]) {
      const report = representativeReport();
      report.overall = { ...report.overall, percentage: bad };
      expect(formatCoverageReport(report, "/repo")).toContain("0.0% (410/500 lines)");
    }
  });

  it("renders hostile counts as 0 without throwing", () => {
    const report = representativeReport();
    report.overall = { lines_hit: Number.NaN, lines_found: -7, percentage: 50 };
    expect(formatCoverageReport(report, "/repo")).toContain("50.0% (0/0 lines)");
  });

  it("survives hostile numbers in every per-language and file field", () => {
    const report = representativeReport();
    report.languages = [
      language({
        language: "Weird",
        files: Number.NaN,
        lines_hit: -3,
        lines_found: Number.POSITIVE_INFINITY,
        percentage: Number.NaN,
      }),
    ];
    report.files = [
      file({ path: "src/x.rs", percentage: -12, lines_hit: Number.NaN, lines_found: -1 }),
    ];
    const text = formatCoverageReport(report, "/repo");
    expect(text).toContain("Weird: 0.0% (0/0 lines, 0 files)");
    expect(text).toContain("- src/x.rs: 0.0% (0/0 lines)");
  });

  it("caps the lowest-covered list at 30 with an accurate counter", () => {
    const report = representativeReport();
    report.files = Array.from({ length: 31 }, (_, i) =>
      file({ path: `src/f${i}.rs`, percentage: i * 3 }),
    );
    const text = formatCoverageReport(report, "/repo");
    expect(text).toContain("(worst first, showing 30 of 31)");
    expect(text).toContain("- src/f0.rs: 0.0% (40/100 lines)");
    expect(text).toContain("- src/f29.rs");
    expect(text).not.toContain("- src/f30.rs");
  });

  it("caps the artifact list at 40 with a …and N more tail", () => {
    const report = representativeReport();
    report.artifacts = Array.from({ length: 41 }, (_, i) =>
      artifact({ path: `artifacts/a${i}.info` }),
    );
    const text = formatCoverageReport(report, "/repo");
    expect(text).toContain("(showing 40 of 41)");
    expect(text).toContain("- artifacts/a39.info (lcov)");
    expect(text).not.toContain("- artifacts/a40.info (lcov)");
    expect(text).toContain("…and 1 more");
  });

  it("lists every family without a report together with its expected formats", () => {
    const report = representativeReport();
    report.families = [
      family({ family: "rust", found: true }),
      family({
        family: "javascript",
        languages: ["JavaScript"],
        expected_formats: ["lcov.info", "coverage-final.json"],
        found: false,
      }),
      family({ family: "go", languages: ["Go"], expected_formats: ["coverage.out"], found: false }),
    ];
    const text = formatCoverageReport(report, "/repo");
    expect(text).toContain("FAMILIES WITHOUT REPORT");
    expect(text).toContain("- javascript (expected: lcov.info, coverage-final.json)");
    expect(text).toContain("- go (expected: coverage.out)");
    expect(text.indexOf("FAMILIES WITHOUT REPORT")).toBeLessThan(text.indexOf("- javascript"));
  });

  it("renders a fully empty report as a zeroed header block without throwing", () => {
    const empty = {} as CoverageReport;
    const emptyText = formatCoverageReport(empty, "/repo");
    expect(emptyText.split("\n")[0]).toBe("Coverage report — /repo");
    expect(emptyText).toContain("OVERALL");
    expect(emptyText).toContain("0.0% (0/0 lines)");

    const bare: CoverageReport = {
      families: [],
      languages: [],
      artifacts: [],
      files: [],
      overall: totals(0, 0, 0),
      truncated: false,
    };
    const text = formatCoverageReport(bare, "");
    expect(text.split("\n")[0]).toBe("Coverage report — ");
    expect(text).toContain("OVERALL");
    expect(text).toContain("0.0% (0/0 lines)");
    expect(text).not.toContain("PER-LANGUAGE");
    expect(text).not.toContain("LOWEST-COVERED FILES");
    expect(text).not.toContain("ARTIFACTS");
    expect(text).not.toContain("FAMILIES WITHOUT REPORT");
  });

  it("skips null entries inside arrays instead of throwing", () => {
    const report = representativeReport();
    report.languages = [null as unknown as CoverageLanguageSplit];
    report.files = [null as unknown as FileCoverageSummary];
    report.artifacts = [null as unknown as CoverageArtifact];
    report.families = [null as unknown as CoverageFamilyStatus];
    const text = formatCoverageReport(report, "/repo");
    expect(text.split("\n")[0]).toBe("Coverage report — /repo");
  });
});
