/**
 * The workbench's closed vocabularies: task statuses, agent permission modes,
 * and how a run connects.
 *
 * Plain constants with no imports, so anything may read them. `client.ts`
 * re-exports every name here and remains the import each existing caller
 * uses; this file exists because `interfaceStore`, `ui/taskView` and
 * `taskHandoff` need the lists but must not pull `@tauri-apps/api` into the
 * preference layer, which loads before any IPC exists. Splitting the
 * constants is cheaper than a second hand-written copy of them.
 */

export const STATUSES = ["inbox", "backlog", "ready", "in_progress", "review", "done"] as const;

export type TaskStatus = (typeof STATUSES)[number];

export const STATUS_LABELS: Record<TaskStatus, string> = {
  inbox: "Inbox",
  backlog: "Backlog",
  ready: "Ready",
  in_progress: "In progress",
  review: "Review",
  done: "Done",
};

/** A status from untrusted input (stored preference, wire payload), or null. */
export function asTaskStatus(value: unknown): TaskStatus | null {
  return STATUSES.find((status) => status === value) ?? null;
}

/**
 * How much an agent may do without asking. Ordered least to most permissive;
 * `bypass` turns the provider's permission checks and sandbox off and is the
 * only one that needs a per-launch acknowledgement.
 */
export const PERMISSION_MODES = ["inspect", "ask", "edit", "auto_review", "preapproved", "bypass"] as const;
export type PermissionMode = (typeof PERMISSION_MODES)[number];

/**
 * Interactive CLIs GitPulse can hand a saved task to.
 *
 * Managed runs remain Codex-only: that connection is Manvi supervising a
 * Codex session, not a property of every agent CLI.
 */
export const AGENT_PROVIDERS = ["claude", "codex", "grok", "agy"] as const;
export type AgentProvider = (typeof AGENT_PROVIDERS)[number];

/** A provider from untrusted input (stored preference, wire payload), or null. */
export function asAgentProvider(value: unknown): AgentProvider | null {
  return AGENT_PROVIDERS.find((provider) => provider === value) ?? null;
}

/** A provider terminal GitPulse opens, or a connection GitPulse supervises. */
export type RunKind = "external_terminal" | "managed";
