<script lang="ts">
  /**
   * Hand one saved task to a coding agent, from the board.
   *
   * The panel inside the editor could only be reached by opening a task and
   * scrolling past every other field; this is the same launch, two clicks from
   * a card. "The same" is literal: the form is `TaskHandoffForm`, shared with
   * the editor's Agent pane, so the checkout it resolves, the gate it applies,
   * the revision it re-reads and the settings it remembers cannot drift
   * between the two places a run can start.
   *
   * Nothing launches on open. The sheet shows exactly what it will run, in
   * which directory, under which permission mode, and waits.
   */
  import { onMount, untrack } from "svelte";
  import { Bot, X } from "@lucide/svelte";
  import { portal } from "../dom/portal";
  import { trapFocus } from "../ui/focusTrap";
  import { LAYERS } from "../ui/layers";
  import { shortcutTextLabel } from "../ui/platformCopy";
  import { hostPlatform } from "../stores/platformStore";
  import TaskHandoffForm from "./TaskHandoffForm.svelte";
  import type { Repository, TaskCard, TaskRun } from "../workbench/client";
  import {
    PROVIDER_LABELS,
    describeHandoff,
    reconcileHandoff,
    type HandoffGate,
    type HandoffSettings,
  } from "../workbench/taskHandoff";
  import type { OpenTabRef } from "../workbench/openMembership";

  let {
    card,
    settings: initialSettings,
    repositories,
    openTabs = [],
    onClose,
    onLaunched,
  }: {
    card: TaskCard;
    settings: HandoffSettings;
    repositories: Repository[];
    openTabs?: OpenTabRef[];
    onClose: () => void;
    onLaunched: (run: TaskRun) => void;
  } = $props();

  // One snapshot per opening: the sheet owns its own working copy, so a board
  // refresh underneath it cannot re-point a launch that is being configured.
  const opened = untrack(() => reconcileHandoff(initialSettings));
  let settings = $state<HandoffSettings>(opened);
  let gate = $state<HandoffGate>({ ok: false, reason: "" });
  let busy = $state(false);
  let pending = $state(false);
  /**
   * Set once the store has accepted a preparation.
   *
   * Everything after that point — starting managed Codex, opening a terminal —
   * can fail without the attempt ceasing to exist. Pressing launch again here
   * would build a new preparation identity and leave two runs for one
   * intention, so the sheet stops offering it and says where the attempt is.
   */
  let prepared = $state(false);
  let form = $state<{ launch: () => Promise<void> }>();
  let closeButton: HTMLButtonElement;

  onMount(() => { closeButton?.focus(); });

  function onKey(event: KeyboardEvent) {
    event.stopPropagation();
    if (event.key === "Escape" && !busy) { event.preventDefault(); onClose(); }
    if ((event.metaKey || event.ctrlKey) && event.key === "Enter") { event.preventDefault(); void form?.launch(); }
  }
</script>

<div use:portal class="backdrop gp-scrim" role="presentation" style="z-index:{LAYERS.PROMPT}">
  <div
    class="sheet gp-card gp-glass shadow-float"
    role="dialog"
    aria-modal="true"
    aria-label="Send task to an agent"
    tabindex="-1"
    onkeydown={onKey}
    use:trapFocus={{ initial: () => closeButton }}
    data-testid="task-handoff"
  >
    <header>
      <div class="min-w-0">
        <h2><Bot size={14} class="text-accent shrink-0" /> Send to agent</h2>
        <p class="title" title={card.title}>{card.title}</p>
        <p class="meta">Saved revision {card.revision} · {describeHandoff(settings)}</p>
      </div>
      <button bind:this={closeButton} type="button" class="gp-icon-btn" aria-label="Close agent handoff" disabled={busy} onclick={onClose}><X size={14} /></button>
    </header>

    <div class="body">
      <TaskHandoffForm
        bind:this={form}
        bind:settings
        taskId={card.id}
        revision={card.revision}
        repositoryIds={card.repository_ids}
        primaryRepositoryId={card.primary_repository_id}
        {repositories}
        {openTabs}
        {onLaunched}
        onGate={(next) => { gate = next; }}
        onBusy={(next) => { busy = next; }}
        onPending={(next) => { pending = next; }}
        onPrepared={() => { prepared = true; }}
      />
    </div>

    <footer>
      {#if prepared && !busy}
        <span class="meta gate">Attempt prepared. Resume or cancel it from this task's Agent tab.</span>
        <button type="button" class="gp-btn-primary" onclick={onClose}>Done</button>
      {:else}
        <span class="meta gate">{gate.ok ? `${shortcutTextLabel("⌘↩", $hostPlatform.os)} to launch` : gate.reason}</span>
        <button type="button" class="gp-btn" disabled={busy} onclick={onClose}>Cancel</button>
        <button type="button" class="gp-btn-primary" disabled={!gate.ok} onclick={() => void form?.launch()}>
          {busy ? "Preparing…" : pending ? "Retry preparation" : settings.kind === "managed" ? "Start managed Codex" : `Launch in ${PROVIDER_LABELS[settings.provider]}`}
        </button>
      {/if}
    </footer>
  </div>
</div>

<style>
  .backdrop{position:fixed;inset:0;display:grid;place-items:center;padding:20px;background:#0005;color:rgb(var(--c-text))}
  .sheet{width:min(520px,100%);max-height:calc(100dvh - 40px);display:flex;flex-direction:column;border-radius:16px;overflow:hidden}
  header{display:flex;align-items:flex-start;justify-content:space-between;gap:10px;padding:16px 20px 12px;border-bottom:1px solid rgb(var(--c-border)/.5);flex-shrink:0}
  h2{display:flex;align-items:center;gap:6px;font-size:14px;font-weight:650;margin:0}
  .title{margin:6px 0 2px;font-size:12px;font-weight:550;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  .meta{margin:0;font-size:11px;color:rgb(var(--c-text-muted));line-height:1.5}
  .body{padding:14px 20px;overflow:auto}
  footer{display:flex;align-items:center;gap:10px;padding:12px 20px 16px;border-top:1px solid rgb(var(--c-border)/.45);flex-shrink:0}
  .gate{flex:1;min-width:0}
  button:disabled{opacity:.5}
</style>
