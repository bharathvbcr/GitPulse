import { askConfirm } from "../stores/modalStore";
import { uninstallExternalTool, type ExternalTool } from "./externalTools";

export interface UninstallDeps {
  askConfirm: typeof askConfirm;
  uninstallExternalTool: typeof uninstallExternalTool;
}

/**
 * Asks, in the app's own dialog, before removing a GitPulse-owned CLI.
 *
 * `window.confirm` returns null under WKWebView (see `modalStore`), so the
 * previous `!window.confirm(...)` guard read every click as a "no" and the
 * button did nothing at all. Resolves true only when the uninstall ran.
 */
export async function confirmAndUninstall(
  tool: ExternalTool,
  deps: UninstallDeps = { askConfirm, uninstallExternalTool },
): Promise<boolean> {
  const confirmed = await deps.askConfirm({
    title: `Remove ${tool}?`,
    message: `Removes the GitPulse-owned ${tool} binary if it lives in the app bin directory. A cargo or go install on PATH is left on disk.`,
    confirmLabel: "Remove",
    destructive: true,
  });
  if (!confirmed) return false;
  await deps.uninstallExternalTool(tool);
  return true;
}
