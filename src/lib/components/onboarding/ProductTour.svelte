<script lang="ts">
  import { onMount, tick } from "svelte";
  import { X, ArrowRight, FolderOpen, Compass } from "@lucide/svelte";
  import { productTour, TOUR_STEPS, type ProductTourStore } from "../../tools/productTour";
  import { hostPlatform } from "../../stores/platformStore";
  import { repositoryAccessGuidance, shortcutTextLabel } from "../../ui/platformCopy";
  import { trapFocus } from "../../ui/focusTrap";
  import { LAYERS } from "../../ui/layers";
  import { VIEW_REGISTRY } from "../../views/viewRegistry";
  import { isImeComposition } from "../../keyboard/imeGuard";

  let { tour = productTour, onOpenRepository, onSettings, onTools }: {
    tour?: ProductTourStore;
    onOpenRepository: () => void;
    onSettings: () => void;
    onTools: () => void;
  } = $props();
  let heading = $state<HTMLHeadingElement | null>(null);
  const step = $derived(TOUR_STEPS[$tour.step]);
  onMount(() => tour.initialize());

  async function move(forward: boolean) {
    if (forward) tour.next(); else tour.back();
    await tick();
    heading?.focus();
  }
  async function leave(action: () => void, complete = false) {
    if (!(complete ? tour.finish() : tour.dismiss())) return;
    await tick();
    action();
  }
  function keydown(event: KeyboardEvent) {
    // Keep app shortcuts out of the walkthrough; Tab is owned by trapFocus.
    event.stopPropagation();
    if (event.key === "Escape" && !isImeComposition(event)) {
      event.preventDefault();
      tour.dismiss();
    }
  }
</script>

{#if $tour.open}
  <div class="gp-scrim bg-black/40 flex items-center justify-center p-4" style:z-index={LAYERS.MODAL}>
    <div class="gp-card shadow-float rounded-2xl w-full max-w-xl max-h-[calc(100dvh-2rem)] overflow-auto p-6 flex flex-col gap-5"
      role="dialog" aria-modal="true" aria-labelledby="product-tour-title" tabindex="-1"
      onkeydown={keydown} use:trapFocus={{ initial: () => heading }}>
      <header class="flex items-center justify-between gap-3">
        <span class="text-xs text-textMuted flex items-center gap-2"><Compass size={16} /> GitPulse walkthrough · {$tour.step + 1} of {TOUR_STEPS.length}</span>
        <button class="gp-icon-btn" aria-label="Close walkthrough and resume later" onclick={() => tour.dismiss()}><X size={16} /></button>
      </header>
      <div class="flex gap-1" aria-hidden="true">
        {#each TOUR_STEPS as _, index}<span class="h-1 flex-1 rounded-full" class:bg-accent={index <= $tour.step} class:bg-border={index > $tour.step}></span>{/each}
      </div>
      <div class="space-y-4 text-sm text-textMuted leading-relaxed">
        <h1 id="product-tour-title" bind:this={heading} tabindex="-1" class="text-xl font-semibold text-textPrimary outline-none">
          {#if step === "welcome"}Your work, in one place
          {:else if step === "repository"}Start with a repository
          {:else if step === "views"}Find your way around
          {:else if step === "tasks"}Turn an idea into a task
          {:else if step === "permissions"}Access, on your terms
          {:else}Ready when you are{/if}
        </h1>
        {#if step === "welcome"}
          <p>Review changes, follow branches, and keep track of work across your repositories. This short tour introduces the workspace and explains the access GitPulse uses.</p>
          <p>You can leave at any time and resume with the Walkthrough button in the title bar.</p>
        {:else if step === "repository"}
          <p>Use Open Repository to choose an existing Git folder, or Clone Repo on the welcome screen to download one. GitPulse asks you to review repository trust before enabling operations.</p>
          <p>Repository tabs keep projects close at hand. Opening a repository does not commit or push your changes.</p>
          <button class="gp-btn" onclick={() => leave(onOpenRepository)}><FolderOpen size={15} /> Choose a repository</button>
        {:else if step === "views"}
          <dl class="space-y-3">
            {#each [VIEW_REGISTRY.work, VIEW_REGISTRY.code, VIEW_REGISTRY.history, VIEW_REGISTRY.insights] as view}
              <div><dt class="font-semibold text-textPrimary">{view.label}</dt><dd>{view.summary}</dd></div>
            {/each}
          </dl>
          <p>Press <kbd class="gp-keycap">{shortcutTextLabel("⌘K", $hostPlatform.os)}</kbd> to find commands, branches, files, and commits.</p>
        {:else if step === "tasks"}
          <p>Open Tasks from the workspace controls to capture an idea, connect its repository, and record what done means. Review changes and run results before accepting agent work.</p>
          <p>DevMap adds code intelligence. Manvi provides policy and agent workflows. Both have an existing setup guide and can be configured later.</p>
          <button class="gp-btn" onclick={() => leave(onTools)}>Set up optional tools</button>
        {:else if step === "permissions"}
          <p>Folder access follows your operating system’s permissions. Repository trust is a separate GitPulse decision; it does not override denied folder access.</p>
          <p>{repositoryAccessGuidance($hostPlatform.os)}</p>
          <p>Launch at login is optional in Settings. Remote Git operations and AI providers use their separately configured credentials. The tour does not enable them.</p>
          <button class="gp-btn" onclick={() => leave(onSettings)}>Open Settings</button>
        {:else}
          <p>Choose a repository to begin, or explore the workspace. Return to this walkthrough from the title bar whenever you need it.</p>
          <button class="gp-btn" onclick={() => leave(onOpenRepository, true)}><FolderOpen size={15} /> Finish and open a repository</button>
        {/if}
      </div>
      {#if $tour.error}
        <div role="alert" class="text-xs text-textMuted space-y-2"><p>{$tour.error}</p><button class="gp-btn" onclick={() => tour.closeForSession()}>Close without saving</button></div>
      {/if}
      <footer class="flex items-center justify-between gap-3 pt-3 border-t border-border">
        <button class="gp-btn" onclick={() => tour.dismiss()}>Later</button>
        <div class="flex gap-2">
          <button class="gp-btn" disabled={$tour.step === 0} onclick={() => move(false)}>Back</button>
          {#if step === "ready"}<button class="gp-btn-primary" onclick={() => tour.finish()}>Finish</button>
          {:else}<button class="gp-btn-primary" onclick={() => move(true)}>Next <ArrowRight size={14} /></button>{/if}
        </div>
      </footer>
    </div>
  </div>
{/if}
