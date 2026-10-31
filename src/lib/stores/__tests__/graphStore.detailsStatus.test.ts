import { get } from "svelte/store";
import { describe, expect, it } from "vitest";
import type { VisualCommitRow } from "../../canvas/GraphRenderer";
import { createGraphStore, type CommitGraphPayload, type InvokeFn } from "../graphStore";

function row(id: string): VisualCommitRow {
  return {
    id,
    parent_ids: [],
    summary: `summary:${id}`,
    author_name: "ada",
    author_email: "ada@example.com",
    timestamp: 1,
    lane: 0,
    color_index: 0,
    active_lanes: [],
    active_lane_colors: [],
    connections: [],
    is_merge: false,
    is_root: false,
  };
}

const PAYLOAD: CommitGraphPayload = {
  rows: [row("a"), row("b")],
  head_id: "a",
  refs: [],
  has_more: false,
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function detailsFor(id: string) {
  return {
    id,
    parent_ids: [],
    author_name: "ada",
    author_email: "ada@example.com",
    author_date: "2026-01-01",
    committer_name: "ada",
    committer_email: "ada@example.com",
    committer_date: "2026-01-01",
    summary: id,
    body: "",
    co_authors: [],
    changed_files: [],
    total_additions: 0,
    total_deletions: 0,
  };
}

const NO_SIGNATURE = { signature: "none", status: "none", key: "", signer: "" };

describe("graphStore commit details status", () => {
  it("goes loading -> ready when the details arrive", async () => {
    const pending = deferred<ReturnType<typeof detailsFor>>();
    const invoke: InvokeFn = async (cmd) => {
      if (cmd === "cmd_get_commit_graph") return PAYLOAD as never;
      if (cmd === "cmd_get_commit_details") return pending.promise as never;
      return NO_SIGNATURE as never;
    };
    const store = createGraphStore({ invoke, diagnostics: { warn: () => {} } });
    store.showRepo("/r");
    const load = store.loadGraph("/r");
    await new Promise((r) => setTimeout(r, 0));
    expect(get(store).selectedCommit?.id).toBe("a");
    expect(get(store).selectedCommitDetailsStatus).toBe("loading");
    expect(get(store).selectedCommitDetails).toBeNull();

    pending.resolve(detailsFor("a"));
    await load;
    expect(get(store).selectedCommitDetailsStatus).toBe("ready");
    expect(get(store).selectedCommitDetails?.id).toBe("a");
    expect(get(store).selectedCommitDetailsError).toBeNull();
  });

  it("goes loading -> error with the message when the read rejects, and still leaves a breadcrumb", async () => {
    const warnings: Array<[string, unknown]> = [];
    const invoke: InvokeFn = async (cmd) => {
      if (cmd === "cmd_get_commit_graph") return PAYLOAD as never;
      if (cmd === "cmd_get_commit_details") throw new Error("git cat-file exploded");
      return NO_SIGNATURE as never;
    };
    const store = createGraphStore({
      invoke,
      diagnostics: { warn: (source: string, detail: unknown) => void warnings.push([source, detail]) },
    });
    store.showRepo("/r");
    await store.loadGraph("/r");

    const state = get(store);
    expect(state.selectedCommitDetailsStatus).toBe("error");
    expect(state.selectedCommitDetailsError).toContain("git cat-file exploded");
    expect(state.selectedCommitDetails).toBeNull();
    expect(warnings.some(([source]) => source === "graph-details")).toBe(true);
  });

  it("Retry (selecting the same commit again) recovers from an error", async () => {
    let fail = true;
    const invoke: InvokeFn = async (cmd) => {
      if (cmd === "cmd_get_commit_graph") return PAYLOAD as never;
      if (cmd === "cmd_get_commit_details") {
        if (fail) throw new Error("transient");
        return detailsFor("a") as never;
      }
      return NO_SIGNATURE as never;
    };
    const store = createGraphStore({ invoke, diagnostics: { warn: () => {} } });
    store.showRepo("/r");
    await store.loadGraph("/r");
    expect(get(store).selectedCommitDetailsStatus).toBe("error");

    fail = false;
    const selected = get(store).selectedCommit;
    expect(selected).not.toBeNull();
    await store.selectCommit(selected!);
    expect(get(store).selectedCommitDetailsStatus).toBe("ready");
    expect(get(store).selectedCommitDetailsError).toBeNull();
  });

  it("a stale failure never marks the newly selected commit as failed", async () => {
    const slowFailure = deferred<never>();
    const invoke: InvokeFn = async (cmd, args) => {
      if (cmd === "cmd_get_commit_graph") return PAYLOAD as never;
      if (cmd === "cmd_get_commit_details") {
        if (args?.commitId === "a") return detailsFor("a") as never;
        return slowFailure.promise as never;
      }
      return NO_SIGNATURE as never;
    };
    const store = createGraphStore({ invoke, diagnostics: { warn: () => {} } });
    store.showRepo("/r");
    await store.loadGraph("/r");
    const rowB = get(store).rows[1];
    const pendingB = store.selectCommit(rowB);
    await new Promise((r) => setTimeout(r, 0));
    expect(get(store).selectedCommitDetailsStatus).toBe("loading");
    // The user moves back to "a" before B's read fails.
    await store.selectCommit(get(store).rows[0]);
    expect(get(store).selectedCommitDetailsStatus).toBe("ready");
    slowFailure.reject(new Error("late"));
    await pendingB;
    expect(get(store).selectedCommit?.id).toBe("a");
    expect(get(store).selectedCommitDetailsStatus).toBe("ready");
    expect(get(store).selectedCommitDetailsError).toBeNull();
  });
});
