<script lang="ts">
  /**
   * Edge chevrons for a scroller whose overflow is otherwise silent.
   *
   * Overlay, not a wrapper: the existing scroller keeps its event handlers,
   * roles and overflow classes, and the cue sits on a `relative` parent so
   * it does not scroll away with the content. A supplied `hint` (the graph
   * gutter's layout math) wins over DOM measurement; everyone else is
   * observed.
   */
  import { ChevronDown, ChevronLeft, ChevronRight, ChevronUp } from "@lucide/svelte";
  import {
    EMPTY_OVERFLOW_HINT,
    observeOverflow,
    scrollOverflowBy,
    type OverflowAxis,
    type OverflowHint,
  } from "../dom/overflowHint";
  import { prefersReducedMotion } from "../motion/easing";

  let {
    target,
    axis = "y",
    hint,
  }: {
    target: HTMLElement | undefined;
    axis?: OverflowAxis;
    hint?: OverflowHint;
  } = $props();

  let measured: OverflowHint = $state(EMPTY_OVERFLOW_HINT);
  let observe = $derived(hint === undefined);
  let resolved = $derived(hint ?? measured);

  $effect(() => {
    if (!observe) return;
    const el = target;
    if (!el) {
      measured = EMPTY_OVERFLOW_HINT;
      return;
    }
    return observeOverflow(el, axis, (next) => {
      measured = next;
    });
  });

  function nudge(toward: "start" | "end") {
    if (!target) return;
    scrollOverflowBy(target, axis, toward, {
      behavior: prefersReducedMotion() ? "auto" : "smooth",
    });
  }
</script>

{#if target && resolved.canScroll}
  {#if resolved.showStart}
    {@render cue("start")}
  {/if}
  {#if resolved.showEnd}
    {@render cue("end")}
  {/if}
{/if}

{#snippet cue(toward: "start" | "end")}
  {@const horizontal = axis === "x"}
  {@const StartIcon = horizontal ? ChevronLeft : ChevronUp}
  {@const EndIcon = horizontal ? ChevronRight : ChevronDown}
  {@const Icon = toward === "start" ? StartIcon : EndIcon}
  {@const label = toward === "start"
    ? (horizontal ? "Scroll left" : "Scroll up")
    : (horizontal ? "Scroll right" : "Scroll down")}
  <div
    class={horizontal
      ? `gp-edge-fade ${toward === "start" ? "gp-edge-fade-start" : "gp-edge-fade-end"}`
      : `gp-edge-fade-y ${toward === "start" ? "gp-edge-fade-top" : "gp-edge-fade-bottom"}`}
  >
    <button
      type="button"
      tabindex="-1"
      class="gp-scroll-cue-btn"
      aria-label={label}
      title={label}
      onpointerdown={(e) => {
        e.preventDefault();
        e.stopPropagation();
      }}
      onclick={(e) => {
        e.preventDefault();
        e.stopPropagation();
        nudge(toward);
      }}
    >
      <Icon size={12} strokeWidth={2.5} />
    </button>
  </div>
{/snippet}
