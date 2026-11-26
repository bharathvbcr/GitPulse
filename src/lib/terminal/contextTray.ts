/**
 * The context tray: file ranges a person has set aside while reading, to hand
 * to an agent together (`Insert all`) instead of one send per range.
 *
 * In memory only. Nothing here is persisted: a tray is a working set for the
 * conversation in front of you, a restart empties it, and the ranges it
 * names are line numbers that a later edit would make point at other code.
 *
 * Bounded on purpose. At `MAX_TRAY_ENTRIES` an add is refused with a reason,
 * never by quietly evicting an older range, and what `Insert all` produces must
 * fit the same limit a composed message does.
 */

import { writable } from "svelte/store";
import { AGENT_MESSAGE_MAX_BYTES } from "../workbench/sendToAgent";
import { safeText } from "./agentPromptText";
import { formatSelectionRef } from "./selectionContent";

export const MAX_TRAY_ENTRIES = 50;
/** The longest path a tray entry holds; the same bound a reveal request uses. */
export const MAX_TRAY_PATH = 1024;

export interface TrayEntry {
  /** The checkout (worktree path) the range was selected in. */
  checkout: string;
  /** Repository-relative path. */
  path: string;
  /** 1-based, `startLine <= endLine`. */
  startLine: number;
  endLine: number;
}

export type TrayAdd = { ok: true; added: boolean } | { ok: false; reason: string };

/** The trays, by repository path. */
export const contextTrays = writable<Readonly<Record<string, readonly TrayEntry[]>>>({});

let trays: Readonly<Record<string, readonly TrayEntry[]>> = {};
contextTrays.subscribe((value) => (trays = value));

/** The tray for `repoPath`; empty when it has none. */
export function trayFor(repoPath: string): readonly TrayEntry[] {
  return trays[repoPath] ?? [];
}

const same = (a: TrayEntry, b: TrayEntry) =>
  a.checkout === b.checkout && a.path === b.path && a.startLine === b.startLine && a.endLine === b.endLine;

/**
 * Adds a range. An identical range (same checkout, path and lines) is already
 * there and is not added twice, which is not a failure. A reversed range is
 * put the right way round; anything that is not a real range is refused.
 */
export function addToTray(repoPath: string, entry: TrayEntry): TrayAdd {
  if (!repoPath) return { ok: false, reason: "Open a repository first." };
  if (!entry.path || entry.path.length > MAX_TRAY_PATH) return { ok: false, reason: "That file's path cannot be added to the tray." };
  if (!Number.isInteger(entry.startLine) || !Number.isInteger(entry.endLine) || entry.startLine < 1 || entry.endLine < 1) {
    return { ok: false, reason: "Select a line range first." };
  }
  const next: TrayEntry = {
    ...entry,
    startLine: Math.min(entry.startLine, entry.endLine),
    endLine: Math.max(entry.startLine, entry.endLine),
  };
  const current = trayFor(repoPath);
  if (current.some((existing) => same(existing, next))) return { ok: true, added: false };
  if (current.length >= MAX_TRAY_ENTRIES) {
    return { ok: false, reason: `The tray holds ${MAX_TRAY_ENTRIES} ranges. Insert or clear it before adding more.` };
  }
  contextTrays.update((all) => ({ ...all, [repoPath]: [...current, next] }));
  return { ok: true, added: true };
}

/** Empties the tray for `repoPath`. */
export function clearTray(repoPath: string): void {
  contextTrays.update(({ [repoPath]: _gone, ...rest }) => rest);
}

/** The reference for one entry: `@path#Lx-Ly`, its path shown safely. */
export function trayReference(entry: TrayEntry): string {
  return formatSelectionRef(safeText(entry.path), entry.startLine, entry.endLine).trimEnd();
}

/** What `Insert all` types: every reference, joined by spaces, ending in one. A single line, never submitted. */
export function trayText(entries: readonly TrayEntry[]): string {
  return entries.length === 0 ? "" : `${entries.map(trayReference).join(" ")} `;
}

/** The size of `trayText`, in bytes of UTF-8, for the tray chip. */
export function trayBytes(entries: readonly TrayEntry[]): number {
  return new TextEncoder().encode(trayText(entries)).length;
}

/** Why `Insert all` cannot go as it is, or null. Checked before anything is inserted. */
export function trayProblem(entries: readonly TrayEntry[]): string | null {
  if (entries.length === 0) return "The tray is empty.";
  const bytes = trayBytes(entries);
  return bytes > AGENT_MESSAGE_MAX_BYTES
    ? `The tray's references are ${bytes} bytes; at most ${AGENT_MESSAGE_MAX_BYTES} can be inserted. Clear some of it.`
    : null;
}
