<script lang="ts">
  import { harnessStore, verdictLabel, type AiSelection } from "../stores/harnessStore";
  import { repoStore } from "../stores/repoStore";
  import { copyText } from "../desktop/clipboard";
  import { invoke } from "@tauri-apps/api/core";
  import type { GrantView } from "../grants/types";
  import {
    contextWindowLabel,
    sweepSummary,
    toolSupportLabel,
    type ScanModel,
    type ScanResult,
  } from "../ai/scan";
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
    SquareTerminal,
    Wrench,
    Percent,
    Gauge,
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

  /**
   * The harness's grant ledger.
   *
   * A verdict whose status is `granted` says a rule fired and someone waived
   * it. Until this pane showed the ledger, there was no way to see who, why, or
   * until when — so a granted allow was indistinguishable from a clean one to
   * anyone reading the journal.
   */
  let grants = $state<GrantView | null>(null);
  let grantsRepo: string | null = null;
  $effect(() => {
    const repo = $repoStore.currentPath;
    // Guarded on a genuine repo switch: this effect re-runs on every store
    // emission, and the ~6s status poll is one.
    if (!repo || repo === grantsRepo) return;
    grantsRepo = repo;
    void invoke<GrantView>("cmd_grants_view", { repoPath: repo })
      .then((v) => {
        grants = v;
      })
      .catch(() => {
        // A failed read leaves the section hidden rather than claiming there
        // are no grants. The two are different and must not look the same.
        grants = null;
      });
  });

  /** Grants that have not expired, newest first. */
  let activeGrants = $derived(
    (grants?.grants ?? [])
      .filter((g) => !g.expires_at || Date.parse(g.expires_at) > Date.now())
      .slice()
      .reverse(),
  );

  type CapabilityTab = "health" | "coverage" | "terminal" | "github";

  function openCapability(tab: CapabilityTab) {
    if (!$repoStore.currentPath) return;
    repoStore.setActiveTab(tab);
  }

  function actionTime(ts: number): string {
    return new Date(ts).toLocaleTimeString();
  }

  function verdictChip(status: string): string {
    switch (status) {
      case "blocked":
        return "bg-rose-500/20 text-rose-300 border-rose-500/40";
      case "granted":
        return "bg-purple-500/20 text-purple-300 border-purple-500/40";
      case "demoted":
        return "bg-blue-500/20 text-blue-300 border-blue-500/40";
      case "widened":
        return "bg-amber-500/20 text-amber-300 border-amber-500/40";
      case "degraded":
        return "bg-yellow-500/20 text-yellow-300 border-yellow-500/40";
      case "warned":
        return "bg-sky-500/15 text-sky-300 border-sky-500/30";
      case "allowed":
        return "bg-emerald-500/10 text-emerald-300 border-emerald-500/25";
      case "unchecked":
      default:
        return "bg-zinc-500/15 text-zinc-400 border-zinc-500/30";
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

  /**
   * What the local-server sweep found.
   *
   * The endpoint list above reports servers GitPulse already resolved. This
   * answers the question it could not: what each of them actually serves, and
   * whether those models support the features about to be offered on them.
   */
  let scan = $state<ScanResult | null>(null);
  let scanning = $state(false);
  let scanError = $state<string | null>(null);

  async function runScan() {
    scanning = true;
    scanError = null;
    try {
      scan = await invoke<ScanResult>("cmd_local_scan");
    } catch (e) {
      // Reported, not swallowed: a sweep that failed is not a machine with no
      // models on it.
      scanError = formatError(e);
      scan = null;
    } finally {
      scanning = false;
    }
  }

  /** Models a chat feature can actually run on, given what the sweep asked. */
  function usable(model: ScanModel): boolean {
    // `capabilities_known` false means nobody asked, so nothing is excluded on
    // the strength of a flag nobody set.
    return !model.capabilities_known || model.supports_completion;
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

  <!-- The live `hello` response is intentionally narrow: GitPulse embeds
       MANVI's policy and local-model planes only. This card keeps that wire
       truth separate from app-owned, user-confirmed command execution. -->
  <section class="gp-card p-4 space-y-3">
    <div class="space-y-1">
      <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">Capability boundary</h3>
      <p class="text-textMuted leading-relaxed">
        No autonomous PTY or app-control API is exposed by <span class="font-mono">manvi serve</span>.
        GitPulse adds narrow, explicit controls around the planes the sidecar actually provides.
      </p>
    </div>

    <div class="grid gap-2 sm:grid-cols-2">
      <div class="rounded-xl border border-border/70 bg-background p-3 space-y-1">
        <div class="flex items-center gap-2 text-textPrimary font-medium">
          <SquareTerminal size={13} class="text-textMuted" />
          <span>Interactive shell</span>
          <span class="ml-auto text-[10px] uppercase text-amber-300">User only</span>
        </div>
        <p class="text-textMuted leading-relaxed">
          MANVI never receives the PTY handle or keystrokes. Shell commands are typed and owned by you.
        </p>
      </div>
      <div class="rounded-xl border border-emerald-500/25 bg-emerald-500/5 p-3 space-y-1">
        <div class="flex items-center gap-2 text-textPrimary font-medium">
          <ShieldCheck size={13} class="text-emerald-400" />
          <span>Scoped action runner</span>
          <span class="ml-auto text-[10px] uppercase text-emerald-300">Available</span>
        </div>
        <p class="text-textMuted leading-relaxed">
          Health and coverage commands require a click, a purpose allowlist, direct argv execution,
          the MANVI policy gate, hard timeouts, bounded output, and stop-on-failure accounting.
        </p>
      </div>
    </div>

    <div class="grid grid-cols-2 sm:grid-cols-4 gap-2">
      <button
        class="gp-btn justify-start"
        disabled={!$repoStore.currentPath}
        onclick={() => openCapability("health")}
        title="Scan dependencies, ask MANVI for a remediation plan, and run approved steps"
      >
        <Wrench size={12} /> Health fixes
      </button>
      <button
        class="gp-btn justify-start"
        disabled={!$repoStore.currentPath}
        onclick={() => openCapability("coverage")}
        title="Generate, scan, and analyze coverage reports. Rust needs cargo-llvm-cov; a full run can take several minutes."
      >
        <Percent size={12} /> Coverage
      </button>
      <button
        class="gp-btn justify-start"
        disabled={!$repoStore.currentPath}
        onclick={() => openCapability("terminal")}
        title="Open the user-owned shell and bounded console"
      >
        <SquareTerminal size={12} /> Terminal
      </button>
      <button
        class="gp-btn justify-start"
        disabled={!$repoStore.currentPath}
        onclick={() => openCapability("github")}
        title="Run this repository's bounded local CI pipeline"
      >
        <Gauge size={12} /> CI:local
      </button>
    </div>
  </section>

  <!-- Model servers -->
  <section class="gp-card p-4 space-y-2">
    <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">Local model servers</h3>
    {#if ai}
      <!--
        Discovery. Separate from the endpoint list above, which reports servers
        GitPulse resolved; this reports what they serve and what those models
        can do.
      -->
      <div class="flex items-center justify-between gap-2">
        <button
          onclick={() => void runScan()}
          disabled={scanning}
          class="text-[10px] text-textMuted hover:text-accent underline inline-flex items-center gap-1 disabled:opacity-50"
          title="Probe the well-known local endpoints and list what each server serves"
        >
          <RefreshCw size={11} class={scanning ? "animate-spin" : ""} />
          <span>{scanning ? "Scanning…" : "Scan local servers"}</span>
        </button>
        {#if scan}
          <span class="text-[10px] text-textMuted">{sweepSummary(scan)}</span>
        {/if}
      </div>

      {#if scanError}
        <div class="rounded-xl border border-amber-500/40 bg-amber-500/5 p-2.5 text-[11px] text-amber-400">
          The scan did not complete, so this is not a list of what is running: {scanError}
        </div>
      {:else if scan && scan.servers.length > 0}
        <div class="space-y-1.5">
          {#each scan.servers as server (server.base_url)}
            <div class="rounded-xl border border-border/70 bg-background p-2.5 space-y-1">
              <div class="flex items-center gap-2 text-[11px]">
                <Server size={12} class="text-accent shrink-0" />
                <span class="font-mono truncate">{server.base_url}</span>
                <span class="shrink-0 text-[9px] uppercase rounded-full bg-surfaceHover border border-border/80 px-1.5 text-textMuted">
                  {server.runtime}
                </span>
                {#if server.version}
                  <span class="shrink-0 text-[9px] text-textMuted">v{server.version}</span>
                {/if}
              </div>
              {#each server.models as model (model.id)}
                <button
                  onclick={() => pick({ base_url: server.base_url, model: model.id })}
                  class="w-full px-2 py-1 rounded-lg flex items-center gap-2 text-left text-[10px] transition-colors
                    {isSelected(server.base_url, model.id)
                      ? 'bg-accent/15 text-accent'
                      : 'hover:bg-surfaceHover text-textPrimary'}
                    {usable(model) ? '' : 'opacity-50'}"
                  title={usable(model)
                    ? `${contextWindowLabel(model)} · ${toolSupportLabel(model)}`
                    : "This model does not generate text — an embedding model answers the same listing as every chat model."}
                >
                  <span class="font-mono truncate flex-1">{model.id}</span>
                  <span class="shrink-0 text-textMuted">{contextWindowLabel(model)}</span>
                  <!--
                    Three states, not two. `capabilities_known` false means
                    nobody asked, and rendering that as "no tools" would make a
                    capable model look incapable.
                  -->
                  <span
                    class="shrink-0 text-[9px] rounded-full px-1.5 border
                      {!model.capabilities_known
                        ? 'text-textMuted border-border/80'
                        : model.supports_tools
                          ? 'text-accent border-accent/40'
                          : 'text-textMuted border-border/80'}"
                  >{toolSupportLabel(model)}</span>
                </button>
              {/each}
            </div>
          {/each}
        </div>
      {/if}

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

  <!-- Grant ledger: who waived which rule, and until when -->
  {#if grants?.available}
    <section class="gp-card p-4 space-y-2">
      <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted flex items-center gap-1.5">
        <ShieldAlert size={11} />
        <span>Grants ({activeGrants.length} active of {grants.grants.length})</span>
      </h3>
      {#if grants.error}
        <!--
          A ledger that exists and could not be parsed must never render as a
          repository where nothing was ever granted.
        -->
        <div class="rounded-xl border border-amber-500/40 bg-amber-500/5 p-3 text-[11px] text-amber-400 leading-relaxed">
          The grant ledger could not be read, so this list is not the whole
          truth: {grants.error}
        </div>
      {:else if grants.grants.length === 0}
        <div class="rounded-xl border border-border/70 bg-background p-3 text-textMuted leading-relaxed">
          No rule has been waived in this repository.
        </div>
      {:else}
        <div class="rounded-xl border border-border/70 bg-background divide-y divide-border/40 max-h-40 overflow-y-auto">
          {#each activeGrants as grant (grant.id)}
            <div class="px-3 py-1.5 flex flex-col gap-0.5 text-[11px]">
              <div class="flex items-center gap-1.5 min-w-0">
                <span class="font-mono text-[10px] text-accent shrink-0">{grant.scope.rule || "policy"}</span>
                <span class="truncate text-textPrimary">{grant.scope.target}</span>
                {#if grant.consumed}
                  <span class="ml-auto shrink-0 text-[9px] uppercase text-textMuted">used</span>
                {/if}
              </div>
              <div class="text-textMuted truncate">
                {grant.grantor.name || grant.grantor.authority || "unknown"}
                {#if grant.reason}· {grant.reason}{/if}
                {#if grant.expires_at}· expires {grant.expires_at}{/if}
              </div>
            </div>
          {/each}
        </div>
        <!--
          Revocation is Manvi's to perform. GitPulse is a read-only consumer of
          this state, and a second writer could interleave with the harness's
          own serialised writes to the same file.
        -->
        <p class="text-[10px] text-textMuted">
          Revoke with <code class="font-mono">manvi grants revoke &lt;id&gt;</code>; GitPulse only reads this ledger.
        </p>
      {/if}
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
          <div class="px-3 py-1.5 flex flex-col gap-0.5 text-[11px]" title={action.verdict?.detail ?? action.label}>
            <div class="flex items-center gap-2">
              <span class="font-mono text-[10px] text-textMuted shrink-0">{actionTime(action.ts)}</span>
              <span class="shrink-0 px-1.5 py-0.5 rounded-full font-mono text-[9px] uppercase border {verdictChip(
                !action.ok ? 'blocked' : (action.verdict?.status ?? 'unchecked')
              )}">
                {action.verdict?.status ?? action.kind}
              </span>
              <span class="truncate {action.ok ? 'text-textPrimary' : 'text-rose-400'}">{action.label || "—"}</span>
              {#if action.verdict?.task_id}
                <span class="ml-auto font-mono text-[9px] px-1.5 py-0.2 rounded bg-surfaceHover text-textMuted shrink-0">
                  {action.verdict.task_id}
                </span>
              {/if}
            </div>
            {#if action.verdict?.grant_id}
              <div class="text-[10px] text-purple-300/80 font-mono pl-14">
                Grant {action.verdict.grant_id} by {action.verdict.granted_by || "human"} · {action.verdict.reason || "waived"}
              </div>
            {:else if action.verdict?.demoted}
              <div class="text-[10px] text-blue-300/80 font-mono pl-14">
                Posture demoted: {action.verdict.demoted}
              </div>
            {:else if action.verdict?.widened}
              <div class="text-[10px] text-amber-300/80 font-mono pl-14">
                Scope widened: {action.verdict.widened}
              </div>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  </section>
</div>
