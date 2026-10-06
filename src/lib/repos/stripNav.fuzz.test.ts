/**
 * Invariants of the strip under random workspaces and random operations.
 *
 * Every case is derived from a seed, and a failure message carries it, so a
 * red run reproduces exactly. The invariants are checked against facts the
 * test computes on its own (who belongs with whom, who is active), never
 * against the layout's own bookkeeping.
 */
import { describe, expect, it } from "vitest";
import { computeTabLayout, type StackingOptions, type TabLayoutResult } from "./tabGroups";
import {
  activationStops,
  canMoveUnit,
  cycleStop,
  moveUnit,
  moveUnitToEdge,
  planDrop,
  unitForTab,
  unitIds,
  type StripUnit,
} from "./stripNav";
import type { OpenRepoTab } from "../stores/repoStore";
import { checkout, repoTab, rng } from "./__tests__/stripFixtures";

const SEEDS = 400;
const ROOTS = ["/r/alpha", "/r/beta", "/r/gamma"];
const GROUPS = [null, null, "core", "agents"];

function randomWorkspace(seed: number) {
  const next = rng(seed);
  const pick = <T,>(list: readonly T[]): T => list[Math.floor(next() * list.length)];
  const count = 1 + Math.floor(next() * 14);
  const tabs: OpenRepoTab[] = [];
  for (let i = 0; i < count; i += 1) {
    const group = pick(GROUPS);
    const roll = next();
    if (roll < 0.2) {
      tabs.push(repoTab(`/solo/${i}`, { group }));
    } else {
      const root = pick(ROOTS);
      // Unknown family: resolved by nothing, so it must never stack.
      const known = next() > 0.1;
      const path = next() < 0.3 ? `${root}#${i}` : `${root}/.claude/worktrees/w${i}`;
      tabs.push(known ? checkout(root, path, { group }) : repoTab(path, { group }));
    }
  }
  const active = Math.floor(next() * tabs.length);
  tabs[active] = { ...tabs[active], isActive: true };
  const stacking: StackingOptions = {
    enabled: next() > 0.15,
    expanded: [],
    lastUsed: new Map(ROOTS.map((root) => [`${root}/.git`, pick(tabs).id])),
  };
  const collapsed = GROUPS.filter((g): g is string => g !== null && next() < 0.3);
  return { tabs, stacking, collapsed, next, pick };
}

function draw(tabs: OpenRepoTab[], stacking: StackingOptions, collapsed: string[]): TabLayoutResult {
  return computeTabLayout(tabs, collapsed, undefined, [], stacking);
}

function apply(tabs: OpenRepoTab[], order: string[], regroup?: { ids: string[]; group: string | null }) {
  const byId = new Map(tabs.map((tab) => [tab.id, tab]));
  const moved = new Set(regroup?.ids ?? []);
  return order.map((id) => {
    const tab = byId.get(id)!;
    return regroup && moved.has(id) ? { ...tab, group: regroup.group } : tab;
  });
}

function checkInvariants(seed: number, tabs: OpenRepoTab[], stacking: StackingOptions, collapsed: string[]) {
  const where = `seed ${seed}`;
  const layout = draw(tabs, stacking, collapsed);
  const ids = tabs.map((t) => t.id);

  // 1. Drawn order is a permutation: no tab lost, none drawn twice.
  expect([...layout.order].sort(), where).toEqual([...ids].sort());
  const tabItems = layout.allItems.filter((item) => item.kind === "tab");
  expect(tabItems.length, where).toBe(tabs.length);

  // 2. Open tabs never leave an empty strip.
  if (tabs.length > 0) expect(layout.visibleItems.length, where).toBeGreaterThan(0);

  // 3. Stacks are exactly the (group, family) pairs with two or more members.
  const pairs = new Map<string, string[]>();
  if (stacking.enabled) {
    for (const tab of tabs) {
      if (!tab.family) continue;
      const key = `${tab.group ?? ""}\u0001${tab.family}`;
      pairs.set(key, [...(pairs.get(key) ?? []), tab.id]);
    }
  }
  const expected = [...pairs.entries()].filter(([, members]) => members.length >= 2);
  expect(layout.stacks.map((s) => s.key).sort(), where).toEqual(expected.map(([k]) => k).sort());
  for (const stack of layout.stacks) {
    const members = pairs.get(stack.key)!;
    expect([...stack.tabIds].sort(), where).toEqual([...members].sort());
    expect(stack.tabIds, where).toContain(stack.current.id);
  }

  // 4. The active tab is always reachable on screen: its own pill, the header
  //    of its folded stack, or a collapsed group whose header says it holds it.
  const active = tabs.find((t) => t.isActive);
  if (active) {
    const pill = layout.visibleTabs.some((t) => t.id === active.id);
    const header = layout.visibleItems.some(
      (item) => item.kind === "stack-header" && item.current.id === active.id,
    );
    const group = layout.visibleItems.some(
      (item) => item.kind === "group-header" && item.isCollapsed && item.hasActiveTab,
    );
    expect(pill || header || group, `${where}: active tab not reachable`).toBe(true);
  }

  // 5. Item ids are unique — Svelte keys the strip on them.
  const keys = layout.allItems.map((item) => `${item.kind}:${item.id}`);
  expect(new Set(keys).size, where).toBe(keys.length);

  // 6. Stops: one per standalone tab / folded stack / unfolded checkout, and
  //    cycling from any stop walks all of them before repeating.
  const stops = activationStops(layout);
  expect(new Set(stops).size, where).toBe(stops.length);
  if (stops.length >= 2) {
    const seen = new Set<string>();
    let at: string | null = stops[0];
    for (let i = 0; i < stops.length; i += 1) {
      seen.add(at!);
      at = cycleStop(layout, at, 1);
    }
    expect(seen.size, where).toBe(stops.length);
    expect(at, where).toBe(stops[0]);
  }
  return layout;
}

function randomUnit(layout: TabLayoutResult, pick: <T>(list: readonly T[]) => T): StripUnit | null {
  if (layout.order.length === 0) return null;
  return unitForTab(layout, pick(layout.order));
}

describe("strip navigation fuzz", () => {
  it(`holds every invariant across ${SEEDS} random workspaces and their moves`, () => {
    for (let seed = 1; seed <= SEEDS; seed += 1) {
      const { tabs: initial, stacking, collapsed, next, pick } = randomWorkspace(seed);
      let tabs = initial;
      if (stacking.enabled && next() < 0.5) {
        const layout = draw(tabs, stacking, collapsed);
        stacking.expanded = layout.stacks.filter(() => next() < 0.5).map((s) => s.key);
      }
      let layout = checkInvariants(seed, tabs, stacking, collapsed);

      for (let step = 0; step < 12; step += 1) {
        const unit = randomUnit(layout, pick);
        if (!unit) break;
        const op = next();
        if (op < 0.45) {
          const delta = next() < 0.5 ? -1 : 1;
          const order = moveUnit(layout, unit, delta);
          expect(order !== null, `seed ${seed}: canMove disagrees with move`).toBe(
            canMoveUnit(layout, unit, delta),
          );
          if (!order) continue;
          const carried = unitIds(layout, unit);
          tabs = apply(tabs, order);
          layout = checkInvariants(seed, tabs, stacking, collapsed);
          // A move is a fixed point: what was stored is what is drawn, and the
          // unit's tabs stay together.
          expect(layout.order, `seed ${seed}: move not a fixed point`).toEqual(order);
          const at = order.indexOf(carried[0]);
          expect(order.slice(at, at + carried.length), `seed ${seed}`).toEqual(carried);
        } else if (op < 0.6) {
          const order = moveUnitToEdge(layout, unit, next() < 0.5 ? "start" : "end");
          if (!order) continue;
          tabs = apply(tabs, order);
          layout = checkInvariants(seed, tabs, stacking, collapsed);
          expect(layout.order, `seed ${seed}: edge move not a fixed point`).toEqual(order);
        } else {
          const target = randomUnit(layout, pick);
          if (!target) continue;
          const before = next() < 0.5;
          const plan = planDrop(layout, unit, target, before);
          if (!plan) continue;
          const carried = unitIds(layout, unit);
          const groupsBefore = new Map(tabs.map((t) => [t.id, t.group ?? null]));
          tabs = apply(tabs, plan.order, plan.group !== undefined ? { ids: carried, group: plan.group } : undefined);
          layout = checkInvariants(seed, tabs, stacking, collapsed);
          // Only the dragged unit may change group, and only to the plan's group.
          for (const tab of tabs) {
            const was = groupsBefore.get(tab.id);
            if (carried.includes(tab.id) && plan.group !== undefined) expect(tab.group ?? null, `seed ${seed}`).toBe(plan.group);
            else expect(tab.group ?? null, `seed ${seed}`).toBe(was);
          }
          if (plan.group === undefined) {
            expect(layout.order, `seed ${seed}: same-container drop not a fixed point`).toEqual(plan.order);
          }
        }
      }
    }
  });

  it("refuses every drop into a stack the dragged unit does not belong to", () => {
    for (let seed = 1; seed <= SEEDS; seed += 1) {
      const { tabs, stacking, collapsed } = randomWorkspace(seed);
      if (!stacking.enabled) continue;
      const pre = draw(tabs, stacking, collapsed);
      stacking.expanded = pre.stacks.map((s) => s.key);
      const layout = draw(tabs, stacking, collapsed);
      for (const stack of layout.stacks) {
        for (const id of layout.order) {
          if (stack.tabIds.includes(id)) continue;
          const source = unitForTab(layout, id)!;
          for (const member of stack.tabIds) {
            for (const before of [true, false]) {
              expect(planDrop(layout, source, { kind: "tab", id: member }, before), `seed ${seed}`).toBeNull();
            }
          }
        }
      }
    }
  });
});
