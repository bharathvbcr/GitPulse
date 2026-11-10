<script lang="ts" module>
  /**
   * What a press should ask for: the opposite of `checked`, or `null` when the
   * box is disabled and nothing may be asked. An indeterminate box is not
   * checked, so the first press asks for `true`, as a native checkbox does.
   */
  export function requestedCheck(checked: boolean, disabled: boolean): boolean | null {
    return disabled ? null : !checked;
  }
</script>

<script lang="ts">
  /**
   * The one checkbox (design language section 9). It picks items from a list or
   * confirms a statement before an action; a setting that takes effect the
   * moment it flips is a switch, which is `Toggle`.
   *
   * A native `<input type="checkbox">` drawn by `gp-check`, so the role,
   * Space, form semantics and the indeterminate state are the browser's own.
   *
   * It is controlled: it never keeps a value of its own. A press restores the
   * box to `checked` / `indeterminate` and asks `onchange` for the opposite, so
   * a parent that refuses the change keeps the old state.
   *
   * Every box needs a name. Pass visible `label` text (the whole row is then a
   * `<label>`, so clicking the text toggles it, with an optional `description`
   * line) or, for a box that sits beside text the caller already owns, a
   * required `ariaLabel`. The props type makes "neither" a compile error.
   */
  type Named = {
    /** Visible text. Renders the label row; also the accessible name unless `ariaLabel` overrides it. */
    label: string;
    /** Optional second line saying what ticking the box commits to. */
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
    /** Some but not all of a group: drawn as a bar, and reported as "mixed". */
    indeterminate?: boolean;
    disabled?: boolean;
    /** DOM id of the input, so a caller-owned `<label for>` can point at it. */
    id?: string;
    onchange?: (next: boolean) => void;
  };

  let {
    label,
    description = "",
    checked = false,
    indeterminate = false,
    disabled = false,
    ariaLabel = "",
    id,
    onchange,
  }: Props = $props();

  const uid = $props.id();
  const generatedId = `checkbox-${uid}`;
  const boxId = $derived(id ?? generatedId);
  const descriptionId = $derived(`${boxId}-description`);

  function press(event: Event & { currentTarget: HTMLInputElement }) {
    const box = event.currentTarget;
    const next = requestedCheck(checked, disabled);
    // The browser has already flipped the box; put it back so the parent decides.
    box.checked = checked;
    box.indeterminate = indeterminate;
    if (next !== null) onchange?.(next);
  }
</script>

{#snippet control()}
  <input
    id={boxId}
    type="checkbox"
    class="gp-check"
    {checked}
    {indeterminate}
    {disabled}
    aria-label={ariaLabel || label || undefined}
    aria-describedby={description ? descriptionId : undefined}
    onchange={press}
  />
{/snippet}

{#if label}
  <label class="flex items-start gap-2 py-1.5 {disabled ? 'cursor-not-allowed' : 'cursor-pointer'}" for={boxId}>
    <span class="mt-px shrink-0">{@render control()}</span>
    <span class="min-w-0" class:opacity-40={disabled}>
      <span class="block text-meta font-medium text-textPrimary">{label}</span>
      {#if description}
        <span id={descriptionId} class="mt-0.5 block text-micro text-textMuted">{description}</span>
      {/if}
    </span>
  </label>
{:else}
  {@render control()}
{/if}
