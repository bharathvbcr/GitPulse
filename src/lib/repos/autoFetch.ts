/**
 * Opt-in periodic fetch, per repository.
 *
 * Off by default: a fetch talks to a remote, can wake a credential helper, and
 * holds the repository's write lock while it runs, so nothing fetches unless
 * someone turned it on for that repository.
 *
 * Three things keep it inside the budget every other subprocess shares
 * (the 2026-10 spawn-budget starvation is what happens when they do not):
 *
 * - The scheduler ticks on `createVisibleInterval`, so a hidden window runs
 *   nothing and a lagging event loop stretches the period.
 * - At most one repository is fetched per tick, the active tab first, so
 *   enabling it on twenty tabs never fans out into twenty fetches at once.
 * - A background tab waits `BACKGROUND_MULTIPLIER` times longer than the tab
 *   in front, and the backend runs every auto-fetch as background work the
 *   spawn gate sheds under load (`GitWriter::auto_fetch`), skipping a
 *   repository another git command holds rather than queueing behind it.
 */

export const AUTO_FETCH_STORAGE_KEY = "gitpulse.autoFetch.v1";
export const DEFAULT_AUTO_FETCH_MINUTES = 10;
export const MIN_AUTO_FETCH_MINUTES = 5;
export const MAX_AUTO_FETCH_MINUTES = 240;
/** A background tab is fetched this many times less often than the active one. */
export const BACKGROUND_MULTIPLIER = 4;
/** How often the scheduler wakes to look for a due repository. */
export const AUTO_FETCH_TICK_MS = 60_000;
/** Bound on remembered repositories, so the stored map cannot grow without end. */
export const MAX_AUTO_FETCH_REPOS = 256;

export interface AutoFetchSetting {
  enabled: boolean;
  minutes: number;
}

export type AutoFetchPrefs = Record<string, AutoFetchSetting>;

/** Mirrors the Rust `AutoFetchOutcome` (`#[serde(tag = "status")]`). */
export type AutoFetchOutcome = { status: "fetched" } | { status: "skipped"; reason: string };

export function parseAutoFetchOutcome(value: unknown): AutoFetchOutcome {
  if (value && typeof value === "object") {
    const record = value as Record<string, unknown>;
    if (record.status === "fetched") return { status: "fetched" };
    if (record.status === "skipped" && typeof record.reason === "string") {
      return { status: "skipped", reason: record.reason };
    }
  }
  throw new Error("Invalid auto-fetch outcome");
}

export function clampMinutes(minutes: unknown): number {
  const value = typeof minutes === "number" && Number.isFinite(minutes) ? Math.round(minutes) : DEFAULT_AUTO_FETCH_MINUTES;
  return Math.min(MAX_AUTO_FETCH_MINUTES, Math.max(MIN_AUTO_FETCH_MINUTES, value));
}

export interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

function browserStorage(): StorageLike | null {
  try {
    return typeof window !== "undefined" && window.localStorage ? window.localStorage : null;
  } catch {
    return null;
  }
}

/** Reads the stored preferences. Anything unreadable is "off everywhere". */
export function loadAutoFetchPrefs(storage: StorageLike | null = browserStorage()): AutoFetchPrefs {
  if (!storage) return {};
  try {
    const raw = storage.getItem(AUTO_FETCH_STORAGE_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    const prefs: AutoFetchPrefs = {};
    for (const [path, setting] of Object.entries(parsed as Record<string, unknown>).slice(0, MAX_AUTO_FETCH_REPOS)) {
      if (!path || !setting || typeof setting !== "object") continue;
      const record = setting as Record<string, unknown>;
      if (record.enabled !== true) continue;
      prefs[path] = { enabled: true, minutes: clampMinutes(record.minutes) };
    }
    return prefs;
  } catch {
    return {};
  }
}

/**
 * Returns the preferences with `path` set. Only enabled repositories are
 * kept: "off" is the default and needs no entry.
 */
export function withAutoFetchSetting(prefs: AutoFetchPrefs, path: string, setting: AutoFetchSetting): AutoFetchPrefs {
  const next: AutoFetchPrefs = { ...prefs };
  delete next[path];
  if (setting.enabled) {
    if (Object.keys(next).length >= MAX_AUTO_FETCH_REPOS) {
      throw new Error(`Auto-fetch can be enabled for at most ${MAX_AUTO_FETCH_REPOS} repositories.`);
    }
    next[path] = { enabled: true, minutes: clampMinutes(setting.minutes) };
  }
  return next;
}

export function saveAutoFetchPrefs(prefs: AutoFetchPrefs, storage: StorageLike | null = browserStorage()): void {
  storage?.setItem(AUTO_FETCH_STORAGE_KEY, JSON.stringify(prefs));
}

export interface AutoFetchCandidate {
  path: string;
  active: boolean;
  /** Why this repository must not be fetched now (parked, loading, …), or null. */
  skip: string | null;
}

/**
 * The one repository to fetch on this tick, or null. Pure: the scheduler
 * supplies the clock, the open tabs and when each was last attempted.
 */
export function pickDueRepository(
  candidates: readonly AutoFetchCandidate[],
  prefs: AutoFetchPrefs,
  lastAttempt: ReadonlyMap<string, number>,
  now: number,
): string | null {
  let best: { path: string; active: boolean; overdue: number } | null = null;
  for (const candidate of candidates) {
    const setting = prefs[candidate.path];
    if (!setting?.enabled || candidate.skip) continue;
    const period = clampMinutes(setting.minutes) * 60_000 * (candidate.active ? 1 : BACKGROUND_MULTIPLIER);
    const last = lastAttempt.get(candidate.path);
    // A repository never attempted is due one full period after it was first
    // seen, not immediately: opening a tab must not fire a network call.
    if (last === undefined) continue;
    const overdue = now - last - period;
    if (overdue < 0) continue;
    if (!best || (candidate.active && !best.active) || (candidate.active === best.active && overdue > best.overdue)) {
      best = { path: candidate.path, active: candidate.active, overdue };
    }
  }
  return best?.path ?? null;
}

export interface AutoFetchDeps {
  candidates(): AutoFetchCandidate[];
  prefs(): AutoFetchPrefs;
  fetch(path: string): Promise<AutoFetchOutcome>;
  now(): number;
  /** Called with each failure; a failure never stops the scheduler. */
  onError?(path: string, error: unknown): void;
}

export interface AutoFetchScheduler {
  /** Runs one tick. Exposed for tests and for the interval. */
  tick(): Promise<void>;
  /** When each repository was last attempted (fetched, skipped or failed). */
  lastAttempt: ReadonlyMap<string, number>;
}

export function createAutoFetchScheduler(deps: AutoFetchDeps): AutoFetchScheduler {
  const lastAttempt = new Map<string, number>();
  let running = false;
  return {
    lastAttempt,
    async tick() {
      if (running) return;
      const now = deps.now();
      const prefs = deps.prefs();
      const candidates = deps.candidates();
      const open = new Set(candidates.map((candidate) => candidate.path));
      for (const path of [...lastAttempt.keys()]) {
        if (!open.has(path) || !prefs[path]?.enabled) lastAttempt.delete(path);
      }
      for (const candidate of candidates) {
        if (prefs[candidate.path]?.enabled && !lastAttempt.has(candidate.path)) lastAttempt.set(candidate.path, now);
      }
      const due = pickDueRepository(candidates, prefs, lastAttempt, now);
      if (!due) return;
      running = true;
      lastAttempt.set(due, now);
      try {
        await deps.fetch(due);
      } catch (error) {
        deps.onError?.(due, error);
      } finally {
        running = false;
      }
    },
  };
}
