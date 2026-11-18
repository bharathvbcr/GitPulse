import { describe, expect, it, vi } from "vitest";
import { fileHistoryQuery, loadFileHistory, MAX_FILE_HISTORY, WHITESPACE_PATH_REASON } from "./fileHistory";

const row = (i: number) => ({
  id: String(i).padStart(40, "0"),
  author_name: `author ${i}`,
  timestamp: 1_700_000_000 - i,
  summary: `change ${i}`,
});

function fakeInvoke(result: unknown, fail = false) {
  return vi.fn(async () => {
    if (fail) throw new Error("git log failed");
    return result;
  }) as unknown as <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
}

describe("file history", () => {
  it("asks the graph for the file's own history, path given literally", () => {
    expect(fileHistoryQuery("src/routes/[id]/+page.svelte")).toBe("path:src/routes/[id]/+page.svelte");
  });

  it("refuses a path the query grammar cannot express instead of searching messages", async () => {
    expect(fileHistoryQuery("docs/my notes.md")).toBeNull();
    expect(fileHistoryQuery("")).toBeNull();
    const invoke = fakeInvoke({ rows: [] });
    expect(await loadFileHistory(invoke, "/r", "docs/my notes.md", 10)).toEqual({
      status: "unsupported",
      reason: WHITESPACE_PATH_REASON,
    });
    expect(invoke).not.toHaveBeenCalled();
  });

  it("maps rows newest first and says when older commits were left out", async () => {
    const invoke = fakeInvoke({ rows: [row(0), row(1)], has_more: true });
    const history = await loadFileHistory(invoke, "/r", "a.ts", 2);
    expect(invoke).toHaveBeenCalledWith("cmd_get_commit_graph", {
      repoPath: "/r",
      maxCommits: 2,
      query: "path:a.ts",
      // From HEAD: with no revision the graph walks every branch and tag,
      // listing changes that were never part of this file's past.
      revision: "HEAD",
      skip: 0,
    });
    expect(history).toEqual({
      status: "loaded",
      hasMore: true,
      commits: [
        { id: row(0).id, author: "author 0", date: row(0).timestamp * 1000, message: "change 0" },
        { id: row(1).id, author: "author 1", date: row(1).timestamp * 1000, message: "change 1" },
      ],
    });
  });

  it("caps an oversized answer and reports it as more", async () => {
    const history = await loadFileHistory(fakeInvoke({ rows: [row(0), row(1), row(2)], has_more: false }), "/r", "a.ts", 2);
    expect(history).toMatchObject({ status: "loaded", hasMore: true });
    if (history.status === "loaded") expect(history.commits).toHaveLength(2);
  });

  it("bounds the request whatever the caller asks for", async () => {
    const invoke = fakeInvoke({ rows: [] });
    await loadFileHistory(invoke, "/r", "a.ts", 10_000);
    await loadFileHistory(invoke, "/r", "a.ts", Number.NaN);
    const asked = vi.mocked(invoke).mock.calls.map((call) => (call as unknown[])[1] as { maxCommits: number });
    expect(asked.map((a) => a.maxCommits)).toEqual([MAX_FILE_HISTORY, 1]);
  });

  it("keeps a failed read distinct from a file with no history", async () => {
    expect(await loadFileHistory(fakeInvoke({ rows: [] }), "/r", "a.ts", 5)).toEqual({ status: "loaded", commits: [], hasMore: false });
    const failed = await loadFileHistory(fakeInvoke(null, true), "/r", "a.ts", 5);
    expect(failed.status).toBe("error");
  });
});
