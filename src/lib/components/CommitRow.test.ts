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

  it("offers cherry-pick and revert through the same menu as checkout", () => {
    // Both commands already exist on the store; leaving them off the menu
    // meant a native Git client that could replay commits only from the
    // terminal. The menu is the discoverable path.
    expect(source).toContain("repoStore.cherryPick([row.id])");
    expect(source).toContain("repoStore.revertCommits([row.id])");
    expect(source).toContain("Cherry-pick onto current branch");
    expect(source).toContain("Revert this commit");
  });

  it("does not show copied feedback when the clipboard rejects the write", () => {
    const shaCopy = source.slice(source.indexOf("async function handleCopySha"), source.indexOf("function openContextMenu"));
    expect(shaCopy).toContain("if (!(await copyText(row.id)))");
    expect(shaCopy).toContain("toastStore.error");
    expect(source).toContain("if (await copyText(row.summary))");
  });
});

/**
 * The all-refs scope puts machine-written ref paths on rows. A real one in
 * this repository is 209 characters; drawn whole it stretches the row until
 * the commit summary is off-screen, so the chip folds it and keeps the whole
 * path in the title where nothing is lost.
 */
describe("CommitRow ref chips", () => {
  const longRef =
    "codex/turn-diffs/checkpoints/" +
    "146c832dd582d2f371d2d7f79aa5f0467658b5e962c28a281f2b46a1529f5c46/" +
    "3c0bd968060e6a19a71608ee26cc63d973b8dc4a8c31e9f95be0c2b68c219178/" +
    "1788535046539/ca796ac6-5927-4170-9a4b-ccadae440ddb";

  it("folds an enormous non-branch ref instead of drawing it whole", () => {
    const { body } = render(CommitRow, {
      props: { row, refs: [{ name: longRef, kind: "other" as const }] },
    });
    // The visible chip text is the folded form...
    expect(body).toContain("codex/turn-diffs/checkpoints/…</span>");
    // ...and the full path appears exactly once, inside the title, where it
    // costs no layout. Folded in the chip, complete on hover: nothing is
    // hidden, only wrapped up.
    expect(body.split(longRef)).toHaveLength(2);
    expect(body).toContain(`title="refs/${longRef} — outside`);
  });

  it("draws a non-branch ref as its own kind, never as a branch", () => {
    const { body } = render(CommitRow, {
      props: { row, refs: [{ name: "cmux/last-turn/abc", kind: "other" as const }] },
    });
    expect(body).toContain("outside branches, remotes and tags");
  });

  it("leaves ordinary branch and tag names whole", () => {
    const { body } = render(CommitRow, {
      props: {
        row,
        refs: [
          { name: "main", kind: "current-branch" as const },
          { name: "v1.2.0", kind: "tag" as const },
        ],
      },
    });
    expect(body).toContain("main");
    expect(body).toContain("v1.2.0");
    expect(body).not.toContain("…");
  });
});
