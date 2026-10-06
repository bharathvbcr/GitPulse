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
    isPermissionMode,
    type PermissionMode,
  } from "../terminal/agentDefaults";
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
