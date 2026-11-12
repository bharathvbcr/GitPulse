import { writable } from "svelte/store";
import { isSessionId, normalizePaneFilePath, type LauncherKind, type SessionKind, type TabKind } from "./tabs";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { AGENT_PROMPT_MAX_BYTES } from "./agentPromptText";
import { isCaseInsensitiveFs, sameRepo, type PathIdentityOptions } from "../repos/paths";
import { TERMINAL_SESSION_PENDING } from "./sessionMessages";
import { formatError } from "../ui/formatError";

export type PromptLauncher = Extract<LauncherKind, "claude" | "codex" | "grok" | "agy">;

/** The one text a canceled terminal start carries, so a caller that passes it on and a test that checks it cannot drift. */
export const TERMINAL_SESSION_CANCELED = "Terminal session canceled";

export const PROMPT_LAUNCHERS: readonly PromptLauncher[] = ["claude", "codex", "grok", "agy"];

export function isPromptLauncher(value: unknown): value is PromptLauncher {
  return PROMPT_LAUNCHERS.some((launcher) => launcher === value);
}

/*
 * The notification flags that make an agent CLI speak to this terminal are
 * not here. They are `terminal_command::notify_flags` in the backend, added
 * at spawn for a plain agent tab and for a task attempt alike: a copy here
 * reached only the tabs this file launches, so every task attempt — which
 * never passes through it — waited on a permission prompt in silence.
 */

/** Interactive CLIs that accept an initial prompt. No shell interpolation. */
export function agentPromptArgs(launcher: SessionKind, prompt?: string): string[] | null {
  if (prompt === undefined) return null;
  if (!isPromptLauncher(launcher)) {
    throw new Error("Initial prompts require Claude Code, Codex, Grok, or Antigravity");
  }
  if (!prompt.trim() || prompt.includes("\0") || new TextEncoder().encode(prompt).length > AGENT_PROMPT_MAX_BYTES) {
    throw new Error("Agent prompt must be nonempty, contain no NUL, and fit within 16000 bytes");
  }
  // Antigravity reads prompts only from `--prompt-interactive` / `--print`, never positionally.
  if (launcher === "agy") return ["--prompt-interactive", prompt];
  return ["--", prompt];
}

/**
 * The arguments that pick a Claude Code or Antigravity conversation back up,
 * optionally with a first prompt: `claude --resume <id> -- <prompt>` or
 * `agy --conversation <id> --prompt-interactive <prompt>`. Nothing here goes
 * through a shell. The prompt is held to the same ceiling as an initial prompt;
 * `sessionId` must be a valid canonical session UUID.
 */
export function resumeArgs(sessionId: string, prompt?: string, provider: "claude" | "agy" = "claude"): string[] {
  if (!isSessionId(sessionId)) throw new Error("Only a Claude Code conversation with a valid session id can be resumed.");
  if (provider === "agy") {
    return ["--conversation", sessionId, ...(agentPromptArgs("agy", prompt) ?? [])];
  }
  return ["--resume", sessionId, ...(agentPromptArgs("claude", prompt) ?? [])];
}

interface LaunchRequest {
  repoPath: string;
  /**
   * Any launcher, not only the two that accept a prompt.
   *
   * Widened so "start a shell here" uses this one channel rather than a
   * fourth request store beside it: the panel already knows how to claim a
   * request, open a tab and report capacity refusals, and `agentPromptArgs`
   * already returns "no arguments" for a promptless launch of any kind.
   */
  launcher: TabKind;
  /** Absent for a plain session; a literal CLI argument when present. */
  prompt?: string;
  /** A browser session's first address (`requestBrowser`); absent otherwise. */
  url?: string;
  /** A file pane's repository-relative path and optional reveal coordinates (`requestFile`). */
  filePath?: string;
  line?: number | null;
  column?: number | null;
  /** Pre-arms quiescent peer auto-wake on unattended kickoff launches. */
  autoWake?: boolean;
  complete(error?: string): void;
}

interface AgentTerminal {
  id: string;
  repoPath: string;
  launcher: PromptLauncher;
  status: string;
  reveal(): void;
}

/** A bounded handoff to a lazily mounted terminal panel, consumed exactly once. */
export function createTerminalLaunchRequests() {
  const store = writable<LaunchRequest | null>(null);
  // Tab references outlive a finished PTY, so its transcript remains reachable.
  // The process registry releases capacity on exit and cannot own this history.
  const sessions = writable<AgentTerminal[]>([]);
  let pending: LaunchRequest | null = null;
  let busy = false;

  /** Hands one launch to the panel for `fields.repoPath`; resolves once it is claimed and opened. */
  function enqueue(fields: Omit<LaunchRequest, "complete">, signal?: AbortSignal): Promise<void> {
    if (!fields.repoPath.trim()) return Promise.reject(new Error("Open a repository before starting a terminal session"));
    if (signal?.aborted) return Promise.reject(new Error(TERMINAL_SESSION_CANCELED));
    if (busy) return Promise.reject(new Error(TERMINAL_SESSION_PENDING));
    return new Promise<void>((resolve, reject) => {
      busy = true;
      let settled = false;
      const complete = (error?: string) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        signal?.removeEventListener("abort", abort);
        pending = null;
        busy = false;
        store.set(null);
        if (error) reject(new Error(error));
        else resolve();
      };
      const abort = () => complete(TERMINAL_SESSION_CANCELED);
      const timer = setTimeout(() => complete("The terminal did not open. Try again or copy the prompt."), 10000);
      signal?.addEventListener("abort", abort, { once: true });
      pending = { ...fields, complete };
      store.set(pending);
    });
  }

  return {
    subscribe: store.subscribe,
    sessions: { subscribe: sessions.subscribe },
    remember(session: AgentTerminal) {
      sessions.update(rows => [...rows.filter(row => row.id !== session.id), session].slice(-16));
    },
    update(id: string, status: string) {
      sessions.update(rows => rows.map(row => row.id === id ? { ...row, status } : row));
    },
    forget(id: string) {
      sessions.update(rows => rows.filter(row => row.id !== id));
    },
    async request(
      repoPath: string,
      launcher: SessionKind,
      prompt?: string,
      signal?: AbortSignal,
      options?: { autoWake?: boolean },
    ): Promise<void> {
      // Validates here, at the boundary, so a bad prompt is refused before a
      // panel is asked to open a tab for it. A promptless launch returns null
      // for every launcher, which is what makes "new shell" fit this channel.
      agentPromptArgs(launcher, prompt);
      return enqueue({ repoPath, launcher, prompt, ...(options?.autoWake ? { autoWake: true } : {}) }, signal);
    },
    /**
     * A browser session on `url` in `repoPath`'s dock: a session already on
     * that address is brought forward and reloaded, otherwise a tab opens on
     * it. The address is judged again by the backend, which only loads
     * loopback ones.
     */
    async requestBrowser(repoPath: string, url: string, signal?: AbortSignal): Promise<void> {
      if (!url.trim() || url.length > 2048) throw new Error("A browser session needs an address of at most 2048 characters");
      return enqueue({ repoPath, launcher: "browser", url: url.trim() }, signal);
    },
    /**
     * Opens or reuses a `TerminalFilePane` split on `filePath` in `repoPath`'s
     * terminal dock, optionally scrolling to 1-based `line` / `column`.
     */
    async requestFile(
      repoPath: string,
      filePath: string,
      line?: number | null,
      column?: number | null,
      signal?: AbortSignal,
    ): Promise<void> {
      const cleanPath = normalizePaneFilePath(filePath);
      if (!cleanPath) {
        throw new Error("A terminal file pane requires a valid repository-relative path");
      }
      return enqueue({ repoPath, launcher: "file", filePath: cleanPath, line: line ?? null, column: column ?? null }, signal);
    },
    /** Whether a launch is waiting to be claimed; a caller with its own queue waits for this. */
    get busy(): boolean {
      return busy;
    },
    /**
     * Claims the pending launch when it is for `repoPath`'s checkout, by
     * identity — the panel holds its tab's path, the caller whatever path it
     * had, and one checkout spelled two ways is still one.
     */
    take(repoPath: string, options: PathIdentityOptions = { caseInsensitive: isCaseInsensitiveFs() }): LaunchRequest | null {
      if (!pending || !sameRepo(pending.repoPath, repoPath, options)) return null;
      const request = pending;
      pending = null;
      store.set(null);
      return request;
    },
  };
}

export const terminalLaunchRequests = createTerminalLaunchRequests();

export const AGENT_KICKOFF_EVENT = "gitpulse-agent-kickoff";
const MAX_QUEUED_KICKOFF = 8;
const KICKOFF_WAIT_MS = 10_000;

export interface AgentKickoffRequest {
  repo: string;
  provider: PromptLauncher;
  prompt: string;
  autoWake: boolean;
}

export function parseAgentKickoffRequest(payload: unknown): AgentKickoffRequest | null {
  if (!payload || typeof payload !== "object") return null;
  const raw = payload as Record<string, unknown>;
  if (typeof raw.repo !== "string" || !raw.repo || raw.repo.length > 4096) return null;
  if (!isPromptLauncher(raw.provider)) return null;
  if (typeof raw.prompt !== "string" || !raw.prompt.trim()) return null;
  try {
    agentPromptArgs(raw.provider, raw.prompt);
  } catch {
    return null;
  }
  return {
    repo: raw.repo,
    provider: raw.provider,
    prompt: raw.prompt,
    autoWake: raw.auto_wake === true,
  };
}

/** Listens for unattended `gitpulse_kickoff_agent` (`spawn: "terminal"`) requests. */
export async function installAgentKickoffRequests(): Promise<UnlistenFn> {
  const [{ repoStore }, { toastStore }] = await Promise.all([
    import("../stores/repoStore"),
    import("../stores/toastStore"),
  ]);
  const waiting: AgentKickoffRequest[] = [];
  let draining = false;
  const sleep = (ms: number) => new Promise<void>((done) => setTimeout(done, ms));

  async function drain(): Promise<void> {
    if (draining) return;
    draining = true;
    try {
      for (let next = waiting.shift(); next; next = waiting.shift()) {
        try {
          let canonical: string | null = null;
          const opened = await repoStore.openRepo(next.repo, {
            activate: true,
            onReady: (path) => (canonical = path),
          });
          const targetRepo = opened ? (canonical ?? next.repo) : null;
          if (!targetRepo) throw new Error(`${next.repo} couldn't be opened`);
          repoStore.setTerminalOpen(true);
          for (let waited = 0; terminalLaunchRequests.busy; waited += 100) {
            if (waited >= KICKOFF_WAIT_MS) throw new Error(TERMINAL_SESSION_PENDING);
            await sleep(100);
          }
          await terminalLaunchRequests.request(targetRepo, next.provider, next.prompt, undefined, {
            autoWake: next.autoWake,
          });
        } catch (cause) {
          toastStore.error(`An agent kickoff session did not open: ${formatError(cause)}`);
        }
      }
    } finally {
      draining = false;
    }
  }

  return listen(AGENT_KICKOFF_EVENT, (event) => {
    const request = parseAgentKickoffRequest(event.payload);
    if (!request) return;
    if (waiting.length >= MAX_QUEUED_KICKOFF) {
      toastStore.error(
        `An agent kickoff session was not opened: ${MAX_QUEUED_KICKOFF} requests are already waiting.`,
      );
      return;
    }
    waiting.push(request);
    void drain();
  });
}
