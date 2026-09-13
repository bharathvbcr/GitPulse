import type { StorageLike } from "../../repos/persist";

/**
 * Hygiene settings resolve in two layers: one host-wide default that every
 * repository inherits, and an optional per-repository override.
 *
 * The previous model stored one flat record per repository, which made two
 * separate mistakes. Retention had no default, so every newly opened
 * repository silently restarted at 30 days and a house rule had to be retyped
 * per repository. And the shared-cache review — which measures caches owned by
 * the host, not by any repository — was stored per repository too, so opting
 * in from five repositories scheduled five weekly scans of the same caches and
 * opting in from one applied nowhere else.
 *
 * Scope is now the thing that decides where a setting lives: a setting about
 * host-wide caches is stored once for the host, and a setting a repository may
 * legitimately differ on is a default plus an explicit override.
 *
 * This layer governs the *manual, previewed* cleanup in Insights → Storage.
 * The scheduled global cleaner keeps its own retention in the backend policy
 * file, because the headless worker that reads it runs with no browser and
 * therefore cannot read this storage. The two are deliberately separate
 * mechanisms, and each control names the scope it governs.
 */

/** Retention presets the UI offers. The bounds below are what it validates. */
export const RETENTION_CHOICES = [7, 14, 30, 90] as const;

/** Retention every repository inherits until it overrides it. */
export const DEFAULT_RETENTION_DAYS = 30;

/**
 * Bounds shared with the native contract's unscheduled retention
 * (`devmap_query::hygiene::validate_retention(days, false)`). Validating the
 * range rather than the preset list keeps a value the backend would accept
 * from being silently discarded on the way back in.
 */
export const MIN_RETENTION_DAYS = 1;
export const MAX_RETENTION_DAYS = 3650;

/** Host-wide hygiene defaults. One record, shared by every repository. */
export interface HygieneDefaults {
  /** Retention a repository uses unless it carries an override. */
  retentionDays: number;
  /**
   * Whether the shared-cache review runs while a Storage page is open.
   *
   * Host-wide because the caches it measures are host-wide: `cmd_cache_inventory`
   * takes no repository argument and reports the Cargo registry, `GOCACHE`, the
   * npm cache and their peers.
   */
  reviewSharedCaches: boolean;
  /**
   * When those host-wide caches were last measured, as epoch milliseconds.
   *
   * One stamp for the host, so the review happens once per week rather than
   * once per week per opted-in repository.
   */
  lastSharedReview: number;
}

/** One repository's departure from the host-wide defaults. */
export interface RepoHygieneOverride {
  /** `null` inherits {@link HygieneDefaults.retentionDays}. */
  retentionDays: number | null;
}

/** A resolved retention, carrying which layer decided it. */
export interface ResolvedRetention {
  days: number;
  source: "default" | "repository";
}

export const DEFAULT_HYGIENE_DEFAULTS: HygieneDefaults = {
  retentionDays: DEFAULT_RETENTION_DAYS,
  reviewSharedCaches: false,
  lastSharedReview: 0,
};

export const INHERITED_OVERRIDE: RepoHygieneOverride = { retentionDays: null };

const DEFAULTS_KEY = "gitpulse:hygiene:defaults:v2";
const REPO_PREFIX = "gitpulse:hygiene:repo:v2:";
/** The flat per-repository record this model replaces. */
const LEGACY_PREFIX = "gitpulse:hygiene:v1:";

function parse(storage: StorageLike | null, key: string): Record<string, unknown> | null {
  try {
    const raw: unknown = JSON.parse(storage?.getItem(key) ?? "null");
    return raw && typeof raw === "object" && !Array.isArray(raw)
      ? (raw as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

function write(storage: StorageLike | null, key: string, value: unknown): boolean {
  if (!storage) return false;
  try {
    storage.setItem(key, JSON.stringify(value));
    return true;
  } catch {
    return false;
  }
}

/** A retention a repository or the host may hold, or `null` if unusable. */
function readRetention(value: unknown): number | null {
  return typeof value === "number" &&
    Number.isInteger(value) &&
    value >= MIN_RETENTION_DAYS &&
    value <= MAX_RETENTION_DAYS
    ? value
    : null;
}

function readStamp(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : 0;
}

/** Host-wide defaults, falling back to the shipped defaults field by field. */
export function readDefaults(storage: StorageLike | null): HygieneDefaults {
  const raw = parse(storage, DEFAULTS_KEY);
  if (!raw) return { ...DEFAULT_HYGIENE_DEFAULTS };
  return {
    retentionDays: readRetention(raw.retentionDays) ?? DEFAULT_RETENTION_DAYS,
    reviewSharedCaches: raw.reviewSharedCaches === true,
    lastSharedReview: readStamp(raw.lastSharedReview),
  };
}

export function saveDefaults(storage: StorageLike | null, defaults: HygieneDefaults): boolean {
  return write(storage, DEFAULTS_KEY, defaults);
}

/**
 * One repository's override.
 *
 * A record that exists but carries no usable retention inherits, rather than
 * falling back to a second hardcoded number: an unreadable override is not
 * evidence that this repository wanted 30 days.
 */
export function readOverride(storage: StorageLike | null, repo: string): RepoHygieneOverride {
  const raw = parse(storage, REPO_PREFIX + repo);
  return raw ? { retentionDays: readRetention(raw.retentionDays) } : { ...INHERITED_OVERRIDE };
}

export function saveOverride(
  storage: StorageLike | null,
  repo: string,
  override: RepoHygieneOverride,
): boolean {
  return write(storage, REPO_PREFIX + repo, override);
}

export function resolveRetention(
  defaults: HygieneDefaults,
  override: RepoHygieneOverride,
): ResolvedRetention {
  return override.retentionDays === null
    ? { days: defaults.retentionDays, source: "default" }
    : { days: override.retentionDays, source: "repository" };
}

/**
 * Load both layers for one repository, migrating that repository's legacy
 * record on the way through.
 *
 * Migration is folded into the load rather than offered as a separate call
 * because `StorageLike` cannot enumerate keys: there is no pass that could
 * find every legacy record up front, so each one is adopted when its
 * repository is next opened. The legacy key is removed as it is adopted, which
 * makes the promotion one-shot — otherwise turning the host-wide review off
 * and then opening another repository whose stale record still said `true`
 * would silently switch it back on.
 */
export function loadHygieneSettings(
  storage: StorageLike | null,
  repo: string,
): { defaults: HygieneDefaults; override: RepoHygieneOverride } {
  const legacy = parse(storage, LEGACY_PREFIX + repo);
  let defaults = readDefaults(storage);
  if (!legacy) return { defaults, override: readOverride(storage, repo) };

  // A legacy retention is only an override when it departs from the default
  // the old model hardcoded; a repository left at 30 days never chose 30.
  const retention = readRetention(legacy.retentionDays);
  const override: RepoHygieneOverride = {
    retentionDays: retention === null || retention === DEFAULT_RETENTION_DAYS ? null : retention,
  };

  if (legacy.weeklyReview === true) {
    defaults = {
      ...defaults,
      reviewSharedCaches: true,
      // Keep the most recent measurement of the shared caches, so adopting a
      // record does not force an immediate rescan of caches just reviewed.
      lastSharedReview: Math.max(defaults.lastSharedReview, readStamp(legacy.lastReview)),
    };
    saveDefaults(storage, defaults);
  }

  saveOverride(storage, repo, override);
  try {
    storage?.removeItem(LEGACY_PREFIX + repo);
  } catch {
    // A storage that refuses removal would re-adopt this record on the next
    // load. Harmless for retention, which is idempotent, and the review flag
    // is only ever promoted to `true` — never used to switch it back off.
  }
  return { defaults, override };
}

/**
 * Whether the host-wide shared caches are due for review.
 *
 * A stamp in the future means the clock moved backwards, which is treated as
 * due rather than as a week of silence.
 */
export function reviewDue(defaults: HygieneDefaults, now: number): boolean {
  return (
    defaults.reviewSharedCaches &&
    (defaults.lastSharedReview > now || now - defaults.lastSharedReview >= 7 * 86_400_000)
  );
}

/**
 * Gitignore-literal escape. Backslashes are doubled first so a crafted `\*`
 * cannot undo a later meta escape (CodeQL `js/incomplete-sanitization`).
 */
export function escapeGitignoreLiteral(value: string): string {
  return value.replace(/\\/g, "\\\\").replace(/[!*?\[\]# ]/g, "\\$&");
}

/** One anchored literal directory rule, never a broad glob or an index edit. */
export function ignoreRule(path: string): string | null {
  if (!path || path.length > 4096 || /[\x00-\x1f\x7f\\]/.test(path) || path.split("/").some(p => !p || p === "." || p === ".." || p === ".git")) return null;
  return "/" + escapeGitignoreLiteral(path) + "/";
}
