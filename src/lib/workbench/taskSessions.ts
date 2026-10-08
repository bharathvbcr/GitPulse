/**
 * What a task attempt's terminal is doing, as its task's Agents pane shows it.
 *
 * A launch no longer moves the reader to the terminal (see `taskTerminal.ts`),
 * so the pane is where they learn whether the agent they just started is
 * starting, running, or waiting — and the store cannot tell them. The store
 * knows the attempt (prepared, running, exited); only the renderer knows its
 * session (is a tab running it in this window, is it still waiting for a
 * slot). Those are two different facts, and this joins them without letting
 * either stand in for the other: an attempt the store calls running with no
 * session here is "running outside this window", never "running here".
 *
 * Pure, so the join is testable without a PTY.
 */

import { isAdoptedSession } from "../terminal/detachedSessions";
import type { TerminalSessionRecord } from "../terminal/sessionRegistry";
import type { AttemptNotice, TaskTerminalRequest } from "../terminal/taskLaunches";
import { asksForReader as attentionAsks, type SessionActivity } from "../terminal/sessionActivity";
import type { OpenRepoTab } from "../stores/repoStore";
import { identityKey, pathSegments, type PathIdentityOptions } from "../repos/paths";
import { runExpired, runHoldsCheckout } from "./taskHandoff";
import type { TaskRun } from "./client";

/** One session in this window that belongs to an attempt. */
export interface AttemptSession {
  record: TerminalSessionRecord;
  /** The attempt's own process, or a conversation resumed from it. */
  role: "attempt" | "resumed";
  phase: "starting" | "running" | "adopted" | "problem";
  /** Short status for the row: what the session says, in plain words. */
  label: string;
}

export interface AttemptTerminalView {
  /** The attempt's own session first, then resumed conversations. */
  sessions: AttemptSession[];
  /**
   * A request for this attempt that no session has taken yet, and why it is
   * still waiting. Null when nothing is waiting.
   */
  waiting: {
    role: "attempt" | "resumed";
    /**
     * `trust`: its checkout opened as a repository nobody has trusted, and
     * the dock will not start an agent there until someone does. `capacity`:
     * every terminal slot is taken. `checkout`: its checkout is not open yet.
     */
    reason: "capacity" | "checkout" | "trust";
    /** The checkout the request waits for, as queued. */
    checkout: string;
  } | null;
}

const STARTING = "starting";
const RUNNING = "running";

function phaseOf(record: TerminalSessionRecord): Pick<AttemptSession, "phase" | "label"> {
  if (isAdoptedSession(record)) return { phase: "adopted", label: "Running · not shown since the window reloaded" };
  if (record.status === STARTING) return { phase: "starting", label: "Starting…" };
  if (record.status === RUNNING) return { phase: "running", label: "Running in this window" };
  // A live record whose status is neither is carrying a message: a pending
  // reconnect, a failed close. Shown as it was said, bounded.
  const text = record.status.trim() || "Unknown terminal state";
  return { phase: "problem", label: text.length > 160 ? `${text.slice(0, 159)}…` : text };
}

/**
 * The sessions and waiting request for one attempt.
 *
 * `capacityFull` is whether every terminal slot is taken: then a waiting
 * request is waiting for a slot, and saying "opening the checkout" would send
 * the reader looking in the wrong place.
 */
export function attemptTerminalView(
  runId: string,
  records: readonly TerminalSessionRecord[],
  requests: readonly TaskTerminalRequest[],
  capacityFull: boolean,
  /** Whether the checkout at this path is open but waiting to be trusted. */
  trustPending: (repoPath: string) => boolean = () => false,
): AttemptTerminalView {
  if (!runId) return { sessions: [], waiting: null };
  const sessions: AttemptSession[] = [];
  for (const record of records) {
    const role = record.taskRunId === runId ? "attempt" : record.continuesRunId === runId ? "resumed" : null;
    if (!role) continue;
    sessions.push({ record, role, ...phaseOf(record) });
  }
  sessions.sort((a, b) => (a.role === b.role ? 0 : a.role === "attempt" ? -1 : 1));
  let waiting: AttemptTerminalView["waiting"] = null;
  for (const request of requests) {
    if (request.runId !== runId || request.attach) continue;
    const role = request.resume ? "resumed" : "attempt";
    // A request whose session already exists is about to be consumed; it is
    // not waiting for anything the reader can act on.
    const served = sessions.some((session) => session.role === role);
    if (served) continue;
    // Trust first: no free slot serves a checkout the dock will not host.
    const reason = trustPending(request.repoPath) ? "trust" : capacityFull ? "capacity" : "checkout";
    waiting = { role, reason, checkout: request.repoPath };
    if (role === "attempt") break;
  }
  return { sessions, waiting };
}

function lastSegment(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

/** The sentence for a waiting request: what it waits for, then what to do. */
export function waitingLabel(waiting: NonNullable<AttemptTerminalView["waiting"]>, limit: number): string {
  const lead = waiting.role === "attempt" ? "" : "Resumed conversation · ";
  const name = lastSegment(waiting.checkout);
  switch (waiting.reason) {
    case "capacity": return `${lead}Waiting for a terminal slot — all ${limit} are in use. Close one, or raise the limit in Settings → Agents.`;
    case "trust": return `${lead}Waiting for you to trust ${name}. It opened without asking; trust it and the agent starts.`;
    default: return `${lead}Waiting for ${name} to open.`;
  }
}

// ---- At a glance ----------------------------------------------------------

/**
 * One session's line in the pane, most urgent first by `GLANCE_RANK`.
 *
 * `needs-you`, `error`, `finished` and `signalled` come from what the agent
 * itself announced; `active` and `quiet` only from when it last printed, and
 * say exactly that — never "working" or "stuck", which output cannot tell.
 */
export type GlanceTone = "needs-you" | "error" | "signalled" | "finished" | "problem" | "starting" | "active" | "quiet" | "adopted";

export const GLANCE_RANK: Record<GlanceTone, number> = {
  "needs-you": 0, error: 1, signalled: 2, finished: 3, problem: 4, starting: 5, active: 6, quiet: 7, adopted: 8,
};

/** Output within this long reads as "output Ns ago" rather than "quiet". */
export const ACTIVE_WINDOW_MS = 10_000;

export interface AgentGlance {
  tone: GlanceTone;
  /** The state, in a few words. */
  headline: string;
  /** What the agent said with it, when it said anything. */
  detail: string | null;
  /** What the program calls itself right now (OSC title), verbatim. */
  title: string | null;
}

/** "4s", "12m", "2h 5m": a span for a status line. */
export function shortSpan(ms: number): string {
  const seconds = Math.max(0, Math.floor((Number.isFinite(ms) ? ms : 0) / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  return minutes % 60 ? `${hours}h ${minutes % 60}m` : `${hours}h`;
}

export function agentGlance(session: AttemptSession, activity: SessionActivity | undefined, now: number): AgentGlance {
  const title = activity?.title ?? null;
  if (session.phase === "starting") return { tone: "starting", headline: "Starting…", detail: null, title };
  // A problem's label is itself what the reader must act on, so it stays.
  if (session.phase === "problem") return { tone: "problem", headline: session.label, detail: null, title };
  const attention = activity?.attention;
  if (attention) {
    // An adopted session is the one the reader cannot see, so what it asked
    // matters most there; the line keeps saying why it is not on screen.
    const where = session.phase === "adopted" ? " · not shown since the window reloaded" : "";
    return { tone: attention.kind, headline: `${attention.label} · ${shortSpan(now - attention.at)} ago${where}`, detail: attention.detail, title };
  }
  if (session.phase === "adopted") return { tone: "adopted", headline: session.label, detail: null, title };
  const last = activity?.lastOutputAt ?? null;
  if (last === null) return { tone: "quiet", headline: "Running · no output yet", detail: null, title };
  const since = now - last;
  return since < ACTIVE_WINDOW_MS
    ? { tone: "active", headline: since < 1000 ? "Output just now" : `Output ${shortSpan(since)} ago`, detail: null, title }
    : { tone: "quiet", headline: `Quiet for ${shortSpan(since)}`, detail: null, title };
}

/** The most urgent glance among an attempt's sessions, or null with none. */
export function mostUrgent(glances: readonly AgentGlance[]): AgentGlance | null {
  let best: AgentGlance | null = null;
  for (const glance of glances) if (!best || GLANCE_RANK[glance.tone] < GLANCE_RANK[best.tone]) best = glance;
  return best;
}

/**
 * How urgent one attempt is, for ordering the pane and badging its tab.
 *
 * Its most urgent session decides, except that a managed attempt's pending
 * requests are themselves "needs you", a start still waiting counts as
 * starting, and an attempt the store calls running with no session connected
 * here is a problem the reader can act on. Null tone: nothing to say.
 */
export function attemptUrgency(
  glances: readonly AgentGlance[],
  extra: { pendingRequests?: number; waiting?: boolean; disconnected?: boolean; failed?: boolean } = {},
): GlanceTone | null {
  const tones: GlanceTone[] = glances.map((glance) => glance.tone);
  if ((extra.pendingRequests ?? 0) > 0) tones.push("needs-you");
  if (extra.failed) tones.push("error");
  if (extra.disconnected) tones.push("problem");
  if (extra.waiting) tones.push("starting");
  let best: GlanceTone | null = null;
  for (const tone of tones) if (best === null || GLANCE_RANK[tone] < GLANCE_RANK[best]) best = tone;
  return best;
}

/**
 * Tones that put an attempt on the reader's plate: the agent's attention
 * asks for the reader (`sessionActivity.asksForReader`, the rule the board
 * and the tray count by) — a bell included, since it is how an agent with no
 * hooks says it is blocked.
 */
export function asksForReader(tone: GlanceTone | null): boolean {
  return (tone === "needs-you" || tone === "error" || tone === "signalled" || tone === "finished") && attentionAsks(tone);
}

// ---- One attempt, as every surface sees it --------------------------------

/** What this window knows beside the store, for monitoring attempts. */
export interface MonitorContext {
  records: readonly TerminalSessionRecord[];
  requests: readonly TaskTerminalRequest[];
  /** Every terminal slot is taken (a waiting request waits for one). */
  capacityFull: boolean;
  /** The activity of a native session, when this window has seen any. */
  activity: (sessionId: string) => SessionActivity | undefined;
  /** A managed attempt's pending requests, when they were read. */
  pending: (runId: string) => PendingRequests | undefined;
  /** For output recency. */
  now: number;
  /** For "still holds its checkout"; the instant the runs were read against. */
  clock: number;
  /**
   * What this window recorded about the attempt's start, when it recorded
   * anything (`taskLaunches.ts::attemptNotices`). Optional: a surface that
   * does not pass it simply shows no start failures.
   */
  notices?: (runId: string) => AttemptNotice | undefined;
  /** Whether the checkout at a path is open but waiting to be trusted. */
  trustPending?: (repoPath: string) => boolean;
}

export interface MonitoredAttempt {
  run: TaskRun;
  view: AttemptTerminalView;
  glances: AgentGlance[];
  tone: GlanceTone | null;
  /** The store says it runs in a terminal; no session here is attached. */
  disconnected: boolean;
  /** Prepared for a terminal that nothing in this window is starting. */
  unstarted: boolean;
  /**
   * Why the attempt's process did not start, when this window saw it fail
   * and nothing has started since: a checkout that would not open, a spawn
   * the host refused, a managed start that failed. Null otherwise.
   */
  failure: string | null;
}

/**
 * One attempt's sessions, their glances, and how urgent the attempt is. The
 * task's Agents pane and the board's cards both read this, so the row a
 * reader sees first and the card that sent them there say the same thing.
 */
export function monitorAttempt(run: TaskRun, context: MonitorContext): MonitoredAttempt {
  const view = attemptTerminalView(run.id, context.records, context.requests, context.capacityFull, context.trustPending);
  const own = view.sessions.some((session) => session.role === "attempt");
  const terminal = run.kind === "external_terminal";
  // A failure stands only while nothing has replaced it: no session of its
  // own here, no request waiting, and a run the store has not seen claimed.
  // A claimed run whose start reply this window lost is running somewhere —
  // that is "not connected here", and Show terminal reconnects it.
  const notice = context.notices?.(run.id);
  const unclaimed = run.state === "prepared" || (run.kind === "managed" && run.state === "starting");
  const failure = notice?.phase === "failed" && unclaimed && !own && view.waiting?.role !== "attempt" ? notice.text : null;
  const disconnected = terminal && !own && !view.waiting && !failure && ["starting", "running"].includes(run.state);
  const unstarted = terminal && !own && !view.waiting && !failure && run.state === "prepared" && !runExpired(run, context.clock);
  const glances = view.sessions.map((session) =>
    agentGlance(session, session.record.sessionId ? context.activity(session.record.sessionId) : undefined, context.now));
  const tone = runHoldsCheckout(run, context.clock)
    ? attemptUrgency(glances, { pendingRequests: context.pending(run.id)?.count ?? 0, waiting: view.waiting !== null, disconnected, failed: failure !== null })
    : attemptUrgency(glances);
  return { run, view, glances, tone, disconnected, unstarted, failure };
}

/**
 * One attempt's place on the ladder every row reads the same way:
 * Preparing worktree → Starting agent → Running → Needs you → Exited /
 * Failed / Expired. (The host's worktree setup runs inside the one
 * preparation call, so it has no rung of its own: nothing reports it apart.)
 *
 * `tone` is the pill's colour: `live` for work in progress, `ask` for a
 * reader wanted, `bad` for something that will not proceed by itself,
 * `done` for an attempt that has ended.
 */
export interface AttemptStage {
  stage: "starting" | "waiting" | "running" | "needs-you" | "failed" | "expired" | "ended" | "unresolved" | "unstarted";
  label: string;
  tone: "live" | "ask" | "bad" | "done";
}

export function attemptStage(row: Pick<MonitoredAttempt, "run" | "view" | "tone" | "failure" | "unstarted" | "disconnected">, clock: number): AttemptStage {
  const { run } = row;
  if (row.failure) return { stage: "failed", label: "Failed to start", tone: "bad" };
  if (run.state === "prepared" && runExpired(run, clock)) return { stage: "expired", label: "Expired", tone: "done" };
  if (run.state === "unresolved") return { stage: "unresolved", label: "Unresolved", tone: "bad" };
  if (run.state === "failed") return { stage: "ended", label: "Failed", tone: "bad" };
  if (run.state === "cancelled") return { stage: "ended", label: "Cancelled", tone: "done" };
  if (run.state === "exited") return { stage: "ended", label: run.outcome_uncertain ? "Ended · outcome unknown" : "Exited", tone: "done" };
  if (asksForReader(row.tone)) return { stage: "needs-you", label: "Needs you", tone: "ask" };
  if (row.view.waiting?.role === "attempt") return { stage: "waiting", label: row.view.waiting.reason === "trust" ? "Needs trust" : "Starting agent", tone: row.view.waiting.reason === "trust" ? "ask" : "live" };
  if (row.unstarted) return { stage: "unstarted", label: "Not started", tone: "live" };
  if (run.state === "prepared" || run.state === "starting" || row.tone === "starting") return { stage: "starting", label: "Starting agent", tone: "live" };
  if (row.disconnected) return { stage: "running", label: "Running elsewhere", tone: "live" };
  return { stage: "running", label: "Running", tone: "live" };
}

/** Most urgent first; among equals, the newest attempt first. */
export function orderMonitored<T extends Pick<MonitoredAttempt, "run" | "tone">>(rows: readonly T[]): T[] {
  const rank = (tone: GlanceTone | null) => (tone === null ? 99 : GLANCE_RANK[tone]);
  return [...rows].sort((a, b) => rank(a.tone) - rank(b.tone) || b.run.created_at - a.run.created_at);
}

/** A task's working agents in one line: how many, how many need you, the most urgent. */
export interface TaskAgentSummary { working: number; asking: number; tone: GlanceTone | null }

/**
 * The working attempts of every task, summarised per task. `runs` may hold
 * attempts that no longer hold a checkout; they are not counted.
 */
export function taskAgentSummaries(runs: readonly TaskRun[], context: MonitorContext): Map<string, TaskAgentSummary> {
  const out = new Map<string, TaskAgentSummary>();
  for (const run of runs) {
    if (!runHoldsCheckout(run, context.clock)) continue;
    const { tone } = monitorAttempt(run, context);
    const current = out.get(run.task_id) ?? { working: 0, asking: 0, tone: null };
    const urgent = current.tone === null || (tone !== null && GLANCE_RANK[tone] < GLANCE_RANK[current.tone]) ? tone : current.tone;
    out.set(run.task_id, { working: current.working + 1, asking: current.asking + (asksForReader(tone) ? 1 : 0), tone: urgent });
  }
  return out;
}

/**
 * What the attempt's checkout holds, from the repository tab GitPulse has
 * open on it. Null whenever that is not a reading: no open tab, or one still
 * loading, failed, or waiting for trust — a 0 there is a missing status read,
 * not a clean tree. `shared` is a checkout that is not this attempt's own
 * worktree, whose changes may be anyone's.
 */
export interface CheckoutChanges { files: number; branch: string | null; shared: boolean }

/** Which attempt is asking, and the other attempts the caller knows are live. */
export interface AttemptIdentity {
  runId: string;
  /** Live attempts as read; the asking one may be among them. */
  peers?: readonly Pick<TaskRun, "id" | "cwd">[];
}

/**
 * Whether `cwd` is the worktree the host made for this attempt.
 *
 * Read from the run, not from the layout: the host places an attempt's own
 * worktree at `<main checkout>/.gitpulse/worktrees/<slug>-<short>`, where
 * `short` is the first 8 ASCII letters and digits of the run id, lowercased
 * (`src-tauri/src/workbench/agent_worktree.rs`, `short` and `provision`). A
 * worktree any agent made — `.claude/worktrees/…`, or another attempt's —
 * does not carry this run's id, so it is not this attempt's own. Transcribed
 * from the host; the run record carries no ownership field to read instead.
 */
export function isAttemptWorktree(cwd: string, runId: string): boolean {
  const short = [...runId].filter((ch) => /[A-Za-z0-9]/.test(ch)).slice(0, 8).join("").toLowerCase();
  if (short.length < 4) return false;
  const segments = pathSegments(cwd);
  if (segments.length < 3) return false;
  const [container, worktrees, name] = segments.slice(-3);
  return container === ".gitpulse" && worktrees === "worktrees" && name.toLowerCase().endsWith(`-${short}`);
}

/**
 * What to say once the host has released an attempt's hold on its checkout.
 *
 * Releasing ends the hold, not the directory: the host never removes a
 * worktree whose attempt it accepted (`agent_worktree.rs` — the agent's work
 * lives there), so for the attempt's own worktree the reader is told it is
 * still on disk, and where, rather than left to think it went.
 */
export function releasedNote(run: Pick<TaskRun, "id" | "cwd">): string {
  return isAttemptWorktree(run.cwd, run.id)
    ? `Released. The attempt's worktree stays on disk at ${run.cwd}, with its changes and branch, and its checkout is free for another attempt.`
    : "Released. The checkout is free for another attempt.";
}

export function checkoutChanges(
  cwd: string,
  tabs: readonly Pick<OpenRepoTab, "path" | "familyRoot" | "isLoading" | "error" | "trustRequired" | "changedCount" | "currentBranch">[],
  options: PathIdentityOptions,
  attempt: AttemptIdentity,
): CheckoutChanges | null {
  const key = identityKey(cwd, options);
  if (!key) return null;
  const tab = tabs.find((candidate) => identityKey(candidate.path, options) === key);
  if (!tab || tab.isLoading || tab.error || tab.trustRequired) return null;
  if (!Number.isFinite(tab.changedCount) || tab.changedCount < 0) return null;
  // Shared unless it is this run's own worktree and no other live attempt is
  // working in it. The main checkout is never an attempt's own.
  const mainCheckout = !!tab.familyRoot && identityKey(tab.familyRoot, options) === key;
  const crowded = (attempt.peers ?? []).some((peer) => peer.id !== attempt.runId && identityKey(peer.cwd, options) === key);
  const own = !mainCheckout && !crowded && isAttemptWorktree(cwd, attempt.runId);
  return { files: tab.changedCount, branch: tab.currentBranch, shared: !own };
}

/**
 * Pending managed requests that still need an answer, from the first page of
 * the run's *pending* requests; `more` when that page was full, so the count
 * is a floor. A page of pending rows whose targets went stale counts none.
 */
export interface PendingRequests { count: number; more: boolean }

export function pendingRequests(page: { items: readonly { state: string; actionable: boolean }[]; has_more: boolean }): PendingRequests {
  return { count: page.items.filter((item) => item.state === "pending" && item.actionable).length, more: page.has_more };
}

/**
 * Pending requests for every managed attempt with a live session, read in
 * parallel. A run whose read failed is left out — no count is shown for it,
 * which is not the same as showing 0.
 */
export async function readPendingRequests(
  runs: readonly Pick<TaskRun, "id" | "kind" | "session_id">[],
  list: (runId: string) => Promise<{ items: readonly { state: string; actionable: boolean }[]; has_more: boolean }>,
): Promise<Map<string, PendingRequests>> {
  const managed = runs.filter((run) => run.kind === "managed" && run.session_id);
  const answers = await Promise.all(managed.map(async (run) => {
    try { return [run.id, pendingRequests(await list(run.id))] as const; }
    catch { return null; }
  }));
  return new Map(answers.filter((answer) => answer !== null));
}

/**
 * The card's line for those requests. A count is a floor when the page was
 * full ("30+"); a full page with nothing answerable on it says only that the
 * read did not reach the end — never "0+ waiting", and never as a request.
 */
export function requestsLine(asked: PendingRequests | undefined): { tone: GlanceTone; text: string } | null {
  if (!asked) return null;
  if (asked.count > 0) {
    const count = asked.more ? `${asked.count}+` : String(asked.count);
    return { tone: "needs-you", text: `${count} ${asked.count === 1 && !asked.more ? "request is" : "requests are"} waiting for you` };
  }
  return asked.more ? { tone: "quiet", text: "More requests than one read shows · Review requests lists them" } : null;
}
