import { describe, expect, it } from "vitest";
import {
  activateAt,
  activateNext,
  activatePrev,
  activateTab,
  assertWorkspaceInvariants,
  closeOtherTabs,
  closeTab,
  closeTabsToTheRight,
  emptyWorkspace,
  MAX_OPEN_TABS,
  openTab,
  pinTab,
  rememberRecent,
  removeRecent,
  reopenLastClosed,
  reorderTab,
  type WorkspaceTabs,
} from "./tabModel";

const opts = { caseInsensitive: true };

function mustOpen(ws: WorkspaceTabs, path: string, pinned = false) {
  const result = openTab(ws, path, opts, { pinned });
  expect(result.ok).toBe(true);
  if (!result.ok) throw new Error("open failed");
  assertWorkspaceInvariants(result.workspace, opts);
  return result;
}

describe("openTab", () => {
  it("rejects invalid paths without mutating the workspace", () => {
    const ws = emptyWorkspace();
    const result = openTab(ws, "   ", opts);
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.reason).toBe("invalid");
    expect(result.workspace).toBe(ws);
  });

  it("activates an existing tab instead of duplicating slash/case variants", () => {
    let ws = emptyWorkspace();
    ws = mustOpen(ws, "/Users/acme/gitpulse").workspace;
    const again = mustOpen(ws, "/Users/acme/gitpulse/");
    expect(again.created).toBe(false);
    expect(again.workspace.tabs).toHaveLength(1);
    expect(again.workspace.activeId).toBe(again.id);
  });

  it("fails closed at the tab cap instead of evicting", () => {
    let ws = emptyWorkspace();
    for (let i = 0; i < MAX_OPEN_TABS; i += 1) {
      ws = mustOpen(ws, `/repos/project-${i}`).workspace;
    }
    const overflow = openTab(ws, "/repos/overflow", opts);
    expect(overflow.ok).toBe(false);
    if (overflow.ok) return;
    expect(overflow.reason).toBe("capacity");
    expect(overflow.workspace.tabs).toHaveLength(MAX_OPEN_TABS);
  });
});

describe("closeTab", () => {
  it("activates the right neighbor, then the left, then empties", () => {
    let ws = emptyWorkspace();
    const a = mustOpen(ws, "/r/a");
    const b = mustOpen(a.workspace, "/r/b");
    const c = mustOpen(b.workspace, "/r/c");
    const closedB = closeTab(c.workspace, b.id);
    expect(closedB.workspace.activeId).toBe(c.id);
    const closedC = closeTab(closedB.workspace, c.id);
    expect(closedC.workspace.activeId).toBe(a.id);
    const closedA = closeTab(closedC.workspace, a.id);
    expect(closedA.workspace.tabs).toHaveLength(0);
    expect(closedA.workspace.activeId).toBeNull();
    expect(closedA.workspace.lastClosed[0]).toBe("/r/a");
  });

  it("is a no-op for an unknown id", () => {
    const ws = mustOpen(emptyWorkspace(), "/r/a").workspace;
    const result = closeTab(ws, "missing");
    expect(result.reason).toBe("missing");
    expect(result.workspace).toBe(ws);
  });
});

describe("activate and reorder", () => {
  it("wraps next/prev and clamps activateAt", () => {
    let ws = emptyWorkspace();
    const a = mustOpen(ws, "/r/a");
    const b = mustOpen(a.workspace, "/r/b");
    const c = mustOpen(b.workspace, "/r/c");
    ws = c.workspace;
    expect(activateNext(ws).activeId).toBe(a.id);
    ws = activatePrev(ws);
    expect(ws.activeId).toBe(b.id);
    expect(activateAt(ws, 99).activeId).toBe(c.id);
    expect(activateTab(ws, "nope")).toBe(ws);
  });

  it("reorders within bounds and ignores out-of-range indexes", () => {
    let ws = emptyWorkspace();
    const a = mustOpen(ws, "/r/a");
    const b = mustOpen(a.workspace, "/r/b");
    const c = mustOpen(b.workspace, "/r/c");
    ws = reorderTab(c.workspace, 0, 2);
    expect(ws.tabs.map((tab) => tab.path)).toEqual(["/r/b", "/r/c", "/r/a"]);
    expect(reorderTab(ws, -1, 1)).toBe(ws);
    expect(reorderTab(ws, 0, 0)).toBe(ws);
  });
});

describe("pin close-others and recents", () => {
  it("close others keeps the requested tab and records the rest", () => {
    let ws = emptyWorkspace();
    const a = mustOpen(ws, "/r/a");
    const b = mustOpen(a.workspace, "/r/b");
    const c = mustOpen(b.workspace, "/r/c");
    ws = closeOtherTabs(c.workspace, b.id);
    expect(ws.tabs).toHaveLength(1);
    expect(ws.tabs[0].id).toBe(b.id);
    expect(ws.lastClosed).toEqual(["/r/c", "/r/a"]);
  });

  it("close to the right drops trailing tabs and keeps the pivot active", () => {
    let ws = emptyWorkspace();
    const a = mustOpen(ws, "/r/a");
    const b = mustOpen(a.workspace, "/r/b");
    ws = mustOpen(b.workspace, "/r/c").workspace;
    ws = activateTab(ws, a.id);
    ws = closeTabsToTheRight(ws, a.id);
    expect(ws.tabs.map((tab) => tab.path)).toEqual(["/r/a"]);
    expect(ws.activeId).toBe(a.id);
    expect(ws.lastClosed).toEqual(["/r/c", "/r/b"]);
  });

  it("pin is a no-op for unknown ids and recents dedupe by identity", () => {
    let ws = mustOpen(emptyWorkspace(), "/r/GitPulse").workspace;
    expect(pinTab(ws, "missing", true)).toBe(ws);
    ws = pinTab(ws, ws.tabs[0].id, true);
    expect(ws.tabs[0].pinned).toBe(true);
    ws = rememberRecent(ws, "/r/gitpulse/", opts);
    expect(ws.recents).toEqual(["/r/gitpulse"]);
    ws = removeRecent(ws, "/R/GITPULSE", opts);
    expect(ws.recents).toEqual([]);
  });

  it("reopens the most recently closed tab", () => {
    let ws = mustOpen(emptyWorkspace(), "/r/a").workspace;
    const closed = closeTab(ws, ws.tabs[0].id);
    const reopened = reopenLastClosed(closed.workspace, opts);
    expect(reopened.ok).toBe(true);
    if (!reopened.ok) return;
    expect(reopened.workspace.tabs[0].path).toBe("/r/a");
    expect(reopened.workspace.lastClosed).toEqual([]);
  });
});

describe("adversarial stress", () => {
  it("keeps invariants across 5_000 random open/close/reorder/pin operations", () => {
    let ws = emptyWorkspace();
    const rng = mulberry32(0x51f1e7);
    for (let i = 0; i < 5_000; i += 1) {
      const roll = rng() % 9;
      if (roll === 0 || ws.tabs.length === 0) {
        const result = openTab(ws, `/stress/repo-${rng() % 40}`, opts, { pinned: rng() % 5 === 0 });
        if (result.ok) ws = result.workspace;
      } else if (roll === 1) {
        const victim = ws.tabs[rng() % ws.tabs.length];
        ws = closeTab(ws, victim.id).workspace;
      } else if (roll === 2) {
        ws = activateNext(ws);
      } else if (roll === 3) {
        ws = activatePrev(ws);
      } else if (roll === 4 && ws.tabs.length > 1) {
        ws = reorderTab(ws, rng() % ws.tabs.length, rng() % ws.tabs.length);
      } else if (roll === 5 && ws.tabs.length > 0) {
        const tab = ws.tabs[rng() % ws.tabs.length];
        ws = pinTab(ws, tab.id, !tab.pinned);
      } else if (roll === 6 && ws.tabs.length > 0) {
        ws = closeOtherTabs(ws, ws.tabs[rng() % ws.tabs.length].id);
      } else if (roll === 7 && ws.tabs.length > 0) {
        ws = closeTabsToTheRight(ws, ws.tabs[rng() % ws.tabs.length].id);
      } else if (ws.lastClosed.length > 0) {
        const reopened = reopenLastClosed(ws, opts);
        if (reopened.ok) ws = reopened.workspace;
      }
      assertWorkspaceInvariants(ws, opts);
    }
  });
});

function mulberry32(seed: number): () => number {
  let t = seed >>> 0;
  return () => {
    t += 0x6d2b79f5;
    let r = Math.imul(t ^ (t >>> 15), 1 | t);
    r ^= r + Math.imul(r ^ (r >>> 7), 61 | r);
    return ((r ^ (r >>> 14)) >>> 0) % 0x100000000;
  };
}
