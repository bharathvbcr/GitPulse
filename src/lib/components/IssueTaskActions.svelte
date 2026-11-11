<script lang="ts">
  import { onMount, untrack, type Snippet } from "svelte";
  import { AlertTriangle, Bug, CheckCircle2, ListPlus, LoaderCircle, RefreshCw } from "@lucide/svelte";
  import { bindForegroundChanges, readBackgroundDocument } from "../runtime/foreground";
  import { isTauri } from "../platform";
  import { createListenerTracker } from "../dom/listenerTracker";
  import { openExternal as openExternalUrl } from "../desktop/openExternal";
  import { describeFailure } from "../ui/copy/describeFailure";
  import { reportPanelError } from "../diagnostics/report";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import { askConfirm } from "../stores/modalStore";
  import { toastStore } from "../stores/toastStore";
  import { verdictLabel } from "../stores/harnessStore";
  import type { IssueInfo } from "../ops/model";
  import { registerRepository, subscribeWorkbenchChanges, type TaskCard } from "../workbench/client";
  import { createLiveLoader } from "../workbench/liveLoader";
  import {
    batchCreateTasksFromIssues,
    createTaskFromIssue,
    findTaskForIssue,
    listAllRepositoryTasks,
    TASK_CATALOG_UNREAD_MESSAGE,
  } from "../workbench/issueTask";

  /**
   * The issue-to-task half of the Remote pane's Open issues block: report an
   * issue, add one or every open issue to the task board, and say when the
   * existing task list could not be read.
   *
   * There is one issue list and it is the pane's own. This component takes that
   * list as `issues`, and hands the per-issue "Add to tasks" control back to
   * the caller as a snippet, so each row stays where the pane draws it and no
   * second list or second read of issues exists here.
   */
  let {
    issues,
    onReported,
    children,
  }: {
    /** Every open issue the pane fetched, not the filtered rows. */
    issues: IssueInfo[];
    /** An issue was filed; the pane re-reads GitHub so the new one shows up. */
    onReported: () => void;
    /** The issue block, given the per-issue control to draw in each row. */
    children?: Snippet<[Snippet<[IssueInfo]>]>;
  } = $props();

  /** "Couldn't <verb> <object>. <Reason>." for a notice. */
  function failure(verb: string, object: string, cause: unknown): string {
    const copy = describeFailure({ verb, object }, cause);
    return copy.detail ? `${copy.headline} ${copy.detail}` : copy.headline;
  }

  /** Feedback for the last action. `tasks` offers a way to the board it landed on. */
  let notice = $state<{ text: string; tone: "info" | "danger"; tasks: boolean } | null>(null);
  function say(text: string, options: { tone?: "info" | "danger"; tasks?: boolean } = {}) {
    notice = { text, tone: options.tone ?? "info", tasks: options.tasks ?? false };
  }

  let reportOpen = $state(false);
  let reporting = $state(false);
  let issueTitle = $state("");
  let issueBody = $state("");
  let issueLabels = $state("bug");

  /** The task catalog as read; `null` while loading or after a failed read, never "empty". */
  let repoTasks = $state<TaskCard[] | null>(null);
  let repoTasksError = $state<string | null>(null);
  let taskAddingNumber = $state<number | null>(null);
  let batchAdding = $state(false);

  const unimportedIssues = $derived(issues.filter((issue) => !findTaskForIssue(repoTasks, issue.number)));
  // Unknown, not zero: with no catalog every issue would look new.
  const unimportedCount = $derived(repoTasks === null ? 0 : unimportedIssues.length);
  const importPaused = $derived(repoTasks === null && issues.length > 0);
  const working = $derived(reporting || batchAdding || taskAddingNumber !== null);

  async function openExternal(url: string) {
    try {
      await openExternalUrl(url);
    } catch (err) {
      say(reportPanelError("github", err), { tone: "danger" });
    }
  }

  function openTasksView() {
    repoStore.setViewSection("work", "tasks");
  }

  async function reportIssue() {
    const title = issueTitle.trim();
    if (!title || working) return;
    const repo = $repoStore.currentPath;
    reporting = true;
    notice = null;
    try {
      const labels = issueLabels.split(",").map((label) => label.trim()).filter(Boolean);
      const outcome = await repoStore.reportIssue(title, issueBody, labels);
      if (!outcome.ok) {
        say(outcome.error ?? "Couldn't report the issue.", { tone: "danger" });
        return;
      }
      say(`Issue reported${outcome.policy ? ` — ${verdictLabel(outcome.policy)}` : ""}.`);
      issueTitle = "";
      issueBody = "";
      reportOpen = false;
      try {
        if (outcome.output) await openExternal(outcome.output);
      } catch {
        // The issue exists even if opening it fails; the URL is in the journal.
      }
      if ($repoStore.currentPath === repo) onReported();
    } catch (error) {
      say(failure("report", "the issue", error), { tone: "danger" });
    } finally {
      reporting = false;
    }
  }

  let repoTasksGeneration = 0;
  /** True once the `workbench-changed` listener is attached; tasks then reload on change, not with the issue list. */
  let tasksFollowChanges = false;
  /**
   * Change-driven task reloads through the shared live loader: one at a time,
   * spaced, and never while the window is behind.
   */
  const repoTaskLoader = createLiveLoader({
    canRun: () => !readBackgroundDocument(),
    read: async () => {
      const repo = $repoStore.currentPath;
      if (repo) await loadRepoTasks(repo);
    },
    onError: (cause) => { repoTasksError = failure("read", "the task list", cause); },
  });
  function scheduleRefreshRepoTasks() { repoTaskLoader.schedule(); }

  async function loadRepoTasks(repoPath: string = $repoStore.currentPath ?? "") {
    if (!repoPath) {
      repoTasks = null;
      repoTasksError = null;
      return;
    }
    const currentGen = ++repoTasksGeneration;
    try {
      const registered = await registerRepository(repoPath);
      if ($repoStore.currentPath !== repoPath || !registered?.id || currentGen !== repoTasksGeneration) return;
      const tasks = await listAllRepositoryTasks(registered.id);
      if ($repoStore.currentPath === repoPath && currentGen === repoTasksGeneration) {
        repoTasks = tasks;
        repoTasksError = null;
      }
    } catch (err) {
      // Unknown, not empty: importing against a catalog that was not read
      // would re-create every task that already exists.
      if ($repoStore.currentPath === repoPath && currentGen === repoTasksGeneration) {
        repoTasks = null;
        repoTasksError = failure("read", "the task list", err);
      }
    }
  }

  async function addIssueToTask(issue: IssueInfo) {
    const repoPath = $repoStore.currentPath;
    if (!repoPath) {
      say("No active repository found.", { tone: "danger" });
      return;
    }
    if (working) return;
    if (repoTasks === null) {
      say(TASK_CATALOG_UNREAD_MESSAGE, { tone: "danger" });
      return;
    }
    taskAddingNumber = issue.number;
    notice = null;
    try {
      const saved = await createTaskFromIssue(issue, repoPath);
      if ($repoStore.currentPath === repoPath) {
        repoTasks = [saved, ...(repoTasks ?? []).filter((t) => t.id !== saved.id)];
        toastStore.success(`Created task for issue #${issue.number}`);
        say(`Task [#${issue.number}] created. Available on the Tasks board for agent handoff.`, { tasks: true });
      }
    } catch (err) {
      if ($repoStore.currentPath === repoPath) say(failure("create", "the task", err), { tone: "danger" });
    } finally {
      taskAddingNumber = null;
    }
  }

  async function addAllIssuesToTasks() {
    const repoPath = $repoStore.currentPath;
    if (!repoPath || issues.length === 0 || working) return;
    if (repoTasks === null) {
      say(TASK_CATALOG_UNREAD_MESSAGE, { tone: "danger" });
      return;
    }
    if (unimportedIssues.length === 0) {
      say("All open issues are already in tasks.");
      return;
    }
    const pending = unimportedIssues;
    const count = pending.length;
    const confirmed = await askConfirm({
      title: "Add all issues to tasks?",
      message: `Create ${count} task${count === 1 ? "" : "s"} from open GitHub issues for coding agents to work on?`,
      confirmLabel: "Add tasks",
    });
    if (!confirmed || $repoStore.currentPath !== repoPath) return;

    batchAdding = true;
    notice = null;
    try {
      const result = await batchCreateTasksFromIssues(pending, repoPath, repoTasks);
      if ($repoStore.currentPath === repoPath) {
        repoTasks = [...result.created, ...(repoTasks ?? [])];
        if (result.created.length > 0) {
          toastStore.success(
            `Created ${result.created.length} task${result.created.length === 1 ? "" : "s"} from GitHub issues`,
          );
          say(`Added ${result.created.length} task${result.created.length === 1 ? "" : "s"} to Tasks board for agent handoff.`, { tasks: true });
        }
        if (result.errors.length > 0) {
          say(`Added ${result.created.length} tasks. Couldn't add ${result.errors.length}. ${result.errors[0].error}`, { tone: "danger", tasks: result.created.length > 0 });
        }
      }
    } catch (err) {
      if ($repoStore.currentPath === repoPath) say(failure("add", "issues to tasks", err), { tone: "danger" });
    } finally {
      batchAdding = false;
    }
  }

  onMount(() => {
    const listeners = createListenerTracker();
    if (isTauri()) {
      // Following changes is a promise to stop re-reading tasks with every issue
      // list, so it is made only while the subscription stands; a failed one
      // says so and the task list keeps following the issue list.
      tasksFollowChanges = true;
      listeners.track(subscribeWorkbenchChanges({
        changed: scheduleRefreshRepoTasks,
        unavailable: (message) => {
          tasksFollowChanges = false;
          say(`Live updates unavailable: ${message}. Tasks refresh when the issue list does.`);
        },
      }));
    }
    const onForeground = () => {
      if (readBackgroundDocument()) {
        repoTaskLoader.reset();
        return;
      }
      scheduleRefreshRepoTasks();
    };
    listeners.track(bindForegroundChanges(document, typeof window === "undefined" ? null : window, onForeground));
    return () => {
      tasksFollowChanges = false;
      listeners.dispose();
      repoTaskLoader.dispose();
    };
  });

  // Keyed on the repository alone: the store republishes on every status poll
  // and re-running the resets per emission would thrash this state.
  $effect(() => {
    const repo = $currentRepoPath;
    untrack(() => {
      repoTasks = null;
      repoTasksError = null;
      notice = null;
      reportOpen = false;
      if (repo) void loadRepoTasks(repo);
    });
  });

  // The pane read a new issue list. Without a live subscription the task list
  // follows it, so "In tasks" does not go stale; with one, the change listener
  // already reloads on every change.
  let seenIssues: IssueInfo[] | null = null;
  $effect(() => {
    const next = issues;
    untrack(() => {
      if (seenIssues !== null && next !== seenIssues && !tasksFollowChanges) {
        void loadRepoTasks();
      }
      seenIssues = next;
    });
  });
</script>

{#snippet rowAction(issue: IssueInfo)}
  {@const inTask = findTaskForIssue(repoTasks, issue.number)}
  {#if inTask}
    <button
      type="button"
      class="gp-chip cursor-pointer gap-1"
      data-tone="accent"
      onclick={openTasksView}
      title={`Issue #${issue.number} is in tasks. Open the Tasks board.`}
    >
      <CheckCircle2 size={11} class="text-ok" />
      <span>In tasks</span>
    </button>
  {:else}
    <button
      type="button"
      class="gp-btn"
      data-size="sm"
      onclick={() => addIssueToTask(issue)}
      disabled={working || repoTasks === null}
      title={`Create a task from issue #${issue.number} for agents to work on`}
    >
      {#if taskAddingNumber === issue.number}
        <LoaderCircle size={11} class="animate-spin text-accent" />
        <span>Adding…</span>
      {:else}
        <ListPlus size={11} class="text-accent" />
        <span>Add to tasks</span>
      {/if}
    </button>
  {/if}
{/snippet}

<div data-testid="issue-task-actions">
  <div class="mb-2 flex flex-wrap items-center gap-2">
    <button
      type="button"
      class="gp-btn"
      data-size="sm"
      aria-expanded={reportOpen}
      onclick={() => (reportOpen = !reportOpen)}
    >
      <Bug size={13} /> Report issue…
    </button>
    {#if importPaused}
      <button
        type="button"
        class="gp-btn"
        data-size="sm"
        onclick={() => loadRepoTasks()}
        title={repoTasksError ?? "Reading the existing task list again"}
      >
        <RefreshCw size={12} /> <span>Try again</span>
      </button>
    {/if}
    {#if issues.length > 0}
      <button
        type="button"
        class="gp-btn"
        data-size="sm"
        onclick={addAllIssuesToTasks}
        disabled={working || repoTasks === null}
        title="Add every open issue that is not a task yet, for agent handoff"
      >
        {#if batchAdding}
          <LoaderCircle size={12} class="animate-spin text-accent" />
          <span>Importing…</span>
        {:else}
          <ListPlus size={12} class="text-accent" />
          <span>Add all to tasks</span>
          {#if unimportedCount > 0}<span class="gp-chip" data-tone="neutral" data-kind="count">{unimportedCount}</span>{/if}
        {/if}
      </button>
    {/if}
  </div>

  {#if reportOpen}
    <div class="gp-card mb-3 space-y-2 p-3">
      <input class="gp-field w-full" maxlength="256" placeholder="Issue title" aria-label="Issue title" bind:value={issueTitle} />
      <textarea
        class="gp-field gp-field-multi min-h-20 w-full"
        maxlength="65536"
        placeholder="What happened, what you expected, and how to reproduce it"
        aria-label="Issue description"
        bind:value={issueBody}
      ></textarea>
      <div class="flex gap-2">
        <input class="gp-field min-w-0 flex-1" placeholder="Labels, comma-separated" aria-label="Issue labels" bind:value={issueLabels} />
        <button type="button" class="gp-btn" data-variant="primary" onclick={reportIssue} disabled={!issueTitle.trim() || working}>
          {#if reporting}<LoaderCircle size={13} class="animate-spin" />{:else}<Bug size={13} />{/if} Report issue
        </button>
        <button type="button" class="gp-btn" onclick={() => (reportOpen = false)} disabled={reporting}>Cancel</button>
      </div>
    </div>
  {/if}

  {#if importPaused && repoTasksError}
    <div class="gp-notice mb-2" data-tone="unknown" role="status">
      <AlertTriangle size={14} class="shrink-0" style="color: var(--gp-notice-ink)" />
      <span class="min-w-0 flex-1">Task import paused. Couldn't read the existing task list ({repoTasksError}), so issues are not checked against it.</span>
    </div>
  {/if}

  {#if notice}
    <div class="gp-notice mb-2" data-tone={notice.tone} role="status">
      <span class="min-w-0 flex-1">{notice.text}</span>
      {#if notice.tasks}
        <button type="button" class="gp-btn shrink-0" data-size="sm" onclick={openTasksView}>Open Tasks</button>
      {/if}
    </div>
  {/if}

  {@render children?.(rowAction)}
</div>
