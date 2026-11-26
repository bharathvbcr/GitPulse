<script lang="ts">
  /**
   * Whether managed agent runs (Codex and Claude driven by Manvi) are kept
   * running in a hosted Manvi server when GitPulse quits or crashes, plus
   * read-only status of the terminal host (`gitpulse-ptyd`).
   *
   * On Unix, PTY terminal sessions always live in `gitpulse-ptyd`, so
   * quitting or crashing does not end them; Cmd-Q asks whether to stop them
   * or leave them running. The stored `keep_terminals_running` toggle (default
   * on when absent) governs hosted Manvi for managed runs; whether Manvi can
   * host them is shown below the note, as reported by `cmd_terminal_backend`,
   * along with any `trouble` reported by the terminal host.
   */
  import { onMount } from "svelte";
  import { AlertTriangle } from "@lucide/svelte";
  import { agentDefaultsStore, loadAgentDefaults, saveAgentDefaults } from "../stores/agentDefaultsStore";
  import { keepsTerminalsRunning, withKeepTerminalsRunning } from "../terminal/agentDefaults";
  import { readTerminalBackend, type TerminalBackendStatus } from "../terminal/quitChoice";
  import { formatError } from "../ui/formatError";
  import SettingToggle from "./SettingToggle.svelte";

  let {
    active = true,
    initialBackend = null,
    initialBackendError = "",
  }: {
    active?: boolean;
    initialBackend?: TerminalBackendStatus | null;
    initialBackendError?: string;
  } = $props();

  let busy = $state(false);
  let error = $state("");
  let loadedBackend = $state<TerminalBackendStatus | null>(null);
  let loadedBackendError = $state("");
  let hasLoadedBackend = $state(false);
  let loaded = false;

  const view = $derived($agentDefaultsStore);
  const chosen = $derived(keepsTerminalsRunning(view.defaults));
  const backend = $derived(hasLoadedBackend ? loadedBackend : initialBackend);
  const backendError = $derived(hasLoadedBackend ? loadedBackendError : initialBackendError);

  function readBackend() {
    hasLoadedBackend = true;
    loadedBackendError = "";
    readTerminalBackend()
      .then((status) => {
        loadedBackend = status;
      })
      .catch((err: unknown) => {
        loadedBackendError = formatError(err);
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

  /** On (`true`) is the default and stored as absence; off stores `false`. */
  async function choose(next: boolean) {
    if (next === chosen) return;
    busy = true;
    error = "";
    try {
      await saveAgentDefaults(withKeepTerminalsRunning(view.defaults, next));
      // Refresh backend status right after saving so managed hosting reflects
      // the new choice.
      readBackend();
    } catch (err) {
      error = formatError(err);
    } finally {
      busy = false;
    }
  }

  const runningInHost = $derived(backend?.backend === "host");
</script>

<div>
  <SettingToggle
    label="Keep managed agent runs running when Waylode quits"
    disabled={busy}
    checked={chosen}
    onchange={(next) => void choose(next)}
  />
  <p class="text-textMuted text-micro leading-snug mt-1" data-testid="keep-terminals-note">
    Terminal sessions run in a separate host process (<code>gitpulse-ptyd</code>), so quitting Waylode — or Waylode crashing — does
    not end them. The next launch lists them, replays buffered output, and reconciles any that finished; quitting asks whether
    to stop them or leave them running.
    {#if backend}
      Terminals now: {runningInHost ? "held by the terminal host (survive quit and crash)" : "held in-process (end when Waylode quits)"}.
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
