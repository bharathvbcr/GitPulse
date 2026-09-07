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
    for (const status of [
      "in_progress",
      "queued",
      "pending",
      "waiting",
      "requested",
      "QUEUED",
    ]) {
      expect(canCancelRun({ status }), status).toBe(true);
    }
    for (const status of ["completed", "cancelled", "failure", ""]) {
      expect(canCancelRun({ status }), status).toBe(false);
    }
  });

  it("covers deployment-protection states that used to render as dead rows", () => {
    // Regression: `waiting` (deployment protection rules) and `requested`
    // (awaiting approval) runs got neither a cancel nor a rerun affordance —
    // an unactionable row that could sit for hours.
    expect(canCancelRun({ status: "waiting" })).toBe(true);
    expect(canCancelRun({ status: "requested" })).toBe(true);
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
    expect(ciLocalVerdict({ passed: 2, failed: 1, skipped: 3 })).toBe(
      "full suite failed (1 step)",
    );
    expect(ciLocalVerdict({ passed: 1, failed: 2, skipped: 0 })).toBe(
      "full suite failed (2 steps)",
    );
    expect(ciLocalVerdict({ passed: 5, failed: 0, skipped: 1 })).toBe(
      "full suite passed with 1 skipped",
    );
    expect(ciLocalVerdict({ passed: 6, failed: 0, skipped: 0 })).toBe(
      "full suite passed (6 steps)",
    );
  });

  it("never badges affected tests from a fail-closed full suite", () => {
    expect(
      ciLocalVerdict({
        passed: 3,
        failed: 0,
        skipped: 0,
        test_scope: { mode: "full_suite", fail_closed: true },
      }),
    ).toBe("full suite passed (3 steps)");
    expect(
      ciLocalVerdict({
        passed: 2,
        failed: 0,
        skipped: 0,
        test_scope: { mode: "affected", fail_closed: false },
      }),
    ).toBe("affected tests passed (2 steps)");
    // Fail-closed with mode still saying affected must not claim the badge.
    expect(
      ciLocalVerdict({
        passed: 2,
        failed: 0,
        skipped: 0,
        test_scope: { mode: "affected", fail_closed: true },
      }),
    ).toBe("full suite passed (2 steps)");
  });
});

describe("ciStepClass", () => {
  it("colors known statuses and mutes unknown ones", () => {
    expect(ciStepClass("passed")).toContain("green");
    expect(ciStepClass("failed")).toContain("red");
    // Both shades: a bare `-400` is unreadable on the light theme's card.
    expect(ciStepClass("passed")).toContain("dark:text-green-400");
    expect(ciStepClass("failed")).toContain("dark:text-red-400");
    expect(ciStepClass("passed")).not.toBe("text-green-400");
    expect(ciStepClass("skipped")).toContain("textMuted");
    expect(ciStepClass("whatever")).toContain("textMuted");
  });
});
