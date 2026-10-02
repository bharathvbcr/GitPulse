/**
 * What a live terminal is doing, as `cmd_terminal_context` reports it, and
 * the two decisions made from it: where a tab opened beside it starts, and
 * whether closing it needs a question first.
 *
 * Every field can be unknown, because the OS can decline to describe a
 * process. Unknown is never read as idle or as the repository root: a new
 * tab with no known directory starts at the root because that is the default,
 * not because the old shell was there, and a tab whose foreground cannot be
 * read closes the way it always has — without a question it cannot justify.
 */

import type { TerminalContext } from "./runResult";
export type { TerminalContext };

const MAX_FIELD = 4096;

function optionalString(value: unknown): string | null | undefined {
  if (value === null || value === undefined) return null;
  if (typeof value !== "string" || value.length > MAX_FIELD || value.includes("\0")) return undefined;
  return value;
}

/** Validates the payload at the boundary. A malformed answer is no answer. */
export function parseTerminalContext(raw: unknown): TerminalContext | null {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  const record = raw as Record<string, unknown>;
  const process = optionalString(record.process);
  const cwd = optionalString(record.cwd);
  const repoDir = optionalString(record.repo_dir);
  const busy = record.busy === null || record.busy === undefined ? null : record.busy;
  if (process === undefined || cwd === undefined || repoDir === undefined) return null;
  if (busy !== null && typeof busy !== "boolean") return null;
  return { process, busy, cwd, repo_dir: repoDir };
}

/**
 * The repository-relative directory a tab opened beside this one should
 * start in, or null for the root. A path that climbs or is absolute is not
 * one the backend would accept, so it is not offered.
 */
export function startDirFrom(context: TerminalContext | null): string | null {
  const dir = context?.repo_dir;
  if (!dir) return null;
  if (dir.startsWith("/") || dir.split(/[\\/]/).includes("..")) return null;
  return dir;
}

/** The question to ask before closing, or null when closing needs none. */
export function closeQuestion(
  context: TerminalContext | null,
  tabLabel: string,
): { title: string; message: string } | null {
  if (context?.busy !== true) return null;
  const program = context.process?.trim() || "A program";
  return {
    title: `Close ${tabLabel}?`,
    message: `${program} is still running in this terminal. Closing it stops ${context.process ? "it" : "that program"}.`,
  };
}
