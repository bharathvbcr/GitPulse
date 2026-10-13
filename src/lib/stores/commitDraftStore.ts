/**
 * The commit message being typed, per repository tab.
 *
 * It used to be a field of the tab's session in repoStore, so every keystroke
 * replaced the session and republished the whole repository state: each
 * subscriber re-ran, the active tab's strip row was rebuilt (its dirty and
 * conflict counts walk every status entry), and the background-scope keys
 * were re-serialized — all for a string only the composer reads. Here a
 * keystroke notifies the active draft's subscribers and nothing else.
 *
 * Keyed by tab id. repoStore owns tab lifecycle, so it says which tab is
 * active, hands a draft over when a tab is replaced by its canonical
 * successor, and names the tabs still open so a closed tab's draft goes
 * with it. Drafts are memory only, as they were on the session.
 */
import { writable, type Readable } from "svelte/store";

export interface CommitDraftStore extends Readable<string> {
  /** Replaces the active tab's draft. Does nothing when no tab is active. */
  set(text: string): void;
  /** The draft of any tab, active or not. */
  read(tabId: string): string;
  /** Points the store at another tab; null when none is open. */
  setActiveTab(tabId: string | null): void;
  /**
   * Hands `fromId`'s draft to `toId`, which is taking its place. The draft
   * leaves `fromId` either way; a draft `toId` already holds is kept.
   */
  move(fromId: string, toId: string): void;
  /** Forgets the drafts of every tab not in `openIds`. */
  retain(openIds: Iterable<string>): void;
}

export function createCommitDraftStore(): CommitDraftStore {
  const drafts = new Map<string, string>();
  let activeId: string | null = null;
  // A writable of a string notifies only when the string changes, so moving
  // between two tabs with the same (often empty) draft costs nothing.
  const active = writable("");
  const sync = () => active.set(activeId === null ? "" : drafts.get(activeId) ?? "");

  return {
    subscribe: active.subscribe,
    set(text) {
      if (activeId === null) return;
      if (text === "") drafts.delete(activeId);
      else drafts.set(activeId, text);
      sync();
    },
    read: (tabId) => drafts.get(tabId) ?? "",
    setActiveTab(tabId) {
      if (tabId === activeId) return;
      activeId = tabId;
      sync();
    },
    move(fromId, toId) {
      if (fromId === toId) return;
      const carried = drafts.get(fromId);
      drafts.delete(fromId);
      if (carried !== undefined && !drafts.has(toId)) drafts.set(toId, carried);
      sync();
    },
    retain(openIds) {
      const open = new Set(openIds);
      for (const id of [...drafts.keys()]) if (!open.has(id)) drafts.delete(id);
      sync();
    },
  };
}
