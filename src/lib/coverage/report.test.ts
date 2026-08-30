import { describe, expect, it } from "vitest";
import {
  buildCoverageIssueDraft,
  classifyCoverageFailure,
  coverageFailureHint,
  type CoverageExclusionNotice,
  formatCoverageReport,
  formatFailedCoverageDiagnostics,
  NO_COVERAGE_DATA,
  type FailedCoverageScript,
} from "./report";
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
    suggested_commands: [],
    setup_commands: [],
    tool_ready: true,
    tool_detail: "",
    duration_hint: "",
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

  it("labels cumulative Rust workspace runs separately from fallback generators", () => {
    const report = representativeReport();
    report.families = [
      family({
        found: false,
        suggested_commands: ["cargo llvm-cov --manifest-path one/Cargo.toml", "cargo llvm-cov --manifest-path two/Cargo.toml"],
      }),
      family({
        family: "javascript",
        found: false,
        suggested_commands: ["npm run coverage", "npx --no-install vitest run --coverage"],
      }),
    ];

    const text = formatCoverageReport(report, "/repo");
    expect(text).toContain("`cargo llvm-cov --manifest-path one/Cargo.toml` then `cargo llvm-cov --manifest-path two/Cargo.toml`");
    expect(text).toContain("`npm run coverage` or `npx --no-install vitest run --coverage`");
  });

  it("renders hostile percentages as 0.0%", () => {
    for (const bad of [Number.NaN, -1, Number.NEGATIVE_INFINITY, Number.POSITIVE_INFINITY]) {
      const report = representativeReport();
      report.overall = { ...report.overall, percentage: bad };
      expect(formatCoverageReport(report, "/repo")).toContain("0.0% (410/500 lines)");
    }
  });

  it("clamps impossible finite percentages and counts", () => {
    const report = representativeReport();
    report.overall = {
      lines_hit: Number.MAX_VALUE,
      lines_found: Number.MAX_VALUE,
      percentage: 1234,
    };
    expect(formatCoverageReport(report, "/repo")).toContain(
      `100.0% (${Number.MAX_SAFE_INTEGER}/${Number.MAX_SAFE_INTEGER} lines)`,
    );
  });

  it("refuses to render a percentage over counts that sanitized to nothing", () => {
    // Was asserted as "50.0% (0/0 lines)". That expectation was wrong: a
    // producer percentage with no lines behind it is not a measurement, and
    // printing it gave a hostile (or merely broken) payload the authority of a
    // coverage figure. Zero measurable lines now renders as "not measured".
    const report = representativeReport();
    report.overall = { lines_hit: Number.NaN, lines_found: -7, percentage: 50 };
    const text = formatCoverageReport(report, "/repo");
    expect(text).not.toContain("50.0%");
    expect(text).toContain(NO_COVERAGE_DATA);
  });

  it("keeps hit counts from exceeding the lines they were counted out of", () => {
    // lines_hit > lines_found cannot come from the scanner (both are derived
    // from one map) but can come from a hand-built or corrupted payload, and
    // "600/500 lines" is not a number any reader can use.
    const report = representativeReport();
    report.overall = { lines_hit: 600, lines_found: 500, percentage: 82 };
    expect(formatCoverageReport(report, "/repo")).toContain("82.0% (500/500 lines)");
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

  it("names the scanner-planned generate commands for families without a report", () => {
    const report = representativeReport();
    report.families = [
      family({
        family: "rust",
        found: false,
        suggested_commands: [
          "cargo llvm-cov --manifest-path src-tauri/Cargo.toml --workspace --lcov --output-path src-tauri/lcov.info",
        ],
      }),
      family({
        family: "javascript",
        found: false,
        suggested_commands: ["npm run coverage", "npx --no-install jest --coverage"],
      }),
      family({
        family: "go",
        found: false,
        suggested_commands: [
          "go -C api test ./... -coverprofile=coverage.out",
          "go -C cli test ./... -coverprofile=coverage.out",
        ],
      }),
    ];
    const text = formatCoverageReport(report, "/repo");
    expect(text).toContain(
      "- rust (expected: lcov.info, cobertura.xml) — run `cargo llvm-cov --manifest-path src-tauri/Cargo.toml --workspace --lcov --output-path src-tauri/lcov.info`",
    );
    expect(text).toContain(
      "- javascript (expected: lcov.info, cobertura.xml) — run `npm run coverage` or `npx --no-install jest --coverage`",
    );
    expect(text).toContain(
      "- go (expected: lcov.info, cobertura.xml) — run `go -C api test ./... -coverprofile=coverage.out` then `go -C cli test ./... -coverprofile=coverage.out`",
    );
  });

  it("names a missing generator toolchain and its setup commands", () => {
    const report = representativeReport();
    report.families = [
      family({
        family: "rust",
        found: false,
        tool_ready: false,
        tool_detail: "cargo-llvm-cov is not installed.",
        duration_hint:
          "Installing cargo-llvm-cov and generating Rust coverage can take several minutes.",
        setup_commands: [
          "rustup component add llvm-tools-preview",
          "cargo install cargo-llvm-cov --locked",
        ],
        suggested_commands: [
          "cargo llvm-cov --manifest-path src-tauri/Cargo.toml --workspace --lcov --output-path src-tauri/lcov.info",
        ],
      }),
    ];
    const text = formatCoverageReport(report, "/repo");
    expect(text).toContain("cargo-llvm-cov is not installed.");
    expect(text).toContain("setup `rustup component add llvm-tools-preview` then `cargo install cargo-llvm-cov --locked`");
    expect(text).toContain(
      "run `cargo llvm-cov --manifest-path src-tauri/Cargo.toml --workspace --lcov --output-path src-tauri/lcov.info`",
    );
    expect(text).toContain("several minutes");
  });

  it("renders a fully empty report as an explicit no-data header without throwing", () => {
    // Was asserted as "0.0% (0/0 lines)" — the same line a repo whose code is
    // real but entirely uncovered would get. One is "we could not measure",
    // the other is a finding; collapsing them sent "0% coverage" into the
    // model prompt, the clipboard and the issue title for repos that simply
    // had no artifact.
    const empty = {} as CoverageReport;
    const emptyText = formatCoverageReport(empty, "/repo");
    expect(emptyText.split("\n")[0]).toBe("Coverage report — /repo");
    expect(emptyText).toContain("OVERALL");
    expect(emptyText).toContain(NO_COVERAGE_DATA);
    expect(emptyText).not.toContain("0.0%");

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
    expect(text).toContain(NO_COVERAGE_DATA);
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
describe("buildCoverageIssueDraft", () => {
  it("creates a bounded issue without leaking the local checkout path", () => {
    const repo = "/Users/example/Secret Checkout";
    const draft = buildCoverageIssueDraft(
      representativeReport(),
      repo,
      `Inspect ${repo}/src/main.py and add edge-case tests.`,
    );

    expect(draft.title).toBe("test(coverage): address 82.0% line coverage");
    expect(draft.body).toContain("<!-- gitpulse:coverage-report:v1 -->");
    expect(draft.body).toContain("## Coverage snapshot");
    expect(draft.body).toContain("## MANVI analysis");
    expect(draft.body).toContain("<repository>/src/main.py");
    expect(draft.body).not.toContain(repo);
    expect(new TextEncoder().encode(draft.body).byteLength).toBeLessThanOrEqual(60 * 1024);
    expect(draft.clipped).toBe(false);
  });

  it("marks partial scans in the title and keeps producer controls literal", () => {
    const report = representativeReport();
    report.truncated = true;
    report.files[0]!.path = "src/injected.ts\n## forged heading\u0007";
    const draft = buildCoverageIssueDraft(report, "/repo");

    expect(draft.title).toContain("(partial scan)");
    expect(draft.body).toContain("src/injected.ts\\n## forged heading\\u{0007}");
    expect(draft.body).not.toContain("src/injected.ts\n## forged heading");
  });

  it("clips hostile multi-byte MANVI prose by UTF-8 bytes with an explicit note", () => {
    const draft = buildCoverageIssueDraft(representativeReport(), "/repo", "🧪".repeat(40_000));
    expect(draft.clipped).toBe(true);
    expect(draft.body).toContain("GitPulse clipped this draft");
    expect(new TextEncoder().encode(draft.body).byteLength).toBeLessThanOrEqual(60 * 1024);
    expect(draft.body).not.toContain("\uFFFD");
  });
});

describe("formatFailedCoverageDiagnostics", () => {
  it("renders single failure with command label and detail", () => {
    const failures: FailedCoverageScript[] = [
      {
        label: "npm run test:coverage",
        detail: "Error: Vitest exited with code 1\nAssertion failed in App.test.ts",
      },
    ];
    const text = formatFailedCoverageDiagnostics(failures, { repoPath: "/repo/gitpulse" });
    expect(text).toContain("Coverage failure diagnostics — /repo/gitpulse");
    expect(text).toContain("Command: npm run test:coverage");
    expect(text).toContain("Status: failed");
    expect(text).toContain("Output:");
    expect(text).toContain("Assertion failed in App.test.ts");
  });

  it("renders multiple failed commands with indexing and counts", () => {
    const failures: FailedCoverageScript[] = [
      {
        label: "cargo llvm-cov",
        detail: "error: failed to compile test harness",
      },
      {
        label: "pytest --cov",
        detail: "FAILED test_api.py::test_login",
      },
    ];
    const text = formatFailedCoverageDiagnostics(failures, { repoPath: "/repo/gitpulse" });
    expect(text).toContain("Unsuccessful coverage commands (2):");
    expect(text).toContain("[1] Command: cargo llvm-cov");
    expect(text).toContain("failed to compile test harness");
    expect(text).toContain("[2] Command: pytest --cov");
    expect(text).toContain("FAILED test_api.py::test_login");
  });

  /**
   * Regression: a command that exits 0 and produces no coverage was reported
   * with the same "Status: failed" line as a command that crashed. They need
   * different fixes — one is a broken suite, the other is a suite that ran and
   * measured nothing — so the diagnostics must not flatten them together.
   */
  it("distinguishes a clean run that produced no coverage from a failure", () => {
    const single = formatFailedCoverageDiagnostics(
      [{ label: "go test ./... -coverprofile=coverage.out", detail: "ok  \tno test files", status: "no_data" }],
      { repoPath: "/repo" },
    );
    expect(single).toContain("Status: exited 0 but produced no coverage data");
    expect(single).not.toContain("Status: failed");

    const batch = formatFailedCoverageDiagnostics(
      [
        { label: "npm run coverage", detail: "boom", status: "failed" },
        { label: "go test ./...", detail: "no test files", status: "no_data" },
      ],
      { repoPath: "/repo" },
    );
    expect(batch).toContain("Status: failed");
    expect(batch).toContain("Status: exited 0 but produced no coverage data");
  });

  it("still reports an unlabelled outcome as a failure", () => {
    // Callers that predate the distinction must keep their old meaning.
    const text = formatFailedCoverageDiagnostics([{ label: "make cov", detail: "x" }], {
      repoPath: "/repo",
    });
    expect(text).toContain("Status: failed");
  });

  it("includes scan error when present", () => {
    const failures: FailedCoverageScript[] = [
      {
        label: "npm test",
        detail: "Exit 1",
      },
    ];
    const text = formatFailedCoverageDiagnostics(failures, {
      repoPath: "/repo/gitpulse",
      scanError: "Corrupted lcov.info artifact",
    });
    expect(text).toContain("Scan error: Corrupted lcov.info artifact");
    expect(text).toContain("Command: npm test");
  });

  it("renders scan error even when there are no script failures", () => {
    const text = formatFailedCoverageDiagnostics([], {
      repoPath: "/repo/gitpulse",
      scanError: "Permission denied reading coverage directory",
    });
    expect(text).toContain("Coverage failure diagnostics — /repo/gitpulse");
    expect(text).toContain("Scan error: Permission denied reading coverage directory");
  });

  it("returns fallback message when nothing failed", () => {
    expect(formatFailedCoverageDiagnostics([])).toBe("No coverage failures recorded.");
  });

  it("handles missing or empty details cleanly without crashing", () => {
    const failures: FailedCoverageScript[] = [
      {
        label: "npm test",
      },
    ];
    const text = formatFailedCoverageDiagnostics(failures);
    expect(text).toContain("Command: npm test");
    expect(text).toContain("(no output recorded)");
  });

  it("hints when go test ran outside a module", () => {
    const text = formatFailedCoverageDiagnostics(
      [
        {
          label: "go test ./... -coverprofile=coverage.out",
          detail:
            "# ./...\npattern ./...: directory prefix . does not contain main module or its selected dependencies",
        },
      ],
      { repoPath: "/Users/bharath/Code/scholarlm" },
    );
    expect(text).toContain("Hint:");
    expect(text).toContain("go -C <module-dir> test ./... -coverprofile=coverage.out");
    expect(text).not.toContain("wrong ecosystem command");
    expect(
      coverageFailureHint(
        "go test ./... -coverprofile=coverage.out",
        "# ./...\npattern ./...: directory prefix . does not contain main module or its selected dependencies",
      ),
    ).toContain("go.mod");
  });

  it("hints when vitest ran and tests failed", () => {
    const detail = [
      "FAIL  frontend/App.test.tsx > renders",
      "React does not recognize the `featureKey` prop on a DOM element",
      "An update to App inside a test was not wrapped in act(...)",
      " Test Files  2 failed | 10 passed (12)",
    ].join("\n");
    const text = formatFailedCoverageDiagnostics([
      {
        label: "npx --no-install vitest run --coverage",
        detail,
      },
    ]);
    expect(text).toContain(
      "Hint: The coverage generator ran; tests failed. This is a test failure, not the wrong ecosystem command.",
    );
    expect(
      coverageFailureHint("npx --no-install vitest run --coverage", detail),
    ).toContain("not the wrong ecosystem command");
  });

  it("does not hint on missing-script or compile noise", () => {
    expect(
      coverageFailureHint("npm run coverage", "npm ERR! missing script: coverage"),
    ).toBeNull();
    expect(
      coverageFailureHint("cargo llvm-cov", "error: failed to compile test harness"),
    ).toBeNull();
  });

  it("hints when a generator binary is missing", () => {
    expect(
      coverageFailureHint(
        "pytest --cov --cov-report=xml",
        "Failed to spawn pytest: No such file or directory (os error 2)",
      ),
    ).toContain("not installed");
    expect(
      coverageFailureHint(
        "dotnet test --collect:\"XPlat Code Coverage\"",
        "Failed to spawn dotnet: No such file or directory (os error 2)",
      ),
    ).toContain("not installed");
  });

  it("hints when MANVI refused an unallowlisted generator", () => {
    expect(
      coverageFailureHint(
        "swift test --enable-code-coverage",
        "MANVI coverage generation action refused: 'swift test --enable-code-coverage' is outside the purpose-specific command allowlist",
      ),
    ).toContain("allowlist");
  });

  it("hints when npx --no-install has no local package", () => {
    expect(
      coverageFailureHint(
        "npx --no-install jest --coverage",
        'npm error npx canceled due to missing packages and no YES option: ["jest@30.4.2"]',
      ),
    ).toContain("will not download");
  });

  it("hints when vitest is missing the coverage provider", () => {
    expect(
      coverageFailureHint(
        "npx --no-install vitest run --coverage",
        "MISSING DEPENDENCY  Cannot find dependency '@vitest/coverage-v8'",
      ),
    ).toContain("@vitest/coverage-v8");
  });

  it("hints when the Gradle wrapper is not in the repository", () => {
    expect(
      coverageFailureHint(
        "./gradlew test jacocoTestReport",
        "MANVI coverage generation action refused: wrapper './gradlew' is not a repository file",
      ),
    ).toContain("Gradle wrapper");
  });
});

describe("coverage report — cap disclosure and no-data honesty (regression)", () => {
  it("headlines the observed file total, not the count that survived the cap", () => {
    // Pre-fix the heading read "showing 30 of 4000": the scanner's own
    // max_files cap was invisible, so a 12,873-file repo reported as a
    // 4,000-file one and the sample read as the inventory.
    const report = representativeReport();
    report.files = Array.from({ length: 40 }, (_, i) =>
      file({ path: `src/f${i}.rs`, percentage: i, lines_hit: i, lines_found: 100 }),
    );
    report.truncated = true;
    report.limit_notices = [{ resource: "covered files", kept: 40, total: 12873 }];

    const text = formatCoverageReport(report, "/repo");
    expect(text).toContain("showing 30 of 12873; 40 retained by the scan cap");
    expect(text).toContain("SCAN LIMITS (the sections above are a bounded sample)");
    expect(text).toContain("- covered files: retained 40 of 12873");
  });

  it("discloses a capped artifact sweep the same way", () => {
    const report = representativeReport();
    report.limit_notices = [{ resource: "coverage artifacts", kept: 2, total: 61 }];
    const text = formatCoverageReport(report, "/repo");
    expect(text).toContain("ARTIFACTS (showing 2 of 61; 2 read before the scan cap)");
  });

  it("leaves complete sections unqualified", () => {
    const text = formatCoverageReport(representativeReport(), "/repo");
    expect(text).toContain("LOWEST-COVERED FILES (worst first, showing 3 of 3)");
    expect(text).toContain("ARTIFACTS (showing 2 of 2)");
    expect(text).not.toContain("SCAN LIMITS");
    expect(text).not.toContain("retained by the scan cap");
  });

  it("never lets a notice shrink a section below the rows it prints", () => {
    // A notice claiming fewer observed than retained is incoherent; believing
    // it would print "showing 3 of 1".
    const report = representativeReport();
    report.limit_notices = [{ resource: "covered files", kept: 3, total: 1 }];
    expect(formatCoverageReport(report, "/repo")).toContain(
      "LOWEST-COVERED FILES (worst first, showing 3 of 3)",
    );
  });

  it("does not title an issue with a coverage figure the scan never measured", () => {
    const empty: CoverageReport = {
      families: [],
      languages: [],
      artifacts: [],
      files: [],
      overall: totals(0, 0, 0),
      truncated: false,
    };
    const draft = buildCoverageIssueDraft(empty, "/repos/acme");
    expect(draft.title).toBe("test(coverage): no coverage data was produced");
    expect(draft.title).not.toContain("0.0%");
    expect(draft.body).toContain(NO_COVERAGE_DATA);
    // The local path still never crosses to GitHub.
    expect(draft.body).not.toContain("/repos/acme");
  });

  it("still titles a real measurement with its percentage", () => {
    const draft = buildCoverageIssueDraft(representativeReport(), "/repos/acme");
    expect(draft.title).toBe("test(coverage): address 82.0% line coverage");
  });

  it("says so when a missing family carries neither a command nor a reason", () => {
    const report = representativeReport();
    report.families = [
      family({ family: "native", found: false, expected_formats: [], suggested_commands: [] }),
    ];
    const text = formatCoverageReport(report, "/repo");
    expect(text).toContain(
      "- native (no artifact locations known) — no generator planned and no reason reported",
    );
  });

  it("prefers the backend's reason when it has one", () => {
    const report = representativeReport();
    report.families = [
      family({
        family: "native",
        found: false,
        expected_formats: ["lcov"],
        suggested_commands: [],
        tool_ready: false,
        tool_detail: "No CMakeLists.txt or Makefile in this repository.",
      }),
    ];
    const text = formatCoverageReport(report, "/repo");
    expect(text).toContain("- native (expected: lcov) — No CMakeLists.txt or Makefile");
    expect(text).not.toContain("no reason reported");
  });
});

/**
 * The failure the user hit repeatedly on a real repository: pytest imported a
 * collected module that called `sys.exit()`, which aborts the whole session
 * before a single test runs.
 */
describe("coverageFailureHint: pytest aborted during collection", () => {
  const realOutput = [
    "stderr:",
    "mainloop: caught unexpected SystemExit!",
    "stdout:",
    "INTERNALERROR> Traceback (most recent call last):",
    'INTERNALERROR>   File "/repo/.venv/lib/python3.14/site-packages/_pytest/runner.py", line 341, in from_call',
    "INTERNALERROR>     result: TResult | None = func()",
    'INTERNALERROR>   File "/repo/.venv/lib/python3.14/site-packages/_pytest/python.py", line 508, in importtestmodule',
    "INTERNALERROR>     mod = import_path(",
    'INTERNALERROR>   File "<frozen importlib._bootstrap>", line 1406, in _gcd_import',
    'INTERNALERROR>   File "/repo/bench/stress_test.py", line 944, in <module>',
    "INTERNALERROR>     sys.exit(1 if FAIL else 0)",
    "INTERNALERROR> SystemExit: 0",
    "",
    "============================ no tests ran in 17.74s ============================",
  ].join("\n");

  it("names the repository module that aborted the session, not a pytest internal", () => {
    const hint = coverageFailureHint(".venv/bin/python -m pytest --cov --cov-report=xml", realOutput);
    expect(hint).toContain("sys.exit()");
    expect(hint).toContain("/repo/bench/stress_test.py:944");
    // The frames inside the virtualenv and the frozen importlib are pytest's
    // own machinery; blaming them would send the reader to the wrong file.
    expect(hint).not.toContain("site-packages");
    expect(hint).not.toContain("importlib");
    expect(hint).not.toContain("_pytest");
  });

  it("offers the three real remedies", () => {
    const hint = coverageFailureHint("pytest --cov", realOutput) ?? "";
    expect(hint).toContain("__main__");
    expect(hint).toContain("--ignore=");
    expect(hint).toContain("python_files");
  });

  it("still classifies without a usable traceback", () => {
    const hint = coverageFailureHint(
      "pytest --cov",
      "INTERNALERROR> SystemExit: 1\nno tests ran in 0.4s",
    );
    expect(hint).toContain("sys.exit()");
    // No frame to name, so it must not invent one.
    expect(hint).not.toContain("The module was");
  });

  it("does not fire on an ordinary failing test suite", () => {
    const hint = coverageFailureHint(
      "pytest --cov",
      "FAILED test_api.py::test_login - AssertionError\n1 failed, 3 passed in 2.1s",
    );
    expect(hint ?? "").not.toContain("sys.exit()");
  });

  it("does not fire on an INTERNALERROR that is not a SystemExit", () => {
    const hint = coverageFailureHint(
      "pytest --cov",
      "INTERNALERROR> AttributeError: module has no attribute 'x'",
    );
    expect(hint ?? "").not.toContain("sys.exit()");
  });

  it("is reachable only because stdout is no longer discarded", () => {
    // The whole traceback lives on stdout; stderr carried one line that
    // classifies as nothing. This is the pairing that made the fix work.
    expect(coverageFailureHint("pytest --cov", "mainloop: caught unexpected SystemExit!")).toBeNull();
  });

  it("is stateless across calls despite the global-flagged frame regex", () => {
    // A shared /g pattern carries `lastIndex` between calls, which would make
    // a second call start mid-string and miss.
    const first = coverageFailureHint("pytest --cov", realOutput);
    const second = coverageFailureHint("pytest --cov", realOutput);
    expect(second).toBe(first);
    expect(second).toContain("stress_test.py:944");
  });
});

describe("formatCoverageReport: recovered runs", () => {
  const base = {
    families: [],
    languages: [],
    artifacts: [],
    files: [],
    overall: { lines_found: 100, lines_hit: 50, percentage: 50 },
    truncated: false,
  } as unknown as CoverageReport;

  it("marks the totals when files were excluded to make the run complete", () => {
    // The exclusion must appear in the same breath as the number, like the
    // scan-truncated marker. A percentage over a quietly reduced denominator
    // is the defect this exists to prevent.
    const out = formatCoverageReport(base, "/repo", [
      { command: "pytest --cov", limitation: { kind: "excluded_paths", paths: ["bench/stress_test.py"] } },
    ]);
    const overall = out.split("\n").find((line) => line.includes("50.0%")) ?? "";
    expect(overall).toContain("RECOVERED RUN — 1 file(s) excluded from measurement");
  });

  it("names every excluded file and the command it was excluded for", () => {
    const out = formatCoverageReport(base, "/repo", [
      { command: "pytest --cov", limitation: { kind: "excluded_paths", paths: ["bench/a.py", "bench/b.py"] } },
    ]);
    expect(out).toContain("RECOVERED RUNS");
    expect(out).toContain("- bench/a.py — excluded so `pytest --cov` could run at all");
    expect(out).toContain("- bench/b.py — excluded so `pytest --cov` could run at all");
    const overall = out.split("\n").find((line) => line.includes("50.0%")) ?? "";
    expect(overall).toContain("2 file(s)");
  });

  it("says nothing when no run was recovered", () => {
    const out = formatCoverageReport(base, "/repo");
    expect(out).not.toContain("RECOVERED");
    expect(formatCoverageReport(base, "/repo", [])).toBe(out);
  });

  it("ignores hostile or empty exclusion payloads without inventing a notice", () => {
    const hostile = [
      null,
      undefined,
      "string",
      42,
      { command: "x" },
      { command: "x", limitation: null },
      { command: "x", limitation: { kind: "excluded_paths", paths: [] } },
      { command: "x", limitation: { kind: "excluded_paths", paths: [null, "", "   "] } },
      { command: "x", limitation: { kind: "scoped_to_modules", modules: [] } },
      { command: "x", limitation: { kind: "invented_kind", paths: ["a"] } },
    ] as unknown as CoverageExclusionNotice[];
    const out = formatCoverageReport(base, "/repo", hostile);
    expect(out).not.toContain("RECOVERED");
  });

  it("neutralizes control characters in an excluded path", () => {
    // Paths reach this from a tool's traceback; one must not be able to inject
    // a new heading or a runnable-looking line into the copied report.
    const out = formatCoverageReport(base, "/repo", [
      { command: "pytest", limitation: { kind: "excluded_paths", paths: ["bench/x.py\nOVERALL\n100.0%"] } },
    ]);
    expect(out).toContain("\\n");
    expect(out.split("\n").filter((line) => line === "OVERALL")).toHaveLength(1);
  });
});

describe("formatCoverageReport: runs scoped to Go modules", () => {
  const base = {
    families: [],
    languages: [],
    artifacts: [],
    files: [],
    overall: { lines_found: 100, lines_hit: 50, percentage: 50 },
    truncated: false,
  } as unknown as CoverageReport;

  const scoped = (modules: string[], partial = false): CoverageExclusionNotice[] => [
    { command: "go test ./...", limitation: { kind: "scoped_to_modules", modules, partial } },
  ];

  it("marks the totals as covering only the modules that ran", () => {
    const out = formatCoverageReport(base, "/repo", scoped(["svc", "tool"]));
    const overall = out.split("\n").find((line) => line.includes("50.0%")) ?? "";
    expect(overall).toContain("RECOVERED RUN — measured only 2 module(s)");
  });

  it("names the modules and says the rest is not covered", () => {
    const out = formatCoverageReport(base, "/repo", scoped(["svc", "tool"]));
    expect(out).toContain("measured only these Go modules: svc, tool");
    expect(out).toContain("code outside them is not covered");
  });

  it("discloses a capped module search rather than implying completeness", () => {
    const out = formatCoverageReport(base, "/repo", scoped(["svc"], true));
    const overall = out.split("\n").find((line) => line.includes("50.0%")) ?? "";
    expect(overall).toContain("that list was itself capped");
    expect(out).toContain("the module search hit its bound");
  });

  it("reports both kinds of narrowing in one run", () => {
    const out = formatCoverageReport(base, "/repo", [
      { command: "pytest", limitation: { kind: "excluded_paths", paths: ["a.py"] } },
      ...scoped(["svc"]),
    ]);
    const overall = out.split("\n").find((line) => line.includes("50.0%")) ?? "";
    expect(overall).toContain("1 file(s) excluded from measurement");
    expect(overall).toContain("measured only 1 module(s)");
  });
});

describe("coverageFailureHint: Go workspace root that is not a module", () => {
  /**
   * The exact text go 1.26 prints for the case the Go recovery exists for,
   * captured from a real workspace whose root is not a module. The pattern
   * only matched the non-workspace wording ("main module"), so the one
   * failure GitPulse can act on was classified as unrecognised.
   */
  const workspaceMessage = [
    "# ./...",
    "pattern ./...: directory prefix . does not contain modules listed in go.work or their selected dependencies",
    "FAIL\t./... [setup failed]",
  ].join("\n");

  it("recognizes the go.work wording", () => {
    expect(classifyCoverageFailure("go test ./...", workspaceMessage)).toEqual({
      kind: "go_missing_module",
    });
  });

  it("still recognizes the non-workspace wording", () => {
    const single = "go: directory prefix . does not contain main module or its selected dependencies";
    expect(classifyCoverageFailure("go test ./...", single)).toEqual({
      kind: "go_missing_module",
    });
  });

  it("does not fire on unrelated go output", () => {
    expect(
      classifyCoverageFailure("go test ./...", "ok  \tsvc\t0.2s\tcoverage: 50.0% of statements"),
    ).toBeNull();
  });
});
