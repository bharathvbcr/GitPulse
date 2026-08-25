import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "DiffViewer.svelte"),
  "utf8"
);

describe("DiffViewer row chrome", () => {
  it("does not draw a horizontal rule under every diff line", () => {
    expect(source).not.toContain("divide-y");
    expect(source).not.toMatch(/border-y\s+border-border/);
  });

  it("lets long diff lines scroll instead of clipping silently", () => {
    // Every unified row container scrolls horizontally; none hard-clips.
    expect(source).not.toMatch(/overflow-hidden[^"']*whitespace-pre/);
    expect(source).toContain('overflow-x-auto" style="height: {ROW_HEIGHT}px;"');
  });

  it("renders commit metadata and binary notices as their own row kinds", () => {
    expect(source).toContain('line.type === "meta"');
    expect(source).toContain('line.type === "binary"');
    expect(source).toMatch(/binary.*<\/span>\s*<span class="whitespace-pre">\{line\.content\}<\/span>/s);
  });

  it("drives the empty state from emptyDiffCopy so merges get merge copy", () => {
    expect(source).toContain("emptyDiffCopy(");
    expect(source).toContain("title={emptyCopy.title}");
    expect(source).toContain('hint={emptyCopy.hint}');
  });

  it("excludes metadata from the visible-lines stat", () => {
    expect(source).toContain("contentLineCount");
    expect(source.match(/lines\.length\.toLocaleString\(\)/)).toBeNull();
  });

  it("offers hunk and line-level staging only for working-tree files", () => {
    expect(source).toContain("Stage Hunk");
    expect(source).toContain("Unstage Hunk");
    expect(source).toContain("Stage Selected");
    expect(source).toContain("Unstage Selected");
    expect(source).toContain("isWorkingTreeFile");
    expect(source).toContain("statuses.some");
  });

  it("supports click/drag range selection on add and del lines", () => {
    expect(source).toContain("onLinePointerDown");
    expect(source).toContain("onLinePointerEnter");
    expect(source).toContain("selectRange");
    expect(source).toContain("onpointerdown");
  });

  it("annotates multi-line replacement blocks, not just adjacent pairs", () => {
    expect(source).toContain("replacementBlockBounds");
    expect(source).toContain("annotateRange(lines, bounds[0], bounds[1])");
  });
});
