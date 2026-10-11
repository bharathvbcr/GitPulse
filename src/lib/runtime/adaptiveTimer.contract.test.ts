import { existsSync, readFileSync } from "node:fs";
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
  "components/files/LivePulseDashboard.svelte",
  "components/ManviOpsPanel.svelte",
  "components/ManviHarnessPane.svelte",
  // The delivery poll's timeline ticker fires every second while a run is in
  // flight, and its `live` state outlives the window being hidden.
  "components/GitHubPanel.svelte",
  "components/FirebasePanel.svelte",
  "components/SecretsPanel.svelte",
  "components/AgentsView.svelte",
] as const;

describe("background UI timers follow the load cadence", () => {
  it.each(CONVERTED)("%s schedules through createAdaptiveTimer", (path) => {
    const source = readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
    expect(source).toContain("createAdaptiveTimer");
    // A bare `window.setInterval` is the shape this replaced. (ManviHarnessPane
    // keeps a `setInterval` *seam* whose real implementation is the timer.)
    expect(source).not.toMatch(/window\.setInterval\(/);
    expect(source).not.toMatch(/[^.\w]setInterval\(\(\) =>/);
  });

  it("there is one visibility-aware timer, not a second copy beside it", () => {
    expect(existsSync(new URL("../dom/visibleInterval.ts", import.meta.url))).toBe(false);
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
