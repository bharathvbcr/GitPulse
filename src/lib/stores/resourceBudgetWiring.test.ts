import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { COMPACT_BUDGETS } from "../resourceBudgets";
import { gpuPolicy, gpuSlots } from "../terminal/gpuRenderer";
import { graphStore } from "./graphStore";
import { refreshHostLoad, resetHostLoadForTests, type HostLoadStatus, type ResourceProfile } from "./hostLoadStore";
import { startResourceBudgets, stopResourceBudgetsForTests } from "./resourceBudgetWiring";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("../platform", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../platform")>()),
  isTauri: () => true,
}));

const profile = (tier: ResourceProfile["tier"], budgets: ResourceProfile["budgets"]): ResourceProfile => ({
  tier,
  basis: "memory",
  memory_bytes: 8 * 2 ** 30,
  logical_cores: 8,
  budgets,
});

/** Only the field the wiring reads; the store keeps whatever the backend sent. */
const statusWith = (p: ResourceProfile): HostLoadStatus =>
  ({
    platform: "governed",
    role: "writer",
    sensor: { state: "fresh", age_ms: 1, level: "normal", enabled: true, reason: null },
    pressure: "normal",
    governor: {
      verdict: {
        admit: true,
        shed: { background: false, reactive: false, cause: null },
        reason: "healthy",
        tier: "clear",
        binding: null,
        sensor: "fresh",
        age_ms: 1,
        message: "",
        warnings: [],
      },
      refusing_for_s: null,
      notice: false,
      largest_foreign: null,
      override_remaining_s: null,
    },
    profile: p,
    snapshot_path: null,
    writer: null,
    writer_is_self: false,
    snapshot: null,
    procs: null,
    tick: { ticks: 0, overruns: 0, missed: 0, last_us: 0, max_us: 0, cpu_ns: 0 },
    problems: [],
    previous_boot: null,
  }) satisfies HostLoadStatus;

const ROOMY = { gpu_contexts: 8, gpu_hide_grace_ms: 300_000, graph_cache_repositories: 6 };

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  resetHostLoadForTests();
  stopResourceBudgetsForTests();
  gpuSlots.setCap(COMPACT_BUDGETS.gpu_contexts);
  gpuPolicy.hideGraceMs = COMPACT_BUDGETS.gpu_hide_grace_ms;
  graphStore.setCacheLimit(COMPACT_BUDGETS.graph_cache_repositories);
});
afterEach(() => stopResourceBudgetsForTests());

describe("the profile sizes the terminal's GPU pool and the graph cache", () => {
  it("holds the compact budgets until the first status arrives", () => {
    startResourceBudgets();
    expect(gpuSlots.cap()).toBe(2);
    expect(gpuPolicy.hideGraceMs).toBe(15_000);
    expect(graphStore.cacheLimit()).toBe(2);
  });

  it("takes a larger budget at once, and a smaller one back", async () => {
    startResourceBudgets();
    vi.mocked(invoke).mockResolvedValueOnce(statusWith(profile("roomy", ROOMY)));
    await refreshHostLoad();
    expect(gpuSlots.cap()).toBe(8);
    expect(gpuPolicy.hideGraceMs).toBe(300_000);
    expect(graphStore.cacheLimit()).toBe(6);

    vi.mocked(invoke).mockResolvedValueOnce(statusWith(profile("compact", COMPACT_BUDGETS)));
    await refreshHostLoad();
    expect(gpuSlots.cap()).toBe(2);
    expect(gpuPolicy.hideGraceMs).toBe(15_000);
    expect(graphStore.cacheLimit()).toBe(2);
  });

  it("releases hidden contexts beyond a smaller cap when the profile shrinks", async () => {
    startResourceBudgets();
    vi.mocked(invoke).mockResolvedValueOnce(statusWith(profile("roomy", ROOMY)));
    await refreshHostLoad();
    const released: number[] = [];
    for (let n = 0; n < 4; n += 1) {
      expect(gpuSlots.take()).toBe(true);
      gpuSlots.idle(() => {
        released.push(n);
        gpuSlots.give();
      });
    }
    vi.mocked(invoke).mockResolvedValueOnce(statusWith(profile("compact", COMPACT_BUDGETS)));
    await refreshHostLoad();
    expect(released).toEqual([0, 1]);
    expect(gpuSlots.held()).toBe(2);
  });

  it("is safe to start twice and to receive the same profile twice", async () => {
    startResourceBudgets();
    startResourceBudgets();
    vi.mocked(invoke).mockResolvedValue(statusWith(profile("roomy", ROOMY)));
    await refreshHostLoad();
    await refreshHostLoad();
    expect(gpuSlots.cap()).toBe(8);
  });
});
