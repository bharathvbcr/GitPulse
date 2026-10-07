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
/** Whether an attempt's own terminal request is still waiting for a tab. */
export function hasTaskTerminalRequest(runId: string): boolean {
  return get(pending).some((item) => !item.resume && !item.attach && item.runId === runId);
}
/** Removes exactly this request, whichever kind it is, once its tab exists. */
export function consumeTaskTerminalRequest(request: TaskTerminalRequest): void {
  const key = requestKey(request);
  pending.update((items) => items.filter((item) => requestKey(item) !== key));
}

/**
 * Run states in which an attempt's own terminal request can still be served:
 * a preparation not yet expired (the spawn claims it), or a claimed attempt
 * whose terminal a reader asked to show again (the spawn reconnects to it).
 */
function requestServable(run: { state: string; expires_at: number }, clock: number): boolean {
  if (run.state === "prepared") return run.expires_at * 1000 > clock;
  return run.state === "starting" || run.state === "running";
}

/**
 * Drops every attempt's own request whose run, as just read, can no longer
 * take a terminal — ended, cancelled, unresolved, or expired while it waited.
 *
 * Only a run that *appears* in `runs` is judged. A read is scoped to one task
 * and paged, so a run missing from it is not evidence of anything: it may be
 * another task's, or have been prepared after the read began. Resumed
 * conversations and reattachments are keyed apart and belong to ended runs by
 * design, so they are never pruned here. Returns the run ids withdrawn.
 */
export function pruneTaskTerminals(runs: readonly { id: string; state: string; expires_at: number }[], clock: number): string[] {
  const dead = new Set(runs.filter((run) => !requestServable(run, clock)).map((run) => run.id));
  if (!dead.size) return [];
  const dropped: string[] = [];
  pending.update((items) => items.filter((item) => {
    if (item.resume || item.attach || !dead.has(item.runId)) return true;
    dropped.push(item.runId);
    return false;
  }));
  // A notice that described a start in progress describes nothing now. A
  // failure stays: it is the only record of why the attempt never ran.
  notices.update((map) => {
    let next: Map<string, AttemptNotice> | null = null;
    for (const runId of dead) {
      const notice = map.get(runId);
      if (notice && notice.tone !== "error") (next ??= new Map(map)).delete(runId);
    }
    return next ?? map;
  });
  return dropped;
}

// ---- What happened to an accepted attempt's start, per run --------------

/**
 * One line about an accepted attempt's start, for the row that shows it.
 *
 * Everything after the store accepts a preparation used to be reported by the
 * form that launched it — and only while that form was mounted, inside a
 * panel that had already folded it away. A failure from the tab that spawns
 * the agent was reported only inside that hidden tab. So the facts have one
 * home, keyed by run, that outlives every component: the launch owner
 * (`taskTerminal.ts::startPreparedAttempt`) and the attempt's terminal
 * session write it; the Agents pane reads it through `monitorAttempt`.
 */
export interface AttemptNotice {
  runId: string;
  /**
   * `starting`: the agent's process is being started. `waiting`: its
   * checkout has not opened yet. `running`: the process started. `failed`:
   * it will not start without the reader (no checkout, a refused open, a
   * spawn the host refused, a managed start that failed).
   */
  phase: "starting" | "waiting" | "running" | "failed";
  tone: "progress" | "ok" | "error";
  text: string;
  at: number;
}

/** Bounded like the queue: there are never more accepted attempts than this. */
const MAX_NOTICES = MAX_LIVE_RUNS * 2;
const notices = writable<ReadonlyMap<string, AttemptNotice>>(new Map());
export const attemptNotices = { subscribe: notices.subscribe };

const TONES: Record<AttemptNotice["phase"], AttemptNotice["tone"]> = {
  starting: "progress", waiting: "progress", running: "ok", failed: "error",
};

/** Records the latest fact about one run's start, replacing the previous one. */
export function noteAttempt(runId: string, phase: AttemptNotice["phase"], text: string, at: number = Date.now()): void {
  if (!runId) return;
  const bounded = text.length > 400 ? `${text.slice(0, 399)}…` : text;
  notices.update((map) => {
    const next = new Map(map);
    next.delete(runId);
    next.set(runId, { runId, phase, tone: TONES[phase], text: bounded, at });
    // Oldest first by insertion; evict from the front.
    while (next.size > MAX_NOTICES) next.delete(next.keys().next().value as string);
    return next;
  });
}

export function clearAttemptNotice(runId: string): void {
  notices.update((map) => {
    if (!map.has(runId)) return map;
    const next = new Map(map);
    next.delete(runId);
    return next;
  });
}

/**
 * Open repository tabs that a queued request is waiting for, matched by
 * checkout identity exactly as `requestFor` matches them inside the panel —
 * so the dock hosts precisely the panels that will consume a request, and a
 * request no open tab can take hosts nothing.
 *
 * A tab whose repository is not trusted yet hosts nothing either. A launch
 * opens its checkout without asking (`deferTrust`), so the first sign of an
 * untrusted checkout is a tab that says so — and hosting it would start the
 * agent in a repository nobody has agreed to. The request waits until the
 * reader trusts it, and the Agents pane says that is what it waits for.
 */
export function awaitedTabIds(
  tabs: readonly { id: string; path: string; trustRequired?: boolean }[],
  requests: readonly TaskTerminalRequest[],
  options: PathIdentityOptions,
): Set<string> {
  const wanted = new Set<string>();
  if (!requests.length) return wanted;
  const keys = new Set(requests.map((request) => identityKey(request.repoPath, options)).filter(Boolean));
  for (const tab of tabs) {
    if (tab.trustRequired) continue;
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
