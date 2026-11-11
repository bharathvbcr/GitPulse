import { get, writable } from "svelte/store";

/**
 * Where a Manvi deep link lands.
 *
 * Manvi used to be one Work section with two panes. Its cards now live with
 * the subject they act on: configuration in Settings → Agents, branch and tag
 * cleanup and the policy journal as collapsed disclosures on Work → Overview,
 * issues, releases and outgoing commits on Work → Remote. A control that says
 * "open Manvi" therefore has to name a card, not a page — the reader's subject
 * is one card among several wherever it landed.
 *
 * Every control that navigates names a target here, and the destination, the
 * anchor, the tooltip copy and the contract test all derive from this one
 * catalog: adding a target means adding an entry and its anchor, and a target
 * without an anchor fails the test rather than silently landing at the top.
 */
export const MANVI_FOCUS_IDS = ["harness", "model", "activity", "cleanup"] as const;
export type ManviFocusId = (typeof MANVI_FOCUS_IDS)[number];

/**
 * The surface that renders a target's card. Settings is a dialog, not a view,
 * so the two kinds are navigated differently and consumed by different
 * components; `takeManviFocusFor` is how each consumer takes only its own.
 */
export type ManviFocusDestination =
  | { readonly kind: "settings"; readonly section: "agents"; readonly label: "Settings → Agents" }
  | {
      readonly kind: "view";
      readonly tab: "work";
      readonly section: "overview";
      readonly label: "Work → Overview";
    };

export type ManviFocusKind = ManviFocusDestination["kind"];

const SETTINGS_AGENTS: ManviFocusDestination = {
  kind: "settings",
  section: "agents",
  label: "Settings → Agents",
};
const WORK_OVERVIEW: ManviFocusDestination = {
  kind: "view",
  tab: "work",
  section: "overview",
  label: "Work → Overview",
};

export interface ManviFocusTarget {
  readonly id: ManviFocusId;
  readonly destination: ManviFocusDestination;
  /** The card's heading, so a tooltip can promise exactly where it lands. */
  readonly label: string;
}

export const MANVI_FOCUS_TARGETS: Readonly<Record<ManviFocusId, ManviFocusTarget>> = {
  harness: { id: "harness", destination: SETTINGS_AGENTS, label: "Manvi policy" },
  model: { id: "model", destination: SETTINGS_AGENTS, label: "Local model servers" },
  activity: { id: "activity", destination: WORK_OVERVIEW, label: "Policy activity" },
  cleanup: { id: "cleanup", destination: WORK_OVERVIEW, label: "Branch and tag cleanup" },
};

/** Targets in declaration order. */
export const MANVI_FOCUS_LIST: readonly ManviFocusTarget[] = Object.values(MANVI_FOCUS_TARGETS);

/**
 * DOM id of a target's card. Derived rather than written twice, so the anchor
 * a link scrolls to and the anchor a card declares cannot drift.
 */
export function manviSectionId(id: ManviFocusId): string {
  return `manvi-section-${id}`;
}

/**
 * The tooltip line a deep-linking control appends. Naming the surface and the
 * card is the point: chips that said "open the Manvi view" were telling the
 * truth and still left the reader hunting.
 */
export function manviFocusHint(id: ManviFocusId): string {
  const target = MANVI_FOCUS_TARGETS[id];
  return `Opens ${target.destination.label} → ${target.label}.`;
}

/**
 * One pending deep link, handed from the control that was clicked to the
 * surface that renders the card, which mounts lazily and therefore cannot be
 * called directly.
 */
const pendingFocus = writable<ManviFocusId | null>(null);

/** Read-only view for the surfaces that consume requests. */
export const manviFocusRequest = { subscribe: pendingFocus.subscribe };

export function requestManviFocus(id: ManviFocusId): void {
  pendingFocus.set(id);
}

/**
 * Takes the pending request and clears it. Clearing on read is what stops a
 * request from firing again the next time the surface is opened for an
 * unrelated reason — a scroll jump nobody asked for reads as a bug, not a
 * shortcut.
 */
export function takeManviFocus(): ManviFocusId | null {
  const requested = get(pendingFocus);
  if (requested !== null) pendingFocus.set(null);
  return requested;
}

/**
 * Takes the pending request only when its card lives on `kind`'s surface.
 *
 * Settings and Overview can both be mounted at once (the dialog opens over the
 * view), and each subscribes to the same channel. A consumer that took every
 * request would swallow the other's: Overview clearing a "model" request
 * before Settings had mounted would leave Settings opening on Appearance.
 */
export function takeManviFocusFor(kind: ManviFocusKind): ManviFocusId | null {
  const requested = get(pendingFocus);
  if (requested === null || MANVI_FOCUS_TARGETS[requested].destination.kind !== kind) return null;
  pendingFocus.set(null);
  return requested;
}
