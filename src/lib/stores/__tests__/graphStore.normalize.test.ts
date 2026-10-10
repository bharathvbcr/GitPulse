import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { createGraphStore, graphPayloadSignature, normalizeDiffPayload, type InvokeFn } from "../graphStore";

const EMPTY = {
  content: "",
  truncated: false,
  included_files: 0,
  skipped_files: [],
  total_files: 0,
  total_additions: 0,
  total_deletions: 0,
};

describe("normalizeDiffPayload: whatever crossed IPC becomes a safe diff", () => {
  it("wraps a legacy bare string as an untruncated payload", () => {
    expect(normalizeDiffPayload("diff --git a b")).toEqual({ ...EMPTY, content: "diff --git a b" });
  });

  it.each([null, undefined, 42, true, [], ["x"]])("degrades %j to an empty diff instead of throwing", (junk) => {
    expect(normalizeDiffPayload(junk)).toEqual(EMPTY);
  });

  it("keeps a well-formed payload", () => {
    const payload = {
      content: "@@",
      truncated: true,
      included_files: 2,
      skipped_files: [{ path: "big.bin", additions: 10, deletions: 3 }],
      total_files: 5,
      total_additions: 12,
      total_deletions: 4,
    };
    expect(normalizeDiffPayload(payload)).toEqual(payload);
  });

  it("only treats a literal true as truncation", () => {
    expect(normalizeDiffPayload({ truncated: "yes" }).truncated).toBe(false);
    expect(normalizeDiffPayload({ truncated: 1 }).truncated).toBe(false);
    expect(normalizeDiffPayload({ truncated: true }).truncated).toBe(true);
  });

  it("zeroes counts that are negative, fractional-NaN, infinite or not numbers", () => {
    const out = normalizeDiffPayload({
      included_files: -3,
      total_files: Number.NaN,
      total_additions: Number.POSITIVE_INFINITY,
      total_deletions: "7",
    });
    expect(out).toMatchObject({ included_files: 0, total_files: 0, total_additions: 0, total_deletions: 0 });
  });

  it("drops skipped-file entries that are not records with a string path, and coerces their counts", () => {
    const out = normalizeDiffPayload({
      content: 5,
      skipped_files: [
        null,
        "path",
        ["a"],
        { path: 3 },
        { path: "keep.rs", additions: -1, deletions: "x" },
        { path: "ok.rs", additions: 4, deletions: 2 },
      ],
    });
    expect(out.content).toBe("");
    expect(out.skipped_files).toEqual([
      { path: "keep.rs", additions: 0, deletions: 0 },
      { path: "ok.rs", additions: 4, deletions: 2 },
    ]);
    expect(normalizeDiffPayload({ skipped_files: "nope" }).skipped_files).toEqual([]);
  });
});

describe("graphPayloadSignature sees every rendered difference", () => {
  const row = (over: Record<string, unknown> = {}) => ({ id: "a", summary: "s", parent_ids: [], ...over });
  const base = { rows: [row()], head_id: "a" };

  it("changes when a lane connection changes", () => {
    const connection = { from_lane: 0, to_lane: 1, to_row_offset: 2, color_index: 3, is_merge: false, is_dangling: false };
    const plain = graphPayloadSignature({ ...base, rows: [row({ connections: [connection] })] });
    expect(graphPayloadSignature({ ...base, rows: [row({ connections: [{ ...connection, is_merge: true }] })] })).not.toBe(plain);
    expect(graphPayloadSignature({ ...base, rows: [row({ connections: [{ ...connection, is_dangling: true }] })] })).not.toBe(plain);
    expect(graphPayloadSignature({ ...base, rows: [row({ connections: [{ ...connection, to_lane: 2 }] })] })).not.toBe(plain);
    expect(graphPayloadSignature({ ...base, rows: [row({ connections: [{ ...connection }] })] })).toBe(plain);
  });

  it("treats an absent optional field and an empty one as the same row", () => {
    expect(graphPayloadSignature({ ...base, rows: [row({ connections: [], active_lanes: [] })] })).toBe(
      graphPayloadSignature(base),
    );
  });

  it("changes with notices, mainline, ref decorations and has_more", () => {
    const sig = graphPayloadSignature(base);
    expect(graphPayloadSignature({ ...base, notices: ["36 not drawn"] })).not.toBe(sig);
    expect(graphPayloadSignature({ ...base, mainline_id: "a", mainline_name: "main" })).not.toBe(sig);
    expect(
      graphPayloadSignature({ ...base, refs: [{ name: "main", kind: "local", commit_id: "a", is_head: true }] }),
    ).not.toBe(sig);
    expect(graphPayloadSignature({ ...base, has_more: true })).not.toBe(sig);
  });
});

describe("a reload of an unchanged graph", () => {
  it("retires the loading flag without republishing the rows", async () => {
    let calls = 0;
    const invoke: InvokeFn = async (cmd) => {
      if (cmd === "cmd_get_commit_graph") {
        calls += 1;
        return { rows: [{ id: "a", summary: "s", parent_ids: [] }], head_id: "a" } as never;
      }
      return { id: "a", changed_files: [] } as never;
    };
    const store = createGraphStore({ invoke });
    store.showRepo("/r");
    await store.loadGraph("/r");
    const rows = get(store).rows;
    // Back to a visible repository that is still "loading" (a tab re-activation).
    store.showRepo("/other");
    store.showRepo("/r");
    await store.loadGraph("/r");
    expect(calls).toBe(2);
    expect(get(store).rows).toBe(rows);
    expect(get(store).isLoading).toBe(false);
  });
});
