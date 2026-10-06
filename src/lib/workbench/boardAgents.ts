/**
 * Which tasks on the board have agents working on them, and whether any of
 * those agents needs the reader.
 *
 * The task sheet's Agents pane says this for one task (`TaskAgentPanel`). The
 * board says it for all of them, from the same reads and the same judgement
 * (`taskSessions.ts::taskAgentSummaries` over `monitorAttempt`), so a card
 * that says "needs you" opens on a pane that says the same.
 *
 * One read per holding state across every task (`listAllLiveRuns`), then the
 * pending requests of each managed attempt with a live session. It polls only
 * while some attempt holds a checkout and the window is in front; otherwise a
 * store write (`workbench-changed`) or the window coming forward wakes it.
 */

import { writable, type Readable } from "svelte/store";
import { explainError, listAllLiveRuns, listPendingDecisions, type TaskRun } from "./client";
import { readPendingRequests, type PendingRequests } from "./taskSessions";
import { runHoldsCheckout } from "./taskHandoff";
import { TASK_RUN_POLL_MS, nextTaskRunPollDelay } from "./runPoll";
import { readEventLoopDelay } from "../runtime/loadCadence";
import { readBackgroundDocument } from "../runtime/foreground";

export interface BoardAgents {
  /** Attempts holding a checkout as of `readAt`. */
  runs: readonly TaskRun[];
  /** False when a state had more than one page: every count is a floor. */
  complete: boolean;
  pending: ReadonlyMap<string, PendingRequests>;
  /** The instant the runs were judged against; null before the first read. */
  readAt: number | null;
  /**
   * Why the last read failed. The runs are then cleared rather than kept:
   * a card still marked from an old read would claim an agent nobody checked.
   */
  error: string | null;
}

export interface BoardAgentsDeps {
  listRuns: () => Promise<{ runs: TaskRun[]; complete: boolean }>;
  listPending: (runId: string) => Promise<{ items: readonly { state: string; actionable: boolean }[]; has_more: boolean }>;
  clock: () => number;
  background: () => boolean;
  lag: () => number;
  setTimer: (run: () => void, ms: number) => ReturnType<typeof setTimeout>;
  clearTimer: (handle: ReturnType<typeof setTimeout>) => void;
}

const DEFAULTS: BoardAgentsDeps = {
  listRuns: listAllLiveRuns,
  listPending: listPendingDecisions,
  clock: () => Date.now(),
  background: readBackgroundDocument,
  lag: readEventLoopDelay,
  setTimer: (run, ms) => setTimeout(run, ms),
  clearTimer: (handle) => clearTimeout(handle),
};

const EMPTY: BoardAgents = Object.freeze({ runs: [], complete: true, pending: new Map(), readAt: null, error: null });

export interface BoardAgentsWatch extends Readable<BoardAgents> {
  /** Reads now and keeps reading while anything is live. Idempotent. */
  start(): void;
  /** Stops reading; a read in flight is discarded. */
  stop(): void;
  /** Something changed (a store write): read again, coalesced with any read in flight. */
  refresh(): Promise<void>;
  /** The window came forward or went back. */
  wake(): void;
}

export function createBoardAgents(overrides: Partial<BoardAgentsDeps> = {}): BoardAgentsWatch {
  const deps: BoardAgentsDeps = { ...DEFAULTS, ...overrides };
  const store = writable<BoardAgents>(EMPTY);
  let state: BoardAgents = EMPTY;
  let started = false;
  let reading: Promise<void> | null = null;
  let readingEpoch = -1;
  let readingId = 0;
  let readCount = 0;
  let again = false;
  let timer: ReturnType<typeof setTimeout> | null = null;
  // A stop and a later start must not let the first run's read land.
  let epoch = 0;

  const clear = () => { if (timer !== null) { deps.clearTimer(timer); timer = null; } };

  function schedule() {
    clear();
    if (!started) return;
    const delay = nextTaskRunPollDelay({
      baseMs: TASK_RUN_POLL_MS,
      lagMs: deps.lag(),
      background: deps.background(),
      live: state.runs.length > 0,
      active: true,
    });
    if (delay === null) return;
    timer = deps.setTimer(() => { timer = null; void refresh(); }, delay);
  }

  async function readOnce(ticket: number) {
    try {
      const { runs, complete } = await deps.listRuns();
      const readAt = deps.clock();
      const live = runs.filter((run) => runHoldsCheckout(run, readAt));
      const pending = await readPendingRequests(live, deps.listPending);
      if (ticket !== epoch) return;
      state = { runs: live, complete, pending, readAt, error: null };
    } catch (cause) {
      if (ticket !== epoch) return;
      state = { ...EMPTY, error: explainError(cause) };
    }
    store.set(state);
  }

  async function refresh(): Promise<void> {
    if (!started) return;
    if (reading && readingEpoch === epoch) { again = true; return reading; }
    // A read from before a stop: let it finish (its result is discarded),
    // then read for this run.
    if (reading) return reading.then(() => refresh());
    const ticket = epoch;
    const id = ++readCount;
    readingId = id;
    readingEpoch = ticket;
    reading = (async () => {
      try {
        do { again = false; await readOnce(ticket); } while (again && started && ticket === epoch);
      } finally {
        if (readingId === id) reading = null;
        if (ticket === epoch) schedule();
      }
    })();
    return reading;
  }

  return {
    subscribe: store.subscribe,
    start() {
      if (started) return;
      started = true;
      void refresh();
    },
    stop() {
      started = false;
      epoch += 1;
      again = false;
      clear();
    },
    refresh,
    wake() {
      if (!started) return;
      if (deps.background()) { clear(); return; }
      void refresh();
    },
  };
}
