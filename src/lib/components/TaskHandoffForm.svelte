<script lang="ts">
  /**
   * The agent-handoff form: which agent, which connection, which checkout,
   * which permission mode — and the launch itself.
   *
   * It exists because there are two places to start a run and they must not be
   * two ideas of what a run is. The board's `TaskHandoffSheet` wraps this in a
   * modal; the editor's `TaskAgentPanel` puts it above the task's run history.
   * Both get the same checkout resolution, the same gate, the same
   * revision re-read, and the same remembered settings, because all of that
   * lives here once.
   *
   * What this component deliberately does *not* own: chrome. No header, no
   * footer, no launch button. The host draws those, calls `launch()`, and
   * reads the gate through `onGate` — so a modal can put the button in a
   * footer and a panel can put it inline without this file knowing.
   */
  import { onDestroy, untrack } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { FolderOpen } from "@lucide/svelte";
  import { interfaceStore } from "../stores/interfaceStore";
  import { repoStore } from "../stores/repoStore";
  import { toastStore } from "../stores/toastStore";
  import { isCaseInsensitiveFs } from "../repos/paths";
  import { enqueueTaskTerminal } from "../terminal/taskLaunches";
  import { bounded } from "../workbench/taskActions";
  import {
    PERMISSION_MODES,
    explainError,
    getRepository,
    getTask,
    launchManagedRun,
    newID,
    prepareTaskRun,
    WorkbenchError,
    type PermissionMode,
    type Repository,
    type RunPreparation,
    type TaskRun,
  } from "../workbench/client";
  import {
    MAX_CHECKOUT_LENGTH,
    PROVIDER_CHOICES,
    PROVIDER_LABELS,
    checkoutCandidates,
    defaultHandoff,
    handoffGate,
    normalizeCheckout,
    preferredCheckout,
    reconcileHandoff,
    supportsManaged,
    type HandoffGate,
    type HandoffSettings,
  } from "../workbench/taskHandoff";
  import type { OpenTabRef } from "../workbench/openMembership";

  let {
    taskId,
    revision,
    repositoryIds,
    primaryRepositoryId,
    repositories,
    openTabs = [],
    settings = $bindable(),
    dirty = false,
    disabled = false,
    onPrepared,
    onLaunched,
    onGate,
    onBusy,
    onPending,
  }: {
    taskId: string;
    revision: number;
    repositoryIds: string[];
    primaryRepositoryId: string;
    repositories: Repository[];
    openTabs?: OpenTabRef[];
    settings: HandoffSettings;
    /** Unsaved editor changes; the gate refuses a launch while they exist. */
    dirty?: boolean;
    disabled?: boolean;
    /**
     * A run now exists in the store, prepared but not yet started.
     *
     * Separate from `onLaunched` because the two answer different questions.
     * A managed attempt whose start reply is lost is still a real prepared
     * run with a resume and a cancel control — a host that only learned about
     * runs on success would leave it invisible and unrecoverable.
     */
    onPrepared?: (run: TaskRun) => void;
    onLaunched: (run: TaskRun) => void;
    onGate?: (gate: HandoffGate) => void;
    onBusy?: (busy: boolean) => void;
    /** True while a preparation outcome is unknown and must be retried as-is. */
    onPending?: (pending: boolean) => void;
  } = $props();

  const PERMISSION_LABELS: Record<PermissionMode, string> = {
    inspect: "Inspect and plan",
    ask: "Ask for permissions",
    edit: "Allow workspace edits",
    auto_review: "Provider automatic review",
    preapproved: "Preapproved actions only",
    bypass: "Bypass permissions (advanced)",
  };

  let repositoryId = $state(untrack(() => primaryRepositoryId));
  let checkout = $state("");
  let acknowledged = $state(false);
  let busy = $state(false);
  let error = $state("");
  let note = $state("");
  let pending = $state<RunPreparation | null>(null);
  let disposed = false;

  const pathOpts = { caseInsensitive: isCaseInsensitiveFs() };
  const selected = $derived(repositoryIds.includes(repositoryId) ? repositoryId : primaryRepositoryId);
  const candidates = $derived(checkoutCandidates(selected, repositories, openTabs, pathOpts));
  /**
   * Every control is locked while a preparation outcome is unknown.
   *
   * `pending` replays one exact request, so a reader who could edit the
   * provider or the checkout underneath it would press "Retry preparation"
   * believing it retries what the form now shows. It does not, and cannot —
   * changing the request would risk a second run for the same attempt.
   */
  const locked = $derived(busy || disabled || pending !== null);
  const gate = $derived(handoffGate({ checkout, settings, acknowledgedBypass: acknowledged, dirty, busy: busy || disabled }));
  const repositoryName = $derived(repositories.find((repo) => repo.id === selected)?.name ?? selected);

  export function launchState(): HandoffGate { return gate; }

  onDestroy(() => { disposed = true; });

  $effect(() => { onGate?.(gate); });
  $effect(() => { onBusy?.(busy); });
  $effect(() => { onPending?.(pending !== null); });

  // The checkout follows the repository. A reader who typed one keeps it; an
  // untouched field takes the best candidate for whatever is now selected.
  let touchedCheckout = false;
  $effect(() => {
    const best = preferredCheckout(candidates);
    if (!touchedCheckout) checkout = best;
  });

  function choose(next: Partial<HandoffSettings>) {
    settings = reconcileHandoff({ ...settings, ...next });
    if (settings.permission !== "bypass") acknowledged = false;
  }

  async function browse() {
    try {
      const path = await invoke<string | null>("cmd_pick_folder");
      if (path && !disposed) { checkout = path; touchedCheckout = true; }
    } catch (cause) { error = explainError(cause); }
  }

  async function openTerminalFor(run: TaskRun) {
    let queued = false;
    const opened = await repoStore.openRepo(run.cwd, {
      onReady: (path) => {
        enqueueTaskTerminal({ runId: run.id, repoPath: path, provider: run.provider, title: run.task_title });
        queued = true;
        interfaceStore.setGlobalSurface("repository");
        repoStore.setTerminalOpen(true);
      },
    });
    if (!opened || !queued) {
      throw new Error("The checkout did not finish opening. The attempt is prepared; open it again from the run history, or cancel it there.");
    }
  }

  /**
   * Prepare, then start.
   *
   * The task is re-read first: the caller carries the revision it last loaded,
   * and launching an agent against a revision that has since changed elsewhere
   * would hand it a brief nobody wrote. `pending` keeps one preparation
   * identity across a retry so a lost reply cannot create two runs.
   */
  export async function launch() {
    if (!gate.ok || busy) return;
    busy = true; error = ""; note = "";
    try {
      if (!pending) {
        const path = normalizeCheckout(checkout);
        if (!path) throw new Error("Choose the working checkout this agent should run in.");
        const latest = await bounded(getTask(taskId));
        if (disposed) return;
        if (latest.revision !== revision) {
          throw new Error("This task changed since it was loaded. Reload the saved task and launch its latest revision.");
        }
        const repo = await bounded(getRepository(selected));
        if (disposed) return;
        pending = {
          kind: settings.kind,
          id: newID(),
          request_id: newID(),
          task_id: latest.id,
          source_revision: latest.revision,
          repository_id: repo.id,
          repository_revision: repo.revision,
          repo_path: path,
          provider: settings.provider,
          permission_mode: settings.permission,
          acknowledge_bypass: acknowledged,
        };
      }
      const run = await bounded(prepareTaskRun(pending));
      pending = null;
      if (disposed) return;
      // Remembered only after a preparation the store accepted, and never
      // with `bypass`. Every other mode is remembered, because re-picking
      // "Allow workspace edits" before each launch is the friction this form
      // exists to remove — but a mode that turns off the agent's sandbox has
      // to be chosen again, with its acknowledgement, every single time.
      if (settings.permission === "bypass") settings = { ...settings, permission: defaultHandoff().permission };
      acknowledged = false;
      interfaceStore.setTaskHandoff(settings);
      // The attempt is real the moment the store accepts it, so publish it
      // before anything can fail. Everything after this point is recoverable
      // from the row it produces.
      onPrepared?.(run);
      if (run.kind === "managed") {
        const started = await bounded(launchManagedRun(run.id));
        if (disposed) return;
        toastStore.success(`Managed Codex ${started.state === "running" ? "started" : started.state}.`);
        onLaunched(started);
        return;
      }
      note = "Opening the provider terminal…";
      await openTerminalFor(run);
      if (disposed) return;
      note = "";
      toastStore.success(`${PROVIDER_LABELS[settings.provider]} requested for revision ${run.source_revision}.`);
      onLaunched(run);
    } catch (cause) {
      if (disposed) return;
      error = explainError(cause);
      if (cause instanceof WorkbenchError && !["transport_error", "worker_error", "store_error"].includes(cause.code)) pending = null;
    } finally { if (!disposed) busy = false; }
  }
</script>

<div class="handoff-form" data-testid="task-handoff-form">
  <div class="pair">
    <div class="gp-segmented" role="group" aria-label="Coding agent">
      {#each PROVIDER_CHOICES as provider (provider)}
        <button type="button" class="gp-seg-btn" data-active={settings.provider === provider} aria-pressed={settings.provider === provider} disabled={locked} onclick={() => choose({ provider })}>{PROVIDER_LABELS[provider]}</button>
      {/each}
    </div>
    <div class="gp-segmented" role="group" aria-label="Connection">
      <button type="button" class="gp-seg-btn" data-active={settings.kind === "external_terminal"} aria-pressed={settings.kind === "external_terminal"} disabled={locked} onclick={() => choose({ kind: "external_terminal" })}>Terminal</button>
      <button
        type="button"
        class="gp-seg-btn"
        data-active={settings.kind === "managed"}
        aria-pressed={settings.kind === "managed"}
        disabled={locked || !supportsManaged(settings.provider)}
        title={supportsManaged(settings.provider) ? "GitPulse supervises the run and shows its requests here." : `${PROVIDER_LABELS[settings.provider]} supports terminal handoffs only.`}
        onclick={() => choose({ kind: "managed" })}
      >Managed</button>
    </div>
  </div>

  {#if repositoryIds.length > 1}
    <label>Repository
      <select class="gp-select" bind:value={repositoryId} disabled={locked} onchange={() => { touchedCheckout = false; }}>
        {#each repositoryIds as id (id)}
          <option value={id}>{repositories.find((repo) => repo.id === id)?.name ?? id}</option>
        {/each}
      </select>
    </label>
  {/if}

  <label>Working checkout
    <span class="checkout">
      {#if candidates.length}
        <select
          class="gp-select"
          value={candidates.some((entry) => entry.path === checkout) ? checkout : "__custom__"}
          disabled={locked}
          aria-label="Known checkouts"
          onchange={(e) => {
            const next = e.currentTarget.value;
            if (next !== "__custom__") { checkout = next; touchedCheckout = true; }
          }}
        >
          {#each candidates as candidate (candidate.path)}
            <option value={candidate.path}>{candidate.label}{candidate.source === "derived" ? " — repository folder" : ""}</option>
          {/each}
          <option value="__custom__">Another folder…</option>
        </select>
      {/if}
      <input
        class="gp-field"
        value={checkout}
        disabled={locked}
        placeholder={`Path to the ${repositoryName} checkout`}
        aria-label="Checkout path"
        maxlength={MAX_CHECKOUT_LENGTH}
        oninput={(e) => { checkout = e.currentTarget.value; touchedCheckout = true; }}
      />
      <button type="button" class="gp-btn" disabled={locked} onclick={browse}><FolderOpen size={12} /> Browse</button>
    </span>
  </label>
  {#if !candidates.length}
    <p class="meta">GitPulse has no open checkout for {repositoryName}. Choose the folder this agent should work in.</p>
  {:else if candidates[0].source === "derived"}
    <p class="meta">Suggested from this repository's git directory. Open the repository in GitPulse to confirm it.</p>
  {/if}

  <label>Permission mode
    <select class="gp-select" bind:value={settings.permission} disabled={locked} onchange={() => { acknowledged = false; }}>
      {#each PERMISSION_MODES as mode (mode)}<option value={mode}>{PERMISSION_LABELS[mode]}</option>{/each}
    </select>
  </label>
  <p class="meta">These are requested settings. {settings.kind === "managed" ? "Manvi verifies the effective Codex settings before sending the task." : "The provider handles requests in its own terminal."}</p>
  {#if settings.permission === "bypass"}
    <label class="ack">
      <input type="checkbox" bind:checked={acknowledged} disabled={locked} />
      I authorize bypass for this launch attempt. Worktrees do not isolate host access.
    </label>
  {/if}

  {#if pending}<p role="status" class="warn">The preparation result is uncertain. Retry this exact attempt before creating another.</p>{/if}
  {#if note}<p role="status" class="meta">{note}</p>{/if}
  {#if error}<p role="alert" class="error">{error}</p>{/if}
</div>

<style>
  .handoff-form{display:flex;flex-direction:column;gap:12px;font-size:12px;min-width:0}
  .pair{display:flex;gap:8px;flex-wrap:wrap}
  .meta{margin:0;font-size:11px;color:rgb(var(--c-text-muted));line-height:1.5}
  label{display:flex;flex-direction:column;gap:6px;margin:0}
  .checkout{display:flex;gap:6px;flex-wrap:wrap;align-items:center}
  .checkout select{flex:1 1 12rem;min-width:0}
  .checkout input{flex:2 1 14rem;min-width:0}
  select,input{padding:7px;border:1px solid rgb(var(--c-border));border-radius:7px;background:rgb(var(--c-bg)/.6);color:inherit;font-size:12px;width:100%;min-width:0}
  .ack{flex-direction:row;align-items:flex-start;gap:7px;font-size:11px;color:rgb(var(--c-text-muted))}
  .ack input{width:auto}
  .error{margin:0;font-size:11px;color:#dc6565}
  .warn{margin:0;font-size:11px;color:rgb(180 83 9)}
  button:disabled{opacity:.5}
</style>
