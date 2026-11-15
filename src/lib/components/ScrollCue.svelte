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
    NO_CUE_YIELD,
    observeCueYield,
    observeOverflow,
    overflowFadeToken,
    scrollOverflowBy,
    setOverflowFade,
    type CueYield,
    type OverflowAxis,
    type OverflowHint,
  } from "../dom/overflowHint";
  import { fade } from "svelte/transition";
  import { prefersReducedMotion } from "../motion/easing";
  import { backdropFade, backdropFadeOut } from "../ui/transitions";

  let {
    target,
    axis = "y",
    hint,
    diffuse = false,
    endLabel,
    onEnd,
  }: {
    target: HTMLElement | undefined;
    axis?: OverflowAxis;
    hint?: OverflowHint;
    /**
     * Opt in to the header-style edge: a progressive blur over the opacity
     * mask on each edge that hides content, and chevrons that show only
     * while the pointer is over the scroller's parent. Off for everyone else.
     */
    diffuse?: boolean;
    /**
     * Vertical only: names what lies past the end edge ("Worktrees"). The end
     * chevron becomes a labelled pill, so a reader learns there is a section
     * below rather than only that the list overflows.
     */
    endLabel?: string;
    /** Replaces the page nudge on the end cue (e.g. scroll that section into view). */
    onEnd?: () => void;
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

  // A chevron that would land on an enclosing scroller's chevron for the same
  // edge gives way to it (see cueYield); the fade below is unaffected.
  let yielded: CueYield = $state(NO_CUE_YIELD);
  $effect(() => {
    const el = target;
    if (!el) {
      yielded = NO_CUE_YIELD;
      return;
    }
    // Compared against a plain local, not `yielded`: the first answer is
    // delivered synchronously, and reading the state here would make this
    // effect depend on its own output.
    let last = NO_CUE_YIELD;
    yielded = NO_CUE_YIELD;
    return observeCueYield(el, axis, (next) => {
      if (next.start === last.start && next.end === last.end) return;
      last = next;
      yielded = next;
    });
  });

  // An edge whose chevron gave way has nothing drawn in its band, so it need
  // not be cleared: the sidebar's fade reads these and only ramps there,
  // instead of hiding a band of rows under a chevron that is not drawn.
  $effect(() => {
    const el = target;
    if (!el) return;
    el.style.setProperty("--gp-cue-clear-start", yielded.start ? "0px" : "");
    el.style.setProperty("--gp-cue-clear-end", yielded.end ? "0px" : "");
    return () => {
      el.style.removeProperty("--gp-cue-clear-start");
      el.style.removeProperty("--gp-cue-clear-end");
    };
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

  // The overlay ignores the pointer, so hover is read off the `relative`
  // parent that holds both the scroller and the chevrons: leaving the
  // scroller for a chevron must not hide it.
  $effect(() => {
    const host = diffuse ? target?.parentElement : null;
    if (!host) return;
    const enter = () => host.setAttribute("data-gp-hover", "");
    const leave = () => host.removeAttribute("data-gp-hover");
    host.addEventListener("pointerenter", enter);
    host.addEventListener("pointerleave", leave);
    return () => {
      host.removeEventListener("pointerenter", enter);
      host.removeEventListener("pointerleave", leave);
      leave();
    };
  });

  function nudge(toward: "start" | "end") {
    if (!target) return;
    scrollOverflowBy(target, axis, toward, {
      behavior: prefersReducedMotion() ? "auto" : "smooth",
    });
  }
</script>

{#if target && resolved.canScroll}
  {#if diffuse && resolved.showStart}
    {@render diffuseEdge("start")}
  {/if}
  {#if diffuse && resolved.showEnd}
    {@render diffuseEdge("end")}
  {/if}
  {#if resolved.showStart && !yielded.start}
    {@render cue("start")}
  {/if}
  {#if resolved.showEnd && !yielded.end}
    {@render cue("end")}
  {/if}
{/if}

{#snippet diffuseEdge(toward: "start" | "end")}
  {@const edge = axis === "x" ? (toward === "start" ? "left" : "right") : (toward === "start" ? "top" : "bottom")}
  <div class="gp-scroll-diffuse" data-edge={edge} aria-hidden="true">
    <div
      class="gp-scroll-diffuse-strip"
      data-strength="light"
      in:fade={backdropFade()}
      out:fade={backdropFadeOut()}
    ></div>
    <div
      class="gp-scroll-diffuse-strip"
      data-strength="heavy"
      in:fade={backdropFade()}
      out:fade={backdropFadeOut()}
    ></div>
  </div>
{/snippet}

{#snippet cue(toward: "start" | "end")}
  {@const horizontal = axis === "x"}
  {@const StartIcon = horizontal ? ChevronLeft : ChevronUp}
  {@const EndIcon = horizontal ? ChevronRight : ChevronDown}
  {@const Icon = toward === "start" ? StartIcon : EndIcon}
  {@const named = !horizontal && toward === "end" && !!endLabel}
  {@const label = toward === "start"
    ? (horizontal ? "Scroll left" : "Scroll up")
    : (horizontal ? "Scroll right" : named ? `Scroll down to ${endLabel}` : "Scroll down")}
  <div
    class={horizontal
      ? `gp-scroll-cue ${toward === "start" ? "gp-scroll-cue-start" : "gp-scroll-cue-end"}`
      : `gp-scroll-cue-y ${toward === "start" ? "gp-scroll-cue-top" : "gp-scroll-cue-bottom"}`}
  >
    <button
      type="button"
      tabindex="-1"
      class={named ? "gp-scroll-cue-pill" : "gp-scroll-cue-btn"}
      data-hover-only={diffuse ? "" : undefined}
      aria-label={label}
      title={label}
      onpointerdown={(e) => {
        e.preventDefault();
        e.stopPropagation();
      }}
      onclick={(e) => {
        e.preventDefault();
        e.stopPropagation();
        if (toward === "end" && onEnd) onEnd();
        else nudge(toward);
      }}
    >
      {#if named}<span class="truncate">{endLabel}</span>{/if}
      <Icon size={12} strokeWidth={2.5} class="shrink-0" />
    </button>
  </div>
{/snippet}
