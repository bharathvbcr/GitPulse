<script lang="ts">
  import { interfaceStore } from "../stores/interfaceStore";
  import { Sparkles, X } from "@lucide/svelte";
  import { fade, scale } from "svelte/transition";
  import { hostPlatform } from "../stores/platformStore";
  import { shortcutTextLabel } from "../ui/platformCopy";

  let {
    id,
    title,
    description,
    shortcut,
    class: extraClass = "",
  }: {
    id: string;
    title: string;
    description: string;
    shortcut?: string;
    class?: string;
  } = $props();

  // Callers author chords in macOS notation, as every other shortcut table in
  // the app does, and the translation happens once here rather than at each
  // call site. Both fields are mapped: the chord sits in the keycap AND is
  // usually named again in the prose ("Press ⌘K anytime to…").
  const shownShortcut = $derived(shortcut ? shortcutTextLabel(shortcut, $hostPlatform.os) : shortcut);
  const shownDescription = $derived(shortcutTextLabel(description, $hostPlatform.os));

  let isSeen = $derived(Boolean($interfaceStore.seenCoachMarks?.[id]));

  function dismiss() {
    interfaceStore.dismissCoachMark(id);
  }
</script>

{#if !isSeen}
  <div
    class="gp-pop gp-card rounded-2xl p-3 border border-accent/40 bg-surface shadow-float max-w-xs z-30 select-none {extraClass}"
    in:scale={{ start: 0.95, duration: 180 }}
    out:fade={{ duration: 120 }}
    role="tooltip"
    aria-label={title}
  >
    <div class="flex items-start gap-2.5">
      <div class="p-1.5 rounded-full bg-accent/15 text-accent shrink-0 mt-0.5 animate-pulse">
        <Sparkles size={13} />
      </div>

      <div class="flex-1 min-w-0">
        <div class="flex items-center justify-between gap-1 mb-1">
          <h4 class="text-xs font-bold text-textPrimary leading-tight">{title}</h4>
          <button
            type="button"
            onclick={dismiss}
            aria-label="Dismiss hint"
            class="p-0.5 text-textMuted hover:text-textPrimary rounded-full transition-colors"
          >
            <X size={12} />
          </button>
        </div>

        <p class="text-[11px] text-textMuted leading-relaxed">
          {shownDescription}
        </p>

        <div class="mt-2.5 flex items-center justify-between gap-2">
          {#if shortcut}
            <kbd class="gp-keycap text-[10px]">{shownShortcut}</kbd>
          {:else}
            <span></span>
          {/if}

          <button
            type="button"
            onclick={dismiss}
            class="gp-btn-primary py-0.5! px-2.5! text-[10px]! font-semibold!"
          >
            Got it
          </button>
        </div>
      </div>
    </div>
  </div>
{/if}
