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
  import { invoke } from "../ipc/invoke";
  import {
    calculateSubtaskRollup,
    decomposeTask,
    formatSubtaskProgress,
  } from "../workbench/intake";

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

  function formatError(err: unknown, fallback: string): string {
    if (err instanceof Error && err.message) return err.message;
    if (typeof err === "string" && err.trim()) return err;
    if (err && typeof err === "object" && "message" in err && typeof (err as { message: unknown }).message === "string") {
      return (err as { message: string }).message;
    }
    return fallback;
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
      error = formatError(err, "Failed to load inbox threads");
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
      error = formatError(err, "Failed to submit answer");
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
      error = formatError(err, "Failed to withdraw ask");
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
          error = "Decision asks require at least two options (one per line)";
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
      error = formatError(err, "Failed to post structured ask");
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
      error = formatError(err, "Failed to decompose task");
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
        error = `Withdrew ${res.withdrawnKeys.length}; errors: ${res.errors.join("; ")}`;
      } else {
        notice = `Withdrew ${res.withdrawnKeys.length} open ${res.withdrawnKeys.length === 1 ? "ask" : "asks"}${res.skippedCount > 0 ? ` (${res.skippedCount} already settled skipped)` : ""}`;
      }
    } catch (err: unknown) {
      error = formatError(err, "Batch withdraw failed");
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
        error = `Resolved ${res.resolvedKeys.length}; errors: ${res.errors.join("; ")}`;
      } else {
        notice = `Posted resolution note across ${res.resolvedKeys.length} ${res.resolvedKeys.length === 1 ? "thread" : "threads"}`;
      }
    } catch (err: unknown) {
      error = formatError(err, "Batch resolve note failed");
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
      error = formatError(err, "Failed to kick off agent session");
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
      error = formatError(err, "Failed to apply mobile subtasks");
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
        <span class="font-semibold text-sm">Agent Inbox</span>
      {/if}
      {#if openAsksCount > 0}
        <span class="badge-open-asks" data-testid="open-asks-badge">
          {openAsksCount} open {openAsksCount === 1 ? "ask" : "asks"}
        </span>
      {/if}
    </div>

    <div class="toolbar-actions">
      <!-- Search -->
      <div class="search-box">
        <Search class="w-3.5 h-3.5 text-muted-foreground" />
        <input
          type="search"
          placeholder="Search threads..."
          bind:value={searchQuery}
          aria-label="Search threads"
        />
      </div>

      <!-- Status Filter Tabs -->
      <div class="filter-tabs" role="tablist" aria-label="Status filters">
        <button
          type="button"
          class="filter-tab"
          class:active={statusFilter === "all"}
          onclick={() => (statusFilter = "all")}
          role="tab"
          aria-selected={statusFilter === "all"}
        >
          All
        </button>
        <button
          type="button"
          class="filter-tab"
          class:active={statusFilter === "open"}
          onclick={() => (statusFilter = "open")}
          role="tab"
          aria-selected={statusFilter === "open"}
        >
          Open Asks
        </button>
        <button
          type="button"
          class="filter-tab"
          class:active={statusFilter === "answered"}
          onclick={() => (statusFilter = "answered")}
          role="tab"
          aria-selected={statusFilter === "answered"}
        >
          Answered
        </button>
        <button
          type="button"
          class="filter-tab"
          class:active={statusFilter === "withdrawn"}
          onclick={() => (statusFilter = "withdrawn")}
          role="tab"
          aria-selected={statusFilter === "withdrawn"}
        >
          Withdrawn
        </button>
      </div>

      <button
        type="button"
        class="action-btn"
        class:active={activeDrawer === "ask"}
        onclick={() => (activeDrawer = activeDrawer === "ask" ? "none" : "ask")}
        data-testid="new-ask-btn"
        title="Post a structured Vespin ask or handoff to an agent"
      >
        <Plus class="w-3.5 h-3.5" /> New Ask
      </button>

      <button
        type="button"
        class="action-btn"
        class:active={activeDrawer === "decompose"}
        onclick={() => (activeDrawer = activeDrawer === "decompose" ? "none" : "decompose")}
        data-testid="decompose-task-btn"
        title="Break down work into Linear-style subtasks and hand off to an agent"
      >
        <Layers class="w-3.5 h-3.5" /> Break Down Task
      </button>

      <button
        type="button"
        class="refresh-btn"
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
    <div class="lanes-strip" data-testid="active-lanes-strip" aria-label="Active agent lanes">
      <span class="lanes-label">Active Lanes ({lanes.length}):</span>
      {#each lanes as lane (lane.agent_id)}
        <span class="lane-pill" title={lane.worktree_path || lane.branch}>
          <Bot class="w-3 h-3 text-accent" />
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
        <span class="font-semibold text-xs">Post Structured Ask / Agent Handoff</span>
        <button type="button" class="close-alert" onclick={() => (activeDrawer = "none")} aria-label="Close new ask form">
          <X class="w-3.5 h-3.5" />
        </button>
      </div>
      <div class="drawer-grid">
        <select bind:value={newAskType} class="drawer-input" aria-label="Ask type">
          <option value="decide">Decide (Options + Recommendation)</option>
          <option value="do">Do (Action Checklist)</option>
          <option value="answer">Answer (Direct Question)</option>
          <option value="review">Review (Branch / Target)</option>
        </select>
        <input
          type="text"
          placeholder="Target agent (optional, e.g. claude-worker-1)"
          bind:value={newAskToAgent}
          class="drawer-input"
        />
        <input
          type="text"
          placeholder="Branch or review target (optional)"
          bind:value={newAskBranch}
          class="drawer-input"
        />
      </div>
      <input
        type="text"
        placeholder="Question or ask title..."
        bind:value={newAskTitle}
        class="drawer-input mt-2"
      />
      {#if newAskType === "decide"}
        <textarea
          rows="2"
          placeholder="Options (one per line, minimum 2)..."
          bind:value={newAskOptionsText}
          class="drawer-input mt-2"
        ></textarea>
        <input
          type="text"
          placeholder="Recommended option (optional)..."
          bind:value={newAskRecommendation}
          class="drawer-input mt-2"
        />
      {:else if newAskType === "do"}
        <textarea
          rows="3"
          placeholder="Checklist steps (one per line)..."
          bind:value={newAskStepsText}
          class="drawer-input mt-2"
        ></textarea>
      {/if}
      <input
        type="text"
        placeholder="Summary / context notes (optional)..."
        bind:value={newAskSummary}
        class="drawer-input mt-2"
      />
      <div class="drawer-actions">
        <button
          type="button"
          class="submit-ask-btn"
          disabled={busy}
          onclick={handlePostStructuredAsk}
          data-testid="submit-new-ask-btn"
        >
          <Send class="w-3.5 h-3.5" /> Post Ask
        </button>
      </div>
    </section>
  {:else if activeDrawer === "decompose"}
    <section class="composer-drawer" data-testid="decompose-task-drawer" aria-label="Break down task">
      <div class="drawer-header">
        <span class="font-semibold text-xs">Linear-Style Task Breakdown & Agent Kickoff</span>
        <button type="button" class="close-alert" onclick={() => (activeDrawer = "none")} aria-label="Close task breakdown form">
          <X class="w-3.5 h-3.5" />
        </button>
      </div>
      <div class="drawer-grid">
        <input
          type="text"
          placeholder="Parent task title..."
          bind:value={breakdownTitle}
          class="drawer-input"
        />
        <input
          type="text"
          placeholder="Assign agent (optional, e.g. agy-worker)"
          bind:value={breakdownAssignee}
          class="drawer-input"
        />
        <input
          type="text"
          placeholder="Branch (optional)"
          bind:value={breakdownBranch}
          class="drawer-input"
        />
      </div>
      <input
        type="text"
        placeholder="Parent description / acceptance context (optional)..."
        bind:value={breakdownDescription}
        class="drawer-input mt-2"
      />
      <textarea
        rows="3"
        placeholder="Subtasks (one per line)..."
        bind:value={breakdownSubtasksText}
        class="drawer-input mt-2"
      ></textarea>
      {#if previewPlan.subtasks.length > 0}
        <div class="plan-preview">
          <span class="text-xs text-muted-foreground">
            Will file parent + {previewPlan.subtasks.length} linked {previewPlan.subtasks.length === 1 ? "subtask" : "subtasks"} ({previewPlan.subtasks.map((s) => s.key).join(", ")})
          </span>
        </div>
      {/if}
      <div class="drawer-actions">
        <button
          type="button"
          class="submit-ask-btn"
          disabled={busy || !breakdownTitle.trim() || previewPlan.subtasks.length === 0}
          onclick={handleDecomposeTask}
          data-testid="submit-decompose-btn"
        >
          <Layers class="w-3.5 h-3.5" /> File Subtasks & Post Handoff
        </button>
      </div>
    </section>
  {/if}

  {#if error}
    <div class="inbox-alert error" role="alert">
      <AlertCircle class="w-4 h-4 shrink-0" />
      <span>{error}</span>
      <button type="button" class="close-alert" onclick={() => (error = "")} aria-label="Dismiss error">
        <X class="w-3.5 h-3.5" />
      </button>
    </div>
  {/if}

  {#if notice}
    <div class="inbox-alert notice" role="status">
      <CheckCircle2 class="w-4 h-4 shrink-0" />
      <span>{notice}</span>
      <button type="button" class="close-alert" onclick={() => (notice = "")} aria-label="Dismiss notice">
        <X class="w-3.5 h-3.5" />
      </button>
    </div>
  {/if}

  {#if selectedKeys.size > 0}
    <div class="batch-toolbar" data-testid="inbox-batch-toolbar" role="region" aria-label="Batch thread actions">
      <div class="batch-meta">
        <span class="font-medium">{selectedKeys.size} selected</span>
        <button
          type="button"
          class="action-btn"
          onclick={handleSelectVisible}
          disabled={busy || filteredThreads.length === 0}
          data-testid="batch-select-visible-btn"
        >
          Select visible ({filteredThreads.length})
        </button>
        <button
          type="button"
          class="action-btn"
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
          class="withdraw-trigger-btn"
          onclick={() => void handleBatchWithdraw()}
          disabled={busy || selectedOpenCount === 0}
          data-testid="batch-withdraw-btn"
          title="Withdraw all open asks among selected threads"
        >
          <XCircle class="w-3.5 h-3.5" /> Batch withdraw open asks ({selectedOpenCount})
        </button>
        <input
          type="text"
          class="batch-note-input"
          placeholder="Concise resolution note..."
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
          class="action-btn"
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
            class="action-btn"
            onclick={handleSelectVisible}
            disabled={busy}
          >
            Select visible ({filteredThreads.length})
          </button>
        </div>
      {/if}
      {#if filteredThreads.length === 0}
        <div class="empty-threads" data-testid="empty-threads">
          <MessageSquare class="w-8 h-8 opacity-40 mb-2" />
          <p class="font-medium">No threads found</p>
          <span class="text-xs text-muted-foreground">
            {statusFilter === "all" ? "No agent asks or messages recorded" : `No threads matching "${statusFilter}"`}
          </span>
        </div>
      {:else}
        <ul class="threads-list" role="list">
          {#each filteredThreads as thread (thread.thread_key)}
            {@const isSelected = selectedThread?.thread_key === thread.thread_key}
            {@const ask = thread.ask_type}
            <li class="thread-row">
              <input
                type="checkbox"
                class="thread-checkbox"
                checked={selectedKeys.has(thread.thread_key)}
                onchange={() => handleToggleThreadSelect(thread.thread_key)}
                aria-label="Select thread {thread.title}"
                data-testid="thread-select-{thread.thread_key}"
              />
              <button
                type="button"
                class="thread-card"
                class:selected={isSelected}
                class:status-open={thread.status === "open"}
                class:status-answered={thread.status === "answered"}
                onclick={() => selectThread(thread.thread_key)}
                data-testid="thread-card-{thread.thread_key}"
              >
                <div class="thread-card-header">
                  <div class="thread-meta">
                    {#if ask === "decide"}
                      <span class="ask-badge decide" title="Decision ask">
                        <Vote class="w-3 h-3" /> Decide
                      </span>
                    {:else if ask === "do"}
                      <span class="ask-badge do" title="Checklist ask">
                        <ListTodo class="w-3 h-3" /> Do
                      </span>
                    {:else if ask === "answer"}
                      <span class="ask-badge answer" title="Question ask">
                        <HelpCircle class="w-3 h-3" /> Answer
                      </span>
                    {:else if ask === "review"}
                      <span class="ask-badge review" title="Review ask">
                        <FileCode class="w-3 h-3" /> Review
                      </span>
                    {/if}
                    <span class="agent-chip" title="Sender agent">
                      <Bot class="w-2.5 h-2.5" /> {thread.from_agent}
                    </span>
                    {#if isMobileOriginated(thread.from_agent)}
                      <span class="ask-badge decide" data-testid="mobile-origin-badge" title="Sent from Graft Mobile on phone">
                        Phone · Graft Mobile
                      </span>
                    {/if}
                  </div>

                  <span class="status-pill {thread.status}">
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
        <div class="no-selection">
          <MessageSquare class="w-10 h-10 opacity-30 mb-2" />
          <p>Select a thread to view details and respond</p>
        </div>
      {:else}
        <!-- Selected Thread Header -->
        <header class="detail-header">
          <div class="detail-header-main">
            <h3 class="detail-title">{selectedThread.title}</h3>
            <div class="detail-tags">
              <span class="detail-key font-mono">{selectedThread.thread_key}</span>
              <span class="status-pill {selectedThread.status}">{selectedThread.status}</span>
              {#if isMobileOriginated(selectedThread.from_agent)}
                <span class="ask-badge decide" data-testid="detail-mobile-origin-badge">
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
                class="submit-ask-btn"
                onclick={() => currentMobileKickoff && void handleLaunchMobileKickoff(currentMobileKickoff)}
                disabled={busy}
                data-testid="mobile-launch-session-btn"
                title="Launch mobile-requested {currentMobileKickoff.provider} ({currentMobileKickoff.mode}) session"
              >
                <Bot class="w-3.5 h-3.5" /> Launch Session
              </button>
            {/if}
            {#if currentMobileBreakdown}
              <button
                type="button"
                class="action-btn"
                onclick={() => currentMobileBreakdown && void handleApplyMobileBreakdown(currentMobileBreakdown)}
                disabled={busy}
                data-testid="mobile-apply-subtasks-btn"
                title="Apply {currentMobileBreakdown.steps.length} subtasks from Graft Mobile"
              >
                <Layers class="w-3.5 h-3.5" /> Apply Subtasks
              </button>
            {/if}
            {#if linkedTaskId && onopenTask}
              <button
                type="button"
                class="action-btn"
                onclick={() => linkedTaskId && onopenTask(linkedTaskId)}
                data-testid="open-task-btn"
                title="Open linked board task"
              >
                <Layers class="w-3.5 h-3.5" /> Open Task
              </button>
            {/if}
            {#if selectedThread.status === "open" && !isWithdrawing}
              <button
                type="button"
                class="withdraw-trigger-btn"
                onclick={() => (isWithdrawing = true)}
                title="Withdraw this ask"
              >
                <XCircle class="w-3.5 h-3.5" /> Withdraw Ask
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
            class="kickoff-select"
            aria-label="Kickoff agent provider"
            bind:value={kickoffProvider}
            disabled={busy}
          >
            {#each KICKOFF_PROVIDERS as provider (provider.id)}
              <option value={provider.id}>{provider.label}</option>
            {/each}
          </select>
          <select
            class="kickoff-select"
            aria-label="Kickoff session mode"
            bind:value={kickoffMode}
            disabled={busy}
          >
            <option value="managed">Managed (warm)</option>
            <option value="terminal">Terminal</option>
          </select>
          <button
            type="button"
            class="submit-ask-btn"
            onclick={() => void handleKickOffAgent()}
            disabled={busy}
            data-testid="kickoff-agent-btn"
          >
            <Bot class="w-3.5 h-3.5" /> Kick Off Agent
          </button>
        </div>

        <!-- Withdraw Modal/Banner -->
        {#if isWithdrawing}
          <div class="withdraw-banner" role="region" aria-label="Withdraw ask">
            <div class="withdraw-header">
              <span class="font-medium">Withdraw Ask</span>
              <button
                type="button"
                class="btn-icon"
                onclick={() => (isWithdrawing = false)}
                aria-label="Cancel withdraw"
              >
                <X class="w-3.5 h-3.5" />
              </button>
            </div>
            <p class="text-xs text-muted-foreground mb-2">
              Explain why this ask is being withdrawn (e.g. resolved independently or obsolete).
            </p>
            <div class="withdraw-inputs">
              <input
                type="text"
                placeholder="Reason for withdrawal..."
                bind:value={withdrawReason}
                class="withdraw-input"
              />
              <button
                type="button"
                class="withdraw-confirm-btn"
                onclick={handleWithdraw}
                disabled={busy}
              >
                Confirm Withdrawal
              </button>
            </div>
          </div>
        {/if}

        <!-- Interactive Structured Ask Widget (when ask is Open) -->
        {#if selectedThread.status === "open" && currentAsk}
          <section class="structured-ask-box" data-testid="structured-ask-box">
            <div class="ask-header">
              <div class="ask-type-label {currentAsk.ask_type}">
                {#if currentAsk.ask_type === "decide"}
                  <Vote class="w-4 h-4" /> Structured Decision Request
                {:else if currentAsk.ask_type === "do"}
                  <ListTodo class="w-4 h-4" /> Structured Action Checklist
                {:else if currentAsk.ask_type === "answer"}
                  <HelpCircle class="w-4 h-4" /> Information Request
                {:else if currentAsk.ask_type === "review"}
                  <FileCode class="w-4 h-4" /> Code Review Request
                {/if}
              </div>
              <span class="text-xs text-muted-foreground">Requires developer response</span>
            </div>

            <!-- Question/Prompt -->
            <p class="ask-prompt">
              {currentAsk.question || currentAsk.prompt || currentAsk.title || selectedThread.title}
            </p>

            {#if currentAsk.summary && currentAsk.ask_type !== "review"}
              <p class="text-xs text-muted-foreground mb-3">{currentAsk.summary}</p>
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
                    class="option-card"
                    class:selected={selectedDecision === option}
                    onclick={() => (selectedDecision = option)}
                    data-testid="decide-option-{option}"
                  >
                    <div class="option-check">
                      {#if selectedDecision === option}
                        <Check class="w-3.5 h-3.5 text-accent" />
                      {/if}
                    </div>
                    <span class="option-text">{option}</span>
                    {#if isRec}
                      <span class="rec-badge" title="Agent recommended">Recommended</span>
                    {/if}
                  </button>
                {/each}
              </div>

              <div class="ask-actions">
                <button
                  type="button"
                  class="submit-ask-btn"
                  disabled={!selectedDecision || busy}
                  onclick={() => selectedDecision && handleAnswerDecision(selectedDecision)}
                  data-testid="confirm-decision-btn"
                >
                  <CheckCircle2 class="w-3.5 h-3.5" /> Confirm Decision
                </button>
              </div>

            <!-- Case 2: DO -->
            {:else if currentAsk.ask_type === "do" && currentDoSteps}
              {#if currentStepRollup}
                <div class="rollup-summary" data-testid="subtask-rollup">
                  <span class="text-xs text-muted-foreground">
                    {formatSubtaskProgress(currentStepRollup)}
                  </span>
                </div>
              {/if}
              <div class="do-checklist">
                {#each currentDoSteps as step, idx}
                  <label class="checklist-item">
                    <input
                      type="checkbox"
                      bind:checked={completedSteps[idx]}
                      data-testid="do-step-{idx}"
                    />
                    <span class="step-text" class:checked={completedSteps[idx]}>{step}</span>
                  </label>
                {/each}
              </div>

              {#if currentAsk.notes}
                <p class="text-xs text-muted-foreground italic mt-2">{currentAsk.notes}</p>
              {/if}

              <div class="ask-actions">
                <button
                  type="button"
                  class="submit-ask-btn"
                  disabled={busy}
                  onclick={handleCompleteSteps}
                  data-testid="complete-steps-btn"
                >
                  <CheckCircle2 class="w-3.5 h-3.5" /> Mark Steps Done
                </button>
              </div>

            <!-- Case 3: ANSWER -->
            {:else if currentAsk.ask_type === "answer"}
              <div class="answer-box">
                <textarea
                  rows="3"
                  placeholder="Type your answer for the agent..."
                  bind:value={textAnswer}
                  class="ask-textarea"
                  data-testid="answer-input"
                ></textarea>
                <div class="ask-actions">
                  <button
                    type="button"
                    class="submit-ask-btn"
                    disabled={!textAnswer.trim() || busy}
                    onclick={handleSubmitAnswer}
                    data-testid="submit-answer-btn"
                  >
                    <Send class="w-3.5 h-3.5" /> Submit Answer
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
                    placeholder="Optional review notes or requested changes..."
                    bind:value={reviewNotes}
                    class="review-input"
                  />
                  <div class="ask-actions split">
                    <button
                      type="button"
                      class="approve-btn"
                      disabled={busy}
                      onclick={() => handleSubmitReview(true)}
                      data-testid="approve-review-btn"
                    >
                      <Check class="w-3.5 h-3.5" /> Approve
                    </button>
                    <button
                      type="button"
                      class="changes-btn"
                      disabled={busy}
                      onclick={() => handleSubmitReview(false)}
                      data-testid="reject-review-btn"
                    >
                      <X class="w-3.5 h-3.5" /> Request Changes
                    </button>
                  </div>
                </div>
              </div>
            {/if}
          </section>
        {/if}

        <!-- Status Callout when Answered or Withdrawn -->
        {#if selectedThread.status === "answered"}
          <div class="thread-status-banner answered">
            <CheckCircle2 class="w-4 h-4 text-emerald-500" />
            <span>This ask has been answered and resolved.</span>
          </div>
        {:else if selectedThread.status === "withdrawn"}
          <div class="thread-status-banner withdrawn">
            <XCircle class="w-4 h-4 text-muted-foreground" />
            <span>This ask was withdrawn.</span>
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
                      <Bot class="w-3 h-3 text-blue-400" />
                      <span class="font-medium">{msg.from_agent}</span>
                    {/if}
                    {#if isMobileOriginated(msg.from_agent)}
                      <span class="ask-badge decide" data-testid="timeline-mobile-origin-badge" title="Sent from Graft Mobile on phone">
                        Phone · Graft Mobile
                      </span>
                    {/if}
                  </div>

                  <span class="msg-kind {msg.kind}">{msg.kind}</span>
                  <span class="msg-time">{formatRelativeTime(msg.created_at_ms)}</span>
                </div>

                <div class="message-content">
                  {#if msg.kind === "ask" && parseStructuredAsk(msg.body)}
                    {@const a = parseStructuredAsk(msg.body)}
                    <div class="parsed-ask-snippet">
                      <span class="font-semibold text-xs">
                        {a?.ask_type?.toUpperCase()}: {a?.question || a?.prompt || a?.title}
                      </span>
                      {#if a?.options}
                        <span class="text-xs text-muted-foreground">Options: {a.options.join(", ")}</span>
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
              placeholder="Send message to thread..."
              bind:value={replyText}
              class="composer-textarea"
              onkeydown={(e) => {
                if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
                  e.preventDefault();
                  handleSendCustomReply();
                }
              }}
            ></textarea>
            <button
              type="button"
              class="send-btn"
              disabled={!replyText.trim() || busy}
              onclick={handleSendCustomReply}
              title="Send reply (Cmd+Enter)"
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
  .agent-inbox {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 480px;
    background: rgb(var(--c-surface, 18 20 28));
    color: rgb(var(--c-text, 230 235 245));
    font-size: 13px;
    border-radius: 8px;
    overflow: hidden;
  }

  /* Embedded in the Agents pane it is part of that pane, not a panel laid on
     it: the pane's own background shows through, the outer box goes, and the
     toolbar and thread list take the pane's divider tone rather than an
     opaque surface of their own. That opaque surface read as a darker slab
     with a different contrast from the rest of the app. */
  .agent-inbox.embedded {
    background: transparent;
    border-radius: 0;
    min-height: 0;
  }

  .agent-inbox.embedded .inbox-toolbar {
    background: transparent;
    border-bottom-color: rgb(var(--c-border) / 0.65);
    padding: 8px 14px;
  }

  .agent-inbox.embedded .threads-pane {
    background: transparent;
    border-right-color: rgb(var(--c-border) / 0.65);
  }

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
    border-bottom: 1px solid rgb(var(--c-border, 45 50 65));
    background: rgb(var(--c-surface-hover, 24 28 40));
    flex-wrap: wrap;
  }

  .toolbar-title {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .badge-open-asks {
    background: rgba(245, 158, 11, 0.18);
    color: #f59e0b;
    border: 1px solid rgba(245, 158, 11, 0.35);
    padding: 2px 8px;
    border-radius: 12px;
    font-size: 11px;
    font-weight: 600;
  }

  .toolbar-actions {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }

  /* The app's pill button (gp-btn): the inbox used to draw its own 6px boxes
     on dark wells, which read as a different app inside this one. */
  .action-btn {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 4px 11px;
    background: rgb(var(--c-surface) / 0.7);
    border: 1px solid rgb(var(--c-border) / 0.7);
    border-radius: 9999px;
    font-size: 11px;
    font-weight: 500;
    color: inherit;
    cursor: pointer;
    transition: color 150ms, background-color 150ms, border-color 150ms;
  }

  .action-btn:hover {
    border-color: rgb(var(--c-accent) / 0.5);
    background: rgb(var(--c-surface-hover) / 0.7);
  }

  .action-btn.active {
    border-color: rgb(var(--c-accent) / 0.5);
    background: rgb(var(--c-accent) / 0.12);
    color: rgb(var(--c-accent));
  }

  .lanes-strip {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 16px;
    border-bottom: 1px solid rgb(var(--c-border, 45 50 65));
    background: rgba(0, 0, 0, 0.14);
    overflow-x: auto;
    font-size: 11px;
  }

  .lanes-label {
    color: rgb(var(--c-text-muted, 140 145 160));
    font-weight: 600;
    white-space: nowrap;
  }

  .lane-pill {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px 8px;
    border-radius: 12px;
    background: rgba(79, 70, 229, 0.12);
    border: 1px solid rgba(79, 70, 229, 0.28);
    white-space: nowrap;
  }

  .composer-drawer {
    padding: 12px 16px;
    border-bottom: 1px solid rgb(var(--c-border, 45 50 65));
    background: rgb(var(--c-bg, 24 28 38));
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

  .drawer-input {
    width: 100%;
    padding: 6px 8px;
    background: rgba(0, 0, 0, 0.22);
    border: 1px solid rgb(var(--c-border, 45 50 65));
    border-radius: 6px;
    color: inherit;
    font-size: 12px;
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

  /* Search, status filters and refresh take the app's field, segmented and
     icon-button shapes (gp-field / gp-segmented / gp-icon-btn). */
  .search-box {
    display: flex;
    align-items: center;
    gap: 6px;
    background: rgb(var(--c-bg) / 0.5);
    border: 1px solid rgb(var(--c-border) / 0.7);
    border-radius: 9999px;
    padding: 4px 11px;
    color: rgb(var(--c-text-muted));
  }

  .search-box:focus-within {
    border-color: rgb(var(--c-accent) / 0.6);
  }

  .search-box input {
    background: transparent;
    border: none;
    outline: none;
    color: inherit;
    font-size: 12px;
    width: 140px;
  }

  .filter-tabs {
    display: flex;
    gap: 2px;
    background: rgb(var(--c-bg) / 0.7);
    border-radius: 9999px;
    padding: 2px;
    border: 1px solid rgb(var(--c-border) / 0.7);
  }

  .filter-tab {
    background: transparent;
    border: none;
    color: rgb(var(--c-text-muted));
    font-size: 11px;
    padding: 3px 10px;
    border-radius: 9999px;
    cursor: pointer;
    transition: color 150ms, background-color 150ms;
  }

  .filter-tab:hover {
    color: rgb(var(--c-text));
  }

  .filter-tab.active {
    background: rgb(var(--c-surface) / 0.9);
    color: rgb(var(--c-accent));
    font-weight: 600;
    box-shadow: 0 1px 3px rgb(0 0 0 / 0.25), inset 0 1px 0 rgb(255 255 255 / 0.05);
  }

  .refresh-btn {
    display: inline-flex;
    align-items: center;
    padding: 5px;
    background: transparent;
    border: 0;
    border-radius: 9999px;
    cursor: pointer;
    color: rgb(var(--c-text-muted));
  }

  .refresh-btn:hover:not(:disabled) {
    background: rgb(var(--c-surface-hover) / 0.7);
    color: rgb(var(--c-text));
  }

  /* Alerts */
  .inbox-alert {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 14px;
    font-size: 12px;
    border-bottom: 1px solid transparent;
  }

  .inbox-alert.error {
    background: rgba(239, 68, 68, 0.12);
    color: #f87171;
    border-color: rgba(239, 68, 68, 0.3);
  }

  .inbox-alert.notice {
    background: rgba(16, 185, 129, 0.12);
    color: #34d399;
    border-color: rgba(16, 185, 129, 0.3);
  }

  .close-alert {
    margin-left: auto;
    background: transparent;
    border: none;
    color: inherit;
    cursor: pointer;
  }

  /* Body Split */
  .inbox-body {
    display: flex;
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }

  /* Threads Pane */
  .threads-pane {
    width: 320px;
    flex-shrink: 0;
    border-right: 1px solid rgb(var(--c-border, 45 50 65));
    overflow-y: auto;
    background: rgba(0, 0, 0, 0.1);
  }

  .threads-list {
    list-style: none;
    margin: 0;
    padding: 6px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .empty-threads {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding: 40px 16px;
    text-align: center;
  }

  .thread-card {
    width: 100%;
    text-align: left;
    background: rgb(var(--c-bg, 24 28 38));
    border: 1px solid rgb(var(--c-border, 45 50 65));
    border-radius: 6px;
    padding: 10px 12px;
    cursor: pointer;
    display: flex;
    flex-direction: column;
    gap: 6px;
    color: inherit;
    transition: all 0.15s ease;
  }

  .thread-card:hover {
    border-color: rgb(var(--c-accent, 79 70 229));
  }

  .thread-card.selected {
    border-color: rgb(var(--c-accent, 79 70 229));
    background: rgba(79, 70, 229, 0.1);
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

  .ask-badge {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    padding: 2px 6px;
    border-radius: 4px;
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
  }

  .ask-badge.decide {
    background: rgba(168, 85, 247, 0.18);
    color: #c084fc;
    border: 1px solid rgba(168, 85, 247, 0.3);
  }

  .ask-badge.do {
    background: rgba(59, 130, 246, 0.18);
    color: #60a5fa;
    border: 1px solid rgba(59, 130, 246, 0.3);
  }

  .ask-badge.answer {
    background: rgba(245, 158, 11, 0.18);
    color: #fbbf24;
    border: 1px solid rgba(245, 158, 11, 0.3);
  }

  .ask-badge.review {
    background: rgba(236, 72, 153, 0.18);
    color: #f472b6;
    border: 1px solid rgba(236, 72, 153, 0.3);
  }

  .agent-chip {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-size: 11px;
    color: rgb(var(--c-text-muted, 140 145 160));
  }

  .status-pill {
    font-size: 10px;
    padding: 1px 6px;
    border-radius: 10px;
    font-weight: 600;
    text-transform: capitalize;
  }

  .status-pill.open {
    background: rgba(245, 158, 11, 0.2);
    color: #f59e0b;
  }

  .status-pill.answered {
    background: rgba(16, 185, 129, 0.2);
    color: #10b981;
  }

  .status-pill.withdrawn {
    background: rgba(156, 163, 175, 0.2);
    color: #9ca3af;
  }

  .thread-title {
    margin: 0;
    font-size: 12px;
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
    font-size: 10px;
    color: rgb(var(--c-text-muted, 140 145 160));
  }

  /* Detail Pane */
  .thread-detail-pane {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-width: 0;
    overflow-y: auto;
    background: rgb(var(--c-surface, 18 20 28));
  }

  .no-selection {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    height: 100%;
    color: rgb(var(--c-text-muted, 140 145 160));
  }

  .detail-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
    padding: 14px 18px;
    border-bottom: 1px solid rgb(var(--c-border, 45 50 65));
    background: rgba(0, 0, 0, 0.1);
  }

  .detail-title {
    margin: 0 0 6px 0;
    font-size: 15px;
    font-weight: 600;
  }

  .detail-tags {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    font-size: 11px;
  }

  .agent-route {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: rgb(var(--c-text-muted, 140 145 160));
  }

  .withdraw-trigger-btn {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 4px 8px;
    background: transparent;
    border: 1px solid rgba(239, 68, 68, 0.4);
    color: #f87171;
    border-radius: 6px;
    font-size: 11px;
    cursor: pointer;
  }

  .withdraw-banner {
    margin: 12px 18px;
    padding: 12px;
    background: rgba(239, 68, 68, 0.1);
    border: 1px solid rgba(239, 68, 68, 0.3);
    border-radius: 8px;
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

  .withdraw-input {
    flex: 1;
    padding: 5px 8px;
    background: rgba(0, 0, 0, 0.2);
    border: 1px solid rgba(239, 68, 68, 0.4);
    border-radius: 6px;
    color: inherit;
    font-size: 12px;
  }

  .withdraw-confirm-btn {
    padding: 5px 12px;
    background: #ef4444;
    color: #fff;
    border: none;
    border-radius: 6px;
    font-weight: 600;
    font-size: 12px;
    cursor: pointer;
  }

  /* Structured Ask Box */
  .structured-ask-box {
    margin: 14px 18px;
    padding: 14px;
    background: rgb(var(--c-bg, 24 28 38));
    border: 1px solid rgb(var(--c-accent, 79 70 229));
    border-radius: 8px;
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.15);
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
    font-size: 12px;
    color: rgb(var(--c-accent, 79 70 229));
  }

  .ask-prompt {
    font-size: 13px;
    font-weight: 500;
    margin: 0 0 12px 0;
  }

  .decide-options {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-bottom: 12px;
  }

  .option-card {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 12px;
    background: rgba(0, 0, 0, 0.15);
    border: 1px solid rgb(var(--c-border, 45 50 65));
    border-radius: 6px;
    cursor: pointer;
    color: inherit;
    text-align: left;
    transition: all 0.15s ease;
  }

  .option-card:hover {
    border-color: rgb(var(--c-accent, 79 70 229));
  }

  .option-card.selected {
    border-color: rgb(var(--c-accent, 79 70 229));
    background: rgba(79, 70, 229, 0.12);
  }

  .option-check {
    width: 16px;
    height: 16px;
    border: 1px solid rgb(var(--c-border, 45 50 65));
    border-radius: 4px;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .rec-badge {
    margin-left: auto;
    background: rgba(16, 185, 129, 0.15);
    color: #10b981;
    border: 1px solid rgba(16, 185, 129, 0.3);
    font-size: 10px;
    padding: 1px 6px;
    border-radius: 10px;
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
    cursor: pointer;
    font-size: 12px;
  }

  .step-text.checked {
    text-decoration: line-through;
    color: rgb(var(--c-text-muted, 140 145 160));
  }

  .ask-textarea {
    width: 100%;
    padding: 8px 10px;
    background: rgba(0, 0, 0, 0.2);
    border: 1px solid rgb(var(--c-border, 45 50 65));
    border-radius: 6px;
    color: inherit;
    font-size: 12px;
    resize: vertical;
    outline: none;
    margin-bottom: 10px;
  }

  .ask-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }

  .ask-actions.split {
    justify-content: flex-start;
  }

  .submit-ask-btn,
  .approve-btn,
  .changes-btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 6px 14px;
    border-radius: 6px;
    font-weight: 600;
    font-size: 12px;
    cursor: pointer;
    border: none;
    transition: all 0.15s ease;
  }

  .submit-ask-btn,
  .approve-btn {
    background: rgb(var(--c-accent, 79 70 229));
    color: #fff;
  }

  .changes-btn {
    background: rgba(239, 68, 68, 0.2);
    color: #f87171;
    border: 1px solid rgba(239, 68, 68, 0.3);
  }

  .review-diff {
    background: rgba(0, 0, 0, 0.3);
    padding: 8px;
    border-radius: 4px;
    max-height: 140px;
    overflow: auto;
    font-size: 11px;
    margin: 8px 0;
  }

  .review-input {
    width: 100%;
    padding: 6px 8px;
    background: rgba(0, 0, 0, 0.2);
    border: 1px solid rgb(var(--c-border, 45 50 65));
    border-radius: 6px;
    color: inherit;
    font-size: 12px;
    margin-bottom: 8px;
  }

  /* Status Banner */
  .thread-status-banner {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 12px 18px;
    padding: 10px 14px;
    border-radius: 6px;
    font-size: 12px;
  }

  .thread-status-banner.answered {
    background: rgba(16, 185, 129, 0.1);
    border: 1px solid rgba(16, 185, 129, 0.25);
    color: #34d399;
  }

  .thread-status-banner.withdrawn {
    background: rgba(156, 163, 175, 0.1);
    border: 1px solid rgba(156, 163, 175, 0.25);
    color: #9ca3af;
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
    font-size: 12px;
    font-weight: 600;
    color: rgb(var(--c-text-muted, 140 145 160));
  }

  .timeline-list {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .timeline-message {
    padding: 8px 12px;
    background: rgba(0, 0, 0, 0.15);
    border: 1px solid rgb(var(--c-border, 45 50 65));
    border-radius: 6px;
  }

  .timeline-message.developer {
    border-left: 3px solid rgb(var(--c-accent, 79 70 229));
  }

  .message-header {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 11px;
    margin-bottom: 4px;
  }

  .author-badge {
    display: flex;
    align-items: center;
    gap: 4px;
  }

  .msg-kind {
    font-size: 10px;
    padding: 1px 4px;
    border-radius: 4px;
    background: rgba(255, 255, 255, 0.08);
  }

  .msg-time {
    margin-left: auto;
    color: rgb(var(--c-text-muted, 140 145 160));
  }

  .message-text {
    margin: 0;
    font-size: 12px;
    white-space: pre-wrap;
    line-height: 1.4;
  }

  /* Reply Composer */
  .reply-composer {
    padding: 10px 18px;
    border-top: 1px solid rgb(var(--c-border, 45 50 65));
    background: rgba(0, 0, 0, 0.1);
  }

  .composer-inner {
    display: flex;
    align-items: flex-end;
    gap: 8px;
  }

  .composer-textarea {
    flex: 1;
    padding: 6px 10px;
    background: rgba(0, 0, 0, 0.2);
    border: 1px solid rgb(var(--c-border, 45 50 65));
    border-radius: 6px;
    color: inherit;
    font-size: 12px;
    resize: none;
    outline: none;
  }

  .send-btn {
    padding: 8px 12px;
    background: rgb(var(--c-accent, 79 70 229));
    color: #fff;
    border: none;
    border-radius: 6px;
    cursor: pointer;
  }

  .batch-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    padding: 8px 16px;
    border-bottom: 1px solid rgb(var(--c-border, 45 50 65));
    background: rgb(var(--c-surface-hover, 24 28 40));
    flex-wrap: wrap;
    font-size: 11px;
  }

  .batch-meta,
  .batch-actions {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }

  .batch-note-input,
  .kickoff-select {
    padding: 4px 8px;
    background: rgba(0, 0, 0, 0.2);
    border: 1px solid rgb(var(--c-border, 45 50 65));
    border-radius: 6px;
    color: inherit;
    font-size: 11px;
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
    margin-top: 12px;
    margin-left: 6px;
    flex-shrink: 0;
    cursor: pointer;
  }

  .kickoff-bar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 18px;
    border-bottom: 1px solid rgb(var(--c-border, 45 50 65));
    background: rgba(0, 0, 0, 0.12);
    flex-wrap: wrap;
    font-size: 11px;
  }

  .kickoff-label {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-weight: 500;
    color: rgb(var(--c-text-muted, 140 145 160));
  }
</style>

