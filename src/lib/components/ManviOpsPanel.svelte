<script lang="ts">
  import { onMount, tick } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { openExternal as openExternalUrl } from "../desktop/openExternal";
  import { formatError } from "../ui/formatError";
  import { reportPanelError } from "../diagnostics/report";
  import {
    AlertTriangle,
    ArrowDownToLine,
    ArrowUpFromLine,
    Bug,
    CheckCircle2,
    GitBranch,
    LoaderCircle,
    RefreshCw,
    Rocket,
    ScanSearch,
    ShieldCheck,
    Tag,
    Trash2,
  } from "lucide-svelte";
  import { repoStore } from "../stores/repoStore";
  import { askConfirm } from "../stores/modalStore";
  import { harnessStore, verdictLabel } from "../stores/harnessStore";
  import { harnessPermissionMode } from "../harness/availability";
  import {
    formatReleaseDate,
    releaseTagSuggestion,
    summarizeCommitReview,
    type BranchCleanupPlan,
    type CommitReviewReport,
  } from "../ops/model";
  import ManviHarnessPane from "./ManviHarnessPane.svelte";
  import {
    MANVI_FOCUS_TARGETS,
    MANVI_PANE_LIST,
    MANVI_PANES,
    manviFocusRequest,
    manviSectionId,
    takeManviFocus,
    type ManviFocusId,
    type ManviPane,
  } from "../ui/manviFocus";
  import type { WorkflowRunInfo, GitHubContext } from "../github/types";


  const ISSUE_REFRESH_MS = 60_000;

  /** One MANVI surface, two panes: guarded operations, and harness/AI controls. */
  let pane = $state<ManviPane>("ops");
  let permissionMode = $derived(harnessPermissionMode($harnessStore.harness));
  let permissionLabel = $derived(
    permissionMode === "connected"
      ? "policy connected"
      : permissionMode === "unguarded"
        ? "MANVI not installed"
        : permissionMode === "blocked"
          ? "mutations blocked"
          : "checking policy",
  );

  let subtitle = $derived(MANVI_PANES[pane].summary);

  let cleanup = $state<BranchCleanupPlan | null>(null);
  let selectedBranches = $state<string[]>([]);
  let review = $state<CommitReviewReport | null>(null);
  let github = $state<GitHubContext | null>(null);
  let busy = $state<string | null>(null);
  /**
   * Set only while the once-a-minute poll refreshes issues in the
   * background. Unlike `busy`, it disables nothing and spins nothing: an
   * automatic refresh must not freeze every quick-action card or spin the
   * header icon once a minute.
   */
  let backgroundRefreshing = $state(false);
  /**
   * Repo whose issue load was requested while an operation held `busy`
   * (e.g. switching repos mid-op). The repo-change effect below fires once
   * per real change; without this queue the new repo's issues would not
   * load until the next interval tick.
   */
  let pendingIssueRepo: string | null = null;
  let notice = $state<string | null>(null);
  /** Background-poll failures, kept apart from action feedback. */
  let pollError = $state<string | null>(null);
  let issueTitle = $state("");
  let issueBody = $state("");
  let issueLabels = $state("bug");
  let releaseTag = $state("");
  let releaseMessage = $state("");
  let releaseConfirmed = $state(false);
  let lastRepo: string | null = null;

  let cleanupInvariant = $derived(
    cleanup
      ? cleanup.protected_branches + cleanup.unmerged_branches + cleanup.candidates.length ===
          cleanup.total_local_branches
      : true,
  );

  function isSelected(name: string): boolean {
    return selectedBranches.includes(name);
  }

  function toggleSelected(name: string) {
    selectedBranches = isSelected(name)
      ? selectedBranches.filter((item) => item !== name)
      : [...selectedBranches, name];
  }

  /**
   * Opens issue/release/run URLs through the canonical OS opener. The shared
   * opener throws on failure; there is deliberately no in-webview navigation
   * fallback — it could replace the whole app shell with the target URL.
   * Failures surface on the panel notice and in the diagnostics ring.
   */
  async function openExternal(url: string) {
    try {
      await openExternalUrl(url);
    } catch (err) {
      notice = reportPanelError("ops", err);
    }
  }

  /** Releases `busy` and drains any issue load an op had deferred. */
  function settleBusy() {
    busy = null;
    const wanted = pendingIssueRepo;
    if (wanted !== null) {
      pendingIssueRepo = null;
      void loadIssues(wanted);
    }
  }

  async function loadIssues(
    repo: string = $repoStore.currentPath ?? "",
    opts: { background?: boolean } = {},
  ) {
    if (!repo) return;
    if (busy !== null) {
      // An operation owns `busy`: a user-initiated request is remembered and
      // run on settle; the background poll simply waits for its next tick.
      if (!opts.background) {
        pendingIssueRepo = repo;
      }
      return;
    }
    if (opts.background) {
      if (backgroundRefreshing) return;
      backgroundRefreshing = true;
    } else {
      busy = "issues";
    }
    try {
      const next = await invoke<GitHubContext>("cmd_github_context", { repoPath: repo });
      if ($repoStore.currentPath === repo) {
        github = next;
        if (opts.background) pollError = null;
      }
    } catch (error) {
      if ($repoStore.currentPath !== repo) return;
      if (opts.background) {
        // A background poll must not clobber action feedback ("Issue
        // reported…"); its failures get their own quieter slot.
        pollError = formatError(error);
      } else {
        notice = formatError(error);
      }
    } finally {
      if (opts.background) backgroundRefreshing = false;
      else settleBusy();
    }
  }

  async function scanBranches() {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    busy = "branches";
    notice = null;
    try {
      const next = await invoke<BranchCleanupPlan>("cmd_branch_cleanup_plan", { repoPath: repo });
      if ($repoStore.currentPath !== repo) return;
      cleanup = next;
      selectedBranches = next.candidates.map((candidate) => candidate.name);
    } catch (error) {
      if ($repoStore.currentPath === repo) notice = formatError(error);
    } finally {
      settleBusy();
    }
  }

  async function cleanBranches() {
    if (selectedBranches.length === 0) return;
    const repo = $repoStore.currentPath;
    if (!repo) return;
    const names = [...selectedBranches];
    const confirmed = await askConfirm({
      title: "Delete merged branches",
      message: `Delete ${names.length} merged local branch${names.length === 1 ? "" : "es"}?\n\n${names.join("\n")}`,
      confirmLabel: "Delete",
    });
    if (!confirmed) {
      return;
    }
    busy = "clean";
    notice = null;
    let deleted = 0;
    const failures: string[] = [];
    try {
      for (const name of names) {
        if ($repoStore.currentPath !== repo) break;
        const outcome = await repoStore.deleteBranch(name, false);
        if (outcome.ok) deleted += 1;
        else failures.push(`${name}: ${outcome.error ?? "failed"}`);
      }
      if ($repoStore.currentPath === repo) {
        notice = failures.length
          ? `Deleted ${deleted} of ${names.length}. ${failures.join(" ")}`
          : `Deleted ${deleted} merged branch${deleted === 1 ? "" : "es"}.`;
        await scanBranches();
      }
    } finally {
      settleBusy();
    }
  }

  async function reviewCommits() {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    busy = "review";
    notice = null;
    try {
      const next = await invoke<CommitReviewReport>("cmd_review_outgoing_commits", { repoPath: repo });
      if ($repoStore.currentPath === repo) review = next;
    } catch (error) {
      if ($repoStore.currentPath === repo) notice = formatError(error);
    } finally {
      settleBusy();
    }
  }

  async function sync(kind: "pull" | "push") {
    busy = kind;
    notice = null;
    try {
      const outcome = kind === "pull" ? await repoStore.pull() : await repoStore.push();
      notice = outcome.ok
        ? `${kind === "pull" ? "Pull" : "Push"} completed${outcome.policy ? ` — ${verdictLabel(outcome.policy)}` : ""}.`
        : outcome.error ?? `${kind} failed`;
    } catch (error) {
      // runMutating resolves invoke failures itself; this keeps an unexpected
      // throw (verdict rendering, store internals) visible in the panel
      // instead of console-only, matching scanBranches/reportIssue.
      notice = formatError(error);
    } finally {
      settleBusy();
    }
  }

  async function reportIssue() {
    const title = issueTitle.trim();
    if (!title) return;
    busy = "report";
    notice = null;
    try {
      const labels = issueLabels.split(",").map((label) => label.trim()).filter(Boolean);
      const outcome = await repoStore.reportIssue(title, issueBody, labels);
      if (!outcome.ok) {
        notice = outcome.error ?? "Issue report failed.";
        return;
      }
      notice = `Issue reported${outcome.policy ? ` — ${verdictLabel(outcome.policy)}` : ""}.`;
      issueTitle = "";
      issueBody = "";
      try {
        if (outcome.output) await openExternal(outcome.output);
      } catch {
        // The issue exists even if opening it fails; the URL is in the journal.
      }
      await loadIssues();
    } catch (error) {
      notice = formatError(error);
    } finally {
      settleBusy();
    }
  }

  async function publishRelease() {
    if (!releaseConfirmed || !releaseTag.trim()) return;
    busy = "release";
    notice = null;
    try {
      const outcome = await repoStore.publishRelease(
        releaseTag.trim(),
        releaseMessage.trim() || `Release ${releaseTag.trim()}`,
      );
      if (!outcome.ok) {
        notice = outcome.error ?? "Release publish failed.";
        return;
      }
      notice = `Pushed ${outcome.output?.tag ?? releaseTag} to ${outcome.output?.remote ?? "the remote"}; the release workflow can now build the app.`;
      releaseConfirmed = false;
      await loadIssues();
    } catch (error) {
      notice = formatError(error);
    } finally {
      settleBusy();
    }
  }

  function runState(run: WorkflowRunInfo): string {
    return run.conclusion || run.status || "unknown";
  }

  /**
   * A deep link from elsewhere in the app (the header chips, the storage
   * panel) names a section, not just this view. Switching the pane is only
   * half the answer: the section still has to be brought on screen and marked,
   * or the reader lands mid-page with no idea which card answered their click.
   */
  let flashTimer: number | null = null;
  let flashed: HTMLElement | null = null;
  let realignTimers: number[] = [];

  function clearFlash() {
    if (flashTimer !== null) {
      window.clearTimeout(flashTimer);
      flashTimer = null;
    }
    // The marked section lives in this panel or in ManviHarnessPane, so the
    // class is applied to the node rather than bound in one component's markup.
    flashed?.classList.remove("gp-focus-flash");
    flashed = null;
  }

  function clearRealign() {
    for (const timer of realignTimers) window.clearTimeout(timer);
    realignTimers = [];
  }

  /**
   * Cards above the target keep settling for a moment after the view mounts —
   * a status probe resolves, the grant list arrives — and every height change
   * slides the section the reader was sent to, typically far enough to push its
   * heading off the top. Two bounded re-alignments cover the settle; a scroll
   * gesture cancels them, because yanking a reader back is worse than drift.
   */
  const REALIGN_DELAYS_MS = [0, 250];

  function keepAligned(section: HTMLElement) {
    clearRealign();
    for (const delay of REALIGN_DELAYS_MS) {
      realignTimers.push(
        window.setTimeout(() => section.scrollIntoView({ block: "start" }), delay),
      );
    }
    for (const event of ["wheel", "touchstart", "keydown"] as const) {
      window.addEventListener(event, clearRealign, { once: true, passive: true });
    }
  }

  async function revealFocus(id: ManviFocusId) {
    const target = MANVI_FOCUS_TARGETS[id];
    pane = target.pane;
    await tick();
    const section = document.getElementById(manviSectionId(id));
    if (!section) return;
    // Instant, like every other scroll-into-view in the app: a deep link should
    // land deterministically, and a smooth 2000px glide is both a lot of motion
    // and, in a webview that is not compositing, no scroll at all.
    section.scrollIntoView({ block: "start" });
    keepAligned(section);
    // Sections carry tabindex="-1": keyboard and screen-reader users land on
    // the same card the scroll brought into view, not back at the page top.
    section.focus({ preventScroll: true });
    clearFlash();
    section.classList.add("gp-focus-flash");
    flashed = section;
    flashTimer = window.setTimeout(() => clearFlash(), 1600);
  }

  // Runs on mount (the view mounts lazily, after the click) and again whenever
  // a request arrives while the view is already open.
  $effect(() => {
    const requested = $manviFocusRequest;
    if (!requested) return;
    takeManviFocus();
    void revealFocus(requested);
  });

  $effect(() => {
    return () => {
      clearFlash();
      clearRealign();
      for (const event of ["wheel", "touchstart", "keydown"] as const) {
        window.removeEventListener(event, clearRealign);
      }
    };
  });

  onMount(() => {
    const timer = window.setInterval(() => {
      // Only poll while the ops pane is visible: the harness pane renders
      // this panel's data away, and each tick costs four parallel gh round
      // trips on the backend.
      if (!document.hidden && pane === "ops" && $repoStore.currentPath) {
        void loadIssues(undefined, { background: true });
      }
    }, ISSUE_REFRESH_MS);
    return () => window.clearInterval(timer);
  });

  // `lastRepo` memoizes this effect's real dependency: repoStore republishes
  // fresh objects on every status poll (~6s), and re-running the resets per
  // emission would thrash panel state. A load skipped because an op holds
  // `busy` is queued in pendingIssueRepo and drained by settleBusy().
  $effect(() => {
    const repo = $repoStore.currentPath;
    if (repo === lastRepo) return;
    lastRepo = repo;
    cleanup = null;
    selectedBranches = [];
    review = null;
    github = null;
    notice = null;
    pollError = null;
    releaseTag = releaseTagSuggestion($repoStore.tags.map((tag) => tag.name));
    releaseMessage = `Release ${releaseTag}`;
    releaseConfirmed = false;
    pendingIssueRepo = null; // a queued request for an older repo is obsolete
    if (repo) void loadIssues(repo);
  });
</script>

<div class="flex-1 overflow-auto bg-background p-4 text-xs text-textPrimary">
  <div class="mx-auto flex w-full max-w-6xl flex-col gap-4">
    <div class="flex items-start justify-between gap-4">
      <div>
        <div class="flex items-center gap-2">
          <ShieldCheck size={19} class="text-accent" />
          <h2 class="text-base font-semibold">MANVI</h2>
          <span class="gp-pill">{permissionLabel}</span>
        </div>
        <p class="mt-1 text-textMuted">{subtitle}</p>
      </div>
      <div class="flex items-center gap-2 shrink-0">
        {#if pane === "ops"}
          <button class="gp-btn" onclick={() => { void scanBranches(); void reviewCommits(); void loadIssues(); }} disabled={busy !== null}>
            <RefreshCw size={13} class={busy ? "animate-spin" : ""} /> Refresh all
          </button>
        {/if}
        <div class="gp-segmented" role="group" aria-label="MANVI view">
          {#each MANVI_PANE_LIST as entry (entry.id)}
            <button type="button" aria-pressed={pane === entry.id} data-active={pane === entry.id ? "true" : "false"} class="gp-seg-btn !text-[11px] !py-1" onclick={() => (pane = entry.id)}>{entry.label}</button>
          {/each}
        </div>
      </div>
    </div>

    {#if notice}
      <div class="rounded-xl border border-border bg-surface px-3 py-2 text-textSecondary">{notice}</div>
    {/if}

    {#if pollError}
      <div class="rounded-xl border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-amber-300">
        Background refresh failed: {pollError}
      </div>
    {/if}

    {#if pane === "harness"}
      <ManviHarnessPane />
    {:else}
    <section class="grid gap-3 md:grid-cols-2 lg:grid-cols-4">
      <button class="gp-card flex items-center gap-3 p-4 text-left hover:border-accent/50" onclick={() => sync("pull")} disabled={busy !== null}>
        <ArrowDownToLine size={20} class="text-accent" />
        <span><strong class="block">Quick pull</strong><span class="text-textMuted">Update the working branch through MANVI.</span></span>
      </button>
      <button class="gp-card flex items-center gap-3 p-4 text-left hover:border-accent/50" onclick={() => sync("push")} disabled={busy !== null}>
        <ArrowUpFromLine size={20} class="text-accent" />
        <span><strong class="block">Quick push</strong><span class="text-textMuted">Publish the current branch safely.</span></span>
      </button>
      <button class="gp-card flex items-center gap-3 p-4 text-left hover:border-accent/50" onclick={scanBranches} disabled={busy !== null}>
        <GitBranch size={20} class="text-accent" />
        <span><strong class="block">Scan branches</strong><span class="text-textMuted">Find merged local branches only.</span></span>
      </button>
      <button class="gp-card flex items-center gap-3 p-4 text-left hover:border-accent/50" onclick={reviewCommits} disabled={busy !== null}>
        <ScanSearch size={20} class="text-accent" />
        <span><strong class="block">Review messages</strong><span class="text-textMuted">Audit commits that are about to ship.</span></span>
      </button>
    </section>

    <div class="grid gap-4 xl:grid-cols-2">
      <section id={manviSectionId("cleanup")} tabindex="-1" class="gp-card p-4">
        <div class="mb-3 flex items-center justify-between">
          <div>
            <h3 class="font-semibold">Branch cleanup</h3>
            <p class="text-textMuted">Dry-run first; current, default, worktree, and unmerged branches stay protected.</p>
          </div>
          {#if cleanup}<span class="gp-pill">{cleanup.candidates.length} eligible / {cleanup.total_local_branches} local</span>{/if}
        </div>
        {#if busy === "branches" || busy === "clean"}
          <div class="flex items-center gap-2 py-6 text-textMuted"><LoaderCircle size={15} class="animate-spin" /> Inspecting Git refs…</div>
        {:else if cleanup}
          {#if !cleanupInvariant}
            <div class="mb-2 flex items-center gap-2 text-rose-400"><AlertTriangle size={14} /> Cleanup coverage is inconsistent; deletion is disabled.</div>
          {/if}
          <div class="max-h-52 space-y-1 overflow-auto">
            {#each cleanup.candidates as branch (branch.name)}
              <label class="flex cursor-pointer items-center gap-2 rounded-lg px-2 py-1.5 hover:bg-surfaceHover">
                <input type="checkbox" checked={isSelected(branch.name)} onchange={() => toggleSelected(branch.name)} />
                <span class="min-w-0 flex-1"><span class="font-mono">{branch.name}</span><span class="ml-2 truncate text-textMuted">{branch.last_summary}</span></span>
                {#if branch.upstream_gone}<span class="gp-pill">upstream gone</span>{/if}
              </label>
            {:else}
              <div class="flex items-center gap-2 py-5 text-textMuted"><CheckCircle2 size={15} class="text-green-400" /> No merged local branches need cleanup.</div>
            {/each}
          </div>
          <div class="mt-3 flex items-center justify-between border-t border-border pt-3 text-textMuted">
            <span>{cleanup.protected_branches} protected · {cleanup.unmerged_branches} unmerged</span>
            <button class="gp-btn" onclick={cleanBranches} disabled={!cleanupInvariant || selectedBranches.length === 0 || busy !== null}>
              <Trash2 size={13} /> Delete {selectedBranches.length} selected
            </button>
          </div>
        {:else}
          <button class="gp-btn" onclick={scanBranches}><GitBranch size={13} /> Build cleanup plan</button>
        {/if}
      </section>

      <section class="gp-card p-4">
        <div class="mb-3 flex items-center justify-between">
          <div><h3 class="font-semibold">Outgoing commit review</h3><p class="text-textMuted">Conventional format, duplicate subjects, issue links, and length.</p></div>
          {#if review}<span class="gp-pill">{review.findings.length} findings</span>{/if}
        </div>
        {#if busy === "review"}
          <div class="flex items-center gap-2 py-6 text-textMuted"><LoaderCircle size={15} class="animate-spin" /> Reviewing commit headers…</div>
        {:else if review}
          <p class="mb-2 text-textSecondary">{summarizeCommitReview(review)} <span class="font-mono text-textMuted">{review.range}</span></p>
          <div class="max-h-56 space-y-1 overflow-auto">
            {#each review.findings as finding (`${finding.commit_id}:${finding.code}`)}
              <div class="rounded-lg border border-border px-2.5 py-2">
                <div class="flex items-center gap-2"><span class="font-mono text-accent">{finding.short_id}</span><span class={finding.severity === "error" ? "text-rose-400" : finding.severity === "warning" ? "text-amber-400" : "text-textMuted"}>{finding.code}</span></div>
                <div class="mt-0.5 truncate">{finding.subject}</div><div class="text-textMuted">{finding.detail}</div>
              </div>
            {:else}
              <div class="flex items-center gap-2 py-5 text-textMuted"><CheckCircle2 size={15} class="text-green-400" /> No commit-message findings.</div>
            {/each}
          </div>
        {:else}
          <button class="gp-btn" onclick={reviewCommits}><ScanSearch size={13} /> Review outgoing commits</button>
        {/if}
      </section>

      <section class="gp-card p-4">
        <div class="mb-3 flex items-center justify-between">
          <div><h3 class="font-semibold">Issue monitor</h3><p class="text-textMuted">Refreshes open GitHub issues every minute while this view is visible.</p></div>
          <button class="gp-icon-btn" title="Refresh issues" onclick={() => loadIssues()} disabled={busy !== null}><RefreshCw size={13} class={busy === "issues" ? "animate-spin" : ""} /></button>
        </div>
        {#if github?.error}
          <div class="text-amber-400">{github.error}</div>
        {:else if github?.issues_error}
          <div class="flex items-center gap-2 text-amber-400"><AlertTriangle size={14} /> Issue monitor unavailable: {github.issues_error}</div>
        {:else}
          <div class="max-h-44 space-y-1 overflow-auto">
            {#each github?.issues ?? [] as issue (issue.number)}
              <button class="flex w-full items-center gap-2 rounded-lg px-2 py-1.5 text-left hover:bg-surfaceHover" onclick={() => openExternal(issue.url)}>
                <Bug size={13} class="text-accent" /><span class="font-mono text-textMuted">#{issue.number}</span><span class="min-w-0 flex-1 truncate">{issue.title}</span>
                {#if issue.labels[0]}<span class="gp-pill">{issue.labels[0]}</span>{/if}
              </button>
            {:else}
              <div class="py-3 text-textMuted">No open issues found.</div>
            {/each}
          </div>
          {#if github?.issues_truncated}<div class="mt-1 text-amber-400">Showing 50 issues; more open issues exist. This is not complete coverage.</div>{/if}
        {/if}
        <div class="mt-3 space-y-2 border-t border-border pt-3">
          <input class="gp-input w-full" maxlength="256" placeholder="Issue title" bind:value={issueTitle} />
          <textarea class="gp-input min-h-20 w-full resize-y" maxlength="65536" placeholder="What happened, what you expected, and how to reproduce it" bind:value={issueBody}></textarea>
          <div class="flex gap-2"><input class="gp-input min-w-0 flex-1" placeholder="labels, comma-separated" bind:value={issueLabels} /><button class="gp-btn" onclick={reportIssue} disabled={!issueTitle.trim() || busy !== null}><Bug size={13} /> Report issue</button></div>
        </div>
      </section>

      <section class="gp-card p-4">
        <div class="mb-3 flex items-center justify-between">
          <div>
            <h3 class="font-semibold">Release monitor & publication</h3>
            <p class="text-textMuted">Monitors published releases and pushes annotated SemVer tags.</p>
          </div>
          {#if (github?.releases?.length ?? 0) > 0}
            <span class="gp-pill">{github?.releases?.length ?? 0} releases</span>
          {/if}
        </div>

        {#if github?.error}
          <div class="mb-3 text-amber-400">{github.error}</div>
        {:else if github?.releases_error}
          <div class="mb-3 flex items-center gap-2 text-amber-400">
            <AlertTriangle size={14} /> Release monitor unavailable: {github.releases_error}
          </div>
        {:else if (github?.releases?.length ?? 0) > 0}
          <div class="mb-3 max-h-44 space-y-1 overflow-auto">
            {#each github?.releases ?? [] as release (release.tag_name || release.name)}
              <button
                class="flex w-full items-center justify-between rounded-lg px-2 py-1.5 text-left hover:bg-surfaceHover"
                onclick={() => release.url && openExternal(release.url)}
              >
                <div class="flex items-center gap-2 min-w-0">
                  <Tag size={13} class="text-accent shrink-0" />
                  <span class="font-mono text-textPrimary font-medium truncate">{release.tag_name}</span>
                  {#if release.name && release.name !== release.tag_name}
                    <span class="text-textMuted truncate max-w-xs">{release.name}</span>
                  {/if}
                  {#if release.is_latest}
                    <span class="gp-pill !bg-emerald-500/10 !text-emerald-400 !border-emerald-500/30">latest</span>
                  {/if}
                  {#if release.is_prerelease}
                    <span class="gp-pill !bg-amber-500/10 !text-amber-400 !border-amber-500/30">pre-release</span>
                  {/if}
                  {#if release.is_draft}
                    <span class="gp-pill">draft</span>
                  {/if}
                </div>
                {#if release.published_at || release.created_at}
                  <span class="text-textMuted text-[11px] shrink-0 font-mono">
                    {formatReleaseDate(release.published_at || release.created_at)}
                  </span>
                {/if}
              </button>
            {/each}
          </div>
          {#if github?.releases_truncated}
            <div class="mb-3 text-amber-400 text-[11px]">Showing 50 releases; more releases exist. This is not complete coverage.</div>
          {/if}
        {:else if github}
          <div class="mb-3 py-2 text-textMuted">No releases found on GitHub.</div>
        {/if}

        <div class="space-y-2 border-t border-border pt-3">
          <div class="text-xs font-semibold text-textPrimary">Publish new release tag</div>
          <input class="gp-input w-full font-mono" placeholder="v1.2.3" bind:value={releaseTag} />
          <input class="gp-input w-full" maxlength="4096" placeholder="Release message" bind:value={releaseMessage} />
          <label class="flex items-start gap-2 rounded-lg bg-surfaceHover p-2 text-textSecondary"><input class="mt-0.5" type="checkbox" bind:checked={releaseConfirmed} /><span>I confirm the working tree is ready. GitPulse will still require a clean, fully synchronized default branch and will refuse duplicate remote tags.</span></label>
          <button class="gp-btn-primary w-full justify-center" onclick={publishRelease} disabled={!releaseConfirmed || !releaseTag.trim() || busy !== null}>
            {#if busy === "release"}<LoaderCircle size={14} class="animate-spin" />{:else}<Rocket size={14} />{/if} Push release tag
          </button>
        </div>
        {#if (github?.workflow_runs?.length ?? 0) > 0}
          <div class="mt-3 border-t border-border pt-3"><div class="mb-1 text-textMuted">Recent release/workflow activity</div>{#each github?.workflow_runs.slice(0, 3) ?? [] as run (run.id)}<button class="flex w-full items-center justify-between rounded px-1 py-1 text-left hover:bg-surfaceHover" onclick={() => openExternal(run.url)}><span class="truncate">{run.title || run.name}</span><span class="gp-pill">{runState(run)}</span></button>{/each}</div>
        {/if}
      </section>
    </div>
    {/if}
  </div>
</div>
