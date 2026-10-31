<script lang="ts">
  import { Image, LoaderCircle } from "@lucide/svelte";
  import EmptyState from "./EmptyState.svelte";

  /**
   * The two sides are read separately and can arrive, be absent, or fail
   * independently, so the pane states which of those it is showing instead of
   * folding them into one blank:
   * - `loading`: the blobs are still being read.
   * - `failed`: neither side could be read; `error` says why and Retry asks again.
   * - `ready`: the reads settled. A side with no picture is absent, and
   *   `oldError` / `newError` carry the reason when that side's read threw —
   *   the pane cannot tell a deleted file from an unreadable one, so it never
   *   presents a failed read as Added or Deleted without saying so.
   */
  let {
    filePath = "image",
    oldSrc = null,
    newSrc = null,
    status = "ready",
    error = null,
    oldError = null,
    newError = null,
    onRetry,
  }: {
    filePath?: string;
    oldSrc?: string | null;
    newSrc?: string | null;
    status?: "loading" | "ready" | "failed";
    error?: string | null;
    oldError?: string | null;
    newError?: string | null;
    onRetry?: () => void;
  } = $props();

  const OLD_ABSENT = "No previous version (added)";
  const NEW_ABSENT = "No current version (deleted)";

  let mode: "2up" | "swipe" | "onion" = $state("swipe");
  let swipePosition = $state(50);
  let opacity = $state(50);
</script>

<div class="flex-1 flex flex-col bg-background h-full text-xs font-sans select-none overflow-hidden">
  <div class="px-3 py-1.5 border-b border-border/60 gp-section-edge bg-surface/60 flex items-center justify-between">
    <div class="flex items-center gap-2 min-w-0">
      <Image size={15} class="text-accent shrink-0" />
      <span class="font-medium text-textPrimary truncate">{filePath}</span>
    </div>
    <div class="gp-segmented" role="group" aria-label="Image comparison mode">
      <button
        type="button"
        onclick={() => (mode = "2up")}
        aria-pressed={mode === "2up"}
        data-active={mode === "2up" ? "true" : "false"}
        class="gp-seg-btn py-0.5! text-[11px]!"
      >2-Up</button>
      <button
        type="button"
        onclick={() => (mode = "swipe")}
        aria-pressed={mode === "swipe"}
        data-active={mode === "swipe" ? "true" : "false"}
        class="gp-seg-btn py-0.5! text-[11px]!"
      >Swipe</button>
      <button
        type="button"
        onclick={() => (mode = "onion")}
        aria-pressed={mode === "onion"}
        data-active={mode === "onion" ? "true" : "false"}
        class="gp-seg-btn py-0.5! text-[11px]!"
      >Onion Skin</button>
    </div>
  </div>

  <div
    class="flex-1 flex items-center justify-center p-8 bg-background relative overflow-hidden"
    aria-busy={status === "loading"}
  >
    {#if status === "loading"}
      <div class="flex items-center gap-2 text-textMuted" role="status">
        <LoaderCircle size={18} class="animate-spin" />
        <span>Loading images…</span>
      </div>
    {:else if status === "failed"}
      <EmptyState
        icon={Image}
        title="Could not read this image"
        hint={error ?? "The image could not be read."}
        action={onRetry ? { label: "Retry", onClick: onRetry, variant: "secondary" } : undefined}
      />
    {:else if !oldSrc && !newSrc}
      <EmptyState icon={Image} title="No image on either side" hint="This change has no image data to compare." />
    {:else if mode === "2up"}
      <div class="flex gap-8 items-center">
        <div class="flex flex-col items-center gap-2">
          <span class="text-textMuted text-[11px]">Before (Old)</span>
          <div class="w-64 h-64 bg-surface border border-border/70 rounded-2xl shadow-card flex items-center justify-center overflow-hidden">
            {#if oldSrc}
              <img src={oldSrc} alt={`Before version of ${filePath}`} class="max-w-full max-h-full object-contain" />
            {:else}
              <span class="text-textMuted text-xs">{OLD_ABSENT}</span>
            {/if}
          </div>
          {#if !oldSrc && oldError}{@render readError(oldError)}{/if}
        </div>
        <div class="flex flex-col items-center gap-2">
          <span class="text-textMuted text-[11px]">After (New)</span>
          <div class="w-64 h-64 bg-surface border border-accent/40 rounded-2xl shadow-card flex items-center justify-center overflow-hidden">
            {#if newSrc}
              <img src={newSrc} alt={`After version of ${filePath}`} class="max-w-full max-h-full object-contain" />
            {:else}
              <span class="text-textMuted text-xs">{NEW_ABSENT}</span>
            {/if}
          </div>
          {#if !newSrc && newError}{@render readError(newError)}{/if}
        </div>
      </div>
    {:else if mode === "swipe"}
      <div class="flex flex-col items-center gap-4 w-md">
        <div class="w-80 h-80 bg-surface border border-border/70 rounded-2xl shadow-card relative overflow-hidden">
          {#if newSrc}
            <img src={newSrc} alt={`After version of ${filePath}`} class="absolute inset-0 w-full h-full object-contain" />
          {/if}
          <div class="absolute inset-0 overflow-hidden border-r-2 border-accent" style="width: {swipePosition}%;">
            {#if oldSrc}
              <img src={oldSrc} alt={`Before version of ${filePath}`} class="absolute inset-0 w-80 h-80 object-contain" />
            {/if}
          </div>
        </div>
        {@render absentNotes()}
        <input
          type="range"
          min="0"
          max="100"
          bind:value={swipePosition}
          aria-label="Swipe comparison divider"
          class="w-full accent-accent"
        />
      </div>
    {:else}
      <div class="flex flex-col items-center gap-4 w-md">
        <div class="w-80 h-80 bg-surface border border-border/70 rounded-2xl shadow-card relative overflow-hidden">
          {#if oldSrc}
            <img src={oldSrc} alt={`Before version of ${filePath}`} class="absolute inset-0 w-full h-full object-contain" />
          {/if}
          {#if newSrc}
            <img src={newSrc} alt={`After version of ${filePath}`} class="absolute inset-0 w-full h-full object-contain" style="opacity: {opacity / 100};" />
          {/if}
        </div>
        {@render absentNotes()}
        <input
          type="range"
          min="0"
          max="100"
          bind:value={opacity}
          aria-label="After-image opacity"
          class="w-full accent-accent"
        />
      </div>
    {/if}
  </div>
</div>

{#snippet readError(message: string)}
  <span class="max-w-64 text-center text-[10px] text-textMuted/80">could not be read: {message}</span>
{/snippet}

<!-- Swipe and onion draw whatever sides exist on one stage, so a side with no
     picture would otherwise look like the other one standing in for both. -->
{#snippet absentNotes()}
  {#if !oldSrc}
    <div class="flex flex-col items-center gap-0.5 text-textMuted text-[11px]">
      <span>{OLD_ABSENT}</span>
      {#if oldError}{@render readError(oldError)}{/if}
    </div>
  {/if}
  {#if !newSrc}
    <div class="flex flex-col items-center gap-0.5 text-textMuted text-[11px]">
      <span>{NEW_ABSENT}</span>
      {#if newError}{@render readError(newError)}{/if}
    </div>
  {/if}
{/snippet}
