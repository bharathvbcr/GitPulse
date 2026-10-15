import { isPermissionMode, type PermissionMode } from "./agentDefaults";
import type { TerminalExitPayload } from "./runResult";
import { isSessionId } from "./tabs";

/**
 * The Claude Code conversation behind every Claude tab GitPulse hosts.
 *
 * Only a task attempt used to have one on record (the attempt's run id is its
 * `--session-id`). A plain Claude tab was a conversation GitPulse could not
 * name, so when ~20 of them died at once from one external SIGTERM every one
 * had to be found and restarted by hand. A fresh launch now carries a
 * `--session-id` GitPulse chose, recorded here and kept in the webview's
 * storage, so a tab that a signal killed can be resumed with
 * `claude --resume <id>`.
 */

/** Flags that already say which conversation a launch is. */
const SESSION_FLAGS = new Set(["--resume", "-r", "--session-id", "--continue", "-c"]);

/** Where the prompt starts: after `--` every argument is text, whatever it looks like. */
function flagEnd(args: readonly string[]): number {
  const at = args.indexOf("--");
  return at === -1 ? args.length : at;
}

/** The value of `--flag value` or `--flag=value` among the flags (before any `--`). */
function flagValue(args: readonly string[], names: readonly string[]): string | null {
  const end = flagEnd(args);
  for (let i = 0; i < end; i++) {
    const arg = args[i];
    if (names.includes(arg)) return i + 1 < end ? args[i + 1] : null;
    const eq = arg.indexOf("=");
    if (eq > 0 && names.includes(arg.slice(0, eq))) return arg.slice(eq + 1);
  }
  return null;
}

/** Whether the arguments already name a conversation (start, resume or continue). */
export function carriesSession(args: readonly string[]): boolean {
  const end = flagEnd(args);
  return args.slice(0, end).some((arg) => SESSION_FLAGS.has(arg) || SESSION_FLAGS.has(arg.split("=")[0]));
}

/**
 * The arguments for a Claude launch and the conversation id it will have.
 *
 * A launch that already names a conversation keeps its arguments exactly as
 * they are, and the id is the one it names when that is a session id. Any
 * other launch gets `--session-id <fresh id>` in front, so Claude Code uses
 * the id GitPulse has recorded rather than one it picks itself. `--continue`
 * resumes whichever conversation Claude Code finds, so no id is known.
 */
export function withSessionId(
  args: readonly string[] | undefined,
  newId: () => string = () => crypto.randomUUID(),
): { args: string[]; sessionId: string | null } {
  const given = args ?? [];
  if (carriesSession(given)) {
    const named = flagValue(given, ["--resume", "-r", "--session-id"]);
    return { args: [...given], sessionId: named && isSessionId(named) ? named.toLowerCase() : null };
  }
  const sessionId = newId().toLowerCase();
  return { args: ["--session-id", sessionId, ...given], sessionId };
}

/** One hosted Claude conversation, as it is kept between app runs. */
export interface HostedClaude {
  /** Claude Code's session id: what `--resume` takes. */
  sessionId: string;
  /** The backend's id for the PTY it runs in, once one has started. */
  terminalId: string | null;
  /** The directory the process started in. */
  repoPath: string;
  startDir: string | null;
  /** The permission mode it ran under; null is the CLI's own default. */
  mode: PermissionMode | null;
  startedAt: number;
  /** The signal that ended it, when one did. Null while it runs or after a clean end. */
  endedSignal: string | null;
}

/** The most conversations kept; the oldest go first. */
export const MAX_HOSTED_CLAUDE = 200;
const STORAGE_KEY = "gitpulse.hostedClaude.v1";

export interface HostedStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

function text(value: unknown, limit: number): string | null {
  return typeof value === "string" && value.length > 0 && value.length <= limit ? value : null;
}

/** A stored record, or null for anything that is not one. A bad record never throws. */
export function readHostedClaude(raw: unknown): HostedClaude | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as Record<string, unknown>;
  const sessionId = typeof r.sessionId === "string" && isSessionId(r.sessionId) ? r.sessionId.toLowerCase() : null;
  const repoPath = text(r.repoPath, 4096);
  if (!sessionId || !repoPath || typeof r.startedAt !== "number" || !Number.isFinite(r.startedAt)) return null;
  return {
    sessionId,
    terminalId: text(r.terminalId, 128),
    repoPath,
    startDir: text(r.startDir, 4096),
    mode: isPermissionMode(r.mode) ? r.mode : null,
    startedAt: r.startedAt,
    endedSignal: text(r.endedSignal, 128),
  };
}

export function createHostedClaudeStore(storage: HostedStorage | null) {
  const records = new Map<string, HostedClaude>();

  try {
    const parsed: unknown = JSON.parse(storage?.getItem(STORAGE_KEY) ?? "[]");
    if (Array.isArray(parsed)) {
      for (const entry of parsed) {
        const record = readHostedClaude(entry);
        if (record) records.set(record.sessionId, record);
      }
    }
  } catch {
    // Unreadable storage is an empty store, not a failed launch.
  }

  function persist() {
    while (records.size > MAX_HOSTED_CLAUDE) {
      let oldest: HostedClaude | null = null;
      for (const record of records.values()) if (!oldest || record.startedAt < oldest.startedAt) oldest = record;
      if (!oldest) break;
      records.delete(oldest.sessionId);
    }
    try {
      storage?.setItem(STORAGE_KEY, JSON.stringify([...records.values()]));
    } catch {
      // Quota or a private window: the in-memory record still serves this run.
    }
  }

  return {
    /** Records a launch before its process exists. */
    begin(record: Omit<HostedClaude, "terminalId" | "endedSignal">) {
      records.set(record.sessionId, { ...record, terminalId: null, endedSignal: null });
      persist();
    },
    /** Ties the conversation to the PTY that runs it. */
    bind(sessionId: string, terminalId: string) {
      const record = records.get(sessionId);
      if (!record) return;
      records.set(sessionId, { ...record, terminalId });
      persist();
    },
    /** Records how it ended: a signal name, or null for any other end. */
    ended(sessionId: string, signal: string | null) {
      const record = records.get(sessionId);
      if (!record) return;
      records.set(sessionId, { ...record, endedSignal: signal });
      persist();
    },
    forget(sessionId: string) {
      if (records.delete(sessionId)) persist();
    },
    get(sessionId: string): HostedClaude | undefined {
      return records.get(sessionId);
    },
    /** The conversation running in this PTY, if one is recorded. */
    byTerminal(terminalId: string): HostedClaude | undefined {
      let found: HostedClaude | undefined;
      for (const record of records.values()) {
        if (record.terminalId === terminalId && (!found || record.startedAt >= found.startedAt)) found = record;
      }
      return found;
    },
    /** Conversations a signal ended and nothing has resumed since. */
    resumable(): HostedClaude[] {
      return [...records.values()].filter((record) => record.endedSignal !== null);
    },
    list(): HostedClaude[] {
      return [...records.values()];
    },
  };
}

function browserStorage(): HostedStorage | null {
  try {
    return typeof window === "undefined" ? null : window.localStorage;
  } catch {
    return null;
  }
}

/** The app-wide store, kept in the webview's local storage. */
export const hostedClaude = createHostedClaudeStore(browserStorage());

/**
 * What a relaunch did: `started` a process; `deferred` — every terminal slot
 * was taken, and the tab starts it when one frees (never counted as started);
 * or `failed`, which the tab itself shows.
 */
export type ResumeOutcome = "started" | "deferred" | "failed";

/** What a tab offers when its process was ended by a signal: relaunch its conversation. */
export type Resumer = () => Promise<ResumeOutcome>;

/** The parts of a PTY's exit that decide what becomes of its conversation. */
export type ExitFacts = Pick<TerminalExitPayload, "signal" | "ended_by_gitpulse" | "error">;

/**
 * Whether an exit left a conversation someone may want back: ended by a
 * signal, and not by GitPulse's own Close. A session GitPulse stopped because
 * it could no longer serve it (`error` says why) still counts — the person did
 * not ask for that end.
 */
export function endedFromOutside(exit: ExitFacts): boolean {
  return exit.signal !== "" && (!exit.ended_by_gitpulse || exit.error !== null);
}

/**
 * The one decision on what a PTY's end does to the conversation recorded for
 * it, for every place that sees the end: the tab running it, and the Sessions
 * record of a process whose tab went away. An end someone asked for — a
 * Close, acknowledged or seen as an exit GitPulse caused, or the program's own
 * end — forgets it. An end from outside keeps it, marked with the signal, and
 * returns true: it may be offered for resumption, once its transcript is
 * found. A view going away is not an end, and never comes here.
 */
export function settleConversation(
  store: Pick<ReturnType<typeof createHostedClaudeStore>, "ended" | "forget">,
  sessionId: string,
  end: ExitFacts | "closed",
): boolean {
  if (end === "closed" || !endedFromOutside(end)) {
    store.forget(sessionId);
    return false;
  }
  store.ended(sessionId, end.signal);
  return true;
}

/**
 * The relaunch each signal-ended tab has on offer, by the backend's PTY id.
 *
 * Tabs register one when their process dies of a signal and take it back when
 * they restart or go away, so an incident can resume exactly the tabs that are
 * still on screen and no others.
 */
export function createResumers() {
  const byTerminal = new Map<string, Resumer>();
  return {
    register(terminalId: string, resume: Resumer) {
      byTerminal.set(terminalId, resume);
    },
    unregister(terminalId: string) {
      byTerminal.delete(terminalId);
    },
    has: (terminalId: string) => byTerminal.has(terminalId),
    get: (terminalId: string) => byTerminal.get(terminalId),
    size: () => byTerminal.size,
  };
}

export const resumers = createResumers();
