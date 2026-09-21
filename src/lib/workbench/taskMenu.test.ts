import { describe, expect, it } from "vitest";
import { STATUS_LABELS, type TaskCard } from "./client";
import { ARCHIVE_STATUS } from "./taskArchive";
import {
  cardsById,
  contextMenuAnchor,
  duplicateTitle,
  flattenVisibleIds,
  isContextMenuKey,
  MAX_MENU_VALUES,
  menuPageItems,
  rangeSelect,
  submenuTitle,
  taskMenuItems,
  toggleSelection,
  typeAheadIndex,
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
    expect(ids).toContain("priority-menu");
    expect(ids).toContain("priority-0");
    // Every value is listed. The one the task already has is marked and
    // disabled rather than omitted, so the menu keeps the same shape whichever
    // card it opened on — and the card's current value is readable from it.
    expect(items.find((item) => item.id === "move-ready")).toMatchObject({ checked: true, disabled: true });
    expect(items.find((item) => item.id === "move-inbox")).toMatchObject({ checked: false, disabled: false });
    expect(items.find((item) => item.id === "priority-1")).toMatchObject({ checked: true, disabled: true });
    expect(items.find((item) => item.id === "priority-0")).toMatchObject({ checked: false });
    expect(ids).toContain("select-column");
    expect(ids).toContain("delete");
    expect(items.find((item) => item.id === "delete")?.danger).toBe(true);
    expect(items.find((item) => item.id === "enhance")?.hint).toBe("E");
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

  // The reason this whole row exists: the board had `Move to… › Done` and no
  // Archive anywhere, so "archive this task" had no answer in the product and
  // the panel called Archive offered only Restore.
  describe("the Archive row", () => {
    it("sits beside Delete on every card, and says where an archived task goes", () => {
      const items = taskMenuItems({ cards: [card({ status: "ready" })], column: "ready" });
      const ids = items.map((item) => item.id);
      expect(ids).toContain("archive");
      // Last two rows, in that order: the two ways work leaves the board.
      expect(ids.slice(-2)).toEqual(["archive", "delete"]);
      const archive = items.find((item) => item.id === "archive");
      expect(archive).toMatchObject({ label: "Archive", icon: "archive", disabled: false });
      expect(archive?.separatorBefore).toBe(true);
      // Named from the vocabulary, never spelled: this hint is the only place
      // the board tells a reader which column archiving files a task into.
      expect(archive?.hint).toBe(STATUS_LABELS[ARCHIVE_STATUS]);
      // Not styled as destructive. Archiving is the ordinary end of a task.
      expect(archive?.danger).toBeUndefined();
    });

    it("refuses the write that would store the status already there", () => {
      const archived = taskMenuItems({ cards: [card({ status: ARCHIVE_STATUS })] })
        .find((item) => item.id === "archive");
      expect(archived?.disabled).toBe(true);
      expect(archived?.hint).toBe(`Already in ${STATUS_LABELS[ARCHIVE_STATUS]}`);
      // Disabled, not hidden — the same rule the Move and Priority rows
      // follow, so the menu keeps its shape whichever card it opened on.
      expect(archived?.label).toBe("Archive");
    });

    it("still offers a mixed selection, and counts what it would archive", () => {
      const items = taskMenuItems({
        cards: [card({ id: "a", status: "ready" }), card({ id: "b", status: ARCHIVE_STATUS })],
      });
      const archive = items.find((item) => item.id === "archive");
      expect(archive?.disabled).toBe(false);
      expect(archive?.label).toBe("Archive 2 tasks");
      expect(archive?.hint).toBe(STATUS_LABELS[ARCHIVE_STATUS]);
    });

    it("goes away while a write is in flight, like every other action", () => {
      const busy = taskMenuItems({ cards: [card({ status: "ready" })], busy: true })
        .find((item) => item.id === "archive");
      expect(busy?.disabled).toBe(true);
    });

    // An empty right-click on a column offers only "new task here". Archiving
    // nothing is not an action, and a disabled row there would be noise.
    it("is absent when the menu opened on no cards at all", () => {
      const items = taskMenuItems({ cards: [], column: "ready" });
      expect(items.map((item) => item.id)).not.toContain("archive");
    });
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

describe("value submenus", () => {
  it("marks a value the whole selection shares, and mixes when it does not", () => {
    const items = taskMenuItems({
      cards: [card({ id: "a", status: "ready", priority: 1 }), card({ id: "b", status: "ready", priority: 3 })],
    });
    expect(items.find((item) => item.id === "move-ready")).toMatchObject({ checked: true, disabled: true });
    expect(items.find((item) => item.id === "priority-1")).toMatchObject({ checked: "mixed" });
    expect(items.find((item) => item.id === "priority-3")).toMatchObject({ checked: "mixed" });
    expect(items.find((item) => item.id === "priority-0")).toMatchObject({ checked: false });
    // A mixed value is still choosable: picking it is how you make them agree.
    expect(items.find((item) => item.id === "priority-1")?.disabled).toBe(false);
  });

  it("offers relative due dates and marks a selection that already has none", () => {
    const none = taskMenuItems({ cards: [card()] });
    expect(menuPageItems(none, "due").map((item) => item.id)).toEqual([
      "due-today", "due-tomorrow", "due-next_week", "due-clear",
    ]);
    expect(none.find((item) => item.id === "due-clear")?.checked).toBe(true);
    const dated = taskMenuItems({ cards: [card({ due_at: 1_800_000_000 })] });
    expect(dated.find((item) => item.id === "due-clear")?.checked).toBe(false);
  });

  it("always offers Unassigned and lists the owners the board has loaded", () => {
    const items = taskMenuItems({
      cards: [card({ owner: "Ada" })],
      vocabulary: { owners: ["Ada", "Grace"] },
    });
    const owners = menuPageItems(items, "owner");
    expect(owners.map((item) => item.label)).toEqual(["Unassigned", "Ada", "Grace"]);
    expect(owners.find((item) => item.label === "Ada")?.checked).toBe(true);
    expect(owners.find((item) => item.label === "Unassigned")?.checked).toBe(false);
    expect(owners.find((item) => item.label === "Grace")?.action).toEqual({ kind: "owner", owner: "Grace" });
    // With no loaded owners the page still exists, so a task can be unassigned.
    expect(menuPageItems(taskMenuItems({ cards: [card()] }), "owner")).toHaveLength(1);
  });

  it("turns a label item into add or remove depending on what the selection has", () => {
    const items = taskMenuItems({
      cards: [card({ labels: ["ui"] }), card({ id: "b", labels: ["ui", "ux"] })],
      vocabulary: { labels: ["ui", "ux", "docs"] },
    });
    const labels = menuPageItems(items, "label");
    expect(labels.find((item) => item.label === "ui")).toMatchObject({ checked: true, action: { kind: "label", label: "ui", add: false } });
    expect(labels.find((item) => item.label === "ux")).toMatchObject({ checked: "mixed", action: { kind: "label", label: "ux", add: true } });
    expect(labels.find((item) => item.label === "docs")).toMatchObject({ checked: false, action: { kind: "label", label: "docs", add: true } });
  });

  it("omits the labels page entirely when the board has loaded no labels", () => {
    const items = taskMenuItems({ cards: [card()] });
    expect(items.map((item) => item.id)).not.toContain("label-menu");
    expect(menuPageItems(items, "label")).toEqual([]);
  });

  it("bounds how many owners and labels one menu will list", () => {
    const many = Array.from({ length: MAX_MENU_VALUES + 20 }, (_, i) => `v${i}`);
    const items = taskMenuItems({ cards: [card()], vocabulary: { owners: many, labels: many } });
    expect(menuPageItems(items, "owner")).toHaveLength(MAX_MENU_VALUES + 1);
    expect(menuPageItems(items, "label")).toHaveLength(MAX_MENU_VALUES);
  });

  it("drops blank, oversized and duplicate vocabulary entries", () => {
    const items = taskMenuItems({
      cards: [card()],
      vocabulary: { labels: ["ui", "ui", "  ", "x".repeat(400), " ux "] },
    });
    expect(menuPageItems(items, "label").map((item) => item.label)).toEqual(["ui", "ux"]);
  });
});

describe("agent handoff entry", () => {
  it("offers one submenu with each provider and connection for a single task", () => {
    const items = taskMenuItems({ cards: [card()], canHandoff: true });
    expect(items.map((item) => item.id)).toContain("agent-menu");
    expect(menuPageItems(items, "agent").map((item) => [item.label, item.hint])).toEqual([
      ["Claude Code", "Terminal"],
      ["Codex", "Terminal"],
      ["Grok", "Terminal"],
      ["Antigravity", "Terminal"],
      ["Claude Code", "Managed"],
      ["Codex", "Managed"],
    ]);
    expect(items.find((item) => item.id === "agent-codex-managed")?.action).toEqual({
      kind: "agent",
      target: { provider: "codex", kind: "managed" },
    });
    expect(items.find((item) => item.id === "agent-claude-managed")?.action).toEqual({
      kind: "agent",
      target: { provider: "claude", kind: "managed" },
    });
    expect(submenuTitle("agent")).toBe("Send to agent");
  });

  it("offers no handoff for a multi-selection or when no repository is registered", () => {
    expect(taskMenuItems({ cards: [card(), card({ id: "b" })] }).map((i) => i.id)).not.toContain("agent-menu");
    expect(taskMenuItems({ cards: [card()], canHandoff: false }).map((i) => i.id)).not.toContain("agent-menu");
  });

  it("disables the handoff entries while another task action is in flight", () => {
    const items = taskMenuItems({ cards: [card()], busy: true, canHandoff: true });
    expect(menuPageItems(items, "agent").every((item) => item.disabled)).toBe(true);
  });
});

describe("typeAheadIndex", () => {
  const items = [{ label: "Inbox" }, { label: "Backlog" }, { label: "Ready" }, { label: "Review" }, { label: "Done" }];

  it("jumps to the next item starting with the typed letter", () => {
    expect(typeAheadIndex(items, "b", -1)).toBe(1);
    expect(typeAheadIndex(items, "R", -1)).toBe(2);
    expect(typeAheadIndex(items, "in", -1)).toBe(0);
  });

  it("cycles through repeated matches instead of sticking on the first", () => {
    expect(typeAheadIndex(items, "r", 2)).toBe(3);
    expect(typeAheadIndex(items, "r", 3)).toBe(2);
  });

  it("skips disabled rows so focus never lands somewhere unusable", () => {
    const withDisabled = [{ label: "Ready", disabled: true }, { label: "Review" }];
    expect(typeAheadIndex(withDisabled, "re", -1)).toBe(1);
  });

  it("returns -1 rather than moving focus when nothing matches", () => {
    expect(typeAheadIndex(items, "z", -1)).toBe(-1);
    expect(typeAheadIndex(items, "", -1)).toBe(-1);
    expect(typeAheadIndex(items, "   ", 0)).toBe(-1);
    expect(typeAheadIndex([], "a", 0)).toBe(-1);
    expect(typeAheadIndex(items, "b", Number.NaN)).toBe(1);
    expect(typeAheadIndex(items, "b", -99)).toBe(1);
  });
});
