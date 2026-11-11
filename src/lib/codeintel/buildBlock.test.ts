import { describe, expect, it } from "vitest";
import { buildBlockReason } from "./buildBlock";
import type { LiveIndexPhase } from "./liveIndex";

describe("buildBlockReason", () => {
  it("blocks while the automatic index is queued or running", () => {
    expect(buildBlockReason(false, "scheduled")).toContain("already running");
    expect(buildBlockReason(false, "running")).toContain("already running");
  });

  it("names the missing CLI, and that beats a running index", () => {
    expect(buildBlockReason(true, undefined)).toBe("The code map tool is not installed — run Setup first");
    expect(buildBlockReason(true, "running")).toBe("The code map tool is not installed — run Setup first");
  });

  it("leaves Build available when the automatic path has stopped or has nothing to do", () => {
    const open: Array<LiveIndexPhase | undefined> = [
      "failed", "paused", "skipped", "idle", "ready", undefined,
    ];
    for (const phase of open) expect(buildBlockReason(false, phase), String(phase)).toBeNull();
  });
});
