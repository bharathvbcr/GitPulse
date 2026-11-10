import { normalizeSeverity, type UpdateKind } from "../../health/format";
import type { HealthTone } from "../../health/summary";

/**
 * The role a Health verdict wears. `HealthTone` is Health's own vocabulary
 * (clear / note / unknown / warn / critical); `gp-chip` and `gp-notice` speak
 * the design roles (ok / info / unknown / warn / danger). This is the one map
 * between them, so no Health surface picks a palette color for a verdict.
 *
 * `unknown` stays `unknown` rather than collapsing into `warn` or `ok`: both
 * `gp-chip` and `gp-notice` draw it differently (a dashed edge on the notice),
 * so a check that could not run never looks like one that ran.
 */
export type RoleTone = "ok" | "info" | "unknown" | "warn" | "danger";

export function roleTone(tone: HealthTone): RoleTone {
  switch (tone) {
    case "critical":
      return "danger";
    case "warn":
      return "warn";
    case "unknown":
      return "unknown";
    case "note":
      return "info";
    default:
      return "ok";
  }
}

/**
 * The role a finding's severity wears. `normalizeSeverity` is the one reader of
 * the raw string (GitHub's casing, CodeQL's `error`/`warning`), so a severity
 * cannot be read two ways. Critical and high are both `danger`; the word on the
 * chip says which.
 */
export function severityTone(severity: string): RoleTone | "neutral" {
  switch (normalizeSeverity(severity)) {
    case "critical":
    case "high":
      return "danger";
    case "moderate":
      return "warn";
    case "low":
      return "info";
    default:
      return "neutral";
  }
}

/** The role a configuration issue's notice wears (`error`, `warning`, anything else is a note). */
export function issueNoticeTone(severity: string): RoleTone {
  switch (severity.trim().toLowerCase()) {
    case "error":
      return "danger";
    case "warning":
      return "warn";
    default:
      return "info";
  }
}

/** The role an update kind wears, for the "latest" column of outdated packages. */
export function updateKindTextClass(kind: UpdateKind): string {
  switch (kind) {
    case "major":
      return "text-danger";
    case "minor":
      return "text-warn";
    case "patch":
    case "prerelease":
      return "text-info";
    default:
      return "text-textMuted";
  }
}

/** The text-color utility for a role, for an icon or a one-line status. */
export function roleTextClass(tone: RoleTone): string {
  switch (tone) {
    case "danger":
      return "text-danger";
    case "warn":
      return "text-warn";
    case "info":
      return "text-info";
    case "unknown":
      return "text-unknown";
    default:
      return "text-ok";
  }
}
