import { describe, expect, it } from "vitest";
import {
  STORAGE_KEY_WORKSPACE,
  WORKSPACE_VERSION,
  loadPersistedWorkspace,
  memoryStorage,
  workspaceToPersisted,
} from "./persist";
import { emptyWorkspace, openTab } from "./tabModel";
import { restoredHistoryList, sanitizeHistoryList } from "../views/historyList";

/**
 * Reflog folded from a History section into History › Graph's "HEAD moves"
 * list. A session written while the reflog was open must still open on it,
 * through the real loader, whichever of the two older shapes wrote it.
 */

const opts = { caseInsensitive: true };

function load(tab: Record<string, unknown>) {
  const storage = memoryStorage({
    [STORAGE_KEY_WORKSPACE]: JSON.stringify({
      version: WORKSPACE_VERSION,
      tabs: [{ path: "/r/one", pinned: false, searchQuery: "", ...tab }],
      activePath: "/r/one",
      recents: [],
      lastClosed: [],
    }),
  });
  const loaded = loadPersistedWorkspace(storage, opts);
  expect(loaded.tabs).toHaveLength(1);
  return loaded.tabs[0];
}

describe("restoring a session left on the reflog", () => {
  it("restores a remembered Reflog section as Graph showing HEAD moves", () => {
    const tab = load({ viewTab: "history", viewSections: { history: "reflog" } });
    expect(tab.viewTab).toBe("history");
    expect(tab.viewSections).toEqual({ history: "graph" });
    expect(tab.historyList).toBe("moves");
  });

  it("restores the older top-level Reflog view the same way", () => {
    const tab = load({ viewTab: "reflog" });
    expect(tab.viewTab).toBe("history");
    expect(tab.viewSections).toEqual({ history: "graph" });
    expect(tab.historyList).toBe("moves");
  });

  it("keeps a session that never opened the reflog on the commit graph", () => {
    expect(load({ viewTab: "history", viewSections: { history: "diff" } }).historyList).toBeUndefined();
    expect(load({ viewTab: "work" }).historyList).toBeUndefined();
  });

  it("reads anything but the literal \"moves\" as the commit graph", () => {
    for (const junk of [true, "MOVES", "commits", 1, null, {}, ["moves"], ["suspects"]]) {
      expect(load({ viewTab: "history", historyList: junk }).historyList, String(junk)).toBeUndefined();
    }
    expect(load({ viewTab: "history", historyList: "moves" }).historyList).toBe("moves");
  });

  it("round-trips the mode, and writes nothing for the default", () => {
    const first = openTab(emptyWorkspace(), "/r/a", opts);
    if (!first.ok) throw new Error("open");
    const second = openTab(first.workspace, "/r/b", opts);
    if (!second.ok) throw new Error("open");
    const persisted = workspaceToPersisted(second.workspace, {
      [first.id]: { activeTab: "history", historyList: "moves" },
      [second.id]: { activeTab: "history", historyList: "commits" },
    });
    expect(persisted.tabs[0].historyList).toBe("moves");
    expect("historyList" in persisted.tabs[1]).toBe(false);
  });
});

describe("restoring a session left on regression suspects", () => {
  // Suspects folded the same way, from a History section into Graph's third list.
  it("restores a remembered Suspects section as Graph showing Suspects", () => {
    const tab = load({ viewTab: "history", viewSections: { history: "suspects" } });
    expect(tab.viewTab).toBe("history");
    expect(tab.viewSections).toEqual({ history: "graph" });
    expect(tab.historyList).toBe("suspects");
  });

  it("restores Suspects even when an older build saved another list beside it", () => {
    // A build that still had the Suspects section wrote `historyList: "moves"`
    // whenever Graph had last been left on HEAD moves, whatever section was on
    // screen. No newer build writes `history: "suspects"` (the section is
    // retired), so the section is the truer record of what was showing.
    const tab = load({ viewTab: "history", viewSections: { history: "suspects" }, historyList: "moves" });
    expect(tab.historyList).toBe("suspects");
    expect(tab.viewSections).toEqual({ history: "graph" });
  });

  it("keeps a list a newer build wrote when the section is a live one", () => {
    expect(load({ viewTab: "history", viewSections: { history: "graph" }, historyList: "moves" }).historyList).toBe("moves");
    expect(load({ viewTab: "history", viewSections: { history: "diff" }, historyList: "suspects" }).historyList).toBe("suspects");
  });

  it("round-trips the Suspects list", () => {
    const first = openTab(emptyWorkspace(), "/r/a", opts);
    if (!first.ok) throw new Error("open");
    const persisted = workspaceToPersisted(first.workspace, {
      [first.id]: { activeTab: "history", historyList: "suspects" },
    });
    expect(persisted.tabs[0].historyList).toBe("suspects");
    expect(load({ viewTab: "history", historyList: "suspects" }).historyList).toBe("suspects");
  });
});

describe("historyList helpers", () => {
  it("narrows strictly", () => {
    expect(sanitizeHistoryList("moves")).toBe("moves");
    expect(sanitizeHistoryList("commits")).toBe("commits");
    expect(sanitizeHistoryList(undefined)).toBe("commits");
    expect(sanitizeHistoryList("reflog")).toBe("commits");
    expect(sanitizeHistoryList("suspects")).toBe("suspects");
    expect(sanitizeHistoryList("SUSPECTS")).toBe("commits");
  });

  it("does not read a hostile section map as the reflog", () => {
    expect(restoredHistoryList({ viewSections: ["reflog"] })).toBe("commits");
    expect(restoredHistoryList({ viewSections: "reflog" })).toBe("commits");
    expect(restoredHistoryList({ viewSections: { work: "reflog" } })).toBe("commits");
  });
});
