import { describe, expect, it } from "vitest";
import { WorkbenchError } from "./client";
import {
  classifyDeleteError,
  deleteAlreadyGone,
  deleteAttempt,
  deleteConfirmCopy,
  deleteRefusal,
  deleteSummary,
  deleteTasks,
  displayTitle,
  isRevision,
  isTaskId,
  MAX_CONFIRM_TITLES,
  MAX_DELETE_BATCH,
  removeFromColumns,
  runBoundedSerial,
  uniqueDeletable,
  withTimeout,
  type DeletableTask,
} from "./taskDelete";
import type { Page, TaskCard } from "./client";

function task(over: Partial<DeletableTask> = {}): DeletableTask {
  return { id: "t1", revision: 2, title: "Keep E42", ...over };
}

function card(over: Partial<TaskCard> = {}): TaskCard {
  return {
    id: "t1", revision: 2, updated_at: 1, title: "Keep E42", kind: "bug", status: "ready",
    priority: 1, severity: null, owner: null, due_at: null, labels: [],
    repository_ids: ["r"], primary_repository_id: "r", home_workspace_id: null, position: 1,
    ...over,
  };
}

function page(items: TaskCard[], total = items.length): Page<TaskCard> {
  return { items, total, shown: items.length, has_more: total > items.length, next_cursor: total > items.length ? "c" : null };
}

describe("displayTitle", () => {
  it("sanitizes empty, non-strings, control characters and oversize titles", () => {
    expect(displayTitle(null)).toBe("(untitled)");
    expect(displayTitle(12)).toBe("(untitled)");
    expect(displayTitle("   ")).toBe("(untitled)");
    expect(displayTitle("a\nb\tc")).toBe("a b c");
    expect(displayTitle("x".repeat(90)).endsWith("…")).toBe(true);
    expect(displayTitle("x".repeat(90), 8).length).toBe(8);
    expect(displayTitle("ok", Number.NaN)).toBe("ok");
  });
});

describe("uniqueDeletable", () => {
  it("keeps the first well-formed row and drops hostile duplicates", () => {
    expect(uniqueDeletable(null as unknown as DeletableTask[])).toEqual([]);
    expect(uniqueDeletable([
      { id: "", revision: 1, title: "x" },
      { id: "ok", revision: 0, title: "x" },
      { id: "ok", revision: 1.5, title: "x" },
      { id: "ok", revision: Number.NaN, title: "x" },
      { id: "a/b", revision: 1, title: "x" },
      { id: "ok", revision: 3, title: "first" },
      { id: "ok", revision: 9, title: "stale" },
      { id: "two", revision: 1, title: "\u0007boom" },
      undefined,
      4,
    ])).toEqual([
      { id: "ok", revision: 3, title: "first" },
      { id: "two", revision: 1, title: "boom" },
    ]);
  });
});

describe("deleteRefusal and confirm copy", () => {
  it("refuses overlapping board operations instead of queueing a delete", () => {
    expect(deleteRefusal({ selected: 1 })).toBeNull();
    expect(deleteRefusal({ selected: 0 })).toMatch(/Select/);
    expect(deleteRefusal({ selected: 2, moving: true })).toMatch(/moving/);
    expect(deleteRefusal({ selected: 2, deleting: true })).toMatch(/already running/);
    expect(deleteRefusal({ selected: 2, enhancing: true })).toMatch(/enhancement/);
  });

  it("lists a capped title sample and tells the truth about restore", () => {
    const one = deleteConfirmCopy([task()]);
    expect(one.title).toContain("Keep E42");
    expect(one.confirmLabel).toBe("Delete task");
    expect(one.message).toContain("cannot be restored");
    expect(one.message).toContain("Manvi");

    const many = Array.from({ length: MAX_CONFIRM_TITLES + 5 }, (_, i) => task({ id: `t${i}`, title: `T${i}` }));
    const copy = deleteConfirmCopy(many);
    expect(copy.title).toContain(`${MAX_CONFIRM_TITLES + 5} tasks`);
    expect(copy.message).toContain("and 5 more");
    expect(copy.confirmLabel).toContain(String(MAX_CONFIRM_TITLES + 5));
    expect(deleteConfirmCopy([]).message).toMatch(/Nothing valid/);
  });
});

describe("deleteAttempt and error classification", () => {
  it("rejects malformed attempts rather than sending them", () => {
    expect(deleteAttempt(task(), "")).toBeNull();
    expect(deleteAttempt(task({ revision: 0 }), "r")).toBeNull();
    expect(deleteAttempt(task(), "ok")?.expected_revision).toBe(2);
  });

  it("treats not_found as already-gone and transport as retryable", () => {
    expect(deleteAlreadyGone(new WorkbenchError("not_found", "gone"))).toBe(true);
    expect(classifyDeleteError(new WorkbenchError("revision_conflict", "stale")).retryable).toBe(false);
    expect(classifyDeleteError(new WorkbenchError("transport_error", "down")).retryable).toBe(true);
    expect(classifyDeleteError("nope").retryable).toBe(true);
  });
});

describe("runBoundedSerial and deleteTasks", () => {
  it("caps the pass and preserves skip counts instead of pretending completeness", async () => {
    const items = [1, 2, 3, 4, 5];
    const result = await runBoundedSerial(items, async (n) => { if (n === 2) throw new Error("boom"); }, { limit: 3 });
    expect(result.done).toEqual([1, 3]);
    expect(result.failed).toHaveLength(1);
    expect(result.skipped).toEqual([4, 5]);
  });

  it("retries a retryable failure once with the same attempt identity", async () => {
    const seen: string[] = [];
    let calls = 0;
    const result = await deleteTasks([task()], async (attempt) => {
      seen.push(attempt.request_id);
      calls += 1;
      if (calls === 1) throw new WorkbenchError("transport_error", "lost");
    }, { newID: () => "req-1" });
    expect(seen).toEqual(["req-1", "req-1"]);
    expect(result.deleted).toEqual(["t1"]);
    expect(result.failed).toEqual([]);
  });

  it("counts not_found as deleted and surfaces revision conflicts", async () => {
    const gone = await deleteTasks([task()], async () => {
      throw new WorkbenchError("not_found", "already gone");
    }, { newID: () => "r" });
    expect(gone.deleted).toEqual(["t1"]);

    const conflict = await deleteTasks([task()], async () => {
      throw new WorkbenchError("revision_conflict", "expected revision 2; current revision is 4");
    }, { newID: () => "r" });
    expect(conflict.deleted).toEqual([]);
    expect(conflict.failed[0]?.code).toBe("revision_conflict");
  });

  it("stops further work when cancelled mid-batch", async () => {
    let n = 0;
    const result = await deleteTasks(
      [task({ id: "a" }), task({ id: "b" }), task({ id: "c" })],
      async () => { n += 1; },
      { newID: () => `id-${n}`, cancelled: () => n >= 1 },
    );
    expect(result.deleted.length + result.failed.length).toBeLessThan(3);
  });

  it("times out a hung delete and classifies it retryable", async () => {
    const result = await deleteTasks(
      [task()],
      () => new Promise(() => {}),
      { newID: () => "r", timeout: 20 },
    );
    expect(result.deleted).toEqual([]);
    expect(result.failed[0]?.retryable).toBe(true);
    expect(result.failed[0]?.code).toBe("transport_error");
    await expect(withTimeout(new Promise(() => {}), 10)).rejects.toMatchObject({ code: "transport_error" });
  });
});

describe("removeFromColumns and summary", () => {
  it("drops matching cards and reduces totals without inventing replacements", () => {
    const columns = {
      ready: page([card(), card({ id: "t2" }), card({ id: "t3" })], 10),
      done: page([card({ id: "t9", status: "done" })]),
    };
    const next = removeFromColumns(columns, new Set(["t2", "missing"]));
    expect(next.ready?.items.map((item) => item.id)).toEqual(["t1", "t3"]);
    expect(next.ready?.total).toBe(9);
    expect(next.ready?.shown).toBe(2);
    expect(next.ready?.has_more).toBe(true);
    expect(next.done?.items).toHaveLength(1);
    expect(removeFromColumns(columns, new Set())).toBe(columns);
  });

  it("reports both sides of a partial batch", () => {
    expect(deleteSummary({ deleted: ["a"], failed: [], skipped: 0, attempted: 1, total: 1 })).toMatch(/Deleted 1/);
    expect(deleteSummary({
      deleted: ["a", "b"],
      failed: [{ id: "c", title: "C", code: "revision_conflict", message: "stale", retryable: false }],
      skipped: 4,
      attempted: 3,
      total: 7,
    })).toMatch(/Held 4 more/);
    expect(deleteSummary({ deleted: [], failed: [], skipped: 0, attempted: 0, total: 0 })).toMatch(/Nothing/);
  });
});

describe("identity guards", () => {
  it("rejects ids and revisions the store would refuse", () => {
    expect(isTaskId("ok_1-2")).toBe(true);
    expect(isTaskId("x".repeat(129))).toBe(false);
    expect(isTaskId("has space")).toBe(false);
    expect(isRevision(1)).toBe(true);
    expect(isRevision(0)).toBe(false);
    expect(isRevision(Number.POSITIVE_INFINITY)).toBe(false);
    expect(MAX_DELETE_BATCH).toBe(50);
  });
});
