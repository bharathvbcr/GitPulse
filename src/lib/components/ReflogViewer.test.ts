import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { functionBody } from "../stores/sourceScan";

const source = readFileSync(new URL("./ReflogViewer.svelte", import.meta.url), "utf8");

describe("ReflogViewer interaction contract", () => {
  it("keeps the entry table inside a scrollable body", () => {
    const tableBranch = source.slice(
      source.indexOf("{:else if entries.length === 0}"),
      source.indexOf("</table>"),
    );

    expect(tableBranch).toContain('class="flex-1 min-h-0 overflow-auto"');
    expect(tableBranch).toContain('<thead class="sticky top-0');
  });

  it("provides a real action control without assigning interactive behavior to a table row", () => {
    const row = source.slice(
      source.indexOf("{#each shown as entry (entry.selector)}"),
      source.indexOf("{/each}", source.indexOf("{#each shown as entry (entry.selector)}")),
    );

    expect(row).toContain('<button\n                    type="button"');
    expect(row).toContain("aria-label={`Inspect ${entry.selector}");
    expect(row).toContain("onclick={() => inspectEntry(entry)}");
    expect(row).toContain("focus-visible:ring-accent");
    expect(row).not.toContain('tabindex="0"');
    expect(row).not.toContain("onkeydown=");
    expect(row).not.toContain('<tr\n                aria-label=');
  });

  it("leaves branch-here and reset-here to the details pane below", () => {
    // They lived on every row here and again in the graph's row menu. The
    // details pane sits under every Graph list and acts on whichever commit it
    // describes, through branches/commitActions.ts (tested there).
    expect(source).not.toContain("branchHere(");
    expect(source).not.toContain("resetHere(");
    expect(source).not.toContain("cmd_reset_preview");
    expect(source).not.toContain("repoStore.createBranch(");
    expect(source).not.toContain("repoStore.resetTo(");
    // The selector still reaches the prompts, as the label of the selection.
    expect(functionBody(source, "inspectEntry")).toContain("label: entry.selector");
    const details = readFileSync(new URL("./CommitDetails.svelte", import.meta.url), "utf8");
    expect(details).toContain("createBranchAt(selectedCommit.id, actionLabel)");
    expect(details).toContain("resetKeepTo(selectedCommit.id, actionLabel)");
    const row = readFileSync(new URL("./CommitRow.svelte", import.meta.url), "utf8");
    expect(row).toContain("await createBranchAt(row.id)");
    expect(row).toContain("await resetKeepTo(row.id)");
    expect(row).not.toContain("askText(");
  });

  it("reloads when HEAD moves in the same repository", () => {
    // A reset from the details pane (or a commit made in the terminal) adds an
    // entry; the list used to reload only on its own row actions.
    expect(source).toContain("const head = $graphStore.headId;");
    // Not on the graph's first head after a cold mount: that is a load, not a move.
    expect(source).toContain("if (prev && prev.repo === repo && prev.head && head && prev.head !== head) void load();");
  });

  it("discloses when the 200-entry request ceiling may hide older history", () => {
    expect(source).toContain("const REFLOG_ENTRY_LIMIT = 200;");
    expect(source).toContain("maxEntries: REFLOG_ENTRY_LIMIT");
    expect(source).toContain("entries.length >= REFLOG_ENTRY_LIMIT");
    expect(source).toContain("Showing the 200 most recent reflog entries");
    expect(source).toContain("Older reflog history may exist");
  });
});
