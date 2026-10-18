import { describe, expect, it, vi } from "vitest";
import { withDeferralRetry } from "./invoke";

const DEFERRAL = {
  kind: "deferred_under_load",
  message: "git status deferred under load after 2.013s: the git spawn rate limit admitted nothing sooner",
};

describe("deferral retry versus the caller's AbortSignal (adversarial round 2)", () => {
  it("an abort during the backoff stops the retry chain, so no further command is sent", async () => {
    vi.useFakeTimers();
    try {
      const raw = vi.fn(async () => {
        throw DEFERRAL;
      });
      const invoke = withDeferralRetry(raw as never, { random: () => 0, whenShown: () => Promise.resolve() });
      const controller = new AbortController();
      const outcome = invoke("cmd_x", { a: 1 }, { signal: controller.signal }).catch((e: unknown) => e);
      await vi.advanceTimersByTimeAsync(1_000);
      expect(raw).toHaveBeenCalledTimes(1);
      controller.abort();
      expect(await outcome).toMatchObject({ name: "AbortError" });
      // Far past the whole backoff (3 + 6 + 12 + 24 + 30 s).
      await vi.advanceTimersByTimeAsync(200_000);
      expect(raw).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("a caller that aborts does not stop a retry another caller is still waiting on", async () => {
    vi.useFakeTimers();
    try {
      let sent = 0;
      const raw = vi.fn(async () => {
        sent += 1;
        if (sent === 1) throw DEFERRAL;
        return "answer";
      });
      const invoke = withDeferralRetry(raw as never, { random: () => 0, whenShown: () => Promise.resolve() });
      const controller = new AbortController();
      const aborting = invoke("cmd_x", { a: 1 }, { signal: controller.signal }).catch((e: unknown) => e);
      await vi.advanceTimersByTimeAsync(10);
      const patient = invoke<string>("cmd_x", { a: 1 });
      controller.abort();
      expect(await aborting).toMatchObject({ name: "AbortError" });
      await vi.advanceTimersByTimeAsync(10_000);
      await expect(patient).resolves.toBe("answer");
      expect(raw).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("a deadline that has passed stops the retry chain too", async () => {
    vi.useFakeTimers();
    try {
      const raw = vi.fn(async () => {
        throw DEFERRAL;
      });
      const invoke = withDeferralRetry(raw as never, { random: () => 0, whenShown: () => Promise.resolve() });
      const outcome = invoke("cmd_x", { a: 1 }, { deadlineMs: 4_000 }).catch((e: unknown) => e);
      await vi.advanceTimersByTimeAsync(4_001);
      expect(await outcome).toMatchObject({ name: "DeadlineError" });
      // The first retry went out at 3 s, inside the 4 s deadline, and that is
      // the caller's to have; nothing goes out once the deadline has passed.
      const sentInTime = raw.mock.calls.length;
      expect(sentInTime).toBe(2);
      await vi.advanceTimersByTimeAsync(200_000);
      expect(raw).toHaveBeenCalledTimes(sentInTime);
    } finally {
      vi.useRealTimers();
    }
  });
});
