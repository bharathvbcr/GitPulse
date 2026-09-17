/**
 * Pure vocabulary for App Hosting rollout states. Kept out of the panel so the
 * mapping from Firebase's states to what a reader sees — which decides whether
 * a commit is shown as live — is unit-testable.
 */
import type { MonitorPhase } from "../delivery/phase";
import { displayText, type TimelineRow } from "../delivery/timeline";
import type { RolloutInfo, RolloutState } from "./types";

/**
 * States that mean the rollout reached production.
 *
 * A closed set, deliberately. Treating everything that is not a known failure
 * as live would give the first state Firebase invents a green badge, and the
 * badge this panel exists to show is "this commit is serving traffic".
 */
const LIVE: ReadonlySet<string> = new Set(["succeeded"]);

/**
 * States that mean the rollout finished and did not reach production.
 *
 * Also closed, and note what is NOT here: `unrecognised` and `unspecified`.
 * Neither is a failure — they are states we cannot judge — so counting them as
 * failures would inflate a change-failure rate with our own ignorance.
 */
const FAILED: ReadonlySet<string> = new Set(["failed", "cancelled"]);

/** States that are still moving. Neither a success nor a failure yet. */
const IN_FLIGHT: ReadonlySet<string> = new Set([
  "queued",
  "pending_build",
  "progressing",
  "paused",
]);

export function isLive(state: RolloutState): boolean {
  return LIVE.has(state.kind);
}

export function isFailed(state: RolloutState): boolean {
  return FAILED.has(state.kind);
}

export function isInFlight(state: RolloutState): boolean {
  return IN_FLIGHT.has(state.kind);
}

const LABELS: Record<string, string> = {
  unspecified: "Unspecified",
  queued: "Queued",
  pending_build: "Building",
  progressing: "Rolling out",
  paused: "Paused",
  succeeded: "Live",
  failed: "Failed",
  cancelled: "Cancelled",
  skipped: "Skipped",
};

/**
 * What the reader sees for a state.
 *
 * An unrecognised state shows its raw value rather than a guess, so a reader
 * looking at an unfamiliar badge can search for the real string.
 */
export function rolloutStateLabel(state: RolloutState): string {
  if (state.kind === "unrecognised") return `Unknown (${state.raw})`;
  return LABELS[state.kind] ?? "Unknown";
}

/**
 * Colour for a state, with both shades on every verdict.
 *
 * A bare `-400` is tuned for the dark theme and sits near 2:1 against the light
 * theme's near-white card — on exactly the labels a reader opened this panel to
 * check.
 */
export function rolloutStateClass(state: RolloutState): string {
  if (isLive(state)) return "text-green-700 dark:text-green-400";
  if (isFailed(state)) return "text-red-700 dark:text-red-400";
  if (isInFlight(state)) return "text-amber-700 dark:text-amber-400";
  // Unspecified, skipped and unrecognised: no verdict to colour.
  return "text-textMuted";
}

/**
 * The rollout currently serving traffic, if the listing shows one.
 *
 * The API returns rollouts newest-first, so the first live one wins. Returns
 * null when nothing in the listing succeeded — which, on a listing that is
 * `truncated` or carries `walk_incomplete`, means "not in what we can see",
 * never "this backend has never deployed". The caller owns that distinction.
 */
export function currentRollout(rollouts: readonly RolloutInfo[]): RolloutInfo | null {
  return rollouts.find((rollout) => isLive(rollout.state)) ?? null;
}

/**
 * Short SHA for display. Seven characters is git's own default abbreviation.
 *
 * Never used to *identify* a commit — a rollout is always created against the
 * full hash, because an abbreviation is an ambiguous target for something that
 * reaches production.
 */
export function shortSha(hash: string): string {
  return hash.slice(0, 7);
}

/**
 * Why a commit is not a usable rollout target, or null when it is.
 *
 * Mirrors the Rust validator rather than replacing it — the backend refuses the
 * same values, and must, because a UI check is a courtesy and not a boundary.
 * What this adds is the reason: a disabled button with no explanation is the
 * shape people work around by pasting something else.
 *
 * Abbreviations are refused on purpose. This value names which commit reaches
 * production, and a prefix that resolves today can become ambiguous tomorrow.
 */
export function commitShaProblem(value: string): string | null {
  const trimmed = value.trim();
  if (trimmed.length === 0) return "Enter the full commit SHA to deploy.";
  if (!/^[0-9a-fA-F]+$/.test(trimmed)) {
    return "A commit SHA is hexadecimal — this contains other characters.";
  }
  if (trimmed.length !== 40) {
    return `A full 40-character SHA is required; this is ${trimmed.length}. An abbreviation is an ambiguous target for something that reaches production.`;
  }
  return null;
}

/**
 * This rollout's phase in the shared delivery vocabulary.
 *
 * Derived from the predicates above rather than from a second set of state
 * lists, so there is exactly one place that decides what `succeeded` means. A
 * parallel mapping here would be free to drift from `isLive`, and the drift
 * would show up as a deploy that the timeline calls green and the badge beside
 * it calls unknown.
 */
export function rolloutPhase(state: RolloutState): MonitorPhase {
  if (isLive(state)) return "settled_ok";
  if (isFailed(state)) return "settled_bad";
  if (isInFlight(state)) return "in_flight";
  // `unspecified`, `skipped` and `unrecognised`: settled, with no verdict.
  return "unknown";
}

/**
 * A rollout as a timeline row.
 *
 * `create_time` is the start. Unlike a workflow run, App Hosting reports no
 * separate execution start, so queue time is inside the measured span — noted
 * here because the two sources' bars are drawn on the same scale, and a
 * rollout's bar therefore includes a wait that a run's bar excludes.
 */
export function rolloutTimelineRow(rollout: RolloutInfo): TimelineRow {
  return {
    id: rollout.id,
    label: displayText(rollout.id),
    // Subject only, then bounded. The subject is the meaningful line; joining
    // a whole commit body into one row would bury it.
    sublabel: displayText((rollout.commit?.message ?? "").split("\n")[0]),
    phase: rolloutPhase(rollout.state),
    stateLabel: rolloutStateLabel(rollout.state),
    startedAt: rollout.create_time ?? "",
    endedAt: rollout.update_time ?? "",
    commitSha: rollout.commit?.hash ?? "",
    branch: rollout.commit?.branch ?? "",
    // App Hosting does not report a trigger; an empty string is the honest
    // answer and renders as no chip rather than as a guessed one.
    trigger: "",
    url: "",
  };
}

/** Every rollout as a timeline row, newest first as the API returns them. */
export function rolloutTimelineRows(rollouts: readonly RolloutInfo[]): TimelineRow[] {
  return rollouts.map(rolloutTimelineRow);
}
