<script lang="ts">
  import { tick } from "svelte";
  import {
    tipGuideOf,
    tipTextOf,
    TOOLTIP_ANCHOR_SELECTOR,
    tooltipAnchorFromTarget,
  } from "../dom/tipText";
  import { destinationGuide, type DestinationGuide } from "../views/viewGuide";
  import { LAYERS } from "../ui/layers";
  import ViewGuideCard from "./ViewGuideCard.svelte";

  /**
   * Global styled tooltip — mounted once, upgrades every `title=` in the app
   * and every `data-tip-guide` destination card.
   *
   * Rather than migrating dozens of call sites to a custom attribute, this
   * intercepts hovers/focuses on anything carrying a `title`, moves the text
   * into a `data-tip-text` attribute (which suppresses the OS-native bubble)
   * and renders it as a themed pill instead. Migration mirrors the title
   * into `aria-label` for icon-only controls, so stripping `title` never
   * erases an accessible name (see dom/tipText.ts). Svelte re-applying
   * `title` on a re-render is harmless: the next hover migrates it again.
   *
   * View and section tabs skip `title` entirely and carry `data-tip-guide`
   * instead: a one-line "Work (F10)" does not tell a new user what the pane
   * is. Those resolve through the view catalog into ViewGuideCard.
   *
   * Shows after a short delay, follows neither mouse nor scroll (scroll hides),
   * flips above the anchor near the viewport bottom, and clamps horizontally.
   * Canvas targets do not inherit a titled ancestor — the commit graph owns
   * GraphNodeTooltip, and a gutter layout hint must not replace it.
   * The entrance reuses the shared gp-pop keyframe (disabled under
   * prefers-reduced-motion).
   */

  const SHOW_DELAY_MS = 350;
  const FOCUS_DELAY_MS = 120;

  let visible = $state(false);
  let placed = $state(false);
  let text = $state("");
  let guide = $state<DestinationGuide | null>(null);
  let left = $state(0);
  let top = $state(0);
  let bubble: HTMLDivElement | undefined = $state();

  let activeEl: HTMLElement | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;

  /** The tooltip text for an element, migrating a native `title` if present. */
  function anchorOf(target: EventTarget | null): HTMLElement | null {
    const el = tooltipAnchorFromTarget(target);
    return el instanceof HTMLElement ? el : null;
  }

  function cancelPending() {
    if (timer !== undefined) {
      clearTimeout(timer);
      timer = undefined;
    }
  }

  function hide() {
    cancelPending();
    activeEl = null;
    visible = false;
    placed = false;
    guide = null;
    text = "";
  }

  function reveal() {
    if (!activeEl || !bubble) return;
    const rect = activeEl.getBoundingClientRect();
    const box = bubble.getBoundingClientRect();
    let nextLeft = rect.left + rect.width / 2 - box.width / 2;
    nextLeft = Math.min(Math.max(8, nextLeft), window.innerWidth - box.width - 8);
    let nextTop = rect.bottom + 8;
    if (rect.bottom + box.height + 12 > window.innerHeight && rect.top - box.height - 8 > 0) {
      nextTop = rect.top - box.height - 8;
    }
    left = Math.max(8, nextLeft);
    top = Math.max(8, nextTop);
    placed = true;
  }

  function scheduleShow(el: HTMLElement, delayMs: number) {
    hide();
    activeEl = el;
    const key = tipGuideOf(el);
    guide = key ? destinationGuide(key) : null;
    text = guide ? "" : tipTextOf(el);
    if (!guide && !text.trim()) return;
    timer = setTimeout(() => {
      visible = true;
      placed = false;
      void tick().then(reveal);
    }, delayMs);
  }

  // Re-measure whenever visibility flips: covers the case where the bubble
  // ref wasn't bound yet when the timer fired.
  $effect(() => {
    if (visible) void tick().then(reveal);
  });

  function onMouseOver(event: MouseEvent) {
    const el = anchorOf(event.target);
    if (!el || el === activeEl) return;
    scheduleShow(el, SHOW_DELAY_MS);
  }

  function onMouseOut(event: MouseEvent) {
    if (!activeEl) return;
    const related = event.relatedTarget;
    if (related instanceof Element && related.closest(TOOLTIP_ANCHOR_SELECTOR) === activeEl) {
      return;
    }
    hide();
  }

  function onFocusIn(event: FocusEvent) {
    const el = anchorOf(event.target);
    if (!el || el === activeEl) return;
    scheduleShow(el, FOCUS_DELAY_MS);
  }

  function onFocusOut() {
    hide();
  }

  function onDismiss() {
    hide();
  }

  function onScrollCapture() {
    // Any scroll anywhere (including nested lists) invalidates the anchor.
    if (visible || timer !== undefined) hide();
  }

  // Capture-phase scroll listener: scrolling inside any container moves the
  // anchor without firing pointer events.
  $effect(() => {
    window.addEventListener("scroll", onScrollCapture, true);
    return () => {
      window.removeEventListener("scroll", onScrollCapture, true);
      cancelPending();
    };
  });
</script>

<svelte:window
  onmouseover={onMouseOver}
  onmouseout={onMouseOut}
  onfocusin={onFocusIn}
  onfocusout={onFocusOut}
  onmousedown={onDismiss}
/>

{#if visible}
  <div
    bind:this={bubble}
    role="tooltip"
    class="gp-pop pointer-events-none fixed border border-border/80 bg-surface text-[11px] leading-snug text-textPrimary shadow-pop {guide
      ? 'w-72 overflow-hidden rounded-xl p-0'
      : 'max-w-xs whitespace-pre-line rounded-lg px-2.5 py-1.5'} {placed
      ? 'opacity-100'
      : 'opacity-0'}"
    style="left: {left}px; top: {top}px; z-index: {LAYERS.TOOLTIP}"
  >
    {#if guide}
      <ViewGuideCard {guide} />
    {:else}
      {text}
    {/if}
  </div>
{/if}
