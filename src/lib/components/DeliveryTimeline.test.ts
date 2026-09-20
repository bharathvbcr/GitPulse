import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import DeliveryTimeline from "./DeliveryTimeline.svelte";
import { TIMELINE_PREVIEW_COUNT, type TimelineRow } from "../delivery/timeline";
import type { MonitorPhase } from "../delivery/phase";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "DeliveryTimeline.svelte"),
  "utf8",
);

const T0 = Date.parse("2026-09-17T08:00:00Z");

function makeRow(overrides: Partial<TimelineRow> = {}): TimelineRow {
  return {
    id: "1",
    label: "run",
    sublabel: "CI",
    phase: "settled_ok" as MonitorPhase,
    stateLabel: "Passed",
    startedAt: new Date(T0).toISOString(),
    endedAt: new Date(T0 + 60_000).toISOString(),
    commitSha: "0123456789abcdef0123456789abcdef01234567",
    branch: "main",
    trigger: "push",
    url: "https://example.invalid/run/1",
    ...overrides,
  };
}

function manyRows(n: number, phase: MonitorPhase = "settled_ok"): TimelineRow[] {
  return Array.from({ length: n }, (_, i) =>
    makeRow({
      id: String(i + 1),
      label: `run ${i + 1}`,
      phase,
      stateLabel: phase === "settled_bad" ? "Failed" : "Passed",
      endedAt: new Date(T0 + (i + 1) * 10_000).toISOString(),
    }),
  );
}

function countAttr(html: string, attr: string): number {
  return html.split(attr).length - 1;
}

function visible(html: string): string {
  return html.replace(/<[^>]+>/g, " ").replace(/\s+/g, " ");
}

describe("DeliveryTimeline preview contracts", () => {
  it("collapses the drawn list with the shared preview helpers, not a local copy", () => {
    // A second expander that counted differently from the run cards is how
    // "show all" came to mean two things on the same rail.
    expect(source).toContain('from "../ui/previewList"');
    expect(source).not.toContain('from "../github/runActions"');
    expect(source).toContain("previewSlice(rows, expanded, TIMELINE_PREVIEW_COUNT)");
    expect(source).toContain("overflowsPreview(rows.length, TIMELINE_PREVIEW_COUNT)");
    expect(source).toContain("expandLabel(rows.length, expanded, sampleNoun)");
  });

  it("measures the full sample even when the list is collapsed", () => {
    // The preview decides how many cards are drawn. Feeding it to the rate,
    // the median, or the bar scale would make "Passed: 70% (14/20)" become
    // "Passed: 0% (0/3)" the moment the first three were red — a collapsed
    // section lying about the repository.
    expect(source).toContain("verdictRate(rows)");
    expect(source).toContain("medianSettledDurationMs(rows, now)");
    expect(source).toContain("longestDurationMs(rows, now)");
    expect(source).not.toContain("verdictRate(shown)");
    expect(source).not.toContain("medianSettledDurationMs(shown");
    expect(source).not.toContain("longestDurationMs(shown");
  });

  it("keys the drawn rows, not the measured sample", () => {
    expect(source).toContain("keyedList(shown,");
    expect(source).not.toContain("keyedList(rows,");
  });

  it("renders glance tiles rather than commit subjects", () => {
    expect(source).toContain("glanceName(row)");
    expect(source).toContain("glanceState(row.phase)");
    expect(source).toContain("glanceTitle(row)");
    expect(source).toContain("data-delivery-glances");
    // Tooltips may still name the subject. The tile face must not.
    expect(source).not.toMatch(/>\s*\{row\.label\}\s*</);
    expect(source).not.toMatch(/>\s*\{row\.stateLabel\}\s*</);
  });
});

describe("DeliveryTimeline collapsed listing", () => {
  it("draws three recent rows of twenty and keeps the sample honest", () => {
    expect(TIMELINE_PREVIEW_COUNT).toBe(3);
    const rows = [
      ...manyRows(6, "settled_bad"),
      ...manyRows(14, "settled_ok").map((row, i) => ({ ...row, id: String(i + 7) })),
    ];
    const { body } = render(DeliveryTimeline, {
      props: { title: "Run duration and outcome", rows, now: T0 + 120_000, sampleNoun: "runs" },
    });
    expect(countAttr(body, "data-delivery-row=")).toBe(TIMELINE_PREVIEW_COUNT);
    expect(countAttr(body, "data-delivery-state=")).toBe(TIMELINE_PREVIEW_COUNT);
    expect(countAttr(body, "data-delivery-strip-seg")).toBe(20);
    const prose = visible(body);
    expect(prose).toContain("70%");
    expect(prose).toContain("(14/20)");
    expect(prose).toContain("Sample: last 20");
    expect(prose).toContain("Show all 20 runs");
    expect(body).toContain('aria-expanded="false"');
  });

  it("shows verdict, duration and short identity — not the commit subject", () => {
    const rows = [
      makeRow({
        id: "1",
        label: "chore(vendor): re-vendor dc-store from upstream",
        sublabel: "CI",
        phase: "settled_bad",
        stateLabel: "Failed",
      }),
      makeRow({
        id: "2",
        label: "docs(changelog): record 1.2.1",
        sublabel: "Code Coverage",
        phase: "settled_ok",
        stateLabel: "Passed",
      }),
      makeRow({
        id: "3",
        label: "merge: the Windows native helper",
        sublabel: "CI",
        phase: "unknown",
        stateLabel: "Completed (no conclusion reported)",
      }),
    ];
    const { body } = render(DeliveryTimeline, {
      props: { title: "Run duration and outcome", rows, now: T0 + 120_000 },
    });
    const prose = visible(body);
    expect(prose).not.toContain("chore(vendor)");
    expect(prose).not.toContain("re-vendor");
    expect(prose).not.toContain("Completed (no conclusion reported)");
    expect(prose).toContain("CI");
    expect(prose).toContain("Code Coverage");
    expect(prose).toContain("Fail");
    expect(prose).toContain("Pass");
    expect(countAttr(body, "data-delivery-glances")).toBe(1);
    expect(body).toContain('data-delivery-state="unknown"');
  });

  it("does not offer an expander when every row already fits", () => {
    const rows = manyRows(TIMELINE_PREVIEW_COUNT);
    const { body } = render(DeliveryTimeline, {
      props: { title: "Run duration and outcome", rows, now: T0 + 120_000 },
    });
    expect(countAttr(body, "data-delivery-row=")).toBe(TIMELINE_PREVIEW_COUNT);
    expect(body).not.toContain("Show all");
    expect(body).not.toContain('data-delivery-expand');
  });

  it("never dresses a failed listing as an empty preview", () => {
    const { body } = render(DeliveryTimeline, {
      props: {
        title: "Run duration and outcome",
        rows: [],
        now: T0,
        checked: false,
        error: "gh: could not resolve host",
        sampleNoun: "runs",
      },
    });
    expect(body).toContain("Could not read runs");
    expect(body).not.toContain("Show all");
    expect(countAttr(body, "data-delivery-row=")).toBe(0);
  });
});
