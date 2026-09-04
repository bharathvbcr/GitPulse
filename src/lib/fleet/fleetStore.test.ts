import { get } from "svelte/store";
import { beforeEach, describe, expect, it } from "vitest";
import { diagnostics } from "../diagnostics/diagnostics";
import { createFleetStore } from "./fleetStore";
import type { FleetSnapshot } from "./types";

function snapshot(paths: string[]): FleetSnapshot {
  return {
    repos: paths.map((repo_path) => ({
      repo_path,
      ok: true,
      error: "",
      worktrees_ok: true,
      worktrees_error: "",
      worktrees: 1,
      agents: { sessions: 0, kinds: [] },
      last_commit_ok: true,
      last_commit_epoch: 1_757_000_000,
      metrics_ok: true,
      metrics_error: "",
      metrics: null,
    })),
    requested: paths.length,
    scanned: paths.length,
    truncated: false,
    duration_ms: 3,
  };
}

/** A fake invoke that records every call and answers per command. */
function fakeInvoke(handlers: Record<string, (args: Record<string, unknown>) => unknown>) {
  const calls: { cmd: string; args: Record<string, unknown> }[] = [];
  const fn = async <T,>(cmd: string, args: Record<string, unknown> = {}): Promise<T> => {
    calls.push({ cmd, args });
    const handler = handlers[cmd];
    if (!handler) throw new Error(`unexpected command ${cmd}`);
    return handler(args) as T;
  };
  return { fn, calls };
}

const LANGUAGES = {
  stats: [
    { language: "Rust", color_hex: "#000", category: "programming", code_lines: 900, file_count: 9, percentage: 90 },
    { language: "TOML", color_hex: "#111", category: "data", code_lines: 100, file_count: 2, percentage: 10 },
  ],
  truncated: false,
  scanned_files: 11,
  candidate_files: 11,
};

beforeEach(() => {
  diagnostics.clear();
});

describe("the cheap sweep", () => {
  it("asks for every path in one round trip", async () => {
    const { fn, calls } = fakeInvoke({ cmd_fleet_snapshot: () => snapshot(["/a", "/b"]) });
    const store = createFleetStore({ invoke: fn });
    await store.refresh(["/a", "/b"]);
    expect(calls).toHaveLength(1);
    expect(calls[0].cmd).toBe("cmd_fleet_snapshot");
    expect(calls[0].args).toEqual({ repoPaths: ["/a", "/b"] });
    expect(get(store).snapshot?.repos).toHaveLength(2);
  });

  it("asks nothing when nothing is open, and clears a stale error", async () => {
    const { fn, calls } = fakeInvoke({
      cmd_fleet_snapshot: () => {
        throw new Error("boom");
      },
    });
    const store = createFleetStore({ invoke: fn });
    await store.refresh(["/a"]);
    expect(get(store).snapshotError).toBeTruthy();
    await store.refresh([]);
    expect(calls).toHaveLength(1);
    expect(get(store).snapshotError).toBeNull();
  });

  it("records a sweep failure and leaves the snapshot untouched", async () => {
    let fail = false;
    const { fn } = fakeInvoke({
      cmd_fleet_snapshot: () => {
        if (fail) throw new Error("backend unavailable");
        return snapshot(["/a"]);
      },
    });
    const store = createFleetStore({ invoke: fn });
    await store.refresh(["/a"]);
    fail = true;
    await store.refresh(["/a"]);
    const state = get(store);
    expect(state.snapshotError).toContain("backend unavailable");
    // The last good sweep is still on screen; blanking it would replace real
    // rows with nothing because a refresh hiccupped.
    expect(state.snapshot?.repos).toHaveLength(1);
    expect(state.snapshotLoading).toBe(false);
  });

  it("routes a sweep failure into the diagnostics ring", async () => {
    const { fn } = fakeInvoke({
      cmd_fleet_snapshot: () => {
        throw new Error("backend unavailable");
      },
    });
    const store = createFleetStore({ invoke: fn });
    await store.refresh(["/a"]);
    const entries = get(diagnostics).filter((entry) => entry.source === "fleet");
    expect(entries.length).toBeGreaterThan(0);
  });

  it("lets the newest refresh win when two overlap", async () => {
    const resolvers: ((value: FleetSnapshot) => void)[] = [];
    const { fn } = fakeInvoke({
      cmd_fleet_snapshot: () => new Promise<FleetSnapshot>((resolve) => resolvers.push(resolve)),
    });
    const store = createFleetStore({ invoke: fn });
    const first = store.refresh(["/a"]);
    const second = store.refresh(["/b"]);
    // Settle them out of order: the abandoned first response must not paint.
    resolvers[1](snapshot(["/b"]));
    resolvers[0](snapshot(["/a"]));
    await Promise.all([first, second]);
    expect(get(store).snapshot?.repos[0].repo_path).toBe("/b");
  });
});

describe("scanning one repository", () => {
  it("runs the family's own command, records it, then re-reads", async () => {
    const { fn, calls } = fakeInvoke({
      cmd_fleet_snapshot: () => snapshot(["/a"]),
      cmd_get_language_stats: () => LANGUAGES,
      cmd_fleet_record_metrics: () => undefined,
    });
    const store = createFleetStore({ invoke: fn });
    await store.refresh(["/a"]);
    await store.scanOne("loc", "/a");
    expect(calls.map((c) => c.cmd)).toEqual([
      "cmd_fleet_snapshot",
      "cmd_get_language_stats",
      "cmd_fleet_record_metrics",
      "cmd_fleet_snapshot",
    ]);
    const recorded = calls[2].args.metrics as Record<string, unknown>;
    expect(recorded.loc).toBe(1000);
    expect(recorded.loc_language).toBe("Rust");
    // A storage scan is not part of this call and must stay null, or it would
    // blank whatever storage number was already recorded.
    expect(recorded.storage_bytes).toBeNull();
  });

  it("records the failure against that repository and family alone", async () => {
    const { fn } = fakeInvoke({
      cmd_fleet_snapshot: () => snapshot(["/a", "/b"]),
      cmd_scan_deps_health: () => {
        throw new Error("npm is not installed");
      },
    });
    const store = createFleetStore({ invoke: fn });
    await store.refresh(["/a", "/b"]);
    await store.scanOne("health", "/a");
    expect(get(store).scanFailures).toEqual({ "/a": { health: "npm is not installed" } });
  });

  it("clears the previous failure before retrying, so a success reads as one", async () => {
    let broken = true;
    const { fn } = fakeInvoke({
      cmd_fleet_snapshot: () => snapshot(["/a"]),
      cmd_storage_scan: () => {
        if (broken) throw new Error("scan timed out");
        return {
          repo_path: "/a",
          generated_at_epoch_secs: 1,
          is_bare: false,
          totals: {
            worktree_bytes: 1,
            git_dir_bytes: 2,
            grand_bytes: 3,
            build_artifacts_bytes: 4,
            cache_artifacts_bytes: 5,
          },
          git: {},
          artifacts: [],
          largest_files: [],
          worktrees: [],
          branches: {},
          scan: { elapsed_ms: 1, files_visited: 1, permission_denied: 0, truncated: false },
        };
      },
      cmd_fleet_record_metrics: () => undefined,
    });
    const store = createFleetStore({ invoke: fn });
    await store.refresh(["/a"]);
    await store.scanOne("storage", "/a");
    expect(get(store).scanFailures["/a"]?.storage).toBeTruthy();
    broken = false;
    await store.scanOne("storage", "/a");
    expect(get(store).scanFailures).toEqual({});
  });
});

describe("sweeping a family across the workspace", () => {
  const targets = [
    { path: "/a", label: "a" },
    { path: "/b", label: "b" },
    { path: "/c", label: "c" },
  ];

  it("reports failures per repository and keeps the rest", async () => {
    const { fn } = fakeInvoke({
      cmd_fleet_snapshot: () => snapshot(["/a", "/b", "/c"]),
      cmd_get_language_stats: (args) => {
        if (args.repoPath === "/b") throw new Error("language scan failed");
        return LANGUAGES;
      },
      cmd_fleet_record_metrics: () => undefined,
    });
    const store = createFleetStore({ invoke: fn });
    await store.refresh(["/a", "/b", "/c"]);
    const report = await store.scanAll("loc", targets);
    expect(report).not.toBeNull();
    expect(report?.succeeded).toBe(2);
    expect(report?.failed).toBe(1);
    // The shortfall is attributed, not averaged away.
    expect(get(store).scanFailures).toEqual({ "/b": { loc: "language scan failed" } });
    expect(get(store).lastRun?.family).toBe("loc");
  });

  it("keeps the finished report so its skips stay readable", async () => {
    const { fn } = fakeInvoke({
      cmd_fleet_snapshot: () => snapshot(["/a"]),
      cmd_get_language_stats: () => LANGUAGES,
      cmd_fleet_record_metrics: () => undefined,
    });
    const store = createFleetStore({ invoke: fn });
    await store.refresh(["/a"]);
    await store.scanAll("loc", targets);
    const run = get(store).lastRun;
    expect(run?.report.results).toHaveLength(3);
    expect(run?.report.results.every((r) => r.status === "ok")).toBe(true);
  });

  it("refuses a second sweep while one is running", async () => {
    let release: () => void = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const { fn } = fakeInvoke({
      cmd_fleet_snapshot: () => snapshot(["/a"]),
      cmd_get_language_stats: async () => {
        await gate;
        return LANGUAGES;
      },
      cmd_fleet_record_metrics: () => undefined,
    });
    const store = createFleetStore({ invoke: fn });
    await store.refresh(["/a"]);
    const first = store.scanAll("loc", targets);
    // Two sweeps would each honor their own concurrency cap and together
    // exceed both.
    expect(await store.scanAll("storage", targets)).toBeNull();
    release();
    await first;
    expect(get(store).scanning).toBeNull();
  });

  it("does nothing with no targets", async () => {
    const { fn, calls } = fakeInvoke({ cmd_fleet_snapshot: () => snapshot([]) });
    const store = createFleetStore({ invoke: fn });
    expect(await store.scanAll("loc", [])).toBeNull();
    expect(calls).toHaveLength(0);
  });

  it("reports the repositories a cancel never reached as skipped, not done", async () => {
    // Storage sweeps two at a time (FAMILY_CONCURRENCY), so with three
    // targets the third is only claimed once one of the first two settles —
    // which is the window a cancel has to land in.
    let release: () => void = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const scanned: string[] = [];
    const { fn } = fakeInvoke({
      cmd_fleet_snapshot: () => snapshot(["/a", "/b", "/c"]),
      cmd_storage_scan: async (args) => {
        scanned.push(String(args.repoPath));
        await gate;
        return {
          repo_path: args.repoPath,
          generated_at_epoch_secs: 1,
          is_bare: false,
          totals: {
            worktree_bytes: 1,
            git_dir_bytes: 2,
            grand_bytes: 3,
            build_artifacts_bytes: 4,
            cache_artifacts_bytes: 5,
          },
          git: {},
          artifacts: [],
          largest_files: [],
          worktrees: [],
          branches: {},
          scan: { elapsed_ms: 1, files_visited: 1, permission_denied: 0, truncated: false },
        };
      },
      cmd_fleet_record_metrics: () => undefined,
    });
    const store = createFleetStore({ invoke: fn });
    await store.refresh(["/a", "/b", "/c"]);
    const running = store.scanAll("storage", targets);
    await Promise.resolve();
    store.cancelScan();
    release();
    const report = await running;
    expect(report?.cancelled).toBe(true);
    // Whatever was not reached is skipped with a reason — never counted as
    // scanned just because the sweep ended.
    expect(scanned).toHaveLength(2);
    expect((report?.succeeded ?? 0) + (report?.skipped ?? 0)).toBe(3);
    expect(report?.skipped).toBe(1);
    for (const result of report?.results ?? []) {
      if (result.status === "skipped") expect(result.reason).toBeTruthy();
    }
  });

  it("clears a stale failure for every repository it is about to revisit", async () => {
    let broken = true;
    const { fn } = fakeInvoke({
      cmd_fleet_snapshot: () => snapshot(["/a"]),
      cmd_get_language_stats: () => {
        if (broken) throw new Error("nope");
        return LANGUAGES;
      },
      cmd_fleet_record_metrics: () => undefined,
    });
    const store = createFleetStore({ invoke: fn });
    await store.refresh(["/a"]);
    await store.scanAll("loc", targets);
    expect(Object.keys(get(store).scanFailures)).toHaveLength(3);
    broken = false;
    await store.scanAll("loc", targets);
    expect(get(store).scanFailures).toEqual({});
  });
});

describe("reset", () => {
  it("drops everything and cancels what is in flight", async () => {
    const { fn } = fakeInvoke({ cmd_fleet_snapshot: () => snapshot(["/a"]) });
    const store = createFleetStore({ invoke: fn });
    await store.refresh(["/a"]);
    store.reset();
    expect(get(store)).toEqual({
      snapshot: null,
      snapshotLoading: false,
      snapshotError: null,
      scanFailures: {},
      scanning: null,
      progress: null,
      lastRun: null,
    });
  });
});
