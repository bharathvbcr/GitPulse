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
    expect(source).toContain("<MarkDevViewer");
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
