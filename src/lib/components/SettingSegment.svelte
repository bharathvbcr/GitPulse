<script lang="ts" generics="T extends string">
  /**
   * One owner for the settings segmented control. Five preferences now use
   * this shape; hand-writing each one is how `aria-pressed` and `data-active`
   * drift apart, which leaves a control that looks selected but announces
   * nothing.
   */
  let {
    ariaLabel,
    options,
    value,
    onselect,
  }: {
    /** Group name for screen readers, e.g. "Theme appearance". */
    ariaLabel: string;
    options: readonly { value: T; label: string; title?: string }[];
    value: T;
    onselect?: (next: T) => void;
  } = $props();
</script>

<div class="gp-segmented" role="group" aria-label={ariaLabel}>
  {#each options as option (option.value)}
    <button
      type="button"
      onclick={() => onselect?.(option.value)}
      aria-pressed={value === option.value}
      data-active={value === option.value ? "true" : "false"}
      class="gp-seg-btn"
      title={option.title ?? option.label}
    >
      {option.label}
    </button>
  {/each}
</div>
