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
 */

import { MergeTeardownFailure } from "../branches/mergeTeardown";

export const DEFAULT_WAVE_SIZE = 3;
export const MAX_WAVE_SIZE = 5;
export const DEFAULT_WAVES = 3;
export const MAX_WAVES = 5;
/** How long one member may take from launch to a verdict before it is held. */
export const MEMBER_DEADLINE_MS = 6 * 60 * 60_000;
/** Most task ids a wave remembers having attempted. */
export const MAX_ATTEMPTED = 200;

export type MemberPhase = "launching" | "running" | "verifying" | "ready" | "merging" | "merged" | "held" | "stopped";
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
/** `dirty` null: the count could not be read. `operation`: what is parked in the checkout ("merge", "squash merge", ...), or null when nothing is. */
export interface MainState { dirty: number | null; operation: string | null }

/** Why the main checkout is not free to launch from or merge into, or "" when it is. A state that could not be read is not free. */
function mainBlocker(state: MainState): string {
  if (state.operation) return `a ${state.operation} is in progress in the main checkout`;
  if (state.dirty === null) return "the main checkout's uncommitted changes could not be counted";
  if (state.dirty > 0) return `the main checkout has ${state.dirty} uncommitted file${state.dirty === 1 ? "" : "s"}`;
  return "";
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
  /** Runs `work.verify.task` for the task and waits for a verdict that is not `running`; rejects once `signal` aborts. */
  verify(taskId: string, signal: AbortSignal): Promise<VerdictRead>;
  /** The commit the run's worktree is at now. */
  headOf(cwd: string, signal: AbortSignal): Promise<string>;
  /**
   * Merges the verified head; throws, merging nothing, on a conflict, a dirty or contended checkout, or a moved head.
   * A conflict or a parked squash is thrown as a `MergeTeardownFailure`, which is what halts the wave. A merge that
   * landed with its cleanup unfinished returns, saying so: it counts as merged.
   */
  merge(target: { taskId: string; runId: string; cwd: string; head: string }): Promise<string>;
  /** The main checkout now: its uncommitted file count (null when it could not be counted) and the parked operation, if any. Throws when it cannot be read. */
  mainState(signal: AbortSignal): Promise<MainState>;
  /** Writes the record durably; a failure stops the wave. */
  persist(record: WaveRecord): Promise<void>;
  now(): number;
  sleep(ms: number, signal: AbortSignal): Promise<void>;
  id(): string;
  pollMs: number;
}

export function clampWaveSize(value: unknown): number {
  const n = typeof value === "number" && Number.isFinite(value) ? Math.floor(value) : DEFAULT_WAVE_SIZE;
  return Math.min(MAX_WAVE_SIZE, Math.max(1, n));
}

export function clampWaves(value: unknown): number {
  const n = typeof value === "number" && Number.isFinite(value) ? Math.floor(value) : DEFAULT_WAVES;
  return Math.min(MAX_WAVES, Math.max(1, n));
}

function normalizePath(path: string): string {
  return path.replace(/\\/g, "/").replace(/^(\.\/)+/, "").replace(/\/+$/, "");
}

/** Whether two path sets touch the same file: equal paths, or one inside a directory the other names. */
export function scopesOverlap(a: readonly string[], b: readonly string[]): boolean {
  const left = a.map(normalizePath).filter(Boolean), right = b.map(normalizePath).filter(Boolean);
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
export function planBatch(candidates: readonly Candidate[], scopes: ReadonlyMap<string, readonly string[]>, n: number, excluded: ReadonlySet<string>): BatchPlan {
  const chosen: Candidate[] = [], skipped: BatchPlan["skipped"] = [];
  const known = (id: string) => (scopes.get(id) ?? []).map(normalizePath).filter(Boolean);
  for (const candidate of candidates) {
    if (chosen.length >= n) break;
    if (excluded.has(candidate.id) || chosen.some((c) => c.id === candidate.id)) continue;
    const mine = known(candidate.id);
    const clash = chosen.find((other) => {
      const theirs = known(other.id);
      return mine.length === 0 || theirs.length === 0 || scopesOverlap(mine, theirs);
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
  /** Stops launching and merging. Runs already started are left to their agents. */
  cancel(): Promise<void>;
}

/** Starts a new wave and returns at once; `finished` settles when it stops running. */
export async function startWave(input: { repositoryId: string; n: number; waves: number; acceptUnchecked?: boolean }, deps: WaveDeps): Promise<WaveRunner> {
  const record: WaveRecord = { v: 1, id: deps.id(), repositoryId: input.repositoryId, n: clampWaveSize(input.n), batchesLeft: clampWaves(input.waves), batch: 0, state: "running", members: [], attempted: [], note: "", seq: 0, acceptUnchecked: input.acceptUnchecked === true };
  return drive(record, deps, true);
}

/** Continues a persisted wave after a restart. A wave that is not running is returned as it was. */
export function resumeWave(record: WaveRecord, deps: WaveDeps): Promise<WaveRunner> {
  return drive(record, deps, false);
}

async function drive(start: WaveRecord, deps: WaveDeps, fresh: boolean): Promise<WaveRunner> {
  // A merge interrupted by a restart may or may not have happened; it is never repeated blind.
  let record = fresh ? start : { ...start, members: start.members.map((m) => (m.phase === "merging" ? { ...m, phase: "held" as MemberPhase, detail: "Interrupted while merging; check the branch before merging again." } : m)) };
  const abort = new AbortController();
  let cancelled = false;
  const save = async (change: Partial<WaveRecord> = {}) => {
    record = { ...record, ...change, seq: record.seq + 1 };
    await deps.persist(record);
  };
  const stopTerminal = async (state: WaveState, note: string) => {
    const members = record.members.map((m) => (TERMINAL.has(m.phase) ? m : { ...m, phase: "stopped" as MemberPhase, detail: m.detail || note }));
    await save({ state, note, members });
  };

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

  const update = async (taskId: string, change: Partial<WaveMember>) => {
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
      return;
    }
    if (m.phase === "verifying") {
      let verdict: VerdictRead, head: string;
      try {
        verdict = await deps.verify(m.taskId, abort.signal);
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
    }
  };

  /** One merge at a time, in launch order, each followed by a re-verify; a failed one halts the merges. */
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
    try {
      const said = await deps.merge({ taskId: next.taskId, runId: next.runId, cwd: next.cwd, head: next.head });
      await update(next.taskId, { phase: "merged", detail: said });
    } catch (cause) {
      const failure = cause instanceof MergeTeardownFailure ? cause.failure : null;
      if (failure?.code === "conflict" || failure?.code === "squash_parked") {
        // Nothing after a conflict may merge: the next member's branch was cut
        // from the same base, and a squash left parked is still in the checkout.
        const pending = failure.code === "conflict" ? `conflict in ${next.title}` : `squash parked in main (${next.title})`;
        await update(next.taskId, { phase: "held", detail: `Not merged: ${message(cause)}` });
        await save({ members: record.members.map((m) => (m.phase === "ready" ? { ...m, phase: "held" as MemberPhase, detail: `not merged: ${pending} pending` } : m)) });
        await stopTerminal("halted", `Merging "${next.title}" ended in a ${failure.code === "conflict" ? "conflict" : "parked squash"}; no further task was launched or merged.`);
        return true;
      }
      await update(next.taskId, { phase: "held", detail: `Not merged: ${message(cause)}` });
      return true;
    }
    let again: VerdictRead | null = null, why = "";
    try { again = await deps.verify(next.taskId, abort.signal); } catch (cause) { why = message(cause); }
    if (cancelled) return true;
    if (!again || again.state !== "passed") {
      await stopTerminal("halted", `Re-verification after merging "${next.title}" ${again ? `was ${again.state.replace("_", " ")}` : `could not run (${why})`}; no further merge was made.`);
    }
    return true;
  };

  const loop = (async (): Promise<WaveRecord> => {
    try {
      if (fresh && !(await launchBatch())) return record;
      while (record.state === "running") {
        if (cancelled) { await stopTerminal("cancelled", "Stopped by the person; no further task was launched or merged."); break; }
        const live = record.members.find((m) => m.phase === "launching" || m.phase === "running" || m.phase === "verifying");
        // Phases advance one member at a time so every transition is persisted before the next effect.
        for (const m of record.members) {
          if (cancelled || record.state !== "running") break;
          if (m.phase === "launching" || m.phase === "running" || m.phase === "verifying") await stepMember(m);
        }
        if (cancelled || record.state !== "running") continue;
        if (await mergeNext()) continue;
        if (record.members.every((m) => TERMINAL.has(m.phase))) {
          if (record.batchesLeft > 0) { if (!(await launchBatch())) break; continue; }
          await save({ state: "done", note: record.note || "Every batch has run." });
          break;
        }
        if (live || record.members.some((m) => m.phase === "ready")) await deps.sleep(deps.pollMs, abort.signal);
      }
    } catch (cause) {
      if (cancelled && record.state === "running") {
        try { await stopTerminal("cancelled", "Stopped by the person; no further task was launched or merged."); } catch { /* the record below still says what happened */ }
      }
      if (record.state === "running") {
        try { await save({ state: "failed", note: `The wave stopped because its state could not be kept or read: ${message(cause)}` }); } catch { record = { ...record, state: "failed", note: `The wave stopped and its state could not be saved: ${message(cause)}` }; }
      }
    }
    return record;
  })();

  return {
    record: () => record,
    finished: loop,
    async cancel() { cancelled = true; abort.abort(); await loop; },
  };
}

function message(cause: unknown): string {
  const text = cause instanceof Error ? cause.message : String(cause);
  return text.length > 400 ? `${text.slice(0, 399)}…` : text;
}
