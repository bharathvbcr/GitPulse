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
 * with it. When given a `storage` target, drafts persist across WebView
 * hibernation and reloads via a bounded deferred write flushed on page hide.
 */
import { writable, type Readable } from "svelte/store";
import { createDeferredWrite, flushOnPageHide } from "../async/deferredWrite";

export const STORAGE_KEY_COMMIT_DRAFTS = "gitpulse.commit-drafts.v1";
const MAX_PERSISTED_DRAFTS = 64;
const MAX_PERSISTED_BYTES = 64 * 1024;
const DEFAULT_FLUSH_DELAY_MS = 400;

export interface CommitDraftStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem?(key: string): void;
}

export interface CommitDraftStoreOptions {
  storage?: CommitDraftStorage | null;
  flushDelayMs?: number;
}

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
  /** Flushes any pending persisted write immediately (no-op when unpersisted). */
  flush(): void;
  /** Whether any tab currently holds a non-empty commit message draft. */
  hasDrafts(): boolean;
}

function loadPersistedDrafts(storage: CommitDraftStorage): Map<string, string> {
  const out = new Map<string, string>();
  try {
    const raw = storage.getItem(STORAGE_KEY_COMMIT_DRAFTS);
    if (!raw || raw.length > MAX_PERSISTED_BYTES * 2) return out;
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return out;
    let totalBytes = 0;
    for (const [id, text] of Object.entries(parsed as Record<string, unknown>)) {
      if (out.size >= MAX_PERSISTED_DRAFTS) break;
      if (!id || typeof text !== "string" || text === "") continue;
      const bytes = id.length + text.length;
      if (totalBytes + bytes > MAX_PERSISTED_BYTES) continue;
      totalBytes += bytes;
      out.set(id, text);
    }
  } catch {
    // Corrupt or restricted storage must never break the commit composer.
  }
  return out;
}

function savePersistedDrafts(storage: CommitDraftStorage, drafts: Map<string, string>): void {
  try {
    if (drafts.size === 0) {
      if (typeof storage.removeItem === "function") storage.removeItem(STORAGE_KEY_COMMIT_DRAFTS);
      else storage.setItem(STORAGE_KEY_COMMIT_DRAFTS, "{}");
      return;
    }
    const payload: Record<string, string> = {};
    let count = 0;
    let totalBytes = 0;
    for (const [id, text] of drafts) {
      if (count >= MAX_PERSISTED_DRAFTS) break;
      if (!id || text === "") continue;
      const bytes = id.length + text.length;
      if (totalBytes + bytes > MAX_PERSISTED_BYTES) continue;
      payload[id] = text;
      count += 1;
      totalBytes += bytes;
    }
    storage.setItem(STORAGE_KEY_COMMIT_DRAFTS, JSON.stringify(payload));
  } catch {
    // Quota or storage errors stay silent rather than blocking typing.
  }
}

export function createCommitDraftStore(options: CommitDraftStoreOptions = {}): CommitDraftStore {
  const storage = options.storage ?? null;
  const drafts = storage ? loadPersistedDrafts(storage) : new Map<string, string>();
  let activeId: string | null = null;
  // A writable of a string notifies only when the string changes, so moving
  // between two tabs with the same (often empty) draft costs nothing.
  const active = writable("");
  const sync = () => active.set(activeId === null ? "" : drafts.get(activeId) ?? "");

  const writer = storage
    ? createDeferredWrite(() => savePersistedDrafts(storage, drafts), options.flushDelayMs ?? DEFAULT_FLUSH_DELAY_MS)
    : null;
  if (writer) {
    flushOnPageHide(() => writer.flush());
  }
  const schedulePersist = () => writer?.schedule();

  return {
    subscribe: active.subscribe,
    set(text) {
      if (activeId === null) return;
      const prev = drafts.get(activeId) ?? "";
      if (prev === text) return;
      if (text === "") drafts.delete(activeId);
      else drafts.set(activeId, text);
      schedulePersist();
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
      const changed = carried !== undefined;
      drafts.delete(fromId);
      if (carried !== undefined && !drafts.has(toId)) drafts.set(toId, carried);
      if (changed) schedulePersist();
      sync();
    },
    retain(openIds) {
      const open = new Set(openIds);
      let changed = false;
      for (const id of [...drafts.keys()]) {
        if (!open.has(id)) {
          drafts.delete(id);
          changed = true;
        }
      }
      if (changed) schedulePersist();
      sync();
    },
    flush() {
      writer?.flush();
    },
    hasDrafts: () => drafts.size > 0,
  };
}
