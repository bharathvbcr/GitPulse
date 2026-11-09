import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { createGraphStore, type InvokeFn } from "../graphStore";

const DETAILS = {
  id: "a",
  summary: "d",
  changed_files: [],
  total_additions: 0,
  total_deletions: 0,
};

function full(id: string, token: string | null) {
  return {
    rows: [{ id, summary: id, author_name: "ada", timestamp: 1, lane: 0, parent_ids: [] }],
    head_id: id,
    graph_token: token,
  };
}

/** What the backend sends when the token still names the repository's state. */
function unchanged(token: string) {
  return { rows: [], refs: [], head_id: "a", has_more: false, graph_token: token, unchanged: true };
}

interface Seen {
  knownToken: unknown;
  maxCommits: unknown;
}

function backend(answer: (call: number, seen: Seen) => unknown) {
  const calls: Seen[] = [];
  const invoke: InvokeFn = async (cmd, args) => {
    if (cmd === "cmd_get_commit_graph") {
      const seen = { knownToken: args?.knownToken, maxCommits: args?.maxCommits };
      calls.push(seen);
      return answer(calls.length, seen) as never;
    }
    if (cmd === "cmd_get_commit_details") return DETAILS as never;
    throw new Error(cmd);
  };
  return { invoke, calls };
}

describe("graphStore revision token", () => {
  it("offers the last token for a reload of the same view and keeps the rows on an unchanged answer", async () => {
    const { invoke, calls } = backend((n) => (n === 1 ? full("a", "t1") : unchanged("t1")));
    const store = createGraphStore({ invoke });
    store.showRepo("/r/a");
    await store.loadGraph("/r/a");
    const rows = get(store).rows;
    expect(rows).toHaveLength(1);

    await store.loadGraph("/r/a");

    expect(calls[0]?.knownToken).toBeNull();
    expect(calls[1]?.knownToken).toBe("t1");
    // The unchanged answer carries no rows: they must not replace the held page.
    expect(get(store).rows).toBe(rows);
    expect(get(store).isLoading).toBe(false);
  });

  it("adopts the new token when the state moved but the drawn page did not", async () => {
    const { invoke, calls } = backend((n) => (n <= 2 ? full("a", `t${n}`) : unchanged("t2")));
    const store = createGraphStore({ invoke });
    store.showRepo("/r/a");
    await store.loadGraph("/r/a");
    await store.loadGraph("/r/a");
    await store.loadGraph("/r/a");
    expect(calls.map((c) => c.knownToken)).toEqual([null, "t1", "t2"]);
  });

  it("never offers a token for a different window, query or branch", async () => {
    const { invoke, calls } = backend(() => full("a", "t1"));
    const store = createGraphStore({ invoke });
    store.showRepo("/r/a");
    await store.loadGraph("/r/a");
    await store.loadMore("/r/a");
    await store.loadGraph("/r/a", "author:ada");
    await store.loadGraph("/r/a", "author:ada", "feature");
    expect(calls.map((c) => c.knownToken)).toEqual([null, null, null, null]);
  });

  it("asks again without a token when an unchanged answer does not vouch for the page held", async () => {
    // An unchanged answer for some other token must never blank or keep the
    // graph on trust: the store re-asks once, for the whole page.
    const { invoke, calls } = backend((n) => (n === 2 ? unchanged("ghost") : full("a", `t${n}`)));
    const store = createGraphStore({ invoke });
    store.showRepo("/r/a");
    await store.loadGraph("/r/a");
    await store.loadGraph("/r/a");
    expect(calls.map((c) => c.knownToken)).toEqual([null, "t1", null]);
    expect(get(store).rows).toHaveLength(1);
  });

  it("does not run the unchanged answer through the identical-payload path", async () => {
    let held: unknown[] | null = null;
    const { invoke } = backend((n) => (n === 1 ? full("a", "t1") : unchanged("t1")));
    const store = createGraphStore({ invoke });
    store.showRepo("/r/a");
    await store.loadGraph("/r/a");
    held = get(store).rows;
    await store.loadGraph("/r/a");
    expect(get(store).rows).toBe(held);
    expect(get(store).selectedCommit?.id).toBe("a");
  });
});
