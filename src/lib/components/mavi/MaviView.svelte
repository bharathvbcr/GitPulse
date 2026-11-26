<script lang="ts">
  /**
   * Mavi: coding agents as conversations instead of terminals.
   *
   * Three columns: threads, the conversation, and what it changed. The rail
   * and the changes column fold away and resize smoothly via WAI-ARIA
   * splitters, so the conversation lane keeps its chosen width. The changes
   * column opens by itself the first time a thread changes a file when enabled
   * in Mavi preferences, and from any file chip or turn footer.
   */
  import { onMount, tick } from "svelte";
  import {
    ArrowRightLeft,
    Laptop,
    MessagesSquare,
    PanelLeftClose,
    PanelLeftOpen,
    PanelRightOpen,
    Play,
    RotateCcw,
    Server,
    Sparkles,
    Square,
    SquareTerminal,
  } from "@lucide/svelte";
  import { bringBackToMavi, canContinueInTerminal, continueInTerminal, openShellHere, terminalHolding } from "../../mavi/handoff";
  import { focusTerminalSession } from "../../terminal/sessionFocus";
  import { askConfirm } from "../../stores/modalStore";
  import { currentRepoPath } from "../../stores/repoStore";
  import { toastStore } from "../../stores/toastStore";
  import { maviStore } from "../../mavi/store.svelte";
  import { MAVI_MODES } from "../../mavi/client";
  import { openApprovals } from "../../mavi/reduce";
  import { collectChanges, hasAnyChange } from "../../mavi/changes";
  import { STATUS_LABEL, statusTone, targetLabel } from "../../mavi/threads";
  import {
    MAVI_CHANGES_WIDTH_BOUNDS,
    MAVI_CHAT_WIDTH_CSS,
    MAVI_DENSITY_CSS,
    MAVI_RAIL_WIDTH_BOUNDS,
    MAVI_RESIZE_STEP,
    maviPrefs,
    type MaviSummaryEngine,
  } from "../../mavi/prefs.svelte";
  import { LAYERS } from "../../ui/layers";
  import MaviRail from "./MaviRail.svelte";
  import MaviTimeline from "./MaviTimeline.svelte";
  import MaviComposer from "./MaviComposer.svelte";
  import MaviChildTabs from "./MaviChildTabs.svelte";
  import MaviChanges from "./MaviChanges.svelte";
  import MaviNewThread from "./MaviNewThread.svelte";
  import MaviCustomizePopover from "./MaviCustomizePopover.svelte";
  import MaviEndSessionDialog from "./MaviEndSessionDialog.svelte";
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
  let customizeOpen = $state(false);
  let composing = $state(false);
  let focusPath = $state<string | null>(null);
  let focusTurn = $state<number | null>(null);
  let initError = $state<string | null>(null);
  let resumeAck = $state(false);
  let resumeError = $state<string | null>(null);
  let resuming = $state(false);

  let pendingHandoff = $state<{ agent: "claude" | "agy"; model: string | null } | null>(null);
  let endDialogOpen = $state(false);
  let endDialogTarget = $state<{ key: string; title: string; working: boolean; hasActivity: boolean } | null>(null);

  let viewEl: HTMLDivElement | undefined = $state();

  // Left rail splitter state
  let draggingRail = $state(false);
  let railDragStartX = 0;
  let railDragStartWidth = 0;
  let lastRailDragEndAt = 0;

  // Right changes splitter state
  let draggingChanges = $state(false);
  let changesDragStartX = 0;
  let changesDragStartWidth = 0;
  let lastChangesDragEndAt = 0;

  function startRailResize(event: PointerEvent) {
    event.preventDefault();
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    draggingRail = true;
    railDragStartX = event.clientX;
    railDragStartWidth = maviPrefs.railWidth;
  }

  function moveRailResize(event: PointerEvent) {
    if (!draggingRail) return;
    maviPrefs.setRailWidthLive(railDragStartWidth + (event.clientX - railDragStartX));
  }

  function endRailResize(event: PointerEvent) {
    if (!draggingRail) return;
    draggingRail = false;
    lastRailDragEndAt = Date.now();
    maviPrefs.setRailWidth(maviPrefs.railWidth);
    const target = event.currentTarget as HTMLElement;
    if (target.hasPointerCapture?.(event.pointerId)) target.releasePointerCapture(event.pointerId);
  }

  function railResizeKeydown(event: KeyboardEvent) {
    switch (event.key) {
      case "ArrowLeft":
        event.preventDefault();
        maviPrefs.setRailWidth(maviPrefs.railWidth - MAVI_RESIZE_STEP);
        break;
      case "ArrowRight":
        event.preventDefault();
        maviPrefs.setRailWidth(maviPrefs.railWidth + MAVI_RESIZE_STEP);
        break;
      case "Home":
        event.preventDefault();
        maviPrefs.setRailWidth(MAVI_RAIL_WIDTH_BOUNDS.min);
        break;
      case "End":
        event.preventDefault();
        maviPrefs.setRailWidth(MAVI_RAIL_WIDTH_BOUNDS.max);
        break;
    }
  }

  function startChangesResize(event: PointerEvent) {
    event.preventDefault();
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    draggingChanges = true;
    changesDragStartX = event.clientX;
    changesDragStartWidth = maviPrefs.changesWidth;
  }

  function moveChangesResize(event: PointerEvent) {
    if (!draggingChanges) return;
    // Moving the left edge of the right column leftwards widens it.
    maviPrefs.setChangesWidthLive(changesDragStartWidth + (changesDragStartX - event.clientX));
  }

  function endChangesResize(event: PointerEvent) {
    if (!draggingChanges) return;
    draggingChanges = false;
    lastChangesDragEndAt = Date.now();
    maviPrefs.setChangesWidth(maviPrefs.changesWidth);
    const target = event.currentTarget as HTMLElement;
    if (target.hasPointerCapture?.(event.pointerId)) target.releasePointerCapture(event.pointerId);
  }

  function changesResizeKeydown(event: KeyboardEvent) {
    switch (event.key) {
      case "ArrowLeft":
        event.preventDefault();
        maviPrefs.setChangesWidth(maviPrefs.changesWidth + MAVI_RESIZE_STEP);
        break;
      case "ArrowRight":
        event.preventDefault();
        maviPrefs.setChangesWidth(maviPrefs.changesWidth - MAVI_RESIZE_STEP);
        break;
      case "Home":
        event.preventDefault();
        maviPrefs.setChangesWidth(MAVI_CHANGES_WIDTH_BOUNDS.min);
        break;
      case "End":
        event.preventDefault();
        maviPrefs.setChangesWidth(MAVI_CHANGES_WIDTH_BOUNDS.max);
        break;
    }
  }

  onMount(() => {
    maviStore.init().catch((e) => (initError = e instanceof Error ? e.message : String(e)));
    // The open thread counts as seen only while this view is on screen: the
    // window visible, and not covered by an expanded terminal dock (which
    // keeps the view mounted but inert underneath it).
    const covered = () => !!viewEl?.closest("[inert]");
    const visible = () => maviStore.setViewing(document.visibilityState === "visible" && !covered());
    visible();
    document.addEventListener("visibilitychange", visible);
    const pane = viewEl?.closest(".gp-view");
    const watch = typeof MutationObserver === "undefined" || !pane ? null : new MutationObserver(visible);
    if (pane) watch?.observe(pane, { attributes: true, attributeFilter: ["inert"] });
    return () => {
      document.removeEventListener("visibilitychange", visible);
      watch?.disconnect();
      maviStore.setViewing(false);
    };
  });

  // "New Mavi thread" from the palette, the dock, Explorer, or `&` handoff.
  let prefill = $state("");
  let preferWorktree = $state(false);
  let initialAgent = $state<"claude" | "agy" | undefined>(undefined);
  let initialModel = $state<string | undefined>(undefined);
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
    initialAgent = undefined;
    initialModel = undefined;
    pendingHandoff = null;
    composing = true;
  });

  function startFreshThread() {
    composeId += 1;
    prefill = "";
    preferWorktree = false;
    initialAgent = undefined;
    initialModel = undefined;
    pendingHandoff = null;
    composing = true;
  }

  let moving = $state(false);

  async function moveToTerminal() {
    if (!record || moving) return;
    // The thread is fixed now: the open one can change while the dialog is up.
    const key = record.key;
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
      const result = await continueInTerminal(key);
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

  function openEndDialog() {
    if (!record) return;
    const hasActivity =
      (thread?.turn ?? 0) > 0 ||
      (thread?.items.length ?? 0) > 0 ||
      hasChanges ||
      (summary?.turns ?? 0) > 0 ||
      (summary?.touched.length ?? 0) > 0;
    endDialogTarget = {
      key: record.key,
      title: record.title,
      working,
      hasActivity,
    };
    endDialogOpen = true;
  }

  async function confirmEndSession(
    key: string,
    opts: { summarize: boolean; engine: MaviSummaryEngine; model: string },
  ) {
    try {
      await maviStore.stop(key);
    } catch (e) {
      toastStore.warning(`Couldn't end the agent: ${e instanceof Error ? e.message : String(e)}`);
      return;
    }
    if (opts.summarize) {
      void maviStore.summarize(key, { engine: opts.engine, model: opts.model });
    }
  }

  function handleSlashAction(actionId: string) {
    switch (actionId) {
      case "new":
        startFreshThread();
        break;
      case "changes":
        if (changesOpen) hideChanges();
        else changesOpen = true;
        break;
      case "terminal":
        void shellHere();
        break;
      case "handoff":
        void moveToTerminal();
        break;
      case "stop":
        if (record) void maviStore.interrupt(record.key);
        break;
      case "end":
        openEndDialog();
        break;
      case "customize":
        customizeOpen = true;
        break;
    }
  }

  function handlePickAgentModel(nextAgent: "claude" | "agy", nextModel: string | null) {
    pendingHandoff = { agent: nextAgent, model: nextModel };
  }

  function confirmHandoff() {
    if (!pendingHandoff) return;
    const chosen = pendingHandoff;
    const draft = record ? (maviStore.drafts[record.key] ?? "").trim() : "";
    composeId += 1;
    prefill = draft;
    preferWorktree = false;
    initialAgent = chosen.agent;
    initialModel = chosen.model ?? "";
    pendingHandoff = null;
    composing = true;
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
  let hasChanges = $derived(thread ? hasAnyChange(thread) : false);
  let changedFileCount = $derived(thread ? collectChanges(thread, root, { kind: "thread" }).length : 0);
  let canSummarizeEnded = $derived(
    !!record &&
      !record.workSummary &&
      ((thread?.turn ?? 0) > 0 ||
        (thread?.items.length ?? 0) > 0 ||
        hasChanges ||
        (summary?.turns ?? 0) > 0 ||
        (summary?.touched.length ?? 0) > 0),
  );
  let showNew = $derived(composing || maviStore.records.length === 0 || !record);

  let laneMax = $derived(MAVI_CHAT_WIDTH_CSS[maviPrefs.chatWidth]);
  let densityCss = $derived(MAVI_DENSITY_CSS[maviPrefs.density]);

  // The changes column opens itself the first time a thread changes a file,
  // unless the reader closed it this run or turned off auto-open.
  let announced = new Set<string>();
  let hiddenByReader = false;
  $effect(() => {
    if (record && hasChanges && !announced.has(record.key)) {
      announced.add(record.key);
      if (!hiddenByReader && maviPrefs.autoOpenChanges) changesOpen = true;
    }
  });

  function hideChanges() {
    hiddenByReader = true;
    changesOpen = false;
  }

  // Which open requests are on screen. The composer's "waiting for your
  // answer" bar points only at those that are not, so it never covers or
  // repeats a card the reader can already see.
  let onScreen = $state<ReadonlySet<string>>(new Set());
  let requestIds = $derived(approvals.map((a) => a.approval?.requestId ?? "").join(","));
  $effect(() => {
    void requestIds;
    if (typeof IntersectionObserver === "undefined") return;
    const seen = new Set<string>();
    let observer: IntersectionObserver | null = null;
    let cancelled = false;
    void tick().then(() => {
      if (cancelled) return;
      observer = new IntersectionObserver((entries) => {
        for (const entry of entries) {
          const id = (entry.target as HTMLElement).dataset.request ?? "";
          if (entry.isIntersecting) seen.add(id);
          else seen.delete(id);
        }
        onScreen = new Set(seen);
      });
      for (const el of document.querySelectorAll<HTMLElement>(".mavi-approval[data-request]")) observer.observe(el);
    });
    return () => {
      cancelled = true;
      observer?.disconnect();
      onScreen = new Set();
    };
  });
  let offScreenApprovals = $derived(approvals.filter((a) => !onScreen.has(a.approval?.requestId ?? "")).length);

  $effect(() => writeFlag(RAIL_KEY, railOpen));
  $effect(() => writeFlag(CHANGES_KEY, changesOpen));

  async function open(key: string) {
    composing = false;
    pendingHandoff = null;
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

  /** Brings the first request off screen into view. Focus lands on the card, which answers nothing by itself. */
  function reviewApproval() {
    const target =
      [...document.querySelectorAll<HTMLElement>(".mavi-approval[data-request]")].find((el) => !onScreen.has(el.dataset.request ?? "")) ??
      document.querySelector<HTMLElement>(".mavi-approval");
    target?.scrollIntoView({ block: "center", behavior: "smooth" });
    target?.focus({ preventScroll: true });
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

<div
  class="mavi-view flex-1 flex min-h-0"
  class:select-none={draggingRail || draggingChanges}
  data-testid="mavi-view"
  bind:this={viewEl}
  style:--mavi-lane-max={laneMax}
  style:--mavi-turn-gap={densityCss.turnGap}
  style:--mavi-row-gap={densityCss.rowGap}
  style:--mavi-bubble-pad={densityCss.bubblePad}
>
  {#if railOpen}
    <div class="mavi-rail-col relative shrink-0" style:width="{maviPrefs.railWidth}px">
      <MaviRail onNew={startFreshThread} onOpen={open} />
      <!-- Justified: WAI-ARIA window-splitter pattern — a focusable `separator` with aria-valuenow/min/max is interactive by spec, though the linter treats every `separator` as non-interactive. -->
      <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
      <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
      <div
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize threads panel"
        tabindex="0"
        aria-valuemin={MAVI_RAIL_WIDTH_BOUNDS.min}
        aria-valuemax={MAVI_RAIL_WIDTH_BOUNDS.max}
        aria-valuenow={maviPrefs.railWidth}
        style:z-index={LAYERS.STICKY}
        class="absolute top-0 right-[-3px] h-full w-[6px] cursor-col-resize hover:bg-accent/40 transition-colors"
        title="Drag to resize · double-click to reset · ←/→ to nudge"
        onpointerdown={startRailResize}
        onpointermove={moveRailResize}
        onpointerup={endRailResize}
        onpointercancel={endRailResize}
        onkeydown={railResizeKeydown}
        ondblclick={() => {
          if (Date.now() - lastRailDragEndAt > 250) maviPrefs.setRailWidth(MAVI_RAIL_WIDTH_BOUNDS.default);
        }}
      ></div>
    </div>
  {/if}

  <section class="mavi-main flex-1 flex flex-col min-w-0 min-h-0" aria-label="Conversation">
    <header class="mavi-head flex items-center gap-2.5">
      <button
        class="gp-icon-btn shrink-0"
        aria-label={railOpen ? "Hide threads" : "Show threads"}
        title={railOpen ? "Hide threads" : "Show threads"}
        onclick={() => (railOpen = !railOpen)}
      >
        {#if railOpen}<PanelLeftClose size={14} aria-hidden="true" />{:else}<PanelLeftOpen size={14} aria-hidden="true" />{/if}
      </button>
      {#if record && !showNew}
        <div class="min-w-0 flex-1">
          <div class="flex items-center gap-2 min-w-0">
            <span class="text-xs font-medium text-textPrimary truncate">{record.title}</span>
            {#if status}
              {@const tone = statusTone(status)}
              <span class="gp-chip shrink-0" data-tone={tone === "muted" ? "neutral" : tone}>
                {STATUS_LABEL[status]}
              </span>
            {/if}
          </div>
          <div class="mavi-meta flex items-center gap-1.5 text-micro text-textMuted min-w-0 whitespace-nowrap overflow-hidden">
            {#if record.target.kind === "ssh"}<Server size={10} aria-hidden="true" />{:else}<Laptop size={10} aria-hidden="true" />{/if}
            <span class="truncate font-mono min-w-0" title={record.target.kind === "ssh" ? `${record.target.host}:${record.target.dir}` : record.target.root}>{targetLabel(record.target)}</span>
            {#if thread?.model}<span>· {thread.model}</span>{/if}
            {#if summary && summary.cost_usd > 0}<span>· ${summary.cost_usd.toFixed(2)}</span>{/if}
          </div>
        </div>
        <div class="flex items-center gap-1.5 shrink-0">
          {#if record.target.kind === "local"}
            <button class="gp-icon-btn" aria-label="Open a terminal here" title="Open a shell in this thread's worktree, in the terminal dock" onclick={shellHere}>
              <SquareTerminal size={14} aria-hidden="true" />
            </button>
            {#if !moved}
              <button
                class="gp-btn"
                data-variant="ghost"
                data-size="sm"
                disabled={!movable.ok || moving}
                aria-label="Continue in terminal"
                title={movable.ok ? "Move this conversation to a Claude Code tab in the terminal dock" : movable.reason}
                onclick={moveToTerminal}
              >
                <ArrowRightLeft size={11} aria-hidden="true" /> <span class="mavi-head-label">Continue in terminal</span>
              </button>
            {/if}
          {/if}
          {#if running}
            <button
              class="gp-btn"
              data-variant="ghost"
              data-size="sm"
              aria-label="End agent"
              title="End this thread's agent process; the conversation can be resumed"
              onclick={openEndDialog}
            >
              <Square size={11} aria-hidden="true" /> <span class="mavi-head-label">End agent</span>
            </button>
          {/if}
        </div>
      {:else}
        <div class="flex-1 text-xs font-medium text-textPrimary">Mavi</div>
      {/if}
      <div class="flex items-center gap-1.5 shrink-0">
        {#if !changesOpen && record && !showNew}
          {#if changedFileCount > 0}
            <button
              type="button"
              class="gp-btn"
              data-variant="ghost"
              data-size="sm"
              aria-label={`Show changes (${changedFileCount})`}
              title={`Show ${changedFileCount} changed ${changedFileCount === 1 ? "file" : "files"}`}
              onclick={() => (changesOpen = true)}
            >
              <PanelRightOpen size={14} aria-hidden="true" />
              <span class="gp-chip" data-kind="count" data-tone="accent">{changedFileCount}</span>
            </button>
          {:else}
            <button class="gp-icon-btn" aria-label="Show changes" title="Show changes" onclick={() => (changesOpen = true)}>
              <PanelRightOpen size={14} aria-hidden="true" />
            </button>
          {/if}
        {/if}
        <MaviCustomizePopover bind:open={customizeOpen} />
      </div>
    </header>

    {#if initError}
      <p class="gp-notice gp-notice-error text-meta" role="alert">Mavi couldn't reach its host: {initError}</p>
    {/if}

    {#if showNew}
      <div class="flex-1 overflow-y-auto flex justify-center items-start mavi-new-wrap">
        <div class="mavi-new-inner w-full flex flex-col items-center gap-6">
          {#if maviStore.records.length === 0}
            <div class="mavi-intro text-center flex flex-col items-center">
              <div class="flex h-11 w-11 items-center justify-center rounded-2xl bg-accent/10 ring-1 ring-accent/25 text-accent mb-3">
                <MessagesSquare size={20} aria-hidden="true" />
              </div>
              <h1 class="text-base font-semibold text-textPrimary">Work with an agent in a conversation</h1>
              <p class="text-xs text-textMuted mt-1.5 leading-relaxed">
                Start with Claude Code or Antigravity as a live timeline — file reads, commands, edits, and inline permission requests alongside every changed file.
              </p>
            </div>
          {:else}
            <h1 class="text-sm font-semibold text-textPrimary">Start a new thread</h1>
          {/if}
          {#key composeId}
            <MaviNewThread
              {prefill}
              {preferWorktree}
              {initialAgent}
              {initialModel}
              onOpenCustomize={() => (customizeOpen = true)}
              onStarted={(key) => open(key)}
              onCancel={record ? () => (composing = false) : undefined}
            />
          {/key}
        </div>
      </div>
    {:else if record && !thread}
      <p class="flex-1 text-xs text-textMuted text-center py-8" role="status">Loading the conversation…</p>
    {:else if record && thread}
      {#key record.key}
        <MaviTimeline
          {thread}
          {root}
          {working}
          autoScroll={maviPrefs.autoScroll}
          showThinking={maviPrefs.showThinking}
          expandTools={maviPrefs.expandTools}
          showTurnMeta={maviPrefs.showTurnMeta}
          textSize={maviPrefs.textSize}
          messageStyle={maviPrefs.messageStyle}
          workSummary={record.workSummary ?? null}
          summarizing={!!maviStore.summarizing[record.key]}
          summaryError={maviStore.summaryErrors[record.key] ?? null}
          onRegenerateSummary={() =>
            void maviStore.summarize(record.key, {
              engine: maviPrefs.summaryEngine,
              model: maviPrefs.summaryModel,
            })}
          onDismissSummary={() => maviStore.clearSummary(record.key)}
          onAnswer={(requestId, decision, message) => maviStore.answer(record.key, requestId, decision, message)}
          onShowTurn={showTurn}
          onOpenFile={showFile}
        />
      {/key}
      {#if maviStore.errors[record.key]}
        <p class="gp-notice gp-notice-error text-meta" role="alert">{maviStore.errors[record.key]}</p>
      {/if}
      <MaviChildTabs threadKey={record.key} onAsk={(text) => maviStore.send(record.key, text)} />
      {#if running}
        {#key record.key}
          <MaviComposer
            {working}
            queue={maviStore.queues[record.key] ?? []}
            pendingApprovals={offScreenApprovals}
            draft={maviStore.drafts[record.key] ?? ""}
            onDraft={(text) => maviStore.setDraft(record.key, text)}
            {modeLabel}
            sendKey={maviPrefs.sendKey}
            textSize={maviPrefs.textSize}
            repoPath={root ?? $currentRepoPath}
            touchedFiles={summary?.touched ?? []}
            {pendingHandoff}
            onConfirmHandoff={confirmHandoff}
            onDismissHandoff={() => (pendingHandoff = null)}
            onSlashAction={handleSlashAction}
            onPickAgentModel={handlePickAgentModel}
            onSend={(text) => maviStore.send(record.key, text)}
            onStop={() => maviStore.interrupt(record.key)}
            onUnqueue={(i) => maviStore.unqueue(record.key, i)}
            onReviewApproval={reviewApproval}
          />
        {/key}
      {:else if moved}
        <div class="mavi-resume-wrap">
          <div class="mavi-lane">
            <div class="mavi-resume gp-card flex flex-wrap items-center gap-3 justify-center text-xs text-textMuted">
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
          </div>
        </div>
      {:else}
        <div class="mavi-resume-wrap">
          <div class="mavi-lane">
            <div class="mavi-resume gp-card flex flex-wrap items-center gap-3 justify-center text-xs text-textMuted">
              <span>{record.conversationId ? "This thread's agent is not running." : "This thread's agent ended before it saved a conversation."}</span>
              {#if record.mode === "bypass"}
                <Checkbox
                  label="Bypass: run everything without asking"
                  checked={resumeAck}
                  onchange={(next) => (resumeAck = next)}
                />
              {/if}
              {#if canSummarizeEnded}
                <button
                  type="button"
                  class="gp-btn"
                  data-variant="secondary"
                  disabled={!!maviStore.summarizing[record.key]}
                  onclick={() =>
                    void maviStore.summarize(record.key, {
                      engine: maviPrefs.summaryEngine,
                      model: maviPrefs.summaryModel,
                    })}
                >
                  <Sparkles size={12} aria-hidden="true" /> Summarize work
                </button>
              {/if}
              <button class="gp-btn" data-variant="primary" disabled={resuming || (record.mode === "bypass" && !resumeAck)} onclick={resume}>
                {#if record.conversationId}<RotateCcw size={12} aria-hidden="true" /> Resume{:else}<Play size={12} aria-hidden="true" /> Start again{/if}
              </button>
              {#if resumeError}<span class="w-full text-center text-danger" role="alert">{resumeError}</span>{/if}
            </div>
          </div>
        </div>
      {/if}
    {/if}
  </section>

  {#if changesOpen && record && thread && !showNew}
    <div class="mavi-changes-col relative shrink-0" style:width="{maviPrefs.changesWidth}px">
      <!-- Justified: WAI-ARIA window-splitter pattern — a focusable `separator` with aria-valuenow/min/max is interactive by spec, though the linter treats every `separator` as non-interactive. -->
      <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
      <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
      <div
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize changes panel"
        tabindex="0"
        aria-valuemin={MAVI_CHANGES_WIDTH_BOUNDS.min}
        aria-valuemax={MAVI_CHANGES_WIDTH_BOUNDS.max}
        aria-valuenow={maviPrefs.changesWidth}
        style:z-index={LAYERS.STICKY}
        class="absolute top-0 left-[-3px] h-full w-[6px] cursor-col-resize hover:bg-accent/40 transition-colors"
        title="Drag to resize · double-click to reset · ←/→ to nudge"
        onpointerdown={startChangesResize}
        onpointermove={moveChangesResize}
        onpointerup={endChangesResize}
        onpointercancel={endChangesResize}
        onkeydown={changesResizeKeydown}
        ondblclick={() => {
          if (Date.now() - lastChangesDragEndAt > 250) maviPrefs.setChangesWidth(MAVI_CHANGES_WIDTH_BOUNDS.default);
        }}
      ></div>
      {#key record.key}
        <MaviChanges
          {thread}
          {root}
          sessionId={record.sessionId}
          bind:focus={focusPath}
          bind:turn={focusTurn}
          onHide={hideChanges}
        />
      {/key}
    </div>
  {/if}

  <MaviEndSessionDialog
    bind:open={endDialogOpen}
    title={endDialogTarget?.title ?? ""}
    working={endDialogTarget?.working ?? false}
    hasActivity={endDialogTarget?.hasActivity ?? false}
    onConfirm={(opts) => {
      const target = endDialogTarget;
      endDialogTarget = null;
      if (target) void confirmEndSession(target.key, opts);
    }}
    onclose={() => {
      endDialogTarget = null;
    }}
  />
</div>

<style>
  .mavi-view {
    container-type: inline-size;
  }
  .mavi-main {
    container-type: inline-size;
  }
  @container (max-width: 740px) {
    .mavi-head-label {
      display: none;
    }
  }
  .mavi-rail-col {
    max-width: 42cqi;
    min-height: 0;
  }
  .mavi-changes-col {
    max-width: 52cqi;
    min-height: 0;
    border-left: 1px solid var(--gp-edge-soft);
  }
  .mavi-head {
    padding: 0 14px 0 10px;
    height: 44px;
    min-height: 44px;
    border-bottom: 1px solid var(--gp-edge-soft);
  }
  .mavi-new-wrap {
    padding: 36px 28px 48px;
  }
  .mavi-new-inner {
    margin: auto 0;
  }
  .mavi-intro {
    max-width: 38rem;
  }
  .mavi-lane {
    max-width: var(--mavi-lane-max, 52rem);
    margin: 0 auto;
    padding: 0 20px;
  }
  .mavi-resume-wrap {
    padding: 8px 0 16px;
  }
  .mavi-resume {
    padding: 12px 16px;
  }
</style>
