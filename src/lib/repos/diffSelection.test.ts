import { describe, expect, it } from "vitest";
import { createDiffSelection, type DiffSelectionSpec } from "./diffSelection";
import type { RepoSession } from "./storeTypes";

function harness(answer: (cmd: string, args: Record<string, unknown>) => Promise<unknown>) {
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
  let epoch = 0;
  const applied: Partial<RepoSession>[] = [];
  const calls: Array<[string, Record<string, unknown>]> = [];
  const sel = createDiffSelection({
    invoke: (async (cmd: string, args: Record<string, unknown>) => {
      calls.push([cmd, args]);
      return answer(cmd, args);
    }) as never,
    activeSession: () => session,
    applyToSession: (_id, _gen, patch) => {
      const next = typeof patch === "function" ? patch(session) : patch;
      applied.push(next);
      session = { ...session, ...next };
      return true;
    },
    openEpoch: () => epoch,
  });
  return { sel, applied, calls, navigate: () => (epoch += 1), session: () => session };
}

const payload = { text: "diff", truncated: false, truncation_reason: null };
const base: DiffSelectionSpec = {
  read: (invoke, repoPath) => invoke("cmd_get_commit_diff", { repoPath, commitId: "abc" }),
  filePath: null, commitId: "abc", isStaged: false, kind: "commit", history: "never",
};

describe("createDiffSelection", () => {
  it("fetches, selects, and leaves the tab alone when history is never", async () => {
    const h = harness(async () => payload);
    await h.sel.select(base);
    expect(h.calls).toEqual([["cmd_get_commit_diff", { repoPath: "/repo", commitId: "abc" }]]);
    const last = h.applied.at(-1)!;
    expect(last).toMatchObject({ selectedCommitId: "abc", selectedDiff: "diff", selectionKind: "commit", selectedDiffPending: false });
    expect(last.activeTab).toBeUndefined();
  });

  it("a worktree read carries and records the session's whitespace preference", async () => {
    const h = harness(async () => payload);
    await h.sel.select({ ...base, read: (invoke, repoPath, ignoreWhitespace) => invoke("cmd_get_file_diff", { repoPath, filePath: "a.ts", isStaged: false, ignoreWhitespace }), filePath: "a.ts", commitId: null, kind: "file", history: "unlessNavigated", worktree: true });
    expect(h.calls[0][1]).toMatchObject({ ignoreWhitespace: true });
    expect(h.applied.at(-1)).toMatchObject({ selectedIgnoreWhitespace: true, activeTab: "history" });
  });

  it("unlessNavigated drops the tab switch when the user navigated meanwhile; always keeps it", async () => {
    for (const [history, expected] of [["unlessNavigated", undefined], ["always", "history"]] as const) {
      let release!: () => void;
      const h = harness(() => new Promise((r) => { release = () => r(payload); }));
      const pending = h.sel.select({ ...base, history });
      h.navigate();
      release();
      await pending;
      expect(h.applied.at(-1)!.activeTab).toBe(expected);
    }
  });

  it("drops a superseded answer, success or failure", async () => {
    const resolvers: Array<(v: unknown) => void> = [];
    const rejecters: Array<(e: unknown) => void> = [];
    const h = harness(() => new Promise((res, rej) => { resolvers.push(res); rejecters.push(rej); }));
    const first = h.sel.select(base);
    const second = h.sel.select({ ...base, commitId: "def", read: (invoke, repoPath) => invoke("cmd_get_commit_diff", { repoPath, commitId: "def" }) });
    rejecters[0](new Error("late failure"));
    resolvers[1](payload);
    await Promise.all([first, second]);
    expect(h.session().selectedCommitId).toBe("def");
    expect(h.session().selectedDiffError ?? null).toBeNull();
  });

  it("a failure clears the diff and reports on the selection; supersede orphans an in-flight read", async () => {
    const h = harness(async () => { throw new Error("boom"); });
    await h.sel.select(base);
    expect(h.session().selectedDiff).toBeNull();
    expect(String((h.session() as { selectedDiffError?: string }).selectedDiffError)).toContain("boom");

    let release!: () => void;
    const g = harness(() => new Promise((r) => { release = () => r(payload); }));
    const pending = g.sel.select(base);
    g.sel.supersede();
    release();
    await pending;
    expect(g.session().selectedDiff).toBeNull();
  });
});
