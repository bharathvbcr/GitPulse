import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { openFileAtAnchor, PENDING_ANCHOR_TTL_MS, requestAnchor, takeAnchor } from "./pendingAnchor";

const here = dirname(fileURLToPath(import.meta.url));
const component = readFileSync(join(here, "../components/MarkdownContent.svelte"), "utf8");
const viewer = readFileSync(join(here, "../components/files/MarkDevViewer.svelte"), "utf8");

describe("pendingAnchor", () => {
  it("hands a cross-file fragment to the viewer of that file exactly once", () => {
    const opened: string[] = [];
    openFileAtAnchor((path) => opened.push(path), "/repo", "docs/other.md", "setup");
    expect(opened).toEqual(["docs/other.md"]);
    expect(takeAnchor("/repo", "docs/other.md")).toBe("setup");
    expect(takeAnchor("/repo", "docs/other.md")).toBeNull();
  });

  it("is not delivered to another file or another repository", () => {
    requestAnchor("/repo", "a.md", "x");
    expect(takeAnchor("/repo", "b.md")).toBeNull();
    expect(takeAnchor("/other", "a.md")).toBeNull();
    expect(takeAnchor("/repo", "a.md")).toBe("x");
  });

  it("expires, so a file that never rendered cannot scroll a later one", () => {
    requestAnchor("/repo", "img.png", "x", 1000);
    expect(takeAnchor("/repo", "img.png", 1000 + PENDING_ANCHOR_TTL_MS + 1)).toBeNull();
  });

  it("a link without a fragment clears an earlier request", () => {
    requestAnchor("/repo", "a.md", "x");
    openFileAtAnchor(() => {}, "/repo", "a.md", null);
    expect(takeAnchor("/repo", "a.md")).toBeNull();
  });

  it("is wired from the link click to the markdown viewer", () => {
    expect(component).toContain("openFileAtAnchor(");
    expect(component).not.toMatch(/repoStore\.selectFilePath\(path\)/);
    expect(viewer).toContain("takeAnchor(");
  });
});
