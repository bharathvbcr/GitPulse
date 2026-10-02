import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * Recurring UI timers that used to wake on a fixed `setInterval` even when
 * the window was hidden or the event loop was already late. Each one has to
 * go through `createAdaptiveTimer`, which is the pause-and-stretch scheduler.
 */
const CONVERTED = [
  "components/GlobalCleaner.svelte",
  "components/HygienePanel.svelte",
  "desktop/StatusPopover.svelte",
  "components/TaskBoard.svelte",
  "components/TaskArchive.svelte",
  "components/TaskManviAssist.svelte",
] as const;

describe("background UI timers follow the load cadence", () => {
  it.each(CONVERTED)("%s schedules through createAdaptiveTimer", (path) => {
    const source = readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
    expect(source).toContain("createAdaptiveTimer");
    expect(source).not.toMatch(/setInterval\s*\(/);
  });

  it("the work-tree poll and the automatic-enhancement poll ask the cadence policy", () => {
    const repoStore = readFileSync(new URL("../stores/repoStore.ts", import.meta.url), "utf8");
    const client = readFileSync(new URL("../workbench/client.ts", import.meta.url), "utf8");
    for (const source of [repoStore, client]) {
      expect(source).toContain("decideCadence");
      expect(source).not.toMatch(/setInterval\s*\(/);
    }
  });
});
