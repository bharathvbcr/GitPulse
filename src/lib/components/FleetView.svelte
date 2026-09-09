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
  import GlobalCleaner from "./GlobalCleaner.svelte";
  let cleanerOpen = $state(false);
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
   * goes through `FleetCell`, and every aggregate above the grid goes through
   * `tally` or `fleetPulse`, both of which carry what they could not count.
   */
  import {
    Boxes,
    CircleAlert,
    Clock,
    FolderGit2,
    GitBranch,
    HardDrive,
    LayoutGrid,
    ArrowDownToLine,
    ChevronDown,
    ChevronRight,
    CloudDownload,
    Columns3,
    RefreshCw,
    Search,
    ShieldAlert,
    Square,
    SquareCheck,
    SquareCode,
    Trash2,
    Trees,
    X,
  } from "@lucide/svelte";
  import { repoStore } from "../stores/repoStore";
  import { interfaceStore } from "../stores/interfaceStore";
  import { toastStore } from "../stores/toastStore";
  import { fleetStore } from "../fleet/fleetStore";
  import { buildFleetRows } from "../fleet/row";
  import {
    bandsBySeverity,
    byUrgency,
    effectiveCollapse,
    fleetHeadline,
    tally,
  } from "../fleet/aggregate";
  import { fleetLanguageMix, fleetPulse } from "../fleet/pulse";
  import { foldFleetLanguages } from "../fleet/languages";
  import {
    DEFAULT_SORT,
    HIDEABLE_COLUMNS,
    hiddenFailures,
    nextSort,
    searchRows,
    sortRows,
    type SortKey,
    type SortState,
  } from "../fleet/sort";
  import { FAMILY_LABEL, SCAN_FAMILIES } from "../fleet/types";
  import { disambiguateLabels, displayName, isPathAmong, isCaseInsensitiveFs } from "../repos/paths";
  import { plural } from "../format";
  import { formatAge, humanBytes } from "../storage/format";
  import { formatAuditCounts } from "../health/format";
  import { firstFailure, isCleanSweep, summarizeRun } from "../repos/workspaceOps";
  import { formatError } from "../ui/formatError";
  import { nextRovingIndex, type RovingKey } from "../dom/rovingFocus";
  import { isImeComposition } from "../keyboard/imeGuard";
  import EmptyState from "./EmptyState.svelte";
  import FleetCell from "./FleetCell.svelte";
  import FleetLanguageBar from "./FleetLanguageBar.svelte";
  import FleetPulsePanel from "./FleetPulse.svelte";
  import FleetSparkline from "./FleetSparkline.svelte";
  import FleetTotals, { type FleetTotal } from "./FleetTotals.svelte";

  let filter = $state<FleetFilter>("all");
  let query = $state("");
  let sort = $state<SortState>(DEFAULT_SORT);
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

  // Filter, then search, then order. The severity sort delegates to the same
  // ranking `byUrgency` uses, so the default order is unchanged from before
  // sorting existed.
  const matched = $derived(searchRows(applyFilter(rows, filter), query));
  const ordered = $derived(
    sort.key === "severity" && sort.direction === "asc" ? byUrgency(matched) : sortRows(matched, sort),
  );

  /**
   * Whether the grid reads as a triage queue or as a plain table.
   *
   * Banding is only meaningful under the severity sort: headings over rows
   * ordered by lines of code would be boundaries that mean nothing. So sorting
   * by any other column drops to a flat list, and clicking back to Severity
   * restores the bands.
   */
  const banded = $derived(sort.key === "severity" && sort.direction === "asc");

  /** Severity bands the reader has collapsed. Clean starts collapsed. */
  let collapsedBands = $state<Set<string>>(new Set(["clean"]));

  /**
   * True once the reader has expanded or collapsed a band themselves.
   *
   * `effectiveCollapse` constrains only the default, so this is what tells the
   * two apart: a state nobody chose may not empty the grid, and a state someone
   * chose is theirs to hold.
   */
  let bandsTouched = $state(false);

  const bands = $derived(banded ? bandsBySeverity(ordered) : []);

  const collapsed = $derived(effectiveCollapse(bands, collapsedBands, bandsTouched));

  /**
   * The rows actually on screen, in render order.
   *
   * Derived from the bands rather than from `ordered` so that keyboard
   * navigation, the roving index and the focus helpers all address exactly
   * what is visible — a collapsed band's rows must not be reachable by arrow
   * key while invisible.
   */
  const visibleRows = $derived(
    banded ? bands.filter((b) => !collapsed.has(b.severity)).flatMap((b) => b.rows) : ordered,
  );

  function toggleBand(severity: string) {
    bandsTouched = true;
    const next = new Set(collapsedBands);
    if (next.has(severity)) next.delete(severity);
    else next.add(severity);
    collapsedBands = next;
  }

  /** The render index of a row, so a banded table keeps one roving sequence. */
  function rowIndex(path: string): number {
    return visibleRows.findIndex((row) => row.path === path);
  }
  const headline = $derived(fleetHeadline(rows));
  const targets = $derived(scanTargets(rows));

  const locTotal = $derived(tally(rows, (row) => row.loc, (value) => value.lines));
  const storageTotal = $derived(tally(rows, (row) => row.storage, (value) => value.bytes));
  const vulnTotal = $derived(tally(rows, (row) => row.health, (value) => value.total));
  const commitTotal = $derived(tally(rows, (row) => row.commits, (value) => value.commits));

  const pulse = $derived(fleetPulse(rows));
  const languages = $derived(fleetLanguageMix(rows));

  /**
   * The five headline readings, in the order a reader wants them.
   *
   * Each carries its own tally so the strip can decide, per reading, whether a
   * bare number is honest — never a formatted string on its own, which is how
   * a total loses the thing that qualifies it.
   */
  const totalStrip = $derived<FleetTotal[]>([
    {
      key: "open",
      label: "Open",
      text: `${headline.open}`,
      tally: null,
      note: headline.attentionClause,
    },
    {
      key: "commits",
      label: pulse.windowDays > 0 ? `Commits · ${pulse.windowDays}d` : "Commits",
      text: commitTotal.counted > 0 ? commitTotal.value.toLocaleString() : "—",
      tally: commitTotal,
    },
    {
      key: "loc",
      label: "Lines",
      text: locTotal.counted > 0 ? locTotal.value.toLocaleString() : "—",
      tally: locTotal,
    },
    {
      key: "storage",
      label: "On disk",
      text: storageTotal.counted > 0 ? humanBytes(storageTotal.value) : "—",
      tally: storageTotal,
    },
    {
      key: "vulns",
      label: "Vulns",
      text: vulnTotal.counted > 0 ? vulnTotal.value.toLocaleString() : "—",
      tally: vulnTotal,
    },
  ]);

  /** Repositories whose scan is actually in flight — never the whole queue. */
  const running = $derived(new Set($fleetStore.progress?.running ?? []));

  /**
   * The fetch or pull sweep currently running, and how far it has got.
   *
   * Kept here rather than in `fleetStore`: the fleet store owns *measuring*,
   * and these two mutate repositories. Routing them through the store would
   * put a write behind a name that has meant "read-only scan" everywhere else
   * in this directory.
   */
  let syncing = $state<"fetch" | "pull" | null>(null);
  let syncProgress = $state<{ done: number; total: number } | null>(null);
  /** Paths a fetch/pull is actually working on, for the same honest per-row
   *  marker the scans use: a queued repository is not a running one. */
  let syncPaths = $state<string[]>([]);
  let syncToken: { aborted: boolean } | null = null;

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

  /** Column headers, in render order, with the sort key each one carries. */
  const COLUMNS: readonly { key: SortKey; label: string; align: "left" | "right" }[] = [
    { key: "repository", label: "Repository", align: "left" },
    { key: "changes", label: "Changes", align: "right" },
    { key: "sync", label: "Sync", align: "right" },
    { key: "work", label: "Work", align: "right" },
    { key: "commits", label: "Commits", align: "right" },
    { key: "activity", label: "Activity", align: "right" },
    { key: "loc", label: "Lines", align: "right" },
    { key: "storage", label: "Storage", align: "right" },
    { key: "health", label: "Vulns", align: "right" },
    { key: "coverage", label: "Coverage", align: "right" },
  ];

  /** Columns the reader has hidden, and the ones actually rendered. */
  const hiddenColumns = $derived(new Set($interfaceStore.fleetHiddenColumns));
  const shownColumns = $derived(COLUMNS.filter((column) => !hiddenColumns.has(column.key)));

  /**
   * Failures a hidden column is keeping off screen.
   *
   * Hiding a column is a display choice; hiding a *failure* is not one this
   * grid gets to make on the reader's behalf. So the count surfaces above the
   * table with the column named, and it disappears the moment the column comes
   * back or the scan succeeds.
   */
  const hiddenIssues = $derived(hiddenFailures(rows, hiddenColumns));

  /** Row padding, so a compact grid fits roughly twice as many repositories. */
  const cellPad = $derived($interfaceStore.fleetCompact ? "px-3 py-1" : "px-3 py-2");

  let columnMenuOpen = $state(false);

  function ariaSort(key: SortKey): "ascending" | "descending" | "none" {
    if (sort.key !== key) return "none";
    return sort.direction === "asc" ? "ascending" : "descending";
  }

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

  /**
   * Fetches or pulls every open repository.
   *
   * Delegates to `repoStore.runAcrossOpenRepos`, which is the app's one owner
   * of this operation — the same call the workspace header makes. It skips
   * repositories that are parked mid-operation, still loading, or holding
   * conflicts, and reports them as *skipped* rather than counting them as
   * fetched, which is the whole reason Fleet can offer the button at all.
   */
  async function syncAll(kind: "fetch" | "pull") {
    if (syncing !== null) return;
    const token = { aborted: false };
    syncToken = token;
    syncing = kind;
    syncProgress = { done: 0, total: targets.length };
    syncPaths = [];
    try {
      const report = await repoStore.runAcrossOpenRepos(kind, {
        signal: token,
        onStart: (target) => {
          syncPaths = [...syncPaths, target.path];
        },
        onProgress: (done, total, latest) => {
          syncProgress = { done, total };
          syncPaths = syncPaths.filter((path) => path !== latest.path);
        },
      });
      const line = summarizeRun(report, kind === "fetch" ? "Fetched" : "Pulled");
      if (isCleanSweep(report)) {
        toastStore.success(line);
        return;
      }
      // A partial sweep names one concrete cause; the rest are on the grid, in
      // the rows they belong to.
      const failure = firstFailure(report);
      toastStore.warning(failure ? `${line} — ${failure.label}: ${failure.error}` : line);
    } finally {
      syncing = null;
      syncProgress = null;
      syncPaths = [];
      syncToken = null;
    }
  }

  /**
   * Fetches one repository, from the row that is behind.
   *
   * Open repositories only, for the same reason `scanTargets` restricts the
   * sweeps: a recents path may no longer resolve, and reaching the network on
   * behalf of a repository the user has not opened is not this view's call.
   */
  async function fetchRow(row: FleetRow) {
    if (row.presence !== "open" || syncing !== null) return;
    syncing = "fetch";
    syncProgress = { done: 0, total: 1 };
    syncPaths = [row.path];
    try {
      // A run of one, through the same owner — so this row's parked merge or
      // unresolved conflict is skipped here exactly as it would be in a sweep,
      // rather than being fetched by a shortcut that does not know the rules.
      const report = await repoStore.runAcrossOpenRepos("fetch", { only: [row.path] });
      const result = report.results[0];
      if (!result) {
        toastStore.warning(`${row.label} is no longer open.`);
      } else if (result.status === "ok") {
        toastStore.success(`Fetched ${row.label}.`);
      } else if (result.status === "skipped") {
        toastStore.warning(`Skipped ${row.label} — ${result.reason ?? "it is not in a fetchable state."}`);
      } else {
        toastStore.warning(`Could not fetch ${row.label}: ${result.error ?? "the fetch failed."}`);
      }
    } catch (err: unknown) {
      toastStore.warning(`Could not fetch ${row.label}: ${formatError(err)}`);
    } finally {
      syncing = null;
      syncProgress = null;
      syncPaths = [];
    }
  }

  /**
   * Scans one repository for one family, from the cell that is missing it.
   *
   * Only for open repositories, exactly as `scanTargets` restricts the sweeps:
   * a recents path may not resolve any more, and scanning it would spawn work
   * against a repository the user has not opened.
   */
  function scanCell(row: FleetRow, family: ScanFamily): (() => void) | undefined {
    if (row.presence !== "open") return undefined;
    if ($fleetStore.scanning !== null) return undefined;
    return () => void fleetStore.scanOne(family, row.path);
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

  /**
   * Page-level shortcuts, for the posture people are actually in.
   *
   * Bound on the view's own container rather than the window: Fleet is *hidden*
   * rather than unmounted when the repository pane is showing, so a window
   * listener would keep firing `/` and `s` at someone typing into the commit
   * box behind it.
   *
   * Every branch bails on a modifier or a text field. A shortcut that steals a
   * keystroke from an input is worse than no shortcut.
   */
  function onGridKeydown(event: KeyboardEvent) {
    if (isImeComposition(event)) return;
    if (event.metaKey || event.ctrlKey || event.altKey) return;
    const target = event.target as HTMLElement | null;
    const tag = target?.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || target?.isContentEditable) {
      // Escape is the one key an input should hand back: it means "give me the
      // grid again", which is exactly what a filter box needs.
      if (event.key === "Escape" && tag === "INPUT") {
        event.preventDefault();
        query = "";
        (target as HTMLInputElement).blur();
      }
      return;
    }
    if (event.key === "/") {
      event.preventDefault();
      document.getElementById("gitpulse-fleet-filter")?.focus();
      return;
    }
    if (event.key === "s" || event.key === "S") {
      // Cycles the sort through the columns actually on screen, so a hidden
      // column can never become an invisible sort nobody can undo.
      event.preventDefault();
      const keys = shownColumns.map((column) => column.key);
      const at = keys.indexOf(sort.key);
      const next = keys[(at + 1) % keys.length];
      if (next) sort = nextSort({ ...sort, key: next === sort.key ? sort.key : next }, next);
      return;
    }
    if (event.key === "p" || event.key === "P") {
      event.preventDefault();
      interfaceStore.toggleFleetPulse();
      return;
    }
    if (event.key === "r" || event.key === "R") {
      event.preventDefault();
      void fleetStore.refresh(sweepPaths);
      return;
    }
    if (/^[1-9]$/.test(event.key)) {
      event.preventDefault();
      const index = Number(event.key) - 1;
      if (index < visibleRows.length) {
        focusedIndex = index;
        document.querySelector<HTMLElement>(`[data-fleet-row="${index}"]`)?.focus();
      }
    }
  }

  /** Moves keyboard focus to a row named elsewhere on the page. */
  function focusRow(path: string) {
    const index = visibleRows.findIndex((row) => row.path === path);
    if (index < 0) return;
    focusedIndex = index;
    document.querySelector<HTMLElement>(`[data-fleet-row="${index}"]`)?.focus();
  }
</script>

<!-- `region` really is non-interactive, so the rule is right about the role
     — but every alternative is worse. A window listener would keep firing
     while Fleet is *hidden* rather than unmounted, stealing "/" and "s" from
     the commit box behind it; promoting the container to `grid` would claim a
     composite-widget contract (roving tabindex, full arrow-key navigation)
     that this list does not implement. The handler stays on a focusable
     region, reachable because keydown bubbles from whatever inside Fleet has
     focus.
     Accepted, not fixed: every shortcut is an accelerator for an action that
     also has a real focusable control, and `onGridKeydown` returns immediately
     for INPUT, TEXTAREA and contenteditable targets, so it never eats typing. -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
  class="gp-view flex-1 flex flex-col min-h-0 bg-background"
  data-testid="fleet-view"
  role="region"
  aria-label="Fleet"
  tabindex="-1"
  onkeydown={onGridKeydown}
>
  <!-- Header band: the one sentence, then the totals. -->
  <header class="shrink-0 border-b border-border gp-section-edge px-4 py-3 flex flex-col gap-3">
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
          class="gp-btn py-1! px-2! text-[11px]!"
          onclick={() => void fleetStore.refresh(sweepPaths)}
          disabled={$fleetStore.snapshotLoading}
          title="Re-read worktrees, agent sessions, commit history and last activity. Cheap: two git calls per repository, three for one that has been quiet all quarter."
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
          Worktrees, agent sessions, commit history and last activity could not be read for any
          repository: {$fleetStore.snapshotError}
        </span>
      </div>
    {/if}

    <!-- Totals, as one strip. Each reading still says what it could not count
         — but only when there is something it could not count, which is what
         let five cards become one line. -->
    <FleetTotals totals={totalStrip} />
  </header>

  <div class="shrink-0 px-4 py-2 border-b border-border">
    <button class="gp-btn" aria-expanded={cleanerOpen} onclick={() => cleanerOpen = !cleanerOpen}>Global build cleaner</button>
    {#if cleanerOpen}<div class="max-h-[60vh] overflow-auto py-4"><GlobalCleaner /></div>{/if}
  </div>

  <!-- Fleet Pulse: the workspace's rhythm, from what the cheap sweep already read. -->
  <FleetPulsePanel
    {pulse}
    {languages}
    open={$interfaceStore.fleetPulseOpen}
    onToggle={() => interfaceStore.toggleFleetPulse()}
    onSelect={focusRow}
    onRemove={(path) => void removeRow(rows.find((row) => row.path === path))}
    windowDays={$fleetStore.windowDays}
    loading={$fleetStore.snapshotLoading}
    onWindow={(days) => void fleetStore.setWindow(days)}
  />

  <!-- Toolbar: filter, search, and the four scans that cost something. -->
  <div class="shrink-0 border-b border-border gp-section-edge px-4 py-2 flex flex-wrap items-center gap-2">
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

    <!-- Same shape as the History filter, so the one search box in the app
         does not have two appearances. -->
    <div
      class="flex items-center gap-1.5 w-56 bg-background border border-border/80 rounded-full px-2.5 py-0.5 transition-colors duration-150 focus-within:border-accent/60 focus-within:shadow-(--ring-focus)"
    >
      <Search size={12} class="text-textMuted shrink-0" />
      <label class="sr-only" for="gitpulse-fleet-filter">
        Filter repositories by name, path, branch or language
      </label>
      <input
        id="gitpulse-fleet-filter"
        type="text"
        bind:value={query}
        placeholder="Filter by name, branch, language…"
        class="w-full bg-transparent text-textPrimary placeholder:text-textMuted/60 text-[11px] focus:outline-hidden"
        data-testid="fleet-search"
      />
      {#if query !== ""}
        <button
          type="button"
          onclick={() => (query = "")}
          aria-label="Clear repository filter"
          title="Clear filter"
          class="gp-icon-btn p-0.5!"
        >
          <X size={11} />
        </button>
      {/if}
    </div>

    {#if query.trim() !== ""}
      <span class="text-[10px] text-textMuted tabular-nums" role="status">
        {visibleRows.length} of {applyFilter(rows, filter).length} match
      </span>
    {/if}

    <div class="flex-1"></div>

    <!-- Fetch and pull are the two operations that change repositories; they
         sit apart from the four scans, which only measure them. -->
    {#if syncing && syncProgress}
      <span class="text-[11px] text-textMuted tabular-nums" role="status">
        {syncing === "fetch" ? "Fetching" : "Pulling"}: {syncProgress.done}/{syncProgress.total}
      </span>
      <button
        type="button"
        class="gp-btn py-1! px-2! text-[11px]!"
        onclick={() => {
          if (syncToken) syncToken.aborted = true;
        }}
      >
        Stop
      </button>
    {:else}
      <button
        type="button"
        class="gp-btn py-1! px-2! text-[11px]!"
        disabled={targets.length === 0 || $fleetStore.scanning !== null}
        onclick={() => void syncAll("fetch")}
        title="Fetch every open repository. Repositories parked mid-operation, still loading, or holding conflicts are skipped and reported as skipped — never counted as fetched."
      >
        <CloudDownload size={11} />
        <span>Fetch all</span>
      </button>
      <button
        type="button"
        class="gp-btn py-1! px-2! text-[11px]!"
        disabled={targets.length === 0 || $fleetStore.scanning !== null}
        onclick={() => void syncAll("pull")}
        title="Pull every open repository. Same skip rules as Fetch all: nothing with uncommitted work or a parked operation is touched."
      >
        <ArrowDownToLine size={11} />
        <span>Pull all</span>
      </button>
      <div class="h-3.5 w-1 rounded-full bg-border/50" aria-hidden="true"></div>
    {/if}

    <!-- Columns and density. A grid of eleven columns is not the same grid for
         someone auditing dependencies and someone reclaiming disk. -->
    <div class="relative">
      <button
        type="button"
        class="gp-btn py-1! px-2! text-[11px]!"
        aria-expanded={columnMenuOpen}
        aria-haspopup="true"
        onclick={() => (columnMenuOpen = !columnMenuOpen)}
        title="Choose which columns the grid shows, and how tightly it packs them."
        data-testid="fleet-columns-toggle"
      >
        <Columns3 size={11} />
        <span>Columns</span>
        {#if hiddenColumns.size > 0}
          <span class="tabular-nums text-textMuted">{shownColumns.length}/{COLUMNS.length}</span>
        {/if}
      </button>
      {#if columnMenuOpen}
        <div
          class="absolute right-0 top-full mt-1 z-20 gp-menu gp-pop p-2 w-52 flex flex-col gap-0.5"
          role="group"
          aria-label="Grid columns"
          data-testid="fleet-columns-menu"
        >
          {#each HIDEABLE_COLUMNS as key (key)}
            {@const column = COLUMNS.find((c) => c.key === key)}
            {#if column}
              <button
                type="button"
                class="flex items-center gap-2 px-1.5 py-1 rounded text-[11px] text-left hover:bg-surfaceHover transition-colors focus:outline-hidden focus-visible:ring-2 focus-visible:ring-accent/60"
                role="switch"
                aria-checked={!hiddenColumns.has(key)}
                onclick={() => interfaceStore.toggleFleetColumn(key)}
              >
                {#if hiddenColumns.has(key)}
                  <Square size={11} class="text-textMuted shrink-0" />
                {:else}
                  <SquareCheck size={11} class="text-accent shrink-0" />
                {/if}
                <span class="flex-1 text-textPrimary">{column.label}</span>
              </button>
            {/if}
          {/each}
          <div class="border-t border-border/60 mt-1 pt-1 flex flex-col gap-0.5">
            <button
              type="button"
              class="flex items-center gap-2 px-1.5 py-1 rounded text-[11px] text-left hover:bg-surfaceHover transition-colors focus:outline-hidden focus-visible:ring-2 focus-visible:ring-accent/60"
              role="switch"
              aria-checked={$interfaceStore.fleetCompact}
              onclick={() => interfaceStore.toggleFleetCompact()}
            >
              {#if $interfaceStore.fleetCompact}
                <SquareCheck size={11} class="text-accent shrink-0" />
              {:else}
                <Square size={11} class="text-textMuted shrink-0" />
              {/if}
              <span class="flex-1 text-textPrimary">Compact rows</span>
            </button>
            <button
              type="button"
              class="px-1.5 py-1 rounded text-[11px] text-left text-textMuted hover:text-textPrimary hover:bg-surfaceHover transition-colors focus:outline-hidden focus-visible:ring-2 focus-visible:ring-accent/60 disabled:opacity-50"
              disabled={hiddenColumns.size === 0}
              onclick={() => interfaceStore.showAllFleetColumns()}
            >
              Show all columns
            </button>
          </div>
        </div>
      {/if}
    </div>

    {#if $fleetStore.scanning && $fleetStore.progress}
      <span class="text-[11px] text-textMuted tabular-nums" role="status">
        {FAMILY_LABEL[$fleetStore.scanning]}: {$fleetStore.progress.done}/{$fleetStore.progress.total}
      </span>
      <button type="button" class="gp-btn py-1! px-2! text-[11px]!" onclick={() => fleetStore.cancelScan()}>
        Stop
      </button>
    {:else}
      {#each SCAN_FAMILIES as family (family)}
        <button
          type="button"
          class="gp-btn py-1! px-2! text-[11px]!"
          disabled={targets.length === 0 || syncing !== null}
          onclick={() => void sweep(family)}
          title="Run this scan across every open repository. Nothing here runs on its own — storage walks the whole tree and the audit spawns your package manager. To fill in one repository, click its cell instead."
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

  <!-- A hidden column is still a column that could not be read. Letting the
       failure disappear with it would be the same lie as a blank cell, moved
       up a level — so hiding is allowed, and what hiding cost is reported. -->
  {#if hiddenIssues.length > 0}
    <p
      class="shrink-0 border-b border-border px-4 py-1.5 text-[10px] text-amber-600 dark:text-amber-400"
      role="status"
      data-testid="fleet-hidden-failures"
    >
      Hidden columns are holding failures:
      {hiddenIssues
        .map(
          (issue) =>
            `${COLUMNS.find((c) => c.key === issue.key)?.label ?? issue.key} (${issue.count})`,
        )
        .join(", ")}. Show the column to see which repositories.
    </p>
  {/if}

  <!-- The grid. -->
  <div class="flex-1 min-h-0 overflow-auto">
    {#if rows.length === 0}
      <EmptyState
        icon={FolderGit2}
        title="No repositories yet"
        hint="Open a repository and it appears here, alongside every other one you are working in."
        action={{ label: "Open Repository", onClick: () => void repoStore.pickAndOpenRepo() }}
      />
    {:else if visibleRows.length === 0 && query.trim() !== ""}
      <EmptyState
        icon={Search}
        compact
        title="Nothing matches that"
        hint="No repository name, path, branch or recorded language contains that text."
        action={{ label: "Clear filter", onClick: () => (query = "") }}
      />
    {:else if visibleRows.length === 0 && matched.length > 0}
      <!-- Rows exist and every band holding them is collapsed. This can only be
           reached deliberately — `effectiveCollapse` refuses to let the default
           empty the grid — so it names the real cause instead of blaming a
           filter that is not on. -->
      <EmptyState
        icon={LayoutGrid}
        compact
        title="Every band is collapsed"
        hint="{plural(matched.length, 'repository', 'repositories')} {matched.length === 1
          ? 'is'
          : 'are'} hidden behind collapsed severity headings."
        action={{
          label: "Expand all",
          onClick: () => {
            bandsTouched = true;
            collapsedBands = new Set();
          },
        }}
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
            {#each shownColumns as column (column.key)}
              <th
                scope="col"
                class="font-medium {cellPad} {column.align === 'right' ? 'text-right' : ''}"
                aria-sort={ariaSort(column.key)}
              >
                <button
                  type="button"
                  class="inline-flex items-center gap-1 hover:text-textPrimary transition-colors focus:outline-hidden focus-visible:ring-2 focus-visible:ring-accent/60 rounded {column.align ===
                  'right'
                    ? 'flex-row-reverse'
                    : ''} {sort.key === column.key ? 'text-textPrimary' : ''}"
                  onclick={() => (sort = nextSort(sort, column.key))}
                  title="Sort by {column.label}. Repositories with no measurement stay at the bottom in both directions — 'not scanned' is not a low score."
                >
                  <span>{column.label}</span>
                  {#if sort.key === column.key}
                    <span aria-hidden="true" class="text-accent"
                      >{sort.direction === "asc" ? "▲" : "▼"}</span
                    >
                  {/if}
                </button>
              </th>
            {/each}
            <th scope="col" class="font-medium {cellPad} text-right w-8"
              ><span class="sr-only">Actions</span></th
            >
          </tr>
        </thead>
        {#snippet repoRow(row: FleetRow, index: number)}
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
              <td class="{cellPad} align-top">
                <div
                  class="flex items-start gap-2 focus:outline-hidden focus-visible:ring-2 focus-visible:ring-accent/60 rounded"
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
                        <span class="gp-chip py-0! px-1! text-[9px]!">not open</span>
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
                        <span class="truncate max-w-40">{row.branch}</span>
                        <span aria-hidden="true">·</span>
                      {/if}
                      <span class="truncate">{row.headline}</span>
                    </span>
                  </span>
                </div>
              </td>

              {#if !hiddenColumns.has("changes")}
              <td class="{cellPad} align-top">
                <FleetCell
                  cell={row.changes}
                  label="Changes"
                  partialNote="Some status rows could not be parsed, so the line counts are floors."
                >
                  {#if row.changes.kind === "read"}
                    <button type="button" class="text-textPrimary hover:text-accent hover:underline" title="Preview uncommitted files and modifications in {row.label}"
                      onclick={() => void repoStore.previewUncommitted(row.path)}>
                    <span>{row.changes.value.files}</span>
                    <span class="text-textMuted text-[10px]">
                      {#if row.changes.value.conflicted > 0}
                        <span class="text-rose-600 dark:text-rose-400"
                          >{row.changes.value.conflicted}✗</span
                        >
                      {/if}
                      +{row.changes.value.additions}/−{row.changes.value.deletions}
                    </span>
                    </button>
                  {/if}
                </FleetCell>
              </td>
              {/if}

              {#if !hiddenColumns.has("sync")}
              <td class="{cellPad} align-top">
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
                    {#if row.presence === "open"}
                      <!-- The action belongs where the fact is. A row showing
                           "↓3" is exactly where someone decides to fetch. -->
                      <button
                        type="button"
                        class="p-0.5 rounded text-textMuted/60 hover:text-accent hover:bg-surfaceHover transition-colors focus:outline-hidden focus-visible:ring-2 focus-visible:ring-accent/60 disabled:opacity-40"
                        disabled={syncing !== null || $fleetStore.scanning !== null}
                        title="Fetch {row.label}"
                        aria-label="Fetch {row.label}"
                        data-testid="fleet-fetch-repo"
                        onclick={(e) => {
                          e.stopPropagation();
                          void fetchRow(row);
                        }}
                      >
                        {#if syncPaths.includes(row.path)}
                          <RefreshCw size={10} class="animate-spin motion-reduce:animate-none" />
                        {:else}
                          <CloudDownload size={10} />
                        {/if}
                      </button>
                    {/if}
                  {/if}
                </FleetCell>
              </td>
              {/if}

              {#if !hiddenColumns.has("work")}
              <td class="{cellPad} align-top">
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
              {/if}

              <!-- Commits: the window's shape, then its total. A window that
                   really is empty renders as a measured zero, not an absence. -->
              {#if !hiddenColumns.has("commits")}
              <td class="{cellPad} align-top">
                <FleetCell
                  cell={row.commits}
                  label="Commits"
                  partialNote="The commit walk stopped at its cap, so these counts are floors."
                >
                  {#if row.commits.kind === "read"}
                    <span class="inline-flex items-center gap-1.5">
                      <FleetSparkline
                        counts={row.commits.value.daily}
                        windowDays={row.commits.value.windowDays}
                        total={row.commits.value.commits}
                        partial={row.commits.partial}
                        height={12}
                        barWidth={2}
                        maxBars={13}
                        label={`${row.label} commits`}
                      />
                      <span class="text-textPrimary">{row.commits.value.commits}</span>
                    </span>
                    <span
                      class="text-[10px] text-textMuted"
                      title="{row.commits.value.authors} author {row.commits.value.authors === 1
                        ? 'email'
                        : 'emails'}, active on {row.commits.value.activeDays} of the last {row.commits
                        .value.windowDays} days"
                      >{row.commits.value.recent}/7d</span
                    >
                  {/if}
                </FleetCell>
              </td>
              {/if}

              {#if !hiddenColumns.has("activity")}
              <td class="{cellPad} align-top">
                <FleetCell cell={row.activity} label="Last commit">
                  {#if row.activity.kind === "read"}
                    <span class="inline-flex items-center gap-1 text-textMuted">
                      <Clock size={10} class="shrink-0" />
                      {row.activity.value > 0 ? formatAge(row.activity.value) : "no commits"}
                    </span>
                  {/if}
                </FleetCell>
              </td>
              {/if}

              <!-- Lines: the total, and the mix behind it when one is on file. -->
              {#if !hiddenColumns.has("loc")}
              <td class="{cellPad} align-top">
                <FleetCell
                  cell={row.loc}
                  label="Lines of code"
                  partialNote="The language scan stopped at a budget, so this total is a floor."
                  onScan={scanCell(row, "loc")}
                  scanning={running.has(row.path) && $fleetStore.scanning === "loc"}
                  deltaUnit="count"
                  deltaGoal="neutral"
                >
                  {#if row.loc.kind === "read"}
                    <span class="inline-flex flex-col items-end gap-0.5 min-w-20">
                      <span class="inline-flex items-baseline gap-1">
                        <span class="text-textPrimary">{row.loc.value.lines.toLocaleString()}</span>
                        {#if row.loc.value.language}
                          <span class="text-[10px] text-textMuted">{row.loc.value.language}</span>
                        {/if}
                      </span>
                      {#if row.loc.value.languages.length > 0}
                        <FleetLanguageBar
                          stats={foldFleetLanguages(row.loc.value.languages, 5)}
                          height={4}
                          label={`${row.label} languages`}
                        />
                      {/if}
                    </span>
                  {/if}
                </FleetCell>
              </td>
              {/if}

              {#if !hiddenColumns.has("storage")}
              <td class="{cellPad} align-top">
                <FleetCell
                  cell={row.storage}
                  label="Storage"
                  partialNote="The disk walk stopped at a budget, so these bytes are a floor."
                  onScan={scanCell(row, "storage")}
                  scanning={running.has(row.path) && $fleetStore.scanning === "storage"}
                  deltaUnit="bytes"
                  deltaGoal="lower"
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
              {/if}

              {#if !hiddenColumns.has("health")}
              <td class="{cellPad} align-top">
                <FleetCell
                  cell={row.health}
                  label="Vulnerabilities"
                  partialNote="Some audit targets never ran, so this count is a floor — not a clean bill of health."
                  onScan={scanCell(row, "health")}
                  scanning={running.has(row.path) && $fleetStore.scanning === "health"}
                  deltaUnit="count"
                  deltaGoal="lower"
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
              {/if}

              {#if !hiddenColumns.has("coverage")}
              <td class="{cellPad} align-top">
                <FleetCell
                  cell={row.coverage}
                  label="Coverage"
                  partialNote="The coverage scan was cut short, so this percentage covers part of the repository."
                  onScan={scanCell(row, "coverage")}
                  scanning={running.has(row.path) && $fleetStore.scanning === "coverage"}
                  deltaUnit="percent"
                  deltaGoal="higher"
                >
                  {#if row.coverage.kind === "read"}
                    <span class="text-textPrimary">{row.coverage.value.toFixed(1)}%</span>
                  {/if}
                </FleetCell>
              </td>
              {/if}

              <td class="{cellPad} align-top text-right">
                <button
                  type="button"
                  class="p-1 rounded text-textMuted hover:text-rose-400 hover:bg-surfaceHover transition-colors focus:outline-hidden focus-visible:ring-2 focus-visible:ring-accent/60"
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
        {/snippet}

        <!-- Banded, the grid reads top-to-bottom as a queue: worst first, each
             band headed by what it is and how many, with Clean collapsed. Under
             any other sort the bands would be boundaries that mean nothing, so
             the table falls back to a flat list. -->
        {#if banded}
          {#each bands as band (band.severity)}
            <!-- The effective set, not the raw one: a header that read
                 `collapsedBands` directly would draw a collapsed chevron over
                 rows the grid is showing anyway. -->
            {@const bandCollapsed = collapsed.has(band.severity)}
            <tbody data-testid="fleet-band" data-severity={band.severity}>
              <tr>
                <th
                  colspan={shownColumns.length + 1}
                  scope="colgroup"
                  class="text-left px-3 py-1.5 bg-surface/60 border-t border-border/60"
                >
                  <button
                    type="button"
                    class="inline-flex items-center gap-1.5 text-[10px] uppercase tracking-wider font-semibold text-textMuted hover:text-textPrimary transition-colors focus:outline-hidden focus-visible:ring-2 focus-visible:ring-accent/60 rounded"
                    aria-expanded={!bandCollapsed}
                    onclick={() => toggleBand(band.severity)}
                    data-testid="fleet-band-toggle"
                  >
                    {#if bandCollapsed}<ChevronRight size={11} />{:else}<ChevronDown size={11} />{/if}
                    <span class="h-2 w-1 rounded-full {SEVERITY_STRIPE[band.severity]}"></span>
                    <span>{band.label}</span>
                    <span class="tabular-nums text-textMuted/70">{band.rows.length}</span>
                  </button>
                </th>
              </tr>
              {#if !bandCollapsed}
                {#each band.rows as row (row.path)}
                  {@render repoRow(row, rowIndex(row.path))}
                {/each}
              {/if}
            </tbody>
          {/each}
        {:else}
          <tbody>
            {#each visibleRows as row, index (row.path)}
              {@render repoRow(row, index)}
            {/each}
          </tbody>
        {/if}
      </table>

      {#if $fleetStore.snapshot?.truncated}
        <p class="px-4 py-2 text-[10px] text-amber-600 dark:text-amber-400" role="status">
          The sweep did not reach every repository, so some rows show worktrees, commits and
          activity as not scanned. Refresh to try the rest.
        </p>
      {/if}
    {/if}
  </div>
</div>
