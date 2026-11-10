import { writable } from "svelte/store";
import { isSessionId, type LauncherKind, type SessionKind } from "./tabs";
import { AGENT_PROMPT_MAX_BYTES } from "./agentPromptText";
import { isCaseInsensitiveFs, sameRepo, type PathIdentityOptions } from "../repos/paths";

export type PromptLauncher = Extract<LauncherKind, "claude" | "codex" | "grok" | "agy">;

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
 * The arguments that pick a Claude Code conversation back up, optionally with a
 * first prompt: `claude --resume <id> -- <prompt>`. `claude [options] [prompt]`
 * takes the prompt positionally (`claude --help`), and `--` is the separator
 * `agentPromptArgs` already uses for every initial prompt, so the text is one
 * argument whatever it looks like — a flag, a shell word, a settings object.
 * Nothing here goes through a shell. The prompt is held to the same ceiling as
 * an initial prompt; `sessionId` must be the canonical form Claude Code takes.
 */
export function resumeArgs(sessionId: string, prompt?: string): string[] {
  if (!isSessionId(sessionId)) throw new Error("Only a Claude Code conversation with a valid session id can be resumed.");
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
  launcher: SessionKind;
  /** Absent for a plain session; a literal CLI argument when present. */
  prompt?: string;
  /** A browser session's first address (`requestBrowser`); absent otherwise. */
  url?: string;
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
    if (signal?.aborted) return Promise.reject(new Error("Terminal launch cancelled"));
    if (busy) return Promise.reject(new Error("A terminal launch is already pending"));
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
      const abort = () => complete("Terminal launch cancelled");
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
    async request(repoPath: string, launcher: SessionKind, prompt?: string, signal?: AbortSignal): Promise<void> {
      // Validates here, at the boundary, so a bad prompt is refused before a
      // panel is asked to open a tab for it. A promptless launch returns null
      // for every launcher, which is what makes "new shell" fit this channel.
      agentPromptArgs(launcher, prompt);
      return enqueue({ repoPath, launcher, prompt }, signal);
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
