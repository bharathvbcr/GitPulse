/**
 * Running the board in waves: launch the top N suggested tasks, wait for each
 * to finish with a verified verdict, merge the verified ones one at a time,
 * then launch the next N.
 *
 * This module is the state machine and nothing else. Every effect (asking
 * Manvi for suggestions, preparing and launching a run, verifying, merging,
 * writing the durable record) arrives through `WaveDeps`, which is what lets
 * a restart, a refusal, a conflict or a cancel be tested without a sidecar.
 *
 * What it will not do, by construction:
 * - merge anything but a task whose verdict is `passed`, not stale, and
 *   reached for the head the worktree is still at. `degraded`, `failed`,
 *   `could_not_run`, `running` and `none` all hold the task for review.
 * - launch two tasks whose declared file scopes overlap, or whose scope is
 *   unknown: an unreadable scope collides with everything, never with nothing.
 * - retry a refused launch, or launch a task the wave already attempted.
 * - relaunch a claimed run after a restart: the run's id and request id are
 *   recorded before the preparation is sent, so a resume adopts the run if it
 *   exists and repeats the same idempotent preparation if it does not.
 * - push. The merge dependency merges and tears down; nothing here has a push.
 * - call a member `merged` before the gate after its merge has an outcome. The
 *   merge is recorded as `verifying_merge` first, with where the target's tip
 *   was before and after it; a restart in that phase runs the gate again. The
 *   gate re-verifies every member merged so far in this batch against the range
 *   from the tip before its own merge to the checkout's tip now, so a later
 *   merge that breaks an earlier member's criteria halts the wave. A batch is at
 *   most MAX_WAVE_SIZE members, so the gate is at most MAX_WAVE_SIZE
 *   verifications per merge, under the same concurrency limit as the first
 *   verifications.
 */

import { MergeTeardownFailure } from "../branches/mergeTeardown";
import { identityKey, isCaseInsensitiveFs, type PathIdentityOptions } from "../repos/paths";
import { formatError } from "../ui/formatError";

export const DEFAULT_WAVE_SIZE = 3;
export const MAX_WAVE_SIZE = 5;
export const DEFAULT_WAVES = 3;
export const MAX_WAVES = 5;
/** How long one member may take from launch to a verdict before it is held. */
export const MEMBER_DEADLINE_MS = 6 * 60 * 60_000;
/** Most task ids a wave remembers having attempted. */
export const MAX_ATTEMPTED = 200;

export type MemberPhase = "launching" | "running" | "verifying" | "ready" | "merging" | "verifying_merge" | "merged" | "held" | "stopped";
export type WaveState = "running" | "done" | "cancelled" | "halted" | "failed" | "refused";

export interface WaveMember {
  taskId: string;
  title: string;
  phase: MemberPhase;
  /** Recorded before the preparation is sent: what makes a resume idempotent. */
  runId: string;
  requestId: string;
  cwd: string | null;
  /** The commit the verdict was reached for; set only once it passed. */
  head: string | null;
  /** What to tell the person: why it is held, what was merged. */
  detail: string;
  startedAt: number;
  /** The target branch's tip before this member's merge and after it, recorded with `verifying_merge`. A record from before the gate existed has neither. */
  mergeBefore?: string | null;
  mergeAfter?: string | null;
}

export interface WaveRecord {
  v: 1;
  id: string;
  repositoryId: string;
  /** Tasks per batch, bounded by MAX_WAVE_SIZE. */
  n: number;
  /** Batches still to launch after the current one. */
  batchesLeft: number;
  batch: number;
  state: WaveState;
  members: WaveMember[];
  /** Every task this wave has launched or tried to; never offered again. */
  attempted: string[];
  /** Why the wave is not running, or what the last batch skipped. */
  note: string;
  /** Strictly increasing per write; a resume trusts the highest. */
  seq: number;
  /** The person allowed launching from a ranking with unchecked verification; kept so a restart honours the same choice. Absent is not allowed. */
  acceptUnchecked?: boolean;
  /** The Suggest next panel's priority filter the wave asks with, kept so a restart asks the same. Absent is any priority. */
  maxPriority?: number;
}

export interface Candidate { id: string; title: string }
/**
 * Ranked candidates and how much of the board the ranking was read from.
 * `unread` is why the ranking cannot be trusted to have seen every blocker
 * (some task's links were never read, or a bound cut the read); `unchecked` is
 * how many tasks had no verification state consulted, which only reorders.
 */
export interface Suggestion { candidates: Candidate[]; unread: string; unchecked: number }
export interface VerdictRead { state: string; stale: boolean; head: string | null }
export interface RunRead { state: string; exitCode: number | null; cwd: string; reason: string }
/** What a merge did: the line to show, and the target's tip before it and after it (null when git could not say). */
export interface MergeRead { message: string; beforeOid: string; afterOid: string | null }
/** A verification of the merged checkout: the range from `base` (a tip before a merge) to `head` (the tip now). */
export interface PostMerge { base: string; head: string }
/** `dirty` null: the count could not be read. `operation`: what is parked in the checkout ("merge", "squash merge", ...), or null when nothing is. */
export interface MainState { dirty: number | null; operation: string | null }

/** Why the main checkout is not free to launch from or merge into, or "" when it is. A state that could not be read is not free. */
function mainBlocker(state: MainState | null | undefined): string {
  // Free only on a reading that says so: a whole-number zero count and no operation. Anything else, a malformed reading included, is not free.
  if (!state || typeof state !== "object") return "the main checkout could not be read";
  if (typeof state.operation === "string" && state.operation) return `a ${state.operation} is in progress in the main checkout`;
  const { dirty } = state;
  if (typeof dirty === "number" && Number.isInteger(dirty) && dirty > 0) return `the main checkout has ${dirty} uncommitted file${dirty === 1 ? "" : "s"}`;
  if (!(typeof dirty === "number" && Number.isInteger(dirty) && dirty === 0)) return "the main checkout's uncommitted changes could not be counted";
  if (state.operation !== null) return "the main checkout's operation state could not be read";
  return "";
}

/** The host's `code` on a rejection that is a conflict or a parked squash, typed or not: a malformed object still says what the host said. */
function haltingCode(cause: unknown): "conflict" | "squash_parked" | null {
  const code = cause instanceof MergeTeardownFailure ? cause.failure.code : cause !== null && typeof cause === "object" ? (cause as { code?: unknown }).code : null;
  return code === "conflict" || code === "squash_parked" ? code : null;
}

export interface WaveDeps {
  /** Ranked suggestions, most important first (Manvi `work.tasks.suggest`). */
  suggest(signal: AbortSignal): Promise<Suggestion>;
  /** Declared file scopes by task id. A task absent from the map, or with no files, has an unknown scope. */
  scopes(ids: readonly string[], signal: AbortSignal): Promise<ReadonlyMap<string, readonly string[]>>;
  /** Prepares and launches one managed run in its own worktree. Throws on refusal (admission, load, spend cap). */
  launch(taskId: string, ids: { runId: string; requestId: string }, signal: AbortSignal): Promise<RunRead>;
  /** The run, or null when the store has no such run. */
  getRun(runId: string, signal: AbortSignal): Promise<RunRead | null>;
  /**
   * Runs `work.verify.task` for the task and waits for a verdict that is not `running`; rejects once `signal` aborts.
   * With `post`, the merged checkout is judged over that range, and the answer is a verdict for `post.head` only.
   */
  verify(taskId: string, signal: AbortSignal, post?: PostMerge): Promise<VerdictRead>;
  /** The commit the run's worktree is at now. */
  headOf(cwd: string, signal: AbortSignal): Promise<string>;
  /**
   * Merges the verified head; throws, merging nothing, on a conflict, a dirty or contended checkout, or a moved head.
   * A conflict or a parked squash is thrown as a `MergeTeardownFailure`, which is what halts the wave. A merge that
   * landed with its cleanup unfinished returns, saying so: it counts as merged.
   */
  merge(target: { taskId: string; runId: string; cwd: string; head: string }): Promise<MergeRead>;
  /** The main checkout now: its uncommitted file count (null when it could not be counted) and the parked operation, if any. Throws when it cannot be read. */
  mainState(signal: AbortSignal): Promise<MainState>;
  /** Writes the record durably; a failure stops the wave. */
  persist(record: WaveRecord): Promise<void>;
  now(): number;
  sleep(ms: number, signal: AbortSignal): Promise<void>;
  id(): string;
  pollMs: number;
  /** Verifications the host runs at once (its `MAX_ACTIVE`): more members than this wait their turn instead of all starting. */
  verifyLimit: number;
}

export function clampWaveSize(value: unknown): number {
  const n = typeof value === "number" && Number.isFinite(value) ? Math.floor(value) : DEFAULT_WAVE_SIZE;
  return Math.min(MAX_WAVE_SIZE, Math.max(1, n));
}

export function clampWaves(value: unknown): number {
  const n = typeof value === "number" && Number.isFinite(value) ? Math.floor(value) : DEFAULT_WAVES;
  return Math.min(MAX_WAVES, Math.max(1, n));
}

/** A path as the filesystem tells files apart: `identityKey` (separators, repeated and trailing slashes, `.` and `..` segments, case on a case-insensitive filesystem). */
function normalizePath(path: string, options: PathIdentityOptions): string {
  return identityKey(path, options);
}

const hostOptions = (): PathIdentityOptions => ({ caseInsensitive: isCaseInsensitiveFs() });

/** Whether two path sets touch the same file: equal paths, or one inside a directory the other names. */
export function scopesOverlap(a: readonly string[], b: readonly string[], options: PathIdentityOptions = hostOptions()): boolean {
  const left = a.map((p) => normalizePath(p, options)).filter(Boolean), right = b.map((p) => normalizePath(p, options)).filter(Boolean);
  for (const x of left) for (const y of right) {
    if (x === y || x.startsWith(`${y}/`) || y.startsWith(`${x}/`)) return true;
  }
  return false;
}

export interface BatchPlan { chosen: Candidate[]; skipped: { id: string; title: string; reason: string }[] }

/**
 * The first `n` candidates, in rank order, whose scopes do not collide. A
 * candidate with no readable scope is admitted only first, and then nothing
 * else is admitted beside it, so unknown never reads as disjoint.
 */
export function planBatch(candidates: readonly Candidate[], scopes: ReadonlyMap<string, readonly string[]>, n: number, excluded: ReadonlySet<string>, options: PathIdentityOptions = hostOptions()): BatchPlan {
  const chosen: Candidate[] = [], skipped: BatchPlan["skipped"] = [];
  const known = (id: string) => (scopes.get(id) ?? []).map((p) => normalizePath(p, options)).filter(Boolean);
  for (const candidate of candidates) {
    if (chosen.length >= n) break;
    if (excluded.has(candidate.id) || chosen.some((c) => c.id === candidate.id)) continue;
    const mine = known(candidate.id);
    const clash = chosen.find((other) => {
      const theirs = known(other.id);
      return mine.length === 0 || theirs.length === 0 || scopesOverlap(mine, theirs, options);
    });
    if (clash) {
      skipped.push({ id: candidate.id, title: candidate.title, reason: mine.length === 0 || known(clash.id).length === 0 ? `its file scope is unknown next to "${clash.title}"` : `its files overlap "${clash.title}"` });
      continue;
    }
    chosen.push(candidate);
  }
  return { chosen, skipped };
}

function member(candidate: Candidate, deps: WaveDeps): WaveMember {
  return { taskId: candidate.id, title: candidate.title.slice(0, 200), phase: "launching", runId: deps.id(), requestId: deps.id(), cwd: null, head: null, detail: "", startedAt: deps.now() };
}

const TERMINAL: ReadonlySet<MemberPhase> = new Set(["merged", "held", "stopped"]);
const RUN_ENDED: ReadonlySet<string> = new Set(["exited", "failed", "cancelled", "unresolved"]);

export interface WaveRunner {
  readonly record: () => WaveRecord;
  /** Resolves when the wave is no longer running. */
  readonly finished: Promise<WaveRecord>;
  /** A stop was asked for and the wave has not settled yet: a merge in flight is let finish, so the panel says it is stopping after it. */
  readonly stopping: () => boolean;
  /** Stops launching and merging. Runs already started are left to their agents. */
  cancel(): Promise<void>;
}

/** At most `limit` of the works handed to `run` are in flight at once; the rest wait their turn, and leave the queue when `signal` aborts. */
class Limiter {
  private active = 0;
  private readonly waiting: (() => void)[] = [];
  constructor(private readonly limit: number) {}

  async run<T>(work: () => Promise<T>, signal: AbortSignal): Promise<T> {
    while (this.active >= this.limit) {
      await new Promise<void>((resolve, reject) => {
        if (signal.aborted) { reject(new Error("The wave was stopped.")); return; }
        const turn = () => { signal.removeEventListener("abort", stop); resolve(); };
        const stop = () => {
          const at = this.waiting.indexOf(turn);
          if (at >= 0) this.waiting.splice(at, 1);
          reject(new Error("The wave was stopped."));
        };
        this.waiting.push(turn);
        signal.addEventListener("abort", stop, { once: true });
      });
    }
    this.active += 1;
    try { return await work(); } finally { this.active -= 1; this.waiting.shift()?.(); }
  }
}

/** Whether a priority filter is one the panel can have chosen. */
function validPriority(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= 0 && value <= 9;
}

/** Starts a new wave and returns at once; `finished` settles when it stops running. */
export async function startWave(input: { repositoryId: string; n: number; waves: number; acceptUnchecked?: boolean; maxPriority?: number }, deps: WaveDeps): Promise<WaveRunner> {
  const record: WaveRecord = { v: 1, id: deps.id(), repositoryId: input.repositoryId, n: clampWaveSize(input.n), batchesLeft: clampWaves(input.waves), batch: 0, state: "running", members: [], attempted: [], note: "", seq: 0, acceptUnchecked: input.acceptUnchecked === true };
  if (validPriority(input.maxPriority)) record.maxPriority = input.maxPriority;
  return drive(record, deps, true);
}

/** Continues a persisted wave after a restart. A wave that is not running is returned as it was. */
export function resumeWave(record: WaveRecord, deps: WaveDeps): Promise<WaveRunner> {
  return drive(record, deps, false);
}

/** Why a verdict for the merged checkout is not a pass, or null when it is one for exactly `tip`. */
function gateFault(verdict: VerdictRead | null, why: string, tip: string): string | null {
  if (!verdict) return `could not run (${why})`;
  if (verdict.state !== "passed") return `was ${verdict.state.replace("_", " ")}`;
  if (verdict.stale || !verdict.head || verdict.head !== tip) return "was not a verdict for the merged checkout's tip";
  return null;
}

async function drive(start: WaveRecord, deps: WaveDeps, fresh: boolean): Promise<WaveRunner> {
  // A merge interrupted by a restart may or may not have happened; it is never repeated blind.
  // A member left in `verifying_merge` did merge: it stays there, and its gate runs again.
  let record = fresh ? start : { ...start, members: start.members.map((m) => (m.phase === "merging" ? { ...m, phase: "held" as MemberPhase, detail: "Interrupted while merging; check the branch before merging again." } : m)) };
  // A merge cut off by a restart may have conflicted and been rolled back, or landed: its outcome is unknown, and every member behind it was cut from the same base.
  const interrupted = !fresh && start.state === "running" ? start.members.find((m) => m.phase === "merging") : undefined;
  const abort = new AbortController();
  const limiter = new Limiter(Math.max(1, Math.floor(deps.verifyLimit) || 1));
  const inflight = new Map<string, Promise<void>>();
  // A verification that settled badly (its record could not be written) is the wave's failure; the loop rethrows it.
  const fatal: { raised: boolean; cause: unknown } = { raised: false, cause: undefined };
  let cancelled = false;
  // Verifications settle concurrently, but the record is written one write at a time, in seq order.
  let writing: Promise<void> = Promise.resolve();
  const save = async (change: Partial<WaveRecord> = {}) => {
    record = { ...record, ...change, seq: record.seq + 1 };
    const snapshot = record;
    const turn = writing.then(() => deps.persist(snapshot));
    writing = turn.then(() => undefined, () => undefined);
    await turn;
  };
  /** Ends the wave in ONE write: `change` is applied to every member, and any left unfinished is stopped. */
  const settle = async (change: (m: WaveMember) => WaveMember, state: WaveState, note: string) => {
    const members = record.members.map((m) => {
      const next = change(m);
      return TERMINAL.has(next.phase) ? next : { ...next, phase: "stopped" as MemberPhase, detail: next.detail || note };
    });
    await save({ state, note, members });
  };
  const stopTerminal = (state: WaveState, note: string) => settle((m) => m, state, note);

  const launchBatch = async (): Promise<boolean> => {
    // A parked merge or a person's uncommitted work in the main checkout is not
    // this wave's to build on: the next batch's merges would land over it.
    let blocker: string;
    try { blocker = mainBlocker(await deps.mainState(abort.signal)); }
    catch (cause) { blocker = `the main checkout could not be read (${message(cause)})`; }
    if (blocker) {
      await save({ state: "refused", note: `Nothing was launched: ${blocker}.` });
      return false;
    }
    const suggested = await deps.suggest(abort.signal);
    if (suggested.unread) {
      // A blocker may be among what was not read; launching from this ranking could start a blocked task.
      await save({ state: "refused", note: `Nothing was launched: ${suggested.unread}` });
      return false;
    }
    if (suggested.unchecked > 0 && record.acceptUnchecked !== true) {
      await save({ state: "refused", note: `Nothing was launched: ${suggested.unchecked} task${suggested.unchecked === 1 ? " was" : "s were"} not checked for a failed verification, so the ranking may be wrong. Start again and allow an unchecked ranking to launch from it.` });
      return false;
    }
    const caveat = suggested.unchecked > 0 ? `Ranked without a verification check on ${suggested.unchecked} task${suggested.unchecked === 1 ? "" : "s"} (allowed by the person). ` : "";
    const ranked = suggested.candidates.slice(0, 20);
    const excluded = new Set(record.attempted);
    const open = ranked.filter((c) => !excluded.has(c.id));
    if (open.length === 0) { await save({ state: "done", note: "No further task is ready to start." }); return false; }
    const scopes = await deps.scopes(open.map((c) => c.id), abort.signal);
    const plan = planBatch(open, scopes, record.n, excluded);
    const members = plan.chosen.map((c) => member(c, deps));
    const note = caveat + (plan.skipped.length ? `Held back from this batch: ${plan.skipped.map((s) => `${s.title} (${s.reason})`).join("; ")}.` : "");
    await save({
      batch: record.batch + 1, batchesLeft: record.batchesLeft - 1, members, note,
      attempted: [...record.attempted, ...members.map((m) => m.taskId)].slice(-MAX_ATTEMPTED),
    });
    return true;
  };

  /** Changes one member of a wave that is still running; a verification that settles after the wave ended writes nothing. */
  const update = async (taskId: string, change: Partial<WaveMember>) => {
    if (record.state !== "running") return;
    await save({ members: record.members.map((m) => (m.taskId === taskId ? { ...m, ...change } : m)) });
  };

  const stepMember = async (m: WaveMember) => {
    if (m.phase === "launching") {
      // A run recorded before a restart is adopted; only an absent one is prepared (with the same ids).
      let run = await deps.getRun(m.runId, abort.signal);
      if (!run) {
        try { run = await deps.launch(m.taskId, { runId: m.runId, requestId: m.requestId }, abort.signal); }
        catch (cause) {
          // A stop ends the wait, not the run it may have prepared; the member is left to be marked stopped.
          if (cancelled) return;
          await update(m.taskId, { phase: "held", detail: `Not launched: ${message(cause)}` }); return; }
      }
      await update(m.taskId, { phase: "running", cwd: run.cwd, detail: "" });
      return;
    }
    if (m.phase === "running") {
      const run = await deps.getRun(m.runId, abort.signal);
      if (!run) { await update(m.taskId, { phase: "held", detail: "The run is no longer in the store." }); return; }
      if (run.cwd && run.cwd !== m.cwd) { await update(m.taskId, { cwd: run.cwd }); return; }
      if (!RUN_ENDED.has(run.state)) {
        if (deps.now() - m.startedAt > MEMBER_DEADLINE_MS) await update(m.taskId, { phase: "held", detail: "Gave up waiting: the run did not finish in time." });
        return;
      }
      if (run.state !== "exited" || run.exitCode !== 0) {
        await update(m.taskId, { phase: "held", detail: `The run ${run.state === "exited" ? `exited with code ${String(run.exitCode)}` : run.state}${run.reason ? `: ${run.reason.slice(0, 200)}` : ""}.` });
        return;
      }
      await update(m.taskId, { phase: "verifying" });
    }
  };

  /** One member's verification, started once and left to settle beside the others, at most `verifyLimit` at a time. */
  const verifyMember = async (m: WaveMember) => {
    let verdict: VerdictRead, head: string;
    try {
      verdict = await limiter.run(() => deps.verify(m.taskId, abort.signal), abort.signal);
      head = m.cwd ? await deps.headOf(m.cwd, abort.signal) : "";
    } catch (cause) {
      if (cancelled) return;
      await update(m.taskId, { phase: "held", detail: `Verification could not run: ${message(cause)}` }); return; }
    if (verdict.state !== "passed") { await update(m.taskId, { phase: "held", detail: `Verification ${verdict.state.replace("_", " ")}; this is not a pass, so it is left for review.` }); return; }
    if (verdict.stale || !verdict.head || !head || verdict.head !== head) {
      await update(m.taskId, { phase: "held", detail: "The passing verdict is not for the commit the worktree is at, so it is left for review." });
      return;
    }
    await update(m.taskId, { phase: "ready", head });
  };

  const startVerification = (m: WaveMember) => {
    const settled = verifyMember(m)
      .catch((cause: unknown) => { if (!fatal.raised) { fatal.raised = true; fatal.cause = cause; } })
      .finally(() => { inflight.delete(m.taskId); });
    inflight.set(m.taskId, settled);
  };

  /**
   * The gate after a merge: re-verifies every member merged so far in this batch
   * over the range from the target's tip before its own merge to the checkout's
   * tip now. Only then is the member that just merged `merged`; a fault halts the
   * wave and is written on the member it was found in.
   */
  const gateMerge = async () => {
    const landed = record.members.find((m) => m.phase === "verifying_merge");
    if (!landed) return;
    const held = (why: string) => (m: WaveMember): WaveMember => (m.taskId === landed.taskId ? { ...m, phase: "held", detail: `${landed.detail} ${why}`.trim() } : m);
    // Every exit below is ONE write that carries the gate outcome with the member's move out of
    // `verifying_merge`, so no record ever holds a terminal member without it, or a halt without its member.
    if (cancelled) {
      await settle(held("The wave was stopped before the merge was re-verified; check the main checkout."), "cancelled", "Stopped by the person; no further task was launched or merged.");
      return;
    }
    const tip = landed.mergeAfter, base = landed.mergeBefore;
    if (!tip || !base) {
      // Fail closed: without where the merge landed there is no range to judge, and nothing may read as verified.
      await settle(held("Where the merge landed was not recorded, so it was not re-verified; check the main checkout."), "halted", `Re-verification after merging "${landed.title}" could not run (where the merge landed was not recorded); no further merge was made.`);
      return;
    }
    const merged = record.members.filter((m) => m.phase === "merged" || m.phase === "verifying_merge");
    const unjudged = merged.filter((m) => !m.mergeBefore);
    if (unjudged.length > 0) {
      // A member merged by a record from before the gate has no pre-merge tip to judge it over; it is never counted as judged.
      await settle(held("An earlier member of this batch has no recorded pre-merge tip, so the merged set was not re-verified; check the main checkout."), "halted", `Re-verification after merging "${landed.title}" could not cover ${unjudged.map((m) => `"${m.title}"`).join(", ")} (merged by a record that kept no pre-merge tip); no further merge was made.`);
      return;
    }
    const judged = merged.flatMap((m) => (m.mergeBefore ? [{ m, base: m.mergeBefore }] : []));
    const results = await Promise.all(judged.map(async ({ m, base: from }) => {
      try { return { m, verdict: await limiter.run(() => deps.verify(m.taskId, abort.signal, { base: from, head: tip }), abort.signal), why: "" }; }
      catch (cause) { return { m, verdict: null, why: message(cause) }; }
    }));
    if (cancelled) {
      await settle(held("The wave was stopped before the merge was re-verified; check the main checkout."), "cancelled", "Stopped by the person; no further task was launched or merged.");
      return;
    }
    const faults = results.flatMap((r) => { const fault = gateFault(r.verdict, r.why, tip); return fault ? [{ m: r.m, fault }] : []; });
    const own = faults.find((f) => f.m.taskId === landed.taskId);
    const first = own ?? faults[0];
    const outcome = (m: WaveMember): WaveMember => {
      if (m.taskId === landed.taskId) return { ...m, phase: "merged", detail: own ? `${landed.detail} Re-verification after the merge ${own.fault}.` : landed.detail };
      const f = faults.find((entry) => entry.m.taskId === m.taskId);
      return f ? { ...m, detail: `${m.detail} Re-verification after merging "${landed.title}" ${f.fault}.`.trim() } : m;
    };
    if (!first) { await save({ members: record.members.map(outcome) }); return; }
    await settle(outcome, "halted", `Re-verification after merging "${landed.title}" ${first === own ? first.fault : `found "${first.m.title}" ${first.fault}`}; no further merge was made.`);
  };

  /** One merge at a time, in launch order; the merge is recorded as `verifying_merge`, and the gate runs on the loop's next turn. */
  const mergeNext = async (): Promise<boolean> => {
    const next = record.members.find((m) => m.phase === "ready");
    if (!next || !next.cwd || !next.head) return false;
    let blocker: string;
    try { blocker = mainBlocker(await deps.mainState(abort.signal)); }
    catch (cause) { if (cancelled) return true; blocker = `the main checkout could not be read (${message(cause)})`; }
    if (blocker) {
      await update(next.taskId, { phase: "held", detail: `Not merged: ${blocker}.` });
      return true;
    }
    await update(next.taskId, { phase: "merging" });
    let said: MergeRead;
    try {
      said = await deps.merge({ taskId: next.taskId, runId: next.runId, cwd: next.cwd, head: next.head });
    } catch (cause) {
      const halting = haltingCode(cause);
      if (halting) {
        // Nothing after a conflict may merge: the next member's branch was cut
        // from the same base, and a squash left parked is still in the checkout.
        const pending = halting === "conflict" ? `conflict in ${next.title}` : `squash parked in main (${next.title})`;
        // One write: the refused member, the ones waiting behind it and the halt, so a restart cannot find the member held and the wave still going.
        await settle(
          (m) => (m.taskId === next.taskId ? { ...m, phase: "held", detail: `Not merged: ${message(cause)}` } : m.phase === "ready" ? { ...m, phase: "held", detail: `not merged: ${pending} pending` } : m),
          "halted", `Merging "${next.title}" ended in a ${halting === "conflict" ? "conflict" : "parked squash"}; no further task was launched or merged.`,
        );
        return true;
      }
      await update(next.taskId, { phase: "held", detail: `Not merged: ${message(cause)}` });
      return true;
    }
    // Written before the gate runs, so a restart between the merge and its re-verify finds the member here and runs the gate.
    // Outside the try: a record that cannot be kept after a merge landed is the wave's failure, never "not merged".
    await update(next.taskId, { phase: "verifying_merge", detail: said.message, mergeBefore: said.beforeOid || null, mergeAfter: said.afterOid || null });
    return true;
  };

  const loop = (async (): Promise<WaveRecord> => {
    try {
      if (fresh && !(await launchBatch())) return record;
      if (interrupted) {
        await save({ members: record.members.map((m) => (m.phase === "ready" ? { ...m, phase: "held" as MemberPhase, detail: `not merged: the merge of ${interrupted.title} was interrupted and its outcome is unknown, pending` } : m)) });
        await stopTerminal("halted", `The merge of "${interrupted.title}" was interrupted by a restart; check the main checkout and the branch before merging anything else. Nothing was launched or merged.`);
      }
      while (record.state === "running") {
        if (fatal.raised) throw fatal.cause;
        // A merge that has no gate outcome yet comes first, after a restart as after a merge.
        if (record.members.some((m) => m.phase === "verifying_merge")) { await gateMerge(); continue; }
        if (cancelled) { await stopTerminal("cancelled", "Stopped by the person; no further task was launched or merged."); break; }
        // Phases advance one member at a time so every transition is persisted before the next effect;
        // verifications are only started here and settle on their own, up to the host's limit.
        for (const m of record.members) {
          if (cancelled || record.state !== "running") break;
          if (m.phase === "launching" || m.phase === "running") await stepMember(m);
          else if (m.phase === "verifying" && !inflight.has(m.taskId)) startVerification(m);
        }
        if (cancelled || record.state !== "running") continue;
        if (await mergeNext()) continue;
        if (record.members.every((m) => TERMINAL.has(m.phase))) {
          if (record.batchesLeft > 0) { if (!(await launchBatch())) break; continue; }
          await save({ state: "done", note: record.note || "Every batch has run." });
          break;
        }
        // A run is polled; a verification says when it settles, so waiting on it alone needs no timer.
        if (record.members.some((m) => m.phase === "launching" || m.phase === "running" || m.phase === "ready")) await Promise.race([deps.sleep(deps.pollMs, abort.signal), ...inflight.values()]);
        else if (inflight.size > 0) await Promise.race(inflight.values());
      }
    } catch (cause) {
      if (cancelled && record.state === "running") {
        try { await stopTerminal("cancelled", "Stopped by the person; no further task was launched or merged."); } catch { /* the record below still says what happened */ }
      }
      if (record.state === "running") {
        try { await save({ state: "failed", note: `The wave stopped because its state could not be kept or read: ${message(cause)}` }); } catch { record = { ...record, state: "failed", note: `The wave stopped and its state could not be saved: ${message(cause)}` }; }
      }
    }
    // Whatever the wave ended on, no verification of it is left waiting out its deadline.
    abort.abort();
    await Promise.allSettled([...inflight.values()]);
    return record;
  })();

  return {
    record: () => record,
    finished: loop,
    stopping: () => cancelled && record.state === "running",
    async cancel() { cancelled = true; abort.abort(); await loop; },
  };
}

/** What the person reads in a member's detail: the canonical rendering of a failure (`formatError`, which reads a typed `{code, message}` rejection), bounded. */
function message(cause: unknown): string {
  const text = formatError(cause);
  return text.length > 400 ? `${text.slice(0, 399)}…` : text;
}
