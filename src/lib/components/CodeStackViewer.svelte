<script module lang="ts">
  import { createRepoPanelCache } from "../panels/repoPanelCache";


  // Survives the per-tab remount so revisiting the stack view renders the
  // last-known hierarchy instantly; the fetch then refreshes it in place.
  const stackCache = createRepoPanelCache<StackHierarchyPayload>();
</script>

<script lang="ts">
  import type {
    StackHierarchyPayload,
    StackedBranchNode,
  } from "../stack/types";
  import {
    cascadePlan,
    describeCascade,
    rootlessBranches,
    stackBranchFacts,
    stackTreeRows,
    type RestackStep,
  } from "../stack/tree";
  import { repoStore } from "../stores/repoStore";
  import { harnessStore, type Guarded } from "../stores/harnessStore";
  import { askConfirm } from "../stores/modalStore";
  import { invoke } from "@tauri-apps/api/core";
  import {
    Layers,
    GitBranch,
    RefreshCw,
    ArrowUp,
    ArrowDown,
    ArrowUpFromLine,
    CircleAlert,
    Info,
  } from "lucide-svelte";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { reportPanelError } from "../diagnostics/report";
  import { formatRelativeTime } from "../format";
  import EmptyState from "./EmptyState.svelte";

  /**
   * The stacked-branch page.
   *
   * Three things changed the shape of this screen, and each is load-bearing:
   *
   * - **It draws the tree.** A stack IS its shape; a flat list with "based on
   *   X" on every row made the reader rebuild the chain in their head.
   * - **It says what it cannot see.** The hierarchy is tip-anchored: a branch
   *   is a child only while it sits exactly on its parent's current tip. Once
   *   the parent moves, git keeps no record that the two were ever related, so
   *   a drifted stack does not render as stale — it renders as separate roots.
   *   That is a true statement about the repository, and it has to be *said*,
   *   or a stack that fell apart reads as a repository that never had one.
   * - **Its action finishes the job.** Rebasing one branch of a stack moves
   *   every branch above it off the commit it was cut from — and, because of
   *   the above, moves them out of the tree at the same time. Updating is a
   *   cascade over the snapshot on screen or it is a trap.
   */

  let payload = $state<StackHierarchyPayload | null>(null);
  let isLoading = $state(false);
  let loadError = $state<string | null>(null);
  let restackError = $state<string | null>(null);
  /** Progress line for the cascade in flight; null when idle. */
  let restackProgress = $state<string | null>(null);
  // Latch for the one mutating action on this page. A frontend guard cancel
  // cannot kill a backend `git rebase`; the latch is what stops a second
  // click from queueing another rebase behind the first.
  let restackingKey = $state<string | null>(null);
  let inflight: AsyncGuard | null = null;
  let restackGuard: AsyncGuard | null = null;

  const stackNodes = $derived<StackedBranchNode[]>(payload?.nodes ?? []);
  const stackBreadcrumb = $derived(payload?.breadcrumb ?? null);
  const stackDefaultBranch = $derived(payload?.default_branch ?? null);
  const rows = $derived(stackTreeRows(stackNodes));
  const unplaced = $derived(
    rootlessBranches(stackNodes, $repoStore.branches, stackDefaultBranch ?? ""),
  );

  /**
   * Branches whose base has moved under them.
   *
   * `commits_behind_base` is measured against the branch the backend compared
   * each one to — the default branch — so this is "your stack is behind main",
   * which is the situation an update actually fixes. A branch the branch list
   * has not measured contributes nothing rather than a zero.
   */
  const behindRows = $derived(
    rows.filter((row) => {
      const facts = stackBranchFacts(row.node.branch_name, $repoStore.branches);
      return facts !== null && facts.behindBase > 0;
    }),
  );

  async function loadStack(repoPath?: string) {
    const path = repoPath ?? $repoStore.currentPath;
    if (!path) return;
    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    isLoading = true;
    // Deliberately does NOT clear `restackError`. A cascade that stopped
    // part-way reloads the tree precisely because branches moved, and this
    // used to erase the report of what moved on the way past — leaving a
    // half-rebased repository with a screen that said nothing had happened.
    // Clearing a previous attempt's error belongs to the next attempt, which
    // does it before its first await; a watcher tick is not an attempt.
    try {
      const next = await invoke<StackHierarchyPayload>("cmd_get_stack_hierarchy", {
        repoPath: path,
      });
      if (!guard.isLive()) return;
      loadError = null;
      payload = next;
      stackCache.set(path, next);
    } catch (err) {
      // An IPC failure must not pose as "no stacked branches": keep any last
      // good nodes and surface why the fetch failed, with a retry.
      if (!guard.isLive()) return;
      loadError = reportPanelError("stack", err);
    } finally {
      if (guard.isLive()) isLoading = false;
    }
  }

  /**
   * Rebases `node` onto its parent and carries every branch above it along.
   *
   * The plan is built from the snapshot on screen *before* the first rewrite,
   * because that is the last moment the fork points exist: once a parent is
   * rebased, the commit its children were cut from is no longer reachable
   * from it, and no later computation can recover it. Each step is an
   * independently gated, independently rolled-back `cmd_restack`, so a
   * failure part-way leaves a repository whose state this can describe
   * exactly — and does, rather than reporting a partial cascade as a failure
   * that touched nothing.
   */
  async function restack(node: StackedBranchNode) {
    const repoPath = $repoStore.currentPath;
    if (!repoPath || !node.parent_branch_name) return;
    if (restackingKey !== null) return;
    const steps = cascadePlan(stackNodes, node.branch_name);
    if (steps.length === 0) return;
    // This rewrites commits on every branch in the plan. Naming them is the
    // difference between a confirmation and a formality.
    const confirmed = await askConfirm({
      title: steps.length === 1 ? "Restack branch" : "Update stack",
      message: describeCascade(steps),
      confirmLabel: steps.length === 1 ? "Restack" : `Rebase ${steps.length} branches`,
    });
    if (!confirmed) return;
    if (restackingKey !== null) return;
    restackGuard?.cancel();
    const guard = createAsyncGuard();
    restackGuard = guard;
    restackingKey = node.branch_name;
    // The red banner from the previous attempt must not imply this attempt
    // already failed; clear before the await, not only on success.
    restackError = null;
    const done: string[] = [];
    try {
      for (const [index, step] of steps.entries()) {
        restackProgress = `${index + 1} of ${steps.length}: ${step.branch} onto ${step.onto}`;
        await runStep(repoPath, step);
        done.push(step.branch);
        if (!guard.isLive() || $repoStore.currentPath !== repoPath) return;
      }
      await repoStore.refresh();
      if (!guard.isLive() || $repoStore.currentPath !== repoPath) return;
      await loadStack(repoPath);
    } catch (err) {
      if (!guard.isLive() || $repoStore.currentPath !== repoPath) return;
      const reported = reportPanelError("stack", err);
      // What moved and what did not. A cascade that rewrote two of four
      // branches and then stopped leaves a repository in a state the reader
      // has to know about — reporting only the error would describe it as if
      // nothing had happened.
      const remaining = steps.slice(done.length + 1).map((s) => s.branch);
      restackError = [
        reported,
        done.length > 0 ? `Rebased: ${done.join(", ")}.` : "No branch was rebased.",
        remaining.length > 0 ? `Still on their old base: ${remaining.join(", ")}.` : "",
      ]
        .filter(Boolean)
        .join(" ");
      // The tree on screen no longer matches the repository once any branch
      // has moved, and a stale tree is what a second click would plan from.
      if (done.length > 0) {
        await repoStore.refresh();
        if (guard.isLive() && $repoStore.currentPath === repoPath) await loadStack(repoPath);
      }
    } finally {
      restackingKey = null;
      restackProgress = null;
    }
  }

  /** One gated rebase, journalled whichever way it settles. */
  async function runStep(repoPath: string, step: RestackStep): Promise<void> {
    const actionLabel = `Restack ${step.branch} onto ${step.onto}`;
    let completed = false;
    try {
      // NOTE: no generic parameter on invoke() itself — the IPC contract
      // scanner only recognizes simple generics; the type rides on the const.
      const result: Guarded<string> = await invoke("cmd_restack", {
        repoPath,
        branch: step.branch,
        onto: step.onto,
        forkPoint: step.forkPoint,
      });
      completed = true;
      // README contract: mutating verdicts are recorded centrally and surface
      // in the header badge. Restack used to bypass both.
      harnessStore.recordVerdict(result.policy, repoPath);
      harnessStore.recordAction({
        repoPath,
        kind: "restack",
        label: actionLabel,
        ok: true,
        verdict: result.policy,
      });
    } catch (err) {
      if (!completed) {
        harnessStore.recordAction({
          repoPath,
          kind: "restack",
          label: actionLabel,
          ok: false,
          verdict: null,
        });
      }
      throw err;
    }
  }

  // Memo guard so unrelated store emissions do not refetch: the stack
  // reloads when the repository changes or its generation bumps (watcher
  // refreshes, branch mutations) — nothing else.
  let prevRepoPath: string | null = null;
  let prevGeneration = -1;

  $effect(() => {
    const repo = $repoStore.currentPath;
    const generation = $repoStore.generation;
    if (!repo) {
      prevRepoPath = null;
      prevGeneration = -1;
      inflight?.cancel();
      restackGuard?.cancel();
      payload = null;
      loadError = null;
      restackError = null;
      isLoading = false;
      return;
    }
    if (repo === prevRepoPath && generation === prevGeneration) return;
    prevRepoPath = repo;
    prevGeneration = generation;
    // Hydrate the last-known hierarchy synchronously so a revisit renders
    // instantly; the fetch below then refreshes it in place.
    const cached = stackCache.get(repo);
    if (cached) payload = cached;
    void loadStack(repo);
    const started = inflight;
    return () => {
      if (inflight === started) {
        started?.cancel();
      }
    };
  });

  $effect(() => {
    return () => {
      inflight?.cancel();
      restackGuard?.cancel();
    };
  });
</script>

<div class="flex-1 flex flex-col bg-background h-full text-xs font-sans p-4 overflow-auto">
  <div class="flex items-start justify-between gap-3 mb-3">
    <div class="min-w-0">
      <h2 class="flex items-center gap-2 text-sm font-semibold text-textPrimary">
        <Layers size={16} class="text-accent" />
        Stack
        <span class="text-textMuted font-normal text-[11px]">
          which branch is built on which, and what an update would move
        </span>
      </h2>
      {#if stackBreadcrumb && stackBreadcrumb.breadcrumb_chain.length > 1}
        <div class="mt-1.5 flex items-center gap-1.5 flex-wrap font-mono text-[11px] text-textMuted">
          {#each stackBreadcrumb.breadcrumb_chain as segment, i (segment)}
            {#if i > 0}
              <span aria-hidden="true">›</span>
            {/if}
            <span class={i === stackBreadcrumb.breadcrumb_chain.length - 1 ? "text-accent font-medium" : ""}>
              {segment}
            </span>
          {/each}
        </div>
      {/if}
    </div>
    <button onclick={() => loadStack()} disabled={isLoading || restackingKey !== null} class="gp-btn shrink-0">
      <RefreshCw size={13} class={isLoading ? "animate-spin" : ""} />
      <span>Refresh</span>
    </button>
  </div>

  {#if restackProgress}
    <div class="mb-3 max-w-3xl flex items-center gap-2 rounded-xl border border-accent/40 bg-accent/10 p-2.5 text-[11px] text-textPrimary">
      <RefreshCw size={13} class="animate-spin shrink-0 text-accent" />
      <span>Updating stack — {restackProgress}</span>
    </div>
  {/if}

  {#if restackError}
    <div role="alert" class="mb-3 max-w-3xl p-2.5 rounded-xl border border-rose-500/30 bg-rose-500/10 text-rose-700 dark:text-rose-300">{restackError}</div>
  {/if}

  {#if loadError}
    <div role="alert" class="mb-3 max-w-3xl p-2.5 rounded-xl border border-rose-500/30 bg-rose-500/10 text-rose-700 dark:text-rose-300 flex items-center justify-between gap-3">
      <span class="min-w-0 truncate" title={loadError}>Failed to load stack: {loadError}</span>
      <button onclick={() => loadStack()} disabled={isLoading || restackingKey !== null} class="gp-btn shrink-0 !py-1 !px-2.5 !text-[11px]">
        <RefreshCw size={12} class={isLoading ? "animate-spin" : ""} />
        <span>Retry</span>
      </button>
    </div>
  {/if}

  {#if behindRows.length > 0}
    <div class="mb-3 max-w-3xl flex items-start gap-2 rounded-xl border border-amber-500/30 bg-amber-500/10 p-2.5 text-[11px] text-amber-700 dark:text-amber-300">
      <CircleAlert size={14} class="shrink-0 mt-px" />
      <span>
        {behindRows.length} branch{behindRows.length === 1 ? "" : "es"} here {behindRows.length === 1
          ? "is"
          : "are"} behind {stackDefaultBranch ?? "the default branch"}. Updating a branch carries everything stacked
        above it along; nothing below it moves.
      </span>
    </div>
  {/if}

  {#if rows.length > 0}
    <div class="max-w-3xl">
      <ul class="space-y-1.5">
        {#each rows as row (row.node.branch_name)}
          {@const node = row.node}
          {@const facts = stackBranchFacts(node.branch_name, $repoStore.branches)}
          {@const isCurrent = node.branch_name === $repoStore.currentBranch}
          {@const isRoot = node.parent_branch_name === null && node.branch_name === stackDefaultBranch}
          <li style="padding-left: {row.depth * 20}px">
            <div
              aria-busy={restackingKey === node.branch_name}
              data-branch={node.branch_name}
              data-depth={row.depth}
              class="relative p-3 bg-surface border border-border/70 rounded-2xl shadow-card flex items-start justify-between gap-3 transition-[border-color,box-shadow] duration-150 hover:border-accent/40 {isCurrent
                ? 'border-accent/50 ring-1 ring-accent/30'
                : ''}"
            >
              <!-- The elbow into the parent's row above. Drawn on the card,
                   not between rows, so it survives wrapping at any width. -->
              {#if row.depth > 0}
                <span
                  aria-hidden="true"
                  class="absolute border-l border-b border-border/70 rounded-bl-md"
                  style="left: -12px; top: -8px; bottom: 50%; width: 10px;"
                ></span>
              {/if}
              <div class="flex items-start gap-2.5 min-w-0">
                <GitBranch size={15} class="mt-0.5 shrink-0 {isCurrent ? 'text-accent' : 'text-textMuted'}" />
                <div class="min-w-0">
                  <div class="font-medium text-textPrimary text-[13px] flex items-center gap-2 flex-wrap">
                    <span class="truncate">{node.branch_name}</span>
                    {#if isCurrent}
                      <span class="text-[10px] bg-accent/20 text-accent px-1.5 py-0.5 rounded-full font-mono">HEAD</span>
                    {/if}
                    {#if isRoot}
                      <span class="text-[10px] bg-border/40 text-textMuted px-1.5 py-0.5 rounded-full font-mono">ROOT</span>
                    {/if}
                    <!-- A base that has moved is the reason to press the
                         button, so it is a chip and not a footnote. -->
                    {#if facts && facts.behindBase > 0}
                      <span
                        class="text-[10px] px-1.5 py-0.5 rounded-full font-mono bg-amber-500/15 text-amber-700 dark:text-amber-300"
                        title="{facts.behindBase} commit{facts.behindBase === 1 ? '' : 's'} on {facts.comparedTo ?? 'the base'} that this branch does not have"
                      >
                        {facts.behindBase} behind {facts.comparedTo ?? "base"}
                      </span>
                    {/if}
                    <!-- Tracking state. "untracked" is stated rather than
                         drawn as 0↑0↓, which is what a pushed-and-current
                         branch looks like. -->
                    {#if facts?.upstream}
                      {#if facts.upstream.gone}
                        <span class="text-[10px] px-1.5 py-0.5 rounded-full font-mono bg-rose-500/10 text-rose-700 dark:text-rose-300" title="{facts.upstream.name} no longer exists on the remote">
                          upstream gone
                        </span>
                      {:else if facts.upstream.ahead > 0 || facts.upstream.behind > 0}
                        <span class="text-[10px] font-mono text-textMuted inline-flex items-center gap-0.5" title="Against {facts.upstream.name}">
                          {#if facts.upstream.ahead > 0}<ArrowUp size={10} />{facts.upstream.ahead}{/if}
                          {#if facts.upstream.behind > 0}<ArrowDown size={10} />{facts.upstream.behind}{/if}
                        </span>
                      {/if}
                    {:else if facts}
                      <span class="text-[10px] font-mono text-textMuted" title="No tracking branch is configured">untracked</span>
                    {/if}
                  </div>
                  <div class="mt-0.5 text-textMuted text-[11px] font-mono flex flex-wrap items-center gap-x-2">
                    <span>
                      {node.parent_branch_name
                        ? `on ${node.parent_branch_name} +${node.commit_count_ahead_of_parent}`
                        : "no branch below it"}
                    </span>
                    {#if row.hasChildren}
                      <span aria-hidden="true">·</span>
                      <span>carries {node.child_branch_names.length} above</span>
                    {/if}
                    {#if facts && facts.lastCommitTimestamp > 0}
                      <span aria-hidden="true">·</span>
                      <span title={facts.lastSummary}>
                        {formatRelativeTime(facts.lastCommitTimestamp)}{facts.lastAuthor ? ` by ${facts.lastAuthor}` : ""}
                      </span>
                    {/if}
                  </div>
                </div>
              </div>

              <div class="flex items-center gap-1.5 shrink-0">
                {#if node.parent_branch_name}
                  {@const plan = cascadePlan(stackNodes, node.branch_name)}
                  <button
                    onclick={() => restack(node)}
                    disabled={restackingKey !== null}
                    class="gp-btn !py-1 !px-2.5 !text-[11px]"
                    title="Rebase {node.branch_name} onto {node.parent_branch_name}{plan.length > 1
                      ? `, then the ${plan.length - 1} branch${plan.length === 2 ? '' : 'es'} stacked above it`
                      : ''}"
                  >
                    {#if restackingKey === node.branch_name}
                      <RefreshCw size={12} class="animate-spin" />
                    {/if}
                    <span>
                      {#if restackingKey === node.branch_name}
                        Updating…
                      {:else if plan.length > 1}
                        <span class="inline-flex items-center gap-1"><ArrowUpFromLine size={11} />Update {plan.length}</span>
                      {:else}
                        Restack
                      {/if}
                    </span>
                  </button>
                {/if}
                {#if !isCurrent}
                  <button
                    onclick={() => repoStore.checkoutBranch(node.branch_name)}
                    disabled={restackingKey !== null}
                    class="gp-btn !py-1 !px-2.5 !text-[11px]"
                  >
                    Checkout
                  </button>
                {/if}
              </div>
            </div>
          </li>
        {/each}
      </ul>

      <!-- Said on every populated stack, not only on the empty one: a reader
           looking at three branches has no way to tell that a fourth dropped
           out of the tree when its parent moved. -->
      <p class="mt-3 flex items-start gap-1.5 text-[10px] leading-relaxed text-textMuted">
        <Info size={11} class="mt-px shrink-0" />
        <span>
          A branch appears here as a child only while it sits on its parent's current tip. Git records no
          “cut from” link, so a branch left behind by a rebase of its parent shows up as its own root, not as a
          stale child.
        </span>
      </p>
    </div>
  {:else if isLoading}
    <div class="space-y-3 max-w-3xl" aria-hidden="true">
      <div class="p-3.5 bg-surface border border-border/70 rounded-2xl animate-pulse h-16"></div>
      <div class="p-3.5 bg-surface border border-border/70 rounded-2xl animate-pulse h-16 w-11/12"></div>
    </div>
    <p class="sr-only">Loading stack hierarchy…</p>
  {:else if !loadError}
    <div class="flex-1 flex">
      <EmptyState
        icon={Layers}
        title="No branch sits on another"
        hint="Every branch here forks straight off the default branch, or below the loaded history. Branches stacked on one another appear as a tree."
      />
    </div>
  {/if}

  {#if unplaced.length > 0}
    <div class="mt-4 max-w-3xl">
      <h3 class="text-[11px] uppercase tracking-wider text-textMuted mb-1.5">
        On no stack ({unplaced.length})
      </h3>
      <p class="text-[10px] text-textMuted mb-2">
        These local branches met no other branch's tip on their first-parent walk — they fork off
        {stackDefaultBranch ?? "the default branch"} directly, or below the history this scan loaded.
      </p>
      <div class="flex flex-wrap gap-1.5">
        {#each unplaced as name (name)}
          <button
            type="button"
            class="gp-pill !text-[10px] font-mono hover:text-accent"
            disabled={restackingKey !== null}
            onclick={() => repoStore.checkoutBranch(name)}
            title="Checkout {name}"
          >
            {name}
          </button>
        {/each}
      </div>
    </div>
  {/if}
</div>
