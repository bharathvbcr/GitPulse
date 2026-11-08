import { describe, expect, it } from "vitest";
import {
  decomposeTask,
  calculateSubtaskRollup,
  formatSubtaskProgress,
  parseSubtaskLines,
  splitCriteriaIntoSubtasks,
} from "./intake";

describe("intake task decomposition", () => {
  it("decomposes task into subtasks with parent linkage and generated keys", () => {
    const plan = decomposeTask("auth-refactor", "Refactor Authentication Flow", [
      "Extract token parser",
      { title: "Add OAuth callback", description: "Handle PKCE exchange", status: "ready" },
      "Write unit tests",
    ]);

    expect(plan.parentKey).toBe("auth-refactor");
    expect(plan.parentTitle).toBe("Refactor Authentication Flow");
    expect(plan.subtasks).toHaveLength(3);
    expect(plan.subtasks[0]).toEqual({
      key: "auth-refactor-sub-1",
      title: "Extract token parser",
      description: "",
      status: "ready",
      parentKey: "auth-refactor",
    });
    expect(plan.subtasks[1]).toEqual({
      key: "auth-refactor-sub-2",
      title: "Add OAuth callback",
      description: "Handle PKCE exchange",
      status: "ready",
      parentKey: "auth-refactor",
    });
    expect(plan.subtasks[2]).toEqual({
      key: "auth-refactor-sub-3",
      title: "Write unit tests",
      description: "",
      status: "ready",
      parentKey: "auth-refactor",
    });
  });

  it("calculates Linear-style subtask rollup metrics", () => {
    const empty = calculateSubtaskRollup([]);
    expect(empty.total).toBe(0);
    expect(empty.isAllComplete).toBe(false);
    expect(formatSubtaskProgress(empty)).toBe("No subtasks");

    const singleDone = calculateSubtaskRollup(["completed"]);
    expect(formatSubtaskProgress(singleDone)).toBe("All 1 subtask done");

    const partial = calculateSubtaskRollup(["done", "in_progress", "ready", "closed"]);
    expect(partial.total).toBe(4);
    expect(partial.completed).toBe(2);
    expect(partial.inProgress).toBe(1);
    expect(partial.pending).toBe(1);
    expect(partial.percentComplete).toBe(50);
    expect(partial.isAllComplete).toBe(false);
    expect(formatSubtaskProgress(partial)).toBe("2 of 4 subtasks done (50%)");

    const allDone = calculateSubtaskRollup(["done", "done", "done"]);
    expect(allDone.total).toBe(3);
    expect(allDone.completed).toBe(3);
    expect(allDone.percentComplete).toBe(100);
    expect(allDone.isAllComplete).toBe(true);
    expect(formatSubtaskProgress(allDone)).toBe("All 3 subtasks done");
  });

  it("parses multi-line subtask text stripping bullets, checkboxes, numbers, duplicates, and placeholders", () => {
    const raw = [
      "  - [ ] Wire onopenTask in AgentsView",
      "* [x] Add batch withdraw toolbar",
      "1. Launch real Claude session",
      "2) Launch real Antigravity session",
      "wire ONOPENTASK in AgentsView",
      "None.",
      "N/A",
      "   ",
    ].join("\n");

    expect(parseSubtaskLines(raw)).toEqual([
      "Wire onopenTask in AgentsView",
      "Add batch withdraw toolbar",
      "Launch real Claude session",
      "Launch real Antigravity session",
    ]);

    const longLine = "a".repeat(350);
    const capped = parseSubtaskLines(longLine, 2);
    expect(capped).toHaveLength(1);
    expect([...capped[0]].length).toBe(300);
  });

  it("splits acceptance criteria into non-duplicate subtask titles", () => {
    const criteria = [
      "- Verify Claude seat isolation",
      "2. Propagate .claude-account to linked worktrees",
      "Add Graft Dashboard metric card",
    ];
    const existing = ["verify claude seat isolation"];
    expect(splitCriteriaIntoSubtasks(criteria, existing)).toEqual([
      "Propagate .claude-account to linked worktrees",
      "Add Graft Dashboard metric card",
    ]);
  });
});

