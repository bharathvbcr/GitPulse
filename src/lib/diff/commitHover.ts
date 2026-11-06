/**
 * What the history rail's hover card reads beyond the graph row: the commit
 * message's body and the commit's plain git note.
 *
 * Neither is in the graph payload. The row carries the summary line and
 * nothing else, so a card that showed "the commit" from the row alone would
 * show the subject of a ten-paragraph message as though it were all of it.
 *
 * Reading on hover is a round trip per commit the pointer crosses, so:
 *
 * - **Debounced.** A pointer sweeping down the rail crosses a row every few
 *   milliseconds; only a row it rests on is read.
 * - **Cached per commit.** Hovering back over a commit answers at once. Only
 *   answers are cached, never failures, so the next hover asks again.
 * - **Latest wins.** An answer for a commit the pointer has left is kept in
 *   the cache but never shown.
 * - **Two facets, failing apart.** The body and the note are separate reads;
 *   a note that could not be read does not hide the body, and the card says
 *   which one failed rather than rendering a missing note as "no note".
 *
 * A note can change without its commit changing, so the cache is keyed by
 * repository and commit and dropped whole by `reset()` whenever the graph
 * reloads (which a watcher event or a write triggers).
 */
import { writable, type Readable } from "svelte/store";
import { formatError } from "../ui/formatError";

export type HoverFacet<T> =
  | { state: "loading" }
  | { state: "ready"; value: T }
  | { state: "error"; error: string };

export interface CommitHoverInfo {
  repoPath: string;
  commitId: string;
  /** The message body below the summary; an empty string is a real "no body". */
  body: HoverFacet<string>;
  /** The plain git note; `null` is a measured "no note". */
  note: HoverFacet<string | null>;
}

export interface CommitHoverDeps {
  readBody(repoPath: string, commitId: string): Promise<string>;
  readNote(repoPath: string, commitId: string): Promise<string | null>;
  /** How long the pointer must rest on a row before it is read. */
  delayMs?: number;
  /** Commits whose answers are kept. */
  capacity?: number;
  setTimer?: (fn: () => void, ms: number) => unknown;
  clearTimer?: (handle: unknown) => void;
}

/** The pointer has to rest this long before a row's details are read. */
export const HOVER_READ_DELAY_MS = 250;
/** Commits remembered; a rail scrolled through a long history evicts oldest first. */
export const HOVER_CACHE_CAPACITY = 200;

interface Cached {
  body?: string;
  note?: string | null;
}

export interface CommitHover extends Readable<CommitHoverInfo | null> {
  /** The pointer (or focus) is on `commitId`: show it, reading what is not cached. */
  show(repoPath: string, commitId: string): void;
  /** Nothing is hovered any more. */
  hide(): void;
  /**
   * Forgets every answer: the graph reloaded, so a note may have changed. A
   * card on screen stays up and is read again.
   */
  reset(): void;
}

export function createCommitHover(deps: CommitHoverDeps): CommitHover {
  const delayMs = deps.delayMs ?? HOVER_READ_DELAY_MS;
  const capacity = Math.max(1, deps.capacity ?? HOVER_CACHE_CAPACITY);
  const setTimer = deps.setTimer ?? ((fn: () => void, ms: number) => setTimeout(fn, ms));
  const clearTimer = deps.clearTimer ?? ((handle: unknown) => clearTimeout(handle as ReturnType<typeof setTimeout>));

  const { subscribe, set, update } = writable<CommitHoverInfo | null>(null);
  const cache = new Map<string, Cached>();
  let timer: unknown = null;
  /** Bumped by every show/hide/reset, so a late answer can tell it is late. */
  let epoch = 0;
  let current: { repoPath: string; commitId: string } | null = null;

  const keyOf = (repoPath: string, commitId: string) => `${repoPath}\u0000${commitId}`;

  function remember(key: string, patch: Cached): void {
    const entry = { ...(cache.get(key) ?? {}), ...patch };
    cache.delete(key);
    cache.set(key, entry);
    while (cache.size > capacity) {
      const oldest = cache.keys().next().value;
      if (oldest === undefined) break;
      cache.delete(oldest);
    }
  }

  function infoFrom(repoPath: string, commitId: string, entry: Cached | undefined): CommitHoverInfo {
    return {
      repoPath,
      commitId,
      body: entry?.body !== undefined ? { state: "ready", value: entry.body } : { state: "loading" },
      note: entry?.note !== undefined ? { state: "ready", value: entry.note } : { state: "loading" },
    };
  }

  function cancelTimer(): void {
    if (timer !== null) clearTimer(timer);
    timer = null;
  }

  function isShowing(mine: number, repoPath: string, commitId: string): boolean {
    return mine === epoch && current?.repoPath === repoPath && current.commitId === commitId;
  }

  function read(mine: number, repoPath: string, commitId: string, entry: Cached | undefined): void {
    const key = keyOf(repoPath, commitId);
    if (entry?.body === undefined) {
      deps.readBody(repoPath, commitId).then(
        (body) => {
          remember(key, { body });
          if (isShowing(mine, repoPath, commitId)) {
            update((info) => (info ? { ...info, body: { state: "ready", value: body } } : info));
          }
        },
        (error: unknown) => {
          if (isShowing(mine, repoPath, commitId)) {
            update((info) => (info ? { ...info, body: { state: "error", error: formatError(error) } } : info));
          }
        },
      );
    }
    if (entry?.note === undefined) {
      deps.readNote(repoPath, commitId).then(
        (note) => {
          remember(key, { note });
          if (isShowing(mine, repoPath, commitId)) {
            update((info) => (info ? { ...info, note: { state: "ready", value: note } } : info));
          }
        },
        (error: unknown) => {
          if (isShowing(mine, repoPath, commitId)) {
            update((info) => (info ? { ...info, note: { state: "error", error: formatError(error) } } : info));
          }
        },
      );
    }
  }

  const api: CommitHover = {
    subscribe,
    show(repoPath, commitId) {
      if (current?.repoPath === repoPath && current.commitId === commitId) return;
      cancelTimer();
      const mine = ++epoch;
      current = { repoPath, commitId };
      const entry = cache.get(keyOf(repoPath, commitId));
      set(infoFrom(repoPath, commitId, entry));
      if (entry?.body !== undefined && entry.note !== undefined) return;
      timer = setTimer(() => {
        timer = null;
        if (mine === epoch) read(mine, repoPath, commitId, entry);
      }, delayMs);
    },
    hide() {
      cancelTimer();
      epoch += 1;
      current = null;
      set(null);
    },
    reset() {
      cancelTimer();
      epoch += 1;
      cache.clear();
      // A card on screen stays, and is read again: a reload under a resting
      // pointer (a page of older history arriving) must not blank the card.
      const showing = current;
      current = null;
      if (showing) api.show(showing.repoPath, showing.commitId);
      else set(null);
    },
  };
  return api;
}
