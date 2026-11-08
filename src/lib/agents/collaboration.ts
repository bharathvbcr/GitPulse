/**
 * Canonical owner for Vespin-style batch inbox triage, multi-agent kickoff
 * (`claude`, `agy`, `codex` across `terminal` and warm `managed` modes), and
 * Linear-style parent-subtask breakdown & task-linked coordination threads.
 */
import { invoke } from "../ipc/invoke";
import { isCaseInsensitiveFs } from "../repos/paths";
import { terminalLaunchRequests } from "../terminal/launchRequests";
import type { OpenTabRef } from "../workbench/openMembership";
import {
  MAX_MANAGED_CONTINUATIONS,
  WorkbenchError,
  explainError,
  getRepository,
  getTask,
  listTasks,
  newID,
  prepareTaskRun,
  putTask,
  registerRepository,
  taskWrite,
  type Repository,
  type RunKind,
  type Task,
  type TaskCard,
  type TaskDraft,
  type TaskRun,
} from "../workbench/client";
import { parseSubtaskLines } from "../workbench/intake";
import { getBoardLineage } from "../workbench/lineage";
import {
  checkoutCandidates,
  normalizeCheckout,
  preferredCheckout,
} from "../workbench/taskHandoff";
import { isTaskId } from "../workbench/taskOpen";
import { startPreparedAttempt } from "../workbench/taskTerminal";
import type {
  AgentBusMessage,
  AgentInboxThread,
  InboxDecomposeReceipt,
  InboxMessageReceipt,
  InboxSnapshot,
  StructuredAsk,
} from "./inboxTypes";

export type KickoffProvider = "claude" | "agy" | "codex";
export type KickoffMode = "terminal" | "managed";

export const KICKOFF_PROVIDERS: readonly { id: KickoffProvider; label: string }[] = [
  { id: "claude", label: "Claude" },
  { id: "agy", label: "Antigravity" },
  { id: "codex", label: "Codex" },
];

const MAX_NOTE_BYTES = 8_000;
const MAX_HANDOFF_PROMPT_BYTES = 8_000;

type InvokeCall = typeof invoke;

export interface CollaborationDeps {
  invoke?: InvokeCall;
  getTask?: typeof getTask;
  getRepository?: typeof getRepository;
  registerRepository?: typeof registerRepository;
  prepareTaskRun?: typeof prepareTaskRun;
  startPreparedAttempt?: typeof startPreparedAttempt;
  requestTerminalLaunch?: typeof terminalLaunchRequests.request;
  putTask?: typeof putTask;
  listTasks?: typeof listTasks;
  getBoardLineage?: typeof getBoardLineage;
  newID?: typeof newID;
}

export function toggleThreadSelection(
  selected: ReadonlySet<string>,
  threadKey: string
): Set<string> {
  const next = new Set(selected);
  if (!next.delete(threadKey)) {
    next.add(threadKey);
  }
  return next;
}

export function selectVisibleThreads(
  visibleThreads: readonly AgentInboxThread[]
): Set<string> {
  return new Set(visibleThreads.map((t) => t.thread_key));
}

export interface BatchWithdrawResult {
  withdrawnKeys: string[];
  skippedCount: number;
  errors: string[];
  updatedThreads: AgentInboxThread[];
}

export async function batchWithdrawOpenThreads(
  params: {
    repoPath?: string;
    threads: readonly AgentInboxThread[];
    selectedKeys: ReadonlySet<string>;
    reason?: string;
  },
  deps: CollaborationDeps = {}
): Promise<BatchWithdrawResult> {
  const invokeFn = deps.invoke ?? invoke;
  const reason = params.reason?.trim() || "Batch withdrawn by developer";
  const withdrawnKeys: string[] = [];
  const errors: string[] = [];
  let skippedCount = 0;

  const updatedThreads = params.threads.map((t) => ({
    ...t,
    messages: [...t.messages],
  }));

  for (const thread of updatedThreads) {
    if (!params.selectedKeys.has(thread.thread_key)) continue;
    if (thread.status !== "open") {
      skippedCount += 1;
      continue;
    }

    try {
      let persistedMsg: AgentBusMessage | null = null;
      if (params.repoPath?.trim()) {
        const receipt = await invokeFn<InboxMessageReceipt>("cmd_inbox_withdraw", {
          repoPath: params.repoPath,
          fromAgent: "developer",
          threadKey: thread.thread_key,
          reason,
        });
        persistedMsg = receipt?.message ?? null;
      }

      const now = Date.now();
      const withdrawMsg: AgentBusMessage = persistedMsg ?? {
        seq: (thread.latest_msg?.seq ?? 0) + 1,
        from_agent: "developer",
        to_agent: thread.from_agent,
        kind: "withdraw",
        body: `Ask withdrawn: ${reason}`,
        branch: null,
        created_at_ms: now,
        thread_key: thread.thread_key,
      };
      thread.status = "withdrawn";
      thread.updated_at_ms = withdrawMsg.created_at_ms;
      thread.latest_msg = withdrawMsg;
      thread.messages = [...thread.messages, withdrawMsg];
      withdrawnKeys.push(thread.thread_key);
    } catch (cause) {
      errors.push(`${thread.thread_key}: ${explainError(cause)}`);
    }
  }

  return { withdrawnKeys, skippedCount, errors, updatedThreads };
}

export interface BatchResolveResult {
  resolvedKeys: string[];
  skippedCount: number;
  errors: string[];
  updatedThreads: AgentInboxThread[];
}

export async function batchResolveThreadsWithNote(
  params: {
    repoPath?: string;
    threads: readonly AgentInboxThread[];
    selectedKeys: ReadonlySet<string>;
    note: string;
  },
  deps: CollaborationDeps = {}
): Promise<BatchResolveResult> {
  const trimmed = params.note.trim();
  if (!trimmed) {
    throw new Error("Resolution note is required.");
  }
  if (new TextEncoder().encode(trimmed).length > MAX_NOTE_BYTES) {
    throw new Error(`Resolution note exceeds ${MAX_NOTE_BYTES} bytes.`);
  }

  const invokeFn = deps.invoke ?? invoke;
  const resolvedKeys: string[] = [];
  const errors: string[] = [];
  let skippedCount = 0;

  const updatedThreads = params.threads.map((t) => ({
    ...t,
    messages: [...t.messages],
  }));

  for (const thread of updatedThreads) {
    if (!params.selectedKeys.has(thread.thread_key)) continue;
    if (thread.status === "withdrawn") {
      skippedCount += 1;
      continue;
    }

    const kind = thread.status === "open" ? "reply" : "note";
    try {
      let persistedMsg: AgentBusMessage | null = null;
      if (params.repoPath?.trim()) {
        const receipt = await invokeFn<InboxMessageReceipt>("cmd_worktree_send_message", {
          repoPath: params.repoPath,
          fromAgent: "developer",
          toAgent: thread.from_agent,
          threadKey: thread.thread_key,
          kind,
          body: trimmed,
        });
        persistedMsg = receipt?.message ?? null;
      }

      const now = Date.now();
      const noteMsg: AgentBusMessage = persistedMsg ?? {
        seq: (thread.latest_msg?.seq ?? 0) + 1,
        from_agent: "developer",
        to_agent: thread.from_agent,
        kind,
        body: trimmed,
        branch: null,
        created_at_ms: now,
        thread_key: thread.thread_key,
      };
      if (thread.status === "open") {
        thread.status = "answered";
      }
      thread.updated_at_ms = noteMsg.created_at_ms;
      thread.latest_msg = noteMsg;
      thread.messages = [...thread.messages, noteMsg];
      resolvedKeys.push(thread.thread_key);
    } catch (cause) {
      errors.push(`${thread.thread_key}: ${explainError(cause)}`);
    }
  }

  return { resolvedKeys, skippedCount, errors, updatedThreads };
}

export function extractTaskIdFromThreadKey(threadKey: string): string | null {
  if (!threadKey.startsWith("task-")) return null;
  const candidate = threadKey.slice("task-".length).trim();
  return isTaskId(candidate) ? candidate : null;
}

export function buildInboxHandoffPrompt(
  thread: AgentInboxThread,
  ask: StructuredAsk | null
): string {
  const lines: string[] = [
    `GitPulse Inbox Handoff (${thread.thread_key})`,
    `Title: ${thread.title}`,
    `From: ${thread.from_agent}`,
  ];
  if (ask?.question || ask?.prompt) {
    lines.push(`Question: ${ask.question || ask.prompt}`);
  }
  if (ask?.summary) {
    lines.push(`Summary: ${ask.summary}`);
  }
  if (ask?.options?.length) {
    lines.push(`Options: ${ask.options.join(", ")}`);
    if (ask.recommendation) lines.push(`Recommended: ${ask.recommendation}`);
  }
  if (ask?.steps?.length) {
    lines.push("Steps:");
    for (const step of ask.steps) {
      lines.push(`- ${step}`);
    }
  } else if (!ask && thread.latest_msg?.body) {
    lines.push(`Latest message: ${thread.latest_msg.body}`);
  }
  lines.push(
    `Coordinate progress and post updates on GitPulse inbox thread "${thread.thread_key}".`
  );

  const joined = lines.join("\n").replace(/\u0000/g, "").trim();
  const encoder = new TextEncoder();
  if (encoder.encode(joined).length <= MAX_HANDOFF_PROMPT_BYTES) {
    return joined;
  }
  let truncated = joined;
  while (encoder.encode(truncated).length > MAX_HANDOFF_PROMPT_BYTES - 32) {
    truncated = truncated.slice(0, Math.floor(truncated.length * 0.9));
  }
  return `${truncated}\n[handoff truncated]`;
}

export async function kickOffAgentForTask(
  params: {
    taskId: string;
    provider: KickoffProvider;
    mode: KickoffMode;
    repoPath?: string;
    repositories?: readonly Repository[];
    openTabs?: readonly OpenTabRef[];
    worktree?: boolean;
  },
  deps: CollaborationDeps = {}
): Promise<{ run: TaskRun; checkoutPath: string; task: Task }> {
  const readTask = deps.getTask ?? getTask;
  const readRepo = deps.getRepository ?? getRepository;
  const regRepo = deps.registerRepository ?? registerRepository;
  const prepare = deps.prepareTaskRun ?? prepareTaskRun;
  const start = deps.startPreparedAttempt ?? startPreparedAttempt;
  const makeId = deps.newID ?? newID;

  const task = await readTask(params.taskId);
  let repositoryId = task.primary_repository_id || task.repository_ids[0] || "";
  let repository: Repository;

  if (!repositoryId) {
    const fallbackPath = params.repoPath?.trim() ?? "";
    if (!fallbackPath) {
      throw new Error("Link a repository to this task before kicking off an agent.");
    }
    repository = await regRepo(fallbackPath);
    repositoryId = repository.id;
  } else {
    repository = await readRepo(repositoryId);
  }

  const pathOpts = { caseInsensitive: isCaseInsensitiveFs() };
  const knownRepos = [
    repository,
    ...(params.repositories ?? []).filter((r) => r.id !== repository.id),
  ];
  const candidates = checkoutCandidates(
    repository.id,
    knownRepos,
    params.openTabs ?? [],
    pathOpts
  );
  const checkoutPath =
    preferredCheckout(candidates) ||
    normalizeCheckout(params.repoPath) ||
    normalizeCheckout(repository.identity_key);

  if (!checkoutPath) {
    throw new Error("Open the task's repository checkout before kicking off an agent.");
  }

  const kind: RunKind = params.mode === "managed" ? "managed" : "external_terminal";
  const run = await prepare({
    kind,
    id: makeId(),
    request_id: makeId(),
    task_id: task.id,
    source_revision: task.revision,
    repository_id: repository.id,
    repository_revision: repository.revision,
    repo_path: checkoutPath,
    provider: params.provider,
    permission_mode: "edit",
    acknowledge_bypass: false,
    worktree: params.worktree ?? true,
  });

  const started = await start(
    run,
    kind === "managed" ? { maxContinuations: MAX_MANAGED_CONTINUATIONS } : {}
  );
  if (!started.ok) {
    throw new Error(started.error);
  }

  return { run: started.run, checkoutPath, task };
}

export async function kickOffAgentFromInboxThread(
  params: {
    thread: AgentInboxThread;
    ask: StructuredAsk | null;
    provider: KickoffProvider;
    mode: KickoffMode;
    repoPath?: string;
    repositories?: readonly Repository[];
    openTabs?: readonly OpenTabRef[];
  },
  deps: CollaborationDeps = {}
): Promise<{
  launchedVia: "task_run" | "terminal_tab";
  run?: TaskRun;
  handoffMessage: AgentBusMessage | null;
}> {
  const invokeFn = deps.invoke ?? invoke;
  const requestTerm =
    deps.requestTerminalLaunch ?? ((path, launcher, prompt) => terminalLaunchRequests.request(path, launcher, prompt));

  const candidateTaskId =
    (params.ask?.task_id && isTaskId(params.ask.task_id) ? params.ask.task_id : null) ??
    extractTaskIdFromThreadKey(params.thread.thread_key);

  let launchedRun: TaskRun | undefined;
  let effectiveRepoPath = params.repoPath?.trim() ?? "";
  let launchedVia: "task_run" | "terminal_tab" = "terminal_tab";

  if (candidateTaskId) {
    try {
      const res = await kickOffAgentForTask(
        {
          taskId: candidateTaskId,
          provider: params.provider,
          mode: params.mode,
          repoPath: effectiveRepoPath || undefined,
          repositories: params.repositories,
          openTabs: params.openTabs,
          worktree: true,
        },
        deps
      );
      launchedRun = res.run;
      effectiveRepoPath = effectiveRepoPath || res.checkoutPath;
      launchedVia = "task_run";
    } catch (cause) {
      if (!(cause instanceof WorkbenchError && cause.code === "not_found")) {
        throw cause;
      }
    }
  }

  if (!launchedRun) {
    if (!effectiveRepoPath) {
      throw new Error("Open a repository before kicking off an agent from this thread.");
    }
    const prompt = buildInboxHandoffPrompt(params.thread, params.ask);
    if (params.mode === "managed") {
      const receipt = await invokeFn<InboxDecomposeReceipt>("cmd_inbox_decompose_task", {
        repoPath: effectiveRepoPath,
        title: params.thread.title || `Inbox Handoff ${params.thread.thread_key}`,
        description: prompt,
        subtasks: params.ask?.steps ?? [],
        assignAgent: params.provider,
        branch: params.ask?.branch ?? undefined,
      });
      const res = await kickOffAgentForTask(
        {
          taskId: receipt.item_id,
          provider: params.provider,
          mode: "managed",
          repoPath: effectiveRepoPath,
          repositories: params.repositories,
          openTabs: params.openTabs,
          worktree: true,
        },
        deps
      );
      launchedRun = res.run;
      launchedVia = "task_run";
    } else {
      await requestTerm(effectiveRepoPath, params.provider, prompt);
      launchedVia = "terminal_tab";
    }
  }

  let handoffMessage: AgentBusMessage | null = null;
  if (effectiveRepoPath) {
    const modeDesc =
      params.mode === "managed" ? "warm managed session" : "interactive terminal session";
    const runRef = launchedRun ? ` (run ${launchedRun.id})` : "";
    const receipt = await invokeFn<InboxMessageReceipt>("cmd_worktree_send_message", {
      repoPath: effectiveRepoPath,
      fromAgent: "developer",
      toAgent: params.provider,
      threadKey: params.thread.thread_key,
      kind: "handoff",
      body: `Kicked off ${params.provider} (${modeDesc})${runRef} for thread "${params.thread.title}".`,
    });
    handoffMessage = receipt?.message ?? null;
  }

  return { launchedVia, run: launchedRun, handoffMessage };
}

export async function createChildSubtasksForParent(
  params: {
    parentTask: Task;
    subtaskTitles: readonly string[];
    repoPath?: string;
  },
  deps: CollaborationDeps = {}
): Promise<Task[]> {
  const cleaned = parseSubtaskLines(params.subtaskTitles.join("\n"));
  if (cleaned.length === 0) {
    throw new Error("Enter at least one subtask line.");
  }

  const writeTask = deps.putTask ?? putTask;
  const invokeFn = deps.invoke ?? invoke;
  const makeId = deps.newID ?? newID;
  const created: Task[] = [];
  const basePos = Date.now();

  for (let i = 0; i < cleaned.length; i += 1) {
    const title = cleaned[i];
    const draft: TaskDraft = {
      title,
      description: "",
      kind: "subtask",
      status: "ready",
      priority: params.parentTask.priority,
      severity: null,
      owner: null,
      due_at: null,
      labels: [...params.parentTask.labels],
      acceptance_criteria: [],
      repository_ids: [...params.parentTask.repository_ids],
      primary_repository_id: params.parentTask.primary_repository_id,
      home_workspace_id: params.parentTask.home_workspace_id,
      position: basePos + i,
      archived: false,
      checklist: [],
      links: [{ kind: "parent", item_id: params.parentTask.id }],
    };
    const saved = await writeTask(taskWrite(makeId(), 0, draft));
    created.push(saved);
  }

  if (params.repoPath?.trim()) {
    const threadKey = `task-${params.parentTask.id}`;
    const payload: StructuredAsk = {
      ask_type: "do",
      title: `Subtasks for ${params.parentTask.title}`,
      task_id: params.parentTask.id,
      steps: cleaned,
    };
    await invokeFn<InboxMessageReceipt>("cmd_inbox_post", {
      repoPath: params.repoPath,
      fromAgent: "developer",
      threadKey,
      kind: "ask",
      title: `Subtasks: ${params.parentTask.title}`,
      body: JSON.stringify(payload),
    });
  }

  return created;
}

export async function loadChildSubtasks(
  parentTaskId: string,
  primaryRepositoryId?: string,
  deps: CollaborationDeps = {}
): Promise<TaskCard[]> {
  const listFn = deps.listTasks ?? listTasks;
  const lineageFn = deps.getBoardLineage ?? getBoardLineage;

  const scope = primaryRepositoryId?.trim()
    ? ({ kind: "repository", id: primaryRepositoryId } as const)
    : ({ kind: "global" } as const);
  const page = await listFn(scope, null, "", undefined, 100, { archived: false });
  if (page.items.length === 0) return [];

  const lineage = await lineageFn(page.items.map((item) => item.id));
  return page.items.filter((card) => lineage.get(card.id)?.parent_item_id === parentTaskId);
}

export function matchesTaskThread(thread: AgentInboxThread, taskId: string): boolean {
  if (thread.thread_key === `task-${taskId}`) return true;
  for (const msg of thread.messages) {
    try {
      const parsed = JSON.parse(msg.body) as { task_id?: unknown };
      if (parsed && typeof parsed === "object" && parsed.task_id === taskId) {
        return true;
      }
    } catch {
      // Plain text message body
    }
  }
  return false;
}

export async function loadLinkedTaskThreads(
  repoPath: string,
  taskId: string,
  deps: CollaborationDeps = {}
): Promise<AgentInboxThread[]> {
  if (!repoPath.trim() || !taskId.trim()) return [];
  const invokeFn = deps.invoke ?? invoke;
  const snapshot = await invokeFn<InboxSnapshot>("cmd_inbox_read", { repoPath });
  return (snapshot?.threads ?? []).filter((t) => matchesTaskThread(t, taskId));
}

export async function replyToTaskThread(
  params: {
    repoPath: string;
    threadKey: string;
    toAgent?: string | null;
    body: string;
  },
  deps: CollaborationDeps = {}
): Promise<AgentBusMessage | null> {
  const trimmed = params.body.trim();
  if (!trimmed) throw new Error("Reply text is required.");
  const invokeFn = deps.invoke ?? invoke;
  const receipt = await invokeFn<InboxMessageReceipt>("cmd_worktree_send_message", {
    repoPath: params.repoPath,
    fromAgent: "developer",
    toAgent: params.toAgent ?? undefined,
    threadKey: params.threadKey,
    kind: "reply",
    body: trimmed,
  });
  return receipt?.message ?? null;
}

export async function postTaskAgentAsk(
  params: {
    repoPath: string;
    taskId: string;
    taskTitle: string;
    toAgent: string;
    question: string;
  },
  deps: CollaborationDeps = {}
): Promise<AgentBusMessage | null> {
  const trimmed = params.question.trim();
  if (!trimmed) throw new Error("Ask or handoff prompt is required.");
  const invokeFn = deps.invoke ?? invoke;
  const threadKey = `task-${params.taskId}`;
  const payload: StructuredAsk = {
    ask_type: "answer",
    title: params.taskTitle,
    question: trimmed,
    task_id: params.taskId,
  };
  const receipt = await invokeFn<InboxMessageReceipt>("cmd_inbox_post", {
    repoPath: params.repoPath,
    fromAgent: "developer",
    toAgent: params.toAgent.trim() || undefined,
    threadKey,
    kind: "ask",
    title: params.taskTitle,
    body: JSON.stringify(payload),
  });
  return receipt?.message ?? null;
}

export interface MobileKickoffDirective {
  provider: KickoffProvider;
  mode: KickoffMode;
  sessionId?: string;
  taskId?: string;
  branch?: string;
  title?: string;
  prompt: string;
}

export interface MobileTaskBreakdown {
  title: string;
  steps: string[];
  assignAgent?: KickoffProvider;
  taskId?: string;
  branch?: string;
}

const VALID_PROVIDERS = new Set<KickoffProvider>(["claude", "agy", "codex"]);
const VALID_MODES = new Set<KickoffMode>(["managed", "terminal"]);

export function isMobileOriginated(fromAgent: string | null | undefined): boolean {
  if (!fromAgent) return false;
  const normalized = fromAgent.trim().toLowerCase();
  return normalized === "graft-mobile" || normalized === "graft-bridge";
}

function parseKickoffBracketDirective(
  rawText: string,
  fallbackTitle?: string
): MobileKickoffDirective | null {
  const trimmed = rawText.trim();
  if (!trimmed.startsWith("[KICKOFF")) return null;
  const closeIdx = trimmed.indexOf("]");
  if (closeIdx < 0) return null;

  const header = trimmed.slice("[KICKOFF".length, closeIdx).trim();
  const attrs: Record<string, string> = {};
  for (const token of header.split(/\s+/)) {
    const eq = token.indexOf("=");
    if (eq > 0) {
      const k = token.slice(0, eq).trim();
      const v = token.slice(eq + 1).trim();
      if (k && v) attrs[k] = v;
    }
  }

  const provider = attrs.provider as KickoffProvider | undefined;
  if (!provider || !VALID_PROVIDERS.has(provider)) {
    return null;
  }

  const modeRaw = (attrs.mode ?? "managed") as KickoffMode;
  if (!VALID_MODES.has(modeRaw)) {
    return null;
  }

  const afterBracket = trimmed.slice(closeIdx + 1);
  const newlineIdx = afterBracket.indexOf("\n");
  let inlineTitle = "";
  let bodyPrompt = "";
  if (newlineIdx >= 0) {
    inlineTitle = afterBracket.slice(0, newlineIdx).trim();
    bodyPrompt = afterBracket.slice(newlineIdx + 1).trim();
  } else {
    inlineTitle = afterBracket.trim();
  }

  const title = inlineTitle || fallbackTitle?.trim() || undefined;
  const prompt = bodyPrompt || inlineTitle || fallbackTitle?.trim() || "";
  if (!prompt) return null;

  return {
    provider,
    mode: modeRaw,
    sessionId: attrs.session || undefined,
    taskId: attrs.task || undefined,
    branch: attrs.branch || undefined,
    title,
    prompt,
  };
}

export function parseMobileKickoffDirective(content: string): MobileKickoffDirective | null {
  if (!content || !content.trim()) return null;
  const trimmed = content.trim();

  if (trimmed.startsWith("{")) {
    try {
      const parsed = JSON.parse(trimmed) as Record<string, unknown>;
      if (parsed && typeof parsed === "object") {
        const title = typeof parsed.title === "string" ? parsed.title.trim() : undefined;
        if (typeof parsed.question === "string" && parsed.question.trim().startsWith("[KICKOFF")) {
          const fromQuestion = parseKickoffBracketDirective(parsed.question, title);
          if (fromQuestion) {
            return {
              ...fromQuestion,
              sessionId:
                fromQuestion.sessionId ??
                (typeof parsed.session_id === "string" ? parsed.session_id : undefined),
              taskId:
                fromQuestion.taskId ??
                (typeof parsed.task_id === "string" ? parsed.task_id : undefined),
              branch:
                fromQuestion.branch ??
                (typeof parsed.branch === "string" ? parsed.branch : undefined),
              title: fromQuestion.title ?? title,
              prompt:
                typeof parsed.prompt === "string" && parsed.prompt.trim()
                  ? parsed.prompt.trim()
                  : fromQuestion.prompt,
            };
          }
        }
        if (parsed.kind === "kickoff") {
          const provider = parsed.provider as KickoffProvider | undefined;
          const mode = (parsed.mode ?? "managed") as KickoffMode;
          const prompt =
            (typeof parsed.prompt === "string" && parsed.prompt.trim()) ||
            (typeof parsed.question === "string" && parsed.question.trim()) ||
            "";
          if (provider && VALID_PROVIDERS.has(provider) && VALID_MODES.has(mode) && prompt) {
            return {
              provider,
              mode,
              sessionId: typeof parsed.session_id === "string" ? parsed.session_id : undefined,
              taskId: typeof parsed.task_id === "string" ? parsed.task_id : undefined,
              branch: typeof parsed.branch === "string" ? parsed.branch : undefined,
              title,
              prompt,
            };
          }
        }
      }
    } catch {
      // Fall through to bracket parser
    }
  }

  return parseKickoffBracketDirective(trimmed);
}

export function parseMobileTaskBreakdown(content: string): MobileTaskBreakdown | null {
  if (!content || !content.trim()) return null;
  const trimmed = content.trim();

  if (trimmed.startsWith("{")) {
    try {
      const parsed = JSON.parse(trimmed) as Record<string, unknown>;
      if (parsed && typeof parsed === "object") {
        const isDecompose =
          parsed.kind === "decompose" ||
          (typeof parsed.question === "string" &&
            parsed.question.trim().startsWith("[TASK BREAKDOWN]"));
        if (isDecompose) {
          let title = typeof parsed.title === "string" ? parsed.title.trim() : "";
          if (!title && typeof parsed.question === "string") {
            const firstLine = parsed.question.trim().split("\n")[0] ?? "";
            title = firstLine.replace(/^\[TASK BREAKDOWN\]\s*/i, "").trim();
          }
          let steps: string[] = [];
          if (Array.isArray(parsed.steps)) {
            steps = parsed.steps
              .filter((s): s is string => typeof s === "string")
              .map((s) => s.trim())
              .filter(Boolean);
          } else if (Array.isArray(parsed.subtasks)) {
            steps = parsed.subtasks
              .map((item) =>
                item && typeof item === "object" && typeof (item as { title?: unknown }).title === "string"
                  ? (item as { title: string }).title.trim()
                  : ""
              )
              .filter(Boolean);
          } else if (typeof parsed.question === "string") {
            const lines = parsed.question.trim().split("\n").slice(1);
            steps = parseSubtaskLines(lines.join("\n"));
          }

          if (!title || steps.length === 0) return null;
          const firstSubtaskAssignee =
            Array.isArray(parsed.subtasks) &&
            parsed.subtasks.length > 0 &&
            parsed.subtasks[0] &&
            typeof parsed.subtasks[0] === "object" &&
            typeof (parsed.subtasks[0] as { assignee?: unknown }).assignee === "string"
              ? (parsed.subtasks[0] as { assignee: string }).assignee.trim()
              : undefined;
          const rawAssignee =
            typeof parsed.assign_agent === "string"
              ? parsed.assign_agent.trim()
              : typeof parsed.assignAgent === "string"
                ? parsed.assignAgent.trim()
                : firstSubtaskAssignee;
          const assignAgent =
            rawAssignee && VALID_PROVIDERS.has(rawAssignee as KickoffProvider)
              ? (rawAssignee as KickoffProvider)
              : undefined;

          return {
            title,
            steps,
            assignAgent,
            taskId:
              typeof parsed.task_id === "string" && parsed.task_id.trim()
                ? parsed.task_id.trim()
                : undefined,
            branch:
              typeof parsed.branch === "string" && parsed.branch.trim()
                ? parsed.branch.trim()
                : undefined,
          };
        }
      }
    } catch {
      // Fall through to plain text parser
    }
  }

  if (trimmed.startsWith("[TASK BREAKDOWN]")) {
    const lines = trimmed.split("\n");
    const firstLine = lines[0] ?? "";
    const title = firstLine.replace(/^\[TASK BREAKDOWN\]\s*/i, "").trim();
    const steps = parseSubtaskLines(lines.slice(1).join("\n"));
    if (!title || steps.length === 0) return null;
    return {
      title,
      steps,
    };
  }

  return null;
}

