import type { Task } from "./client";

export const FP = "0123456789abcdef";
export const proposal = { fingerprint: FP, category: "dead_symbols", status: "new", title: "Remove quuxify", description: "No callers.", kind: "maintenance", priority: 2, labels: ["manvi-scan"],
  signals: [{ source: "devmap dead_symbols", result: "confidence 0.9" }, { source: "dcgrep -uu", result: "no outside reference" }] };
export const view = (over: Record<string, unknown> = {}) => ({ scan_id: "scan-1", state: "done", repo_root: "/r", max_proposals: 25, started_at: 1, complete: true, board_complete: true,
  new_count: 1, attached_count: 0, over_cap: 0, coverage: [{ category: "dead_symbols", status: "ok" }], proposals: [proposal], ...over });
export const task: Task = { id: "t1", revision: 4, updated_at: 1, title: "Ship exporter", description: "d", acceptance_criteria: ["Existing"], kind: "feature", status: "inbox", priority: 2, severity: null,
  owner: null, due_at: null, labels: [], repository_ids: ["r"], primary_repository_id: "r", home_workspace_id: null, position: 1, archived: false, completed_at: null, checklist: [], links: [] };
export const refine = (over: Record<string, unknown> = {}) => ({ ok: true, applied: false, task_id: "t1", request_id: "q1", source_revision: 4, provider: "local", model: "m",
  proposal: { acceptance_criteria: [{ text: "Existing", cites: [] }, { text: "Exports CSV", cites: ["devmap:Exporter"] }], split_into: [{ title: "Indexer", description: "part", cites: [] }], duplicate_of: [{ id: "t9", reason: "same", cites: [] }], rationale: "why" },
  dropped: [{ kind: "duplicate_of", ref: "zz", reason: "not on the board" }], grounding: { status: "grounded", index_fresh: true, symbols: [] }, limits: {}, ...over });
