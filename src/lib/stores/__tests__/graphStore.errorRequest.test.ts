import { get } from "svelte/store";
import { describe, expect, it } from "vitest";
import type { VisualCommitRow } from "../../canvas/GraphRenderer";
import { createGraphStore, type CommitGraphPayload, type InvokeFn } from "../graphStore";
import { DEFAULT_MAX_COMMITS, nextLoadLimit } from "../graphLimits";

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
  has_more: true,
};

const NO_SIGNATURE = { signature: "none", status: "none", key: "", signer: "" };

/** Serves the first graph load, then fails every later one with `failure`. */
function flaky(failure: string) {
  let graphCalls = 0;
  const asked: Array<Record<string, unknown>> = [];
  const invoke: InvokeFn = async (cmd, args) => {
    if (cmd === "cmd_get_commit_graph") {
      graphCalls += 1;
      asked.push(args ?? {});
      if (graphCalls > 1) throw new Error(failure);
      return PAYLOAD as never;
    }
    if (cmd === "cmd_get_commit_details") {
      return {
        id: args?.commitId,
        parent_ids: [],
        author_name: "ada",
        author_email: "ada@example.com",
        author_date: "",
        committer_name: "ada",
        committer_email: "ada@example.com",
        committer_date: "",
        summary: "",
        body: "",
        co_authors: [],
        changed_files: [],
        total_additions: 0,
        total_deletions: 0,
      } as never;
    }
    return NO_SIGNATURE as never;
  };
  return { invoke, asked };
}

describe("graphStore failed reloads", () => {
  it("a failed filter reload keeps hasMore and records a refining request at the asked limit", async () => {
    const { invoke } = flaky("timed out");
    const store = createGraphStore({ invoke, diagnostics: { warn: () => {} } });
    store.showRepo("/r");
    await store.loadGraph("/r");
    expect(get(store).hasMore).toBe(true);
    expect(get(store).errorRequest).toBeNull();

    await store.loadGraph("/r", "needle");
    const state = get(store);
    expect(state.error).toContain("timed out");
    expect(state.hasMore).toBe(true);
    expect(state.errorRequest).toEqual({ limit: DEFAULT_MAX_COMMITS, refining: true });
    // The previous filter's rows stay, which is why the pane must say so.
    expect(state.rows.map((r) => r.id)).toEqual(["a", "b"]);
  });

  it("a failed load-more records the larger limit and is not refining", async () => {
    const { invoke } = flaky("boom");
    const store = createGraphStore({ invoke, diagnostics: { warn: () => {} } });
    store.showRepo("/r");
    await store.loadGraph("/r");
    const bigger = nextLoadLimit(DEFAULT_MAX_COMMITS);
    expect(bigger).not.toBeNull();

    await store.loadMore("/r");
    const state = get(store);
    expect(state.error).toContain("boom");
    expect(state.hasMore).toBe(true);
    expect(state.errorRequest).toEqual({ limit: bigger, refining: false });
  });

  it("a successful reload clears the failed request", async () => {
    let fail = false;
    const base = flaky("x");
    const invoke: InvokeFn = async (cmd, args) => {
      if (cmd === "cmd_get_commit_graph") {
        if (fail) throw new Error("later");
        return PAYLOAD as never;
      }
      return base.invoke(cmd, args);
    };
    const store = createGraphStore({ invoke, diagnostics: { warn: () => {} } });
    store.showRepo("/r");
    await store.loadGraph("/r");
    fail = true;
    await store.loadGraph("/r", "q");
    expect(get(store).errorRequest).not.toBeNull();
    fail = false;
    await store.loadGraph("/r", "q");
    expect(get(store).error).toBeNull();
    expect(get(store).errorRequest).toBeNull();
  });
});
