import { describe, expect, it } from "vitest";
import type { VisualCommitRow } from "../canvas/GraphRenderer";
import type { RefDecoration } from "../stores/graphStore";
import {
  authorCountFor,
  buildCloseTargets,
  buildRefsByCommit,
  countCommitsByAuthor,
} from "./graphDecorations";

const ref = (name: string, kind: RefDecoration["kind"], commit_id: string, is_head = false): RefDecoration => ({
  name,
  kind,
  commit_id,
  is_head,
});

const row = (id: string, over: Partial<VisualCommitRow> = {}): VisualCommitRow => ({
  id,
  parent_ids: [],
  summary: id,
  author_name: "Ada",
  author_email: "ada@example.com",
  timestamp: 0,
  lane: 0,
  color_index: 0,
  active_lanes: [0],
  active_lane_colors: [0],
  connections: [],
  is_merge: false,
  is_root: false,
  ...over,
});

describe("buildRefsByCommit", () => {
  it("labels every ref kind, the checked-out branch apart from the rest", () => {
    const map = buildRefsByCommit(
      [
        ref("HEAD", "head", "c1"),
        ref("main", "local", "c1", true),
        ref("feat", "local", "c2"),
        ref("origin/main", "remote", "c1"),
        ref("v1", "tag", "c2"),
        ref("refs/agent/x", "other", "c2"),
      ],
      [],
    );
    expect(map.get("c1")?.map((r) => [r.name, r.kind])).toEqual([
      ["HEAD", "head"],
      ["main", "current-branch"],
      ["origin/main", "remote-branch"],
    ]);
    expect(map.get("c2")?.map((r) => [r.name, r.kind])).toEqual([
      ["feat", "local-branch"],
      ["v1", "tag"],
      // Never drawn as a branch: an agent checkpoint is not one.
      ["refs/agent/x", "other"],
    ]);
  });

  it("attaches the tag listing's standing to tags only, and absent stays absent", () => {
    const map = buildRefsByCommit(
      [ref("v1", "tag", "c1"), ref("v1", "local", "c2"), ref("v2", "tag", "c3")],
      [{ name: "v1", commits_ahead_of_base: 3, compared_to: "main" }],
    );
    expect(map.get("c1")?.[0]).toMatchObject({ aheadOfBase: 3, comparedTo: "main" });
    // A branch that happens to share a tag's name borrows nothing from it.
    expect(map.get("c2")?.[0]).toMatchObject({ aheadOfBase: null, comparedTo: null });
    // An unmeasured tag is null, not a zero that would read as "merged".
    expect(map.get("c3")?.[0]).toMatchObject({ aheadOfBase: null, comparedTo: null });
  });

  it("keeps one chip per name and kind", () => {
    const map = buildRefsByCommit([ref("main", "local", "c1"), ref("main", "local", "c1")], []);
    expect(map.get("c1")).toHaveLength(1);
  });
});

describe("buildCloseTargets", () => {
  it("names the commit a branch's closing line descends to", () => {
    const rows = [
      row("tip", { lane: 1, connections: [{ from_lane: 1, to_lane: 0, to_row_offset: 2, is_merge: false, color_index: 1 }] }),
      row("main1"),
      row("base"),
    ];
    expect(buildCloseTargets(rows).get("tip")?.id).toBe("base");
  });

  it("ignores straight lines, merges, dangling edges and offsets outside the rows", () => {
    const rows = [
      row("straight", { connections: [{ from_lane: 0, to_lane: 0, to_row_offset: 1, is_merge: false, color_index: 0 }] }),
      row("merge", { connections: [{ from_lane: 0, to_lane: 1, to_row_offset: 1, is_merge: true, color_index: 0 }] }),
      row("dangling", { connections: [{ from_lane: 1, to_lane: 0, to_row_offset: 1, is_merge: false, color_index: 0, is_dangling: true }] }),
      row("past", { connections: [{ from_lane: 1, to_lane: 0, to_row_offset: 99, is_merge: false, color_index: 0 }] }),
      row("last"),
    ];
    expect(buildCloseTargets(rows).size).toBe(0);
  });
});

describe("countCommitsByAuthor", () => {
  it("counts one person once across display-name changes", () => {
    const rows = [
      row("a", { author_name: "Ada" }),
      row("b", { author_name: "Ada Lovelace" }),
      row("c", { author_name: "Bob", author_email: "bob@example.com" }),
    ];
    const counts = countCommitsByAuthor(rows);
    expect(authorCountFor(counts, rows[0])).toBe(2);
    expect(authorCountFor(counts, rows[2])).toBe(1);
    expect(authorCountFor(counts, { author_name: "Eve", author_email: "eve@example.com" })).toBeNull();
  });
});
