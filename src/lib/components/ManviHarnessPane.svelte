<script lang="ts">
  import { harnessStore, verdictLabel, type AiSelection } from "../stores/harnessStore";
  import { repoStore } from "../stores/repoStore";
  import { copyText } from "../desktop/clipboard";
  import { formatError } from "../ui/formatError";
  import {
    RefreshCw,
    ShieldCheck,
    ShieldAlert,
    Server,
    GitBranch,
    Check,
    History,
    Clipboard,
  } from "lucide-svelte";

  let branchSuggestion = $state<string>("");
  let branchWarnings = $state<string[]>([]);
  let isSuggesting = $state(false);
  let branchError = $state<string | null>(null);
  let logCopied = $state(false);

  let ai = $derived($harnessStore.ai);
  let harness = $derived($harnessStore.harness);
  let preferred = $derived($harnessStore.preferred);
  // Newest first: the question the journal answers is "what just happened".
  let recentActions = $derived($harnessStore.actions.slice().reverse());

  function actionTime(ts: number): string {
    return new Date(ts).toLocaleTimeString();
  }

  function verdictChip(status: string): string {
    switch (status) {
      case "blocked":
        return "bg-rose-500/20 text-rose-300 border-rose-500/40";
      case "unchecked":
        return "bg-amber-500/15 text-amber-400 border-amber-500/30";
      case "warned":
        return "bg-sky-500/15 text-sky-300 border-sky-500/30";
      default:
        return "bg-emerald-500/10 text-emerald-300 border-emerald-500/25";
    }
  }

  let logCopyTimer: number | null = null;

  /** The journal as plain text, for pasting into a bug report or notes file. */
  async function copyLog() {
    const lines = $harnessStore.actions.map((action) => {
      const time = actionTime(action.ts);
      const state = !action.ok
        ? "failed"
        : action.verdict
          ? verdictLabel(action.verdict).replace(/^Policy: /, "")
          : "no gate ran";
      return `${time}\t${action.kind}\t${action.label || "—"}\t[${state}]`;
    });
    if (await copyText(lines.join("\n"))) {
      logCopied = true;
      if (logCopyTimer !== null) window.clearTimeout(logCopyTimer);
      logCopyTimer = window.setTimeout(() => (logCopied = false), 1500);
    }
  }

  $effect(() => {
    return () => {
      if (logCopyTimer !== null) window.clearTimeout(logCopyTimer);
    };
  });

  function isSelected(endpointUrl: string, model: string): boolean {
    if (preferred) return preferred.base_url === endpointUrl && preferred.model === model;
    return ai?.selected?.base_url === endpointUrl && ai?.selected?.model === model;
  }

  async function pick(selection: AiSelection | null) {
    await harnessStore.selectModel(selection);
  }

  async function suggestBranch() {
    const path = $repoStore.currentPath;
    if (!path) return;
    isSuggesting = true;
    branchError = null;
    branchSuggestion = "";
    branchWarnings = [];
    try {
      const result = await harnessStore.suggestBranchName(path);
      branchSuggestion = result.text;
      branchWarnings = result.warnings;
    } catch (err: unknown) {
      branchError = formatError(err);
    } finally {
      isSuggesting = false;
    }
  }

  async function createSuggestedBranch() {
    if (!branchSuggestion) return;
    try {
      await repoStore.createBranch(branchSuggestion);
      branchSuggestion = "";
    } catch (err: unknown) {
      branchError = formatError(err);
    }
  }
</script>

<div class="space-y-4">
  <!-- Harness -->
  <section class="gp-card p-4 space-y-2">
    <div class="flex items-center justify-between gap-2">
      <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">MANVI harness</h3>
      <button
        onclick={() => harnessStore.reconnect()}
        class="gp-btn !py-1 !text-[11px]"
        title="Restart the MANVI sidecar and sweep for model servers again"
      >
        <RefreshCw size={12} class={$harnessStore.isProbing ? "animate-spin" : ""} />
        <span>Reconnect</span>
      </button>
    </div>
    {#if harness?.available}
      <div class="rounded-xl border border-emerald-500/25 bg-emerald-500/5 p-3 space-y-1.5">
        <div class="flex items-center gap-2 text-emerald-400 font-medium">
          <ShieldCheck size={14} />
          <span>Connected — protocol {harness.protocol}, posture {harness.posture}</span>
        </div>
        <div class="font-mono text-[11px] text-textMuted break-all">{harness.binary}</div>
        <p class="text-textMuted leading-relaxed">
          Commits, pushes, merges, rebases, branch deletions, discards and conflict-editor
          saves are put to the harness's gates before they run. Hard rules — force pushes,
          verification bypass flags, writes to credential paths — are refused here.
        </p>
        <div class="flex flex-wrap gap-1 pt-1">
          {#each harness.ops as op}
            <span class="px-1.5 py-0.5 rounded-full bg-surfaceHover border border-border/80 font-mono text-[10px] text-textMuted">{op}</span>
          {/each}
        </div>
      </div>
    {:else}
      <div class="rounded-xl border border-amber-500/25 bg-amber-500/5 p-3 space-y-1.5">
        <div class="flex items-center gap-2 text-amber-400 font-medium">
          <ShieldAlert size={14} />
          <span>Unavailable</span>
        </div>
        <div class="text-textMuted">{harness?.error ?? "Not probed yet."}</div>
        <p class="text-textMuted leading-relaxed">
          GitPulse still works. Every action it performs without a gate is reported as
          <span class="text-amber-400">not checked</span> — never as approved. Install MANVI,
          or point <span class="font-mono">GITPULSE_MANVI_BIN</span> at the binary, then
          press Reconnect.
        </p>
      </div>
    {/if}
  </section>

  <!-- Model servers -->
  <section class="gp-card p-4 space-y-2">
    <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">Local model servers</h3>
    {#if ai}
      {#if ai.endpoints.filter((e) => e.reachable).length === 0}
        <div class="rounded-xl border border-border/70 bg-background p-3 text-textMuted leading-relaxed">
          {ai.detail || "No local model server answered."}
          <p class="mt-1.5">
            GitPulse only ever talks to a model server on this machine: the transport refuses
            any address that is not loopback, so a diff cannot leave the machine through a
            mistyped setting.
          </p>
        </div>
      {:else}
        <div class="space-y-2">
          {#each ai.endpoints.filter((e) => e.reachable) as endpoint}
            <div class="rounded-xl border border-border/70 bg-background p-3 space-y-2">
              <div class="flex items-center gap-2 text-textPrimary font-medium">
                <Server size={13} class="text-accent" />
                <span class="font-mono text-[11px]">{endpoint.base_url}</span>
              </div>
              <div class="grid grid-cols-1 gap-1">
                {#each endpoint.models as model}
                  <button
                    onclick={() => pick({ base_url: endpoint.base_url, model })}
                    class="px-2.5 py-1.5 rounded-full flex items-center justify-between gap-2 text-left transition-colors
                      {isSelected(endpoint.base_url, model)
                        ? 'bg-accent/15 text-accent border border-accent/40'
                        : 'hover:bg-surfaceHover border border-transparent text-textPrimary'}"
                  >
                    <span class="font-mono text-[11px] truncate">{model}</span>
                    {#if isSelected(endpoint.base_url, model)}
                      <Check size={13} />
                    {/if}
                  </button>
                {/each}
              </div>
            </div>
          {/each}
          {#if preferred}
            <button
              onclick={() => pick(null)}
              class="text-[11px] text-textMuted hover:text-textPrimary underline"
            >
              Clear the pinned model and let discovery choose
            </button>
          {/if}
        </div>
      {/if}

      {#if ai.model_info}
        <div class="rounded-xl border border-border/70 bg-background p-3 space-y-1">
          <div class="text-textPrimary font-medium">{ai.model_info.model}</div>
          <!-- Provenance, not just a number: a window read off the server
               and one typed into a default produce the same request and
               completely different confidence. -->
          <div class="text-textMuted">
            Context: {ai.model_info.describe}
            {#if !ai.model_info.discovered}
              <span class="text-amber-400"> (declared, not discovered)</span>
            {/if}
          </div>
          {#if ai.model_info.capabilities_known}
            <div class="flex flex-wrap gap-1 pt-1">
              {#if ai.model_info.supports_tools}<span class="px-1.5 py-0.5 rounded-full bg-surfaceHover border border-border/80 text-[10px]">tools</span>{/if}
              {#if ai.model_info.supports_vision}<span class="px-1.5 py-0.5 rounded-full bg-surfaceHover border border-border/80 text-[10px]">vision</span>{/if}
              {#if ai.model_info.supports_reasoning}<span class="px-1.5 py-0.5 rounded-full bg-surfaceHover border border-border/80 text-[10px]">reasoning</span>{/if}
            </div>
          {:else}
            <div class="text-textMuted">The server published no capability list.</div>
          {/if}
        </div>
      {:else if ai.model_detail}
        <div class="text-amber-400">{ai.model_detail}</div>
      {/if}
    {:else}
      <div class="text-textMuted">Probing…</div>
    {/if}
  </section>

  <!-- Branch naming -->
  {#if $repoStore.currentPath}
    <section class="gp-card p-4 space-y-2">
      <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">Name a branch for the work in progress</h3>
      <div class="flex items-center gap-2">
        <button
          onclick={suggestBranch}
          disabled={isSuggesting || !ai?.ready}
          class="gp-btn-primary"
        >
          <GitBranch size={13} />
          <span>{isSuggesting ? "Thinking…" : "Suggest a name"}</span>
        </button>
        {#if branchSuggestion}
          <input
            bind:value={branchSuggestion}
            class="gp-field flex-1 min-w-0 font-mono"
          />
          <button
            onclick={createSuggestedBranch}
            class="gp-btn"
          >
            Create
          </button>
        {/if}
      </div>
      {#if branchError}
        <div class="text-rose-400">{branchError}</div>
      {/if}
      {#each branchWarnings as warning}
        <div class="text-amber-400">{warning}</div>
      {/each}
    </section>
  {/if}

  <!-- Agent activity journal -->
  <section class="gp-card p-4 space-y-2">
    <div class="flex items-center justify-between">
      <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted flex items-center gap-1.5">
        <History size={11} />
        <span>Agent activity ({recentActions.length})</span>
      </h3>
      <div class="flex items-center gap-3">
        {#if recentActions.length > 0}
          <button
            onclick={() => void copyLog()}
            class="text-[10px] text-textMuted hover:text-textPrimary underline inline-flex items-center gap-1"
            title="Copy the activity log to the clipboard"
          >
            <Clipboard size={11} />
            <span>{logCopied ? "Copied" : "Copy log"}</span>
          </button>
          <button
            onclick={() => harnessStore.clearActions()}
            class="text-[10px] text-textMuted hover:text-textPrimary underline"
          >
            Clear
          </button>
        {/if}
      </div>
    </div>
    {#if recentActions.length === 0}
      <div class="rounded-xl border border-border/70 bg-background p-3 text-textMuted leading-relaxed">
        No actions yet. Every commit, push, rebase, discard, conflict save and worktree
        change GitPulse performs lands here — with whether a policy gate checked it.
      </div>
    {:else}
      <div class="rounded-xl border border-border/70 bg-background divide-y divide-border/40 max-h-56 overflow-y-auto">
        {#each recentActions as action (action.id)}
          <div class="px-3 py-1.5 flex items-center gap-2 text-[11px]" title={action.verdict?.detail ?? action.label}>
            <span class="font-mono text-[10px] text-textMuted shrink-0">{actionTime(action.ts)}</span>
            <span class="shrink-0 px-1.5 py-0.5 rounded-full font-mono text-[9px] uppercase border {verdictChip(
              !action.ok ? 'blocked' : (action.verdict?.status ?? 'unchecked')
            )}">
              {action.kind}
            </span>
            <span class="truncate {action.ok ? 'text-textPrimary' : 'text-rose-400'}">{action.label || "—"}</span>
          </div>
        {/each}
      </div>
    {/if}
  </section>
</div>
