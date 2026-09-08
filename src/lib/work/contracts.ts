/** Validate the fields Overview consumes at the IPC boundary. */
const MAX_RECORDS = 10_000;
const MAX_TEXT = 65_536;

function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("expected an object");
  return value as Record<string, unknown>;
}
function text(value: unknown): void {
  if (typeof value !== "string" || value.length > MAX_TEXT) throw new Error("invalid or oversized text");
}
function texts(value: unknown): void { list(value, text); }
function list(value: unknown, check: (item: unknown) => void): void {
  if (!Array.isArray(value)) throw new Error("expected a list");
  if (value.length > MAX_RECORDS) throw new Error(`response exceeds ${MAX_RECORDS} records`);
  for (const item of value) check(item);
}
function fields(value: unknown, names: readonly string[]): Record<string, unknown> {
  const item = record(value);
  for (const name of names) text(item[name]);
  return item;
}
function count(value: unknown): void {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) throw new Error("invalid count");
}
function nullableText(value: unknown): void { if (value != null) text(value); }
function flag(value: unknown): void { if (typeof value !== "boolean") throw new Error("invalid flag"); }

/** Throws only into the loader's per-source error channel. Never repairs data into a pass. */
export function validateWorkResponse(command: string, value: unknown): void {
  try {
    switch (command) {
      case "cmd_list_worktrees":
        list(value, entry => {
          const w = fields(entry, ["path", "name"]);
          if (!w.path) throw new Error("empty worktree path");
          nullableText(w.branch);
          flag(w.is_bare);
        });
        break;
      case "cmd_ledger_tail":
        list(value, entry => {
          const e = record(entry);
          count(e.id);
          nullableText(e.task_id); nullableText(e.worktree_path); nullableText(e.verdict_json);
        });
        break;
      case "cmd_task_view": {
        const v = record(value); flag(v.available); nullableText(v.error);
        list(v.leases, entry => {
          const l = fields(entry, ["task_id", "owner", "status"]);
          nullableText(l.agent); nullableText(l.expires_at);
        });
        break;
      }
      case "cmd_github_context": {
        const v = record(value); flag(v.available); nullableText(v.error); nullableText(v.runs_error);
        if (v.warnings != null) texts(v.warnings);
        list(v.pull_requests, entry => {
          const pr = fields(entry, ["title", "head_ref", "url", "ci_status", "review_decision"]);
          count(pr.number); flag(pr.is_draft);
        });
        list(v.workflow_runs, entry => {
          const run = fields(entry, ["name", "title", "head_branch", "status", "conclusion", "url", "created_at"]);
          count(run.id);
        });
        break;
      }
      case "cmd_grants_view": {
        const v = record(value); flag(v.available); nullableText(v.error);
        list(v.grants, entry => { fields(record(entry).scope, ["task_id"]); });
        break;
      }
      case "cmd_worktree_task": nullableText(value); break;
      case "cmd_task_scope": if (value != null) fields(value, ["title"]); break;
      case "cmd_repo_operation": {
        if (value === null) break;
        const op = fields(value, ["kind"]);
        if (!["Merge", "Rebase", "RebaseApply", "ApplyMailbox", "CherryPick", "Revert", "Bisect"].includes(String(op.kind))) throw new Error("unknown operation kind");
        count(op.conflicted_total);
        for (const key of ["current_step", "total_steps"]) if (op[key] != null) count(op[key]);
        break;
      }
      case "cmd_collision_risk": {
        const risk = record(value); flag(risk.ok); flag(risk.truncated); text(risk.error);
        for (const name of ["overlapping_files", "worktrees_involved", "scanned_worktrees", "unscanned_worktrees", "failed_worktrees"]) count(risk[name]);
        list(risk.items, entry => {
          const item = fields(entry, ["path"]);
          list(item.worktrees, party => { const p = fields(party, ["path", "agent_kind"]); nullableText(p.branch); });
        });
        break;
      }
    }
  } catch (error) {
    throw new Error(`${command}: invalid response (${error instanceof Error ? error.message : "unknown shape"})`);
  }
}
