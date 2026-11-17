/**
 * When an automatic (not clicked) scan may start.
 *
 * A heavy scan (Kingfisher, the dependency audit) used to start the moment its
 * pane mounted. A visit shorter than the scan then paid for work it never
 * looked at, and on every return the same thing happened again. Two rules fix
 * the class, and both live here so each pane asks the same question:
 *
 *  - a still-valid result for the repository means no scan at all
 *    ({@link isScanStale});
 *  - otherwise the scan waits for the pane to stay on screen for a short
 *    settle, and not at all while the document is hidden
 *    ({@link scheduleAutoScan}).
 *
 * A click is not automatic: callers run it directly, with neither rule.
 */
import { whenDocumentShown } from "./foreground";

/** A scan result older than this is refreshed when the pane reopens. */
export const SCAN_FRESH_MS = 5 * 60_000;

/** How long a pane must stay mounted before an automatic scan starts. */
export const AUTO_SCAN_SETTLE_MS = 1_500;

/**
 * Whether a result stamped `scannedAtMs` (epoch ms) is too old, or unstamped,
 * to stand as the answer. A stamp of 0 or less means no scan finished.
 */
export function isScanStale(scannedAtMs: number, nowMs: number): boolean {
  return scannedAtMs <= 0 || nowMs - scannedAtMs >= SCAN_FRESH_MS;
}

/**
 * The scans in flight, by key. Module-level in the pane that uses it, not per
 * instance: a pane that is left and reopened (or rebuilt around the scan)
 * must find the scan it started and listen to it, not start a second one
 * beside it. A scan that finishes is forgotten at once, and a failure is the
 * failure of every caller that joined it.
 */
export interface ScanJoin<T> {
  has(key: string): boolean;
  /** Joins the scan running for `key`, or starts one with `start`. */
  run(key: string, start: () => Promise<T>): Promise<T>;
}

export function createScanJoin<T>(): ScanJoin<T> {
  const running = new Map<string, Promise<T>>();
  return {
    has: (key) => running.has(key),
    run(key, start) {
      const joined = running.get(key);
      if (joined) return joined;
      const started: Promise<T> = start().finally(() => {
        if (running.get(key) === started) running.delete(key);
      });
      running.set(key, started);
      return started;
    },
  };
}

/** Test seam: the harness pages shorten the settle so their other checks stay quick. */
const settle = { ms: AUTO_SCAN_SETTLE_MS };

export function setAutoScanSettleMsForTest(ms: number | null): void {
  settle.ms = ms === null ? AUTO_SCAN_SETTLE_MS : Math.max(0, ms);
}

/**
 * A pane's wait is not restarted by the pane being rebuilt around it. Health
 * renders its secrets section from three branches of one chain, so the
 * dependency scan finishing swaps the instance for a fresh one mid-wait; a
 * wait that began again from zero each time would start the scan a whole
 * settle later, or never while the swaps kept coming. A wait for the same key
 * that begins within this long of the last one ending is the same visit.
 */
export const REMOUNT_GRACE_MS = 250;

const waits = new Map<string, { startedAt: number; active: number; leftAt: number }>();

export interface AutoScanRequest {
  /** Names what is being scanned, e.g. `secrets:/path/to/repo`, so a rebuilt pane resumes its wait. */
  key: string;
  /**
   * Asked again when the settle ends, and again after a hidden document is
   * shown. False means what the scan was for is already answered (a sibling
   * pane finished it, or the user's click did), so nothing starts.
   */
  stillNeeded: () => boolean;
  /** Starts the scan. Never called after {@link cancel}. */
  run: () => void;
}

/**
 * Starts `request.run` once the pane has settled. Returns the cancel: call it
 * when the pane leaves or the repository changes, and a scan that has not
 * started never will. Cancelling after the scan started does nothing; the
 * scan itself decides what leaving means for its own result.
 */
export function scheduleAutoScan(request: AutoScanRequest): () => void {
  const now = Date.now();
  const earlier = waits.get(request.key);
  const resumed = earlier !== undefined && (earlier.active > 0 || now - earlier.leftAt < REMOUNT_GRACE_MS);
  const entry = resumed ? earlier : { startedAt: now, active: 0, leftAt: now };
  entry.active += 1;
  waits.set(request.key, entry);

  let cancelled = false;
  let finished = false;
  const hidden = new AbortController();
  const finish = () => {
    if (finished) return;
    finished = true;
    entry.active -= 1;
    entry.leftAt = Date.now();
  };
  const timer = setTimeout(
    () => {
      void whenDocumentShown(hidden.signal).then(() => {
        if (cancelled) return;
        // Started or answered, either way this wait is over for every pane
        // holding it; a later one is a new visit.
        if (waits.get(request.key) === entry) waits.delete(request.key);
        finish();
        if (!request.stillNeeded()) return;
        request.run();
      });
    },
    Math.max(0, settle.ms - (now - entry.startedAt)),
  );
  return () => {
    if (cancelled) return;
    cancelled = true;
    clearTimeout(timer);
    // Stops a wait for a hidden document from leaving its listener behind.
    hidden.abort();
    finish();
    // Every wait for this key has been cancelled: keep the entry only for the
    // grace in which a rebuilt pane may resume it, then forget it.
    if (entry.active === 0) {
      setTimeout(() => {
        if (entry.active === 0 && waits.get(request.key) === entry) waits.delete(request.key);
      }, REMOUNT_GRACE_MS);
    }
  };
}

/** Test seam: how many keys still hold a wait record. */
export function pendingWaitCountForTest(): number {
  return waits.size;
}

/**
 * A scan that failed is not retried by an automatic start until this long
 * after it failed. It is the freshness window: a result is trusted for
 * {@link SCAN_FRESH_MS}, and a failure that was just seen is trusted not to
 * have fixed itself in the same time. A click is never held back.
 */
export const AUTO_SCAN_RETRY_MS = SCAN_FRESH_MS;

export interface AutoScanBackoff {
  /** What the failed scan said. */
  reason: string;
  failedAtMs: number;
  /** When an automatic scan may try again. */
  retryAtMs: number;
}

const MAX_TRACKED_FAILURES = 64;
const failures = new Map<string, AutoScanBackoff>();

/** Remembers that the scan named by `key` failed at `nowMs`. */
export function noteScanFailure(key: string, reason: string, nowMs: number = Date.now()): void {
  failures.delete(key);
  failures.set(key, { reason, failedAtMs: nowMs, retryAtMs: nowMs + AUTO_SCAN_RETRY_MS });
  while (failures.size > MAX_TRACKED_FAILURES) {
    const oldest = failures.keys().next();
    if (oldest.done) break;
    failures.delete(oldest.value);
  }
}

/** A scan that finished well, or was asked for by a click and finished. */
export function clearScanFailure(key: string): void {
  failures.delete(key);
}

/**
 * The failure holding automatic scans off for `key`, or null when there is none
 * or its interval has passed (an expired one is forgotten).
 */
export function activeScanBackoff(key: string, nowMs: number = Date.now()): AutoScanBackoff | null {
  const held = failures.get(key);
  if (!held) return null;
  if (nowMs >= held.retryAtMs) {
    failures.delete(key);
    return null;
  }
  return held;
}

/** The sentence a pane prints beside a failure it is holding automatic scans off for. */
export function describeBackoff(held: AutoScanBackoff): string {
  const at = new Date(held.retryAtMs).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  return `Automatic scanning is paused after this failure and tries again from ${at}. Rescan tries now.`;
}

/** Test seam: forget every recorded failure. */
export function resetScanFailuresForTest(): void {
  failures.clear();
}
