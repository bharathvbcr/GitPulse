import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "MediaViewer.svelte"), "utf8");

describe("MediaViewer", () => {
  it("supports image viewing with zoom controls and fit options", () => {
    expect(source).toContain("imageZoom");
    expect(source).toContain("imageFit");
    expect(source).toContain("imageSrc");
  });

  it("delegates markdown viewing to integrated MarkDevViewer", () => {
    expect(source).toContain("MarkDevViewer");
    expect(source).toContain("isMarkdown");
    // The reader and its 21 KB parser are deferred rather than shipped in the
    // startup chunk, so the delegation goes through LazyMount. The loader must
    // stay at module scope: LazyMount keys its cache on loader identity, and
    // an inline arrow would remount the reader on every parent update.
    expect(source).toMatch(
      /const loadMarkDevViewer = \(\) => import\("\.\/MarkDevViewer\.svelte"\)/,
    );
    expect(source).toContain("load={loadMarkDevViewer}");
  });

  it("hands the deferred markdown reader the same props it took directly", () => {
    // A deferred component is only equivalent if nothing was dropped on the
    // way through the prop bag.
    const bag = source.slice(source.indexOf("load={loadMarkDevViewer}"));
    const props = bag.slice(0, bag.indexOf("/>"));
    for (const prop of [
      "filePath",
      "blob",
      "draftContent",
      "dirty",
      "onSave",
      "onDraftChange",
      "onRequestDiscard",
    ]) {
      expect(props).toContain(prop);
    }
  });

  it("forwards the canonical draft lifecycle to every CodeViewer surface", () => {
    expect(source).toContain("draftContent");
    expect(source).toContain("onDraftChange");
    expect(source).toContain("onRequestDiscard");
    expect(source).toContain("dirty");
  });

  it("supports binary hex dump viewing", () => {
    expect(source).toContain("hexRows");
    expect(source).toContain("hexDumpRows");
    expect(source).toContain("bytesFromBase64Prefix");
    expect(source).toContain("blob.is_binary");
    expect(source).toContain("Offset");
    expect(source).toContain("Decoded ASCII");
    expect(source).toContain("joinWorktreePath");
  });
});
