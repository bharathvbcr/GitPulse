/**
 * Moving around the repository strip in the order it is drawn.
 *
 * The strip is a tree — groups hold tabs and worktree stacks, stacks hold
 * checkouts — and its drawn order can differ from the stored tab order: a
 * group or stack is drawn where its first member sits, so members stored far
 * apart are drawn together. Every operation here therefore works on the tree
 * and returns a whole new stored order. Applying that order makes the stored
 * list agree with the picture, which is what keeps "move right" from swapping
 * with a tab the reader cannot see and a drop from landing somewhere other
 * than the bar it showed.
 *
 * A unit is what moves as one piece: a folded stack moves with every checkout
 * in it; a tab (standalone, or inside an unfolded stack) moves alone. Units
 * move only among their siblings — a group's tabs stay in that group, and a
 * checkout cannot be dragged into or out of its repository's stack, because
 * family is a fact about the checkout, not a placement.
 */
import type { StripNode, TabLayoutResult } from "./tabGroups";

export type StripUnit = { kind: "tab"; id: string } | { kind: "stack"; key: string };

interface Location {
  /** The sibling list the unit sits in; a stable array owned by the layout. */
  siblings: StripNode[];
  index: number;
  /** User group of the container, null at the top level. */
  group: string | null;
  /** Stack key when the container is an unfolded stack. */
  stack: string | null;
}

function nodeIds(node: StripNode): string[] {
  if (node.kind === "tab") return [node.item.id];
  if (node.kind === "stack") return node.members.map((member) => member.id);
  return node.children.flatMap(nodeIds);
}

function matches(node: StripNode, unit: StripUnit): boolean {
  if (unit.kind === "tab") return node.kind === "tab" && node.item.id === unit.id;
  return node.kind === "stack" && node.header.key === unit.key;
}

function locate(tree: StripNode[], unit: StripUnit): Location | null {
  const walk = (siblings: StripNode[], group: string | null, stack: string | null): Location | null => {
    for (let index = 0; index < siblings.length; index += 1) {
      const node = siblings[index];
      if (matches(node, unit)) return { siblings, index, group, stack };
      if (node.kind === "group") {
        const found = walk(node.children, node.header.group, null);
        if (found) return found;
      } else if (node.kind === "stack" && node.header.isExpanded && unit.kind === "tab") {
        const found = walk(node.children, node.header.group, node.header.key);
        if (found) return found;
      }
    }
    return null;
  };
  return walk(tree, null, null);
}

/** The unit a tab moves as: its folded stack when it is inside one. */
export function unitForTab(layout: TabLayoutResult, tabId: string): StripUnit | null {
  const stack = layout.stacks.find((item) => item.tabIds.includes(tabId));
  if (stack) return stack.isExpanded ? { kind: "tab", id: tabId } : { kind: "stack", key: stack.key };
  return layout.order.includes(tabId) ? { kind: "tab", id: tabId } : null;
}

/** Stored order with one sibling list replaced by `reordered`. */
function flattenWith(tree: StripNode[], target: StripNode[], reordered: StripNode[]): string[] {
  const ids: string[] = [];
  const visit = (siblings: StripNode[]) => {
    for (const node of siblings === target ? reordered : siblings) {
      if (node.kind === "tab") ids.push(node.item.id);
      else visit(node.children);
    }
  };
  visit(tree);
  return ids;
}

function sameOrder(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((id, i) => id === b[i]);
}

function reorderAt(layout: TabLayoutResult, at: Location, destination: number): string[] | null {
  const target = Math.max(0, Math.min(at.siblings.length - 1, destination));
  if (target === at.index) return null;
  const reordered = [...at.siblings];
  const [moved] = reordered.splice(at.index, 1);
  reordered.splice(target, 0, moved);
  const order = flattenWith(layout.tree, at.siblings, reordered);
  return sameOrder(order, layout.order) ? null : order;
}

/** New stored order after stepping a unit among its siblings; null for a no-op. */
export function moveUnit(layout: TabLayoutResult, unit: StripUnit, delta: number): string[] | null {
  if (!Number.isInteger(delta) || delta === 0) return null;
  const at = locate(layout.tree, unit);
  if (!at) return null;
  const destination = at.index + delta;
  if (destination < 0 || destination >= at.siblings.length) return null;
  return reorderAt(layout, at, destination);
}

/** New stored order with the unit first or last among its siblings. */
export function moveUnitToEdge(layout: TabLayoutResult, unit: StripUnit, edge: "start" | "end"): string[] | null {
  const at = locate(layout.tree, unit);
  if (!at) return null;
  return reorderAt(layout, at, edge === "start" ? 0 : at.siblings.length - 1);
}

/** Whether a unit can step that way at all, for disabling menu items. */
export function canMoveUnit(layout: TabLayoutResult, unit: StripUnit, delta: -1 | 1): boolean {
  const at = locate(layout.tree, unit);
  if (!at) return false;
  const destination = at.index + delta;
  return destination >= 0 && destination < at.siblings.length;
}

/** Ids a unit carries, in stored order. */
export function unitIds(layout: TabLayoutResult, unit: StripUnit): string[] {
  const at = locate(layout.tree, unit);
  return at ? nodeIds(at.siblings[at.index]) : [];
}

export interface DropPlan {
  order: string[];
  /** Present only when the drop moves the unit into another user group (null = no group). */
  group?: string | null;
}

/**
 * Where a dragged unit lands when dropped on the `before`/after half of
 * another unit. Null when the drop would change nothing or is not allowed:
 * a checkout cannot join or leave its repository's stack by being dragged,
 * except by leaving for another user group, where it stacks with its family.
 */
export function planDrop(
  layout: TabLayoutResult,
  source: StripUnit,
  target: StripUnit,
  before: boolean,
): DropPlan | null {
  const from = locate(layout.tree, source);
  const to = locate(layout.tree, target);
  if (!from || !to) return null;
  if (from.siblings === to.siblings && from.index === to.index) return null;
  if (from.stack !== to.stack && (to.stack !== null || from.group === to.group)) return null;

  if (from.siblings === to.siblings) {
    let insertAt = before ? to.index : to.index + 1;
    if (insertAt > from.index) insertAt -= 1;
    const order = reorderAt(layout, from, insertAt);
    return order ? { order } : null;
  }

  const moving = nodeIds(from.siblings[from.index]);
  const movingSet = new Set(moving);
  const targetIds = nodeIds(to.siblings[to.index]);
  const remaining = layout.order.filter((id) => !movingSet.has(id));
  const anchor = before ? targetIds[0] : targetIds[targetIds.length - 1];
  const anchorAt = remaining.indexOf(anchor);
  if (anchorAt < 0) return null;
  const insertAt = before ? anchorAt : anchorAt + 1;
  const order = [...remaining.slice(0, insertAt), ...moving, ...remaining.slice(insertAt)];
  if (from.group !== to.group) return { order, group: to.group };
  return sameOrder(order, layout.order) ? null : { order };
}

/**
 * The tabs a number key or a cycle steps through, in drawn order: each
 * standalone tab, each folded stack (as the checkout its header shows), and
 * each checkout of an unfolded stack. A collapsed group still contributes —
 * activating a member opens it, as it always has.
 */
export function activationStops(layout: TabLayoutResult): string[] {
  const stops: string[] = [];
  const visit = (node: StripNode) => {
    if (node.kind === "tab") stops.push(node.item.id);
    else if (node.kind === "stack" && !node.header.isExpanded) stops.push(node.header.current.id);
    else node.children.forEach(visit);
  };
  layout.tree.forEach(visit);
  return stops;
}

/** The stop `step` away from the active tab, wrapping; null with fewer than two stops. */
export function cycleStop(layout: TabLayoutResult, activeId: string | null, step: 1 | -1): string | null {
  const stops = activationStops(layout);
  if (stops.length < 2) return null;
  const index = activeId ? stops.indexOf(activeId) : -1;
  if (index < 0) return stops[0];
  return stops[(index + step + stops.length) % stops.length];
}
