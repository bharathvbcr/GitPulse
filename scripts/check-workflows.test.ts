import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
  INSTALL_HINT,
  hasWorkflowLevelPermissions,
  interpret,
  workflowFiles,
  workflowsMissingPermissions,
} from "./check-workflows.mjs";

describe("check:workflows", () => {
  it("finds the repository's workflow files", () => {
    // `fileURLToPath`, never `URL.pathname`: on Windows the latter yields
    // "/D:/a/..." and every fs call against it misses or doubles the drive.
    const files = workflowFiles(fileURLToPath(new URL("../.github/workflows", import.meta.url)));
    expect(files).toContain("ci.yml");
    expect(files).toContain("coverage.yml");
    expect(files).toContain("release.yml");
  });

  it("reports a missing directory as empty rather than throwing", () => {
    expect(workflowFiles("/nonexistent/workflows")).toEqual([]);
  });

  it("separates 'actionlint is absent' from 'workflows are faulty'", () => {
    const enoent = Object.assign(new Error("spawn actionlint ENOENT"), { code: "ENOENT" });
    // A checker that could not run must never look like a checker that passed,
    // and must be distinguishable from one that ran and found problems.
    expect(interpret({ status: null, error: enoent })).toEqual({ code: 2, message: INSTALL_HINT });
    expect(interpret({ status: 1 }).code).toBe(1);
    expect(interpret({ status: 0 })).toEqual({ code: 0, message: null });
  });

  it("treats actionlint's own failure codes as a check that could not run", () => {
    // actionlint exits 2 for bad options and 3 for a fatal error; neither is a
    // verdict about the workflows.
    expect(interpret({ status: 2 }).code).toBe(2);
    expect(interpret({ status: 3 }).code).toBe(2);
  });

  it("requires a workflow-level permissions block so GITHUB_TOKEN is fail-closed", () => {
    expect(hasWorkflowLevelPermissions("on: push\njobs:\n  a:\n    runs-on: ubuntu-latest\n")).toBe(
      false,
    );
    expect(
      hasWorkflowLevelPermissions(
        "on: push\npermissions:\n  contents: read\njobs:\n  a:\n    runs-on: ubuntu-latest\n",
      ),
    ).toBe(true);
    // A job-only map is not enough: GitHub's default token still applies to
    // every other job, which is the finding this check exists to keep closed.
    expect(
      hasWorkflowLevelPermissions("on: push\njobs:\n  a:\n    permissions:\n      contents: read\n"),
    ).toBe(false);

    const dir = fileURLToPath(new URL("../.github/workflows", import.meta.url));
    expect(workflowsMissingPermissions(dir)).toEqual([]);
  });

  it("prints the rust coverage summary without rerunning the tests", () => {
    const source = readFileSync(
      fileURLToPath(new URL("../.github/workflows/coverage.yml", import.meta.url)),
      "utf8",
    );
    const summary = source
      .split("\n")
      .find((line) => line.includes("--summary-only"));
    expect(summary, source).toBeDefined();
    expect(summary).toContain("--no-run");
  });

  it("caps llvm-cov test threads so instrumented suites do not starve pipe and sidecar fixtures", () => {
    const coverage = readFileSync(
      fileURLToPath(new URL("../.github/workflows/coverage.yml", import.meta.url)),
      "utf8",
    );
    const lcov = coverage
      .split("\n")
      .find((line) => line.includes("--lcov") && line.includes("output-path lcov.info"));
    expect(lcov, coverage).toBeDefined();
    expect(lcov).toContain("--test-threads=1");
    expect(lcov).not.toContain("--no-run");
    const pkg = JSON.parse(
      readFileSync(fileURLToPath(new URL("../package.json", import.meta.url)), "utf8"),
    );
    expect(pkg.scripts["ci:local"]).toContain("--test-threads=1");
  });

  it("caps uninstrumented cargo test threads so sidecar hello fixtures are not starved", () => {
    for (const name of ["ci.yml", "release.yml"]) {
      const source = readFileSync(
        fileURLToPath(new URL(`../.github/workflows/${name}`, import.meta.url)),
        "utf8",
      );
      const lines = source.split("\n").filter((line) => line.includes("cargo test --manifest-path"));
      expect(lines.length, name).toBeGreaterThan(0);
      for (const line of lines) {
        expect(line, name).toContain("--test-threads=1");
      }
    }
  });

  it("scopes rust-cache to the runner image so native artifacts are not reused across MSVC upgrades", () => {
    for (const name of ["ci.yml", "coverage.yml", "release.yml"]) {
      const source = readFileSync(
        fileURLToPath(new URL(`../.github/workflows/${name}`, import.meta.url)),
        "utf8",
      );
      const caches = source.split("uses: Swatinem/rust-cache@v2");
      expect(caches.length, name).toBeGreaterThan(1);
      for (const block of caches.slice(1)) {
        // ImageOS is a runner process env var, not a workflow `env` key, so
        // interpolating it here is always the empty string.
        expect(block, name).not.toMatch(/prefix-key:\s*\$\{\{\s*env\.ImageOS\s*\}\}/);
        expect(block, name).toContain("prefix-key: ${{ env.RUST_CACHE_PREFIX }}");
      }
      const exports = source.split("Key the Rust cache to this runner image");
      expect(exports.length, name).toBe(caches.length);
      for (const block of exports.slice(1)) {
        expect(block, name).toContain('RUST_CACHE_PREFIX=${ImageOS:-unknown}-${ImageVersion:-unknown}');
        expect(block, name).toContain("$GITHUB_ENV");
      }
    }
  });
});
