import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import WorktreesPanel from "./WorktreesPanel.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "WorktreesPanel.svelte"),
  "utf8"
);

describe("WorktreesPanel", () => {
  it("labels the add-worktree button for screen readers", () => {
    const { body } = render(WorktreesPanel);
    expect(body).toContain('aria-label="Create worktree"');
  });

  it("gives the two-step remove button a spoken label, including the arm state", () => {
    // Rows render from backend data (absent in SSR), so the remove control is
    // asserted at source level like DiffViewer.test.ts does.
    expect(source).toContain('aria-label={removingPath === wt.path');
    expect(source).toContain("Click again to remove");
    expect(source).toContain(`Remove worktree \${wt.name}`);
  });

  it("drops stale cmd_list_worktrees responses via an epoch counter", () => {
    // Overlapping loads after rapid create/remove must not land out of order:
    // every apply path re-checks the epoch captured at trigger time.
    expect(source).toContain("loadEpoch");
    expect(source).toMatch(/epoch !== loadEpoch/);
    // A superseded load's finally must not clear the newer load's spinner.
    expect(source).toContain("if (epoch === loadEpoch) isLoading = false;");
  });
});
