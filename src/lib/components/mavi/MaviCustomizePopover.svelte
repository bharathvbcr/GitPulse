<script lang="ts">
  /**
   * Anchored popover for Mavi's appearance, defaults, and behavior preferences.
   * Mounts in the header on both the start page and active threads.
   */
  import { RotateCcw, SlidersHorizontal } from "@lucide/svelte";
  import { portal } from "../../dom/portal";
  import { LAYERS } from "../../ui/layers";
  import { popupEnter } from "../../ui/headerMotion";
  import { popover, popoverExit, restoreFocusTo, type PopoverOptions } from "../../ui/popover";
  import { shortcutTextLabel } from "../../ui/platformCopy";
  import { hostPlatform } from "../../stores/platformStore";
  import {
    maviPrefs,
    type MaviChatWidth,
    type MaviDefaultAgent,
    type MaviDensity,
    type MaviMessageStyle,
    type MaviSendKey,
    type MaviTextSize,
  } from "../../mavi/prefs.svelte";
  import Toggle from "../Toggle.svelte";

  const uid = $props.id();

  let { open = $bindable(false) }: { open?: boolean } = $props();
  let triggerEl: HTMLButtonElement | undefined = $state();

  const dismissal: PopoverOptions = $derived({
    anchor: { kind: "element", element: triggerEl, gap: 6, align: "end" },
    inset: 8,
    dismiss: {
      inside: `[data-mavi-customize="${uid}"]`,
      escape: "capture",
      resize: true,
    },
    onDismiss: (reason) => {
      open = false;
      if (reason === "escape") restoreFocusTo(triggerEl);
    },
  });

  const WIDTH_OPTIONS: readonly { value: MaviChatWidth; label: string }[] = [
    { value: "compact", label: "Compact" },
    { value: "standard", label: "Standard" },
    { value: "wide", label: "Wide" },
    { value: "full", label: "Full" },
  ];

  const DENSITY_OPTIONS: readonly { value: MaviDensity; label: string }[] = [
    { value: "compact", label: "Compact" },
    { value: "comfortable", label: "Comfortable" },
    { value: "spacious", label: "Spacious" },
  ];

  const TEXT_SIZE_OPTIONS: readonly { value: MaviTextSize; label: string }[] = [
    { value: "sm", label: "Small" },
    { value: "md", label: "Medium" },
    { value: "lg", label: "Large" },
  ];

  const MESSAGE_STYLE_OPTIONS: readonly { value: MaviMessageStyle; label: string }[] = [
    { value: "bubbles", label: "Bubbles" },
    { value: "editorial", label: "Editorial" },
  ];

  const DEFAULT_AGENT_OPTIONS: readonly { value: MaviDefaultAgent; label: string }[] = [
    { value: "claude", label: "Claude Code" },
    { value: "agy", label: "Antigravity" },
  ];

  let sendKeyOptions: readonly { value: MaviSendKey; label: string }[] = $derived([
    { value: "enter", label: "Enter" },
    { value: "mod_enter", label: shortcutTextLabel("⌘↵", $hostPlatform.os) },
  ]);
</script>

<div class="relative" data-mavi-customize={uid}>
  <button
    type="button"
    class="gp-icon-btn"
    bind:this={triggerEl}
    aria-expanded={open}
    aria-haspopup="dialog"
    aria-label="Customize Mavi"
    title="Customize layout and behavior"
    data-testid="mavi-customize-btn"
    onclick={() => {
      open = !open;
    }}
  >
    <SlidersHorizontal size={14} aria-hidden="true" />
  </button>

  {#if open}
    <div
      use:portal={"body"}
      use:popover={dismissal}
      in:popupEnter
      out:popoverExit
      data-mavi-customize={uid}
      data-testid="mavi-customize-popover"
      class="mavi-customize-panel fixed gp-menu w-80"
      style:z-index={LAYERS.MENU}
      role="dialog"
      aria-label="Customize Mavi"
    >
      <div class="p-2.5 flex flex-col gap-3">
        <div class="flex items-center justify-between gap-2">
          <span class="text-xs font-semibold text-textPrimary">Customize</span>
          <button
            type="button"
            class="gp-btn"
            data-variant="ghost"
            data-size="sm"
            data-testid="mavi-customize-reset"
            onclick={() => maviPrefs.reset()}
            title="Restore all Mavi layout and display defaults"
          >
            <RotateCcw size={11} aria-hidden="true" />
            <span>Reset defaults</span>
          </button>
        </div>

        <div class="flex flex-col gap-2">
          <span class="text-meta font-medium text-textMuted">Appearance</span>
          <div class="flex flex-col gap-2.5">
            <div class="flex flex-col gap-1">
              <span class="gp-label">Width</span>
              <div class="gp-segmented w-full" data-size="sm" role="group" aria-label="Conversation width">
                {#each WIDTH_OPTIONS as opt (opt.value)}
                  <button
                    type="button"
                    class="gp-seg-btn flex-1 justify-center"
                    data-active={maviPrefs.chatWidth === opt.value}
                    aria-pressed={maviPrefs.chatWidth === opt.value}
                    onclick={() => (maviPrefs.chatWidth = opt.value)}
                  >
                    {opt.label}
                  </button>
                {/each}
              </div>
            </div>

            <div class="flex flex-col gap-1">
              <span class="gp-label">Density</span>
              <div class="gp-segmented w-full" data-size="sm" role="group" aria-label="Conversation density">
                {#each DENSITY_OPTIONS as opt (opt.value)}
                  <button
                    type="button"
                    class="gp-seg-btn flex-1 justify-center"
                    data-active={maviPrefs.density === opt.value}
                    aria-pressed={maviPrefs.density === opt.value}
                    onclick={() => (maviPrefs.density = opt.value)}
                  >
                    {opt.label}
                  </button>
                {/each}
              </div>
            </div>

            <div class="flex flex-col gap-1">
              <span class="gp-label">Text size</span>
              <div class="gp-segmented w-full" data-size="sm" role="group" aria-label="Text size">
                {#each TEXT_SIZE_OPTIONS as opt (opt.value)}
                  <button
                    type="button"
                    class="gp-seg-btn flex-1 justify-center"
                    data-active={maviPrefs.textSize === opt.value}
                    aria-pressed={maviPrefs.textSize === opt.value}
                    onclick={() => (maviPrefs.textSize = opt.value)}
                  >
                    {opt.label}
                  </button>
                {/each}
              </div>
            </div>

            <div class="flex items-center justify-between gap-3">
              <span class="gp-label">Message style</span>
              <div class="gp-segmented" data-size="sm" role="group" aria-label="Message style">
                {#each MESSAGE_STYLE_OPTIONS as opt (opt.value)}
                  <button
                    type="button"
                    class="gp-seg-btn justify-center"
                    data-active={maviPrefs.messageStyle === opt.value}
                    aria-pressed={maviPrefs.messageStyle === opt.value}
                    onclick={() => (maviPrefs.messageStyle = opt.value)}
                  >
                    {opt.label}
                  </button>
                {/each}
              </div>
            </div>
          </div>
        </div>

        <div class="flex flex-col gap-2">
          <span class="text-meta font-medium text-textMuted">Defaults &amp; Behavior</span>
          <div class="flex flex-col gap-2.5">
            <div class="flex items-center justify-between gap-3">
              <span class="gp-label">Default agent</span>
              <div class="gp-segmented" data-size="sm" role="group" aria-label="Default agent">
                {#each DEFAULT_AGENT_OPTIONS as opt (opt.value)}
                  <button
                    type="button"
                    class="gp-seg-btn justify-center"
                    data-active={maviPrefs.defaultAgent === opt.value}
                    aria-pressed={maviPrefs.defaultAgent === opt.value}
                    onclick={() => (maviPrefs.defaultAgent = opt.value)}
                  >
                    {opt.label}
                  </button>
                {/each}
              </div>
            </div>

            <div class="flex items-center justify-between gap-3">
              <span class="gp-label">Send key</span>
              <div class="gp-segmented" data-size="sm" role="group" aria-label="Send key">
                {#each sendKeyOptions as opt (opt.value)}
                  <button
                    type="button"
                    class="gp-seg-btn justify-center"
                    data-active={maviPrefs.sendKey === opt.value}
                    aria-pressed={maviPrefs.sendKey === opt.value}
                    onclick={() => (maviPrefs.sendKey = opt.value)}
                  >
                    {opt.label}
                  </button>
                {/each}
              </div>
            </div>
          </div>

          <div class="rounded-xl bg-background/50 px-2.5 py-1.5 flex flex-col">
            <Toggle
              label="Auto-scroll while streaming"
              checked={maviPrefs.autoScroll}
              onchange={(next) => (maviPrefs.autoScroll = next)}
            />
            <Toggle
              label="Auto-open changes on edit"
              checked={maviPrefs.autoOpenChanges}
              onchange={(next) => (maviPrefs.autoOpenChanges = next)}
            />
            <Toggle
              label="Show thinking blocks"
              checked={maviPrefs.showThinking}
              onchange={(next) => (maviPrefs.showThinking = next)}
            />
            <Toggle
              label="Expand tool runs by default"
              checked={maviPrefs.expandTools}
              onchange={(next) => (maviPrefs.expandTools = next)}
            />
            <Toggle
              label="Show turn duration & cost"
              checked={maviPrefs.showTurnMeta}
              onchange={(next) => (maviPrefs.showTurnMeta = next)}
            />
            <Toggle
              label="Summarize work on end"
              checked={maviPrefs.summarizeOnEnd}
              onchange={(next) => (maviPrefs.summarizeOnEnd = next)}
            />
          </div>
        </div>
      </div>
    </div>
  {/if}
</div>

<style>
  .mavi-customize-panel {
    max-height: min(82vh, 42rem);
    overflow-y: auto;
  }
</style>
