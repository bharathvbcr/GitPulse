import { describe, expect, it } from "vitest";
import { createGraphStore, type CommitGraphPayload, type InvokeFn } from "../graphStore";

const EMPTY: CommitGraphPayload = { rows: [], head_id: null, refs: [], has_more: false };

/** Graph reads that wait on the test, with the abort signal each was sent with. */
function backend(onAbort: "abort" | "cancellation" = "abort") {
  const reads: Array<{ query: unknown; signal: AbortSignal | undefined; answer: () => void }> = [];
  const invoke = ((cmd: string, args?: Record<string, unknown>, options?: { signal?: AbortSignal }) =>
    new Promise<unknown>((resolve, reject) => {
      if (cmd !== "cmd_get_commit_graph") return resolve(null);
      reads.push({ query: args?.query, signal: options?.signal, answer: () => resolve(EMPTY) });
      // What the backend's answer to a cancelled read looks like, when it
      // reaches a caller before that caller's own abort does.
      const refusal = onAbort === "cancellation" ? { kind: "unclassified", message: "git read cancelled: its caller is gone" } : new DOMException("aborted", "AbortError");
      options?.signal?.addEventListener("abort", () => reject(refusal), { once: true });
    })) as unknown as InvokeFn;
  return { reads, store: createGraphStore({ invoke }) };
}

describe("a newer graph load gives up the older walk", () => {
  it("aborts the older read once the newer one is issued, not before", async () => {
    const { reads, store } = backend();
    const older = store.loadGraph("/repo", "");
    expect(reads).toHaveLength(1);
    expect(reads[0].signal?.aborted).toBe(false);
    const newer = store.loadGraph("/repo", "fix");
    expect(reads).toHaveLength(2);
    expect(reads[0].signal?.aborted, "the older walk has no caller left").toBe(true);
    expect(reads[1].signal?.aborted).toBe(false);
    reads[1].answer();
    await Promise.all([older, newer]);
  });

  it("a cancellation reaching the given-up load is dropped, never shown as the graph's error", async () => {
    const { reads, store } = backend("cancellation");
    store.showRepo("/repo");
    const older = store.loadGraph("/repo", "");
    const newer = store.loadGraph("/repo", "fix");
    reads[1].answer();
    await Promise.all([older, newer]);
    let error: unknown = "unset";
    store.subscribe((state) => {
      error = state.error;
    })();
    expect(error ?? null).toBeNull();
  });

  it("closing the tab gives up the walk in flight", async () => {
    const { reads, store } = backend();
    const load = store.loadGraph("/repo", "");
    store.evict("/repo");
    expect(reads[0].signal?.aborted).toBe(true);
    await load;
  });

  it("another repository's load is left alone", async () => {
    const { reads, store } = backend();
    const first = store.loadGraph("/one", "");
    const second = store.loadGraph("/two", "");
    expect(reads.map((read) => read.signal?.aborted)).toEqual([false, false]);
    reads[0].answer();
    reads[1].answer();
    await Promise.all([first, second]);
  });
});
