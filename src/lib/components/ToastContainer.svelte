<script lang="ts">
  import { toastStore, type ToastItem, type ToastKind } from "../stores/toastStore";
  import { CheckCircle2, Info, AlertTriangle, AlertCircle, X } from "@lucide/svelte";
  import { fly, fade } from "svelte/transition";
  import { LAYERS } from "../ui/layers";
  import { focusStayedInside, notificationPile } from "../ui/notificationPile";

  const KIND_CONFIG: Record<
    ToastKind,
    { icon: typeof CheckCircle2; border: string; text: string; iconColor: string }
  > = {
    success: {
      icon: CheckCircle2,
      border: "border-emerald-500/40",
      text: "text-emerald-700 dark:text-emerald-300",
      iconColor: "text-emerald-500",
    },
    info: {
      icon: Info,
      border: "border-accent/40",
      text: "text-textPrimary",
      iconColor: "text-accent",
    },
    warning: {
      icon: AlertTriangle,
      border: "border-amber-500/40",
      text: "text-amber-700 dark:text-amber-300",
      iconColor: "text-amber-500",
    },
    error: {
      icon: AlertCircle,
      border: "border-rose-500/40",
      text: "text-rose-700 dark:text-rose-300",
      iconColor: "text-rose-500",
    },
  };

  async function handleAction(toast: ToastItem) {
    if (!toast.action) return;
    try {
      await toast.action.onClick();
    } finally {
      toastStore.dismiss(toast.id);
    }
  }

  let expanded = $state(false);
  let pile = $state<HTMLDivElement | undefined>();
  const toasts = $derived($toastStore);
  const stack = $derived(notificationPile(toasts.length, expanded));
  // slice(-0) is slice(0) and would return every toast. An empty store stays empty.
  const shown = $derived(toasts.length === 0 ? [] : toasts.slice(-stack.shown));
  const lips = $derived(Array.from({ length: stack.peeks }, (_, index) => stack.peeks - index));

  function engage() {
    expanded = true;
    toastStore.pauseAll();
  }

  function release(event: MouseEvent | FocusEvent) {
    const active = document.activeElement;
    if (event.type === "focusout" && focusStayedInside(pile, event.relatedTarget)) return;
    if (event.type === "mouseleave" && focusStayedInside(pile, active)) return;
    expanded = false;
    toastStore.resumeAll();
  }
</script>

<!--
  The live region is the CONTAINER, not each toast.

  Every toast carried its own `aria-live` and was inserted with it, so the
  region and its content arrived together — which most screen readers do not
  announce, because a live region has to exist before content lands in it to be
  watched. This wrapper is in the DOM from first paint.

  Two regions rather than one: errors are assertive (they interrupt, because
  they are the ones that stop the user's work) and everything else is polite.
  A single region cannot be both, and switching the politeness of a live region
  at runtime is unreliable.
-->
<div
  class="fixed bottom-4 right-4 z-50 flex flex-col gap-2 max-w-sm w-full pointer-events-none select-none"
  style="z-index: {LAYERS.MODAL};"
  role="region"
  aria-label="Notifications"
>
  <div class="sr-only" role="alert" aria-live="assertive" aria-atomic="false">
    {#each $toastStore.filter((t) => t.kind === "error") as toast (toast.id)}
      <p>{toast.message}</p>
    {/each}
  </div>
  <div class="sr-only" role="status" aria-live="polite" aria-atomic="false">
    {#each $toastStore.filter((t) => t.kind !== "error") as toast (toast.id)}
      <p>{toast.message}</p>
    {/each}
  </div>

  {#if toasts.length > 0}
    <!-- Newest toast in front. Lips above it are only edges. Hover or focus
         opens every toast so dismiss and actions stay reachable, and freezes
         the countdown while the pointer or focus is here. The container is
         pointer-events-none, so this pile is what can see that. Cards stay
         aria-hidden because the live regions above already announced them. -->
    <div
      class="pile pointer-events-auto"
      class:open={stack.peeks === 0}
      role="group"
      aria-hidden="true"
      data-testid="toast-pile"
      bind:this={pile}
      onmouseenter={engage}
      onmouseleave={release}
      onfocusin={engage}
      onfocusout={release}
    >
      {#each lips as depth (depth)}
        <div class="peek gp-card" style:--depth={depth} aria-hidden="true"></div>
      {/each}
      {#each shown as toast (toast.id)}
        {@const config = KIND_CONFIG[toast.kind]}
        {@const Icon = config.icon}
        <div
          aria-hidden="true"
          role="presentation"
          in:fly={{ y: 12, duration: 160 }}
          out:fade={{ duration: 120 }}
          class="toast pointer-events-auto gp-pop gp-card rounded-2xl p-3 border shadow-float flex items-start gap-2.5 bg-surface {config.border}"
        >
          <div class="shrink-0 mt-0.5 {config.iconColor}">
            <Icon size={16} />
          </div>

          <div class="flex-1 min-w-0">
            <p class="text-xs font-medium leading-snug {config.text} wrap-break-word">
              {toast.message}
            </p>

            {#if toast.action}
              <div class="mt-2">
                <button
                  type="button"
                  onclick={() => handleAction(toast)}
                  class="gp-btn py-0.5! px-2.5! text-[11px] font-semibold hover:border-accent/60"
                >
                  {toast.action.label}
                </button>
              </div>
            {/if}
          </div>

          <button
            type="button"
            onclick={() => toastStore.dismiss(toast.id)}
            aria-label="Dismiss notification"
            class="shrink-0 p-1 rounded-full text-textMuted hover:text-textPrimary hover:bg-surfaceHover transition-colors"
          >
            <X size={13} />
          </button>
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  /* Lips sit above the card and tuck under it by about its padding, so the message stays clear. */
  .pile{position:relative;display:flex;flex-direction:column;gap:8px}
  .pile:not(.open){padding-top:16px}
  .toast{position:relative;z-index:1}
  .peek{position:absolute;z-index:0;height:16px;pointer-events:none;left:calc(var(--depth) * 10px);right:calc(var(--depth) * 10px);top:calc((2 - var(--depth)) * 8px)}
</style>
