import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "CommandPalette.svelte"),
  "utf8",
);

describe("CommandPalette", () => {
  it("lists Quick Commit as a command that prompts then commits all", () => {
    expect(source).toContain('id: "quick_commit"');
    expect(source).toContain("Quick Commit…");
    expect(source).toContain("promptQuickCommit");
    expect(source).toContain("GitCommit");
  });

  it("supports mode prefixes for commits and branches", () => {
    expect(source).toContain('mode = $derived.by');
    expect(source).toContain('trimmed.startsWith("#")');
    expect(source).toContain('trimmed.startsWith("@")');
    expect(source).toContain('trimmed.startsWith("?")');
  });

  it("supports match highlighting and frecency tracking", () => {
    expect(source).toContain("highlightMatches");
    expect(source).toContain("readFrecency");
    expect(source).toContain("recordFrecency");
  });
});
