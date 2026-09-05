<script module lang="ts">
  import type { FleetRow, ScanFamily } from "../fleet/types";

  /** Which rows the grid shows. */
  export type FleetFilter = "all" | "attention";

  export function applyFilter(rows: readonly FleetRow[], filter: FleetFilter): FleetRow[] {
    if (filter === "all") return [...rows];
    // "Needs attention" is about the workspace, so a recents row — which is
    // unknown by construction — would otherwise fill the list with rows
    // nobody can act on without opening them first.
    return rows.filter((row) => row.presence === "open" && row.severity !== "clean");
  }

  /** Targets for a family sweep: open repositories only. */
  export function scanTargets(rows: readonly FleetRow[]): { path: string; label: string }[] {
    return rows
      .filter((row) => row.presence === "open")
      .map((row) => ({ path: row.path, label: row.label }));
  }

  /** Tone for a row's severity stripe. Spelled out so a new severity is a
   *  compile-time gap rather than a stripe that silently renders as default. */
  export const SEVERITY_STRIPE: Record<FleetRow["severity"], string> = {
    conflicts: "bg-rose-500",
    operation: "bg-amber-500",
    unknown: "bg-textMuted/50",
    uncommitted: "bg-sky-500",
    unpushed: "bg-sky-500/60",
    stash: "bg-textMuted/40",
    clean: "bg-emerald-500/50",
  };
</script>

<script lang="ts">
  /**
   * The Fleet dashboard: every open repository, and every recent one, on a
   * single grid.
   *
   * Workspace-scoped rather than a per-repository view, so it is mounted
   * alongside the repo pane and hidden rather than swapped — the repo block is
   * keyed on `currentPath` and holds the live terminal, and destroying it to
   * show this would kill the PTY and re-hydrate every tab.
   *
   * The grid renders `buildFleetRows`, which is pure and fully unit-tested;
   * everything here is presentation. In particular there is no place in this
   * file where a missing measurement can become a zero — every measurable cell
   * goes through `FleetCell`.
   */
  import {
    Boxes,
    CircleAlert,
    Clock,
    FolderGit2,
    GitBranch,
    HardDrive,
    LayoutGrid,
    RefreshCw,
    ShieldAlert,
    SquareCode,
    Trash2,
    Trees,
    X,
  } from "lucide-svelte";
  import { repoStore } from "../stores/repoStore";
  import { interfaceStore } from "../stores/interfaceStore";
  import { toastStore } from "../stores/toastStore";
  import { fleetStore } from "../fleet/fleetStore";
  import { buildFleetRows } from "../fleet/row";
  import { byUrgency, describeTally, fleetHeadline, tally } from "../fleet/aggregate";
  import { FAMILY_LABEL, SCAN_FAMILIES } from "../fleet/types";
  import { disambiguateLabels, displayName, isPathAmong, isCaseInsensitiveFs } from "../repos/paths";
  import { formatAge, humanBytes } from "../storage/format";
  import { formatAuditCounts } from "../health/format";
  import { firstFailure, isCleanSweep, summarizeRun } from "../repos/workspaceOps";
  import { nextRovingIndex, type RovingKey } from "../dom/rovingFocus";
  import { isImeComposition } from "../keyboard/imeGuard";
  import EmptyState from "./EmptyState.svelte";
  import FleetCell from "./FleetCell.svelte";

  let filter = $state<FleetFilter>("all");
  let focusedIndex = $state(0);
  const pathOpts = { caseInsensitive: isCaseInsensitiveFs() };

  const openFacts = $derived.by(() => {
    // Touch the session fields the facts are derived from so this re-runs
    // whenever any of them changes.
    void $repoStore.openTabs;
    void $repoStore.statuses;
    void $repoStore.operation;
    void $repoStore.stashEntries;
    void $repoStore.watch;
    return repoStore.repoFacts();
  });

  const recents = $derived.by(() => {
    const openPaths = openFacts.map((facts) => facts.path);
    const paths = $repoStore.recentRepos.filter((path) => !isPathAmong(path, openPaths, pathOpts));
    const labels = disambiguateLabels(paths);
    return paths.map((path) => ({ path, label: labels.get(path) ?? displayName(path) }));
  });

  const rows = $derived(
    buildFleetRows({
      open: openFacts,
      recents,
      snapshot: $fleetStore.snapshot,
      snapshotError: $fleetStore.snapshotError,
      scanFailures: $fleetStore.scanFailures,
      now: Date.now(),
    }),
  );

  const visibleRows = $derived(byUrgency(applyFilter(rows, filter)));
  const headline = $derived(fleetHeadline(rows));
  const targets = $derived(scanTargets(rows));

  const locTotal = $derived(tally(rows, (row) => row.loc, (value) => value.lines));
  const storageTotal = $derived(tally(rows, (row) => row.storage, (value) => value.bytes));
  const vulnTotal = $derived(tally(rows, (row) => row.health, (value) => value.total));

  /** Every path the sweep should cover: open tabs first, then recents. */
  const sweepPaths = $derived([
    ...openFacts.map((facts) => facts.path),
    ...recents.map((entry) => entry.path),
  ]);

  // The cheap sweep re-runs whenever the set of repositories changes, and on
  // nothing else. Expensive families never run from an effect.
  let lastSwept = "";
  $effect(() => {
    const key = sweepPaths.join("\u0000");
    if (key === lastSwept) return;
    lastSwept = key;
    void fleetStore.refresh(sweepPaths);
  });

  $effect(() => {
    if (focusedIndex > visibleRows.length - 1) focusedIndex = Math.max(0, visibleRows.length - 1);
  });

  async function sweep(family: ScanFamily) {
    const report = await fleetStore.scanAll(family, targets);
    if (!report) return;
    const line = summarizeRun(report, `Scanned ${FAMILY_LABEL[family].toLowerCase()} for`);
    if (isCleanSweep(report)) {
      toastStore.success(line);
      return;
    }
    // A partial sweep names one concrete cause; the rest are on the grid, in
    // the cells they belong to.
    const failure = firstFailure(report);
    toastStore.warning(failure ? `${line} — ${failure.label}: ${failure.error}` : line);
  }

  function onRowKeydown(event: KeyboardEvent, index: number) {
    if (isImeComposition(event)) return;
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      void openRow(visibleRows[index]);
      return;
    }
    if (event.key === "Delete" || event.key === "Backspace") {
      event.preventDefault();
      void removeRow(visibleRows[index]);
      return;
    }
    const next = nextRovingIndex(index, visibleRows.length, event.key as RovingKey, "vertical");
    if (next === null) return;
    event.preventDefault();
    focusedIndex = next;
    const target = document.querySelector<HTMLElement>(`[data-fleet-row="${next}"]`);
    target?.focus();
  }

  async function openRow(row: FleetRow | undefined) {
    if (!row) return;
    await repoStore.openRepo(row.path);
    interfaceStore.setFleetOpen(false);
  }

  async function removeRow(row: FleetRow | undefined) {
    if (!row) return;
    await repoStore.removeRepo(row.path);
  }
</script>

<div class="gp-view flex-1 flex flex-col min-h-0 bg-background" data-testid="fleet-view">
  <!-- Header band: the one sentence, then the totals. -->
  <header class="shrink-0 border-b border-border px-4 py-3 flex flex-col gap-3">
    <div class="flex items-start gap-3">
      <div class="flex items-center gap-2 min-w-0 flex-1">
        <LayoutGrid size={16} class="text-accent shrink-0" />
        <div class="min-w-0">
          <h1 class="text-sm font-semibold text-textPrimary">Fleet</h1>
          <p class="text-[11px] text-textMuted truncate" data-testid="fleet-headline">
            {headline.sentence}
          </p>
        </div>
      </div>
      <div class="flex items-center gap-1.5 shrink-0">
        <button
          type="button"
          class="gp-btn !py-1 !px-2 !text-[11px]"
          onclick={() => void fleetStore.refresh(sweepPaths)}
          disabled={$fleetStore.snapshotLoading}
          title="Re-read worktrees, agent sessions and last activity. Cheap: two git calls per repository."
        >
          <RefreshCw size={11} class={$fleetStore.snapshotLoading ? "animate-spin" : ""} />
          <span>Refresh</span>
        </button>
        <button
          type="button"
          class="gp-icon-btn"
          onclick={() => interfaceStore.setFleetOpen(false)}
          title="Close Fleet"
          aria-label="Close Fleet"
        >
          <X size={14} />
        </button>
      </div>
    </div>

    {#if $fleetStore.snapshotError}
      <div
        class="flex items-start gap-2 rounded-lg border border-rose-500/30 bg-rose-500/10 px-3 py-2 text-[11px] text-rose-700 dark:text-rose-300"
        role="alert"
      >
        <CircleAlert size={12} class="shrink-0 mt-0.5" />
        <span>
          Worktrees, agent sessions and last activity could not be read for any repository:
          {$fleetStore.snapshotError}
        </span>
      </div>
    {/if}

    <!-- Totals. Each one says what it could not count rather than rounding it
         into the number. -->
    <div class="grid grid-cols-2 sm:grid-cols-4 gap-2">
      <div class="gp-card rounded-lg px-3 py-2">
        <div class="text-[10px] uppercase tracking-wider text-textMuted">Open</div>
        <div class="text-sm font-semibold text-textPrimary tabular-nums">{headline.open}</div>
        <div class="text-[10px] text-textMuted">
          {headline.attention} need attention
        </div>
      </div>
      <div class="gp-card rounded-lg px-3 py-2">
        <div class="text-[10px] uppercase tracking-wider text-textMuted">Lines of code</div>
        <div class="text-sm font-semibold text-textPrimary tabular-nums">
          {locTotal.counted > 0 ? locTotal.value.toLocaleString() : "—"}
        </div>
        <div class="text-[10px] text-textMuted truncate" title={describeTally(locTotal)}>
          {describeTally(locTotal) || "across all open repositories"}
        </div>
      </div>
      <div class="gp-card rounded-lg px-3 py-2">
        <div class="text-[10px] uppercase tracking-wider text-textMuted">On disk</div>
        <div class="text-sm font-semibold text-textPrimary tabular-nums">
          {storageTotal.counted > 0 ? humanBytes(storageTotal.value) : "—"}
        </div>
        <div class="text-[10px] text-textMuted truncate" title={describeTally(storageTotal)}>
          {describeTally(storageTotal) || "across all open repositories"}
        </div>
      </div>
      <div class="gp-card rounded-lg px-3 py-2">
        <div class="text-[10px] uppercase tracking-wider text-textMuted">Vulnerabilities</div>
        <div class="text-sm font-semibold text-textPrimary tabular-nums">
          {vulnTotal.counted > 0 ? vulnTotal.value.toLocaleString() : "—"}
        </div>
        <div class="text-[10px] text-textMuted truncate" title={describeTally(vulnTotal)}>
          {describeTally(vulnTotal) || "across all open repositories"}
        </div>
      </div>
    </div>
  </header>

  <!-- Toolbar: filter, and the four scans that cost something. -->
  <div class="shrink-0 border-b border-border px-4 py-2 flex flex-wrap items-center gap-2">
    <div class="flex items-center gap-1" role="group" aria-label="Filter repositories">
      <button
        type="button"
        class="gp-chip {filter === 'all' ? 'ring-1 ring-accent/60' : ''}"
        aria-pressed={filter === "all"}
        onclick={() => (filter = "all")}>All ({rows.length})</button
      >
      <button
        type="button"
        class="gp-chip {filter === 'attention' ? 'ring-1 ring-accent/60' : ''}"
        aria-pressed={filter === "attention"}
        onclick={() => (filter = "attention")}>Needs attention ({headline.attention})</button
      >
    </div>

    <div class="flex-1"></div>

    {#if $fleetStore.scanning && $fleetStore.progress}
      <span class="text-[11px] text-textMuted tabular-nums" role="status">
        {FAMILY_LABEL[$fleetStore.scanning]}: {$fleetStore.progress.done}/{$fleetStore.progress.total}
      </span>
      <button type="button" class="gp-btn !py-1 !px-2 !text-[11px]" onclick={() => fleetStore.cancelScan()}>
        Stop
      </button>
    {:else}
      {#each SCAN_FAMILIES as family (family)}
        <button
          type="button"
          class="gp-btn !py-1 !px-2 !text-[11px]"
          disabled={targets.length === 0}
          onclick={() => void sweep(family)}
          title="Run this scan across every open repository. Nothing here runs on its own — storage walks the whole tree and the audit spawns your package manager."
        >
          {#if family === "loc"}<SquareCode size={11} />
          {:else if family === "storage"}<HardDrive size={11} />
          {:else if family === "health"}<ShieldAlert size={11} />
          {:else}<Boxes size={11} />{/if}
          <span>{FAMILY_LABEL[family]}</span>
        </button>
      {/each}
    {/if}
  </div>

  <!-- The grid. -->
  <div class="flex-1 min-h-0 overflow-auto">
    {#if rows.length === 0}
      <EmptyState
        icon={FolderGit2}
        title="No repositories yet"
        hint="Open a repository and it appears here, alongside every other one you are working in."
        action={{ label: "Open Repository", onClick: () => void repoStore.pickAndOpenRepo() }}
      />
    {:else if visibleRows.length === 0}
      <EmptyState
        icon={LayoutGrid}
        compact
        title="Nothing needs attention"
        hint="Every open repository is clean. Switch to All to see them anyway."
        action={{ label: "Show all", onClick: () => (filter = "all") }}
      />
    {:else}
      <table class="w-full text-[11px] border-collapse">
        <thead class="sticky top-0 z-10 bg-surface">
          <tr class="text-left text-textMuted uppercase tracking-wider text-[10px]">
            <th scope="col" class="font-medium px-3 py-2">Repository</th>
            <th scope="col" class="font-medium px-3 py-2 text-right">Changes</th>
            <th scope="col" class="font-medium px-3 py-2 text-right">Sync</th>
            <th scope="col" class="font-medium px-3 py-2 text-right">Work</th>
            <th scope="col" class="font-medium px-3 py-2 text-right">Activity</th>
            <th scope="col" class="font-medium px-3 py-2 text-right">Lines</th>
            <th scope="col" class="font-medium px-3 py-2 text-right">Storage</th>
            <th scope="col" class="font-medium px-3 py-2 text-right">Vulns</th>
            <th scope="col" class="font-medium px-3 py-2 text-right">Coverage</th>
            <th scope="col" class="font-medium px-3 py-2 text-right w-8"><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          {#each visibleRows as row, index (row.path)}
            <tr
              class="border-t border-border/60 hover:bg-surfaceHover/60 focus-within:bg-surfaceHover/60 {row.presence ===
              'recent'
                ? 'opacity-60'
                : ''}"
              data-testid="fleet-row"
              data-presence={row.presence}
              data-severity={row.severity}
            >
              <!-- Repository: stripe, label, branch, and why it is here. -->
              <td class="px-3 py-2 align-top">
                <div
                  class="flex items-start gap-2 focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60 rounded"
                  role="button"
                  tabindex={index === focusedIndex ? 0 : -1}
                  data-fleet-row={index}
                  onkeydown={(e) => onRowKeydown(e, index)}
                  onclick={() => void openRow(row)}
                  onfocus={() => (focusedIndex = index)}
                  title={row.presence === "recent"
                    ? `${row.path} — not open. Click to open it.`
                    : `${row.path} — click to switch to this repository.`}
                >
                  <span class="mt-1 h-3 w-1 rounded-full shrink-0 {SEVERITY_STRIPE[row.severity]}"></span>
                  <span class="min-w-0">
                    <span class="flex items-center gap-1.5">
                      <span class="font-medium text-textPrimary truncate">{row.label}</span>
                      {#if row.presence === "recent"}
                        <span class="gp-chip !py-0 !px-1 !text-[9px]">not open</span>
                      {/if}
                      {#if row.watchWarning}
                        <CircleAlert
                          size={10}
                          class="text-amber-600 dark:text-amber-400 shrink-0"
                          aria-label={row.watchWarning}
                        />
                      {/if}
                    </span>
                    <span class="flex items-center gap-1 text-[10px] text-textMuted">
                      {#if row.branch}
                        <GitBranch size={9} class="shrink-0" />
                        <span class="truncate max-w-[10rem]">{row.branch}</span>
                        <span aria-hidden="true">·</span>
                      {/if}
                      <span class="truncate">{row.headline}</span>
                    </span>
                  </span>
                </div>
              </td>

              <td class="px-3 py-2 align-top">
                <FleetCell
                  cell={row.changes}
                  label="Changes"
                  partialNote="Some status rows could not be parsed, so the line counts are floors."
                >
                  {#if row.changes.kind === "read"}
                    <span class="text-textPrimary">{row.changes.value.files}</span>
                    <span class="text-textMuted text-[10px]">
                      {#if row.changes.value.conflicted > 0}
                        <span class="text-rose-600 dark:text-rose-400"
                          >{row.changes.value.conflicted}✗</span
                        >
                      {/if}
                      +{row.changes.value.additions}/−{row.changes.value.deletions}
                    </span>
                  {/if}
                </FleetCell>
              </td>

              <td class="px-3 py-2 align-top">
                <FleetCell
                  cell={row.sync}
                  label="Sync"
                  partialNote="The stash could not be read, so the stash count is a floor."
                >
                  {#if row.sync.kind === "read"}
                    <span class="text-textPrimary">↑{row.sync.value.ahead}</span>
                    <span class="text-textMuted text-[10px]">
                      ↓{row.sync.value.behind}{row.sync.value.stash > 0
                        ? ` · ${row.sync.value.stash} stashed`
                        : ""}
                    </span>
                  {/if}
                </FleetCell>
              </td>

              <td class="px-3 py-2 align-top">
                <FleetCell cell={row.work} label="Worktrees and agents">
                  {#if row.work.kind === "read"}
                    <span class="inline-flex items-center gap-1 text-textPrimary">
                      <Trees size={10} class="shrink-0" />{row.work.value.worktrees}
                    </span>
                    {#if row.work.value.agentSessions > 0}
                      <span
                        class="text-[10px] text-textMuted"
                        title="Agent sessions: {row.work.value.agentKinds.join(', ')}"
                        >{row.work.value.agentSessions} agent</span
                      >
                    {/if}
                  {/if}
                </FleetCell>
              </td>

              <td class="px-3 py-2 align-top">
                <FleetCell cell={row.activity} label="Last commit">
                  {#if row.activity.kind === "read"}
                    <span class="inline-flex items-center gap-1 text-textMuted">
                      <Clock size={10} class="shrink-0" />
                      {row.activity.value > 0 ? formatAge(row.activity.value) : "no commits"}
                    </span>
                  {/if}
                </FleetCell>
              </td>

              <td class="px-3 py-2 align-top">
                <FleetCell
                  cell={row.loc}
                  label="Lines of code"
                  partialNote="The language scan stopped at a budget, so this total is a floor."
                >
                  {#if row.loc.kind === "read"}
                    <span class="text-textPrimary">{row.loc.value.lines.toLocaleString()}</span>
                    {#if row.loc.value.language}
                      <span class="text-[10px] text-textMuted">{row.loc.value.language}</span>
                    {/if}
                  {/if}
                </FleetCell>
              </td>

              <td class="px-3 py-2 align-top">
                <FleetCell
                  cell={row.storage}
                  label="Storage"
                  partialNote="The disk walk stopped at a budget, so these bytes are a floor."
                >
                  {#if row.storage.kind === "read"}
                    <span class="text-textPrimary">{humanBytes(row.storage.value.bytes)}</span>
                    {#if row.storage.value.reclaimableBytes > 0}
                      <span
                        class="text-[10px] text-amber-600 dark:text-amber-400"
                        title="Build output and caches that can be deleted"
                        >{humanBytes(row.storage.value.reclaimableBytes)} reclaimable</span
                      >
                    {/if}
                  {/if}
                </FleetCell>
              </td>

              <td class="px-3 py-2 align-top">
                <FleetCell
                  cell={row.health}
                  label="Vulnerabilities"
                  partialNote="Some audit targets never ran, so this count is a floor — not a clean bill of health."
                >
                  {#if row.health.kind === "read"}
                    <span
                      class={row.health.value.total > 0
                        ? "text-rose-600 dark:text-rose-400"
                        : "text-textPrimary"}
                      title={formatAuditCounts(row.health.value, {
                        complete: row.health.value.complete,
                        ran: true,
                      })}>{row.health.value.total}</span
                    >
                  {/if}
                </FleetCell>
              </td>

              <td class="px-3 py-2 align-top">
                <FleetCell
                  cell={row.coverage}
                  label="Coverage"
                  partialNote="The coverage scan was cut short, so this percentage covers part of the repository."
                >
                  {#if row.coverage.kind === "read"}
                    <span class="text-textPrimary">{row.coverage.value.toFixed(1)}%</span>
                  {/if}
                </FleetCell>
              </td>

              <td class="px-3 py-2 align-top text-right">
                <button
                  type="button"
                  class="p-1 rounded text-textMuted hover:text-rose-400 hover:bg-surfaceHover transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
                  title={row.presence === "open"
                    ? `Remove ${row.label} from Fleet (closes repository)`
                    : `Remove ${row.label} from Fleet`}
                  aria-label={`Remove ${row.label} from Fleet`}
                  data-testid="fleet-remove-repo"
                  onclick={(e) => {
                    e.stopPropagation();
                    void removeRow(row);
                  }}
                >
                  <Trash2 size={12} />
                </button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>

      {#if $fleetStore.snapshot?.truncated}
        <p class="px-4 py-2 text-[10px] text-amber-600 dark:text-amber-400" role="status">
          The sweep did not reach every repository, so some rows show worktrees and activity as not
          scanned. Refresh to try the rest.
        </p>
      {/if}
    {/if}
  </div>
</div>
