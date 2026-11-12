import { describe, expect, it } from "vitest";
import type { BranchInfo } from "../branches/types";
import { createSession, project } from "./projection";
import type { InternalState } from "./storeTypes";

// The tab hover card reads ahead/behind and the last fetch from the hovered
// tab's own session, so any tab that has been read answers for itself and one
// that has not says so (undefined / null), instead of only the active tab.

const branch = (over: Partial<BranchInfo>): BranchInfo => ({
  name: "main",
  is_current: true,
  is_remote: false,
  tip_commit_id: "abc",
  ahead_count: 0,
  behind_count: 0,
  is_default: true,
  is_gone: false,
  last_commit_timestamp: 0,
  last_author: "",
  last_summary: "",
  commits_ahead_of_base: 0,
  ...over,
} as BranchInfo);

function stateWith(sessions: ReturnType<typeof createSession>[], activeId: string): InternalState {
  return {
    workspace: {
      tabs: sessions.map((session) => ({ id: session.id, path: session.path, pinned: false })),
      activeId,
      recents: [],
      lastClosed: [],
    },
    sessions: Object.fromEntries(sessions.map((session) => [session.id, session])),
    workspaceError: null,
  };
}

describe("a tab's own sync and last fetch", () => {
  it("projects them for a background tab that has been read", () => {
    const active = createSession({ id: "a", path: "/code/a", pinned: false }, { hasHydrated: true, currentBranch: "main", branches: [branch({})] });
    const background = createSession(
      { id: "b", path: "/code/b", pinned: false },
      {
        hasHydrated: true,
        currentBranch: "dev",
        branches: [branch({ name: "dev", ahead_count: 3, behind_count: 1 })],
        fetchedAt: 1_700_000_000_000,
      },
    );
    const state = project(stateWith([active, background], "a"), { caseInsensitive: false });
    const row = state.openTabs.find((tab) => tab.id === "b");
    expect(row?.isActive).toBe(false);
    expect(row?.sync).toEqual({ ahead: 3, behind: 1 });
    expect(row?.fetchedAt).toBe(1_700_000_000_000);
  });

  it("says unread, not clean, for a tab that has not been read", () => {
    const active = createSession({ id: "a", path: "/code/a", pinned: false }, { hasHydrated: true });
    const parked = createSession({ id: "p", path: "/code/p", pinned: false }, { hasHydrated: false });
    const state = project(stateWith([active, parked], "a"), { caseInsensitive: false });
    const row = state.openTabs.find((tab) => tab.id === "p");
    expect(row?.sync).toBeNull();
    expect(row?.fetchedAt).toBeUndefined();
  });

  it("keeps a recorded absence of fetch distinct from unread", () => {
    const read = createSession({ id: "a", path: "/code/a", pinned: false }, { hasHydrated: true, fetchedAt: null, currentBranch: "main", branches: [branch({})] });
    const row = project(stateWith([read], "a"), { caseInsensitive: false }).openTabs[0];
    expect(row.fetchedAt).toBeNull();
    expect(row.sync).toEqual({ ahead: 0, behind: 0 });
  });
});
