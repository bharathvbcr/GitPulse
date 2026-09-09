import { derived, writable } from "svelte/store";
import { repoStore } from "../stores/repoStore";
import { interfaceStore } from "../stores/interfaceStore";
import { themeStore } from "../stores/themeStore";
import { updateCheckInFlight } from "../updates/updateCheck";
import { buildMenuState } from "./menuState";

/** Dialog/read activity before a Git command starts; the store owns actual mutations. */
export const menuActivity = writable<Record<string, string[]>>({});
export const nativeMenuState = derived(
  [repoStore, interfaceStore, themeStore.preferenceState, repoStore.mutationActivity, menuActivity, updateCheckInFlight],
  ([repo, prefs, theme, activity, pending, checking]) =>
    buildMenuState(repo, prefs, theme, { ...pending, ...activity }, checking, activity),
);
