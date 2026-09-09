import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { automaticQueueCount, automaticStatus, automaticUpdates, automationSettings, getAutomation, putAutomation, refreshAutomatic, wakeAutomatic, watchAutomatic } from "./client";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const native = vi.mocked(invoke);
const settings = { id: "profile", revision: 1, updated_at: 0, enabled: true, provider: null, model: null };
const status = { ok: true, state: "idle", reason: "", task_id: "", proposal_id: "", next_check_at: 0 };
beforeEach(() => { native.mockReset(); });

describe("automatic enhancement controls", () => {
  it("requires explicit settings and refuses incomplete or mismatched overrides", () => {
    expect(automationSettings(settings)).toEqual(settings);
    expect(automationSettings({ ...settings, enabled: false, provider: "local", model: "chosen" })).toMatchObject({ enabled: false, model: "chosen" });
    for (const change of [{ enabled: undefined }, { enabled: 1 }, { id: "other" }, { revision: 0 }, { provider: "local" }, { model: "chosen" }, { provider: "", model: "chosen" }, { provider: "local", model: " " }, { provider: "x".repeat(129), model: "chosen" }, { provider: "local", model: "x".repeat(513) }]) {
      expect(() => automationSettings({ ...settings, ...change })).toThrow();
    }
  });

  it("keeps coordinator state separate from provider outcomes and validates its envelope", () => {
    expect(automaticStatus({ ...status, state: "paused", reason: "Outcome uncertain", task_id: "task", proposal_id: "proposal" })).toMatchObject({ state: "paused", reason: "Outcome uncertain" });
    for (const change of [{ ok: false }, { state: "complete" }, { task_id: "../repo" }, { proposal_id: null }, { next_check_at: -1 }, { next_check_at: Number.MAX_SAFE_INTEGER + 1 }, { reason: "x".repeat(4097) }]) {
      expect(() => automaticStatus({ ...status, ...change })).toThrow();
    }
  });

  it("reads settings and a bounded queue without waking a model", async () => {
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, item: settings }));
    expect(await getAutomation()).toEqual(settings);
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, items: [{ id: "task", revision: 3, not_before_ms: 1234 }], total: 400, shown: 1, has_more: true, next_cursor: "opaque" }));
    expect(await automaticQueueCount()).toBe(400);
    expect(native.mock.calls).toEqual([
      ["cmd_workbench_request", { method: "automation.get", input: '{"id":"profile"}' }],
      ["cmd_workbench_request", { method: "automation.list", input: '{"limit":1}' }],
    ]);
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, items: [{ id: "task", revision: 0, not_before_ms: 0 }], total: 1, shown: 1, has_more: false, next_cursor: null }));
    await expect(automaticQueueCount()).rejects.toThrow();
  });

  it("coalesces a burst of wake hints and still delivers a hint that arrives in flight", async () => {
    let release: (value: string) => void = () => { throw new Error("Wake was not started"); };
    native.mockImplementationOnce(() => new Promise((resolve) => { release = resolve; }));
    native.mockResolvedValue(JSON.stringify(status));
    const first = wakeAutomatic();
    const hints = Array.from({ length: 50 }, () => wakeAutomatic());
    expect(native).toHaveBeenCalledTimes(1);
    release(JSON.stringify({ ...status, state: "checking" }));
    await Promise.all([first, ...hints]);
    expect(native).toHaveBeenCalledTimes(2);
    const seen: string[] = [];
    const stop = automaticUpdates.subscribe((update) => { if (update.status) seen.push(update.status.state); });
    expect(seen).toEqual(["idle"]);
    stop();
  });

  it("does not let an older status response overwrite a newer wake failure", async () => {
    let release: (value: string) => void = () => { throw new Error("Status was not requested"); };
    native.mockImplementationOnce(() => new Promise((resolve) => { release = resolve; }));
    const read = refreshAutomatic();
    const secondRead = refreshAutomatic();
    native.mockRejectedValueOnce({ code: "not_installed", message: "Manvi is unavailable" });
    await wakeAutomatic();
    release(JSON.stringify(status));
    await Promise.all([read, secondRead]);
    const errors: string[] = [];
    const stop = automaticUpdates.subscribe((update) => { errors.push(update.error); });
    expect(errors).toEqual(["Manvi is unavailable"]);
    expect(native).toHaveBeenCalledTimes(2);
    stop();
  });

  it("retains committed settings if the coordinator cannot wake and keeps exact mutation identities", async () => {
    const input = { id: "profile", request_id: "disable-once", expected_revision: 1, enabled: false };
    native.mockRejectedValueOnce({ code: "transport_error", message: "Reply lost" });
    await expect(putAutomation(input)).rejects.toMatchObject({ code: "transport_error" });
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, item: { ...settings, revision: 2, enabled: false } }));
    native.mockRejectedValueOnce({ code: "closed", message: "Worker closed" });
    expect(await putAutomation(input)).toMatchObject({ revision: 2, enabled: false });
    await vi.waitFor(() => expect(native).toHaveBeenCalledTimes(3));
    expect(native.mock.calls[0]).toEqual(native.mock.calls[1]);
    const errors: string[] = [];
    const stop = automaticUpdates.subscribe((update) => { errors.push(update.error); });
    expect(errors).toEqual(["Worker closed"]);
    stop();
  });

  it("shares one visible-board timer and stops polling for idle or hidden boards", async () => {
    vi.useFakeTimers();
    const first = watchAutomatic(), second = watchAutomatic();
    try {
      native.mockResolvedValueOnce(JSON.stringify({ ...status, state: "generating", task_id: "task", proposal_id: "proposal" }));
      await wakeAutomatic();
      expect(vi.getTimerCount()).toBe(1);
      first(); first();
      expect(vi.getTimerCount()).toBe(1);
      native.mockResolvedValueOnce(JSON.stringify(status));
      await vi.advanceTimersByTimeAsync(2000);
      expect(native).toHaveBeenCalledTimes(2);
      expect(vi.getTimerCount()).toBe(0);
      native.mockResolvedValueOnce(JSON.stringify({ ...status, state: "waiting" }));
      await wakeAutomatic();
      expect(vi.getTimerCount()).toBe(1);
      second();
      expect(vi.getTimerCount()).toBe(0);
      await vi.advanceTimersByTimeAsync(6000);
      expect(native).toHaveBeenCalledTimes(3);
    } finally { first(); second(); vi.useRealTimers(); }
  });
});
