import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import { compile } from "svelte/compiler";
import type { VisualCommitRow } from "../canvas/GraphRenderer";
import CommitRow from "./CommitRow.svelte";

const source = readFileSync(new URL("./CommitRow.svelte", import.meta.url), "utf8");

/**
 * Keyboard/AT parity for the graph's connector attribution.
 *
 * The hover tooltip names where a closing branch merges; a commit row is
 * the accessible path, so the SAME relationship must be part of the row's
 * accessible content — pointer-only context is a gap, not a design.
 */

const row: VisualCommitRow = {
  id: "0123456789abcdef0123456789abcdef01234567",
  parent_ids: ["fedcba98"],
  summary: "Close out the feature branch",
  author_name: "Ada Lovelace",
  author_email: "ada@example.com",
  timestamp: 1_700_000_000,
  lane: 2,
  color_index: 3,
  active_lanes: [0, 2],
  active_lane_colors: [0, 3],
  connections: [
    { from_lane: 2, to_lane: 0, to_row_offset: 5, is_merge: false, color_index: 3 },
  ],
  is_merge: false,
  is_root: false,
};

describe("CommitRow accessible graph context", () => {
  it("announces the merge destination of a closing branch to assistive tech", () => {
    const { body } = render(CommitRow, {
      props: {
        row,
        mergeTarget: {
          id: "fedcba9876543210fedcba9876543210fedcba98",
          summary: "Land the feature on main",
        },
      },
    });
    const flat = body.replace(/\s+/g, " ");
    expect(flat).toContain("sr-only");
    expect(flat).toContain("Merges into fedcba9");
    expect(flat).toContain("Land the feature on main");
  });

  it("adds no merge text for commits that do not close into another branch", () => {
    const { body } = render(CommitRow, { props: { row, mergeTarget: null } });
    expect(body).not.toContain("Merges into");
  });

  it("stays a keyboard-operable button", () => {
    const { body } = render(CommitRow, { props: { row } });
    expect(body).toContain('role="button"');
    expect(body).toContain('tabindex="0"');
  });

  it("has no accessibility compiler warnings", () => {
    const { warnings } = compile(source, { generate: "client" });
    expect(warnings.filter(({ code }) => code.startsWith("a11y_"))).toEqual([]);
  });

  it("opens the context menu through the standard keyboard context-menu keys", () => {
    expect(source).toContain('e.key === "ContextMenu"');
    expect(source).toContain('e.key === "F10"');
  });
});
