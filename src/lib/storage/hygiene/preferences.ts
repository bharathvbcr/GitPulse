import type { StorageLike } from "../../repos/persist";

export interface HygienePreferences {
  retentionDays: number;
  weeklyReview: boolean;
  lastReview: number;
}

export const DEFAULT_PREFERENCES: HygienePreferences = { retentionDays: 30, weeklyReview: false, lastReview: 0 };
const PREFIX = "gitpulse:hygiene:v1:";

export function readPreferences(storage: StorageLike | null, repo: string): HygienePreferences {
  try {
    const raw: unknown = JSON.parse(storage?.getItem(PREFIX + repo) ?? "null");
    if (!raw || typeof raw !== "object") return { ...DEFAULT_PREFERENCES };
    const retentionDays = "retentionDays" in raw && typeof raw.retentionDays === "number" && Number.isInteger(raw.retentionDays) && raw.retentionDays >= 1 && raw.retentionDays <= 3650 ? raw.retentionDays : 30;
    const weeklyReview = "weeklyReview" in raw && raw.weeklyReview === true;
    const lastReview = "lastReview" in raw && typeof raw.lastReview === "number" && Number.isFinite(raw.lastReview) && raw.lastReview >= 0 ? raw.lastReview : 0;
    return { retentionDays, weeklyReview, lastReview };
  } catch { return { ...DEFAULT_PREFERENCES }; }
}

export function savePreferences(storage: StorageLike | null, repo: string, preferences: HygienePreferences): boolean {
  if (!storage) return false;
  try { storage.setItem(PREFIX + repo, JSON.stringify(preferences)); return true; } catch { return false; }
}

export function reviewDue(preferences: HygienePreferences, now: number): boolean {
  return preferences.weeklyReview && (preferences.lastReview > now || now - preferences.lastReview >= 7 * 86_400_000);
}

/** One anchored literal directory rule, never a broad glob or an index edit. */
export function ignoreRule(path: string): string | null {
  if (!path || path.length > 4096 || /[\x00-\x1f\x7f\\]/.test(path) || path.split("/").some(p => !p || p === "." || p === ".." || p === ".git")) return null;
  return "/" + path.replace(/[!*?\[\]# ]/g, "\\$&") + "/";
}
