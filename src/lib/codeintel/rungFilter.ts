/**
 * Rung filter labels and "what you did not see" copy for RungHistogram.
 *
 * `min_rung` cannot combine with layered impact — callers must suppress the
 * rung control wherever layered impact is the active view.
 */

import type { CodeintelRung, CodeintelRungHistogram } from "./types";

export const RUNG_OPTIONS: Array<{ value: CodeintelRung | "all"; label: string }> = [
  { value: "all", label: "All rungs" },
  { value: "speculative", label: "Speculative+" },
  { value: "high", label: "High+" },
  { value: "deterministic", label: "Deterministic only" },
];

export function rungParam(minRung: CodeintelRung | "all" | null | undefined): CodeintelRung | undefined {
  if (!minRung || minRung === "all") return undefined;
  return minRung;
}

/**
 * One line explaining the population before filtering — so "3 edges" is not
 * confused with "the graph only had three".
 */
export function rungHistogramLine(
  hist: CodeintelRungHistogram | null | undefined,
): string | null {
  if (!hist) return null;
  const parts = [
    `${hist.deterministic} deterministic`,
    `${hist.high} high`,
    `${hist.speculative} speculative`,
  ];
  if (hist.filtered_out > 0) {
    parts.push(`${hist.filtered_out} filtered out by min_rung`);
  }
  return `What you did not necessarily see: ${parts.join(" · ")}`;
}

export function shouldShowRungControl(layeredImpactActive: boolean): boolean {
  return !layeredImpactActive;
}
