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

type ListenerHost = Pick<Document, "addEventListener" | "removeEventListener">;

/**
 * The surface the user last interacted with (a pointer press or focus landing
 * in it), null until they have. A readable listens only while something is
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
    host.addEventListener("pointerdown", record, true);
    host.addEventListener("focusin", record, true);
    return () => {
      host.removeEventListener("pointerdown", record, true);
      host.removeEventListener("focusin", record, true);
    };
  });
}

export const lastSurface = createLastSurface(typeof document === "undefined" ? undefined : document);
