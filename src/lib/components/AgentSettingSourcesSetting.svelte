<script lang="ts">
  /**
   * Which of Claude Code's settings files an agent GitPulse starts in a
   * terminal loads — a task terminal and a plain Claude tab alike.
   *
   * All three is the CLI's own default and is stored as absence, so a reader
   * who never chose passes nothing. Leaving out `project` and `local` stops a
   * repository's `.claude/settings*.json` from widening an agent's
   * permissions, at the cost of that project's allow-lists and hooks; that
   * trade is the reader's, which is why this is a choice and not a default.
   * The managed lane always loads `user` only — Manvi's rule, not this one.
   *
   * Its own control and catalog row, for the same reason as the live-run
   * limit: the modal hides each entry's wrapper by search. A save carries the
   * other agent defaults through unchanged.
   */
  import { onMount } from "svelte";
  import { AlertTriangle } from "@lucide/svelte";
  import { agentDefaultsStore, loadAgentDefaults, saveAgentDefaults } from "../stores/agentDefaultsStore";
  import {
    CLAUDE_SETTING_SOURCES,
    canonicalSettingSources,
    settingSources,
    type ClaudeSettingSource,
  } from "../terminal/agentDefaults";
  import { formatError } from "../ui/formatError";

  let { active = true }: { active?: boolean } = $props();

  let busy = $state(false);
  let error = $state("");
  let loaded = false;

  const view = $derived($agentDefaultsStore);
  const chosen = $derived(settingSources(view.defaults));

  const DESCRIPTIONS: Record<ClaudeSettingSource, string> = {
    user: "Your own ~/.claude/settings.json",
    project: "The repository's shared .claude/settings.json",
    local: "The repository's .claude/settings.local.json",
  };

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

  async function toggle(source: ClaudeSettingSource, input: HTMLInputElement) {
    const next = input.checked ? [...chosen, source] : chosen.filter((s) => s !== source);
    const canonical = canonicalSettingSources(next);
    if (canonical === null) {
      error = "Claude Code needs at least one settings source.";
      input.checked = true;
      return;
    }
    error = "";
    const rest = { ...view.defaults };
    delete rest.claude_setting_sources;
    busy = true;
    try {
      await saveAgentDefaults(canonical === undefined ? rest : { ...rest, claude_setting_sources: canonical });
    } catch (err) {
      error = formatError(err);
      input.checked = chosen.includes(source);
    } finally {
      busy = false;
    }
  }
</script>

<fieldset>
  <legend class="text-textPrimary text-[11px]">Claude Code settings files</legend>
  <div class="mt-1.5 flex flex-col gap-1" data-testid="agent-setting-sources">
    {#each CLAUDE_SETTING_SOURCES as source (source)}
      <label class="flex items-start gap-2 text-[11px] text-textPrimary">
        <input
          type="checkbox"
          class="mt-0.5"
          data-testid={`agent-setting-source-${source}`}
          disabled={busy}
          checked={chosen.includes(source)}
          onchange={(event) => void toggle(source, event.currentTarget)}
        />
        <span class="min-w-0">
          <span class="font-mono">{source}</span>
          <span class="text-textMuted"> — {DESCRIPTIONS[source]}</span>
        </span>
      </label>
    {/each}
  </div>
  <p class="text-textMuted text-[10px] leading-snug mt-1" data-testid="agent-setting-sources-note">
    Which settings files Claude Code loads when GitPulse starts it in a terminal, for task attempts and new Claude tabs alike.
    Turning off project and local stops a repository's own settings from widening an agent's permissions, but also drops that repository's allow-lists and hooks.
    Instructions in CLAUDE.md are read either way. Managed sessions always load your user settings only.
  </p>
  {#if error}
    <div class="flex items-start gap-1.5 text-amber-600 dark:text-amber-400 text-[10px] mt-1.5">
      <AlertTriangle size={12} class="shrink-0 mt-px" />
      <span data-testid="agent-setting-sources-error">{error}</span>
    </div>
  {/if}
</fieldset>
