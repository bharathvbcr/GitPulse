import { isMacOS } from "../platform";

/**
 * Native shortcut ownership shared by App and RepoTabBar.
 *
 * Under Tauri the native application menu registers accelerators for a
 * subset of these (see src-tauri/src/desktop/menu.rs):
 *   - Cmd/Ctrl+Shift+W  → close active tab   (CLOSE_REPO_TAB_ACCEL)
 *   - Ctrl+Tab          → next tab           (NEXT_REPO_TAB_ACCEL)
 *   - Ctrl+Shift+Tab    → previous tab       (PREV_REPO_TAB_ACCEL)
 * When both the menu accelerator and the window keydown listener fire, every
 * action happens twice (the historical double-close bug). `shouldSkipWebviewShortcut`
 * reports when JS must stand down because the native layer owns the chord.
 * In plain browser builds no native menu exists, so nothing is skipped.
 */

export type ShortcutFamily =
  | "closeActiveTab"
  | "jumpToTab"
  | "cycleTabs"
  | "zoom"
  | "shortcuts"
  | "openRepo";

export interface ShortcutKeyState {
  key: string;
  code: string;
  ctrlKey: boolean;
  altKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
}

/** Families whose accelerators are also registered on the native Tauri menu. */
const NATIVE_OWNED_FAMILIES: ReadonlySet<ShortcutFamily> = new Set([
  "closeActiveTab",
  "cycleTabs",
  "zoom",
  "shortcuts",
]);

export function classifyShortcut(e: ShortcutKeyState): ShortcutFamily | null {
  const meta = e.metaKey || e.ctrlKey;
  // Only the exact native accelerators stand down. Shift+= remains a
  // webview alias for zoom in, and plain '?' still opens the shortcuts sheet.
  if (meta && !e.altKey && !e.shiftKey) {
    if (e.key === "=" || e.key === "-" || e.key === "0") return "zoom";
    if (e.key === "/") return "shortcuts";
  }
  if (meta && e.shiftKey && e.key.toLowerCase() === "w") {
    return "closeActiveTab";
  }
  if (e.ctrlKey && e.altKey && !e.metaKey && /^Digit([1-9])$/.test(e.code)) {
    return "jumpToTab";
  }
  if (e.key === "Tab" && e.ctrlKey && !e.metaKey && !e.altKey) {
    return "cycleTabs";
  }
  if (meta && !e.shiftKey && e.key.toLowerCase() === "t") {
    return "openRepo";
  }
  return null;
}

/**
 * True when this key event must NOT be handled by the webview because the
 * native Tauri menu accelerator already performs the action.
 */
export function shouldSkipWebviewShortcut(
  event: ShortcutKeyState,
  tauri: boolean,
  macOS = isMacOS(),
): boolean {
  if (!tauri) return false;
  const family = classifyShortcut(event);
  if (family === null || !NATIVE_OWNED_FAMILIES.has(family)) return false;
  // Ctrl+Tab is Control everywhere. CmdOrCtrl resolves to Command on macOS
  // and Control elsewhere; the other modifier remains a webview alias.
  if (family === "cycleTabs") return true;
  const nativeModifier = macOS
    ? event.metaKey && !event.ctrlKey
    : event.ctrlKey && !event.metaKey;
  return nativeModifier && !event.altKey;
}
