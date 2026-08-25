export function coverageBarColor(percentage: number): string {
  const pct = Number.isFinite(percentage) && percentage >= 0 ? percentage : 0;
  if (pct >= 80) return "#34d399";
  if (pct >= 50) return "#fbbf24";
  return "#f87171";
}

export function coverageHitClass(hits: number | undefined): string {
  if (hits === undefined || !Number.isFinite(hits) || hits < 0) return "";
  if (hits > 0) return "bg-emerald-500/15";
  return "bg-red-500/20";
}

export function formatCoveragePercent(percentage: number): string {
  if (!Number.isFinite(percentage)) return "0.0%";
  return `${Math.max(0, percentage).toFixed(1)}%`;
}
