<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { graphStore } from "../stores/graphStore";
  import { invoke } from "@tauri-apps/api/core";
  import { Github, GitPullRequest, ExternalLink, Play, GitBranch, Tag } from "lucide-svelte";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import type { GitHubContextBase, WorkflowRunInfo } from "../github/types";
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
  let checkingOut = $state<Set<number>>(new Set());
  let inflight: AsyncGuard | null = null;

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

  async function load() {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    await loadFor(repo);
  }

  $effect(() => {
    return () => inflight?.cancel();
  });

  $effect(() => {
    const repo = $repoStore.currentPath;
    ctx = null;
    checkingOut = new Set();
    if (!repo) {
      inflight?.cancel();
      loading = false;
      return;
    }
    void loadFor(repo);
    const started = inflight;
    return () => {
      if (inflight === started) {
        started?.cancel();
      }
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
      <button type="button" onclick={load} class="gp-btn">Refresh</button>
    </div>
  </div>

  {#if loading}
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
                  >
                    <GitBranch size={14} />
                  </button>
                  <button
                    type="button"
                    onclick={() => openExternal(pr.url)}
                    class="gp-icon-btn"
                    title="Open on GitHub"
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
                  >
                    <ExternalLink size={14} />
                  </button>
                {/if}
              </div>
            {/each}
          </div>
          {#if ctx.releases_truncated}
            <div class="mt-2 text-amber-400 text-[11px]">Showing 50 releases; more releases exist.</div>
          {/if}
        {/if}
      </section>

      <section>
        <h3 class="text-[11px] uppercase tracking-wider text-textMuted mb-2">Workflow runs</h3>
        {#if ctx.workflow_runs.length === 0}
          <EmptyState icon={Play} title="No recent Actions runs" compact />
        {:else}
          <div class="space-y-2">
            {#each ctx.workflow_runs as run}
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
                <button
                  type="button"
                  onclick={() => openExternal(run.url)}
                  class="gp-icon-btn"
                  title="Open workflow run"
                >
                  <ExternalLink size={14} />
                </button>
              </div>
            {/each}
          </div>
        {/if}
      </section>
    </div>
  {/if}
</div>
