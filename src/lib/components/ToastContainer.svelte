<script lang="ts">
  import { toastStore, type ToastItem, type ToastKind } from "../stores/toastStore";
  import { CheckCircle2, Info, AlertTriangle, AlertCircle, X } from "lucide-svelte";
  import { fly, fade } from "svelte/transition";
  import { LAYERS } from "../ui/layers";

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

  {#each $toastStore as toast (toast.id)}
    {@const config = KIND_CONFIG[toast.kind]}
    {@const Icon = config.icon}
    <!-- Countdowns freeze while the pointer or focus is on a toast: an "Undo"
         that expires while you reach for it is not an offer. The container is
         pointer-events-none, so the card is the element that can see this.
         `aria-hidden` because the announcement is made by the live regions
         above — without it every toast is read twice. -->
    <div
      aria-hidden="true"
      role="presentation"
      onmouseenter={() => toastStore.pauseAll()}
      onmouseleave={() => toastStore.resumeAll()}
      onfocusin={() => toastStore.pauseAll()}
      onfocusout={() => toastStore.resumeAll()}
      in:fly={{ y: 12, duration: 160 }}
      out:fade={{ duration: 120 }}
      class="pointer-events-auto gp-pop gp-card rounded-2xl p-3 border shadow-float flex items-start gap-2.5 bg-surface {config.border}"
    >
      <div class="shrink-0 mt-0.5 {config.iconColor}">
        <Icon size={16} />
      </div>

      <div class="flex-1 min-w-0">
        <p class="text-xs font-medium leading-snug {config.text} break-words">
          {toast.message}
        </p>

        {#if toast.action}
          <div class="mt-2">
            <button
              type="button"
              onclick={() => handleAction(toast)}
              class="gp-btn !py-0.5 !px-2.5 text-[11px] font-semibold hover:border-accent/60"
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
