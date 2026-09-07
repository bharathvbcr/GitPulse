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
  import { onMount } from "svelte";
  import { repoStore } from "../stores/repoStore";
  import { interfaceStore } from "../stores/interfaceStore";
  import { toastStore } from "../stores/toastStore";
  import {
    firstFailure,
    isCleanSweep,
    summarizeRun,
    type BulkRunReport,
  } from "../repos/workspaceOps";
  import { describeWorkspace, wipDestination } from "../repos/wipSummary";
  import { describeDestination } from "../views/viewRegistry";
  import { shouldDismissOverlay } from "../ui/dismiss";
  import { portal } from "../dom/portal";
  import { LAYERS } from "../ui/layers";
  import { CloudDownload, Loader2, AlertTriangle, CircleCheck, X } from "lucide-svelte";

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

  /**
   * The pane that answers whatever this repository is currently holding.
   *
   * Keyed on live state rather than on which list the row was drawn in, so a
   * repository the last sweep skipped for conflicts opens the resolver — the
   * same place its work-in-progress row would have sent the reader.
   */
  function destinationFor(path: string) {
    return wipDestination(wip.repos.find((repo) => repo.path === path)?.severity ?? null);
  }

  /**
   * Opens the repository a row names, on the pane its worst reason lives on.
   *
   * Both lists in this panel used to be inert text: they named the repository
   * holding work — or the one a sweep could not finish — and then left the
   * reader to find it in the tab strip themselves. Every row is an open tab,
   * so this is an activation rather than an open; `openRepo` is the fallback
   * for the one race where a tab was closed between render and click.
   *
   * `activateTab` swaps the active session before its first await, so the
   * `setActiveTab` below lands on the repository just activated rather than
   * on the one being left.
   */
  function reveal(path: string) {
    const { tab, section } = destinationFor(path);
    const open = $repoStore.openTabs.find((entry) => entry.path === path);
    if (open) {
      void repoStore.activateTab(open.id);
      repoStore.setActiveTab(tab, section);
    } else {
      void repoStore.openRepo(path).then(() => repoStore.setActiveTab(tab, section));
    }
    // The repository surface is hidden while Fleet is up, so activating a tab
    // underneath it would look exactly like the inert row this replaced.
    interfaceStore.setFleetOpen(false);
    detailsOpen = false;
  }

  function handlePointerDown(event: PointerEvent) {
    if (!detailsOpen) return;
    // The trigger counts as inside: dismissing on its own pointerdown would
    // close the panel a beat before its click reopened it.
    if (!shouldDismissOverlay(event.target, "[data-workspace-wip], [data-workspace-wip-trigger]")) {
      return;
    }
    detailsOpen = false;
  }

  function handleKey(event: KeyboardEvent) {
    if (event.key === "Escape" && detailsOpen) {
      event.preventDefault();
      detailsOpen = false;
    }
  }

  onMount(() => {
    window.addEventListener("pointerdown", handlePointerDown, true);
    window.addEventListener("keydown", handleKey);
    return () => {
      window.removeEventListener("pointerdown", handlePointerDown, true);
      window.removeEventListener("keydown", handleKey);
    };
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
      data-workspace-wip-trigger
      class="gp-btn !py-1 !px-2 !text-[11px] inline-flex items-center gap-1.5 {wip.allClear
        ? `text-textMuted hover:text-textPrimary ${detailsOpen ? '!border-accent/50 !bg-surfaceHover !text-textPrimary' : ''}`
        : `!border-amber-500/50 !bg-amber-500/20 text-amber-700 hover:!bg-amber-500/30 hover:!border-amber-500/70 dark:text-amber-300 dark:!bg-amber-500/25 dark:hover:!bg-amber-500/35 ${detailsOpen ? '!border-amber-500/70 !bg-amber-500/30 dark:!bg-amber-500/40 ring-1 ring-amber-500/40' : ''}`}"
      onclick={() => (detailsOpen = !detailsOpen)}
      title={describeWorkspace(wip)}
      aria-haspopup="dialog"
      aria-expanded={detailsOpen}
    >
      {#if wip.allClear}
        <CircleCheck size={11} class="text-emerald-600 dark:text-emerald-400" />
        <span>All clean</span>
      {:else}
        <AlertTriangle size={11} class="shrink-0 text-amber-600 dark:text-amber-400" />
        <span>{wip.repos.length} with work</span>
      {/if}
    </button>

    {#if detailsOpen}
      <div
        use:portal
        data-workspace-wip
        class="fixed right-3 top-20 w-80 gp-pop shadow-float rounded-xl p-3 text-[11px] bg-surface/95 border border-border/80"
        style="z-index: {LAYERS.MENU}"
        role="dialog"
        aria-label="Workspace status"
      >
        <div class="mb-2 flex items-start gap-2">
          <p class="min-w-0 flex-1 font-semibold text-textPrimary">{describeWorkspace(wip)}</p>
          <!-- Closing is the one action this panel always offers, so it sits
               where a window's close sits rather than below content that can
               run long enough to scroll it out of reach. -->
          <button
            type="button"
            class="shrink-0 inline-flex h-5 w-5 items-center justify-center rounded-full border border-rose-500/30 bg-rose-500/10 text-rose-700 transition-colors hover:bg-rose-500/20 dark:text-rose-300"
            onclick={() => (detailsOpen = false)}
            title="Close (Esc)"
            aria-label="Close workspace status"
          >
            <X size={11} />
          </button>
        </div>

        {#if wip.repos.length > 0}
          <ul class="mb-2 space-y-1">
            {#each wip.repos as repo (repo.path)}
              {@const to = destinationFor(repo.path)}
              <li>
                <!-- The row is the door to the repository it names: a list
                     that says "alpha has 3 conflicts" and cannot take you to
                     them is a notification, not a control. -->
                <button
                  type="button"
                  class="w-full rounded-lg border border-border/50 px-2 py-1.5 text-left transition-colors hover:border-accent/60 hover:bg-surfaceHover"
                  onclick={() => reveal(repo.path)}
                  title="Open {repo.label} — {describeDestination(to.tab, to.section)}"
                >
                  <p class="truncate font-medium text-textPrimary">{repo.label}</p>
                  <p class="truncate text-textMuted">
                    {repo.reasons.map((reason) => reason.detail).join(" · ")}
                  </p>
                </button>
              </li>
            {/each}
          </ul>
        {/if}

        {#if report}
          <!-- The last sweep's outcome, including what it did NOT do. -->
          <p class="mb-1 font-semibold text-textPrimary">Last sweep</p>
          <ul class="space-y-1">
            {#each report.results.filter((r) => r.status !== "ok") as result (result.path)}
              {@const to = destinationFor(result.path)}
              <li>
                <!-- Same rule as the list above: the repository a sweep could
                     not finish is exactly the one the reader wants to open. -->
                <button
                  type="button"
                  class="flex w-full items-start gap-1.5 rounded-lg px-1 py-0.5 text-left transition-colors hover:bg-surfaceHover"
                  onclick={() => reveal(result.path)}
                  title="Open {result.label} — {describeDestination(to.tab, to.section)}"
                >
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
                </button>
              </li>
            {/each}
          </ul>
          {#if report.results.every((r) => r.status === "ok")}
            <p class="text-textMuted">Every repository succeeded.</p>
          {/if}
        {/if}
      </div>
    {/if}
  </div>
{/if}
