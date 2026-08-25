import { describe, expect, it, vi, afterEach } from "vitest";
import {
  STORAGE_KEY_LAST_PATH,
  STORAGE_KEY_RECENT,
  STORAGE_KEY_WORKSPACE,
  WORKSPACE_VERSION,
  loadMigrated,
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
});

describe("persist workspace migrations", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  const v1Blob = {
    version: 1,
    tabs: [{ path: "/r/a", pinned: true, viewTab: "diff", searchQuery: "feat:", selectedBranch: "main" }],
    activePath: "/r/a",
    recents: ["/r/a"],
    lastClosed: [],
  };

  it("passes a current-version blob through migration unchanged (validated, not trusted)", () => {
    const migrated = loadMigrated(structuredClone(v1Blob), opts);
    expect(migrated).toEqual(loadPersistedWorkspace(memoryStorage({
      [STORAGE_KEY_WORKSPACE]: JSON.stringify(v1Blob),
    }), opts));
    expect(migrated?.tabs[0]).toMatchObject({ path: "/r/a", pinned: true, viewTab: "diff" });
    expect(migrated?.version).toBe(WORKSPACE_VERSION);
  });

  it("applies registered migrations sequentially, feeding each step's output to the next", () => {
    // Two chained upgrade steps (v1->v2->v3) each rewrite the tab path; only
    // running BOTH in order can yield the final path.
    const migrations: Record<number, (blob: Record<string, unknown>) => Record<string, unknown>> = {
      1: (blob) => ({
        ...blob,
        version: 2,
        tabs: [{ ...(blob.tabs as Array<Record<string, unknown>>)[0], path: "/r/step-one" }],
      }),
      2: (blob) => ({
        ...blob,
        version: 3,
        tabs: [{ ...(blob.tabs as Array<Record<string, unknown>>)[0], path: "/r/step-two" }],
      }),
    };
    const migrated = loadMigrated({ ...v1Blob }, opts, migrations, 3);
    expect(migrated?.tabs.map((tab) => tab.path)).toEqual(["/r/step-two"]);
    expect(migrated?.activePath).toBe("/r/step-two");
  });

  it("rejects a blob whose version has no registered migration below target", () => {
    // A blob claiming version 0 with no step registered for it cannot be
    // upgraded; it must fall back rather than skip ahead.
    expect(loadMigrated({ ...v1Blob, version: 0 }, opts)).toBeNull();
    // A missing step mid-chain aborts too.
    expect(loadMigrated({ ...v1Blob }, opts, {}, 2)).toBeNull();
  });

  it("falls back to legacy recovery for a FUTURE version and warns exactly once", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const storage = memoryStorage({
      [STORAGE_KEY_WORKSPACE]: JSON.stringify({ ...v1Blob, version: 99 }),
      [STORAGE_KEY_RECENT]: JSON.stringify(["/r/legacy"]),
    });

    // Future blobs are never migrated down or wiped silently: the loader
    // ignores the blob but recovers what the legacy keys still hold.
    expect(loadPersistedWorkspace(storage, opts).tabs).toEqual([]);
    expect(loadPersistedWorkspace(storage, opts).recents).toEqual(["/r/legacy"]);
    expect(warn).toHaveBeenCalledTimes(1);
    expect(String(warn.mock.calls[0]?.[0])).toContain("99");
  });

  it("falls back silently for unreadable versions without touching the warn path", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const storage = memoryStorage({
      [STORAGE_KEY_WORKSPACE]: JSON.stringify({ ...v1Blob, version: "garbage" }),
    });
    expect(loadPersistedWorkspace(storage, opts).tabs).toEqual([]);
    expect(warn).not.toHaveBeenCalled();
  });

  it("treats a v1 blob with no tabs array like absent state (legacy recovery)", () => {
    const storage = memoryStorage({
      [STORAGE_KEY_WORKSPACE]: JSON.stringify({ version: 1, note: "half-written" }),
      [STORAGE_KEY_LAST_PATH]: "/r/recovered",
    });
    const loaded = loadPersistedWorkspace(storage, opts);
    expect(loaded.tabs.map((tab) => tab.path)).toEqual(["/r/recovered"]);
    expect(loaded.activePath).toBe("/r/recovered");
  });
});
