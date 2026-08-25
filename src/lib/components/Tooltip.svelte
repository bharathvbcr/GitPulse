<script lang="ts">
  import { tick } from "svelte";

  /**
   * Global styled tooltip — mounted once, upgrades every `title=` in the app.
   *
   * Rather than migrating dozens of call sites to a custom attribute, this
   * intercepts hovers/focuses on anything carrying a `title`, moves the text
   * into a `data-tip-text` attribute (which suppresses the OS-native bubble),
   * and renders it as a themed pill instead. Svelte re-applying `title` on a
   * re-render is harmless: the next hover migrates it again.
   *
   * Shows after a short delay, follows neither mouse nor scroll (scroll hides),
   * flips above the anchor near the viewport bottom, and clamps horizontally.
   * The entrance reuses the shared gp-pop keyframe (disabled under
   * prefers-reduced-motion).
   */

  const SHOW_DELAY_MS = 350;
  const FOCUS_DELAY_MS = 120;

  let visible = $state(false);
  let placed = $state(false);
  let text = $state("");
  let left = $state(0);
  let top = $state(0);
  let bubble: HTMLDivElement | undefined = $state();

  let activeEl: HTMLElement | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;

  /** The tooltip text for an element, migrating a native `title` if present. */
  function tipTextOf(el: HTMLElement): string {
    const migrated = el.getAttribute("data-tip-text");
    if (migrated !== null) return migrated;
    const native = el.getAttribute("title");
    if (native === null) return "";
    el.setAttribute("data-tip-text", native);
    el.removeAttribute("title");
    return native;
  }

  function anchorOf(target: EventTarget | null): HTMLElement | null {
    if (!(target instanceof Element)) return null;
    const el = target.closest<HTMLElement>("[title], [data-tip-text]");
    if (!el) return null;
    return tipTextOf(el).trim().length > 0 ? el : null;
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
    text = tipTextOf(el);
    if (!text.trim()) return;
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
    if (related instanceof Element && related.closest("[title], [data-tip-text]") === activeEl) {
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
    class="gp-pop pointer-events-none fixed z-[60] max-w-xs whitespace-pre-line rounded-lg border border-border/80 bg-surface px-2.5 py-1.5 text-[11px] leading-snug text-textPrimary shadow-pop {placed
      ? 'opacity-100'
      : 'opacity-0'}"
    style="left: {left}px; top: {top}px"
  >
    {text}
  </div>
{/if}
