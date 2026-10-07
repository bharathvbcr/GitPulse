import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { compile } from "svelte/compiler";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import CodeViewer from "./CodeViewer.svelte";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "CodeViewer.svelte"), "utf8");

describe("CodeViewer", () => {
  it("renders immutable previews without an Edit or Save control", () => {
    const { body } = render(CodeViewer, { props: { filePath: "stash.diff", content: "+new content\n", readOnly: true } });
    expect(body).not.toContain("<span>Edit</span>");
    expect(body).not.toContain("Save (⌘S)");
    expect(body).toContain('aria-readonly="true"');
  });
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
    // The source is the text as the textarea can hold it (CRLF -> LF), so a
    // CRLF file is not dirty until something other than its endings changes.
    expect(source).toContain("onDraftChange?.(value, editBaseline)");
    expect(source).toContain('content.replace(/\\r\\n/g, "\\n")');
    expect(source).toContain("Unsaved");
  });

  it("shows the file's line endings", () => {
    const crlf = render(CodeViewer, { props: { filePath: "win.txt", content: "a\r\nb\r\n", eol: "crlf" } }).body;
    expect(crlf).toMatch(/data-testid="code-eol"[^>]*>CRLF</);
    expect(crlf).toContain("<span>Edit</span>");
    const lf = render(CodeViewer, { props: { filePath: "unix.txt", content: "a\n", eol: "lf" } }).body;
    expect(lf).toMatch(/data-testid="code-eol"[^>]*>LF</);
  });

  it("names a lossy decode and refuses to edit it", () => {
    const { body } = render(CodeViewer, {
      props: { filePath: "latin1.txt", content: "caf\u{FFFD}\n", eol: "lf", invalidUtf8Bytes: 2 },
    });
    expect(body).toContain("Not UTF-8 · 2 bytes shown as \u{FFFD}");
    expect(body).toMatch(/<button[^>]*disabled[^>]*title="2 bytes in this file are not valid UTF-8[^"]*saving would replace them permanently/);
  });

  it("refuses to edit a file with mixed line endings", () => {
    const { body } = render(CodeViewer, { props: { filePath: "mixed.txt", content: "a\r\nb\n", eol: "mixed" } });
    expect(body).toMatch(/data-testid="code-eol"[^>]*>Mixed EOL</);
    expect(body).toMatch(/<button[^>]*disabled[^>]*title="This file mixes line endings/);
  });

  it("never saves while the file cannot be edited faithfully", () => {
    const save = source.slice(source.indexOf("async function saveChanges"));
    expect(save.indexOf("if (editBlockedReason)")).toBeGreaterThan(-1);
    expect(save.indexOf("if (editBlockedReason)")).toBeLessThan(save.indexOf("cmd_write_file_content"));
    expect(source).toContain("if (readOnly || editBlockedReason) return;");
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

  it("scrolls long lines and chrome instead of letting them overlap", () => {
    expect(source).toContain("contentWidth");
    expect(source).toContain("scaledRowHeight");
    expect(source).toContain("style:height=\"{rowPx}px\"");
    expect(source).toContain("rowHeight={rowPx}");
    expect(source).not.toContain("leading-5");
    expect(source).not.toContain("min-w-0 pr-4");
    expect(source).not.toContain("absolute top-10");
    expect(source).toContain("gp-header-scroll");
    expect(source).toContain('data-tip-place="above"');
    expect(source).toContain("<ScrollCue");
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

  describe("reveal requests", () => {
    const effect = source.slice(source.indexOf("const target = consumeReveal(path)") - 400);

    it("waits for rows before acting on a reveal", () => {
      // The request is recorded before the file is read. Scrolling a viewer
      // with no rows lands at zero and looks like the reveal never happened.
      expect(effect).toContain("const lineCount = rawLines.length");
      expect(effect).toContain("if (!path || lineCount === 0) return;");
    });

    it("asks only for a reveal naming this viewer's own file", () => {
      // A request for another file must stay in the slot, not be eaten here.
      expect(effect).toContain("consumeReveal(path)");
    });

    it("clamps a line past the end of the file rather than refusing it", () => {
      expect(effect).toContain("Math.min(target.line, lineCount)");
    });

    it("does not make the reveal depend on what scrolling reads", () => {
      // `scrollToLine` reads the zoom level; tracking it would re-run this
      // effect on every zoom change, long after the request was collected.
      expect(effect).toContain("untrack(() => {");
    });
  });
});
