<script lang="ts">
  /**
   * The one empty-state shape: soft bubble card, tinted icon medallion, title,
   * optional hint, and optional action call-to-action button. Every view's
   * "nothing here yet" renders through this so the quiet screens speak the same
   * language as the busy ones.
   */
  export interface EmptyStateAction {
    label: string;
    onClick: () => void;
    icon?: any;
    variant?: "primary" | "secondary";
  }

  let {
    icon,
    title,
    hint = "",
    compact = false,
    action,
  }: {
    icon: typeof import("lucide-svelte").FolderOpen;
    title: string;
    hint?: string;
    compact?: boolean;
    action?: EmptyStateAction;
  } = $props();

  const Icon = $derived(icon);
  const ActionIcon = $derived(action?.icon);
</script>

<div class="flex items-center justify-center {compact ? 'p-4' : 'p-8'}">
  <div class="gp-pop gp-card rounded-2xl text-center {compact ? 'px-6 py-5' : 'px-8 py-7'} max-w-sm">
    <div
      class="mx-auto mb-3 flex items-center justify-center rounded-full bg-accent/10 text-accent ring-1 ring-accent/25 shadow-sm {compact
        ? 'h-9 w-9'
        : 'h-11 w-11'}"
    >
      <Icon size={compact ? 16 : 20} />
    </div>
    <p class="font-semibold text-textPrimary text-xs">{title}</p>
    {#if hint}
      <p class="mt-1.5 text-[11px] leading-relaxed text-textMuted">{hint}</p>
    {/if}

    {#if action}
      <div class="mt-3.5">
        <button
          type="button"
          onclick={action.onClick}
          class="{action.variant === 'secondary' ? 'gp-btn' : 'gp-btn-primary'} !py-1.5 !px-3.5 !text-xs inline-flex items-center gap-1.5"
        >
          {#if ActionIcon}
            <ActionIcon size={13} />
          {/if}
          <span>{action.label}</span>
        </button>
      </div>
    {/if}
  </div>
</div>
