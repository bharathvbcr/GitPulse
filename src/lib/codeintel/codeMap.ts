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
  paintCodeGraph as packagePaintCodeGraph,
  type CodeGraphPaintRequest,
} from "../../../src-tauri/vendored/devmap-query/assets/code-map/src/codeGraphRenderer";
import type { GraphVizNode } from "../../../src-tauri/vendored/devmap-query/assets/code-map/src/types";

export * from "../../../src-tauri/vendored/devmap-query/assets/code-map/src/areaTree";
export * from "../../../src-tauri/vendored/devmap-query/assets/code-map/src/areaTreeNav";
export * from "../../../src-tauri/vendored/devmap-query/assets/code-map/src/globeLayout";
export * from "../../../src-tauri/vendored/devmap-query/assets/code-map/src/graphNavigation";
export * from "../../../src-tauri/vendored/devmap-query/assets/code-map/src/graphPayload";
export * from "../../../src-tauri/vendored/devmap-query/assets/code-map/src/labelDisambiguation";
export * from "../../../src-tauri/vendored/devmap-query/assets/code-map/src/nodeLabel";
export {
  DEFAULT_CODE_GRAPH_THEME,
  GLOBE_LABEL_MIN_DEPTH,
  hitTestCodeGraph,
  type CodeGraphGlobeFrame,
  type CodeGraphHit,
  type CodeGraphHitKind,
  type CodeGraphPaintRequest,
  type CodeGraphTheme,
} from "../../../src-tauri/vendored/devmap-query/assets/code-map/src/codeGraphRenderer";

/** A node's swatch for the theme, from the app's language palette. */
export function getCodeGraphColor(node: GraphVizNode, light = false): string {
  return packageCodeGraphColor(node, light, getLanguageIconColor);
}

/** Paint the code graph, in the app's language palette unless the caller supplies one. */
export function paintCodeGraph(ctx: CanvasRenderingContext2D, req: CodeGraphPaintRequest): void {
  packagePaintCodeGraph(ctx, { ...req, colorFor: req.colorFor ?? getLanguageIconColor });
}
