<script lang="ts">
  /**
   * Starts a thread: where the agent runs, how much it may do, and the first
   * message. "This repository" is the one open in GitPulse. An SSH host is
   * checked before anything starts, with an answer that says what to fix
   * (sign-in, host key, missing Claude Code, wrong directory); ssh never
   * prompts here, so a host that needs a password says so instead of hanging.
   */
  import { onMount } from "svelte";
  import { Laptop, Server, LoaderCircle, CircleCheck, TriangleAlert } from "@lucide/svelte";
  import { MAVI_MODES, maviClient, type MaviModeId, type Probe } from "../../mavi/client";
  import { maviStore } from "../../mavi/store.svelte";
  import { currentRepoPath } from "../../stores/repoStore";
  import Checkbox from "../Checkbox.svelte";
  import { shortcutTextLabel } from "../../ui/platformCopy";
  import { hostPlatform } from "../../stores/platformStore";

  let { onStarted, onCancel }: { onStarted: (key: string) => void; onCancel?: () => void } = $props();

  let where = $state<"local" | "ssh">("local");
  let host = $state("");
  let dir = $state("~/");
  let hosts = $state<string[]>([]);
  let probe = $state<Probe | null>(null);
  let probing = $state(false);
  let mode = $state<MaviModeId>("ask");
  let acknowledged = $state(false);
  let model = $state("");
  let message = $state("");
  let starting = $state(false);
  let error = $state<string | null>(null);

  onMount(() => {
    void maviClient.sshHosts().then((list) => (hosts = list)).catch(() => (hosts = []));
  });

  let modeInfo = $derived(MAVI_MODES.find((m) => m.id === mode)!);
  let probeOk = $derived(probe?.kind === "ok" || probe?.kind === "not_a_repo");
  let ready = $derived(
    message.trim().length > 0 &&
      (mode !== "bypass" || acknowledged) &&
      (where === "local" ? !!$currentRepoPath : host.trim().length > 0 && dir.trim().length > 0 && probeOk),
  );

  $effect(() => {
    // A changed host or directory needs a new check.
    void host;
    void dir;
    probe = null;
  });

  async function check() {
    probing = true;
    error = null;
    try {
      probe = await maviClient.sshProbe(host.trim(), dir.trim());
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      probing = false;
    }
  }

  async function start() {
    if (!ready || starting) return;
    starting = true;
    error = null;
    try {
      const key = await maviStore.create({
        target:
          where === "local"
            ? { kind: "local", repoPath: $currentRepoPath! }
            : { kind: "ssh", host: host.trim(), dir: dir.trim() },
        mode,
        acknowledged: mode === "bypass" ? acknowledged : false,
        model: model.trim() || undefined,
        firstMessage: message.trim(),
      });
      onStarted(key);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      starting = false;
    }
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      void start();
    }
  }
</script>

<form class="mavi-new gp-card" onsubmit={(e) => { e.preventDefault(); void start(); }} aria-label="New Mavi thread">
  <h2 class="text-sm font-medium text-textPrimary">New thread</h2>

  <fieldset class="mt-3">
    <legend class="gp-label">Where the agent runs</legend>
    <div class="gp-segmented mt-1" role="group">
      <button type="button" class="gp-seg-btn inline-flex items-center gap-1.5" data-active={where === "local"} onclick={() => (where = "local")}>
        <Laptop size={12} aria-hidden="true" /> This repository
      </button>
      <button type="button" class="gp-seg-btn inline-flex items-center gap-1.5" data-active={where === "ssh"} onclick={() => (where = "ssh")}>
        <Server size={12} aria-hidden="true" /> SSH host
      </button>
    </div>
    {#if where === "local"}
      <p class="text-meta text-textMuted mt-1.5 font-mono truncate">{$currentRepoPath ?? "Open a repository first."}</p>
    {:else}
      <div class="grid gap-2 mt-2 mavi-ssh">
        <label class="grid gap-1">
          <span class="gp-label">Host</span>
          <input class="gp-field" list="mavi-ssh-hosts" bind:value={host} placeholder="devbox or me@devbox.lan" autocomplete="off" spellcheck="false" />
          <datalist id="mavi-ssh-hosts">
            {#each hosts as h (h)}<option value={h}></option>{/each}
          </datalist>
        </label>
        <label class="grid gap-1">
          <span class="gp-label">Directory on the host</span>
          <input class="gp-field font-mono" bind:value={dir} placeholder="~/code/app" spellcheck="false" />
        </label>
        <div class="flex items-center gap-2">
          <button type="button" class="gp-btn" disabled={!host.trim() || probing} onclick={check}>
            {#if probing}<LoaderCircle size={12} class="animate-spin" aria-hidden="true" />{/if} Check connection
          </button>
          {#if probe}
            <span class="text-meta inline-flex items-center gap-1" class:text-ok={probeOk} class:text-danger={!probeOk} role="status">
              {#if probeOk}<CircleCheck size={12} aria-hidden="true" />{:else}<TriangleAlert size={12} aria-hidden="true" />{/if}
              {probe.message}
              <span class="text-textMuted">· {probe.millis} ms</span>
            </span>
          {/if}
        </div>
        <p class="text-micro text-textMuted">
          Uses your ssh config, keys and known hosts. The agent runs on the host for as long as this connection lasts; if it drops, resume the thread to continue the conversation.
        </p>
      </div>
    {/if}
  </fieldset>

  <fieldset class="mt-3">
    <legend class="gp-label">What it may do</legend>
    <div class="flex flex-wrap gap-1 mt-1" role="radiogroup">
      {#each MAVI_MODES as m (m.id)}
        <button
          type="button"
          class="gp-chip"
          role="radio"
          aria-checked={mode === m.id}
          data-tone={mode === m.id ? (m.id === "bypass" ? "danger" : "accent") : "neutral"}
          onclick={() => (mode = m.id)}
        >{m.label}</button>
      {/each}
    </div>
    <p class="text-meta text-textMuted mt-1">{modeInfo.hint}</p>
    {#if mode === "bypass"}
      <div class="mt-1">
        <Checkbox
          label="I understand the agent will run any command and edit any file without asking."
          checked={acknowledged}
          onchange={(next) => (acknowledged = next)}
        />
      </div>
    {/if}
  </fieldset>

  <label class="grid gap-1 mt-3">
    <span class="gp-label">Model (optional)</span>
    <input class="gp-field font-mono" bind:value={model} placeholder="The agent's default" spellcheck="false" />
  </label>

  <label class="grid gap-1 mt-3">
    <span class="gp-label">First message</span>
    <textarea
      class="gp-field gp-field-multi py-1.5 text-sm"
      rows="4"
      bind:value={message}
      onkeydown={onKeydown}
      placeholder="What should the agent do?"
    ></textarea>
  </label>

  {#if error}<p class="gp-notice gp-notice-error text-meta mt-2 mx-0" role="alert">{error}</p>{/if}

  <div class="flex items-center gap-2 mt-3">
    <button type="submit" class="gp-btn" data-variant="primary" data-size="md" disabled={!ready || starting}>
      {#if starting}<LoaderCircle size={12} class="animate-spin" aria-hidden="true" />{/if} Start thread
    </button>
    <span class="gp-keycap">{shortcutTextLabel("⌘↵", $hostPlatform.os)}</span>
    {#if onCancel}<button type="button" class="gp-btn ml-auto" data-variant="ghost" onclick={onCancel}>Cancel</button>{/if}
  </div>
</form>

<style>
  .mavi-new {
    padding: 16px 18px;
    width: 100%;
    max-width: 36rem;
  }
  fieldset {
    border: 0;
    padding: 0;
    margin-inline: 0;
  }
</style>
