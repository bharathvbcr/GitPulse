import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import ImageDiffViewer from "./ImageDiffViewer.svelte";

// The pane's states are rendered server-side here; the live behaviour — a
// previous file's picture clearing on selection, Retry reading again — is
// checked in harness/diff.html, because it lives in DiffViewer's effects.
const IMG = "data:image/png;base64,QUJD";
const body = (props: Record<string, unknown>) => render(ImageDiffViewer, { props }).body;

describe("ImageDiffViewer states", () => {
  it("says it is loading while the pictures are read, and never claims there is nothing to compare", () => {
    const html = body({ filePath: "logo.png", status: "loading" });
    expect(html).toContain("Loading images");
    expect(html).toContain('aria-busy="true"');
    expect(html).not.toContain("No image");
    expect(html).not.toContain("Nothing to compare");
  });

  it("names a failed read with its reason and offers Try again", () => {
    const html = body({ status: "failed", error: "blob unreadable", onRetry: () => {} });
    expect(html).toContain("Couldn't read this image");
    expect(html).toContain("blob unreadable");
    expect(html).toContain("Try again");
    expect(html).not.toContain("No image");
  });

  it("keeps the real empty state for a read that settled with no picture on either side", () => {
    const html = body({ status: "ready" });
    expect(html).toContain("No image on either side");
    expect(html).not.toContain("Loading images");
    expect(html).not.toContain("Couldn't read");
  });

  it("labels the absent side in swipe mode instead of drawing one picture as both", () => {
    const deleted = body({ filePath: "a.png", oldSrc: IMG });
    expect(deleted).toContain("No current version (deleted)");
    expect(deleted).not.toContain("No previous version");
    const added = body({ filePath: "a.png", newSrc: IMG });
    expect(added).toContain("No previous version (added)");
    expect(added).not.toContain("No current version");
  });

  it("puts a side's own read error under its label, so a failure is not presented as Deleted", () => {
    const html = body({ oldSrc: IMG, newError: "gone from the working tree" });
    expect(html).toContain("No current version (deleted)");
    expect(html).toContain("could not be read: gone from the working tree");
  });

  it("shows no absence note when both pictures are there", () => {
    const html = body({ oldSrc: IMG, newSrc: IMG });
    expect(html).not.toContain("No previous version");
    expect(html).not.toContain("No current version");
  });

  it("wires Try again to the caller's handler", () => {
    const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "ImageDiffViewer.svelte"), "utf8");
    expect(source).toContain('onclick={onRetry}>Try again</button>');
  });
});
