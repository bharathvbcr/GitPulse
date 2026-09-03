import { invoke } from "@tauri-apps/api/core";
import { get, writable, type Readable } from "svelte/store";
import { createAsyncGuard, type AsyncGuard } from "../async/guard";
import { createRepoPanelCache, type RepoPanelCache } from "../panels/repoPanelCache";
import { reportPanelError } from "../diagnostics/report";
import type {
  DoraReport,
  KnowledgeReport,
  PulseReport,
  PulseSnapshotEntry,
  PulseSnapshotInput,
} from "./types";

export const DEFAULT_PULSE_WINDOW = 5_000;

export interface PulseState {
  readonly report: PulseReport | null;
  readonly knowledge: KnowledgeReport | null;
  readonly dora: DoraReport | null;
  readonly snapshots: readonly PulseSnapshotEntry[];
  readonly loading: boolean;
  readonly knowledgeLoading: boolean;
  readonly doraLoading: boolean;
  /** Primary walk failure. Set here alone blanks the whole view. */
  readonly error: string | null;
  /**
   * Secondary-source failures. These used to be swallowed, so a blame walk
   * that errored rendered as a repository with no knowledge concentration and
   * a DORA scorecard of zeroes — a check that could not run wearing the face
   * of one that ran and found nothing.
   */
  readonly knowledgeError: string | null;
  readonly doraError: string | null;
  readonly snapshotsError: string | null;
  readonly currentRepoPath: string | null;
  readonly maxCommits: number;
}

export interface PulseStore extends Readable<PulseState> {
  load(repoPath: string, maxCommits?: number): Promise<void>;
  reload(): Promise<void>;
  loadKnowledge(maxFiles?: number): Promise<void>;
  loadDora(windowDays?: number): Promise<void>;
  recordSnapshot(totalLoc: number): Promise<void>;
  setLimit(maxCommits: number): Promise<void>;
  reset(): void;
}

export interface PulseStoreDeps {
  readonly fetchReport?: (repoPath: string, maxCommits?: number) => Promise<PulseReport>;
  readonly fetchKnowledge?: (repoPath: string, maxFiles?: number) => Promise<KnowledgeReport>;
  readonly fetchDora?: (repoPath: string, windowDays?: number) => Promise<DoraReport>;
  readonly fetchSnapshots?: (repoPath: string, limit?: number) => Promise<PulseSnapshotEntry[]>;
  readonly saveSnapshot?: (repoPath: string, snapshot: PulseSnapshotInput) => Promise<void>;
  readonly cache?: RepoPanelCache<PulseReport>;
}

export function defaultFetchReport(repoPath: string, maxCommits?: number): Promise<PulseReport> {
  return invoke<PulseReport>("cmd_get_pulse_report", {
    repoPath,
    maxCommits: maxCommits ?? DEFAULT_PULSE_WINDOW,
  });
}

export function defaultFetchKnowledge(
  repoPath: string,
  maxFiles?: number,
): Promise<KnowledgeReport> {
  return invoke<KnowledgeReport>("cmd_get_knowledge_report", {
    repoPath,
    maxFiles: maxFiles ?? 128,
  });
}

export function defaultFetchDora(repoPath: string, windowDays?: number): Promise<DoraReport> {
  return invoke<DoraReport>("cmd_get_dora_report", {
    repoPath,
    windowDays: windowDays ?? 90,
  });
}

export function defaultFetchSnapshots(
  repoPath: string,
  limit?: number,
): Promise<PulseSnapshotEntry[]> {
  return invoke<PulseSnapshotEntry[]>("cmd_get_pulse_snapshots", {
    repoPath,
    limit: limit ?? 30,
  });
}

export function defaultSaveSnapshot(
  repoPath: string,
  snapshot: PulseSnapshotInput,
): Promise<void> {
  return invoke<void>("cmd_record_pulse_snapshot", {
    repoPath,
    snapshot,
  });
}

export function createPulseStore(deps?: PulseStoreDeps): PulseStore {
  const fetcher = deps?.fetchReport ?? defaultFetchReport;
  const knowledgeFetcher = deps?.fetchKnowledge ?? defaultFetchKnowledge;
  const doraFetcher = deps?.fetchDora ?? defaultFetchDora;
  const snapshotsFetcher = deps?.fetchSnapshots ?? defaultFetchSnapshots;
  const snapshotSaver = deps?.saveSnapshot ?? defaultSaveSnapshot;
  const cache = deps?.cache ?? createRepoPanelCache<PulseReport>();

  const initialState: PulseState = {
    report: null,
    knowledge: null,
    dora: null,
    snapshots: [],
    loading: false,
    knowledgeLoading: false,
    doraLoading: false,
    error: null,
    knowledgeError: null,
    doraError: null,
    snapshotsError: null,
    currentRepoPath: null,
    maxCommits: DEFAULT_PULSE_WINDOW,
  };

  const { subscribe, set, update } = writable<PulseState>(initialState);
  let inflight: AsyncGuard | null = null;
  let activePath: string | null = null;
  let activeLimit = DEFAULT_PULSE_WINDOW;

  async function load(repoPath: string, maxCommits?: number): Promise<void> {
    if (!repoPath) {
      reset();
      return;
    }

    const limit = maxCommits ?? activeLimit;
    const switchingRepo = activePath !== repoPath;
    activePath = repoPath;
    activeLimit = limit;

    const cached = cache.get(repoPath);

    update((s) => ({
      ...s,
      currentRepoPath: repoPath,
      maxCommits: limit,
      // Never render repo A's heatmap under repo B's name. Cached data for
      // *this* path is fine; leftover data from the previous path is not.
      report: cached ?? (switchingRepo ? null : s.report),
      knowledge: switchingRepo ? null : s.knowledge,
      dora: switchingRepo ? null : s.dora,
      snapshots: switchingRepo ? [] : s.snapshots,
      loading: true,
      knowledgeLoading: false,
      doraLoading: false,
      error: null,
      knowledgeError: null,
      doraError: null,
      snapshotsError: null,
    }));

    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;

    try {
      // Fire primary report fetch
      const report = await fetcher(repoPath, limit);
      if (!guard.isLive()) return;

      cache.set(repoPath, report);
      update((s) => ({
        ...s,
        report,
        loading: false,
        error: null,
      }));

      // Non-blocking secondary fetches for Tier 2/3
      void loadKnowledge();
      void loadDora();
      void loadSnapshots();
    } catch (err) {
      if (!guard.isLive()) return;
      // The reporter formats once, records into the diagnostics ring tagged
      // `pulse`, and hands back the banner text — so the failure is reachable
      // from the Diagnostics modal (with the backend log tail, where a panic
      // backtrace lands) instead of dying in this catch.
      const errorStr = reportPanelError("pulse", err, { severity: "error" });
      update((s) => ({
        ...s,
        loading: false,
        error: errorStr,
      }));
    }
  }

  async function loadKnowledge(maxFiles = 128): Promise<void> {
    if (!activePath) return;
    const path = activePath;
    update((s) => ({ ...s, knowledgeLoading: true }));
    try {
      const knowledge = await knowledgeFetcher(path, maxFiles);
      if (activePath !== path) return;
      update((s) => ({
        ...s,
        knowledge,
        knowledgeLoading: false,
        knowledgeError: null,
      }));
    } catch (err) {
      if (activePath !== path) return;
      const message = reportPanelError("pulse", err);
      update((s) => ({ ...s, knowledgeLoading: false, knowledgeError: message }));
    }
  }

  async function loadDora(windowDays = 90): Promise<void> {
    if (!activePath) return;
    const path = activePath;
    update((s) => ({ ...s, doraLoading: true }));
    try {
      const dora = await doraFetcher(path, windowDays);
      if (activePath !== path) return;
      update((s) => ({
        ...s,
        dora,
        doraLoading: false,
        doraError: null,
      }));
    } catch (err) {
      if (activePath !== path) return;
      const message = reportPanelError("pulse", err);
      update((s) => ({ ...s, doraLoading: false, doraError: message }));
    }
  }

  async function loadSnapshots(): Promise<void> {
    if (!activePath) return;
    const path = activePath;
    try {
      const snapshots = await snapshotsFetcher(path, 30);
      if (activePath !== path) return;
      update((s) => ({
        ...s,
        snapshots,
        snapshotsError: null,
      }));
    } catch (err) {
      if (activePath !== path) return;
      update((s) => ({ ...s, snapshotsError: reportPanelError("pulse", err) }));
    }
  }

  async function recordSnapshot(totalLoc: number): Promise<void> {
    if (!activePath) return;
    const path = activePath;
    const stateSnap = get({ subscribe });
    if (!stateSnap || !stateSnap.report) return;

    const today = new Date().toISOString().slice(0, 10);
    const input: PulseSnapshotInput = {
      day: today,
      total_commits: stateSnap.report.total_commits_scanned,
      total_loc: totalLoc,
      bus_factor: stateSnap.knowledge?.bus_factor ?? 0,
      coverage_pct: null,
      snapshot_json: JSON.stringify({
        commits: stateSnap.report.total_commits_scanned,
        half_life_days: stateSnap.knowledge?.half_life_days ?? 0,
      }),
    };

    try {
      await snapshotSaver(path, input);
      void loadSnapshots();
    } catch (err) {
      // A snapshot that never persisted must not look like one that did.
      update((s) => ({ ...s, snapshotsError: reportPanelError("pulse", err) }));
    }
  }

  async function reload(): Promise<void> {
    if (activePath) {
      await load(activePath, activeLimit);
    }
  }

  async function setLimit(maxCommits: number): Promise<void> {
    if (activePath) {
      await load(activePath, maxCommits);
    }
  }

  function reset(): void {
    inflight?.cancel();
    inflight = null;
    activePath = null;
    activeLimit = DEFAULT_PULSE_WINDOW;
    set(initialState);
  }

  return {
    subscribe,
    load,
    reload,
    loadKnowledge,
    loadDora,
    recordSnapshot,
    setLimit,
    reset,
  };
}

export const pulseStore = createPulseStore();

