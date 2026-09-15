import {
  changeEnhancement,
  completeEnhancement,
  getTask,
  newID,
  WorkbenchError,
  type Enhancement,
  type EnhancementConfiguration,
  type EnhancementField,
  type EnhancementMutation,
  type EnhancementState,
  type EnhancementSummary,
  type ModelSelection,
  type Task,
} from "./client";
import { canQuickEnhance, enhanceableFields } from "./taskOrganize";
import { bounded } from "./taskActions";
import { selectionWire } from "./taskModel";
import {
  APPLE_ENGINE,
  APPLE_MODEL,
  appleErrorCode,
  draftWithApple,
  explainAppleError,
} from "../ai/appleIntelligence";

/**
 * The engines a task sheet can draft with, and the one name each is called by.
 *
 * This module already owns both paths — `startQuickEnhance` reaches Manvi and
 * `runAppleEnhancement` runs on-device — so it owns what they are called too.
 * The sheet writes about the engine in placeholders and in the message that
 * refuses a shortcut mid-generation; the assist section draws the picker. A
 * name spelled in either of those would be a second source of truth that drifts
 * the moment an engine is added or renamed.
 */
export type AssistEngine = "manvi" | "apple";

const ASSIST_ENGINE_NAMES: Readonly<Record<AssistEngine, string>> = Object.freeze(
  Object.assign(Object.create(null) as Record<AssistEngine, string>, {
    manvi: "Manvi",
    apple: "Apple Intelligence",
  }),
);

/** Every engine, in the order the picker offers them. */
export const ASSIST_ENGINE_LIST: readonly AssistEngine[] = Object.freeze(["manvi", "apple"] as const);

/** The engine used until the reader picks otherwise, or Apple is not reachable. */
export const DEFAULT_ASSIST_ENGINE: AssistEngine = "manvi";

export const assistEngineName = (engine: AssistEngine): string => ASSIST_ENGINE_NAMES[engine];

export const liveEnhancement = (proposal: Pick<Enhancement, "state"> | null): boolean =>
  proposal !== null && ["pending", "running", "cancel_requested"].includes(proposal.state);

/**
 * How each proposal state reads.
 *
 * One map, read by the history picker's option text and by the heading of the
 * review that option selects. They used to be the same literal table written
 * once inside the component, which was fine while there was one reader; a
 * picker whose row says "Ready for review" above a heading that says something
 * else is the drift this prevents.
 */
export const ENHANCEMENT_STATE_LABELS: Readonly<Record<EnhancementState, string>> = Object.freeze(
  Object.assign(Object.create(null) as Record<EnhancementState, string>, {
    pending: "Waiting to start", running: "Generating", cancel_requested: "Cancellation requested",
    ready: "Ready for review", failed: "Generation failed", cancelled: "Cancelled",
    interrupted: "Outcome uncertain", dismissed: "Dismissed", accepted: "Accepted", undone: "Undone",
  }),
);

/**
 * One line naming a past suggestion, built from the list payload alone.
 *
 * `enhancements.list` returns summaries — the proposed *text* only arrives with
 * `enhancements.get` — so a picker that needed the wording to label its rows
 * would have to fetch every entry to draw itself. Everything here is on the
 * summary.
 *
 * The old rows read `state · model` plus `Task revision N`, which could not
 * tell two attempts apart at all: same model, same revision, same string. The
 * two additions that fix that are the time and the ordinal, and they fix
 * different halves of it — the time separates attempts made minutes apart, the
 * ordinal separates attempts made in the same second.
 *
 * `ordinal` is a reading aid, not an identity: it is derived from the store's
 * total at the moment the page was read, so a proposal landing between pages
 * shifts every one of them. Identity is the id, which is what the option
 * carries as its value. Do not promote this to a stored field.
 */
export function enhancementOptionLabel(
  entry: Pick<
    EnhancementSummary,
    "state" | "model" | "source_revision" | "automatic" | "edited_fields" | "outcome_uncertain"
  >,
  context: { ordinal: number; when: string },
): string {
  const parts = [
    `#${context.ordinal}`,
    ENHANCEMENT_STATE_LABELS[entry.state] ?? entry.state,
    entry.model || "unnamed model",
  ];
  // Put identity first and the qualifiers last: `gp-select` truncates, and
  // what must survive the ellipsis is what tells two rows apart.
  if (context.when) parts.push(context.when);
  parts.push(`rev ${entry.source_revision}`);
  if (entry.automatic) parts.push("Automatic");
  if (entry.edited_fields.length) parts.push("Edited");
  if (entry.outcome_uncertain) parts.push("Uncertain");
  return parts.join(" · ");
}

/**
 * Why a selected proposal cannot be applied to the task in front of the reader,
 * or "" when it can.
 *
 * Picking an older entry and finding no Apply button was the history drawer's
 * quietest dead end. The refusal is not the picker's opinion — `enhancements.
 * accept` carries `expected_task_revision` and the store checks it — so the
 * honest thing is to name the reason rather than to render nothing.
 */
export function enhancementApplyBlock(
  proposal: Pick<Enhancement, "state" | "source_revision"> | null,
  task: Pick<Task, "revision" | "locked_fields"> | null,
): string {
  if (!proposal || !task) return "";
  if (proposal.state !== "ready") {
    return `This suggestion is ${(ENHANCEMENT_STATE_LABELS[proposal.state] ?? proposal.state).toLowerCase()}, so there is nothing left to apply.`;
  }
  if (proposal.source_revision !== task.revision) {
    return `Written against revision ${proposal.source_revision}; this task is at revision ${task.revision}. Ask for a fresh suggestion.`;
  }
  if (enhanceableFields(task).length === 0) return "Title and description are locked against enhancement.";
  return "";
}

type PendingEnhancement = { method: EnhancementMutation; input: Record<string, unknown>; taskID: string; result: Enhancement | null };

/** One receipt per action, including retries after a committed but lost reply. */
export class EnhancementAction {
  pending: PendingEnhancement | null = null;
  private running = false;
  constructor(
    private io = { change: changeEnhancement, read: getTask },
    private selection: () => ModelSelection | null = () => null,
  ) {}

  async run(method: EnhancementMutation, input: Record<string, unknown>, taskID: string): Promise<{ proposal: Enhancement; task: Task | null }> {
    if (this.running) throw new WorkbenchError("busy", "An enhancement action is already in progress.");
    if (this.pending && (method !== this.pending.method || input.request_id !== this.pending.input.request_id || taskID !== this.pending.taskID)) {
      throw new WorkbenchError("busy", "Retry the pending enhancement action first.");
    }
    this.pending ??= { method, input: structuredClone(input), taskID, result: null };
    this.running = true;
    try {
      // Creation can advance once to generation. Neither retries nor reads can
      // start another inference or recreate a proposal under another identity.
      for (let step = 0; step < 2; step++) {
        const action: PendingEnhancement | null = this.pending;
        if (!action) throw new WorkbenchError("protocol_error", "Missing enhancement action.");
        const proposal: Enhancement = action.result ?? await bounded(this.io.change(action.method, action.input));
        if (proposal.id !== action.input.id || proposal.task_id !== action.taskID ||
            (action.method === "enhancements.create" && (proposal.task_id !== action.input.task_id || proposal.source_revision !== action.input.source_revision)) ||
            proposal.revision <= Number(action.input.expected_revision)) {
          throw new WorkbenchError("protocol_error", "Enhancement confirmation does not match the request.");
        }
        if ((action.method === "enhancements.accept" && (proposal.state !== "accepted" || proposal.source_revision !== action.input.expected_task_revision)) ||
            (action.method === "enhancements.undo" && proposal.state !== "undone")) {
          throw new WorkbenchError("protocol_error", "Enhancement receipt did not confirm the requested action.");
        }
        action.result = proposal;
        let task: Task | null = null;
        if (action.method === "enhancements.accept" || action.method === "enhancements.undo") {
          task = await bounded(this.io.read(proposal.task_id));
          if (task.id !== proposal.task_id || task.revision <= Number(action.input.expected_task_revision)) {
            throw new WorkbenchError("protocol_error", "Task confirmation does not match the enhancement.");
          }
        }
        if (action.method === "enhancements.create" && proposal.state === "pending") {
          this.pending = {
            method: "enhancements.generate",
            input: {
              id: proposal.id,
              request_id: newID(),
              expected_revision: proposal.revision,
              ...selectionWire(this.selection()),
            },
            taskID: action.taskID,
            result: null,
          };
          continue;
        }
        this.pending = null;
        return { proposal, task };
      }
      throw new WorkbenchError("protocol_error", "Enhancement did not finish its start sequence.");
    } catch (cause) {
      if (cause instanceof WorkbenchError && !["transport_error", "worker_error", "store_error", "protocol_error"].includes(cause.code) && !(cause.code === "busy" && this.pending?.method === "enhancements.generate")) this.pending = null;
      throw cause;
    } finally { this.running = false; }
  }
}

export interface QuickEnhanceStart {
  task: Task;
  proposal: Enhancement;
}

export function createEnhancementInput(
  task: Task,
  fields: readonly EnhancementField[],
  provider: string,
  model: string,
  ids: { id: string; requestId: string },
): Record<string, unknown> | null {
  const allowed = enhanceableFields(task);
  const requested = [...new Set(fields)].filter((field) => allowed.includes(field));
  if (!requested.length) return null;
  const p = provider.trim();
  const m = model.trim();
  if (!p || p.length > 128 || !m || m.length > 512 || /[\u0000-\u001f\u007f]/.test(p + m)) return null;
  if (!ids.id || !ids.requestId) return null;
  return {
    id: ids.id,
    request_id: ids.requestId,
    expected_revision: 0,
    task_id: task.id,
    source_revision: task.revision,
    fields: requested,
    provider: p,
    model: m,
  };
}

export async function startQuickEnhance(
  task: Task,
  fields: readonly EnhancementField[],
  configuration: EnhancementConfiguration,
  action = new EnhancementAction(),
): Promise<QuickEnhanceStart> {
  const gate = canQuickEnhance(task, configuration, null);
  if (!gate.ok) throw new Error(gate.reason);
  const requested = fields.filter((field) => gate.fields.includes(field));
  const input = createEnhancementInput(task, requested, configuration.provider, configuration.model, {
    id: newID(),
    requestId: newID(),
  });
  if (!input) throw new Error("Nothing is available to enhance.");
  const { proposal } = await action.run("enhancements.create", input, task.id);
  return { task, proposal };
}

/**
 * One Apple Intelligence enhancement, start to finish.
 *
 * The reason this is not a branch inside `startQuickEnhance` is the shape of
 * the lifecycle, not the shape of the code. A Manvi enhancement is *handed
 * off*: `create` then `generate`, and the sidecar completes it minutes later
 * while the UI polls. An Apple Intelligence enhancement is *run here*: create,
 * generate on this thread, complete. Sharing the entry point would mean one
 * function whose second half is dead for one of its callers.
 *
 * What is shared is everything that matters — the proposal is a real record in
 * the local store with an id, a revision and a source revision, so accepting,
 * undoing, the history drawer and the field locks all work without knowing
 * which engine wrote the text.
 *
 * A failure is written back as a failed proposal before it is rethrown. A
 * generation that vanished without a trace would leave a `pending` record
 * holding the "one live attempt" lock until its lease expired.
 */
export async function runAppleEnhancement(
  task: Task,
  fields: readonly EnhancementField[],
  source: { kind: "draft" | "improve" | "extract"; notes: string; title: string; description: string; context: string },
  io = { create: changeEnhancement, complete: completeEnhancement, draft: draftWithApple },
): Promise<Enhancement> {
  const requested = [...new Set(fields)].filter((field) => enhanceableFields(task).includes(field));
  const input = createEnhancementInput(task, requested, APPLE_ENGINE, APPLE_MODEL, {
    id: newID(),
    requestId: newID(),
  });
  if (!input) throw new WorkbenchError("invalid_input", "Nothing is available to enhance.");
  const proposal = await bounded(io.create("enhancements.create", input));
  if (proposal.id !== input.id || proposal.task_id !== task.id || proposal.source_revision !== task.revision) {
    throw new WorkbenchError("protocol_error", "Enhancement confirmation does not match the request.");
  }
  const identity = { id: proposal.id, request_id: newID(), expected_revision: proposal.revision };
  try {
    const draft = await io.draft({
      kind: source.kind,
      fields: [...requested],
      notes: source.notes,
      title: source.title,
      description: source.description,
      context: source.context,
    });
    const completed = await bounded(io.complete({
      ...identity,
      ...(draft.title !== null && requested.includes("title") ? { title: draft.title } : {}),
      ...(draft.description !== null && requested.includes("description") ? { description: draft.description } : {}),
      ...(draft.rationale ? { rationale: draft.rationale } : {}),
    }));
    if (completed.id !== proposal.id || completed.revision <= proposal.revision) {
      throw new WorkbenchError("protocol_error", "Enhancement completion does not match the request.");
    }
    return completed;
  } catch (cause) {
    const message = explainAppleError(cause);
    // Best effort: the record has a 180-second lease and will expire on its
    // own, so a failed cleanup must not replace the real error with its own.
    try {
      await bounded(io.complete({ ...identity, request_id: newID(), failure: message.slice(0, 2048) }));
    } catch { /* the original failure is the one worth reporting */ }
    throw cause instanceof WorkbenchError ? cause : new WorkbenchError(appleErrorCode(cause), message);
  }
}

export function acceptEnhancementInput(
  proposal: Enhancement,
  task: Task,
  fields: readonly EnhancementField[],
  requestId: string,
): Record<string, unknown> | null {
  if (proposal.state !== "ready" || proposal.task_id !== task.id || proposal.source_revision !== task.revision) return null;
  if (typeof requestId !== "string" || !requestId || requestId.length > 128) return null;
  const allowed = enhanceableFields(task);
  const requested = [...new Set(fields)].filter((field) => proposal.fields.includes(field) && allowed.includes(field));
  if (!requested.length) return null;
  return {
    id: proposal.id,
    request_id: requestId,
    expected_revision: proposal.revision,
    expected_task_revision: task.revision,
    fields: requested,
  };
}
