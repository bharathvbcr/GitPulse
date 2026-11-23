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
  import { PanelLeftClose, PanelLeftOpen, PanelRightOpen, Server, Laptop, Play, Square, RotateCcw, SquareTerminal, ArrowRightLeft } from "@lucide/svelte";
  import { bringBackToMavi, canContinueInTerminal, continueInTerminal, openShellHere, terminalHolding } from "../../mavi/handoff";
  import { focusTerminalSession } from "../../terminal/sessionFocus";
  import { askConfirm } from "../../stores/modalStore";
  import { toastStore } from "../../stores/toastStore";
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
    // The open thread counts as seen only while this view is on screen.
    const visible = () => maviStore.setViewing(document.visibilityState === "visible");
    visible();
    document.addEventListener("visibilitychange", visible);
    return () => {
      document.removeEventListener("visibilitychange", visible);
      maviStore.setViewing(false);
    };
  });

  // "New Mavi thread" from the palette, the dock or Explorer.
  let prefill = $state("");
  let preferWorktree = $state(false);
  let composeId = $state(0);
  /** The last request taken; plain, so the effect never reads what it writes. */
  let handledCompose = 0;
  $effect(() => {
    const request = maviStore.composeRequest;
    if (!request || request.id === handledCompose) return;
    handledCompose = request.id;
    composeId = request.id;
    prefill = request.message;
    preferWorktree = request.newWorktree;
    composing = true;
  });

  let moving = $state(false);

  async function moveToTerminal() {
    if (!record || moving) return;
    const bypass = record.mode === "bypass";
    const ok = await askConfirm({
      title: "Continue in a terminal",
      message:
        `Moves this conversation to a Claude Code tab in the terminal dock, in ${targetLabel(record.target)}. ` +
        "Mavi's agent for it ends first; the terminal resumes the same conversation with the same permission mode" +
        (bypass ? " (Bypass: the tab will ask you to acknowledge it again)." : ".") +
        " You can bring it back to Mavi once the terminal session ends.",
      confirmLabel: "Move to terminal",
    });
    if (!ok) return;
    moving = true;
    try {
      const result = await continueInTerminal(record.key);
      if (!result.ok) toastStore.warning(result.reason);
      else if (result.note) toastStore.info(result.note);
    } finally {
      moving = false;
    }
  }

  async function bringBack() {
    if (!record || moving) return;
    moving = true;
    resumeError = null;
    try {
      const result = await bringBackToMavi(record.key, record.mode === "bypass" ? resumeAck : false);
      if (!result.ok) resumeError = result.reason;
    } finally {
      moving = false;
    }
  }

  async function showTerminalTab() {
    const holder = record?.conversationId ? terminalHolding(record.conversationId) : null;
    if (holder) await focusTerminalSession(holder);
    else toastStore.info("No terminal tab is running this conversation now; bring it back to Mavi to continue.");
  }

  async function shellHere() {
    if (!record) return;
    const result = await openShellHere(record.key);
    if (!result.ok) toastStore.warning(result.reason);
  }

  let record = $derived(maviStore.openKey ? maviStore.record(maviStore.openKey) : undefined);
  let summary = $derived(record ? maviStore.summaryOf(record) : null);
  let thread = $derived(record ? maviStore.timelines[record.key] : undefined);
  let root = $derived(record?.target.kind === "local" ? record.target.root : null);
  let moved = $derived(!!record?.movedToTerminalAt);
  let movable = $derived(record ? canContinueInTerminal(record) : { ok: false as const, reason: "" });
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
      const ack = record.mode === "bypass" ? resumeAck : false;
      if (record.conversationId) {
        // Refuses while a terminal tab still runs this conversation.
        const result = await bringBackToMavi(record.key, ack);
        if (!result.ok) resumeError = result.reason;
      } else {
        await maviStore.resume(record.key, ack);
      }
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
        {#if record.target.kind === "local"}
          <button class="gp-icon-btn" aria-label="Open a terminal here" title="Open a shell in this thread's worktree, in the terminal dock" onclick={shellHere}>
            <SquareTerminal size={14} aria-hidden="true" />
          </button>
          {#if !moved}
            <button
              class="gp-btn"
              data-variant="ghost"
              disabled={!movable.ok || moving}
              title={movable.ok ? "Move this conversation to a Claude Code tab in the terminal dock" : movable.reason}
              onclick={moveToTerminal}
            >
              <ArrowRightLeft size={11} aria-hidden="true" /> Continue in terminal
            </button>
          {/if}
        {/if}
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
          {#key composeId}
            <MaviNewThread {prefill} {preferWorktree} onStarted={(key) => open(key)} onCancel={record ? () => (composing = false) : undefined} />
          {/key}
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
      {:else if moved}
        <div class="mavi-resume flex flex-wrap items-center gap-2 justify-center text-xs text-textMuted">
          <span>This conversation continues in a terminal tab.</span>
          <button class="gp-btn" onclick={showTerminalTab}><SquareTerminal size={12} aria-hidden="true" /> Show the tab</button>
          {#if record.mode === "bypass"}
            <Checkbox label="Bypass: run everything without asking" checked={resumeAck} onchange={(next) => (resumeAck = next)} />
          {/if}
          <button class="gp-btn" data-variant="primary" disabled={moving || (record.mode === "bypass" && !resumeAck)} onclick={bringBack}>
            <ArrowRightLeft size={12} aria-hidden="true" /> Bring back to Mavi
          </button>
          {#if resumeError}<span class="w-full text-center text-danger" role="alert">{resumeError}</span>{/if}
        </div>
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
