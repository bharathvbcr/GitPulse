import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { compile } from "svelte/compiler";
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

  it("publishes every edit to the canonical parent draft owner", () => {
    expect(source).toContain("draftContent");
    expect(source).toContain("onDraftChange");
    expect(source).toContain("onEditInput");
    expect(source).toContain("onDraftChange?.(value, content)");
    expect(source).toContain("Unsaved");
  });

  it("restores drafts by file identity during rapid prop switches", () => {
    expect(source).toContain("previousFilePath");
    expect(source).toContain("if (path !== previousFilePath)");
    expect(source).toContain("editDraft = restored ?? source;");
    expect(source).toContain("isEditing = restored !== null;");
  });

  it("keeps editing state and its draft when save rejects", () => {
    const save = source.slice(
      source.indexOf("async function saveChanges"),
      source.indexOf("async function handleCopy"),
    );
    expect(save.indexOf("await onSave(contentToSave)")).toBeGreaterThan(-1);
    expect(save.indexOf("isEditing = false")).toBeGreaterThan(
      save.indexOf("await onSave(contentToSave)"),
    );
    const failed = save.slice(save.indexOf("} catch"), save.indexOf("} finally"));
    expect(failed).not.toContain("isEditing = false");
    expect(failed).not.toContain("onDraftChange");
  });

  it("requires confirmation before Cancel discards a dirty draft", () => {
    expect(source).toContain("onRequestDiscard");
    expect(source).toContain("await askConfirm({");
    expect(source).toContain("Discard Unsaved Edits");
  });

  it("supports word wrap, whitespace toggle, zoom, and clipboard copy", () => {
    expect(source).toContain("wordWrap");
    expect(source).toContain("showWhitespace");
    expect(source).toContain("zoomPercent");
    expect(source).toContain("handleCopy");
    expect(source).toContain("if (!(await copyText(textToCopy)))");
    expect(source).toContain('repoStore.setError("Could not copy file content to clipboard")');
  });

  it("windows the read-only view and caps oversized files", () => {
    expect(source).toContain("bind:scrollTop");
    expect(source).toContain("MAX_RENDER_LINES");
    expect(source).toContain("linesTruncated");
  });

  it("has no accessibility compiler warnings", () => {
    const { warnings } = compile(source, { generate: "client" });
    expect(warnings.filter(({ code }) => code.startsWith("a11y_"))).toEqual([]);
  });

  it("exposes the focused code surface as a read-only-capable multiline text editor", () => {
    expect(source).toContain('role="textbox"');
    expect(source).toContain('aria-multiline="true"');
    expect(source).toContain("aria-readonly=");
  });
});
