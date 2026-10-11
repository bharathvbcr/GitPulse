import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));

/**
 * What the blame page does is checked by rendering it: `harness/blame.html`
 * (`node scripts/browser-regressions.mjs --harness blame`) mounts the real
 * BlameViewer against fixture IPC and checks the timeline, selection, rail,
 * history walk, retry, coverage notice, explorer toggle, refresh and
 * diagnostics reporting in Chrome. Only what a rendered page cannot see stays
 * here, as a contract on the source.
 */
const source = readFileSync(join(here, "BlameViewer.svelte"), "utf8");

describe("BlameViewer source contracts", () => {
  it("holds blame rows in raw state: replaced wholesale, never written in place", () => {
    // A file can have tens of thousands of lines; a deep proxy over each row
    // and an in-place mutation would re-render the whole list on any write.
    expect(source).toContain("$state.raw([])");
    expect(source).not.toMatch(/blameLines\.(?:push|pop|shift|unshift|splice|sort|reverse|fill)\(/);
    expect(source).not.toMatch(/blameLines\[[^\]]+\](?:\.\w+)*\s*=[^=]/);
  });

  it("formats no rejection by hand: every failure goes through the diagnostics seam", () => {
    // That the seam is reached is a rendered check (the failed blame lands in
    // the ring); that nothing bypasses it with `String(err)` is only visible here.
    expect(source).not.toMatch(/String\(\s*(err|reason|error|e)\s*\)/);
  });
});
