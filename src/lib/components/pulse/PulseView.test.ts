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

describe("PulseView export card wiring", () => {
  const optionsBlock = /<PulseExportModal[\s\S]*?options=\{\{([\s\S]*?)\}\}/.exec(source)?.[1];

  it("hands the card an options block", () => {
    expect(optionsBlock).toBeTruthy();
  });

  it("never collapses an unmeasured metric to zero on the exported card", () => {
    expect(optionsBlock).not.toMatch(/\?\?\s*0/);
    expect(optionsBlock).not.toMatch(/:\s*0\s*,/);
    for (const field of ["totalLoc", "busFactor", "halfLifeDays", "conventionalPct", "signedPct"]) {
      expect(optionsBlock).toMatch(new RegExp(`${field}:[^,]*null`));
    }
  });

  it("takes the commit count and its active days from one population", () => {
    expect(optionsBlock).toContain("totalCommits: cardWindow.commits");
    expect(optionsBlock).toContain("activeDays: cardWindow.activeDays");
    expect(source).toContain("computeCommitWindow(visibleCommits)");
  });

  it("tells the card which scans were capped and which author it is scoped to", () => {
    expect(optionsBlock).toContain("truncated: report.truncated");
    expect(optionsBlock).toContain("locPartial:");
    expect(optionsBlock).toContain("blamePartial:");
    expect(optionsBlock).toContain("authorScope: scopedAuthor");
  });

  it("treats a blame report that scanned nothing as unmeasured", () => {
    expect(source).toContain("(knowledge?.scanned_files ?? 0) > 0");
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

describe("PulseView diagnostics affordance", () => {
  const source = readFileSync(new URL("./PulseView.svelte", import.meta.url), "utf8");

  it("routes the user to Diagnostics, where the backend log tail lands", () => {
    expect(source).toContain('new CustomEvent("gitpulse:diagnostics")');
    expect(source).toContain("Open Diagnostics");
  });

  it("shows the underlying message rather than the headline alone", () => {
    expect(source).toContain("{error}");
  });

  it("hands each secondary failure to the panel that owns it", () => {
    expect(source).toContain("error={knowledgeError}");
    expect(source).toContain("error={doraError}");
  });
});
