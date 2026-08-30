import { describe, expect, it } from "vitest";
import { formatArgv, planCoverageRecovery, repoRelativePath } from "./recovery";

const REPO = "/Users/me/Code/Manvi";
const PYTEST = [".venv/bin/python", "-m", "pytest", "--cov", "--cov-report=xml"];

/** The real Manvi failure, trimmed to the frames that carry the diagnosis. */
function collectionAbort(modulePath = `${REPO}/bench/stress_test.py`): string {
  return [
    "INTERNALERROR> Traceback (most recent call last):",
    'INTERNALERROR>   File "/Users/me/Code/Manvi/.venv/lib/python3.14/site-packages/_pytest/pathlib.py", line 596, in import_path',
    'INTERNALERROR>   File "<frozen importlib._bootstrap>", line 1406, in _gcd_import',
    `INTERNALERROR>   File "${modulePath}", line 944, in <module>`,
    "INTERNALERROR>     sys.exit(1 if FAIL else 0)",
    "INTERNALERROR> SystemExit: 0",
    "============================ no tests ran in 17.30s ============================",
  ].join("\n");
}

describe("repoRelativePath", () => {
  it("relativizes a path inside the repository", () => {
    expect(repoRelativePath(REPO, `${REPO}/bench/stress_test.py`)).toBe("bench/stress_test.py");
  });

  it("accepts a path that is already relative", () => {
    expect(repoRelativePath(REPO, "bench/stress_test.py")).toBe("bench/stress_test.py");
    expect(repoRelativePath(REPO, "./bench/stress_test.py")).toBe("bench/stress_test.py");
  });

  it("tolerates a trailing slash on the repository path", () => {
    expect(repoRelativePath(`${REPO}/`, `${REPO}/bench/x.py`)).toBe("bench/x.py");
  });

  it("refuses a path outside the repository", () => {
    // A traceback frame can point anywhere; only repository files may become
    // arguments. The backend gate refuses these too.
    expect(repoRelativePath(REPO, "/etc/passwd")).toBeNull();
    expect(repoRelativePath(REPO, "/Users/me/Code/Other/x.py")).toBeNull();
  });

  it("refuses a sibling directory that merely shares the prefix", () => {
    expect(repoRelativePath(REPO, "/Users/me/Code/Manvi-fork/x.py")).toBeNull();
  });

  it("refuses a path that escapes upward", () => {
    expect(repoRelativePath(REPO, "../outside.py")).toBeNull();
    expect(repoRelativePath(REPO, "bench/../../outside.py")).toBeNull();
  });

  it("refuses empty input", () => {
    expect(repoRelativePath(REPO, "")).toBeNull();
    expect(repoRelativePath("", "x.py")).toBeNull();
  });
});

describe("formatArgv", () => {
  it("leaves ordinary arguments alone", () => {
    expect(formatArgv(["python", "-m", "pytest", "--cov"])).toBe("python -m pytest --cov");
  });

  it("quotes an argument containing a space", () => {
    expect(formatArgv(["pytest", "--ignore=my tests/x.py"])).toBe("pytest '--ignore=my tests/x.py'");
  });

  it("escapes an embedded single quote", () => {
    expect(formatArgv(["pytest", "--ignore=it's.py"])).toBe("pytest '--ignore=it'\\''s.py'");
  });
});

describe("planCoverageRecovery: pytest collection abort", () => {
  it("plans a retry that excludes the aborting module", () => {
    const plan = planCoverageRecovery(PYTEST, collectionAbort(), { repoPath: REPO });
    expect(plan).not.toBeNull();
    expect(plan!.steps[0].argv).toEqual([...PYTEST, "--ignore=bench/stress_test.py"]);
    expect(plan!.limitation).toEqual({ kind: "excluded_paths", paths: ["bench/stress_test.py"] });
  });

  it("states what the retry gave up", () => {
    // The note travels with the result everywhere it is shown; a recovered
    // percentage that does not say what it excluded is a wrong number.
    const plan = planCoverageRecovery(PYTEST, collectionAbort(), { repoPath: REPO });
    expect(plan!.note).toContain("bench/stress_test.py");
    expect(plan!.note).toContain("excludes");
  });

  it("keeps a path with a space as a single argument", () => {
    const plan = planCoverageRecovery(PYTEST, collectionAbort(`${REPO}/my bench/stress_test.py`), { repoPath: REPO });
    expect(plan!.steps[0].argv.at(-1)).toBe("--ignore=my bench/stress_test.py");
    // Display quoting must not leak back into execution.
    expect(plan!.steps[0].command).toContain("'--ignore=my bench/stress_test.py'");
  });

  it("refuses when the aborting module is outside the repository", () => {
    const outside = collectionAbort("/Users/me/elsewhere/stress_test.py");
    expect(planCoverageRecovery(PYTEST, outside, { repoPath: REPO })).toBeNull();
  });

  it("refuses when the traceback names no repository module", () => {
    // Only site-packages and frozen frames — nothing GitPulse may exclude.
    const detail = [
      "INTERNALERROR> Traceback (most recent call last):",
      'INTERNALERROR>   File "/Users/me/Code/Manvi/.venv/lib/python3.14/site-packages/_pytest/x.py", line 1, in <module>',
      "INTERNALERROR> SystemExit: 0",
    ].join("\n");
    expect(planCoverageRecovery(PYTEST, detail, { repoPath: REPO })).toBeNull();
  });

  it("refuses to retry a command that already ignores that path", () => {
    // The bound that makes this terminate: the same exclusion twice cannot
    // change the outcome.
    const argv = [...PYTEST, "--ignore=bench/stress_test.py"];
    expect(planCoverageRecovery(argv, collectionAbort(), { repoPath: REPO })).toBeNull();
    const spaced = [...PYTEST, "--ignore", "bench/stress_test.py"];
    expect(planCoverageRecovery(spaced, collectionAbort(), { repoPath: REPO })).toBeNull();
  });

  it("refuses when the failing command does not run pytest", () => {
    // `--ignore=` is a pytest flag. Appending it to another runner produces a
    // command that fails differently, which is worse than not retrying.
    expect(planCoverageRecovery(["go", "test", "./..."], collectionAbort(), { repoPath: REPO })).toBeNull();
  });
});

describe("planCoverageRecovery: causes that must not be retried", () => {
  const cases: [string, string][] = [
    ["a failing test suite", "FAILED tests/test_math.py::test_add - assert 1 == 2"],
    ["a missing generator", "Failed to spawn pytest"],
    ["a refused npx install", "npx canceled due to missing packages and no YES option"],
    ["a missing gradle wrapper", "wrapper './gradlew' is not a repository file"],
    ["an allowlist refusal", "outside the purpose-specific command allowlist"],
  ];

  for (const [name, detail] of cases) {
    it(`refuses to retry ${name}`, () => {
      expect(planCoverageRecovery(PYTEST, detail, { repoPath: REPO })).toBeNull();
    });
  }

  it("refuses when there is no output to classify", () => {
    expect(planCoverageRecovery(PYTEST, "", { repoPath: REPO })).toBeNull();
    expect(planCoverageRecovery(PYTEST, null, { repoPath: REPO })).toBeNull();
    expect(planCoverageRecovery(PYTEST, undefined, { repoPath: REPO })).toBeNull();
  });
});

describe("planCoverageRecovery: hostile input", () => {
  it("never throws and never invents a command", () => {
    const hostile: unknown[] = [
      null,
      undefined,
      [],
      "not an array",
      [""],
      ["\0"],
      ["pytest", "‮--ignore=x"],
      Array.from({ length: 500 }, () => "pytest"),
    ];
    for (const argv of hostile) {
      expect(() =>
        planCoverageRecovery(argv as string[], collectionAbort(), { repoPath: REPO }),
      ).not.toThrow();
    }
    expect(planCoverageRecovery([], collectionAbort(), { repoPath: REPO })).toBeNull();
    expect(planCoverageRecovery(PYTEST, collectionAbort(), { repoPath: "" })).toBeNull();
  });

  it("does not accept a repository path that is not a string", () => {
    expect(planCoverageRecovery(PYTEST, collectionAbort(), null as unknown as { repoPath: string })).toBeNull();
  });
});

describe("planCoverageRecovery: Go module-root failure", () => {
  const GO = ["go", "test", "./...", "-coverprofile=coverage.out"];
  const missingModule =
    "go: directory prefix . does not contain main module or its selected dependencies";
  const ctx = (modules: string[], partial = false) => ({
    repoPath: REPO,
    goModules: modules,
    goModulesPartial: partial,
  });

  it("plans one command per discovered module", () => {
    // Go coverage is cumulative: every module has to run for the totals to
    // mean the repository.
    const plan = planCoverageRecovery(GO, missingModule, ctx(["svc", "tool"]));
    expect(plan).not.toBeNull();
    expect(plan!.mode).toBe("all");
    expect(plan!.steps.map((step) => step.argv)).toEqual([
      ["go", "-C", "svc", "test", "./...", "-coverprofile=coverage.out"],
      ["go", "-C", "tool", "test", "./...", "-coverprofile=coverage.out"],
    ]);
  });

  it("puts -C before the subcommand, where go requires it", () => {
    const plan = planCoverageRecovery(GO, missingModule, ctx(["svc"]));
    expect(plan!.steps[0].command).toBe("go -C svc test ./... -coverprofile=coverage.out");
  });

  it("records that only those modules were measured", () => {
    const plan = planCoverageRecovery(GO, missingModule, ctx(["svc", "tool"]));
    expect(plan!.limitation).toEqual({
      kind: "scoped_to_modules",
      modules: ["svc", "tool"],
      partial: false,
    });
    expect(plan!.note).toContain("only those modules");
  });

  it("says so when the module list itself was capped", () => {
    // A capped list means there may be modules this run never measured, and
    // a scoped total that hides that reads as more complete than it is.
    const plan = planCoverageRecovery(GO, missingModule, ctx(["a", "b"], true));
    expect(plan!.limitation).toEqual({
      kind: "scoped_to_modules",
      modules: ["a", "b"],
      partial: true,
    });
    expect(plan!.note).toContain("capped");
  });

  it("refuses when the scan discovered no modules", () => {
    // Nothing to retry with; inventing a directory would reproduce the same
    // failure with a different message.
    expect(planCoverageRecovery(GO, missingModule, ctx([]))).toBeNull();
    expect(planCoverageRecovery(GO, missingModule, { repoPath: REPO })).toBeNull();
  });

  it("drops the repository root from the module list", () => {
    // The root is where the failing command already ran.
    const plan = planCoverageRecovery(GO, missingModule, ctx(["", "svc"]));
    expect(plan!.limitation).toEqual({
      kind: "scoped_to_modules",
      modules: ["svc"],
      partial: false,
    });
  });

  it("refuses module directories that leave the repository", () => {
    expect(planCoverageRecovery(GO, missingModule, ctx(["/etc", "../up"]))).toBeNull();
    const plan = planCoverageRecovery(GO, missingModule, ctx(["/etc", "svc"]));
    expect(plan!.steps).toHaveLength(1);
  });

  it("does not retry a command that already named a directory", () => {
    // `go -C x` failed for x specifically. Retrying the same scope cannot
    // terminate, and retrying a different one is a new plan, not a retry.
    const scoped = ["go", "-C", "svc", "test", "./...", "-coverprofile=coverage.out"];
    expect(planCoverageRecovery(scoped, missingModule, ctx(["tool"]))).toBeNull();
  });

  it("refuses when the failing command is not go", () => {
    expect(planCoverageRecovery(PYTEST, missingModule, ctx(["svc"]))).toBeNull();
  });

  it("survives a hostile module list without inventing a command", () => {
    const hostile = [null, 42, {}, "", "   ", "svc"] as unknown as string[];
    const plan = planCoverageRecovery(GO, missingModule, {
      repoPath: REPO,
      goModules: hostile,
    });
    expect(plan!.limitation).toEqual({
      kind: "scoped_to_modules",
      modules: ["svc"],
      partial: false,
    });
    expect(() =>
      planCoverageRecovery(GO, missingModule, {
        repoPath: REPO,
        goModules: "not an array" as unknown as string[],
      }),
    ).not.toThrow();
  });
});

describe("recovery command shapes (backend contract)", () => {
  /**
   * These exact shapes are asserted runnable against the MANVI gate by
   * `recovery_commands_are_runnable` in src-tauri/src/analyzer/coverage.rs.
   * Both halves have to move together: a recovery the backend refuses is an
   * offer that cannot run.
   */
  it("builds the Go module command the gate was shown", () => {
    const plan = planCoverageRecovery(
      ["go", "test", "./...", "-coverprofile=coverage.out"],
      "go: directory prefix . does not contain main module",
      { repoPath: REPO, goModules: ["svc"] },
    );
    expect(plan!.steps[0].argv).toEqual([
      "go",
      "-C",
      "svc",
      "test",
      "./...",
      "-coverprofile=coverage.out",
    ]);
  });

  it("builds the pytest exclusion command the gate was shown", () => {
    const plan = planCoverageRecovery(
      ["pytest", "--cov", "--cov-report=xml"],
      collectionAbort(),
      { repoPath: REPO },
    );
    expect(plan!.steps[0].argv).toEqual([
      "pytest",
      "--cov",
      "--cov-report=xml",
      "--ignore=bench/stress_test.py",
    ]);
  });
});
