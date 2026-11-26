import { afterEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import {
  analyzeSvgDocument,
  clearRememberedSvgViewModes,
  clearSvgPreviewPreferences,
  clearSvgViewMode,
  clearSvgViewModeIfMatches,
  computeSvgDisplaySize,
  consumeSvgViewMode,
  encodeSvgDataUrl,
  extractSvgBlobText,
  inlineLocalSvgAssets,
  isSvgFile,
  loadSvgPreviewPreferences,
  matchSvgViewMode,
  MAX_SVG_INLINED_IMAGE_BYTES,
  MAX_SVG_INLINED_IMAGES,
  MAX_SVG_PREVIEW_BYTES,
  peekSvgViewModeMatch,
  prepareSvgPreviewDocument,
  recalledSvgViewMode,
  rememberSvgViewMode,
  requestSvgViewMode,
  resolveSvgPreviewLayout,
  resolveSvgPreviewShortcut,
  resolveSvgSplitDirection,
  resolveTerminalSvgCandidate,
  saveSvgPreviewPreferences,
  stepSvgZoom,
  SVG_MODE_TTL_MS,
  SVG_SPLIT_HORIZONTAL_MIN_WIDTH,
  SVG_TOOLBAR_INLINE_MIN_WIDTH,
  SVG_ZOOM_MAX,
  SVG_ZOOM_MIN,
  svgBlobToDataUrl,
  svgBlobToPreviewResult,
  svgViewModeRequest,
} from "./svgPreview";
import type { FileBlob } from "./types";

function decodeDataUrl(dataUrl: string): string {
  const prefix = "data:image/svg+xml;base64,";
  expect(dataUrl.startsWith(prefix)).toBe(true);
  const b64 = dataUrl.slice(prefix.length);
  const bin = atob(b64);
  const bytes = Uint8Array.from(bin, (ch) => ch.charCodeAt(0));
  return new TextDecoder("utf-8").decode(bytes);
}

afterEach(() => {
  clearSvgViewMode();
  clearRememberedSvgViewModes();
  clearSvgPreviewPreferences();
});

describe("isSvgFile", () => {
  it("recognizes .svg paths case-insensitively and ignores query/hash suffixes", () => {
    expect(isSvgFile("logo.svg")).toBe(true);
    expect(isSvgFile("assets/ICON.SVG")).toBe(true);
    expect(isSvgFile("docs/diagram.svg?v=2#layer")).toBe(true);
    expect(isSvgFile("win\\sub\\mark.svg")).toBe(true);
  });

  it("rejects empty, bare dotfiles, compressed .svgz, and non-svg paths", () => {
    expect(isSvgFile("")).toBe(false);
    expect(isSvgFile(null)).toBe(false);
    expect(isSvgFile(undefined)).toBe(false);
    expect(isSvgFile(".svg")).toBe(false);
    expect(isSvgFile("assets/.svg")).toBe(false);
    expect(isSvgFile("icon.svgz")).toBe(false);
    expect(isSvgFile("icon.svg.bak")).toBe(false);
    expect(isSvgFile("src/svg")).toBe(false);
    expect(isSvgFile("index.html")).toBe(false);
  });
});

describe("svgViewModeRequest one-shot store", () => {
  it("records, matches, and consumes mode requests once", () => {
    expect(requestSvgViewMode("/repo/", "./assets/logo.svg", "split", 1000)).toBe(true);
    const current = get(svgViewModeRequest);
    expect(current?.mode).toBe("split");
    expect(peekSvgViewModeMatch(current, "/repo", "assets/logo.svg", 1050)).toBe(true);
    expect(peekSvgViewModeMatch(current, "/other", "assets/logo.svg", 1050)).toBe(false);
    expect(peekSvgViewModeMatch(current, "/repo", "assets/other.svg", 1050)).toBe(false);

    expect(consumeSvgViewMode("/other", "assets/logo.svg", 1050)).toBeNull();
    expect(consumeSvgViewMode("/repo", "assets/logo.svg", 1050)).toBe("split");
    expect(consumeSvgViewMode("/repo", "assets/logo.svg", 1050)).toBeNull();
  });

  it("expires stale requests past TTL", () => {
    requestSvgViewMode("/repo", "logo.svg", "preview", 1000);
    expect(consumeSvgViewMode("/repo", "logo.svg", 1000 + SVG_MODE_TTL_MS + 1)).toBeNull();
    expect(get(svgViewModeRequest)).toBeNull();
  });

  it("rejects non-SVG paths and bumps nonce on repeat requests", () => {
    expect(requestSvgViewMode("/repo", "src/app.ts", "preview")).toBe(false);
    expect(requestSvgViewMode("/repo", "logo.svg", "preview", 100)).toBe(true);
    const first = get(svgViewModeRequest);
    expect(requestSvgViewMode("/repo", "logo.svg", "preview", 200)).toBe(true);
    const second = get(svgViewModeRequest);
    expect(second!.nonce).toBeGreaterThan(first!.nonce);
  });

  it("supports non-destructive multi-host matching via matchSvgViewMode", () => {
    expect(requestSvgViewMode("/repo", "assets/logo.svg", "split", 1000)).toBe(true);
    const req = get(svgViewModeRequest);
    const firstHost = matchSvgViewMode(req, "/repo", "assets/logo.svg", 1010);
    const secondHost = matchSvgViewMode(get(svgViewModeRequest), "/repo", "assets/logo.svg", 1020);
    expect(firstHost).not.toBeNull();
    expect(secondHost).toEqual(firstHost);

    clearSvgViewModeIfMatches("/repo", "assets/other.svg", 1025);
    expect(get(svgViewModeRequest)).not.toBeNull();
    clearSvgViewModeIfMatches("/repo", "assets/logo.svg", 1030);
    expect(get(svgViewModeRequest)).toBeNull();
  });

  it("remembers per-file modes in a bounded LRU cache", () => {
    rememberSvgViewMode("/repo/", "a.svg", "code");
    expect(recalledSvgViewMode("/repo", "./a.svg")).toBe("code");
    for (let i = 0; i < 70; i++) {
      rememberSvgViewMode("/repo", `i${i}.svg`, "split");
    }
    expect(recalledSvgViewMode("/repo", "a.svg")).toBeNull();
    expect(recalledSvgViewMode("/repo", "i69.svg")).toBe("split");
  });
});

describe("resolveSvgPreviewLayout, computeSvgDisplaySize & stepSvgZoom", () => {
  it("resolves toolbar and split breakpoints", () => {
    expect(resolveSvgPreviewLayout(0)).toEqual({
      mode: "unmeasured",
      splitDirection: "horizontal",
      toolbar: "inline",
    });
    expect(resolveSvgPreviewLayout(SVG_TOOLBAR_INLINE_MIN_WIDTH - 1)).toEqual({
      mode: "compact",
      splitDirection: "vertical",
      toolbar: "stacked",
    });
    expect(resolveSvgPreviewLayout(SVG_SPLIT_HORIZONTAL_MIN_WIDTH - 1)).toEqual({
      mode: "compact",
      splitDirection: "vertical",
      toolbar: "inline",
    });
    expect(resolveSvgPreviewLayout(SVG_SPLIT_HORIZONTAL_MIN_WIDTH)).toEqual({
      mode: "wide",
      splitDirection: "horizontal",
      toolbar: "inline",
    });
    expect(resolveSvgSplitDirection(SVG_SPLIT_HORIZONTAL_MIN_WIDTH - 1, "horizontal")).toEqual({
      direction: "vertical",
      canToggle: false,
    });
    expect(resolveSvgSplitDirection(SVG_SPLIT_HORIZONTAL_MIN_WIDTH, "vertical")).toEqual({
      direction: "vertical",
      canToggle: true,
    });
  });

  it("scales icons up cleanly in Fit mode (capped at 1600%), scales banners down, and handles 1:1 / manual zoom", () => {
    const iconFit = computeSvgDisplaySize({
      intrinsicWidth: 24,
      intrinsicHeight: 24,
      stageWidth: 800,
      stageHeight: 600,
      fit: true,
      zoom: 100,
    });
    expect(iconFit).toEqual({ width: 384, height: 384, effectivePercent: 1600 });

    const wideBannerFit = computeSvgDisplaySize({
      intrinsicWidth: 1200,
      intrinsicHeight: 300,
      stageWidth: 648,
      stageHeight: 448,
      fit: true,
      zoom: 100,
    });
    expect(wideBannerFit).toEqual({ width: 600, height: 150, effectivePercent: 50 });

    const unmeasuredFit = computeSvgDisplaySize({
      intrinsicWidth: 64,
      intrinsicHeight: 32,
      stageWidth: 0,
      stageHeight: 0,
      fit: true,
      zoom: 100,
    });
    expect(unmeasuredFit).toEqual({ width: 64, height: 32, effectivePercent: 100 });

    const manual = computeSvgDisplaySize({
      intrinsicWidth: 100,
      intrinsicHeight: 50,
      stageWidth: 800,
      stageHeight: 600,
      fit: false,
      zoom: 200,
    });
    expect(manual).toEqual({ width: 200, height: 100, effectivePercent: 200 });
  });

  it("steps zoom cleanly through SVG_ZOOM_STEPS within bounds", () => {
    expect(stepSvgZoom(100, 1)).toBe(125);
    expect(stepSvgZoom(100, -1)).toBe(75);
    expect(stepSvgZoom(SVG_ZOOM_MIN, -1)).toBe(SVG_ZOOM_MIN);
    expect(stepSvgZoom(SVG_ZOOM_MAX, 1)).toBe(SVG_ZOOM_MAX);
  });
});

describe("extractSvgBlobText & encodeSvgDataUrl", () => {
  it("extracts text blobs directly, decodes legacy base64 blobs, and flags NUL bytes or invalid base64", () => {
    expect(extractSvgBlobText({ text: "<svg/>", base64: null })).toEqual({
      text: "<svg/>",
      fromBase64: false,
      hasNul: false,
      invalidBase64: false,
    });
    const b64 = btoa("<svg><text>Hi</text></svg>");
    expect(extractSvgBlobText({ text: null, base64: b64 })).toEqual({
      text: "<svg><text>Hi</text></svg>",
      fromBase64: true,
      hasNul: false,
      invalidBase64: false,
    });
    expect(extractSvgBlobText({ text: "<svg>\0</svg>", base64: null }).hasNul).toBe(true);
    expect(extractSvgBlobText({ text: null, base64: btoa("abc\0def") }).hasNul).toBe(true);
    expect(extractSvgBlobText({ text: null, base64: "!!!not-valid-base64!!!" })).toEqual({
      text: "",
      fromBase64: true,
      hasNul: false,
      invalidBase64: true,
    });
  });

  it("round-trips non-ASCII UTF-8, #, and quotes via base64 data URI", () => {
    const xml = '<svg xmlns="http://www.w3.org/2000/svg"><text fill="#ff00aa">Café 🚀 &amp; "Quotes"</text></svg>';
    expect(decodeDataUrl(encodeSvgDataUrl(xml))).toBe(xml);
  });
});

describe("analyzeSvgDocument & prepareSvgPreviewDocument", () => {
  it("injects missing xmlns, xmlns:xlink, explicit dimensions from viewBox, and canvas currentColor", () => {
    const raw = `<svg viewBox="0 0 24 24"><path fill="currentColor" d="M2 2h20v20z"/><use xlink:href="#a"/></svg>`;
    const stats = analyzeSvgDocument(raw);
    expect(stats.hasXmlns).toBe(false);
    expect(stats.usesXlink).toBe(true);
    expect(stats.hasXlinkXmlns).toBe(false);
    expect(stats.usesCurrentColor).toBe(true);
    expect(stats.intrinsicWidth).toBe(24);
    expect(stats.intrinsicHeight).toBe(24);
    expect(stats.dimensionSource).toBe("viewBox");
    expect(stats.pathCount).toBe(1);

    const light = prepareSvgPreviewDocument(raw, { canvas: "light" });
    expect(light.parseError).toBeNull();
    const lightXml = decodeDataUrl(light.dataUrl);
    expect(lightXml).toContain('xmlns="http://www.w3.org/2000/svg"');
    expect(lightXml).toContain('xmlns:xlink="http://www.w3.org/1999/xlink"');
    expect(lightXml).toContain('width="24"');
    expect(lightXml).toContain('height="24"');
    expect(lightXml).toContain('color="#0f172a"');

    const dark = prepareSvgPreviewDocument(raw, { canvas: "dark" });
    expect(decodeDataUrl(dark.dataUrl)).toContain('color="#f1f5f9"');
  });

  it("replaces percentage dimensions using viewBox and defaults to 300x150 when dimensionless", () => {
    const pct = prepareSvgPreviewDocument('<svg width="100%" height="100%" viewBox="0 0 80 40"><rect/></svg>');
    expect(pct.stats.intrinsicWidth).toBe(80);
    expect(pct.stats.intrinsicHeight).toBe(40);
    expect(pct.stats.dimensionSource).toBe("viewBox");
    const pctXml = decodeDataUrl(pct.dataUrl);
    expect(pctXml).toContain('width="80"');
    expect(pctXml).toContain('height="40"');
    expect(pctXml).not.toContain('100%');

    const bare = prepareSvgPreviewDocument("<svg><circle r='5'/></svg>");
    expect(bare.stats.intrinsicWidth).toBe(300);
    expect(bare.stats.intrinsicHeight).toBe(150);
    expect(bare.stats.dimensionSource).toBe("default");
  });

  it("parses unit lengths (pt, cm, em) and derives missing axis from viewBox", () => {
    const withUnits = analyzeSvgDocument('<svg width="24pt" viewBox="0 0 100 50"><rect/></svg>');
    expect(withUnits.intrinsicWidth).toBe(32);
    expect(withUnits.intrinsicHeight).toBe(16);
    expect(withUnits.dimensionSource).toBe("attributes");

    const cmDoc = analyzeSvgDocument('<svg width="2.54cm" height="1in"><rect/></svg>');
    expect(cmDoc.intrinsicWidth).toBe(96);
    expect(cmDoc.intrinsicHeight).toBe(96);
  });

  it("preserves explicit author color attribute and detects currentColor inside <style>", () => {
    const explicit = prepareSvgPreviewDocument(
      '<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" color="#e11d48"><style>path{stroke:currentColor}</style><path d="M0 0"/></svg>',
      { canvas: "dark" },
    );
    expect(explicit.stats.usesCurrentColor).toBe(true);
    const xml = decodeDataUrl(explicit.dataUrl);
    expect(xml).toContain('color="#e11d48"');
    expect(xml).not.toContain("#f1f5f9");
  });

  it("strips <script>, on* handlers, javascript: hrefs, and DTD internal subsets while ignoring comments and CDATA", () => {
    const malicious = `<?xml version="1.0"?>
<!DOCTYPE svg [<!ENTITY xxe "bad">]>
<!-- <script>alert("comment")</script> -->
<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" onload="alert(1)">
  <title>  Safe &amp; Clean  </title>
  <script>alert(2)</script>
  <script type="text/ecmascript" />
  <a href="java&#x09;script:alert(3)" onclick="alert(4)">
    <rect width="10" height="10"/>
  </a>
  <text><![CDATA[<script>not executed</script>]]></text>
</svg>`;
    const prepared = prepareSvgPreviewDocument(malicious);
    expect(prepared.parseError).toBeNull();
    expect(prepared.stats.title).toBe("Safe & Clean");
    expect(prepared.stats.scriptCount).toBe(2);
    expect(prepared.stats.eventHandlerCount).toBe(2);
    expect(prepared.stats.unsafeHrefCount).toBe(1);
    expect(prepared.stats.strippedDtd).toBe(true);

    const out = decodeDataUrl(prepared.dataUrl);
    expect(out).not.toContain("<!ENTITY");
    expect(out).not.toContain("onload=");
    expect(out).not.toContain("onclick=");
    expect(out).not.toContain("alert(2)");
    expect(out).not.toContain("alert(3)");
    expect(out).toContain("<!-- <script>alert(\"comment\")</script> -->");
    expect(out).toContain("<![CDATA[<script>not executed</script>]]>");
  });

  it("reports empty, oversized, missing <svg>, unclosed <svg> tag, and unclosed </svg>", () => {
    expect(prepareSvgPreviewDocument("   \n ").empty).toBe(true);
    expect(prepareSvgPreviewDocument("x".repeat(MAX_SVG_PREVIEW_BYTES + 16)).tooLarge).toBe(true);
    expect(prepareSvgPreviewDocument("<div>not svg</div>").parseError).toContain("Missing root <svg>");
    expect(prepareSvgPreviewDocument("<svg width='10'").parseError).toContain("Unclosed <svg> start tag");
    expect(prepareSvgPreviewDocument("<svg><rect/>").parseError).toContain("Unclosed <svg> element");
  });

  it("converts FileBlobs through svgBlobToDataUrl", () => {
    const uri = svgBlobToDataUrl({ text: '<svg viewBox="0 0 12 12"><circle r="4"/></svg>' }, "dark");
    expect(uri).not.toBeNull();
    const xml = decodeDataUrl(uri!);
    expect(xml).toContain('width="12"');
    expect(xml).toContain('color="#f1f5f9"');
    expect(svgBlobToDataUrl({ text: "<svg>\0</svg>" })).toBeNull();
    expect(svgBlobToDataUrl({ text: "" })).toBeNull();
  });
});

describe("inlineLocalSvgAssets", () => {
  const makeBlob = (partial: Partial<FileBlob>): FileBlob => ({
    path: partial.path ?? "file",
    is_binary: partial.is_binary ?? false,
    is_image: partial.is_image ?? false,
    mime: partial.mime ?? "text/plain",
    text: partial.text ?? null,
    base64: partial.base64 ?? null,
    invalid_utf8_bytes: 0,
  });

  it("inlines local PNG and sanitized nested SVG images while blocking escapes, self-refs, and oversized assets", async () => {
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64">
      <image href="./chip.png" width="16" height="16"/>
      <feImage xlink:href="./badge.svg"/>
      <image href="./self.svg"/>
      <image href="./missing.png"/>
      <image href="./giant.png"/>
      <image href="../../secret.png"/>
      <image href="https://example.com/remote.png"/>
    </svg>`;

    const blobs: Record<string, FileBlob> = {
      "art/chip.png": makeBlob({ path: "art/chip.png", is_binary: true, is_image: true, mime: "image/png", base64: "iVBORw==" }),
      "art/badge.svg": makeBlob({
        path: "art/badge.svg",
        is_image: true,
        mime: "image/svg+xml",
        text: '<svg viewBox="0 0 8 8"><script>alert(1)</script><rect width="8" height="8"/></svg>',
      }),
      "art/self.svg": makeBlob({ path: "art/self.svg", is_image: true, mime: "image/svg+xml", text: svg }),
      "art/giant.png": makeBlob({
        path: "art/giant.png",
        is_binary: true,
        is_image: true,
        mime: "image/png",
        base64: "A".repeat(Math.ceil((MAX_SVG_INLINED_IMAGE_BYTES * 4) / 3) + 64),
      }),
    };

    const result = await inlineLocalSvgAssets(
      svg,
      { repoPath: "/repo", filePath: "art/self.svg" },
      async (_repo, path) => blobs[path] ?? null,
    );

    expect(result.inlinedImageCount).toBe(2);
    expect(result.images["./chip.png"]).toBe("data:image/png;base64,iVBORw==");
    expect(result.images["./badge.svg"]?.startsWith("data:image/svg+xml;base64,")).toBe(true);
    expect(decodeDataUrl(result.images["./badge.svg"]!)).not.toContain("alert(1)");
    expect(result.missingAssets).toContain("./self.svg");
    expect(result.missingAssets).toContain("./missing.png");
    expect(result.missingAssets).toContain("../../secret.png");
    expect(result.skippedCapCount).toBe(1);

    const prepared = prepareSvgPreviewDocument(svg, { inlinedImages: result.images });
    expect(prepared.inlinedImageCount).toBe(2);
    expect(prepared.stats.externalRefCount).toBe(1);
    const out = decodeDataUrl(prepared.dataUrl);
    expect(out).toContain('href="data:image/png;base64,iVBORw=="');
    expect(out).toContain('xlink:href="data:image/svg+xml;base64,');
  });

  it("caps total number of inlined local images", async () => {
    const imgs = Array.from({ length: MAX_SVG_INLINED_IMAGES + 3 }, (_, i) => `<image href="./i${i}.png"/>`).join("");
    const result = await inlineLocalSvgAssets(
      `<svg>${imgs}</svg>`,
      { repoPath: "/repo", filePath: "art/grid.svg" },
      async (_repo, path) => makeBlob({ path, is_binary: true, is_image: true, mime: "image/png", base64: "iVBORw==" }),
    );
    expect(result.inlinedImageCount).toBe(MAX_SVG_INLINED_IMAGES);
    expect(result.skippedCapCount).toBe(3);
  });

  it("forwards parent canvas surface into child local .svg image inlining", async () => {
    const parent = `<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><image href="./badge.svg" width="16" height="16"/></svg>`;
    const child = `<svg viewBox="0 0 16 16"><path fill="currentColor" d="M0 0h16v16z"/></svg>`;
    const read = async (_repo: string, path: string) =>
      path === "art/badge.svg"
        ? makeBlob({ path, is_image: true, mime: "image/svg+xml", text: child })
        : null;

    const darkRes = await inlineLocalSvgAssets(parent, { repoPath: "/repo", filePath: "art/p.svg" }, read, { canvas: "dark" });
    expect(decodeDataUrl(darkRes.images["./badge.svg"]!)).toContain('color="#f1f5f9"');

    const darkGridRes = await inlineLocalSvgAssets(parent, { repoPath: "/repo", filePath: "art/p.svg" }, read, { canvas: "dark-grid" });
    expect(decodeDataUrl(darkGridRes.images["./badge.svg"]!)).toContain('color="#f1f5f9"');

    const lightRes = await inlineLocalSvgAssets(parent, { repoPath: "/repo", filePath: "art/p.svg" }, read, { canvas: "light" });
    expect(decodeDataUrl(lightRes.images["./badge.svg"]!)).toContain('color="#0f172a"');
  });
});

describe("SVG viewBox synthesis, strict XML root/case handling, and extended sanitization", () => {
  it("synthesizes viewBox on non-viewBox SVGs (explicit width/height, dimensionless, and malformed viewBox)", () => {
    const explicit = prepareSvgPreviewDocument('<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"><rect width="24" height="24"/></svg>');
    expect(explicit.parseError).toBeNull();
    expect(explicit.stats.viewBox).toBeNull();
    expect(explicit.synthesizedViewBox).toBe(true);
    expect(explicit.addedXmlns).toBe(false);
    expect(decodeDataUrl(explicit.dataUrl)).toContain('viewBox="0 0 24 24"');

    const bare = prepareSvgPreviewDocument("<svg><circle cx='50' cy='50' r='40'/></svg>");
    expect(bare.synthesizedViewBox).toBe(true);
    expect(bare.addedXmlns).toBe(true);
    const bareXml = decodeDataUrl(bare.dataUrl);
    expect(bareXml).toContain('viewBox="0 0 300 150"');
    expect(bareXml).toContain('width="300"');
    expect(bareXml).toContain('height="150"');

    const invalidVb = prepareSvgPreviewDocument('<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20" viewBox="bad"><rect/></svg>');
    expect(invalidVb.synthesizedViewBox).toBe(true);
    const invalidVbXml = decodeDataUrl(invalidVb.dataUrl);
    expect(invalidVbXml).toContain('viewBox="0 0 40 20"');
    expect(invalidVbXml.match(/\bviewBox=/g)?.length).toBe(1);

    const existingVb = prepareSvgPreviewDocument('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><rect/></svg>');
    expect(existingVb.synthesizedViewBox).toBe(false);
    expect(existingVb.addedXmlns).toBe(false);
  });

  it("strips on* and unsafe href attributes on <style>, <title>, and <desc> tokens as well as regular tags", () => {
    const raw = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16">
      <title onclick="alert(1)" href="javascript:alert(2)">Icon</title>
      <desc onmouseover="alert(3)">Icon description text</desc>
      <style onload="alert(4)" xlink:href="data:text/html,<script>alert(5)</script>">rect{fill:currentColor}</style>
      <rect width="16" height="16"/>
    </svg>`;
    const prepared = prepareSvgPreviewDocument(raw);
    expect(prepared.parseError).toBeNull();
    expect(prepared.stats.title).toBe("Icon");
    expect(prepared.stats.description).toBe("Icon description text");
    expect(prepared.stats.eventHandlerCount).toBe(3);
    expect(prepared.stats.unsafeHrefCount).toBe(2);
    const out = decodeDataUrl(prepared.dataUrl);
    expect(out).not.toContain("onclick=");
    expect(out).not.toContain("onmouseover=");
    expect(out).not.toContain("onload=");
    expect(out).not.toContain("javascript:");
    expect(out).not.toContain("data:text/html");
    expect(out).toContain("<title>Icon</title>");
    expect(out).toContain("<desc>Icon description text</desc>");
  });

  it("strips <?xml-stylesheet?>, data:text/html / xhtml hrefs, and unsafe SMIL targets while counting file://, ftp://, and CSS external refs", () => {
    const raw = `<?xml version="1.0" encoding="UTF-8"?>
<?xml-stylesheet type="text/xsl" href="https://evil.example/sheet.xsl"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">
  <style><![CDATA[
    @import "https://fonts.example/a.css";
    @import url("file:///etc/hosts");
    /* url(https://commented.example/ignored.png) */
    .box { background: url(ftp://files.example/bg.png); content: "url(https://string.example/ignored.png)"; }
    .local { fill: url(#grad1); }
  ]]></style>
  <a href="data:text/html;base64,PHNjcmlwdD4="><rect style="background-image: url('https://cdn.example/tile.png')" width="10" height="10"/></a>
  <a xlink:href="data:application/xhtml+xml,<svg/>"><circle r="4"/></a>
  <image href="file:///etc/passwd" width="4" height="4"/>
  <image href="ftp://mirror.example/pic.png" width="4" height="4"/>
  <set attributeName="href" to="javascript:alert(1)"/>
  <animate attributeName="xlink:href" values="javascript:alert(2)"></animate>
  <set attributeName="onload" to="alert(3)"/>
  <animate attributeName="opacity" from="0" to="1" dur="1s"/>
</svg>`;
    const prepared = prepareSvgPreviewDocument(raw);
    expect(prepared.parseError).toBeNull();
    expect(prepared.stats.strippedStylesheetPiCount).toBe(1);
    expect(prepared.stats.unsafeHrefCount).toBe(2);
    expect(prepared.stats.unsafeSmilCount).toBe(3);
    expect(prepared.stats.animationCount).toBe(4);
    // 1 (xml-stylesheet https) + 3 (<style>: @import https, @import url file, url ftp) + 1 (inline style https) + 2 (<image> file, ftp) = 7
    expect(prepared.stats.externalRefCount).toBe(7);

    const out = decodeDataUrl(prepared.dataUrl);
    expect(out).toContain('<?xml version="1.0" encoding="UTF-8"?>');
    expect(out).not.toContain("xml-stylesheet");
    expect(out).not.toContain("data:text/html");
    expect(out).not.toContain("data:application/xhtml+xml");
    expect(out).not.toContain('attributeName="href"');
    expect(out).not.toContain('attributeName="xlink:href"');
    expect(out).not.toContain('attributeName="onload"');
    expect(out).not.toContain("</animate>");
    expect(out).toContain('attributeName="opacity"');
  });

  it("rejects non-root <svg> wrappers, uppercase <SVG>, and non-SVG root namespaces while preserving exact XML case on <SVG:svg>, ViewBox, and Color", () => {
    expect(prepareSvgPreviewDocument("<div><svg viewBox='0 0 10 10'><rect/></svg></div>").parseError).toContain("Missing root <svg> element.");
    expect(prepareSvgPreviewDocument("<SVG width='10' height='10'></SVG>").parseError).toContain("Missing root <svg> element.");
    expect(prepareSvgPreviewDocument('<svg xmlns="http://www.w3.org/1999/xhtml"><rect/></svg>').parseError).toContain("SVG namespace");

    const prefixed = prepareSvgPreviewDocument('<SVG:svg width="24" height="24"><SVG:rect width="24" height="24"/></SVG:svg>');
    expect(prefixed.parseError).toBeNull();
    expect(prefixed.addedXmlns).toBe(true);
    const prefixedXml = decodeDataUrl(prefixed.dataUrl);
    expect(prefixedXml).toContain('xmlns:SVG="http://www.w3.org/2000/svg"');
    expect(prefixedXml).toContain('viewBox="0 0 24 24"');

    const wrongCaseAttrs = prepareSvgPreviewDocument(
      '<svg xmlns="http://www.w3.org/2000/svg" ViewBox="0 0 24 24" Color="#e11d48"><path fill="currentColor" d="M0 0"/></svg>',
      { canvas: "dark-grid" },
    );
    expect(wrongCaseAttrs.stats.viewBox).toBeNull();
    expect(wrongCaseAttrs.synthesizedViewBox).toBe(true);
    const wrongCaseXml = decodeDataUrl(wrongCaseAttrs.dataUrl);
    expect(wrongCaseXml).toContain('viewBox="0 0 300 150"');
    expect(wrongCaseXml).toContain('color="#f1f5f9"');
  });

  it("surfaces explicit errors from svgBlobToPreviewResult instead of silently returning null error", () => {
    expect(svgBlobToPreviewResult({ text: "<svg>\0</svg>" })).toEqual({
      dataUrl: null,
      error: "SVG contains NUL bytes and cannot be rendered as XML.",
    });
    expect(svgBlobToPreviewResult({ text: "   " })).toEqual({
      dataUrl: null,
      error: "SVG document is empty.",
    });
    expect(svgBlobToPreviewResult({ text: "x".repeat(MAX_SVG_PREVIEW_BYTES + 10) }).error).toContain("1 MiB");
    expect(svgBlobToPreviewResult({ text: "<div><svg/></div>" }).error).toContain("Missing root <svg>");
    expect(svgBlobToPreviewResult({ text: null, base64: "!!!bad!!!" }).error).toContain("couldn't be decoded");
    expect(svgBlobToPreviewResult(null)).toEqual({ dataUrl: null, error: null });
  });

  it("handles adversarial CSS escapes/entities, custom XLink prefixes, multiline XML-STYLESHEET, nested foreignObject SVG, root xmlns:xlink, and huge Fit viewBoxes", () => {
    // 1. Huge artboard Fit mode must not clamp at 1% and overflow the stage
    const hugeFit = computeSvgDisplaySize({
      intrinsicWidth: 100_000,
      intrinsicHeight: 100_000,
      stageWidth: 500,
      stageHeight: 500,
      fit: true,
      zoom: 100,
      inset: 28,
    });
    expect(hugeFit.width).toBe(472);
    expect(hugeFit.height).toBe(472);
    expect(SVG_ZOOM_MAX).toBe(1600);
    expect(stepSvgZoom(1600, -1)).toBe(1200);
    expect(stepSvgZoom(1600, 1)).toBe(1600);

    // 2. Custom XLink prefix `xl:href`, entity-encoded SMIL attributeName, multiline uppercase XML-STYLESHEET,
    //    nested <svg> inside <foreignObject>, and CSS hex escapes / XML entities / comments in <style>
    const adversarial = `<?XML-STYLESHEET
      type="text/xsl"
      href="https://evil.example/multiline.xsl"?>
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xl="http://www.w3.org/1999/xlink" viewBox="0 0 32 32">
  <style>
    @\\69 mport "https://evil.example/esc-import.css";
    @import &quot;https://evil.example/entity-import.css&quot;;
    .a { fill: url(&#x68;ttps://evil.example/entity-url.svg#g); }
    .b { background: url("\\68 ttps://evil.example/hex-str.png"); }
    .c { background: \\75 rl(https://evil.example/hex-fn.png); }
    .d { background: url(/*comment*/"https://evil.example/commented.png"); }
    /* \\75 rl(https://evil.example/in-comment.png) */
  </style>
  <rect fill="\\75 rl(https://evil.example/pres-attr.svg#p)" width="8" height="8"/>
  <a xl:href="javascript:alert(1)"><rect width="4" height="4"/></a>
  <a href="data:text/ecmascript,alert(2)"><rect width="4" height="4"/></a>
  <image xl:href="https://evil.example/xl-img.png" width="4" height="4"/>
  <set attributeName="&#x68;ref" to="javascript:alert(3)"/>
  <foreignObject width="16" height="16">
    <div xmlns="http://www.w3.org/1999/xhtml" onclick="alert(4)">
      <svg xmlns="http://www.w3.org/2000/svg" onload="alert(5)">
        <rect onmouseover="alert(6)" width="4" height="4"/>
      </svg>
    </div>
  </foreignObject>
</svg>`;
    const prep = prepareSvgPreviewDocument(adversarial);
    expect(prep.parseError).toBeNull();
    expect(prep.stats.strippedStylesheetPiCount).toBe(1);
    expect(prep.stats.unsafeHrefCount).toBe(2);
    expect(prep.stats.unsafeSmilCount).toBe(1);
    expect(prep.stats.eventHandlerCount).toBe(3);
    // 1 (XML-STYLESHEET) + 6 (<style>) + 1 (fill presentation attr) + 1 (xl:href image) = 9
    expect(prep.stats.externalRefCount).toBe(9);
    const out = decodeDataUrl(prep.dataUrl);
    expect(out).not.toContain("XML-STYLESHEET");
    expect(out).not.toContain("javascript:");
    expect(out).not.toContain("data:text/ecmascript");
    expect(out).not.toContain("onclick=");
    expect(out).not.toContain("onload=");
    expect(out).not.toContain("onmouseover=");
    expect(out).not.toContain("<set");

    // 3. Descendant xmlns:xlink must not suppress root xmlns:xlink injection for sibling xlink:href
    const descendantXlink = prepareSvgPreviewDocument(
      '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><use xlink:href="#a"/><g xmlns:xlink="http://www.w3.org/1999/xlink"/></svg>',
    );
    expect(descendantXlink.parseError).toBeNull();
    expect(descendantXlink.stats.hasXlinkXmlns).toBe(false);
    expect(decodeDataUrl(descendantXlink.dataUrl)).toMatch(/<svg[^>]*xmlns:xlink="http:\/\/www\.w3\.org\/1999\/xlink"/);

    // 4. Whitespace-padded root xmlns is invalid in XML namespaces and must be rejected
    const paddedNs = prepareSvgPreviewDocument(
      '<svg xmlns=" http://www.w3.org/2000/svg " viewBox="0 0 16 16"><rect/></svg>',
    );
    expect(paddedNs.parseError).toContain("SVG namespace");
  });

  it("supports custom Fit inset with zero-overflow floor rounding, keyboard shortcut mapping, preferences, and terminal SVG candidate resolution", () => {
    expect(
      computeSvgDisplaySize({
        intrinsicWidth: 1000,
        intrinsicHeight: 500,
        stageWidth: 428,
        stageHeight: 228,
        fit: true,
        zoom: 100,
        inset: 28,
      }),
    ).toEqual({ width: 400, height: 200, effectivePercent: 40 });

    expect(resolveSvgPreviewShortcut({ key: "+", ctrlKey: false, metaKey: false, altKey: false })).toBe("zoom-in");
    expect(resolveSvgPreviewShortcut({ key: "=", ctrlKey: false, metaKey: false, altKey: false })).toBe("zoom-in");
    expect(resolveSvgPreviewShortcut({ key: "-", ctrlKey: false, metaKey: false, altKey: false })).toBe("zoom-out");
    expect(resolveSvgPreviewShortcut({ key: "0", ctrlKey: false, metaKey: false, altKey: false })).toBe("fit");
    expect(resolveSvgPreviewShortcut({ key: "1", ctrlKey: false, metaKey: false, altKey: false })).toBe("actual");
    expect(resolveSvgPreviewShortcut({ key: "b", ctrlKey: false, metaKey: false, altKey: false })).toBe("bounds");
    expect(resolveSvgPreviewShortcut({ key: "b", ctrlKey: true, metaKey: false, altKey: false })).toBeNull();
    expect(resolveSvgPreviewShortcut({ key: "0", ctrlKey: false, metaKey: true, altKey: false })).toBeNull();

    // Terminal SVG candidate resolution parity with HTML
    expect(resolveTerminalSvgCandidate("public/gitpulse-mark.svg", "/repo")).toEqual({
      path: "public/gitpulse-mark.svg",
      line: null,
      column: null,
      fallbackPath: undefined,
    });
    expect(resolveTerminalSvgCandidate("./icons/logo.svg:12:4", "/repo")).toEqual({
      path: "icons/logo.svg",
      line: 12,
      column: 4,
      fallbackPath: undefined,
    });
    expect(resolveTerminalSvgCandidate('"mark.svg"', "/repo", "packages/ui")).toEqual({
      path: "packages/ui/mark.svg",
      line: null,
      column: null,
      fallbackPath: "mark.svg",
    });
    expect(resolveTerminalSvgCandidate("../escape.svg", "/repo")).toBeNull();
    expect(resolveTerminalSvgCandidate("https://example.com/a.svg", "/repo")).toBeNull();
    expect(resolveTerminalSvgCandidate("index.html", "/repo")).toBeNull();

    // Preference helpers degrade cleanly without localStorage in Node and persist when storage exists
    expect(loadSvgPreviewPreferences()).toEqual({
      canvasSurface: "grid",
      showBounds: false,
      preferredSplitDirection: "horizontal",
    });
    const mem = new Map<string, string>();
    const origStorage = globalThis.localStorage;
    Object.defineProperty(globalThis, "localStorage", {
      value: {
        getItem: (k: string) => mem.get(k) ?? null,
        setItem: (k: string, v: string) => void mem.set(k, v),
        removeItem: (k: string) => void mem.delete(k),
      },
      configurable: true,
    });
    try {
      saveSvgPreviewPreferences({ canvasSurface: "dark-grid", showBounds: true, preferredSplitDirection: "vertical" });
      expect(loadSvgPreviewPreferences()).toEqual({
        canvasSurface: "dark-grid",
        showBounds: true,
        preferredSplitDirection: "vertical",
      });
      mem.set("gitpulse_svg_preview_prefs", "{bad json");
      expect(loadSvgPreviewPreferences().canvasSurface).toBe("grid");
    } finally {
      Object.defineProperty(globalThis, "localStorage", { value: origStorage, configurable: true });
    }
  });
});
