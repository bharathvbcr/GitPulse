import { describe, expect, it } from "vitest";
import { renderTables } from "./churn-bench.mjs";

describe("the slow-switching context bench report", () => {
  const summary = {
    tier: "compact",
    output: "stream",
    pool: { cap: 2, hold_ms: 15_000, max: 8 },
    phases: [
      { every_s: 20, minutes: 10, switches: 30, created: 30, forced: [] },
      { every_s: 30, minutes: 10, switches: 20, created: 21, forced: ["idle"] },
    ],
    idle_final: "reset",
    idle_context_lost: true,
    dock_final: ["dom", "dom", "dom", "dom", "webgl"],
    total_created: 51,
    total_forced: 1,
    elapsed_s: 1201,
  };

  it("names the tier, the pool, each schedule's creations and forced losses, and the idle pane's end state", () => {
    const table = renderTables(summary);
    expect(table).toContain("compact tier, one pane receiving steady output (cap 2, hold 15 s, window max 8)");
    expect(table).toContain("| every 20 s for 10 min | 30 | 30 | 0 | none |");
    expect(table).toContain("| every 30 s for 10 min | 20 | 21 | 1 | idle |");
    expect(table).toContain("idle pane final renderer: reset (its context is lost)");
    expect(table).toContain("total created 51, forced 1");
  });
});
