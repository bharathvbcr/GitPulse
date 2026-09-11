import { afterEach, describe, expect, it, vi } from "vitest";
import { WORK_TIMEOUT_MS, withinDeadline, workTimeout } from "./request";

describe("workTimeout", () => {
  it("returns the default when a value is invalid", () => {
    expect(workTimeout()).toBe(WORK_TIMEOUT_MS);
    expect(workTimeout(NaN)).toBe(WORK_TIMEOUT_MS);
    expect(workTimeout(0)).toBe(WORK_TIMEOUT_MS);
    expect(workTimeout(-1)).toBe(WORK_TIMEOUT_MS);
  });

  it("caps long timeouts and enforces a positive lower bound", () => {
    expect(workTimeout(50_000)).toBe(WORK_TIMEOUT_MS);
    expect(workTimeout(0.5)).toBe(1);
  });

  it("keeps valid values as-is when already in range", () => {
    expect(workTimeout(5_000)).toBe(5_000);
  });
});

describe("withinDeadline", () => {
  it("rejects immediately when the deadline has already passed", async () => {
    await expect(withinDeadline(async () => "ok", Date.now() - 1)).rejects.toThrow(
      "Overview refresh deadline exceeded",
    );
  });

  it("resolves with request result when the deadline is not exceeded", async () => {
    const result = await withinDeadline(async () => "done", Date.now() + 250);
    expect(result).toBe("done");
  });

  it("rejects when request exceeds the remaining time budget", async () => {
    vi.useFakeTimers();
    try {
      vi.spyOn(Date, "now").mockReturnValue(0);
      const request = vi.fn(() => new Promise(() => undefined));
      const deadline = 40;
      const result = withinDeadline(request, deadline);
      const assertion = expect(result).rejects.toThrow("Overview refresh deadline exceeded");
      await vi.advanceTimersByTimeAsync(50);
      await assertion;
    } finally {
      vi.useRealTimers();
      vi.restoreAllMocks();
    }
  });

  it("surfaces request rejections without replacing them", async () => {
    const requestError = new Error("fetch failed");
    await expect(
      withinDeadline(async () => Promise.reject(requestError), Date.now() + 250),
    ).rejects.toBe(requestError);
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });
});
