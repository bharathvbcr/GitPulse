/**
 * Canonical URL opener for the whole frontend: everything goes through the
 * Tauri opener plugin so links land in the OS browser/shell instead of the
 * webview. There is deliberately NO window.open fallback — inside a Tauri
 * webview it can navigate the app shell itself, and the URLs handed here
 * come from advisory/GitHub payloads. Failures throw so each caller decides
 * how to surface them (panel banner + diagnostics).
 */
import { openUrl } from "@tauri-apps/plugin-opener";

export type UrlOpener = (url: string) => Promise<void>;

/** Binds the opening behavior behind an injected opener (unit-testable). */
export function createOpener(opener: UrlOpener): (url: string) => Promise<void> {
  return async (url: string) => {
    if (!url.trim()) {
      throw new Error("Cannot open an empty URL");
    }
    await opener(url);
  };
}

/** App-wide canonical opener bound to the Tauri opener plugin. */
export const openExternal = createOpener(openUrl);
