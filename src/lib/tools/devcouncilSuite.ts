/**
 * How the DevCouncil component inventory is worded.
 *
 * Separate from the panel that renders it because every function here encodes
 * a distinction that is easy to lose in markup and impossible to test inside
 * it: *installed* is not *versioned*, and *unchecked* is not *healthy*.
 */

import type {
  ComponentNeed,
  ComponentStatus,
  SuiteReport,
  VersionReading,
} from "../codeintel/types";

export const NEED_LABEL: Record<ComponentNeed, string> = {
  required: "Required",
  host_resolved: "Used by the Manvi host",
  optional: "Not used by GitPulse",
};

export function needLabel(need: ComponentNeed): string {
  return NEED_LABEL[need] ?? "Unknown";
}

/**
 * One line describing a component's version.
 *
 * `dcstore`, `dcverify` and `dcgrep` reject `--version`, so their reading is
 * `not_exposed`. Rendering that as an empty string would read as "unknown, so
 * probably old"; it means the opposite — the binary ran, and there is simply
 * nothing to ask.
 */
export function versionText(version: VersionReading): string {
  switch (version.kind) {
    case "reported":
      return version.version;
    case "not_exposed":
      return `version not reported — ${version.detail}`;
    default:
      return `could not run — ${version.detail}`;
  }
}

/** Components GitPulse or its host needs that are not installed. */
export function missingNeeded(components: readonly ComponentStatus[]): ComponentStatus[] {
  return components.filter(
    (component) => !component.installed && component.need !== "optional",
  );
}

export function missingSummary(components: readonly ComponentStatus[]): string {
  const missing = missingNeeded(components);
  if (missing.length === 0) {
    return "Every component GitPulse or its Manvi host uses is installed.";
  }
  const ids = missing.map((component) => component.id).join(", ");
  return missing.length === 1
    ? `1 component is missing: ${ids}`
    : `${missing.length} components are missing: ${ids}`;
}

export type HealthSummary =
  | { kind: "unchecked"; reason: string }
  | { kind: "clean" }
  | { kind: "warnings"; warnings: string[] };

/**
 * What to say about installation health.
 *
 * The `unchecked` case is the whole reason this is a function. `devmap doctor`
 * needs a repository to run in; without one there are no warnings *because
 * nothing looked*, and an empty list rendered as "nothing to report" would be
 * a check that could not run reporting the same thing as a check that ran and
 * passed.
 */
export function healthSummary(report: SuiteReport): HealthSummary {
  if (report.doctor_reason) {
    return { kind: "unchecked", reason: report.doctor_reason };
  }
  if (!report.doctor || !report.doctor.available) {
    return {
      kind: "unchecked",
      reason: "devmap doctor did not answer",
    };
  }
  if (report.warnings.length === 0) return { kind: "clean" };
  return { kind: "warnings", warnings: report.warnings };
}

/** `devmap · 2 of 3 installed — missing dcgrep`, for one install preset. */
export function presetSummary(preset: {
  id: string;
  present: number;
  total: number;
  missing: string[];
}): string {
  const head = `${preset.present} of ${preset.total} installed`;
  return preset.missing.length > 0
    ? `${head} — missing ${preset.missing.join(", ")}`
    : head;
}
