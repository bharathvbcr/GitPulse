import { get, writable } from "svelte/store";

/**
 * The MANVI view's two panes, and where a deep link into it lands.
 *
 * The view is one tab with two panes and several stacked sections, so
 * "open MANVI" is not an answer to "show me this model" — it drops the reader
 * on the Ops pane with the subject of their click somewhere below the fold on
 * the other pane. Every control that navigates there names a target here
 * instead, and the pane switch, the section anchor, the tooltip copy and the
 * contract test all derive from this one catalog: adding a target means adding
 * an entry and its anchor, and a target without an anchor fails the test
 * rather than silently landing at the top.
 */
export const MANVI_PANE_IDS = ["ops", "harness"] as const;
export type ManviPane = (typeof MANVI_PANE_IDS)[number];

export interface ManviPaneRegistration {
  readonly id: ManviPane;
  /** Segmented-control label, and the middle term of every deep-link tooltip. */
  readonly label: string;
  /** One line under the view heading saying what the pane covers. */
  readonly summary: string;
}

export const MANVI_PANES: Readonly<Record<ManviPane, ManviPaneRegistration>> = {
  ops: {
    id: "ops",
    label: "Ops",
    summary:
      "Guarded repository operations, release readiness, and issue monitoring in one place.",
  },
  harness: {
    id: "harness",
    label: "Harness & AI",
    summary:
      "Harness connection, local model servers, branch naming, and the agent activity journal.",
  },
};

/** Registrations in segmented-control order. */
export const MANVI_PANE_LIST: readonly ManviPaneRegistration[] =
  Object.values(MANVI_PANES);

export const MANVI_FOCUS_IDS = [
  "harness",
  "model",
  "activity",
  "cleanup",
] as const;
export type ManviFocusId = (typeof MANVI_FOCUS_IDS)[number];

export interface ManviFocusTarget {
  readonly id: ManviFocusId;
  /** Pane that owns the section. */
  readonly pane: ManviPane;
  /** The section's heading, so a tooltip can promise exactly where it lands. */
  readonly label: string;
}

export const MANVI_FOCUS_TARGETS: Readonly<
  Record<ManviFocusId, ManviFocusTarget>
> = {
  harness: { id: "harness", pane: "harness", label: "MANVI harness" },
  model: { id: "model", pane: "harness", label: "Local model servers" },
  activity: { id: "activity", pane: "harness", label: "Agent activity" },
  cleanup: { id: "cleanup", pane: "ops", label: "Branch cleanup" },
};

/** Targets in declaration order. */
export const MANVI_FOCUS_LIST: readonly ManviFocusTarget[] =
  Object.values(MANVI_FOCUS_TARGETS);

/**
 * DOM id of a target's section. Derived rather than written twice, so the
 * anchor a link scrolls to and the anchor a section declares cannot drift.
 */
export function manviSectionId(id: ManviFocusId): string {
  return `manvi-section-${id}`;
}

/**
 * The tooltip line a deep-linking control appends. Naming the pane and section
 * is the point: two chips that both said "open the MANVI view" were telling
 * the truth and still left the reader hunting.
 */
export function manviFocusHint(id: ManviFocusId): string {
  const target = MANVI_FOCUS_TARGETS[id];
  return `Opens MANVI → ${MANVI_PANES[target.pane].label} → ${target.label}.`;
}

/**
 * One pending deep link, handed from the control that was clicked to the
 * MANVI view, which mounts lazily and therefore cannot be called directly.
 */
const pendingFocus = writable<ManviFocusId | null>(null);

/** Read-only view for the pane that consumes requests. */
export const manviFocusRequest = { subscribe: pendingFocus.subscribe };

export function requestManviFocus(id: ManviFocusId): void {
  pendingFocus.set(id);
}

/**
 * Takes the pending request and clears it. Clearing on read is what stops a
 * request from firing again the next time the view is opened for an unrelated
 * reason — a scroll jump nobody asked for reads as a bug, not a shortcut.
 */
export function takeManviFocus(): ManviFocusId | null {
  const requested = get(pendingFocus);
  if (requested !== null) pendingFocus.set(null);
  return requested;
}
