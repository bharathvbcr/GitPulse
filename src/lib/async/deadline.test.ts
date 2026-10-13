import { afterEach, describe, expect, it, vi } from "vitest";
import { DeadlineError, isDeadlineError, withDeadline } from "./deadline";

afterEach(() => {
  vi.useRealTimers();
});

describe("withDeadline", () => {
  it("rejects without starting the work when the budget is already spent", async () => {
    const work = vi.fn(async () => "ok");
    await expect(withDeadline(work, 0, { message: "spent" })).rejects.toThrow("spent");
    expect(work).not.toHaveBeenCalled();
  });

  it("resolves with the work's answer and leaves no timer behind", async () => {
    vi.useFakeTimers();
    await expect(withDeadline(async () => "done", 250)).resolves.toBe("done");
    expect(vi.getTimerCount()).toBe(0);
  });

  it("rejects with a distinct DeadlineError when the work never answers", async () => {
    vi.useFakeTimers();
    const outcome = withDeadline(new Promise(() => undefined), 40, { message: "too slow" }).catch((e: unknown) => e);
    await vi.advanceTimersByTimeAsync(50);
    const error = await outcome;
    expect(isDeadlineError(error)).toBe(true);
    expect(error).toMatchObject({ name: "DeadlineError", message: "too slow", ms: 40 });
    expect(error).toBeInstanceOf(DeadlineError);
  });

  it("surfaces the work's own rejection unchanged", async () => {
    const failure = new Error("fetch failed");
    await expect(withDeadline(async () => Promise.reject(failure), 250)).rejects.toBe(failure);
  });

  it("ends the wait at once on abort, and consumes the late answer", async () => {
    vi.useFakeTimers();
    const controller = new AbortController();
    let late!: (value: string) => void;
    const outcome = withDeadline(new Promise<string>((resolve) => { late = resolve; }), 60_000, { signal: controller.signal }).catch((e: unknown) => e);
    await vi.advanceTimersByTimeAsync(10);
    controller.abort(new Error("switched away"));
    await expect(outcome).resolves.toMatchObject({ message: "switched away" });
    expect(vi.getTimerCount()).toBe(0);
    late("too late");
    await vi.advanceTimersByTimeAsync(1);
  });

  it("never starts the work for an already-aborted signal", async () => {
    const work = vi.fn(async () => "ok");
    const controller = new AbortController();
    controller.abort();
    await expect(withDeadline(work, 1000, { signal: controller.signal })).rejects.toMatchObject({ name: "AbortError" });
    expect(work).not.toHaveBeenCalled();
  });

  it("lets a caller map the timeout onto its own error type", async () => {
    vi.useFakeTimers();
    const outcome = withDeadline(new Promise(() => undefined), 5, { timeoutError: (m) => new RangeError(m) }).catch((e: unknown) => e);
    await vi.advanceTimersByTimeAsync(6);
    expect(await outcome).toBeInstanceOf(RangeError);
  });
});
