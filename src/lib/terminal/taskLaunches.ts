import { get, writable } from "svelte/store";
import { identityKey, type PathIdentityOptions } from "../repos/paths";
import { MAX_LIVE_RUNS, type AgentProvider } from "../workbench/vocabulary";

/**
 * Prepared task attempts waiting for their repository's terminal dock.
 *
 * A request is queued *before* its checkout is opened, not from the open's
 * ready callback. Opening a repository is cancelled by any later navigation,
 * so launching a second task — in another repository, or just clicking a file
 * — while the first was still opening used to drop the first terminal on the
 * floor: its callback never ran, the launch threw, and its attempt sat
 * prepared and holding its checkout. Queued first, the request simply waits
 * until that repository's dock mounts, and the dock matches it by checkout
 * identity rather than by the exact string, because the open may resolve the
 * same checkout to a differently spelled path.
 *
 * Bounded by the store's own capacity: there can never be more prepared
 * attempts than that, so a smaller bound only refused real work.
 */
export interface TaskTerminalRequest { runId: string; repoPath: string; provider: AgentProvider; title: string }
const pending = writable<TaskTerminalRequest[]>([]);
export const taskTerminalRequests = { subscribe: pending.subscribe };

export function enqueueTaskTerminal(request: TaskTerminalRequest): void {
  const current = get(pending);
  if (current.some((item) => item.runId === request.runId)) return;
  if (current.length >= MAX_LIVE_RUNS) {
    throw new Error(`${MAX_LIVE_RUNS} task terminals are already waiting to open. Open or cancel one of those attempts first.`);
  }
  pending.set([...current, request]);
}
export function consumeTaskTerminal(runId: string): void { pending.update((items) => items.filter((item) => item.runId !== runId)); }

/** The first request whose checkout is `repoPath`, by identity, or undefined. */
export function requestFor(
  requests: readonly TaskTerminalRequest[],
  repoPath: string | null,
  options: PathIdentityOptions,
): TaskTerminalRequest | undefined {
  const key = repoPath ? identityKey(repoPath, options) : "";
  return key ? requests.find((request) => identityKey(request.repoPath, options) === key) : undefined;
}
