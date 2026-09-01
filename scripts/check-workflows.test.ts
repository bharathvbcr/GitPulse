import { describe, expect, it } from "vitest";
import { INSTALL_HINT, interpret, workflowFiles } from "./check-workflows.mjs";

describe("check:workflows", () => {
  it("finds the repository's workflow files", () => {
    const files = workflowFiles(new URL("../.github/workflows", import.meta.url).pathname);
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
});
