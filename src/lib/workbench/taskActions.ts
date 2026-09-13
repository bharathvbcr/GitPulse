import { deleteTask, explainError, getTask, newID, putTask, taskDraft, taskWrite, WorkbenchError, STATUSES, type Task, type TaskCard, type TaskStatus } from "./client";

export const MAX_TASK_SELECTION = 100;
export const TASK_ACTION_TIMEOUT_MS = 30_000;
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
  | { kind: "label"; label: string; add: boolean };
export type ActionState = "waiting" | "running" | "done" | "failed" | "uncertain";
export interface ActionResult { id: string; title: string; state: ActionState; error: string }
interface Entry extends ActionResult { revision: number; input: Record<string, unknown> | null }
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

export async function bounded<T>(work: Promise<T>): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try { return await Promise.race([work, new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new WorkbenchError("transport_error", "Task action timed out. Retry to confirm its result.")), TASK_ACTION_TIMEOUT_MS); })]); }
  finally { clearTimeout(timer); }
}

/** One immutable selection and one receipt identity per task, shared by every task action surface. */
export class TaskBatch {
  private entries: Entry[];
  private running = false;
  private paused = false;
  private action: TaskAction;
  constructor(cards: readonly TaskCard[], action: TaskAction, private io: IO = defaultIO) {
    if (!cards.length || cards.length > MAX_TASK_SELECTION || new Set(cards.map(c => c.id)).size !== cards.length) throw new Error(`Select between 1 and ${MAX_TASK_SELECTION} distinct tasks.`);
    if (cards.some(card => !card.id || !Number.isSafeInteger(card.revision) || card.revision < 1)) throw new Error("Task selection contains an invalid saved revision.");
    if (action.kind === "update") {
      const {status,priority,position,owner,due_at} = action.changes;
      if (!Object.keys(action.changes).length || status !== undefined && !STATUSES.includes(status) || priority !== undefined && (!Number.isInteger(priority) || priority < 0 || priority > 3) || position !== undefined && (!Number.isSafeInteger(position) || position < 0)) throw new Error("Invalid task organization change.");
      // Owner and due date reach the wire from a menu, so they are checked to
      // the same standard as a typed field rather than trusted for being
      // "internal": a control character or an out-of-range epoch is refused
      // here, where one message covers every caller.
      if (owner !== undefined && owner !== null && (typeof owner !== "string" || owner.length > MAX_OWNER_LENGTH || /[\u0000-\u001f\u007f]/.test(owner))) throw new Error("Invalid task owner.");
      if (due_at !== undefined && due_at !== null && (!Number.isSafeInteger(due_at) || due_at <= 0)) throw new Error("Invalid task due date.");
    }
    if (action.kind === "reorder" && (!STATUSES.includes(action.status) || Object.keys(action.positions).length !== cards.length || cards.some(card => !Number.isSafeInteger(action.positions[card.id]) || action.positions[card.id] < 0))) throw new Error("Invalid task ordering plan.");
    if (action.kind === "label") {
      const label = typeof action.label === "string" ? action.label.trim() : "";
      if (!label || label.length > 64 || /[\u0000-\u001f\u007f]/.test(label)) throw new Error("Invalid task label.");
    }
    this.entries = cards.map(card => ({ id: card.id, title: card.title, revision: card.revision, state: "waiting", error: "", input: null }));
    this.action = action.kind === "delete" ? {kind:"delete"} : action.kind === "reorder" ? {...action,positions:{...action.positions}} : action.kind === "label" ? {kind:"label", label:action.label.trim(), add:action.add === true} : {kind:"update", changes:{...action.changes}};
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
              const changes = this.action.kind === "reorder"
                ? {status:this.action.status,position:this.action.positions[entry.id]}
                : this.action.kind === "label"
                  ? {labels: nextLabels(draft.labels, this.action.label, this.action.add)}
                  : this.action.changes;
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
