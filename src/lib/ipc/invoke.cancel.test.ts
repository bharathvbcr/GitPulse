import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";
import { configureReadCancellation, configureSharedReads, withDeferralRetry } from "./invoke";
import { cancelRead, CANCELLABLE_READ_COMMANDS, SHARED_READ_COMMANDS } from "./sharedReads";

/** What the backend is sent to stop a read; the one command `cancelRead` names. */
const CANCEL_READ_COMMAND = "cmd_cancel_read";

/** A backend that answers each call when the test says so. */
function backend() {
  const pending: Array<{ cmd: string; args: Record<string, unknown> | undefined; answer: (value: unknown) => void; fail: (error: unknown) => void }> = [];
  const raw = vi.fn(
    (cmd: string, args?: Record<string, unknown>) =>
      new Promise<unknown>((answer, fail) => {
        pending.push({ cmd, args, answer, fail });
      }),
  );
  const invoke = withDeferralRetry(raw as never, { random: () => 0, whenShown: () => Promise.resolve() });
  const sent = (cmd: string) => pending.filter((call) => call.cmd === cmd);
  return { raw, pending, invoke, sent, cancels: () => sent(CANCEL_READ_COMMAND) };
}

/** Lets the cancel decision, which waits one task for a replacing caller to join, run. */
const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 5));

const GRAPH = "cmd_get_commit_graph";
const DIFF = "cmd_get_commit_diff";

beforeAll(() => {
  configureSharedReads(SHARED_READ_COMMANDS);
  configureReadCancellation(CANCELLABLE_READ_COMMANDS, cancelRead);
});
afterAll(() => {
  configureSharedReads(new Set());
  configureReadCancellation(new Set(), null);
});

describe("a read nobody waits for any longer is cancelled in the backend", () => {
  it("sends the cancel when the only caller aborts, under the id the read went out with", async () => {
    const b = backend();
    const click = new AbortController();
    void b.invoke(DIFF, { repoPath: "/r", commitId: "a" }, { signal: click.signal }).catch(() => undefined);
    await settle();
    const read = b.pending[0];
    expect(read.args).toMatchObject({ repoPath: "/r", commitId: "a" });
    const id = read.args?.requestId;
    expect(typeof id).toBe("string");
    expect(b.cancels()).toHaveLength(0);
    click.abort();
    await settle();
    expect(b.cancels()).toHaveLength(1);
    expect(b.cancels()[0].args).toEqual({ requestId: id });
  });

  it("waits for the last of several callers of one joined read, and not before", async () => {
    const b = backend();
    const first = new AbortController();
    const second = new AbortController();
    void b.invoke(GRAPH, { repoPath: "/r", maxCommits: 10 }, { signal: first.signal }).catch(() => undefined);
    void b.invoke(GRAPH, { repoPath: "/r", maxCommits: 10 }, { signal: second.signal }).catch(() => undefined);
    await settle();
    expect(b.sent(GRAPH)).toHaveLength(1);
    first.abort();
    await settle();
    expect(b.cancels(), "one caller leaving must not stop a read the other waits for").toHaveLength(0);
    second.abort();
    await settle();
    expect(b.cancels()).toHaveLength(1);
    expect(b.cancels()[0].args).toEqual({ requestId: b.sent(GRAPH)[0].args?.requestId });
  });

  it("a caller that never gives up keeps the read alive whoever else leaves", async () => {
    const b = backend();
    const click = new AbortController();
    void b.invoke(GRAPH, { repoPath: "/r", maxCommits: 10 }, { signal: click.signal }).catch(() => undefined);
    const patient = b.invoke(GRAPH, { repoPath: "/r", maxCommits: 10 });
    click.abort();
    await settle();
    expect(b.cancels()).toHaveLength(0);
    b.sent(GRAPH)[0].answer("rows");
    await expect(patient).resolves.toBe("rows");
  });

  it("a newer click on the same read joins it instead of letting it be cancelled", async () => {
    const b = backend();
    const older = new AbortController();
    const newer = new AbortController();
    void b.invoke(GRAPH, { repoPath: "/r", maxCommits: 10 }, { signal: older.signal }).catch(() => undefined);
    // The store issues the newer read, then gives up the older one.
    const replacement = b.invoke(GRAPH, { repoPath: "/r", maxCommits: 10 }, { signal: newer.signal });
    older.abort();
    await settle();
    expect(b.cancels()).toHaveLength(0);
    b.sent(GRAPH)[0].answer("rows");
    await expect(replacement).resolves.toBe("rows");
  });

  it("a newer click on a different read cancels only the older one", async () => {
    const b = backend();
    const older = new AbortController();
    void b.invoke(DIFF, { repoPath: "/r", commitId: "a" }, { signal: older.signal }).catch(() => undefined);
    const newer = new AbortController();
    void b.invoke(DIFF, { repoPath: "/r", commitId: "b" }, { signal: newer.signal });
    older.abort();
    await settle();
    expect(b.cancels()).toHaveLength(1);
    expect(b.cancels()[0].args).toEqual({ requestId: b.sent(DIFF)[0].args?.requestId });
    expect(b.sent(DIFF)[0].args?.requestId).not.toBe(b.sent(DIFF)[1].args?.requestId);
  });

  it("a cancel that loses the race with the read's own answer is never sent", async () => {
    const b = backend();
    const click = new AbortController();
    const read = b.invoke(DIFF, { repoPath: "/r", commitId: "a" }, { signal: click.signal });
    await settle();
    b.sent(DIFF)[0].answer("diff");
    await expect(read).resolves.toBe("diff");
    click.abort();
    await settle();
    expect(b.cancels()).toHaveLength(0);
  });

  it("a caller arriving after the cancel was sent starts its own read", async () => {
    const b = backend();
    const click = new AbortController();
    void b.invoke(GRAPH, { repoPath: "/r", maxCommits: 10 }, { signal: click.signal }).catch(() => undefined);
    await settle();
    click.abort();
    await settle();
    expect(b.cancels()).toHaveLength(1);
    const late = b.invoke(GRAPH, { repoPath: "/r", maxCommits: 10 }, { signal: new AbortController().signal });
    await settle();
    expect(b.sent(GRAPH)).toHaveLength(2);
    expect(b.sent(GRAPH)[1].args?.requestId).not.toBe(b.sent(GRAPH)[0].args?.requestId);
    b.sent(GRAPH)[1].answer("fresh");
    await expect(late).resolves.toBe("fresh");
  });
});

describe("a call without a signal, or to a read that cannot be stopped, is sent as it always was", () => {
  it("adds no id without a signal", async () => {
    const b = backend();
    void b.invoke(DIFF, { repoPath: "/r", commitId: "a" });
    expect(b.pending[0].args).toEqual({ repoPath: "/r", commitId: "a" });
  });

  it("adds no id to a read outside the list, and never cancels it", async () => {
    const b = backend();
    const click = new AbortController();
    void b.invoke("cmd_get_status", { repoPath: "/r" }, { signal: click.signal }).catch(() => undefined);
    await settle();
    expect(b.pending[0].args).toEqual({ repoPath: "/r" });
    click.abort();
    await settle();
    expect(b.cancels()).toHaveLength(0);
  });

  it("adds no id in a window that registers no cancellable reads", async () => {
    configureReadCancellation(new Set(), null);
    try {
      const b = backend();
      const click = new AbortController();
      void b.invoke(DIFF, { repoPath: "/r", commitId: "a" }, { signal: click.signal }).catch(() => undefined);
      await settle();
      expect(b.pending[0].args).toEqual({ repoPath: "/r", commitId: "a" });
      click.abort();
      await settle();
      expect(b.pending).toHaveLength(1);
    } finally {
      configureReadCancellation(CANCELLABLE_READ_COMMANDS, cancelRead);
    }
  });

  it("a failing cancel is not an error for anyone", async () => {
    const b = backend();
    const click = new AbortController();
    void b.invoke(DIFF, { repoPath: "/r", commitId: "a" }, { signal: click.signal }).catch(() => undefined);
    await settle();
    click.abort();
    await settle();
    b.cancels()[0].fail("backend gone");
    await settle();
    expect(b.cancels()).toHaveLength(1);
  });

  it("ids do not repeat across calls", async () => {
    const b = backend();
    for (let n = 0; n < 5; n += 1) {
      void b.invoke(DIFF, { repoPath: "/r", commitId: String(n) }, { signal: new AbortController().signal }).catch(() => undefined);
    }
    await settle();
    const ids = new Set(b.sent(DIFF).map((call) => call.args?.requestId));
    expect(ids.size).toBe(5);
    for (const id of ids) expect(String(id)).toMatch(/^r[0-9a-f]{16}-[0-9a-z]+$/);
  });
});
