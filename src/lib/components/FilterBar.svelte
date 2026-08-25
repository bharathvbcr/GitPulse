<script lang="ts">
  import { onMount } from "svelte";
  import { filterStore } from "../stores/filterStore";
  import { Search, X } from "lucide-svelte";

  let inputEl: HTMLInputElement | undefined = $state();

  function focusFilter() {
    inputEl?.focus();
    inputEl?.select();
  }

  function handleKeyDown(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "f") {
      e.preventDefault();
      focusFilter();
    }
  }

  onMount(() => {
    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("gitpulse:focus-filter", focusFilter);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("gitpulse:focus-filter", focusFilter);
    };
  });
</script>

<div class="h-10 bg-surface/60 border-b border-border/60 px-3 flex items-center gap-3 text-xs select-none">
  <div
    class="flex-1 max-w-xl flex items-center gap-2 bg-background border border-border/80 rounded-full px-3 py-1.5 transition-colors duration-150 focus-within:border-accent/60 focus-within:shadow-[var(--ring-focus)]"
  >
    <Search size={14} class="text-textMuted shrink-0" />
    <input
      bind:this={inputEl}
      id="gitpulse-filter"
      type="text"
      placeholder="Search commits, authors, paths (e.g. author:alice, feat:)..."
      value={$filterStore.searchQuery}
      oninput={(e) => filterStore.setSearch((e.target as HTMLInputElement).value)}
      class="w-full bg-transparent text-textPrimary placeholder:text-textMuted/60 text-xs focus:outline-none"
    />
    {#if $filterStore.searchQuery}
      <button onclick={() => filterStore.setSearch("")} aria-label="Clear search" title="Clear search" class="gp-icon-btn !p-0.5">
        <X size={13} />
      </button>
    {/if}
  </div>

  {#if $filterStore.selectedBranch}
    <button
      type="button"
      onclick={() => filterStore.selectBranch(null)}
      class="gp-chip bg-accent/15 text-accent border-accent/40 hover:bg-accent/25 transition-colors"
      title="Clear branch filter"
    >
      <span class="truncate max-w-[10rem]">{$filterStore.selectedBranch}</span>
      <X size={11} />
    </button>
  {/if}
</div>
