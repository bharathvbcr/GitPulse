<script lang="ts">
  import Toggle from "./Toggle.svelte";

  /**
   * One owner for the settings switch row. Every preference toggle used to
   * carry its own copy of the track/knob classes and its own `role="switch"`
   * wiring, so a row added later could silently differ in size, colour or
   * accessible name from the rows above it.
   *
   * The switch itself is `Toggle`; this component only adds the settings row
   * around it (a label and description on the left). Its props are unchanged.
   */
  let {
    label,
    description = "",
    checked = false,
    disabled = false,
    ariaLabel = "",
    onchange,
  }: {
    label: string;
    /** Optional second line explaining what the switch costs or changes. */
    description?: string;
    checked?: boolean;
    disabled?: boolean;
    /** Defaults to the visible label; set it when the label needs context. */
    ariaLabel?: string;
    onchange?: (next: boolean) => void;
  } = $props();

  const switchId = `setting-toggle-${Math.random().toString(36).slice(2, 10)}`;
</script>

<div class="flex items-start justify-between gap-3 py-1.5">
  <div class="min-w-0" class:opacity-40={disabled}>
    <label class="text-textPrimary text-meta font-medium cursor-pointer" for={switchId}>{label}</label>
    {#if description}
      <div class="text-textMuted text-micro mt-0.5">{description}</div>
    {/if}
  </div>
  <div class="mt-0.5 shrink-0">
    <Toggle id={switchId} ariaLabel={ariaLabel || label} {checked} {disabled} {onchange} />
  </div>
</div>
