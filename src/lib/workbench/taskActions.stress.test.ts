import { describe, expect, it } from "vitest";
import { WorkbenchError } from "./client";
import {
  deleteConfirmCopy,
  deleteTasks,
  displayTitle,
  MAX_DELETE_BATCH,
  removeFromColumns,
  uniqueDeletable,
} from "./taskDelete";
import { contextMenuAnchor, duplicateTitle, flattenVisibleIds, rangeSelect, taskMenuItems, toggleSelection } from "./taskMenu";
import { canStartEnhanceFromDraft, cardMatchesFacet, collectFacetOptions, dueInputValue, dueState, emptyFacet, hiddenTaskDetails, parseDueInput } from "./taskOrganize";
import type { Task, TaskCard, TaskStatus } from "./client";

function card(i: number): TaskCard {
  return {
    id: `t${i}`, revision: i + 1, updated_at: 1, title: `T${i}`, kind: i % 2 ? "bug" : "feature",
    status: "inbox", priority: i % 4, severity: null, owner: i % 5 === 0 ? "Pat" : null,
    due_at: i % 7 === 0 ? 1 : null, labels: i % 3 === 0 ? ["ui"] : [],
    repository_ids: ["r"], primary_repository_id: "r", home_workspace_id: null, position: i,
  };
}

describe("adversarial stress", () => {
  it("uniqueDeletable stays bounded on a large hostile mix", () => {
    const mix = Array.from({ length: 10_000 }, (_, i) => {
      if (i % 17 === 0) return { id: `bad ${i}`, revision: 1, title: "x" };
      if (i % 19 === 0) return { id: `t${i % 50}`, revision: Number.NaN, title: "x" };
      if (i % 23 === 0) return null;
      return { id: `t${i % 80}`, revision: 1 + (i % 9), title: `n${i}` };
    });
    const unique = uniqueDeletable(mix);
    expect(unique.length).toBeLessThanOrEqual(80);
    expect(new Set(unique.map((row) => row.id)).size).toBe(unique.length);
  });

  it("deleteTasks never exceeds the batch cap even when every call succeeds", async () => {
    const tasks = Array.from({ length: MAX_DELETE_BATCH + 25 }, (_, i) => ({
      id: `t${i}`, revision: 1, title: `T${i}`,
    }));
    const result = await deleteTasks(tasks, async () => undefined, { newID: () => crypto.randomUUID() });
    expect(result.deleted).toHaveLength(MAX_DELETE_BATCH);
    expect(result.skipped).toBe(25);
    expect(result.deleted.length + result.skipped).toBe(tasks.length);
  });

  it("retries do not mint a second request identity under transport loss", async () => {
    const ids: string[] = [];
    await deleteTasks([{ id: "t1", revision: 1, title: "A" }], async (attempt) => {
      ids.push(attempt.request_id);
      if (ids.length < 2) throw new WorkbenchError("transport_error", "lost");
    }, { newID: () => "same" });
    expect(ids).toEqual(["same", "same"]);
  });

  it("confirm copy cannot be inflated by control characters or huge titles", () => {
    const copy = deleteConfirmCopy([{
      id: "t1",
      revision: 1,
      title: `line1\n${"A".repeat(5000)}\u0000<script>x</script>`,
    }]);
    expect(copy.message).not.toContain("\nA".repeat(10));
    expect(copy.title.length).toBeLessThan(200);
    expect(copy.message).not.toContain("\u0000");
  });

  it("removeFromColumns on an empty and a full board never goes negative", () => {
    const statuses = ["inbox", "backlog", "ready", "in_progress", "review", "done"] as TaskStatus[];
    const columns = Object.fromEntries(statuses.map((status) => [status, {
      items: Array.from({ length: 30 }, (_, i) => card(i)).map((c) => ({ ...c, status })),
      total: 30, shown: 30, has_more: false, next_cursor: null,
    }]));
    const ids = new Set(Array.from({ length: 30 }, (_, i) => `t${i}`));
    const next = removeFromColumns(columns, ids);
    for (const status of statuses) {
      expect(next[status]?.items).toEqual([]);
      expect(next[status]?.total).toBe(0);
    }
    expect(removeFromColumns({}, ids)).toEqual({});
  });

  it("menu construction stays finite for a 200-card selection", () => {
    const cards = Array.from({ length: 200 }, (_, i) => card(i));
    const items = taskMenuItems({ cards, column: "inbox", busy: false });
    expect(items.length).toBeGreaterThan(3);
    expect(items.length).toBeLessThan(40);
    expect(items.filter((item) => item.action.kind === "delete")).toHaveLength(1);
    expect(items.filter((item) => item.action.kind === "submenu")).toHaveLength(3);
  });

  it("range select and toggle never throw on garbage", () => {
    expect(() => rangeSelect(["a", "b"], "a", "b")).not.toThrow();
    expect(() => toggleSelection(new Set(["a"]), "b")).not.toThrow();
    expect(flattenVisibleIds({}, ["inbox"], () => true)).toEqual([]);
    expect(contextMenuAnchor({ clientX: Number.NEGATIVE_INFINITY, clientY: undefined }, {
      left: Number.NaN, top: 0, width: -4, height: 10,
    }).x).toBeGreaterThanOrEqual(0);
  });

  it("facet matching over thousands of cards stays a boolean, never throws", () => {
    const cards = Array.from({ length: 3000 }, (_, i) => card(i));
    const facet = { ...emptyFacet(), label: "ui", due: "overdue" as const };
    let hits = 0;
    for (const row of cards) if (cardMatchesFacet(row, facet, 10_000)) hits += 1;
    expect(hits).toBeGreaterThan(0);
    expect(collectFacetOptions(cards).labels).toEqual(["ui"]);
    expect(dueState(Number.POSITIVE_INFINITY, 1)).toBe("none");
  });

  it("due parsing and duplicate titles stay bounded on hostile input", () => {
    expect(parseDueInput("x".repeat(10_000))).toBeNull();
    expect(parseDueInput(Number.POSITIVE_INFINITY)).toBeNull();
    expect(dueInputValue(Number.NaN)).toBe("");
    expect(duplicateTitle("\u0000".repeat(400)).length).toBeLessThanOrEqual(300);
    expect(canStartEnhanceFromDraft({ title: "A".repeat(10_000), repository_ids: ["r"] })).toBeNull();
  });

  it("hidden details survive empty, huge and binary-looking fields", () => {
    const task: Task = {
      ...card(0),
      description: `${"x".repeat(20_000)}\u0007`,
      acceptance_criteria: ["", "ok", "x".repeat(4096)],
      locked_fields: ["title", "description"],
      owner: null,
      due_at: null,
    };
    const details = hiddenTaskDetails(task, () => undefined);
    expect(details.some((row) => row.key === "description" && row.value.includes("x"))).toBe(true);
    expect(displayTitle("\u0000")).toBe("(untitled)");
  });
});
