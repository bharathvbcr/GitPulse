import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

// Commands and query behavior now live in the typed palette modules; preserve
// these entry-point contracts across the component and its canonical owners.
const source = ["CommandPalette.svelte", "../palette/catalog.ts", "../palette/model.ts"]
  .map(path => readFileSync(join(dirname(fileURLToPath(import.meta.url)), path), "utf8")).join("\n");

describe("CommandPalette", () => {
  it("lists Quick Commit as a command that prompts then commits all", () => {
    expect(source).toContain('id: "quick_commit"');
    expect(source).toContain("Quick Commit…");
    expect(source).toContain("promptQuickCommit");
    expect(source).toContain("GitCommit");
  });

  it("supports mode prefixes for commits, branches, and code symbols", () => {
    expect(source).toContain('mode = $derived.by');
    expect(source).toContain('trimmed.startsWith("#")');
    expect(source).toContain('trimmed.startsWith("@")');
    expect(source).toContain('trimmed.startsWith(":")');
    expect(source).toContain('trimmed.startsWith("?")');
  });

  it("supports match highlighting and frecency tracking", () => {
    expect(source).toContain("highlightMatches");
    expect(source).toContain("readFrecency");
    expect(source).toContain("recordFrecency");
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
});
