<script lang="ts">
  /**
   * Whether terminals live in GitPulse or in the terminal host
   * (`gitpulse-ptyd`), where they survive a quit or a crash. Stored with the
   * agent defaults as `keep_terminals_running`; the app reads it when it
   * starts, so the row says which backend holds the terminals now and that a
   * change waits for the next launch.
   *
   * What is wrong with the host — another version's host still running
   * sessions, a host that would not start — is shown here as the backend
   * reports it, never summarised into "fine".
   */
  import { onMount } from "svelte";
  import { AlertTriangle } from "@lucide/svelte";
  import { agentDefaultsStore, loadAgentDefaults, saveAgentDefaults } from "../stores/agentDefaultsStore";
  import type { AgentDefaults } from "../terminal/agentDefaults";
  import { readTerminalBackend, type TerminalBackendStatus } from "../terminal/quitChoice";
  import { formatError } from "../ui/formatError";

  let { active = true }: { active?: boolean } = $props();

  let busy = $state(false);
  let error = $state("");
  let backend = $state<TerminalBackendStatus | null>(null);
  let backendError = $state("");
  let loaded = false;

  const view = $derived($agentDefaultsStore);
  const chosen = $derived(view.defaults.keep_terminals_running === true);

  function load() {
    if (loaded) return;
    loaded = true;
    void loadAgentDefaults();
    readTerminalBackend()
      .then((status) => {
        backend = status;
      })
      .catch((err: unknown) => {
        backendError = formatError(err);
      });
  }

  onMount(() => {
    if (active) load();
  });

  $effect(() => {
    if (active) load();
  });

  /** Off is stored as absence, like every other default. */
  async function choose(input: HTMLInputElement) {
    const next = input.checked;
    if (next === chosen) return;
    const rest: AgentDefaults = { ...view.defaults };
    delete rest.keep_terminals_running;
    busy = true;
    error = "";
    try {
      await saveAgentDefaults(next ? { ...rest, keep_terminals_running: true } : rest);
    } catch (err) {
      error = formatError(err);
      input.checked = chosen;
    } finally {
      busy = false;
    }
  }

  const running = $derived(backend?.backend === "host");
  const pending = $derived(backend !== null && running !== chosen);
</script>

<div>
  <div class="flex items-center gap-2">
    <label class="text-textPrimary text-[11px] flex-1 min-w-0" for="gp-keep-terminals">
      Keep terminals running when GitPulse quits
    </label>
    <input
      id="gp-keep-terminals"
      type="checkbox"
      data-testid="keep-terminals"
      disabled={busy}
      checked={chosen}
      onchange={(event) => void choose(event.currentTarget)}
    />
  </div>
  <p class="text-textMuted text-[10px] leading-snug mt-1" data-testid="keep-terminals-note">
    Terminals and the agents in them run in a process of their own, so quitting GitPulse — or GitPulse crashing — does not end
    them. The next launch lists them, shows what they printed meanwhile, and records any that finished. Quitting asks whether to
    stop them or leave them running.
    {#if backend}
      Now: {running ? "kept running when GitPulse quits" : "ended when GitPulse quits"}{pending ? "; your choice applies when GitPulse next starts" : ""}.
    {/if}
  </p>
  {#if backendError}
    <div class="flex items-start gap-1.5 text-amber-600 dark:text-amber-400 text-[10px] mt-1.5">
      <AlertTriangle size={12} class="shrink-0 mt-px" />
      <span data-testid="keep-terminals-backend-error">Could not read which process holds the terminals: {backendError}</span>
    </div>
  {/if}
  {#each backend?.trouble ?? [] as trouble (trouble)}
    <div class="flex items-start gap-1.5 text-amber-600 dark:text-amber-400 text-[10px] mt-1.5">
      <AlertTriangle size={12} class="shrink-0 mt-px" />
      <span data-testid="keep-terminals-trouble">{trouble}</span>
    </div>
  {/each}
  {#if error}
    <div class="flex items-start gap-1.5 text-amber-600 dark:text-amber-400 text-[10px] mt-1.5">
      <AlertTriangle size={12} class="shrink-0 mt-px" />
      <span data-testid="keep-terminals-error">{error}</span>
    </div>
  {/if}
</div>
