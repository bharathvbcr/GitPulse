import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "MarkDevViewer.svelte"), "utf8");

describe("MarkDevViewer", () => {
  it("supports tri-mode viewing with rendered, split, and raw view options", () => {
    expect(source).toContain("viewMode");
    expect(source).toContain('"rendered"');
    expect(source).toContain('"split"');
    expect(source).toContain('"raw"');
  });

  it("integrates MarkDevLogo and brand header", () => {
    expect(source).toContain("MarkDevLogo");
    expect(source).toContain("MarkDev");
    expect(source).toContain(".MD");
  });

  it("calculates document reading stats and outline", () => {
    expect(source).toContain("calculateDocumentStats");
    expect(source).toContain("extractDocumentOutline");
    expect(source).toContain("showOutline");
    expect(source).toContain("stats.wordCount");
    expect(source).toContain("stats.readingTimeMinutes");
  });

  it("renders markdown through renderMarkDevMarkdown parser", () => {
    expect(source).toContain("renderMarkDevMarkdown");
    expect(source).toContain("renderedHtml");
  });

  it("embeds CodeViewer for raw and split modes with onSave support", () => {
    expect(source).toContain("<CodeViewer");
    expect(source).toContain("onSave");
    expect(source).toContain("draftContent");
    expect(source).toContain("onDraftChange");
    expect(source).toContain("onRequestDiscard");
    expect(source).toContain("dirty");
  });

  it("provides an action to open in MarkDev desktop application", () => {
    expect(source).toContain("openInMarkDev");
    expect(source).toContain("openPath");
    expect(source).toContain("Open in MarkDev");
  });

  it("handles copy code block events and source copying", () => {
    expect(source).toContain("handlePreviewClick");
    expect(source).toContain("copy-code-btn");
    expect(source).toContain("handleCopySource");
    expect(source).toContain("if (!(await copyText(rawContent)))");
    expect(source).toContain("if (!(await copyText(code)))");
    expect(source).toContain('repoStore.setError("Could not copy to clipboard")');
  });
});
