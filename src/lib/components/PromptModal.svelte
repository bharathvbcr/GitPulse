<script lang="ts">
  import { fade, scale } from "svelte/transition";
  import { fadeParams, scaleParams } from "../motion/easing";
  import {
    cancelPrompt,
    completePrompt,
    promptState,
  } from "../stores/modalStore";

  let inputEl: HTMLInputElement | undefined = $state();
  let confirmEl: HTMLButtonElement | undefined = $state();
  let value = $state("");
  let previousFocus: Element | null = null;

  let pending = $derived($promptState);
  let options = $derived(pending?.options ?? null);
  let isConfirm = $derived(options?.mode === "confirm");

  $effect(() => {
    const current = pending;
    if (!current) {
      if (previousFocus instanceof HTMLElement) previousFocus.focus();
      previousFocus = null;
      return;
    }
    value = current.options.mode === "text" ? current.options.initialValue ?? "" : "";
    // Focus lands after the DOM update; the prior holder is restored on close.
    previousFocus = document.activeElement;
    if (current.options.mode === "text") {
      inputEl?.focus();
      inputEl?.select();
    } else {
      confirmEl?.focus();
    }
  });

  function submit(event?: SubmitEvent) {
    event?.preventDefault();
    completePrompt(isConfirm ? true : value);
  }

  function handleKeydown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      cancelPrompt();
    }
  }
</script>

{#if pending && options}
  <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
  <div
    role="dialog"
    aria-modal="true"
    aria-label={options.title}
    tabindex="-1"
    onclick={cancelPrompt}
    onkeydown={handleKeydown}
    transition:fade={fadeParams()}
    class="fixed inset-0 bg-black/40 backdrop-blur-sm z-[60] flex items-center justify-center p-4 select-none gp-gpu"
  >
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      onclick={(e) => e.stopPropagation()}
      in:scale={scaleParams()}
      class="w-full max-w-md gp-card shadow-float rounded-2xl overflow-hidden flex flex-col font-sans text-xs gp-gpu"
    >
      <div class="p-4 border-b border-border/60 flex items-center justify-between">
        <span class="text-sm font-semibold text-textPrimary">{options.title}</span>
      </div>

      <form class="p-4 space-y-3" onsubmit={submit}>
        {#if options.message}
          <p class="text-textMuted leading-relaxed whitespace-pre-wrap break-words">{options.message}</p>
        {/if}

        {#if !isConfirm}
          <input
            bind:this={inputEl}
            bind:value
            type="text"
            placeholder={options.mode === "text" ? options.placeholder ?? "" : ""}
            class="gp-field w-full"
          />
        {/if}

        <div class="flex justify-end gap-2 pt-1">
          <button type="button" onclick={cancelPrompt} class="gp-btn">
            {options.cancelLabel ?? "Cancel"}
          </button>
          <button bind:this={confirmEl} type="submit" class="gp-btn-primary">
            {options.confirmLabel ?? "OK"}
          </button>
        </div>
      </form>
    </div>
  </div>
{/if}
