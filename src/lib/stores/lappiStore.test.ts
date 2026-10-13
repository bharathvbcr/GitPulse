import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

vi.mock("../ipc/invoke", () => ({ invoke: vi.fn() }));
vi.mock("../platform", () => ({ isTauri: vi.fn() }));

const { invoke } = await import("../ipc/invoke");
const { isTauri } = await import("../platform");
const { DEFAULT_LAPPI_SETTINGS, lappi, refreshLappi, saveLappi } = await import("./lappiStore");

const invokeMock = vi.mocked(invoke);
const isTauriMock = vi.mocked(isTauri);

const ON = { ask_on_ambiguous_commit_type: true, record_caller_data: true };

beforeEach(() => {
  invokeMock.mockReset();
  isTauriMock.mockReset();
});

describe("Lappi switches in a browser preview", () => {
  it("reads the defaults without asking a backend that is not there", async () => {
    isTauriMock.mockReturnValue(false);
    const view = await refreshLappi();
    expect(view.settings).toEqual(DEFAULT_LAPPI_SETTINGS);
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("keeps a saved choice in the store for the session, and sends nothing", async () => {
    isTauriMock.mockReturnValue(false);
    const view = await saveLappi(ON);
    expect(view.settings).toEqual(ON);
    expect(get(lappi).settings).toEqual(ON);
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("starts with both switches off", () => {
    expect(DEFAULT_LAPPI_SETTINGS).toEqual({ ask_on_ambiguous_commit_type: false, record_caller_data: false });
  });
});

describe("Lappi switches in the app", () => {
  it("publishes what the backend reports", async () => {
    isTauriMock.mockReturnValue(true);
    const reported = {
      settings: ON,
      collect_forced_off: true,
      socket: "/tmp/lappi.sock",
      store: null,
      transport_supported: true,
    };
    invokeMock.mockResolvedValue(reported);
    await expect(refreshLappi()).resolves.toEqual(reported);
    expect(invokeMock).toHaveBeenCalledWith("cmd_lappi_settings");
    expect(get(lappi)).toEqual(reported);
  });

  it("saves through the backend and shows the backend's answer, not the request", async () => {
    isTauriMock.mockReturnValue(true);
    const answer = {
      settings: { ask_on_ambiguous_commit_type: true, record_caller_data: false },
      collect_forced_off: true,
      socket: null,
      store: null,
      transport_supported: true,
    };
    invokeMock.mockResolvedValue(answer);
    await expect(saveLappi(ON)).resolves.toEqual(answer);
    expect(invokeMock).toHaveBeenCalledWith("cmd_lappi_settings_save", { settings: ON });
    expect(get(lappi).settings.record_caller_data).toBe(false);
  });

  it("lets a failed save reach the caller and leaves the store as it was", async () => {
    isTauriMock.mockReturnValue(true);
    const before = get(lappi);
    invokeMock.mockImplementation(() => Promise.reject(new Error("save refused")));
    await expect(saveLappi(ON)).rejects.toThrow("save refused");
    expect(get(lappi)).toEqual(before);
  });
});
