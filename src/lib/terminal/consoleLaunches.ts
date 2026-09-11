import { get, writable } from "svelte/store";

/**
 * Bounded handoff from Setup / Settings into the Console tab.
 *
 * `cmd_terminal_run` still needs an open repository (cwd + git identity).
 * The wizard disables Run when none is open and Copy still works.
 */
export interface ConsoleLaunchRequest {
  command: string;
  label: string;
  timeoutSecs?: number;
}

const pending = writable<ConsoleLaunchRequest[]>([]);

export const consoleLaunchRequests = { subscribe: pending.subscribe };

export function enqueueConsoleLaunch(request: ConsoleLaunchRequest): void {
  const command = request.command.trim();
  if (!command) throw new Error("Install command is empty");
  if (command.length > 8192) {
    throw new Error("Install command exceeds the 8192-character console bound");
  }
  const current = get(pending);
  if (current.some((item) => item.command === command)) return;
  if (current.length >= 2) {
    throw new Error("Two console commands are waiting. Let one finish, or cancel it.");
  }
  pending.set([...current, { ...request, command }]);
}

export function consumeConsoleLaunch(): ConsoleLaunchRequest | null {
  const current = get(pending);
  if (current.length === 0) return null;
  const [head, ...rest] = current;
  pending.set(rest);
  return head;
}
