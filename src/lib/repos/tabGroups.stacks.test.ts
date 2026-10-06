import { describe, expect, it } from "vitest";
import { computeTabLayout, stackKeyFor, type StackingOptions, type TabLayoutItem } from "./tabGroups";
import { checkout, repoTab } from "./__tests__/stripFixtures";

const ROOT = "/code/GitPulse";
const wt = (slug: string) => `${ROOT}/.claude/worktrees/${slug}`;
const ON: StackingOptions = { enabled: true };

function drawn(items: readonly TabLayoutItem[]): string[] {
  return items.map((item) =>
    item.kind === "group-header"
      ? `[${item.group}]`
      : item.kind === "stack-header"
        ? `{${item.label}:${item.tabCount}}`
        : item.tab.label,
  );
}

describe("worktree stacks in the strip layout", () => {
  const tabs = () => [
    checkout(ROOT, ROOT, { isActive: true, currentBranch: "main" }),
    repoTab("/code/Other"),
    checkout(ROOT, wt("handoff-fix"), { currentBranch: "fix/handoff" }),
    checkout(ROOT, wt("agent-models"), { isDirty: true, conflictedCount: 2 }),
  ];

  it("draws one header for every checkout of a repository instead of a tab each", () => {
    const layout = computeTabLayout(tabs(), [], undefined, [], ON);
    // Four tabs, two things on the strip: the stack, then the unrelated repo.
    expect(drawn(layout.visibleItems)).toEqual(["{GitPulse:3}", "Other"]);
    expect(layout.stacks).toHaveLength(1);
    expect(layout.visibleTabs.map((t) => t.label)).toEqual(["Other"]);
    // Drawn order pulls the stacked worktree next to its repository.
    expect(layout.order).toEqual([ROOT, wt("handoff-fix"), wt("agent-models"), "/code/Other"]);
  });

  it("changes nothing at all when stacking is off (the old strip, byte for byte)", () => {
    const off = computeTabLayout(tabs(), [], undefined, [], { enabled: false });
    const legacy = computeTabLayout(tabs(), []);
    expect(off.stacks).toEqual([]);
    expect(drawn(off.visibleItems)).toEqual(["GitPulse", "Other", "handoff-fix", "agent-models"]);
    expect(drawn(legacy.visibleItems)).toEqual(drawn(off.visibleItems));
    expect(off.order).toEqual(tabs().map((t) => t.id));
  });

  it("folds aggregates across checkouts so a hidden worktree's trouble still shows", () => {
    const counts = new Map([[wt("handoff-fix"), 2], [ROOT, 1]]);
    const [stack] = computeTabLayout(tabs(), [], counts, [], ON).stacks;
    expect(stack.isDirty).toBe(true);
    expect(stack.conflictedCount).toBe(2);
    expect(stack.terminalCount).toBe(3);
    expect(stack.hasActiveTab).toBe(true);
    expect(stack.tabIds).toEqual([ROOT, wt("handoff-fix"), wt("agent-models")]);
  });

  it("shows the active checkout, else the last used, else the primary, else the first", () => {
    const base = tabs().map((t) => ({ ...t, isActive: false }));
    const pick = (list: typeof base, lastUsed?: Map<string, string>) =>
      computeTabLayout(list, [], undefined, [], { enabled: true, lastUsed }).stacks[0].current.id;

    const active = base.map((t) => (t.id === wt("agent-models") ? { ...t, isActive: true } : t));
    expect(pick(active, new Map([[`${ROOT}/.git`, wt("handoff-fix")]]))).toBe(wt("agent-models"));
    expect(pick(base, new Map([[`${ROOT}/.git`, wt("handoff-fix")]]))).toBe(wt("handoff-fix"));
    // A remembered id that is no longer a member is ignored, not trusted.
    expect(pick(base, new Map([[`${ROOT}/.git`, "/gone"]]))).toBe(ROOT);
    const noPrimary = base.filter((t) => t.id !== ROOT);
    expect(pick(noPrimary)).toBe(wt("handoff-fix"));
  });

  it("unfolds into a header followed by each checkout's own tab", () => {
    const key = stackKeyFor(null, `${ROOT}/.git`);
    const layout = computeTabLayout(tabs(), [], undefined, [], { enabled: true, expanded: [key] });
    expect(drawn(layout.visibleItems)).toEqual(["{GitPulse:3}", "GitPulse", "handoff-fix", "agent-models", "Other"]);
    expect(layout.stacks[0].isExpanded).toBe(true);
  });

  it("leaves a lone checkout as an ordinary tab: a stack of one is noise", () => {
    const layout = computeTabLayout(
      [checkout(ROOT, wt("solo")), repoTab("/code/Other")],
      [],
      undefined,
      [],
      ON,
    );
    expect(layout.stacks).toEqual([]);
    expect(drawn(layout.visibleItems)).toEqual(["solo", "Other"]);
  });

  it("never stacks tabs whose family is unknown, even when their paths look related", () => {
    const layout = computeTabLayout(
      [repoTab(ROOT), repoTab(wt("a")), repoTab(wt("b"))],
      [],
      undefined,
      [],
      ON,
    );
    expect(layout.stacks).toEqual([]);
    expect(layout.visibleItems).toHaveLength(3);
  });

  it("lets a person's own group win: a family split across groups stacks once in each", () => {
    const list = [
      checkout(ROOT, ROOT, { group: "core" }),
      checkout(ROOT, wt("a"), { group: "core" }),
      checkout(ROOT, wt("b"), { group: "agents" }),
      checkout(ROOT, wt("c"), { group: "agents" }),
      checkout(ROOT, wt("d")),
    ];
    const layout = computeTabLayout(list, [], undefined, [], ON);
    expect(drawn(layout.visibleItems)).toEqual(["[core]", "{GitPulse:2}", "[agents]", "{GitPulse:2}", "d"]);
    const keys = layout.stacks.map((s) => s.key);
    expect(new Set(keys).size).toBe(2);
    expect(layout.stacks.map((s) => s.group)).toEqual(["core", "agents"]);
  });

  it("hides a stack inside a collapsed group but keeps the group header and its counts", () => {
    const list = [
      checkout(ROOT, ROOT, { group: "core", isDirty: true }),
      checkout(ROOT, wt("a"), { group: "core" }),
      repoTab("/code/Other"),
    ];
    const layout = computeTabLayout(list, ["core"], undefined, [], ON);
    expect(drawn(layout.visibleItems)).toEqual(["[core]", "Other"]);
    expect(layout.stacks[0].isInsideCollapsedGroup).toBe(true);
    expect(layout.groups[0].isDirty).toBe(true);
  });

  it("does not confuse a group named like a repository with that repository's stack", () => {
    const list = [
      checkout(ROOT, ROOT, { group: "GitPulse" }),
      checkout(ROOT, wt("a"), { group: "GitPulse" }),
    ];
    const layout = computeTabLayout(list, [], undefined, [], ON);
    expect(layout.allItems.map((i) => i.id)).toEqual([
      "group:GitPulse",
      `stack:${stackKeyFor("GitPulse", `${ROOT}/.git`)}`,
      ROOT,
      wt("a"),
    ]);
    expect(new Set(layout.allItems.map((i) => i.id)).size).toBe(layout.allItems.length);
  });

  it("gives two unrelated repositories that share a name distinct headers", () => {
    const list = [
      checkout("/work/api", "/work/api"),
      checkout("/work/api", "/work/api-wt"),
      checkout("/home/api", "/home/api"),
      checkout("/home/api", "/home/api/.claude/worktrees/x"),
    ];
    const labels = computeTabLayout(list, [], undefined, [], ON).stacks.map((s) => s.label);
    expect(labels).toEqual(["work/api", "home/api"]);
  });
});
