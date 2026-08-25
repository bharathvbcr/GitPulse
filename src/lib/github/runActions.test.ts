import { describe, expect, it } from "vitest";
import {
  canCancelRun,
  canRerunRun,
  ciLocalVerdict,
  ciStepClass,
  isWorkflowDispatchable,
  workflowStateLabel,
} from "./runActions";

describe("canRerunRun", () => {
  it("allows only completed runs", () => {
    expect(canRerunRun({ status: "completed" })).toBe(true);
    expect(canRerunRun({ status: "COMPLETED" })).toBe(true);
    expect(canRerunRun({ status: "in_progress" })).toBe(false);
    expect(canRerunRun({ status: "queued" })).toBe(false);
    expect(canRerunRun({ status: "" })).toBe(false);
  });
});

describe("canCancelRun", () => {
  it("allows every in-flight state and refuses finished ones", () => {
    for (const status of ["in_progress", "queued", "pending", "QUEUED"]) {
      expect(canCancelRun({ status }), status).toBe(true);
    }
    for (const status of ["completed", "cancelled", "failure", ""]) {
      expect(canCancelRun({ status }), status).toBe(false);
    }
  });
});

describe("workflowStateLabel", () => {
  it("maps gh states to UI labels and passes unknowns through", () => {
    expect(workflowStateLabel("active")).toBe("active");
    expect(workflowStateLabel("disabled_manually")).toBe("disabled");
    expect(workflowStateLabel("disabled_inactivity")).toBe("inactive");
    expect(workflowStateLabel("deleted_foo")).toBe("deleted_foo");
  });
});

describe("isWorkflowDispatchable", () => {
  it("accepts exactly the active state", () => {
    expect(isWorkflowDispatchable("active")).toBe(true);
    expect(isWorkflowDispatchable("disabled_manually")).toBe(false);
    expect(isWorkflowDispatchable("")).toBe(false);
  });
});

describe("ciLocalVerdict", () => {
  it("fails loudly first, then reports skips, then plain passes", () => {
    expect(ciLocalVerdict({ passed: 2, failed: 1, skipped: 3 })).toBe("failed (1 step)");
    expect(ciLocalVerdict({ passed: 1, failed: 2, skipped: 0 })).toBe("failed (2 steps)");
    expect(ciLocalVerdict({ passed: 5, failed: 0, skipped: 1 })).toBe("passed with 1 skipped");
    expect(ciLocalVerdict({ passed: 6, failed: 0, skipped: 0 })).toBe("passed (6 steps)");
  });
});

describe("ciStepClass", () => {
  it("colors known statuses and mutes unknown ones", () => {
    expect(ciStepClass("passed")).toContain("green");
    expect(ciStepClass("failed")).toContain("red");
    expect(ciStepClass("skipped")).toContain("textMuted");
    expect(ciStepClass("whatever")).toContain("textMuted");
  });
});
