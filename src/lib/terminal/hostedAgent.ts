import { isPermissionMode, type PermissionMode } from "./agentDefaults";
import type { TerminalExitPayload } from "./runResult";
import { isSessionId } from "./tabs";

/**
 * The hosted agent conversation (Claude Code or Antigravity) behind every
 * managed agent tab GitPulse hosts.
 *
 * A fresh launch carries a session id GitPulse chose (--session-id for Claude,
 * --conversation for Antigravity), recorded here and kept in the webview's
 * storage, so a tab ended by an external signal can be resumed.
 */

export type AgentProvider = "claude" | "agy";

/** Flags that already say which conversation a launch is. */
const SESSION_FLAGS = new Set(["--resume", "-r", "--session-id", "--continue", "-c", "--conversation"]);

/** Where the prompt starts: after `--` every argument is text, whatever it looks like. */
function flagEnd(args: readonly string[]): number {
  const at = args.indexOf("--");
  return at === -1 ? args.length : at;
}

function cleanSessionValue(val: string): string {
  let s = val.trim();
  if ((s.startsWith('"') && s.endsWith('"')) || (s.startsWith("'") && s.endsWith("'"))) {
    s = s.slice(1, -1).trim();
  }
  return s;
}

/** Finds a canonical session id from matching flags, supporting quotes, whitespace, and multiple occurrences. */
function namedSessionId(args: readonly string[], names: readonly string[]): string | null {
  const end = flagEnd(args);
  let result: string | null = null;
  for (let i = 0; i < end; i++) {
    const arg = args[i];
    let candidate: string | null = null;
    if (names.includes(arg)) {
      if (i + 1 < end) {
        candidate = args[i + 1];
        i += 1;
      }
    } else {
      const eq = arg.indexOf("=");
      if (eq > 0 && names.includes(arg.slice(0, eq))) {
        candidate = arg.slice(eq + 1);
      }
    }
    if (candidate !== null) {
      const cleaned = cleanSessionValue(candidate);
      if (isSessionId(cleaned)) {
        result = cleaned.toLowerCase();
      }
    }
  }
  return result;
}

/** Whether the arguments already name a conversation (start, resume or continue). */
export function carriesSession(args: readonly string[]): boolean {
  const end = flagEnd(args);
  return args.slice(0, end).some((arg) => SESSION_FLAGS.has(arg) || SESSION_FLAGS.has(arg.split("=")[0]));
}

/**
 * The arguments for an agent launch and the conversation id it will have.
 *
 * A launch that already names a conversation keeps its arguments exactly as
 * they are, and the id is the one it names when that is a session id. A fresh
 * Claude launch gets `--session-id <fresh id>` in front so the CLI uses the id
 * GitPulse recorded; Antigravity's `--conversation` is resume-only (passing a
 * fresh UUID emits `warning: conversation "<id>" not found`), so fresh `agy`
 * launches keep their arguments unchanged until resumed with `--conversation`.
 */
export function withSessionId(
  args: readonly string[] | undefined,
  newId: () => string = () => crypto.randomUUID(),
  provider: AgentProvider = "claude",
): { args: string[]; sessionId: string | null } {
  const given = args ?? [];
  if (carriesSession(given)) {
    const sessionId = namedSessionId(given, ["--resume", "-r", "--session-id", "--conversation"]);
    return { args: [...given], sessionId };
  }
  if (provider === "agy") {
    return { args: [...given], sessionId: null };
  }
  const sessionId = newId().toLowerCase();
  return { args: ["--session-id", sessionId, ...given], sessionId };
}

/** One hosted agent conversation, as it is kept between app runs. */
export interface HostedAgent {
  /** The session/conversation id: what resume takes. */
  sessionId: string;
  /** The agent provider for this session: "claude" or "agy". */
  provider: AgentProvider;
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

export type HostedClaude = HostedAgent;

export const AGY_CONTINUE_SENTINEL = "__agy_continue__";

function workspaceDirForRecord(record: { repoPath: string; startDir: string | null }): string {
  const base = record.repoPath.replace(/[\\/]+$/, "");
  const sub = record.startDir?.replace(/^[\\/]+|[\\/]+$/g, "") ?? "";
  if (!base) return sub;
  return sub ? `${base}/${sub}` : base;
}

/**
 * The conversation an agent tab's next process runs, decided where the
 * process starts and recorded only when the start makes one.
 */
export function conversationForStart(
  store: Pick<ReturnType<typeof createHostedAgentStore>, "begin" | "byTerminal">,
  start: {
    attach: string | null;
    resumeId: string | null;
    args: string[] | undefined;
    record: Omit<HostedAgent, "sessionId" | "terminalId" | "endedSignal" | "provider"> & { provider?: AgentProvider };
    newId?: () => string;
    provider?: AgentProvider;
  },
): { args: string[] | undefined; sessionId: string | null } {
  if (start.attach) return { args: start.args, sessionId: store.byTerminal(start.attach)?.sessionId ?? null };
  const provider = start.provider ?? start.record.provider ?? "claude";
  const workspaceDir = workspaceDirForRecord(start.record);
  const resumeArgs = start.resumeId
    ? (provider === "agy"
        ? (start.resumeId === AGY_CONTINUE_SENTINEL
            ? ["--continue", ...(workspaceDir ? ["--add-dir", workspaceDir] : [])]
            : ["--conversation", start.resumeId])
        : ["--resume", start.resumeId])
    : start.args;
  const hosted = withSessionId(resumeArgs, start.newId, provider);
  if (hosted.sessionId) store.begin({ ...start.record, provider, sessionId: hosted.sessionId });
  return hosted;
}

/** The most conversations kept; the oldest go first. */
export const MAX_HOSTED_AGENT = 200;
export const MAX_HOSTED_CLAUDE = MAX_HOSTED_AGENT;

const STORAGE_KEY = "gitpulse.hostedAgent.v1";
const LEGACY_STORAGE_KEY = "gitpulse.hostedClaude.v1";

export interface HostedStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

function text(value: unknown, limit: number): string | null {
  return typeof value === "string" && value.length > 0 && value.length <= limit ? value : null;
}

/** A stored record, or null for anything that is not one. A bad record never throws. */
export function readHostedAgent(raw: unknown): HostedAgent | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as Record<string, unknown>;
  const sessionId = typeof r.sessionId === "string" && isSessionId(r.sessionId) ? r.sessionId.toLowerCase() : null;
  const repoPath = text(r.repoPath, 4096);
  if (!sessionId || !repoPath || typeof r.startedAt !== "number" || !Number.isFinite(r.startedAt)) return null;
  const provider: AgentProvider = r.provider === "agy" ? "agy" : "claude";
  return {
    sessionId,
    provider,
    terminalId: text(r.terminalId, 128),
    repoPath,
    startDir: text(r.startDir, 4096),
    mode: isPermissionMode(r.mode) ? r.mode : null,
    startedAt: r.startedAt,
    endedSignal: text(r.endedSignal, 128),
  };
}

export const readHostedClaude = readHostedAgent;

export function createHostedAgentStore(storage: HostedStorage | null) {
  const records = new Map<string, HostedAgent>();

  try {
    const raw = storage?.getItem(STORAGE_KEY) ?? storage?.getItem(LEGACY_STORAGE_KEY) ?? "[]";
    const parsed: unknown = JSON.parse(raw);
    if (Array.isArray(parsed)) {
      for (const entry of parsed) {
        const record = readHostedAgent(entry);
        if (record) records.set(record.sessionId, record);
      }
    }
  } catch {
    // Unreadable storage is an empty store, not a failed launch.
  }

  function persist() {
    while (records.size > MAX_HOSTED_AGENT) {
      let oldest: HostedAgent | null = null;
      for (const record of records.values()) if (!oldest || record.startedAt < oldest.startedAt) oldest = record;
      if (!oldest) break;
      records.delete(oldest.sessionId);
    }
    try {
      const serialized = JSON.stringify([...records.values()]);
      storage?.setItem(STORAGE_KEY, serialized);
      storage?.setItem(LEGACY_STORAGE_KEY, serialized);
    } catch {
      // Quota or a private window: the in-memory record still serves this run.
    }
  }

  return {
    /** Records a launch before its process exists. */
    begin(record: Omit<HostedAgent, "terminalId" | "endedSignal" | "provider"> & { provider?: AgentProvider; sessionId: string }) {
      records.set(record.sessionId, {
        ...record,
        provider: record.provider ?? "claude",
        terminalId: null,
        endedSignal: null,
      });
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
    get(sessionId: string): HostedAgent | undefined {
      return records.get(sessionId);
    },
    /** The conversation running in this PTY, if one is recorded. */
    byTerminal(terminalId: string): HostedAgent | undefined {
      let found: HostedAgent | undefined;
      for (const record of records.values()) {
        if (record.terminalId === terminalId && (!found || record.startedAt >= found.startedAt)) found = record;
      }
      return found;
    },
    /** Conversations a signal ended and nothing has resumed since. */
    resumable(): HostedAgent[] {
      return [...records.values()].filter((record) => record.endedSignal !== null);
    },
    list(): HostedAgent[] {
      return [...records.values()];
    },
  };
}

export const createHostedClaudeStore = createHostedAgentStore;

function browserStorage(): HostedStorage | null {
  try {
    return typeof window === "undefined" ? null : window.localStorage;
  } catch {
    return null;
  }
}

/** The app-wide store, kept in the webview's local storage. */
export const hostedAgent = createHostedAgentStore(browserStorage());
export const hostedClaude = hostedAgent;

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
 * signal, and not by GitPulse's own Close.
 */
export function endedFromOutside(exit: ExitFacts): boolean {
  return exit.signal !== "" && (!exit.ended_by_gitpulse || exit.error !== null);
}

/**
 * The one decision on what a PTY's end does to the conversation recorded for
 * it, for every place that sees the end.
 */
export function settleConversation(
  store: Pick<ReturnType<typeof createHostedAgentStore>, "ended" | "forget">,
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

export interface TranscriptCheckAnswer {
  state: "found" | "absent" | "unknown";
  reason?: string | null;
}

/**
 * Applies the transcript check result for a signal-ended conversation,
 * forgetting absent records so dead UUIDs never leak in localStorage.
 */
export function resolveTranscriptAnswer(
  store: Pick<ReturnType<typeof createHostedAgentStore>, "forget">,
  conversation: string,
  provider: AgentProvider,
  answer: TranscriptCheckAnswer,
): { offer: string | null; note: string | null } {
  if (answer.state === "found") {
    return { offer: conversation, note: null };
  }
  if (answer.state === "absent") {
    store.forget(conversation);
    return {
      offer: null,
      note:
        provider === "agy"
          ? "Antigravity saved no conversation for this session, so there is nothing to resume."
          : "Claude Code saved no conversation for this session, so there is nothing to resume.",
    };
  }
  return {
    offer: null,
    note: `Couldn't check whether a saved conversation exists. ${answer.reason ?? "No reason was reported."}`,
  };
}

/**
 * Recovery offer for a fresh Antigravity tab (`hostedId === null`) ended by an
 * external signal: relaunches with `agy --continue --add-dir <dir>`.
 */
export function unindexedExitOffer(
  provider: AgentProvider | null,
  hostedId: string | null,
  end: ExitFacts | "closed",
): string | null {
  if (provider !== "agy" || hostedId !== null || end === "closed" || !endedFromOutside(end)) {
    return null;
  }
  return AGY_CONTINUE_SENTINEL;
}

/**
 * The relaunch each signal-ended tab has on offer, by the backend's PTY id.
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
