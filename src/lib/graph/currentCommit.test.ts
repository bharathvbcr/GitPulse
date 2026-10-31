import { describe, expect, it } from "vitest";
import {
  planCommitReconcile,
  resolveCurrentCommitId,
  type ReconcileInput,
} from "./currentCommit";

const rows = [{ id: "a" }, { id: "b" }, { id: "c" }];

describe("resolveCurrentCommitId", () => {
  it("fresh open: the repo store names nothing, so the graph's auto-selected commit wins", () => {
    expect(resolveCurrentCommitId(null, "a", rows)).toBe("a");
  });

  it("a filter excluded the clicked commit: the repo id is not in the rows, so the graph id wins", () => {
    expect(resolveCurrentCommitId("gone", "a", rows)).toBe("a");
  });

  it("inspect then mount: a repo id that is in the rows wins over the graph's auto-selection", () => {
    expect(resolveCurrentCommitId("c", "a", rows)).toBe("c");
  });

  it("names nothing when neither store selects a loaded commit", () => {
    expect(resolveCurrentCommitId(null, null, rows)).toBeNull();
    expect(resolveCurrentCommitId("gone", null, [])).toBeNull();
  });
});

function input(over: Partial<ReconcileInput>): ReconcileInput {
  return {
    repoId: null,
    graphId: null,
    repoInRows: false,
    prev: null,
    foreignSelection: false,
    diff: { loaded: false, pending: false, error: null },
    ...over,
  };
}

describe("planCommitReconcile", () => {
  it("fresh open: the diff follows the graph's auto-selected commit", () => {
    expect(planCommitReconcile(input({ graphId: "a" }))).toEqual({
      selectGraphId: null,
      loadDiffId: "a",
    });
  });

  it("a filter dropping the clicked commit re-reads the diff for the new top row", () => {
    const plan = planCommitReconcile(
      input({
        repoId: "x",
        graphId: "a",
        repoInRows: false,
        prev: { repoId: "x", graphId: "x", repoInRows: true },
        diff: { loaded: true, pending: false, error: null },
      }),
    );
    expect(plan).toEqual({ selectGraphId: null, loadDiffId: "a" });
  });

  it("inspectCommitInHistory: the graph follows the repo id and its unread diff is fetched", () => {
    const plan = planCommitReconcile(
      input({
        repoId: "c",
        graphId: "a",
        repoInRows: true,
        prev: { repoId: null, graphId: "a", repoInRows: false },
      }),
    );
    expect(plan).toEqual({ selectGraphId: "c", loadDiffId: "c" });
  });

  it("does not refetch a diff that is already loaded for the inspected commit", () => {
    const plan = planCommitReconcile(
      input({
        repoId: "a",
        graphId: "a",
        repoInRows: true,
        diff: { loaded: true, pending: false, error: null },
      }),
    );
    expect(plan).toEqual({ selectGraphId: null, loadDiffId: null });
  });

  it("plans nothing when no id changed (a watcher refresh republishing the same ids)", () => {
    const seen = { repoId: "x", graphId: "a", repoInRows: false };
    expect(planCommitReconcile(input({ ...seen, prev: seen }))).toEqual({
      selectGraphId: null,
      loadDiffId: null,
    });
  });

  it("a click is not undone: the graph moved on while the repo id is still the old, in-rows one", () => {
    const plan = planCommitReconcile(
      input({
        repoId: "a",
        graphId: "b",
        repoInRows: true,
        prev: { repoId: "a", graphId: "a", repoInRows: true },
        diff: { loaded: true, pending: true, error: null },
      }),
    );
    expect(plan).toEqual({ selectGraphId: null, loadDiffId: null });
  });

  it("never reads a diff while one is pending", () => {
    const plan = planCommitReconcile(
      input({ graphId: "a", diff: { loaded: false, pending: true, error: null } }),
    );
    expect(plan.loadDiffId).toBeNull();
  });

  it("does not re-fire for a commit whose diff read failed (no loop)", () => {
    // A failed read leaves repoId = the target, selectedDiff null and the error set.
    const failed = planCommitReconcile(
      input({
        repoId: "a",
        graphId: "a",
        repoInRows: true,
        prev: null,
        diff: { loaded: false, pending: false, error: "git exploded" },
      }),
    );
    expect(failed.loadDiffId).toBeNull();
    const seen = { repoId: "a", graphId: "a", repoInRows: true };
    const again = planCommitReconcile(
      input({ ...seen, prev: seen, diff: { loaded: false, pending: false, error: "git exploded" } }),
    );
    expect(again).toEqual({ selectGraphId: null, loadDiffId: null });
  });

  it("does nothing without a graph selection", () => {
    expect(planCommitReconcile(input({}))).toEqual({ selectGraphId: null, loadDiffId: null });
  });

  it("does not replace a working-tree file or range selection with a commit diff", () => {
    const plan = planCommitReconcile(input({ graphId: "a", foreignSelection: true }));
    expect(plan.loadDiffId).toBeNull();
  });
});
