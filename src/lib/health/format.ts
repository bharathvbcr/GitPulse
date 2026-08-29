export type AuditSeverity = "critical" | "high" | "moderate" | "low" | "info";

export function normalizeSeverity(severity: string): AuditSeverity {
  const key = severity.trim().toLowerCase();
  if (key === "critical" || key === "high" || key === "low" || key === "info") {
    return key;
  }
  if (key === "moderate" || key === "medium") {
    return "moderate";
  }
  return "info";
}

export function severityClass(severity: string): string {
  switch (normalizeSeverity(severity)) {
    case "critical":
      return "text-rose-300 bg-rose-500/15";
    case "high":
      return "text-red-300 bg-red-500/15";
    case "moderate":
      return "text-amber-300 bg-amber-500/15";
    case "low":
      return "text-sky-300 bg-sky-500/15";
    default:
      return "text-textMuted bg-surfaceHover";
  }
}

export function issueClass(severity: string): string {
  const key = severity.trim().toLowerCase();
  if (key === "error") return "border-rose-500/30 bg-rose-500/10 text-rose-200";
  if (key === "warning") return "border-amber-500/30 bg-amber-500/10 text-amber-200";
  return "border-border bg-surface text-textMuted";
}

export type UpdateKind = "major" | "minor" | "patch" | "prerelease" | "same" | "unknown";

interface ParsedSemver {
  core: [number, number, number];
  prerelease: string[] | null;
}

function parseSemver(raw: string): ParsedSemver | null {
  const match = raw.trim().match(
    /^v?(\d+)(?:\.(\d+))?(?:\.(\d+))?(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$/,
  );
  if (!match) return null;
  return {
    core: [Number(match[1]), Number(match[2] ?? "0"), Number(match[3] ?? "0")],
    prerelease: match[4] ? match[4].split(".") : null,
  };
}

function comparePrerelease(a: string[] | null, b: string[] | null): number {
  if (a === null && b === null) return 0;
  if (a === null) return 1;
  if (b === null) return -1;
  const count = Math.max(a.length, b.length);
  for (let index = 0; index < count; index += 1) {
    if (a[index] === undefined) return -1;
    if (b[index] === undefined) return 1;
    if (a[index] === b[index]) continue;
    const aNumeric = /^\d+$/.test(a[index]);
    const bNumeric = /^\d+$/.test(b[index]);
    if (aNumeric && bNumeric) return Number(a[index]) - Number(b[index]);
    if (aNumeric !== bNumeric) return aNumeric ? -1 : 1;
    return a[index].localeCompare(b[index]);
  }
  return 0;
}

export function updateKind(current: string, latest: string): UpdateKind {
  const a = parseSemver(current);
  const b = parseSemver(latest);
  if (!a || !b) return "unknown";
  if (b.core[0] > a.core[0]) return "major";
  if (b.core[0] === a.core[0] && b.core[1] > a.core[1]) return "minor";
  if (
    b.core[0] === a.core[0] &&
    b.core[1] === a.core[1] &&
    b.core[2] > a.core[2]
  ) return "patch";
  if (
    b.core[0] === a.core[0] &&
    b.core[1] === a.core[1] &&
    b.core[2] === a.core[2] &&
    comparePrerelease(a.prerelease, b.prerelease) < 0
  ) return "prerelease";
  return "same";
}

export function updateKindClass(kind: UpdateKind): string {
  switch (kind) {
    case "major":
      return "text-rose-300";
    case "minor":
      return "text-amber-300";
    case "patch":
    case "prerelease":
      return "text-sky-300";
    default:
      return "text-textMuted";
  }
}

export function formatAuditCounts(
  summary: {
    critical: number;
    high: number;
    moderate: number;
    low: number;
    unknown?: number;
    total: number;
  },
  options?: { complete?: boolean; ran?: boolean },
): string {
  if (summary.total === 0) {
    if (options?.complete === true) return "No known vulnerabilities";
    if (options?.ran === true) return "Audit incomplete";
    return "Audit did not run";
  }
  const parts: string[] = [];
  if (summary.critical) parts.push(`${summary.critical} critical`);
  if (summary.high) parts.push(`${summary.high} high`);
  if (summary.moderate) parts.push(`${summary.moderate} moderate`);
  if (summary.low) parts.push(`${summary.low} low`);
  if (summary.unknown) parts.push(`${summary.unknown} unranked`);
  return parts.join(" · ") || `${summary.total} findings`;
}
