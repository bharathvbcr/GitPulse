/**
 * Pull-request velocity: how long PRs have been open, and how long they wait
 * for a first review.
 *
 * Backlog C2. Time-open and time-to-first-review are the numbers that show
 * where a pipeline stalls; state and CI status do not.
 *
 * Every function is pure and takes `nowMs`, so the panel renders deterministically
 * and the arithmetic is testable without a clock.
 */

/** The timing fields the backend adds to each pull request. */
export interface PullRequestTiming {
  /** RFC 3339 from gh, or "" when gh did not supply it. */
  readonly created_at: string;
  /** RFC 3339 of the earliest submitted review, or "" when unreviewed. */
  readonly first_review_at: string;
  readonly is_draft?: boolean;
}

const MS_PER_HOUR = 3_600_000;

/**
 * Parse an RFC 3339 timestamp to epoch ms, or null when it is absent or
 * unusable. Returning null rather than 0 keeps "no timestamp" distinct from
 * "the epoch", which would otherwise render as a 56-year-old pull request.
 */
export function parseTimestamp(value: string | null | undefined): number | null {
  if (typeof value !== "string" || value.trim() === "") return null;
  const ms = Date.parse(value);
  return Number.isFinite(ms) ? ms : null;
}

/** Hours a pull request has been open, or null when its creation is unknown. */
export function openHours(pr: PullRequestTiming, nowMs: number): number | null {
  const created = parseTimestamp(pr.created_at);
  if (created === null || !Number.isFinite(nowMs)) return null;
  // A creation timestamp in the future (clock skew) clamps to zero rather than
  // reporting a negative age.
  return Math.max(0, (nowMs - created) / MS_PER_HOUR);
}

/**
 * Hours between opening and the first submitted review, or null when the PR
 * has not been reviewed. Null is the signal the panel actually cares about:
 * it means "still waiting", not "reviewed instantly".
 */
export function hoursToFirstReview(pr: PullRequestTiming): number | null {
  const created = parseTimestamp(pr.created_at);
  const reviewed = parseTimestamp(pr.first_review_at);
  if (created === null || reviewed === null) return null;
  return Math.max(0, (reviewed - created) / MS_PER_HOUR);
}

/** True when a non-draft PR is open and nobody has reviewed it. */
export function isAwaitingFirstReview(pr: PullRequestTiming): boolean {
  if (pr.is_draft) return false;
  return parseTimestamp(pr.first_review_at) === null;
}

/** Compact age label: "3h", "2d", "5w". */
export function formatAge(hours: number | null): string {
  if (hours === null) return "—";
  if (hours < 1) return "<1h";
  if (hours < 24) return `${Math.floor(hours)}h`;
  const days = hours / 24;
  if (days < 14) return `${Math.floor(days)}d`;
  return `${Math.floor(days / 7)}w`;
}

export interface VelocitySummary {
  /** Non-draft open PRs the summary was computed from. */
  readonly considered: number;
  /** Median hours open, or null when nothing could be measured. */
  readonly medianOpenHours: number | null;
  /** Median hours to first review across PRs that have one. */
  readonly medianFirstReviewHours: number | null;
  /** Non-draft PRs with no review yet. */
  readonly awaitingReview: number;
  /** Longest-open non-draft PR, in hours. */
  readonly oldestOpenHours: number | null;
}

function median(values: number[]): number | null {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 1 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
}

/**
 * Aggregate open pull requests.
 *
 * Drafts are excluded: they are not waiting on anyone, and counting them makes
 * a queue look worse than it is. The median is used rather than the mean so one
 * PR left open for a year does not swamp the number.
 */
export function summarizeVelocity(
  pullRequests: readonly PullRequestTiming[],
  nowMs: number,
): VelocitySummary {
  const active = pullRequests.filter((pr) => !pr.is_draft);
  const ages = active
    .map((pr) => openHours(pr, nowMs))
    .filter((hours): hours is number => hours !== null);
  const reviewWaits = active
    .map((pr) => hoursToFirstReview(pr))
    .filter((hours): hours is number => hours !== null);

  return {
    considered: active.length,
    medianOpenHours: median(ages),
    medianFirstReviewHours: median(reviewWaits),
    awaitingReview: active.filter(isAwaitingFirstReview).length,
    oldestOpenHours: ages.length > 0 ? Math.max(...ages) : null,
  };
}
