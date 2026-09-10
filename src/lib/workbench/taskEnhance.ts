import {
  changeEnhancement,
  enhancementConfiguration,
  getEnhancement,
  getTask,
  listEnhancements,
  newID,
  type Enhancement,
  type EnhancementConfiguration,
  type EnhancementField,
  type EnhancementSummary,
  type Task,
} from "./client";
import { canQuickEnhance, enhanceableFields } from "./taskOrganize";

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
  if (!p || p.length > 128 || !m || m.length > 512) return null;
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

export async function loadQuickEnhance(
  taskId: string,
): Promise<{
  task: Task;
  configuration: EnhancementConfiguration;
  entries: EnhancementSummary[];
  proposal: Enhancement | null;
}> {
  const [task, configuration, page] = await Promise.all([
    getTask(taskId),
    enhancementConfiguration(),
    listEnhancements(taskId),
  ]);
  const proposal = page.items[0] ? await getEnhancement(page.items[0].id) : null;
  return { task, configuration, entries: page.items, proposal };
}

export async function startQuickEnhance(
  task: Task,
  fields: readonly EnhancementField[],
  configuration: EnhancementConfiguration,
): Promise<QuickEnhanceStart> {
  const gate = canQuickEnhance(task, configuration, null);
  if (!gate.ok) throw new Error(gate.reason);
  const requested = fields.filter((field) => gate.fields.includes(field));
  const input = createEnhancementInput(task, requested.length ? requested : gate.fields, configuration.provider, configuration.model, {
    id: newID(),
    requestId: newID(),
  });
  if (!input) throw new Error("Nothing is available to enhance.");
  let proposal = await changeEnhancement("enhancements.create", input);
  if (proposal.state === "pending") {
    proposal = await changeEnhancement("enhancements.generate", {
      id: proposal.id,
      request_id: newID(),
      expected_revision: proposal.revision,
    });
  }
  return { task, proposal };
}

export function reviewable(proposal: Enhancement | null): boolean {
  return proposal?.state === "ready";
}

export function acceptEnhancementInput(
  proposal: Enhancement,
  task: Task,
  fields: readonly EnhancementField[],
  requestId: string,
): Record<string, unknown> | null {
  if (proposal.state !== "ready") return null;
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
