/**
 * The Fleet dashboard's data, above the free tier.
 *
 * Tier 0 is not here: it is `repoStore.repoFacts()`, already in memory, read
 * reactively by the view. This store owns the two things that cost something —
 * the cheap sweep and the four expensive per-family scans — and the record of
 * what failed while trying.
 *
 * Three rules shape the whole file:
 *
 * 1. **A sweep that visited 11 of 24 repositories says so.** Every scan runs
 *    through `runAcrossRepos`, which keeps ok / failed / *skipped* strictly
 *    apart, and the report is kept rather than collapsed into a boolean.
 * 2. **A failure is recorded against the repository and family it happened
 *    to.** One repository's missing `npm` must not blank the other
 *    twenty-three, and it must not silently leave that cell looking unscanned.
 * 3. **Nothing expensive runs on its own.** Opening the grid triggers the
 *    cheap sweep and nothing else; storage, audits, coverage and language
 *    counts happen when someone asks for them, exactly like `autoRunCoverage`
 *    and `checkForUpdates` elsewhere in the app.
 */

import { invoke } from "@tauri-apps/api/core";
import { get, writable, type Readable } from "svelte/store";
import { createAsyncGuard, type AsyncGuard } from "../async/guard";
import { reportPanelError } from "../diagnostics/report";
import { formatError } from "../ui/formatError";
import {
  runAcrossRepos,
  type BulkRunReport,
  type RepoTarget,
  type RunOptions,
} from "../repos/workspaceOps";
import { fetchFleetSnapshot, recordFleetMetrics, scanRepoFamily, type InvokeFn } from "./client";
import type { ScanFailures } from "./row";
import { FAMILY_CONCURRENCY, type FleetSnapshot, type ScanFamily } from "./types";

/** How a family's sweep is progressing, for the button that started it. */
export interface ScanProgress {
  readonly family: ScanFamily;
  readonly done: number;
  readonly total: number;
}

export interface FleetState {
  readonly snapshot: FleetSnapshot | null;
  readonly snapshotLoading: boolean;
  /** A failure of the sweep as a whole. Per-repository failures live in the facets. */
  readonly snapshotError: string | null;
  /** Per repository, per family, why this session's scan failed. */
  readonly scanFailures: ScanFailures;
  /** The family currently sweeping, or null. One at a time, by design. */
  readonly scanning: ScanFamily | null;
  readonly progress: ScanProgress | null;
  /** The last finished sweep, kept so its skips and failures stay readable. */
  readonly lastRun: { readonly family: ScanFamily; readonly report: BulkRunReport } | null;
}

export interface FleetStore extends Readable<FleetState> {
  /** Runs the cheap sweep over these paths. Safe to call on every grid open. */
  refresh(repoPaths: readonly string[]): Promise<void>;
  /** Scans one repository for one family, then records and re-reads it. */
  scanOne(family: ScanFamily, repoPath: string): Promise<void>;
  /** Scans every listed repository for one family, bounded and cancellable. */
  scanAll(family: ScanFamily, targets: readonly RepoTarget[]): Promise<BulkRunReport | null>;
  /** Stops the sweep in flight before its next repository. */
  cancelScan(): void;
  /** Forgets a recorded failure, so a retry starts from a clean cell. */
  clearFailure(repoPath: string, family: ScanFamily): void;
  reset(): void;
}

export interface FleetStoreDeps {
  readonly invoke?: InvokeFn;
}

const INITIAL: FleetState = {
  snapshot: null,
  snapshotLoading: false,
  snapshotError: null,
  scanFailures: {},
  scanning: null,
  progress: null,
  lastRun: null,
};

function withFailure(
  failures: ScanFailures,
  repoPath: string,
  family: ScanFamily,
  reason: string,
): ScanFailures {
  return { ...failures, [repoPath]: { ...failures[repoPath], [family]: reason } };
}

function withoutFailure(
  failures: ScanFailures,
  repoPath: string,
  family: ScanFamily,
): ScanFailures {
  const forRepo = failures[repoPath];
  if (!forRepo || forRepo[family] === undefined) return failures;
  const next = { ...forRepo };
  delete next[family];
  const out = { ...failures };
  if (Object.keys(next).length === 0) delete out[repoPath];
  else out[repoPath] = next;
  return out;
}

export function createFleetStore(deps: FleetStoreDeps = {}): FleetStore {
  const call: InvokeFn = deps.invoke ?? invoke;
  const { subscribe, set, update } = writable<FleetState>(INITIAL);
  let inflight: AsyncGuard | null = null;
  let cancelToken: { aborted: boolean } | null = null;
  /** The paths the last refresh covered, so a scan can re-read them. */
  let lastPaths: readonly string[] = [];

  async function refresh(repoPaths: readonly string[]): Promise<void> {
    lastPaths = [...repoPaths];
    if (repoPaths.length === 0) {
      // Nothing to ask about. Clearing the error too, so a previous failure
      // does not haunt an empty workspace.
      update((s) => ({ ...s, snapshot: null, snapshotLoading: false, snapshotError: null }));
      return;
    }
    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    update((s) => ({ ...s, snapshotLoading: true }));
    try {
      const snapshot = await fetchFleetSnapshot(repoPaths, call);
      if (!guard.isLive()) return;
      update((s) => ({ ...s, snapshot, snapshotLoading: false, snapshotError: null }));
    } catch (err: unknown) {
      if (!guard.isLive()) return;
      // Reported through the panel reporter so the failure is reachable from
      // the Diagnostics modal rather than dying in this catch.
      const message = reportPanelError("fleet", err, { severity: "error" });
      update((s) => ({ ...s, snapshotLoading: false, snapshotError: message }));
    }
  }

  /**
   * Scans one repository and records the result.
   *
   * Throws on failure rather than swallowing: `scanAll` needs the throw to
   * mark that repository failed in its report, and `scanOne` catches it to
   * record the per-cell reason.
   */
  async function runOne(family: ScanFamily, repoPath: string): Promise<void> {
    const metrics = await scanRepoFamily(family, repoPath, call);
    await recordFleetMetrics(repoPath, metrics, call);
  }

  async function scanOne(family: ScanFamily, repoPath: string): Promise<void> {
    update((s) => ({ ...s, scanFailures: withoutFailure(s.scanFailures, repoPath, family) }));
    try {
      await runOne(family, repoPath);
    } catch (err: unknown) {
      const reason = formatError(err);
      reportPanelError("fleet", err);
      update((s) => ({ ...s, scanFailures: withFailure(s.scanFailures, repoPath, family, reason) }));
    }
    // Re-read whatever the sweep last covered so the new value lands with its
    // stamp, whether the scan succeeded or not.
    await refresh(lastPaths);
  }

  async function scanAll(
    family: ScanFamily,
    targets: readonly RepoTarget[],
  ): Promise<BulkRunReport | null> {
    // One family at a time. Two concurrent sweeps would each honor their own
    // concurrency cap and together blow through both.
    if (get({ subscribe }).scanning !== null) return null;
    if (targets.length === 0) return null;

    const token = { aborted: false };
    cancelToken = token;
    update((s) => ({
      ...s,
      scanning: family,
      progress: { family, done: 0, total: targets.length },
      // A repository about to be re-scanned starts from no recorded failure,
      // or a retry would look like it failed again before it ran.
      scanFailures: targets.reduce(
        (acc, target) => withoutFailure(acc, target.path, family),
        s.scanFailures,
      ),
    }));

    const options: RunOptions = {
      concurrency: FAMILY_CONCURRENCY[family],
      signal: token,
      onProgress: (done, total) => {
        update((s) => ({ ...s, progress: { family, done, total } }));
      },
    };

    const report = await runAcrossRepos(
      targets,
      async (target) => {
        await runOne(family, target.path);
      },
      options,
    );

    // Every failure is attributed to its own repository and family, so the
    // grid shows "could not read" in exactly the cells it applies to instead
    // of one banner over twenty-four intact rows.
    update((s) => {
      let failures = s.scanFailures;
      for (const result of report.results) {
        if (result.status === "failed") {
          failures = withFailure(failures, result.path, family, result.error ?? "the scan failed");
        }
      }
      return { ...s, scanFailures: failures, scanning: null, progress: null, lastRun: { family, report } };
    });
    cancelToken = null;
    await refresh(lastPaths);
    return report;
  }

  function cancelScan(): void {
    if (cancelToken) cancelToken.aborted = true;
  }

  return {
    subscribe,
    refresh,
    scanOne,
    scanAll,
    cancelScan,
    clearFailure: (repoPath, family) =>
      update((s) => ({ ...s, scanFailures: withoutFailure(s.scanFailures, repoPath, family) })),
    reset: () => {
      inflight?.cancel();
      inflight = null;
      cancelToken = null;
      lastPaths = [];
      set(INITIAL);
    },
  };
}

export const fleetStore = createFleetStore();
