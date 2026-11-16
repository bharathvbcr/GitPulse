import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));

describe("CommandPalette optional-tools entry points", () => {
  const source = ["CommandPalette.svelte", "../palette/catalog.ts", "../palette/search.ts"]
    .map(path => readFileSync(join(here, path), "utf8")).join("\n");

  it("distinguishes cannot-search from zero symbol hits", () => {
    expect(source).toContain("symbolSearchNote");
    expect(source).toContain("not the same as zero matches");
    // The Set up / Build index call-to-action is the shared notice, mounted
    // only when symbol search failed; a bare "Set up code map" button here
    // offered install for timeouts and other failures too.
    expect(source).toContain('<MissingToolNotice repoPath={repoPath}');
    expect(source).toContain('mode === "symbols" && searchResult.failed');
    expect(source).not.toContain('openSetupWizard("devmap", "explain"); }}>Set up code map');
    // A failed symbol search hands its note to the notice, which prints it
    // once; the palette's own note block stands aside so the reason is not
    // printed again above an install prompt.
    expect(source).toContain("otherText={searchNote}");
    expect(source).toContain('(searchNote || historyNote) && !(mode === "symbols" && searchResult.failed)');
  });

  it("offers optional tools setup from the palette", () => {
    expect(source).toContain("optional_tools_setup");
    expect(source).toContain("openSetupWizard");
  });
});
