import { describe, expect, it } from "vitest";
import {
  COMPACT_ROW_THRESHOLD,
  RUN_PREVIEW_COUNT,
  WORKFLOW_PREVIEW_COUNT,
  canCancelRun,
  canRerunRun,
  ciLocalVerdict,
  ciStepClass,
  expandLabel,
  isWorkflowDispatchable,
  overflowsPreview,
  previewSlice,
  useCompactRows,
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

describe("previewSlice", () => {
  const list = (n: number) => Array.from({ length: n }, (_, i) => `row-${i}`);

  it("caps the collapsed list at the preview count and expands to all", () => {
    expect(previewSlice(list(50), false, WORKFLOW_PREVIEW_COUNT)).toHaveLength(
      WORKFLOW_PREVIEW_COUNT,
    );
    expect(previewSlice(list(50), true, WORKFLOW_PREVIEW_COUNT)).toHaveLength(50);
    expect(previewSlice(list(20), false, RUN_PREVIEW_COUNT)).toHaveLength(
      RUN_PREVIEW_COUNT,
    );
  });

  it("keeps the first rows, so expanding only ever appends", () => {
    // A collapsed preview that showed a different slice than the head of the
    // expanded list would make the button look like it reordered the section.
    const all = list(12);
    const collapsed = previewSlice(all, false, WORKFLOW_PREVIEW_COUNT);
    expect(collapsed).toEqual(all.slice(0, collapsed.length));
    expect(previewSlice(all, true, WORKFLOW_PREVIEW_COUNT).slice(0, collapsed.length)).toEqual(
      collapsed,
    );
  });

  it("never renders an empty list for a repository that has rows", () => {
    // The class of bug this guards: a default collapse that renders zero rows
    // is indistinguishable from "no Actions workflows", so the section lies
    // about the repository rather than merely hiding part of it.
    for (const n of [1, 2, 3, 4, 5, 6, 20, 50]) {
      for (const count of [WORKFLOW_PREVIEW_COUNT, RUN_PREVIEW_COUNT, 0, -1]) {
        expect(previewSlice(list(n), false, count).length).toBeGreaterThan(0);
        expect(previewSlice(list(n), true, count).length).toBeGreaterThan(0);
      }
    }
  });

  it("shows every row, collapsed or not, when there are few", () => {
    const all = list(WORKFLOW_PREVIEW_COUNT);
    expect(previewSlice(all, false, WORKFLOW_PREVIEW_COUNT)).toEqual(all);
  });

  it("leaves an empty list empty rather than inventing a row", () => {
    expect(previewSlice([], false, WORKFLOW_PREVIEW_COUNT)).toEqual([]);
    expect(previewSlice([], true, WORKFLOW_PREVIEW_COUNT)).toEqual([]);
  });

  it("does not alias the caller's array", () => {
    const all = list(3);
    const expanded = previewSlice(all, true, WORKFLOW_PREVIEW_COUNT);
    expanded.push("mutated");
    expect(all).toHaveLength(3);
  });
});

describe("overflowsPreview", () => {
  it("offers the expander only when rows are actually hidden", () => {
    // Off-by-one here renders a "Show all 5 workflows" button that changes
    // nothing when clicked.
    expect(overflowsPreview(WORKFLOW_PREVIEW_COUNT, WORKFLOW_PREVIEW_COUNT)).toBe(false);
    expect(overflowsPreview(WORKFLOW_PREVIEW_COUNT + 1, WORKFLOW_PREVIEW_COUNT)).toBe(true);
    expect(overflowsPreview(0, WORKFLOW_PREVIEW_COUNT)).toBe(false);
  });

  it("agrees with previewSlice about whether anything is hidden", () => {
    for (const count of [WORKFLOW_PREVIEW_COUNT, RUN_PREVIEW_COUNT]) {
      for (let n = 0; n <= 25; n += 1) {
        const hidden = n - previewSlice(Array.from({ length: n }), false, count).length;
        expect(overflowsPreview(n, count)).toBe(hidden > 0);
      }
    }
  });
});

describe("expandLabel", () => {
  it("names the count to reveal, and offers the way back once expanded", () => {
    expect(expandLabel(23, false, "workflows")).toBe("Show all 23 workflows");
    expect(expandLabel(20, false, "runs")).toBe("Show all 20 runs");
    expect(expandLabel(23, true, "workflows")).toBe("Show fewer");
  });

  it("groups large counts the way the rest of the panel does", () => {
    expect(expandLabel(1234, false, "workflows")).toBe("Show all 1,234 workflows");
  });
});

describe("useCompactRows", () => {
  it("tightens rows only once enough of them are on screen", () => {
    expect(useCompactRows(COMPACT_ROW_THRESHOLD - 1)).toBe(false);
    expect(useCompactRows(COMPACT_ROW_THRESHOLD)).toBe(true);
    expect(useCompactRows(0)).toBe(false);
  });

  it("leaves every collapsed preview comfortable", () => {
    // The previews are short by construction, so neither should ever trip the
    // compact threshold — otherwise every repository gets the dense row.
    expect(useCompactRows(WORKFLOW_PREVIEW_COUNT)).toBe(false);
    expect(useCompactRows(RUN_PREVIEW_COUNT)).toBe(false);
  });
});
