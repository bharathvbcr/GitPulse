<script module lang="ts">
  /**
   * Whether the workspace-wide controls are worth showing at all.
   *
   * With one repository open, "fetch all" is just "fetch" and the work-in-
   * progress roll-up says nothing the tab does not already say. The controls
   * earn their space from the second repository onward.
   */
  export function showsWorkspaceControls(openCount: number): boolean {
    return openCount > 1;
  }
</script>

<script lang="ts">
  /**
   * Workspace-wide actions: fetch or pull every open repository, and one
   * honest answer to "is anything unsaved anywhere".
   *
   * The reporting rule is the whole point. A sweep over 24 repositories that
   * skipped 3 must never render as "fetched everything" — so the summary
   * always names failures and skips, and the detail list says why each one was
   * skipped rather than leaving the user to guess.
   */
  import { repoStore } from "../stores/repoStore";
  import { toastStore } from "../stores/toastStore";
  import {
    firstFailure,
    isCleanSweep,
    summarizeRun,
    type BulkRunReport,
  } from "../repos/workspaceOps";
  import { describeWorkspace } from "../repos/wipSummary";
  import { portal } from "../dom/portal";
  import { LAYERS } from "../ui/layers";
  import { CloudDownload, Loader2, AlertTriangle, CircleCheck } from "lucide-svelte";

  let running = $state<"fetch" | "pull" | null>(null);
  let progress = $state<{ done: number; total: number } | null>(null);
  let report = $state<BulkRunReport | null>(null);
  let detailsOpen = $state(false);
  /** Cooperative cancellation handed to the run. */
  let cancelToken = $state<{ aborted: boolean } | null>(null);

  const openCount = $derived($repoStore.openTabs.length);
  const visible = $derived(showsWorkspaceControls(openCount));
  // Recomputed from live session state, so it always agrees with the tabs.
  const wip = $derived.by(() => {
    // Touch the fields the summary reads so the derivation re-runs with them.
    void $repoStore.openTabs;
    void $repoStore.statuses;
    void $repoStore.operation;
    void $repoStore.stashEntries;
    return repoStore.workspaceWip();
  });

  async function run(kind: "fetch" | "pull") {
    if (running) return;
    running = kind;
    detailsOpen = false;
    const token = { aborted: false };
    cancelToken = token;
    progress = { done: 0, total: openCount };
    try {
      const result = await repoStore.runAcrossOpenRepos(kind, {
        signal: token,
        onProgress: (done, total) => {
          progress = { done, total };
        },
      });
      report = result;
      const verb = kind === "fetch" ? "Fetched" : "Pulled";
      const line = summarizeRun(result, verb);
      if (isCleanSweep(result)) {
        toastStore.success(line);
      } else {
        // A partial sweep names one concrete cause; the rest are in the
        // details panel rather than buried in a count.
        const failure = firstFailure(result);
        toastStore.warning(failure ? `${line} — ${failure.label}: ${failure.error}` : line);
        detailsOpen = true;
      }
    } finally {
      running = null;
      progress = null;
      cancelToken = null;
    }
  }
</script>

{#if visible}
  <div class="flex items-center gap-1">
    <button
      type="button"
      class="gp-btn !py-1 !px-2 !text-[11px] inline-flex items-center gap-1.5"
      disabled={running !== null}
      onclick={() => void run("fetch")}
      title="Fetch every open repository. Repositories that are mid-merge or hold conflicts are skipped and reported."
    >
      {#if running === "fetch"}
        <Loader2 size={11} class="animate-spin" />
      {:else}
        <CloudDownload size={11} />
      {/if}
      <span>
        {#if progress && running === "fetch"}
          {progress.done}/{progress.total}
        {:else}
          Fetch all
        {/if}
      </span>
    </button>

    {#if running && cancelToken}
      <button
        type="button"
        class="gp-btn !py-1 !px-2 !text-[11px]"
        onclick={() => {
          if (cancelToken) cancelToken.aborted = true;
        }}
        title="Stop before the next repository. Repositories already fetched stay fetched."
      >
        Stop
      </button>
    {/if}

    <button
      type="button"
      class="inline-flex items-center gap-1 rounded px-1.5 py-1 text-[11px] transition-colors {wip.allClear
        ? 'text-textMuted hover:text-textPrimary'
        : 'text-amber-600 dark:text-amber-400 hover:bg-amber-500/10'}"
      onclick={() => (detailsOpen = !detailsOpen)}
      title={describeWorkspace(wip)}
      aria-expanded={detailsOpen}
    >
      {#if wip.allClear}
        <CircleCheck size={11} />
        <span>All clean</span>
      {:else}
        <AlertTriangle size={11} />
        <span>{wip.repos.length} with work</span>
      {/if}
    </button>

    {#if detailsOpen}
      <div
        use:portal
        class="fixed right-3 top-20 w-80 gp-card gp-pop rounded-xl p-3 text-[11px]"
        style="z-index: {LAYERS.MENU}"
        role="dialog"
        aria-label="Workspace status"
      >
        <p class="mb-2 font-semibold text-textPrimary">{describeWorkspace(wip)}</p>

        {#if wip.repos.length > 0}
          <ul class="mb-2 space-y-1">
            {#each wip.repos as repo (repo.path)}
              <li class="rounded-lg border border-border/50 px-2 py-1.5">
                <p class="truncate font-medium text-textPrimary">{repo.label}</p>
                <p class="truncate text-textMuted">
                  {repo.reasons.map((reason) => reason.detail).join(" · ")}
                </p>
              </li>
            {/each}
          </ul>
        {/if}

        {#if report}
          <!-- The last sweep's outcome, including what it did NOT do. -->
          <p class="mb-1 font-semibold text-textPrimary">Last sweep</p>
          <ul class="space-y-1">
            {#each report.results.filter((r) => r.status !== "ok") as result (result.path)}
              <li class="flex items-start gap-1.5">
                <span
                  class="mt-0.5 shrink-0 rounded-full px-1.5 text-[10px] {result.status === 'failed'
                    ? 'bg-red-500/15 text-red-600 dark:text-red-400'
                    : 'bg-amber-500/15 text-amber-600 dark:text-amber-400'}"
                >
                  {result.status}
                </span>
                <span class="min-w-0">
                  <span class="text-textPrimary">{result.label}</span>
                  <span class="text-textMuted"> — {result.error ?? result.reason}</span>
                </span>
              </li>
            {/each}
          </ul>
          {#if report.results.every((r) => r.status === "ok")}
            <p class="text-textMuted">Every repository succeeded.</p>
          {/if}
        {/if}

        <button
          type="button"
          class="gp-btn mt-2 !py-1 !px-2 !text-[11px] w-full"
          onclick={() => (detailsOpen = false)}
        >
          Close
        </button>
      </div>
    {/if}
  </div>
{/if}
