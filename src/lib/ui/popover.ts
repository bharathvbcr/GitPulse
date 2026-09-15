/**
 * One owner for anchored popovers: where a panel opens, and every reason it
 * closes.
 *
 * Ten surfaces used to hand-roll this — a `clampMenuPosition` call, a
 * `shouldDismissOverlay` call, and an `onMount` block pairing
 * `addEventListener` with `removeEventListener` for some subset of
 * pointerdown / click / contextmenu / scroll / resize / keydown. They had
 * drifted into ten different answers to the same questions, and the
 * differences were invisible at the call site: whether the pointer listener
 * ran on the capture phase, whether an outside *scroll* dismissed, whether a
 * resize closed or repositioned, whether the menu re-measured after its
 * content changed size. Three of those differences were defects rather than
 * decisions (see below).
 *
 * The action attaches to the popover node itself, which is the single most
 * useful thing about it: **listener lifetime becomes popover lifetime**. The
 * old blocks lived for the whole component and opened with `if (!open)
 * return` guards, so a component could — and did — hold four window listeners
 * while showing nothing. A node inside `{#if open}` cannot.
 *
 * What stays with the call site, because it genuinely differs there:
 *
 * * **The dismiss selector.** What counts as "inside" is a per-surface fact;
 *   most sites must count their own trigger as inside, or the pointerdown
 *   that opens the panel closes it a beat before the click reopens it.
 * * **Focus restoration.** Which element regains focus — and whether any
 *   does — belongs to the surface. `restoreFocusTo` is offered for the
 *   deferred spelling most sites want; nothing forces it.
 * * **Keyboard ownership.** A menu with roving focus (arrows, Home/End,
 *   type-ahead) already owns its keydown handler, and Escape is one branch of
 *   it. Those sites pass `escape: "none"` rather than have two owners fight
 *   over one key.
 */

import { clampMenuPosition } from "../branches/menuPosition";
import { shouldDismissOverlay } from "./dismiss";

/** Why the popover was asked to close. Call sites that treat every reason
 *  alike can ignore it; those that restore focus only for Escape cannot. */
export type DismissReason = "pointer" | "contextmenu" | "escape" | "scroll" | "resize";

export type PopoverAnchor =
  /** A point in viewport coordinates — a right-click, or a keyboard menu key. */
  | { kind: "point"; x: number; y: number }
  /**
   * A trigger element. The panel opens flush with its left edge, `gap` pixels
   * below it — or above it, for a trigger in the status bar, where "below"
   * is off the bottom of the window.
   */
  | {
      kind: "element";
      element: HTMLElement | null | undefined;
      gap?: number;
      place?: "below" | "above";
    };

export interface PopoverDismissOptions {
  /**
   * Selector for everything that counts as INSIDE, trigger included. Omitted,
   * the node's own subtree is inside and nothing else is.
   */
  inside?: string;
  /**
   * Which outside pointer event dismisses, and — not incidentally — on which
   * phase. `"pointerdown"` listens on capture, so dismissal cannot be
   * suppressed by a `stopPropagation` further down. `"click"` listens on
   * bubble, which is what the two context menus that stop propagation on
   * their own container rely on to keep themselves open while a row runs.
   * The phase is not separately configurable because no site wants the other
   * combination, and the ones that rely on bubbling would silently lose a
   * behaviour if it flipped.
   */
  pointer?: "pointerdown" | "click" | "none";
  /** A right-click outside dismisses, rather than stacking a second menu. */
  contextmenu?: boolean;
  /**
   * An outside scroll dismisses: the anchor has moved out from under the
   * panel. Registered on the capture phase because scroll does not bubble —
   * a bubble listener on `window` never sees an inner scroller move at all.
   */
  scroll?: boolean;
  /** A resize dismisses: the clamped position now points at nothing useful. */
  resize?: boolean;
  /**
   * Escape. `"capture"` also stops propagation, so one Escape dismisses one
   * thing when the panel sits inside another surface that closes on Escape.
   * `"none"` leaves the key to a handler the popover already has.
   */
  escape?: "capture" | "bubble" | "none";
}

export interface PopoverOptions {
  /**
   * Where to put the panel. Omitted, the action positions nothing and does
   * dismissal only — which is the whole contract for a panel CSS already
   * places (an `absolute` dropdown under its trigger, a fixed corner panel).
   */
  anchor?: PopoverAnchor;
  /**
   * Fallback size, used only while the node reports no layout box. The node
   * is in the document by the time the action runs, so a measured size is
   * almost always available and always wins; this is what keeps a panel from
   * clamping against a zero-sized rect in a renderer-less environment.
   */
  estimate?: { width: number; height: number };
  /** Keep-out margin from every viewport edge. */
  inset?: number;
  dismiss?: PopoverDismissOptions;
  onDismiss?: (reason: DismissReason) => void;
  /**
   * Re-measure and re-place when this value changes. Position already
   * follows the anchor; this is for content that changes the panel's *size*
   * without moving it — a submenu page, a filtered list, a menu whose
   * conditional items changed under a background refresh.
   */
  revision?: unknown;
}

type ListenerKind = "pointer" | "contextmenu" | "scroll" | "resize" | "escape";

/**
 * Duck-typed, like `dismiss.ts`'s `closest` probe and for the same reason:
 * this module is imported and unit-tested in plain Node, where `Node` is not
 * a global and a bare `target instanceof Node` is a ReferenceError rather
 * than a false.
 */
function isNodeLike(value: unknown): value is Node {
  return typeof value === "object" && value !== null && "nodeType" in value;
}

/** One window listener: event name, handler, capture phase. */
type Registration = [type: string, handler: EventListener, capture: boolean];

/**
 * Hand focus back to the element that opened a popover, on the next task.
 *
 * Deferred because the popover node is still in the document while the
 * handler runs; focusing synchronously can land on a node that is about to
 * be removed, which drops focus to `<body>`. Re-checked on the way in *and*
 * again when the timer fires: a background refresh can delete the row whose
 * kebab opened the menu in between.
 */
export function restoreFocusTo(element: HTMLElement | null | undefined): void {
  if (typeof window === "undefined" || !element?.isConnected) return;
  window.setTimeout(() => {
    if (element.isConnected) element.focus();
  }, 0);
}

/**
 * Svelte action for the popover node.
 *
 * Positions it against its anchor (when it has one), registers exactly the
 * dismissal listeners it asks for, and tears every one of them down when the
 * node leaves — which, for a node inside `{#if open}`, is the moment it
 * closes.
 */
export function popover(node: HTMLElement, options: PopoverOptions = {}) {
  if (typeof window === "undefined") {
    // SSR / bare Node: nothing to measure and no window to listen on.
    return { update(_next: PopoverOptions) {}, destroy() {} };
  }

  let config = options;
  const active = new Map<ListenerKind, () => void>();

  const fire = (reason: DismissReason) => config.onDismiss?.(reason);

  /**
   * Inside by the site's own selector when it named one — that is how a
   * trigger outside the panel's subtree still counts as inside — and by
   * plain containment otherwise.
   */
  const isInside = (target: unknown): boolean => {
    const inside = config.dismiss?.inside;
    if (inside) return !shouldDismissOverlay(target, inside);
    return isNodeLike(target) && node.contains(target);
  };

  const onPointer = (event: Event) => {
    if (!isInside(event.target)) fire("pointer");
  };

  const onContext = (event: Event) => {
    if (!isInside(event.target)) fire("contextmenu");
  };

  /**
   * Judged by containment alone, never by the `inside` selector: a scroll
   * inside the panel is the reader reading the list, not leaving it. Sites
   * whose selector also covers the trigger would otherwise treat scrolling
   * the page-behind as inside and never dismiss.
   */
  const onScroll = (event: Event) => {
    if (isNodeLike(event.target) && node.contains(event.target)) return;
    fire("scroll");
  };

  const onResize = () => fire("resize");

  const onKey = (event: KeyboardEvent) => {
    if (event.key !== "Escape") return;
    event.preventDefault();
    if (config.dismiss?.escape === "capture") event.stopPropagation();
    fire("escape");
  };

  const place = () => {
    const anchor = config.anchor;
    if (!anchor) return;
    const inset = config.inset ?? 0;
    // Measured wins; the estimate only covers a node with no layout box.
    const width = node.offsetWidth || config.estimate?.width || 0;
    const height = node.offsetHeight || config.estimate?.height || 0;

    let x: number;
    let y: number;
    if (anchor.kind === "point") {
      x = anchor.x;
      y = anchor.y;
    } else {
      if (!anchor.element) return;
      const rect = anchor.element.getBoundingClientRect();
      const gap = anchor.gap ?? 0;
      x = rect.left;
      // "above" is expressed as a top, not a bottom, so one clamp covers both
      // placements. With a measured height the panel still grows upward as
      // its content grows — and, unlike a raw `bottom:`, it cannot grow off
      // the top of the window.
      y = anchor.place === "above" ? rect.top - gap - height : rect.bottom + gap;
    }

    const clamped = clampMenuPosition(
      x,
      y,
      width,
      height,
      window.innerWidth - inset,
      window.innerHeight - inset,
    );
    node.style.left = `${Math.max(inset, clamped.left)}px`;
    node.style.top = `${Math.max(inset, clamped.top)}px`;
  };

  const wanted = (): Partial<Record<ListenerKind, Registration>> => {
    const dismiss = config.dismiss ?? {};
    const pointer = dismiss.pointer ?? "pointerdown";
    const escape = dismiss.escape ?? "none";
    const registrations: Partial<Record<ListenerKind, Registration>> = {};
    if (pointer !== "none") {
      registrations.pointer = [pointer, onPointer, pointer === "pointerdown"];
    }
    if (dismiss.contextmenu) registrations.contextmenu = ["contextmenu", onContext, false];
    if (dismiss.scroll) registrations.scroll = ["scroll", onScroll, true];
    if (dismiss.resize) registrations.resize = ["resize", onResize, false];
    if (escape !== "none") {
      registrations.escape = ["keydown", onKey as EventListener, escape === "capture"];
    }
    return registrations;
  };

  /**
   * Bring the registered set in line with the options. Re-registering a kind
   * whose phase or event name changed is what keeps `update` honest — a
   * listener left on the wrong phase is the failure this module exists to
   * make impossible.
   */
  let signature = "";
  const syncListeners = () => {
    const registrations = wanted();
    const next = JSON.stringify(
      (Object.keys(registrations) as ListenerKind[]).map((kind) => {
        const [type, , capture] = registrations[kind]!;
        return [kind, type, capture];
      }),
    );
    if (next === signature) return;
    signature = next;
    for (const remove of active.values()) remove();
    active.clear();
    for (const [kind, registration] of Object.entries(registrations)) {
      const [type, handler, capture] = registration;
      window.addEventListener(type, handler, capture);
      active.set(kind as ListenerKind, () =>
        window.removeEventListener(type, handler, capture),
      );
    }
  };

  syncListeners();
  place();

  return {
    update(next: PopoverOptions) {
      config = next;
      syncListeners();
      place();
    },
    destroy() {
      for (const remove of active.values()) remove();
      active.clear();
    },
  };
}
