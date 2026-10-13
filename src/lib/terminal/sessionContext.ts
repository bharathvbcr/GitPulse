/**
 * What a live terminal is doing, as `cmd_terminal_context` reports it, and
 * the two decisions made from it: where a tab opened beside it starts, and
 * whether closing it needs a question first.
 *
 * Every field can be unknown, because the OS can decline to describe a
 * process, and the read itself can time out or fail. The two decisions treat
 * unknown differently, on purpose. A new tab with no known directory starts at
 * the root because that is the default, not because the old shell was there.
 * A close that cannot tell whether something is running ASKS: an unknown is
 * never read as idle, because a needless question costs one click and the
 * opposite guess costs a killed job nobody was warned about.
 */

import type { TerminalContext } from "./runResult";
import { relativeStartDir } from "./tabs";
export type { TerminalContext };

/**
 * One attempt to learn what a session is running. Three outcomes, kept apart
 * by type so that a caller cannot collapse "could not tell" into "nothing":
 *
 * - `known`: the host answered; fields of the context may still be unreadable.
 * - `none`: there is no process to interrupt, established by the tab's own
 *   lifecycle (it ended, or never started one), not by a failed read.
 * - `unknown`: the read timed out, failed or was malformed, or the session is
 *   in a state the tab cannot vouch for.
 */
export type ContextRead =
  | { readonly kind: "known"; readonly context: TerminalContext }
  | { readonly kind: "none" }
  | { readonly kind: "unknown" };

const NO_PROCESS: ContextRead = Object.freeze({ kind: "none" });
export const CONTEXT_UNKNOWN: ContextRead = Object.freeze({ kind: "unknown" });

/** What the tab itself knows about its session, independent of any read. */
export interface SessionVantage {
  /** The host's id for the live session, or empty when the tab holds none. */
  sessionId: string;
  /** The tab is still the owner of that session (not disposed, not superseded). */
  current: boolean;
  /** A spawn is in flight: a process may exist that the tab has no id for yet. */
  spawning: boolean;
  /** The host reported the process gone. */
  exited: boolean;
}

/**
 * Reads what a session is running, failing toward "unknown".
 *
 * `read` carries its own deadline (the caller wraps the IPC in
 * `terminalDeadline`); a rejection, a timeout and a malformed answer all land
 * on `unknown`. `none` is returned only on the tab's own evidence that no
 * process exists: the host reported the exit, or no spawn was ever started.
 */
export async function readSessionContext(
  vantage: SessionVantage,
  read: (sessionId: string) => Promise<unknown>,
): Promise<ContextRead> {
  if (vantage.exited) return NO_PROCESS;
  if (!vantage.sessionId) return vantage.spawning ? CONTEXT_UNKNOWN : NO_PROCESS;
  if (!vantage.current) return CONTEXT_UNKNOWN;
  try {
    return contextReadOf(await read(vantage.sessionId));
  } catch {
    return CONTEXT_UNKNOWN;
  }
}

/** A payload the host sent, as a read: `known` when it parses, else `unknown`. */
export function contextReadOf(raw: unknown): ContextRead {
  const context = parseTerminalContext(raw);
  return context ? { kind: "known", context } : CONTEXT_UNKNOWN;
}

/**
 * Whether the host positively reported an idle shell: nothing in the
 * foreground and no job left under it. Either field unreadable is not idle.
 */
export function isProvablyIdle(context: TerminalContext): boolean {
  return context.busy === false && context.jobs === false;
}

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
  const jobs = record.jobs === null || record.jobs === undefined ? null : record.jobs;
  if (jobs !== null && typeof jobs !== "boolean") return null;
  return { process, busy, ...(jobs === null ? {} : { jobs }), cwd, repo_dir: repoDir };
}

/**
 * The repository-relative directory a tab opened beside this one should
 * start in, or null for the root. A path that climbs or is absolute is not
 * one the backend would accept, so it is not offered. An unknown read offers
 * none either: here unknown means "the default", the root, which is safe in a
 * way that unknown-means-idle is not for a close.
 */
export function startDirFrom(read: ContextRead): string | null {
  return read.kind === "known" ? relativeStartDir(read.context.repo_dir) : null;
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
 *
 * Fails closed. Null means the tab is positively known to hold nothing worth
 * stopping: no process (`none`), or a host-reported idle shell. A read that
 * failed, timed out or came back with the foreground or the job list
 * unreadable asks, and says it could not tell rather than naming a cause.
 */
export function closeQuestion(
  read: ContextRead,
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
  if (read.kind === "none") return null;
  const title = `Close ${tabLabel}?`;
  if (read.kind === "unknown") return { title, message: COULD_NOT_TELL };
  const { context } = read;
  if (context.busy === true) {
    const program = context.process?.trim() || "A program";
    return {
      title,
      message: `${program} is still running in this terminal. Closing it stops ${context.process ? "it" : "that program"}.`,
    };
  }
  // Nothing in the foreground, but a background or stopped job is still the
  // user's work, and a close kills it as surely as it kills the foreground.
  if (context.jobs === true) {
    return { title, message: "A background or stopped job is still running in this terminal. Closing it stops that job." };
  }
  return isProvablyIdle(context) ? null : { title, message: COULD_NOT_TELL };
}

const COULD_NOT_TELL = "GitPulse could not tell whether something is still running in this terminal. Close it anyway?";
