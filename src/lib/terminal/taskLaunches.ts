import { get, writable } from "svelte/store";
import { identityKey, type PathIdentityOptions } from "../repos/paths";
import { MAX_LIVE_RUNS } from "../workbench/vocabulary";
import { isSessionId, type LauncherKind, type ResumeLaunch } from "./tabs";

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
 *
 * The same queue carries a resumed conversation (`resume` set): an ended
 * attempt's Claude Code session reopened in its checkout. It needs exactly
 * what a launch needs — wait for the right dock, match the checkout by
 * identity — and is keyed apart from the attempt's own terminal so neither
 * can absorb the other.
 *
 * And it carries a session a reloaded page left running (`attach` set),
 * shown again in its repository's dock. That is not a task at all, which is
 * why `provider` is any launcher here; it shares the queue for the same
 * reason a resume does.
 */
export interface TaskTerminalRequest {
  runId: string;
  repoPath: string;
  provider: LauncherKind;
  title: string;
  resume?: ResumeLaunch;
  attach?: { sessionId: string };
  /** See `TaskLaunch.startDir`: where below `repoPath` the work ran. */
  startDir?: string;
}
const pending = writable<TaskTerminalRequest[]>([]);
export const taskTerminalRequests = { subscribe: pending.subscribe };

function requestKey(request: TaskTerminalRequest): string {
  if (request.attach) return `attach:${request.attach.sessionId}`;
  return request.resume ? `resume:${request.resume.sessionId.toLowerCase()}` : `run:${request.runId}`;
}

export function enqueueTaskTerminal(request: TaskTerminalRequest): void {
  if (request.resume && (request.provider !== "claude" || !isSessionId(request.resume.sessionId))) {
    throw new Error("Only a Claude Code conversation with a valid session id can be resumed.");
  }
  if (request.attach && (request.resume || !/^[\w-]{1,128}$/.test(request.attach.sessionId))) {
    throw new Error("That terminal session cannot be shown.");
  }
  const current = get(pending);
  const key = requestKey(request);
  if (current.some((item) => requestKey(item) === key)) return;
  if (current.length >= MAX_LIVE_RUNS) {
    throw new Error(`${MAX_LIVE_RUNS} task terminals are already waiting to open. Open or cancel one of those attempts first.`);
  }
  pending.set([...current, request]);
}
/** Withdraws an attempt's own terminal request (not a resumed conversation). */
export function consumeTaskTerminal(runId: string): void { pending.update((items) => items.filter((item) => item.resume || item.attach || item.runId !== runId)); }
/** Removes exactly this request, whichever kind it is, once its tab exists. */
export function consumeTaskTerminalRequest(request: TaskTerminalRequest): void {
  const key = requestKey(request);
  pending.update((items) => items.filter((item) => requestKey(item) !== key));
}

/**
 * Open repository tabs that a queued request is waiting for, matched by
 * checkout identity exactly as `requestFor` matches them inside the panel —
 * so the dock hosts precisely the panels that will consume a request, and a
 * request no open tab can take hosts nothing.
 */
export function awaitedTabIds(
  tabs: readonly { id: string; path: string }[],
  requests: readonly TaskTerminalRequest[],
  options: PathIdentityOptions,
): Set<string> {
  const wanted = new Set<string>();
  if (!requests.length) return wanted;
  const keys = new Set(requests.map((request) => identityKey(request.repoPath, options)).filter(Boolean));
  for (const tab of tabs) {
    const key = identityKey(tab.path, options);
    if (key && keys.has(key)) wanted.add(tab.id);
  }
  return wanted;
}

/** The first request whose checkout is `repoPath`, by identity, or undefined. */
export function requestFor(
  requests: readonly TaskTerminalRequest[],
  repoPath: string | null,
  options: PathIdentityOptions,
): TaskTerminalRequest | undefined {
  const key = repoPath ? identityKey(repoPath, options) : "";
  return key ? requests.find((request) => identityKey(request.repoPath, options) === key) : undefined;
}
