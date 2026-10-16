import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

// Commands and query behavior now live in the typed palette modules; preserve
// these entry-point contracts across the component and its canonical owners.
const source = ["CommandPalette.svelte", "../palette/catalog.ts", "../palette/model.ts"]
  .map(path => readFileSync(join(dirname(fileURLToPath(import.meta.url)), path), "utf8")).join("\n");

describe("CommandPalette", () => {
  // Rendered in harness/paletteChecks.js: "fuzzy aliases find Quick Commit",
  // "clean-tree Quick Commit explains why it cannot run", "follow-on prompts
  // receive focus after the palette closes"; the ? help, : symbol, :: workspace,
  // @ branch and # commit modes are each typed and driven there.
  it("supports match highlighting and frecency tracking", () => {
    expect(source).toContain("highlightMatches");
    expect(source).toContain("readFrecency");
    expect(source).toContain("recordFrecency");
  });

  // Behaviour is driven in harness/paletteChecks.js ("a remote branch row
  // checks out its local tracking name"): a remote row carries `origin/x`, and
  // the palette checks out `x`, as BranchList does.
  it("checks out a remote branch row by its local tracking name", () => {
    expect(source).toContain("repoStore.checkoutBranch(localNameFor(branch))");
    expect(source).not.toContain("repoStore.checkoutBranch(branch.name)");
  });

  it("exposes Settings and MCP setup as commands", () => {
    expect(source).toContain('id: "settings"');
    expect(source).toContain('id: "mcp_setup"');
    expect(source).toContain("gitpulse:settings");
    expect(source).toContain("MCP 2.0");
  });

  it("renders LanguageLogo for symbol and file hits", () => {
    expect(source).toContain("LanguageLogo");
    expect(source).toContain("filePath={cmd.filePath}");
  });

  it("offers commands to move the active repository tab", () => {
    expect(source).toContain('id: "move_tab_left"');
    expect(source).toContain('id: "move_tab_right"');
    expect(source).toContain("Move Repository Tab Left");
    expect(source).toContain("Move Repository Tab Right");
    expect(source).toContain("repoStore.moveTabBy");
  });

  it("points help at Map docs / link candidates", () => {
    expect(source).toContain("help_map_docs");
    expect(source).toContain("cross-repo link candidates");
  });

  it("keeps the hint footer unpainted so glass cannot composite a black bar", () => {
    const footer = source.match(/\.palette-footer \{([^}]*)\}/)?.[1];
    expect(footer).toBeDefined();
    expect(footer).not.toMatch(/background(?:-color)?:/);
    expect(source).not.toMatch(/background:\s*var\(--color-background\)/);
  });

  it("resets paging without tracking the page/highlight writes", () => {
    const start = source.indexOf("void query; void repoPath");
    expect(start).toBeGreaterThan(-1);
    const effect = source.slice(start, start + 220);
    expect(effect).toContain("untrack(() => {");
    expect(effect.indexOf("untrack(() => {")).toBeLessThan(effect.indexOf("page = 0"));
    expect(effect).toContain("highlighted = 0");
    expect(effect).toContain("actionError = null");
  });
});
