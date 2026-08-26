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

const mergeTarget: VisualCommitRow = {
  ...row,
  id: "fedcba9876543210fedcba9876543210fedcba98",
  summary: "Land the feature on main",
  is_merge: false,
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
    // Context strips are variant-only: a plain node hover shows none.
    expect(body).not.toContain("Merges into");
    expect(body).not.toContain("Branch line");
    expect(body).not.toContain("in the loaded history");
  });

  it("shows the merge strip whenever a target is known, keyboard focus included", () => {
    // A keyboard-focus card has no pointer hit kind; the context must not
    // be gated behind one. Pointer-only context is a gap, not a design.
    const { body } = render(GraphNodeTooltip, {
      props: { row, hitKind: "node", mergeTarget },
    });
    expect(body).toContain("Merges into");
    expect(body).toContain(mergeTarget.id.slice(0, 7));
  });

  it("shows the author stats whenever supplied, keyboard focus included", () => {
    const { body } = render(GraphNodeTooltip, {
      props: { row, hitKind: "node", authorCommitCount: 7 },
    });
    expect(body.replace(/\s+/g, " ")).toContain(
      "7 commits by Ada Lovelace in the loaded history",
    );
  });

  it("names the merge point when the hover is an in-flight connector", () => {
    const { body } = render(GraphNodeTooltip, {
      props: { row, hitKind: "connector", mergeTarget },
    });
    expect(body).toContain("Merges into");
    expect(body).toContain(mergeTarget.id.slice(0, 7));
    expect(body).toContain("Land the feature on main");
  });

  it("stays honest when a connector's merge point is unavailable", () => {
    const { body } = render(GraphNodeTooltip, {
      props: { row, hitKind: "connector", mergeTarget: null },
    });
    expect(body).toContain("Merges into another branch below");
  });

  it("marks a pass-through hover as the branch line, not the pointed row", () => {
    const { body } = render(GraphNodeTooltip, {
      props: { row, hitKind: "lane" },
    });
    expect(body).toContain("Branch line");
  });

  it("shows the author's commit count on avatar hover", () => {
    const { body } = render(GraphNodeTooltip, {
      props: { row, hitKind: "avatar", authorCommitCount: 42 },
    });
    // Svelte wraps template text across source lines; compare flattened.
    expect(body.replace(/\s+/g, " ")).toContain(
      "42 commits by Ada Lovelace in the loaded history",
    );
  });

  it("pluralizes and omits the count strip when it is unknown or hostile", () => {
    const single = render(GraphNodeTooltip, {
      props: { row, hitKind: "avatar", authorCommitCount: 1 },
    });
    expect(single.body.replace(/\s+/g, " ")).toContain("1 commit by Ada Lovelace");
    for (const count of [null, 0, -3, Number.NaN]) {
      const { body } = render(GraphNodeTooltip, {
        props: { row, hitKind: "avatar", authorCommitCount: count },
      });
      expect(
        body.replace(/\s+/g, " "),
        `count ${count} must not render a strip`,
      ).not.toContain("in the loaded history");
    }
  });
});
