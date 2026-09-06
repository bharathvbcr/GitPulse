import { get } from "svelte/store";
import { describe, expect, it } from "vitest";
import type { VisualCommitRow } from "../../canvas/GraphRenderer";
import {
  createGraphStore,
  type CommitGraphPayload,
  type InvokeFn,
} from "../graphStore";

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

const WARNINGS = [
  "HEAD unavailable (fatal: bad object HEAD); commit graph may lack the HEAD marker",
  "ref decorations unavailable (broken for-each-ref alias); branches/tags will not be labeled",
];

/**
 * A completeness disclosure, not a failure: the load worked and the graph
 * simply does not draw everything the repository holds.
 */
const NOTICE =
  "36 commit(s) reachable only from refs outside branches, remotes and tags are not " +
  'drawn. Those refs live in: refs/cmux (18 refs). Set the graph\'s ref scope to "All refs" ' +
  "to include them.";

function graphPayload(withWarnings: boolean): CommitGraphPayload {
  const base: CommitGraphPayload = {
    rows: [row("a")],
    head_id: null,
    refs: [],
    has_more: false,
  };
  return withWarnings ? { ...base, warnings: [...WARNINGS] } : base;
}

/** Recording sink per report.test.ts's isolation convention: nothing touches
 * the app-wide ring, so assertions read plain captured calls. */
function sinkFactory() {
  const calls: Array<[string, string]> = [];
  return {
    calls,
    diagnostics: {
      warn: (source: string, detail: unknown) => {
        calls.push([source, String(detail)]);
      },
    },
  };
}

describe("graphStore graph payload warnings", () => {
  it("logs one diagnostic per backend warning on a successful load", async () => {
    const { calls, diagnostics } = sinkFactory();
    const invoke: InvokeFn = ((cmd: string) => {
      if (cmd === "cmd_get_commit_graph") return Promise.resolve(graphPayload(true));
      // Details fetches succeed here; their failure breadcrumb is pinned
      // separately in the graph-details describe below.
      return Promise.resolve({ id: "a", summary: "d", changed_files: [] });
    }) as InvokeFn;
    const store = createGraphStore({ invoke, diagnostics });

    await store.loadGraph("/repo");

    expect(calls).toEqual(WARNINGS.map((w): [string, string] => ["graph", `/repo: ${w}`]));
  });

  it("logs nothing when the payload carries no warnings", async () => {
    const { calls, diagnostics } = sinkFactory();
    const invoke: InvokeFn = ((cmd: string) => {
      if (cmd === "cmd_get_commit_graph") return Promise.resolve(graphPayload(false));
      return Promise.resolve({ id: "a", summary: "d", changed_files: [] });
    }) as InvokeFn;
    const store = createGraphStore({ invoke, diagnostics });

    await store.loadGraph("/repo");

    expect(calls).toEqual([]);
  });

  it("does not re-log warnings on a structurally identical reload", async () => {
    const { calls, diagnostics } = sinkFactory();
    const invoke: InvokeFn = ((cmd: string) => {
      if (cmd === "cmd_get_commit_graph") return Promise.resolve(graphPayload(true));
      return Promise.resolve({ id: "a", summary: "d", changed_files: [] });
    }) as InvokeFn;
    const store = createGraphStore({ invoke, diagnostics });

    await store.loadGraph("/repo");
    await store.loadGraph("/repo");

    expect(calls).toEqual(WARNINGS.map((w): [string, string] => ["graph", `/repo: ${w}`]));
  });

  /**
   * Regression: the disclosure that history exists outside the walked scope
   * is TRUE, and it was written to the diagnostics ring on every launch of
   * every repository that has agent-harness refs — a crash log filling with
   * correct sentences. It is repository state, not a fault, and belongs on
   * screen instead.
   */
  it("never files a notice as a diagnostic", async () => {
    const { calls, diagnostics } = sinkFactory();
    const invoke: InvokeFn = ((cmd: string) => {
      if (cmd === "cmd_get_commit_graph")
        return Promise.resolve({ ...graphPayload(false), notices: [NOTICE] });
      return Promise.resolve({ id: "a", summary: "d", changed_files: [] });
    }) as InvokeFn;
    const store = createGraphStore({ invoke, diagnostics });

    store.showRepo("/repo");
    await store.loadGraph("/repo");

    expect(calls).toEqual([]);
    // And it is not simply dropped: the pane renders it. Reported somewhere
    // is the whole point — silence here would make "not drawn" and "does not
    // exist" the same thing again.
    expect(get(store).notices).toEqual([NOTICE]);
  });

  /**
   * Regression: notices are rendered, so they are part of "did the output
   * change". Left out of the signature, a repository whose hidden count moved
   * from 36 to 37 while its drawn rows stayed identical would short-circuit
   * the republish and leave a stale number on screen.
   */
  it("republishes when only the notice changed", async () => {
    const { diagnostics } = sinkFactory();
    let notice = NOTICE;
    const invoke: InvokeFn = ((cmd: string) => {
      if (cmd === "cmd_get_commit_graph")
        return Promise.resolve({ ...graphPayload(false), notices: [notice] });
      return Promise.resolve({ id: "a", summary: "d", changed_files: [] });
    }) as InvokeFn;
    const store = createGraphStore({ invoke, diagnostics });

    store.showRepo("/repo");
    await store.loadGraph("/repo");
    expect(get(store).notices).toEqual([NOTICE]);

    notice = NOTICE.replace("36 commit(s)", "37 commit(s)");
    await store.loadGraph("/repo");
    expect(get(store).notices).toEqual([notice]);
  });

  // Regression: a failed best-effort details fetch used to vanish silently,
  // leaving the details pane blank with nothing to distinguish "loading"
  // from "broken". It must leave a diagnostics breadcrumb.
  it("logs a graph-details breadcrumb when the details fetch fails", async () => {
    const { calls, diagnostics } = sinkFactory();
    const invoke: InvokeFn = ((cmd: string) => {
      if (cmd === "cmd_get_commit_graph") return Promise.resolve(graphPayload(false));
      return Promise.reject(new Error("details traversal failed"));
    }) as InvokeFn;
    const store = createGraphStore({ invoke, diagnostics });

    await store.loadGraph("/repo");

    expect(calls).toEqual([
      ["graph-details", "/repo/a: details traversal failed"],
    ]);
  });
});
