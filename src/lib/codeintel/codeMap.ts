/**
 * The one seam through which GitPulse consumes DevCouncil's shared code-map
 * package (vendored at `src-tauri/vendored/devmap-query/assets/code-map/src`).
 *
 * The package owns the pure map logic (payload layout, navigation, globe
 * projection, area tree, label disambiguation, the canvas renderer) so the
 * standalone `devmap viz` page and this app draw the same map. It knows no
 * app state: the host injects what only the host owns. Here that is the
 * language palette, `getLanguageIconColor`, so a node is painted in the same
 * colour as its language logo everywhere else in the app. The `--c-*` theme
 * is resolved at the component boundary by `canvas/codeGraph/themeFromCss`.
 *
 * Nothing under `assets/code-map/src/page` (the DOM shell of the standalone
 * page) or `assets/code-map/tests` (DevCouncil's bun suite) is imported or
 * type-checked from here. App code imports from this file, never from the
 * vendored path, so a package move is a one-file change.
 */

import { getLanguageIconColor } from "../language/languageLogos";
import {
  getCodeGraphColor as packageCodeGraphColor,
  hitTestCodeGraph as packageHitTestCodeGraph,
  paintCodeGraph as packagePaintCodeGraph,
  type CodeGraphGlobeFrame,
  type CodeGraphHit,
  type CodeGraphPaintRequest,
} from "../../../src-tauri/vendored/devmap-query/assets/code-map/src/codeGraphRenderer";
import {
  buildCodeGraphModel as packageBuildCodeGraphModel,
  CODE_GRAPH_LAYOUT_HEIGHT,
  CODE_GRAPH_LAYOUT_WIDTH,
  layoutNodes as packageLayoutNodes,
  type CodeGraphModel,
  type LaidOutNode,
} from "../../../src-tauri/vendored/devmap-query/assets/code-map/src/graphPayload";
import type { GraphVizLink, GraphVizNode } from "../../../src-tauri/vendored/devmap-query/assets/code-map/src/types";

export * from "../../../src-tauri/vendored/devmap-query/assets/code-map/src/areaTree";
export * from "../../../src-tauri/vendored/devmap-query/assets/code-map/src/areaTreeNav";
export * from "../../../src-tauri/vendored/devmap-query/assets/code-map/src/globeLayout";
export * from "../../../src-tauri/vendored/devmap-query/assets/code-map/src/graphNavigation";
export {
  CODE_GRAPH_LAYOUT_HEIGHT,
  CODE_GRAPH_LAYOUT_WIDTH,
  graphLanguageLegend,
  isGraphTestPath,
  nodeCommunity,
  nodeLanguageKey,
  normalizeCounts,
  truncationLegend,
  type CodeGraphModel,
  type GraphCommunity,
  type LaidOutNode,
  type TruncationLegend,
} from "../../../src-tauri/vendored/devmap-query/assets/code-map/src/graphPayload";
export * from "../../../src-tauri/vendored/devmap-query/assets/code-map/src/labelDisambiguation";
export * from "../../../src-tauri/vendored/devmap-query/assets/code-map/src/nodeLabel";
export {
  DEFAULT_CODE_GRAPH_THEME,
  GLOBE_LABEL_MIN_DEPTH,
  type CodeGraphGlobeFrame,
  type CodeGraphHit,
  type CodeGraphHitKind,
  type CodeGraphPaintRequest,
  type CodeGraphTheme,
} from "../../../src-tauri/vendored/devmap-query/assets/code-map/src/codeGraphRenderer";

let layoutCount = 0;
let hitTestCount = 0;

/**
 * How many times the code-graph layout (`layoutNodes`, directly or via
 * `buildCodeGraphModel`) has run. A probe seam, like `fileTreeBuildCount`:
 * the browser harness reads it across viewport resizes to verify resizes
 * never re-lay out.
 */
export function codeGraphLayoutCount(): number {
  return layoutCount;
}

/**
 * How many times `hitTestCodeGraph` has run. A probe seam for the browser
 * harness to verify hover hit-testing runs at most once per animation frame.
 */
export function codeGraphHitTestCount(): number {
  return hitTestCount;
}

/** Lay out graph nodes into fixed content space and count each layout pass. */
export function layoutNodes(
  nodes: GraphVizNode[],
  width: number,
  height: number,
  options?: { scaleDocExtent?: boolean; links?: readonly GraphVizLink[] },
): LaidOutNode[] {
  layoutCount += 1;
  return packageLayoutNodes(nodes, width, height, options);
}

/** Build a render model in fixed content space and count its layout pass. */
export function buildCodeGraphModel(
  payload: unknown,
  width = CODE_GRAPH_LAYOUT_WIDTH,
  height = CODE_GRAPH_LAYOUT_HEIGHT,
): CodeGraphModel {
  // `packageBuildCodeGraphModel` calls `graphPayload.ts`'s internal `layoutNodes`
  // unconditionally once per model build, so incrementing here counts that pass
  // without double-counting.
  layoutCount += 1;
  return packageBuildCodeGraphModel(payload, width, height);
}

/** Hit-test in CSS pixel space and count each invocation. */
export function hitTestCodeGraph(
  model: CodeGraphModel,
  cssX: number,
  cssY: number,
  panX: number,
  panY: number,
  scale: number,
  visibleIds?: ReadonlySet<string> | null,
  globe?: CodeGraphGlobeFrame | null,
): CodeGraphHit | null {
  hitTestCount += 1;
  return packageHitTestCodeGraph(model, cssX, cssY, panX, panY, scale, visibleIds, globe);
}

/** A node's swatch for the theme, from the app's language palette. */
export function getCodeGraphColor(node: GraphVizNode, light = false): string {
  return packageCodeGraphColor(node, light, getLanguageIconColor);
}

/** Paint the code graph, in the app's language palette unless the caller supplies one. */
export function paintCodeGraph(ctx: CanvasRenderingContext2D, req: CodeGraphPaintRequest): void {
  packagePaintCodeGraph(ctx, { ...req, colorFor: req.colorFor ?? getLanguageIconColor });
}
