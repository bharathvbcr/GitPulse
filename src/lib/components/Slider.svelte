<script lang="ts" module>
  /** Decimal places `n` needs, up to the 12 a double holds exactly enough for a slider step. */
  function decimalsOf(n: number): number {
    let places = 0;
    while (places < 12 && Math.round(n * 10 ** places) / 10 ** places !== n) places += 1;
    return places;
  }

  /**
   * The value `steps` slider steps from `value`, kept on the step grid that
   * starts at `min` and clamped to `[min, max]`.
   *
   * A native range input floors page keys at a tenth of its range, which is
   * not "ten steps" for a coarse or fine step, so the component computes the
   * page jump itself. The result is rounded to the precision of `step` and
   * `min`, so 0.05 steps do not drift to 1.0000000000000002.
   */
  export function stepValue(value: number, steps: number, min: number, max: number, step: number): number {
    if (!Number.isFinite(value) || !Number.isFinite(min) || !Number.isFinite(max) || !(step > 0) || max < min) {
      return Number.isFinite(value) ? value : min;
    }
    const places = Math.max(decimalsOf(step), decimalsOf(min));
    const onGrid = min + Math.round((value - min) / step) * step + steps * step;
    const clamped = Math.min(max, Math.max(min, onGrid));
    return Number(clamped.toFixed(places));
  }

  /** Steps a page key moves: ten, as the design language specifies. */
  export const PAGE_STEPS = 10;

  /** Direction of a key the slider moves by itself (the page keys), or 0. */
  export function pageKeySteps(key: string): number {
    if (key === "PageUp") return PAGE_STEPS;
    if (key === "PageDown") return -PAGE_STEPS;
    return 0;
  }
</script>

<script lang="ts">
  /**
   * The one range control (design language section 9): a native
   * `<input type="range">` underneath, so the browser supplies the role,
   * `aria-valuemin/max/now`, pointer dragging and the arrow, Home and End
   * keys (one step each, to either end). The thumb and track are the
   * `input[type="range"]` rules in app.css. This adds the value label and
   * makes Page Up / Page Down move ten steps.
   *
   * `format` turns the number into the label text and into `aria-valuetext`
   * ("120%", "3 days"), so a screen reader hears what a sighted user reads.
   *
   * Every slider needs a name: visible `label` text, or a required
   * `ariaLabel` when the caller owns the text beside it.
   */
  type Named = { label: string; ariaLabel?: string };
  type Anonymous = { label?: undefined; ariaLabel: string };
  type Props = (Named | Anonymous) & {
    value: number;
    min: number;
    max: number;
    step?: number;
    disabled?: boolean;
    format?: (value: number) => string;
    id?: string;
    oninput?: (value: number) => void;
  };

  let {
    value = $bindable(),
    min,
    max,
    step = 1,
    disabled = false,
    format = (n: number) => String(n),
    label,
    ariaLabel = "",
    id,
    oninput,
  }: Props = $props();

  const uid = $props.id();
  const generatedId = `slider-${uid}`;
  const inputId = $derived(id ?? generatedId);
  const valueText = $derived(format(value));

  function commit(next: number) {
    if (next === value) return;
    value = next;
    oninput?.(next);
  }

  function onInput(event: Event & { currentTarget: HTMLInputElement }) {
    commit(Number(event.currentTarget.value));
  }

  function onKeydown(event: KeyboardEvent) {
    if (disabled || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
    const steps = pageKeySteps(event.key);
    if (steps === 0) return;
    event.preventDefault();
    commit(stepValue(value, steps, min, max, step));
  }
</script>

{#snippet control()}
  <input
    id={inputId}
    type="range"
    {min}
    {max}
    {step}
    {value}
    {disabled}
    aria-label={ariaLabel || undefined}
    aria-valuetext={valueText}
    oninput={onInput}
    onkeydown={onKeydown}
    class="min-w-0 flex-1 disabled:opacity-40"
  />
{/snippet}

{#if label}
  <div class="flex flex-col gap-1">
    <div class="flex items-center justify-between gap-2 text-meta">
      <label class="font-medium text-textPrimary" for={inputId}>{label}</label>
      <span class="font-medium tabular-nums text-accent" aria-hidden="true">{valueText}</span>
    </div>
    <div class="flex items-center">{@render control()}</div>
  </div>
{:else}
  <div class="flex items-center gap-2 text-meta">
    {@render control()}
    <span class="shrink-0 font-medium tabular-nums text-accent" aria-hidden="true">{valueText}</span>
  </div>
{/if}
