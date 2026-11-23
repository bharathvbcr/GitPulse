/**
 * The line range a file viewer currently has selected, for the one surface that
 * is not the viewer: the command palette.
 *
 * The selection lives in the viewer, and the palette is built from the
 * repository's state, which does not hold it. A viewer registers its selection
 * here while it has one and removes it when it loses it or goes away. A
 * second viewer (a split pane) that selects later replaces the first; the first
 * going away afterwards does not remove the second's, because removal names its
 * owner. In memory only.
 */

import { get, writable } from "svelte/store";

export interface ActiveSelection {
  path: string;
  startLine: number;
  endLine: number;
  /** Types `@path#Lx-Ly` into the recipient session. */
  sendReference(): void;
  /** Types the selected lines in a fenced block into the recipient session. */
  sendContent(): void;
  /** Sets the range aside in the repository's context tray. */
  addToTray(): void;
}

const current = writable<{ owner: object; selection: ActiveSelection } | null>(null);

/** The selection the palette would act on, or null when no viewer has one. */
export function activeSelection(): ActiveSelection | null {
  return get(current)?.selection ?? null;
}

/** `owner` has a selection now; it replaces whoever's was there. */
export function setActiveSelection(owner: object, selection: ActiveSelection): void {
  current.set({ owner, selection });
}

/** `owner` has no selection any more. Does nothing if another viewer's has replaced it. */
export function clearActiveSelection(owner: object): void {
  if (get(current)?.owner === owner) current.set(null);
}
