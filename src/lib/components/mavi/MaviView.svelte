<script lang="ts">
  /**
   * Mavi: coding agents as conversations instead of terminals.
   *
   * Three columns: threads, the conversation, and what it changed. The rail
   * and the changes column fold away, so the conversation keeps its 46rem
   * lane at laptop widths. The changes column opens by itself the first time
   * a thread changes a file, and from any file chip or turn footer.
   */
  import { onMount } from "svelte";
  import { PanelLeftClose, PanelLeftOpen, PanelRightOpen, Server, Laptop, Play, Square, RotateCcw } from "@lucide/svelte";
  import { maviStore } from "../../mavi/store.svelte";
  import { MAVI_MODES } from "../../mavi/client";
  import { openApprovals } from "../../mavi/reduce";
  import { collectChanges } from "../../mavi/changes";
  import { STATUS_LABEL, targetLabel } from "../../mavi/threads";
  import MaviRail from "./MaviRail.svelte";
  import MaviTimeline from "./MaviTimeline.svelte";
  import MaviComposer from "./MaviComposer.svelte";
  import MaviChanges from "./MaviChanges.svelte";
  import MaviNewThread from "./MaviNewThread.svelte";
  import Checkbox from "../Checkbox.svelte";

  const RAIL_KEY = "gitpulse.mavi.rail";
  const CHANGES_KEY = "gitpulse.mavi.changes";

  function readFlag(key: string, fallback: boolean): boolean {
    try {
      const v = localStorage.getItem(key);
      return v === null ? fallback : v === "1";
    } catch {
      return fallback;
    }
  }
  function writeFlag(key: string, value: boolean) {
    try {
      localStorage.setItem(key, value ? "1" : "0");
    } catch {
      /* in memory only */
    }
  }

  let railOpen = $state(readFlag(RAIL_KEY, true));
  let changesOpen = $state(readFlag(CHANGES_KEY, true));
  let composing = $state(false);
  let focusPath = $state<string | null>(null);
  let focusTurn = $state<number | null>(null);
  let initError = $state<string | null>(null);
  let resumeAck = $state(false);
  let resumeError = $state<string | null>(null);
  let resuming = $state(false);

  onMount(() => {
    maviStore.init().catch((e) => (initError = e instanceof Error ? e.message : String(e)));
  });

  let record = $derived(maviStore.openKey ? maviStore.record(maviStore.openKey) : undefined);
  let summary = $derived(record ? maviStore.summaryOf(record) : null);
  let thread = $derived(record ? maviStore.timelines[record.key] : undefined);
  let root = $derived(record?.target.kind === "local" ? record.target.root : null);
  let working = $derived(summary?.phase === "working" || summary?.phase === "approval");
  let running = $derived(!!summary && summary.phase !== "ended");
  let approvals = $derived(thread ? openApprovals(thread) : []);
  let status = $derived(record ? maviStore.status(record) : null);
  let modeLabel = $derived(MAVI_MODES.find((m) => m.id === record?.mode)?.label ?? record?.mode ?? "");
  let hasChanges = $derived(thread ? collectChanges(thread, root, { kind: "thread" }).length > 0 : false);
  let showNew = $derived(composing || maviStore.records.length === 0 || !record);

  // The changes column opens itself the first time a thread changes a file.
  let announced = new Set<string>();
  $effect(() => {
    if (record && hasChanges && !announced.has(record.key)) {
      announced.add(record.key);
      changesOpen = true;
    }
  });

  $effect(() => writeFlag(RAIL_KEY, railOpen));
  $effect(() => writeFlag(CHANGES_KEY, changesOpen));

  async function open(key: string) {
    composing = false;
    focusPath = null;
    focusTurn = null;
    resumeError = null;
    await maviStore.open(key);
  }

  function showFile(path: string) {
    focusPath = path;
    changesOpen = true;
  }

  function showTurn(turn: number) {
    focusTurn = turn;
    focusPath = null;
    changesOpen = true;
  }

  function reviewApproval() {
    document.querySelector<HTMLElement>(".mavi-approval")?.focus();
    document.querySelector<HTMLElement>(".mavi-approval")?.scrollIntoView({ block: "center", behavior: "smooth" });
  }

  async function resume() {
    if (!record) return;
    resuming = true;
    resumeError = null;
    try {
      await maviStore.resume(record.key, record.mode === "bypass" ? resumeAck : false);
    } catch (e) {
      resumeError = e instanceof Error ? e.message : String(e);
    } finally {
      resuming = false;
    }
  }
</script>

<div class="mavi-view flex-1 flex min-h-0" data-testid="mavi-view">
  {#if railOpen}
    <div class="mavi-rail-col">
      <MaviRail onNew={() => (composing = true)} onOpen={open} />
    </div>
  {/if}

  <section class="mavi-main flex-1 flex flex-col min-w-0 min-h-0" aria-label="Conversation">
    <header class="mavi-head flex items-center gap-2">
      <button
        class="gp-icon-btn"
        aria-label={railOpen ? "Hide threads" : "Show threads"}
        title={railOpen ? "Hide threads" : "Show threads"}
        onclick={() => (railOpen = !railOpen)}
      >
        {#if railOpen}<PanelLeftClose size={14} aria-hidden="true" />{:else}<PanelLeftOpen size={14} aria-hidden="true" />{/if}
      </button>
      {#if record && !showNew}
        <div class="min-w-0 flex-1">
          <div class="text-xs font-medium text-textPrimary truncate">{record.title}</div>
          <div class="flex items-center gap-1.5 text-micro text-textMuted min-w-0">
            {#if record.target.kind === "ssh"}<Server size={10} aria-hidden="true" />{:else}<Laptop size={10} aria-hidden="true" />{/if}
            <span class="truncate font-mono" title={record.target.kind === "ssh" ? `${record.target.host}:${record.target.dir}` : record.target.root}>{targetLabel(record.target)}</span>
            {#if thread?.model}<span>· {thread.model}</span>{/if}
            {#if summary && summary.cost_usd > 0}<span>· ${summary.cost_usd.toFixed(2)}</span>{/if}
            {#if status}<span>· {STATUS_LABEL[status]}</span>{/if}
          </div>
        </div>
        {#if running}
          <button class="gp-btn" data-variant="ghost" title="End this thread's agent" onclick={() => record && maviStore.stop(record.key)}>
            <Square size={11} aria-hidden="true" /> End
          </button>
        {/if}
      {:else}
        <div class="flex-1 text-xs font-medium text-textPrimary">Mavi</div>
      {/if}
      {#if !changesOpen && record && !showNew}
        <button class="gp-icon-btn" aria-label="Show changes" title="Show changes" onclick={() => (changesOpen = true)}>
          <PanelRightOpen size={14} aria-hidden="true" />
        </button>
      {/if}
    </header>

    {#if initError}
      <p class="gp-notice gp-notice-error text-meta" role="alert">Mavi couldn't reach its host: {initError}</p>
    {/if}

    {#if showNew}
      <div class="flex-1 overflow-y-auto flex justify-center items-start mavi-new-wrap">
        <div class="w-full flex flex-col items-center gap-4">
          {#if maviStore.records.length === 0}
            <div class="mavi-intro text-center">
              <h1 class="text-base font-medium text-textPrimary">Work with an agent in a conversation</h1>
              <p class="text-xs text-textMuted mt-1">
                Mavi runs Claude Code here or on an SSH host and shows its work as a timeline: what it read, ran and edited, every permission request inline with the change it proposes, and a live list of what each turn changed.
              </p>
            </div>
          {/if}
          <MaviNewThread onStarted={(key) => open(key)} onCancel={record ? () => (composing = false) : undefined} />
        </div>
      </div>
    {:else if record && thread}
      <MaviTimeline
        {thread}
        {root}
        {working}
        onAnswer={(requestId, decision, message) => maviStore.answer(record.key, requestId, decision, message)}
        onShowTurn={showTurn}
        onOpenFile={showFile}
      />
      {#if maviStore.errors[record.key]}
        <p class="gp-notice gp-notice-error text-meta" role="alert">{maviStore.errors[record.key]}</p>
      {/if}
      {#if running}
        <MaviComposer
          {working}
          queue={maviStore.queues[record.key] ?? []}
          pendingApprovals={approvals.length}
          {modeLabel}
          onSend={(text) => maviStore.send(record.key, text)}
          onStop={() => maviStore.interrupt(record.key)}
          onUnqueue={(i) => maviStore.unqueue(record.key, i)}
          onReviewApproval={reviewApproval}
        />
      {:else}
        <div class="mavi-resume flex flex-wrap items-center gap-2 justify-center text-xs text-textMuted">
          <span>{record.conversationId ? "This thread's agent is not running." : "This thread's agent ended before it saved a conversation."}</span>
          {#if record.mode === "bypass"}
            <Checkbox
              label="Bypass: run everything without asking"
              checked={resumeAck}
              onchange={(next) => (resumeAck = next)}
            />
          {/if}
          <button class="gp-btn" data-variant="primary" disabled={resuming || (record.mode === "bypass" && !resumeAck)} onclick={resume}>
            {#if record.conversationId}<RotateCcw size={12} aria-hidden="true" /> Resume{:else}<Play size={12} aria-hidden="true" /> Start again{/if}
          </button>
          {#if resumeError}<span class="w-full text-center text-danger" role="alert">{resumeError}</span>{/if}
        </div>
      {/if}
    {/if}
  </section>

  {#if changesOpen && record && thread && !showNew}
    <div class="mavi-changes-col">
      <MaviChanges
        {thread}
        {root}
        sessionId={record.sessionId}
        bind:focus={focusPath}
        bind:turn={focusTurn}
        onHide={() => (changesOpen = false)}
      />
    </div>
  {/if}
</div>

<style>
  .mavi-view {
    container-type: inline-size;
  }
  .mavi-rail-col {
    flex: 0 0 232px;
    min-height: 0;
  }
  .mavi-changes-col {
    flex: 0 0 clamp(300px, 34%, 460px);
    min-height: 0;
    border-left: 1px solid var(--gp-edge-soft);
  }
  .mavi-head {
    padding: 6px 12px 6px 8px;
    min-height: 44px;
    border-bottom: 1px solid var(--gp-edge-soft);
  }
  .mavi-new-wrap {
    padding: 32px 20px;
  }
  .mavi-intro {
    max-width: 36rem;
  }
  .mavi-resume {
    padding: 12px 20px 16px;
    border-top: 1px solid var(--gp-edge-soft);
  }
  /* Narrow windows: the rail yields first, the changes column overlays. */
  @container (max-width: 980px) {
    .mavi-rail-col {
      flex-basis: 200px;
    }
    .mavi-changes-col {
      flex-basis: 300px;
    }
  }
</style>
