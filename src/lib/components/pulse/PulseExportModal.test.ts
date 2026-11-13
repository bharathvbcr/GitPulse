import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("./PulseExportModal.svelte", import.meta.url), "utf8");
const dialog = readFileSync(new URL("../Dialog.svelte", import.meta.url), "utf8");

describe("PulseExportModal component", () => {
  it("delegates card generation to the tested exporter", () => {
    expect(source).toContain("generatePulseSvgCard");
  });

  it("does not generate a card while the modal is closed", () => {
    expect(source).toMatch(/if \(open\) lastCard = generatePulseSvgCard\(options\)/);
  });

  it("is the shared Dialog, so it ships with an outro, a labelled dialog role and a close button", () => {
    // The scrim, focus trap, Escape, scrim click, stacking and the in/out
    // transitions used to be hand-rolled here; Dialog.svelte owns them now.
    expect(source).toContain('import Dialog from "../Dialog.svelte"');
    expect(source).toContain("<Dialog");
    expect(source).toContain('title="Export Pulse summary card"');
    expect(source).not.toContain("gp-scrim");
    expect(source).not.toContain("use:trapFocus");
    expect(dialog).toContain('role="dialog"');
    expect(dialog).toContain('aria-modal="true"');
    expect(dialog).toContain("LAYERS.MODAL");
    expect(dialog).toContain("out:fade");
    expect(dialog).toContain("out:scale");
  });

  it("closes through the Dialog and tells the parent", () => {
    expect(source).toContain("onclose={closeModal}");
    expect(source).toMatch(/function closeModal\(\) \{[\s\S]*onClose\(\)/);
  });

  it("uses the resilient clipboard seam and exposes denied copy attempts", () => {
    expect(source).toContain('from "../../desktop/clipboard"');
    expect(source).toContain("await copyText(svgContent)");
    expect(source).not.toContain("navigator.clipboard");
    expect(source).toContain("Couldn't copy the SVG");
    expect(source).toContain('role="status"');
  });

  it("cleans up transient copy feedback timers when it closes or unmounts", () => {
    expect(source).toContain("copyTimer");
    expect(source).toContain("clearTimeout(copyTimer)");
  });

  it("saves through the native save seam and shows what happened", () => {
    expect(source).toContain('from "../../desktop/saveFile"');
    expect(source).not.toContain("createObjectURL");
    // A failed save is an alert in the footer; a save or a requested download is a status.
    expect(source).toContain('role={saveNote.tone === "danger" ? "alert" : "status"}');
    expect(source).toMatch(/saveNote = \{ tone: "danger", text: copy\.detail/);
    // Closing or reopening the dialog clears the last result.
    expect(source).toMatch(/function resetCopyState\(\) \{[\s\S]*saveNote = null;[\s\S]*saveRun \+= 1;/);
    // A save that lands after a close or a newer save is dropped, on success and on failure.
    expect(source).toContain("const run = ++saveRun;");
    expect(source.match(/if \(run !== saveRun\) return;/g)).toHaveLength(2);
  });

  it("wears the shared button grammar", () => {
    expect(source).toContain('class="gp-btn" data-size="md"');
    expect(source).toContain('data-variant="primary"');
  });
});
