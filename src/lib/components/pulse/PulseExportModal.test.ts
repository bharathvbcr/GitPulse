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
});
