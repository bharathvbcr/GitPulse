<script lang="ts">
  /**
   * A task's checklist, its links to other tasks, Linear-style child subtask
   * breakdown + multi-agent kickoff, and task-linked Inbox collaboration.
   *
   * Checklist and links are stored fields (dc-store schema 11). Child subtasks
   * are real board tasks linked to this task via `{ kind: "parent", item_id }`
   * so each subtask can be independently handed off to Claude, Antigravity, or
   * Codex in an isolated worktree or warm managed session.
   */
  import { untrack } from "svelte";
  import { Bot, Layers, Link2, MessageSquare, Plus, Send, X } from "@lucide/svelte";
  import type { AgentInboxThread } from "../agents/inboxTypes";
  import {
    KICKOFF_PROVIDERS,
    createChildSubtasksForParent,
    kickOffAgentForTask,
    loadChildSubtasks,
    loadLinkedTaskThreads,
    postTaskAgentAsk,
    replyToTaskThread,
    type KickoffMode,
    type KickoffProvider,
  } from "../agents/collaboration";
  import {
    LINK_KINDS,
    MAX_CHECKLIST,
    MAX_CHECKLIST_TEXT,
    MAX_TASK_LINKS,
    STATUS_LABELS,
    WorkbenchError,
    explainError,
    getTask,
    listTasks,
    type ChecklistEntry,
    type LinkKind,
    type Repository,
    type Task,
    type TaskCard,
    type TaskLink,
  } from "../workbench/client";
  import {
    calculateSubtaskRollup,
    formatSubtaskProgress,
    parseSubtaskLines,
    splitCriteriaIntoSubtasks,
  } from "../workbench/intake";
  import type { OpenTabRef } from "../workbench/openMembership";
  import { requestTaskOpen } from "../workbench/taskOpen";

  let {
    checklist = $bindable(),
    links = $bindable(),
    taskId = null,
    task = null,
    acceptanceCriteria = [],
    repositories = [],
    openTabs = [],
    repositoryPath = "",
    initialSubtasks,
    initialInboxThreads,
    disabled = false,
    onchange,
  }: {
    checklist: ChecklistEntry[];
    links: TaskLink[];
    /** The task being edited; null for one not saved yet, which has no id to exclude. */
    taskId?: string | null;
    task?: Task | null;
    acceptanceCriteria?: readonly string[];
    repositories?: Repository[];
    openTabs?: OpenTabRef[];
    repositoryPath?: string;
    initialSubtasks?: TaskCard[];
    initialInboxThreads?: AgentInboxThread[];
    disabled?: boolean;
    onchange: () => void;
  } = $props();

  /** How a link reads from this task. */
  const KIND_LABELS: Record<LinkKind, string> = {
    parent: "Subtask of",
    blocks: "Blocks",
    related: "Related to",
    duplicate_of: "Duplicate of",
  };

  let entry = $state("");
  let kind = $state<LinkKind>("related");
  let query = $state("");
  let results = $state<TaskCard[]>([]);
  let searching = $state(false);
  let error = $state("");
  /** Titles for linked ids; `null` once a lookup found the task deleted. */
  let titles = $state<Record<string, string | null>>({});

  // Linear-style subtask breakdown & kickoff state
  let childSubtasks = $state<TaskCard[]>(
    untrack(() => (initialSubtasks ? [...initialSubtasks] : []))
  );
  let breakdownText = $state("");
  let subtaskKickoffMode = $state<KickoffMode>("managed");
  let breakdownBusy = $state(false);
  let collabNotice = $state("");
  let collabError = $state("");

  // Task-linked Agent Inbox threads
  let linkedThreads = $state<AgentInboxThread[]>(
    untrack(() => (initialInboxThreads ? [...initialInboxThreads] : []))
  );
  let threadReplyText = $state<Record<string, string>>({});
  let askTargetAgent = $state<KickoffProvider>("claude");
  let askQuestionText = $state("");

  const done = $derived(checklist.filter((item) => item.done).length);
  const hasParent = $derived(links.some((link) => link.kind === "parent"));
  const parsedDraftSubtasks = $derived(parseSubtaskLines(breakdownText));
  const splitCandidates = $derived(
    splitCriteriaIntoSubtasks(
      acceptanceCriteria,
      childSubtasks.map((sub) => sub.title)
    )
  );
  const childRollup = $derived(
    calculateSubtaskRollup(childSubtasks.map((sub) => sub.status))
  );

  /** Ids already asked for, so a lookup in flight is not asked for again. Not state: nothing renders it. */
  const requested = new Set<string>();
  function name(id: string, title: string | null) {
    titles = { ...titles, [id]: title };
  }
  function resolve(id: string) {
    requested.add(id);
    void getTask(id).then(
      (found) => name(id, found.title),
      // A deleted target is named as one; any other failure leaves the id on screen.
      (cause) =>
        name(id, cause instanceof WorkbenchError && cause.code === "not_found" ? null : id)
    );
  }
  // Resolve titles for links whose target this sheet has not asked about yet.
  $effect(() => {
    for (const link of links) {
      if (!requested.has(link.item_id)) untrack(() => resolve(link.item_id));
    }
  });

  let loadedSubtasksKey = "";
  $effect(() => {
    if (initialSubtasks !== undefined) {
      childSubtasks = [...initialSubtasks];
      return;
    }
    const id = taskId ?? "";
    const primaryRepo = task?.primary_repository_id ?? "";
    const nextKey = `${id}\0${primaryRepo}`;
    if (!id || nextKey === loadedSubtasksKey) return;
    loadedSubtasksKey = nextKey;
    void refreshChildSubtasks(id, primaryRepo);
  });

  let loadedThreadsKey = "";
  $effect(() => {
    if (initialInboxThreads !== undefined) {
      linkedThreads = [...initialInboxThreads];
      return;
    }
    const id = taskId ?? "";
    const repo = repositoryPath.trim();
    const nextKey = `${id}\0${repo}`;
    if (!id || !repo || nextKey === loadedThreadsKey) return;
    loadedThreadsKey = nextKey;
    void refreshLinkedThreads(repo, id);
  });

  async function refreshChildSubtasks(id: string, primaryRepo: string) {
    try {
      childSubtasks = await loadChildSubtasks(id, primaryRepo);
    } catch {
      // Best-effort read; explicit mutations report their own errors.
    }
  }

  async function refreshLinkedThreads(repo: string, id: string) {
    try {
      linkedThreads = await loadLinkedTaskThreads(repo, id);
    } catch {
      // Best-effort read when coordination bus is empty or uninitialized.
    }
  }

  function parseAskSummary(body: string): string {
    try {
      const parsed = JSON.parse(body) as {
        question?: string;
        prompt?: string;
        title?: string;
      };
      if (parsed && typeof parsed === "object") {
        return parsed.question || parsed.prompt || parsed.title || body;
      }
    } catch {
      // Plain text message
    }
    return body;
  }

  function mergeUniqueSubtasks(existing: readonly TaskCard[], added: readonly TaskCard[]): TaskCard[] {
    const byId = new Map<string, TaskCard>();
    for (const item of existing) byId.set(item.id, item);
    for (const item of added) byId.set(item.id, item);
    return Array.from(byId.values());
  }

  async function handleCreateSubtasks() {
    if (!task || parsedDraftSubtasks.length === 0 || breakdownBusy) return;
    breakdownBusy = true;
    collabError = "";
    collabNotice = "";
    try {
      const created = await createChildSubtasksForParent({
        parentTask: task,
        subtaskTitles: parsedDraftSubtasks,
        repoPath: repositoryPath,
      });
      childSubtasks = mergeUniqueSubtasks(childSubtasks, created);
      breakdownText = "";
      collabNotice = `Created ${created.length} linked ${created.length === 1 ? "subtask" : "subtasks"}.`;
      if (repositoryPath.trim()) {
        await refreshLinkedThreads(repositoryPath.trim(), task.id);
      }
    } catch (cause) {
      collabError = explainError(cause);
    } finally {
      breakdownBusy = false;
    }
  }

  async function handleSplitCriteria() {
    if (!task || splitCandidates.length === 0 || breakdownBusy) return;
    breakdownBusy = true;
    collabError = "";
    collabNotice = "";
    try {
      const created = await createChildSubtasksForParent({
        parentTask: task,
        subtaskTitles: splitCandidates,
        repoPath: repositoryPath,
      });
      childSubtasks = mergeUniqueSubtasks(childSubtasks, created);
      collabNotice = `Split ${created.length} acceptance ${created.length === 1 ? "criterion" : "criteria"} into subtasks.`;
      if (repositoryPath.trim()) {
        await refreshLinkedThreads(repositoryPath.trim(), task.id);
      }
    } catch (cause) {
      collabError = explainError(cause);
    } finally {
      breakdownBusy = false;
    }
  }

  async function handleKickOffSubtask(subtaskId: string, provider: KickoffProvider) {
    if (breakdownBusy) return;
    breakdownBusy = true;
    collabError = "";
    collabNotice = "";
    try {
      const { run } = await kickOffAgentForTask({
        taskId: subtaskId,
        provider,
        mode: subtaskKickoffMode,
        repoPath: repositoryPath || undefined,
        repositories,
        openTabs,
        worktree: true,
      });
      collabNotice = `Started ${provider} (${subtaskKickoffMode === "managed" ? "warm managed" : "worktree terminal"}) run ${run.id}.`;
    } catch (cause) {
      collabError = explainError(cause);
    } finally {
      breakdownBusy = false;
    }
  }

  async function handleReplyThread(thread: AgentInboxThread) {
    const body = (threadReplyText[thread.thread_key] ?? "").trim();
    if (!body || !repositoryPath.trim() || !taskId || breakdownBusy) return;
    breakdownBusy = true;
    collabError = "";
    collabNotice = "";
    try {
      await replyToTaskThread({
        repoPath: repositoryPath.trim(),
        threadKey: thread.thread_key,
        toAgent: thread.from_agent,
        body,
      });
      threadReplyText = { ...threadReplyText, [thread.thread_key]: "" };
      await refreshLinkedThreads(repositoryPath.trim(), taskId);
      collabNotice = `Replied on ${thread.thread_key}.`;
    } catch (cause) {
      collabError = explainError(cause);
    } finally {
      breakdownBusy = false;
    }
  }

  async function handlePostTaskAsk() {
    const question = askQuestionText.trim();
    if (!question || !repositoryPath.trim() || !task || breakdownBusy) return;
    breakdownBusy = true;
    collabError = "";
    collabNotice = "";
    try {
      await postTaskAgentAsk({
        repoPath: repositoryPath.trim(),
        taskId: task.id,
        taskTitle: task.title,
        toAgent: askTargetAgent,
        question,
      });
      askQuestionText = "";
      await refreshLinkedThreads(repositoryPath.trim(), task.id);
      collabNotice = `Posted handoff ask to ${askTargetAgent}.`;
    } catch (cause) {
      collabError = explainError(cause);
    } finally {
      breakdownBusy = false;
    }
  }

  function addEntry() {
    const text = entry.trim();
    if (
      !text ||
      checklist.length >= MAX_CHECKLIST ||
      new TextEncoder().encode(text).length > MAX_CHECKLIST_TEXT
    ) {
      return;
    }
    checklist = [...checklist, { text, done: false }];
    entry = "";
    onchange();
  }
  function toggle(index: number, value: boolean) {
    checklist = checklist.map((item, at) => (at === index ? { ...item, done: value } : item));
    onchange();
  }
  function removeEntry(index: number) {
    checklist = checklist.filter((_, at) => at !== index);
    onchange();
  }

  async function search() {
    const text = query.trim();
    if (!text) {
      results = [];
      return;
    }
    searching = true;
    error = "";
    try {
      const page = await listTasks({ kind: "global" }, null, text, undefined, 8);
      results = page.items.filter((card) => card.id !== taskId);
    } catch (cause) {
      error = explainError(cause);
    } finally {
      searching = false;
    }
  }
  function link(card: TaskCard) {
    if (links.length >= MAX_TASK_LINKS) {
      error = `A task holds at most ${MAX_TASK_LINKS} links.`;
      return;
    }
    if (kind === "parent" && hasParent) {
      error = "A task has one parent. Remove the current one first.";
      return;
    }
    if (links.some((existing) => existing.kind === kind && existing.item_id === card.id)) return;
    requested.add(card.id);
    name(card.id, card.title);
    links = [...links, { kind, item_id: card.id }];
    results = [];
    query = "";
    error = "";
    onchange();
  }
  function unlink(index: number) {
    links = links.filter((_, at) => at !== index);
    onchange();
  }
</script>

<section class="relations" aria-label="Checklist and linked tasks" data-testid="task-relations">
  <div class="block">
    <div class="head">
      <span>Checklist</span>
      {#if checklist.length}
        <span class="count" data-testid="task-checklist-count">{done} of {checklist.length} done</span>
      {/if}
    </div>
    {#if checklist.length}
      <ul data-testid="task-checklist">
        {#each checklist as item, index (index)}
          <li>
            <label>
              <input
                type="checkbox"
                checked={item.done}
                {disabled}
                onchange={(e) => toggle(index, e.currentTarget.checked)}
              />
              <span class:done={item.done}>{item.text}</span>
            </label>
            <button
              type="button"
              class="remove"
              {disabled}
              aria-label="Remove checklist item: {item.text}"
              onclick={() => removeEntry(index)}
            >
              <X size={12} />
            </button>
          </li>
        {/each}
      </ul>
    {/if}
    <div class="add">
      <input
        class="gp-field"
        data-testid="task-checklist-add"
        bind:value={entry}
        {disabled}
        maxlength={MAX_CHECKLIST_TEXT}
        placeholder="Add a checklist item…"
        onkeydown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            addEntry();
          }
        }}
      />
      <button
        type="button"
        class="gp-btn"
        onclick={addEntry}
        disabled={disabled || !entry.trim() || checklist.length >= MAX_CHECKLIST}
      >
        <Plus size={12} /> Add
      </button>
    </div>
  </div>

  <div class="block">
    <div class="head">
      <span>Linked tasks</span>
      {#if links.length}<span class="count">{links.length}</span>{/if}
    </div>
    {#if links.length}
      <ul data-testid="task-links">
        {#each links as entry_, index (`${entry_.kind}:${entry_.item_id}`)}
          <li>
            <span class="kind">{KIND_LABELS[entry_.kind]}</span>
            <span class="title" class:gone={titles[entry_.item_id] === null}>
              {titles[entry_.item_id] === null
                ? `${entry_.item_id} (deleted)`
                : titles[entry_.item_id] ?? entry_.item_id}
            </span>
            <button
              type="button"
              class="remove"
              {disabled}
              aria-label="Remove link to {titles[entry_.item_id] ?? entry_.item_id}"
              onclick={() => unlink(index)}
            >
              <X size={12} />
            </button>
          </li>
        {/each}
      </ul>
    {/if}
    <div class="add">
      <select class="gp-select" aria-label="Link kind" bind:value={kind} {disabled}>
        {#each LINK_KINDS as option (option)}
          <option value={option} disabled={option === "parent" && hasParent}>
            {KIND_LABELS[option]}
          </option>
        {/each}
      </select>
      <input
        class="gp-field"
        type="search"
        data-testid="task-link-search"
        bind:value={query}
        {disabled}
        maxlength="512"
        placeholder="Find a task by title…"
        onkeydown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            void search();
          }
        }}
      />
      <button
        type="button"
        class="gp-btn"
        disabled={disabled || searching || !query.trim()}
        onclick={() => void search()}
      >
        <Link2 size={12} /> Find
      </button>
    </div>
    {#if results.length}
      <ul class="results" data-testid="task-link-results">
        {#each results as card (card.id)}
          <li>
            <button type="button" class="pick" {disabled} onclick={() => link(card)}>
              {card.title}
            </button>
          </li>
        {/each}
      </ul>
    {/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
  </div>

  <!-- Subtask Breakdown & Direct Multi-Agent Fan-Out (isolated from parent draft dirty tracking) -->
  <div
    class="block collab-block"
    data-testid="task-breakdown-section"
    oninput={(e) => e.stopPropagation()}
    onchange={(e) => e.stopPropagation()}
  >
    <div class="head">
      <span class="head-with-icon"><Layers size={12} /> Break down into subtasks</span>
      {#if childSubtasks.length > 0}
        <span class="count" data-testid="task-subtasks-rollup">
          {formatSubtaskProgress(childRollup)}
        </span>
      {/if}
    </div>

    {#if !task}
      <p class="hint">Save this task first to split it into linked subtasks and fan out agents.</p>
    {:else}
      {#if childSubtasks.length > 0}
        <div class="kickoff-mode-row">
          <span class="kind">Subtask launch mode</span>
          <select
            class="gp-select compact-select"
            aria-label="Subtask kickoff mode"
            bind:value={subtaskKickoffMode}
            disabled={disabled || breakdownBusy}
          >
            <option value="managed">Managed (warm multi-turn)</option>
            <option value="terminal">Terminal worktree</option>
          </select>
        </div>
        <ul class="subtask-cards" data-testid="task-child-subtasks">
          {#each childSubtasks as sub (sub.id)}
            <li class="subtask-card">
              <div class="subtask-row-top">
                <span class="status-tag">{STATUS_LABELS[sub.status] ?? sub.status}</span>
                <button
                  type="button"
                  class="subtask-open"
                  onclick={() => requestTaskOpen(sub.id)}
                  title="Open subtask {sub.title}"
                >
                  {sub.title}
                </button>
              </div>
              <div class="kickoff-pills" role="group" aria-label="Kick off agent for {sub.title}">
                {#each KICKOFF_PROVIDERS as provider (provider.id)}
                  <button
                    type="button"
                    class="gp-btn kickoff-pill"
                    data-testid="subtask-kickoff-{provider.id}-{sub.id}"
                    disabled={disabled || breakdownBusy}
                    onclick={() => void handleKickOffSubtask(sub.id, provider.id)}
                    aria-label="Kick off {provider.label} on {sub.title}"
                  >
                    <Bot size={11} /> {provider.label}
                  </button>
                {/each}
              </div>
            </li>
          {/each}
        </ul>
      {/if}

      <textarea
        class="gp-field breakdown-area"
        rows="2"
        data-testid="task-breakdown-input"
        aria-label="Break down into subtasks"
        bind:value={breakdownText}
        disabled={disabled || breakdownBusy}
        placeholder="Paste or enter subtasks, one per line…"
      ></textarea>
      <div class="breakdown-actions">
        <button
          type="button"
          class="gp-btn"
          data-testid="task-breakdown-submit"
          disabled={disabled || breakdownBusy || parsedDraftSubtasks.length === 0}
          onclick={() => void handleCreateSubtasks()}
        >
          <Plus size={12} /> Create {parsedDraftSubtasks.length > 0 ? `${parsedDraftSubtasks.length} ` : ""}subtasks
        </button>
        <button
          type="button"
          class="gp-btn"
          data-testid="task-split-criteria-btn"
          disabled={disabled || breakdownBusy || splitCandidates.length === 0}
          onclick={() => void handleSplitCriteria()}
        >
          <Layers size={12} /> Split acceptance criteria into subtasks{splitCandidates.length > 0 ? ` (${splitCandidates.length})` : ""}
        </button>
      </div>
    {/if}
  </div>

  <!-- Linked Inbox Thread & Agent Collaboration -->
  {#if taskId}
    <div
      class="block collab-block"
      data-testid="task-linked-inbox"
      oninput={(e) => e.stopPropagation()}
      onchange={(e) => e.stopPropagation()}
    >
      <div class="head">
        <span class="head-with-icon"><MessageSquare size={12} /> Linked Inbox &amp; Agent Collaboration</span>
        {#if linkedThreads.length > 0}
          <span class="count">{linkedThreads.length} {linkedThreads.length === 1 ? "thread" : "threads"}</span>
        {/if}
      </div>

      {#if linkedThreads.length > 0}
        <ul class="linked-threads">
          {#each linkedThreads as thread (thread.thread_key)}
            <li class="linked-thread-item">
              <div class="subtask-row-top">
                <span class="status-tag">{thread.status}</span>
                <span class="kind">{thread.from_agent}</span>
                <span class="title">{parseAskSummary(thread.latest_msg.body)}</span>
              </div>
              {#if repositoryPath.trim()}
                <div class="add">
                  <input
                    class="gp-field"
                    aria-label="Reply to {thread.thread_key}"
                    value={threadReplyText[thread.thread_key] ?? ""}
                    oninput={(e) => {
                      threadReplyText = {
                        ...threadReplyText,
                        [thread.thread_key]: e.currentTarget.value,
                      };
                    }}
                    disabled={disabled || breakdownBusy}
                    placeholder="Quick reply to {thread.from_agent}…"
                    onkeydown={(e) => {
                      if (e.key === "Enter") {
                        e.preventDefault();
                        void handleReplyThread(thread);
                      }
                    }}
                  />
                  <button
                    type="button"
                    class="gp-btn"
                    disabled={disabled || breakdownBusy || !(threadReplyText[thread.thread_key] ?? "").trim()}
                    onclick={() => void handleReplyThread(thread)}
                  >
                    <Send size={11} /> Reply
                  </button>
                </div>
              {/if}
            </li>
          {/each}
        </ul>
      {:else}
        <p class="hint">No open inbox asks linked to this task yet.</p>
      {/if}

      {#if task && repositoryPath.trim()}
        <div class="add">
          <select
            class="gp-select compact-select"
            aria-label="Target agent"
            bind:value={askTargetAgent}
            disabled={disabled || breakdownBusy}
          >
            {#each KICKOFF_PROVIDERS as provider (provider.id)}
              <option value={provider.id}>{provider.label}</option>
            {/each}
          </select>
          <input
            class="gp-field"
            data-testid="task-inbox-ask-input"
            aria-label="Ask or handoff message"
            bind:value={askQuestionText}
            disabled={disabled || breakdownBusy}
            placeholder="Ask or coordinate with another agent…"
            onkeydown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                void handlePostTaskAsk();
              }
            }}
          />
          <button
            type="button"
            class="gp-btn"
            disabled={disabled || breakdownBusy || !askQuestionText.trim()}
            onclick={() => void handlePostTaskAsk()}
          >
            <Send size={11} /> Ask
          </button>
        </div>
      {/if}

      {#if collabNotice}<p class="hint" role="status">{collabNotice}</p>{/if}
      {#if collabError}<p class="error" role="alert">{collabError}</p>{/if}
    </div>
  {/if}
</section>

<style>
  .relations{display:flex;flex-direction:column;gap:12px;margin-bottom:13px}
  .block{display:flex;flex-direction:column;gap:6px}
  .collab-block{padding-top:8px;border-top:1px solid rgb(var(--c-border) / 0.45)}
  .head{display:flex;align-items:baseline;justify-content:space-between;font-weight:500}
  .head-with-icon{display:inline-flex;align-items:center;gap:5px}
  .count,.hint{font-weight:400;color:rgb(var(--c-text-muted));font-size:11px;margin:0}
  ul{list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:4px;max-height:180px;overflow-y:auto}
  li{display:flex;align-items:center;gap:8px;padding:4px 8px;border-radius:6px;border:1px solid rgb(var(--c-border) / 0.5);font-size:11px}
  .subtask-card,.linked-thread-item{flex-direction:column;align-items:stretch;gap:6px;padding:6px 8px}
  .subtask-row-top{display:flex;align-items:center;gap:6px;min-width:0}
  .status-tag{font-size:10px;padding:1px 6px;border-radius:999px;background:rgb(var(--c-surface-hover) / 0.8);color:rgb(var(--c-text-muted));flex-shrink:0}
  .subtask-open{background:transparent;border:0;cursor:pointer;color:inherit;text-align:left;font:inherit;flex:1;min-width:0;padding:0;overflow-wrap:anywhere}
  .subtask-open:hover{text-decoration:underline}
  .kickoff-mode-row,.kickoff-pills,.breakdown-actions{display:flex;align-items:center;gap:6px;flex-wrap:wrap}
  .compact-select{width:auto;font-size:11px;padding:4px 8px}
  .kickoff-pill{display:inline-flex;align-items:center;gap:4px;font-size:10px;padding:2px 7px}
  .breakdown-area{font-size:11px;padding:6px 8px}
  li label{display:flex;align-items:center;gap:6px;flex:1;min-width:0}
  li label span,.title{overflow-wrap:anywhere;flex:1;min-width:0}
  .done{text-decoration:line-through;color:rgb(var(--c-text-muted))}
  .gone{color:rgb(var(--c-text-muted));font-style:italic}
  .kind{color:rgb(var(--c-text-muted));flex-shrink:0}
  .remove{background:transparent;border:0;cursor:pointer;padding:2px;display:inline-flex;color:rgb(var(--c-text-muted));border-radius:4px}
  .remove:hover{color:rgb(var(--c-text))}
  .add{display:flex;gap:6px;align-items:center}
  .add input{flex:1;font-size:11px;padding:5px 8px}
  .add button,.add select{flex-shrink:0;font-size:11px;padding:5px 10px}
  .results .pick{background:transparent;border:0;cursor:pointer;color:inherit;text-align:left;width:100%;font:inherit}
  .error{color:#dc6565;margin:0;font-size:11px}
</style>
