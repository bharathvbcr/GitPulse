import type { MountLoader } from "./LazyMount.svelte";

/**
 * Loaders for components kept out of the entry chunk (see lazyChunks.test.ts).
 *
 * Declared once, here, because `LazyMount` caches a component by the IDENTITY of
 * its loader: the warm-up below, the call sites and the harnesses must all hand
 * it these very functions, or a "warmed" chunk would be fetched again as a
 * second, separately cached copy.
 */
export const loadProductTour: MountLoader = () => import("./onboarding/ProductTour.svelte");
export const loadMergeBranchDialog: MountLoader = () => import("./MergeBranchDialog.svelte");
export const loadNativeNotificationBridge: MountLoader = () => import("./NativeNotificationBridgeView.svelte");
export const loadViewGuideArt: MountLoader = () => import("./ViewGuideArt.svelte");

/**
 * Chunks worth having before the first interaction asks for them. Both are
 * opened by a hover or a click, where an import round trip is visible: the guide
 * tooltip would show an empty schematic band, and the merge dialog would lag its
 * button. The walkthrough is not here; App mounts it at startup.
 */
export const WARMED_LOADERS: readonly MountLoader[] = [loadViewGuideArt, loadMergeBranchDialog];

type Schedule = (task: () => void) => void;

/** Idle time where the webview has it; WKWebView does not, so a short timer there. */
const afterFirstPaint: Schedule = (task) => {
  if (typeof requestIdleCallback === "function") requestIdleCallback(() => task(), { timeout: 4000 });
  else setTimeout(task, 1500);
};

/**
 * Fetches the deferred chunks once the app has painted. A failure here is
 * deliberately quiet: nothing was asked for yet, and the real use goes through
 * `LazyMount`, which retries the import and reports a failure to the user.
 */
export function warmLazyChunks(
  schedule: Schedule = afterFirstPaint,
  loaders: readonly MountLoader[] = WARMED_LOADERS,
): void {
  schedule(() => {
    for (const load of loaders) void load().catch(() => undefined);
  });
}
