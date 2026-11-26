import { get, writable, type Writable } from "svelte/store";
import { repoStore } from "../stores/repoStore";

/**
 * DOM id of the conflict editor's section on Work → Overview.
 *
 * The editor leads Overview whenever a merge needs a person. It is named once
 * here because the doors that bring it on screen live in other components.
 */
export const WORK_CONFLICTS_ID = "work-conflicts";

/**
 * One pending "show me the conflicts", handed from whichever door was used to
 * the Overview pane, which mounts lazily and so cannot be called directly.
 *
 * Switching to Overview is not enough on its own: a reader already on
 * Overview, scrolled into the rows, would see nothing happen, and from another
 * view the editor does not exist yet when the switch returns.
 */
const pending = writable(false);
const pendingReveal = writable(false);
const panelOpen = writable(false);
const draftProbe = writable(0);

/** Read-only view for the Overview pane, which consumes requests. */
export const conflictsFocusRequest = { subscribe: pending.subscribe };

/** Read-only view of whether the Overview conflict editor is currently open. */
export const conflictsPanelOpen = { subscribe: panelOpen.subscribe };

/** Monotonic probe counter bumped whenever conflict drafts may have changed. */
export const conflictDraftProbe = { subscribe: draftProbe.subscribe };

/** Records whether `WorkOverviewPane` currently renders `#work-conflicts`. */
export function setConflictsPanelOpen(open: boolean): void {
  panelOpen.set(open);
}

/** Notifies Overview draft counters to re-read `conflictSessions.summaries`. */
export function bumpConflictDraftProbe(): void {
  draftProbe.update((n) => n + 1);
}

/** Asks Overview to bring the conflict editor on screen once it can. */
export function requestConflictsFocus(options?: { reveal?: boolean }): void {
  if (options?.reveal) pendingReveal.set(true);
  else if (!get(pending)) pendingReveal.set(false);
  pending.set(true);
}

function takeFlag(store: Writable<boolean>): boolean {
  const requested = get(store);
  if (requested) store.set(false);
  return requested;
}

/**
 * Takes the pending reveal request and clears it. Explicit doors (`openConflicts`)
 * ask to reveal the editor even when clean; passive tab switches do not.
 */
export const takeConflictsReveal = (): boolean => takeFlag(pendingReveal);

/**
 * Takes the pending request and clears it, so a later visit to Overview for
 * an unrelated reason does not jump to the editor.
 */
export const takeConflictsFocus = (): boolean => takeFlag(pending);

/** The one door to the conflict editor: Overview, scrolled to the editor. */
export function openConflicts(): void {
  requestConflictsFocus({ reveal: true });
  repoStore.setActiveTab("work", "overview");
}
