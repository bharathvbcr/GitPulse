import { describe, expect, it, vi } from "vitest";
import { createCommitPicker, type CommitFilesRead } from "./commitPick";
import type { CommitFileChange } from "../stores/graphStore";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}
const file = (path: string) => ({ path }) as unknown as CommitFileChange;

function setup(listFiles: (repo: string, commit: string) => Promise<CommitFileChange[]>) {
  const state = { repo: "/r" as string | null, reads: [] as CommitFilesRead[], opened: [] as string[] };
  const pick = createCommitPicker({
    repo: () => state.repo,
    listFiles,
    cached: () => null,
    remember: (read) => state.reads.push(read),
    openFile: (commit, path) => state.opened.push(`file:${commit}:${path}`),
    openCommit: (commit) => state.opened.push(`commit:${commit}`),
  });
  return { state, pick };
}

describe("ft-038e6ba5 commit quick picks", () => {
  it("ends on the last pick even when an earlier pick's file list answers last", async () => {
    const a = deferred<CommitFileChange[]>();
    const b = deferred<CommitFileChange[]>();
    const { state, pick } = setup((_r, commit) => (commit === "A" ? a.promise : b.promise));
    const first = pick("A");
    const second = pick("B");
    b.resolve([file("b.txt")]);
    expect(await second).toBe("opened");
    a.resolve([file("a.txt")]);
    expect(await first).toBe("stale");
    expect(state.opened).toEqual(["file:B:b.txt"]);
  });

  it("drops an answer that lands after a repository switch", async () => {
    const a = deferred<CommitFileChange[]>();
    const { state, pick } = setup(() => a.promise);
    const picked = pick("A");
    state.repo = "/other";
    a.resolve([file("a.txt")]);
    expect(await picked).toBe("stale");
    expect(state.opened).toEqual([]);
    expect(state.reads).toEqual([]);
  });

  it("a failed list opens nothing and is recorded as an error, not as no files", async () => {
    const { state, pick } = setup(async () => { throw new Error("git: bad object"); });
    expect(await pick("A")).toBe("failed");
    expect(state.opened).toEqual([]);
    expect(state.reads).toEqual([{ commitId: "A", files: null, error: "git: bad object" }]);
  });

  it("a retry after a failure opens the commit", async () => {
    const list = vi.fn<(r: string, c: string) => Promise<CommitFileChange[]>>()
      .mockRejectedValueOnce(new Error("busy"))
      .mockResolvedValueOnce([file("x.txt")]);
    const { state, pick } = setup(list);
    expect(await pick("A")).toBe("failed");
    expect(await pick("A")).toBe("opened");
    expect(state.opened).toEqual(["file:A:x.txt"]);
  });

  it("a commit that really has no files shows the whole commit", async () => {
    const { state, pick } = setup(async () => []);
    expect(await pick("M")).toBe("opened");
    expect(state.opened).toEqual(["commit:M"]);
  });

  it("uses a cached successful list without asking again", async () => {
    const list = vi.fn(async () => [file("n.txt")]);
    const opened: string[] = [];
    const pick = createCommitPicker({
      repo: () => "/r", listFiles: list, cached: () => [file("c.txt")], remember: () => {},
      openFile: (_c, p) => opened.push(p), openCommit: () => {},
    });
    expect(await pick("A")).toBe("opened");
    expect(list).not.toHaveBeenCalled();
    expect(opened).toEqual(["c.txt"]);
  });
});
