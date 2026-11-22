import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const read = (relative: string) => readFileSync(join(here, relative), "utf8");

// The Svelte components are not mounted by vitest and their `$effect`s do not
// run, so what only a source read can pin is pinned here; the behaviour itself
// is driven in the browser harnesses (`harness/file-viewer.html`, `harness/terminal.html`).
describe("sending code to an agent: wiring that has no other test", () => {
  const panel = read("TerminalPanel.svelte");
  const code = read("files/CodeViewer.svelte");

  it("the panel handles a reference request before any branch that opens a tab", () => {
    const reference = panel.indexOf('claimed.launcher === "reference"');
    expect(reference).toBeGreaterThan(0);
    expect(reference).toBeLessThan(panel.indexOf('claimed.launcher === "browser"'));
    const opensTab = panel.search(/newTab\(\s*claimed\.launcher/);
    expect(opensTab).toBeGreaterThan(0);
    expect(reference).toBeLessThan(opensTab);
  });

  it("the panel's send reports a refusal to the caller, and inserts through the checked path", () => {
    expect(panel).toContain("insertChecked(reference, { focus: true })");
    expect(panel).toContain("claimed.complete(reference ? sendReferenceToAgent(");
  });

  it("the panel picks the recipient by the shared rule and publishes its sessions for the viewers' label", () => {
    expect(panel).toContain("pickRecipient(recipientCandidates()");
    expect(panel).toContain("publishRecipients(here, rows)");
    expect(panel).toContain("forgetRecipients(publishedRecipientsFor)");
  });

  it("the viewer binds three chords: the key for the reference and the content, the physical key under Option for the tray", () => {
    expect(code).toContain('e.altKey && !e.shiftKey && e.code === "KeyL"');
    expect(code).toMatch(/!e\.altKey && e\.shiftKey && e\.key\.toLowerCase\(\) === "l"/);
    expect(code).toMatch(/!e\.altKey && !e\.shiftKey && e\.key\.toLowerCase\(\) === "l"/);
  });

  it("the viewer sends the lines it shows, and labels a draft only while the editor holds a different one", () => {
    expect(code).toContain("lines: rawLines.slice(selectionRange.lo - 1, selectionRange.hi)");
    expect(code).toContain("unsaved: isEditing && editDraft !== editBaseline");
  });

  it("the Code page hands its sends to the dock channel, which reports failure", () => {
    expect(read("FileViewer.svelte")).toContain("onSendSelectionToAgent={(text, checkout) => void sendToAgentFromPage(text, checkout)}");
  });
});
