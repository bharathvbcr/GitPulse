/**
 * Opt-in release check for GitPulse itself.
 *
 * The comparison lives in Rust (`src-tauri/src/updates`), which owns the tag
 * listing and the version ordering; this module owns the *policy* around it:
 * when an automatic check is allowed to run, and how each outcome reads.
 *
 * Nothing here fires on its own. `shouldAutoCheck` is the single gate, and it
 * returns false unless the user has explicitly enabled the preference.
 */
import { invoke } from "@tauri-apps/api/core";

/** Mirrors `crate::updates::UpdateCheck` (serde `camelCase`). */
export interface UpdateCheck {
  currentVersion: string;
  latestVersion: string;
  updateAvailable: boolean;
  releaseUrl: string;
  /** False means the check did not complete — the rest is unknown. */
  checked: boolean;
  error: string | null;
}

/** How the check reads to a user. Never collapses "failed" into "current". */
export type UpdateStatusKind = "available" | "current" | "failed";

export interface UpdateStatus {
  kind: UpdateStatusKind;
  message: string;
}

/** Minimum gap between two *automatic* checks. Manual checks ignore it. */
export const AUTO_CHECK_INTERVAL_MS = 24 * 60 * 60 * 1000;

/**
 * Whether an automatic check may run now.
 *
 * Fails closed on every axis: the preference must be on, and the throttle
 * must have elapsed. A `lastCheckedAt` in the future (clock moved backwards,
 * restored profile) is treated as "just checked" rather than as an elapsed
 * interval, so a bad timestamp cannot turn this into a check on every launch.
 */
export function shouldAutoCheck(
  prefs: { checkForUpdates: boolean; lastUpdateCheckAt: number },
  now: number,
): boolean {
  if (!prefs.checkForUpdates) return false;
  const last = prefs.lastUpdateCheckAt;
  if (!Number.isFinite(last) || last <= 0) return true;
  if (last > now) return false;
  return now - last >= AUTO_CHECK_INTERVAL_MS;
}

/**
 * Whether this specific version has already been dismissed by the user.
 *
 * Scoped to the version string, so dismissing 0.1.0 stays quiet for 0.1.0 and
 * speaks up again for 0.2.0. An unchecked or unavailable result is never
 * "dismissed" — there is nothing to dismiss.
 */
export function isDismissed(result: UpdateCheck, dismissedVersion: string): boolean {
  if (!result.checked || !result.updateAvailable) return false;
  return dismissedVersion !== "" && dismissedVersion === result.latestVersion;
}

/** Renders one outcome as a status kind plus a human sentence. */
export function describeUpdateCheck(result: UpdateCheck): UpdateStatus {
  if (!result.checked) {
    const reason = result.error?.trim();
    return {
      kind: "failed",
      message: reason
        ? `Could not check for updates: ${reason}`
        : "Could not check for updates.",
    };
  }
  if (result.updateAvailable) {
    return {
      kind: "available",
      message: `GitPulse ${result.latestVersion} is available (you have ${result.currentVersion}).`,
    };
  }
  return {
    kind: "current",
    message: `GitPulse ${result.currentVersion} is the latest release.`,
  };
}

/**
 * The registered backend command. A module-level literal, not an inline
 * argument: `scripts/check-ipc-contract.mjs` resolves the name statically to
 * prove this seam is registered, and a name it cannot resolve fails the
 * contract check rather than passing unverified.
 */
const APP_UPDATE_COMMAND = "cmd_check_app_update";

/** Injectable IPC seam, so tests never touch the real bridge. */
export type UpdateInvoker = (command: string) => Promise<UpdateCheck>;

const defaultInvoker: UpdateInvoker = () => invoke<UpdateCheck>(APP_UPDATE_COMMAND);

/**
 * Runs one check.
 *
 * The backend command is infallible, but the IPC bridge itself is not (the
 * webview can tear down mid-call). A rejected invoke becomes an unchecked
 * result rather than a thrown error, so no caller can accidentally render a
 * transport failure as "up to date".
 */
export async function checkForAppUpdate(
  invokeFn: UpdateInvoker = defaultInvoker,
): Promise<UpdateCheck> {
  try {
    return await invokeFn(APP_UPDATE_COMMAND);
  } catch (error) {
    return {
      currentVersion: "",
      latestVersion: "",
      updateAvailable: false,
      releaseUrl: "",
      checked: false,
      error: error instanceof Error ? error.message : String(error),
    };
  }
}

/** Everything the background check needs, injected so it is testable. */
export interface UpdateNotifyDeps {
  prefs: {
    checkForUpdates: boolean;
    lastUpdateCheckAt: number;
    dismissedUpdateVersion: string;
  };
  now: number;
  check: () => Promise<UpdateCheck>;
  /** Records a *completed* check, restarting the throttle. */
  markChecked: (at: number) => void;
  /** Surfaces an available, undismissed release to the user. */
  notify: (result: UpdateCheck) => void;
  /** Where a failed background check goes — diagnostics, not the user. */
  onError: (message: string) => void;
}

/** What one background pass did, for tests and diagnostics. */
export type UpdateNotifyOutcome = "skipped" | "current" | "notified" | "failed";

/**
 * The whole automatic path, in one testable function.
 *
 * Contract, in order:
 *   1. `shouldAutoCheck` gates everything — opted out means no network call.
 *   2. Only a *completed* check restarts the 24h throttle, so a flaky network
 *      retries on the next launch instead of going quiet for a day.
 *   3. A failed check is reported to diagnostics, never to the user: nobody
 *      opted into a toast about a background request they did not make.
 *   4. A dismissed version stays silent, but only that exact version.
 */
export async function maybeNotifyUpdate(
  deps: UpdateNotifyDeps,
): Promise<UpdateNotifyOutcome> {
  if (!shouldAutoCheck(deps.prefs, deps.now)) return "skipped";

  const result = await deps.check();
  if (!result.checked) {
    deps.onError(describeUpdateCheck(result).message);
    return "failed";
  }
  deps.markChecked(deps.now);
  if (!result.updateAvailable) return "current";
  if (isDismissed(result, deps.prefs.dismissedUpdateVersion)) return "current";
  deps.notify(result);
  return "notified";
}
