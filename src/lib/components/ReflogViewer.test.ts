import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

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
      source.indexOf("{#each entries as entry}"),
      source.indexOf("{/each}", source.indexOf("{#each entries as entry}")),
    );

    expect(row).toContain('<button\n                    type="button"');
    expect(row).toContain("aria-label={`Inspect ${entry.selector}");
    expect(row).toContain("onclick={() => inspectEntry(entry)}");
    expect(row).toContain("focus-visible:ring-accent");
    expect(row).not.toContain('tabindex="0"');
    expect(row).not.toContain("onkeydown=");
    expect(row).not.toContain('<tr\n                aria-label=');
  });

  it("discloses when the 200-entry request ceiling may hide older history", () => {
    expect(source).toContain("const REFLOG_ENTRY_LIMIT = 200;");
    expect(source).toContain("maxEntries: REFLOG_ENTRY_LIMIT");
    expect(source).toContain("entries.length >= REFLOG_ENTRY_LIMIT");
    expect(source).toContain("Showing the 200 most recent reflog entries");
    expect(source).toContain("Older reflog history may exist");
  });
});
