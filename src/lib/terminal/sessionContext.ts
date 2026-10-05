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

/** What the panel knows about a tab whose process *is* an agent CLI. */
export interface AgentTab {
  /** The CLI's name as the tab strip shows it ("Claude"). */
  name: string;
  /** The tab's own lifecycle says the process is starting or running. */
  running: boolean;
  /** The process is a task attempt's, which a close ends for good. */
  taskAttempt: boolean;
}

/**
 * The question to ask before closing, or null when closing needs none.
 *
 * Two ways to have something worth asking about. A shell with a job in the
 * foreground reports `busy`. An agent tab never does: its PTY's child *is*
 * the agent, so the foreground leader is the root process and `busy` reads
 * false — which is how closing a working Claude Code tab, or a task
 * attempt's, used to kill it without a word. So a running agent tab asks on
 * the strength of the panel's own lifecycle, even when the OS could not
 * describe the process at all.
 */
export function closeQuestion(
  context: TerminalContext | null,
  tabLabel: string,
  agent: AgentTab | null = null,
): { title: string; message: string } | null {
  if (agent?.running) {
    return {
      title: `Close ${tabLabel}?`,
      message: agent.taskAttempt
        ? `${agent.name} is still working on this task. Closing the tab stops it and ends this attempt; it cannot be restarted, only resumed as a new conversation.`
        : `${agent.name} is still running in this tab. Closing it stops the agent.`,
    };
  }
  if (context?.busy !== true) return null;
  const program = context.process?.trim() || "A program";
  return {
    title: `Close ${tabLabel}?`,
    message: `${program} is still running in this terminal. Closing it stops ${context.process ? "it" : "that program"}.`,
  };
}
