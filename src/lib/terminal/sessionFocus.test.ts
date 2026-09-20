import { describe, expect, it, vi } from "vitest";
import { focusTerminalSession, type RepoFocusActions } from "./sessionFocus";

/**
 * A repository store stub that RECORDS THE ORDER of what it was asked to do.
 *
 * The order is the contract: the dock is per repository tab, so opening it
 * before the switch opens it on the repository the user is leaving, and
 * revealing before the panel renders focuses a terminal nobody can see.
 * Asserting the final state alone would pass for all six orderings.
 */
function stub(
  openTabs: { id: string; path: string }[],
  activeTabId: string | null,
  overrides: Partial<RepoFocusActions> = {},
) {
  const calls: string[] = [];
  let active = activeTabId;
  let dockOpen = false;
  const actions: RepoFocusActions = {
    snapshot: () => ({ openTabs, activeTabId: active }),
    activateTab: (id) => {
      calls.push(`activate:${id}`);
      active = id;
    },
    setTerminalOpen: (open) => {
      calls.push(`dock:${open}`);
      const changed = dockOpen !== open;
      dockOpen = open;
      return changed;
    },
    openRepo: (path) => {
      calls.push(`open:${path}`);
      return true;
    },
    afterRender: async () => {
      calls.push("render");
    },
    ...overrides,
  };
  return { actions, calls, dockOpenNow: () => dockOpen };
}

const TABS = [
  { id: "alpha", path: "/r/alpha" },
  { id: "beta", path: "/r/beta" },
];

describe("focusTerminalSession", () => {
  it("switches repository, opens that tab's dock, then reveals — in that order", async () => {
    const { actions, calls } = stub(TABS, "alpha");
    const reveal = vi.fn(() => void calls.push("reveal"));

    const outcome = await focusTerminalSession({ repoPath: "/r/beta", reveal }, actions);

    expect(outcome).toEqual({ ok: true, switchedRepo: true, openedDock: true });
    // The whole contract in one assertion. The dock must open AFTER the
    // activation — `setTerminalOpen` acts on whichever tab is active, so the
    // reverse order opens the terminal on /r/alpha, the repository the user
    // just left, and leaves /r/beta's dock shut. And the reveal must come
    // after a render, or it focuses an xterm that is still hidden.
    expect(calls).toEqual(["activate:beta", "dock:true", "render", "reveal"]);
    expect(reveal).toHaveBeenCalledOnce();
  });

  it("reveals without switching when the session is in the repository already in front", async () => {
    const reveal = vi.fn();
    const { actions, calls } = stub(TABS, "alpha");

    const outcome = await focusTerminalSession({ repoPath: "/r/alpha", reveal }, actions);

    expect(outcome).toEqual({ ok: true, switchedRepo: false, openedDock: true });
    expect(calls).not.toContain("activate:alpha");
    expect(reveal).toHaveBeenCalledOnce();
  });

  it("reports an already-showing session as a no-op rather than an open", async () => {
    const reveal = vi.fn();
    const { actions } = stub(TABS, "alpha", {});
    // First jump opens the dock; the second finds it already open.
    await focusTerminalSession({ repoPath: "/r/alpha", reveal }, actions);
    const second = await focusTerminalSession({ repoPath: "/r/alpha", reveal }, actions);

    expect(second).toEqual({ ok: true, switchedRepo: false, openedDock: false });
    expect(reveal).toHaveBeenCalledTimes(2);
  });

  it("reopens a repository whose tab is gone before revealing", async () => {
    const reveal = vi.fn();
    const { actions, calls } = stub(TABS, "alpha");

    const outcome = await focusTerminalSession({ repoPath: "/r/gamma", reveal }, actions);

    expect(outcome).toEqual({ ok: true, switchedRepo: true, openedDock: true });
    expect(calls.indexOf("open:/r/gamma")).toBeLessThan(calls.indexOf("dock:true"));
    expect(reveal).toHaveBeenCalledOnce();
  });

  it("does not open a dock for a repository it could not reopen", async () => {
    const reveal = vi.fn();
    const { actions, calls, dockOpenNow } = stub(TABS, "alpha", { openRepo: () => false });

    const outcome = await focusTerminalSession({ repoPath: "/r/gone", reveal }, actions);

    expect(outcome).toEqual({ ok: false, reason: "unavailable" });
    // Failing closed matters: opening the dock here would start a shell in
    // whichever repository happened to be in front.
    expect(calls).not.toContain("dock:true");
    expect(dockOpenNow()).toBe(false);
    expect(reveal).not.toHaveBeenCalled();
  });

  it("refuses a record with no reveal before moving anything", async () => {
    const { actions, calls, dockOpenNow } = stub(TABS, "alpha");
    const outcome = await focusTerminalSession({ repoPath: "/r/beta" }, actions);
    expect(outcome).toEqual({ ok: false, reason: "unavailable" });
    // A session that can never be shown must not leave the user in a
    // different repository with a dock — and so a shell — opened on the way
    // to finding that out.
    expect(calls).toEqual([]);
    expect(dockOpenNow()).toBe(false);
  });

  it("treats a missing record as no session, touching nothing", async () => {
    const { actions, calls } = stub(TABS, "alpha");
    expect(await focusTerminalSession(null, actions)).toEqual({ ok: false, reason: "no-session" });
    expect(await focusTerminalSession(undefined, actions)).toEqual({ ok: false, reason: "no-session" });
    expect(calls).toEqual([]);
  });

  it("reads the active tab fresh, so a stale snapshot cannot skip the switch", async () => {
    const reveal = vi.fn();
    // Active tab is beta at call time even though the list was built earlier.
    const { actions, calls } = stub(TABS, "beta");
    await focusTerminalSession({ repoPath: "/r/alpha", reveal }, actions);
    expect(calls).toContain("activate:alpha");
  });
});
