import { get } from "svelte/store";
import { describe, expect, it } from "vitest";
import type { VisualCommitRow } from "../../canvas/GraphRenderer";
import { createGraphStore, type CommitGraphPayload, type InvokeFn } from "../graphStore";

/**
 * A reflog entry can name a commit the graph walk does not draw: a reset or an
 * amend left it reachable only from the reflog. Selecting it by id must still
 * put it in the details pane, and a graph reload must not quietly swap it for
 * the newest row.
 */

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

function payload(...ids: string[]): CommitGraphPayload {
  return { rows: ids.map(row), head_id: ids[0] ?? null, refs: [], has_more: false };
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
    summary: `details:${id}`,
    body: "",
    co_authors: [],
    changed_files: [],
    total_additions: 0,
    total_deletions: 0,
  };
}

const NO_SIGNATURE = { signature: "none", status: "none", key: "", signer: "" };

function harness(opts: { graph: () => CommitGraphPayload; failDetailsFor?: string }) {
  const detailReads: string[] = [];
  const invoke: InvokeFn = async (cmd, args) => {
    if (cmd === "cmd_get_commit_graph") return opts.graph() as never;
    if (cmd === "cmd_get_commit_details") {
      const id = String(args?.commitId);
      detailReads.push(id);
      if (id === opts.failDetailsFor) throw new Error("bad object");
      return detailsFor(id) as never;
    }
    return NO_SIGNATURE as never;
  };
  const store = createGraphStore({ invoke, diagnostics: { warn: () => {} } });
  return { store, detailReads };
}

describe("graphStore.selectCommitById", () => {
  it("selects a loaded row by id, the same as clicking it", async () => {
    const { store } = harness({ graph: () => payload("a", "b") });
    store.showRepo("/r");
    await store.loadGraph("/r");
    await store.selectCommitById("b", { summary: "ignored" });
    const state = get(store);
    expect(state.selectedCommit).toBe(state.rows[1]);
    expect(state.selectedCommitDetails?.id).toBe("b");
    expect(state.selectedCommitDetailsStatus).toBe("ready");
  });

  it("selects and reads a commit the graph does not draw", async () => {
    const { store, detailReads } = harness({ graph: () => payload("a", "b") });
    store.showRepo("/r");
    await store.loadGraph("/r");
    await store.selectCommitById("orphan", { summary: "commit: lost work" });
    const state = get(store);
    expect(state.selectedCommit?.id).toBe("orphan");
    expect(state.selectedCommit?.summary).toBe("commit: lost work");
    expect(state.rows.some((r) => r.id === "orphan")).toBe(false);
    expect(detailReads).toContain("orphan");
    expect(state.selectedCommitDetails?.id).toBe("orphan");
    expect(state.selectedCommitDetailsStatus).toBe("ready");
  });

  it("keeps an off-graph selection across a reload that still does not draw it", async () => {
    let rows = ["a", "b"];
    const { store } = harness({ graph: () => payload(...rows) });
    store.showRepo("/r");
    await store.loadGraph("/r");
    await store.selectCommitById("orphan", { summary: "lost" });
    rows = ["c", "a", "b"]; // a new commit lands: the payload changes
    await store.loadGraph("/r");
    const state = get(store);
    expect(state.rows[0]?.id).toBe("c");
    expect(state.selectedCommit?.id).toBe("orphan");
    expect(state.selectedCommitDetails?.id).toBe("orphan");
  });

  it("adopts the real row once a reload draws the commit", async () => {
    let rows = ["a", "b"];
    const { store } = harness({ graph: () => payload(...rows) });
    store.showRepo("/r");
    await store.loadGraph("/r");
    await store.selectCommitById("orphan", { summary: "lost" });
    rows = ["orphan", "a", "b"]; // a branch was created at it
    await store.loadGraph("/r");
    const state = get(store);
    expect(state.selectedCommit).toBe(state.rows[0]);
    expect(state.selectedCommit?.summary).toBe("summary:orphan");
  });

  it("still falls back to the newest row when a filter drops a drawn selection", async () => {
    let rows = ["a", "b"];
    const { store } = harness({ graph: () => payload(...rows) });
    store.showRepo("/r");
    await store.loadGraph("/r");
    await store.selectCommitById("b");
    rows = ["a"];
    await store.loadGraph("/r", "only-a");
    expect(get(store).selectedCommit?.id).toBe("a");
  });

  // HEAD moves and Suspects select a commit by id from a list that is not the
  // graph. When that commit happens to be drawn and a reload then drops it (a
  // filter typed above the reflog, the commit cap, a watcher refresh), the
  // details pane must stay on the row the reader clicked in that list rather
  // than jump to the newest commit the list does not show.
  it("keeps a pinned selection a reload stops drawing", async () => {
    let rows = ["a", "b"];
    const { store } = harness({ graph: () => payload(...rows) });
    store.showRepo("/r");
    await store.loadGraph("/r");
    await store.selectCommitById("b", { pin: true });
    rows = ["a"];
    await store.loadGraph("/r", "only-a");
    const state = get(store);
    expect(state.selectedCommit?.id).toBe("b");
    expect(state.selectedCommitDetails?.id).toBe("b");
  });

  it("drops the pin when the graph itself is clicked", async () => {
    let rows = ["a", "b"];
    const { store } = harness({ graph: () => payload(...rows) });
    store.showRepo("/r");
    await store.loadGraph("/r");
    await store.selectCommitById("b", { pin: true });
    await store.selectCommit(get(store).rows[1]!);
    rows = ["a"];
    await store.loadGraph("/r", "only-a");
    expect(get(store).selectedCommit?.id).toBe("a");
  });

  it("reports a pruned object as a details error, not an empty commit", async () => {
    const { store } = harness({ graph: () => payload("a"), failDetailsFor: "gone" });
    store.showRepo("/r");
    await store.loadGraph("/r");
    await store.selectCommitById("gone", { summary: "pruned" });
    const state = get(store);
    expect(state.selectedCommit?.id).toBe("gone");
    expect(state.selectedCommitDetailsStatus).toBe("error");
    expect(state.selectedCommitDetailsError).toContain("bad object");
  });
});
