import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const source = readFileSync(new URL("./HygienePanel.svelte", import.meta.url), "utf8");

/**
 * The panel deletes files, so its failures are the ones most worth finding
 * afterwards. A rejection used to be shown through `String(e)`: unredacted,
 * never recorded in Diagnostics, and unreadable if a command rejected with a
 * plain object. The behaviour (the alert text and the recorded entry) is
 * exercised in harness/hygiene.html, which drives the mounted panel; these
 * contracts cover what a rendered page cannot show — which seam each catch
 * goes through.
 */
describe("HygienePanel failure reporting", () => {
  it("stringifies no rejection by hand", () => {
    expect(source).not.toMatch(/\bString\(\s*(err|reason|error|e)\b/);
  });

  it("records the inventory, prepare, cancel and execute failures under hygiene", () => {
    // The literal form, not a variable: scripts/diagnostics-contract.test.ts
    // derives the sources in use from exactly this shape.
    const reported = source.match(/reportPanelError\("hygiene", e\)/g) ?? [];
    expect(reported).toHaveLength(4);
  });

  it("formats the ignore-rule copy failure through the shared formatter", () => {
    expect(source).toMatch(/catch \(e\) \{ error = formatError\(e\); \}/);
  });
});
