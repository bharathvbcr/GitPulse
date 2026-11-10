import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { configureReadCancellation, configureSharedReads, withDeferralRetry } from "./invoke";
import { cancelRead, CANCELLABLE_READ_COMMANDS, SHARED_READ_COMMANDS } from "./sharedReads";

/**
 * A caller that has not aborted is never handed a cancellation as its result,
 * on any path by which it can attach to a read another caller started: the
 * shared-read join, and both joins onto a call waiting to retry after the
 * backend deferred it. These tests drive the interleavings where the read it
 * attaches to has already been told to stop.
 */

const CANCELLATION = { kind: "unclassified", message: "git read cancelled: its caller is gone" };
const DEFERRED = { kind: "deferred_under_load", message: "git log deferred under load after 1s" };

function backend() {
  const pending: Array<{ cmd: string; args: Record<string, unknown> | undefined; answer: (value: unknown) => void; fail: (error: unknown) => void }> = [];
  const naps: Array<() => void> = [];
  const raw = (cmd: string, args?: Record<string, unknown>) =>
    new Promise<unknown>((answer, fail) => {
      pending.push({ cmd, args, answer, fail });
    });
  const invoke = withDeferralRetry(raw as never, {
    random: () => 0,
    whenShown: () => Promise.resolve(),
    // The backoff before each retry waits for the test.
    sleep: () => new Promise<void>((wake) => naps.push(wake)),
  });
  const sent = (cmd: string) => pending.filter((call) => call.cmd === cmd);
  return { pending, invoke, sent, naps, cancels: () => sent("cmd_cancel_read") };
}

const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 5));

beforeAll(() => {
  configureSharedReads(SHARED_READ_COMMANDS);
  configureReadCancellation(CANCELLABLE_READ_COMMANDS, cancelRead);
});
afterAll(() => {
  configureSharedReads(new Set());
  configureReadCancellation(new Set(), null);
});

for (const [cmd, args] of [
  ["cmd_get_commit_diff", { repoPath: "/r", commitId: "x" }],
  ["cmd_get_commit_graph", { repoPath: "/r", maxCommits: 10 }],
] as const) {
  describe(`${cmd}: a caller returning to a read that was cancelled mid-retry`, () => {
    it("starts a fresh read and receives data, never the cancellation", async () => {
      const b = backend();
      // 1. The read is deferred and its retry attempt goes out under id R.
      const first = new AbortController();
      void b.invoke(cmd, { ...args }, { signal: first.signal }).catch(() => undefined);
      await settle();
      b.sent(cmd)[0].fail(DEFERRED);
      await settle();
      b.naps[0]();
      await settle();
      expect(b.sent(cmd)).toHaveLength(2);
      const retried = b.sent(cmd)[1].args?.requestId;
      expect(typeof retried).toBe("string");
      // 2. The caller gives up; the cancel for R goes out while that attempt runs.
      first.abort();
      await settle();
      expect(b.cancels().map((call) => call.args)).toEqual([{ requestId: retried }]);
      // 3. The user clicks back before the attempt answers.
      const returning = b.invoke<string>(cmd, { ...args }, { signal: new AbortController().signal });
      await settle();
      b.sent(cmd)[1].fail(CANCELLATION);
      await settle();
      expect(b.sent(cmd), "the returning caller needs a read of its own").toHaveLength(3);
      expect(b.sent(cmd)[2].args?.requestId).not.toBe(retried);
      b.sent(cmd)[2].answer("data");
      await expect(returning).resolves.toBe("data");
    });

    it("sends no attempt under an id that was cancelled when the cancel lands between attempts", async () => {
      const b = backend();
      const first = new AbortController();
      void b.invoke(cmd, { ...args }, { signal: first.signal }).catch(() => undefined);
      await settle();
      const original = b.sent(cmd)[0].args?.requestId;
      b.sent(cmd)[0].fail(DEFERRED);
      await settle();
      // The retry is waiting out its backoff; the caller leaves, the cancel is sent.
      first.abort();
      await settle();
      expect(b.cancels()).toHaveLength(1);
      // A caller wanting the same read arrives, then the backoff ends.
      const returning = b.invoke<string>(cmd, { ...args }, { signal: new AbortController().signal });
      await settle();
      for (const wake of b.naps) wake();
      await settle();
      const dead = b.cancels()[0].args?.requestId;
      expect(dead).toBe(original);
      for (const call of b.sent(cmd).slice(1)) {
        expect(call.args?.requestId, "an attempt was sent under a cancelled id").not.toBe(dead);
      }
      expect(b.sent(cmd).length).toBeGreaterThanOrEqual(2);
      b.sent(cmd).at(-1)?.answer("data");
      await expect(returning).resolves.toBe("data");
    });

    it("a caller that joined the retry before any cancel still gets the retry's answer", async () => {
      const b = backend();
      const first = new AbortController();
      void b.invoke(cmd, { ...args }, { signal: first.signal }).catch(() => undefined);
      await settle();
      b.sent(cmd)[0].fail(DEFERRED);
      await settle();
      const staying = b.invoke<string>(cmd, { ...args }, { signal: new AbortController().signal });
      await settle();
      first.abort();
      await settle();
      expect(b.cancels(), "a caller still waits for this read").toHaveLength(0);
      b.naps[0]();
      await settle();
      b.sent(cmd).at(-1)?.answer("data");
      await expect(staying).resolves.toBe("data");
    });
  });
}
