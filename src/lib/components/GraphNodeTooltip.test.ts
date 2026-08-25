import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import type { VisualCommitRow } from "../canvas/GraphRenderer";
import GraphNodeTooltip from "./GraphNodeTooltip.svelte";

const row: VisualCommitRow = {
  id: "0123456789abcdef0123456789abcdef01234567",
  parent_ids: ["parent-one", "parent-two"],
  summary: "Merge the tooltip branch",
  author_name: "Ada Lovelace",
  author_email: "ada@example.com",
  timestamp: 1_700_000_000,
  lane: 2,
  color_index: 3,
  active_lanes: [0, 1, 2],
  active_lane_colors: [0, 1, 3],
  connections: [],
  is_merge: true,
  is_root: false,
};

describe("GraphNodeTooltip", () => {
  it("shows the commit details and refs needed to identify a branch node", () => {
    const { body } = render(GraphNodeTooltip, {
      props: {
        row,
        refs: [
          { name: "feature/tooltips", kind: "current-branch" },
          { name: "v1.2.0", kind: "tag" },
        ],
      },
    });

    expect(body).toContain('role="tooltip"');
    expect(body).toContain("Merge the tooltip branch");
    expect(body).toContain(row.id);
    expect(body).toContain("Ada Lovelace");
    expect(body).toContain("ada@example.com");
    expect(body).toContain("feature/tooltips");
    expect(body).toContain("v1.2.0");
    expect(body).toContain("Merge commit");
    expect(body).toContain("2 parents");
  });
});
