/**
 * Linear-style Task Decomposition and Subtask Rollup Helpers.
 *
 * Provides utilities for splitting tasks into subtasks with parent-child linkage,
 * computing hierarchical progress rollups, and formatting subtask completion metrics.
 */

export interface SubtaskDraft {
  title: string;
  description?: string;
  status?: "inbox" | "backlog" | "ready" | "in_progress" | "review" | "done";
  priority?: number;
  assignee?: string;
}

export interface DecomposedTaskPlan {
  parentTitle: string;
  parentKey: string;
  subtasks: Array<{
    key: string;
    title: string;
    description: string;
    status: string;
    parentKey: string;
  }>;
}

export interface SubtaskRollupMetrics {
  total: number;
  completed: number;
  inProgress: number;
  pending: number;
  percentComplete: number;
  isAllComplete: boolean;
}

/**
 * Decomposes a task title and list of subtask items into structured child tasks
 * linked by parentKey.
 */
export function decomposeTask(
  parentKey: string,
  parentTitle: string,
  subtasks: Array<string | SubtaskDraft>
): DecomposedTaskPlan {
  const normalizedSubtasks = subtasks.map((item, index) => {
    const isString = typeof item === "string";
    const title = isString ? item.trim() : item.title.trim();
    const description = isString ? "" : item.description ?? "";
    const status = isString ? "ready" : item.status ?? "ready";
    const key = `${parentKey}-sub-${index + 1}`;

    return {
      key,
      title,
      description,
      status,
      parentKey,
    };
  });

  return {
    parentKey,
    parentTitle,
    subtasks: normalizedSubtasks,
  };
}

/**
 * Computes progress rollup metrics across a set of child subtask statuses.
 */
export function calculateSubtaskRollup(
  subtaskStatuses: readonly string[]
): SubtaskRollupMetrics {
  const total = subtaskStatuses.length;
  if (total === 0) {
    return {
      total: 0,
      completed: 0,
      inProgress: 0,
      pending: 0,
      percentComplete: 0,
      isAllComplete: false,
    };
  }

  let completed = 0;
  let inProgress = 0;
  let pending = 0;

  for (const status of subtaskStatuses) {
    const s = status.toLowerCase();
    if (s === "done" || s === "completed" || s === "closed") {
      completed++;
    } else if (s === "in_progress" || s === "review" || s === "active") {
      inProgress++;
    } else {
      pending++;
    }
  }

  const percentComplete = Math.round((completed / total) * 100);
  const isAllComplete = completed === total && total > 0;

  return {
    total,
    completed,
    inProgress,
    pending,
    percentComplete,
    isAllComplete,
  };
}

/**
 * Generates human-readable Linear-style progress labels.
 * E.g., "3 of 5 subtasks done (60%)" or "All 4 subtasks done"
 */
export function formatSubtaskProgress(metrics: SubtaskRollupMetrics): string {
  if (metrics.total === 0) {
    return "No subtasks";
  }
  if (metrics.isAllComplete) {
    return `All ${metrics.total} ${metrics.total === 1 ? "subtask" : "subtasks"} done`;
  }
  return `${metrics.completed} of ${metrics.total} subtasks done (${metrics.percentComplete}%)`;
}

const SUBTASK_PLACEHOLDER = /^(?:no\s+acceptance\s+criteria\s+recorded|none|n\/a)\.?$/i;
const SUBTASK_TITLE_LIMIT = 300;
const SUBTASK_COUNT_LIMIT = 32;

function normalizeSubtaskTitle(rawLine: string): string {
  let text = rawLine.trim();
  text = text.replace(/^(?:[-*+]|\d+[.)])(?:\s+|$)/, "").trim();
  text = text.replace(/^\[[ xX]\]\s*/, "").trim();
  text = text.replace(/^(?:[-*+]|\d+[.)])(?:\s+|$)/, "").trim();
  if (!text || SUBTASK_PLACEHOLDER.test(text)) return "";
  const chars = [...text];
  return chars.length > SUBTASK_TITLE_LIMIT ? chars.slice(0, SUBTASK_TITLE_LIMIT).join("") : text;
}

/**
 * Parses multi-line user breakdown input into bounded, deduplicated subtask titles.
 */
export function parseSubtaskLines(rawText: string, maxItems = SUBTASK_COUNT_LIMIT): string[] {
  const cap =
    Number.isSafeInteger(maxItems) && maxItems > 0
      ? Math.min(maxItems, SUBTASK_COUNT_LIMIT)
      : SUBTASK_COUNT_LIMIT;
  const seen = new Set<string>();
  const titles: string[] = [];
  for (const line of rawText.split(/\r?\n/)) {
    if (titles.length >= cap) break;
    const cleaned = normalizeSubtaskTitle(line);
    if (!cleaned) continue;
    const key = cleaned.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    titles.push(cleaned);
  }
  return titles;
}

/**
 * Splits acceptance criteria lines into non-duplicate child subtask titles.
 */
export function splitCriteriaIntoSubtasks(
  criteria: readonly string[],
  existingTitles: readonly string[] = [],
  maxItems = SUBTASK_COUNT_LIMIT
): string[] {
  const existing = new Set(
    existingTitles
      .map((t) => normalizeSubtaskTitle(t).toLowerCase())
      .filter((t) => t.length > 0)
  );
  const candidates = parseSubtaskLines(criteria.join("\n"), SUBTASK_COUNT_LIMIT);
  const cap =
    Number.isSafeInteger(maxItems) && maxItems > 0
      ? Math.min(maxItems, SUBTASK_COUNT_LIMIT)
      : SUBTASK_COUNT_LIMIT;
  return candidates.filter((title) => !existing.has(title.toLowerCase())).slice(0, cap);
}

