import type { CliDeferral, DevmapCliStatus, LastGoodIndex } from "./types";

/**
 * True when `devmap status` was deferred under load: the spawn gate declined to
 * start the probe, so nothing about the index is known right now. That is not
 * a missing tool and not a broken index, and the panel must not say either.
 */
export function isStatusDeferred(
  cli: DevmapCliStatus | null | undefined,
): cli is DevmapCliStatus & { deferral: CliDeferral } {
  return cli != null && !cli.available && cli.deferral != null;
}

/** "42 s", "5 min", "3 h", "2 d": how old a reading is, rounded down. */
export function formatAge(ageMs: number): string {
  const seconds = Math.max(0, Math.floor((Number.isFinite(ageMs) ? ageMs : 0) / 1_000));
  if (seconds < 60) return `${seconds} s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  if (hours < 48) return `${hours} h`;
  return `${Math.floor(hours / 24)} d`;
}

/**
 * What the last good reading says about the index, in words that never claim
 * it is current: its generation, how old the reading is, and that it was not
 * re-checked. `fresh` is shown only as "already stale" when the reading said
 * so; true never reaches the screen, because the check that would establish it
 * is the one that was deferred.
 */
export function lastGoodText(last: LastGoodIndex): string {
  const generation = last.generation_id == null ? "unknown" : String(last.generation_id);
  const stale = last.fresh === false ? "; it was already stale" : "";
  return `Showing the last good index (generation ${generation}, read ${formatAge(last.age_ms)} ago), not re-checked${stale}.`;
}

/** The strip's text for a deferred status. */
export function deferredStripText(deferral: CliDeferral): { headline: string; detail: string } {
  return {
    headline: "Index check deferred under load",
    detail: deferral.last_good
      ? lastGoodText(deferral.last_good)
      : "No earlier reading of this index in this session; the check retries on its own.",
  };
}
