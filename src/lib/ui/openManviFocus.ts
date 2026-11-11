import { repoStore } from "../stores/repoStore";
import { MANVI_FOCUS_TARGETS, requestManviFocus, type ManviFocusId } from "./manviFocus";

/**
 * Navigates to a Manvi card and asks its surface to scroll to it.
 *
 * Callers used to pair `requestManviFocus` with their own `setActiveTab`, and
 * each pair could name a different destination from the catalog's — one did
 * not navigate at all and left the request to fire on some later visit. The
 * destination now comes from the catalog alone.
 *
 * Kept apart from `manviFocus.ts` so the catalog stays free of the repository
 * store and its contract test stays a pure import.
 */
export function openManviFocus(id: ManviFocusId): void {
  requestManviFocus(id);
  const destination = MANVI_FOCUS_TARGETS[id].destination;
  if (destination.kind === "view") {
    repoStore.setActiveTab(destination.tab, destination.section);
  } else {
    window.dispatchEvent(new CustomEvent("gitpulse:settings"));
  }
}
