import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";
import { render } from "svelte/server";
import HtmlPreviewViewer from "./HtmlPreviewViewer.svelte";
import {
  clearHtmlViewMode,
  clearRememberedHtmlViewModes,
  MAX_HTML_PREVIEW_BYTES,
  requestHtmlViewMode,
} from "../../files/htmlPreview";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "HtmlPreviewViewer.svelte"), "utf8");

afterEach(() => {
  clearHtmlViewMode();
  clearRememberedHtmlViewModes();
});

describe("HtmlPreviewViewer contracts", () => {
  it("compiles with zero accessibility warnings", () => {
    const { warnings } = compile(source, { generate: "client" });
    expect(warnings.filter(({ code }) => code.startsWith("a11y_"))).toEqual([]);
  });

  it("sandboxes rendered HTML strictly inside an opaque srcdoc iframe without scripts or same-origin", () => {
    expect(source).toContain('sandbox="allow-forms"');
    expect(source).not.toContain("allow-same-origin");
    expect(source).not.toContain("allow-scripts");
    expect(source).not.toContain("allow-top-navigation");
    expect(source).toContain('referrerpolicy="no-referrer"');
    expect(source).not.toContain("{@html");
  });

  it("opens externally via the validated shell gate, never the opener plugin", () => {
    expect(source).toContain("openInDefaultApp(repo, filePath)");
    expect(source).not.toContain("@tauri-apps/plugin-opener");
  });

  it("renders preview mode by default with sandboxed iframe and toolbar controls", () => {
    const { body } = render(HtmlPreviewViewer, {
      props: {
        filePath: "docs/demo.html",
        repoPathOverride: "/repo",
        blob: {
          path: "docs/demo.html",
          is_binary: false,
          is_image: false,
          mime: "text/html",
          text: "<!DOCTYPE html><html><head><title>Live Card</title><script>alert(1)</script></head><body><h1>Hello HTML</h1></body></html>",
        },
      },
    });
    expect(body).toContain('data-testid="html-preview-viewer"');
    expect(body).toContain('data-testid="html-preview-iframe"');
    expect(body).toContain('sandbox="allow-forms"');
    expect(body).toContain('data-testid="html-mode-preview"');
    expect(body).toContain('data-testid="html-mode-split"');
    expect(body).toContain('data-testid="html-mode-code"');
    expect(body).toContain('data-testid="html-scripts-sandboxed-badge"');
    expect(body).toContain("Live Card");
    expect(body).toContain("Hello HTML");
    expect(body).not.toContain("alert(1)");
  });

  it("honors a one-shot split mode request (right-click Open Code & Preview)", () => {
    requestHtmlViewMode("/repo", "docs/split.html", "split");
    const { body } = render(HtmlPreviewViewer, {
      props: {
        filePath: "docs/split.html",
        repoPathOverride: "/repo",
        blob: {
          path: "docs/split.html",
          is_binary: false,
          is_image: false,
          mime: "text/html",
          text: "<h1>Split View</h1>",
        },
      },
    });
    expect(body).toContain("data-html-split-code");
    expect(body).toContain("data-html-split-preview");
    expect(body).toContain('data-testid="html-split-sash"');
    expect(body).toContain('data-testid="html-preview-iframe"');
  });

  it("honors a one-shot code mode request (right-click Open in editor)", () => {
    requestHtmlViewMode("/repo", "docs/code.html", "code");
    const { body } = render(HtmlPreviewViewer, {
      props: {
        filePath: "docs/code.html",
        repoPathOverride: "/repo",
        blob: {
          path: "docs/code.html",
          is_binary: false,
          is_image: false,
          mime: "text/html",
          text: "<p>Source only</p>",
        },
      },
    });
    expect(body).not.toContain('data-testid="html-preview-iframe"');
    expect(body).toContain('data-active="true" data-testid="html-mode-code"');
  });

  it("shows an honest size-limit notice for oversized HTML files instead of rendering an empty broken frame", () => {
    const { body } = render(HtmlPreviewViewer, {
      props: {
        filePath: "dist/bundle.html",
        repoPathOverride: "/repo",
        blob: {
          path: "dist/bundle.html",
          is_binary: false,
          is_image: false,
          mime: "text/html",
          text: "x".repeat(MAX_HTML_PREVIEW_BYTES + 64),
        },
      },
    });
    expect(body).toContain("too large for inline live preview");
    expect(body).not.toContain('data-testid="html-preview-iframe"');
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

  it("delivers one right-click mode request to concurrently mounted FileViewer and TerminalFilePane hosts without stealing", () => {
    requestHtmlViewMode("/repo", "docs/shared.html", "split");
    const props = {
      filePath: "docs/shared.html",
      repoPathOverride: "/repo",
      blob: {
        path: "docs/shared.html",
        is_binary: false,
        is_image: false,
        mime: "text/html",
        text: "<h1>Shared</h1>",
      },
    };
    const first = render(HtmlPreviewViewer, { props });
    const second = render(HtmlPreviewViewer, { props });
    expect(first.body).toContain("data-html-split-code");
    expect(second.body).toContain("data-html-split-code");
  });

  it("keeps explicit preview request active even when a stale revealTarget is still present on the pane", () => {
    requestHtmlViewMode("/repo", "docs/preview.html", "preview");
    const { body } = render(HtmlPreviewViewer, {
      props: {
        filePath: "docs/preview.html",
        repoPathOverride: "/repo",
        revealTarget: { line: 12, column: null, seq: 1 },
        blob: {
          path: "docs/preview.html",
          is_binary: false,
          is_image: false,
          mime: "text/html",
          text: "<h1>Preview Wins</h1>",
        },
      },
    });
    expect(body).toContain('data-testid="html-preview-iframe"');
    expect(body).toContain('data-active="true" data-testid="html-mode-preview"');
  });

  it("uses mx-auto framing without overflow-clipping items-center, skips digests on asset reads, and supports vertical split sashes", () => {
    expect(source).not.toMatch(/overflow-auto[^"]*items-center/);
    expect(source).toContain("withDigest: false");
    expect(source).toContain('aria-orientation="horizontal"');
    expect(source).toContain('aria-orientation="vertical"');
    expect(source).toContain("use:fileSearchControl={previewSearchController}");
    expect(source).toContain("const iframeKey = $derived(`${repoPath ?? \"\"}\\0${filePath}\\0${refreshNonce}`);");
    expect(source).toContain("{#key iframeKey}");
    expect(source).toContain("REVEAL_TTL_MS");
    expect(source).toContain('data-testid="html-split-orientation-toggle"');
  });

  it("renders the split orientation toggle button in split mode when pane width permits", () => {
    requestHtmlViewMode("/repo", "docs/split-toggle.html", "split");
    const { body } = render(HtmlPreviewViewer, {
      props: {
        filePath: "docs/split-toggle.html",
        repoPathOverride: "/repo",
        blob: {
          path: "docs/split-toggle.html",
          is_binary: false,
          is_image: false,
          mime: "text/html",
          text: "<h1>Split Layout Toggle</h1>",
        },
      },
    });
    expect(body).toContain('data-testid="html-split-orientation-toggle"');
    expect(body).toContain('data-html-split="horizontal"');
  });
});
