import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { configureReadCancellation, configureSharedReads, withDeferralRetryProbed } from "./invoke";
import { cancelRead, CANCELLABLE_READ_COMMANDS, SHARED_READ_COMMANDS } from "./sharedReads";

/**
 * A call leaves nothing behind once it has settled. `waitersOf` holds only live,
 * signalled callers of unsettled requests: a caller that can never leave (no
 * signal) is never entered in it, and an entry made for one that can is removed
 * when its call settles, so what the wrapper holds does not grow with the number
 * of calls made.
 */

const N = 500;
const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 5));

function backend() {
  const raw = (cmd: string) => Promise.resolve(cmd === "cmd_get_status" || cmd === "cmd_get_commit_graph" ? { ok: true } : null);
  const probed = withDeferralRetryProbed(raw as never, { random: () => 0, whenShown: () => Promise.resolve(), sleep: () => Promise.resolve() });
  return Object.assign(probed.invoke, { retained: probed.retained });
}

const none = { sharing: 0, retrying: 0, leaving: 0 };

beforeAll(() => {
  configureSharedReads(SHARED_READ_COMMANDS);
  configureReadCancellation(CANCELLABLE_READ_COMMANDS, cancelRead);
});
afterAll(() => {
  configureSharedReads(new Set());
  configureReadCancellation(new Set(), null);
});

describe("what the wrapper holds after its calls have settled", () => {
  it("is nothing for calls with no signal and no deadline", async () => {
    const invoke = backend();
    for (let i = 0; i < N; i += 1) {
      await invoke("cmd_get_status", { repoPath: "/r" });
      await invoke("cmd_some_write", { i });
    }
    expect(invoke.retained()).toEqual(none);
  });

  it("is nothing for calls with a signal that never aborts", async () => {
    const invoke = backend();
    const controller = new AbortController();
    for (let i = 0; i < N; i += 1) {
      await invoke("cmd_get_commit_graph", { repoPath: "/r", i }, { signal: controller.signal });
    }
    expect(invoke.retained()).toEqual(none);
  });

  it("is nothing for a call whose signal aborts after it settled", async () => {
    const invoke = backend();
    const controllers: AbortController[] = [];
    for (let i = 0; i < N; i += 1) {
      const controller = new AbortController();
      controllers.push(controller);
      await invoke("cmd_get_commit_graph", { repoPath: "/r", i }, { signal: controller.signal });
    }
    for (const controller of controllers) controller.abort();
    await settle();
    expect(invoke.retained()).toEqual(none);
  });

  it("is nothing for calls with only a deadline", async () => {
    const invoke = backend();
    for (let i = 0; i < N; i += 1) await invoke("cmd_get_status", { repoPath: "/r", i }, { deadlineMs: 60_000 });
    expect(invoke.retained()).toEqual(none);
  });

  it("is nothing once callers that joined one request, or another's retry, have all settled", async () => {
    const pending: Array<(value: unknown) => void> = [];
    const failures: Array<(error: unknown) => void> = [];
    const naps: Array<() => void> = [];
    const raw = (cmd: string) =>
      cmd !== "cmd_get_commit_graph"
        ? Promise.resolve(null)
        : new Promise((resolve, reject) => {
            pending.push(resolve);
            failures.push(reject);
          });
    const probed = withDeferralRetryProbed(raw as never, {
      random: () => 0,
      whenShown: () => Promise.resolve(),
      sleep: () => new Promise<void>((wake) => naps.push(wake)),
    });
    const invoke = Object.assign(probed.invoke, { retained: probed.retained });
    const a = new AbortController();
    const b = new AbortController();
    const first = invoke("cmd_get_commit_graph", { repoPath: "/r" }, { signal: a.signal });
    await settle();
    failures[0]({ kind: "deferred_under_load", message: "deferred" });
    await settle();
    naps[0]();
    await settle();
    void invoke("cmd_some_write", { x: 1 });
    await settle();
    const second = invoke("cmd_get_commit_graph", { repoPath: "/r" }, { signal: b.signal });
    const third = invoke("cmd_get_commit_graph", { repoPath: "/r" });
    await settle();
    expect(invoke.retained().retrying).toBe(1);
    pending.at(-1)?.({ ok: true });
    await Promise.all([first, second, third]);
    pending.forEach((answer) => answer(null));
    await settle();
    expect(invoke.retained()).toEqual(none);
  });
});
