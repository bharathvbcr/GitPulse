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
import type { TabKind } from "./tabs";

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

export function resizeBranch(
  root: SplitNode | null,
  branchId: string,
  ratio: number,
): SplitNode | null {
  if (!root || !branchId) return root;
  const clamped = clampSplitRatio(ratio);

  const walk = (node: SplitNode): SplitNode => {
    if (node.kind === "leaf") return node;
    if (node.id === branchId) {
      return Math.abs(node.ratio - clamped) < 1e-6 ? node : { ...node, ratio: clamped };
    }
    const nextA = walk(node.a);
    if (nextA !== node.a) return { ...node, a: nextA };
    const nextB = walk(node.b);
    if (nextB !== node.b) return { ...node, b: nextB };
    return node;
  };
  return walk(root);
}

export function equalizeSplits(root: SplitNode | null, branchId?: string): SplitNode | null {
  if (!root) return null;
  if (branchId) return resizeBranch(root, branchId, 0.5);

  const walk = (node: SplitNode): SplitNode => {
    if (node.kind === "leaf") return node;
    const nextA = walk(node.a);
    const nextB = walk(node.b);
    const sameRatio = Math.abs(node.ratio - 0.5) < 1e-6;
    if (sameRatio && nextA === node.a && nextB === node.b) return node;
    return { ...node, ratio: 0.5, a: nextA, b: nextB };
  };
  return walk(root);
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

  if (zoomedPaneId && ((root?.kind === "branch" && hasPane(root, zoomedPaneId)) || (!root && zoomedPaneId === activeId))) {
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
  readonly filePath?: string;
  readonly revealLine?: number | null;
  readonly revealColumn?: number | null;
  readonly url?: string;
  readonly checkout?: string;
}

export interface PersistedSplitWorkspace {
  readonly version: 2;
  readonly tree: SplitNode;
  readonly leaves: Record<string, PersistedSplitLeafMeta>;
}

export function serializeSplitTree(root: SplitNode | null): string | null {
  if (!root || root.kind !== "branch") return null;
  return JSON.stringify(root);
}

export function serializeSplitWorkspace(
  root: SplitNode | null,
  leaves: Record<string, PersistedSplitLeafMeta>,
): string | null {
  if (!root || root.kind !== "branch") return null;
  const usedIds = leafPaneIds(root);
  const filtered: Record<string, PersistedSplitLeafMeta> = {};
  for (const id of usedIds) {
    if (leaves[id]) filtered[id] = leaves[id];
  }
  const payload: PersistedSplitWorkspace = { version: 2, tree: root, leaves: filtered };
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

export function parsePersistedSplitWorkspace(
  raw: string | null | undefined,
): { tree: SplitNode; leaves: Record<string, PersistedSplitLeafMeta> } | null {
  if (!raw) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return null;
  const rec = parsed as Record<string, unknown>;
  if (rec.version === 2 && rec.tree && typeof rec.tree === "object") {
    const tree = parseRawSplitNode(rec.tree, 0);
    if (!tree || tree.kind !== "branch") return null;
    const leavesRaw =
      rec.leaves && typeof rec.leaves === "object" && !Array.isArray(rec.leaves)
        ? (rec.leaves as Record<string, unknown>)
        : {};
    const leaves: Record<string, PersistedSplitLeafMeta> = {};
    for (const [id, item] of Object.entries(leavesRaw)) {
      if (!item || typeof item !== "object" || Array.isArray(item)) continue;
      const meta = item as Record<string, unknown>;
      if (typeof meta.launcher !== "string") continue;
      leaves[id] = {
        launcher: meta.launcher as TabKind,
        slotIndex: typeof meta.slotIndex === "number" && Number.isFinite(meta.slotIndex) ? meta.slotIndex : 0,
        ...(typeof meta.backendSessionId === "string" && meta.backendSessionId ? { backendSessionId: meta.backendSessionId } : {}),
        ...(typeof meta.filePath === "string" && meta.filePath ? { filePath: meta.filePath } : {}),
        ...(typeof meta.revealLine === "number" ? { revealLine: meta.revealLine } : {}),
        ...(typeof meta.revealColumn === "number" ? { revealColumn: meta.revealColumn } : {}),
        ...(typeof meta.url === "string" && meta.url ? { url: meta.url } : {}),
        ...(typeof meta.checkout === "string" && meta.checkout ? { checkout: meta.checkout } : {}),
      };
    }
    return { tree, leaves };
  }
  const v1Tree = parseRawSplitNode(parsed, 0);
  if (!v1Tree || v1Tree.kind !== "branch") return null;
  return { tree: v1Tree, leaves: {} };
}

export function remapSplitTree(
  tree: SplitNode | null,
  paneIdMap: ReadonlyMap<string, string>,
): SplitNode | null {
  if (!tree) return null;
  const seen = new Set<string>();
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
    return {
      kind: "branch",
      id: mintBranchId(),
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

export type SplitChord =
  | "split-right"
  | "split-down"
  | "zoom-pane"
  | "focus-left"
  | "focus-right"
  | "focus-up"
  | "focus-down"
  | "ignore";

/**
 * Spatial split and pane-zoom keyboard chords:
 * - macOS: `⌘D` (split right), `⌘⇧D` (split down), `⌘⇧Enter` (zoom pane), `⌘⌥←/→/↑/↓` (focus neighbor)
 * - Linux/Windows: `Ctrl+Shift+D`, `Ctrl+Shift+Alt+D`, `Ctrl+Shift+Enter`, `Ctrl+Alt+Arrow`
 */
export function terminalSplitChord(
  event: {
    key: string;
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
    if (event.metaKey && event.altKey && !event.ctrlKey && !event.shiftKey) {
      if (event.key === "ArrowLeft") return "focus-left";
      if (event.key === "ArrowRight") return "focus-right";
      if (event.key === "ArrowUp") return "focus-up";
      if (event.key === "ArrowDown") return "focus-down";
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
    if (event.altKey && !event.shiftKey) {
      if (event.key === "ArrowLeft") return "focus-left";
      if (event.key === "ArrowRight") return "focus-right";
      if (event.key === "ArrowUp") return "focus-up";
      if (event.key === "ArrowDown") return "focus-down";
    }
  }
  return null;
}
