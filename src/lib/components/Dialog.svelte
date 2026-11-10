<script lang="ts" module>
  import type { TransitionConfig } from "svelte/transition";
  import { motionDuration, type MediaMatch } from "../motion/easing";
  import { cardScale, cardScaleOut, CARD_IN_MS, CARD_OUT_MS } from "../ui/transitions";
  import { isDialogEscape } from "../keyboard/dialogEscape";
  import { enumerateFocusables, type FocusableProbe } from "../ui/focusTrap";

  export type DialogSize = "md" | "lg" | "sheet";
  export type SheetEdge = "left" | "right";

  /**
   * Whether this key closes a dialog. Escape closes it, except the Escape that
   * belongs to an IME composition, and only when the dialog may be dismissed.
   * A dialog guarding unsaved or in-flight work passes `dismissable={false}`
   * and keeps its own Cancel button as the only way out.
   */
  export function closesOnKey(event: { key: string; isComposing?: boolean; keyCode?: number }, dismissable: boolean): boolean {
    return dismissable && isDialogEscape(event);
  }

  type Region<E extends FocusableProbe> = { querySelectorAll(selectors: string): ArrayLike<E> };

  /**
   * Where focus lands on open. The header's close button is the first
   * focusable in the card, and a dialog that opens with Close focused makes
   * Enter dismiss it, so look in the body, then the footer, and fall back to
   * the card itself.
   */
  export function preferredFocus<E extends FocusableProbe, C extends { querySelector(selectors: string): Region<E> | null }>(
    card: C,
  ): E | C {
    for (const part of ["[data-dialog-body]", "[data-dialog-footer]"]) {
      const region = card.querySelector(part);
      const first = region ? enumerateFocusables(region)[0] : undefined;
      if (first) return first;
    }
    return card;
  }

  /** `translateX` percentage of a sheet that is `progress` (0 hidden, 1 shown) of the way in. */
  export function sheetOffset(progress: number, edge: SheetEdge): number {
    const hidden = 1 - Math.min(1, Math.max(0, progress));
    return hidden === 0 ? 0 : (edge === "left" ? -1 : 1) * hidden * 100;
  }

  /**
   * The slide of a `sheet`: a transform from the anchored edge, on the same
   * duration and curve the dialog card uses (spatial in, exit out), so a sheet
   * and a centered card feel like one family. The eased progress is clamped to
   * 1 because the spatial spring overshoots, and an overshoot would pull the
   * panel off the edge it is anchored to and show the page behind it. The
   * outro fades as it slides, so the shorter exit reads as leaving.
   */
  export function sheetSlide(
    _node: Element,
    params: { edge: SheetEdge; outro?: boolean; media?: MediaMatch | null },
  ): TransitionConfig {
    const { edge, outro = false, media } = params;
    const curve = outro ? cardScaleOut(media) : cardScale(media);
    return {
      duration: motionDuration(outro ? CARD_OUT_MS : CARD_IN_MS, media),
      easing: curve.easing,
      css: (t) => {
        const offset = sheetOffset(t, edge);
        return outro
          ? `transform: translateX(${offset}%); opacity: ${Math.min(1, Math.max(0, t))};`
          : `transform: translateX(${offset}%);`;
      },
    };
  }
</script>

<script lang="ts">
  import type { Snippet } from "svelte";
  import { fade, scale } from "svelte/transition";
  import { X } from "@lucide/svelte";
  import { backdropFade, backdropFadeOut } from "../ui/transitions";
  import { trapFocus } from "../ui/focusTrap";
  import { LAYERS } from "../ui/layers";

  /**
   * The one dialog and sheet shell (design language section 9): a scrim, a
   * card, a title, a body and an optional footer. Callers supply content; the
   * shell owns the stacking, the focus trap, Escape, the scrim click, the
   * enter and exit transitions and the reduced-motion handling, so a dialog
   * can no longer ship without an outro.
   *
   * `size`: `md` is 480 wide, `lg` 720, and `sheet` is a full-height panel
   * anchored to `edge` that slides in with a transform. Focus moves into the
   * card on open and returns to the element that held it when the dialog
   * closes (the focus trap records it on mount). In WebKit a mouse click does
   * not focus a button, so a dialog opened by click returns focus to nothing
   * in particular; it is exact for keyboard opens.
   *
   * `open` is bindable and `onclose` fires after the shell sets it false. Set
   * `dismissable={false}` to disable Escape, the scrim click and the close
   * button while the dialog guards work in flight.
   */
  let {
    open = $bindable(false),
    title,
    children,
    footer,
    size = "md",
    edge = "right",
    dismissable = true,
    onclose,
    initialFocus,
  }: {
    open?: boolean;
    /** Plain text, or a snippet for a title with an icon. */
    title: string | Snippet;
    /** The body. */
    children: Snippet;
    /** Buttons row; omitted when the dialog has no actions of its own. */
    footer?: Snippet;
    size?: DialogSize;
    /** Which edge a `sheet` is anchored to. Ignored for `md` and `lg`. */
    edge?: SheetEdge;
    dismissable?: boolean;
    onclose?: () => void;
    /** Preferred element to focus on open; the first focusable otherwise. */
    initialFocus?: () => HTMLElement | null;
  } = $props();

  const uid = $props.id();
  const titleId = `dialog-title-${uid}`;
  const sheet = $derived(size === "sheet");

  function close() {
    if (!dismissable) return;
    open = false;
    onclose?.();
  }

  function onKeydown(event: KeyboardEvent) {
    if (!closesOnKey(event, dismissable)) return;
    event.preventDefault();
    event.stopPropagation();
    close();
  }

  // A press that starts inside the card and ends on the scrim (a text
  // selection dragged past the edge) is not a click on the scrim.
  let pressedOnScrim = false;
  function onScrimPointerDown(event: PointerEvent) {
    pressedOnScrim = event.target === event.currentTarget;
  }
  function onScrimClick(event: MouseEvent) {
    const hit = pressedOnScrim && event.target === event.currentTarget;
    pressedOnScrim = false;
    if (hit) close();
  }

  function trapped(card: HTMLElement) {
    return trapFocus(card, {
      initial: () => {
        const requested = initialFocus ? initialFocus() : null;
        if (requested) return requested;
        const preferred = preferredFocus(card);
        return preferred instanceof HTMLElement ? preferred : card;
      },
    });
  }

  const sheetRadius = $derived(
    edge === "left"
      ? "0 var(--gp-r-xl) var(--gp-r-xl) 0"
      : "var(--gp-r-xl) 0 0 var(--gp-r-xl)",
  );
</script>

{#if open}
  <!-- The card's transitions are `|global`: Svelte transitions are local by
       default, and the card sits in a nested `{#if sheet}` block, so a local
       outro would be skipped when `open` (the outer block) goes false and the
       card would vanish with the scrim's fade as its only exit. -->
  <!-- Justified: the scrim is a pointer-only dismiss target. Keyboard users
       close the dialog with Escape on the card (role="dialog", which owns the
       key handler), so a key handler here would be a second, unreachable path. -->
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div
    role="presentation"
    data-dialog-scrim
    onpointerdown={onScrimPointerDown}
    onclick={onScrimClick}
    in:fade={backdropFade()}
    out:fade={backdropFadeOut()}
    class="gp-scrim gp-gpu flex select-none bg-black/50 {sheet
      ? edge === 'left'
        ? 'justify-start'
        : 'justify-end'
      : 'items-center justify-center p-4'}"
    style="z-index: {LAYERS.MODAL};"
  >
    {#if sheet}
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabindex="-1"
        data-dialog-size={size}
        use:trapped
        onkeydown={onKeydown}
        in:sheetSlide|global={{ edge }}
        out:sheetSlide|global={{ edge, outro: true }}
        class="gp-card gp-gpu flex h-full w-full max-w-120 select-text flex-col overflow-hidden shadow-float"
        style="border-radius: {sheetRadius}; --gp-r-outer: var(--gp-r-xl);"
      >
        {@render shell()}
      </div>
    {:else}
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabindex="-1"
        data-dialog-size={size}
        use:trapped
        onkeydown={onKeydown}
        in:scale|global={cardScale()}
        out:scale|global={cardScaleOut()}
        class="gp-card gp-gpu flex max-h-[85vh] w-full select-text flex-col overflow-hidden shadow-float {size === 'lg'
          ? 'max-w-180'
          : 'max-w-120'}"
        style="border-radius: var(--gp-r-xl); --gp-r-outer: var(--gp-r-xl);"
      >
        {@render shell()}
      </div>
    {/if}
  </div>
{/if}

{#snippet shell()}
  <div class="flex shrink-0 items-start gap-3 px-5 pt-5 pb-3">
    <h2 id={titleId} class="m-0 min-w-0 flex-1 text-lg font-semibold text-textPrimary">
      {#if typeof title === "string"}{title}{:else}{@render title()}{/if}
    </h2>
    {#if dismissable}
      <button type="button" class="gp-icon-btn -mr-1.5 -mt-1 shrink-0" data-size="md" aria-label="Close" onclick={close}>
        <X size={16} />
      </button>
    {/if}
  </div>
  <div data-dialog-body class="min-h-0 flex-1 overflow-y-auto px-5 py-2 text-xs text-textPrimary">
    {@render children()}
  </div>
  {#if footer}
    <div data-dialog-footer class="flex shrink-0 items-center justify-end gap-2 px-5 pt-3 pb-5">
      {@render footer()}
    </div>
  {/if}
{/snippet}
