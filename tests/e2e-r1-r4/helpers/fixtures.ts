import type { PlaneProbe, PlaneTask, PlaneTerminal, TaskProbe } from "../../../src/lib/agents/plane";
import type { MenuState } from "../../../src/lib/desktop/menuState";
import type { InsightsSnapshot } from "../../../src/lib/insights/types";
import type { FileStatus, RepoOperation } from "../../../src/lib/repos/storeTypes";

// ---------------------------------------------------------------------------
// 1. Claude Code Hook Payloads
// ---------------------------------------------------------------------------

export type ClaudeNotificationMatcher =
  | "permission_prompt"
  | "idle_prompt"
  | "agent_needs_input"
  | "agent_completed"
  | "elicitation_dialog"
  | "elicitation_url_dialog";

export interface ClaudeHookPayloadOptions {
  session_id?: string;
  cwd?: string;
  message?: string;
  tool_name?: string;
  tool_input?: Record<string, unknown>;
  last_assistant_message?: string;
  error?: string;
  agent_id?: string;
}

const DEFAULT_UUID = "52d6abcd-6965-454b-a465-56a4a88526e7";
const DEFAULT_CWD = "/tmp/gitpulse-e2e-repo";

export function claudePermissionRequest(opts: ClaudeHookPayloadOptions = {}) {
  return {
    hook_event_name: "PermissionRequest",
    session_id: opts.session_id ?? DEFAULT_UUID,
    cwd: opts.cwd ?? DEFAULT_CWD,
    tool_name: opts.tool_name ?? "Bash",
    tool_input: opts.tool_input ?? { command: "git status" },
    message: opts.message ?? "Allow Bash tool to run git status?",
    ...(opts.agent_id ? { agent_id: opts.agent_id } : {}),
  };
}

export function claudeNotification(
  matcher: ClaudeNotificationMatcher,
  opts: ClaudeHookPayloadOptions = {},
) {
  const defaultMessages: Record<ClaudeNotificationMatcher, string> = {
    permission_prompt: "Claude needs your permission to edit src/main.rs",
    idle_prompt: "Claude is waiting for your next instruction",
    agent_needs_input: "Claude needs your input to continue",
    agent_completed: "Claude finished its task",
    elicitation_dialog: "Choose target deployment environment",
    elicitation_url_dialog: "Authorize external tool link",
  };
  return {
    hook_event_name: "Notification",
    notification_type: matcher,
    matcher,
    session_id: opts.session_id ?? DEFAULT_UUID,
    cwd: opts.cwd ?? DEFAULT_CWD,
    message: opts.message ?? defaultMessages[matcher],
    ...(opts.agent_id ? { agent_id: opts.agent_id } : {}),
  };
}

export function claudePostToolUse(opts: ClaudeHookPayloadOptions = {}) {
  return {
    hook_event_name: "PostToolUse",
    session_id: opts.session_id ?? DEFAULT_UUID,
    cwd: opts.cwd ?? DEFAULT_CWD,
    tool_name: opts.tool_name ?? "Bash",
    tool_input: opts.tool_input ?? { command: "git status" },
    ...(opts.agent_id ? { agent_id: opts.agent_id } : {}),
  };
}

export function claudeUserPromptSubmit(opts: ClaudeHookPayloadOptions = {}) {
  return {
    hook_event_name: "UserPromptSubmit",
    session_id: opts.session_id ?? DEFAULT_UUID,
    cwd: opts.cwd ?? DEFAULT_CWD,
    prompt: opts.message ?? "Continue with the implementation",
  };
}

export function claudeStop(opts: ClaudeHookPayloadOptions = {}) {
  return {
    hook_event_name: "Stop",
    session_id: opts.session_id ?? DEFAULT_UUID,
    cwd: opts.cwd ?? DEFAULT_CWD,
    last_assistant_message: opts.last_assistant_message ?? "Turn complete.",
    ...(opts.agent_id ? { agent_id: opts.agent_id } : {}),
  };
}

export function claudeStopFailure(opts: ClaudeHookPayloadOptions = {}) {
  return {
    hook_event_name: "StopFailure",
    session_id: opts.session_id ?? DEFAULT_UUID,
    cwd: opts.cwd ?? DEFAULT_CWD,
    error: opts.error ?? "Model invocation failed: rate limit exceeded",
    ...(opts.agent_id ? { agent_id: opts.agent_id } : {}),
  };
}

export function claudeSessionEnd(opts: ClaudeHookPayloadOptions = {}) {
  return {
    hook_event_name: "SessionEnd",
    session_id: opts.session_id ?? DEFAULT_UUID,
    cwd: opts.cwd ?? DEFAULT_CWD,
  };
}

// ---------------------------------------------------------------------------
// 2. Antigravity (agy) Hook Payloads
// ---------------------------------------------------------------------------

export interface AgyHookPayloadOptions {
  conversationId?: string;
  cwd?: string;
  message?: string;
  terminationReason?: string;
  fullyIdle?: boolean;
  error?: string;
  toolName?: string;
  toolSummary?: string;
}

const DEFAULT_AGY_UUID = "91a8ef12-3456-4789-abcd-ef0123456789";

export function agySessionStart(opts: AgyHookPayloadOptions = {}) {
  return {
    hookEventName: "SessionStart",
    conversationId: opts.conversationId ?? DEFAULT_AGY_UUID,
    cwd: opts.cwd ?? DEFAULT_CWD,
  };
}

export function agyPreInvocation(opts: AgyHookPayloadOptions = {}) {
  return {
    hookEventName: "PreInvocation",
    conversationId: opts.conversationId ?? DEFAULT_AGY_UUID,
    cwd: opts.cwd ?? DEFAULT_CWD,
  };
}

export function agyPostInvocation(opts: AgyHookPayloadOptions = {}) {
  return {
    hookEventName: "PostInvocation",
    conversationId: opts.conversationId ?? DEFAULT_AGY_UUID,
    cwd: opts.cwd ?? DEFAULT_CWD,
  };
}

export function agyPostToolUse(opts: AgyHookPayloadOptions = {}) {
  return {
    hookEventName: "PostToolUse",
    conversationId: opts.conversationId ?? DEFAULT_AGY_UUID,
    cwd: opts.cwd ?? DEFAULT_CWD,
    tool_name: opts.toolName ?? "run_command",
    ...(opts.toolSummary ? { tool_summary: opts.toolSummary } : {}),
  };
}

export function agyStop(opts: AgyHookPayloadOptions = {}) {
  return {
    hookEventName: "Stop",
    conversationId: opts.conversationId ?? DEFAULT_AGY_UUID,
    cwd: opts.cwd ?? DEFAULT_CWD,
    terminationReason: opts.terminationReason ?? "NO_TOOL_CALL",
    fullyIdle: opts.fullyIdle ?? (opts.terminationReason ?? "NO_TOOL_CALL") === "NO_TOOL_CALL",
    ...(opts.message ? { message: opts.message } : {}),
    ...(opts.error ? { error: opts.error } : {}),
  };
}

export function agyNotification(opts: AgyHookPayloadOptions = {}) {
  return {
    hookEventName: "Notification",
    conversationId: opts.conversationId ?? DEFAULT_AGY_UUID,
    cwd: opts.cwd ?? DEFAULT_CWD,
    message: opts.message ?? "Antigravity needs approval for command execution",
  };
}

export function agySessionEnd(opts: AgyHookPayloadOptions = {}) {
  return {
    hookEventName: "SessionEnd",
    conversationId: opts.conversationId ?? DEFAULT_AGY_UUID,
    cwd: opts.cwd ?? DEFAULT_CWD,
  };
}

// ---------------------------------------------------------------------------
// 3. MenuState Payloads (R2)
// ---------------------------------------------------------------------------

const DEFAULT_ENABLED_ACTIONS = [
  "open",
  "clone",
  "settings",
  "shortcuts",
  "diagnostics",
  "documentation",
  "release-notes",
  "report-issue",
  "setup-tools",
  "toggle-theme",
  "theme-system",
  "theme-light",
  "theme-dark",
  "fleet",
  "agents",
  "palette",
  "refresh",
  "terminal-dock",
  "copy-branch",
  "copy-repo-path",
  "copy-commit",
  "reveal-repo",
  "open-remote",
  "tab-work",
  "tab-history",
  "tab-code",
  "tab-insights",
  "section:work:overview",
  "section:history:graph",
  "section:insights:pulse",
];

export interface MenuAgentRequestFixture {
  id: string;
  sessionId?: string;
  agent?: string;
  kind?: string;
  label?: string;
  repoPath?: string;
  repoLabel?: string;
  urgency?: string;
  event?: string;
  detail?: string;
  at?: number;
}

export type MenuStateFixture = MenuState & {
  agentRequests?: MenuAgentRequestFixture[];
  agentRequestsHidden?: number;
  agentsRunning?: number;
  agentsInformational?: number;
};

export function makeMenuState(
  overrides: Partial<MenuStateFixture> & { statusOverrides?: Partial<MenuState["status"]> } = {},
): MenuStateFixture {
  const { statusOverrides, ...rest } = overrides;
  const activePath = rest.activePath !== undefined ? rest.activePath : "/Projects/GitPulse";
  const base: MenuStateFixture = {
    enabled: [...DEFAULT_ENABLED_ACTIONS],
    checked: ["theme-dark", "tab-work", "section:work:overview"],
    labels: [
      { id: "terminal-dock", text: "Show Terminal" },
      { id: "copy-commit", text: "Copy HEAD SHA" },
    ],
    repositories: activePath
      ? [
          {
            path: activePath,
            label: "GitPulse",
            active: true,
            changed: 3,
            conflicts: 0,
            busy: false,
          },
          {
            path: "/Projects/DevCouncil",
            label: "DevCouncil",
            active: false,
            changed: 0,
            conflicts: 0,
            busy: false,
          },
        ]
      : [],
    repositoriesHidden: 0,
    activePath,
    showStatusIcon: true,
    hideDockWhenClosed: true,
    trayTitle: null,
    trayDetails: activePath
      ? [
          `Repository: ${activePath}`,
          "Branch: main",
          "3 changed · 1 staged · 0 conflicts",
          "0 listed stashes",
          "1 ahead · 0 behind origin/main (fetched just now)",
          "Live updates",
        ]
      : [],
    traySummary: {
      id: activePath ? "section:work:overview" : "open",
      text: activePath ? "3 changed · 1 staged" : "Open a repository…",
    },
    trayDetail: activePath ? "GitPulse · main" : "GitPulse",
    agentsWaiting: 0,
    status: {
      repository: "GitPulse",
      branch: activePath ? "main" : "",
      changed: activePath ? 3 : null,
      staged: activePath ? 1 : null,
      conflicts: activePath ? 0 : null,
      ahead: activePath ? 1 : null,
      behind: activePath ? 0 : null,
      upstream: activePath ? "origin/main" : null,
      headline: activePath ? "Ready to review" : "Your work, at a glance",
      tone: activePath ? "changed" : "neutral",
      primaryLabel: activePath ? "Review changes" : "Open repository",
      watchStatus: "watching",
      reduceMotion: false,
      stashes: activePath ? 0 : null,
      operation: null,
      activity: null,
      elsewhere: 0,
      fetchedAt: activePath ? Date.now() - 60_000 : null,
      ...(statusOverrides ?? {}),
    },
    ...rest,
  };
  return base;
}

export function makeMenuStateWithRequests(
  requests: MenuAgentRequestFixture[],
  opts: {
    agentsRunning?: number;
    agentsInformational?: number;
    agentRequestsHidden?: number;
    conflicts?: number;
  } = {},
): MenuStateFixture {
  const conflicts = opts.conflicts ?? 0;
  return makeMenuState({
    agentsWaiting: requests.length,
    agentRequests: requests,
    agentRequestsHidden: opts.agentRequestsHidden ?? 0,
    agentsRunning: opts.agentsRunning ?? 1,
    agentsInformational: opts.agentsInformational ?? 0,
    traySummary: {
      id: "section:work:overview",
      text: conflicts > 0 ? `Resolve ${conflicts} conflicts` : "3 changed · 1 staged",
    },
    statusOverrides: conflicts > 0
      ? {
          conflicts,
          headline: "Conflicts need your attention",
          tone: "warning",
          primaryLabel: "Resolve conflicts",
        }
      : {},
  });
}

// ---------------------------------------------------------------------------
// 4. Work / Overview / Conflict Fixtures (R3)
// ---------------------------------------------------------------------------

export interface RepoFixtureSet {
  repoPath: string;
  statuses: FileStatus[];
  operation: RepoOperation | null;
  worktrees: {
    path: string;
    name: string;
    branch: string;
    is_main: boolean;
    is_bare: boolean;
    is_detached: boolean;
    is_locked: boolean;
    is_prunable: boolean;
    dirty_files: number | null;
  }[];
  conflictDocs: Record<string, unknown>;
}

export function makeRepoFixtures(
  mode: "clean" | "dirty" | "conflicted" | "parked",
  repoPath = "/workspace/GitPulse",
): RepoFixtureSet {
  const statuses: FileStatus[] =
    mode === "clean"
      ? []
      : mode === "dirty"
        ? [
            {
              path: "src/app.css",
              status_code: "M ",
              is_staged: true,
              is_conflicted: false,
              additions: 4,
              deletions: 1,
            },
            {
              path: "src/main.ts",
              status_code: " M",
              is_staged: false,
              is_conflicted: false,
              additions: 2,
              deletions: 0,
            },
          ]
        : mode === "conflicted"
          ? [
              {
                path: "src/conflict.ts",
                status_code: "UU",
                is_staged: false,
                is_conflicted: true,
                additions: 0,
                deletions: 0,
              },
            ]
          : [
              {
                path: "src/resolved.ts",
                status_code: "M ",
                is_staged: true,
                is_conflicted: false,
                additions: 3,
                deletions: 2,
              },
            ];

  const operation: RepoOperation | null =
    mode === "conflicted" || mode === "parked"
      ? {
          kind: "Merge",
          current_step: null,
          total_steps: null,
          head_ref: "main",
          incoming_ref: "feature/r3-overview",
          conflicted_paths: mode === "conflicted" ? ["src/conflict.ts"] : [],
          conflicted_total: mode === "conflicted" ? 1 : 0,
          available: mode === "conflicted" ? ["abort"] : ["continue", "abort"],
        }
      : null;

  const conflictDocs: Record<string, unknown> =
    mode === "conflicted"
      ? {
          "src/conflict.ts": {
            file_path: "src/conflict.ts",
            revision: `${repoPath}:rev1`,
            operation: "Merge",
            document: {
              file_path: "src/conflict.ts",
              segments: [
                {
                  Conflict: {
                    chunk_index: 0,
                    start_line: 1,
                    end_line: 7,
                    ours_label: "HEAD · main",
                    theirs_label: "feature/r3-overview",
                    ours_content: "export const mode = 'ours';",
                    theirs_content: "export const mode = 'theirs';",
                    base_content: "export const mode = 'base';",
                    resolution: "Unresolved",
                    ours_crlf: [],
                    theirs_crlf: [],
                    base_crlf: [],
                    local_crlf: false,
                  },
                },
              ],
              total_conflicts: 1,
              crlf: false,
              trailing_newline: true,
              final_crlf: false,
              normal_crlf_flags: [],
              diagnostics: [],
              marker_size: 7,
            },
            stages: [
              {
                stage: 2,
                mode: "100644",
                oid: "obj-ours",
                size: 27,
                text: "export const mode = 'ours';",
              },
              {
                stage: 3,
                mode: "100644",
                oid: "obj-theirs",
                size: 29,
                text: "export const mode = 'theirs';",
              },
            ],
            reason: null,
            worktree_mode: "100644",
          },
        }
      : {};

  return {
    repoPath,
    statuses,
    operation,
    worktrees: [
      {
        path: repoPath,
        name: "GitPulse",
        branch: "main",
        is_main: true,
        is_bare: false,
        is_detached: false,
        is_locked: false,
        is_prunable: false,
        dirty_files: statuses.length,
      },
    ],
    conflictDocs,
  };
}

// ---------------------------------------------------------------------------
// 5. AgentsView / AgentPlane Fixtures (R4)
// ---------------------------------------------------------------------------

export function makeInsightsSnapshot(
  repoPath = "/code/devtools/GitPulse",
  worktrees: InsightsSnapshot["worktrees"]["items"] = [],
): InsightsSnapshot {
  return {
    repo_path: repoPath,
    branch: "main",
    branch_ok: true,
    deadline_expired: false,
    duration_ms: 4,
    worktrees: {
      ok: true,
      error: "",
      count: worktrees.length,
      scanned: worktrees.length,
      dirty: worktrees.filter((w) => (w.dirty_files ?? 0) > 0).length,
      dirty_unknown: 0,
      blocked: worktrees.filter((w) => Boolean(w.operation_kind)).length,
      blocked_unknown: 0,
      truncated: false,
      items: worktrees,
    },
    agents: {
      ok: true,
      sessions: worktrees.filter((w) => !w.is_main && Boolean(w.agent_kind)).length,
      kinds: [],
      truncated: false,
      live: { ok: true, sessions: 0, kinds: [], worktrees: [] },
    },
    changes: {
      ok: true,
      error: "",
      files: 0,
      staged: 0,
      unstaged: 0,
      untracked: 0,
      conflicted: 0,
      additions: 0,
      deletions: 0,
      churn_warnings: 0,
      churn_overflowed: false,
      truncated: false,
    },
    collisions: {
      ok: true,
      error: "",
      overlapping_files: 0,
      worktrees_involved: 0,
      scanned_worktrees: worktrees.length,
      unscanned_worktrees: 0,
      failed_worktrees: 0,
      truncated: false,
      items: [],
      shared_worktree_files: 0,
      shared_worktrees: [],
      sessions_ok: true,
      sessions_error: "",
    },
    ledger: { recording: true, path: "", dropped: 0, error: "", error_code: "" },
    codeintel: { available: false, db_path: "" },
  };
}

export function makePlaneProbe(
  repoPath = "/code/devtools/GitPulse",
  worktrees: InsightsSnapshot["worktrees"]["items"] = [],
): PlaneProbe {
  return {
    path: repoPath,
    label: repoPath.split("/").pop() || "GitPulse",
    snapshot: makeInsightsSnapshot(repoPath, worktrees),
    error: "",
    skipped: false,
    skipReason: "",
  };
}

export function makePlaneTerminal(overrides: Partial<PlaneTerminal> = {}): PlaneTerminal {
  return {
    key: "term-claude-1",
    repoPath: "/code/devtools/GitPulse",
    label: "Claude Code",
    title: "claude-main",
    status: "running",
    sessionId: "pty-claude-1",
    taskRunId: "",
    continuesRunId: "",
    cwd: "/code/devtools/GitPulse/.claude/worktrees/alpha",
    attention: null,
    ...overrides,
  };
}

export function makeTaskProbe(tasks: PlaneTask[] = []): TaskProbe {
  return {
    ok: true,
    unread: false,
    error: "",
    complete: true,
    tasks,
  };
}
