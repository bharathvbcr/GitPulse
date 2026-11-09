/**
 * Browser sessions: a terminal-dock tab that shows a local dev server's page
 * in a native view laid over the tab (`src-tauri/src/browser`).
 *
 * The page is not in this document. It is a separate native webview with no
 * route to GitPulse's commands, and this module is the whole of what the
 * frontend says to it: the `cmd_browser_*` commands, the `browser-pane` page
 * events, and the pure rules for where the view goes and when it shows
 * (`BrowserSession.svelte` wires them to a tab).
 */

import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "../ipc/invoke";

/** Where a pane sits, in window points from the content area's top-left. */
export interface PaneBounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** What a session's page did, as `src-tauri/src/browser` reports it. */
export type PageEvent =
  | { id: string; kind: "loading"; url: string }
  | { id: string; kind: "loaded"; url: string }
  | { id: string; kind: "title"; title: string }
  | { id: string; kind: "refused"; url: string };

export type HistoryStep = "back" | "forward" | "reload";

export const browserIpc = {
  open: (id: string, url: string, bounds: PaneBounds) => invoke<void>("cmd_browser_open", { id, url, bounds }),
  setBounds: (id: string, bounds: PaneBounds) => invoke<void>("cmd_browser_set_bounds", { id, bounds }),
  setVisible: (id: string, visible: boolean) => invoke<void>("cmd_browser_set_visible", { id, visible }),
  navigate: (id: string, url: string) => invoke<void>("cmd_browser_navigate", { id, url }),
  go: (id: string, to: HistoryStep) => invoke<void>("cmd_browser_go", { id, to }),
  close: (id: string) => invoke<void>("cmd_browser_close", { id }),
};

/** The longest page text taken from an event; the backend caps it too. */
const MAX_EVENT_TEXT = 2048;

function text(value: unknown): string | null {
  return typeof value === "string" ? value.slice(0, MAX_EVENT_TEXT) : null;
}

/** A `browser-pane` payload, or null for anything that is not one. */
export function parsePageEvent(payload: unknown): PageEvent | null {
  if (!payload || typeof payload !== "object") return null;
  const raw = payload as Record<string, unknown>;
  const id = text(raw.id);
  if (!id) return null;
  switch (raw.kind) {
    case "loading":
    case "loaded":
    case "refused": {
      const url = text(raw.url);
      return url === null ? null : { id, kind: raw.kind, url };
    }
    case "title": {
      const title = text(raw.title);
      return title === null ? null : { id, kind: "title", title };
    }
    default:
      return null;
  }
}

const handlers = new Map<string, (event: PageEvent) => void>();
let listening: Promise<UnlistenFn> | null = null;

/**
 * Calls `handler` with session `id`'s page events until the returned function
 * is called. One app-wide listener serves every session.
 */
export function onPageEvents(id: string, handler: (event: PageEvent) => void): () => void {
  handlers.set(id, handler);
  listening ??= listen("browser-pane", (event) => {
    const parsed = parsePageEvent(event.payload);
    if (parsed) handlers.get(parsed.id)?.(parsed);
  }).catch((error: unknown) => {
    listening = null;
    throw error;
  });
  return () => {
    if (handlers.get(id) === handler) handlers.delete(id);
  };
}

/** What the address bar offers before anything has been typed. */
export const DEFAULT_ADDRESS = "http://localhost:3000/";

/**
 * What was typed, as a URL to load: a bare port or `host:port` is taken as
 * `http://` on that host (`3000`, `localhost:5173/app`). Anything else is
 * passed through for the backend to judge; only loopback is loaded.
 */
export function normalizeAddress(input: string): string {
  const typed = input.trim();
  if (/^\d{1,5}(\/.*)?$/.test(typed)) return `http://localhost:${typed}`;
  if (/^(localhost|127(\.\d{1,3}){3}|\[::1\])(:\d{1,5})?(\/.*)?$/i.test(typed)) return `http://${typed}`;
  return typed;
}

const LAST_ADDRESS_KEY = "gitpulse.browser.lastAddress";

/** The last address loaded in any session, or the default. */
export function lastAddress(): string {
  try {
    const stored = globalThis.localStorage?.getItem(LAST_ADDRESS_KEY);
    return stored && stored.length <= MAX_EVENT_TEXT ? stored : DEFAULT_ADDRESS;
  } catch {
    return DEFAULT_ADDRESS;
  }
}

export function rememberAddress(url: string): void {
  try {
    if (url.length <= MAX_EVENT_TEXT) globalThis.localStorage?.setItem(LAST_ADDRESS_KEY, url);
  } catch {
    /* A convenience only: a session works the same without it. */
  }
}

/** A DOM rectangle in CSS pixels, as `getBoundingClientRect` gives it. */
export interface CssRect {
  left: number;
  top: number;
  width: number;
  height: number;
}

/**
 * Window points for a rectangle measured in CSS pixels. The page's own zoom
 * (`uiScale.pageZoom`) scales CSS pixels on screen; the native view is placed
 * in unscaled window points, so it is multiplied back in.
 */
export function toWindowPoints(rect: CssRect, zoom: number): PaneBounds {
  const z = Number.isFinite(zoom) && zoom > 0 ? zoom : 1;
  return {
    x: rect.left * z,
    y: rect.top * z,
    width: Math.max(0, rect.width * z),
    height: Math.max(0, rect.height * z),
  };
}

/** Whether two placements differ by less than half a point everywhere. */
export function sameBounds(a: PaneBounds | null, b: PaneBounds | null): boolean {
  if (!a || !b) return a === b;
  return (
    Math.abs(a.x - b.x) < 0.5 &&
    Math.abs(a.y - b.y) < 0.5 &&
    Math.abs(a.width - b.width) < 0.5 &&
    Math.abs(a.height - b.height) < 0.5
  );
}

/**
 * Points (CSS pixels) to hit-test for something drawn over `rect`: the
 * centre, the four corners and the four edge midpoints, each pulled in by
 * `inset` so a neighbour's border is not mistaken for an overlay.
 */
export function samplePoints(rect: CssRect, inset = 3): { x: number; y: number }[] {
  if (rect.width <= inset * 2 || rect.height <= inset * 2) return [];
  const xs = [rect.left + inset, rect.left + rect.width / 2, rect.left + rect.width - inset];
  const ys = [rect.top + inset, rect.top + rect.height / 2, rect.top + rect.height - inset];
  return xs.flatMap((x) => ys.map((y) => ({ x, y })));
}

/**
 * Whether anything of GitPulse's own is drawn over `host` at any of `points`:
 * the topmost element there is neither `host` nor inside it. A native view
 * sits above every element of this page, so a menu, dialog or toast that
 * opens over the session would otherwise be hidden behind it.
 *
 * Hit-testing skips elements with `pointer-events: none`, so a purely
 * decorative overlay (a tooltip) is not seen; see `BrowserSession.svelte`.
 */
export function isOccluded(
  host: Element,
  points: readonly { x: number; y: number }[],
  topAt: (x: number, y: number) => Element | null,
): boolean {
  return points.some(({ x, y }) => {
    const top = topAt(x, y);
    return top !== null && top !== host && !host.contains(top);
  });
}

/** Whether a session's native view should be on screen. */
export function paneShown(state: {
  opened: boolean;
  onscreen: boolean;
  occluded: boolean;
  bounds: PaneBounds | null;
}): boolean {
  return (
    state.opened &&
    state.onscreen &&
    !state.occluded &&
    state.bounds !== null &&
    state.bounds.width >= 1 &&
    state.bounds.height >= 1
  );
}
