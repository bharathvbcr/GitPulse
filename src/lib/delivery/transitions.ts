/**
 * What changed between two observations of a delivery listing.
 *
 * The point of a live poll is the moment something goes red, so that moment
 * has to be detected from two snapshots — and detected without inventing
 * anything. Three rules do the work, and each one exists because the obvious
 * implementation gets it wrong:
 *
 *  - Keyed by id, never by position. The listing is newest-first and capped,
 *    so a new run shifts every row down one. Comparing index to index reports
 *    every row as changed on every push.
 *  - A row settled on its first sighting is not a transition. Mounting the
 *    panel would otherwise announce every historical failure in the window as
 *    though it had just happened.
 *  - A row that vanished reports nothing. Rows fall off the display cap for
 *    reasons that have nothing to do with their outcome, and a disappearance
 *    is not evidence of one.
 *
 * Memory is bounded by construction: the answer is a pure function of two
 * capped snapshots, so nothing accumulates across polls. An "already
 * announced" set would grow for the lifetime of the window, and is not needed
 * — once a row is settled in the previous snapshot, settled-to-settled is not
 * a transition, so it cannot be announced twice.
 */
import { isInFlight, isSettled, isVerdict, type MonitorPhase } from "./phase";

/** The minimum a row needs to be diffable. */
export interface PhasedRow {
  /** Stable identity from the source. Compared as a string. */
  id: string;
  phase: MonitorPhase;
}

export interface PhaseTransition<T extends PhasedRow> {
  row: T;
  from: MonitorPhase;
  to: MonitorPhase;
}

/**
 * Index a snapshot by id, tolerating duplicates.
 *
 * Ids from both sources are unique, so a duplicate means something upstream is
 * wrong. First-wins rather than last-wins because the listings are
 * newest-first: if a duplicate id ever does arrive, the newer row is the one
 * worth believing.
 */
function byId<T extends PhasedRow>(rows: readonly T[]): Map<string, T> {
  const index = new Map<string, T>();
  for (const row of rows) {
    if (!index.has(row.id)) index.set(row.id, row);
  }
  return index;
}

/**
 * Rows that were moving in `previous` and have stopped in `next`.
 *
 * `previous` empty returns nothing at all — the first observation of a session
 * establishes the baseline and announces none of it.
 */
export function settledSince<T extends PhasedRow>(
  previous: readonly T[],
  next: readonly T[],
): PhaseTransition<T>[] {
  if (previous.length === 0) return [];
  const before = byId(previous);
  const transitions: PhaseTransition<T>[] = [];
  for (const row of next) {
    const prior = before.get(row.id);
    // Not seen before: a new row, whatever phase it is in. A new row that is
    // already settled was never observed moving, so its outcome is history.
    if (prior === undefined) continue;
    if (isInFlight(prior.phase) && isSettled(row.phase)) {
      transitions.push({ row, from: prior.phase, to: row.phase });
    }
  }
  return transitions;
}

/**
 * The transitions worth telling someone about: the ones that ended badly.
 *
 * `unknown` is excluded even though it is settled. An unjudgeable state is not
 * a failure, and a notice that cries failure over a state this build has never
 * heard of teaches people to ignore the notices.
 */
export function failuresSince<T extends PhasedRow>(
  previous: readonly T[],
  next: readonly T[],
): PhaseTransition<T>[] {
  return settledSince(previous, next).filter((t) => t.to === "settled_bad");
}

/**
 * The transitions that ended well.
 *
 * Separate from the failures rather than derived by negation, because the
 * complement of "ended badly" includes `unknown`, and calling that a success
 * is the exact mistake [`isVerdict`] exists to prevent.
 */
export function successesSince<T extends PhasedRow>(
  previous: readonly T[],
  next: readonly T[],
): PhaseTransition<T>[] {
  return settledSince(previous, next).filter((t) => t.to === "settled_ok");
}

/** Whether any row in a snapshot is still moving — the poll gate. */
export function anyInFlight(rows: readonly PhasedRow[]): boolean {
  return rows.some((row) => isInFlight(row.phase));
}

/**
 * Pass rate over the settled rows that carry a verdict, with its denominator.
 *
 * Returns `null` for the rate when nothing in the sample can be judged, rather
 * than 0 — the DORA cards learned this the hard way: a rate of zero computed
 * over an empty sample renders identically to a measured total failure. The
 * `judged` count is what makes the number readable, so callers must show it.
 */
export function verdictRate(rows: readonly PhasedRow[]): {
  ratePct: number | null;
  judged: number;
  passed: number;
  failed: number;
  /** Rows excluded because they are in flight or unjudgeable. */
  unjudged: number;
} {
  let passed = 0;
  let failed = 0;
  let unjudged = 0;
  for (const row of rows) {
    if (!isVerdict(row.phase)) {
      unjudged += 1;
      continue;
    }
    if (row.phase === "settled_ok") passed += 1;
    else failed += 1;
  }
  const judged = passed + failed;
  return {
    ratePct: judged === 0 ? null : Math.round((passed / judged) * 1000) / 10,
    judged,
    passed,
    failed,
    unjudged,
  };
}
