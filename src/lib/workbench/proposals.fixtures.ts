import type { Task } from "./client";

export const task: Task = { id: "t1", revision: 4, updated_at: 1, title: "Ship exporter", description: "d", acceptance_criteria: ["Existing"], kind: "feature", status: "inbox", priority: 2, severity: null,
  owner: null, due_at: null, labels: [], repository_ids: ["r"], primary_repository_id: "r", home_workspace_id: null, position: 1, archived: false, completed_at: null, checklist: [], links: [] };
export const refine = (over: Record<string, unknown> = {}) => ({ ok: true, applied: false, enhancement_id: "enh-1", task_id: "t1", request_id: "q1", source_revision: 4, provider: "local", model: "m",
  proposal: { acceptance_criteria: [{ text: "Existing", cites: [] }, { text: "Exports CSV", cites: ["devmap:Exporter"] }], split_into: [{ title: "Indexer", description: "part", cites: [] }], duplicate_of: [{ id: "t9", reason: "same", cites: [] }], rationale: "why" },
  dropped: [{ kind: "duplicate_of", ref: "zz", reason: "not on the board" }], grounding: { status: "grounded", index_fresh: true, symbols: [] }, limits: {}, ...over });
/** The proposal as dc-store holds it (`enhancements.get`), at the revision a review conditions the accept on. */
export const storedRefine = (over: Record<string, unknown> = {}) => ({ ok: true, item: { id: "enh-1", revision: 2, updated_at: 1, kind: "refine", state: "ready", task_id: "t1", source_revision: 4, fields: ["acceptance_criteria", "split_into", "duplicate_of"],
  provider: "local", model: "m", automatic: false, created_at: 1, expires_at: 2, proposed: { acceptance_criteria: ["Exports CSV"], split_into: [{ title: "Indexer", description: "part" }], duplicate_of: [{ id: "t9", reason: "same" }] }, ...over } });
