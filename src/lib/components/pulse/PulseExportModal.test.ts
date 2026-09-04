import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("./PulseExportModal.svelte", import.meta.url), "utf8");

describe("PulseExportModal component", () => {
  it("delegates card generation to the tested exporter", () => {
    expect(source).toContain("generatePulseSvgCard");
  });

  it("does not generate a card while the modal is closed", () => {
    expect(source).toMatch(/open \? generatePulseSvgCard\(options\) : ""/);
  });

  it("is a labelled modal dialog for assistive tech", () => {
    expect(source).toContain('role="dialog"');
    expect(source).toContain('aria-modal="true"');
    expect(source).toContain('aria-labelledby="export-modal-title"');
    expect(source).toContain('aria-label="Close modal"');
  });

  it("uses the shared modal focus, layering, dismissal, and viewport contracts", () => {
    expect(source).toContain("use:trapFocus");
    expect(source).toContain("LAYERS.MODAL");
    expect(source).toContain('e.key === "Escape"');
    expect(source).toContain("e.target === e.currentTarget");
    expect(source).toContain("max-h-[calc(100vh-2rem)]");
    expect(source).toContain("min-h-0 flex-1 overflow-y-auto");
  });

  it("uses the resilient clipboard seam and exposes denied copy attempts", () => {
    expect(source).toContain('from "../../desktop/clipboard"');
    expect(source).toContain("await copyText(svgContent)");
    expect(source).not.toContain("navigator.clipboard");
    expect(source).toContain("Copy failed");
    expect(source).toContain('role="status"');
  });

  it("cleans up transient copy feedback timers when it closes or unmounts", () => {
    expect(source).toContain("copyTimer");
    expect(source).toContain("clearTimeout(copyTimer)");
  });
});
