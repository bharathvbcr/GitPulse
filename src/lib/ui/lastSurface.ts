import { readable, type Readable } from "svelte/store";
import { VIEW_PANE_ID } from "../views/viewRegistry";
import { eventTargetElement } from "./dismiss";

/**
 * The two surfaces a person works in side by side: the terminal dock, and the
 * page (the view bar and the view above the dock).
 */
export type Surface = "terminal" | "page";

const DOCK = "[data-terminal-dock]";
const PAGE_ROOTS = ["[data-viewbar]", `#${VIEW_PANE_ID}`];

/**
 * Which surface `target` belongs to, or null for neither. The sidebar, a menu
 * portaled to <body> and everything else are null, so interacting with them
 * never changes which surface was last used: a click on the sidebar's file row
 * is a request about the last surface, not a move to a third one.
 *
 * The dock is asked first: it sits inside the view pane's element, so the
 * page test alone would claim it.
 */
export function classifySurface(target: unknown): Surface | null {
  const element = eventTargetElement(target);
  if (!element) return null;
  if (element.closest(DOCK)) return "terminal";
  if (PAGE_ROOTS.some((selector) => element.closest(selector))) return "page";
  return null;
}

/** Events only a person produces; `focusin` is deliberately absent (see `createLastSurface`). */
const USER_EVENTS = ["pointerdown", "keyup"] as const;

type ListenerHost =Pick<Document, "addEventListener" | "removeEventListener">;

/**
 * The surface the user last interacted with, null until they have. Only what a
 * person does counts: a pointer press, or a key release. A key release lands on
 * whatever holds focus after the key acted, so Tab into the page, a shortcut
 * that moves focus, and typing in the terminal all count. Focus alone does not:
 * code moves focus too (the dock focuses its terminal when it mounts, so a dock
 * restored open at launch would otherwise read as "the user is in the
 * terminal" before they have touched anything). A readable listens only while something is
 * subscribed, so a consumer that mounts late (the lazily loaded sidebar
 * explorer) must not be the only subscriber: the terminal panel holds one for
 * as long as the dock lives, and this keeps the last value across a stop.
 */
export function createLastSurface(host: ListenerHost | undefined): Readable<Surface | null> {
  return readable<Surface | null>(null, (set) => {
    if (!host) return undefined;
    const record = (event: Event) => {
      const surface = classifySurface(event.target);
      if (surface) set(surface);
    };
    for (const type of USER_EVENTS) host.addEventListener(type, record, true);
    return () => {
      for (const type of USER_EVENTS) host.removeEventListener(type, record, true);
    };
  });
}

export const lastSurface = createLastSurface(typeof document === "undefined" ? undefined : document);
