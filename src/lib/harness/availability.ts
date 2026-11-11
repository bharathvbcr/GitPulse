import type { HarnessStatus } from "../stores/harnessStore";

export type HarnessPermissionMode =
  | "not-probed"
  | "connected"
  | "unguarded"
  | "blocked";

/**
 * Mirrors the native gate's permission boundary. A deliberately absent MANVI
 * installation is the sole condition that may proceed unchecked; a sidecar
 * that exists but is busy, wedged, timed out, or incompatible fails closed.
 */
export function harnessPermissionMode(
  status: HarnessStatus | null,
): HarnessPermissionMode {
  if (!status) return "not-probed";
  if (status.available) return "connected";
  return status.error_code === "not_installed" ? "unguarded" : "blocked";
}

/** User-facing permission truth shared by every MANVI status surface. */
export function harnessPermissionSummary(status: HarnessStatus | null): string {
  switch (harnessPermissionMode(status)) {
    case "connected":
      return `Manvi policy checks are connected${status?.protocol ? ` (protocol ${status.protocol})` : ""}.`;
    case "unguarded":
      return "Manvi is not installed. Git actions can proceed without a policy check and are reported as not checked.";
    case "blocked":
      return `Manvi policy checks are failing. Guarded mutations are blocked until the gate recovers.${status?.error ? ` ${status.error}` : ""}`;
    default:
      return "Manvi policy status has not been checked yet.";
  }
}

/**
 * What Manvi reports as its `posture`, in words a person reads. `host` and
 * `devcouncil` are the two values `manvi serve --posture` takes. Any other
 * value is shown as Manvi sent it, with underscores and hyphens as spaces,
 * rather than guessed at; an empty one says nothing.
 */
export function harnessPostureLabel(posture: string | null | undefined): string | null {
  const value = (posture ?? "").trim();
  if (!value) return null;
  switch (value.toLowerCase()) {
    case "host":
      return "hosted by GitPulse";
    case "devcouncil":
      return "DevCouncil rules";
    default:
      return value.replace(/[_-]+/g, " ").toLowerCase();
  }
}
