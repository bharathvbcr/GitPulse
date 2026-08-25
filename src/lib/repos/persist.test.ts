import { describe, expect, it } from "vitest";
import {
  STORAGE_KEY_LAST_PATH,
  STORAGE_KEY_RECENT,
  STORAGE_KEY_WORKSPACE,
  loadPersistedWorkspace,
  memoryStorage,
  savePersistedWorkspace,
  workspaceToPersisted,
} from "./persist";
import { emptyWorkspace, openTab } from "./tabModel";

const opts = { caseInsensitive: true };

describe("persist workspace", () => {
  it("migrates last_repo + recents when v1 is missing", () => {
    const storage = memoryStorage({
      [STORAGE_KEY_LAST_PATH]: "/Users/acme/gitpulse/",
      [STORAGE_KEY_RECENT]: JSON.stringify(["/Users/acme/gitpulse", "/tmp/other", 12, ""]),
    });
    const loaded = loadPersistedWorkspace(storage, opts);
    expect(loaded.tabs).toHaveLength(1);
    expect(loaded.tabs[0].path).toBe("/Users/acme/gitpulse");
    expect(loaded.activePath).toBe("/Users/acme/gitpulse");
    expect(loaded.recents[0]).toBe("/Users/acme/gitpulse");
    expect(loaded.recents).toContain("/tmp/other");
    expect(loaded.recents).not.toContain("");
  });

  it("fails closed on corrupt JSON and still recovers recents", () => {
    const storage = memoryStorage({
      [STORAGE_KEY_WORKSPACE]: "{not-json",
      [STORAGE_KEY_RECENT]: "[]",
    });
    const loaded = loadPersistedWorkspace(storage, opts);
    expect(loaded.tabs).toEqual([]);
    expect(loaded.activePath).toBeNull();
  });

  it("drops duplicate identities, invalid view tabs, and missing activePath", () => {
    const storage = memoryStorage({
      [STORAGE_KEY_WORKSPACE]: JSON.stringify({
        version: 1,
        tabs: [
          { path: "/r/a/", pinned: true, viewTab: "nope", searchQuery: "feat:" },
          { path: "/r/a", pinned: false, viewTab: "diff" },
          { path: "\0bad", pinned: false },
          { path: "/r/b", pinned: false, viewTab: "blame", selectedBranch: "main" },
        ],
        activePath: "/r/missing",
        recents: ["/r/b", "/r/b/", "/r/a"],
        lastClosed: ["/r/z"],
      }),
    });
    const loaded = loadPersistedWorkspace(storage, opts);
    expect(loaded.tabs.map((tab) => tab.path)).toEqual(["/r/a", "/r/b"]);
    expect(loaded.tabs[0].pinned).toBe(true);
    expect(loaded.tabs[0].viewTab).toBe("history");
    expect(loaded.tabs[1].viewTab).toBe("blame");
    expect(loaded.activePath).toBe("/r/a");
    expect(loaded.recents).toEqual(["/r/b", "/r/a"]);
  });

  it("round-trips workspace tabs and last-closed", () => {
    const first = openTab(emptyWorkspace(), "/r/a", opts);
    if (!first.ok) throw new Error("open");
    const second = openTab(first.workspace, "/r/b", opts);
    if (!second.ok) throw new Error("open");
    const persisted = workspaceToPersisted(second.workspace, {
      [first.id]: { activeTab: "diff", searchQuery: "author:ada", selectedBranch: "main" },
      [second.id]: { activeTab: "health", searchQuery: "", selectedBranch: null },
    });
    const storage = memoryStorage();
    savePersistedWorkspace(storage, persisted);
    const loaded = loadPersistedWorkspace(storage, opts);
    expect(loaded.tabs.map((tab) => tab.path)).toEqual(["/r/a", "/r/b"]);
    expect(loaded.tabs[0].viewTab).toBe("diff");
    expect(loaded.tabs[0].searchQuery).toBe("author:ada");
    expect(loaded.tabs[1].viewTab).toBe("health");
    expect(loaded.activePath).toBe("/r/b");
    expect(JSON.parse(storage.getItem(STORAGE_KEY_RECENT) ?? "[]")).toEqual(["/r/b", "/r/a"]);
  });

  it("writes legacy keys BEFORE the workspace blob so a quota failure cannot desync them", () => {
    const persisted = workspaceToPersisted(
      (() => {
        const first = openTab(emptyWorkspace(), "/r/a", opts);
        if (!first.ok) throw new Error("open");
        return first.workspace;
      })(),
      {},
    );
    // Simulate a mid-write quota exhaustion on the authoritative blob.
    const storage = memoryStorage({
      [STORAGE_KEY_WORKSPACE]: JSON.stringify({ version: 1, tabs: [], activePath: null, recents: [], lastClosed: [] }),
    });
    const order: string[] = [];
    const baseSetItem = storage.setItem;
    storage.setItem = (key, value) => {
      if (key === STORAGE_KEY_WORKSPACE) {
        order.push(key);
        throw new Error("quota exceeded");
      }
      order.push(key);
      baseSetItem(key, value);
    };
    expect(() => savePersistedWorkspace(storage, persisted)).not.toThrow();
    // Legacy keys were attempted first; the blob write came last and failed.
    expect(order).toEqual([STORAGE_KEY_RECENT, STORAGE_KEY_LAST_PATH, STORAGE_KEY_WORKSPACE]);
    // The previous blob survives intact — the loader prefers it, so state
    // stays consistent rather than half-updated.
    expect(loadPersistedWorkspace(storage, opts).tabs).toEqual([]);
  });

  /// Regression (audit M12): the save outcome must be observable, so the
  /// store's dedup marker only advances past payloads that actually landed.
  it("reports success so a failed write can be retried", () => {
    const first = openTab(emptyWorkspace(), "/r/retry", opts);
    if (!first.ok) throw new Error("open");
    const persisted = workspaceToPersisted(first.workspace, {});

    const failing = memoryStorage();
    const baseSetItem = failing.setItem;
    failing.setItem = (key, value) => {
      if (key === STORAGE_KEY_WORKSPACE) throw new Error("quota");
      baseSetItem(key, value);
    };
    expect(savePersistedWorkspace(failing, persisted)).toBe(false);

    expect(savePersistedWorkspace(memoryStorage(), persisted)).toBe(true);
  });
});
