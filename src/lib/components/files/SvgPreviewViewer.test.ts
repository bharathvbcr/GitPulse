import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";
import { render } from "svelte/server";
import SvgPreviewViewer from "./SvgPreviewViewer.svelte";
import {
  clearRememberedSvgViewModes,
  clearSvgPreviewPreferences,
  clearSvgViewMode,
  MAX_SVG_PREVIEW_BYTES,
  requestSvgViewMode,
} from "../../files/svgPreview";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "SvgPreviewViewer.svelte"), "utf8");
const mediaSource = readFileSync(join(here, "MediaViewer.svelte"), "utf8");

afterEach(() => {
  clearSvgViewMode();
  clearRememberedSvgViewModes();
  clearSvgPreviewPreferences();
});

describe("SvgPreviewViewer contracts", () => {
  it("compiles with zero accessibility warnings", () => {
    const { warnings } = compile(source, { generate: "client" });
    expect(warnings.filter(({ code }) => code.startsWith("a11y_"))).toEqual([]);
  });

  it("renders untrusted SVGs strictly via base64 <img> without {@html}, inline untrusted <svg>, or overflow-clipping items-center", () => {
    expect(source).toContain('data-testid="svg-preview-img"');
    expect(source).not.toContain("{@html");
    expect(source).not.toMatch(/overflow-auto[^"]*items-center/);
    expect(source).toContain("withDigest: false");
    expect(source).toContain('aria-orientation="horizontal"');
    expect(source).toContain('aria-orientation="vertical"');
    expect(source).toContain("use:fileSearchControl={previewSearchController}");
    expect(source).toContain("REVEAL_TTL_MS");
  });

  it("opens externally via the validated shell gate, never the opener plugin", () => {
    expect(source).toContain("openInDefaultApp(repo, filePath)");
    expect(source).not.toContain("@tauri-apps/plugin-opener");
  });

  it("renders preview mode by default with normalized base64 <img> and toolbar controls", () => {
    const { body } = render(SvgPreviewViewer, {
      props: {
        filePath: "assets/mark.svg",
        repoPathOverride: "/repo",
        blob: {
          path: "assets/mark.svg",
          is_binary: false,
          is_image: true,
          mime: "image/svg+xml",
          text: '<svg viewBox="0 0 24 24"><title>Brand Mark</title><script>alert(1)</script><path d="M2 2h20v20z" onclick="alert(2)"/></svg>',
        },
      },
    });
    expect(body).toContain('data-testid="svg-preview-viewer"');
    expect(body).toContain('data-testid="svg-preview-img"');
    expect(body).toContain("data:image/svg+xml;base64,");
    expect(body).toContain('data-testid="svg-mode-preview"');
    expect(body).toContain('data-testid="svg-mode-split"');
    expect(body).toContain('data-testid="svg-mode-code"');
    expect(body).toContain('data-testid="svg-sanitized-badge"');
    expect(body).toContain("Brand Mark");
    expect(body).toContain("24 × 24 px");
    expect(body).not.toContain("alert(1)");
  });

  it("honors a one-shot split mode request (right-click Open Code & Preview)", () => {
    requestSvgViewMode("/repo", "assets/split.svg", "split");
    const { body } = render(SvgPreviewViewer, {
      props: {
        filePath: "assets/split.svg",
        repoPathOverride: "/repo",
        blob: {
          path: "assets/split.svg",
          is_binary: false,
          is_image: true,
          mime: "image/svg+xml",
          text: '<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><circle cx="16" cy="16" r="10"/></svg>',
        },
      },
    });
    expect(body).toContain("data-svg-split-code");
    expect(body).toContain("data-svg-split-preview");
    expect(body).toContain('data-testid="svg-split-sash"');
    expect(body).toContain('data-testid="svg-preview-img"');
    expect(body).toContain('data-testid="svg-split-orientation-toggle"');
  });

  it("honors a one-shot code mode request (right-click Open in editor)", () => {
    requestSvgViewMode("/repo", "assets/code.svg", "code");
    const { body } = render(SvgPreviewViewer, {
      props: {
        filePath: "assets/code.svg",
        repoPathOverride: "/repo",
        blob: {
          path: "assets/code.svg",
          is_binary: false,
          is_image: true,
          mime: "image/svg+xml",
          text: '<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><rect width="16" height="16"/></svg>',
        },
      },
    });
    expect(body).not.toContain('data-testid="svg-preview-img"');
    expect(body).toContain('data-active="true" data-testid="svg-mode-code"');
  });

  it("shows honest oversized, empty, and missing-<svg> error notices", () => {
    const oversized = render(SvgPreviewViewer, {
      props: {
        filePath: "dist/huge.svg",
        repoPathOverride: "/repo",
        blob: {
          path: "dist/huge.svg",
          is_binary: false,
          is_image: true,
          mime: "image/svg+xml",
          text: "x".repeat(MAX_SVG_PREVIEW_BYTES + 64),
        },
      },
    });
    expect(oversized.body).toContain("too large for inline live preview");
    expect(oversized.body).toContain('data-testid="svg-too-large"');

    const empty = render(SvgPreviewViewer, {
      props: {
        filePath: "art/empty.svg",
        repoPathOverride: "/repo",
        blob: {
          path: "art/empty.svg",
          is_binary: false,
          is_image: true,
          mime: "image/svg+xml",
          text: "   \n",
        },
      },
    });
    expect(empty.body).toContain('data-testid="svg-empty-state"');

    const malformed = render(SvgPreviewViewer, {
      props: {
        filePath: "art/bad.svg",
        repoPathOverride: "/repo",
        blob: {
          path: "art/bad.svg",
          is_binary: false,
          is_image: true,
          mime: "image/svg+xml",
          text: "<div>not an svg</div>",
        },
      },
    });
    expect(malformed.body).toContain('data-testid="svg-parse-error"');
    expect(malformed.body).toContain("Missing root &lt;svg> element.");
  });

  it("delivers one right-click mode request to concurrently mounted hosts without stealing and preserves explicit preview over stale revealTarget", () => {
    requestSvgViewMode("/repo", "art/shared.svg", "split");
    const props = {
      filePath: "art/shared.svg",
      repoPathOverride: "/repo",
      blob: {
        path: "art/shared.svg",
        is_binary: false,
        is_image: true,
        mime: "image/svg+xml",
        text: '<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><rect width="16" height="16"/></svg>',
      },
    };
    const first = render(SvgPreviewViewer, { props });
    const second = render(SvgPreviewViewer, { props });
    expect(first.body).toContain("data-svg-split-code");
    expect(second.body).toContain("data-svg-split-code");

    requestSvgViewMode("/repo", "art/preview.svg", "preview");
    const explicit = render(SvgPreviewViewer, {
      props: {
        filePath: "art/preview.svg",
        repoPathOverride: "/repo",
        revealTarget: { line: 4, column: null, seq: 1 },
        blob: {
          path: "art/preview.svg",
          is_binary: false,
          is_image: true,
          mime: "image/svg+xml",
          text: '<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><circle r="6"/></svg>',
        },
      },
    });
    expect(explicit.body).toContain('data-testid="svg-preview-img"');
    expect(explicit.body).toContain('data-active="true" data-testid="svg-mode-preview"');
  });

  it("forwards CodeViewer draft lifecycle and reveal props in both code and split modes", () => {
    expect(source).toContain("<CodeViewer");
    expect(source.match(/\{revealTarget\}/g)?.length).toBe(2);
    expect(source.match(/\{onSendSelectionToAgent\}/g)?.length).toBe(2);
    expect(source.match(/\{draftContent\}/g)?.length).toBe(2);
    expect(source.match(/\{onSave\}/g)?.length).toBe(2);
    expect(source.match(/\{onDraftChange\}/g)?.length).toBe(2);
    expect(source.match(/\{onRequestDiscard\}/g)?.length).toBe(2);
  });

  it("renders legacy base64-backed SVGs in read-only mode and reports NUL bytes cleanly", () => {
    const validB64 = btoa('<svg width="24" height="24"><desc>Legacy base64 icon</desc><g><rect width="24" height="24"/></g></svg>');
    const fromB64 = render(SvgPreviewViewer, {
      props: {
        filePath: "legacy/icon.svg",
        repoPathOverride: "/repo",
        blob: {
          path: "legacy/icon.svg",
          is_binary: true,
          is_image: true,
          mime: "image/svg+xml",
          text: null,
          base64: validB64,
        },
      },
    });
    expect(fromB64.body).toContain('data-testid="svg-preview-img"');
    expect(fromB64.body).toContain("24 × 24 px");
    expect(source.match(/readOnly=\{extracted\.fromBase64\}/g)?.length).toBe(2);

    // Code mode on a text-backed SVG offers Edit, while a base64-backed SVG stays read-only (no Edit button)
    requestSvgViewMode("/repo", "legacy/editable.svg", "code");
    const codeText = render(SvgPreviewViewer, {
      props: {
        filePath: "legacy/editable.svg",
        repoPathOverride: "/repo",
        blob: {
          path: "legacy/editable.svg",
          is_binary: false,
          is_image: true,
          mime: "image/svg+xml",
          text: '<svg width="24" height="24"><rect width="24" height="24"/></svg>',
          base64: null,
        },
      },
    });
    expect(codeText.body).toContain("<span>Edit</span>");

    requestSvgViewMode("/repo", "legacy/icon.svg", "code");
    const codeB64 = render(SvgPreviewViewer, {
      props: {
        filePath: "legacy/icon.svg",
        repoPathOverride: "/repo",
        blob: {
          path: "legacy/icon.svg",
          is_binary: true,
          is_image: true,
          mime: "image/svg+xml",
          text: null,
          base64: validB64,
        },
      },
    });
    expect(codeB64.body).not.toContain("<span>Edit</span>");
    expect(codeB64.body).toContain("Legacy");

    // NUL bytes in either text or base64 render the inline parse-error surface
    const nulText = render(SvgPreviewViewer, {
      props: {
        filePath: "bad/nul.svg",
        repoPathOverride: "/repo",
        blob: {
          path: "bad/nul.svg",
          is_binary: false,
          is_image: true,
          mime: "image/svg+xml",
          text: "<svg>\0</svg>",
        },
      },
    });
    expect(nulText.body).toContain('data-testid="svg-parse-error"');
    expect(nulText.body).toContain("NUL bytes");
    expect(nulText.body).not.toContain('data-testid="svg-preview-img"');

    const nulB64 = render(SvgPreviewViewer, {
      props: {
        filePath: "bad/nul-b64.svg",
        repoPathOverride: "/repo",
        blob: {
          path: "bad/nul-b64.svg",
          is_binary: true,
          is_image: true,
          mime: "image/svg+xml",
          text: null,
          base64: btoa("<svg>\0</svg>"),
        },
      },
    });
    expect(nulB64.body).toContain('data-testid="svg-parse-error"');
    expect(nulB64.body).toContain("NUL bytes");

    const invalidB64 = render(SvgPreviewViewer, {
      props: {
        filePath: "bad/corrupt-b64.svg",
        repoPathOverride: "/repo",
        blob: {
          path: "bad/corrupt-b64.svg",
          is_binary: true,
          is_image: true,
          mime: "image/svg+xml",
          text: null,
          base64: "!!!not-valid-base64!!!",
        },
      },
    });
    expect(invalidB64.body).toContain('data-testid="svg-parse-error"');
    expect(invalidB64.body).toContain("couldn't be decoded");
    expect(invalidB64.body).not.toContain('data-testid="svg-empty-state"');
  });

  it("wires dark transparency grid, Copy Data URI / PNG export, direct stage manipulation, and raster MediaViewer overflow safety", () => {
    expect(source).toContain('data-testid="svg-canvas-dark-grid"');
    expect(source).toContain(".svg-grid-dark-surface");
    expect(source).toContain("color-mix(in srgb, CanvasText");
    expect(source).toContain('data-testid="svg-copy-data-uri"');
    expect(source).toContain('data-testid="svg-copy-png"');
    expect(source).toContain("copyImageBlob(");
    expect(source).toContain("resolveSvgPreviewShortcut(");
    expect(source).toContain("loadSvgPreviewPreferences()");
    expect(source).toContain("saveSvgPreviewPreferences(");
    expect(source).toContain("prepared.stats.groupCount");
    expect(source).toContain("prepared.stats.imageCount");
    expect(source).toContain("prepared.stats.animationCount");
    expect(source).toContain("prepared.stats.description");
    expect(source).toContain("prepared.synthesizedViewBox");
    expect(source).toContain("prepared.addedXmlns");

    // Raster MediaViewer stage must use m-auto (never flex items-center justify-center on overflow-auto)
    // and remove Preflight max-width clamping when zoomed past fit.
    expect(mediaSource).toContain("data-raster-stage");
    expect(mediaSource).toContain("'max-w-none'");
    expect(mediaSource).not.toMatch(/flex\s+items-center\s+justify-center[^"]*overflow-auto/);
  });
});
