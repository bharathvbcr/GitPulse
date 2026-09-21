/**
 * Issue to Task transformation and agent handoff preparation.
 *
 * Provides a robust, boundary-hardened bridge between the GitHub issue monitor
 * (e.g. in the Policy page / ManviOpsPanel) and Workbench tasks.
 *
 * Every issue converted through this module produces a well-formed TaskDraft
 * that satisfies all repository and SQLite workbench store invariants:
 * - Title length bounded to MAX_ISSUE_TASK_TITLE (300 characters).
 * - Description byte length strictly under 64 KB (65,536 bytes UTF-8).
 * - Label count bounded to MAX_ISSUE_TASK_LABELS (32) and normalized.
 * - Kind strictly mapped to valid Task kinds ("bug" | "feature" | "task").
 * - Priority normalized to 0..3.
 * - Rich agent instructions so coding agents (Codex, Claude, Grok, AGY)
 *   have actionable context to investigate, reproduce, fix, and test.
 */

import {
  newID,
  putTask,
  registerRepository,
  listTasks,
  taskWrite,
  explainError,
  type Task,
  type TaskCard,
  type TaskDraft,
  type TaskStatus,
} from "./client";
import { bounded, MAX_TASK_LABELS } from "./taskActions";
import type { IssueInfo } from "../ops/model";

export const MAX_ISSUE_TASK_TITLE = 300;
export const MAX_ISSUE_TASK_DESCRIPTION_BYTES = 65_536;
export const MAX_ISSUE_TASK_LABELS = MAX_TASK_LABELS; // 32
export const MAX_LABEL_LENGTH = 64;

export interface IssueTaskOptions {
  status?: TaskStatus;
  priority?: number;
  kind?: string;
  extraLabels?: readonly string[];
  guidance?: string;
  homeWorkspaceId?: string | null;
}

export interface BatchCreateResult {
  created: Task[];
  skipped: number;
  errors: Array<{ number: number; title: string; error: string }>;
}

/** Sanitize title: trim, remove ASCII control characters, clamp to cap. */
export function sanitizeIssueTitle(rawTitle: unknown, issueNumber: number): string {
  const text = typeof rawTitle === "string" ? rawTitle : "";
  // Strip control chars (\x00-\x1F, \x7F) but preserve regular whitespace
  const stripped = text.replace(/[\x00-\x1F\x7F]+/g, " ").trim();
  const safeTitle = stripped.length > 0 ? stripped : `Issue #${issueNumber}`;
  const prefix = issueNumber > 0 ? `[#${issueNumber}] ` : "";
  const full = `${prefix}${safeTitle}`;

  if (full.length <= MAX_ISSUE_TASK_TITLE) {
    return full;
  }
  return `${full.slice(0, MAX_ISSUE_TASK_TITLE - 1)}…`;
}

/** Infer Task kind ("bug" | "feature" | "task") from issue labels. */
export function inferTaskKind(labels: readonly unknown[]): "bug" | "feature" | "task" {
  if (!Array.isArray(labels)) return "bug";
  const normalized = labels
    .filter((l): l is string => typeof l === "string")
    .map((l) => l.toLowerCase().trim());

  if (
    normalized.some((l) =>
      ["feature", "enhancement", "proposal", "feat", "request"].some((k) => l.includes(k)),
    )
  ) {
    return "feature";
  }

  if (
    normalized.some((l) =>
      ["doc", "docs", "documentation", "chore", "task", "refactor", "maintenance"].some((k) =>
        l.includes(k),
      ),
    )
  ) {
    return "task";
  }

  // Default to bug for issues
  return "bug";
}

/** Infer priority (0..3) from issue labels. */
export function inferTaskPriority(labels: readonly unknown[]): number {
  if (!Array.isArray(labels)) return 1;
  const normalized = labels
    .filter((l): l is string => typeof l === "string")
    .map((l) => l.toLowerCase().trim());

  if (
    normalized.some((l) =>
      ["p0", "critical", "blocker", "urgent", "priority:critical", "priority:urgent"].some((k) =>
        l === k || l.startsWith(`${k}:`) || l.endsWith(`:${k}`),
      ),
    )
  ) {
    return 3;
  }

  if (
    normalized.some((l) =>
      ["p1", "high", "important", "priority:high"].some(
        (k) => l === k || l.startsWith(`${k}:`) || l.endsWith(`:${k}`),
      ),
    )
  ) {
    return 2;
  }

  if (
    normalized.some((l) =>
      ["p3", "low", "minor", "trivial", "priority:low"].some(
        (k) => l === k || l.startsWith(`${k}:`) || l.endsWith(`:${k}`),
      ),
    )
  ) {
    return 0;
  }

  return 1;
}

/** Infer severity string ("critical" | "high" | null) from labels. */
export function inferTaskSeverity(labels: readonly unknown[]): string | null {
  if (!Array.isArray(labels)) return null;
  const normalized = labels
    .filter((l): l is string => typeof l === "string")
    .map((l) => l.toLowerCase().trim());

  if (normalized.some((l) => ["critical", "sev0", "sev1", "blocker"].includes(l))) {
    return "critical";
  }
  if (normalized.some((l) => ["sev2", "major", "high-severity"].includes(l))) {
    return "high";
  }
  return null;
}

/** Sanitize, bound, deduplicate and decorate labels for workbench. */
export function sanitizeIssueLabels(
  labels: readonly unknown[],
  issueNumber: number,
  extraLabels: readonly string[] = [],
): string[] {
  const result = new Set<string>();
  result.add("github-issue");
  if (issueNumber > 0) {
    result.add(`issue-${issueNumber}`);
  }

  const candidateStrings = [
    ...(Array.isArray(labels) ? labels : []),
    ...(Array.isArray(extraLabels) ? extraLabels : []),
  ];

  for (const raw of candidateStrings) {
    if (typeof raw !== "string") continue;
    const clean = raw
      .replace(/[\x00-\x1F\x7F]+/g, " ")
      .trim()
      .toLowerCase();
    if (!clean) continue;
    const bounded = clean.slice(0, MAX_LABEL_LENGTH).trim();
    if (!bounded) continue;
    result.add(bounded);
    if (result.size >= MAX_ISSUE_TASK_LABELS) break;
  }

  return Array.from(result).slice(0, MAX_ISSUE_TASK_LABELS);
}

/** Build structured agent-ready markdown description bounded to 64 KB. */
export function formatAgentDescription(issue: Partial<IssueInfo>, guidance?: string): string {
  const num = typeof issue.number === "number" && !Number.isNaN(issue.number) ? issue.number : 0;
  const title = typeof issue.title === "string" ? issue.title.trim() : "";
  const author = typeof issue.author === "string" && issue.author.trim() ? issue.author.trim() : "unknown";
  const url = typeof issue.url === "string" && issue.url.trim() ? issue.url.trim() : "";
  const state = typeof issue.state === "string" && issue.state.trim() ? issue.state.trim() : "open";
  const updated = typeof issue.updated_at === "string" && issue.updated_at.trim() ? issue.updated_at.trim() : "";
  const labels = Array.isArray(issue.labels)
    ? issue.labels.filter((l): l is string => typeof l === "string").join(", ")
    : "";

  const sections: string[] = [];

  sections.push(`## GitHub Issue #${num}: ${title}`);
  sections.push("");
  sections.push(`- **Issue URL**: ${url || "N/A"}`);
  sections.push(`- **Reported by**: ${author ? `@${author}` : "unknown"}`);
  sections.push(`- **State**: ${state}`);
  if (labels) sections.push(`- **GitHub Labels**: ${labels}`);
  if (updated) sections.push(`- **Last Updated**: ${updated}`);
  sections.push("");
  sections.push("---");
  sections.push("");
  sections.push("### Context & Objective");
  sections.push(
    `This task was imported from GitHub issue #${num} via the policy page issue monitor so autonomous coding agents can resolve it.`,
  );

  if (guidance?.trim()) {
    sections.push("");
    sections.push("### Additional Instructions");
    sections.push(guidance.trim());
  }

  sections.push("");
  sections.push("### Coding Agent Workflow");
  sections.push("1. **Locate & Audit**: Use DevMap search/explore to find the affected components and review caller impact.");
  sections.push("2. **Reproduce**: Identify failing behavior or write an automated regression test reproducing the issue.");
  sections.push("3. **Implement**: Apply a robust, universal fix addressing the root cause rather than a single edge case.");
  sections.push("4. **Verify**: Run unit tests, type checks, and verify all contract invariants hold before completing.");

  const rawMarkdown = sections.join("\n");
  const encoder = new TextEncoder();
  const bytes = encoder.encode(rawMarkdown);

  if (bytes.length <= MAX_ISSUE_TASK_DESCRIPTION_BYTES) {
    return rawMarkdown;
  }

  // Bound to safe size
  const notice = "\n\n… [Description truncated to fit 64 KB storage limit]";
  const noticeBytes = encoder.encode(notice).length;
  const targetBytes = MAX_ISSUE_TASK_DESCRIPTION_BYTES - noticeBytes;

  const decoder = new TextDecoder("utf-8");
  return `${decoder.decode(bytes.slice(0, targetBytes))}${notice}`;
}

/** Formulate default acceptance criteria for an issue task. */
export function formatAcceptanceCriteria(issue: Partial<IssueInfo>): string[] {
  const num = typeof issue.number === "number" && !Number.isNaN(issue.number) ? issue.number : 0;
  const rawTitle = typeof issue.title === "string" ? issue.title : "";
  const cleanTitle = rawTitle.replace(/[\x00-\x1F\x7F]+/g, " ").trim();
  const criteria1 = cleanTitle
    ? `Resolve GitHub issue #${num}: ${cleanTitle}`.slice(0, 300).trim()
    : `Resolve GitHub issue #${num}`;
  return [
    criteria1,
    "Verify fix with automated test suite ensuring no regressions",
  ];
}

/** Check if a TaskCard matches a GitHub issue number by title or label. */
export function isIssueInTask(
  card: Pick<TaskCard, "title" | "labels">,
  issueNumber: number,
): boolean {
  if (!card || issueNumber <= 0) return false;
  if (card.labels && Array.isArray(card.labels)) {
    if (
      card.labels.includes(`issue-${issueNumber}`) ||
      card.labels.includes(`#${issueNumber}`)
    ) {
      return true;
    }
  }
  if (typeof card.title === "string") {
    // Match only if the issue number appears as the primary issue identifier
    // at the beginning of the title (e.g. "[#42] Fix...", "#42: Fix...", "#42 - Fix...", "[Issue #42] Fix...")
    // This strictly prevents false-positive collisions with cross-references
    // (e.g. "[#100] Fix bug referencing #42" must NOT match issue 42).
    const trimmed = card.title.trim();
    const pattern = new RegExp(
      `^(?:\\[#${issueNumber}\\]|#${issueNumber}(?::|\\s|-|\\]|\\b)|\\[issue\\s*#${issueNumber}\\])`,
      "i",
    );
    if (pattern.test(trimmed)) return true;
  }
  return false;
}

/** Find existing task matching issue number. */
export function findTaskForIssue(
  tasks: readonly TaskCard[],
  issueNumber: number,
): TaskCard | undefined {
  if (!Array.isArray(tasks) || issueNumber <= 0) return undefined;
  return tasks.find((t) => isIssueInTask(t, issueNumber));
}

/** Check if issue number exists in task list. */
export function isIssueInTasks(
  tasks: readonly TaskCard[],
  issueNumber: number,
): boolean {
  return findTaskForIssue(tasks, issueNumber) !== undefined;
}

/** Transform an IssueInfo record into a valid TaskDraft. */
export function issueToTaskDraft(
  issue: IssueInfo,
  repositoryId: string,
  options: IssueTaskOptions = {},
): TaskDraft {
  const num = typeof issue?.number === "number" && !Number.isNaN(issue.number) ? issue.number : 0;
  const title = sanitizeIssueTitle(issue?.title, num);
  const labels = sanitizeIssueLabels(issue?.labels ?? [], num, options.extraLabels);
  const kind = options.kind || inferTaskKind(issue?.labels ?? []);
  const priority =
    typeof options.priority === "number"
      ? Math.max(0, Math.min(3, Math.floor(options.priority)))
      : inferTaskPriority(issue?.labels ?? []);
  const severity = inferTaskSeverity(issue?.labels ?? []);
  const description = formatAgentDescription(issue, options.guidance);
  const acceptance_criteria = formatAcceptanceCriteria(issue);

  return {
    title,
    kind,
    status: options.status || "inbox",
    priority,
    severity,
    owner: null,
    due_at: null,
    labels,
    repository_ids: repositoryId ? [repositoryId] : [],
    primary_repository_id: repositoryId || "",
    home_workspace_id: options.homeWorkspaceId ?? null,
    position: 0,
    description,
    acceptance_criteria,
  };
}

/**
 * Creates a workbench Task from a GitHub issue, registering the repository
 * first if needed.
 */
export async function createTaskFromIssue(
  issue: IssueInfo,
  repoPath: string,
  options: IssueTaskOptions = {},
): Promise<Task> {
  if (!repoPath || !repoPath.trim()) {
    throw new Error("Cannot create task: repository path is not available.");
  }
  if (!issue || typeof issue.number !== "number" || issue.number <= 0) {
    throw new Error("Cannot create task: invalid or missing issue information.");
  }

  // 1. Register or resolve repository in SQLite workbench store
  const repo = await bounded(registerRepository(repoPath));
  if (!repo || !repo.id) {
    throw new Error("Failed to register repository in workbench store.");
  }

  // 2. Build draft
  const draft = issueToTaskDraft(issue, repo.id, options);

  // 3. Persist new task
  const taskId = newID();
  const writeBody = taskWrite(taskId, 0, draft);
  const saved = await bounded(putTask(writeBody));

  return saved;
}

/**
 * Batch create workbench tasks from multiple GitHub issues, skipping any
 * issues that already have a corresponding task.
 */
export async function batchCreateTasksFromIssues(
  issues: readonly IssueInfo[],
  repoPath: string,
  existingTasks: readonly TaskCard[] = [],
  options: IssueTaskOptions = {},
): Promise<BatchCreateResult> {
  if (!repoPath || !repoPath.trim()) {
    throw new Error("Cannot batch create tasks: repository path is missing.");
  }

  const result: BatchCreateResult = {
    created: [],
    skipped: 0,
    errors: [],
  };

  if (!Array.isArray(issues) || issues.length === 0) {
    return result;
  }

  // Register repository once for the batch
  const repo = await bounded(registerRepository(repoPath));
  if (!repo || !repo.id) {
    throw new Error("Failed to register repository in workbench store.");
  }

  // Keep track of tasks created during the current batch to avoid internal duplicates
  const currentTasks = [...existingTasks];

  for (const issue of issues) {
    if (!issue || typeof issue.number !== "number" || issue.number <= 0) {
      continue;
    }

    if (isIssueInTasks(currentTasks, issue.number)) {
      result.skipped += 1;
      continue;
    }

    try {
      const draft = issueToTaskDraft(issue, repo.id, options);
      const taskId = newID();
      const writeBody = taskWrite(taskId, 0, draft);
      const saved = await bounded(putTask(writeBody));

      result.created.push(saved);
      currentTasks.push(saved);
    } catch (cause) {
      result.errors.push({
        number: issue.number,
        title: issue.title ?? "",
        error: explainError(cause),
      });
    }
  }

  return result;
}

/**
 * Loads all tasks associated with a repository across all statuses
 * (inbox, ready, in_progress, review, done), paginating until all
 * tasks are retrieved or maxTasks is reached.
 */
export async function listAllRepositoryTasks(
  repositoryId: string,
  maxTasks = 1000,
): Promise<TaskCard[]> {
  if (!repositoryId) return [];
  const all: TaskCard[] = [];
  let cursor: string | undefined = undefined;

  while (all.length < maxTasks) {
    const page = await listTasks(
      { kind: "repository", id: repositoryId },
      undefined,
      "",
      cursor,
      200,
    );
    all.push(...page.items);
    if (!page.has_more || !page.next_cursor || page.items.length === 0) {
      break;
    }
    cursor = page.next_cursor;
  }

  return all.slice(0, maxTasks);
}
