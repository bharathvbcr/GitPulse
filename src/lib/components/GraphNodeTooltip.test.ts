import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
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

  it("labels a mainline row with the branch the straight rail belongs to", () => {
    const onMain = { ...row, is_mainline: true, lane: 0, color_index: 0 };
    const named = render(GraphNodeTooltip, { props: { row: onMain, mainlineName: "main" } });
    expect(named.body).toContain('data-testid="mainline-chip"');
    expect(named.body).toContain("main · first-parent line");

    // A pinned chain no ref could name still says what the line is.
    const unnamed = render(GraphNodeTooltip, { props: { row: onMain, mainlineName: null } });
    expect(unnamed.body).toContain("mainline · first-parent line");
    const blank = render(GraphNodeTooltip, { props: { row: onMain, mainlineName: "   " } });
    expect(blank.body).toContain("mainline · first-parent line");

    // Rows off the rail never claim it, whatever the payload named.
    const offMain = render(GraphNodeTooltip, {
      props: { row: { ...row, is_mainline: false }, mainlineName: "main" },
    });
    expect(offMain.body).not.toContain("mainline-chip");
    const legacy = render(GraphNodeTooltip, { props: { row, mainlineName: "main" } });
    expect(legacy.body).not.toContain("mainline-chip");
  });

  it("explains a fading stub: which parent is missing and why", () => {
    const cut: VisualCommitRow = {
      ...row,
      parent_ids: ["973005e086777e99c9a79c52144ceba5a22c919e"],
      connections: [
        { from_lane: 1, to_lane: 1, to_row_offset: 1, is_merge: false, color_index: 3, is_dangling: true },
      ],
    };
    const window = render(GraphNodeTooltip, { props: { row: cut, hasMore: true } });
    expect(window.body).toContain('data-testid="dangling-strip"');
    expect(window.body).toContain("973005e");
    expect(window.body).toContain("outside the loaded history");
    expect(window.body).toContain("load older history to follow it");

    const exhausted = render(GraphNodeTooltip, { props: { row: cut, hasMore: false } });
    expect(exhausted.body).toContain("outside the loaded history");
    expect(exhausted.body).not.toContain("load older history");

    // Filters never produce stubs (the backend relinks survivors to their
    // nearest kept ancestors), so the strip has one explanation only.
    expect(window.body).not.toContain("hidden by the current filter");

    // Two missing parents on one merge are counted, not listed.
    const octopus: VisualCommitRow = {
      ...cut,
      parent_ids: [cut.parent_ids[0], "b".repeat(40)],
      connections: [cut.connections[0], { ...cut.connections[0], is_merge: true }],
    };
    const counted = render(GraphNodeTooltip, { props: { row: octopus } });
    expect(counted.body).toContain("and 1 more are outside the loaded history");

    // Live edges never earn the strip; a hostile dangling edge with no
    // parent id behind it is ignored rather than rendered as "Parent ".
    const live = render(GraphNodeTooltip, { props: { row, hasMore: true } });
    expect(live.body).not.toContain("dangling-strip");
    const hostile = render(GraphNodeTooltip, {
      props: { row: { ...cut, parent_ids: [] }, hasMore: true },
    });
    expect(hostile.body).not.toContain("dangling-strip");
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

describe("GraphNodeTooltip id line layout", () => {
  const source = readFileSync(new URL("./GraphNodeTooltip.svelte", import.meta.url), "utf8");

  it("lets the chips wrap under the commit id instead of squeezing it", () => {
    // The id is a break-all span; with the kind, branch-line and mainline
    // chips as shrink-0 siblings on one non-wrapping flex line, a 320px
    // tooltip left it 6px wide and rendered the 40-character id one glyph
    // per line (an 800px-tall card). The line must wrap so the id keeps
    // its width and the chips flow onto the next line.
    const idLine = source.slice(source.indexOf('<span class="select-all break-all">{row.id}</span>') - 400, source.indexOf('<span class="select-all break-all">{row.id}</span>'));
    const container = idLine.slice(idLine.lastIndexOf("<div class="));
    expect(container).toContain("flex-wrap");
    expect(container).toContain("gap-x-2");
  });
});
