import { WorkbenchError, explainError, type Page, type TaskCard, type TaskStatus } from "./client";

/** Upper bound for one user-confirmed delete pass. The rest stay selected. */
export const MAX_DELETE_BATCH = 50;
/** Titles listed in the confirm body; the remainder is a count. */
export const MAX_CONFIRM_TITLES = 8;
/** Per-attempt IPC bound. Hung deletes stay retryable, never silent. */
export const DELETE_ATTEMPT_TIMEOUT_MS = 30_000;
export const TASK_ID_RE = /^[A-Za-z0-9_-]{1,128}$/;

export interface DeletableTask {
  id: string;
  revision: number;
  title: string;
}

export interface DeleteAttempt {
  id: string;
  request_id: string;
  expected_revision: number;
}

export interface DeleteFailure {
  id: string;
  title: string;
  code: string;
  message: string;
  retryable: boolean;
}

export interface DeleteBatchResult {
  deleted: string[];
  failed: DeleteFailure[];
  skipped: number;
  attempted: number;
  total: number;
}

const RETRYABLE_CODES = new Set(["transport_error", "worker_error", "store_error", "protocol_error"]);

export function displayTitle(title: unknown, cap = 80): string {
  if (typeof title !== "string") return "(untitled)";
  const cleaned = title.replace(/[\u0000-\u001F\u007F]/g, " ").replace(/\s+/g, " ").trim();
  if (!cleaned) return "(untitled)";
  if (!Number.isFinite(cap) || cap < 8) return cleaned.slice(0, 8);
  return cleaned.length > cap ? `${cleaned.slice(0, cap - 1)}…` : cleaned;
}

export function isTaskId(id: unknown): id is string {
  return typeof id === "string" && TASK_ID_RE.test(id);
}

export function isRevision(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 1;
}

/**
 * First well-formed occurrence wins. Duplicates, empty ids, and non-revisions
 * are dropped rather than sent to storage — a second row with a stale
 * revision must not overwrite the identity the user actually selected.
 */
export function uniqueDeletable(tasks: readonly unknown[]): DeletableTask[] {
  if (!Array.isArray(tasks)) return [];
  const seen = new Set<string>();
  const out: DeletableTask[] = [];
  for (const raw of tasks) {
    if (typeof raw !== "object" || raw === null) continue;
    const rec = raw as Record<string, unknown>;
    if (!isTaskId(rec.id) || !isRevision(rec.revision) || seen.has(rec.id)) continue;
    seen.add(rec.id);
    out.push({ id: rec.id, revision: rec.revision, title: displayTitle(rec.title, 120) });
  }
  return out;
}

export function deleteRefusal(state: {
  moving?: boolean;
  opening?: boolean;
  deleting?: boolean;
  enhancing?: boolean;
  selected?: number;
}): string | null {
  if (state.deleting) return "A delete is already running.";
  if (state.moving) return "Finish moving the card before deleting.";
  if (state.opening) return "Wait for the task to finish opening.";
  if (state.enhancing) return "Finish the Manvi enhancement before deleting.";
  if ((state.selected ?? 0) <= 0) return "Select a task to delete.";
  return null;
}

export function deleteConfirmCopy(tasks: readonly DeletableTask[]): {
  title: string;
  message: string;
  confirmLabel: string;
} {
  const items = uniqueDeletable(tasks);
  if (items.length === 0) {
    return {
      title: "Delete tasks",
      message: "Nothing valid is selected to delete.",
      confirmLabel: "Delete",
    };
  }
  const listed = items.slice(0, MAX_CONFIRM_TITLES).map((task) => `• ${task.title}`);
  const extra = items.length - listed.length;
  if (extra > 0) listed.push(`• and ${extra} more`);
  const noun = items.length === 1 ? "task" : "tasks";
  const heading = items.length === 1 ? `Delete “${items[0].title}”?` : `Delete ${items.length} tasks?`;
  return {
    title: heading,
    message: [
      listed.join("\n"),
      "",
      `This removes the ${noun} from every board. History stays in the store, but the same task ID cannot be restored from this screen. Queued Manvi suggestions for ${items.length === 1 ? "this task" : "these tasks"} are dropped.`,
    ].join("\n"),
    confirmLabel: items.length === 1 ? "Delete task" : `Delete ${items.length} tasks`,
  };
}

export function deleteAttempt(task: DeletableTask, requestId: string): DeleteAttempt | null {
  if (!isTaskId(task.id) || !isRevision(task.revision)) return null;
  if (typeof requestId !== "string" || requestId.length === 0 || requestId.length > 128) return null;
  return { id: task.id, request_id: requestId, expected_revision: task.revision };
}

export function classifyDeleteError(error: unknown): { code: string; message: string; retryable: boolean } {
  const code = error instanceof WorkbenchError ? error.code : "transport_error";
  const message = explainError(error);
  if (code === "not_found") return { code, message, retryable: false };
  return { code, message, retryable: RETRYABLE_CODES.has(code) };
}

export function isRetryableDelete(error: unknown): boolean {
  return classifyDeleteError(error).retryable;
}

/** `not_found` means the board is already consistent — count it as deleted. */
export function deleteAlreadyGone(error: unknown): boolean {
  return classifyDeleteError(error).code === "not_found";
}

export async function runBoundedSerial<T>(
  items: readonly T[],
  each: (item: T) => Promise<void>,
  options: { limit?: number; cancelled?: () => boolean } = {},
): Promise<{ done: T[]; failed: { item: T; error: unknown }[]; skipped: T[] }> {
  const list = Array.isArray(items) ? items : [];
  const cap = Number.isSafeInteger(options.limit) && (options.limit ?? 0) > 0
    ? Math.min(options.limit as number, 10_000)
    : MAX_DELETE_BATCH;
  const skipped = list.slice(cap);
  const done: T[] = [];
  const failed: { item: T; error: unknown }[] = [];
  for (const item of list.slice(0, cap)) {
    if (options.cancelled?.()) break;
    try {
      await each(item);
      done.push(item);
    } catch (error) {
      failed.push({ item, error });
    }
  }
  return { done, failed, skipped };
}

export async function withTimeout<T>(work: Promise<T>, ms: number): Promise<T> {
  const cap = Number.isFinite(ms) && ms > 0 ? Math.min(ms, 120_000) : DELETE_ATTEMPT_TIMEOUT_MS;
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      work,
      new Promise<never>((_, reject) => {
        timer = setTimeout(
          () => reject(new WorkbenchError("transport_error", "Delete timed out. Retry to confirm its result.")),
          cap,
        );
      }),
    ]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

export async function deleteTasks(
  tasks: readonly DeletableTask[],
  run: (attempt: DeleteAttempt) => Promise<void>,
  options: { newID: () => string; limit?: number; cancelled?: () => boolean; timeout?: number },
): Promise<DeleteBatchResult> {
  const items = uniqueDeletable(tasks);
  const attempts = items
    .map((task) => {
      const attempt = deleteAttempt(task, options.newID());
      return attempt ? { task, attempt } : null;
    })
    .filter((row): row is { task: DeletableTask; attempt: DeleteAttempt } => row !== null);
  const exec = (attempt: DeleteAttempt) => withTimeout(run(attempt), options.timeout ?? DELETE_ATTEMPT_TIMEOUT_MS);

  const serial = await runBoundedSerial(attempts, async (row) => {
    try {
      await exec(row.attempt);
    } catch (error) {
      if (deleteAlreadyGone(error)) return;
      if (!isRetryableDelete(error)) throw error;
      await exec(row.attempt);
    }
  }, { limit: options.limit, cancelled: options.cancelled });

  const failed: DeleteFailure[] = serial.failed.map((entry) => {
    const classified = classifyDeleteError(entry.error);
    return {
      id: entry.item.task.id,
      title: entry.item.task.title,
      code: classified.code,
      message: classified.message,
      retryable: classified.retryable,
    };
  });

  return {
    deleted: serial.done.map((row) => row.task.id),
    failed,
    skipped: serial.skipped.length,
    attempted: serial.done.length + serial.failed.length,
    total: items.length,
  };
}

export function removeFromColumns(
  columns: Partial<Record<TaskStatus, Page<TaskCard>>>,
  ids: ReadonlySet<string>,
): Partial<Record<TaskStatus, Page<TaskCard>>> {
  if (!(ids instanceof Set) || ids.size === 0) return columns;
  const next: Partial<Record<TaskStatus, Page<TaskCard>>> = {};
  for (const [status, page] of Object.entries(columns) as [TaskStatus, Page<TaskCard> | undefined][]) {
    if (!page) continue;
    const items = page.items.filter((item) => !ids.has(item.id));
    const removed = page.items.length - items.length;
    const total = Math.max(0, page.total - removed);
    next[status] = {
      ...page,
      items,
      shown: items.length,
      total,
      has_more: page.has_more && total > items.length,
    };
  }
  return next;
}

export function deleteSummary(result: DeleteBatchResult): string {
  const parts: string[] = [];
  if (result.deleted.length === 1) parts.push("Deleted 1 task.");
  else if (result.deleted.length > 1) parts.push(`Deleted ${result.deleted.length} tasks.`);
  if (result.failed.length) {
    const first = result.failed[0];
    const extra = result.failed.length - 1;
    parts.push(
      extra > 0
        ? `${result.failed.length} failed (${first.code}: ${first.message}).`
        : `Could not delete “${first.title}” (${first.code}: ${first.message}).`,
    );
  }
  if (result.skipped > 0) {
    parts.push(`Held ${result.skipped} more — confirm again to continue (cap ${MAX_DELETE_BATCH} per pass).`);
  }
  if (parts.length === 0) return "Nothing was deleted.";
  return parts.join(" ");
}
