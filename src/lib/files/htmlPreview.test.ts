import { afterEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import {
  analyzeHtmlDocument,
  clearHtmlViewMode,
  clearHtmlViewModeIfMatches,
  clearRememberedHtmlViewModes,
  consumeHtmlViewMode,
  extractTerminalTokenAtCell,
  extractTerminalTokenAtColumn,
  HTML_MODE_TTL_MS,
  HTML_PREVIEW_SANDBOX_CSP,
  HTML_SPLIT_HORIZONTAL_MIN_WIDTH,
  HTML_TOOLBAR_INLINE_MIN_WIDTH,
  htmlViewModeRequest,
  inlineLocalHtmlAssets,
  isCellInTerminalRange,
  isHtmlFile,
  matchHtmlViewMode,
  MAX_HTML_PREVIEW_BYTES,
  MAX_INLINED_CSS_BYTES,
  MAX_INLINED_IMAGE_BYTES,
  MAX_INLINED_IMAGES,
  MAX_INLINED_STYLESHEETS,
  peekHtmlViewModeMatch,
  prepareHtmlPreviewDocument,
  recalledHtmlViewMode,
  rememberHtmlViewMode,
  requestHtmlViewMode,
  resolveHtmlPreviewLayout,
  resolveHtmlSplitDirection,
  resolveRelativeRepoAsset,
  resolveTerminalHtmlCandidate,
  scanLocalCssUrlSpans,
} from "./htmlPreview";
import type { LinkBuffer } from "../terminal/links";
import type { FileBlob } from "./types";

afterEach(() => {
  clearHtmlViewMode();
  clearRememberedHtmlViewModes();
});

describe("isHtmlFile", () => {
  it("recognizes .html, .htm, and .xhtml files case-insensitively", () => {
    expect(isHtmlFile("index.html")).toBe(true);
    expect(isHtmlFile("docs/GUIDE.HTM")).toBe(true);
    expect(isHtmlFile("src/views/page.xhtml")).toBe(true);
    expect(isHtmlFile("docs/preview.html?v=1#top")).toBe(true);
  });

  it("rejects non-HTML, dotfile-only, and empty paths", () => {
    expect(isHtmlFile("")).toBe(false);
    expect(isHtmlFile(null)).toBe(false);
    expect(isHtmlFile(undefined)).toBe(false);
    expect(isHtmlFile(".html")).toBe(false);
    expect(isHtmlFile("docs/.htm")).toBe(false);
    expect(isHtmlFile("index.html.bak")).toBe(false);
    expect(isHtmlFile("src/html")).toBe(false);
    expect(isHtmlFile("README.md")).toBe(false);
  });
});

describe("htmlViewModeRequest one-shot store", () => {
  it("records, matches, and consumes mode requests once", () => {
    expect(requestHtmlViewMode("/repo/", "./docs/index.html", "split", 1000)).toBe(true);
    const current = get(htmlViewModeRequest);
    expect(current?.mode).toBe("split");
    expect(peekHtmlViewModeMatch(current, "/repo", "docs/index.html", 1050)).toBe(true);
    expect(peekHtmlViewModeMatch(current, "/other", "docs/index.html", 1050)).toBe(false);
    expect(peekHtmlViewModeMatch(current, "/repo", "docs/other.html", 1050)).toBe(false);

    expect(consumeHtmlViewMode("/other", "docs/index.html", 1050)).toBeNull();
    expect(consumeHtmlViewMode("/repo", "docs/index.html", 1050)).toBe("split");
    expect(consumeHtmlViewMode("/repo", "docs/index.html", 1050)).toBeNull();
  });

  it("expires stale requests past TTL", () => {
    requestHtmlViewMode("/repo", "index.html", "preview", 1000);
    expect(consumeHtmlViewMode("/repo", "index.html", 1000 + HTML_MODE_TTL_MS + 1)).toBeNull();
    expect(get(htmlViewModeRequest)).toBeNull();
  });

  it("rejects non-HTML paths and bumps nonce on repeat requests to the same file", () => {
    expect(requestHtmlViewMode("/repo", "src/app.ts", "preview")).toBe(false);
    expect(requestHtmlViewMode("/repo", "index.html", "preview", 100)).toBe(true);
    const first = get(htmlViewModeRequest);
    expect(requestHtmlViewMode("/repo", "index.html", "preview", 200)).toBe(true);
    const second = get(htmlViewModeRequest);
    expect(second!.nonce).toBeGreaterThan(first!.nonce);
  });

  it("matches fallbackPath when a subdir terminal link resolves to repository root", () => {
    expect(requestHtmlViewMode("/repo", "sub/index.html", "split", 1000, "index.html")).toBe(true);
    expect(consumeHtmlViewMode("/repo", "index.html", 1010)).toBe("split");
    expect(consumeHtmlViewMode("/repo", "sub/index.html", 1010)).toBeNull();
  });

  it("supports non-destructive multi-host matching via matchHtmlViewMode and clears only matching file", () => {
    expect(requestHtmlViewMode("/repo", "docs/index.html", "split", 1000)).toBe(true);
    const req = get(htmlViewModeRequest);
    const firstHost = matchHtmlViewMode(req, "/repo", "docs/index.html", 1010);
    const secondHost = matchHtmlViewMode(get(htmlViewModeRequest), "/repo", "docs/index.html", 1020);
    expect(firstHost).not.toBeNull();
    expect(secondHost).toEqual(firstHost);

    clearHtmlViewModeIfMatches("/repo", "docs/other.html", 1025);
    expect(get(htmlViewModeRequest)).not.toBeNull();
    clearHtmlViewModeIfMatches("/repo", "docs/index.html", 1030);
    expect(get(htmlViewModeRequest)).toBeNull();
  });

  it("remembers per-file modes in a bounded cache", () => {
    rememberHtmlViewMode("/repo/", "a.html", "code");
    expect(recalledHtmlViewMode("/repo", "./a.html")).toBe("code");
    for (let i = 0; i < 70; i++) {
      rememberHtmlViewMode("/repo", `p${i}.html`, "split");
    }
    expect(recalledHtmlViewMode("/repo", "a.html")).toBeNull();
    expect(recalledHtmlViewMode("/repo", "p69.html")).toBe("split");
  });
});

describe("resolveHtmlPreviewLayout", () => {
  it("handles unmeasured, narrow, and wide widths", () => {
    expect(resolveHtmlPreviewLayout(0)).toEqual({
      mode: "unmeasured",
      splitDirection: "horizontal",
      toolbar: "inline",
    });
    expect(resolveHtmlPreviewLayout(HTML_TOOLBAR_INLINE_MIN_WIDTH - 1)).toEqual({
      mode: "compact",
      splitDirection: "vertical",
      toolbar: "stacked",
    });
    expect(resolveHtmlPreviewLayout(HTML_SPLIT_HORIZONTAL_MIN_WIDTH - 1)).toEqual({
      mode: "compact",
      splitDirection: "vertical",
      toolbar: "inline",
    });
    expect(resolveHtmlPreviewLayout(HTML_SPLIT_HORIZONTAL_MIN_WIDTH)).toEqual({
      mode: "wide",
      splitDirection: "horizontal",
      toolbar: "inline",
    });
  });

  it("allows toggling split orientation only when pane width is wide enough", () => {
    expect(resolveHtmlSplitDirection(0, "horizontal")).toEqual({
      direction: "horizontal",
      canToggle: true,
    });
    expect(resolveHtmlSplitDirection(HTML_SPLIT_HORIZONTAL_MIN_WIDTH, "horizontal")).toEqual({
      direction: "horizontal",
      canToggle: true,
    });
    expect(resolveHtmlSplitDirection(HTML_SPLIT_HORIZONTAL_MIN_WIDTH, "vertical")).toEqual({
      direction: "vertical",
      canToggle: true,
    });
    expect(resolveHtmlSplitDirection(HTML_SPLIT_HORIZONTAL_MIN_WIDTH - 1, "horizontal")).toEqual({
      direction: "vertical",
      canToggle: false,
    });
    expect(resolveHtmlSplitDirection(HTML_SPLIT_HORIZONTAL_MIN_WIDTH - 1, "vertical")).toEqual({
      direction: "vertical",
      canToggle: false,
    });
  });
});

describe("resolveRelativeRepoAsset", () => {
  it("resolves repo-contained relative and root-relative paths", () => {
    expect(resolveRelativeRepoAsset("/repo", "docs/sub/page.html", "./style.css?v=2#top")).toBe(
      "docs/sub/style.css",
    );
    expect(resolveRelativeRepoAsset("/repo", "docs/sub/page.html", "../img/logo.png")).toBe(
      "docs/img/logo.png",
    );
    expect(resolveRelativeRepoAsset("/repo", "docs/sub/page.html", "/assets/main.css")).toBe(
      "assets/main.css",
    );
  });

  it("rejects traversal escapes, external schemes, and malformed paths", () => {
    for (const bad of [
      "../../etc/passwd",
      "https://cdn.example.com/a.css",
      "http://localhost/a.css",
      "//cdn.example.com/a.css",
      "javascript:alert(1)",
      "data:image/png;base64,AAA",
      "file:///etc/hosts",
      "#section",
      "bad%ZZ.css",
      "null%00.css",
      "folder/",
      "",
    ]) {
      expect(resolveRelativeRepoAsset("/repo", "docs/page.html", bad), bad).toBeNull();
    }
  });
});

describe("analyzeHtmlDocument & prepareHtmlPreviewDocument", () => {
  it("extracts title, scripts, inline styles, local and external resources", () => {
    const html = `<!DOCTYPE html>
<html>
  <head>
    <title>Demo &amp; Preview</title>
    <link rel="stylesheet" href="./styles.css">
    <link rel="stylesheet" href="https://cdn.example.com/reset.css">
    <style>body { margin: 0; }</style>
    <script src="https://cdn.example.com/app.js"></script>
  </head>
  <body>
    <img src="./hero.png" alt="Hero">
    <script>alert("xss")</script>
  </body>
</html>`;
    const stats = analyzeHtmlDocument(html);
    expect(stats.title).toBe("Demo & Preview");
    expect(stats.hasDoctype).toBe(true);
    expect(stats.scriptCount).toBe(2);
    expect(stats.externalScriptCount).toBe(1);
    expect(stats.inlineStyleCount).toBe(1);
    expect(stats.localStylesheetRefs).toEqual(["./styles.css"]);
    expect(stats.externalStylesheetCount).toBe(1);
    expect(stats.localImageRefs).toEqual(["./hero.png"]);
  });

  it("strips scripts, refresh meta, base tags, injects strict CSP, and inlines local assets safely", () => {
    const html = `<!DOCTYPE html>
<html>
  <head>
    <base href="https://evil.example/">
    <meta http-equiv="refresh" content="0;url=https://evil.example">
    <link rel="stylesheet" href="./site.css">
    <SCRIPT type="module">window.parent.postMessage("boom", "*")</SCRIPT>
    <script src="./evil.js" />
  </head>
  <body>
    <img src="./pic.png" alt="Pic">
    <h1>Hello</h1>
  </body>
</html>`;
    const prepared = prepareHtmlPreviewDocument(html, {
      canvas: "light",
      inlinedStylesheets: {
        "./site.css": "body { color: red; } </style><script>alert(1)</script>",
      },
      inlinedImages: {
        "./pic.png": "data:image/png;base64,AAAA",
      },
    });

    expect(prepared.tooLarge).toBe(false);
    expect(prepared.inlinedStylesheetCount).toBe(1);
    expect(prepared.inlinedImageCount).toBe(1);
    expect(prepared.srcdoc).toContain(HTML_PREVIEW_SANDBOX_CSP);
    const withoutInlinedStyle = prepared.srcdoc.replace(
      /<style data-gitpulse-inlined="[^"]*">[\s\S]*?<\/style>/g,
      "",
    );
    expect(withoutInlinedStyle).not.toMatch(/<script\b/i);
    expect(prepared.srcdoc).not.toMatch(/http-equiv="refresh"/i);
    expect(prepared.srcdoc).not.toMatch(/<base\b/i);
    expect(prepared.srcdoc).toContain("<\\/style>");
    expect(prepared.srcdoc).toContain('src="data:image/png;base64,AAAA"');
    expect(prepared.srcdoc).toContain("<h1>Hello</h1>");
  });

  it("wraps bare HTML fragments with head and CSP", () => {
    const prepared = prepareHtmlPreviewDocument("<div class='card'>Hi</div>", { canvas: "dark" });
    expect(prepared.srcdoc).toContain("<!DOCTYPE html>");
    expect(prepared.srcdoc).toContain(HTML_PREVIEW_SANDBOX_CSP);
    expect(prepared.srcdoc).toContain("color-scheme:dark");
    expect(prepared.srcdoc).toContain("<div class='card'>Hi</div>");
  });

  it("refuses oversized HTML documents cleanly with omittedBytes", () => {
    const huge = "a".repeat(MAX_HTML_PREVIEW_BYTES + 128);
    const prepared = prepareHtmlPreviewDocument(huge);
    expect(prepared.tooLarge).toBe(true);
    expect(prepared.omittedBytes).toBe(128);
    expect(prepared.srcdoc).toBe("");
  });

  it("handles empty HTML cleanly", () => {
    const prepared = prepareHtmlPreviewDocument("");
    expect(prepared.tooLarge).toBe(false);
    expect(prepared.stats.byteCount).toBe(0);
    expect(prepared.srcdoc).toContain(HTML_PREVIEW_SANDBOX_CSP);
  });

  it("ignores <script>, <head>, <link>, and <img> inside HTML comments so comments never truncate the page or neutralize CSP", () => {
    const html = `<!-- <head> note: never use <script> tags or <link rel="stylesheet" href="./ghost.css"> here -->
<!DOCTYPE html>
<html>
  <head><title>Real Page</title></head>
  <body><h1>Survived</h1></body>
</html>`;
    const prepared = prepareHtmlPreviewDocument(html);
    expect(prepared.stats.scriptCount).toBe(0);
    expect(prepared.stats.localStylesheetRefs).toEqual([]);
    expect(prepared.stats.hasDoctype).toBe(true);
    expect(prepared.stats.title).toBe("Real Page");
    expect(prepared.srcdoc).toContain("<h1>Survived</h1>");
    const commentEnd = prepared.srcdoc.indexOf("-->");
    const cspPos = prepared.srcdoc.indexOf("Content-Security-Policy");
    const realHeadPos = prepared.srcdoc.indexOf("<head>", commentEnd);
    expect(cspPos).toBeGreaterThan(realHeadPos);
  });

  it("does not clobber data-src or data-href attributes and respects > inside quoted attributes", () => {
    const html = `<link data-href="./fake.css" title="a > b" rel="stylesheet" href="./real.css">
<img data-src="./lazy.png" alt="1 > 0" src="./real.png">`;
    const stats = analyzeHtmlDocument(html);
    expect(stats.localStylesheetRefs).toEqual(["./real.css"]);
    expect(stats.localImageRefs).toEqual(["./real.png"]);

    const prepared = prepareHtmlPreviewDocument(html, {
      inlinedStylesheets: { "./real.css": "body{color:teal;}" },
      inlinedImages: {
        "./lazy.png": "data:image/png;base64,LAZY",
        "./real.png": "data:image/svg+xml;utf8,$&$$REAL",
      },
    });
    expect(prepared.srcdoc).toContain('data-src="./lazy.png"');
    expect(prepared.srcdoc).toContain('src="data:image/svg+xml;utf8,$&amp;$$REAL"');
    expect(prepared.srcdoc).toContain("body{color:teal;}");
  });

  it("contains external <a href> links with target=_blank and rel=noopener noreferrer while preserving in-page #fragment anchors", () => {
    const html = `<!DOCTYPE html><html><head></head><body>
      <a href="#section">Jump</a>
      <a href="https://example.com/docs">External</a>
      <a href="./other.html" target="_self">Relative</a>
      <a href="./bool.html" target>Boolean target</a>
      <a href="./dup.html" target="_self" target="_top">Duplicate target</a>
      <a href="https://example.com/ext" rel="external">With rel</a>
      <a href="https://example.com/part" target="_self" rel="noopener">Partial rel</a>
      <a href="https://example.com/brel" target="_self" rel>Boolean rel</a>
      <a href="https://example.com/safe" target="_blank" rel="noopener noreferrer">Already safe</a>
    </body></html>`;
    const prepared = prepareHtmlPreviewDocument(html);
    expect(prepared.srcdoc).toContain('<a href="#section">Jump</a>');
    expect(prepared.srcdoc).toContain(
      '<a href="https://example.com/docs" target="_blank" rel="noopener noreferrer">External</a>',
    );
    expect(prepared.srcdoc).toContain(
      '<a href="./other.html" target="_blank" rel="noopener noreferrer">Relative</a>',
    );
    expect(prepared.srcdoc).toContain(
      '<a href="./bool.html" target="_blank" rel="noopener noreferrer">Boolean target</a>',
    );
    expect(prepared.srcdoc).not.toMatch(/href="\.\/bool\.html"[^>]*\btarget\b[^>]*\btarget=/i);
    expect(prepared.srcdoc).not.toMatch(/target="_top"/i);
    expect(prepared.srcdoc).toContain(
      '<a href="https://example.com/ext" rel="external noopener noreferrer" target="_blank">With rel</a>',
    );
    expect(prepared.srcdoc).toContain(
      '<a href="https://example.com/part" target="_blank" rel="noopener noreferrer">Partial rel</a>',
    );
    expect(prepared.srcdoc).toContain(
      '<a href="https://example.com/brel" target="_blank" rel="noopener noreferrer">Boolean rel</a>',
    );
    expect(prepared.srcdoc).toContain(
      '<a href="https://example.com/safe" target="_blank" rel="noopener noreferrer">Already safe</a>',
    );
  });

  it("preserves RCDATA inside <title> and <textarea> without swallowing the document, while still stripping scripts inside <svg><title>", () => {
    const html = `<!DOCTYPE html>
<html>
  <head><title>Guide to <script> tags</title></head>
  <body>
    <textarea style="background: url('./paper.png')"><script>console.log(1)</script><link rel="stylesheet" href="./nope.css"></textarea>
    <svg><title><script>alert(1)</script></title></svg>
    <h1>Body Survived</h1>
  </body>
</html>`;
    const prepared = prepareHtmlPreviewDocument(html, {
      inlinedImages: { "./paper.png": "data:image/png;base64,PAPER" },
    });
    expect(prepared.stats.title).toBe("Guide to <script> tags");
    expect(prepared.stats.scriptCount).toBe(1);
    expect(prepared.stats.localStylesheetRefs).toEqual([]);
    expect(prepared.srcdoc).toContain("<title>Guide to <script> tags</title>");
    expect(prepared.srcdoc).toContain("<script>console.log(1)</script>");
    expect(prepared.srcdoc).toContain('style="background: url(&quot;data:image/png;base64,PAPER&quot;)"');
    expect(prepared.srcdoc).toContain("<h1>Body Survived</h1>");
    expect(prepared.srcdoc).not.toContain("alert(1)");
  });

  it("contains self-closing unquoted target with trailing space, <area href>, SVG <a xlink:href>, and empty <a href>", () => {
    const html = `<!DOCTYPE html><html><head></head><body>
      <a href="https://example.com/slash" target=_self/ >SlashSpace</a>
      <map name="m"><area shape="rect" coords="0,0,10,10" href="https://example.com/area"></map>
      <svg><a xlink:href="https://example.com/svg"><rect width="10" height="10"/></a></svg>
      <a href="">EmptyHref</a>
    </body></html>`;
    const prepared = prepareHtmlPreviewDocument(html);
    expect(prepared.srcdoc).toContain(
      '<a href="https://example.com/slash" target="_blank" rel="noopener noreferrer"/ >SlashSpace</a>',
    );
    expect(prepared.srcdoc).toContain(
      '<area shape="rect" coords="0,0,10,10" href="https://example.com/area" target="_blank" rel="noopener noreferrer">',
    );
    expect(prepared.srcdoc).toContain(
      '<a xlink:href="https://example.com/svg" target="_blank" rel="noopener noreferrer">',
    );
    expect(prepared.srcdoc).toContain(
      '<a href="" target="_blank" rel="noopener noreferrer">EmptyHref</a>',
    );
  });

  it("inlines local CSS url(...) images inside inline <style> blocks and external stylesheets", async () => {
    const html = `<!DOCTYPE html><html><head>
      <link rel="stylesheet" href="./css/site.css">
      <style>.hero { background-image: url('./img/banner.svg'); } /* url('./ignored.png') */</style>
    </head><body><div class="hero"></div></body></html>`;
    const blobs: Record<string, FileBlob> = {
      "docs/css/site.css": {
        path: "docs/css/site.css",
        is_binary: false,
        is_image: false,
        mime: "text/css",
        text: `.card { background: url("../img/tile.png") repeat; content: "url(../img/not-a-url.png)"; }`,
        base64: null,
        invalid_utf8_bytes: 0,
      },
      "docs/img/banner.svg": {
        path: "docs/img/banner.svg",
        is_binary: false,
        is_image: true,
        mime: "image/svg+xml",
        text: "<svg></svg>",
        base64: null,
        invalid_utf8_bytes: 0,
      },
      "docs/img/tile.png": {
        path: "docs/img/tile.png",
        is_binary: true,
        is_image: true,
        mime: "image/png",
        text: null,
        base64: "iVBORw==",
        invalid_utf8_bytes: 0,
      },
    };
    expect(scanLocalCssUrlSpans(blobs["docs/css/site.css"].text!).map((s) => s.rawRef)).toEqual([
      "../img/tile.png",
    ]);

    const assets = await inlineLocalHtmlAssets(
      html,
      { repoPath: "/repo", filePath: "docs/index.html" },
      async (_r, p) => blobs[p] ?? null,
    );
    expect(assets.inlinedStylesheetCount).toBe(1);
    expect(assets.inlinedImageCount).toBe(2);
    expect(assets.stylesheets["./css/site.css"]).toContain('url("data:image/png;base64,iVBORw==")');
    expect(assets.stylesheets["./css/site.css"]).toContain('content: "url(../img/not-a-url.png)"');

    const prepared = prepareHtmlPreviewDocument(html, {
      inlinedStylesheets: assets.stylesheets,
      inlinedImages: assets.images,
    });
    expect(assets.images["./img/banner.svg"]?.startsWith("data:image/svg+xml;base64,")).toBe(true);
    const decodedBanner = new TextDecoder("utf-8").decode(
      Uint8Array.from(atob(assets.images["./img/banner.svg"]!.slice("data:image/svg+xml;base64,".length)), (c) => c.charCodeAt(0)),
    );
    expect(decodedBanner).toContain('xmlns="http://www.w3.org/2000/svg"');
    expect(decodedBanner).toContain('viewBox="0 0 300 150"');
    expect(prepared.srcdoc).toContain(`url("${assets.images["./img/banner.svg"]}")`);
    expect(prepared.srcdoc).toContain('url("data:image/png;base64,iVBORw==")');
  });
});

describe("inlineLocalHtmlAssets", () => {
  const makeBlob = (partial: Partial<FileBlob>): FileBlob => ({
    path: partial.path ?? "file",
    is_binary: partial.is_binary ?? false,
    is_image: partial.is_image ?? false,
    mime: partial.mime ?? "text/plain",
    text: partial.text ?? null,
    base64: partial.base64 ?? null,
    invalid_utf8_bytes: 0,
  });

  it("inlines local CSS, PNG, and sanitized SVG while reporting missing assets and enforcing caps", async () => {
    const html = `
      <link rel="stylesheet" href="./a.css">
      <link rel="stylesheet" href="./missing.css">
      <link rel="stylesheet" href="./huge.css">
      <img src="./logo.png">
      <img src="./icon.svg">
      <img src="./giant.png">
      <img src="../../outside.png">
    `;
    const blobs: Record<string, FileBlob> = {
      "docs/a.css": makeBlob({ path: "docs/a.css", mime: "text/css", text: "h1 { color: green; }" }),
      "docs/huge.css": makeBlob({ path: "docs/huge.css", mime: "text/css", text: "x".repeat(MAX_INLINED_CSS_BYTES + 10) }),
      "docs/logo.png": makeBlob({ path: "docs/logo.png", is_binary: true, is_image: true, mime: "image/png", base64: "iVBORw==" }),
      "docs/icon.svg": makeBlob({
        path: "docs/icon.svg",
        is_image: true,
        mime: "image/svg+xml",
        text: '<svg width="16" height="16" onload="alert(1)"><script>alert(2)</script><rect width="16" height="16"/></svg>',
      }),
      "docs/giant.png": makeBlob({
        path: "docs/giant.png",
        is_binary: true,
        is_image: true,
        mime: "image/png",
        base64: "A".repeat(Math.ceil((MAX_INLINED_IMAGE_BYTES * 4) / 3) + 64),
      }),
    };

    const result = await inlineLocalHtmlAssets(
      html,
      { repoPath: "/repo", filePath: "docs/page.html" },
      async (_repo, path) => blobs[path] ?? null,
    );

    expect(result.inlinedStylesheetCount).toBe(1);
    expect(result.stylesheets["./a.css"]).toBe("h1 { color: green; }");
    expect(result.inlinedImageCount).toBe(2);
    expect(result.images["./logo.png"]).toBe("data:image/png;base64,iVBORw==");
    expect(result.images["./icon.svg"]?.startsWith("data:image/svg+xml;base64,")).toBe(true);
    const decodedIcon = new TextDecoder("utf-8").decode(
      Uint8Array.from(atob(result.images["./icon.svg"]!.slice("data:image/svg+xml;base64,".length)), (c) => c.charCodeAt(0)),
    );
    expect(decodedIcon).toContain('xmlns="http://www.w3.org/2000/svg"');
    expect(decodedIcon).toContain('viewBox="0 0 16 16"');
    expect(decodedIcon).not.toContain("alert(");
    expect(decodedIcon).not.toContain("onload=");
    expect(result.missingAssets).toContain("./missing.css");
    expect(result.missingAssets).toContain("../../outside.png");
    expect(result.skippedCapCount).toBe(2);
  });

  it("caps total number of inlined stylesheets and images", async () => {
    const cssLinks = Array.from(
      { length: MAX_INLINED_STYLESHEETS + 3 },
      (_, i) => `<link rel="stylesheet" href="./s${i}.css">`,
    ).join("");
    const imgTags = Array.from(
      { length: MAX_INLINED_IMAGES + 2 },
      (_, i) => `<img src="./i${i}.svg">`,
    ).join("");
    const result = await inlineLocalHtmlAssets(
      cssLinks + imgTags,
      { repoPath: "/repo", filePath: "index.html" },
      async (_repo, path) =>
        path.endsWith(".css")
          ? makeBlob({ path, mime: "text/css", text: "body{}" })
          : makeBlob({ path, mime: "image/svg+xml", text: "<svg/>" }),
    );
    expect(result.inlinedStylesheetCount).toBe(MAX_INLINED_STYLESHEETS);
    expect(result.inlinedImageCount).toBe(MAX_INLINED_IMAGES);
    expect(result.skippedCapCount).toBe(5);
  });
});

describe("resolveTerminalHtmlCandidate", () => {
  it("resolves hovered links, subdir paths, and bare selected HTML filenames inside repo", () => {
    expect(resolveTerminalHtmlCandidate("docs/preview.html", "/repo")).toEqual({
      path: "docs/preview.html",
      line: null,
      column: null,
      fallbackPath: undefined,
    });
    expect(resolveTerminalHtmlCandidate("./demo.htm:14:2", "/repo")).toEqual({
      path: "demo.htm",
      line: 14,
      column: 2,
      fallbackPath: undefined,
    });
    expect(resolveTerminalHtmlCandidate('"index.html"', "/repo")).toEqual({
      path: "index.html",
      line: null,
      column: null,
    });
    expect(resolveTerminalHtmlCandidate("report.html", "/repo", "packages/web")).toEqual({
      path: "packages/web/report.html",
      line: null,
      column: null,
      fallbackPath: "report.html",
    });
  });

  it("rejects escapes, URLs, multiline selections, and non-HTML files", () => {
    for (const bad of [
      "../outside.html",
      "/etc/index.html",
      "https://example.com/index.html",
      "src/app.ts",
      "index.html\nother.html",
      "two words.html",
      "",
    ]) {
      expect(resolveTerminalHtmlCandidate(bad, "/repo"), bad).toBeNull();
    }
  });

  it("extracts bare and path-qualified tokens under a terminal column for direct right-click", () => {
    const lsLine = "README.md  index.html  docs/preview.htm:12  styles.css";
    expect(extractTerminalTokenAtColumn(lsLine, 12)).toBe("index.html");
    expect(extractTerminalTokenAtColumn(lsLine, 26)).toBe("docs/preview.htm:12");
    expect(extractTerminalTokenAtColumn(lsLine, 9)).toBeNull();
    expect(extractTerminalTokenAtColumn('open "page.xhtml".', 8)).toBe("page.xhtml");
    expect(extractTerminalTokenAtColumn("index.html", -1)).toBeNull();
    expect(extractTerminalTokenAtColumn("index.html", 99)).toBeNull();
  });
});

describe("extractTerminalTokenAtCell", () => {
  function makeTestBuffer(
    rows: Array<{ text: string; isWrapped?: boolean; widths?: number[]; omitWidths?: boolean }>,
  ): LinkBuffer {
    return {
      row(index: number) {
        const spec = rows[index - 1];
        if (!spec) return null;
        const columns: number[] = [];
        const widths: number[] = [];
        let col = 1;
        let glyphIndex = 0;
        for (const ch of spec.text) {
          const w = spec.widths?.[glyphIndex] ?? 1;
          for (let u = 0; u < ch.length; u++) {
            columns.push(col);
            widths.push(w);
          }
          col += w;
          glyphIndex += 1;
        }
        return {
          isWrapped: Boolean(spec.isWrapped),
          text: spec.text,
          columns,
          ...(spec.omitWidths ? {} : { widths }),
        };
      },
    };
  }

  it("hits exact token cells and returns null on blank space past a shorter row", () => {
    const buffer = makeTestBuffer([
      { text: "ls docs/index.html" }, // cols 1..18, ends at col 18
      { text: "open page.html     " }, // cols 1..19, trailing spaces at 15..19
    ]);
    // Exact hits on row 1 (`docs/index.html` occupies cols 4..18)
    expect(extractTerminalTokenAtCell(buffer, 1, 4)).toBe("docs/index.html");
    expect(extractTerminalTokenAtCell(buffer, 1, 10)).toBe("docs/index.html");
    expect(extractTerminalTokenAtCell(buffer, 1, 18)).toBe("docs/index.html");
    // Space between tokens on row 1
    expect(extractTerminalTokenAtCell(buffer, 1, 3)).toBeNull();
    // Blank space past end of shorter row 1 MUST miss (previously clamped to col 18)
    expect(extractTerminalTokenAtCell(buffer, 1, 19)).toBeNull();
    expect(extractTerminalTokenAtCell(buffer, 1, 60)).toBeNull();
    // Space-padded row 2 hits token on 6..14 and misses trailing spaces and past-end
    expect(extractTerminalTokenAtCell(buffer, 2, 6)).toBe("page.html");
    expect(extractTerminalTokenAtCell(buffer, 2, 14)).toBe("page.html");
    expect(extractTerminalTokenAtCell(buffer, 2, 15)).toBeNull();
    expect(extractTerminalTokenAtCell(buffer, 2, 40)).toBeNull();
    // Invalid / missing coordinates
    expect(extractTerminalTokenAtCell(buffer, 0, 1)).toBeNull();
    expect(extractTerminalTokenAtCell(buffer, 1, 0)).toBeNull();
    expect(extractTerminalTokenAtCell(buffer, 9, 1)).toBeNull();
  });

  it("handles double-width CJK and surrogate-pair emoji glyphs accurately", () => {
    // `世界 ` -> cols 1-2 (`世`), 3-4 (`界`), 5 (` `), then `docs/🚀.html` -> `docs/` 6..10, `🚀` 11-12, `.html` 13..17
    const buffer = makeTestBuffer([
      {
        text: "世界 docs/🚀.html",
        widths: [2, 2, 1, 1, 1, 1, 1, 1, 2, 1, 1, 1, 1, 1],
      },
      {
        // Wide glyph at the very end of a row (no trailing cells), both with and without explicit widths
        text: "docs/世界",
        widths: [1, 1, 1, 1, 1, 2, 2],
        omitWidths: true,
      },
    ]);
    expect(extractTerminalTokenAtCell(buffer, 1, 5)).toBeNull();
    expect(extractTerminalTokenAtCell(buffer, 1, 6)).toBe("docs/🚀.html");
    // Both left (11) and right (12) halves of `🚀` hit the token
    expect(extractTerminalTokenAtCell(buffer, 1, 11)).toBe("docs/🚀.html");
    expect(extractTerminalTokenAtCell(buffer, 1, 12)).toBe("docs/🚀.html");
    expect(extractTerminalTokenAtCell(buffer, 1, 17)).toBe("docs/🚀.html");
    // Col 18 is blank space right past `.html`
    expect(extractTerminalTokenAtCell(buffer, 1, 18)).toBeNull();

    // Row 2: `docs/` cols 1..5, `世` cols 6-7, `界` cols 8-9
    expect(extractTerminalTokenAtCell(buffer, 2, 8)).toBe("docs/世界");
    expect(extractTerminalTokenAtCell(buffer, 2, 9)).toBe("docs/世界");
    expect(extractTerminalTokenAtCell(buffer, 2, 10)).toBeNull();
  });

  it("resolves tokens across wrapped rows without falsely hitting blank space on either row", () => {
    const buffer = makeTestBuffer([
      { text: "open docs/sub/日本語", widths: [1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2] }, // cols 1..20 (`語` at 19-20)
      { text: "-report.html", isWrapped: true }, // cols 1..12
    ]);
    // Hit first row token including right half of trailing wide glyph `語` at col 20
    expect(extractTerminalTokenAtCell(buffer, 1, 6)).toBe("docs/sub/日本語-report.html");
    expect(extractTerminalTokenAtCell(buffer, 1, 19)).toBe("docs/sub/日本語-report.html");
    expect(extractTerminalTokenAtCell(buffer, 1, 20)).toBe("docs/sub/日本語-report.html");
    expect(extractTerminalTokenAtCell(buffer, 1, 21)).toBeNull();
    // Hit continuation row 2 (`-report.html` at cols 1..12)
    expect(extractTerminalTokenAtCell(buffer, 2, 1)).toBe("docs/sub/日本語-report.html");
    expect(extractTerminalTokenAtCell(buffer, 2, 12)).toBe("docs/sub/日本語-report.html");
    // Blank space past end of continuation row 2 MUST miss
    expect(extractTerminalTokenAtCell(buffer, 2, 13)).toBeNull();
    expect(extractTerminalTokenAtCell(buffer, 2, 50)).toBeNull();
  });

  it("does not inflate narrow cell widths across sparse column gaps when widths are omitted", () => {
    const sparse: LinkBuffer = {
      row(index: number) {
        if (index !== 1) return null;
        return {
          isWrapped: false,
          text: "aindex.html",
          columns: [1, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        };
      },
    };
    expect(extractTerminalTokenAtCell(sparse, 1, 1)).toBe("aindex.html");
    expect(extractTerminalTokenAtCell(sparse, 1, 2)).toBeNull();
    expect(extractTerminalTokenAtCell(sparse, 1, 5)).toBeNull();
    expect(extractTerminalTokenAtCell(sparse, 1, 6)).toBe("aindex.html");
  });
});

describe("isCellInTerminalRange", () => {
  it("matches cells strictly within single-row and wrapped multi-row link ranges", () => {
    const single = { start: { x: 4, y: 2 }, end: { x: 18, y: 2 } };
    expect(isCellInTerminalRange(single, 2, 4)).toBe(true);
    expect(isCellInTerminalRange(single, 2, 10)).toBe(true);
    expect(isCellInTerminalRange(single, 2, 18)).toBe(true);
    expect(isCellInTerminalRange(single, 2, 3)).toBe(false);
    expect(isCellInTerminalRange(single, 2, 19)).toBe(false);
    expect(isCellInTerminalRange(single, 3, 10)).toBe(false);

    const wrapped = { start: { x: 70, y: 5 }, end: { x: 12, y: 6 } };
    expect(isCellInTerminalRange(wrapped, 5, 69)).toBe(false);
    expect(isCellInTerminalRange(wrapped, 5, 70)).toBe(true);
    expect(isCellInTerminalRange(wrapped, 5, 80)).toBe(true);
    expect(isCellInTerminalRange(wrapped, 6, 1)).toBe(true);
    expect(isCellInTerminalRange(wrapped, 6, 12)).toBe(true);
    expect(isCellInTerminalRange(wrapped, 6, 13)).toBe(false);
    expect(isCellInTerminalRange(null, 1, 1)).toBe(false);
  });
});
