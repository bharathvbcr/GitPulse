/**
 * Pure agent worktree layout detection, provider vocabulary, and attention urgency rules.
 * Zero Svelte, DOM, or Tauri dependencies.
 */

export const AGENT_PROVIDERS = ["claude", "codex", "grok", "agy"] as const;
export type AgentProvider = (typeof AGENT_PROVIDERS)[number];

export const PROVIDER_LABELS: Record<AgentProvider, string> = {
  claude: "Claude Code",
  codex: "Codex",
  grok: "Grok",
  agy: "Antigravity",
};

export function asAgentProvider(value: unknown): AgentProvider | null {
  return AGENT_PROVIDERS.find((provider) => provider === value) ?? null;
}

export interface AgentLayout {
  kind: string;
  slug: string;
}

const WORKTREES_SEGMENT = "worktrees";
const GITPULSE_LANE_KIND = "gitpulse";

function normalisedSegments(path: string): string[] {
  return path.split(/[\\/]+/).filter(Boolean);
}

function agentDirectoryName(segment: string): string {
  if (!segment.startsWith(".")) return "";
  const name = segment.slice(1);
  if (!name || name.startsWith(".")) return "";
  if (name.toLowerCase() === "git") return "";
  return name;
}

export function agentLayout(path: string): AgentLayout | null {
  if (!path) return null;
  const segments = normalisedSegments(path);
  for (let i = 0; i + 1 < segments.length; i += 1) {
    const kind = agentDirectoryName(segments[i]);
    if (!kind || segments[i + 1] !== WORKTREES_SEGMENT) continue;
    return { kind, slug: segments[i + 2] ?? "" };
  }
  return null;
}

export function isAgentWorktree(path: string): boolean {
  return agentLayout(path) !== null;
}

export function agentKind(path: string): string {
  return agentLayout(path)?.kind ?? "";
}

export function isWaylodeLane(kind: string): boolean {
  return kind.toLowerCase() === GITPULSE_LANE_KIND;
}

export function agentKindLabel(kind: string): string {
  const trimmed = kind.trim();
  if (!trimmed) return "";
  if (isWaylodeLane(trimmed)) return "Waylode task";
  const provider = asAgentProvider(trimmed.toLowerCase());
  if (provider) return PROVIDER_LABELS[provider];
  return trimmed
    .split(/[-_\s]+/)
    .filter(Boolean)
    .map((word) => word.charAt(0).toUpperCase() + word.slice(1))
    .join(" ");
}

export function agentSessionSlug(path: string): string {
  return agentLayout(path)?.slug ?? "";
}

export type AttentionUrgency = "blocking" | "error" | "informational" | "clear";
export type SessionAttentionUrgency = Exclude<AttentionUrgency, "clear">;
export type AttentionKind = "needs-you" | "finished" | "error" | "signalled";

export const ATTENTION_EVENTS = {
  permission_prompt: { kind: "needs-you", urgency: "blocking", label: "Needs your permission" },
  permission_request: { kind: "needs-you", urgency: "blocking", label: "Needs your permission" },
  idle_prompt: { kind: "finished", urgency: "informational", label: "Waiting for you" },
  agent_needs_input: { kind: "needs-you", urgency: "blocking", label: "Needs your input" },
  elicitation_dialog: { kind: "needs-you", urgency: "blocking", label: "Asking you a question" },
  elicitation_url_dialog: { kind: "needs-you", urgency: "blocking", label: "Asking you to open a link" },
  agent_completed: { kind: "finished", urgency: "informational", label: "Finished its work" },
  turn_finished: { kind: "finished", urgency: "informational", label: "Finished its turn" },
  error: { kind: "error", urgency: "error", label: "Stopped on an error" },
  prompt_submitted: { kind: "clear", urgency: "clear", label: "Working again" },
  tool_finished: { kind: "clear", urgency: "clear", label: "Working again" },
  session_ended: { kind: "clear", urgency: "clear", label: "Session ended" },
} as const satisfies Record<
  string,
  { kind: AttentionKind | "clear"; urgency: AttentionUrgency; label: string }
>;

export interface SessionAttention {
  kind: AttentionKind;
  urgency?: SessionAttentionUrgency;
  event?: string | null;
  channel?: string;
  label: string;
  detail: string | null;
  at: number;
}

const VALID_STANDING_URGENCIES = new Set<string>(["blocking", "error", "informational"]);

export function classifyAttentionUrgency(
  target: SessionAttention | AttentionKind | null | undefined,
  bellAsks = false,
): SessionAttentionUrgency | null {
  if (!target) return null;
  if (typeof target === "string") {
    if (target === "needs-you") return "blocking";
    if (target === "error") return "error";
    if (target === "finished") return "informational";
    if (target === "signalled") return bellAsks ? "blocking" : "informational";
    return null;
  }
  if (target.kind === "signalled") {
    if (target.urgency === "blocking" || target.urgency === "error") return target.urgency;
    return bellAsks ? "blocking" : "informational";
  }
  if (target.urgency && VALID_STANDING_URGENCIES.has(target.urgency)) {
    return target.urgency;
  }
  if (target.event && Object.hasOwn(ATTENTION_EVENTS, target.event)) {
    const mapped = ATTENTION_EVENTS[target.event as keyof typeof ATTENTION_EVENTS].urgency;
    return mapped === "clear" ? null : mapped;
  }
  return classifyAttentionUrgency(target.kind, bellAsks);
}

export function asksForReader(
  kindOrAttention: SessionAttention | AttentionKind | null | undefined,
  bellAsks = false,
): boolean {
  const urgency = classifyAttentionUrgency(kindOrAttention, bellAsks);
  return urgency === "blocking" || urgency === "error";
}
