<script lang="ts">
  import { interfaceStore } from "../stores/interfaceStore";
  import { Sparkles, X } from "@lucide/svelte";
  import { fade, scale } from "svelte/transition";
  import { LAYERS } from "../ui/layers";
  import { backdropFadeOut, cardScale } from "../ui/transitions";
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
    class="gp-pop gp-card fixed rounded-2xl p-3 border border-accent/35 bg-surface shadow-float max-w-xs select-none {extraClass}"
    style="z-index: {LAYERS.TOUR}"
    in:scale={cardScale()}
    out:fade={backdropFadeOut()}
    role="tooltip"
    aria-label={title}
  >
    <div class="flex items-start gap-2.5">
      <div class="p-1.5 rounded-full bg-accent/15 text-accent shrink-0 mt-0.5 animate-pulse">
        <Sparkles size={13} />
      </div>

      <div class="flex-1 min-w-0">
        <div class="flex items-center justify-between gap-1 mb-1">
          <h4 class="text-xs font-semibold text-textPrimary leading-tight">{title}</h4>
          <button
            type="button"
            onclick={dismiss}
            aria-label="Dismiss hint"
            class="p-0.5 text-textMuted hover:text-textPrimary rounded-full transition-colors"
          >
            <X size={12} />
          </button>
        </div>

        <p class="text-meta text-textMuted leading-relaxed">
          {shownDescription}
        </p>

        <div class="mt-2.5 flex items-center justify-between gap-2">
          {#if shortcut}
            <kbd class="gp-keycap text-micro">{shownShortcut}</kbd>
          {:else}
            <span></span>
          {/if}

          <button
            type="button"
            onclick={dismiss}
            class="gp-btn-primary py-0.5! px-2.5! text-micro! font-semibold!"
          >
            Got it
          </button>
        </div>
      </div>
    </div>
  </div>
{/if}
