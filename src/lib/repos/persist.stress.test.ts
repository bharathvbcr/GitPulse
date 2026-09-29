import { describe, expect, it } from "vitest";
import {
  coalescePersistedWorkspace,
  loadPersistedWorkspace,
  memoryStorage,
  savePersistedWorkspace,
  STORAGE_KEY_WORKSPACE,
  STORAGE_KEY_WORKSPACE_BACKUP,
  workspaceToPersisted,
  type PersistedWorkspace,
} from "./persist";
import { emptyWorkspace, openTab, setTabGroup } from "./tabModel";

const opts = { caseInsensitive: true };

function workspace(paths: string[], epoch: number, groupFor: (path: string) => string | null = () => null): PersistedWorkspace {
  let ws = emptyWorkspace();
  for (const path of paths) {
    const opened = openTab(ws, path, opts, { group: groupFor(path), activate: false });
    if (!opened.ok) throw new Error(opened.reason);
    ws = opened.workspace;
  }
  return { ...workspaceToPersisted(ws, {}), epoch, collapsedGroups: [] };
}

describe("workspace persistence under hostile snapshots", () => {
  it("refuses a same-epoch snapshot that drops half the tabs and their groups", () => {
    const full = workspace(
      ["/r/a", "/r/b", "/r/c", "/r/d"],
      4,
      (path) => (path.endsWith("a") || path.endsWith("b") ? "devtools" : "web"),
    );
    full.collapsedGroups = ["web"];
    const storage = memoryStorage();
    expect(savePersistedWorkspace(storage, full, [], opts)).toBe(true);

    const half = workspace(["/r/a", "/r/b"], 4, () => null);
    expect(savePersistedWorkspace(storage, half, [], opts)).toBe(true);

    const loaded = loadPersistedWorkspace(storage, opts);
    expect(loaded.tabs.map((tab) => tab.path)).toEqual(["/r/a", "/r/b", "/r/c", "/r/d"]);
    expect(loaded.tabs.map((tab) => tab.group ?? null)).toEqual(["devtools", "devtools", "web", "web"]);
    expect(loaded.collapsedGroups).toEqual(["web"]);
  });

  it("lets a newer epoch close tabs it names, and puts back any it merely forgot", () => {
    const full = workspace(["/r/a", "/r/b", "/r/c", "/r/d"], 2, () => "devtools");
    const closed = workspace(["/r/a", "/r/c"], 3, () => "devtools");
    const kept = coalescePersistedWorkspace(full, closed, opts, ["/r/b"]);
    expect(kept.tabs.map((tab) => tab.path).sort()).toEqual(["/r/a", "/r/c", "/r/d"]);
    expect(kept.tabs.every((tab) => tab.group === "devtools")).toBe(true);
  });

  it("clears groups only when the epoch advanced", () => {
    const grouped = workspace(["/r/a", "/r/b"], 1, () => "devtools");
    const stripped = workspace(["/r/a", "/r/b"], 1, () => null);
    expect(coalescePersistedWorkspace(grouped, stripped, opts).tabs.every((tab) => tab.group === "devtools")).toBe(true);

    const ungrouped = workspace(["/r/a", "/r/b"], 2, () => null);
    expect(coalescePersistedWorkspace(grouped, ungrouped, opts).tabs.every((tab) => tab.group == null)).toBe(true);
  });

  it("falls back to the backup when the primary blob does not parse", () => {
    const storage = memoryStorage();
    const first = workspace(["/r/a", "/r/b"], 1, () => "devtools");
    savePersistedWorkspace(storage, first, [], opts);
    const second = workspace(["/r/a", "/r/b", "/r/c"], 2, () => "devtools");
    savePersistedWorkspace(storage, second, [], opts);
    storage.setItem(STORAGE_KEY_WORKSPACE, "{not json");
    const loaded = loadPersistedWorkspace(storage, opts);
    expect(loaded.tabs.map((tab) => tab.path)).toEqual(["/r/a", "/r/b"]);
    expect(storage.getItem(STORAGE_KEY_WORKSPACE_BACKUP)).toBeTruthy();
  });

  it("keeps every tab across random partial, duplicate, and case-variant overwrites", () => {
    const paths = Array.from({ length: 16 }, (_, index) => `/repo/${index}`);
    let current = workspace(paths, 1, (path) => `g${Number(path.split("/").pop()) % 4}`);
    const storage = memoryStorage();
    savePersistedWorkspace(storage, current, [], opts);

    let seed = 17;
    const rand = () => {
      seed = (seed * 16807) % 2147483647;
      return seed / 2147483647;
    };

    for (let round = 0; round < 40; round += 1) {
      const keep = Math.max(1, Math.floor(rand() * paths.length));
      const subset = paths.filter(() => rand() > 0.5).slice(0, keep);
      const attack = workspace(
        subset.length > 0 ? subset : [paths[0]],
        1,
        () => (rand() > 0.5 ? null : "nope"),
      );
      attack.tabs.push({ ...attack.tabs[0], path: attack.tabs[0].path.toUpperCase() });
      savePersistedWorkspace(storage, attack, [paths[Math.floor(rand() * paths.length)]], opts);
      current = loadPersistedWorkspace(storage, opts);
      expect(current.tabs).toHaveLength(paths.length);
      expect(current.tabs.every((tab) => tab.group)).toBe(true);
      const ids = current.tabs.map((tab) => tab.path.toLowerCase());
      expect(new Set(ids).size).toBe(ids.length);
    }
  });

  it("round-trips a grouped workspace built through the tab model", () => {
    let ws = emptyWorkspace();
    for (const path of ["/projects/devtools/alpha", "/projects/devtools/beta", "/projects/web/gamma"]) {
      const opened = openTab(ws, path, opts);
      if (!opened.ok) throw new Error("open");
      ws = opened.workspace;
    }
    ws = setTabGroup(ws, ws.tabs[0].id, "devtools");
    ws = setTabGroup(ws, ws.tabs[1].id, "devtools");
    ws = setTabGroup(ws, ws.tabs[2].id, "web");
    ws = { ...ws, collapsedGroups: ["web"] };
    const storage = memoryStorage();
    savePersistedWorkspace(storage, workspaceToPersisted(ws, {}, 3), [], opts);
    const loaded = loadPersistedWorkspace(storage, opts);
    expect(loaded.tabs.map((tab) => [tab.path, tab.group])).toEqual([
      ["/projects/devtools/alpha", "devtools"],
      ["/projects/devtools/beta", "devtools"],
      ["/projects/web/gamma", "web"],
    ]);
    expect(loaded.collapsedGroups).toEqual(["web"]);
    expect(loaded.epoch).toBe(3);
  });
});
