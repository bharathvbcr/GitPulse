<script lang="ts">
  /**
   * Opt-in toggle controlling whether authenticated MCP / `gitpulse-hook term`
   * callers may list and read secret-scrubbed screen output from user-opened
   * shell sessions inside the same trusted repository (`allow_agent_shell_reads`).
   *
   * Off by default: executing commands in shells remains strictly limited to
   * new shells that the agent spawned itself.
   */
  import { onMount } from "svelte";
  import { agentDefaultsStore, loadAgentDefaults, saveAgentDefaults } from "../stores/agentDefaultsStore";
  import { allowsAgentShellReads, withAllowAgentShellReads } from "../terminal/agentDefaults";
  import { formatError } from "../ui/formatError";
  import SettingToggle from "./SettingToggle.svelte";

  let {
    active = true,
  }: {
    active?: boolean;
  } = $props();

  let busy = $state(false);
  let error = $state("");
  let loaded = false;

  const view = $derived($agentDefaultsStore);
  const chosen = $derived(allowsAgentShellReads(view.defaults));

  function load() {
    if (loaded) return;
    loaded = true;
    void loadAgentDefaults();
  }

  onMount(() => {
    if (active) load();
  });

  $effect(() => {
    if (active) load();
  });

  async function choose(next: boolean) {
    if (next === chosen) return;
    busy = true;
    error = "";
    try {
      await saveAgentDefaults(withAllowAgentShellReads(view.defaults, next));
    } catch (err) {
      error = formatError(err);
    } finally {
      busy = false;
    }
  }
</script>

<div>
  <SettingToggle
    label="Allow agents to read repository shell outputs"
    ariaLabel="Allow agents to read repository shell outputs"
    disabled={busy}
    checked={chosen}
    onchange={(next) => void choose(next)}
  />
  <p class="text-textMuted text-micro leading-snug mt-1" data-testid="agent-shell-reads-note">
    Off by default: when enabled, agents running via MCP or <code>gitpulse-hook term</code> inside a trusted
    repository can list and read secret-redacted screen output from user-opened shell tabs. Sending input and
    executing commands remains restricted to new shells the agent spawned itself.
  </p>
  {#if error}
    <p class="text-danger text-micro mt-1" role="alert">{error}</p>
  {/if}
</div>
