<script lang="ts">
  import { onMount, tick } from "svelte";
  import { X, ArrowRight, FolderOpen, Compass, Check } from "@lucide/svelte";
  import { productTour, TOUR_STEPS, type ProductTourStore } from "../../tools/productTour";
  import { observeTourTarget, type TourRect } from "../../tools/productTourTarget";
  import { hostPlatform } from "../../stores/platformStore";
  import { repositoryAccessGuidance, shortcutTextLabel } from "../../ui/platformCopy";
  import { trapFocus } from "../../ui/focusTrap";
  import { LAYERS } from "../../ui/layers";
  import { VIEW_REGISTRY } from "../../views/viewRegistry";
  import type { ViewTab } from "../../repos/persist";
  import { isImeComposition } from "../../keyboard/imeGuard";

  let { tour = productTour, onOpenRepository, onSettings, onTools, onTasks, onView,
    repositoryPath = null, activeView = "work", repositoryVisible = true, tasksOpen = false, settingsOpen = false }: {
    tour?: ProductTourStore;
    onOpenRepository: () => void | Promise<unknown>;
    onSettings: () => void;
    onTools: () => void;
    onTasks: () => void;
    onView: (view: ViewTab) => void;
    repositoryPath?: string | null;
    activeView?: ViewTab;
    repositoryVisible?: boolean;
    tasksOpen?: boolean;
    settingsOpen?: boolean;
  } = $props();
  let heading = $state<HTMLHeadingElement | null>(null);
  let target = $state<TourRect | null>(null);
  let position = $state({ left: 16, top: 80 });
  let busy = $state(false);
  let actionError = $state<string | null>(null);
  let settingsVisited = $state(false);
  const step = $derived(TOUR_STEPS[$tour.step]);
  const live = $derived(step !== "welcome" && step !== "ready");
  const selector = $derived(step === "views" && !repositoryVisible ? "[data-tour='unavailable']" : `[data-tour='${step}']`);
  const achieved = $derived(step === "repository" ? !!repositoryPath : step === "views" ? !!repositoryPath && repositoryVisible : step === "tasks" ? tasksOpen : step === "permissions" && settingsVisited);
  onMount(() => tour.initialize());
  $effect(() => { if ($tour.open && step === "permissions" && settingsOpen) settingsVisited = true; });

  function guide(node: HTMLElement) {
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    heading?.focus();
    $effect(() => {
      if (live) return observeTourTarget(selector, node, (rect, next) => { target = rect; position = next; });
      const trap = trapFocus(node, { autofocus: false });
      return () => trap.destroy();
    });
    return { destroy() {
      // Do not steal focus from a picker, trust prompt, or other app surface.
      if (node.contains(document.activeElement) || document.activeElement === document.body) {
        const replay = document.querySelector<HTMLElement>("[data-tour='replay']");
        if (replay) replay.focus(); else if (previous?.isConnected) previous.focus();
      }
    } };
  }
  async function move(forward: boolean) {
    actionError = null;
    if (forward) tour.next(); else tour.back();
    await tick(); heading?.focus();
  }
  async function openRepository(complete = false) {
    if (busy || (complete && !tour.finish())) return;
    busy = true; actionError = null;
    try { await onOpenRepository(); }
    catch { actionError = "The repository could not be opened. Try again or skip this step."; }
    finally { busy = false; }
  }
  async function leaveForTools() {
    if (!tour.dismiss()) return;
    await tick(); onTools();
  }
  function focusTarget() {
    const element = Array.from(document.querySelectorAll<HTMLElement>(selector)).find(el => el.getClientRects().length > 0);
    const control = element?.matches("button") ? element : element?.querySelector<HTMLElement>("[aria-selected='true'], button");
    control?.focus();
  }
  function keydown(event: KeyboardEvent) {
    event.stopPropagation();
    if (event.key === "Escape" && !isImeComposition(event)) {
      event.preventDefault(); tour.dismiss();
    }
  }
</script>

{#if $tour.open}
  {#if live && target && !settingsOpen}
    <div class="tour-spotlight" aria-hidden="true" style:z-index={LAYERS.TOUR}
      style:left={`${target.left - 4}px`} style:top={`${target.top - 4}px`}
      style:width={`${target.width + 8}px`} style:height={`${target.height + 8}px`}></div>
  {/if}
  <div class:tour-modal={!live} class:tour-live={live} style:z-index={live ? LAYERS.TOUR : LAYERS.MODAL}>
    <div class="gp-card shadow-float rounded-2xl tour-card" class:tour-coach={live}
      style:left={live ? `${position.left}px` : undefined} style:top={live ? `${position.top}px` : undefined}
      role="dialog" aria-modal={!live} aria-labelledby="product-tour-title" tabindex="-1"
      onkeydown={keydown} use:guide>
      <header class="flex items-center justify-between gap-3">
        <span class="text-xs text-textMuted flex items-center gap-2"><Compass size={16} /> Walkthrough · {$tour.step + 1} of {TOUR_STEPS.length}</span>
        <button class="gp-icon-btn" aria-label="Close walkthrough and resume later" onclick={() => tour.dismiss()}><X size={16} /></button>
      </header>
      <div class="flex gap-1" aria-hidden="true">
        {#each TOUR_STEPS as _, index}<span class="h-1 flex-1 rounded-full" class:bg-accent={index <= $tour.step} class:bg-border={index > $tour.step}></span>{/each}
      </div>
      <div class="space-y-3 text-sm text-textMuted leading-relaxed">
        <h1 id="product-tour-title" bind:this={heading} tabindex="-1" class="text-xl font-semibold text-textPrimary outline-none">
          {#if step === "welcome"}Your work, in one place
          {:else if step === "repository"}Start with a repository
          {:else if step === "views"}Find your way around
          {:else if step === "tasks"}Turn an idea into a task
          {:else if step === "permissions"}Access, on your terms
          {:else}Ready when you are{/if}
        </h1>
        {#if step === "welcome"}
          <p>Take a hands-on tour of GitPulse. Open a repository, try the workspace views, and find Tasks and Settings.</p>
          <p>The guide will move beside the controls as you explore. Every step is optional; resume later from Settings → Appearance → Guided walkthrough.</p>
        {:else if step === "repository"}
          <p>{target ? "Try the highlighted Open menu to choose or clone a repository. You can also use the button below." : "Use the button below to choose a repository. The Open menu is not currently visible."} Review repository trust when asked.</p>
          <button class="gp-btn" disabled={busy} onclick={() => openRepository()}><FolderOpen size={15} /> {busy ? "Opening…" : "Choose a repository"}</button>
          {#if repositoryPath}<p role="status" class="tour-success"><Check size={15} /> Repository open. Continue when you’re ready.</p>
          {:else}<p class="text-xs">No repository open yet. Canceling the picker keeps you here; you can try again or skip.</p>{/if}
        {:else if step === "views"}
          <p>Try a view below or use the highlighted tabs. Each opens a different part of the same repository.</p>
          <div class="grid grid-cols-2 gap-2" aria-label="Try workspace views">
            {#each Object.values(VIEW_REGISTRY) as view}
              <button class="gp-btn justify-center" disabled={!repositoryPath}
                aria-pressed={!!repositoryPath && repositoryVisible && activeView === view.id}
                onclick={() => onView(view.id)}>{view.label}</button>
            {/each}
          </div>
          {#if repositoryPath && repositoryVisible}
            <p role="status"><strong class="text-textPrimary">{VIEW_REGISTRY[activeView].label}</strong> — {VIEW_REGISTRY[activeView].summary}</p>
          {:else if !repositoryPath}
            <p>Open a repository to try these views, or skip for now.</p>
            <button class="gp-btn" disabled={busy} onclick={() => openRepository()}>Choose a repository</button>
          {:else}<p>Select a view to return to your repository.</p>{/if}
          <p class="text-xs">Find commands with <kbd class="gp-keycap">{shortcutTextLabel("⌘K", $hostPlatform.os)}</kbd>.</p>
        {:else if step === "tasks"}
          <p>Open the real task board to see where ideas, repositories, and acceptance criteria come together. You can explore without creating a task.</p>
          <button class="gp-btn" onclick={onTasks}>Open Tasks</button>
          {#if tasksOpen}<p role="status" class="tour-success"><Check size={15} /> Tasks is open. Explore the board, then continue here.</p>{/if}
          <p class="text-xs">DevMap adds code intelligence; Manvi provides policy and agent workflows. Configure either when you need it.</p>
          <button class="gp-btn" onclick={leaveForTools}>Set up optional tools</button>
        {:else if step === "permissions"}
          <p>{repositoryAccessGuidance($hostPlatform.os)}</p>
          <p class="text-xs">Repository trust is a separate GitPulse decision; it does not override denied folder access. Launch at login is optional in Settings.</p>
          <button class="gp-btn" onclick={onSettings}>Open Settings</button>
          {#if settingsVisited}<p role="status" class="tour-success"><Check size={15} /> Settings opened. No permission grant is required to finish.</p>
          {:else}<p class="text-xs">Explore Settings, then close it to return to this step. The tour does not change permissions or credentials.</p>{/if}
        {:else}
          <p>You’ve reached the end of the walkthrough. Keep exploring, or replay it any time from Settings → Appearance → Guided walkthrough.</p>
          <p class="text-xs">Skipped steps stay optional. Finishing records the tour as complete; it does not grant access or confirm a repository operation.</p>
          <button class="gp-btn" onclick={() => openRepository(true)}><FolderOpen size={15} /> Finish and open a repository</button>
        {/if}
        {#if live && target}<button class="tour-focus" onclick={focusTarget}>Focus highlighted control</button>{/if}
      </div>
      {#if $tour.error || actionError}
        <div role="alert" class="text-xs text-textMuted space-y-2"><p>{$tour.error ?? actionError}</p>
          {#if $tour.error}<button class="gp-btn" onclick={() => tour.closeForSession()}>Close without saving</button>{/if}
        </div>
      {/if}
      <footer class="flex items-center justify-between gap-3 pt-3 border-t border-border">
        <button class="gp-btn" onclick={() => tour.dismiss()}>Later</button>
        <div class="flex gap-2">
          <button class="gp-btn" disabled={$tour.step === 0} onclick={() => move(false)}>Back</button>
          {#if step === "ready"}<button class="gp-btn-primary" onclick={() => tour.finish()}>Finish</button>
          {:else}<button class="gp-btn-primary" onclick={() => move(true)}>{step === "welcome" ? "Start tour" : achieved ? "Continue" : "Skip step"} <ArrowRight size={14} /></button>{/if}
        </div>
      </footer>
    </div>
  </div>
{/if}

<style>
  .tour-modal { position: fixed; inset: 0; display: flex; align-items: center; justify-content: center; padding: 16px; background: #0006; }
  .tour-live { position: fixed; inset: 0; pointer-events: none; }
  .tour-card { width: min(520px, calc(100vw - 32px)); max-height: calc(100dvh - 32px); overflow: auto; padding: 24px; display: flex; flex-direction: column; gap: 16px; pointer-events: auto; }
  .tour-coach { position: absolute; width: min(380px, calc(100vw - 32px)); padding: 20px; }
  .tour-spotlight { position: fixed; border: 2px solid var(--color-accent); border-radius: 12px; box-shadow: 0 0 0 4px color-mix(in srgb, var(--color-accent) 15%, transparent); pointer-events: none; }
  .tour-success { display: flex; align-items: flex-start; gap: 8px; color: var(--color-textPrimary); }
  .tour-success :global(svg) { flex-shrink: 0; margin-top: 3px; color: var(--color-accent); }
  .tour-focus { font-size: 12px; color: var(--color-accent); text-decoration: underline; text-underline-offset: 3px; }
  @media (prefers-reduced-motion: no-preference) { .tour-card { animation: tour-enter 150ms ease-out; } }
  @keyframes tour-enter { from { opacity: 0; transform: translateY(4px); } to { opacity: 1; transform: translateY(0); } }
</style>
