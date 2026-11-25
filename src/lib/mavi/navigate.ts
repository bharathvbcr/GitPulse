/**
 * Every way into Mavi from elsewhere in the app goes through here, so a
 * toast, the palette, the dock and the Live pulse column all land the same
 * way: the repository surface, the Mavi view, and the thread (or the new
 * thread form, prefilled).
 */
import { interfaceStore } from "../stores/interfaceStore";
import { repoStore } from "../stores/repoStore";
import { maviStore } from "./store.svelte";

export function showMaviThread(key: string): Promise<void> {
  interfaceStore.setGlobalSurface("repository");
  repoStore.setActiveTab("mavi");
  return maviStore.open(key);
}

/** Opens the new-thread form, with `message` as the start of the first message. */
export function composeMaviThread(message = "", options: { newWorktree?: boolean } = {}): void {
  interfaceStore.setGlobalSurface("repository");
  repoStore.setActiveTab("mavi");
  maviStore.requestCompose(message, options);
}
