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
  import { interfaceStore } from "../stores/interfaceStore";
  import { summarizeRun, type BulkRunReport } from "../repos/workspaceOps";
  import { describeWorkspace, wipDestination } from "../repos/wipSummary";
  import { describeDestination } from "../views/viewRegistry";
  import { popover } from "../ui/popover";
  import { portal } from "../dom/portal";
  import { LAYERS } from "../ui/layers";
  import { CloudDownload, AlertTriangle, CircleCheck, X } from "@lucide/svelte";

  let running = $state<"fetch" | "pull" | null>(null);
  let progress = $state<{ done: number; total: number } | null>(null);
  let report = $state<BulkRunReport | null>(null);
  /** The last sweep's one-line outcome. It names failures and skips, never just "done". */
  let sweepLine = $state<string | null>(null);
  let detailsOpen = $state(false);
  /** Cooperative cancellation handed to the run. */
  let cancelToken = $state<{ aborted: boolean } | null>(null);

  /** Ring geometry: a 14px viewBox and a 5px radius. The dash length is set per update, never animated. */
  const RING_CIRCUMFERENCE = 2 * Math.PI * 5;
  const ringDash = $derived(
    progress && progress.total > 0
      ? `${((progress.done / progress.total) * RING_CIRCUMFERENCE).toFixed(2)} ${RING_CIRCUMFERENCE.toFixed(2)}`
      : `0 ${RING_CIRCUMFERENCE.toFixed(2)}`,
  );
  const runLabel = $derived(running === "pull" ? "Pulling repositories" : "Fetching repositories");

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
    if (tab === "history" && section === "diff") {
      void repoStore.previewUncommitted(path);
      detailsOpen = false;
      return;
    }
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

  /**
   * Dismissal only, from the shared popover owner: this panel docks at a
   * fixed corner of the window rather than against a trigger, so there is no
   * anchor to clamp and the owner leaves its position alone.
   *
   * The trigger counts as inside, or its own pointerdown would close the
   * panel a beat before its click reopened it.
   */
  const dismissal = {
    dismiss: {
      inside: "[data-workspace-wip], [data-workspace-wip-trigger]",
      escape: "bubble" as const,
    },
    onDismiss: () => { detailsOpen = false; },
  };

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
      // The result reads in the workspace status panel, not as a notification: a
      // partial sweep keeps its failures and skips beside the repositories
      // that own them, and a clean one says so in the same place.
      sweepLine = summarizeRun(result, kind === "fetch" ? "Fetched" : "Pulled");
      detailsOpen = true;
    } finally {
      running = null;
      progress = null;
      cancelToken = null;
    }
  }
</script>

{#if visible}
  <div class="flex shrink-0 items-center gap-1">
    {#if running && progress}
      <!-- While a sweep runs, the button becomes its own pill: a progress ring,
           the count and Stop. -->
      <div class="gp-chip shrink-0 gap-1.5 pr-1" data-tone="accent" role="group" aria-label={runLabel}>
        <svg
          viewBox="0 0 14 14"
          width="14"
          height="14"
          class="shrink-0"
          role="progressbar"
          aria-label={runLabel}
          aria-valuemin="0"
          aria-valuemax={progress.total}
          aria-valuenow={progress.done}
        >
          <circle cx="7" cy="7" r="5" fill="none" stroke="currentColor" stroke-opacity="0.25" stroke-width="2" />
          <circle
            cx="7"
            cy="7"
            r="5"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-dasharray={ringDash}
            transform="rotate(-90 7 7)"
          />
        </svg>
        <span class="tabular-nums">{progress.done}/{progress.total}</span>
        {#if cancelToken}
          <button
            type="button"
            class="gp-btn"
            data-variant="ghost"
            data-size="sm"
            onclick={() => {
              if (cancelToken) cancelToken.aborted = true;
            }}
            title="Stop before the next repository. Repositories already fetched stay fetched."
          >
            Stop
          </button>
        {/if}
      </div>
    {:else}
      <button
        type="button"
        class="gp-btn"
        data-size="sm"
        onclick={() => void run("fetch")}
        title="Fetch every open repository. Repositories that are mid-merge or hold conflicts are skipped and reported."
        aria-label="Fetch all"
      >
        <CloudDownload size={11} />
        <span class="max-[1099px]:sr-only">Fetch all</span>
      </button>
    {/if}

    <span class="sr-only" role="status" aria-live="polite" aria-atomic="true">{sweepLine ?? ""}</span>

    <button
      type="button"
      data-workspace-wip-trigger
      class="gp-chip min-h-6 shrink-0 cursor-pointer"
      data-tone={wip.allClear ? "ok" : "warn"}
      onclick={() => (detailsOpen = !detailsOpen)}
      title={describeWorkspace(wip)}
      aria-label={wip.allClear ? "All clean" : `${wip.repos.length} with work`}
      aria-haspopup="dialog"
      aria-expanded={detailsOpen}
    >
      {#if wip.allClear}
        <CircleCheck size={11} />
        <span class="max-[1099px]:sr-only">All clean</span>
      {:else}
        <AlertTriangle size={11} class="shrink-0" />
        <span class="max-[1099px]:sr-only">{wip.repos.length} with work</span>
      {/if}
    </button>

    {#if detailsOpen}
      <div
        use:portal
        use:popover={dismissal}
        data-workspace-wip
        class="fixed right-3 top-20 w-80 gp-menu gp-pop shadow-float text-meta"
        style="z-index: {LAYERS.MENU}"
        role="dialog"
        aria-label="Workspace status"
      >
        <div class="gp-scroll max-h-[70vh] overflow-y-auto p-2">
          <div class="mb-2 flex items-start gap-2">
            <!-- The sweep's outcome leads when there is one: it answers the click
                 that just ran, and it names failures and skips. -->
            <p class="min-w-0 flex-1 font-semibold text-textPrimary" data-sweep-summary={sweepLine ? "" : undefined}>
              {sweepLine ?? describeWorkspace(wip)}
            </p>
            <!-- Closing is the one action this panel always offers, so it sits
                 where a window's close sits rather than below content that can
                 run long enough to scroll it out of reach. -->
            <button
              type="button"
              class="gp-icon-btn shrink-0"
              data-size="sm"
              data-tone="danger"
              onclick={() => (detailsOpen = false)}
              title="Close (Esc)"
              aria-label="Close workspace status"
            >
              <X size={11} />
            </button>
          </div>

          {#if sweepLine}
            <p class="mb-2 text-textMuted">{describeWorkspace(wip)}</p>
          {/if}

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
                    class="gp-menu-item"
                    onclick={() => reveal(repo.path)}
                    title="Open {repo.label} — {describeDestination(to.tab, to.section)}"
                  >
                    <span class="min-w-0 flex-1">
                      <span class="block truncate font-medium text-textPrimary">{repo.label}</span>
                      <span class="block truncate text-textMuted">
                        {repo.reasons.map((reason) => reason.detail).join(" · ")}
                      </span>
                    </span>
                  </button>
                  {#if repo.severity !== "uncommitted" && repo.reasons.some(reason => reason.kind === "uncommitted")}
                    <button
                      type="button"
                      class="gp-btn text-warn"
                      data-variant="ghost"
                      data-size="sm"
                      title="Preview uncommitted files in {repo.label}"
                      onclick={() => { void repoStore.previewUncommitted(repo.path); detailsOpen = false; }}
                    >Preview uncommitted changes</button>
                  {/if}
                </li>
              {/each}
            </ul>
          {/if}

          {#if report}
            <!-- The last sweep's outcome, including what it did NOT do. -->
            <p class="gp-label mb-1">Last sweep</p>
            <ul class="space-y-1">
              {#each report.results.filter((r) => r.status !== "ok") as result (result.path)}
                {@const to = destinationFor(result.path)}
                <li>
                  <!-- Same rule as the list above: the repository a sweep could
                       not finish is exactly the one the reader wants to open. -->
                  <button
                    type="button"
                    class="gp-menu-item items-start gap-1.5"
                    onclick={() => reveal(result.path)}
                    title="Open {result.label} — {describeDestination(to.tab, to.section)}"
                  >
                    <span
                      class="gp-chip mt-0.5 shrink-0"
                      data-tone={result.status === 'failed' ? 'danger' : 'warn'}
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
      </div>
    {/if}
  </div>
{/if}
