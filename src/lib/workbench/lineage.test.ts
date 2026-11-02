import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));
const answer = (value: unknown) => invoke.mockResolvedValue(JSON.stringify(value));
const sent = () => invoke.mock.calls.filter(([, args]) => args !== undefined).map(([, args]) => JSON.parse((args as { input: string }).input));

import type { TaskCard } from "./client";
import { taskDraft } from "./client";
import {
  MAX_LINEAGE_BATCH,
  MAX_LINEAGE_READS,
  childProgressLabel,
  getBoardLineage,
  getLineage,
  sameLineage,
  withLineage,
  type CardLineage,
} from "./lineage";

beforeEach(() => invoke.mockReset());

const entry = (parent: string | null, total: number, done: number): CardLineage => ({ parent_item_id: parent, children: { total, done } });
const wire = (parent: string | null, total: number, done: number) => ({ parent_item_id: parent, children: { total, done } });

describe("getLineage", () => {
  it("reads every card in one request, deduplicated", async () => {
    answer({ ok: true, items: { a: wire(null, 3, 1), b: wire("a", 0, 0) } });
    const out = await getLineage(["a", "b", "c", "a"]);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke.mock.calls[0][1]).toMatchObject({ method: "items.lineage" });
    expect(sent()).toEqual([{ ids: ["a", "b", "c"] }]);
    expect([...out]).toEqual([["a", entry(null, 3, 1)], ["b", entry("a", 0, 0)]]);
    expect(out.has("c")).toBe(false);
  });

  it("makes no request for an empty board", async () => {
    expect((await getLineage([])).size).toBe(0);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("fails loudly when the read fails, rather than answering no subtasks", async () => {
    invoke.mockRejectedValueOnce({ code: "store_error", message: "store unavailable" });
    const outcome = await getLineage(["a"]).then(() => "resolved", (error: unknown) => `rejected: ${error instanceof Error ? error.message : String(error)}`);
    expect(outcome).toContain("rejected");
    expect(outcome).toContain("store unavailable");
  });

  it("refuses a malformed answer", async () => {
    for (const bad of [
      { ok: false },
      { ok: true },
      { ok: true, items: { a: { parent_item_id: null } } },
      { ok: true, items: { a: wire(null, 1, 2) } },
      { ok: true, items: { a: wire(null, -1, 0) } },
      { ok: true, items: { a: wire(null, 1.5, 0) } },
      { ok: true, items: { a: wire("bad id with spaces", 0, 0) } },
      { ok: true, items: { a: wire(7 as unknown as string, 0, 0) } },
    ]) {
      answer(bad);
      await expect(getLineage(["a"])).rejects.toThrow();
    }
  });

  it("ignores an entry for a card it was not asked about", async () => {
    answer({ ok: true, items: { a: wire(null, 1, 0), z: wire(null, 9, 9) } });
    expect([...(await getLineage(["a"])).keys()]).toEqual(["a"]);
  });

  it("refuses more cards than one read takes", async () => {
    const ids = Array.from({ length: MAX_LINEAGE_BATCH + 1 }, (_, n) => `t${n}`);
    await expect(getLineage(ids)).rejects.toThrow();
    expect(invoke).not.toHaveBeenCalled();
  });
});

describe("getBoardLineage", () => {
  it("reads a larger board in parts, each at most one batch", async () => {
    const ids = Array.from({ length: MAX_LINEAGE_BATCH * 2 + 3 }, (_, n) => `t${n}`);
    invoke.mockImplementation(async (...all: unknown[]) => {
      // Only the host command is answered; the app's own bookkeeping calls carry no arguments.
      const args = all[1] as { input: string } | undefined;
      if (!args) return "null";
      const { ids: asked } = JSON.parse(args.input) as { ids: string[] };
      return JSON.stringify({ ok: true, items: Object.fromEntries(asked.map((id) => [id, wire(null, 0, 0)])) });
    });
    const out = await getBoardLineage(ids);
    expect(sent().map((call: { ids: string[] }) => call.ids.length)).toEqual([MAX_LINEAGE_BATCH, MAX_LINEAGE_BATCH, 3]);
    expect(out.size).toBe(ids.length);
  });

  it("refuses a board past the read limit rather than reading part of it", async () => {
    const ids = Array.from({ length: MAX_LINEAGE_BATCH * MAX_LINEAGE_READS + 1 }, (_, n) => `t${n}`);
    await expect(getBoardLineage(ids)).rejects.toThrow();
    expect(invoke).not.toHaveBeenCalled();
  });

  it("fails the whole read when one part fails, so no card is drawn from half an answer", async () => {
    const ids = Array.from({ length: MAX_LINEAGE_BATCH + 1 }, (_, n) => `t${n}`);
    invoke.mockResolvedValueOnce(JSON.stringify({ ok: true, items: {} }));
    invoke.mockRejectedValueOnce({ code: "store_error", message: "second part failed" });
    await expect(getBoardLineage(ids)).rejects.toThrow("second part failed");
  });
});

describe("what a card shows", () => {
  const card = (over: Partial<TaskCard> = {}): TaskCard => ({
    id: "a", revision: 1, updated_at: 1, title: "A", kind: "", status: "ready", priority: 0, severity: null, owner: null,
    due_at: null, labels: [], repository_ids: ["r"], primary_repository_id: "r", home_workspace_id: null, position: 0,
    archived: false, completed_at: null, ...over,
  });

  it("fills the lineage of a card the read named and leaves any other card unknown", () => {
    const read = new Map([["a", entry("p", 3, 2)]]);
    expect(withLineage(card(), read)).toMatchObject({ parent_item_id: "p", children: { total: 3, done: 2 } });
    const other = withLineage(card({ id: "b" }), read);
    expect("parent_item_id" in other).toBe(false);
    expect("children" in other).toBe(false);
  });

  it("says how many subtasks are done, and says nothing for none or for unknown", () => {
    expect(childProgressLabel({ total: 3, done: 2 })).toBe("2 of 3 subtasks done");
    expect(childProgressLabel({ total: 1, done: 0 })).toBe("0 of 1 subtask done");
    expect(childProgressLabel({ total: 0, done: 0 })).toBeNull();
    expect(childProgressLabel(undefined)).toBeNull();
  });

  it("compares reads by what they say about every card", () => {
    const shown = new Map([["a", entry(null, 2, 1)], ["b", entry("a", 0, 0)]]);
    expect(sameLineage(shown, new Map([["b", entry("a", 0, 0)], ["a", entry(null, 2, 1)]]))).toBe(true);
    expect(sameLineage(shown, new Map([["a", entry(null, 2, 2)], ["b", entry("a", 0, 0)]]))).toBe(false);
    expect(sameLineage(shown, new Map([["a", entry(null, 2, 1)], ["b", entry(null, 0, 0)]]))).toBe(false);
    expect(sameLineage(shown, new Map([["a", entry(null, 2, 1)]]))).toBe(false);
  });

  it("is never sent back with a task write", () => {
    const full = {
      ...card({ parent_item_id: "p", children: { total: 1, done: 0 } }),
      description: "", acceptance_criteria: [], checklist: [], links: [],
    };
    const draft = taskDraft(full) as unknown as Record<string, unknown>;
    expect("parent_item_id" in draft).toBe(false);
    expect("children" in draft).toBe(false);
    expect(draft.title).toBe("A");
  });
});
