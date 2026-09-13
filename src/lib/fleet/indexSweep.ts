/**
 * Index every open repository in one pass.
 *
 * The automatic path indexes what you are looking at: the live gate refreshes
 * retained tabs, and initialization runs for the active one. That is the right
 * default — indexing is CPU-heavy and nobody wants their laptop rebuilding
 * eleven graphs because eleven tabs are open — but it leaves the fleet case
 * unserved. Someone who has just installed devmap, or who wants cross-repository
 * search to actually span the workspace, has no way to say "do all of them"
 * short of clicking through every tab.
 *
 * Deliberately an *action*, not a fifth scan family. The four scans measure a
 * repository and persist a number; this one writes state and answers
 * "indexed / already current / failed". Folding it into the metrics ledger
 * would mean inventing a metric for something that is not a measurement.
 *
 * Sequential on purpose. `devmap build` saturates the cores it is given, and
 * the kernel takes a per-repository build lock, so a parallel sweep would
 * contend for both and finish no sooner. Cancellation is therefore checked
 * between repositories: a build that has started is left to finish rather than
 * killed halfway through writing a store.
 */

import { buildDevmap, initializeDevcouncil } from "../codeintel/client";
import type { DevmapBuildOutcome, InitReport } from "../codeintel/types";

export interface IndexTarget {
  path: string;
  label: string;
}

/**
 * What became of one repository.
 *
 * `current` is kept apart from `indexed` because they are different facts: one
 * says work was done, the other says none was needed. Collapsing them would
 * make a sweep over an already-built fleet look identical to one that rebuilt
 * everything.
 */
export type IndexStatus = "indexed" | "current" | "failed" | "skipped";

export interface IndexOutcome {
  path: string;
  label: string;
  status: IndexStatus;
  /** Why, for everything except a plain success. */
  reason: string | null;
}

export interface IndexSweepReport {
  outcomes: IndexOutcome[];
  /** How many repositories the sweep set out to do, whatever it managed. */
  total: number;
  /** The user stopped it. Outcomes are then a prefix, never the whole fleet. */
  aborted: boolean;
}

export const DEVMAP_MISSING =
  "devmap is not installed, so there is nothing to build the index with";

export interface IndexSweepOptions {
  initialize?: (repoPath: string, openRepos: string[]) => Promise<InitReport>;
  build?: (repoPath: string) => Promise<DevmapBuildOutcome>;
  signal?: { aborted: boolean };
  onStart?: (target: IndexTarget) => void;
  onProgress?: (done: number, total: number, latest: IndexTarget) => void;
}

/**
 * Did the kernel say this build changed nothing?
 *
 * `report` is `unknown` on the wire because it is the CLI's own JSON, so this
 * reads it defensively: anything that is not literally `unchanged: true` is
 * treated as work done, which errs towards "indexed" rather than towards
 * claiming a repository was already current when we cannot tell.
 */
export function buildWasUnchanged(outcome: DevmapBuildOutcome): boolean {
  const report: unknown = outcome.report;
  if (!report || typeof report !== "object") return false;
  return (report as { unchanged?: unknown }).unchanged === true;
}

function outcomeFor(target: IndexTarget, built: DevmapBuildOutcome): IndexOutcome {
  if (!built.ok) {
    return {
      path: target.path,
      label: target.label,
      status: "failed",
      reason: built.timed_out
        ? "the build ran past its deadline"
        : built.stderr.trim() || "the build failed",
    };
  }
  return {
    path: target.path,
    label: target.label,
    status: buildWasUnchanged(built) ? "current" : "indexed",
    reason: null,
  };
}

/**
 * Run the sweep.
 *
 * Every target gets an outcome unless the user stopped it, so a caller can
 * always tell "not reached" from "nothing to do". The one early exit is a
 * missing devmap: the first repository that reports it ends the sweep, and
 * every remaining target is recorded as skipped *with that reason* rather than
 * being quietly dropped — a run that could not do anything must not read the
 * same as a fleet that needed nothing.
 */
export async function runIndexSweep(
  targets: readonly IndexTarget[],
  options: IndexSweepOptions = {},
): Promise<IndexSweepReport> {
  const initialize = options.initialize ?? initializeDevcouncil;
  const build = options.build ?? buildDevmap;
  const openRepos = targets.map((target) => target.path);
  const outcomes: IndexOutcome[] = [];
  const total = targets.length;

  for (let i = 0; i < targets.length; i += 1) {
    const target = targets[i];
    if (options.signal?.aborted) {
      return { outcomes, total, aborted: true };
    }
    options.onStart?.(target);
    try {
      const report = await initialize(target.path, openRepos);
      if (!report.devmap_available) {
        for (const remaining of targets.slice(i)) {
          outcomes.push({
            path: remaining.path,
            label: remaining.label,
            status: "skipped",
            reason: DEVMAP_MISSING,
          });
        }
        options.onProgress?.(outcomes.length, total, target);
        return { outcomes, total, aborted: false };
      }
      outcomes.push(outcomeFor(target, await build(target.path)));
    } catch (error) {
      outcomes.push({
        path: target.path,
        label: target.label,
        status: "failed",
        reason: error instanceof Error ? error.message : String(error),
      });
    }
    options.onProgress?.(outcomes.length, total, target);
  }
  return { outcomes, total, aborted: false };
}

export function countByStatus(report: IndexSweepReport): Record<IndexStatus, number> {
  const counts: Record<IndexStatus, number> = {
    indexed: 0,
    current: 0,
    failed: 0,
    skipped: 0,
  };
  for (const outcome of report.outcomes) counts[outcome.status] += 1;
  return counts;
}

/** Everything the sweep set out to do, done, with nothing to report. */
export function isCleanIndexSweep(report: IndexSweepReport): boolean {
  if (report.aborted) return false;
  if (report.outcomes.length !== report.total) return false;
  const counts = countByStatus(report);
  return counts.failed === 0 && counts.skipped === 0;
}

/** The first failure, for a message that names one concrete cause. */
export function firstIndexFailure(report: IndexSweepReport): IndexOutcome | null {
  return report.outcomes.find((outcome) => outcome.status === "failed") ?? null;
}

function repositories(count: number): string {
  return `${count} ${count === 1 ? "repository" : "repositories"}`;
}

/**
 * One line describing the sweep.
 *
 * Carries both numbers whenever they differ, because the whole point of the
 * button is a claim about the fleet: "Indexed 3 repositories" after a sweep of
 * eleven, two of which failed, is the kind of sentence that makes someone trust
 * an index that is not there.
 */
export function summarizeIndexSweep(report: IndexSweepReport): string {
  const counts = countByStatus(report);
  const reached = report.outcomes.length;

  if (report.aborted) {
    const head = `Stopped after ${reached} of ${repositories(report.total)}`;
    const built = counts.indexed > 0 ? ` — indexed ${counts.indexed}` : "";
    return `${head}${built}.`;
  }
  if (counts.skipped === report.total && report.total > 0) {
    // Nothing ran at all, and the reason is the same for every one of them.
    return `Indexed nothing — ${report.outcomes[0]?.reason ?? DEVMAP_MISSING}.`;
  }
  if (report.total === 0) return "No open repositories to index.";

  const parts: string[] = [];
  if (counts.current > 0) parts.push(`${counts.current} already current`);
  if (counts.failed > 0) parts.push(`${counts.failed} failed`);
  if (counts.skipped > 0) parts.push(`${counts.skipped} skipped`);

  const head =
    counts.indexed === report.total
      ? `Indexed ${repositories(report.total)}`
      : `Indexed ${counts.indexed} of ${repositories(report.total)}`;
  return parts.length > 0 ? `${head} — ${parts.join(", ")}.` : `${head}.`;
}
