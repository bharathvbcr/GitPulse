import { describe, expect, it } from "vitest";
import type { ChangeKind, RepoChange } from "./events";
import { readRepoChange, routeRepoChange } from "./changeScope";

const REPO = "/repos/app";

function change(kinds: ChangeKind[], paths: string[] = []): RepoChange {
  return { kinds, paths, paths_truncated: false };
}

function route(touched: RepoChange | null, blocked = false, path: string | null = REPO) {
  const calls: string[] = [];
  routeRepoChange(path ?? undefined, touched, blocked, {
    repoState: (repo) => calls.push(`state:${repo ?? "current"}`),
    metrics: (repo, given) => calls.push(`metrics:${repo}:${given === touched}`),
    codeIndex: (repo) => calls.push(`index:${repo}`),
    docs: (repo) => calls.push(`docs:${repo}`),
  });
  return calls;
}

describe("routing a repository change to the views that depend on it", () => {
  it("a fetch refreshes repository state but neither the code index nor the documents", () => {
    expect(route(change(["refs", "objects"]))).toEqual([`state:${REPO}`, `metrics:${REPO}:true`]);
  });

  it("a source edit reaches the code index but not the document vault", () => {
    expect(route(change(["worktree"], ["src"]))).toEqual([
      `state:${REPO}`, `metrics:${REPO}:true`, `index:${REPO}`,
    ]);
  });

  it("a document edit reaches the vault", () => {
    expect(route(change(["worktree", "documents"], ["README.md"]))).toEqual([
      `state:${REPO}`, `metrics:${REPO}:true`, `index:${REPO}`, `docs:${REPO}`,
    ]);
  });

  it("an index write may have rewritten any tracked file, so it reaches every content view", () => {
    for (const kinds of [["index"], ["ignore"]] as ChangeKind[][]) {
      expect(route(change(kinds))).toEqual([
        `state:${REPO}`, `metrics:${REPO}:true`, `index:${REPO}`, `docs:${REPO}`,
      ]);
    }
  });

  it("an unknown change, or none, refreshes every view", () => {
    const all = [`state:${REPO}`, `metrics:${REPO}:true`, `index:${REPO}`, `docs:${REPO}`];
    expect(route(change(["config", "unknown"]))).toEqual(all);
    expect(route(null)).toEqual(all);
  });

  it("a repository awaiting trust refreshes its state and nothing derived from its files", () => {
    expect(route(change(["worktree", "documents"]), true)).toEqual([`state:${REPO}`]);
  });

  it("a pathless event still refreshes the current repository's state", () => {
    expect(route(change(["worktree"]), false, null)).toEqual(["state:current"]);
  });
});

describe("reading the change off the wire", () => {
  it("accepts the watcher's shape", () => {
    expect(readRepoChange({ kinds: ["refs", "documents"], paths: ["a.md"], paths_truncated: true }))
      .toEqual({ kinds: ["refs", "documents"], paths: ["a.md"], paths_truncated: true });
  });

  it("treats anything it cannot trust as no information, which refreshes everything", () => {
    for (const hostile of [
      undefined,
      null,
      "refs",
      {},
      { kinds: [], paths: [], paths_truncated: false },
      // A kind a newer backend added: this build does not know what it
      // touches, so it must not guess that nothing depends on it.
      { kinds: ["submodules"], paths: [], paths_truncated: false },
      { kinds: ["refs"], paths: [1], paths_truncated: false },
      { kinds: ["refs"], paths: [], paths_truncated: "no" },
    ]) {
      expect(readRepoChange(hostile)).toBeNull();
    }
  });
});
