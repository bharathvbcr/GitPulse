<script lang="ts" module>
  /**
   * The value a press should ask for: the opposite of `checked`, or `null`
   * when the switch is disabled and nothing may be asked. Space and Enter
   * reach this through the button's own click, so one rule covers the mouse
   * and the keyboard.
   */
  export function requestedState(checked: boolean, disabled: boolean): boolean | null {
    return disabled ? null : !checked;
  }
</script>

<script lang="ts">
  /**
   * The one on/off switch (design language section 9). A native `<button>`
   * with `role="switch"`, so Space and Enter toggle it, focus is visible
   * through the global `button:focus-visible` ring, and a screen reader hears
   * "switch, on/off". The 32x18 track and 14px knob are what `SettingToggle`
   * has always drawn.
   *
   * It is controlled: it never flips `checked` itself, it asks `onchange` for
   * the next value, so a parent that refuses the change keeps the old state.
   *
   * Every switch needs a name. Pass visible `label` text (rendered as a
   * clickable `<label>` row, with an optional `description` line) or, for a
   * switch that sits beside text the caller already owns, a required
   * `ariaLabel`. The props type makes "neither" a compile error.
   */
  type Named = {
    /** Visible text. Renders the label row; also the accessible name unless `ariaLabel` overrides it. */
    label: string;
    /** Optional second line saying what the switch costs or changes. */
    description?: string;
    /** Set it when the visible label needs context to be understood alone. */
    ariaLabel?: string;
  };
  type Anonymous = {
    label?: undefined;
    description?: undefined;
    /** Required when there is no visible label. */
    ariaLabel: string;
  };
  type Props = (Named | Anonymous) & {
    checked?: boolean;
    disabled?: boolean;
    /** DOM id of the switch, so a caller-owned `<label for>` can point at it. */
    id?: string;
    onchange?: (next: boolean) => void;
  };

  let {
    label,
    description = "",
    checked = false,
    disabled = false,
    ariaLabel = "",
    id,
    onchange,
  }: Props = $props();

  const uid = $props.id();
  const generatedId = `toggle-${uid}`;
  const switchId = $derived(id ?? generatedId);

  function flip() {
    const next = requestedState(checked, disabled);
    if (next !== null) onchange?.(next);
  }
</script>

{#snippet control()}
  <button
    id={switchId}
    type="button"
    role="switch"
    aria-checked={checked}
    aria-label={ariaLabel || label || undefined}
    {disabled}
    onclick={flip}
    class="relative h-[18px] w-8 shrink-0 rounded-full transition-[background-color] duration-(--gp-dur-instant) ease-(--gp-ease-effect) disabled:opacity-40 {checked
      ? 'bg-accent'
      : 'bg-border'}"
  >
    <span
      class="absolute left-[2px] top-[2px] h-[14px] w-[14px] rounded-full bg-white shadow-xs transition-[transform] duration-(--gp-dur-fast) ease-(--gp-ease-spatial-fast) {checked
        ? 'translate-x-[14px]'
        : 'translate-x-0'}"
    ></span>
  </button>
{/snippet}

{#if label}
  <div class="flex items-start justify-between gap-3 py-1.5">
    <div class="min-w-0" class:opacity-40={disabled}>
      <label class="cursor-pointer text-meta font-medium text-textPrimary" for={switchId}>{label}</label>
      {#if description}
        <div class="mt-0.5 text-micro text-textMuted">{description}</div>
      {/if}
    </div>
    <div class="mt-0.5 shrink-0">{@render control()}</div>
  </div>
{:else}
  {@render control()}
{/if}
