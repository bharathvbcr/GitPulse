import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { toastStore } from "./toastStore";
import { diagnostics } from "../diagnostics/diagnostics";
import {
  admitAnyway,
  DEFAULT_IDLE,
  DEFAULT_LEASES,
  DEFAULT_THRESHOLDS,
  thresholdsAreDefault,
  ensureHostLoadSettings,
  hostLoadSettings,
  DEFAULT_HOST_LOAD_SETTINGS,
  updateHostLoadSettings,
  HOST_LOAD_PULSE_EVENT,
  hostLoadPulse,
  hostPressure,
  loadIdleReport,
  loadLeaseStatus,
  parkSession,
  pulseOf,
  refreshHostLoad,
  resetHostLoadForTests,
  saveHostLoadSettings,
  startHostLoad,
  terminateOrphan,
  type HostLoadPulse,
  type HostLoadStatus,
} from "./hostLoadStore";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("../platform", () => ({ isTauri: () => true }));

const calmGovernor: HostLoadStatus["governor"] = {
  verdict: {
    admit: true,
    shed: { background: false, reactive: false, cause: null },
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
    snapshot_path: "/x/host-load.v1",
    writer: { pid: 1, start_us: 2, version: "1.5.0", path: "/a/gitpulse" },
    writer_is_self: true,
    snapshot: null,
    procs: null,
    tick: { ticks: 1, overruns: 0, missed: 0, last_us: 1, max_us: 1, cpu_ns: 1 },
    problems: [],
    previous_boot: null,
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
    const stored = { enabled: false, govern_launches: true, hook_mode: "enforce" as const, leases: DEFAULT_LEASES, idle: DEFAULT_IDLE, thresholds: DEFAULT_THRESHOLDS };
    vi.mocked(invoke).mockResolvedValue(stored);
    await expect(saveHostLoadSettings(stored)).resolves.toEqual(stored);
    expect(invoke).toHaveBeenCalledWith("cmd_host_load_settings_save", { settings: stored });
  });

  it("saves one row's change on top of the latest settings, not a stale copy", async () => {
    const first = { enabled: true, govern_launches: true, hook_mode: "enforce" as const, leases: DEFAULT_LEASES, idle: DEFAULT_IDLE, thresholds: DEFAULT_THRESHOLDS };
    vi.mocked(invoke).mockResolvedValueOnce(first);
    await ensureHostLoadSettings();
    // A row saves a new swap rule; the store takes what the backend returns.
    const swap = { mode: "shed" as const, elevated_at: 0.4, severe_at: 0.9 };
    vi.mocked(invoke).mockImplementationOnce(async (_cmd, args) => (args as { settings: unknown }).settings);
    await updateHostLoadSettings({ leases: DEFAULT_LEASES, idle: DEFAULT_IDLE, thresholds: { ...DEFAULT_THRESHOLDS, swap } });
    // A toggle elsewhere carries only its own change: the swap edit survives.
    vi.mocked(invoke).mockImplementationOnce(async (_cmd, args) => (args as { settings: unknown }).settings);
    await updateHostLoadSettings({ govern_launches: false });
    expect(invoke).toHaveBeenLastCalledWith("cmd_host_load_settings_save", {
      settings: { enabled: true, govern_launches: false, hook_mode: "enforce", leases: DEFAULT_LEASES, idle: DEFAULT_IDLE, thresholds: { ...DEFAULT_THRESHOLDS, swap } },
    });
    expect(get(hostLoadSettings)?.thresholds.swap).toEqual(swap);
  });

  it("saves the command gate's mode on top of the latest settings, and defaults to enforcing", async () => {
    expect(DEFAULT_HOST_LOAD_SETTINGS.hook_mode).toBe("enforce");
    const first = { enabled: true, govern_launches: false, hook_mode: "enforce" as const, leases: DEFAULT_LEASES, idle: DEFAULT_IDLE, thresholds: DEFAULT_THRESHOLDS };
    vi.mocked(invoke).mockResolvedValueOnce(first);
    await ensureHostLoadSettings();
    vi.mocked(invoke).mockImplementationOnce(async (_cmd, args) => (args as { settings: unknown }).settings);
    await updateHostLoadSettings({ hook_mode: "advise" });
    expect(invoke).toHaveBeenLastCalledWith("cmd_host_load_settings_save", {
      settings: { ...first, hook_mode: "advise" },
    });
    expect(get(hostLoadSettings)?.hook_mode).toBe("advise");
  });

  it("reads the settings once however many rows ask at the same time", async () => {
    vi.mocked(invoke).mockClear();
    vi.mocked(invoke).mockResolvedValue({ enabled: true, govern_launches: true, hook_mode: "enforce", leases: DEFAULT_LEASES, idle: DEFAULT_IDLE, thresholds: DEFAULT_THRESHOLDS });
    await Promise.all([ensureHostLoadSettings(), ensureHostLoadSettings(), ensureHostLoadSettings()]);
    expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === "cmd_host_load_settings")).toHaveLength(1);
  });

  it("carries the governor's verdict from the status into the pulse", () => {
    const refusing = status({
      governor: {
        ...calmGovernor,
        verdict: {
          ...calmGovernor.verdict,
          admit: false,
          shed: { background: true, reactive: true, cause: null },
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

  it("reads build slots and idle sessions from their own commands and parks by terminal session id", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "cmd_host_load_leases") return { on: true, holders: [] } as never;
      if (cmd === "cmd_host_load_idle") return { sessions: [], idle: 0 } as never;
      if (cmd === "cmd_host_load_park") return "Parked." as never;
      return undefined as never;
    });
    expect(await loadLeaseStatus()).toMatchObject({ on: true });
    expect(await loadIdleReport()).toMatchObject({ idle: 0 });
    expect(await parkSession("term-1")).toBe("Parked.");
    expect(vi.mocked(invoke)).toHaveBeenCalledWith("cmd_host_load_park", { sessionId: "term-1" });
    // The terminal pill asks for the queue alone: no process-table read.
    await loadLeaseStatus(true);
    expect(vi.mocked(invoke)).toHaveBeenCalledWith("cmd_host_load_leases", { queueOnly: true });
    await loadLeaseStatus();
    expect(vi.mocked(invoke)).toHaveBeenCalledWith("cmd_host_load_leases", { queueOnly: false });
  });

  it("does not swallow a refused park: a No or a changed session rejects", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "cmd_host_load_park") throw new Error("not confirmed");
      return undefined as never;
    });
    await expect(parkSession("term-1")).rejects.toThrow("not confirmed");
  });

  it("keeps the automatic disk floors on a round trip that never touched them", async () => {
    // The backend shows the automatic floors as numbers for this computer.
    const shown = { ...DEFAULT_THRESHOLDS, disk: { mode: "refuse" as const, elevated_at: 32, severe_at: 16 } };
    const first = { enabled: true, govern_launches: true, hook_mode: "enforce" as const, thresholds: shown };
    vi.mocked(invoke).mockResolvedValueOnce(first);
    await ensureHostLoadSettings();
    vi.mocked(invoke).mockImplementationOnce(async (_cmd, args) => (args as { settings: unknown }).settings);
    const swap = { mode: "shed" as const, elevated_at: 0.4, severe_at: 0.9 };
    await updateHostLoadSettings({ thresholds: { ...get(hostLoadSettings)!.thresholds, swap } });
    const saved = get(hostLoadSettings)!.thresholds;
    expect(saved.disk_auto).toBe(true);
    expect(saved.disk_guard).toEqual(DEFAULT_THRESHOLDS.disk_guard);
    expect(saved.swap).toEqual(swap);
  });

  it("calls the thresholds default by what is stored, never by the numbers the automatic floors show", () => {
    expect(thresholdsAreDefault(DEFAULT_THRESHOLDS)).toBe(true);
    const shown = { ...DEFAULT_THRESHOLDS, disk: { mode: "refuse" as const, elevated_at: 32, severe_at: 16 } };
    expect(thresholdsAreDefault(shown)).toBe(true);
    expect(thresholdsAreDefault({ ...shown, disk: { ...shown.disk, mode: "shed" } })).toBe(false);
    // The same numbers a person set are theirs, and not the default.
    expect(thresholdsAreDefault({ ...shown, disk_auto: false })).toBe(false);
    expect(thresholdsAreDefault({ ...DEFAULT_THRESHOLDS, disk_auto: false })).toBe(false);
    const guard = { ...DEFAULT_THRESHOLDS.disk_guard, coverage: { mode: "refuse" as const, headroom_gib: 80 } };
    expect(thresholdsAreDefault({ ...shown, disk_guard: guard })).toBe(false);
  });
});
