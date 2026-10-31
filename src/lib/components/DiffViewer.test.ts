import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "DiffViewer.svelte"),
  "utf8"
);

/**
 * What the diff pane does is checked by rendering it: `harness/diff.html`
 * (`node scripts/browser-regressions.mjs --harness diff`) mounts the real
 * DiffViewer against fixture IPC and checks the busy state, staging (which
 * hunk or line each action sends, and what a truncated diff refuses), find,
 * change stepping, split pairing, wrapping and wrapped-row sizing, the empty
 * state, accessible names, that a refresh keeps the reader's place, each row's
 * language and the header icon, the gutter's width and pinning, the minimap, a
 * late impact answer being dropped, image blobs, and the preference seeding. `harness/uncommitted.html`
 * checks the file rail beside it. Only what a rendered page cannot see stays
 * here: invariants about cost.
 */
describe("DiffViewer source contracts", () => {
  it("reads image blobs without the content digest it never uses", () => {
    // Both sides go through one helper, so there is one read to check and no
    // side that could be added later without the flag.
    const reads = source.split('invoke<FileBlob>("cmd_get_file_blob", {').slice(1);
    expect(reads.length).toBe(1);
    // Each read's argument object ends at the first `});` after it opens.
    for (const read of reads) expect(read.slice(0, read.indexOf("});"))).toContain("withDigest: false");
    expect(source.split("readImageSide(").length - 1).toBe(3); // the definition and the two sides
  });

  it("reads the two image sides separately, so one failing does not discard the other", () => {
    const load = source.slice(source.indexOf("async function loadImageBlobs"), source.indexOf("function retryImage"));
    expect(load).toContain("Promise.all");
    // The outer catch that nulled both sides is gone; a throw is caught per side.
    expect(load).not.toContain("catch");
    expect(source.slice(source.indexOf("async function readImageSide"), source.indexOf("async function loadImageBlobs"))).toContain("catch");
  });

  it("keeps the window key handler from taking another control's Enter and Escape", () => {
    // What a key does to a focused button is a rendered fact (harness/diff.html
    // dispatches it); these pin the structure that makes it true.
    const handler = source.slice(source.indexOf("function onWindowKeydown"), source.indexOf("Switches the diff to another commit"));
    expect(handler.slice(0, 400)).toContain("if (event.defaultPrevented) return;");
    // Enter that is not in the find box belongs to a control unless it is on the diff body.
    expect(handler).not.toMatch(/searchOpen && !typing && event\.key === "Enter"/);
    expect(handler).toContain("!onControl");
    expect(source).toContain("onkeydown={onSearchKeydown}");
  });

  it("says a copied patch is partial when the backend cut the diff", () => {
    const copy = source.slice(source.indexOf("async function copyPatch"), source.indexOf("let selectedGraphRow"));
    expect(copy).toContain("if (cutByBackend) toastStore.warning(");
    // A renderer cut does not shorten `selectedDiff`, so it must not rename the button.
    expect(source).toContain('cutByBackend ? "Copy partial patch" : "Copy patch"');
    expect(source).not.toContain('title="Copy the whole patch as unified diff text"');
  });

  it("compiles", () => {
    const { warnings } = compile(source, { generate: "client", filename: "DiffViewer.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });

  it("parses through one reference-identity cache and composes each line's spans once", () => {
    // A scroll frame must not re-parse the diff or re-tokenize a visible line;
    // a render check sees the same rows either way and cannot tell.
    expect(source).toContain("const parseCache = createParseCache();");
    expect(source).not.toContain("parseUnifiedDiff($repoStore.selectedDiff || \"\")");
    expect(source).toContain("const spanCache = new WeakMap<");
  });

  it("keeps the codeintel query effects off the whole repo store", () => {
    // `$repoStore.x` inside an effect subscribes to the whole store, which
    // publishes a fresh object per keystroke and per status poll, so the
    // impact and head queries would be cancelled and re-sent on every one.
    for (const marker of [
      "getImpactAtRung(repoPath, filePath",
      'invoke<string>("cmd_get_head_id"',
      "getImpactLayeredMany(repo, kept",
    ]) {
      const at = source.indexOf(marker);
      expect(at, `${marker} is no longer in the component`).toBeGreaterThan(-1);
      const start = source.lastIndexOf("$effect(() => {", at);
      const end = source.indexOf("\n  });\n", at);
      expect(start).toBeGreaterThan(-1);
      expect(source.slice(start, end), `the effect around ${marker} reads $repoStore`).not.toContain("$repoStore");
    }
  });
});
