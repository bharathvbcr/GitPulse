import { describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { diagnostics } from "../diagnostics/diagnostics";
import { createRepoPanelCache } from "../panels/repoPanelCache";
import { createPulseStore, DEFAULT_PULSE_WINDOW } from "./pulseStore";
import type { PulseReport } from "./types";

function createMockReport(overrides: Partial<PulseReport> = {}): PulseReport {
  return {
    commits: [],
    top_files_by_churn: [],
    extensions: [],
    has_mailmap: false,
    total_commits_scanned: 0,
    truncated: false,
    payload_truncated: false,
    duration_ms: 12,
    ...overrides,
  };
}

describe("pulseStore", () => {
  it("initializes with empty state", () => {
    const store = createPulseStore();
    let state: any;
    store.subscribe((s) => (state = s))();
    expect(state.report).toBeNull();
    expect(state.loading).toBe(false);
    expect(state.error).toBeNull();
    expect(state.maxCommits).toBe(DEFAULT_PULSE_WINDOW);
  });

  it("loads report and updates state", async () => {
    const mockReport = createMockReport({ total_commits_scanned: 42 });
    const fetchReport = vi.fn().mockResolvedValue(mockReport);
    const store = createPulseStore({ fetchReport });

    let state: any;
    const unsub = store.subscribe((s) => (state = s));

    await store.load("/path/to/repo", 1000);

    expect(fetchReport).toHaveBeenCalledWith("/path/to/repo", 1000);
    expect(state.loading).toBe(false);
    expect(state.error).toBeNull();
    expect(state.report).toEqual(mockReport);

    unsub();
  });

  it("hydrates from cache synchronously before fetching", async () => {
    const cachedReport = createMockReport({ total_commits_scanned: 10 });
    const freshReport = createMockReport({ total_commits_scanned: 20 });
    const cache = createRepoPanelCache<PulseReport>();
    cache.set("/path/to/repo", cachedReport);

    let resolveFetch: (r: PulseReport) => void;
    const fetchPromise = new Promise<PulseReport>((resolve) => {
      resolveFetch = resolve;
    });
    const fetchReport = vi.fn().mockReturnValue(fetchPromise);

    const store = createPulseStore({ fetchReport, cache });

    const emitted: any[] = [];
    const unsub = store.subscribe((s) => emitted.push(s));

    const loadPromise = store.load("/path/to/repo");

    // First emission after load call should have cached report with loading=true
    const loadingState = emitted[emitted.length - 1];
    expect(loadingState.loading).toBe(true);
    expect(loadingState.report).toEqual(cachedReport);

    resolveFetch!(freshReport);
    await loadPromise;

    const finalState = emitted[emitted.length - 1];
    expect(finalState.loading).toBe(false);
    expect(finalState.report).toEqual(freshReport);

    unsub();
  });

  it("drops stale responses when repository switches in-flight", async () => {
    let resolveFirst: (r: PulseReport) => void;
    const firstPromise = new Promise<PulseReport>((resolve) => {
      resolveFirst = resolve;
    });

    const secondReport = createMockReport({ total_commits_scanned: 99 });
    const fetchReport = vi.fn().mockImplementation((path: string) => {
      if (path === "/first") return firstPromise;
      return Promise.resolve(secondReport);
    });

    const store = createPulseStore({ fetchReport });
    let state: any;
    const unsub = store.subscribe((s) => (state = s));

    const p1 = store.load("/first");
    const p2 = store.load("/second");

    await p2;
    expect(state.report).toEqual(secondReport);
    expect(state.currentRepoPath).toBe("/second");

    // Now resolve first promise late:
    const firstReport = createMockReport({ total_commits_scanned: 1 });
    resolveFirst!(firstReport);
    await p1;

    // Must NOT overwrite with first report:
    expect(state.report).toEqual(secondReport);
    expect(state.currentRepoPath).toBe("/second");

    unsub();
  });

  it("does not keep the previous repo's report while a new repo loads", async () => {
    const firstReport = createMockReport({ total_commits_scanned: 1 });
    let resolveSecond: (r: PulseReport) => void;
    const secondPromise = new Promise<PulseReport>((resolve) => {
      resolveSecond = resolve;
    });
    const fetchReport = vi.fn().mockImplementation((path: string) => {
      if (path === "/first") return Promise.resolve(firstReport);
      return secondPromise;
    });
    const store = createPulseStore({ fetchReport });
    let state: any;
    const unsub = store.subscribe((s) => (state = s));

    await store.load("/first");
    expect(state.report).toEqual(firstReport);

    const pending = store.load("/second");
    expect(state.currentRepoPath).toBe("/second");
    expect(state.loading).toBe(true);
    expect(state.report).toBeNull();

    resolveSecond!(createMockReport({ total_commits_scanned: 99 }));
    await pending;
    expect(state.report?.total_commits_scanned).toBe(99);
    unsub();
  });

  it("reset cancels an in-flight fetch so it cannot write after teardown", async () => {
    let resolveFetch: (r: PulseReport) => void;
    const fetchPromise = new Promise<PulseReport>((resolve) => {
      resolveFetch = resolve;
    });
    const fetchReport = vi.fn().mockReturnValue(fetchPromise);
    const store = createPulseStore({ fetchReport });
    let state: any;
    const unsub = store.subscribe((s) => (state = s));

    const pending = store.load("/path/to/repo");
    store.reset();
    expect(state.report).toBeNull();
    expect(state.currentRepoPath).toBeNull();

    resolveFetch!(createMockReport({ total_commits_scanned: 7 }));
    await pending;
    expect(state.report).toBeNull();
    expect(state.currentRepoPath).toBeNull();
    unsub();
  });

  it("captures fetch errors without crashing", async () => {
    const fetchReport = vi.fn().mockRejectedValue(new Error("Git log failed"));
    const store = createPulseStore({ fetchReport });

    let state: any;
    const unsub = store.subscribe((s) => (state = s));

    await store.load("/bad/path");

    expect(state.loading).toBe(false);
    expect(state.error).toContain("Git log failed");

    unsub();
  });

  it("loads knowledge report and dora report", async () => {
    const mockKnowledge = {
      scanned_files: 10,
      candidate_files: 15,
      scanned_lines: 500,
      bus_factor: 2,
      primary_authors: [],
      orphaned_files: [],
      age_distribution: {
        fresh_lines: 100,
        recent_lines: 100,
        maturing_lines: 100,
        legacy_lines: 100,
        ancient_lines: 100,
        total_lines: 500,
      },
      half_life_days: 90,
      truncated: false,
      duration_ms: 15,
    };
    const mockDora = {
      deploy_frequency_per_week: 2.5,
      deploy_rating: "High",
      total_releases: 12,
      median_lead_time_hours: 4.5,
      lead_time_rating: "Elite",
      change_failure_rate_pct: 5.0,
      is_cfr_approximation: true,
      mttr_hours: 2.0,
      is_mttr_approximation: true,
      window_days: 90,
    };

    const fetchReport = vi.fn().mockResolvedValue(createMockReport());
    const fetchKnowledge = vi.fn().mockResolvedValue(mockKnowledge);
    const fetchDora = vi.fn().mockResolvedValue(mockDora);
    const store = createPulseStore({ fetchReport, fetchKnowledge, fetchDora });

    let state: any;
    store.subscribe((s) => (state = s));

    await store.load("/path/to/repo");
    await store.loadKnowledge();
    await store.loadDora();

    expect(state.knowledge).toEqual(mockKnowledge);
    expect(state.dora).toEqual(mockDora);
  });
});

/**
 * Secondary Pulse sources used to swallow their errors outright, so a blame
 * walk or a tag walk that failed rendered as a repository with a bus factor of
 * zero and no releases. Each failure must now be both visible in state and
 * recorded under `pulse` in the diagnostics ring the Diagnostics modal reads.
 */
describe("pulseStore failure reporting", () => {
  const ok = () => Promise.resolve(createMockReport());

  it("records a primary walk failure and surfaces it as the banner text", async () => {
    diagnostics.clear();
    const store = createPulseStore({
      fetchReport: () => Promise.reject(new Error("backend task panicked")),
      cache: createRepoPanelCache(),
    });

    await store.load("/repo");

    expect(get(store).error).toContain("backend task panicked");
    const entries = get(diagnostics).filter((e) => e.source === "pulse");
    expect(entries.length).toBeGreaterThan(0);
    expect(entries.some((e) => e.message.includes("backend task panicked"))).toBe(true);
  });

  it("keeps a failed blame scan distinct from an empty one", async () => {
    diagnostics.clear();
    const store = createPulseStore({
      fetchReport: ok,
      fetchKnowledge: () => Promise.reject(new Error("blame timed out")),
      fetchDora: () => Promise.reject(new Error("no such ref")),
      fetchSnapshots: () => Promise.resolve([]),
      cache: createRepoPanelCache(),
    });

    await store.load("/repo");
    await store.loadKnowledge();
    await store.loadDora();

    const state = get(store);
    // The primary report succeeded, so the page is not blanked...
    expect(state.error).toBeNull();
    expect(state.report).not.toBeNull();
    // ...but neither secondary failure is allowed to read as "no data".
    expect(state.knowledge).toBeNull();
    expect(state.knowledgeError).toContain("blame timed out");
    expect(state.dora).toBeNull();
    expect(state.doraError).toContain("no such ref");
    expect(get(diagnostics).filter((e) => e.source === "pulse").length).toBeGreaterThanOrEqual(2);
  });

  it("clears a stale failure once the source succeeds", async () => {
    diagnostics.clear();
    let fail = true;
    const store = createPulseStore({
      fetchReport: ok,
      fetchKnowledge: () =>
        fail
          ? Promise.reject(new Error("transient"))
          : Promise.resolve({
              scanned_files: 1,
              candidate_files: 1,
              scanned_lines: 10,
              bus_factor: 1,
              primary_authors: [],
              orphaned_files: [],
              age_distribution: {
                fresh_lines: 10,
                recent_lines: 0,
                maturing_lines: 0,
                legacy_lines: 0,
                ancient_lines: 0,
                total_lines: 10,
              },
              half_life_days: 1,
              truncated: false,
              duration_ms: 1,
            }),
      cache: createRepoPanelCache(),
    });

    await store.load("/repo");
    await store.loadKnowledge();
    expect(get(store).knowledgeError).toContain("transient");

    fail = false;
    await store.loadKnowledge();
    expect(get(store).knowledgeError).toBeNull();
    expect(get(store).knowledge).not.toBeNull();
  });
});
