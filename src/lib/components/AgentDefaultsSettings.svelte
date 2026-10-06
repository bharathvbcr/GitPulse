<script lang="ts">
  /**
   * What an agent CLI starts with when it is launched from a terminal tab.
   *
   * Separate from `SessionAlertSettings` above it, which governs whether a
   * session may interrupt you. This one governs how much authority it begins
   * with — a different question with different stakes, and one that becomes
   * argv rather than a notification decision.
   *
   * The chooser offers only launchers the backend says have a policy, and only
   * modes it says it can expand. A mode shown here that the backend could not
   * apply would be a setting that fails at spawn, which is the one thing a
   * defaults panel must not produce.
   */
  import { onMount } from "svelte";
  import { AlertTriangle } from "@lucide/svelte";
  import {
    agentDefaultsStore,
    loadAgentDefaults,
    saveAgentDefaults,
  } from "../stores/agentDefaultsStore";
  import {
    BYPASS_MODE,
    PERMISSION_LABELS,
    isLiveRunLimit,
    isPermissionMode,
    liveRunLimit,
    type PermissionMode,
  } from "../terminal/agentDefaults";
  import { DEFAULT_LIVE_RUNS, MAX_LIVE_RUNS } from "../workbench/vocabulary";
  import { launcherLabel, type LauncherKind } from "../terminal/tabs";
  import { formatError } from "../ui/formatError";

  let { active = true }: { active?: boolean } = $props();

  let busy = $state(false);
  let error = $state("");
  let loaded = false;

  const view = $derived($agentDefaultsStore);

  /**
   * "Whatever the CLI does on its own" is a real choice and the shipped one,
   * so it is an option rather than an empty select. Its value is the empty
   * string because absence is what the backend stores for it.
   */
  const INHERIT = "";

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

  async function choose(launcher: LauncherKind, raw: string) {
    const next = { ...view.defaults.permission };
    if (raw === INHERIT) delete next[launcher];
    else if (isPermissionMode(raw)) next[launcher] = raw;
    else return;
    busy = true;
    error = "";
    try {
      await saveAgentDefaults({ ...view.defaults, permission: next });
    } catch (err) {
      error = formatError(err);
    } finally {
      busy = false;
    }
  }

  const chosen = (launcher: LauncherKind): PermissionMode | "" =>
    view.defaults.permission[launcher] ?? INHERIT;

  const liveRuns = $derived(liveRunLimit(view.defaults));

  /**
   * Saves how many attempts may run at once. The default is stored as
   * absence, so a later change to the store's default reaches a reader who
   * never chose; an out-of-range entry is refused here with the bound named,
   * rather than sent for the backend to refuse.
   */
  async function chooseLiveRuns(input: HTMLInputElement) {
    const next = Number(input.value);
    if (!isLiveRunLimit(next)) {
      error = `Agents running at once must be a whole number from 1 to ${MAX_LIVE_RUNS}.`;
      input.value = String(liveRuns);
      return;
    }
    if (next === liveRuns) return;
    const rest = { ...view.defaults };
    delete rest.max_live_runs;
    busy = true;
    error = "";
    try {
      await saveAgentDefaults(next === DEFAULT_LIVE_RUNS ? rest : { ...rest, max_live_runs: next });
    } catch (err) {
      error = formatError(err);
      input.value = String(liveRuns);
    } finally {
      busy = false;
    }
  }

  /**
   * Launchers whose stored default turns permission checks off. Listed rather
   * than counted: a reader who set one months ago should be able to see which
   * one without opening each select.
   */
  const bypassing = $derived(
    view.launchers.filter((launcher) => view.defaults.permission[launcher] === BYPASS_MODE),
  );
</script>

<div data-setting="agent-defaults">
  <div class="text-textMuted text-[10px] mb-1.5">Agent launch defaults</div>
  <p class="text-textMuted text-[10px] leading-snug mb-2">
    How much a coding agent may do when you start it from a terminal tab. Applies to new
    sessions; a task handed to an agent keeps the permission mode chosen for that task.
  </p>

  {#if error}
    <div class="flex items-start gap-1.5 text-amber-600 dark:text-amber-400 text-[10px] mb-2">
      <AlertTriangle size={12} class="shrink-0 mt-px" />
      <span data-testid="agent-defaults-error">{error}</span>
    </div>
  {/if}

  <div class="space-y-1.5">
    {#each view.launchers as launcher (launcher)}
      <div class="flex items-center gap-2">
        <label class="text-textPrimary text-[11px] w-24 shrink-0" for="gp-agent-default-{launcher}">
          {launcherLabel(launcher)}
        </label>
        <select
          id="gp-agent-default-{launcher}"
          class="gp-input text-[11px] py-0.5! flex-1 min-w-0"
          data-testid="agent-default-{launcher}"
          disabled={busy}
          value={chosen(launcher)}
          onchange={(event) => void choose(launcher, event.currentTarget.value)}
        >
          <option value={INHERIT}>Use {launcherLabel(launcher)}'s own default</option>
          {#each view.modes as mode (mode)}
            <option value={mode}>{PERMISSION_LABELS[mode].label}</option>
          {/each}
        </select>
      </div>
      {#if chosen(launcher) !== INHERIT}
        <p class="text-textMuted text-[10px] leading-snug pl-26">
          {PERMISSION_LABELS[chosen(launcher) as PermissionMode].detail}
        </p>
      {/if}
    {/each}
  </div>

  <div class="flex items-center gap-2 mt-3" data-setting="agent-live-runs">
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
      onchange={(event) => void chooseLiveRuns(event.currentTarget)}
    />
  </div>
  <p class="text-textMuted text-[10px] leading-snug mt-1" data-testid="agent-live-runs-note">
    Task attempts that may be live together, across every repository — up to {MAX_LIVE_RUNS}, {DEFAULT_LIVE_RUNS} unless you change it.
    Each checkout still runs one at a time; a busy one gives the next attempt its own worktree.
    Terminal attempts also need a free terminal session.
  </p>

  {#if bypassing.length}
    <!-- Named rather than counted, and phrased as what will happen rather than
         as a warning about what was chosen: the reader chose it deliberately,
         and what they need is the reminder that it is still in force. -->
    <div
      class="flex items-start gap-1.5 text-amber-600 dark:text-amber-400 text-[10px] mt-2.5"
      data-testid="agent-defaults-bypass-note"
    >
      <AlertTriangle size={12} class="shrink-0 mt-px" />
      <span>
        {bypassing.map(launcherLabel).join(" and ")}
        {bypassing.length > 1 ? "start" : "starts"} with no permission checks and no sandbox.
        GitPulse asks you to confirm each of those sessions before it opens.
      </span>
    </div>
  {/if}
</div>
