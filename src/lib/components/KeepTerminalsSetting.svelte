<script lang="ts">
  /**
   * Whether terminals live in GitPulse or in the terminal host
   * (`gitpulse-ptyd`), where they survive a quit or a crash. Stored with the
   * agent defaults as `keep_terminals_running`; the app reads it when it
   * starts, so the row says which backend holds the terminals now and that a
   * change waits for the next launch.
   *
   * The same choice keeps managed agent runs (Codex and Claude driven by
   * Manvi) running, when the installed Manvi can serve on a socket. Whether it
   * does — and, when it cannot, why managed runs still end with the app — is
   * its own line below the note, as the backend reports it.
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
  import SettingToggle from "./SettingToggle.svelte";

  let { active = true }: { active?: boolean } = $props();

  let busy = $state(false);
  let error = $state("");
  let backend = $state<TerminalBackendStatus | null>(null);
  let backendError = $state("");
  let loaded = false;

  const view = $derived($agentDefaultsStore);
  const chosen = $derived(view.defaults.keep_terminals_running === true);

  function readBackend() {
    backendError = "";
    readTerminalBackend()
      .then((status) => {
        backend = status;
      })
      .catch((err: unknown) => {
        backendError = formatError(err);
      });
  }

  function load() {
    if (loaded) return;
    loaded = true;
    void loadAgentDefaults();
    readBackend();
  }

  onMount(() => {
    if (active) load();
  });

  $effect(() => {
    if (active) load();
  });

  /** Off is stored as absence, like every other default. */
  async function choose(next: boolean) {
    if (next === chosen) return;
    const rest: AgentDefaults = { ...view.defaults };
    delete rest.keep_terminals_running;
    busy = true;
    error = "";
    try {
      await saveAgentDefaults(next ? { ...rest, keep_terminals_running: true } : rest);
      // What the installed Manvi can do about managed runs follows the saved
      // choice at once; the terminals' answer still waits for the next launch.
      readBackend();
    } catch (err) {
      error = formatError(err);
    } finally {
      busy = false;
    }
  }

  const running = $derived(backend?.backend === "host");
  const pending = $derived(backend !== null && running !== chosen);
</script>

<div>
  <SettingToggle
    label="Keep terminals running when GitPulse quits"
    disabled={busy}
    checked={chosen}
    onchange={(next) => void choose(next)}
  />
  <p class="text-textMuted text-micro leading-snug mt-1" data-testid="keep-terminals-note">
    Terminals and the agents in them run in a process of their own, so quitting GitPulse — or GitPulse crashing — does not end
    them. The next launch lists them, shows what they printed meanwhile, and records any that finished. Quitting asks whether to
    stop them or leave them running. Managed agent runs (Codex and Claude driven by Manvi) are kept the same way when the
    installed Manvi can host them.
    {#if backend}
      Now: {running ? "kept running when GitPulse quits" : "ended when GitPulse quits"}{pending ? "; your choice applies when GitPulse next starts" : ""}.
    {/if}
  </p>
  {#if backend?.managed}
    <p
      class="text-micro leading-snug mt-1 {backend.managed.requested && !backend.managed.hosted ? 'text-warn' : 'text-textMuted'}"
      data-testid="keep-managed-status"
    >
      {backend.managed.detail}
    </p>
  {/if}
  {#if backendError}
    <div class="flex items-start gap-1.5 text-warn text-micro mt-1.5">
      <AlertTriangle size={12} class="shrink-0 mt-px" />
      <span data-testid="keep-terminals-backend-error">Couldn't read which process holds the terminals. {backendError}</span>
    </div>
  {/if}
  {#each backend?.trouble ?? [] as trouble (trouble)}
    <div class="flex items-start gap-1.5 text-warn text-micro mt-1.5">
      <AlertTriangle size={12} class="shrink-0 mt-px" />
      <span data-testid="keep-terminals-trouble">{trouble}</span>
    </div>
  {/each}
  {#if error}
    <div class="flex items-start gap-1.5 text-warn text-micro mt-1.5">
      <AlertTriangle size={12} class="shrink-0 mt-px" />
      <span data-testid="keep-terminals-error">{error}</span>
    </div>
  {/if}
</div>
