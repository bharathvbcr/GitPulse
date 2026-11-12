import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const source = readFileSync(new URL("./FileTreeColumn.svelte", import.meta.url), "utf8");

describe("FileTreeColumn source contract", () => {
  it("is a keyboard splitter for the shared file-tree width", () => {
    expect(source).toContain('aria-label="Resize file tree"');
    expect(source).toContain('role="separator"');
    expect(source).toContain('aria-orientation="vertical"');
    expect(source).toContain("aria-valuemin={FILE_TREE_MIN_WIDTH}");
    expect(source).toContain("aria-valuemax={maxWidth}");
    expect(source).toContain("aria-valuenow={displayed}");
    expect(source).toContain('event.key === "ArrowLeft"');
    expect(source).toContain('event.key === "ArrowRight"');
    expect(source).toContain('event.key === "Home"');
    expect(source).toContain('event.key === "End"');
    expect(source).toContain("FILE_TREE_RESIZE_STEP");
    expect(source).toContain("fileTreeColumn.setWidthLive");
    expect(source).toContain("fileTreeColumn.setWidth");
    expect(source).toContain("data-file-tree-column");
    expect(source).toContain("max-width: 50%");
  });

  it("double-click resets to the old w-72 unless the pointer just dragged", () => {
    expect(source).toContain("fileTreeColumn.setWidth(FILE_TREE_DEFAULT_WIDTH, paneWidth, reserve)");
    expect(source).toContain("if (Date.now() - lastDragEndAt <= 250) return;");
    expect(source).toContain("if (moved) {");
    expect(source).toContain("lastDragEndAt = Date.now();");
  });
});
