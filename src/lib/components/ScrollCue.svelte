<script lang="ts">
  /**
   * Edge chevrons for a scroller whose overflow is otherwise silent.
   *
   * Overlay, not a wrapper: the existing scroller keeps its event handlers,
   * roles and overflow classes, and the cue sits on a `relative` parent so
   * it does not scroll away with the content. The overlay paints nothing but
   * the chevron; the fade is the scroller masking its own edge (the
   * `data-gp-fade` token), so it works over glass, surface and base alike.
   * A supplied `hint` (the graph gutter's layout math) wins over DOM
   * measurement; everyone else is observed.
   */
  import { ChevronDown, ChevronLeft, ChevronRight, ChevronUp } from "@lucide/svelte";
  import {
    EMPTY_OVERFLOW_HINT,
    observeOverflow,
    overflowFadeToken,
    scrollOverflowBy,
    setOverflowFade,
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

  // The token follows the hint; clearing is tied to the element alone, so a
  // retarget or unmount never leaves a mask behind and a hint change is one
  // attribute write, not a delete-then-set.
  $effect(() => {
    const el = target;
    if (!el) return;
    return () => setOverflowFade(el, null);
  });

  $effect(() => {
    const el = target;
    if (el) setOverflowFade(el, overflowFadeToken(axis, resolved));
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
      ? `gp-scroll-cue ${toward === "start" ? "gp-scroll-cue-start" : "gp-scroll-cue-end"}`
      : `gp-scroll-cue-y ${toward === "start" ? "gp-scroll-cue-top" : "gp-scroll-cue-bottom"}`}
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
