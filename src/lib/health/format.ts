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

export type UpdateKind = "major" | "minor" | "patch" | "same" | "unknown";

function parseSemver(raw: string): [number, number, number] | null {
  const cleaned = raw.trim().replace(/^v/, "").split("-")[0] ?? "";
  const parts = cleaned.split(".");
  if (parts.length < 1) return null;
  const major = Number(parts[0]);
  const minor = Number(parts[1] ?? "0");
  const patch = Number(parts[2] ?? "0");
  if (![major, minor, patch].every((n) => Number.isFinite(n))) return null;
  return [major, minor, patch];
}

export function updateKind(current: string, latest: string): UpdateKind {
  const a = parseSemver(current);
  const b = parseSemver(latest);
  if (!a || !b) return "unknown";
  if (b[0] > a[0]) return "major";
  if (b[0] === a[0] && b[1] > a[1]) return "minor";
  if (b[0] === a[0] && b[1] === a[1] && b[2] > a[2]) return "patch";
  return "same";
}

export function updateKindClass(kind: UpdateKind): string {
  switch (kind) {
    case "major":
      return "text-rose-300";
    case "minor":
      return "text-amber-300";
    case "patch":
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
  options?: { ran?: boolean },
): string {
  if (summary.total === 0) {
    return options?.ran ? "No known vulnerabilities" : "Audit did not run";
  }
  const parts: string[] = [];
  if (summary.critical) parts.push(`${summary.critical} critical`);
  if (summary.high) parts.push(`${summary.high} high`);
  if (summary.moderate) parts.push(`${summary.moderate} moderate`);
  if (summary.low) parts.push(`${summary.low} low`);
  if (summary.unknown) parts.push(`${summary.unknown} unranked`);
  return parts.join(" · ") || `${summary.total} findings`;
}
