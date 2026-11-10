import { describe, expect, it } from "vitest";
import { createDiffSelection, type DiffSelectionSpec } from "./diffSelection";
import type { RepoSession } from "./storeTypes";

/** A selection whose reads wait on the test, and which records each call's abort signal. */
function harness() {
  let session = {
    id: "t1",
    generation: 1,
    path: "/repo",
    selectedFilePath: null,
    selectedCommitId: null,
    selectedIsStaged: false,
    selectedIgnoreWhitespace: true,
    selectedDiff: null,
    viewSections: { history: "list" },
    activeTab: "work",
  } as unknown as RepoSession;
  const reads: Array<{ commitId: unknown; signal: AbortSignal | undefined; answer: (text: string) => void }> = [];
  const sel = createDiffSelection({
    invoke: ((_cmd: string, args: Record<string, unknown>, options?: { signal?: AbortSignal }) =>
      new Promise((resolve, reject) => {
        const read = { commitId: args.commitId, signal: options?.signal, answer: (text: string) => resolve({ text, truncated: false, truncation_reason: null }) };
        reads.push(read);
        options?.signal?.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")), { once: true });
      })) as never,
    activeSession: () => session,
    applyToSession: (_id, _gen, patch) => {
      session = { ...session, ...(typeof patch === "function" ? patch(session) : patch) };
      return true;
    },
    openEpoch: () => 0,
  });
  return { sel, reads, session: () => session };
}

const commit = (commitId: string): DiffSelectionSpec => ({
  read: (invoke, repoPath) => invoke("cmd_get_commit_diff", { repoPath, commitId }),
  filePath: null, commitId, isStaged: false, kind: "commit", history: "never",
});

describe("a newer diff selection gives up the older read", () => {
  it("aborts the older read once the newer one is issued, not before", async () => {
    const h = harness();
    const older = h.sel.select(commit("a"));
    expect(h.reads).toHaveLength(1);
    expect(h.reads[0].signal?.aborted).toBe(false);
    const newer = h.sel.select(commit("b"));
    expect(h.reads).toHaveLength(2);
    expect(h.reads[0].signal?.aborted, "the older read is the one given up").toBe(true);
    expect(h.reads[1].signal?.aborted).toBe(false);
    h.reads[1].answer("diff b");
    await Promise.all([older, newer]);
    expect(h.session().selectedDiff).toBe("diff b");
    expect(h.session().selectedCommitId).toBe("b");
  });

  it("supersede() gives up the read in flight", async () => {
    const h = harness();
    const pending = h.sel.select(commit("a"));
    h.sel.supersede();
    expect(h.reads[0].signal?.aborted).toBe(true);
    await pending;
    expect(h.session().selectedDiff).toBeNull();
    expect(h.session().selectedDiffError ?? null).toBeNull();
  });

  it("a read that finished is not given up when the next selection starts", async () => {
    const h = harness();
    const first = h.sel.select(commit("a"));
    h.reads[0].answer("diff a");
    await first;
    expect(h.reads[0].signal?.aborted).toBe(false);
    void h.sel.select(commit("b"));
    expect(h.reads[1].signal?.aborted).toBe(false);
  });
});
