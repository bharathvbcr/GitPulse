import type { PolicyVerdict } from "../stores/harnessStore";

/**
 * A record of one guarded action GitPulse performed.
 *
 * When an agent drives the client, actions arrive faster than anyone watches;
 * the journal is what makes "what just happened to my repository" answerable
 * after the fact, including which actions ran with no policy gate at all.
 */
export interface AgentActionEntry {
  id: number;
  ts: number;
  /** Coarse verb: commit, push, rebase, stage, discard, edit, worktree… */
  kind: string;
  /** Human-readable target: branch name, file path, remote… */
  label: string;
  ok: boolean;
  /** The policy decision it ran under, when one exists. */
  verdict: PolicyVerdict | null;
}

/**
 * How many journal entries the UI holds for display.
 *
 * This was the ring-buffer size — the hard limit on how much history existed
 * at all, and everything past it was gone. The durable ledger is now the
 * record, so this is a *display* cap: rows beyond it are on disk and reachable
 * by paging `cmd_ledger_tail`.
 */
export const MAX_AGENT_ACTIONS = 200;

let nextId = 1;

export function makeAgentAction(
  input: { kind: string; label: string; ok: boolean; verdict?: AgentActionEntry["verdict"] },
  now: number = Date.now()
): AgentActionEntry {
  const { kind, label, ok, verdict = null } = input;
  return { id: nextId++, ts: now, kind, label, ok, verdict };
}

/** Immutable append that keeps the newest entries past the cap. */
export function appendAction(
  list: AgentActionEntry[],
  entry: AgentActionEntry
): AgentActionEntry[] {
  const next = list.length >= MAX_AGENT_ACTIONS ? list.slice(1) : list.slice();
  next.push(entry);
  return next;
}

/** Maps an invoked Tauri command to a journal verb. Unknown names pass as-is. */
export function actionKindForCommand(cmd: string): string {
  const stripped = cmd.startsWith("cmd_") ? cmd.slice(4) : cmd;
  switch (stripped) {
    case "commit":
    case "quick_commit":
      return "commit";
    case "push":
      return "push";
    case "pull":
      return "pull";
    case "fetch":
      return "fetch";
    case "merge_branch":
      return "merge";
    case "rebase_interactive":
    case "restack":
      return "rebase";
    case "checkout_branch":
      return "checkout";
    case "create_branch":
      return "branch";
    case "delete_branch":
      return "branch-delete";
    case "rename_branch":
      return "branch-rename";
    case "stage_file":
    case "stage_selective_patch":
      return "stage";
    case "unstage_file":
      return "unstage";
    case "discard_changes":
      return "discard";
    case "stash_save":
      return "stash";
    case "stash_pop":
    case "stash_action":
      return "unstash";
    // Every mutating surface must land in the journal under a verb a human
    // recognises. An unmapped command falls through to its raw name, which is
    // legible but inconsistent — and the journal is the record of what an
    // unattended agent did to the repository.
    case "cherry_pick":
      return "cherry-pick";
    case "revert":
      return "revert";
    case "reset":
      return "reset";
    case "repo_operation_action":
      return "operation";
    case "remote_change":
      return "remote";
    case "submodule_change":
      return "submodule";
    case "add_worktree":
    case "remove_worktree":
      return "worktree";
    case "write_file_content":
      return "edit";
    case "terminal_run":
      return "terminal";
    default:
      return stripped || "action";
  }
}
