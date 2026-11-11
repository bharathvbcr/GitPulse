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
  snapshot: (id: string) => invoke<string>("cmd_browser_snapshot", { id }),
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

/**
 * Whether `raw` is an http(s) address on this machine (`localhost`,
 * `127.0.0.0/8`, `[::1]`) — the addresses a browser session opens. Only a
 * routing choice: the backend judges the address again before loading it.
 */
export function isLoopbackUrl(raw: string): boolean {
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return false;
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return false;
  const host = url.hostname.toLowerCase();
  return host === "localhost" || host === "[::1]" || /^127(\.\d{1,3}){3}$/.test(host);
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

/** Hit-test points along one axis: both ends (pulled in by `inset`) and evenly between. */
function axisPoints(start: number, length: number, inset: number, spacing: number, max: number): number[] {
  const from = start + inset;
  const to = start + length - inset;
  if (to <= from) return [];
  const steps = Math.min(max - 1, Math.max(1, Math.ceil((to - from) / spacing)));
  return Array.from({ length: steps + 1 }, (_, i) => from + ((to - from) * i) / steps);
}

/**
 * Points (CSS pixels) to hit-test for something drawn over `rect`: a grid at
 * most `spacing` apart, or a ninth of the span once that would take more than
 * `maxPerAxis` points a side, edges pulled in by `inset` so a neighbour's border
 * is not mistaken for an overlay. An overlay at least one grid step wide and
 * tall is always hit; smaller ones are found by `overlapsRect` when they are
 * portaled to the body, which every menu, dialog and toast is.
 */
export function samplePoints(rect: CssRect, inset = 3, spacing = 64, maxPerAxis = 10): { x: number; y: number }[] {
  const xs = axisPoints(rect.left, rect.width, inset, spacing, maxPerAxis);
  const ys = axisPoints(rect.top, rect.height, inset, spacing, maxPerAxis);
  return xs.flatMap((x) => ys.map((y) => ({ x, y })));
}

/**
 * Whether `other` overlaps `rect` by any area. A box under `minSide` on
 * either side (a screen-reader live region, an empty wrapper) does not count.
 */
export function overlapsRect(rect: CssRect, other: CssRect, minSide = 4): boolean {
  if (other.width < minSide || other.height < minSide) return false;
  return (
    rect.left < other.left + other.width &&
    other.left < rect.left + rect.width &&
    rect.top < other.top + other.height &&
    other.top < rect.top + rect.height
  );
}

/**
 * Whether anything of GitPulse's own is drawn over `host` at any of `points`:
 * the topmost element there is neither `host` nor inside it. A native view
 * sits above every element of this page, so a menu, dialog or toast that
 * opens over the session would otherwise be hidden behind it.
 *
 * Hit-testing skips elements with `pointer-events: none`; portaled ones are
 * caught by their box instead (`overlapsRect`, `BrowserSession.svelte`).
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
