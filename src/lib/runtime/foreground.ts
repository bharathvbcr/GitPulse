/**
 * Whether background work should stop.
 *
 * `document.hidden` is the wrong signal on WKWebView: switching to another
 * app leaves `visibilityState` "visible" and only takes focus. A control in
 * this document, including the terminal, keeps `hasFocus()` true, so this
 * pause does not stop a poll while the user is typing in the shell.
 *
 * No document means the caller is not in a browser, so work still runs.
 * A missing `hasFocus` is not a blurred window. A throw, or any return other
 * than `true`, is a failed check and counts as background. A focus or blur
 * event records an override first, because `hasFocus()` can still report the
 * previous answer inside that event.
 */

export interface ForegroundDocument {
  hidden?: boolean;
  visibilityState?: string;
  hasFocus?: () => boolean;
}

/** `null` until a focus or blur event is observed. */
let focusOverride: boolean | null = null;

/** Record a focus or blur event before listeners read the document. */
export function noteForegroundFocus(focused: boolean): void {
  focusOverride = focused;
}

/** Test isolation. Production has one window, so the override lives for the page. */
export function resetForegroundFocus(): void {
  focusOverride = null;
}

/**
 * True when `doc` is hidden or the webview does not have focus.
 * A null document is not background.
 */
export function isBackgroundDocument(doc: ForegroundDocument | null | undefined): boolean {
  if (doc == null) return false;
  if (doc.hidden === true || doc.visibilityState === "hidden") return true;
  if (focusOverride !== null) return !focusOverride;
  const hasFocus = doc.hasFocus;
  if (typeof hasFocus !== "function") return false;
  try {
    return hasFocus.call(doc) !== true;
  } catch {
    return true;
  }
}

/** The live document, or false when this process has no DOM. */
export function readBackgroundDocument(): boolean {
  if (typeof document === "undefined") return false;
  return isBackgroundDocument(document);
}

const wrappers = new WeakMap<() => void, Map<string, { target: EventTarget; wrapped: () => void }>>();

/**
 * Subscribe to one foreground event.
 * Focus and blur land on `frame` (the window). Everything else lands on `doc`.
 * A missing frame skips focus and blur rather than throwing.
 */
export function addForegroundListener(
  doc: EventTarget,
  frame: EventTarget | null,
  type: string,
  listener: () => void,
): void {
  const target = type === "focus" || type === "blur" ? frame : doc;
  if (target == null) return;
  let byType = wrappers.get(listener);
  if (!byType) {
    byType = new Map();
    wrappers.set(listener, byType);
  }
  if (byType.has(type)) return;
  const wrapped = () => {
    if (type === "blur") noteForegroundFocus(false);
    else if (type === "focus") noteForegroundFocus(true);
    listener();
  };
  byType.set(type, { target, wrapped });
  target.addEventListener(type, wrapped);
}

/** Remove the subscription added for this listener and event type. */
export function removeForegroundListener(type: string, listener: () => void): void {
  const byType = wrappers.get(listener);
  if (!byType) return;
  const entry = byType.get(type);
  if (!entry) return;
  entry.target.removeEventListener(type, entry.wrapped);
  byType.delete(type);
}

/**
 * Listen for hide, show, focus, and blur. The disposer removes all three.
 * Focus and blur update the override before `onChange` reads it.
 */
export function bindForegroundChanges(
  doc: EventTarget,
  frame: EventTarget | null,
  onChange: () => void,
): () => void {
  addForegroundListener(doc, frame, "visibilitychange", onChange);
  addForegroundListener(doc, frame, "focus", onChange);
  addForegroundListener(doc, frame, "blur", onChange);
  return () => {
    removeForegroundListener("visibilitychange", onChange);
    removeForegroundListener("focus", onChange);
    removeForegroundListener("blur", onChange);
  };
}
