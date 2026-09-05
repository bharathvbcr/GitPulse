/**
 * Applying the UI scale preference to something the user can actually see.
 *
 * The previous mechanism could not work. `--ui-font-scale` was written on a
 * `<div>` inside `<body>` and read by a `body { font-size: ... }` rule; custom
 * properties inherit downward only, so `body` always resolved the `:root`
 * declaration of `1` and the preference moved nothing. Two settings surfaces
 * (⌘+/⌘−/⌘0 and the Appearance slider) changed a persisted number and left the
 * window identical, and the only test in the area asserted the store's
 * clamping — a check that could not have caught it.
 *
 * The replacement is the webview's own zoom rather than a font-size rule.
 * Scaling text alone leaves the canvas commit graph, the terminal and every
 * fixed row height behind, so a "larger interface" setting would still hand
 * back a half-scaled window. `setZoom` is implemented on all three shipped
 * platforms — WKWebView `setPageZoom`, WebKitGTK `set_zoom_level`, WebView2
 * `SetZoomFactor` — and scales layout, canvas and PTY output together.
 *
 * The CSS fallback below is for contexts with no webview to ask (tests, a
 * plain browser, a Tauri build whose zoom call is refused). It writes the
 * variable to `documentElement`, which is where the stylesheet reads it, and
 * scales the root font size so every rem-based Tailwind utility follows. The
 * two paths are mutually exclusive: whichever applies sets the other's input
 * to the identity value, so a scale can never be applied twice.
 */

/** Bounds mirrored from interfaceStore's clampScale; see MIN/MAX there. */
export const MIN_UI_SCALE = 0.75;
export const MAX_UI_SCALE = 1.5;

export interface UiScaleTargets {
  /** Sets the native webview zoom; absent or throwing selects the CSS path. */
  setZoom?: ((scale: number) => Promise<void>) | null;
  /** Element carrying the CSS custom property; normally documentElement. */
  root?: { style: { setProperty(name: string, value: string): void } } | null;
}

/**
 * Clamps to the supported range and drops values that are not real numbers.
 *
 * A non-finite scale reaching `setZoom` is not a smaller problem than a wrong
 * one: WebView2 rejects it and WKWebView applies NaN, which blanks the window.
 */
export function normalizeUiScale(scale: number): number {
  if (!Number.isFinite(scale)) return 1;
  return Math.min(MAX_UI_SCALE, Math.max(MIN_UI_SCALE, scale));
}

/** Which mechanism actually carried the scale, so callers can report it. */
export type UiScaleMode = "webview-zoom" | "css-fallback";

/**
 * Applies `scale` and says which path did it.
 *
 * Deliberately reports rather than returning void: "the setting did nothing"
 * is exactly the failure this module exists to end, so a caller that wants to
 * surface or log the outcome can.
 */
export async function applyUiScale(
  scale: number,
  targets: UiScaleTargets = {},
): Promise<UiScaleMode> {
  const value = normalizeUiScale(scale);
  const root =
    targets.root ??
    (typeof document !== "undefined" ? document.documentElement : null);

  if (targets.setZoom) {
    try {
      await targets.setZoom(value);
      // Native zoom already scales the whole document. Leaving the CSS
      // variable at the user's value here would scale the root font a second
      // time on top of it.
      root?.style.setProperty("--ui-font-scale", "1");
      return "webview-zoom";
    } catch {
      /* fall through to the stylesheet path */
    }
  }

  root?.style.setProperty("--ui-font-scale", String(value));
  return "css-fallback";
}

/**
 * The webview zoom setter for this process, or null outside Tauri.
 *
 * Imported dynamically so a browser or test context never pulls the Tauri
 * webview module, matching how `windowChrome` reaches the window API.
 */
export async function nativeZoomSetter(
  isTauriHost: boolean,
): Promise<((scale: number) => Promise<void>) | null> {
  if (!isTauriHost) return null;
  try {
    const { getCurrentWebview } = await import("@tauri-apps/api/webview");
    const webview = getCurrentWebview();
    return (scale: number) => webview.setZoom(scale);
  } catch {
    return null;
  }
}
