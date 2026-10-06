<script lang="ts">
  /**
   * How many task attempts may be live at once, across every repository.
   *
   * Its own control, not a row of the permission panel: the modal hides each
   * catalog entry's wrapper by search, so a reader searching "concurrent"
   * must find this without the permission defaults beside it. It shares their
   * store because both are agent defaults in one `tools.json` block; a save
   * here carries the permission map through unchanged.
   *
   * The bounds are the store's (`run-capacity-contract.test.ts` ties them to
   * the vendored dc-store), never written here.
   */
  import { onMount } from "svelte";
  import { AlertTriangle } from "@lucide/svelte";
  import { agentDefaultsStore, loadAgentDefaults, saveAgentDefaults } from "../stores/agentDefaultsStore";
  import { isLiveRunLimit, liveRunLimit } from "../terminal/agentDefaults";
  import { DEFAULT_LIVE_RUNS, MAX_LIVE_RUNS } from "../workbench/vocabulary";
  import { formatError } from "../ui/formatError";

  let { active = true }: { active?: boolean } = $props();

  let busy = $state(false);
  let error = $state("");
  let loaded = false;

  const view = $derived($agentDefaultsStore);
  const liveRuns = $derived(liveRunLimit(view.defaults));

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
   * The default is stored as absence, so a later change to the store's
   * default reaches a reader who never chose. An out-of-range entry is refused
   * here with the bound named, rather than sent for the backend to refuse.
   */
  async function choose(input: HTMLInputElement) {
    const next = Number(input.value);
    if (!isLiveRunLimit(next)) {
      error = `Agents running at once must be a whole number from 1 to ${MAX_LIVE_RUNS}.`;
      input.value = String(liveRuns);
      return;
    }
    error = "";
    if (next === liveRuns) return;
    const rest = { ...view.defaults };
    delete rest.max_live_runs;
    busy = true;
    try {
      await saveAgentDefaults(next === DEFAULT_LIVE_RUNS ? rest : { ...rest, max_live_runs: next });
    } catch (err) {
      error = formatError(err);
      input.value = String(liveRuns);
    } finally {
      busy = false;
    }
  }
</script>

<div>
  <div class="flex items-center gap-2">
    <label class="text-textPrimary text-[11px] flex-1 min-w-0" for="gp-agent-live-runs">
      Agents running at once
    </label>
    <input
      id="gp-agent-live-runs"
      type="number"
      inputmode="numeric"
      min="1"
      max={MAX_LIVE_RUNS}
      step="1"
      class="gp-input text-[11px] py-0.5! w-16 text-right"
      data-testid="agent-live-runs"
      disabled={busy}
      value={liveRuns}
      onchange={(event) => void choose(event.currentTarget)}
    />
  </div>
  <p class="text-textMuted text-[10px] leading-snug mt-1" data-testid="agent-live-runs-note">
    Task attempts that may be live together, across every repository — up to {MAX_LIVE_RUNS}, {DEFAULT_LIVE_RUNS} unless you change it.
    Each checkout still runs one at a time; a busy one gives the next attempt its own worktree.
    Terminal attempts also need a free terminal session.
  </p>
  {#if error}
    <div class="flex items-start gap-1.5 text-amber-600 dark:text-amber-400 text-[10px] mt-1.5">
      <AlertTriangle size={12} class="shrink-0 mt-px" />
      <span data-testid="agent-live-runs-error">{error}</span>
    </div>
  {/if}
</div>
