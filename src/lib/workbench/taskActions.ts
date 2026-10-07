import { deleteTask, explainError, getTask, newID, putTask, taskDraft, taskWrite, WorkbenchError, STATUSES, STATUS_LABELS, type Task, type TaskCard, type TaskDraft, type TaskStatus } from "./client";
import { plural } from "../format";

export const MAX_TASK_SELECTION = 100;
export const TASK_ACTION_TIMEOUT_MS = 30_000;
/**
 * How long a confirmed board deletion waits before it is written.
 *
 * The store cannot bring a deleted task back — a deleted id is never reused —
 * so the only undo a deletion can have is not sending it yet. Matches the
 * toast store's window for an action ("Undo" after a branch delete).
 */
export const DEFERRED_DELETE_MS = 12_000;
/** How long the board keeps offering to undo a change it already wrote. */
export const UNDO_OFFER_MS = 60_000;
/** Longest owner name a bulk action may set. Matches the editor's own cap. */
export const MAX_OWNER_LENGTH = 300;
/** Labels a task may carry after a bulk add. */
export const MAX_TASK_LABELS = 32;
export type TaskChanges = {
  status?: TaskStatus;
  priority?: number;
  position?: number;
  owner?: string | null;
  due_at?: number | null;
};
export type TaskAction =
  | { kind: "delete" }
  | { kind: "update"; changes: TaskChanges }
  | { kind: "reorder"; status: TaskStatus; positions: Record<string, number> }
  /**
   * Add or remove one label across the selection.
   *
   * Its own kind rather than a `changes.labels` array because every card has
   * a different label set: one shared array would overwrite each task's other
   * labels with whatever the first card happened to have.
   */
  | { kind: "label"; label: string; add: boolean }
  /**
   * Put back the values another batch replaced, one task at a time.
   *
   * Per task, like `label`: each card had its own status, labels or due date
   * before the change, so one shared `changes` object could not undo them.
   * Only the fields the original batch wrote are restored, against the
   * revision it produced — a task edited since then is refused, not reverted.
   */
  | { kind: "restore"; fields: Record<string, RestoreFields> };
/** The fields a board action can change, and so the fields an undo restores. */
export type RestoreFields = Partial<Pick<TaskDraft, "status" | "priority" | "position" | "owner" | "due_at" | "labels">>;
const RESTORABLE = ["status", "priority", "position", "owner", "due_at", "labels"] as const;
/** The least a batch needs to know about a card: which task, which revision. */
export type BatchCard = Pick<TaskCard, "id" | "title" | "revision">;
/** A batch that undoes another, ready to run. */
export interface UndoPlan { cards: BatchCard[]; action: TaskAction; label: string }
export type ActionState = "waiting" | "running" | "done" | "failed" | "uncertain";
export interface ActionResult { id: string; title: string; state: ActionState; error: string }
interface Entry extends ActionResult { revision: number; input: Record<string, unknown> | null; before: RestoreFields | null }

/** Which saved fields an action overwrites; what its undo has to put back. */
function touchedFields(action: TaskAction): (keyof RestoreFields)[] {
  if (action.kind === "update") return RESTORABLE.filter((key) => key in action.changes);
  if (action.kind === "reorder") return ["status", "position"];
  if (action.kind === "label") return ["labels"];
  return [];
}

function validLabel(label: unknown): label is string {
  return typeof label === "string" && label.trim().length > 0 && label.length <= 64 && !/[\u0000-\u001f\u007f]/.test(label);
}

function validChanges(changes: TaskChanges): boolean {
  const {status,priority,position,owner,due_at} = changes;
  if (status !== undefined && !STATUSES.includes(status)) return false;
  if (priority !== undefined && (!Number.isInteger(priority) || priority < 0 || priority > 3)) return false;
  if (position !== undefined && (!Number.isSafeInteger(position) || position < 0)) return false;
  // Owner and due date reach the wire from a menu, so they are checked to
  // the same standard as a typed field rather than trusted for being
  // "internal": a control character or an out-of-range epoch is refused
  // here, where one message covers every caller.
  if (owner !== undefined && owner !== null && (typeof owner !== "string" || owner.length > MAX_OWNER_LENGTH || /[\u0000-\u001f\u007f]/.test(owner))) throw new Error("Invalid task owner.");
  if (due_at !== undefined && due_at !== null && (!Number.isSafeInteger(due_at) || due_at <= 0)) throw new Error("Invalid task due date.");
  return true;
}

/**
 * One line saying what a batch did, for the undo offer and the live region.
 *
 * Archiving is a status change to Done (`taskArchive.ts`), so it reads as a
 * move to that column — which is exactly what its undo reverses.
 */
export function describeTaskAction(action: TaskAction, count: number): string {
  const tasks = plural(count, "task");
  if (action.kind === "delete") return `Deleted ${tasks}`;
  if (action.kind === "restore") return `Restored ${tasks}`;
  if (action.kind === "label") return action.add ? `Added label “${action.label}” to ${tasks}` : `Removed label “${action.label}” from ${tasks}`;
  if (action.kind === "reorder") return `Reordered ${tasks} in ${STATUS_LABELS[action.status]}`;
  const { status, priority, owner, due_at } = action.changes;
  if (status !== undefined) return `Moved ${tasks} to ${STATUS_LABELS[status]}`;
  if (priority !== undefined) return `Changed priority of ${tasks}`;
  if (owner !== undefined) return owner === null ? `Cleared the owner of ${tasks}` : `Assigned ${tasks} to ${owner}`;
  if (due_at !== undefined) return due_at === null ? `Cleared the due date of ${tasks}` : `Changed the due date of ${tasks}`;
  return `Updated ${tasks}`;
}

/**
 * A write that waits, and can be called off until it starts.
 *
 * `flush` runs it now; `cancel` drops it without running. Either settles the
 * deferral, so a timer that fires afterwards does nothing — the one property
 * a deferred deletion needs, since "undone" and "deleted anyway" must never
 * both happen.
 */
export function defer(ms: number, run: () => void): { cancel: () => boolean; flush: () => boolean } {
  let settled = false;
  const timer = setTimeout(() => { if (!settled) { settled = true; run(); } }, ms);
  return {
    cancel: () => { if (settled) return false; settled = true; clearTimeout(timer); return true; },
    flush: () => { if (settled) return false; settled = true; clearTimeout(timer); run(); return true; },
  };
}
interface IO { read: typeof getTask; write: typeof putTask; remove: (input: Record<string, unknown>) => Promise<void> }
const defaultIO: IO = { read: getTask, write: putTask, remove: deleteTask };
const uncertain = (cause: unknown) => !(cause instanceof WorkbenchError) || ["transport_error", "worker_error", "store_error", "protocol_error"].includes(cause.code);

/**
 * One task's labels after an add or a remove.
 *
 * Reads the saved labels, so a concurrent edit that added a label is kept:
 * the batch already refuses to write when the revision moved under it, and
 * this keeps the non-conflicting case from losing work either.
 */
export function nextLabels(current: readonly string[], label: string, add: boolean): string[] {
  const wanted = label.trim();
  const kept = current.filter((entry) => entry !== wanted);
  if (!add) return kept;
  if (kept.length >= MAX_TASK_LABELS) return [...current];
  return [...kept, wanted];
}

/**
 * How long an attempt that makes its own worktree may take to be prepared.
 *
 * The host builds the worktree and runs the repository's post_create setup
 * inside that one call, and each setup command may run for 15 minutes
 * (`worktree_hooks.rs::HOOK_TIMEOUT`) — a dependency install. The 30-second
 * bound gave up while setup was still running, and the retry it invited was
 * refused as "still setting up its worktree". An hour covers several setup
 * commands; past it the answer is still on the attempt's row once it lands.
 */
export const WORKTREE_PREPARE_TIMEOUT_MS = 60 * 60_000;

export async function bounded<T>(work: Promise<T>, ms: number = TASK_ACTION_TIMEOUT_MS): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try { return await Promise.race([work, new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new WorkbenchError("transport_error", "Task action timed out. Retry to confirm its result.")), ms); })]); }
  finally { clearTimeout(timer); }
}

/** One immutable selection and one receipt identity per task, shared by every task action surface. */
export class TaskBatch {
  private entries: Entry[];
  private running = false;
  private paused = false;
  private action: TaskAction;
  constructor(cards: readonly BatchCard[], action: TaskAction, private io: IO = defaultIO) {
    if (!cards.length || cards.length > MAX_TASK_SELECTION || new Set(cards.map(c => c.id)).size !== cards.length) throw new Error(`Select between 1 and ${MAX_TASK_SELECTION} distinct tasks.`);
    if (cards.some(card => !card.id || !Number.isSafeInteger(card.revision) || card.revision < 1)) throw new Error("Task selection contains an invalid saved revision.");
    if (action.kind === "update" && (!Object.keys(action.changes).length || !validChanges(action.changes))) throw new Error("Invalid task organization change.");
    if (action.kind === "reorder" && (!STATUSES.includes(action.status) || Object.keys(action.positions).length !== cards.length || cards.some(card => !Number.isSafeInteger(action.positions[card.id]) || action.positions[card.id] < 0))) throw new Error("Invalid task ordering plan.");
    if (action.kind === "label") {
      const label = typeof action.label === "string" ? action.label.trim() : "";
      if (!validLabel(label)) throw new Error("Invalid task label.");
    }
    if (action.kind === "restore") {
      const ids = Object.keys(action.fields);
      if (ids.length !== cards.length || cards.some(card => !(card.id in action.fields))) throw new Error("Invalid task restore plan.");
      for (const fields of Object.values(action.fields)) {
        const keys = Object.keys(fields);
        if (!keys.length || keys.some(key => !(RESTORABLE as readonly string[]).includes(key))) throw new Error("Invalid task restore plan.");
        const { labels, ...changes } = fields;
        if (!validChanges(changes)) throw new Error("Invalid task restore plan.");
        if (labels !== undefined && (!Array.isArray(labels) || labels.length > MAX_TASK_LABELS || !labels.every(validLabel))) throw new Error("Invalid task restore plan.");
      }
    }
    this.entries = cards.map(card => ({ id: card.id, title: card.title, revision: card.revision, state: "waiting", error: "", input: null, before: null }));
    this.action = action.kind === "delete" ? {kind:"delete"}
      : action.kind === "reorder" ? {...action,positions:{...action.positions}}
      : action.kind === "label" ? {kind:"label", label:action.label.trim(), add:action.add === true}
      : action.kind === "restore" ? {kind:"restore", fields:Object.fromEntries(Object.entries(action.fields).map(([id, fields]) => [id, {...fields, ...(fields.labels ? {labels:[...fields.labels]} : {})}]))}
      : {kind:"update", changes:{...action.changes}};
  }
  /**
   * The batch that puts back what this one wrote, for the tasks it changed.
   *
   * Null when there is nothing to undo: no task changed, a deletion (whose
   * undo is never sending it — see `DEFERRED_DELETE_MS`), or a restore, whose
   * own undo would be a redo nobody asked for. Each card carries the revision
   * this batch produced, so the undo is refused for a task edited since.
   */
  undo(): UndoPlan | null {
    if (this.action.kind === "delete" || this.action.kind === "restore") return null;
    const changed = this.entries.filter(entry => entry.state === "done" && entry.before !== null);
    if (!changed.length) return null;
    return {
      cards: changed.map(entry => ({ id: entry.id, title: entry.title, revision: entry.revision + 1 })),
      action: { kind: "restore", fields: Object.fromEntries(changed.map(entry => [entry.id, { ...entry.before }])) },
      label: describeTaskAction(this.action, changed.length),
    };
  }
  snapshot(): ActionResult[] { return this.entries.map(({id,title,state,error}) => ({id,title,state,error})); }
  stop() { this.paused = true; }
  async run(changed: () => void = () => {}): Promise<void> {
    if (this.running) return;
    this.running = true; this.paused = false;
    try {
      for (const entry of this.entries) {
        if (this.paused) break;
        if (entry.state === "done" || entry.state === "failed") continue;
        entry.state = "running"; entry.error = ""; changed();
        try {
          if (!entry.input) {
            if (this.action.kind === "delete") entry.input = {id:entry.id, expected_revision:entry.revision, request_id:newID()};
            else {
              const full = await bounded(this.io.read(entry.id));
              if (full.id !== entry.id || full.revision !== entry.revision) throw new WorkbenchError("revision_conflict", "This task changed while you were moving it. Refresh tasks and review the latest version.");
              if (this.paused) { entry.state = "waiting"; changed(); break; }
              const draft = taskDraft(full);
              const changes: RestoreFields = this.action.kind === "reorder"
                ? {status:this.action.status,position:this.action.positions[entry.id]}
                : this.action.kind === "label"
                  ? {labels: nextLabels(draft.labels, this.action.label, this.action.add)}
                  : this.action.kind === "restore"
                    ? this.action.fields[entry.id] ?? {}
                    : this.action.changes;
              // What the saved task held before this write, read from the same
              // revision the write is checked against. That is the only value
              // an undo may put back; the card the board drew can be stale.
              const touched = touchedFields(this.action);
              entry.before = touched.length ? Object.fromEntries(touched.map(key => [key, key === "labels" ? [...draft.labels] : draft[key]])) : null;
              entry.input = taskWrite(entry.id, entry.revision, {...draft, ...changes});
            }
          }
          if (this.action.kind === "delete") await bounded(this.io.remove(entry.input));
          else {
            const saved: Task = await bounded(this.io.write(entry.input));
            if (saved.id !== entry.id || saved.revision !== entry.revision + 1) throw new WorkbenchError("protocol_error", "Task update confirmation does not match the request.");
          }
          entry.state = "done";
        } catch (cause) {
          entry.state = uncertain(cause) ? "uncertain" : "failed";
          entry.error = explainError(cause);
        }
        changed();
        if (entry.state === "uncertain") break;
      }
    } finally { this.running = false; changed(); }
  }
}
