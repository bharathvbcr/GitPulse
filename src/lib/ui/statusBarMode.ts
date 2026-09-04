/**
 * How much of the bottom status bar the user wants on screen.
 *
 * "hidden" is a preference, not a guarantee — see `resolveStatusBarMode`.
 */
export const STATUS_BAR_MODES = ["full", "minimal", "hidden"] as const;
export type StatusBarMode = (typeof STATUS_BAR_MODES)[number];

export function isStatusBarMode(value: unknown): value is StatusBarMode {
  return (
    typeof value === "string" && (STATUS_BAR_MODES as readonly string[]).includes(value)
  );
}

export interface StatusBarSignals {
  /** A merge / rebase / cherry-pick / revert / bisect is parked mid-flight. */
  operationParked: boolean;
  /** Files still carrying conflict markers. */
  conflictedCount: number;
  /** The repository watcher is degraded, so what is on screen may be stale. */
  watchDegraded: boolean;
}

export interface ResolvedStatusBar {
  mode: StatusBarMode;
  /** True when a signal overrode the preference, so the bar can say why. */
  forced: boolean;
}

/**
 * The status bar is the only always-on surface that says a merge is parked,
 * conflicts are unresolved, or live updates stopped arriving. Decluttering
 * must never turn one of those into silence, so any of the three pulls a
 * hidden bar back to "minimal": the preference still holds for the quiet
 * case it was asked for, and the loud case still reaches the user.
 *
 * "minimal" needs no override — it keeps exactly those three signals and
 * drops only the ambient readouts.
 */
export function resolveStatusBarMode(
  preference: StatusBarMode,
  signals: StatusBarSignals,
): ResolvedStatusBar {
  if (preference !== "hidden") return { mode: preference, forced: false };
  const alarming =
    signals.operationParked || signals.conflictedCount > 0 || signals.watchDegraded;
  return alarming ? { mode: "minimal", forced: true } : { mode: "hidden", forced: false };
}
