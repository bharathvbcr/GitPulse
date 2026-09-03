import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "PulseView.svelte"),
  "utf8",
);
const heatmap = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "PulseHeatmap.svelte"),
  "utf8",
);

describe("PulseView honesty contracts", () => {
  it("does not present a failed language scan as zero LOC", () => {
    expect(source).toContain('status: "failed"');
    expect(source).not.toContain("totalLoc = 0");
    expect(source).toContain("LOC is not shown as zero");
  });

  it("surfaces a partial language scan instead of a complete count", () => {
    expect(source).toContain("langReport?.truncated");
    expect(source).toContain('status: truncated ? "partial" : "ok"');
  });

  it("does not deepen a payload-capped walk", () => {
    expect(source).toContain("!report?.payload_truncated");
    expect(source).toContain("payload budget");
  });

  it("scopes personal tiles behind an explicit author filter", () => {
    expect(source).toContain("All contributors (every local and remote branch)");
    expect(source).toContain("authorFilter");
  });

  it("lazy-loads coverage on the hotspots tab", () => {
    expect(source).toContain('activeTab === "hotspots"');
    expect(source).toContain("cmd_scan_coverage");
  });
});

describe("PulseHeatmap click-to-filter", () => {
  it("writes a date: prefix the Graph filter actually understands", () => {
    expect(heatmap).toContain("date:${day.date}");
    expect(heatmap).toContain('repoStore.setActiveTab("history")');
  });

  it("labels the 53-week calendar as 53 weeks", () => {
    expect(heatmap).toContain("Past 53 Weeks");
    expect(heatmap).not.toContain("Past 52 Weeks");
  });
});
