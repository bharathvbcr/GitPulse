import { describe, expect, it } from "vitest";
import { arrangeTabs, emptyWorkspace, groupByParentFolder, openTab, type WorkspaceTabs } from "./tabModel";

const options = { caseInsensitive: false };

function workspace(paths: string[]): WorkspaceTabs {
  let ws = emptyWorkspace();
  for (const path of paths) {
    const result = openTab(ws, path, options);
    if (!result.ok) throw new Error(`could not open ${path}`);
    ws = result.workspace;
  }
  return ws;
}

describe("arrangeTabs", () => {
  const ws = workspace(["/a", "/b", "/c"]);

  it("puts the tabs in exactly the given order and keeps every other field", () => {
    const next = arrangeTabs(ws, ["/c", "/a", "/b"]);
    expect(next.tabs.map((t) => t.id)).toEqual(["/c", "/a", "/b"]);
    expect(next.activeId).toBe(ws.activeId);
    expect(next.recents).toBe(ws.recents);
  });

  it("returns the same workspace for the current order, so callers can detect a no-op", () => {
    expect(arrangeTabs(ws, ["/a", "/b", "/c"])).toBe(ws);
  });

  it("refuses anything that is not a permutation of the open tabs", () => {
    for (const bad of [
      [],
      ["/a", "/b"],
      ["/a", "/b", "/c", "/d"],
      ["/a", "/a", "/b"],
      ["/a", "/b", "/stale"],
    ]) {
      expect(arrangeTabs(ws, bad)).toBe(ws);
    }
  });
});

describe("groupByParentFolder with repository roots", () => {
  it("groups an agent worktree with the repository it belongs to, not in a 'worktrees' group", () => {
    const main = "/code/devtools/GitPulse";
    const agent = `${main}/.claude/worktrees/handoff-fix`;
    const ws = workspace([main, "/code/web/site", agent]);
    const roots = new Map([[main, main], [agent, main]]);
    const grouped = groupByParentFolder(ws, (tab) => roots.get(tab.path));
    const groupOf = (path: string) => grouped.tabs.find((t) => t.path === path)?.group;
    expect(groupOf(main)).toBe("devtools");
    expect(groupOf(agent)).toBe("devtools");
    expect(groupOf("/code/web/site")).toBe("web");
    // Clustered: the worktree is drawn beside its repository.
    expect(grouped.tabs.map((t) => t.path)).toEqual([main, agent, "/code/web/site"]);
  });

  it("falls back to the tab's own parent when the root is unknown", () => {
    const agent = "/code/devtools/GitPulse/.claude/worktrees/x";
    const grouped = groupByParentFolder(workspace([agent]), () => null);
    expect(grouped.tabs[0].group).toBe("worktrees");
  });
});
