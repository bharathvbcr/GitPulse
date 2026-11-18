import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { createPanelTabCounts } from "./panelTabCounts";

describe("panelTabCounts", () => {
  it("tracks open dock tab counts by normalized repo identity without redundant publishes", () => {
    const counts = createPanelTabCounts({ caseInsensitive: true });
    let publishes = 0;
    const stop = counts.subscribe(() => {
      publishes += 1;
    });
    publishes = 0;

    counts.report("/Work/Repo/", 2);
    expect(publishes).toBe(1);
    expect(counts.countFor(get(counts), "/work/repo", { caseInsensitive: true })).toBe(2);

    // Reporting the same count is a no-op
    counts.report("/work/repo", 2);
    expect(publishes).toBe(1);

    // Zero or negative removes the entry cleanly
    counts.report("/WORK/REPO", 0);
    expect(publishes).toBe(2);
    expect(counts.countFor(get(counts), "/work/repo", { caseInsensitive: true })).toBe(0);

    // Reporting 0 again when absent does not republish
    counts.report("/work/repo", -3);
    expect(publishes).toBe(2);

    counts.report("/work/other", 1);
    counts.forget("/work/other");
    expect(counts.countFor(get(counts), "/work/other", { caseInsensitive: true })).toBe(0);

    counts.report("/work/a", 3);
    counts.reset();
    expect(get(counts).size).toBe(0);
    stop();
  });
});
