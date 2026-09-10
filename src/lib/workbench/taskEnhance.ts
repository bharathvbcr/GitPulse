import {
  changeEnhancement,
  getTask,
  newID,
  WorkbenchError,
  type Enhancement,
  type EnhancementConfiguration,
  type EnhancementField,
  type EnhancementMutation,
  type ModelSelection,
  type Task,
} from "./client";
import { canQuickEnhance, enhanceableFields } from "./taskOrganize";
import { bounded } from "./taskActions";
import { selectionWire } from "./taskModel";

export const liveEnhancement = (proposal: Pick<Enhancement, "state"> | null): boolean =>
  proposal !== null && ["pending", "running", "cancel_requested"].includes(proposal.state);

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
