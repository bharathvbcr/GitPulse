import { describe, expect, it, vi } from "vitest";
import { withDeferralRetry } from "./invoke";

const DEFERRAL = {
  kind: "deferred_under_load",
  message: "git status deferred under load after 2.013s: the git spawn rate limit admitted nothing sooner",
};

describe("deferral retry versus the caller's AbortSignal (adversarial round 2)", () => {
  // `it.fails` passes while the break stands and fails once it is fixed, so a
  // fix has to flip it to `it`.
  it.fails("break: an abort during the backoff stops the retry chain, so no further command is sent", async () => {
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

  it.fails("break: a deadline that has passed stops the retry chain too", async () => {
    vi.useFakeTimers();
    try {
      const raw = vi.fn(async () => {
        throw DEFERRAL;
      });
      const invoke = withDeferralRetry(raw as never, { random: () => 0, whenShown: () => Promise.resolve() });
      const outcome = invoke("cmd_x", { a: 1 }, { deadlineMs: 4_000 }).catch((e: unknown) => e);
      await vi.advanceTimersByTimeAsync(4_001);
      expect(await outcome).toMatchObject({ name: "DeadlineError" });
      await vi.advanceTimersByTimeAsync(200_000);
      expect(raw).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });
});
