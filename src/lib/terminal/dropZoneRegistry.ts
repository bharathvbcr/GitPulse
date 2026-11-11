/**
 * Unified drop-zone hit-testing for OS Finder file drops (`onDragDropEvent`)
 * and in-app Explorer HTML5 file drags (`FileTreePanel`).
 *
 * Why a registry exists:
 * - Tauri's native window drag-drop intercepts OS file drags at the `WKWebView`
 *   level, reporting physical-pixel coordinates without firing DOM `dragover` /
 *   `drop` on inner elements, and previously raised the full-window "Drop a Git
 *   repository" veil across the entire window.
 * - Registering visible terminal/split panes here lets `nativeShell.ts` hit-test
 *   the cursor in CSS pixels, suppress the repository-open veil while hovering
 *   the terminal dock, highlight center ("attach/paste") vs edge ("split & open
 *   file") regions, and route the drop into the exact pane.
 */

export type DropRegion = "center" | "left" | "right" | "top" | "bottom";

export interface DropRect {
  left: number;
  top: number;
  width: number;
  height: number;
}

export interface DropZoneTarget {
  id: string;
  getRect: () => DropRect | null;
  isVisible: () => boolean;
  /** Whether outer edges may offer a split-open file zone for `paths`. */
  canSplitEdges?: (paths: readonly string[]) => boolean;
  onHover?: (region: DropRegion | null, paths: readonly string[]) => void;
  onDrop: (paths: string[], region: DropRegion) => boolean;
}

export interface InAppFileDrag {
  paths: string[];
  filesOnly: boolean;
}

const EDGE_BAND_RATIO = 0.22;

/**
 * Converts Tauri's physical-pixel webview cursor position into CSS pixels
 * matching `Element.getBoundingClientRect()`.
 *
 * `scaleFactor` converts physical backing pixels into window points, and
 * `pageZoom` converts window points into zoomed CSS pixels (inverse of
 * `toWindowPoints` in `browserSession.ts`).
 */
export function physicalToCssPoint(
  physical: { x: number; y: number },
  scaleFactor: number,
  zoom: number,
): { x: number; y: number } {
  const sf = Number.isFinite(scaleFactor) && scaleFactor > 0 ? scaleFactor : 1;
  const z = Number.isFinite(zoom) && zoom > 0 ? zoom : 1;
  const divisor = sf * z;
  return {
    x: physical.x / divisor,
    y: physical.y / divisor,
  };
}

/**
 * Classifies a CSS-pixel cursor point inside `rect` into `"center"` or one of
 * the four outer 22% split edges (`"left" | "right" | "top" | "bottom"`).
 */
export function classifyDropRegion(
  point: { x: number; y: number },
  rect: DropRect,
  allowEdges = true,
): DropRegion | null {
  if (rect.width <= 0 || rect.height <= 0) return null;
  if (
    point.x < rect.left ||
    point.x > rect.left + rect.width ||
    point.y < rect.top ||
    point.y > rect.top + rect.height
  ) {
    return null;
  }
  if (!allowEdges) return "center";

  const rx = (point.x - rect.left) / rect.width;
  const ry = (point.y - rect.top) / rect.height;
  const distLeft = rx;
  const distRight = 1 - rx;
  const distTop = ry;
  const distBottom = 1 - ry;
  const minEdge = Math.min(distLeft, distRight, distTop, distBottom);

  if (minEdge >= EDGE_BAND_RATIO) return "center";
  if (minEdge === distLeft) return "left";
  if (minEdge === distRight) return "right";
  if (minEdge === distTop) return "top";
  return "bottom";
}

export interface DropZoneRegistry {
  register: (target: DropZoneTarget) => () => void;
  hoverCssPoint: (
    point: { x: number; y: number },
    paths: readonly string[],
  ) => { id: string; region: DropRegion } | null;
  clearHover: () => void;
  dropAtCssPoint: (point: { x: number; y: number }, paths: readonly string[]) => boolean;
  beginInAppDrag: (paths: readonly string[], filesOnly?: boolean) => void;
  currentInAppDrag: () => InAppFileDrag | null;
  endInAppDrag: () => void;
}

export function createDropZoneRegistry(options?: {
  isPointOccluded?: (point: { x: number; y: number }) => boolean;
}): DropZoneRegistry {
  const targets = new Map<string, DropZoneTarget>();
  let hoveredId: string | null = null;
  let hoveredRegion: DropRegion | null = null;
  let inAppDrag: InAppFileDrag | null = null;

  function resolveHit(
    point: { x: number; y: number },
    paths: readonly string[],
  ): { target: DropZoneTarget; region: DropRegion } | null {
    if (options?.isPointOccluded?.(point)) return null;
    // Iterate in reverse registration order so later/topmost panes win if overlapping.
    const list = [...targets.values()];
    for (let i = list.length - 1; i >= 0; i--) {
      const target = list[i];
      if (!target.isVisible()) continue;
      const rect = target.getRect();
      if (!rect) continue;
      const allowEdges = target.canSplitEdges ? target.canSplitEdges(paths) : true;
      const region = classifyDropRegion(point, rect, allowEdges);
      if (region) return { target, region };
    }
    return null;
  }

  function clearHover() {
    if (hoveredId !== null) {
      const prev = targets.get(hoveredId);
      hoveredId = null;
      hoveredRegion = null;
      prev?.onHover?.(null, []);
    }
  }

  return {
    register(target) {
      targets.set(target.id, target);
      return () => {
        if (hoveredId === target.id) {
          hoveredId = null;
          hoveredRegion = null;
        }
        if (targets.get(target.id) === target) {
          targets.delete(target.id);
        }
      };
    },
    hoverCssPoint(point, paths) {
      const hit = resolveHit(point, paths);
      if (!hit) {
        clearHover();
        return null;
      }
      if (hoveredId !== hit.target.id) {
        if (hoveredId !== null) {
          targets.get(hoveredId)?.onHover?.(null, []);
        }
        hoveredId = hit.target.id;
        hoveredRegion = hit.region;
        hit.target.onHover?.(hit.region, paths);
      } else if (hoveredRegion !== hit.region) {
        hoveredRegion = hit.region;
        hit.target.onHover?.(hit.region, paths);
      }
      return { id: hit.target.id, region: hit.region };
    },
    clearHover,
    dropAtCssPoint(point, paths) {
      const clean = paths.filter((p) => p.length > 0);
      if (clean.length === 0) {
        clearHover();
        return false;
      }
      const hit = resolveHit(point, clean);
      clearHover();
      if (!hit) return false;
      return hit.target.onDrop(clean, hit.region);
    },
    beginInAppDrag(paths, filesOnly = true) {
      const clean = paths.filter((p) => p.length > 0);
      inAppDrag = clean.length > 0 ? { paths: clean, filesOnly } : null;
    },
    currentInAppDrag() {
      return inAppDrag;
    },
    endInAppDrag() {
      inAppDrag = null;
      clearHover();
    },
  };
}

function isPointCoveredByBodyPortal(point: { x: number; y: number }): boolean {
  if (typeof document === "undefined" || !document.body) return false;
  const appRoot = document.getElementById("app");
  for (const child of document.body.children) {
    if (child === appRoot || !(child instanceof HTMLElement)) continue;
    const role = child.getAttribute("role");
    if (role !== "dialog" && role !== "alertdialog" && !child.hasAttribute("data-modal-backdrop")) {
      continue;
    }
    const rect = child.getBoundingClientRect();
    if (
      rect.width >= 4 &&
      rect.height >= 4 &&
      point.x >= rect.left &&
      point.x <= rect.left + rect.width &&
      point.y >= rect.top &&
      point.y <= rect.top + rect.height
    ) {
      return true;
    }
  }
  return false;
}

export const terminalDropZones = createDropZoneRegistry({
  isPointOccluded: isPointCoveredByBodyPortal,
});
