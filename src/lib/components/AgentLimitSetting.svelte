<script lang="ts">
  /**
   * One whole-number limit among the agent defaults: how many agents run at
   * once, how many terminal sessions may be open. Both rows use this control
   * and differ only in the field, its bounds and what the note says.
   *
   * Each is its own catalog row rather than part of the permission panel:
   * the modal hides each entry's wrapper by search, so a reader searching
   * "concurrent" must find the limit without the permission defaults beside
   * it. A save carries every other agent default through unchanged.
   *
   * The bounds come from the caller, which takes them from the constants the
   * contract tests tie to Rust — never written here.
   */
  import { onMount, type Snippet } from "svelte";
  import { AlertTriangle } from "@lucide/svelte";
  import { agentDefaultsStore, loadAgentDefaults, saveAgentDefaults } from "../stores/agentDefaultsStore";
  import type { AgentDefaults } from "../terminal/agentDefaults";
  import { formatError } from "../ui/formatError";

  type LimitField = "max_live_runs" | "max_terminal_sessions";

  let {
    field,
    label,
    testid,
    max,
    fallback,
    active = true,
    children,
  }: {
    field: LimitField;
    label: string;
    testid: string;
    max: number;
    /** The value when nothing is stored; choosing it stores absence. */
    fallback: number;
    active?: boolean;
    children: Snippet;
  } = $props();

  let busy = $state(false);
  let error = $state("");
  let loaded = false;

  const inRange = (value: unknown): value is number =>
    typeof value === "number" && Number.isInteger(value) && value >= 1 && value <= max;

  const view = $derived($agentDefaultsStore);
  const current = $derived.by(() => {
    const stored = view.defaults[field];
    return inRange(stored) ? stored : fallback;
  });

  onMount(() => {
    if (active && !loaded) {
      loaded = true;
      void loadAgentDefaults();
    }
  });

  $effect(() => {
    if (active && !loaded) {
      loaded = true;
      void loadAgentDefaults();
    }
  });

  /**
   * The default is stored as absence, so a later change to the default
   * reaches a reader who never chose. An out-of-range entry is refused here
   * with the bound named, rather than sent for the backend to refuse.
   */
  async function choose(input: HTMLInputElement) {
    const next = Number(input.value);
    if (!inRange(next)) {
      error = `${label} must be a whole number from 1 to ${max}.`;
      input.value = String(current);
      return;
    }
    error = "";
    if (next === current) return;
    const rest: AgentDefaults = { ...view.defaults };
    delete rest[field];
    busy = true;
    try {
      await saveAgentDefaults(next === fallback ? rest : { ...rest, [field]: next });
    } catch (err) {
      error = formatError(err);
      input.value = String(current);
    } finally {
      busy = false;
    }
  }
</script>

<div>
  <div class="flex items-center gap-2">
    <label class="text-textPrimary text-[11px] flex-1 min-w-0" for={`gp-${testid}`}>
      {label}
    </label>
    <input
      id={`gp-${testid}`}
      type="number"
      inputmode="numeric"
      min="1"
      {max}
      step="1"
      class="gp-input text-[11px] py-0.5! w-16 text-right"
      data-testid={testid}
      disabled={busy}
      value={current}
      onchange={(event) => void choose(event.currentTarget)}
    />
  </div>
  <p class="text-textMuted text-[10px] leading-snug mt-1" data-testid={`${testid}-note`}>
    {@render children()}
  </p>
  {#if error}
    <div class="flex items-start gap-1.5 text-amber-600 dark:text-amber-400 text-[10px] mt-1.5">
      <AlertTriangle size={12} class="shrink-0 mt-px" />
      <span data-testid={`${testid}-error`}>{error}</span>
    </div>
  {/if}
</div>
