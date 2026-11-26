/**
 * How long a surface keeps showing its last good answer when the host says it
 * is busy.
 *
 * The host refuses reads with the code "busy" once its admission queue is full
 * (`workbench.rs`: 8 in flight, 32 queued, 3 s wait). One task change fans out
 * a dozen reads, so under a sustained writer some of them are refused, and a
 * surface that blanked its chips and raised a banner on each refusal flickered
 * for as long as the writer ran. A refusal says the store is loaded, not that
 * the answer on screen is wrong, so the answer stays for a bounded grace.
 *
 * The grace ends, and the surface clears what it showed and says why as it
 * always did, once the refusals are no longer a blip: at least
 * `BUSY_GRACE_FAILURES` in a row AND at least `BUSY_GRACE_MS` since the first
 * of them. Both are required: two refusals a second apart are one burst, and a
 * single refusal is not evidence of anything. The clock starts at the first
 * refusal, not at the last good read, because on an idle board that read may
 * be a minute old, and counting from it would end the grace at the second
 * refusal of every burst. Any other failure ends it at once, and a surface
 * with no good answer has nothing to hold.
 */
import { WorkbenchError } from "./client";

/** Consecutive busy refusals after which the last good answer may stop being shown. */
export const BUSY_GRACE_FAILURES = 2;
/** Milliseconds since the first refusal of a streak after which the last good answer may stop being shown. */
export const BUSY_GRACE_MS = 5_000;

export function isBusy(cause: unknown): boolean {
  return cause instanceof WorkbenchError && cause.code === "busy";
}

export interface BusyGrace {
  /** A read landed: the answer on screen is current. */
  succeeded(): void;
  /** A read failed. True when the last good answer may stay on screen; false means clear it and say why. */
  failed(cause: unknown): boolean;
  /** The surface shows something else now (another scope, an explicit failure): nothing is held. */
  forget(): void;
  /** When the answer on screen was read, while it is being held past a refusal; null otherwise. */
  held(): number | null;
}

export function createBusyGrace(clock: () => number = Date.now): BusyGrace {
  let goodAt: number | null = null;
  let failures = 0;
  let streakAt = 0;
  let holding = false;
  return {
    succeeded() { goodAt = clock(); failures = 0; holding = false; },
    failed(cause) {
      if (goodAt !== null && isBusy(cause)) {
        if (failures === 0) streakAt = clock();
        failures += 1;
        if (failures < BUSY_GRACE_FAILURES || clock() - streakAt < BUSY_GRACE_MS) { holding = true; return true; }
      }
      goodAt = null; failures = 0; holding = false;
      return false;
    },
    forget() { goodAt = null; failures = 0; holding = false; },
    held: () => (holding ? goodAt : null),
  };
}
