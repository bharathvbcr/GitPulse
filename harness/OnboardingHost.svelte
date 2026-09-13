<script module lang="ts">
  import type { ViewTab } from "../src/lib/repos/persist";
  export interface TourWorkspace {
    path: string | null;
    view: ViewTab;
    tasks: boolean;
    settings: boolean;
    hideTargets: boolean;
  }
</script>
<script lang="ts">
  import type { Writable } from "svelte/store";
  import ProductTour from "../src/lib/components/onboarding/ProductTour.svelte";
  import HeaderRepoMenu from "../src/lib/components/HeaderRepoMenu.svelte";
  import Logo from "../src/lib/components/Logo.svelte";
  import { VIEW_REGISTRY } from "../src/lib/views/viewRegistry";
  import type { ProductTourStore } from "../src/lib/tools/productTour";
  import { trapFocus } from "../src/lib/ui/focusTrap";
  let { tour, workspace, openRepository, openSettings, openTools }: {
    tour: ProductTourStore;
    workspace: Writable<TourWorkspace>;
    openRepository: () => Promise<void>;
    openSettings: () => void;
    openTools: () => void;
  } = $props();
  function view(id: ViewTab) { workspace.update(s => ({ ...s, view: id, tasks: false })); }
  function tasks() { workspace.update(s => ({ ...s, tasks: true })); }
</script>
<div class="gp-shell bg-background text-textPrimary font-sans min-h-screen">
  <header class="bg-surface border-b border-border flex items-center gap-5 px-5 h-14">
    <Logo size={24} variant="badge" /><strong>GitPulse</strong>
    <div class:hidden={$workspace.hideTargets}><HeaderRepoMenu onOpen={() => void openRepository()} onClone={() => void openRepository()} /></div>
    {#if $workspace.path && !$workspace.hideTargets}
      <div data-tour="views" class="flex gap-2" role="tablist" aria-label="Workspace views">
        {#each Object.values(VIEW_REGISTRY) as entry}
          <button role="tab" aria-selected={$workspace.view === entry.id} tabindex={$workspace.view === entry.id ? 0 : -1}
            class="gp-btn" onclick={() => view(entry.id)}>{entry.label}</button>
        {/each}
      </div>
    {/if}
    <button id="replay" data-tour="replay" class="gp-btn ml-auto" onclick={() => tour.open()}>Walkthrough</button>
  </header>
  <div class="flex items-center gap-3 p-3 border-b border-border">
    {#if !$workspace.hideTargets}<button data-tour="tasks" class="gp-btn" onclick={tasks}>Tasks</button>{/if}
    <span class="text-xs text-textMuted">Interactive walkthrough fixture · no repository operations</span>
  </div>
  <main class="p-12">
    <h2 class="text-2xl font-semibold">{$workspace.tasks ? "Tasks" : $workspace.path ? VIEW_REGISTRY[$workspace.view].label : "Welcome to GitPulse"}</h2>
    <p class="mt-3 max-w-md text-textMuted">{$workspace.tasks ? "Capture ideas, connect repositories, and review work." : $workspace.path ? VIEW_REGISTRY[$workspace.view].summary : "Open a repository to explore the workspace. The walkthrough uses the same production component as the desktop app."}</p>
  </main>
  <ProductTour {tour} onOpenRepository={openRepository} onSettings={openSettings} onTools={openTools}
    onTasks={tasks} onView={view} repositoryPath={$workspace.path} activeView={$workspace.view}
    repositoryVisible={!$workspace.tasks} tasksOpen={$workspace.tasks} settingsOpen={$workspace.settings} />
  {#if $workspace.settings}
    <div class="fixed inset-0 bg-black/40 grid place-items-center" style:z-index={50}>
      <div role="dialog" aria-modal="true" aria-label="Settings fixture" class="gp-card rounded-xl p-8" use:trapFocus>
        <h2>Settings</h2><p class="my-4">Settings interaction fixture. No system preferences are changed.</p>
        <button class="gp-btn" onclick={() => workspace.update(s => ({ ...s, settings: false }))}>Close Settings</button>
      </div>
    </div>
  {/if}
</div>
