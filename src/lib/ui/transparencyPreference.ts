import { listen } from "@tauri-apps/api/event";
import { invoke } from "../ipc/invoke";
import { isTauri } from "../platform";

/**
 * The system's Reduce Transparency setting, stamped on `<html>`.
 *
 * WKWebView does not implement `prefers-reduced-transparency` (both `reduce`
 * and `no-preference` evaluate false, verified 2026-11-09 on macOS 27.2), so
 * the media-query fallbacks in app.css and StatusPopover.svelte never fire for
 * a user who turned the setting on. The native side
 * (`src-tauri/src/desktop/accessibility.rs`) reads
 * `NSWorkspace.accessibilityDisplayShouldReduceTransparency`, and this module
 * mirrors it as `data-transparency` so the same rules can key off the
 * attribute beside the media query.
 *
 * Three states, never two: `"unknown"` means the read could not run. It is
 * stamped as itself, so a check that could not run never looks like the
 * preference being off, and no rule claims reduced from it.
 */
export const TRANSPARENCY_ATTRIBUTE = "data-transparency";
export const REDUCED = "reduced";
export const UNKNOWN = "unknown";

export const ACCESSIBILITY_EVENT = "gitpulse-accessibility-display";
export const ACCESSIBILITY_COMMAND = "cmd_get_accessibility_display";

export type Transparency = "reduced" | "standard" | "unknown";

/** `AccessibilityDisplay` in src-tauri/src/desktop/accessibility.rs. */
export interface AccessibilityDisplay {
  reduceTransparency: Transparency;
  /** Why the answer is unknown, or why later changes will not arrive. */
  reason: string | null;
  /** Orders reads: a larger number is a later read. */
  seq: number;
}

/** The element carrying the attribute; normally `<html>`. */
export interface TransparencyTarget {
  setAttribute(name: string, value: string): void;
  removeAttribute(name: string): void;
}

function documentRoot(): TransparencyTarget | null {
  return typeof document !== "undefined" ? document.documentElement : null;
}

/**
 * The reader's own "Transparency: reduced" choice (Settings -> Appearance).
 * It is one-way, like the motion switch: it can add reduction to a system that
 * reports `standard` or `unknown`, and can never remove the system's. It lives
 * here because this module is the attribute's single writer; a second writer
 * would be undone the next time the window regains focus and re-reads the
 * system answer.
 */
let userReduced = false;
/** The system's last answer, so a change of the user's choice can re-stamp. */
let systemState: Transparency = "standard";

export function setUserTransparencyReduced(
  reduced: boolean,
  target?: TransparencyTarget | null,
): void {
  userReduced = reduced;
  applyTransparency(systemState, target);
}

/** Stamps `reduced` or `unknown`; clears the attribute for `standard`. */
export function applyTransparency(
  state: Transparency,
  target?: TransparencyTarget | null,
): void {
  systemState = state;
  const root = target ?? documentRoot();
  if (!root) return;
  if (state === "reduced" || userReduced) root.setAttribute(TRANSPARENCY_ATTRIBUTE, REDUCED);
  else if (state === "unknown") root.setAttribute(TRANSPARENCY_ATTRIBUTE, UNKNOWN);
  else root.removeAttribute(TRANSPARENCY_ATTRIBUTE);
}

/** What the native side may be asked and may say; injected by tests. */
export interface TransparencySource {
  read(): Promise<AccessibilityDisplay>;
  subscribe(receive: (display: AccessibilityDisplay) => void): Promise<() => void>;
  /** Where a window gets a reason to re-read: it was shown or focused again. */
  onRecheck?(recheck: () => void): () => void;
  target?: TransparencyTarget | null;
  /** Called with the reason whenever the state is unknown or unobserved. */
  report?(message: string): void;
}

function nativeSource(): TransparencySource {
  return {
    read: () => invoke<AccessibilityDisplay>(ACCESSIBILITY_COMMAND),
    subscribe: (receive) =>
      listen<AccessibilityDisplay>(ACCESSIBILITY_EVENT, ({ payload }) => receive(payload)),
    onRecheck: (recheck) => {
      if (typeof window === "undefined") return () => {};
      window.addEventListener("focus", recheck);
      return () => window.removeEventListener("focus", recheck);
    },
    report: (message) => console.warn(message),
  };
}

/** True for a payload this module can act on; anything else is "unknown". */
function isDisplay(value: unknown): value is AccessibilityDisplay {
  if (typeof value !== "object" || value === null) return false;
  const v = value as Record<string, unknown>;
  return (
    (v.reduceTransparency === "reduced" ||
      v.reduceTransparency === "standard" ||
      v.reduceTransparency === "unknown") &&
    typeof v.seq === "number"
  );
}

/**
 * Subscribes first, then asks for the current value, so a change between the
 * two is not lost; `seq` keeps a slow reply from overwriting a newer event.
 * Resolves to a stop function. Outside Tauri there is no native side, so the
 * attribute is left alone for a harness or test to set.
 */
export async function installTransparencyPreference(
  source: TransparencySource | null = isTauri() ? nativeSource() : null,
): Promise<() => void> {
  if (!source) return () => {};
  let applied = -1;
  let stopped = false;
  const accept = (payload: unknown) => {
    if (stopped) return;
    if (!isDisplay(payload)) {
      applyTransparency("unknown", source.target);
      source.report?.("Reduce Transparency: the native answer was malformed");
      return;
    }
    if (payload.seq < applied) return;
    applied = payload.seq;
    applyTransparency(payload.reduceTransparency, source.target);
    if (payload.reason) source.report?.(`Reduce Transparency: ${payload.reason}`);
  };
  const recheck = () => {
    source.read().then(accept, failed);
  };
  const failed = (cause: unknown) => {
    if (stopped) return;
    applyTransparency("unknown", source.target);
    source.report?.(`Reduce Transparency couldn't be read: ${String(cause)}`);
  };

  let unsubscribe: () => void = () => {};
  try {
    unsubscribe = await source.subscribe(accept);
  } catch (cause) {
    failed(cause);
  }
  const unrecheck = source.onRecheck?.(recheck) ?? (() => {});
  try {
    accept(await source.read());
  } catch (cause) {
    failed(cause);
  }
  return () => {
    stopped = true;
    unsubscribe();
    unrecheck();
  };
}
