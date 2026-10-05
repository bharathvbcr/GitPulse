<script lang="ts">
  /**
   * The task sheet's Agent pane: start a run, and see the runs already started.
   *
   * This replaces `TaskRuns`, which held both halves and implemented the
   * launch half itself — a second copy of the board handoff's form, with its
   * own checkout field that started empty every time and its own idea of which
   * settings survive a launch. The form is now `TaskHandoffForm`, shared with
   * `TaskHandoffSheet`; what is left here is the history, and the decision of
   * which of the two to show first.
   *
   * That decision is the one real addition: when a run is already prepared or
   * running, *it* is what the reader came for, so the form folds away behind
   * "New attempt". With no live run the form is open, because then starting
   * one is the only thing this pane is for.
   */
  import { onDestroy, untrack } from "svelte";
  import { ChevronDown, ChevronRight } from "@lucide/svelte";
  import AgentDecisions from "./AgentDecisions.svelte";
  import TaskHandoffForm from "./TaskHandoffForm.svelte";
  import { interfaceStore } from "../stores/interfaceStore";
  import { consumeTaskTerminal } from "../terminal/taskLaunches";
  import { openTaskTerminal, queuedTerminalNote } from "../workbench/taskTerminal";
  import {
    cancelTaskRun,
    explainError,
    getTaskRun,
    launchManagedRun,
    listTaskRuns,
    releaseTaskRun,
    stopManagedRun,
    type PermissionMode,
    type Repository,
    type Task,
    type TaskRun,
  } from "../workbench/client";
  import {
    PROVIDER_LABELS,
    canRelease,
    runExpired,
    runHoldsCheckout,
    runStatusLabel,
    sanitizeHandoff,
    type HandoffGate,
    type HandoffSettings,
  } from "../workbench/taskHandoff";
  import { TASK_RUN_POLL_MS, nextTaskRunPollDelay } from "../workbench/runPoll";
  import { readEventLoopDelay } from "../runtime/loadCadence";
  import { bindForegroundChanges, readBackgroundDocument } from "../runtime/foreground";
  import type { OpenTabRef } from "../workbench/openMembership";

  let { task, repositories, openTabs = [], disabled = false, dirty = false, active = true, onCount }: {
    task: Task;
    repositories: Repository[];
    openTabs?: OpenTabRef[];
    /** Blocked for a reason other than unsaved edits (saving, deleting, Manvi). */
    disabled?: boolean;
    dirty?: boolean;
    active?: boolean;
    onCount?: (count: number) => void;
  } = $props();

  const PERMISSION_LABELS: Record<PermissionMode, string> = {
    inspect: "Inspect and plan",
    ask: "Ask for permissions",
    edit: "Allow workspace edits",
    auto_review: "Provider automatic review",
    preapproved: "Preapproved actions only",
    bypass: "Bypass permissions (advanced)",
  };
  /** States whose terminal can still be opened (an `unresolved` one cannot). */
  const OPENABLE = ["prepared", "starting", "running"];
  /**
   * One instant per render pass, refreshed wherever polling is reconsidered.
   *
   * A prepared attempt stops being live at a wall-clock moment, not on any
   * event, so without a value the view can depend on, an expired preparation
   * keeps rendering as "Prepared" — which is precisely what it did: the panel
   * stopped polling it the second it expired and then showed that last frame
   * forever, offering a recover button the store would always refuse.
   */
  let clock = $state(Date.now());

  let settings = $state<HandoffSettings>(untrack(() => sanitizeHandoff($interfaceStore.taskHandoff)));
  let gate = $state<HandoffGate>({ ok: false, reason: "" });
  let launching = $state(false);
  let preparePending = $state(false);
  let form = $state<{ launch: () => Promise<void> }>();

  let detail = $state<TaskRun | null>(null);
  let busy = $state(false);
  let loading = $state(false);
  let error = $state("");
  let historyError = $state("");
  let note = $state("");
  let runs = $state<TaskRun[]>([]);
  let total = $state(0);
  let cursor = $state<string | null>(null);
  let reviewingRunID = $state<string | null>(null);
  let decisionRefresh = $state(0);
  let expandedForm = $state<boolean | null>(null);
  let disposed = false;
  let generation = 0;
  let timer: ReturnType<typeof setTimeout> | null = null;

  const live = $derived(runs.filter((run) => runHoldsCheckout(run, clock)));
  // Null means "nobody has chosen"; the default follows whether a run is live.
  const formOpen = $derived(expandedForm ?? live.length === 0);

  $effect(() => { onCount?.(total); });

  function schedule() {
    if (timer) clearTimeout(timer);
    timer = null;
    if (disposed || !active) return;
    // Advanced before the decision below, so "keep polling" and "still live"
    // are answered against one instant. The final tick after an expiry is what
    // flips the row from Prepared to expired and then stops the loop.
    clock = Date.now();
    const delay = nextTaskRunPollDelay({
      baseMs: TASK_RUN_POLL_MS,
      lagMs: readEventLoopDelay(),
      background: readBackgroundDocument(),
      live: runs.some((run) => ["starting", "running"].includes(run.state) || (run.state === "prepared" && run.expires_at * 1000 > clock)),
      active: true,
    });
    if (delay === null) return;
    timer = setTimeout(() => { timer = null; void refresh(); }, delay);
  }
  async function refresh(more = false) {
    if (loading || disposed || !active) return;
    loading = true;
    const ticket = generation;
    try {
      const page = await listTaskRuns(task.id, more ? cursor ?? undefined : undefined);
      if (disposed || ticket !== generation) return;
      runs = more ? [...new Map([...runs, ...page.items].map((run) => [run.id, run])).values()] : page.items;
      total = page.total; cursor = page.next_cursor;
      decisionRefresh += 1;
      historyError = "";
    } catch (cause) { if (!disposed && ticket === generation) historyError = explainError(cause); }
    finally { loading = false; schedule(); }
  }
  $effect(() => {
    if (!active) { if (timer) clearTimeout(timer); timer = null; return; }
    untrack(() => { void refresh(); });
    const wake = () => {
      if (readBackgroundDocument()) { if (timer) { clearTimeout(timer); timer = null; } return; }
      void refresh();
    };
    const unbind = bindForegroundChanges(document, typeof window === "undefined" ? null : window, wake);
    return () => { unbind(); if (timer) clearTimeout(timer); timer = null; };
  });
  onDestroy(() => { disposed = true; generation += 1; if (timer) clearTimeout(timer); });

  function launched(run: TaskRun) {
    runs = [run, ...runs.filter((item) => item.id !== run.id)];
    total = Math.max(total, runs.length);
    if (run.kind === "managed") reviewingRunID = run.id;
    // A fresh run is the answer now, so stop showing the question.
    expandedForm = false;
    schedule();
  }
  async function open(run: TaskRun) {
    if ((await openTaskTerminal(run)) === "queued" && !disposed) note = queuedTerminalNote(run.cwd);
  }
  async function cancel(run: TaskRun) {
    busy = true; error = "";
    try { const saved = await cancelTaskRun(run); consumeTaskTerminal(run.id); runs = runs.map((item) => item.id === saved.id ? saved : item); }
    catch (cause) { error = explainError(cause); }
    finally { busy = false; schedule(); }
  }
  async function manage(run: TaskRun) {
    const current = await launchManagedRun(run.id);
    if (disposed) return;
    runs = runs.map((item) => item.id === current.id ? current : item);
    reviewingRunID = current.id;
    note = current.state === "running" ? `Managed ${PROVIDER_LABELS[current.provider]} started. Review its requests below.` : `Managed attempt: ${current.state}.`;
    schedule();
  }
  async function resumeManaged(run: TaskRun) {
    busy = true; error = "";
    try { await manage(run); } catch (cause) { error = explainError(cause); }
    finally { busy = false; schedule(); }
  }
  async function stopManaged(run: TaskRun) {
    busy = true; error = "";
    try { await stopManagedRun(run.id); note = "Stop requested. Waiting for process confirmation."; }
    catch (cause) { error = explainError(cause); }
    finally { busy = false; schedule(); }
  }
  /**
   * Ask the host to free a checkout this attempt still holds. It decides on
   * process evidence, so the answer is either a release or the reason there
   * was none — never a silent no-op.
   */
  async function release(run: TaskRun) {
    busy = true; error = ""; note = "";
    try {
      const result = await releaseTaskRun(run.id);
      if (disposed) return;
      runs = runs.map((item) => item.id === result.run.id ? result.run : item);
      note = result.released ? "Released. The checkout is free for another attempt." : result.reason;
    } catch (cause) { error = explainError(cause); }
    finally { busy = false; schedule(); }
  }
  async function inspect(run: TaskRun) {
    busy = true; error = "";
    try { const current = await getTaskRun(run.id); if (!disposed) detail = current; }
    catch (cause) { error = explainError(cause); }
    finally { busy = false; }
  }
  async function reopen(run: TaskRun) { busy = true; try { await open(run); } catch (cause) { error = explainError(cause); } finally { busy = false; } }
</script>

<section class="task-runs" aria-label="Task agent runs" data-testid="task-agent-panel">
  <button
    type="button"
    class="form-toggle"
    data-testid="task-agent-toggle"
    aria-expanded={formOpen}
    aria-controls="task-agent-launch"
    onclick={() => { expandedForm = !formOpen; }}
  >
    {#if formOpen}<ChevronDown size={12} />{:else}<ChevronRight size={12} />{/if}
    <span class="flex-1 text-left" data-testid="task-agent-toggle-label">{live.length ? "New attempt" : "Send to an agent"}</span>
    <span class="meta">Revision {task.revision}</span>
  </button>

  <div id="task-agent-launch" hidden={!formOpen}>
    <TaskHandoffForm
      bind:this={form}
      bind:settings
      taskId={task.id}
      revision={task.revision}
      repositoryIds={task.repository_ids}
      primaryRepositoryId={task.primary_repository_id}
      {repositories}
      {openTabs}
      {dirty}
      {disabled}
      onPrepared={launched}
      onLaunched={launched}
      onGate={(next) => { gate = next; }}
      onBusy={(next) => { launching = next; }}
      onPending={(next) => { preparePending = next; }}
    />
    <div class="launch-row">
      <button class="gp-btn-primary" type="button" onclick={() => void form?.launch()} disabled={!gate.ok}>
        {launching ? "Preparing…" : preparePending ? "Retry preparation" : settings.kind === "managed" ? `Start managed ${PROVIDER_LABELS[settings.provider]}` : `Launch in ${PROVIDER_LABELS[settings.provider]}`}
      </button>
      {#if !gate.ok && gate.reason}<span class="meta gate">{gate.reason}</span>{/if}
    </div>
    <p class="meta">Completing a run leaves task acceptance for review.</p>
  </div>

  {#if note}<p role="status">{note}</p>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}

  <div class="history-heading">
    <h4>Run history{#if total}<span class="meta"> · {runs.length} of {total}</span>{/if}</h4>
    <button class="gp-btn" type="button" onclick={() => refresh()} disabled={loading}>Refresh</button>
  </div>
  {#if historyError}<p class="error" role="alert">{historyError}</p>{/if}
  {#if !runs.length}<p>{loading ? "Loading runs…" : "No runs yet."}</p>{/if}
  {#each runs as run (run.id)}
    {@const expired = runExpired(run, clock)}
    <article class:is-live={runHoldsCheckout(run, clock)}>
      <strong>{PROVIDER_LABELS[run.provider] ?? run.provider} · {run.kind === "managed" ? "Managed" : "Terminal"} · {runStatusLabel(run, clock)}</strong>
      <small>Revision {run.source_revision} · {PERMISSION_LABELS[run.permission_mode] ?? run.permission_mode}{run.exit_code !== null ? ` · exit ${run.exit_code}` : ""}</small>
      <small title={run.cwd}>{run.cwd}</small>
      {#if run.reason}<p>{run.reason}</p>{/if}
      {#if run.state === "unresolved"}<p class="error">This attempt's outcome is unresolved and it still holds its checkout. Release it once its agent has stopped; another worktree can run meanwhile.</p>
      {:else if run.outcome_uncertain}<p>Released without an observed exit. Review what it changed before relying on it.</p>{/if}
      {#if expired}<p>This preparation expired before it started, so it no longer holds its checkout. Cancel it to clear it from the history, then prepare a new attempt.</p>{/if}
      {#if run.kind === "external_terminal" && OPENABLE.includes(run.state) && !expired}<button class="gp-btn" type="button" onclick={() => reopen(run)} disabled={busy}>Open terminal</button>{/if}
      {#if run.kind === "managed"}
        {#if ["prepared", "starting"].includes(run.state) && !expired}<button class="gp-btn" type="button" onclick={() => resumeManaged(run)} disabled={busy}>Start or recover managed launch</button>{/if}
        {#if ["starting", "running", "unresolved"].includes(run.state)}<button class="gp-btn" type="button" onclick={() => stopManaged(run)} disabled={busy}>Stop managed run</button>{/if}
        {#if run.provider_state}<p>Provider turn: {run.provider_state}. {run.provider_state === "completed" ? "Review the result before accepting the task." : ""}</p>{/if}
        <button class="gp-btn" type="button" onclick={() => inspect(run)} disabled={busy}>View output and settings</button>
        {#if detail?.id === run.id}
          <section aria-label="Managed run details">
            <p>Saved output · revision {detail.revision}</p>
            <textarea class="gp-field gp-field-multi" readonly rows="7" aria-label="Managed agent output" value={detail.output ?? "No output saved yet."}></textarea>
            {#if detail.output_truncated}<p>Output reached the 128 KiB retention limit.</p>{/if}
            <details><summary>Effective provider settings</summary><textarea class="gp-field gp-field-multi" readonly rows="7" aria-label="Effective provider settings" value={detail.effective_configuration ?? "Settings are not recorded yet."}></textarea></details>
            <button class="gp-btn" type="button" onclick={() => inspect(run)} disabled={busy}>Refresh output</button><button class="gp-btn" type="button" onclick={() => { detail = null; }}>Close details</button>
          </section>
        {/if}
      {/if}
      {#if run.state === "prepared"}<button class="gp-btn" type="button" onclick={() => cancel(run)} disabled={busy}>Cancel preparation</button>{/if}
      {#if canRelease(run)}<button class="gp-btn" type="button" onclick={() => release(run)} disabled={busy} title="Frees the checkout if the agent and the GitPulse that launched it have both stopped. A running agent is never released.">Release checkout</button>{/if}
      {#if run.session_id && run.kind === "managed"}<button class="gp-btn" type="button" onclick={() => { reviewingRunID = reviewingRunID === run.id ? null : run.id; }}>{reviewingRunID === run.id ? "Hide requests" : "Review requests"}</button>{/if}
      {#if reviewingRunID === run.id}<AgentDecisions {run} {active} refreshToken={decisionRefresh} />{/if}
    </article>
  {/each}
  {#if cursor}<button class="gp-btn" type="button" onclick={() => refresh(true)} disabled={loading || runs.length >= 180}>Load more ({runs.length} of {total})</button>{/if}
</section>

<style>
  .task-runs{font-size:12px;padding-top:2px}
  h4{margin:0;font-weight:650}
  p{color:rgb(var(--c-text-muted));line-height:1.5;margin:8px 0}
  .meta{color:rgb(var(--c-text-muted));font-size:11px}
  .form-toggle{display:flex;align-items:center;gap:7px;width:100%;padding:7px 8px;margin-bottom:10px;border:1px solid rgb(var(--c-border));border-radius:8px;font-weight:600;background:rgb(var(--c-bg)/.35)}
  .form-toggle:hover{background:rgb(var(--c-surface-hover)/.7)}
  .form-toggle:focus-visible{outline:2px solid rgb(var(--c-accent)/.6);outline-offset:-1px}
  #task-agent-launch[hidden]{display:none}
  .launch-row{display:flex;align-items:center;gap:9px;flex-wrap:wrap;margin-top:12px}
  .gate{flex:1;min-width:8rem}
  .history-heading{display:flex;align-items:center;justify-content:space-between;gap:8px;margin-top:20px;padding-top:12px;border-top:1px solid rgb(var(--c-border))}
  article{padding:10px 0;border-bottom:1px solid rgb(var(--c-border))}
  article.is-live{border-left:2px solid rgb(var(--c-accent));padding-left:9px}
  small{display:block;color:rgb(var(--c-text-muted));margin:5px 0;overflow-wrap:anywhere}
  textarea{min-width:0;width:100%;padding:7px;border:1px solid rgb(var(--c-border));border-radius:6px;background:var(--mac-fill-bg,rgb(var(--c-bg)));color:inherit}
  button:disabled{opacity:.5}
  .error{color:#dc6565}
</style>
