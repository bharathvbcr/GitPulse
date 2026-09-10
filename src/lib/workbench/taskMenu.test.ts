import { describe, expect, it } from "vitest";
import type { TaskCard } from "./client";
import {
  cardsById,
  contextMenuAnchor,
  duplicateTitle,
  flattenVisibleIds,
  isContextMenuKey,
  menuPageItems,
  rangeSelect,
  submenuTitle,
  taskMenuItems,
  toggleSelection,
} from "./taskMenu";

function card(over: Partial<TaskCard> = {}): TaskCard {
  return {
    id: "t1", revision: 1, updated_at: 1, title: "A", kind: "bug", status: "inbox",
    priority: 2, severity: null, owner: null, due_at: null, labels: [],
    repository_ids: ["r"], primary_repository_id: "r", home_workspace_id: null, position: 1,
    ...over,
  };
}

describe("context menu input", () => {
  it("recognises keyboard context-menu keys and ignores the rest", () => {
    expect(isContextMenuKey({ key: "ContextMenu" })).toBe(true);
    expect(isContextMenuKey({ key: "F10", shiftKey: true })).toBe(true);
    expect(isContextMenuKey({ key: "F10", shiftKey: false })).toBe(false);
    expect(isContextMenuKey({ key: "Enter" })).toBe(false);
    expect(isContextMenuKey({})).toBe(false);
  });

  it("uses pointer coordinates when finite and positive, otherwise the row rect", () => {
    expect(contextMenuAnchor({ clientX: 40, clientY: 80 }, null)).toEqual({ x: 40, y: 80 });
    expect(contextMenuAnchor({ clientX: 0, clientY: 0 }, { left: 10, top: 20, width: 100, height: 16 })).toEqual({ x: 42, y: 36 });
    expect(contextMenuAnchor({ clientX: Number.NaN, clientY: Number.POSITIVE_INFINITY }, null)).toEqual({ x: 8, y: 8 });
  });
});

describe("selection", () => {
  it("toggles only well-formed ids", () => {
    const one = toggleSelection(new Set(), "t1");
    expect([...one]).toEqual(["t1"]);
    expect([...toggleSelection(one, "t1")]).toEqual([]);
    expect([...toggleSelection(one, "bad id")]).toEqual(["t1"]);
  });

  it("range-selects through the visible order and fails closed on unknowns", () => {
    expect([...rangeSelect(["a", "b", "c", "d"], "b", "d")].sort()).toEqual(["b", "c", "d"]);
    expect([...rangeSelect(["a", "b"], null, "b")]).toEqual(["b"]);
    expect([...rangeSelect(["a"], "missing", "nope")]).toEqual([]);
    expect([...rangeSelect(["a"], "a", "missing")]).toEqual([]);
  });
});

describe("taskMenuItems", () => {
  it("offers open, enhance, duplicate, nested organize and delete for a single card", () => {
    const items = taskMenuItems({ cards: [card({ status: "ready", priority: 1 })], column: "ready" });
    const ids = items.map((item) => item.id);
    expect(ids).toContain("open");
    expect(ids).toContain("enhance");
    expect(ids).toContain("duplicate");
    expect(ids).toContain("copy-menu");
    expect(ids).toContain("copy-brief");
    expect(ids).toContain("copy-agent");
    expect(menuPageItems(items, "root").map((item) => item.id)).not.toContain("copy-agent");
    expect(menuPageItems(items, "copy").map((item) => item.id)).toContain("copy-agent");
    expect(ids).toContain("move-menu");
    expect(ids).toContain("move-inbox");
    expect(ids).not.toContain("move-ready");
    expect(ids).toContain("priority-menu");
    expect(ids).toContain("priority-0");
    expect(ids).not.toContain("priority-1");
    expect(ids).toContain("select-column");
    expect(ids).toContain("delete");
    expect(items.find((item) => item.id === "delete")?.danger).toBe(true);
    expect(items.find((item) => item.id === "enhance")?.hint).toBe("Manvi");
    expect(menuPageItems(items, "root").map((item) => item.id)).not.toContain("move-inbox");
    expect(menuPageItems(items, "move").map((item) => item.id)).toContain("move-inbox");
    expect(submenuTitle("copy")).toBe("Copy");
    expect(submenuTitle("move")).toBe("Move to");
    expect(submenuTitle("root")).toBe("Task actions");
  });

  it("hides single-task actions for a multi-selection and still allows bulk delete", () => {
    const items = taskMenuItems({
      cards: [card({ id: "a", status: "inbox" }), card({ id: "b", status: "done" })],
    });
    const ids = items.map((item) => item.id);
    expect(ids).not.toContain("open");
    expect(ids).not.toContain("enhance");
    expect(ids).not.toContain("duplicate");
    expect(ids).toContain("move-menu");
    expect(ids).toContain("move-inbox");
    expect(ids).toContain("move-done");
    expect(ids).toContain("copy-menu");
    expect(ids).toContain("copy-agent");
    expect(ids).not.toContain("copy-brief");
    expect(items.find((item) => item.id === "copy-agent")?.label).toContain("2");
    expect(menuPageItems(items, "copy").map((item) => item.id)).toContain("copy-agent");
    expect(items.find((item) => item.id === "delete")?.label).toContain("2");
  });

  it("caps duplicate titles and refuses to invent an untitled original", () => {
    expect(duplicateTitle("Keep E42")).toBe("Keep E42 (copy)");
    expect(duplicateTitle("   ")).toBe("Untitled copy");
    expect(duplicateTitle("x".repeat(300)).endsWith(" (copy)")).toBe(true);
    expect(duplicateTitle("x".repeat(300)).length).toBe(300);
  });

  it("disables actions while busy and still offers new-in-column on empty chrome", () => {
    const empty = taskMenuItems({ cards: [], column: "review", busy: true });
    expect(empty).toHaveLength(1);
    expect(empty[0].disabled).toBe(true);
    expect(empty[0].action).toEqual({ kind: "newInColumn", status: "review" });
    expect(taskMenuItems({ cards: [] })).toEqual([]);
  });
});

describe("column flattening", () => {
  it("walks visible cards in status order and ignores duplicates", () => {
    const columns = {
      inbox: { items: [card({ id: "a" }), card({ id: "skip" })] },
      ready: { items: [card({ id: "a", status: "ready" }), card({ id: "b", status: "ready" })] },
    };
    expect(flattenVisibleIds(columns, ["inbox", "ready"], (c) => c.id !== "skip")).toEqual(["a", "b"]);
    expect(cardsById(columns, new Set(["b", "missing"])).map((c) => c.id)).toEqual(["b"]);
  });
});
