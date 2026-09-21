import { writable } from "svelte/store";
import type { LauncherKind } from "./tabs";

export type PromptLauncher = Extract<LauncherKind, "claude" | "codex" | "grok" | "agy">;

export const PROMPT_LAUNCHERS: readonly PromptLauncher[] = ["claude", "codex", "grok", "agy"];

export function isPromptLauncher(value: unknown): value is PromptLauncher {
  return PROMPT_LAUNCHERS.some((launcher) => launcher === value);
}

/**
 * The arguments that make an agent CLI speak to this terminal.
 *
 * Neither Claude Code nor Codex says anything here by default. Claude Code
 * sends a desktop notification only in Ghostty, kitty and iTerm2 and is
 * otherwise silent unless `preferredNotifChannel` is set; Codex probes for a
 * terminal it recognises and falls back to nothing it can be sure of. So an
 * agent working in a GitPulse tab used to stop on a permission prompt with no
 * sign at all, which is the whole defect.
 *
 * Each flag below is that CLI's own documented, **session-scoped** override:
 *
 * * `claude --settings '<json>'` sits above the user's files and below managed
 *   settings, merges key by key — a key not named here keeps its value from
 *   wherever it was set — lasts one session and writes no file.
 * * `codex -c key=value` is parsed as TOML and applies to that invocation.
 *   The inner quotes are part of the TOML, not the shell: these are argv
 *   entries and nothing expands them.
 *
 * `notification_condition = "always"` is deliberate. Codex's default is to
 * notify only when it believes the terminal is unfocused, a judgement it can
 * only make from escape sequences the tab may not send. GitPulse knows the
 * answer exactly — it knows which tab is on screen and whether its own window
 * has focus — so the CLI is asked to always report and GitPulse decides.
 *
 * Launchers absent from this map get nothing. Manvi, Grok and Antigravity
 * publish no notification setting this code has read, and inventing a flag for
 * them would at best be ignored and at worst refuse to start. They still reach
 * the user if they emit a bell or an OSC notification of their own accord,
 * because the detection side does not depend on any of this.
 */
export const AGENT_NOTIFY_ARGS: Partial<Record<LauncherKind, readonly string[]>> = {
  claude: ["--settings", JSON.stringify({ preferredNotifChannel: "terminal_bell" })],
  codex: [
    "-c",
    "tui.notifications=true",
    "-c",
    'tui.notification_method="osc9"',
    "-c",
    'tui.notification_condition="always"',
  ],
};

export function agentNotifyArgs(launcher: LauncherKind, enabled: boolean): string[] {
  if (!enabled) return [];
  return [...(AGENT_NOTIFY_ARGS[launcher] ?? [])];
}

/** Interactive CLIs that accept an initial prompt. No shell interpolation. */
export function agentPromptArgs(launcher: LauncherKind, prompt?: string): string[] | null {
  if (prompt === undefined) return null;
  if (!isPromptLauncher(launcher)) {
    throw new Error("Initial prompts require Claude Code, Codex, Grok, or Antigravity");
  }
  if (!prompt.trim() || prompt.includes("\0") || new TextEncoder().encode(prompt).length > 16000) {
    throw new Error("Agent prompt must be nonempty, contain no NUL, and fit within 16000 bytes");
  }
  // Antigravity reads prompts only from `--prompt-interactive` / `--print`, never positionally.
  if (launcher === "agy") return ["--prompt-interactive", prompt];
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
