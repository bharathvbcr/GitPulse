import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { toastStore } from "./toastStore";
import { diagnostics } from "../diagnostics/diagnostics";
import {
  admitAnyway,
  HOST_LOAD_PULSE_EVENT,
  PROFILE_RETRY_BASE_MS,
  PROFILE_RETRY_CAP_MS,
  hostLoadPulse,
  hostPressure,
  pulseOf,
  refreshHostLoad,
  resetHostLoadForTests,
  resourceProfile,
  saveHostLoadSettings,
  startHostLoad,
  terminateOrphan,
  type HostLoadPulse,
  type HostLoadStatus,
  type ResourceProfile,
} from "./hostLoadStore";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("../platform", () => ({ isTauri: () => true }));

const calmGovernor: HostLoadStatus["governor"] = {
  verdict: {
    admit: true,
    shed: { background: false, reactive: false },
    reason: "healthy",
    tier: "clear",
    binding: null,
    sensor: "fresh",
    age_ms: 500,
    message: "memory pressure normal (kernel level 1)",
    warnings: [],
  },
  refusing_for_s: null,
  notice: false,
  largest_foreign: null,
  override_remaining_s: null,
};

const calmPulse: HostLoadPulse["governor"] = {
  admit: true,
  shed_background: false,
  shed_reactive: false,
  binding: null,
  refusing_for_s: null,
  notice: false,
  overridden: false,
  largest_foreign: null,
};

function status(over: Partial<HostLoadStatus> = {}): HostLoadStatus {
  return {
    platform: "governed",
    role: "writer",
    sensor: { state: "fresh", age_ms: 500, level: "warn", enabled: true, reason: null },
    pressure: "warn",
    governor: calmGovernor,
    profile: {
      tier: "roomy",
      basis: "memory",
      memory_bytes: 32 * 2 ** 30,
      logical_cores: 10,
      budgets: { gpu_contexts: 8, gpu_hide_grace_ms: 300_000, graph_cache_repositories: 6 },
    },
    snapshot_path: "/x/host-load.v1",
    writer: { pid: 1, start_us: 2, version: "1.5.0", path: "/a/gitpulse" },
    writer_is_self: true,
    snapshot: null,
    procs: null,
    tick: { ticks: 1, overruns: 0, missed: 0, last_us: 1, max_us: 1, cpu_ns: 1 },
    problems: [],
    ...over,
  };
}

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  vi.mocked(listen).mockClear();
  resetHostLoadForTests();
});

describe("hostLoadStore", () => {
  it("is unavailable until something has been read — never a guessed normal", () => {
    expect(get(hostPressure)).toBe("unavailable");
    expect(get(hostLoadPulse)).toBeNull();
  });

  it("listens for the sampler's pulse once and seeds it from the status", async () => {
    vi.mocked(invoke).mockResolvedValue(status());
    startHostLoad();
    startHostLoad();
    expect(listen).toHaveBeenCalledTimes(1);
    expect(vi.mocked(listen).mock.calls[0][0]).toBe(HOST_LOAD_PULSE_EVENT);
    await vi.waitFor(() => expect(get(hostPressure)).toBe("warn"));
    expect(invoke).toHaveBeenCalledWith("cmd_host_load_status");

    const handler = vi.mocked(listen).mock.calls[0][1] as (e: { payload: HostLoadPulse }) => void;
    handler({
      payload: { ...pulseOf(status()), pressure: "critical", level: "critical" },
    } as never);
    expect(get(hostPressure)).toBe("critical");
  });

  it("rereads the status on a pulse when the startup read failed, so the profile is not left unknown", async () => {
    vi.mocked(invoke).mockRejectedValueOnce(new Error("backend not ready")).mockResolvedValue(status());
    startHostLoad();
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
    await vi.waitFor(() => expect(listen).toHaveBeenCalledTimes(1));
    // Let the rejected startup read settle, as the two seconds before the first pulse do.
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(get(resourceProfile), "nothing has been read yet").toBeNull();
    // The sampler's first pulse comes a tick (two seconds) after start-up.
    vi.useFakeTimers({ toFake: ["Date"] });
    try {
      vi.setSystemTime(Date.now() + PROFILE_RETRY_BASE_MS);

      const handler = vi.mocked(listen).mock.calls[0][1] as (e: { payload: HostLoadPulse }) => void;
      handler({ payload: pulseOf(status()) } as never);
      await vi.waitFor(() => expect(get(resourceProfile)?.tier).toBe("roomy"));
      expect(invoke).toHaveBeenCalledTimes(2);

      // Once a status is held, pulses do not each cost a read.
      handler({ payload: pulseOf(status()) } as never);
      handler({ payload: pulseOf(status()) } as never);
      expect(invoke).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("derives the pulse from a status exactly as the backend does", () => {
    const stale = status({
      sensor: { state: "stale_old", age_ms: 400_000, level: "critical", enabled: true, reason: null },
      pressure: "unavailable",
    });
    expect(pulseOf(stale)).toEqual({
      state: "stale_old",
      pressure: "unavailable",
      level: "critical",
      age_ms: 400_000,
      enabled: true,
      compressor_thrash: false,
      role: "writer",
      governor: calmPulse,
    });
    expect(pulseOf(status({ sensor: { ...stale.sensor, enabled: null } })).enabled).toBe(false);
  });

  it("sends the orphan's identity under the names the command takes, then rereads", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd: string) =>
      cmd === "cmd_host_load_terminate_orphan" ? "Sent SIGTERM" : status(),
    );
    await expect(terminateOrphan(4242, 1_791_000_000_000_000)).resolves.toBe("Sent SIGTERM");
    expect(invoke).toHaveBeenCalledWith("cmd_host_load_terminate_orphan", {
      pid: 4242,
      startUs: 1_791_000_000_000_000,
    });
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith("cmd_host_load_status"));
  });

  it("saves the kill switch and returns what was stored", async () => {
    vi.mocked(invoke).mockResolvedValue({ enabled: false, govern_launches: true, profile: "auto" });
    await expect(
      saveHostLoadSettings({ enabled: false, govern_launches: true, profile: "auto" }),
    ).resolves.toEqual({
      enabled: false,
      govern_launches: true,
      profile: "auto",
    });
    expect(invoke).toHaveBeenCalledWith("cmd_host_load_settings_save", {
      settings: { enabled: false, govern_launches: true, profile: "auto" },
    });
  });

  it("has no resource profile until a status arrives, then carries the backend's", async () => {
    expect(get(resourceProfile)).toBeNull();
    const derived: ResourceProfile = {
      tier: "compact",
      basis: "memory",
      memory_bytes: 8 * 2 ** 30,
      logical_cores: 8,
      budgets: { gpu_contexts: 2, gpu_hide_grace_ms: 15_000, graph_cache_repositories: 2 },
    };
    vi.mocked(invoke).mockResolvedValue(status({ profile: derived }));
    await refreshHostLoad();
    expect(get(resourceProfile)).toEqual(derived);
  });

  it("rereads the status after the profile is saved, so the page follows without a restart", async () => {
    const roomy: ResourceProfile = {
      tier: "roomy",
      basis: "override",
      memory_bytes: 8 * 2 ** 30,
      logical_cores: 8,
      budgets: { gpu_contexts: 8, gpu_hide_grace_ms: 300_000, graph_cache_repositories: 6 },
    };
    vi.mocked(invoke).mockImplementation(async (cmd: string) =>
      cmd === "cmd_host_load_settings_save"
        ? { enabled: true, govern_launches: true, profile: "roomy" }
        : status({ profile: roomy }),
    );
    await saveHostLoadSettings({ enabled: true, govern_launches: true, profile: "roomy" });
    await vi.waitFor(() => expect(get(resourceProfile)?.tier).toBe("roomy"));
    expect(invoke).toHaveBeenCalledWith("cmd_host_load_status");
  });

  it("carries the governor's verdict from the status into the pulse", () => {
    const refusing = status({
      governor: {
        ...calmGovernor,
        verdict: {
          ...calmGovernor.verdict,
          admit: false,
          shed: { background: true, reactive: true },
          reason: "pressure",
          tier: "severe",
          binding: "compressor_thrash",
        },
        refusing_for_s: 61,
        largest_foreign: 5_000,
        override_remaining_s: 30,
      },
    });
    expect(pulseOf(refusing).governor).toEqual({
      admit: false,
      shed_background: true,
      shed_reactive: true,
      binding: "compressor_thrash",
      refusing_for_s: 61,
      notice: false,
      overridden: true,
      largest_foreign: 5_000,
    });
  });

  it("asks for Admit anyway through the one command and rereads the status", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd: string) =>
      cmd === "cmd_host_load_admit_anyway" ? "admitted" : status(),
    );
    await expect(admitAnyway()).resolves.toBe("admitted");
    expect(invoke).toHaveBeenCalledWith("cmd_host_load_admit_anyway");
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith("cmd_host_load_status"));
    // A No in the native dialog rejects, and the caller sees it.
    vi.mocked(invoke).mockRejectedValueOnce("Not admitted");
    await expect(admitAnyway()).rejects.toBe("Not admitted");
  });

  it("raises the 30-minute notice once per run, with a one-click flip that saves the setting", async () => {
    toastStore.clear();
    vi.mocked(invoke).mockResolvedValue(status());
    startHostLoad();
    await vi.waitFor(() => expect(get(hostPressure)).toBe("warn"));
    const handler = vi.mocked(listen).mock.calls[0][1] as (e: { payload: HostLoadPulse }) => void;
    const refusing = (notice: boolean) =>
      ({
        payload: {
          ...pulseOf(status()),
          governor: { ...calmPulse, admit: false, binding: "memory_pressure", refusing_for_s: 1_800, notice, largest_foreign: 12 * 1024 ** 3 },
        },
      }) as never;
    handler(refusing(false));
    expect(get(toastStore)).toHaveLength(0);
    handler(refusing(true));
    handler(refusing(true));
    const toasts = get(toastStore);
    expect(toasts).toHaveLength(1);
    expect(toasts[0].message).toBe(
      "Refusing agent launches for 30 min under memory pressure; largest foreign footprint 12.0 GB.",
    );
    expect(toasts[0].duration).toBe(0);
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockImplementation(async (cmd: string) =>
      cmd === "cmd_host_load_settings" ? { enabled: true, govern_launches: true } : { enabled: true, govern_launches: false },
    );
    await toasts[0].action!.onClick();
    expect(invoke).toHaveBeenCalledWith("cmd_host_load_settings_save", {
      settings: { enabled: true, govern_launches: false },
    });
    // The run ending re-arms the notice for the next one.
    handler({ payload: { ...pulseOf(status()), governor: calmPulse } } as never);
    handler(refusing(true));
    expect(get(toastStore)).toHaveLength(2);
    toastStore.clear();
  });

  it("listens again on the next call when the first listen was refused, and says so once", async () => {
    const warn = vi.spyOn(diagnostics, "warn");
    vi.mocked(invoke).mockResolvedValue(status());
    vi.mocked(listen).mockRejectedValueOnce(new Error("no event bridge"));
    startHostLoad();
    expect(listen).toHaveBeenCalledTimes(1);
    await vi.waitFor(() => expect(warn).toHaveBeenCalledTimes(1));
    expect(String(warn.mock.calls[0][1])).toContain("no event bridge");

    startHostLoad();
    expect(listen).toHaveBeenCalledTimes(2);
    // The second attach holds: a third call does not listen again.
    await Promise.resolve();
    startHostLoad();
    expect(listen).toHaveBeenCalledTimes(2);
    expect(warn).toHaveBeenCalledTimes(1);
    warn.mockRestore();
  });

  it("shares one status read between concurrent callers", async () => {
    let release!: (value: HostLoadStatus) => void;
    vi.mocked(invoke).mockImplementation(
      () => new Promise<HostLoadStatus>((resolve) => (release = resolve)) as never,
    );
    const first = refreshHostLoad();
    const second = refreshHostLoad();
    expect(invoke).toHaveBeenCalledTimes(1);
    release(status());
    await expect(first).resolves.toEqual(status());
    await expect(second).resolves.toEqual(status());

    // Once it settles the next call reads again.
    vi.mocked(invoke).mockResolvedValue(status());
    await refreshHostLoad();
    expect(invoke).toHaveBeenCalledTimes(2);
  });

  it("lets a failed read go, so the next call can succeed", async () => {
    vi.mocked(invoke).mockRejectedValueOnce(new Error("backend down"));
    await expect(refreshHostLoad()).rejects.toThrow("backend down");
    vi.mocked(invoke).mockResolvedValue(status());
    await expect(refreshHostLoad()).resolves.toEqual(status());
  });

  it("rereads after a change instead of joining a read that began before it", async () => {
    const releases: Array<(value: HostLoadStatus) => void> = [];
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "cmd_host_load_admit_anyway") return "admitted" as never;
      return (await new Promise<HostLoadStatus>((resolve) => releases.push(resolve))) as never;
    });
    const running = refreshHostLoad();
    await admitAnyway();
    // The read that was running is the only one so far.
    expect(releases).toHaveLength(1);
    releases[0](status({ pressure: "normal" }));
    await running;
    await vi.waitFor(() => expect(releases).toHaveLength(2));
    releases[1](status({ pressure: "critical" }));
    await vi.waitFor(() => expect(get(hostPressure)).toBe("critical"));
  });

  it("propagates a status read failure to the caller", async () => {
    vi.mocked(invoke).mockRejectedValue(new Error("backend down"));
    await expect(refreshHostLoad()).rejects.toThrow("backend down");
    expect(get(hostPressure)).toBe("unavailable");
  });

  describe("rereading an unknown or unreadable profile", () => {
    type Handler = (e: { payload: HostLoadPulse }) => void;
    const unreadable = () =>
      status({
        profile: {
          tier: "standard",
          basis: "memory_unreadable",
          memory_bytes: null,
          logical_cores: 10,
          budgets: { gpu_contexts: 4, gpu_hide_grace_ms: 60_000, graph_cache_repositories: 4 },
        },
      });

    /** Fires one pulse per sampler tick (two seconds) for the given span of seconds. */
    async function pulseFor(handler: Handler, seconds: number): Promise<void> {
      for (let at = 0; at < seconds; at += 2) {
        vi.setSystemTime(Date.now() + 2_000);
        handler({ payload: pulseOf(status()) } as never);
        await new Promise((resolve) => setTimeout(resolve, 0));
      }
    }

    async function started(): Promise<Handler> {
      startHostLoad();
      await vi.waitFor(() => expect(listen).toHaveBeenCalledTimes(1));
      await new Promise((resolve) => setTimeout(resolve, 0));
      return vi.mocked(listen).mock.calls[0][1] as Handler;
    }

    beforeEach(() => vi.useFakeTimers({ toFake: ["Date"] }));
    afterEach(() => vi.useRealTimers());

    it("keeps rereading while the first status says the memory was unreadable, then stops once it can be read", async () => {
      vi.mocked(invoke).mockResolvedValue(unreadable());
      const handler = await started();
      expect(get(resourceProfile)?.basis).toBe("memory_unreadable");
      expect(invoke).toHaveBeenCalledTimes(1);

      await pulseFor(handler, PROFILE_RETRY_BASE_MS / 1000 + 2);
      expect(vi.mocked(invoke).mock.calls.length, "a reread was made once the backoff had passed").toBeGreaterThan(1);

      vi.mocked(invoke).mockResolvedValue(status());
      await pulseFor(handler, 120);
      expect(get(resourceProfile)?.tier).toBe("roomy");
      const settled = vi.mocked(invoke).mock.calls.length;
      await pulseFor(handler, 600);
      expect(invoke, "a readable profile is not reread by pulses").toHaveBeenCalledTimes(settled);
    });

    /** When each status read was made. */
    let reads: number[] = [];
    function recordReads(answer: () => Promise<HostLoadStatus>): void {
      reads = [];
      vi.mocked(invoke).mockImplementation(async () => {
        reads.push(Date.now());
        return (await answer()) as never;
      });
    }
    function gaps(times: number[]): number[] {
      return times.slice(1).map((at, index) => at - times[index]);
    }

    /** The waits double, level off at the cap, and the rereads never stop: at most one a minute, for good. */
    function expectBackoffThatLevelsOff(): void {
      const times = reads.slice();
      const waits = gaps(times);
      // Never denser than the first wait, however many pulses come.
      expect(Math.min(...waits)).toBeGreaterThanOrEqual(PROFILE_RETRY_BASE_MS);
      // Far fewer reads than the one-per-tick of 1,800 pulses, and a bounded rate at the end.
      expect(times.length).toBeLessThan(80);
      expect(waits.slice(-5).every((wait) => wait >= PROFILE_RETRY_CAP_MS)).toBe(true);
      // Still reading in the last ten minutes: the tier is never left stranded.
      expect(times.filter((at) => at > times[0] + 3_000_000).length).toBeGreaterThanOrEqual(5);
    }

    it("backs off to one reread a minute for an unreadable profile, and does not stop", async () => {
      recordReads(async () => unreadable());
      const handler = await started();
      await pulseFor(handler, 3_600);
      expectBackoffThatLevelsOff();
    });

    it("backs off to one reread a minute for a status read that always fails, and does not stop", async () => {
      recordReads(async () => {
        throw new Error("backend down");
      });
      const handler = await started();
      expect(get(resourceProfile)).toBeNull();
      await pulseFor(handler, 3_600);
      expectBackoffThatLevelsOff();
    });

    it("lets a person's change of setting start the retries over", async () => {
      const saved = { enabled: true, govern_launches: true, profile: "auto" } as const;
      reads = [];
      vi.mocked(invoke).mockImplementation(async (cmd: string) => {
        if (cmd === "cmd_host_load_settings_save") return saved as never;
        reads.push(Date.now());
        return unreadable() as never;
      });
      const handler = await started();
      await pulseFor(handler, 3_600);
      const leveled = reads.length;

      await saveHostLoadSettings(saved);
      await pulseFor(handler, 12);
      // The waits start again from the first one: several reads in twelve seconds,
      // where the leveled-off cadence would allow at most one in a minute.
      expect(reads.length - leveled, "retries resumed after the change").toBeGreaterThanOrEqual(3);
    });
  });
});
