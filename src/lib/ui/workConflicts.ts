import { get, writable } from "svelte/store";
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

/** Read-only view for the Overview pane, which consumes requests. */
export const conflictsFocusRequest = { subscribe: pending.subscribe };

/** Asks Overview to bring the conflict editor on screen once it can. */
export function requestConflictsFocus(): void {
  pending.set(true);
}

/**
 * Takes the pending request and clears it, so a later visit to Overview for
 * an unrelated reason does not jump to the editor.
 */
export function takeConflictsFocus(): boolean {
  const requested = get(pending);
  if (requested) pending.set(false);
  return requested;
}

/** The one door to the conflict editor: Overview, scrolled to the editor. */
export function openConflicts(): void {
  requestConflictsFocus();
  repoStore.setActiveTab("work", "overview");
}
