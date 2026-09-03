import { invoke } from "@tauri-apps/api/core";
import { get, writable, type Readable } from "svelte/store";
import { createAsyncGuard, type AsyncGuard } from "../async/guard";
import { createRepoPanelCache, type RepoPanelCache } from "../panels/repoPanelCache";
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
  readonly error: string | null;
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
      const errorStr = err instanceof Error ? err.message : String(err);
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
      }));
    } catch {
      if (activePath !== path) return;
      update((s) => ({ ...s, knowledgeLoading: false }));
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
      }));
    } catch {
      if (activePath !== path) return;
      update((s) => ({ ...s, doraLoading: false }));
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
      }));
    } catch {
      // Silent degradation for ledger snapshot loading
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
    } catch {
      // Silent ledger write failure degradation
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

