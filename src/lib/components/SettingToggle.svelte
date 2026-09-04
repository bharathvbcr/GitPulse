<script lang="ts">
  /**
   * One owner for the settings switch row. Every preference toggle used to
   * carry its own copy of the track/knob classes and its own `role="switch"`
   * wiring, so a row added later could silently differ in size, colour or
   * accessible name from the rows above it.
   */
  let {
    label,
    description = "",
    checked = false,
    ariaLabel = "",
    onchange,
  }: {
    label: string;
    /** Optional second line explaining what the switch costs or changes. */
    description?: string;
    checked?: boolean;
    /** Defaults to the visible label; set it when the label needs context. */
    ariaLabel?: string;
    onchange?: (next: boolean) => void;
  } = $props();
</script>

<div class="flex items-start justify-between gap-3 py-1.5">
  <div class="min-w-0">
    <div class="text-textPrimary text-[11px] font-medium">{label}</div>
    {#if description}
      <div class="text-textMuted text-[10px] leading-snug mt-0.5">{description}</div>
    {/if}
  </div>
  <button
    type="button"
    role="switch"
    aria-checked={checked}
    aria-label={ariaLabel || label}
    onclick={() => onchange?.(!checked)}
    class="relative w-8 h-[18px] rounded-full transition-colors shrink-0 mt-0.5 {checked
      ? 'bg-accent'
      : 'bg-border'}"
  >
    <span
      class="absolute top-[2px] left-[2px] w-[14px] h-[14px] rounded-full bg-white shadow-sm transition-transform {checked
        ? 'translate-x-[14px]'
        : 'translate-x-0'}"
    ></span>
  </button>
</div>
