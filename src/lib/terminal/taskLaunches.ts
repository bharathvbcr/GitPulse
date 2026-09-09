import { get, writable } from "svelte/store";

export interface TaskTerminalRequest { runId: string; repoPath: string; provider: "claude" | "codex"; title: string }
const pending = writable<TaskTerminalRequest[]>([]);
export const taskTerminalRequests = { subscribe: pending.subscribe };

export function enqueueTaskTerminal(request: TaskTerminalRequest): void {
  const current = get(pending);
  if (current.some((item) => item.runId === request.runId)) return;
  if (current.length >= 2) throw new Error("Two task terminals are waiting to open. Open or cancel an existing attempt first.");
  pending.set([...current, request]);
}
export function consumeTaskTerminal(runId: string): void { pending.update((items) => items.filter((item) => item.runId !== runId)); }
