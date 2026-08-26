import type { GraphRenderer, GraphTheme, VisualCommitRow } from "./GraphRenderer";
import { themeSignatureOf, type GraphStaticCache } from "./graphCache";
import { fillOpaqueBackground, syncCanvasBackingStore } from "./gpuContext";

/**
 * Per-frame composition of the commit graph onto the visible canvas.
 *
 * Extracted from CommitTable so the layering contract — background fill,
 * static strip blits, dangling-parent stubs, live emphasis rings — is one
 * owned, unit-testable seam instead of an ordering implicit in component
 * code. The cache bypass path (degenerate graphs, zero-width gutter) falls
 * back to a full render here and nothing else about it changes.
 */

export interface GraphFrameRequest {
  rows: VisualCommitRow[];
  /** Bumped when the row array identity changed (payload, filter, repo). */
  dataVersion: number;
  widthCss: number;
  heightCss: number;
  dpr: number;
  /** Density mode signature ("spacious" / "compact"). */
  densitySignature: string;
  /** Resolved palette; also keys the strip cache via its signature. */
  theme: GraphTheme;
  scrollTop: number;
  startIndex: number;
  endIndex: number;
  selectedCommitId: string | null;
  headCommitId: string | null;
  hoveredCommitId: string | null;
  hoverStrength: number;
  selectionStrength: number;
  /**
   * Author-avatar column. When enabled, avatars are baked into the static
   * strips (and drawn on the bypass path); the live overlay never stamps
   * them. `avatarX` is the column centre in CSS pixels — the caller owns
   * gutter sizing, the composite only forwards.
   */
  showAvatars?: boolean;
  avatarX?: number | null;
}

/**
 * Paints one frame. Returns true when the static layer covered the request
 * (strips blitted, only emphasis re-stroked); false means a full render ran.
 */
export function paintGraphFrame(
  ctx: CanvasRenderingContext2D,
  canvas: HTMLCanvasElement,
  renderer: GraphRenderer,
  cache: GraphStaticCache,
  req: GraphFrameRequest,
): boolean {
  const { rowHeight } = renderer.getConfig();
  syncCanvasBackingStore(canvas, ctx, req.widthCss, req.heightCss, req.dpr);
  fillOpaqueBackground(ctx, req.widthCss, req.heightCss, req.theme.background);

  // Any changed input drops the strips inside the cache.
  cache.sync(
    {
      dataVersion: req.dataVersion,
      cssWidth: req.widthCss,
      densitySignature: req.densitySignature,
      themeSignature: themeSignatureOf(req.theme),
      dpr: req.dpr,
      backgroundCssColor: req.theme.background,
      showAvatars: req.showAvatars === true,
    },
    { rowHeight, totalRows: req.rows.length },
  );

  const staticBlitted = cache.paint(ctx, {
    contentTopCss: req.scrollTop,
    viewportHeightCss: req.heightCss,
  });

  // Dangling-parent stubs live outside the strip cache on purpose: translucent
  // geometry crossing a strip edge would clip mid-fade and show alpha seams at
  // each tile boundary. Visible stubs are few (bounded by lookback), so
  // drawing them whole on this single surface costs nothing measurable.
  renderer.drawDanglingStubs(
    ctx,
    req.rows,
    req.startIndex,
    req.endIndex,
    req.scrollTop,
    req.heightCss,
  );

  // Long connectors are likewise overlay-owned: a span beyond LOOKBACK_ROWS
  // cannot be baked into tiles without its middle landing on no strip seam
  // contract, so when strips covered the frame the overlay supplies those
  // edges whole here (the bypass path below draws everything itself).
  if (staticBlitted) {
    renderer.drawLongConnectors(
      ctx,
      req.rows,
      req.startIndex,
      req.endIndex,
      req.scrollTop,
      req.heightCss,
      { theme: req.theme },
    );
  }

  renderer.render(
    ctx,
    req.rows,
    req.startIndex,
    req.endIndex,
    req.scrollTop,
    req.selectedCommitId ?? undefined,
    {
      theme: req.theme,
      headCommitId: req.headCommitId,
      hoveredCommitId: req.hoveredCommitId,
      hoverStrength: req.hoverStrength,
      selectionStrength: req.selectionStrength,
      viewportHeight: req.heightCss,
      emphasisOnly: staticBlitted,
      showAvatars: req.showAvatars === true,
      avatarX: req.showAvatars ? (req.avatarX ?? null) : null,
    },
  );

  return staticBlitted;
}
