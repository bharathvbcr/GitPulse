<script lang="ts">
  import { SvelteSet } from "svelte/reactivity";
  import { repoStore } from "../stores/repoStore";
  import { graphStore } from "../stores/graphStore";
  import { invoke } from "@tauri-apps/api/core";
  import {
    Github,
    GitPullRequest,
    ExternalLink,
    Play,
    GitBranch,
    Tag,
    Workflow,
    RotateCcw,
    Square,
    Terminal,
  } from "lucide-svelte";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import type {
    GitHubContextBase,
    WorkflowInfo,
    WorkflowRunInfo,
    WorkflowsReport,
    CiLocalReport,
  } from "../github/types";
  import {
    canCancelRun,
    canRerunRun,
    ciLocalVerdict,
    ciStepClass,
    isWorkflowDispatchable,
    workflowStateLabel,
  } from "../github/runActions";
  import { formatReleaseDate } from "../ops/model";
  import { formatError } from "../ui/formatError";
  import EmptyState from "./EmptyState.svelte";

  interface PullRequestInfo {
    number: number;
    title: string;
    state: string;
    head_ref: string;
    base_ref: string;
    url: string;
    is_draft: boolean;
    ci_status: string;
  }

  interface GitHubContext extends GitHubContextBase {
    cli_present: boolean;
    host: string;
    html_url: string;
    pull_requests: PullRequestInfo[];
  }

  let ctx = $state<GitHubContext | null>(null);
  let loading = $state(false);
  /** PR numbers with a checkout in flight; any in-flight checkout disables all. */
  const checkingOut = new SvelteSet<number>();
  let inflight: AsyncGuard | null = null;

  // --- Actions workflows (CI/CD) ---
  let workflows = $state<WorkflowsReport | null>(null);
  let workflowsLoading = $state(false);
  let workflowsInflight: AsyncGuard | null = null;
  /** Ref a dispatched workflow runs at; prefilled from the current branch. */
  let dispatchRef = $state("");
  /** Selector of the one trigger in flight; non-null disables every action. */
  let triggering = $state(false);

  // --- Run actions (re-run / cancel) ---
  let busyRunAction = $state<string | null>(null);

  // --- CI:local ---
  let ciRunning = $state(false);
  let ciReport = $state<CiLocalReport | null>(null);
  let ciError = $state<string | null>(null);

  /** Panel-scoped feedback for the last attempted action; cleared on reload. */
  let actionNotice = $state<string | null>(null);
  let actionError = $state<string | null>(null);

  function emptyContext(error: string): GitHubContext {
    return {
      available: false,
      cli_present: false,
      host: "",
      owner: "",
      repo: "",
      html_url: "",
      pull_requests: [],
      workflow_runs: [],
      releases: [],
      releases_truncated: false,
      releases_error: null,
      error,
    };
  }

  async function loadFor(repo: string) {
    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    loading = true;
    try {
      const next = await invoke<GitHubContext>("cmd_github_context", { repoPath: repo });
      if (!guard.isLive()) return;
      ctx = next;
    } catch (err) {
      if (!guard.isLive()) return;
      ctx = emptyContext(formatError(err));
    } finally {
      if (guard.isLive()) loading = false;
    }
  }

  async function loadWorkflowsFor(repo: string) {
    workflowsInflight?.cancel();
    const guard = createAsyncGuard();
    workflowsInflight = guard;
    workflowsLoading = true;
    try {
      const next = await invoke<WorkflowsReport>("cmd_github_workflows", { repoPath: repo });
      if (!guard.isLive()) return;
      workflows = next;
    } catch (err) {
      if (!guard.isLive()) return;
      workflows = {
        available: false,
        cli_present: true,
        workflows: [],
        truncated: false,
        error: formatError(err),
      };
    } finally {
      if (guard.isLive()) workflowsLoading = false;
    }
  }

  async function loadAll() {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    actionNotice = null;
    actionError = null;
    await Promise.all([loadFor(repo), loadWorkflowsFor(repo)]);
  }

  $effect(() => {
    return () => {
      inflight?.cancel();
      workflowsInflight?.cancel();
    };
  });

  $effect(() => {
    const repo = $repoStore.currentPath;
    ctx = null;
    checkingOut.clear();
    workflows = null;
    ciReport = null;
    ciError = null;
    ciRunning = false;
    actionNotice = null;
    actionError = null;
    // Dispatch ref is repo-scoped: default to the checked-out branch.
    dispatchRef = $repoStore.currentBranch ?? "";
    if (!repo) {
      inflight?.cancel();
      workflowsInflight?.cancel();
      loading = false;
      workflowsLoading = false;
      return;
    }
    void loadFor(repo);
    void loadWorkflowsFor(repo);
    const startedCtx = inflight;
    const startedWf = workflowsInflight;
    return () => {
      if (inflight === startedCtx) startedCtx?.cancel();
      if (workflowsInflight === startedWf) startedWf?.cancel();
    };
  });

  function ciClass(status: string): string {
    const s = status.toLowerCase();
    if (s === "success" || s === "completed") return "text-green-400";
    if (s === "failure" || s === "cancelled" || s === "timed_out") return "text-red-400";
    if (s === "pending" || s === "in_progress" || s === "queued") return "text-amber-400";
    return "text-textMuted";
  }

  function runLabel(run: WorkflowRunInfo): string {
    return run.conclusion || run.status || "unknown";
  }

  async function openExternal(url: string) {
    // No window.open fallback: inside a Tauri webview it can navigate the
    // app shell itself, and these URLs come from advisory/GitHub payloads.
    // If the opener plugin fails, surfacing the failure beats handing the
    // webview to an arbitrary URL.
    try {
      await openUrl(url);
    } catch (err) {
      console.error("openUrl failed for", url, err);
    }
  }

  async function checkoutPr(number: number) {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    // Track per-id so concurrent checkouts cannot clobber each other's
    // spinner state; while any is running every button is disabled.
    checkingOut.add(number);
    try {
      await invoke("cmd_github_checkout_pr", {
        repoPath: repo,
        number,
      });
      if ($repoStore.currentPath !== repo) return;
      await repoStore.refresh(repo);
      if ($repoStore.currentPath !== repo) return;
      await graphStore.loadGraph(repo);
    } catch (err: unknown) {
      if ($repoStore.currentPath === repo) {
        repoStore.setError(formatError(err));
      }
    } finally {
      // Clear only this id unconditionally: this component remounts per repo
      // ({#key} on currentPath), so a stale settle after a switch cannot
      // clobber a newer instance — but a path-gated reset here would strand
      // the spinner when the user switches repos mid-checkout.
      checkingOut.delete(number);
    }
  }

  async function triggerWorkflow(workflow: WorkflowInfo) {
    const repo = $repoStore.currentPath;
    if (!repo || triggering || busyRunAction) return;
    const refName = dispatchRef.trim();
    if (!refName) {
      actionError = "Enter a branch or tag to dispatch against.";
      return;
    }
    triggering = true;
    actionError = null;
    actionNotice = null;
    try {
      const output = await invoke<string>("cmd_github_trigger_workflow", {
        repoPath: repo,
        workflow: workflow.path,
        gitRef: refName,
      });
      actionNotice =
        output ||
        `Dispatched ${workflow.name || workflow.path} at ${refName}. It may take a moment to appear.`;
      await Promise.all([loadFor(repo), loadWorkflowsFor(repo)]);
    } catch (err: unknown) {
      actionError = formatError(err);
    } finally {
      triggering = false;
    }
  }

  async function rerunRun(run: WorkflowRunInfo) {
    const repo = $repoStore.currentPath;
    if (!repo || busyRunAction || triggering) return;
    busyRunAction = `rerun:${run.id}`;
    actionError = null;
    actionNotice = null;
    try {
      const output = await invoke<string>("cmd_github_rerun_run", {
        repoPath: repo,
        runId: run.id,
      });
      actionNotice = output || `Re-running “${run.title || run.name}”.`;
      await loadFor(repo);
    } catch (err: unknown) {
      actionError = formatError(err);
    } finally {
      busyRunAction = null;
    }
  }

  async function cancelRun(run: WorkflowRunInfo) {
    const repo = $repoStore.currentPath;
    if (!repo || busyRunAction || triggering) return;
    busyRunAction = `cancel:${run.id}`;
    actionError = null;
    actionNotice = null;
    try {
      const output = await invoke<string>("cmd_github_cancel_run", {
        repoPath: repo,
        runId: run.id,
      });
      actionNotice = output || `Cancellation requested for “${run.title || run.name}”.`;
      await loadFor(repo);
    } catch (err: unknown) {
      actionError = formatError(err);
    } finally {
      busyRunAction = null;
    }
  }

  async function runCiLocally() {
    const repo = $repoStore.currentPath;
    if (!repo || ciRunning) return;
    ciRunning = true;
    ciReport = null;
    ciError = null;
    try {
      ciReport = await invoke<CiLocalReport>("cmd_ci_local", { repoPath: repo });
    } catch (err: unknown) {
      ciError = formatError(err);
    } finally {
      ciRunning = false;
    }
  }
</script>

<div class="flex-1 flex flex-col bg-background h-full text-xs font-sans p-4 overflow-auto">
  <div class="flex items-center justify-between mb-4">
    <div class="flex items-center gap-2">
      <Github size={18} class="text-accent" />
      <h2 class="text-sm font-semibold text-textPrimary">GitHub</h2>
      {#if ctx?.owner}
        <span class="text-textMuted font-mono">{ctx.owner}/{ctx.repo}</span>
      {/if}
    </div>
    <div class="flex items-center gap-2">
      {#if ctx?.html_url}
        <button type="button" class="gp-btn" onclick={() => ctx && openExternal(`${ctx.html_url}/actions`)}>
          Actions
        </button>
      {/if}
      <button type="button" class="gp-btn" onclick={runCiLocally} disabled={ciRunning}>
        <span class="inline-flex items-center gap-1.5">
          <Terminal size={13} />
          {ciRunning ? "Running…" : "Run CI locally"}
        </span>
      </button>
      <button type="button" onclick={loadAll} class="gp-btn">Refresh</button>
    </div>
  </div>

  {#if ciRunning}
    <div class="mb-4 p-3 rounded-xl border border-border/70 bg-surface text-textMuted text-xs max-w-xl">
      Running this repository's CI pipeline locally (type-check, tests, build, fmt, clippy, cargo test)…
    </div>
  {:else if ciError}
    <div class="mb-4 p-3 rounded-xl border border-red-500/30 bg-red-500/10 text-red-300 text-xs max-w-xl">
      CI:local could not start: {ciError}
    </div>
  {:else if ciReport}
    <div class="mb-4 p-3 rounded-xl border border-border/70 bg-surface max-w-4xl">
      <div class="flex items-center justify-between mb-2">
        <span class="font-medium {ciReport.failed > 0 ? 'text-red-300' : 'text-green-300'}">
          CI:local {ciLocalVerdict(ciReport)}
        </span>
        <span class="text-[11px] text-textMuted font-mono">
          {(ciReport.total_duration_ms / 1000).toFixed(1)}s total
        </span>
      </div>
      <div class="space-y-1">
        {#each ciReport.steps as step (step.name)}
          <details class="group rounded-lg px-2 py-1 hover:bg-surfaceHover/60">
            <summary class="flex cursor-pointer list-none items-center justify-between gap-3">
              <span class="flex min-w-0 items-center gap-2">
                <span class="{ciStepClass(step.status)} shrink-0 w-14">{step.status}</span>
                <span class="truncate text-textPrimary">{step.name}</span>
                <span class="truncate font-mono text-[10px] text-textMuted hidden md:inline">{step.command}</span>
              </span>
              <span class="shrink-0 text-[11px] text-textMuted font-mono">{(step.duration_ms / 1000).toFixed(1)}s</span>
            </summary>
            {#if step.detail || step.command}
              <pre class="mt-1 mb-1 ml-16 whitespace-pre-wrap break-all rounded bg-background p-2 text-[10px] leading-relaxed text-textMuted overflow-x-auto">{step.detail || ""}</pre>
            {/if}
          </details>
        {/each}
      </div>
    </div>
  {/if}

  {#if loading || workflowsLoading}
    <div class="text-textMuted">Loading GitHub status…</div>
  {:else if ctx?.error}
    <div class="p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-300 text-xs max-w-xl">
      {ctx.error}
      {#if ctx.html_url}
        <div class="mt-2">
          <button class="text-accent underline" onclick={() => ctx && openExternal(ctx.html_url)}>{ctx.html_url}</button>
        </div>
      {/if}
    </div>
  {:else if ctx}
    {#if actionNotice || actionError}
      <div
        class="mb-4 p-3 rounded-xl border text-xs max-w-3xl {actionError
          ? 'border-red-500/30 bg-red-500/10 text-red-300'
          : 'border-emerald-500/30 bg-emerald-500/10 text-emerald-300'}"
      >
        {actionError ?? actionNotice}
      </div>
    {/if}
    <div class="grid grid-cols-1 lg:grid-cols-2 xl:grid-cols-3 gap-5 max-w-7xl">
      <section>
        <h3 class="text-[11px] uppercase tracking-wider text-textMuted mb-2">Open pull requests</h3>
        {#if ctx.pull_requests.length === 0}
          <EmptyState icon={GitPullRequest} title="No open pull requests" compact />
        {:else}
          <div class="space-y-2">
            {#each ctx.pull_requests as pr}
              <div class="p-3.5 bg-surface border border-border/70 rounded-2xl shadow-card flex items-start justify-between gap-3 transition-[border-color,box-shadow] duration-150 hover:border-accent/40">
                <div class="min-w-0">
                  <div class="flex items-center gap-2 text-textPrimary font-medium flex-wrap">
                    <GitPullRequest size={14} class="text-accent shrink-0" />
                    <span class="truncate">#{pr.number} {pr.title}</span>
                    {#if pr.is_draft}
                      <span class="text-[10px] px-1.5 py-0.5 rounded-full bg-surfaceHover text-textMuted">draft</span>
                    {/if}
                  </div>
                  <div class="mt-1 text-[11px] text-textMuted font-mono">
                    {pr.head_ref} → {pr.base_ref}
                    <span class="{ciClass(pr.ci_status)} ml-2">CI {pr.ci_status}</span>
                  </div>
                </div>
                <div class="flex items-center gap-1 shrink-0">
                  <button
                    type="button"
                    onclick={() => void checkoutPr(pr.number)}
                    disabled={checkingOut.size > 0}
                    class="gp-icon-btn hover:text-accent disabled:opacity-40 disabled:cursor-not-allowed"
                    title={checkingOut.has(pr.number) ? "Checking out…" : "Checkout pull request"}
                    aria-label={checkingOut.has(pr.number) ? `Checking out PR #${pr.number}` : `Checkout pull request #${pr.number}`}
                  >
                    <GitBranch size={14} />
                  </button>
                  <button
                    type="button"
                    onclick={() => openExternal(pr.url)}
                    class="gp-icon-btn"
                    title="Open on GitHub"
                    aria-label={`Open PR #${pr.number} on GitHub`}
                  >
                    <ExternalLink size={14} />
                  </button>
                </div>
              </div>
            {/each}
          </div>
        {/if}
      </section>

      <section>
        <h3 class="text-[11px] uppercase tracking-wider text-textMuted mb-2">Workflows</h3>
        {#if workflows?.error}
          <div class="p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-300 text-xs">
            Workflow listing unavailable: {workflows.error}
          </div>
        {:else if !workflows || workflows.workflows.length === 0}
          <EmptyState icon={Workflow} title="No Actions workflows" compact />
        {:else}
          <div class="flex items-center gap-2 mb-2">
            <input
              class="gp-field flex-1 min-w-0"
              placeholder="branch or tag (dispatch ref)"
              bind:value={dispatchRef}
              aria-label="Dispatch ref"
            />
          </div>
          <div class="space-y-2">
            {#each workflows.workflows as wf (wf.id)}
              <div class="p-3.5 bg-surface border border-border/70 rounded-2xl shadow-card flex items-start justify-between gap-3 transition-[border-color,box-shadow] duration-150 hover:border-accent/40">
                <div class="min-w-0">
                  <div class="flex items-center gap-2 text-textPrimary font-medium flex-wrap">
                    <Workflow size={14} class="text-accent shrink-0" />
                    <span class="truncate">{wf.name}</span>
                    <span class="text-[10px] px-1.5 py-0.5 rounded-full {isWorkflowDispatchable(wf.state) ? 'bg-emerald-500/10 border border-emerald-500/30 text-emerald-400' : 'bg-surfaceHover text-textMuted'}">
                      {workflowStateLabel(wf.state)}
                    </span>
                  </div>
                  <div class="mt-1 text-[11px] text-textMuted font-mono truncate">{wf.path}</div>
                </div>
                <button
                  type="button"
                  onclick={() => void triggerWorkflow(wf)}
                  disabled={!isWorkflowDispatchable(wf.state) || triggering || busyRunAction !== null}
                  class="gp-icon-btn hover:text-accent disabled:opacity-40 disabled:cursor-not-allowed shrink-0"
                  title={isWorkflowDispatchable(wf.state)
                    ? `Dispatch ${wf.name} at ${dispatchRef.trim() || "<ref>"}`
                    : "Only active workflows can be dispatched"}
                  aria-label={isWorkflowDispatchable(wf.state)
                    ? `Dispatch ${wf.name} at ${dispatchRef.trim() || "<ref>"}`
                    : `Workflow ${wf.name} cannot be dispatched`}
                >
                  <Play size={14} />
                </button>
              </div>
            {/each}
          </div>
          {#if workflows.truncated}
            <div class="mt-2 text-amber-400 text-[11px]">Showing {workflows.workflows.length} workflows; more exist.</div>
          {/if}
        {/if}
      </section>

      <section>
        <div class="flex items-center justify-between mb-2">
          <h3 class="text-[11px] uppercase tracking-wider text-textMuted">Releases</h3>
          {#if (ctx.releases?.length ?? 0) > 0}
            <span class="gp-pill text-[10px]">{ctx.releases.length}</span>
          {/if}
        </div>
        {#if ctx.releases_error}
          <div class="p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-300 text-xs">
            Release listing unavailable: {ctx.releases_error}
          </div>
        {:else if !ctx.releases || ctx.releases.length === 0}
          <EmptyState icon={Tag} title="No releases found" compact />
        {:else}
          <div class="space-y-2">
            {#each ctx.releases as release (release.tag_name || release.name)}
              <div class="p-3.5 bg-surface border border-border/70 rounded-2xl shadow-card flex items-start justify-between gap-3 transition-[border-color,box-shadow] duration-150 hover:border-accent/40">
                <div class="min-w-0 flex-1">
                  <div class="flex items-center gap-2 text-textPrimary font-medium flex-wrap">
                    <Tag size={14} class="text-accent shrink-0" />
                    {#if release.tag_name}
                      <span class="font-mono text-accent">{release.tag_name}</span>
                    {/if}
                    {#if release.name && release.name !== release.tag_name}
                      <span class="truncate">{release.name}</span>
                    {/if}
                    {#if release.is_latest}
                      <span class="text-[10px] px-1.5 py-0.5 rounded-full bg-emerald-500/10 border border-emerald-500/30 text-emerald-400 font-medium">latest</span>
                    {/if}
                    {#if release.is_prerelease}
                      <span class="text-[10px] px-1.5 py-0.5 rounded-full bg-amber-500/10 border border-amber-500/30 text-amber-400 font-medium">pre-release</span>
                    {/if}
                    {#if release.is_draft}
                      <span class="text-[10px] px-1.5 py-0.5 rounded-full bg-surfaceHover text-textMuted">draft</span>
                    {/if}
                  </div>
                  {#if release.published_at || release.created_at}
                    <div class="mt-1 text-[11px] text-textMuted">
                      {formatReleaseDate(release.published_at || release.created_at)}
                    </div>
                  {/if}
                </div>
                {#if release.url}
                  <button
                    type="button"
                    onclick={() => openExternal(release.url)}
                    class="gp-icon-btn shrink-0"
                    title="Open release on GitHub"
                    aria-label={`Open release ${release.tag_name || release.name} on GitHub`}
                  >
                    <ExternalLink size={14} />
                  </button>
                {/if}
              </div>
            {/each}
          </div>
          {#if ctx.releases_truncated}
            <div class="mt-2 text-amber-400 text-[11px]">Showing {ctx.releases.length} releases; more releases exist.</div>
          {/if}
        {/if}
      </section>

      <section>
        <h3 class="text-[11px] uppercase tracking-wider text-textMuted mb-2">Workflow runs</h3>
        {#if ctx.workflow_runs.length === 0}
          <EmptyState icon={Play} title="No recent Actions runs" compact />
        {:else}
          <div class="space-y-2">
            {#each ctx.workflow_runs as run (run.id)}
              <div class="p-3.5 bg-surface border border-border/70 rounded-2xl shadow-card flex items-start justify-between gap-3 transition-[border-color,box-shadow] duration-150 hover:border-accent/40">
                <div class="min-w-0">
                  <div class="flex items-center gap-2 text-textPrimary font-medium">
                    <Play size={14} class="text-accent shrink-0" />
                    <span class="truncate">{run.title || run.name}</span>
                  </div>
                  <div class="mt-1 text-[11px] text-textMuted font-mono">
                    {run.name}
                    {#if run.head_branch}
                      · {run.head_branch}
                    {/if}
                    <span class="{ciClass(runLabel(run))} ml-2">{runLabel(run)}</span>
                  </div>
                </div>
                <div class="flex items-center gap-1 shrink-0">
                  {#if canCancelRun(run)}
                    <button
                      type="button"
                      onclick={() => void cancelRun(run)}
                      disabled={busyRunAction !== null || triggering}
                      class="gp-icon-btn hover:text-red-400 disabled:opacity-40 disabled:cursor-not-allowed"
                      title={busyRunAction === `cancel:${run.id}` ? "Cancelling…" : "Cancel run"}
                      aria-label={busyRunAction === `cancel:${run.id}` ? `Cancelling run ${run.title || run.name}` : `Cancel run ${run.title || run.name}`}
                    >
                      <Square size={13} />
                    </button>
                  {:else if canRerunRun(run)}
                    <button
                      type="button"
                      onclick={() => void rerunRun(run)}
                      disabled={busyRunAction !== null || triggering}
                      class="gp-icon-btn hover:text-accent disabled:opacity-40 disabled:cursor-not-allowed"
                      title={busyRunAction === `rerun:${run.id}` ? "Re-running…" : "Re-run workflow"}
                      aria-label={busyRunAction === `rerun:${run.id}` ? `Re-running run ${run.title || run.name}` : `Re-run workflow ${run.title || run.name}`}
                    >
                      <RotateCcw size={13} />
                    </button>
                  {/if}
                  <button
                    type="button"
                    onclick={() => openExternal(run.url)}
                    class="gp-icon-btn"
                    title="Open workflow run"
                    aria-label={`Open workflow run ${run.title || run.name}`}
                  >
                    <ExternalLink size={14} />
                  </button>
                </div>
              </div>
            {/each}
          </div>
        {/if}
      </section>
    </div>
  {/if}
</div>
