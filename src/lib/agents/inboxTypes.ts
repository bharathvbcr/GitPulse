/**
 * Wire types for the Agent Coordination Bus and Vespin-modeled Agent Inbox IPC surface.
 *
 * Checked bidirectionally against Rust serde definitions by `scripts/check-coverage-types.mjs`:
 * - `src-tauri/src/engine/agent_bus.rs`: `AgentLaneClaim`, `AgentBusMessage`, `AgentInboxThread`
 * - `src-tauri/src/commands/worktrees.rs`: `InboxSnapshot`, `InboxMessageReceipt`
 * - `src-tauri/src/workbench.rs`: `InboxDecomposeSubtask`, `InboxDecomposeReceipt`
 */

export interface AgentLaneClaim {
  agent_id: string;
  branch: string;
  worktree_path: string | null;
  sparse_cone: string[];
  claimed_files: string[];
  claimed_symbols: string[];
  updated_at_ms: number;
}

export interface AgentBusMessage {
  seq: number;
  from_agent: string;
  to_agent: string | null;
  kind: string;
  body: string;
  branch: string | null;
  created_at_ms: number;
  thread_key?: string;
  msg_id?: string;
  read_by?: string[];
  hop?: number;
  max_hops?: number;
}

export type BusMessage = AgentBusMessage;

export interface AgentInboxThread {
  thread_key: string;
  title: string;
  status: "open" | "answered" | "withdrawn" | "settled" | string;
  ask_type: "decide" | "do" | "answer" | "review" | string | null;
  latest_msg: AgentBusMessage;
  messages: AgentBusMessage[];
  from_agent: string;
  to_agent: string | null;
  updated_at_ms: number;
}

export interface InboxSnapshot {
  ok: boolean;
  repo_path: string;
  threads: AgentInboxThread[];
  lanes: AgentLaneClaim[];
  total: number;
}

export interface InboxMessageReceipt {
  ok: boolean;
  thread_key: string | null;
  message: AgentBusMessage;
}

export interface InboxDecomposeSubtask {
  item_id: string;
  title: string;
  status: string;
}

export interface InboxDecomposeReceipt {
  ok: boolean;
  item_id: string;
  thread_key: string;
  subtasks: InboxDecomposeSubtask[];
  inbox_message: AgentBusMessage;
}

export interface StructuredAsk {
  ask_type: "decide" | "do" | "answer" | "review";
  title?: string;
  question?: string;
  prompt?: string;
  options?: string[];
  recommendation?: string;
  default_option?: number;
  steps?: string[];
  action?: string;
  notes?: string;
  branch?: string;
  target?: string;
  summary?: string;
  diff?: string;
  task_id?: string;
}
