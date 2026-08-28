import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "CodeViewer.svelte"), "utf8");

describe("CodeViewer", () => {
  it("integrates syntax tokenizer and language detection", () => {
    expect(source).toContain("detectLanguageFromPath");
    expect(source).toContain("tokenizeLine");
    expect(source).toContain("tokenClass");
  });

  it("supports in-file search with regex and match navigation", () => {
    expect(source).toContain("isSearchOpen");
    expect(source).toContain("searchQuery");
    expect(source).toContain("isRegex");
    expect(source).toContain("nextMatch");
    expect(source).toContain("prevMatch");
  });

  it("supports line numbers, jump to line, and line selection", () => {
    expect(source).toContain("selectedLine");
    expect(source).toContain("handleLineClick");
    expect(source).toContain("goToLineOpen");
    expect(source).toContain("handleGoToLine");
  });

  it("supports inline editing and file saving via cmd_write_file_content", () => {
    expect(source).toContain("isEditing");
    expect(source).toContain("startEdit");
    expect(source).toContain("saveChanges");
    expect(source).toContain('"cmd_write_file_content"');
  });

  it("supports word wrap, whitespace toggle, zoom, and clipboard copy", () => {
    expect(source).toContain("wordWrap");
    expect(source).toContain("showWhitespace");
    expect(source).toContain("zoomPercent");
    expect(source).toContain("handleCopy");
  });

  it("windows the read-only view and caps oversized files", () => {
    expect(source).toContain("bind:scrollTop");
    expect(source).toContain("MAX_RENDER_LINES");
    expect(source).toContain("linesTruncated");
  });
});
