/**
 * Which terminal sessions are protected from batch closes.
 *
 * A protected session is spared by "Close other tabs", "Close tabs to the
 * right", "Close ended tabs", closing a pair, "Stop children", agent closes and
 * the load governor; the tab's own Close, the Sessions list's Stop and quitting
 * with "stop everything" still stop it. The flag lives on the session ledger
 * row (`cmd_terminal_set_title` with `protected`), which is what lets it
 * survive a page reload: `cmd_terminal_sessions` lists it, and a reloaded page
 * calls `replaceProtection` with what it was told.
 *
 * The batch routes decide here, before asking anything or killing anything,
 * which of their targets are spared. The backend's refusal is the fallback for
 * a flag this page had not learned yet (`killOrigin.protectedSkipOf`), never
 * the first line of defence.
 */

import { writable, type Readable } from "svelte/store";
import { invoke as tauriInvoke } from "../ipc/invoke";

type Invoke = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;

const protectedStore = writable<ReadonlySet<string>>(new Set());

/** Backend session ids this page knows to be protected. */
export const protectedSessions: Readable<ReadonlySet<string>> = { subscribe: protectedStore.subscribe };

/** Replaces what is known with a full host listing: an id not listed is a session that is gone. */
export function replaceProtection(listings: readonly { id: string; protected?: boolean }[]): void {
  protectedStore.set(new Set(listings.filter((listing) => listing.protected === true).map((listing) => listing.id)));
}

/** Records one session's flag, for a refusal that taught this page something. */
export function noteProtection(sessionId: string, on: boolean): void {
  protectedStore.update((current) => {
    if (current.has(sessionId) === on) return current;
    const next = new Set(current);
    if (on) next.add(sessionId);
    else next.delete(sessionId);
    return next;
  });
}

/**
 * Sets a session's protection and, only once the backend has stored it, shows
 * it. The backend refuses a session it has no ledger row for, so a failure
 * rejects and nothing is drawn locked.
 */
export async function setSessionProtected(
  sessionId: string,
  on: boolean,
  invoke: Invoke = tauriInvoke as Invoke,
): Promise<void> {
  // `title` is left out on purpose: the backend then leaves the name alone.
  await invoke("cmd_terminal_set_title", { sessionId, protected: on });
  noteProtection(sessionId, on);
}

/** Splits the targets of a batch close into those it may close and those the flag spares. */
export function partitionByProtection<T>(
  items: readonly T[],
  isProtected: (item: T) => boolean,
): { closable: T[]; spared: T[] } {
  const closable: T[] = [];
  const spared: T[] = [];
  for (const item of items) (isProtected(item) ? spared : closable).push(item);
  return { closable, spared };
}

const NAMES_SHOWN = 6;

/** What a batch says about the sessions it left running, naming them. Null when none were spared. */
export function sparedNotice(labels: readonly string[], route: string): string | null {
  if (labels.length === 0) return null;
  const names = labels.map((label) => label.trim() || "Untitled session");
  const shown = names.slice(0, NAMES_SHOWN).join(", ");
  const rest = names.length > NAMES_SHOWN ? `, and ${names.length - NAMES_SHOWN} more` : "";
  const count = names.length;
  return (
    `${shown}${rest} ${count === 1 ? "is" : "are"} protected from ${route} and ${count === 1 ? "was" : "were"} left running. ` +
    `Close ${count === 1 ? "it" : "each"} directly, or remove its protection, to stop ${count === 1 ? "it" : "them"}.`
  );
}
