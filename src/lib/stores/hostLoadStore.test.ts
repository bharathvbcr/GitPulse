import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { toastStore } from "./toastStore";
import {
  admitAnyway,
  HOST_LOAD_PULSE_EVENT,
  hostLoadPulse,
  hostPressure,
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
    vi.mocked(invoke).mockResolvedValue({ enabled: false, govern_launches: true });
    await expect(saveHostLoadSettings({ enabled: false, govern_launches: true })).resolves.toEqual({
      enabled: false,
      govern_launches: true,
    });
    expect(invoke).toHaveBeenCalledWith("cmd_host_load_settings_save", {
      settings: { enabled: false, govern_launches: true },
    });
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

  it("propagates a status read failure to the caller", async () => {
    vi.mocked(invoke).mockRejectedValue(new Error("backend down"));
    await expect(refreshHostLoad()).rejects.toThrow("backend down");
    expect(get(hostPressure)).toBe("unavailable");
  });
});
