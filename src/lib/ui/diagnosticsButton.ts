/**
 * When the header's diagnostics button is on screen.
 *
 * "issues" is the decluttered setting: the button appears only once something
 * has actually been recorded. It is safe precisely because the condition is
 * "the log is empty", not "no errors" — a warning-only log still shows the
 * button, and Diagnostics stays reachable from the command palette either way.
 */
export const DIAGNOSTICS_BUTTON_MODES = ["always", "issues"] as const;
export type DiagnosticsButtonMode = (typeof DIAGNOSTICS_BUTTON_MODES)[number];

export function isDiagnosticsButtonMode(value: unknown): value is DiagnosticsButtonMode {
  return (
    typeof value === "string" &&
    (DIAGNOSTICS_BUTTON_MODES as readonly string[]).includes(value)
  );
}

/** `recordedCount` is every diagnostics entry, not just the errors. */
export function showsDiagnosticsButton(
  mode: DiagnosticsButtonMode,
  recordedCount: number,
): boolean {
  return mode === "always" || recordedCount > 0;
}
