<script lang="ts">
  /**
   * One input the governor decides on besides memory pressure (swap, the
   * memory compressor, CPU load, heat, battery, free disk): what it may do,
   * the thresholds it acts at, and the live reading beside them.
   *
   * Each input is its own Settings row (its own catalog entry and
   * `data-setting` wrapper in the modal), so a search for "swap" or "disk"
   * lands on exactly this row. All rows and the two switches save through
   * `updateHostLoadSettings`, which merges onto the latest settings, so no
   * row saves another's stale copy.
   */
  import { AlertTriangle } from "@lucide/svelte";
  import { explainError } from "../workbench/client";
  import {
    DEFAULT_THRESHOLDS,
    ensureHostLoadSettings,
    hostLoadSettings,
    hostLoadStatus,
    updateHostLoadSettings,
    type InputMode,
    type Rule,
    type Thresholds,
  } from "../stores/hostLoadStore";
  import { MODE_LABELS, liveReading, ruleProblem, specOf, type InputId } from "../health/hostLoadInputs";
  import { pollHostLoad } from "../health/hostLoadPoll";
  import { loadOnce } from "../settings/loadOnce";

  let { input, active = true }: { input: InputId; active?: boolean } = $props();

  const spec = $derived(specOf(input));
  const settings = $derived($hostLoadSettings);
  const stored = $derived(settings?.thresholds[input]);
  const reading = $derived(liveReading(input, $hostLoadStatus?.snapshot ?? null));
  const modes = $derived((["off", "shed", "refuse"] as const).filter((mode) => mode !== "refuse" || spec.canRefuse));
  const isDefault = $derived(
    stored !== undefined && JSON.stringify(stored) === JSON.stringify(DEFAULT_THRESHOLDS[input]),
  );

  let busy = $state(false);
  let error = $state("");

  async function load() {
    try {
      await ensureHostLoadSettings();
    } catch (cause) {
      error = explainError(cause);
    }
  }
  const loader = loadOnce(load);
  $effect(() => {
    if (active) loader.request();
  });

  // The live reading comes from the sampler's status; reread it while the
  // row is on screen so "now" does not go stale. The read is single-flight.
  $effect(() => {
    if (!active) return;
    return pollHostLoad();
  });

  async function save(next: Thresholds[InputId], rejected?: () => void) {
    if (!settings) return;
    busy = true;
    error = "";
    try {
      await updateHostLoadSettings({ thresholds: { ...settings.thresholds, [input]: next } });
    } catch (cause) {
      error = explainError(cause);
      rejected?.();
    } finally {
      busy = false;
    }
  }

  function chooseMode(select: HTMLSelectElement) {
    const mode = select.value as InputMode;
    void save({ ...stored, mode } as Thresholds[InputId], () => (select.value = stored?.mode ?? "off"));
  }

  /** `which` is the threshold the field holds; the other is carried through. */
  function chooseThreshold(field: HTMLInputElement, which: "elevated_at" | "severe_at") {
    if (!stored || !("elevated_at" in stored)) return;
    const next: Rule = { ...stored, [which]: field.value.trim() === "" ? Number.NaN : Number(field.value) };
    const problem = ruleProblem(spec, next);
    if (problem) {
      error = problem;
      field.value = String(stored[which]);
      return;
    }
    void save(next, () => (field.value = String(stored[which])));
  }

  function reset() {
    void save(DEFAULT_THRESHOLDS[input]);
  }
</script>

<div class="space-y-1.5 text-meta" data-testid={`host-load-input-${input}`}>
  <div class="flex items-center gap-2">
    <label class="text-textPrimary flex-1 min-w-0" for={`gp-host-load-${input}`}>{spec.label}</label>
    {#if reading}
      <span class="text-textMuted text-micro shrink-0" data-testid={`host-load-input-${input}-now`}>{reading}</span>
    {/if}
    <select
      id={`gp-host-load-${input}`}
      class="gp-select text-meta" data-size="sm"
      data-testid={`host-load-input-${input}-mode`}
      disabled={busy || !settings || !settings.enabled}
      value={stored?.mode ?? "off"}
      onchange={(event) => chooseMode(event.currentTarget)}
    >
      {#each modes as mode (mode)}
        <option value={mode}>{MODE_LABELS[mode]}</option>
      {/each}
    </select>
  </div>
  <p class="text-textMuted text-micro leading-snug">{spec.description}</p>
  {#if spec.rule && stored && "elevated_at" in stored && stored.mode !== "off"}
    <div class="flex flex-wrap items-center gap-x-3 gap-y-1">
      {#each [["elevated_at", spec.rule.elevatedLabel], ["severe_at", spec.rule.severeLabel]] as const as [which, label] (which)}
        <label class="flex items-center gap-1.5 text-textMuted text-micro">
          <span class={which === "severe_at" && stored.mode !== "refuse" ? "opacity-50" : ""}>{label}</span>
          <input
            type="number"
            inputmode="decimal"
            min="0"
            max={spec.rule.max}
            step={spec.rule.step}
            class="gp-field text-meta w-24 text-right" data-size="sm"
            data-testid={`host-load-input-${input}-${which}`}
            disabled={busy || !settings?.enabled || (which === "severe_at" && stored.mode !== "refuse")}
            value={stored[which]}
            onchange={(event) => chooseThreshold(event.currentTarget, which)}
          />
          <span>{spec.rule.unit}</span>
        </label>
      {/each}
    </div>
  {/if}
  {#if !isDefault && stored}
    <button
      type="button"
      class="gp-btn text-micro px-2 py-0.5"
      disabled={!settings || busy}
      data-testid={`host-load-input-${input}-reset`}
      onclick={reset}
    >
      Reset {spec.label.toLowerCase()} to default
    </button>
  {/if}
  {#if error}
    <div class="flex items-start gap-1.5 text-warn text-micro">
      <AlertTriangle size={12} class="shrink-0 mt-px" />
      <span data-testid={`host-load-input-${input}-error`}>{error}</span>
    </div>
  {/if}
</div>
