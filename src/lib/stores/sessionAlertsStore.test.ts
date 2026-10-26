import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

vi.mock("../ipc/invoke", () => ({ invoke: vi.fn() }));
vi.mock("../platform", () => ({ isTauri: vi.fn() }));

type Store = typeof import("./sessionAlertsStore");
const invoke = vi.mocked((await import("../ipc/invoke")).invoke);
const isTauri = vi.mocked((await import("../platform")).isTauri);

/**
 * The store keeps its load state in module scope (that is what makes the first
 * read cheap for every later terminal), so each test gets a fresh module.
 */
async function freshStore(): Promise<Store> {
  vi.resetModules();
  return import("./sessionAlertsStore");
}

function viewWith(overrides: Partial<{ enabled: boolean; delivered: number }> = {}) {
  return {
    settings: {
      enabled: overrides.enabled ?? false,
      sound: true,
      shell_bell: true,
      configure_agents: false,
      hook_bridge: false,
      quiet_start: 22,
      quiet_end: 7,
    },
    status: {
      running: true,
      delivered: overrides.delivered ?? 3,
      suppressed_disabled: 0,
      suppressed_not_agent: 0,
      suppressed_attended: 0,
      suppressed_quiet: 0,
      coalesced: 0,
      resolved: 0,
      deferred: 0,
      displaced: 0,
      rate_limited: 0,
      dropped_queue: 0,
      dropped_scan: 0,
      failed: 0,
      bridge_rejected: 0,
      stale: 0,
      bridge_listening: true,
      bridge_path: "/tmp/bridge.sock",
      last_error: null,
    },
    bridge_supported: true,
  };
}

beforeEach(() => {
  invoke.mockReset();
  isTauri.mockReset();
});

describe("loading the alert settings", () => {
  it("answers the defaults in a browser preview and never asks a backend", async () => {
    isTauri.mockReturnValue(false);
    const store = await freshStore();
    const view = await store.loadSessionAlerts();
    expect(view.settings).toEqual(store.DEFAULT_SESSION_ALERT_SETTINGS);
    expect(invoke).not.toHaveBeenCalled();
    expect(store.sessionAlertSettings()).toEqual(store.DEFAULT_SESSION_ALERT_SETTINGS);
  });

  it("loads once and serves every later reader from the cache", async () => {
    isTauri.mockReturnValue(true);
    invoke.mockResolvedValue(viewWith({ enabled: true }));
    const store = await freshStore();
    await store.loadSessionAlerts();
    const second = await store.loadSessionAlerts();
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("cmd_session_alerts");
    expect(second.settings.enabled).toBe(true);
    expect(store.sessionAlertSettings().enabled).toBe(true);
    expect(get(store.sessionAlerts).status.delivered).toBe(3);
  });

  it("joins concurrent first reads into one round trip", async () => {
    isTauri.mockReturnValue(true);
    let release!: (view: ReturnType<typeof viewWith>) => void;
    invoke.mockReturnValue(new Promise((resolve) => (release = resolve as never)) as never);
    const store = await freshStore();
    const a = store.loadSessionAlerts();
    const b = store.loadSessionAlerts();
    release(viewWith());
    await Promise.all([a, b]);
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("keeps the defaults when the backend fails, and tries again on the next read", async () => {
    isTauri.mockReturnValue(true);
    invoke.mockRejectedValueOnce(new Error("backend down"));
    const store = await freshStore();
    const view = await store.loadSessionAlerts();
    expect(view.settings).toEqual(store.DEFAULT_SESSION_ALERT_SETTINGS);
    invoke.mockResolvedValueOnce(viewWith({ enabled: true }));
    await store.loadSessionAlerts();
    expect(invoke).toHaveBeenCalledTimes(2);
    expect(store.sessionAlertSettings().enabled).toBe(true);
  });
});

describe("refreshing and saving", () => {
  it("refresh re-reads the counters, and is a no-op outside the app", async () => {
    const store = await freshStore();
    isTauri.mockReturnValue(false);
    await store.refreshSessionAlerts();
    expect(invoke).not.toHaveBeenCalled();

    isTauri.mockReturnValue(true);
    invoke.mockResolvedValue(viewWith({ delivered: 9 }));
    const view = await store.refreshSessionAlerts();
    expect(view.status.delivered).toBe(9);
    expect(get(store.sessionAlerts).status.delivered).toBe(9);
    // A refresh counts as a load: the next read does not go back.
    await store.loadSessionAlerts();
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("save keeps the choice locally in a browser preview", async () => {
    isTauri.mockReturnValue(false);
    const store = await freshStore();
    const settings = { ...store.DEFAULT_SESSION_ALERT_SETTINGS, enabled: false, sound: true };
    const view = await store.saveSessionAlerts(settings);
    expect(view.settings).toEqual(settings);
    expect(store.sessionAlertSettings()).toEqual(settings);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("save publishes what the backend answered, not what was asked", async () => {
    isTauri.mockReturnValue(true);
    const store = await freshStore();
    invoke.mockResolvedValue(viewWith({ enabled: true }));
    const asked = { ...store.DEFAULT_SESSION_ALERT_SETTINGS, enabled: false };
    const view = await store.saveSessionAlerts(asked);
    expect(invoke).toHaveBeenCalledWith("cmd_session_alerts_save", { settings: asked });
    expect(view.settings.enabled).toBe(true);
    expect(store.sessionAlertSettings().enabled).toBe(true);
  });

  it("a failed save reaches the caller and leaves the settings alone", async () => {
    isTauri.mockReturnValue(true);
    const store = await freshStore();
    invoke.mockRejectedValueOnce(new Error("save refused"));
    await expect(store.saveSessionAlerts({ ...store.DEFAULT_SESSION_ALERT_SETTINGS, sound: true })).rejects.toThrow("save refused");
    expect(store.sessionAlertSettings()).toEqual(store.DEFAULT_SESSION_ALERT_SETTINGS);
  });
});

describe("attention and attendance", () => {
  it("reports that a question was answered, and only inside the app", async () => {
    const store = await freshStore();
    isTauri.mockReturnValue(false);
    store.reportAttentionAnswered("s1");
    expect(invoke).not.toHaveBeenCalled();

    isTauri.mockReturnValue(true);
    invoke.mockResolvedValue(undefined);
    store.reportAttentionAnswered("s1");
    expect(invoke).toHaveBeenCalledWith("cmd_session_attention_answered", { sessionId: "s1" });
  });

  it("logs rather than throws when the answer cannot be reported", async () => {
    const store = await freshStore();
    isTauri.mockReturnValue(true);
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    invoke.mockRejectedValueOnce(new Error("socket gone"));
    store.reportAttentionAnswered("s1");
    await vi.waitFor(() => expect(warn).toHaveBeenCalled());
    expect(String(warn.mock.calls[0]?.[0])).toContain("answered attention not reported");
    warn.mockRestore();
  });

  it("asks the backend what every session is standing on, and has nothing to say outside it", async () => {
    const store = await freshStore();
    isTauri.mockReturnValue(false);
    await expect(store.standingAttention()).resolves.toEqual([]);
    expect(invoke).not.toHaveBeenCalled();

    isTauri.mockReturnValue(true);
    invoke.mockResolvedValue([{ session: "s1" }]);
    await expect(store.standingAttention()).resolves.toEqual([{ session: "s1" }]);
    expect(invoke).toHaveBeenCalledWith("cmd_session_attention_standing");
  });

  it("pushes the visible sessions to the notifier inside the app, and records without pushing outside it", async () => {
    const store = await freshStore();
    isTauri.mockReturnValue(false);
    store.terminalAttendance.report("a", true);
    await store.terminalAttendance.flush();
    expect(invoke).not.toHaveBeenCalled();
    expect(store.terminalAttendance.visible()).toEqual(["a"]);

    isTauri.mockReturnValue(true);
    invoke.mockResolvedValue(undefined);
    store.terminalAttendance.report("b", true);
    await store.terminalAttendance.flush();
    expect(invoke).toHaveBeenCalledWith("cmd_session_alerts_visible", { sessionIds: ["a", "b"], backgroundIds: [] });
  });
});
