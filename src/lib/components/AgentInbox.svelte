<script lang="ts">
  /**
   * AgentInbox - Vespin-modeled Agent Coordination & Structured Ask Inbox.
   *
   * Displays persistent inter-agent conversation threads and structured asks:
   * - decide: Option cards with recommendations and single-choice submission.
   * - do: Interactive checklist steps with confirmation.
   * - answer: Direct question prompts with input / response submission.
   * - review: Code review / diff requests with approval / changes submission.
   *
   * Also provides thread-level messaging, ask withdrawal, and status rollups.
   */
  import { onMount, untrack } from "svelte";
  import {
    Bot,
    Check,
    CheckCircle2,
    GitBranch,
    HelpCircle,
    Inbox,
    Layers,
    ListTodo,
    MessageSquare,
    Plus,
    RefreshCw,
    Search,
    Send,
    User,
    Vote,
    FileCode,
    X,
    XCircle,
    AlertCircle,
    ChevronRight,
  } from "@lucide/svelte";
  import type {
    AgentInboxThread,
    AgentLaneClaim,
    BusMessage,
    InboxDecomposeReceipt,
    InboxMessageReceipt,
    InboxSnapshot,
    StructuredAsk,
  } from "../agents/inboxTypes";
  import {
    KICKOFF_PROVIDERS,
    batchResolveThreadsWithNote,
    batchWithdrawOpenThreads,
    extractTaskIdFromThreadKey,
    isMobileOriginated,
    kickOffAgentFromInboxThread,
    parseMobileKickoffDirective,
    parseMobileTaskBreakdown,
    selectVisibleThreads,
    toggleThreadSelection,
    type KickoffMode,
    type KickoffProvider,
    type MobileKickoffDirective,
    type MobileTaskBreakdown,
  } from "../agents/collaboration";
  import { formatRelativeTime } from "../format";
  import { describeFailure } from "../ui/copy/describeFailure";
  import { shortcutTextLabel } from "../ui/platformCopy";
  import { hostPlatform } from "../stores/platformStore";
  import EmptyState from "./EmptyState.svelte";
  import Toggle from "./Toggle.svelte";
  import { isMacOS } from "../platform";
  import { invoke } from "../ipc/invoke";
  import {
    calculateSubtaskRollup,
    decomposeTask,
    formatSubtaskProgress,
  } from "../workbench/intake";

  /** Only macOS draws the glass selection pill on a tablist. */
  const macos = isMacOS();

  /** The status filter tabs, in order. */
  const STATUS_FILTERS = [
    { id: "all", label: "All" },
    { id: "open", label: "Open asks" },
    { id: "answered", label: "Answered" },
    { id: "withdrawn", label: "Withdrawn" },
  ] as const;

  /** The `gp-chip` tone of a thread status. */
  const STATUS_TONE: Record<string, string> = {
    open: "warn",
    answered: "ok",
    withdrawn: "neutral",
  };

  interface Props {
    repoPath?: string;
    initialThreads?: AgentInboxThread[];
    initialLanes?: AgentLaneClaim[];
    initialSelectedKeys?: string[];
    onopenTask?: (taskId: string) => Promise<void>;
    onkickoff?: (directive: MobileKickoffDirective, thread: AgentInboxThread) => Promise<void> | void;
    embedded?: boolean;
  }

  let {
    repoPath,
    initialThreads,
    initialLanes,
    initialSelectedKeys,
    onopenTask,
    onkickoff,
    embedded = false,
  }: Props = $props();

  let threads = $state<AgentInboxThread[]>(
    untrack(() => (initialThreads ? [...initialThreads] : []))
  );
  let lanes = $state<AgentLaneClaim[]>(
    untrack(() => (initialLanes ? [...initialLanes] : []))
  );

  $effect(() => {
    if (initialThreads) {
      threads = initialThreads;
    }
  });

  $effect(() => {
    if (initialLanes) {
      lanes = initialLanes;
    }
  });

  let lastLoadedRepoPath: string | undefined = undefined;
  $effect(() => {
    const activeRepo = repoPath;
    if (!initialThreads && activeRepo && activeRepo !== lastLoadedRepoPath) {
      lastLoadedRepoPath = activeRepo;
      void refreshInbox();
    }
  });

  let selectedThreadKey = $state<string | null>(null);
  let selectedKeys = $state<Set<string>>(
    untrack(() => new Set(initialSelectedKeys ?? []))
  );
  let batchNote = $state<string>("");
  let kickoffProvider = $state<KickoffProvider>("claude");
  let kickoffMode = $state<KickoffMode>("managed");
  let statusFilter = $state<"all" | "open" | "answered" | "withdrawn">("all");
  let searchQuery = $state<string>("");
  let replyText = $state<string>("");
  let busy = $state<boolean>(false);
  let error = $state<string>("");
  let notice = $state<string>("");

  // Ask-specific interactive states
  let selectedDecision = $state<string | null>(null);
  let completedSteps = $state<Record<number, boolean>>({});
  let textAnswer = $state<string>("");
  let reviewNotes = $state<string>("");
  let isWithdrawing = $state<boolean>(false);
  let withdrawReason = $state<string>("");

  // Composer drawer modes: New Ask / Handoff (`cmd_inbox_post`) and Task Breakdown (`cmd_inbox_decompose_task`)
  let activeDrawer = $state<"none" | "ask" | "decompose">("none");
  let newAskType = $state<"decide" | "do" | "answer" | "review">("decide");
  let newAskTitle = $state<string>("");
  let newAskToAgent = $state<string>("");
  let newAskBranch = $state<string>("");
  let newAskOptionsText = $state<string>("");
  let newAskRecommendation = $state<string>("");
  let newAskStepsText = $state<string>("");
  let newAskSummary = $state<string>("");

  let breakdownTitle = $state<string>("");
  let breakdownDescription = $state<string>("");
  let breakdownSubtasksText = $state<string>("");
  let breakdownAssignee = $state<string>("");
  let breakdownBranch = $state<string>("");

  const parsedBreakdownLines = $derived(
    breakdownSubtasksText
      .split("\n")
      .map((line) => line.replace(/^[-*\d.)\s]+/, "").trim())
      .filter((line) => line.length > 0)
  );

  const previewPlan = $derived(
    decomposeTask(
      breakdownTitle.trim() ? `gp-${breakdownTitle.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").slice(0, 32)}` : "gp-task",
      breakdownTitle.trim() || "Untitled task",
      parsedBreakdownLines
    )
  );

  onMount(() => {
    if (threads.length === 0 && repoPath) {
      lastLoadedRepoPath = repoPath;
      void refreshInbox();
    }
  });

  /** "Couldn't <verb> <object>. <Reason>." for an error line. */
  function failure(verb: string, object: string, cause: unknown): string {
    const copy = describeFailure({ verb, object }, cause);
    return copy.detail ? `${copy.headline} ${copy.detail}` : copy.headline;
  }

  export async function refreshInbox(): Promise<void> {
    if (!repoPath) return;
    busy = true;
    error = "";
    try {
      const res = await invoke<InboxSnapshot>("cmd_inbox_read", {
        repoPath,
      });

      if (res?.threads) {
        threads = res.threads;
      }
      if (res?.lanes) {
        lanes = res.lanes;
      }
    } catch (err: unknown) {
      error = failure("load", "inbox threads", err);
    } finally {
      busy = false;
    }
  }

  export function parseStructuredAsk(body: string): StructuredAsk | null {
    try {
      const parsed = JSON.parse(body);
      if (parsed && typeof parsed === "object" && typeof parsed.ask_type === "string") {
        return parsed as StructuredAsk;
      }
    } catch {
      // Not JSON structured ask
    }
    return null;
  }

  const openAsksCount = $derived(
    threads.filter((t) => t.status === "open" && t.ask_type !== null).length
  );

  const filteredThreads = $derived(
    threads.filter((t) => {
      if (statusFilter !== "all" && t.status !== statusFilter) return false;
      if (searchQuery.trim()) {
        const q = searchQuery.toLowerCase();
        const matchesTitle = t.title.toLowerCase().includes(q);
        const matchesKey = t.thread_key.toLowerCase().includes(q);
        const matchesAgent = t.from_agent.toLowerCase().includes(q) || (t.to_agent && t.to_agent.toLowerCase().includes(q));
        if (!matchesTitle && !matchesKey && !matchesAgent) return false;
      }
      return true;
    })
  );

  const selectedThread = $derived(
    threads.find((t) => t.thread_key === selectedThreadKey) ?? (filteredThreads.length > 0 ? filteredThreads[0] : null)
  );

  const currentAsk = $derived.by(() => {
    if (!selectedThread) return null;
    for (let i = selectedThread.messages.length - 1; i >= 0; i -= 1) {
      if (selectedThread.messages[i].kind === "ask") {
        const parsed = parseStructuredAsk(selectedThread.messages[i].body);
        if (parsed) return parsed;
      }
    }
    return selectedThread.latest_msg?.body ? parseStructuredAsk(selectedThread.latest_msg.body) : null;
  });

  const currentDoSteps = $derived.by(() => {
    if (!currentAsk || currentAsk.ask_type !== "do") return null;
    if (currentAsk.steps && currentAsk.steps.length > 0) return currentAsk.steps;
    if (currentAsk.action && currentAsk.action.trim()) return [currentAsk.action.trim()];
    return null;
  });

  const currentStepRollup = $derived.by(() => {
    if (!currentDoSteps || currentDoSteps.length === 0) return null;
    const statuses = currentDoSteps.map((_, idx) => (completedSteps[idx] ? "done" : "ready"));
    return calculateSubtaskRollup(statuses);
  });

  function selectThread(key: string) {
    selectedThreadKey = key;
    selectedDecision = null;
    completedSteps = {};
    textAnswer = "";
    reviewNotes = "";
    isWithdrawing = false;
    withdrawReason = "";
    replyText = "";
  }

  async function sendAnswer(body: string) {
    if (!selectedThread) return;
    busy = true;
    error = "";
    notice = "";
    try {
      const targetKey = selectedThread.thread_key;
      const targetAgent = selectedThread.from_agent;
      let persistedMsg: BusMessage | null = null;

      if (repoPath) {
        const receipt = await invoke<InboxMessageReceipt>(
          "cmd_worktree_send_message",
          {
            repoPath,
            fromAgent: "developer",
            toAgent: targetAgent,
            threadKey: targetKey,
            kind: "reply",
            body,
          }
        );
        persistedMsg = receipt?.message ?? null;
      }

      const now = Date.now();
      const newMsg: BusMessage = persistedMsg ?? {
        seq: (selectedThread.latest_msg?.seq ?? 0) + 1,
        from_agent: "developer",
        to_agent: targetAgent,
        kind: "reply",
        body,
        branch: null,
        created_at_ms: now,
        thread_key: targetKey,
      };

      selectedThread.status = "answered";
      selectedThread.updated_at_ms = newMsg.created_at_ms;
      selectedThread.latest_msg = newMsg;
      selectedThread.messages = [...selectedThread.messages, newMsg];
      threads = [...threads];

      notice = "Response sent to agent";
    } catch (err: unknown) {
      error = failure("send", "the response", err);
    } finally {
      busy = false;
    }
  }

  function handleAnswerDecision(opt: string) {
    void sendAnswer(`Decision selected: ${opt}`);
  }

  function handleCompleteSteps() {
    if (!currentDoSteps) return;
    const completedList = currentDoSteps
      .filter((_, idx) => completedSteps[idx])
      .map((s) => `[x] ${s}`)
      .join("\n");
    void sendAnswer(`Completed checklist steps:\n${completedList || "All steps marked done"}`);
  }

  function handleSubmitAnswer() {
    if (!textAnswer.trim()) return;
    const answerBody = textAnswer.trim();
    textAnswer = "";
    void sendAnswer(answerBody);
  }

  function handleSubmitReview(approved: boolean) {
    const verdict = approved ? "Approved" : "Changes Requested";
    const body = reviewNotes.trim() ? `${verdict}:\n${reviewNotes.trim()}` : verdict;
    reviewNotes = "";
    void sendAnswer(body);
  }

  async function handleWithdraw() {
    if (!selectedThread) return;
    busy = true;
    error = "";
    try {
      const targetKey = selectedThread.thread_key;
      const reason = withdrawReason.trim() || "Withdrawn by developer";
      let persistedMsg: BusMessage | null = null;

      if (repoPath) {
        const receipt = await invoke<InboxMessageReceipt>(
          "cmd_inbox_withdraw",
          {
            repoPath,
            fromAgent: "developer",
            threadKey: targetKey,
            reason,
          }
        );
        persistedMsg = receipt?.message ?? null;
      }

      const now = Date.now();
      const withdrawMsg: BusMessage = persistedMsg ?? {
        seq: (selectedThread.latest_msg?.seq ?? 0) + 1,
        from_agent: "developer",
        to_agent: selectedThread.from_agent,
        kind: "withdraw",
        body: `Ask withdrawn: ${reason}`,
        branch: null,
        created_at_ms: now,
        thread_key: targetKey,
      };

      selectedThread.status = "withdrawn";
      selectedThread.updated_at_ms = withdrawMsg.created_at_ms;
      selectedThread.latest_msg = withdrawMsg;
      selectedThread.messages = [...selectedThread.messages, withdrawMsg];
      threads = [...threads];

      isWithdrawing = false;
      withdrawReason = "";
      notice = "Ask withdrawn";
    } catch (err: unknown) {
      error = failure("withdraw", "the ask", err);
    } finally {
      busy = false;
    }
  }

  async function handlePostStructuredAsk() {
    const title = newAskTitle.trim();
    if (!title) {
      error = "Ask title or question is required";
      return;
    }
    busy = true;
    error = "";
    notice = "";
    try {
      const slug = title.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "").slice(0, 40) || "ask";
      const threadKey = `${newAskType}-${slug}-${Date.now() % 100000}`;
      const payload: StructuredAsk = {
        ask_type: newAskType,
        title,
        question: title,
        summary: newAskSummary.trim() || undefined,
        branch: newAskBranch.trim() || undefined,
      };
      if (newAskType === "decide") {
        const opts = newAskOptionsText
          .split("\n")
          .map((o) => o.trim())
          .filter(Boolean);
        if (opts.length < 2) {
          error = "Decision asks require at least two choices (one per line)";
          busy = false;
          return;
        }
        payload.options = opts;
        if (newAskRecommendation.trim()) {
          payload.recommendation = newAskRecommendation.trim();
        }
      } else if (newAskType === "do") {
        const steps = newAskStepsText
          .split("\n")
          .map((s) => s.trim())
          .filter(Boolean);
        if (steps.length === 0) {
          error = "Action checklist asks require at least one step";
          busy = false;
          return;
        }
        payload.steps = steps;
      } else if (newAskType === "review" && newAskBranch.trim()) {
        payload.target = newAskBranch.trim();
      }

      const body = JSON.stringify(payload);
      const toAgent = newAskToAgent.trim() || null;

      if (repoPath) {
        await invoke<InboxMessageReceipt>("cmd_inbox_post", {
          repoPath,
          fromAgent: "developer",
          toAgent: toAgent ?? undefined,
          threadKey,
          kind: "ask",
          title,
          body,
          branch: newAskBranch.trim() || undefined,
        });
        await refreshInbox();
      } else {
        const now = Date.now();
        const msg: BusMessage = {
          seq: (threads[0]?.latest_msg?.seq ?? 0) + 1,
          from_agent: "developer",
          to_agent: toAgent,
          kind: "ask",
          body,
          branch: newAskBranch.trim() || null,
          created_at_ms: now,
          thread_key: threadKey,
        };
        threads = [
          {
            thread_key: threadKey,
            title,
            status: "open",
            ask_type: newAskType,
            latest_msg: msg,
            messages: [msg],
            from_agent: "developer",
            to_agent: toAgent,
            updated_at_ms: now,
          },
          ...threads,
        ];
      }

      selectedThreadKey = threadKey;
      activeDrawer = "none";
      newAskTitle = "";
      newAskOptionsText = "";
      newAskRecommendation = "";
      newAskStepsText = "";
      newAskSummary = "";
      notice = "Structured ask posted to coordination bus";
    } catch (err: unknown) {
      error = failure("post", "the structured ask", err);
    } finally {
      busy = false;
    }
  }

  async function handleDecomposeTask() {
    const title = breakdownTitle.trim();
    if (!title || parsedBreakdownLines.length === 0) {
      error = "Task title and at least one subtask line are required";
      return;
    }
    if (!repoPath) {
      error = "Select a repository before filing decomposed tasks";
      return;
    }
    busy = true;
    error = "";
    notice = "";
    try {
      const res = await invoke<InboxDecomposeReceipt>("cmd_inbox_decompose_task", {
        repoPath,
        title,
        description: breakdownDescription.trim() || undefined,
        subtasks: previewPlan.subtasks.map((s) => s.title),
        assignAgent: breakdownAssignee.trim() || undefined,
        branch: breakdownBranch.trim() || undefined,
      });
      await refreshInbox();
      if (res?.thread_key) {
        selectedThreadKey = res.thread_key;
      }
      activeDrawer = "none";
      breakdownTitle = "";
      breakdownDescription = "";
      breakdownSubtasksText = "";
      notice = `Filed parent task with ${previewPlan.subtasks.length} subtasks and posted agent handoff`;
    } catch (err: unknown) {
      error = failure("break down", "the task", err);
    } finally {
      busy = false;
    }
  }

  function handleSendCustomReply() {
    if (!replyText.trim()) return;
    const body = replyText.trim();
    replyText = "";
    void sendAnswer(body);
  }

  const linkedTaskId = $derived(
    currentAsk?.task_id ?? (selectedThread ? extractTaskIdFromThreadKey(selectedThread.thread_key) : null)
  );

  const selectedOpenCount = $derived(
    threads.filter((t) => selectedKeys.has(t.thread_key) && t.status === "open").length
  );

  function handleToggleThreadSelect(key: string) {
    selectedKeys = toggleThreadSelection(selectedKeys, key);
  }

  function handleSelectVisible() {
    selectedKeys = selectVisibleThreads(filteredThreads);
  }

  function handleClearSelection() {
    selectedKeys = new Set();
    batchNote = "";
  }

  async function handleBatchWithdraw() {
    if (selectedKeys.size === 0 || busy) return;
    busy = true;
    error = "";
    notice = "";
    try {
      const res = await batchWithdrawOpenThreads({
        repoPath,
        threads,
        selectedKeys,
      });
      threads = res.updatedThreads;
      selectedKeys = new Set();
      if (res.errors.length > 0) {
        error = `Withdrew ${res.withdrawnKeys.length}. Couldn't withdraw the rest: ${res.errors.join("; ")}`;
      } else {
        notice = `Withdrew ${res.withdrawnKeys.length} open ${res.withdrawnKeys.length === 1 ? "ask" : "asks"}${res.skippedCount > 0 ? ` (${res.skippedCount} already settled skipped)` : ""}`;
      }
    } catch (err: unknown) {
      error = failure("withdraw", "the selected asks", err);
    } finally {
      busy = false;
    }
  }

  async function handleBatchResolve() {
    if (selectedKeys.size === 0 || !batchNote.trim() || busy) return;
    busy = true;
    error = "";
    notice = "";
    try {
      const res = await batchResolveThreadsWithNote({
        repoPath,
        threads,
        selectedKeys,
        note: batchNote,
      });
      threads = res.updatedThreads;
      selectedKeys = new Set();
      batchNote = "";
      if (res.errors.length > 0) {
        error = `Resolved ${res.resolvedKeys.length}. Couldn't resolve the rest: ${res.errors.join("; ")}`;
      } else {
        notice = `Posted resolution note across ${res.resolvedKeys.length} ${res.resolvedKeys.length === 1 ? "thread" : "threads"}`;
      }
    } catch (err: unknown) {
      error = failure("resolve", "the selected threads", err);
    } finally {
      busy = false;
    }
  }

  async function handleKickOffAgent() {
    if (!selectedThread || busy) return;
    busy = true;
    error = "";
    notice = "";
    try {
      const res = await kickOffAgentFromInboxThread({
        thread: selectedThread,
        ask: currentAsk,
        provider: kickoffProvider,
        mode: kickoffMode,
        repoPath,
      });
      if (repoPath) {
        await refreshInbox();
      } else if (res.handoffMessage) {
        selectedThread.latest_msg = res.handoffMessage;
        selectedThread.messages = [...selectedThread.messages, res.handoffMessage];
        selectedThread.updated_at_ms = res.handoffMessage.created_at_ms;
        threads = [...threads];
      }
      notice =
        res.launchedVia === "task_run"
          ? `Kicked off ${kickoffProvider} (${kickoffMode === "managed" ? "warm managed" : "worktree terminal"}) session`
          : `Opened interactive ${kickoffProvider} terminal with thread handoff`;
    } catch (err: unknown) {
      error = failure("kick off", "the agent session", err);
    } finally {
      busy = false;
    }
  }

  const currentMobileKickoff = $derived.by((): MobileKickoffDirective | null => {
    if (!selectedThread) return null;
    for (let i = selectedThread.messages.length - 1; i >= 0; i -= 1) {
      const parsed = parseMobileKickoffDirective(selectedThread.messages[i].body);
      if (parsed) return parsed;
    }
    return selectedThread.latest_msg?.body
      ? parseMobileKickoffDirective(selectedThread.latest_msg.body)
      : null;
  });

  const currentMobileBreakdown = $derived.by((): MobileTaskBreakdown | null => {
    if (!selectedThread) return null;
    for (let i = selectedThread.messages.length - 1; i >= 0; i -= 1) {
      const parsed = parseMobileTaskBreakdown(selectedThread.messages[i].body);
      if (parsed) return parsed;
    }
    return selectedThread.latest_msg?.body
      ? parseMobileTaskBreakdown(selectedThread.latest_msg.body)
      : null;
  });

  async function handleLaunchMobileKickoff(directive: MobileKickoffDirective) {
    if (!selectedThread || busy) return;
    kickoffProvider = directive.provider;
    kickoffMode = directive.mode;
    if (onkickoff) {
      await onkickoff(directive, selectedThread);
    }
    if (repoPath) {
      await handleKickOffAgent();
    } else {
      notice = `Prepared ${directive.provider} (${directive.mode}) session from Graft Mobile`;
    }
  }

  async function handleApplyMobileBreakdown(breakdown: MobileTaskBreakdown) {
    if (!selectedThread || busy) return;
    if (breakdown.taskId && onopenTask) {
      await onopenTask(breakdown.taskId);
    }
    if (!repoPath) {
      breakdownTitle = breakdown.title;
      breakdownSubtasksText = breakdown.steps.join("\n");
      breakdownAssignee = breakdown.assignAgent ?? "";
      breakdownBranch = breakdown.branch ?? "";
      activeDrawer = "decompose";
      notice = `Loaded ${breakdown.steps.length} subtasks from Graft Mobile`;
      return;
    }
    busy = true;
    error = "";
    notice = "";
    try {
      await invoke<InboxDecomposeReceipt>("cmd_inbox_decompose_task", {
        repoPath,
        title: breakdown.title,
        subtasks: breakdown.steps,
        assignAgent: breakdown.assignAgent,
        branch: breakdown.branch,
      });
      await refreshInbox();
      notice = `Applied ${breakdown.steps.length} subtasks from Graft Mobile`;
    } catch (err: unknown) {
      error = failure("apply", "the subtasks from Graft Mobile", err);
    } finally {
      busy = false;
    }
  }
</script>

<div class="agent-inbox" class:embedded data-testid="agent-inbox">
  <!-- Top Navigation & Summary Toolbar -->
  <header class="inbox-toolbar">
    <div class="toolbar-title">
      <!-- Embedded, the Agents header above already names this view (its
           Inbox switch), so a second title would only repeat it. -->
      {#if !embedded}
        <Inbox class="w-4 h-4 text-accent" />
        <span class="font-semibold text-sm">Agent inbox</span>
      {/if}
      {#if openAsksCount > 0}
        <span class="gp-chip" data-tone="warn" data-kind="label" data-testid="open-asks-badge">
          {openAsksCount} open {openAsksCount === 1 ? "ask" : "asks"}
        </span>
      {/if}
    </div>

    <div class="toolbar-actions">
      <!-- Search -->
      <div class="relative">
        <Search class="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-textMuted" />
        <input
          type="search"
          class="gp-field w-44 pl-8"
          placeholder="Search threads…"
          bind:value={searchQuery}
          aria-label="Search threads"
        />
      </div>

      <!-- Status Filter Tabs -->
      <div class="gp-segmented" class:gp-liquid-tabs={macos} role="tablist" aria-label="Status filters">
        {#each STATUS_FILTERS as filter (filter.id)}
          <button
            type="button"
            class="gp-seg-btn"
            data-active={statusFilter === filter.id}
            onclick={() => (statusFilter = filter.id)}
            role="tab"
            aria-selected={statusFilter === filter.id}
          >
            {filter.label}
          </button>
        {/each}
      </div>

      <button
        type="button"
        class="gp-btn aria-pressed:bg-accent/12 aria-pressed:text-accent"
        data-size="sm"
        aria-pressed={activeDrawer === "ask"}
        onclick={() => (activeDrawer = activeDrawer === "ask" ? "none" : "ask")}
        data-testid="new-ask-btn"
        title="Post a structured Vespin ask or handoff to an agent"
      >
        <Plus class="w-3.5 h-3.5" /> New ask
      </button>

      <button
        type="button"
        class="gp-btn aria-pressed:bg-accent/12 aria-pressed:text-accent"
        data-size="sm"
        aria-pressed={activeDrawer === "decompose"}
        onclick={() => (activeDrawer = activeDrawer === "decompose" ? "none" : "decompose")}
        data-testid="decompose-task-btn"
        title="Break down work into Linear-style subtasks and hand off to an agent"
      >
        <Layers class="w-3.5 h-3.5" /> Break down task
      </button>

      <button
        type="button"
        class="gp-icon-btn"
        data-size="sm"
        onclick={() => refreshInbox()}
        disabled={busy}
        aria-label="Refresh inbox"
        title="Refresh inbox"
      >
        <RefreshCw class="w-3.5 h-3.5 {busy ? 'animate-spin' : ''}" />
      </button>
    </div>
  </header>

  {#if lanes.length > 0}
    <div class="lanes-strip" data-testid="active-lanes-strip" aria-label="Active agent worktrees">
      <span class="lanes-label">Active worktrees ({lanes.length}):</span>
      {#each lanes as lane (lane.agent_id)}
        <span class="gp-chip" data-tone="accent" data-kind="label" title={lane.worktree_path || lane.branch}>
          <Bot class="w-3 h-3" />
          <span class="font-medium">{lane.agent_id}</span>
          <GitBranch class="w-2.5 h-2.5 opacity-70" />
          <span class="font-mono">{lane.branch}</span>
        </span>
      {/each}
    </div>
  {/if}

  {#if activeDrawer === "ask"}
    <section class="composer-drawer" data-testid="new-ask-drawer" aria-label="New structured ask">
      <div class="drawer-header">
        <span class="font-semibold text-xs">Post a structured ask or agent handoff</span>
        <button type="button" class="gp-icon-btn" data-size="sm" onclick={() => (activeDrawer = "none")} aria-label="Close new ask form">
          <X class="w-3.5 h-3.5" />
        </button>
      </div>
      <div class="drawer-grid">
        <select bind:value={newAskType} class="gp-select" aria-label="Ask type">
          <option value="decide">Decide (choices + recommendation)</option>
          <option value="do">Do (action checklist)</option>
          <option value="answer">Answer (direct question)</option>
          <option value="review">Review (branch or target)</option>
        </select>
        <input
          type="text"
          placeholder="Target agent (optional, e.g. claude-worker-1)"
          bind:value={newAskToAgent}
          class="gp-field"
        />
        <input
          type="text"
          placeholder="Branch or review target (optional)"
          bind:value={newAskBranch}
          class="gp-field"
        />
      </div>
      <input
        type="text"
        placeholder="Question or ask title…"
        bind:value={newAskTitle}
        class="gp-field w-full mt-2"
      />
      {#if newAskType === "decide"}
        <textarea
          rows="2"
          placeholder="Choices, one per line (at least 2)…"
          bind:value={newAskOptionsText}
          class="gp-field gp-field-multi w-full mt-2"
        ></textarea>
        <input
          type="text"
          placeholder="Recommended choice (optional)…"
          bind:value={newAskRecommendation}
          class="gp-field w-full mt-2"
        />
      {:else if newAskType === "do"}
        <textarea
          rows="3"
          placeholder="Checklist steps, one per line…"
          bind:value={newAskStepsText}
          class="gp-field gp-field-multi w-full mt-2"
        ></textarea>
      {/if}
      <input
        type="text"
        placeholder="Summary or context notes (optional)…"
        bind:value={newAskSummary}
        class="gp-field w-full mt-2"
      />
      <div class="drawer-actions">
        <button
          type="button"
          class="gp-btn"
          data-variant="primary"
          data-size="sm"
          disabled={busy}
          onclick={handlePostStructuredAsk}
          data-testid="submit-new-ask-btn"
        >
          <Send class="w-3.5 h-3.5" /> Post ask
        </button>
      </div>
    </section>
  {:else if activeDrawer === "decompose"}
    <section class="composer-drawer" data-testid="decompose-task-drawer" aria-label="Break down task">
      <div class="drawer-header">
        <span class="font-semibold text-xs">Break down a task and start an agent</span>
        <button type="button" class="gp-icon-btn" data-size="sm" onclick={() => (activeDrawer = "none")} aria-label="Close task breakdown form">
          <X class="w-3.5 h-3.5" />
        </button>
      </div>
      <div class="drawer-grid">
        <input
          type="text"
          placeholder="Parent task title…"
          bind:value={breakdownTitle}
          class="gp-field"
        />
        <input
          type="text"
          placeholder="Assign agent (optional, e.g. agy-worker)"
          bind:value={breakdownAssignee}
          class="gp-field"
        />
        <input
          type="text"
          placeholder="Branch (optional)"
          bind:value={breakdownBranch}
          class="gp-field"
        />
      </div>
      <input
        type="text"
        placeholder="Parent description or acceptance context (optional)…"
        bind:value={breakdownDescription}
        class="gp-field w-full mt-2"
      />
      <textarea
        rows="3"
        placeholder="Subtasks, one per line…"
        bind:value={breakdownSubtasksText}
        class="gp-field gp-field-multi w-full mt-2"
      ></textarea>
      {#if previewPlan.subtasks.length > 0}
        <div class="plan-preview">
          <span class="text-xs text-textMuted">
            Will file parent + {previewPlan.subtasks.length} linked {previewPlan.subtasks.length === 1 ? "subtask" : "subtasks"} ({previewPlan.subtasks.map((s) => s.key).join(", ")})
          </span>
        </div>
      {/if}
      <div class="drawer-actions">
        <button
          type="button"
          class="gp-btn"
          data-variant="primary"
          data-size="sm"
          disabled={busy || !breakdownTitle.trim() || previewPlan.subtasks.length === 0}
          onclick={handleDecomposeTask}
          data-testid="submit-decompose-btn"
        >
          <Layers class="w-3.5 h-3.5" /> File subtasks and post handoff
        </button>
      </div>
    </section>
  {/if}

  {#if error}
    <div class="inbox-alert">
      <div class="gp-notice" data-tone="danger" role="alert">
        <AlertCircle class="w-4 h-4 shrink-0" style="color: var(--gp-notice-ink)" />
        <span class="min-w-0 flex-1">{error}</span>
        <button type="button" class="gp-icon-btn" data-size="sm" onclick={() => (error = "")} aria-label="Dismiss error">
          <X class="w-3.5 h-3.5" />
        </button>
      </div>
    </div>
  {/if}

  {#if notice}
    <div class="inbox-alert">
      <div class="gp-notice" data-tone="ok" role="status">
        <CheckCircle2 class="w-4 h-4 shrink-0" style="color: var(--gp-notice-ink)" />
        <span class="min-w-0 flex-1">{notice}</span>
        <button type="button" class="gp-icon-btn" data-size="sm" onclick={() => (notice = "")} aria-label="Dismiss notice">
          <X class="w-3.5 h-3.5" />
        </button>
      </div>
    </div>
  {/if}

  {#if selectedKeys.size > 0}
    <div class="batch-toolbar" data-testid="inbox-batch-toolbar" role="region" aria-label="Batch thread actions">
      <div class="batch-meta">
        <span class="font-medium">{selectedKeys.size} selected</span>
        <button
          type="button"
          class="gp-btn"
          data-size="sm"
          onclick={handleSelectVisible}
          disabled={busy || filteredThreads.length === 0}
          data-testid="batch-select-visible-btn"
        >
          Select visible ({filteredThreads.length})
        </button>
        <button
          type="button"
          class="gp-btn"
          data-size="sm"
          onclick={handleClearSelection}
          disabled={busy}
          data-testid="batch-clear-btn"
        >
          Clear
        </button>
      </div>
      <div class="batch-actions">
        <button
          type="button"
          class="gp-btn text-danger"
          data-size="sm"
          onclick={() => void handleBatchWithdraw()}
          disabled={busy || selectedOpenCount === 0}
          data-testid="batch-withdraw-btn"
          title="Withdraw all open asks among selected threads"
        >
          <XCircle class="w-3.5 h-3.5" /> Batch withdraw open asks ({selectedOpenCount})
        </button>
        <input
          type="text"
          class="gp-field"
          placeholder="Concise resolution note…"
          aria-label="Batch resolution note"
          bind:value={batchNote}
          disabled={busy}
          onkeydown={(e) => {
            if (e.key === "Enter" && batchNote.trim()) {
              e.preventDefault();
              void handleBatchResolve();
            }
          }}
        />
        <button
          type="button"
          class="gp-btn"
          data-size="sm"
          onclick={() => void handleBatchResolve()}
          disabled={busy || !batchNote.trim()}
          data-testid="batch-resolve-btn"
        >
          <CheckCircle2 class="w-3.5 h-3.5" /> Batch resolve note
        </button>
      </div>
    </div>
  {/if}

  <!-- Split Pane: Threads & Detail -->
  <div class="inbox-body">
    <!-- Thread List Pane -->
    <aside class="threads-pane" aria-label="Conversation threads">
      {#if filteredThreads.length > 0 && selectedKeys.size === 0}
        <div class="threads-pane-tools">
          <button
            type="button"
            class="gp-btn"
            data-size="sm"
            onclick={handleSelectVisible}
            disabled={busy}
          >
            Select visible ({filteredThreads.length})
          </button>
        </div>
      {/if}
      {#if filteredThreads.length === 0}
        <div data-testid="empty-threads">
          <EmptyState
            compact
            icon={MessageSquare}
            title="No threads found"
            hint={statusFilter === "all" ? "No agent asks or messages recorded" : `No threads matching "${statusFilter}"`}
          />
        </div>
      {:else}
        <ul class="threads-list" role="list">
          {#each filteredThreads as thread (thread.thread_key)}
            {@const isSelected = selectedThread?.thread_key === thread.thread_key}
            {@const ask = thread.ask_type}
            <li class="thread-row">
              <span class="thread-checkbox" data-testid="thread-select-{thread.thread_key}">
                <Toggle
                  checked={selectedKeys.has(thread.thread_key)}
                  onchange={() => handleToggleThreadSelect(thread.thread_key)}
                  ariaLabel="Select thread {thread.title}"
                />
              </span>
              <button
                type="button"
                class="thread-card"
                class:selected={isSelected}
                data-status={thread.status}
                onclick={() => selectThread(thread.thread_key)}
                data-testid="thread-card-{thread.thread_key}"
              >
                <div class="thread-card-header">
                  <div class="thread-meta">
                    {#if ask === "decide"}
                      <span class="gp-chip" data-tone="accent" data-kind="label" title="Decision ask">
                        <Vote class="w-3 h-3" /> Decide
                      </span>
                    {:else if ask === "do"}
                      <span class="gp-chip" data-tone="info" data-kind="label" title="Checklist ask">
                        <ListTodo class="w-3 h-3" /> Do
                      </span>
                    {:else if ask === "answer"}
                      <span class="gp-chip" data-tone="warn" data-kind="label" title="Question ask">
                        <HelpCircle class="w-3 h-3" /> Answer
                      </span>
                    {:else if ask === "review"}
                      <span class="gp-chip" data-tone="neutral" data-kind="label" title="Review ask">
                        <FileCode class="w-3 h-3" /> Review
                      </span>
                    {/if}
                    <span class="agent-name" title="Sender agent">
                      <Bot class="w-2.5 h-2.5" /> {thread.from_agent}
                    </span>
                    {#if isMobileOriginated(thread.from_agent)}
                      <span class="gp-chip" data-tone="accent" data-kind="label" data-testid="mobile-origin-badge" title="Sent from Graft Mobile on phone">
                        Phone · Graft Mobile
                      </span>
                    {/if}
                  </div>

                  <span class="gp-chip" data-tone={STATUS_TONE[thread.status] ?? "neutral"} data-kind="label">
                    {thread.status}
                  </span>
                </div>

                <h4 class="thread-title">{thread.title}</h4>

                <div class="thread-footer">
                  <span class="thread-slug font-mono">{thread.thread_key}</span>
                  <span class="thread-time" title={new Date(thread.updated_at_ms).toLocaleString()}>
                    {formatRelativeTime(thread.updated_at_ms)}
                  </span>
                </div>
              </button>
            </li>
          {/each}
        </ul>
      {/if}
    </aside>

    <!-- Thread Detail Pane -->
    <main class="thread-detail-pane" aria-label="Thread detail">
      {#if !selectedThread}
        <EmptyState compact icon={MessageSquare} title="Select a thread" hint="Pick a thread to view details and respond." />
      {:else}
        <!-- Selected Thread Header -->
        <header class="detail-header">
          <div class="detail-header-main">
            <h3 class="detail-title">{selectedThread.title}</h3>
            <div class="detail-tags">
              <span class="detail-key font-mono">{selectedThread.thread_key}</span>
              <span class="gp-chip" data-tone={STATUS_TONE[selectedThread.status] ?? "neutral"} data-kind="label">{selectedThread.status}</span>
              {#if isMobileOriginated(selectedThread.from_agent)}
                <span class="gp-chip" data-tone="accent" data-kind="label" data-testid="detail-mobile-origin-badge">
                  Phone · Graft Mobile
                </span>
              {/if}
              <span class="agent-route">
                <Bot class="w-3 h-3" /> {selectedThread.from_agent}
                {#if selectedThread.to_agent}
                  <ChevronRight class="w-2.5 h-2.5" /> {selectedThread.to_agent}
                {/if}
              </span>
            </div>
          </div>

          <div class="detail-header-actions">
            {#if currentMobileKickoff}
              <button
                type="button"
                class="gp-btn"
                data-variant="primary"
                data-size="sm"
                onclick={() => currentMobileKickoff && void handleLaunchMobileKickoff(currentMobileKickoff)}
                disabled={busy}
                data-testid="mobile-launch-session-btn"
                title="Run the mobile-requested {currentMobileKickoff.provider} ({currentMobileKickoff.mode}) session"
              >
                <Bot class="w-3.5 h-3.5" /> Run session
              </button>
            {/if}
            {#if currentMobileBreakdown}
              <button
                type="button"
                class="gp-btn"
                data-size="sm"
                onclick={() => currentMobileBreakdown && void handleApplyMobileBreakdown(currentMobileBreakdown)}
                disabled={busy}
                data-testid="mobile-apply-subtasks-btn"
                title="Apply {currentMobileBreakdown.steps.length} subtasks from Graft Mobile"
              >
                <Layers class="w-3.5 h-3.5" /> Apply subtasks
              </button>
            {/if}
            {#if linkedTaskId && onopenTask}
              <button
                type="button"
                class="gp-btn"
                data-size="sm"
                onclick={() => linkedTaskId && onopenTask(linkedTaskId)}
                data-testid="open-task-btn"
                title="Open linked board task"
              >
                <Layers class="w-3.5 h-3.5" /> Open task
              </button>
            {/if}
            {#if selectedThread.status === "open" && !isWithdrawing}
              <button
                type="button"
                class="gp-btn text-danger"
                data-size="sm"
                onclick={() => (isWithdrawing = true)}
                title="Withdraw this ask"
              >
                <XCircle class="w-3.5 h-3.5" /> Withdraw ask
              </button>
            {/if}
          </div>
        </header>

        <!-- Direct Agent Kickoff Bar -->
        <div class="kickoff-bar" data-testid="kickoff-agent-bar" role="region" aria-label="Kick off agent">
          <span class="kickoff-label">
            <Bot class="w-3.5 h-3.5 text-accent" /> Delegate thread:
          </span>
          <select
            class="gp-select"
            aria-label="Kickoff agent provider"
            bind:value={kickoffProvider}
            disabled={busy}
          >
            {#each KICKOFF_PROVIDERS as provider (provider.id)}
              <option value={provider.id}>{provider.label}</option>
            {/each}
          </select>
          <select
            class="gp-select"
            aria-label="Kickoff session mode"
            bind:value={kickoffMode}
            disabled={busy}
          >
            <option value="managed">Managed (warm)</option>
            <option value="terminal">Terminal</option>
          </select>
          <button
            type="button"
            class="gp-btn"
            data-variant="primary"
            data-size="sm"
            onclick={() => void handleKickOffAgent()}
            disabled={busy}
            data-testid="kickoff-agent-btn"
          >
            <Bot class="w-3.5 h-3.5" /> Kick off agent
          </button>
        </div>

        <!-- Withdraw Modal/Banner -->
        {#if isWithdrawing}
          <div class="withdraw-banner" role="region" aria-label="Withdraw ask">
            <div class="withdraw-header">
              <span class="font-medium">Withdraw ask</span>
              <button
                type="button"
                class="gp-icon-btn"
                data-size="sm"
                onclick={() => (isWithdrawing = false)}
                aria-label="Cancel withdraw"
              >
                <X class="w-3.5 h-3.5" />
              </button>
            </div>
            <p class="text-xs text-textMuted mb-2">
              Explain why this ask is being withdrawn (e.g. resolved independently or obsolete).
            </p>
            <div class="withdraw-inputs">
              <input
                type="text"
                placeholder="Reason for withdrawal…"
                bind:value={withdrawReason}
                class="gp-field flex-1"
              />
              <button
                type="button"
                class="gp-btn"
                data-variant="danger"
                data-size="sm"
                onclick={handleWithdraw}
                disabled={busy}
              >
                Confirm withdrawal
              </button>
            </div>
          </div>
        {/if}

        <!-- Interactive Structured Ask Widget (when ask is Open) -->
        {#if selectedThread.status === "open" && currentAsk}
          <section class="structured-ask-box gp-card" data-testid="structured-ask-box">
            <div class="ask-header">
              <div class="ask-type-label {currentAsk.ask_type}">
                {#if currentAsk.ask_type === "decide"}
                  <Vote class="w-4 h-4" /> Structured decision request
                {:else if currentAsk.ask_type === "do"}
                  <ListTodo class="w-4 h-4" /> Structured action checklist
                {:else if currentAsk.ask_type === "answer"}
                  <HelpCircle class="w-4 h-4" /> Information request
                {:else if currentAsk.ask_type === "review"}
                  <FileCode class="w-4 h-4" /> Code review request
                {/if}
              </div>
              <span class="text-xs text-textMuted">Requires developer response</span>
            </div>

            <!-- Question/Prompt -->
            <p class="ask-prompt">
              {currentAsk.question || currentAsk.prompt || currentAsk.title || selectedThread.title}
            </p>

            {#if currentAsk.summary && currentAsk.ask_type !== "review"}
              <p class="text-xs text-textMuted mb-3">{currentAsk.summary}</p>
            {/if}

            <!-- Case 1: DECIDE -->
            {#if currentAsk.ask_type === "decide" && currentAsk.options}
              <div class="decide-options">
                {#each currentAsk.options as option, idx}
                  {@const isRec =
                    currentAsk.recommendation === option ||
                    (typeof currentAsk.default_option === "number" && currentAsk.default_option === idx)}
                  <button
                    type="button"
                    class="gp-row w-full text-left"
                    data-selected={selectedDecision === option}
                    onclick={() => (selectedDecision = option)}
                    data-testid="decide-option-{option}"
                  >
                    <span class="option-check">
                      {#if selectedDecision === option}
                        <Check class="w-3.5 h-3.5" />
                      {/if}
                    </span>
                    <span class="option-text">{option}</span>
                    {#if isRec}
                      <span class="gp-chip ml-auto" data-tone="ok" data-kind="label" title="Agent recommended">Recommended</span>
                    {/if}
                  </button>
                {/each}
              </div>

              <div class="ask-actions">
                <button
                  type="button"
                  class="gp-btn"
                  data-variant="primary"
                  data-size="sm"
                  disabled={!selectedDecision || busy}
                  onclick={() => selectedDecision && handleAnswerDecision(selectedDecision)}
                  data-testid="confirm-decision-btn"
                >
                  <CheckCircle2 class="w-3.5 h-3.5" /> Confirm decision
                </button>
              </div>

            <!-- Case 2: DO -->
            {:else if currentAsk.ask_type === "do" && currentDoSteps}
              {#if currentStepRollup}
                <div class="rollup-summary" data-testid="subtask-rollup">
                  <span class="text-xs text-textMuted">
                    {formatSubtaskProgress(currentStepRollup)}
                  </span>
                </div>
              {/if}
              <div class="do-checklist">
                {#each currentDoSteps as step, idx}
                  <div class="checklist-item">
                    <span data-testid="do-step-{idx}">
                      <Toggle
                        checked={completedSteps[idx] === true}
                        onchange={(next) => (completedSteps[idx] = next)}
                        ariaLabel="Mark done: {step}"
                      />
                    </span>
                    <span class="step-text" class:checked={completedSteps[idx]}>{step}</span>
                  </div>
                {/each}
              </div>

              {#if currentAsk.notes}
                <p class="text-xs text-textMuted italic mt-2">{currentAsk.notes}</p>
              {/if}

              <div class="ask-actions">
                <button
                  type="button"
                  class="gp-btn"
                  data-variant="primary"
                  data-size="sm"
                  disabled={busy}
                  onclick={handleCompleteSteps}
                  data-testid="complete-steps-btn"
                >
                  <CheckCircle2 class="w-3.5 h-3.5" /> Mark steps done
                </button>
              </div>

            <!-- Case 3: ANSWER -->
            {:else if currentAsk.ask_type === "answer"}
              <div class="answer-box">
                <textarea
                  rows="3"
                  placeholder="Type your answer for the agent…"
                  bind:value={textAnswer}
                  class="gp-field gp-field-multi w-full mb-2"
                  data-testid="answer-input"
                ></textarea>
                <div class="ask-actions">
                  <button
                    type="button"
                    class="gp-btn"
                    data-variant="primary"
                    data-size="sm"
                    disabled={!textAnswer.trim() || busy}
                    onclick={handleSubmitAnswer}
                    data-testid="submit-answer-btn"
                  >
                    <Send class="w-3.5 h-3.5" /> Submit answer
                  </button>
                </div>
              </div>

            <!-- Case 4: REVIEW -->
            {:else if currentAsk.ask_type === "review"}
              <div class="review-box">
                {#if currentAsk.target || currentAsk.branch}
                  <div class="review-field">
                    <span class="field-label">Target:</span>
                    <span class="font-mono text-xs">{currentAsk.target || currentAsk.branch}</span>
                  </div>
                {/if}
                {#if currentAsk.summary}
                  <div class="review-summary">
                    <span class="field-label">Summary:</span>
                    <p class="text-xs">{currentAsk.summary}</p>
                  </div>
                {/if}
                {#if currentAsk.diff}
                  <pre class="review-diff"><code>{currentAsk.diff}</code></pre>
                {/if}

                <div class="review-feedback">
                  <input
                    type="text"
                    placeholder="Optional review notes or requested changes…"
                    bind:value={reviewNotes}
                    class="gp-field w-full mb-2"
                  />
                  <div class="ask-actions split">
                    <button
                      type="button"
                      class="gp-btn"
                      data-variant="primary"
                      data-size="sm"
                      disabled={busy}
                      onclick={() => handleSubmitReview(true)}
                      data-testid="approve-review-btn"
                    >
                      <Check class="w-3.5 h-3.5" /> Approve
                    </button>
                    <button
                      type="button"
                      class="gp-btn"
                      data-size="sm"
                      disabled={busy}
                      onclick={() => handleSubmitReview(false)}
                      data-testid="reject-review-btn"
                    >
                      <X class="w-3.5 h-3.5" /> Request changes
                    </button>
                  </div>
                </div>
              </div>
            {/if}
          </section>
        {/if}

        <!-- Status Callout when Answered or Withdrawn -->
        {#if selectedThread.status === "answered"}
          <div class="thread-status-banner">
            <div class="gp-notice" data-tone="ok">
              <CheckCircle2 class="w-4 h-4 shrink-0" style="color: var(--gp-notice-ink)" />
              <span>This ask has been answered and resolved.</span>
            </div>
          </div>
        {:else if selectedThread.status === "withdrawn"}
          <div class="thread-status-banner">
            <div class="gp-notice" data-tone="info">
              <XCircle class="w-4 h-4 shrink-0" style="color: var(--gp-notice-ink)" />
              <span>This ask was withdrawn.</span>
            </div>
          </div>
        {/if}

        <!-- Message History Timeline -->
        <section class="messages-timeline" aria-label="Message history">
          <h4 class="timeline-heading">
            <MessageSquare class="w-3.5 h-3.5" /> Timeline ({selectedThread.messages.length})
          </h4>

          <div class="timeline-list">
            {#each selectedThread.messages as msg, idx (msg.seq ?? idx)}
              {@const isDev = msg.from_agent === "developer" || msg.from_agent === "human-dev"}
              <div class="timeline-message" class:developer={isDev}>
                <div class="message-header">
                  <div class="author-badge">
                    {#if isDev}
                      <User class="w-3 h-3 text-accent" />
                      <span class="font-medium">Developer</span>
                    {:else}
                      <Bot class="w-3 h-3 text-info" />
                      <span class="font-medium">{msg.from_agent}</span>
                    {/if}
                    {#if isMobileOriginated(msg.from_agent)}
                      <span class="gp-chip" data-tone="accent" data-kind="label" data-testid="timeline-mobile-origin-badge" title="Sent from Graft Mobile on phone">
                        Phone · Graft Mobile
                      </span>
                    {/if}
                  </div>

                  <span class="gp-chip" data-tone="neutral" data-kind="label">{msg.kind}</span>
                  <span class="msg-time">{formatRelativeTime(msg.created_at_ms)}</span>
                </div>

                <div class="message-content">
                  {#if msg.kind === "ask" && parseStructuredAsk(msg.body)}
                    {@const a = parseStructuredAsk(msg.body)}
                    <div class="parsed-ask-snippet">
                      <span class="font-semibold text-xs">
                        {a?.ask_type ? a.ask_type.charAt(0).toUpperCase() + a.ask_type.slice(1) : ""}: {a?.question || a?.prompt || a?.title}
                      </span>
                      {#if a?.options}
                        <span class="text-xs text-textMuted">Choices: {a.options.join(", ")}</span>
                      {/if}
                    </div>
                  {:else}
                    <p class="message-text">{msg.body}</p>
                  {/if}
                </div>
              </div>
            {/each}
          </div>
        </section>

        <!-- Quick Reply Composer -->
        <footer class="reply-composer">
          <div class="composer-inner">
            <textarea
              rows="2"
              placeholder="Send message to thread…"
              bind:value={replyText}
              class="gp-field gp-field-multi flex-1"
              onkeydown={(e) => {
                if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
                  e.preventDefault();
                  handleSendCustomReply();
                }
              }}
            ></textarea>
            <button
              type="button"
              class="gp-btn"
              data-variant="primary"
              data-size="md"
              disabled={!replyText.trim() || busy}
              onclick={handleSendCustomReply}
              aria-label="Send reply"
              title="Send reply ({shortcutTextLabel('⌘↵', $hostPlatform.os)})"
            >
              <Send class="w-3.5 h-3.5" />
            </button>
          </div>
        </footer>
      {/if}
    </main>
  </div>
</div>

<style>
  /* Tone separates, lines do not (design language section 3): the toolbar, the
     strips and the panes are told apart by a tonal step and by spacing, never
     by a hairline. Buttons, chips, fields and notices are the app's own
     (gp-btn, gp-chip, gp-field, gp-select, gp-notice); what is left here is
     layout and the tonal steps. */
  .agent-inbox {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 480px;
    background: var(--mac-fill-surface, rgb(var(--c-surface)));
    color: rgb(var(--c-text));
    font-size: 0.75rem;
    border-radius: var(--gp-r-md);
    overflow: hidden;
  }

  /* Embedded in the Agents pane it is part of that pane, not a panel laid on
     it: the pane's own background shows through and the outer box goes. */
  .agent-inbox.embedded {
    background: transparent;
    border-radius: 0;
    min-height: 0;
  }

  .agent-inbox.embedded .inbox-toolbar {
    background: transparent;
    padding: 8px 14px;
  }

  .agent-inbox.embedded .threads-pane,
  .agent-inbox.embedded .thread-detail-pane {
    background: transparent;
  }

  /* Toolbar */
  .inbox-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 10px 16px;
    background: var(--mac-fill-surface-hover, rgb(var(--c-surface-hover)));
    flex-wrap: wrap;
  }

  .toolbar-title {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .toolbar-actions {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }

  .lanes-strip {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 16px;
    background: rgb(var(--c-bg) / 0.5);
    overflow-x: auto;
    font-size: 0.6875rem;
  }

  .lanes-label {
    color: rgb(var(--c-text-muted));
    font-weight: 600;
    white-space: nowrap;
  }

  .composer-drawer {
    padding: 12px 16px;
    background: var(--mac-fill-bg, rgb(var(--c-bg)));
  }

  .drawer-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 8px;
  }

  .drawer-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
    gap: 8px;
  }

  .mt-2 {
    margin-top: 8px;
  }

  .plan-preview {
    margin-top: 6px;
  }

  .drawer-actions {
    display: flex;
    justify-content: flex-end;
    margin-top: 8px;
  }

  .detail-header-actions {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .rollup-summary {
    margin-bottom: 8px;
  }

  /* Alerts: the notice carries the tone; this only insets it. */
  .inbox-alert {
    padding: 8px 14px 0;
  }

  /* Body Split */
  .inbox-body {
    display: flex;
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }

  /* Threads Pane: one tonal step below the detail pane. */
  .threads-pane {
    width: 320px;
    flex-shrink: 0;
    overflow-y: auto;
    background: rgb(var(--c-bg) / 0.35);
  }

  .threads-list {
    list-style: none;
    margin: 0;
    padding: 6px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  /* A thread is a multi-line row: tone on rest, accent on hover and selection. */
  .thread-card {
    width: 100%;
    text-align: left;
    background: var(--mac-fill-bg, rgb(var(--c-bg)));
    border-radius: var(--gp-r-md);
    padding: 10px 12px;
    cursor: pointer;
    display: flex;
    flex-direction: column;
    gap: 6px;
    color: inherit;
    transition-property: color, background-color;
    transition-duration: var(--gp-dur-instant);
    transition-timing-function: var(--gp-ease-effect);
  }

  .thread-card:hover {
    background: var(--mac-fill-surface-hover, rgb(var(--c-surface-hover)));
  }

  .thread-card.selected {
    background: rgb(var(--c-accent) / 0.12);
  }

  .thread-card-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 6px;
  }

  .thread-meta {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }

  .agent-name {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-size: 0.6875rem;
    color: rgb(var(--c-text-muted));
  }

  .thread-title {
    margin: 0;
    font-size: 0.75rem;
    font-weight: 600;
    line-height: 1.3;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .thread-footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    font-size: 0.625rem;
    color: rgb(var(--c-text-muted));
  }

  /* Detail Pane */
  .thread-detail-pane {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-width: 0;
    overflow-y: auto;
    background: var(--mac-fill-surface, rgb(var(--c-surface)));
  }

  .detail-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
    padding: 14px 18px;
    background: rgb(var(--c-bg) / 0.35);
  }

  .detail-title {
    margin: 0 0 6px 0;
    font-size: 0.9375rem;
    font-weight: 600;
  }

  .detail-tags {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    font-size: 0.6875rem;
  }

  .agent-route {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: rgb(var(--c-text-muted));
  }

  .withdraw-banner {
    margin: 12px 18px;
    padding: 12px;
    background: rgb(var(--c-danger) / 0.1);
    border-radius: var(--gp-r-md);
  }

  .withdraw-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 4px;
  }

  .withdraw-inputs {
    display: flex;
    gap: 8px;
  }

  /* Structured Ask Box (a gp-card) */
  .structured-ask-box {
    margin: 14px 18px;
    padding: 14px;
    --gp-pad: 14px;
  }

  .ask-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 10px;
  }

  .ask-type-label {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-weight: 600;
    font-size: 0.75rem;
    color: rgb(var(--c-accent));
  }

  .ask-prompt {
    font-size: 0.8125rem;
    font-weight: 500;
    margin: 0 0 12px 0;
  }

  .decide-options {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-bottom: 12px;
  }

  .option-check {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    flex-shrink: 0;
  }

  .do-checklist {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-bottom: 12px;
  }

  .checklist-item {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 0.75rem;
  }

  .step-text.checked {
    text-decoration: line-through;
    color: rgb(var(--c-text-muted));
  }

  .ask-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }

  .ask-actions.split {
    justify-content: flex-start;
  }

  .review-diff {
    background: var(--mac-fill-bg, rgb(var(--c-bg)));
    padding: 8px;
    border-radius: var(--gp-r-xs);
    max-height: 140px;
    overflow: auto;
    font-size: 0.6875rem;
    margin: 8px 0;
  }

  /* Status Banner: the notice carries the tone; this only insets it. */
  .thread-status-banner {
    margin: 12px 18px;
  }

  /* Timeline */
  .messages-timeline {
    flex: 1;
    padding: 14px 18px;
    overflow-y: auto;
  }

  .timeline-heading {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0 0 12px 0;
    font-size: 0.75rem;
    font-weight: 600;
    color: rgb(var(--c-text-muted));
  }

  .timeline-list {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .timeline-message {
    padding: 8px 12px;
    background: rgb(var(--c-bg) / 0.5);
    border-radius: var(--gp-r-sm);
  }

  .timeline-message.developer {
    background: rgb(var(--c-accent) / 0.1);
  }

  .message-header {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 0.6875rem;
    margin-bottom: 4px;
  }

  .author-badge {
    display: flex;
    align-items: center;
    gap: 4px;
  }

  .msg-time {
    margin-left: auto;
    color: rgb(var(--c-text-muted));
  }

  .message-text {
    margin: 0;
    font-size: 0.75rem;
    white-space: pre-wrap;
    line-height: 1.4;
  }

  /* Reply Composer */
  .reply-composer {
    padding: 10px 18px;
    background: rgb(var(--c-bg) / 0.35);
  }

  .composer-inner {
    display: flex;
    align-items: flex-end;
    gap: 8px;
  }

  .batch-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    padding: 8px 16px;
    background: var(--mac-fill-surface-hover, rgb(var(--c-surface-hover)));
    flex-wrap: wrap;
    font-size: 0.6875rem;
  }

  .batch-meta,
  .batch-actions {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }

  .threads-pane-tools {
    padding: 6px 10px 2px;
    display: flex;
    justify-content: flex-end;
  }

  .thread-row {
    display: flex;
    align-items: flex-start;
    gap: 6px;
  }

  .thread-checkbox {
    margin-top: 10px;
    margin-left: 6px;
    flex-shrink: 0;
  }

  .kickoff-bar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 18px;
    background: rgb(var(--c-bg) / 0.5);
    flex-wrap: wrap;
    font-size: 0.6875rem;
  }

  .kickoff-label {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-weight: 500;
    color: rgb(var(--c-text-muted));
  }
</style>
