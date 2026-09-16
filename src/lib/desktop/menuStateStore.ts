import { derived, writable } from "svelte/store";
import { repoStore } from "../stores/repoStore";
import { interfaceStore } from "../stores/interfaceStore";
import { themeStore } from "../stores/themeStore";
import { updateCheckInFlight } from "../updates/updateCheck";
import { diagnostics } from "../diagnostics/diagnostics";
import { buildMenuState } from "./menuState";
import { sendableMenuState } from "./menuContract";

/** Dialog/read activity before a Git command starts; the store owns actual mutations. */
export const menuActivity = writable<Record<string, string[]>>({});

/**
 * Reported once per distinct fault rather than once per projection.
 *
 * The store republishes on every status tick, so a payload the native side
 * would refuse is not one event — it is one per tick, for as long as the
 * workspace stays in that shape. The shipped build logged five `desktop:menu-state`
 * errors for a single cause, which reads as five faults and named none of them.
 */
const reported = new Set<string>();

export const nativeMenuState = derived(
  [repoStore, interfaceStore, themeStore.preferenceState, repoStore.mutationActivity, menuActivity, updateCheckInFlight],
  ([repo, prefs, theme, activity, pending, checking]) => {
    const built = buildMenuState(repo, prefs, theme, { ...pending, ...activity }, checking, activity);
    // The last thing before the IPC boundary. `buildMenuState` is held to
    // producing only sendable payloads, and this is what keeps a lapse in that
    // from costing the menu bar, the tray and the popover at once: the native
    // side applies nothing at all when it refuses, so a repaired payload is the
    // difference between a missing repository list and an inert application.
    const { state, problem } = sendableMenuState(built);
    if (problem !== null && !reported.has(problem)) {
      reported.add(problem);
      diagnostics.error("desktop:menu-state", `Repaired an unsendable native menu payload: ${problem}`);
    }
    return state;
  },
);
