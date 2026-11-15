<script lang="ts">
  /**
   * One of the two disk guards beyond the level: how soon the disk is
   * projected to fill ("fill") and the free space a coverage build needs
   * before it starts ("coverage"). Each is its own Settings row, saved through
   * `updateHostLoadSettings` like the input rows, so no row saves another's
   * stale copy.
   */
  import { AlertTriangle } from "@lucide/svelte";
  import { explainError } from "../workbench/client";
  import {
    DEFAULT_THRESHOLDS,
    ensureHostLoadSettings,
    hostLoadSettings,
    updateHostLoadSettings,
    type DiskGuard,
    type InputMode,
  } from "../stores/hostLoadStore";
  import { GUARD_SPECS, MODE_LABELS, coverageProblem, fillProblem, type GuardId } from "../health/hostLoadInputs";
  import { loadOnce } from "../settings/loadOnce";

  let { guard, active = true }: { guard: GuardId; active?: boolean } = $props();

  const spec = $derived(GUARD_SPECS[guard]);
  const settings = $derived($hostLoadSettings);
  const stored = $derived(settings?.thresholds.disk_guard[guard]);
  const isDefault = $derived(
    stored !== undefined && JSON.stringify(stored) === JSON.stringify(DEFAULT_THRESHOLDS.disk_guard[guard]),
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

  async function save(next: DiskGuard[GuardId], rejected?: () => void) {
    if (!settings) return;
    busy = true;
    error = "";
    try {
      await updateHostLoadSettings({
        thresholds: { ...settings.thresholds, disk_guard: { ...settings.thresholds.disk_guard, [guard]: next } },
      });
    } catch (cause) {
      error = explainError(cause);
      rejected?.();
    } finally {
      busy = false;
    }
  }

  function chooseMode(select: HTMLSelectElement) {
    const mode = select.value as InputMode;
    void save({ ...stored, mode } as DiskGuard[GuardId], () => (select.value = stored?.mode ?? "off"));
  }

  type Field = "elevated_within_min" | "severe_within_min" | "headroom_gib";

  function chooseNumber(input: HTMLInputElement, field: Field) {
    if (!stored) return;
    const value = input.value.trim() === "" ? Number.NaN : Number(input.value);
    const next = { ...stored, [field]: value } as DiskGuard[GuardId];
    const problem =
      guard === "fill" ? fillProblem(next as DiskGuard["fill"]) : coverageProblem(next as DiskGuard["coverage"]);
    const before = (stored as unknown as Record<Field, number>)[field];
    if (problem) {
      error = problem;
      input.value = String(before);
      return;
    }
    void save(next, () => (input.value = String(before)));
  }

  const fields = $derived<readonly { field: Field; label: string; unit: string; step: number }[]>(
    guard === "fill"
      ? [
          { field: "elevated_within_min", label: "Slow background work under", unit: "min to the floor", step: 5 },
          { field: "severe_within_min", label: "Refuse launches under", unit: "min to the floor", step: 5 },
        ]
      : [{ field: "headroom_gib", label: "Refuse a coverage build below", unit: "GiB free", step: 5 }],
  );
</script>

<div class="space-y-1.5 text-meta" data-testid={`host-load-guard-${guard}`}>
  <div class="flex items-center gap-2">
    <label class="text-textPrimary flex-1 min-w-0" for={`gp-host-load-guard-${guard}`}>{spec.label}</label>
    <select
      id={`gp-host-load-guard-${guard}`}
      class="gp-select text-meta" data-size="sm"
      data-testid={`host-load-guard-${guard}-mode`}
      disabled={busy || !settings || !settings.enabled}
      value={stored?.mode ?? "off"}
      onchange={(event) => chooseMode(event.currentTarget)}
    >
      {#each ["off", "shed", "refuse"] as const as mode (mode)}
        <option value={mode}>{guard === "coverage" && mode === "shed" ? "Say so, never refuse" : MODE_LABELS[mode]}</option>
      {/each}
    </select>
  </div>
  <p class="text-textMuted text-micro leading-snug">{spec.description}</p>
  {#if stored && stored.mode !== "off"}
    <div class="flex flex-wrap items-center gap-x-3 gap-y-1">
      {#each fields as { field, label, unit, step } (field)}
        <label class="flex items-center gap-1.5 text-textMuted text-micro">
          <span class={field === "severe_within_min" && stored.mode !== "refuse" ? "opacity-50" : ""}>{label}</span>
          <input
            type="number"
            inputmode="decimal"
            min="0"
            {step}
            class="gp-field text-meta w-24 text-right" data-size="sm"
            data-testid={`host-load-guard-${guard}-${field}`}
            disabled={busy || !settings?.enabled || (field === "severe_within_min" && stored.mode !== "refuse")}
            value={(stored as unknown as Record<Field, number>)[field]}
            onchange={(event) => chooseNumber(event.currentTarget, field)}
          />
          <span>{unit}</span>
        </label>
      {/each}
    </div>
  {/if}
  {#if !isDefault && stored}
    <button
      type="button"
      class="gp-btn text-micro px-2 py-0.5"
      disabled={!settings || busy}
      data-testid={`host-load-guard-${guard}-reset`}
      onclick={() => void save(DEFAULT_THRESHOLDS.disk_guard[guard])}
    >
      Reset {spec.label.toLowerCase()} to default
    </button>
  {/if}
  {#if error}
    <div class="flex items-start gap-1.5 text-warn text-micro">
      <AlertTriangle size={12} class="shrink-0 mt-px" />
      <span data-testid={`host-load-guard-${guard}-error`}>{error}</span>
    </div>
  {/if}
</div>
