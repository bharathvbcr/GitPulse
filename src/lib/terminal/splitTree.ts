/**
 * Immutable recursive binary split tree for the terminal workspace.
 *
 * Panes stay mounted in one flat keyed `{#each}` in `TerminalPanel.svelte` so
 * splitting, resizing, swapping, zooming, or closing a split never reparents
 * or unmounts a live `TerminalSession`, `BrowserSession`, or `TerminalFilePane`.
 * This module owns the pure tree geometry that projects a `SplitNode` onto
 * normalized `[0, 1]` pane boxes and interactive sash descriptors.
 */
import { MAX_VISIBLE_SESSIONS } from "./attendance";
import { cleanTabName, type TabKind } from "./tabs";

export type SplitDirection = "row" | "col";

export interface SplitLeaf {
  readonly kind: "leaf";
  readonly paneId: string;
}

export interface SplitBranch {
  readonly kind: "branch";
  readonly id: string;
  readonly direction: SplitDirection;
  readonly ratio: number;
  readonly a: SplitNode;
  readonly b: SplitNode;
}

export type SplitNode = SplitLeaf | SplitBranch;

export interface NormalizedBox {
  readonly left: number;
  readonly top: number;
  readonly width: number;
  readonly height: number;
}

export interface SashDescriptor {
  readonly branchId: string;
  readonly direction: SplitDirection;
  readonly ratio: number;
  readonly box: NormalizedBox;
  readonly parentBox: NormalizedBox;
}

export const SPLIT_RATIO_MIN = 0.15;
export const SPLIT_RATIO_MAX = 0.85;
export const MAX_SPLIT_PANES = MAX_VISIBLE_SESSIONS;

const FULL_BOX: NormalizedBox = { left: 0, top: 0, width: 1, height: 1 };

let nextBranchSeq = 1;
function mintBranchId(): string {
  const id = `split-${nextBranchSeq}`;
  nextBranchSeq += 1;
  return id;
}

export function clampSplitRatio(ratio: number): number {
  if (!Number.isFinite(ratio)) return 0.5;
  return Math.min(SPLIT_RATIO_MAX, Math.max(SPLIT_RATIO_MIN, ratio));
}

export function leaf(paneId: string): SplitLeaf {
  return { kind: "leaf", paneId };
}

export function leafPaneIds(root: SplitNode | null): string[] {
  if (!root) return [];
  const out: string[] = [];
  const walk = (node: SplitNode) => {
    if (node.kind === "leaf") {
      out.push(node.paneId);
      return;
    }
    walk(node.a);
    walk(node.b);
  };
  walk(root);
  return out;
}

export function hasPane(root: SplitNode | null, paneId: string): boolean {
  if (!root || !paneId) return false;
  if (root.kind === "leaf") return root.paneId === paneId;
  return hasPane(root.a, paneId) || hasPane(root.b, paneId);
}

function normalizeRoot(node: SplitNode | null): SplitNode | null {
  if (!node || node.kind === "leaf") return null;
  return node;
}

/**
 * Splits `targetPaneId` with `newPaneId`. When `root` is `null`, forms the
 * initial 2-pane split branch. Returns `root` unchanged when at capacity,
 * when `newPaneId` is already in `root`, or when `targetPaneId` is absent.
 */
export function splitPane(
  root: SplitNode | null,
  targetPaneId: string,
  newPaneId: string,
  direction: SplitDirection = "row",
  position: "before" | "after" = "after",
  ratio = 0.5,
): SplitNode | null {
  if (!targetPaneId || !newPaneId || targetPaneId === newPaneId) return root;
  const clamped = clampSplitRatio(ratio);

  if (!root) {
    const first = leaf(position === "before" ? newPaneId : targetPaneId);
    const second = leaf(position === "before" ? targetPaneId : newPaneId);
    return {
      kind: "branch",
      id: mintBranchId(),
      direction,
      ratio: clamped,
      a: first,
      b: second,
    };
  }

  const leaves = leafPaneIds(root);
  if (leaves.length >= MAX_SPLIT_PANES) return root;
  if (leaves.includes(newPaneId) || !leaves.includes(targetPaneId)) return root;

  const insert = (node: SplitNode): SplitNode => {
    if (node.kind === "leaf") {
      if (node.paneId !== targetPaneId) return node;
      const added = leaf(newPaneId);
      return {
        kind: "branch",
        id: mintBranchId(),
        direction,
        ratio: clamped,
        a: position === "before" ? added : node,
        b: position === "before" ? node : added,
      };
    }
    const nextA = insert(node.a);
    if (nextA !== node.a) return { ...node, a: nextA };
    const nextB = insert(node.b);
    if (nextB !== node.b) return { ...node, b: nextB };
    return node;
  };

  return insert(root);
}

function removeLeafRaw(node: SplitNode, paneId: string): SplitNode | null {
  if (node.kind === "leaf") {
    return node.paneId === paneId ? null : node;
  }
  const nextA = removeLeafRaw(node.a, paneId);
  const nextB = removeLeafRaw(node.b, paneId);
  if (nextA === node.a && nextB === node.b) return node;
  if (!nextA) return nextB;
  if (!nextB) return nextA;
  return { ...node, a: nextA, b: nextB };
}

/**
 * Removes `paneId` and collapses its parent branch. When fewer than two panes
 * remain, normalizes to `null` (single-pane mode).
 */
export function removePane(root: SplitNode | null, paneId: string): SplitNode | null {
  if (!root || !paneId) return root;
  return normalizeRoot(removeLeafRaw(root, paneId));
}

/**
 * Replaces `oldPaneId` with `newPaneId` in place. A no-op when `newPaneId` is
 * already visible in `root` so duplicate leaves can never form.
 */
export function replacePane(
  root: SplitNode | null,
  oldPaneId: string,
  newPaneId: string,
): SplitNode | null {
  if (!root || !oldPaneId || !newPaneId || oldPaneId === newPaneId) return root;
  if (hasPane(root, newPaneId) || !hasPane(root, oldPaneId)) return root;

  const walk = (node: SplitNode): SplitNode => {
    if (node.kind === "leaf") {
      return node.paneId === oldPaneId ? leaf(newPaneId) : node;
    }
    const nextA = walk(node.a);
    const nextB = walk(node.b);
    if (nextA === node.a && nextB === node.b) return node;
    return { ...node, a: nextA, b: nextB };
  };
  return walk(root);
}

/** Swaps two leaves already present in `root`. */
export function swapPanes(
  root: SplitNode | null,
  paneA: string,
  paneB: string,
): SplitNode | null {
  if (!root || !paneA || !paneB || paneA === paneB) return root;
  if (!hasPane(root, paneA) || !hasPane(root, paneB)) return root;

  const walk = (node: SplitNode): SplitNode => {
    if (node.kind === "leaf") {
      if (node.paneId === paneA) return leaf(paneB);
      if (node.paneId === paneB) return leaf(paneA);
      return node;
    }
    const nextA = walk(node.a);
    const nextB = walk(node.b);
    if (nextA === node.a && nextB === node.b) return node;
    return { ...node, a: nextA, b: nextB };
  };
  return walk(root);
}

/**
 * Moves `paneId` (whether already in `root` or dragged from the tab strip) to
 * split `targetPaneId` on the requested edge.
 */
export function movePaneToSplit(
  root: SplitNode | null,
  paneId: string,
  targetPaneId: string,
  direction: SplitDirection,
  position: "before" | "after" = "after",
): SplitNode | null {
  if (!paneId || !targetPaneId || paneId === targetPaneId) return root;
  if (!root) {
    return splitPane(null, targetPaneId, paneId, direction, position);
  }
  if (!hasPane(root, targetPaneId)) return root;
  if (hasPane(root, paneId)) {
    const pruned = removeLeafRaw(root, paneId);
    if (!pruned) return root;
    if (pruned.kind === "leaf") {
      return splitPane(null, pruned.paneId, paneId, direction, position);
    }
    return splitPane(pruned, targetPaneId, paneId, direction, position);
  }
  return splitPane(root, targetPaneId, paneId, direction, position);
}

/** Rebuilds the branch `branchId` with `update`; returns `root` when it is absent or unchanged. */
function replaceBranch(
  root: SplitNode,
  branchId: string,
  update: (branch: SplitBranch) => SplitBranch,
): SplitNode {
  const walk = (node: SplitNode): SplitNode => {
    if (node.kind === "leaf") return node;
    if (node.id === branchId) return update(node);
    const nextA = walk(node.a);
    if (nextA !== node.a) return { ...node, a: nextA };
    const nextB = walk(node.b);
    if (nextB !== node.b) return { ...node, b: nextB };
    return node;
  };
  return walk(root);
}

/**
 * The panes of one same-direction run, as fractions of `node`'s box along
 * `direction`. A run is a chain of branches that all split the same way: three
 * side-by-side panes are `A | (B | C)`, one run of three segments. A leaf or a
 * branch split the other way is one segment.
 */
function runSegments(node: SplitNode, direction: SplitDirection, share = 1, out: number[] = []): number[] {
  if (node.kind === "branch" && node.direction === direction) {
    const r = clampSplitRatio(node.ratio);
    runSegments(node.a, direction, share * r, out);
    runSegments(node.b, direction, share * (1 - r), out);
  } else {
    out.push(share);
  }
  return out;
}

/** Writes one size per segment of `branch`'s run back as ratios, keeping unchanged nodes. */
function applyRunSizes(branch: SplitBranch, sizes: readonly number[]): SplitBranch {
  let next = 0;
  const walk = (node: SplitNode): { node: SplitNode; total: number } => {
    if (node.kind !== "branch" || node.direction !== branch.direction) {
      const total = sizes[next] ?? 0;
      next += 1;
      return { node, total };
    }
    const a = walk(node.a);
    const b = walk(node.b);
    const total = a.total + b.total;
    // Rounded so a drag lands exactly on the clamp and float error does not
    // build up over the many moves of one drag.
    const ratio = clampSplitRatio(total > 0 ? Math.round((a.total / total) * 1e9) / 1e9 : 0.5);
    if (Math.abs(ratio - node.ratio) < 1e-6 && a.node === node.a && b.node === node.b) {
      return { node, total };
    }
    return { node: { ...node, ratio, a: a.node, b: b.node }, total };
  };
  return walk(branch).node as SplitBranch;
}

/**
 * Moves the sash of `branchId` to `ratio` of its own box. Only the two panes
 * beside that sash change size: inside a run (`A | (B | C)`), dragging the
 * A|B sash leaves C where it is instead of rescaling B and C together. Each
 * of the two panes keeps at least `SPLIT_RATIO_MIN` of the branch's box, and a
 * pane already smaller than that is never shrunk further. The one exception:
 * when a nested branch of the run is at its own ratio clamp (C already at its
 * least share of B|C), C is pushed just enough to keep that share, so the
 * sash still follows the pointer.
 */
export function resizeBranch(
  root: SplitNode | null,
  branchId: string,
  ratio: number,
): SplitNode | null {
  if (!root || !branchId) return root;
  const target = clampSplitRatio(ratio);
  return replaceBranch(root, branchId, (branch) => {
    const sizes = runSegments(branch, branch.direction);
    const boundary = runSegments(branch.a, branch.direction).length;
    const before = sizes.slice(0, boundary).reduce((sum, size) => sum + size, 0);
    const left = sizes[boundary - 1];
    const right = sizes[boundary];
    const lo = Math.min(before, before - left + SPLIT_RATIO_MIN);
    const hi = Math.max(before, before + right - SPLIT_RATIO_MIN);
    const delta = Math.min(hi, Math.max(lo, target)) - before;
    if (Math.abs(delta) < 1e-9) return branch;
    const nextSizes = [...sizes];
    nextSizes[boundary - 1] = left + delta;
    nextSizes[boundary] = right - delta;
    return applyRunSizes(branch, nextSizes);
  });
}

/** The id of the outermost branch of the same-direction run holding `branchId`. */
function runTopId(root: SplitNode, branchId: string): string | null {
  const walk = (node: SplitNode, top: SplitBranch | null): string | null => {
    if (node.kind === "leaf") return null;
    const runTop = top && top.direction === node.direction ? top : node;
    if (node.id === branchId) return runTop.id;
    return walk(node.a, runTop) ?? walk(node.b, runTop);
  };
  return walk(root, null);
}

function equalizeRun(branch: SplitBranch): SplitBranch {
  const count = runSegments(branch, branch.direction).length;
  return applyRunSizes(branch, new Array<number>(count).fill(1 / count));
}

/**
 * Gives every pane of a same-direction run an equal share: three side-by-side
 * panes get a third each, not the 50/25/25 that halving every branch gives.
 * With `branchId`, only the run holding that sash is equalized; without it,
 * every run in the tree is. A share below `SPLIT_RATIO_MIN` of a branch (a
 * run of seven or more nested one way) is held at that minimum.
 */
export function equalizeSplits(root: SplitNode | null, branchId?: string): SplitNode | null {
  if (!root) return null;
  if (branchId) {
    const topId = runTopId(root, branchId);
    return topId ? replaceBranch(root, topId, equalizeRun) : root;
  }

  const walk = (node: SplitNode, parentDirection: SplitDirection | null): SplitNode => {
    if (node.kind === "leaf") return node;
    const nextA = walk(node.a, node.direction);
    const nextB = walk(node.b, node.direction);
    const inner = nextA === node.a && nextB === node.b ? node : { ...node, a: nextA, b: nextB };
    return parentDirection === node.direction ? inner : equalizeRun(inner);
  };
  return walk(root, null);
}

/**
 * Toggles `"row" <-> "col"` on the split branch immediately enclosing
 * `targetPaneId` (or on `root` when `targetPaneId` is omitted/absent),
 * preserving branch IDs, sash ratios, and leaf order.
 */
export function flipSplitOrientation(
  root: SplitNode | null,
  targetPaneId?: string | null,
): SplitNode | null {
  if (!root || root.kind !== "branch") return root;
  const flipDir = (direction: SplitDirection): SplitDirection =>
    direction === "row" ? "col" : "row";

  if (targetPaneId && hasPane(root, targetPaneId)) {
    const walk = (node: SplitNode): SplitNode => {
      if (node.kind === "leaf") return node;
      if (
        (node.a.kind === "leaf" && node.a.paneId === targetPaneId) ||
        (node.b.kind === "leaf" && node.b.paneId === targetPaneId)
      ) {
        return { ...node, direction: flipDir(node.direction) };
      }
      const nextA = walk(node.a);
      if (nextA !== node.a) return { ...node, a: nextA };
      const nextB = walk(node.b);
      if (nextB !== node.b) return { ...node, b: nextB };
      return node;
    };
    return walk(root);
  }

  return { ...root, direction: flipDir(root.direction) };
}

/**
 * Swaps `paneId` with the next cyclic leaf in `root`, preserving all branch
 * directions and sash ratios.
 */
export function swapWithAdjacentSplitPane(
  root: SplitNode | null,
  paneId: string | null,
): SplitNode | null {
  if (!root || root.kind !== "branch" || !paneId || !hasPane(root, paneId)) return root;
  const partner = cycleVisibleSplitPane(root, paneId, 1);
  if (!partner || partner === paneId) return root;
  return swapPanes(root, paneId, partner);
}

export function visiblePaneIds(
  root: SplitNode | null,
  activeId: string | null,
  zoomedPaneId: string | null = null,
): string[] {
  if (zoomedPaneId) {
    if (root?.kind === "branch" && hasPane(root, zoomedPaneId)) return [zoomedPaneId];
    if (!root && zoomedPaneId === activeId) return [zoomedPaneId];
  }
  if (root?.kind === "branch") return leafPaneIds(root);
  return activeId ? [activeId] : [];
}

/** Whether `zoomedPaneId` names a pane that is shown, so it alone fills the area. */
function zoomApplies(root: SplitNode | null, activeId: string | null, zoomedPaneId: string | null): boolean {
  if (!zoomedPaneId) return false;
  return (root?.kind === "branch" && hasPane(root, zoomedPaneId)) || (!root && zoomedPaneId === activeId);
}

export function computeSplitLayout(
  root: SplitNode | null,
  activeId: string | null,
  zoomedPaneId: string | null = null,
): {
  panes: Record<string, NormalizedBox>;
  sashes: SashDescriptor[];
} {
  const panes: Record<string, NormalizedBox> = {};
  const sashes: SashDescriptor[] = [];

  if (zoomedPaneId && zoomApplies(root, activeId, zoomedPaneId)) {
    panes[zoomedPaneId] = FULL_BOX;
    return { panes, sashes };
  }

  if (!root || root.kind === "leaf") {
    const id = root?.kind === "leaf" ? root.paneId : activeId;
    if (id) panes[id] = FULL_BOX;
    return { panes, sashes };
  }

  const walk = (node: SplitNode, box: NormalizedBox) => {
    if (node.kind === "leaf") {
      panes[node.paneId] = box;
      return;
    }
    const r = clampSplitRatio(node.ratio);
    if (node.direction === "row") {
      const splitX = box.left + box.width * r;
      sashes.push({
        branchId: node.id,
        direction: "row",
        ratio: r,
        box: { left: splitX, top: box.top, width: 0, height: box.height },
        parentBox: box,
      });
      walk(node.a, { left: box.left, top: box.top, width: box.width * r, height: box.height });
      walk(node.b, { left: splitX, top: box.top, width: box.width * (1 - r), height: box.height });
    } else {
      const splitY = box.top + box.height * r;
      sashes.push({
        branchId: node.id,
        direction: "col",
        ratio: r,
        box: { left: box.left, top: splitY, width: box.width, height: 0 },
        parentBox: box,
      });
      walk(node.a, { left: box.left, top: box.top, width: box.width, height: box.height * r });
      walk(node.b, { left: box.left, top: splitY, width: box.width, height: box.height * (1 - r) });
    }
  };

  walk(root, FULL_BOX);
  return { panes, sashes };
}

/**
 * The least height a pane is laid out at; a shorter dock scrolls its panes.
 * A pane's own chrome (split header h-7 = 28px, status row h-8 = 32px) takes
 * ~60px and Find another 36px in the active one, leaving ~104px of xterm rows
 * at minimum height with Find open.
 */
export const MIN_PANE_HEIGHT_PX = 200;

/**
 * The height the pane area must be for every visible pane of
 * `computeSplitLayout` to get at least `minPanePx`.
 *
 * Pane boxes are fractions of the pane area, so on their own they can only
 * shrink with it: a short dock would squeeze each pane of a split down to a
 * grid of no rows. Laying the panes out on a canvas at least this tall lets
 * the pane area scroll instead. A split right needs its taller side; a split
 * down needs each side's minimum at its own share of the height, which an
 * uneven ratio makes larger than the sum.
 */
export function minimumSplitHeight(
  root: SplitNode | null,
  activeId: string | null,
  zoomedPaneId: string | null = null,
  minPanePx: number = MIN_PANE_HEIGHT_PX,
): number {
  if (zoomApplies(root, activeId, zoomedPaneId) || !root || root.kind === "leaf") return minPanePx;
  const need = (node: SplitNode): number => {
    if (node.kind === "leaf") return minPanePx;
    const a = need(node.a);
    const b = need(node.b);
    if (node.direction === "row") return Math.max(a, b);
    const r = clampSplitRatio(node.ratio);
    return Math.max(a / r, b / (1 - r));
  };
  return need(root);
}

/**
 * Spatial neighbor lookup (`⌘⌥←/→/↑/↓`) across arbitrary 2D split geometry.
 */
export function adjacentPane(
  root: SplitNode | null,
  fromPaneId: string,
  dir: "left" | "right" | "up" | "down",
): string | null {
  if (!root || root.kind !== "branch" || !hasPane(root, fromPaneId)) return null;
  const { panes } = computeSplitLayout(root, fromPaneId, null);
  const from = panes[fromPaneId];
  if (!from) return null;

  const eps = 1e-4;
  let bestId: string | null = null;
  let bestDistance = Number.POSITIVE_INFINITY;
  let bestAlign = Number.POSITIVE_INFINITY;

  for (const [id, box] of Object.entries(panes)) {
    if (id === fromPaneId) continue;
    if (dir === "right" || dir === "left") {
      const overlap = Math.min(from.top + from.height, box.top + box.height) - Math.max(from.top, box.top);
      if (overlap <= eps) continue;
      const dist = dir === "right"
        ? box.left - (from.left + from.width)
        : from.left - (box.left + box.width);
      if (dist < -eps) continue;
      const align = Math.abs(box.top - from.top);
      if (dist < bestDistance - eps || (Math.abs(dist - bestDistance) <= eps && align < bestAlign)) {
        bestDistance = Math.max(0, dist);
        bestAlign = align;
        bestId = id;
      }
    } else {
      const overlap = Math.min(from.left + from.width, box.left + box.width) - Math.max(from.left, box.left);
      if (overlap <= eps) continue;
      const dist = dir === "down"
        ? box.top - (from.top + from.height)
        : from.top - (box.top + box.height);
      if (dist < -eps) continue;
      const align = Math.abs(box.left - from.left);
      if (dist < bestDistance - eps || (Math.abs(dist - bestDistance) <= eps && align < bestAlign)) {
        bestDistance = Math.max(0, dist);
        bestAlign = align;
        bestId = id;
      }
    }
  }
  return bestId;
}

export interface PersistedSplitLeafMeta {
  readonly launcher: TabKind;
  readonly slotIndex: number;
  readonly backendSessionId?: string;
  readonly parentSessionId?: string;
  readonly filePath?: string;
  readonly revealLine?: number | null;
  readonly revealColumn?: number | null;
  readonly snippet?: {
    readonly title: string;
    readonly content: string;
  };
  readonly pinned?: boolean;
  /** A `file` pane a double-click kept (`TerminalTab.kept`). */
  readonly kept?: boolean;
  readonly url?: string;
  readonly checkout?: string;
  readonly name?: string;
  readonly launchName?: string;
  readonly userNamed?: boolean;
  readonly title?: string;
}

export const MAX_PERSISTED_SNIPPET_CHARS = 16_384;

export type TabDropZone =
  | {
      readonly paneId: string;
      readonly mode: "split";
      readonly direction: SplitDirection;
      readonly position: "before" | "after";
    }
  | {
      readonly paneId: string;
      readonly mode: "swap";
    };

/**
 * Classifies a pointer position inside `paneId` into one of 4 outer split zones
 * (`< 0.25` from an edge) or the center `[0.25..0.75]` swap/replace zone.
 */
export function computeTabDropHint(
  rect: { left: number; top: number; width: number; height: number },
  clientX: number,
  clientY: number,
  paneId: string,
): TabDropZone | null {
  if (!paneId || rect.width <= 0 || rect.height <= 0) return null;
  const rx = Math.max(0, Math.min(1, (clientX - rect.left) / rect.width));
  const ry = Math.max(0, Math.min(1, (clientY - rect.top) / rect.height));
  if (rx >= 0.25 && rx <= 0.75 && ry >= 0.25 && ry <= 0.75) {
    return { paneId, mode: "swap" };
  }
  const distLeft = rx;
  const distRight = 1 - rx;
  const distTop = ry;
  const distBottom = 1 - ry;
  const min = Math.min(distLeft, distRight, distTop, distBottom);
  if (min === distLeft) return { paneId, mode: "split", direction: "row", position: "before" };
  if (min === distRight) return { paneId, mode: "split", direction: "row", position: "after" };
  if (min === distTop) return { paneId, mode: "split", direction: "col", position: "before" };
  return { paneId, mode: "split", direction: "col", position: "after" };
}

export type SplitReplacementChoice =
  | { readonly mode: "replace"; readonly targetPaneId: string }
  | { readonly mode: "split"; readonly anchorPaneId: string }
  | { readonly mode: "blocked" };

/**
 * Picks which split slot a background tab click should occupy without evicting
 * any pinned split pane:
 *  1. Replaces `preferredId` when it is an unpinned leaf in `root`.
 *  2. Otherwise replaces the first unpinned leaf in `root`.
 *  3. When all leaves in `root` are pinned, splits alongside `preferredId` if
 *     under `MAX_SPLIT_PANES`, or returns `{ mode: "blocked" }` at capacity.
 */
export function chooseSplitReplacementSlot(
  root: SplitNode | null,
  preferredId: string | null,
  pinnedIds: ReadonlySet<string>,
): SplitReplacementChoice | null {
  if (!root || root.kind !== "branch") return null;
  const leaves = leafPaneIds(root);
  if (leaves.length === 0) return null;
  if (preferredId && leaves.includes(preferredId) && !pinnedIds.has(preferredId)) {
    return { mode: "replace", targetPaneId: preferredId };
  }
  const firstUnpinned = leaves.find((id) => !pinnedIds.has(id));
  if (firstUnpinned) {
    return { mode: "replace", targetPaneId: firstUnpinned };
  }
  if (leaves.length < MAX_SPLIT_PANES) {
    const anchorPaneId =
      preferredId && leaves.includes(preferredId) ? preferredId : leaves[leaves.length - 1];
    return { mode: "split", anchorPaneId };
  }
  return { mode: "blocked" };
}

export interface HiddenSplitSummary {
  readonly hiddenIds: string[];
  readonly hiddenCount: number;
  readonly blockedCount: number;
  readonly errorCount: number;
  readonly doneCount: number;
  readonly unreadCount: number;
  readonly urgentPaneId: string | null;
  readonly badgeLabel: string | null;
  readonly tone: "warn" | "danger" | "accent" | "info" | null;
}

/**
 * Summarizes unseen activity and OSC 7501 attention states across hidden split
 * siblings while `zoomedPaneId` fills the workspace.
 */
export function summarizeHiddenSplitSiblings(
  root: SplitNode | null,
  zoomedPaneId: string | null,
  unreadIds: ReadonlySet<string>,
  agentStatuses: Readonly<
    Record<string, { state?: string; detail?: string | null } | null | undefined>
  >,
): HiddenSplitSummary {
  const empty: HiddenSplitSummary = {
    hiddenIds: [],
    hiddenCount: 0,
    blockedCount: 0,
    errorCount: 0,
    doneCount: 0,
    unreadCount: 0,
    urgentPaneId: null,
    badgeLabel: null,
    tone: null,
  };
  if (!root || root.kind !== "branch" || !zoomedPaneId || !hasPane(root, zoomedPaneId)) {
    return empty;
  }
  const hiddenIds = leafPaneIds(root).filter((id) => id !== zoomedPaneId);
  return summarizePaneSetAttention(hiddenIds, unreadIds, agentStatuses);
}

/**
 * Cycles focus across visible leaves of `root` without touching non-split tabs
 * or evicting any pane.
 */
export function cycleVisibleSplitPane(
  root: SplitNode | null,
  currentId: string | null,
  step: 1 | -1,
): string | null {
  if (!root || root.kind !== "branch") return null;
  const leaves = leafPaneIds(root);
  if (leaves.length < 2) return null;
  const idx = currentId ? leaves.indexOf(currentId) : -1;
  if (idx === -1) return step === 1 ? leaves[0] : leaves[leaves.length - 1];
  return leaves[(idx + step + leaves.length) % leaves.length];
}

export interface SplitGroup {
  readonly id: string;
  readonly tree: SplitBranch;
  readonly name?: string;
  readonly activePaneId?: string;
}

export type TabStripSegment<T extends { id: string }> =
  | {
      readonly kind: "group";
      readonly group: SplitGroup;
      readonly groupIndex: number;
      readonly tabs: T[];
    }
  | {
      readonly kind: "single";
      readonly tab: T;
    };

export interface PersistedSplitGroup {
  readonly id: string;
  readonly tree: SplitNode;
  readonly name?: string;
  readonly activePaneId?: string;
}

export type PersistedSplitWorkspace =
  | {
      readonly version: 2;
      readonly tree: SplitNode;
      readonly leaves: Record<string, PersistedSplitLeafMeta>;
    }
  | {
      readonly version: 3;
      readonly activeGroupId: string | null;
      readonly groups: readonly PersistedSplitGroup[];
      readonly leaves: Record<string, PersistedSplitLeafMeta>;
    };

let nextGroupSeq = 1;
export function mintSplitGroupId(): string {
  const id = `pair-${nextGroupSeq}`;
  nextGroupSeq += 1;
  return id;
}

export function allGroupedPaneIds(groups: readonly SplitGroup[]): Set<string> {
  const out = new Set<string>();
  for (const group of groups) {
    for (const id of leafPaneIds(group.tree)) {
      out.add(id);
    }
  }
  return out;
}

export function findGroupForPane(
  groups: readonly SplitGroup[],
  paneId: string | null | undefined,
): SplitGroup | null {
  if (!paneId) return null;
  for (const group of groups) {
    if (hasPane(group.tree, paneId)) return group;
  }
  return null;
}

export function normalizeSplitGroups(
  groups: readonly SplitGroup[],
  validPaneIds?: ReadonlySet<string>,
): SplitGroup[] {
  const out: SplitGroup[] = [];
  const claimed = new Set<string>();
  const seenGroupIds = new Set<string>();

  for (const group of groups) {
    if (!group || !group.tree) continue;
    const candidateLeaves = leafPaneIds(group.tree);
    const allowedMap = new Map<string, string>();
    for (const id of candidateLeaves) {
      if (claimed.has(id)) continue;
      if (validPaneIds && !validPaneIds.has(id)) continue;
      allowedMap.set(id, id);
    }
    const pruned = remapSplitTree(group.tree, allowedMap);
    if (!pruned || pruned.kind !== "branch") continue;

    const survivingLeaves = leafPaneIds(pruned);
    for (const id of survivingLeaves) claimed.add(id);

    const cleanedId = cleanTabName(group.id, 48);
    const id = cleanedId && !seenGroupIds.has(cleanedId) ? cleanedId : mintSplitGroupId();
    seenGroupIds.add(id);

    const name = group.name ? cleanTabName(group.name, 48) || undefined : undefined;
    const activePaneId =
      group.activePaneId && hasPane(pruned, group.activePaneId)
        ? group.activePaneId
        : survivingLeaves[0];

    out.push({
      id,
      tree: pruned,
      ...(name ? { name } : {}),
      activePaneId,
    });
  }

  return out;
}

function stripPanesFromTree(root: SplitBranch, removeIds: ReadonlySet<string>): SplitBranch | null {
  let current: SplitNode | null = root;
  for (const id of removeIds) {
    if (!current || current.kind !== "branch") return null;
    if (hasPane(current, id)) {
      current = removePane(current, id);
    }
  }
  return current && current.kind === "branch" ? current : null;
}

export function upsertSplitGroup(
  groups: readonly SplitGroup[],
  nextTree: SplitNode | null,
  options?: {
    groupId?: string;
    name?: string;
    activePaneId?: string | null;
  },
): SplitGroup[] {
  const targetId = options?.groupId;
  if (!nextTree || nextTree.kind !== "branch") {
    return targetId ? groups.filter((group) => group.id !== targetId) : [...groups];
  }

  const newLeaves = new Set(leafPaneIds(nextTree));
  const resolvedId = targetId && groups.some((group) => group.id === targetId)
    ? targetId
    : (targetId || mintSplitGroupId());
  const nextName =
    options?.name !== undefined
      ? cleanTabName(options.name, 48) || undefined
      : groups.find((group) => group.id === resolvedId)?.name;

  const updated: SplitGroup[] = [];
  let replacedExisting = false;

  for (const group of groups) {
    if (group.id === resolvedId) {
      replacedExisting = true;
      const leaves = leafPaneIds(nextTree);
      const activeCandidate =
        options?.activePaneId && hasPane(nextTree, options.activePaneId)
          ? options.activePaneId
          : group.activePaneId && hasPane(nextTree, group.activePaneId)
            ? group.activePaneId
            : leaves[0];
      updated.push({
        id: resolvedId,
        tree: nextTree,
        ...(nextName ? { name: nextName } : {}),
        activePaneId: activeCandidate,
      });
      continue;
    }

    const stripped = stripPanesFromTree(group.tree, newLeaves);
    if (!stripped) continue;
    const surviving = leafPaneIds(stripped);
    updated.push({
      ...group,
      tree: stripped,
      activePaneId:
        group.activePaneId && hasPane(stripped, group.activePaneId)
          ? group.activePaneId
          : surviving[0],
    });
  }

  if (!replacedExisting) {
    const leaves = leafPaneIds(nextTree);
    const activeCandidate =
      options?.activePaneId && hasPane(nextTree, options.activePaneId)
        ? options.activePaneId
        : leaves[0];
    updated.push({
      id: resolvedId,
      tree: nextTree,
      ...(nextName ? { name: nextName } : {}),
      activePaneId: activeCandidate,
    });
  }

  return normalizeSplitGroups(updated);
}

export function removeSplitGroup(groups: readonly SplitGroup[], groupId: string): SplitGroup[] {
  if (!groupId) return [...groups];
  return groups.filter((group) => group.id !== groupId);
}

export function removePaneFromGroups(groups: readonly SplitGroup[], paneId: string): SplitGroup[] {
  if (!paneId) return groups as SplitGroup[];
  let changed = false;
  const next: SplitGroup[] = [];
  for (const group of groups) {
    if (!hasPane(group.tree, paneId)) {
      next.push(group);
      continue;
    }
    changed = true;
    const pruned = removePane(group.tree, paneId);
    if (!pruned || pruned.kind !== "branch") continue;
    const surviving = leafPaneIds(pruned);
    next.push({
      ...group,
      tree: pruned,
      activePaneId:
        group.activePaneId && hasPane(pruned, group.activePaneId)
          ? group.activePaneId
          : surviving[0],
    });
  }
  return changed ? next : (groups as SplitGroup[]);
}

export function renameSplitGroup(
  groups: readonly SplitGroup[],
  groupId: string,
  rawName: string,
): SplitGroup[] {
  const cleaned = cleanTabName(rawName, 48) || undefined;
  let changed = false;
  const next = groups.map((group) => {
    if (group.id !== groupId) return group;
    if (group.name === cleaned) return group;
    changed = true;
    return cleaned
      ? { ...group, name: cleaned }
      : { id: group.id, tree: group.tree, ...(group.activePaneId ? { activePaneId: group.activePaneId } : {}) };
  });
  return changed ? next : (groups as SplitGroup[]);
}

export function recordGroupActivePane(
  groups: readonly SplitGroup[],
  activePaneId: string | null,
): SplitGroup[] {
  if (!activePaneId) return groups as SplitGroup[];
  let changed = false;
  const next = groups.map((group) => {
    if (!hasPane(group.tree, activePaneId) || group.activePaneId === activePaneId) return group;
    changed = true;
    return { ...group, activePaneId };
  });
  return changed ? next : (groups as SplitGroup[]);
}

export function orderGroupsByTabOrder(
  groups: readonly SplitGroup[],
  tabs: readonly { id: string }[],
): SplitGroup[] {
  if (groups.length <= 1) return groups as SplitGroup[];
  const indexById = new Map<string, number>();
  tabs.forEach((tab, index) => indexById.set(tab.id, index));
  const earliestFor = (group: SplitGroup): number => {
    let min = Number.POSITIVE_INFINITY;
    for (const id of leafPaneIds(group.tree)) {
      const idx = indexById.get(id);
      if (idx !== undefined && idx < min) min = idx;
    }
    return min;
  };
  const sorted = [...groups].sort((a, b) => earliestFor(a) - earliestFor(b));
  const unchanged = sorted.every((group, index) => group === groups[index]);
  return unchanged ? (groups as SplitGroup[]) : sorted;
}

export function coLocateTabsForGroups<T extends { id: string }>(
  tabs: readonly T[],
  groups: readonly SplitGroup[],
): T[] {
  if (tabs.length < 2 || groups.length === 0) return tabs as T[];
  const tabById = new Map<string, T>();
  for (const tab of tabs) tabById.set(tab.id, tab);

  const ownerByTabId = new Map<string, SplitGroup>();
  for (const group of groups) {
    for (const id of leafPaneIds(group.tree)) {
      if (tabById.has(id) && !ownerByTabId.has(id)) {
        ownerByTabId.set(id, group);
      }
    }
  }

  const emittedGroups = new Set<string>();
  const out: T[] = [];
  for (const tab of tabs) {
    const owner = ownerByTabId.get(tab.id);
    if (!owner) {
      out.push(tab);
      continue;
    }
    if (emittedGroups.has(owner.id)) continue;
    emittedGroups.add(owner.id);
    for (const leafId of leafPaneIds(owner.tree)) {
      const member = tabById.get(leafId);
      if (member) out.push(member);
    }
  }

  if (out.length === tabs.length && out.every((tab, idx) => tab === tabs[idx])) {
    return tabs as T[];
  }
  return out;
}

export function buildTabStripSegments<T extends { id: string }>(
  tabs: readonly T[],
  groups: readonly SplitGroup[],
): TabStripSegment<T>[] {
  const orderedTabs = coLocateTabsForGroups(tabs, groups);
  const orderedGroups = orderGroupsByTabOrder(groups, orderedTabs);
  const ownerByTabId = new Map<string, SplitGroup>();
  const groupIndexById = new Map<string, number>();

  orderedGroups.forEach((group, idx) => {
    groupIndexById.set(group.id, idx + 1);
    for (const leafId of leafPaneIds(group.tree)) {
      ownerByTabId.set(leafId, group);
    }
  });

  const tabById = new Map<string, T>();
  for (const tab of orderedTabs) tabById.set(tab.id, tab);

  const emitted = new Set<string>();
  const segments: TabStripSegment<T>[] = [];
  for (const tab of orderedTabs) {
    const group = ownerByTabId.get(tab.id);
    if (!group) {
      segments.push({ kind: "single", tab });
      continue;
    }
    if (emitted.has(group.id)) continue;
    emitted.add(group.id);
    const groupTabs = leafPaneIds(group.tree)
      .map((leafId) => tabById.get(leafId))
      .filter((item): item is T => item !== undefined);
    if (groupTabs.length >= 2) {
      segments.push({
        kind: "group",
        group,
        groupIndex: groupIndexById.get(group.id) ?? 1,
        tabs: groupTabs,
      });
    } else {
      for (const orphan of groupTabs) segments.push({ kind: "single", tab: orphan });
    }
  }
  return segments;
}

export function splitGroupDisplayLabel(
  group: SplitGroup,
  indexZeroBased: number,
  totalGroups: number,
): string {
  if (group.name) return group.name;
  if (totalGroups > 1) return `Pair ${indexZeroBased + 1}`;
  return leafPaneIds(group.tree).length > 2 ? "Split" : "Pair";
}

export function cycleSplitGroup(
  groups: readonly SplitGroup[],
  currentGroupId: string | null,
  step: 1 | -1,
): { group: SplitGroup; targetPaneId: string } | null {
  if (groups.length === 0) return null;
  const idx = currentGroupId ? groups.findIndex((group) => group.id === currentGroupId) : -1;
  const group =
    idx === -1
      ? step === 1
        ? groups[0]
        : groups[groups.length - 1]
      : groups[(idx + step + groups.length) % groups.length];
  const leaves = leafPaneIds(group.tree);
  if (leaves.length === 0) return null;
  const targetPaneId =
    group.activePaneId && hasPane(group.tree, group.activePaneId)
      ? group.activePaneId
      : leaves[0];
  return { group, targetPaneId };
}

/**
 * The loose tab a plain Split (toolbar button, ⌘D) re-pairs with the active
 * tab, or null when it should open a new session of `kind` instead.
 *
 * Only a tab the user would have asked for qualifies: same kind as the
 * chosen launcher, same checkout, and not held by any pair. Splitting a tab
 * that is already paired always adds a new pane — growing a pair by pulling
 * in whatever happened to be loose in the strip is the guess that put a File
 * Explorer tab into a shell pair as a third pane. Any other tab still pairs
 * explicitly: "Pair With Open Tab…", "Open in Split", or a drag.
 */
export function chooseSplitPartner<
  T extends { id: string; launcher: string; checkout?: string | null },
>(
  tabs: readonly T[],
  groups: readonly SplitGroup[],
  activeId: string | null,
  kind: string,
  /** Tabs whose process has ended: a dead session is never the guess. */
  endedIds: readonly string[] = [],
): string | null {
  if (!activeId) return null;
  const active = tabs.find((tab) => tab.id === activeId);
  if (!active) return null;
  const grouped = allGroupedPaneIds(groups);
  if (grouped.has(activeId)) return null;
  const checkout = active.checkout || null;
  const partner = tabs.find(
    (tab) =>
      tab.id !== activeId &&
      !grouped.has(tab.id) &&
      !endedIds.includes(tab.id) &&
      tab.launcher === kind &&
      (tab.checkout || null) === checkout,
  );
  return partner?.id ?? null;
}

export interface PairJoinTarget {
  readonly groupId: string;
  readonly label: string;
  /** The pane the joining tab is split beside: the pair's remembered focus. */
  readonly anchorPaneId: string;
}

/**
 * The pairs `tabId` can be added to from a menu — the same move as dropping
 * the tab on a pair's badge. Its own pair and pairs already at the pane limit
 * are left out. Labels match the strip's badges.
 */
export function pairJoinTargets(
  groups: readonly SplitGroup[],
  tabId: string,
): PairJoinTarget[] {
  const targets: PairJoinTarget[] = [];
  groups.forEach((group, index) => {
    const leaves = leafPaneIds(group.tree);
    if (leaves.length === 0 || leaves.includes(tabId)) return;
    if (leaves.length >= MAX_SPLIT_PANES) return;
    const anchorPaneId =
      group.activePaneId && leaves.includes(group.activePaneId) ? group.activePaneId : leaves[0];
    targets.push({
      groupId: group.id,
      label: splitGroupDisplayLabel(group, index, groups.length),
      anchorPaneId,
    });
  });
  return targets;
}

export function moveTabWithGroups<T extends { id: string }>(
  tabs: readonly T[],
  groups: readonly SplitGroup[],
  id: string,
  step: -1 | 1,
): { tabs: T[]; groups: SplitGroup[] } {
  const coLocated = coLocateTabsForGroups(tabs, groups);
  const orderedGroups = orderGroupsByTabOrder(groups, coLocated);
  const segments = buildTabStripSegments(coLocated, orderedGroups);
  const segIdx = segments.findIndex((seg) =>
    seg.kind === "group" ? seg.tabs.some((tab) => tab.id === id) : seg.tab.id === id,
  );
  if (segIdx === -1) return { tabs: coLocated, groups: orderedGroups };

  const seg = segments[segIdx];
  if (seg.kind === "group") {
    const leafIdx = seg.tabs.findIndex((tab) => tab.id === id);
    const neighborIdx = leafIdx + step;
    if (neighborIdx >= 0 && neighborIdx < seg.tabs.length) {
      const partnerId = seg.tabs[neighborIdx].id;
      const swappedTree = swapPanes(seg.group.tree, id, partnerId);
      if (swappedTree && swappedTree.kind === "branch") {
        const nextGroups = orderedGroups.map((candidate) =>
          candidate.id === seg.group.id ? { ...candidate, tree: swappedTree } : candidate,
        );
        const nextTabs = coLocateTabsForGroups(coLocated, nextGroups);
        return { tabs: nextTabs, groups: orderGroupsByTabOrder(nextGroups, nextTabs) };
      }
    }
  }

  const adjIdx = segIdx + step;
  if (adjIdx < 0 || adjIdx >= segments.length) {
    return { tabs: coLocated, groups: orderedGroups };
  }
  const reorderedSegments = [...segments];
  [reorderedSegments[segIdx], reorderedSegments[adjIdx]] = [
    reorderedSegments[adjIdx],
    reorderedSegments[segIdx],
  ];
  const nextTabs = reorderedSegments.flatMap((entry) =>
    entry.kind === "group" ? entry.tabs : [entry.tab],
  );
  return { tabs: nextTabs, groups: orderGroupsByTabOrder(orderedGroups, nextTabs) };
}

export type TabStripDropMode = "before" | "after" | "pair";

/**
 * Classifies a pointer offset `rx` (`[0, 1]`) across a tab in the strip.
 * Plain drags reorder (`before` / `after`) so users never accidentally create
 * a split when rearranging tabs; holding `Alt` explicitly pairs into a split.
 */
export function classifyTabStripDropZone(rx: number, altKey = false): TabStripDropMode {
  if (altKey) return "pair";
  return rx < 0.5 ? "before" : "after";
}

/**
 * Executes a tab-strip drop deterministically:
 * - `mode === "pair"`: pairs `movingTabId` with `targetTabId` (or swaps if already in the same group),
 *   respecting `MAX_SPLIT_PANES` and `pinnedIds`.
 * - `mode === "before" | "after"` within the same `SplitGroup`: reorders/swaps their split pane slots.
 * - `mode === "before" | "after"` onto another group's tab: reorders `movingTabId` cleanly before or
 *   after the entire target group capsule without forcing an unwanted 3rd split pane.
 * - `mode === "before" | "after"` onto an ungrouped tab: detaches `movingTabId` (if grouped) and
 *   inserts it at the requested side of `targetTabId`.
 */
export function applyTabStripDrop<T extends { id: string }>(
  tabs: readonly T[],
  groups: readonly SplitGroup[],
  drop: {
    movingTabId: string;
    targetTabId: string;
    mode: TabStripDropMode;
    pinnedIds?: ReadonlySet<string>;
  },
): {
  tabs: T[];
  groups: SplitGroup[];
  activeTabId?: string;
  refusedReason?: string;
} {
  const { movingTabId, targetTabId, mode, pinnedIds } = drop;
  if (
    !movingTabId ||
    !targetTabId ||
    movingTabId === targetTabId ||
    !tabs.some((tab) => tab.id === movingTabId) ||
    !tabs.some((tab) => tab.id === targetTabId)
  ) {
    return { tabs: [...tabs], groups: [...groups] };
  }

  if (mode === "pair") {
    const existingTargetGroup = findGroupForPane(groups, targetTabId);
    if (existingTargetGroup && hasPane(existingTargetGroup.tree, movingTabId)) {
      const swapped = swapPanes(existingTargetGroup.tree, movingTabId, targetTabId);
      const nextGroups = upsertSplitGroup(groups, swapped, {
        groupId: existingTargetGroup.id,
        name: existingTargetGroup.name,
        activePaneId: movingTabId,
      });
      const coLocated = coLocateTabsForGroups(tabs, nextGroups);
      return {
        tabs: coLocated,
        groups: orderGroupsByTabOrder(nextGroups, coLocated),
        activeTabId: movingTabId,
      };
    }

    const baseGroups = removePaneFromGroups(groups, movingTabId);
    const refreshedTargetGroup = findGroupForPane(baseGroups, targetTabId);
    if (refreshedTargetGroup) {
      let nextTree: SplitNode | null = null;
      if (leafPaneIds(refreshedTargetGroup.tree).length < MAX_SPLIT_PANES) {
        nextTree = splitPane(refreshedTargetGroup.tree, targetTabId, movingTabId, "row", "after", 0.5);
      } else {
        const choice = chooseSplitReplacementSlot(
          refreshedTargetGroup.tree,
          targetTabId,
          pinnedIds ?? new Set<string>(),
        );
        if (choice?.mode === "replace") {
          nextTree = replacePane(refreshedTargetGroup.tree, choice.targetPaneId, movingTabId);
        } else {
          const coLocated = coLocateTabsForGroups(tabs, groups);
          return {
            tabs: coLocated,
            groups: orderGroupsByTabOrder(groups, coLocated),
            refusedReason:
              "That split pane is pinned and all 8 split slots are full — unpin or remove a pane first.",
          };
        }
      }
      const nextGroups = upsertSplitGroup(baseGroups, nextTree, {
        groupId: refreshedTargetGroup.id,
        name: refreshedTargetGroup.name,
        activePaneId: movingTabId,
      });
      const coLocated = coLocateTabsForGroups(tabs, nextGroups);
      return {
        tabs: coLocated,
        groups: orderGroupsByTabOrder(nextGroups, coLocated),
        activeTabId: movingTabId,
      };
    }

    const newTree = splitPane(null, targetTabId, movingTabId, "row", "after", 0.5);
    const nextGroups = upsertSplitGroup(baseGroups, newTree, { activePaneId: movingTabId });
    const coLocated = coLocateTabsForGroups(tabs, nextGroups);
    return {
      tabs: coLocated,
      groups: orderGroupsByTabOrder(nextGroups, coLocated),
      activeTabId: movingTabId,
    };
  }

  const sourceGroup = findGroupForPane(groups, movingTabId);
  const targetGroup = findGroupForPane(groups, targetTabId);

  // Reordering within the same split group updates its leaf order directly while
  // preserving every branch direction ("row"/"col") and custom sash ratio.
  if (sourceGroup && targetGroup && sourceGroup.id === targetGroup.id) {
    const leaves = leafPaneIds(sourceGroup.tree);
    const fromLeafIdx = leaves.indexOf(movingTabId);
    const toLeafRaw = leaves.indexOf(targetTabId);
    if (fromLeafIdx < 0 || toLeafRaw < 0 || fromLeafIdx === toLeafRaw) {
      const coLocated = coLocateTabsForGroups(tabs, groups);
      return {
        tabs: coLocated,
        groups: orderGroupsByTabOrder(groups, coLocated),
      };
    }
    const nextLeaves = [...leaves];
    nextLeaves.splice(fromLeafIdx, 1);
    const adjustedLeafTo = fromLeafIdx < toLeafRaw ? toLeafRaw - 1 : toLeafRaw;
    nextLeaves.splice(mode === "after" ? adjustedLeafTo + 1 : adjustedLeafTo, 0, movingTabId);
    if (nextLeaves.every((id, idx) => id === leaves[idx])) {
      const coLocated = coLocateTabsForGroups(tabs, groups);
      return {
        tabs: coLocated,
        groups: orderGroupsByTabOrder(groups, coLocated),
      };
    }
    let cursor = 0;
    const permuteLeaves = (node: SplitNode): SplitNode => {
      if (node.kind === "leaf") {
        const assigned = nextLeaves[cursor] ?? node.paneId;
        cursor += 1;
        return assigned === node.paneId ? node : leaf(assigned);
      }
      const nextA = permuteLeaves(node.a);
      const nextB = permuteLeaves(node.b);
      return nextA === node.a && nextB === node.b ? node : { ...node, a: nextA, b: nextB };
    };
    const reorderedTree = permuteLeaves(sourceGroup.tree);
    const nextGroups = orderedGroupsForSameGroupUpdate(groups, sourceGroup, reorderedTree);
    const coLocated = coLocateTabsForGroups(tabs, nextGroups);
    return {
      tabs: coLocated,
      groups: orderGroupsByTabOrder(nextGroups, coLocated),
    };
  }

  // Detach from any previous group and place before/after target (or target's whole group capsule).
  const baseGroups = removePaneFromGroups(groups, movingTabId);
  const coLocatedBase = coLocateTabsForGroups(tabs, baseGroups);
  const refreshedTargetGroup = findGroupForPane(baseGroups, targetTabId);
  let anchorTabId = targetTabId;
  if (refreshedTargetGroup) {
    const groupLeaves = leafPaneIds(refreshedTargetGroup.tree);
    if (groupLeaves.length > 0) {
      anchorTabId = mode === "before" ? groupLeaves[0] : groupLeaves[groupLeaves.length - 1];
    }
  }

  const fromIdx = coLocatedBase.findIndex((tab) => tab.id === movingTabId);
  const toRaw = coLocatedBase.findIndex((tab) => tab.id === anchorTabId);
  if (fromIdx < 0 || toRaw < 0 || fromIdx === toRaw) {
    return {
      tabs: coLocatedBase,
      groups: orderGroupsByTabOrder(baseGroups, coLocatedBase),
    };
  }

  const nextTabs = [...coLocatedBase];
  const [picked] = nextTabs.splice(fromIdx, 1);
  const adjustedTo = fromIdx < toRaw ? toRaw - 1 : toRaw;
  nextTabs.splice(mode === "after" ? adjustedTo + 1 : adjustedTo, 0, picked);
  const finalTabs = coLocateTabsForGroups(nextTabs, baseGroups);
  return {
    tabs: finalTabs,
    groups: orderGroupsByTabOrder(baseGroups, finalTabs),
  };
}

/**
 * Detaches `tabId` from any split group and places it at the end of the strip,
 * keeping its underlying session alive.
 */
export function detachTabToStripEnd<T extends { id: string }>(
  tabs: readonly T[],
  groups: readonly SplitGroup[],
  tabId: string,
): { tabs: T[]; groups: SplitGroup[] } {
  const idx = tabs.findIndex((tab) => tab.id === tabId);
  if (idx < 0) return { tabs: [...tabs], groups: [...groups] };
  const nextGroups = removePaneFromGroups(groups, tabId);
  const coLocated = coLocateTabsForGroups(tabs, nextGroups);
  const currentIdx = coLocated.findIndex((tab) => tab.id === tabId);
  if (currentIdx < 0) {
    return { tabs: coLocated, groups: orderGroupsByTabOrder(nextGroups, coLocated) };
  }
  const reordered = [...coLocated];
  const [picked] = reordered.splice(currentIdx, 1);
  reordered.push(picked);
  const finalTabs = coLocateTabsForGroups(reordered, nextGroups);
  return {
    tabs: finalTabs,
    groups: orderGroupsByTabOrder(nextGroups, finalTabs),
  };
}

function orderedGroupsForSameGroupUpdate(
  groups: readonly SplitGroup[],
  sourceGroup: SplitGroup,
  reorderedTree: SplitNode,
): SplitGroup[] {
  if (reorderedTree.kind !== "branch") return [...groups];
  return groups.map((candidate) =>
    candidate.id === sourceGroup.id ? { ...candidate, tree: reorderedTree } : candidate,
  );
}

/**
 * Reorders an entire `SplitGroup` capsule before or after another group or single
 * tab in the strip without ungrouping either.
 */
export function reorderSplitGroupInStrip<T extends { id: string }>(
  tabs: readonly T[],
  groups: readonly SplitGroup[],
  movingGroupId: string,
  targetId: string,
  position: "before" | "after",
): { tabs: T[]; groups: SplitGroup[] } {
  const coLocated = coLocateTabsForGroups(tabs, groups);
  const orderedGroups = orderGroupsByTabOrder(groups, coLocated);
  const segments = buildTabStripSegments(coLocated, orderedGroups);
  const fromSegIdx = segments.findIndex(
    (seg) => seg.kind === "group" && seg.group.id === movingGroupId,
  );
  const toSegIdx = segments.findIndex((seg) =>
    seg.kind === "group"
      ? seg.group.id === targetId || seg.tabs.some((tab) => tab.id === targetId)
      : seg.tab.id === targetId,
  );
  if (fromSegIdx < 0 || toSegIdx < 0 || fromSegIdx === toSegIdx) {
    return { tabs: coLocated, groups: orderedGroups };
  }
  const nextSegments = [...segments];
  const [picked] = nextSegments.splice(fromSegIdx, 1);
  const adjustedTo = fromSegIdx < toSegIdx ? toSegIdx - 1 : toSegIdx;
  nextSegments.splice(position === "after" ? adjustedTo + 1 : adjustedTo, 0, picked);
  const nextTabs = nextSegments.flatMap((seg) => (seg.kind === "group" ? seg.tabs : [seg.tab]));
  return {
    tabs: nextTabs,
    groups: orderGroupsByTabOrder(orderedGroups, nextTabs),
  };
}

/**
 * Moves an entire `SplitGroup` capsule to the end of the tab strip without
 * ungrouping any of its panes.
 */
export function moveSplitGroupToStripEnd<T extends { id: string }>(
  tabs: readonly T[],
  groups: readonly SplitGroup[],
  movingGroupId: string,
): { tabs: T[]; groups: SplitGroup[] } {
  const coLocated = coLocateTabsForGroups(tabs, groups);
  const orderedGroups = orderGroupsByTabOrder(groups, coLocated);
  const segments = buildTabStripSegments(coLocated, orderedGroups);
  const fromSegIdx = segments.findIndex(
    (seg) => seg.kind === "group" && seg.group.id === movingGroupId,
  );
  if (fromSegIdx < 0 || fromSegIdx === segments.length - 1) {
    return { tabs: coLocated, groups: orderedGroups };
  }
  const nextSegments = [...segments];
  const [picked] = nextSegments.splice(fromSegIdx, 1);
  nextSegments.push(picked);
  const nextTabs = nextSegments.flatMap((seg) => (seg.kind === "group" ? seg.tabs : [seg.tab]));
  return {
    tabs: nextTabs,
    groups: orderGroupsByTabOrder(orderedGroups, nextTabs),
  };
}

/**
 * Shifts an entire `SplitGroup` capsule one segment left (`step = -1`) or
 * right (`step = 1`) across the tab strip without ungrouping or mutating any
 * internal split tree.
 */
export function moveSplitGroupByStep<T extends { id: string }>(
  tabs: readonly T[],
  groups: readonly SplitGroup[],
  groupId: string,
  step: -1 | 1,
): { tabs: T[]; groups: SplitGroup[] } {
  const coLocated = coLocateTabsForGroups(tabs, groups);
  const orderedGroups = orderGroupsByTabOrder(groups, coLocated);
  const segments = buildTabStripSegments(coLocated, orderedGroups);
  const segIdx = segments.findIndex(
    (seg) => seg.kind === "group" && seg.group.id === groupId,
  );
  const targetIdx = segIdx + step;
  if (segIdx < 0 || targetIdx < 0 || targetIdx >= segments.length) {
    return { tabs: coLocated, groups: orderedGroups };
  }
  const neighbor = segments[targetIdx];
  const targetId = neighbor.kind === "group" ? neighbor.group.id : neighbor.tab.id;
  return reorderSplitGroupInStrip(
    coLocated,
    orderedGroups,
    groupId,
    targetId,
    step === -1 ? "before" : "after",
  );
}

function summarizePaneSetAttention(
  paneIds: readonly string[],
  unreadIds: ReadonlySet<string>,
  agentStatuses: Readonly<
    Record<string, { state?: string; detail?: string | null } | null | undefined>
  >,
): HiddenSplitSummary {
  const empty: HiddenSplitSummary = {
    hiddenIds: paneIds as string[],
    hiddenCount: paneIds.length,
    blockedCount: 0,
    errorCount: 0,
    doneCount: 0,
    unreadCount: 0,
    urgentPaneId: null,
    badgeLabel: null,
    tone: null,
  };
  if (paneIds.length === 0) return empty;

  let blockedCount = 0;
  let errorCount = 0;
  let doneCount = 0;
  let unreadCount = 0;
  let firstBlocked: string | null = null;
  let firstError: string | null = null;
  let firstDone: string | null = null;
  let firstUnread: string | null = null;

  for (const id of paneIds) {
    const st = agentStatuses[id]?.state;
    if (st === "blocked") {
      blockedCount += 1;
      firstBlocked ??= id;
    } else if (st === "error") {
      errorCount += 1;
      firstError ??= id;
    } else if (st === "done") {
      doneCount += 1;
      firstDone ??= id;
    }
    if (unreadIds.has(id)) {
      unreadCount += 1;
      firstUnread ??= id;
    }
  }

  if (blockedCount > 0) {
    return {
      hiddenIds: [...paneIds],
      hiddenCount: paneIds.length,
      blockedCount,
      errorCount,
      doneCount,
      unreadCount,
      urgentPaneId: firstBlocked,
      badgeLabel: blockedCount > 1 ? `${blockedCount} need input` : "Needs input",
      tone: "warn",
    };
  }
  if (errorCount > 0) {
    return {
      hiddenIds: [...paneIds],
      hiddenCount: paneIds.length,
      blockedCount,
      errorCount,
      doneCount,
      unreadCount,
      urgentPaneId: firstError,
      badgeLabel: errorCount > 1 ? `${errorCount} failed` : "Failed",
      tone: "danger",
    };
  }
  if (doneCount > 0) {
    return {
      hiddenIds: [...paneIds],
      hiddenCount: paneIds.length,
      blockedCount,
      errorCount,
      doneCount,
      unreadCount,
      urgentPaneId: firstDone,
      badgeLabel: doneCount > 1 ? `${doneCount} done` : "Done",
      tone: "accent",
    };
  }
  if (unreadCount > 0) {
    return {
      hiddenIds: [...paneIds],
      hiddenCount: paneIds.length,
      blockedCount,
      errorCount,
      doneCount,
      unreadCount,
      urgentPaneId: firstUnread,
      badgeLabel: `${unreadCount} unread`,
      tone: "info",
    };
  }
  return empty;
}

/**
 * Summarizes unseen activity and OSC 7501 attention states across all panes
 * of a background terminal pair when that pair is not currently active.
 */
export function summarizeSplitGroupAttention(
  group: SplitGroup,
  isGroupActive: boolean,
  unreadIds: ReadonlySet<string>,
  agentStatuses: Readonly<
    Record<string, { state?: string; detail?: string | null } | null | undefined>
  >,
): HiddenSplitSummary {
  if (isGroupActive) {
    return {
      hiddenIds: [],
      hiddenCount: 0,
      blockedCount: 0,
      errorCount: 0,
      doneCount: 0,
      unreadCount: 0,
      urgentPaneId: null,
      badgeLabel: null,
      tone: null,
    };
  }
  return summarizePaneSetAttention(leafPaneIds(group.tree), unreadIds, agentStatuses);
}

export function serializeSplitTree(root: SplitNode | null): string | null {
  if (!root || root.kind !== "branch") return null;
  return JSON.stringify(root);
}

export function serializeSplitWorkspace(
  rootOrGroups: SplitNode | readonly SplitGroup[] | null,
  leaves: Record<string, PersistedSplitLeafMeta>,
  activeGroupId?: string | null,
): string | null {
  if (Array.isArray(rootOrGroups)) {
    const normalized = normalizeSplitGroups(rootOrGroups);
    if (normalized.length === 0) return null;
    const usedIds = allGroupedPaneIds(normalized);
    const filtered: Record<string, PersistedSplitLeafMeta> = {};
    for (const id of usedIds) {
      if (leaves[id]) filtered[id] = leaves[id];
    }
    const groupsPayload: PersistedSplitGroup[] = normalized.map((group) => ({
      id: group.id,
      ...(group.name ? { name: group.name } : {}),
      ...(group.activePaneId ? { activePaneId: group.activePaneId } : {}),
      tree: group.tree,
    }));
    const validActiveGroup =
      activeGroupId && groupsPayload.some((group) => group.id === activeGroupId)
        ? activeGroupId
        : null;
    const payload: PersistedSplitWorkspace = {
      version: 3,
      activeGroupId: validActiveGroup,
      groups: groupsPayload,
      leaves: filtered,
    };
    return JSON.stringify(payload);
  }
  const singleRoot = rootOrGroups as SplitNode | null;
  if (!singleRoot || singleRoot.kind !== "branch") return null;
  const usedIds = leafPaneIds(singleRoot);
  const filtered: Record<string, PersistedSplitLeafMeta> = {};
  for (const id of usedIds) {
    if (leaves[id]) filtered[id] = leaves[id];
  }
  const payload: PersistedSplitWorkspace = { version: 2, tree: singleRoot, leaves: filtered };
  return JSON.stringify(payload);
}

function parseRawSplitNode(value: unknown, depth: number): SplitNode | null {
  if (!value || typeof value !== "object" || depth > MAX_SPLIT_PANES) return null;
  const rec = value as Record<string, unknown>;
  if (rec.kind === "leaf") {
    const id = typeof rec.paneId === "string" ? rec.paneId : "";
    return id ? leaf(id) : null;
  }
  if (rec.kind === "branch") {
    const direction: SplitDirection = rec.direction === "col" ? "col" : "row";
    const ratio = clampSplitRatio(typeof rec.ratio === "number" ? rec.ratio : 0.5);
    const a = parseRawSplitNode(rec.a, depth + 1);
    const b = parseRawSplitNode(rec.b, depth + 1);
    if (!a || !b) return null;
    return {
      kind: "branch",
      id: mintBranchId(),
      direction,
      ratio,
      a,
      b,
    };
  }
  return null;
}

export function parsePersistedLeaves(rawLeaves: unknown): Record<string, PersistedSplitLeafMeta> {
  const leavesRaw =
    rawLeaves && typeof rawLeaves === "object" && !Array.isArray(rawLeaves)
      ? (rawLeaves as Record<string, unknown>)
      : {};
  const leaves: Record<string, PersistedSplitLeafMeta> = {};
  for (const [id, item] of Object.entries(leavesRaw)) {
    if (!item || typeof item !== "object" || Array.isArray(item)) continue;
    const meta = item as Record<string, unknown>;
    if (typeof meta.launcher !== "string") continue;
    const cleanName = typeof meta.name === "string" ? cleanTabName(meta.name, 120) : "";
    const cleanLaunchName = typeof meta.launchName === "string" ? cleanTabName(meta.launchName, 120) : "";
    const cleanTitle = typeof meta.title === "string" ? cleanTabName(meta.title, 256) : "";
    const rawSnippet =
      meta.snippet && typeof meta.snippet === "object" && !Array.isArray(meta.snippet)
        ? (meta.snippet as Record<string, unknown>)
        : null;
    const snippet =
      rawSnippet && typeof rawSnippet.content === "string"
        ? {
            title: cleanTabName(typeof rawSnippet.title === "string" ? rawSnippet.title : "", 64) || "Command output",
            content: rawSnippet.content.slice(0, MAX_PERSISTED_SNIPPET_CHARS),
          }
        : undefined;
    leaves[id] = {
      launcher: meta.launcher as TabKind,
      slotIndex: typeof meta.slotIndex === "number" && Number.isFinite(meta.slotIndex) ? meta.slotIndex : 0,
      ...(typeof meta.backendSessionId === "string" && meta.backendSessionId ? { backendSessionId: meta.backendSessionId } : {}),
      ...(typeof meta.parentSessionId === "string" && meta.parentSessionId ? { parentSessionId: meta.parentSessionId } : {}),
      ...(typeof meta.filePath === "string" && meta.filePath ? { filePath: meta.filePath } : {}),
      ...(typeof meta.revealLine === "number" ? { revealLine: meta.revealLine } : {}),
      ...(typeof meta.revealColumn === "number" ? { revealColumn: meta.revealColumn } : {}),
      ...(snippet ? { snippet } : {}),
      ...(meta.pinned === true ? { pinned: true } : {}),
      ...(meta.kept === true && meta.launcher === "file" ? { kept: true } : {}),
      ...(typeof meta.url === "string" && meta.url ? { url: meta.url } : {}),
      ...(typeof meta.checkout === "string" && meta.checkout ? { checkout: meta.checkout } : {}),
      ...(cleanName ? { name: cleanName } : {}),
      ...(cleanLaunchName ? { launchName: cleanLaunchName } : {}),
      ...(typeof meta.userNamed === "boolean" ? { userNamed: meta.userNamed } : {}),
      ...(cleanTitle ? { title: cleanTitle } : {}),
    };
  }
  return leaves;
}

export function parsePersistedSplitWorkspace(
  raw: string | null | undefined,
): {
  tree: SplitNode;
  groups: SplitGroup[];
  activeGroupId: string | null;
  leaves: Record<string, PersistedSplitLeafMeta>;
} | null {
  if (!raw) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return null;
  const rec = parsed as Record<string, unknown>;
  if (rec.version === 3 && Array.isArray(rec.groups)) {
    const rawGroups: SplitGroup[] = [];
    for (const item of rec.groups.slice(0, 32)) {
      if (!item || typeof item !== "object" || Array.isArray(item)) continue;
      const gRec = item as Record<string, unknown>;
      const gTree = parseRawSplitNode(gRec.tree, 0);
      if (!gTree || gTree.kind !== "branch") continue;
      const id = typeof gRec.id === "string" ? cleanTabName(gRec.id, 48) || mintSplitGroupId() : mintSplitGroupId();
      const name = typeof gRec.name === "string" ? cleanTabName(gRec.name, 48) || undefined : undefined;
      const activePaneId = typeof gRec.activePaneId === "string" ? gRec.activePaneId : undefined;
      rawGroups.push({
        id,
        tree: gTree,
        ...(name ? { name } : {}),
        ...(activePaneId ? { activePaneId } : {}),
      });
    }
    const groups = normalizeSplitGroups(rawGroups);
    if (groups.length === 0) return null;
    const requestedActive =
      typeof rec.activeGroupId === "string" && groups.some((group) => group.id === rec.activeGroupId)
        ? rec.activeGroupId
        : null;
    const activeGroup = (requestedActive ? groups.find((g) => g.id === requestedActive) : null) ?? groups[0];
    return {
      tree: activeGroup.tree,
      groups,
      activeGroupId: requestedActive,
      leaves: parsePersistedLeaves(rec.leaves),
    };
  }
  if (rec.version === 2 && rec.tree && typeof rec.tree === "object") {
    const tree = parseRawSplitNode(rec.tree, 0);
    if (!tree || tree.kind !== "branch") return null;
    const group: SplitGroup = { id: mintSplitGroupId(), tree };
    return {
      tree,
      groups: [group],
      activeGroupId: group.id,
      leaves: parsePersistedLeaves(rec.leaves),
    };
  }
  const v1Tree = parseRawSplitNode(parsed, 0);
  if (!v1Tree || v1Tree.kind !== "branch") return null;
  const group: SplitGroup = { id: mintSplitGroupId(), tree: v1Tree };
  return {
    tree: v1Tree,
    groups: [group],
    activeGroupId: group.id,
    leaves: {},
  };
}

export function remapSplitGroups(
  groups: readonly SplitGroup[],
  paneIdMap: ReadonlyMap<string, string>,
): SplitGroup[] {
  const mapped: SplitGroup[] = [];
  for (const group of groups) {
    const remapped = remapSplitTree(group.tree, paneIdMap);
    if (!remapped || remapped.kind !== "branch") continue;
    const mappedActive = group.activePaneId ? paneIdMap.get(group.activePaneId) : undefined;
    mapped.push({
      id: group.id,
      tree: remapped,
      ...(group.name ? { name: group.name } : {}),
      ...(mappedActive ? { activePaneId: mappedActive } : {}),
    });
  }
  return normalizeSplitGroups(mapped);
}

export function remapSplitTree(
  tree: SplitNode | null,
  paneIdMap: ReadonlyMap<string, string>,
): SplitNode | null {
  if (!tree) return null;
  const seen = new Set<string>();
  const seenBranches = new Set<string>();
  const walk = (node: SplitNode, depth: number): SplitNode | null => {
    if (depth > MAX_SPLIT_PANES) return null;
    if (node.kind === "leaf") {
      const mapped = paneIdMap.get(node.paneId);
      if (!mapped || seen.has(mapped) || seen.size >= MAX_SPLIT_PANES) return null;
      seen.add(mapped);
      return leaf(mapped);
    }
    const a = walk(node.a, depth + 1);
    const b = walk(node.b, depth + 1);
    if (!a && !b) return null;
    if (!a) return b;
    if (!b) return a;
    // Keep the branch's id: a sash drag holds it from pointer-down, and the
    // sashes are keyed by it, so a fresh id on every write ends the drag.
    const id = node.id && !seenBranches.has(node.id) ? node.id : mintBranchId();
    seenBranches.add(id);
    return {
      kind: "branch",
      id,
      direction: node.direction,
      ratio: clampSplitRatio(node.ratio),
      a,
      b,
    };
  };
  return normalizeRoot(walk(tree, 0));
}

export function hydrateSplitTree(
  raw: string | null | undefined,
  validPaneIds: ReadonlySet<string>,
): SplitNode | null {
  const parsed = parsePersistedSplitWorkspace(raw);
  if (!parsed) return null;
  const identityMap = new Map<string, string>();
  for (const id of validPaneIds) identityMap.set(id, id);
  return remapSplitTree(parsed.tree, identityMap);
}

export type SplitSlotChord =
  | "focus-pane-1"
  | "focus-pane-2"
  | "focus-pane-3"
  | "focus-pane-4"
  | "focus-pane-5"
  | "focus-pane-6"
  | "focus-pane-7"
  | "focus-pane-8";

export type SplitChord =
  | "split-right"
  | "split-down"
  | "zoom-pane"
  | "unsplit"
  | "focus-left"
  | "focus-right"
  | "focus-up"
  | "focus-down"
  | "focus-next-pane"
  | "focus-prev-pane"
  | "focus-next-group"
  | "focus-prev-group"
  | SplitSlotChord
  | "ignore";

function altSplitKey(event: { key: string; code?: string }): SplitChord | null {
  if (event.key === "ArrowLeft") return "focus-left";
  if (event.key === "ArrowRight") return "focus-right";
  if (event.key === "ArrowUp") return "focus-up";
  if (event.key === "ArrowDown") return "focus-down";
  if (event.key === "]" || event.code === "BracketRight") return "focus-next-pane";
  if (event.key === "[" || event.code === "BracketLeft") return "focus-prev-pane";
  if (event.key === "0" || event.code === "Digit0") return "unsplit";
  const digitMatch =
    /^[1-8]$/.exec(event.key)?.[0] ??
    /^Digit([1-8])$/.exec(event.code ?? "")?.[1];
  if (digitMatch) {
    return `focus-pane-${digitMatch}` as SplitSlotChord;
  }
  return null;
}

function altShiftSplitKey(event: { key: string; code?: string }): SplitChord | null {
  if (event.code === "BracketRight" || event.key === "}" || event.key === "]") {
    return "focus-next-group";
  }
  if (event.code === "BracketLeft" || event.key === "{" || event.key === "[") {
    return "focus-prev-group";
  }
  return null;
}

/**
 * Spatial split, pane-zoom, non-destructive split-pane focus cycling, pair switching, and slot jump chords:
 * - macOS: `⌘D` (split right), `⌘⇧D` (split down), `⌘⇧Enter` (zoom pane),
 *   `⌘⌥←/→/↑/↓` (focus neighbor), `⌘⌥]` / `⌘⌥[` (next/prev split pane),
 *   `⌘⌥⇧]` / `⌘⌥⇧[` (next/prev terminal pair),
 *   `⌘⌥1–8` (jump to split pane slot), `⌘⌥0` (close split view)
 * - Linux/Windows: `Ctrl+Shift+D`, `Ctrl+Shift+Alt+D`, `Ctrl+Shift+Enter`,
 *   `Ctrl+Alt+Arrow`, `Ctrl+Alt+]` / `Ctrl+Alt+[`, `Ctrl+Alt+Shift+]` / `[`,
 *   `Ctrl+Alt+1–8`, `Ctrl+Alt+0`
 */
export function terminalSplitChord(
  event: {
    key: string;
    code?: string;
    ctrlKey: boolean;
    metaKey: boolean;
    altKey: boolean;
    shiftKey: boolean;
    isComposing?: boolean;
    repeat?: boolean;
  },
  os: string = "macos",
): SplitChord | null {
  if (event.isComposing) return null;
  const lower = event.key.toLowerCase();

  if (os === "macos") {
    if (event.metaKey && !event.ctrlKey && !event.altKey) {
      if (lower === "d") {
        if (event.repeat) return "ignore";
        return event.shiftKey ? "split-down" : "split-right";
      }
      if (event.key === "Enter" && event.shiftKey) {
        return event.repeat ? "ignore" : "zoom-pane";
      }
    }
    if (event.metaKey && event.altKey && !event.ctrlKey) {
      if (event.shiftKey) return altShiftSplitKey(event);
      const hit = altSplitKey(event);
      if (hit === "unsplit" && event.repeat) return "ignore";
      return hit;
    }
    return null;
  }

  if (event.ctrlKey && !event.metaKey) {
    if (event.shiftKey && lower === "d") {
      if (event.repeat) return "ignore";
      return event.altKey ? "split-down" : "split-right";
    }
    if (event.shiftKey && !event.altKey && event.key === "Enter") {
      return event.repeat ? "ignore" : "zoom-pane";
    }
    if (event.altKey) {
      if (event.shiftKey) return altShiftSplitKey(event);
      const hit = altSplitKey(event);
      if (hit === "unsplit" && event.repeat) return "ignore";
      return hit;
    }
  }
  return null;
}
