/**
 * Mavi's typed model. Components read these shapes only; the agent's wire
 * format (Claude Code stream-json) stays inside `reduce.ts`.
 */

/** Where a thread's agent runs (`Target` in src-tauri/src/mavi/session.rs). */
export type MaviTarget =
  | { readonly kind: "local"; readonly root: string }
  | { readonly kind: "ssh"; readonly host: string; readonly dir: string };

/** The host's phase for a thread (`Phase` in session.rs). */
export type MaviPhase = "ready" | "idle" | "working" | "approval" | "ended";

/** `Summary` in session.rs: what the host keeps without a replay. */
export interface MaviSummary {
  readonly id: string;
  readonly target: MaviTarget;
  readonly mode: string;
  readonly model: string | null;
  readonly phase: MaviPhase;
  readonly conversation_id: string | null;
  readonly pending: number;
  readonly turns: number;
  readonly cost_usd: number;
  readonly touched: string[];
  readonly background_tasks: number;
  readonly started_at_ms: number;
  readonly last_activity_ms: number;
  readonly exit: string | null;
  /** Ended cleanly: exit code 0, or stopped on the person's request. */
  readonly exit_ok: boolean;
  /** How the last turn ended: "ok", "failed" or "interrupted"; null before any. */
  readonly last_turn: string | null;
}

export interface MaviEntry {
  readonly seq: number;
  readonly line: string;
}

/** `MaviReplay` in src-tauri/src/mavi/mod.rs (`cmd_mavi_events`). */
export interface MaviReplay {
  readonly entries: MaviEntry[];
  readonly next_seq: number;
  readonly summary: MaviSummary;
}

/** `MaviFileDiff` in mod.rs (`cmd_mavi_file_diff`). */
export interface MaviFileDiff {
  readonly patch: string;
  readonly truncated: boolean;
}

/** `MaviProbeKind` in src-tauri/src/mavi/ssh.rs. */
export type MaviProbeKind =
  | "ok"
  | "auth_failed"
  | "host_key"
  | "unreachable"
  | "no_such_dir"
  | "agent_missing"
  | "not_a_repo"
  | "timeout"
  | "ssh_missing"
  | "failed";

/** `MaviProbe` in ssh.rs (`cmd_mavi_ssh_probe`). */
export interface MaviProbe {
  readonly kind: MaviProbeKind;
  readonly message: string;
  readonly root: string | null;
  readonly millis: number;
}

/** `MaviTranscriptState` in src-tauri/src/mavi/history.rs. */
export type MaviTranscriptState = "found" | "elsewhere" | "absent" | "unknown";

/** `MaviConversationCheck` in history.rs (`cmd_mavi_conversation_check`). */
export interface MaviConversationCheck {
  readonly state: MaviTranscriptState;
  readonly reason: string | null;
  /** The canonical worktree root it would resume in, when found there. */
  readonly root: string | null;
}

/** The `mavi-event` payload: a batch of one thread's lines. */
export interface MaviEventPayload {
  readonly id: string;
  readonly entries: MaviEntry[];
}

/** One hunk of an edit, as Claude Code reports it in `structuredPatch`. */
export interface MaviHunk {
  readonly oldStart: number;
  readonly oldLines: number;
  readonly newStart: number;
  readonly newLines: number;
  /** Each line starts with " ", "-" or "+". */
  readonly lines: readonly string[];
}

export type ToolStatus = "running" | "ok" | "error" | "denied" | "interrupted";

export type ApprovalState = "open" | "allowed" | "denied" | "void";

export interface UserItem {
  readonly kind: "user";
  readonly id: string;
  readonly turn: number;
  text: string;
  at: number | null;
}

export interface AssistantItem {
  readonly kind: "assistant";
  readonly id: string;
  readonly turn: number;
  readonly messageId: string;
  text: string;
  /** Still receiving deltas. */
  streaming: boolean;
  /** The complete block has arrived (not just deltas). */
  final: boolean;
}

export interface ThinkingItem {
  readonly kind: "thinking";
  readonly id: string;
  readonly turn: number;
  readonly messageId: string;
  streaming: boolean;
  final: boolean;
  /** Visible reasoning text, when the model shares any (often empty). */
  text: string;
}

export interface ToolItem {
  readonly kind: "tool";
  /** The agent's `tool_use` id. */
  readonly id: string;
  readonly turn: number;
  name: string;
  input: Record<string, unknown>;
  /** Partial input JSON while it streams. */
  partialInput: string;
  status: ToolStatus;
  result: string;
  /** Exact hunks for an edit, from `tool_use_result.structuredPatch`. */
  patch: MaviHunk[] | null;
  /** Subagent tool calls name the tool call they belong to. */
  parentId: string | null;
  approval: { requestId: string; state: ApprovalState; reason: string | null } | null;
  /** Running in the background after its call returned. */
  background: boolean;
}

export interface NoticeItem {
  readonly kind: "notice";
  readonly id: string;
  readonly turn: number;
  readonly tone: "info" | "warn" | "error";
  readonly text: string;
}

export interface TurnSummary {
  /** `post_turn_summary.status_category`: completed, review_ready, blocked… */
  readonly category: string;
  readonly detail: string;
  readonly needsAction: string;
}

export interface TurnEndItem {
  readonly kind: "turn_end";
  readonly id: string;
  readonly turn: number;
  /** `result.subtype`: success, error_during_execution, error_max_turns… */
  readonly subtype: string;
  readonly ok: boolean;
  readonly interrupted: boolean;
  readonly durationMs: number | null;
  /** This turn's cost: the cumulative total minus the previous total. */
  readonly costUsd: number | null;
  summary: TurnSummary | null;
  /** An earlier turn read from the transcript: no duration or cost is known. */
  readonly history: boolean;
}

export type TimelineItem = UserItem | AssistantItem | ThinkingItem | ToolItem | NoticeItem | TurnEndItem;

/** A path that moved in the worktree during a turn (`Moved` in worktree.rs). */
export interface MovedPath {
  readonly path: string;
  readonly code: string;
}

export interface TurnFiles {
  readonly available: boolean;
  readonly partial: boolean;
  readonly moved: readonly MovedPath[];
  readonly reason: string | null;
}

export interface ThreadState {
  items: TimelineItem[];
  /** The newest turn number seen. */
  turn: number;
  conversationId: string | null;
  model: string | null;
  permissionMode: string | null;
  cwd: string | null;
  agentVersion: string | null;
  /** The agent's one-line account of what it is doing now. */
  activity: string | null;
  /** Cumulative cost the agent last reported. */
  costUsd: number;
  backgroundTasks: string[];
  turnFiles: Record<number, TurnFiles>;
  stderr: string[];
  ended: string | null;
  lastSeq: number;
}
