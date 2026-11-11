import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { configureReadCancellation, configureSharedReads, withDeferralRetry } from "./invoke";
import { cancelRead, CANCELLABLE_READ_COMMANDS, SHARED_READ_COMMANDS } from "./sharedReads";

/**
 * One request, one set of callers. A caller that attaches to a shared read is
 * attached to the callers of the request that was actually sent, however many
 * calls came between: the read's generation ending (a command that is not a
 * shared read) must not leave two sets standing for one request, or the caller
 * that joined the second is judged by the first's callers leaving.
 *
 * The sequence, for a read that was deferred and is being retried:
 *   A starts and is deferred; B asks after a generation ended and so cannot
 *   join A's call, but does join A's retry; C asks in B's generation and joins
 *   B's call. A and B then abort.
 */

const CANCELLATION = { kind: "read_cancelled", message: "git read cancelled: its caller is gone" };
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

type Outcome = { state: "pending" } | { state: "data"; value: unknown } | { state: "error"; error: unknown };
function watch(promise: Promise<unknown>): () => Outcome {
  let outcome: Outcome = { state: "pending" };
  promise.then(
    (value) => (outcome = { state: "data", value }),
    (error) => (outcome = { state: "error", error }),
  );
  return () => outcome;
}

for (const cmd of SHARED_READ_COMMANDS) {
  const args = { repoPath: "/r", maxCommits: 10 };
  const stoppable = CANCELLABLE_READ_COMMANDS.has(cmd);

  describe(`${cmd}: a caller that joined a second set of callers for one request`, () => {
    for (const cSignal of stoppable ? [true, false] : [false]) {
      it(`is not handed the cancellation when the first callers leave mid-attempt (C ${cSignal ? "with" : "without"} a signal)`, async () => {
        const b = backend();
        const a = new AbortController();
        const bb = new AbortController();
        const aOutcome = watch(b.invoke(cmd, { ...args }, { signal: a.signal }));
        aOutcome();
        await settle();
        b.sent(cmd)[0].fail(DEFERRED);
        await settle();
        b.naps[0]();
        await settle();
        expect(b.sent(cmd), "the retry attempt is on the wire").toHaveLength(2);
        // A command that is not a shared read ends the join generation.
        const other = b.invoke("cmd_some_write", { x: 1 });
        await settle();
        b.pending.find((call) => call.cmd === "cmd_some_write")?.answer(null);
        await other;
        const bOutcome = watch(b.invoke(cmd, { ...args }, { signal: bb.signal }));
        bOutcome();
        await settle();
        const c = cSignal ? new AbortController() : null;
        const cOutcome = watch(b.invoke(cmd, { ...args }, c ? { signal: c.signal } : undefined));
        await settle();
        a.abort();
        bb.abort();
        await settle();
        if (stoppable) expect(b.cancels(), "C is still waiting on this request").toHaveLength(0);
        const live = b.sent(cmd).filter((call) => call.args?.requestId !== undefined || !stoppable);
        // The attempt on the wire answers however the backend would: with the
        // cancellation if it was told to stop, else with data.
        const wire = b.sent(cmd)[1];
        if (b.cancels().length > 0) wire.fail(CANCELLATION);
        else wire.answer("data");
        await settle();
        void live;
        expect(cOutcome()).toEqual({ state: "data", value: "data" });
      });
    }

    it("still gets the retry's answer when the first callers leave during the backoff", async () => {
      const b = backend();
      const a = new AbortController();
      const bb = new AbortController();
      watch(b.invoke(cmd, { ...args }, { signal: a.signal }));
      await settle();
      b.sent(cmd)[0].fail(DEFERRED);
      await settle();
      const other = b.invoke("cmd_some_write", { x: 1 });
      await settle();
      b.pending.find((call) => call.cmd === "cmd_some_write")?.answer(null);
      await other;
      watch(b.invoke(cmd, { ...args }, { signal: bb.signal }));
      await settle();
      const cOutcome = watch(b.invoke(cmd, { ...args }));
      await settle();
      a.abort();
      bb.abort();
      await settle();
      b.naps[0]();
      await settle();
      expect(cOutcome(), "a caller still waiting is not answered with the deferral early").toEqual({ state: "pending" });
      b.sent(cmd).at(-1)?.answer("data");
      await settle();
      expect(cOutcome()).toEqual({ state: "data", value: "data" });
    });
  });
}
