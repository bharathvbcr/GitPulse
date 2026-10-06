import { describe, expect, it } from "vitest";
import { computeTabLayout, stackKeyFor, type StackingOptions } from "./tabGroups";
import {
  activationStops,
  canMoveUnit,
  cycleStop,
  moveUnit,
  moveUnitToEdge,
  planDrop,
  unitForTab,
  unitIds,
} from "./stripNav";
import type { OpenRepoTab } from "../stores/repoStore";
import { checkout, repoTab } from "./__tests__/stripFixtures";

const ROOT = "/code/GitPulse";
const FAMILY = `${ROOT}/.git`;
const wt = (slug: string) => `${ROOT}/.claude/worktrees/${slug}`;
const STACK = stackKeyFor(null, FAMILY);

function layoutOf(tabs: OpenRepoTab[], stacking: StackingOptions = { enabled: true }, collapsed: string[] = []) {
  return computeTabLayout(tabs, collapsed, undefined, [], stacking);
}

/** Apply an order the way the store does: reorder the records, then redraw. */
function apply(tabs: OpenRepoTab[], order: string[] | null): OpenRepoTab[] {
  if (!order) return tabs;
  const byId = new Map(tabs.map((tab) => [tab.id, tab]));
  return order.map((id) => byId.get(id)!);
}

const strip = () => [
  repoTab("/code/A", { isActive: true }),
  checkout(ROOT, ROOT),
  repoTab("/code/B"),
  checkout(ROOT, wt("one")),
  checkout(ROOT, wt("two")),
  repoTab("/code/C"),
];

describe("moving units in drawn order", () => {
  it("moves a folded stack with every checkout in it", () => {
    const layout = layoutOf(strip());
    const unit = unitForTab(layout, wt("one"));
    expect(unit).toEqual({ kind: "stack", key: STACK });
    expect(unitIds(layout, unit!)).toEqual([ROOT, wt("one"), wt("two")]);
    const order = moveUnit(layout, unit!, 1);
    expect(order).toEqual(["/code/A", "/code/B", ROOT, wt("one"), wt("two"), "/code/C"]);
  });

  it("steps a tab past a folded stack in one move instead of into it", () => {
    // Drawn: A, {GitPulse:3}, B, C. Moving A right once lands after the stack.
    const layout = layoutOf(strip());
    expect(moveUnit(layout, { kind: "tab", id: "/code/A" }, 1)).toEqual([
      ROOT, wt("one"), wt("two"), "/code/A", "/code/B", "/code/C",
    ]);
  });

  it("is a fixed point: the stored order a move returns is exactly what is then drawn", () => {
    let tabs = strip();
    for (const step of [1, 1, -1, 1, -1, -1, -1]) {
      const layout = layoutOf(tabs);
      const order = moveUnit(layout, { kind: "stack", key: STACK }, step);
      tabs = apply(tabs, order);
      expect(layoutOf(tabs).order).toEqual(tabs.map((t) => t.id));
    }
  });

  it("keeps an unfolded checkout among its own siblings", () => {
    const layout = layoutOf(strip(), { enabled: true, expanded: [STACK] });
    const unit = unitForTab(layout, wt("one"));
    expect(unit).toEqual({ kind: "tab", id: wt("one") });
    expect(moveUnit(layout, unit!, -1)).toEqual(["/code/A", wt("one"), ROOT, wt("two"), "/code/B", "/code/C"]);
    // The last checkout cannot step out of the stack.
    const last = { kind: "tab" as const, id: wt("two") };
    expect(canMoveUnit(layout, last, 1)).toBe(false);
    expect(moveUnit(layout, last, 1)).toBeNull();
    expect(moveUnitToEdge(layout, last, "start")).toEqual(["/code/A", wt("two"), ROOT, wt("one"), "/code/B", "/code/C"]);
  });

  it("refuses non-integer, zero and out-of-range steps", () => {
    const layout = layoutOf(strip());
    const a = { kind: "tab" as const, id: "/code/A" };
    for (const delta of [0, 0.5, Number.NaN, Infinity, -1, -5, 99]) {
      expect(moveUnit(layout, a, delta)).toBeNull();
    }
    expect(moveUnit(layout, { kind: "tab", id: "/nope" }, 1)).toBeNull();
    expect(moveUnit(layout, { kind: "stack", key: "nope" }, 1)).toBeNull();
    expect(moveUnitToEdge(layout, a, "start")).toBeNull();
  });

  it("moves a group member only within its group", () => {
    const tabs = [
      repoTab("/g/a", { group: "g" }),
      repoTab("/x"),
      repoTab("/g/b", { group: "g" }),
    ];
    const layout = layoutOf(tabs);
    // Drawn: [g] a b, x. b is last in its group, so it cannot move right.
    expect(canMoveUnit(layout, { kind: "tab", id: "/g/b" }, 1)).toBe(false);
    expect(moveUnit(layout, { kind: "tab", id: "/g/b" }, -1)).toEqual(["/g/b", "/g/a", "/x"]);
  });
});

describe("planning drops", () => {
  it("reorders within a container and reports nothing for a drop that changes nothing", () => {
    const layout = layoutOf(strip());
    const a = { kind: "tab" as const, id: "/code/A" };
    const c = { kind: "tab" as const, id: "/code/C" };
    expect(planDrop(layout, a, c, false)?.order).toEqual([ROOT, wt("one"), wt("two"), "/code/B", "/code/C", "/code/A"]);
    expect(planDrop(layout, a, a, true)).toBeNull();
    // Dropping A on the left half of its right-hand neighbour is where it already is.
    expect(planDrop(layout, a, { kind: "stack", key: STACK }, true)).toBeNull();
  });

  it("refuses to drag a tab into a repository it is not a checkout of", () => {
    const layout = layoutOf(strip(), { enabled: true, expanded: [STACK] });
    expect(planDrop(layout, { kind: "tab", id: "/code/A" }, { kind: "tab", id: wt("one") }, true)).toBeNull();
  });

  it("refuses to drag a checkout out of its stack within the same group", () => {
    const layout = layoutOf(strip(), { enabled: true, expanded: [STACK] });
    expect(planDrop(layout, { kind: "tab", id: wt("one") }, { kind: "tab", id: "/code/C" }, false)).toBeNull();
  });

  it("moves a tab into the group it is dropped among, next to the tab it was dropped on", () => {
    const tabs = [repoTab("/g/a", { group: "g" }), repoTab("/g/b", { group: "g" }), repoTab("/x")];
    const layout = layoutOf(tabs);
    const plan = planDrop(layout, { kind: "tab", id: "/x" }, { kind: "tab", id: "/g/a" }, false);
    expect(plan).toEqual({ order: ["/g/a", "/x", "/g/b"], group: "g" });
    const out = planDrop(layout, { kind: "tab", id: "/g/b" }, { kind: "tab", id: "/x" }, false);
    expect(out).toEqual({ order: ["/g/a", "/x", "/g/b"], group: null });
  });

  it("carries a whole folded stack into another group", () => {
    const tabs = [...strip(), repoTab("/g/a", { group: "g" })];
    const layout = layoutOf(tabs);
    const plan = planDrop(layout, { kind: "stack", key: STACK }, { kind: "tab", id: "/g/a" }, true);
    expect(plan?.group).toBe("g");
    expect(plan?.order.slice(-4)).toEqual([ROOT, wt("one"), wt("two"), "/g/a"]);
  });

  it("lets an unfolded checkout leave for another group, where it stacks with its family", () => {
    const tabs = [...strip(), repoTab("/g/a", { group: "g" })];
    const layout = layoutOf(tabs, { enabled: true, expanded: [STACK] });
    const plan = planDrop(layout, { kind: "tab", id: wt("two") }, { kind: "tab", id: "/g/a" }, false);
    expect(plan?.group).toBe("g");
    expect(plan?.order.at(-1)).toBe(wt("two"));
  });
});

describe("activation stops", () => {
  it("counts a folded stack once, as the checkout it shows", () => {
    const layout = layoutOf(strip(), { enabled: true, lastUsed: new Map([[FAMILY, wt("two")]]) });
    expect(activationStops(layout)).toEqual(["/code/A", wt("two"), "/code/B", "/code/C"]);
  });

  it("counts every checkout of an unfolded stack, and every tab of a collapsed group", () => {
    const tabs = [...strip(), repoTab("/g/a", { group: "g" }), repoTab("/g/b", { group: "g" })];
    const layout = layoutOf(tabs, { enabled: true, expanded: [STACK] }, ["g"]);
    expect(activationStops(layout)).toEqual([
      "/code/A", ROOT, wt("one"), wt("two"), "/code/B", "/code/C", "/g/a", "/g/b",
    ]);
  });

  it("cycles through stops, wrapping, and skips the hidden checkouts of a folded stack", () => {
    const tabs = strip().map((t) => ({ ...t, isActive: t.id === ROOT }));
    const layout = layoutOf(tabs);
    expect(cycleStop(layout, ROOT, 1)).toBe("/code/B");
    expect(cycleStop(layout, ROOT, -1)).toBe("/code/A");
    expect(cycleStop(layout, "/code/C", 1)).toBe("/code/A");
    expect(cycleStop(layout, null, 1)).toBe("/code/A");
    expect(cycleStop(layoutOf([repoTab("/only")]), "/only", 1)).toBeNull();
  });
});
