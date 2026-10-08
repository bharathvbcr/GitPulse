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
  /**
   * The repository family of `repoPath` (`repoStore.familyOf`), when known.
   * Lets another open checkout of the same repository host the terminal, so
   * an agent in a fresh worktree does not need a repository tab of its own
   * (see `hostTabFor`).
   */
  family?: string;
}

/** What hosting needs to know about an open repository tab. */
export interface HostCandidate {
  id: string;
  path: string;
  trustRequired?: boolean;
  missing?: boolean;
  family?: string | null;
  familyRoot?: string | null;
}

/**
 * The open repository tab whose dock takes `request`, or undefined.
 *
 * The request's own checkout when that is open — trusted or not, so an
 * untrusted one still waits for trust rather than slipping into a sibling.
 * Otherwise, when the request names its family, a trusted checkout of the
 * same repository that is still on disk: the repository's own directory
 * first, then the first in tab order. A process's working directory is the
 * attempt's own (the host spawns it there), so the hosting tab decides only
 * where the panel lives, never where the agent runs.
 *
 * This used to be "the checkout's own tab, always", and every agent launched
 * into a new worktree opened a repository tab for it. Those tabs counted
 * against the reader's own bound, which was then 24 open repositories (and
 * a native watcher cap of the same size), so a reader with fourteen
 * repositories open could start about ten worktree agents, and every later
 * launch was refused with "Too many open repositories" while the live-run
 * and session limits stood mostly unused. Each such tab also paid a full
 * hydrate and an index build. However large the tab bound is, an agent is
 * not a repository the reader opened and should not spend one.
 *
 * Pure and deterministic over the same tab list, so the dock (which panels to
 * host) and the panel (which request to take) always agree.
 */
export function hostTabFor<T extends HostCandidate>(
  request: Pick<TaskTerminalRequest, "repoPath" | "family">,
  tabs: readonly T[],
  options: PathIdentityOptions,
): T | undefined {
  const key = identityKey(request.repoPath, options);
  if (!key) return undefined;
  const own = tabs.find((tab) => identityKey(tab.path, options) === key);
  if (own) return own;
  if (!request.family) return undefined;
  const kin = tabs.filter((tab) => tab.family === request.family && !tab.trustRequired && !tab.missing);
  return kin.find((tab) => !!tab.familyRoot && identityKey(tab.path, options) === identityKey(tab.familyRoot, options)) ?? kin[0];
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
/**
 * Which panel holds a queued request: one that opened a tab for it that does
 * not hold a session slot yet. Keyed like the queue.
 *
 * The host is recomputed from the open tabs (`hostTabFor`), so it can move
 * while a start waits for its slot — the reader reorders tabs, trusts the
 * repository's own checkout, or opens the worktree from a file link. Without
 * a hold the new host opened a second tab for the same request and launched
 * the same attempt twice, and the second launch failed as already claimed.
 */
const holds = writable<ReadonlyMap<string, symbol>>(new Map());
export const taskTerminalHolds = { subscribe: holds.subscribe };

/** Whether another panel than `holder` holds `request`. */
export function heldElsewhere(request: TaskTerminalRequest, holder: symbol, held: ReadonlyMap<string, symbol> = get(holds)): boolean {
  const by = held.get(requestKey(request));
  return by !== undefined && by !== holder;
}

/** Records that `holder` opened a tab for `request`. False when another holds it. */
export function holdTaskTerminal(request: TaskTerminalRequest, holder: symbol): boolean {
  if (heldElsewhere(request, holder)) return false;
  const key = requestKey(request);
  holds.update((map) => (map.get(key) === holder ? map : new Map(map).set(key, holder)));
  return true;
}

/** Gives up `holder`'s hold on `request` (or every one it has, with none named). */
export function releaseTaskTerminal(holder: symbol, request?: TaskTerminalRequest): void {
  const key = request ? requestKey(request) : null;
  holds.update((map) => {
    let next: Map<string, symbol> | null = null;
    for (const [held, by] of map) {
      if (by === holder && (key === null || held === key)) (next ??= new Map(map)).delete(held);
    }
    return next ?? map;
  });
}

function forgetHolds(keep: (key: string) => boolean): void {
  holds.update((map) => {
    let next: Map<string, symbol> | null = null;
    for (const key of map.keys()) if (!keep(key)) (next ??= new Map(map)).delete(key);
    return next ?? map;
  });
}

/** Withdraws an attempt's own terminal request (not a resumed conversation). */
export function consumeTaskTerminal(runId: string): void {
  pending.update((items) => items.filter((item) => item.resume || item.attach || item.runId !== runId));
  forgetHolds((key) => key !== `run:${runId}`);
}
/** Whether an attempt's own terminal request is still waiting for a tab. */
export function hasTaskTerminalRequest(runId: string): boolean {
  return get(pending).some((item) => !item.resume && !item.attach && item.runId === runId);
}
/** Removes exactly this request, whichever kind it is, once its tab exists. */
export function consumeTaskTerminalRequest(request: TaskTerminalRequest): void {
  const key = requestKey(request);
  pending.update((items) => items.filter((item) => requestKey(item) !== key));
  forgetHolds((held) => held !== key);
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
  if (dropped.length) forgetHolds((key) => !dropped.some((runId) => key === `run:${runId}`));
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
 * Open repository tabs that a queued request is waiting for — each request's
 * `hostTabFor`, exactly as `requestFor` picks inside the panel — so the dock
 * hosts precisely the panels that will consume a request, and a request no
 * open tab can take hosts nothing.
 *
 * A tab whose repository is not trusted yet hosts nothing either. A launch
 * opens its checkout without asking (`deferTrust`), so the first sign of an
 * untrusted checkout is a tab that says so — and hosting it would start the
 * agent in a repository nobody has agreed to. The request waits until the
 * reader trusts it, and the Agents pane says that is what it waits for.
 */
export function awaitedTabIds(
  tabs: readonly HostCandidate[],
  requests: readonly TaskTerminalRequest[],
  options: PathIdentityOptions,
): Set<string> {
  const wanted = new Set<string>();
  for (const request of requests) {
    const host = hostTabFor(request, tabs, options);
    if (host && !host.trustRequired) wanted.add(host.id);
  }
  return wanted;
}

/**
 * The first request the panel for `repoPath` takes, or undefined: one whose
 * `hostTabFor` among `tabs` is this checkout. Without `tabs` (or with the
 * request's own checkout not among them and no family) that is the request
 * whose checkout is `repoPath`, by identity.
 */
export function requestFor(
  requests: readonly TaskTerminalRequest[],
  repoPath: string | null,
  options: PathIdentityOptions,
  tabs: readonly HostCandidate[] = [],
): TaskTerminalRequest | undefined {
  const key = repoPath ? identityKey(repoPath, options) : "";
  if (!key) return undefined;
  return requests.find((request) => identityKey(hostTabFor(request, tabs, options)?.path ?? request.repoPath, options) === key);
}

/**
 * The launch a panel opens for a request it took: the request itself, plus
 * the checkout to run in when the panel belongs to another checkout of the
 * same repository. A process started in the panel's own directory would run
 * in the wrong worktree.
 */
export function launchFor(
  request: TaskTerminalRequest,
  panelPath: string,
  options: PathIdentityOptions,
): TaskTerminalRequest & { checkout?: string } {
  return identityKey(request.repoPath, options) === identityKey(panelPath, options)
    ? request
    : { ...request, checkout: request.repoPath };
}
