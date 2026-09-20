import { writable } from "svelte/store";
import type { LauncherKind } from "./tabs";

export type PromptLauncher = Extract<LauncherKind, "claude" | "codex">;

/** Both interactive CLIs accept a positional prompt. No shell interpolation. */
export function agentPromptArgs(launcher: LauncherKind, prompt?: string): string[] | null {
  if (prompt === undefined) return null;
  if (launcher !== "claude" && launcher !== "codex") {
    throw new Error("Initial prompts require Claude Code or Codex");
  }
  if (!prompt.trim() || prompt.includes("\0") || new TextEncoder().encode(prompt).length > 16000) {
    throw new Error("Agent prompt must be nonempty, contain no NUL, and fit within 16000 bytes");
  }
  return ["--", prompt];
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
  launcher: LauncherKind;
  /** Absent for a plain session; a literal CLI argument when present. */
  prompt?: string;
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
    async request(repoPath: string, launcher: LauncherKind, prompt?: string, signal?: AbortSignal): Promise<void> {
      if (!repoPath.trim()) throw new Error("Open a repository before starting a terminal session");
      // Validates here, at the boundary, so a bad prompt is refused before a
      // panel is asked to open a tab for it. A promptless launch returns null
      // for every launcher, which is what makes "new shell" fit this channel.
      agentPromptArgs(launcher, prompt);
      if (signal?.aborted) throw new Error("Terminal launch cancelled");
      if (busy) throw new Error("A terminal launch is already pending");
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
        pending = { repoPath, launcher, prompt, complete };
        store.set(pending);
      });
    },
    take(repoPath: string): LaunchRequest | null {
      if (pending?.repoPath !== repoPath) return null;
      const request = pending;
      pending = null;
      store.set(null);
      return request;
    },
  };
}

export const terminalLaunchRequests = createTerminalLaunchRequests();
