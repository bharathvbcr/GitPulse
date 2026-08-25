/**
 * Per-repository storage-usage history, persisted to localStorage.
 *
 * Every completed scan appends one snapshot per repository so the Storage
 * view can show growth over time ("+180 MB since yesterday") without the
 * Rust side carrying any state. Snapshots coalesce within a short window
 * (rescans inside 15 minutes replace the newest entry instead of stacking),
 * each repository's history is a hard-capped ring, and everything read back
 * from storage passes a shape-validating sanitizer: a corrupted or
 * hand-edited entry degrades to "no history", never a crash or NaN bars.
 */

import type { StorageLike } from "../repos/persist";
import { memoryStorage } from "../repos/persist";

export const STORAGE_KEY_STORAGE_HISTORY = "gitpulse_storage_history_v1";

/** One point-in-time usage sample for a repository. */
export interface StorageSnapshot {
  /** Epoch milliseconds. */
  t: number;
  grand: number;
  git: number;
  build: number;
  cache: number;
}

export type StorageHistoryMap = Record<string, StorageSnapshot[]>;

export const MAX_HISTORY_PER_REPO = 120;
/** Repositories we keep history for; oldest-inserted keys are dropped. */
export const MAX_REPOS_WITH_HISTORY = 200;
/** Rescans within this window replace the newest snapshot (coalescing). */
export const COALESCE_WINDOW_MS = 15 * 60 * 1000;

function isFiniteNonNegative(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0;
}

function sanitizeSnapshot(raw: unknown): StorageSnapshot | null {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  const record = raw as Record<string, unknown>;
  if (!isFiniteNonNegative(record.t) || record.t <= 0) return null;
  if (!isFiniteNonNegative(record.grand)) return null;
  if (!isFiniteNonNegative(record.git)) return null;
  if (!isFiniteNonNegative(record.build)) return null;
  if (!isFiniteNonNegative(record.cache)) return null;
  return {
    t: record.t,
    grand: record.grand,
    git: record.git,
    build: record.build,
    cache: record.cache,
  };
}

function sanitizeSeries(raw: unknown): StorageSnapshot[] {
  if (!Array.isArray(raw)) return [];
  const out: StorageSnapshot[] = [];
  const seen = new Set<number>();
  for (const item of raw) {
    const snap = sanitizeSnapshot(item);
    if (!snap || seen.has(snap.t)) continue;
    seen.add(snap.t);
    out.push(snap);
  }
  // Chronological order, newest last. The ring cap keeps the NEWEST
  // entries: history must always reflect recent scans, and an early
  // baseline ages out naturally.
  out.sort((a, b) => a.t - b.t);
  if (out.length > MAX_HISTORY_PER_REPO) {
    out.splice(0, out.length - MAX_HISTORY_PER_REPO);
  }
  return out;
}

function sanitizeMap(raw: unknown): StorageHistoryMap {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return {};
  const out: StorageHistoryMap = {};
  const entries = Object.entries(raw as Record<string, unknown>).filter(
    ([key]) => key.length > 0,
  );
  // Keep the newest-inserted repositories beyond the cap, mirroring the
  // eviction order recordSnapshot uses.
  const keepFrom = Math.max(0, entries.length - MAX_REPOS_WITH_HISTORY);
  for (const [key, value] of entries.slice(keepFrom)) {
    const series = sanitizeSeries(value);
    if (series.length === 0) continue;
    out[key] = series;
  }
  return out;
}

/** Reads and validates persisted history. Missing/corrupt → empty map. */
export function loadHistory(storage: StorageLike | null): StorageHistoryMap {
  if (!storage) return {};
  const raw = storage.getItem(STORAGE_KEY_STORAGE_HISTORY);
  if (!raw) return {};
  try {
    return sanitizeMap(JSON.parse(raw));
  } catch {
    return {};
  }
}

/** Persists history; quota/private-mode failures fail closed (kept in RAM). */
export function saveHistory(storage: StorageLike | null, map: StorageHistoryMap): void {
  if (!storage) return;
  try {
    storage.setItem(STORAGE_KEY_STORAGE_HISTORY, JSON.stringify(map));
  } catch {
    /* quota / private mode — in-memory state still serves this session */
  }
}

/**
 * Appends (or coalesces) a snapshot for `repoKey` immutably: rescans inside
 * [`COALESCE_WINDOW_MS`] REPLACE the newest entry — rapid rescan clicking
 * must not fabricate a steep-growth staircase.
 */
export function recordSnapshot(
  map: StorageHistoryMap,
  repoKey: string,
  snapshot: StorageSnapshot,
): StorageHistoryMap {
  if (!repoKey) return map;
  const existing = sanitizeSeries(map[repoKey]);
  const next: StorageSnapshot[] = existing.slice();

  const last = next[next.length - 1];
  if (last && snapshot.t - last.t < COALESCE_WINDOW_MS && snapshot.t >= last.t) {
    next[next.length - 1] = snapshot;
  } else {
    next.push(snapshot);
  }

  next.sort((a, b) => a.t - b.t);
  if (next.length > MAX_HISTORY_PER_REPO) {
    next.splice(0, next.length - MAX_HISTORY_PER_REPO);
  }

  // Rebuild the key map with FIFO eviction: when the cap is saturated,
  // exactly the OLDEST-inserted other repository makes room for the new
  // one — never a middle entry, never two.
  const others = Object.keys(map).filter((key) => key !== repoKey);
  const keepFrom = Math.max(0, others.length - (MAX_REPOS_WITH_HISTORY - 1));
  const out: StorageHistoryMap = {};
  for (const key of others.slice(keepFrom)) {
    out[key] = map[key];
  }
  out[repoKey] = next;
  return out;
}

/** Chronological series for one repo (oldest first). Never returns null. */
export function historyFor(map: StorageHistoryMap, repoKey: string): StorageSnapshot[] {
  return sanitizeSeries(map[repoKey]);
}

export interface HistoryDelta {
  bytes: number;
  sinceMs: number;
}

/**
 * Change between the two most recent snapshots. Null until there are two.
 */
export function deltaVsPrevious(series: StorageSnapshot[]): HistoryDelta | null {
  if (series.length < 2) return null;
  const latest = series[series.length - 1];
  const previous = series[series.length - 2];
  return { bytes: latest.grand - previous.grand, sinceMs: latest.t - previous.t };
}

/**
 * Change from the oldest snapshot within `windowMs` of the newest — the
 * "this week" view. Null when fewer than two points fall inside.
 */
export function deltaOver(
  series: StorageSnapshot[],
  windowMs: number,
  nowMs: number = Date.now(),
): HistoryDelta | null {
  if (series.length < 2 || !(windowMs > 0)) return null;
  const latest = series[series.length - 1];
  if (nowMs - latest.t > windowMs) return null;
  const cutoff = latest.t - windowMs;
  let oldestInWindow: StorageSnapshot | null = null;
  for (const snap of series) {
    if (snap.t <= latest.t && snap.t >= cutoff) {
      if (!oldestInWindow || snap.t < oldestInWindow.t) oldestInWindow = snap;
    }
  }
  if (!oldestInWindow || oldestInWindow === latest) return null;
  return { bytes: latest.grand - oldestInWindow.grand, sinceMs: latest.t - oldestInWindow.t };
}

/** Drops every trace of one repository's history (immutably). */
export function clearRepoHistory(map: StorageHistoryMap, repoKey: string): StorageHistoryMap {
  if (!(repoKey in map)) return map;
  const out: StorageHistoryMap = {};
  for (const [key, value] of Object.entries(map)) {
    if (key !== repoKey) out[key] = value;
  }
  return out;
}

/** Test convenience: an isolated in-memory store preloaded with nothing. */
export function memoryHistoryStorage(): StorageLike {
  return memoryStorage();
}
