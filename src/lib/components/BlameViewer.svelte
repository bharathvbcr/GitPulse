<script lang="ts">
  import { untrack } from "svelte";
  import { hostPlatform } from "../stores/platformStore";
  import { shortcutTextLabel } from "../ui/platformCopy";
  import type { BlameLine } from "../files/types";
  import { densityStore } from "../stores/densityStore";
  import { rowHeight } from "../ui/density";
  import { repoStore } from "../stores/repoStore";
  import { invoke } from "@tauri-apps/api/core";
  import { FileCode, PanelLeftClose, PanelLeftOpen, Search, X } from "@lucide/svelte";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { coverageHitClass } from "../coverage/format";
  import { buildHitMap, fetchFileCoverage, hitBadgeClass } from "../coverage/fileCoverage";
  import { plural, shortHash } from "../format";
  import { timestampFormat } from "../ui/timestampFormat";
  import {
    bandColor,
    blameRowTint,
    buildAgeTicks,
    buildBlameTimeline,
    columnHeight,
    describePeriod,
    formatShare,
    granularityLabel,
    isUncommittedLine,
    sameSelection,
    selectionLabel,
    selectionMatches,
    toneColor,
    type BandShare,
    type BlamePeriod,
    type BlameSelection,
  } from "../metrics/blameTimeline";
  // The rail is a map of a scrolling list, which the diff minimap already
  // solved. Same geometry, same centring rule, no second copy.
  import { ratioFromPointer, scrollForRatio, viewportBand } from "../diff/minimap";
  import { reportPanelError } from "../diagnostics/report";
  import { paneDetails } from "../diagnostics/paneCrash";
  import VirtualList from "./VirtualList.svelte";
  import EmptyState from "./EmptyState.svelte";
  import FileTreePanel from "./files/FileTreePanel.svelte";

  /** Tallest timeline column, in px. The strip is chrome, not a chart pane. */
  const AXIS_HEIGHT = 30;

  let filePath = $state("");
  let blameLines: BlameLine[] = $state([]);
  /**
   * The instant this file's blame is measured against.
   *
   * Captured once per load rather than read as `Date.now()` inside the derived
   * timeline: the axis, the row tints and the bucket filter all have to agree
   * on when "now" is, and a clock read per recomputation lets a line drift
   * across a band boundary between the column that counted it and the filter
   * that selects it.
   */
  let blameNow = $state(0);
  /** The one period, off-axis group or age band the gutter is filtered to. */
  let selection = $state<BlameSelection | null>(null);
  /** Two-way with the list, so the rail can drive it and follow it. */
  let listScroll = $state(0);
  /** The rail's height IS the list's viewport height: they are siblings. */
  let railHeight = $state(0);
  /** Commit under the pointer, so a commit's lines highlight as one block. */
  let hoverCommit = $state<string | null>(null);
  let coverageHits = $state<Map<number, number>>(new Map());
  // Distinguishes "file has no coverage data" (dim dots) from "the coverage
  // lookup itself failed" — a silent catch would conflate the two.
  let coverageFailed = $state(false);
  let isLoading = $state(false);
  let errorMsg = $state<string | null>(null);
  let explorerOpen = $state(true);
  let inflight: AsyncGuard | null = null;
  const contentRevisions = repoStore.contentRevisions;
  let running: { repo: string; path: string } | null = null;
  let queued: { repo: string; path: string } | null = null;
  let disposed = false;
  const detailScope = paneDetails.register("blame");
  let requestCount = 0;

  async function loadBlameFor(repo: string, path: string) {
    if (!repo || !path) return;
    if (running) {
      // One IPC pair at a time, plus the latest requested refresh. Content
      // storms cannot cancel/restart the same slow request indefinitely.
      if (running.repo !== repo || running.path !== path) inflight?.cancel();
      queued = { repo, path };
      isLoading = true;
      errorMsg = null;
      coverageFailed = false;
      return;
    }
    running = { repo, path };
    detailScope.update(repo, "blame", { blame: ++requestCount });
    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    isLoading = true;
    errorMsg = null;
    // The previous file's verdict must not bleed into the loading frame:
    // old lines linger until the guard applies, so clear eagerly.
    coverageFailed = false;
    try {
      const [next, coverage] = await Promise.all([
        invoke<BlameLine[]>("cmd_get_file_blame", {
          repoPath: repo,
          filePath: path,
        }),
        fetchFileCoverage(repo, path)
          .then((res) => ({ ok: true as const, hits: buildHitMap(res.lines) }))
          .catch(() => ({ ok: false as const, hits: new Map<number, number>() })),
      ]);
      if (!guard.isLive()) return;
      blameLines = next;
      blameNow = Date.now();
      // A period selected in the file being replaced says nothing about the
      // one arriving; carrying it over would open the new file pre-filtered.
      selection = null;
      listScroll = 0;
      hoverCommit = null;
      coverageHits = coverage.hits;
      coverageFailed = !coverage.ok;
    } catch (err: unknown) {
      if (!guard.isLive()) return;
      errorMsg = reportPanelError("blame", err);
      blameLines = [];
      selection = null;
      listScroll = 0;
      hoverCommit = null;
      coverageHits = new Map();
      coverageFailed = false;
    } finally {
      if (guard.isLive()) isLoading = false;
      running = null;
      const next = queued;
      queued = null;
      if (next && !disposed) void loadBlameFor(next.repo, next.path);
    }
  }

  /**
   * Reload the selected file after a failure.
   *
   * Retry used to mean re-typing the path into Blame's own box and pressing
   * Enter — the box was the only way back, and it existed only because Blame
   * was a destination you could arrive at with nothing selected. Code's
   * Explorer section owns picking a file now, so what is left here is the one
   * thing a selection cannot express: asking for the same file again. The
   * store does not notify for an unchanged value, so this calls the loader
   * directly.
   */
  function retryBlame() {
    const repo = $repoStore.currentPath;
    const path = $repoStore.selectedFilePath;
    if (!repo || !path) return;
    void loadBlameFor(repo, path);
  }

  $effect(() => {
    return () => { disposed = true; queued = null; inflight?.cancel(); detailScope.dispose(); };
  });

  // Selection- and freshness-driven blame load, memoized on a fingerprint of
  // its real dependencies. Status-poll emissions only re-evaluate the derived
  // fingerprint unless something that can change blame output moved: the
  // selection, the file's worktree/index status, or the checked-out branch's
  // tip (external commits land through watcher refreshes).
  const blameFingerprint = $derived.by(() => {
    const selected = $repoStore.selectedFilePath;
    const repo = $repoStore.currentPath;
    const statusCode = selected
      ? ($repoStore.statuses.find((s) => s.path === selected)?.status_code ?? "")
      : "";
    const tip =
      $repoStore.branches.find((b) => b.is_current)?.tip_commit_id ?? "";
    const revision = repo ? ($contentRevisions[repo] ?? "") : "";
    return `${repo ?? ""}\u0000${selected ?? ""}\u0000${statusCode}\u0000${tip}\u0000${revision}`;
  });

  let prevKey: string | null = null;
  $effect(() => {
    // Only a changed fingerprint invalidates this effect. Reading the whole
    // store here used to cancel the live guard on every poll, then skip its
    // replacement because prevKey had not changed: Loading… forever.
    const key = blameFingerprint;
    return untrack(() => {
      if (key === prevKey) return;
      prevKey = key;
      const selected = $repoStore.selectedFilePath;
      const repo = $repoStore.currentPath;
      filePath = selected ?? "";
      if (!repo || !selected) {
        queued = null;
        inflight?.cancel();
        blameLines = [];
        selection = null;
        listScroll = 0;
        hoverCommit = null;
        coverageHits = new Map();
        coverageFailed = false;
        errorMsg = null;
        isLoading = false;
        return;
      }
      void loadBlameFor(repo, selected);
    });
  });

  /**
   * The file's age distribution: one owner for the axis, the row tints and
   * the bucket filter, so a column cannot claim a share it would not select.
   */
  const timeline = $derived(buildBlameTimeline(blameLines, blameNow));

  /**
   * The selection's label, resolved against the timeline that is drawn.
   *
   * Null when the selection names something this file does not have, and then
   * the whole file shows. Filtering to nothing would read as "this file is
   * empty", which is the one thing a stale filter must not say.
   */
  const activeLabel = $derived(selectionLabel(selection, timeline));

  const visibleLines = $derived.by(() => {
    // Bound to a const before the closure reads it: a narrowed `let` does not
    // stay narrowed across one.
    const active = selection;
    if (active === null || activeLabel === null) return blameLines;
    return blameLines.filter((line) => selectionMatches(line, active, timeline));
  });

  const ROW = $derived(rowHeight("blame", $densityStore));

  /**
   * The rail maps the list ACTUALLY drawn, not the whole file: under a filter
   * the two are different lists, and a map of the other one points every mark
   * at a row that is not there. This is the diff minimap's lesson, restated
   * because the filter makes it easy to get wrong here too.
   */
  const ageTicks = $derived(buildAgeTicks(visibleLines, timeline.nowMs));
  const scrollBand = $derived(viewportBand(listScroll, railHeight, visibleLines.length, ROW));

  const summary = $derived.by(() => {
    const parts = [plural(timeline.totalLines, "line")];
    if (timeline.commits > 0) parts.push(plural(timeline.commits, "commit"));
    if (timeline.authors > 0) parts.push(plural(timeline.authors, "author"));
    if (timeline.medianAgeDays !== null) parts.push(`median age ${timeline.medianAgeDays}d`);
    return parts.join(" · ");
  });

  const axisStartLabel = $derived.by(() => {
    const first = timeline.periods[0];
    if (!first) return "";
    return first.foldedOlder ? `${first.label} or earlier` : first.label;
  });

  /** Click the active one again to clear it — one filter at a time. */
  function toggle(next: BlameSelection) {
    selection = sameSelection(selection, next) ? null : next;
    listScroll = 0;
  }

  function selectPeriod(period: BlamePeriod) {
    // An empty period has nothing to show; selecting it would blank the pane.
    if (period.lines === 0) return;
    toggle({ kind: "bucket", key: period.key });
  }

  function selectBand(share: BandShare) {
    if (share.lines === 0) return;
    toggle({ kind: "band", id: share.band.id });
  }

  const isSelected = $derived((next: BlameSelection) => sameSelection(selection, next));

  /**
   * Scroll the list to where the pointer landed on the rail.
   *
   * `scrollForRatio` centres the target rather than top-aligning it, which is
   * the difference between a map and a scrollbar: aiming at the last mark has
   * to show the last mark, and a top-aligned jump clamps it off screen.
   */
  function onRailPointer(event: PointerEvent) {
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
    const ratio = ratioFromPointer(event.clientY, rect.top, rect.height);
    listScroll = scrollForRatio(ratio, visibleLines.length, ROW, railHeight);
  }

  function handleWindowKeydown(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "b" && !e.shiftKey) {
      e.preventDefault();
      explorerOpen = !explorerOpen;
    }
  }
</script>

<svelte:window onkeydown={handleWindowKeydown} />

<div class="flex-1 flex flex-col bg-background h-full text-xs font-mono select-none overflow-hidden">
  <!-- Toolbar -->
  <div class="px-4 py-2 border-b border-border/60 gp-section-edge bg-surface/60 flex items-center justify-between gap-3 font-sans shrink-0">
    <div class="flex items-center gap-3 min-w-0">
      <button
        type="button"
        onclick={() => (explorerOpen = !explorerOpen)}
        title="{explorerOpen ? 'Hide' : 'Show'} Explorer ({shortcutTextLabel('⌘B', $hostPlatform.os)})"
        class="p-1 rounded-full text-textMuted hover:text-accent hover:bg-surfaceHover transition-colors"
      >
        {#if explorerOpen}
          <PanelLeftClose size={15} />
        {:else}
          <PanelLeftOpen size={15} />
        {/if}
      </button>
      <FileCode size={16} class="text-accent" />
      <!-- The file, named rather than typed. Blame reads the selection Code's
           Explorer section sets, so a second path box here would be a second
           way to say the same thing — and the one that could disagree. -->
      <span data-blame-path class="truncate font-mono text-xs text-textPrimary" title={filePath || undefined}>
        {filePath || "No file selected"}
      </span>
    </div>

    <!-- min-w-0, not shrink-0: the summary is the longest string in this row,
         and a rigid one would push the path it sits beside down to nothing on
         a narrow pane. Both sides give, and both truncate. -->
    <div class="flex items-center gap-3 text-[11px] text-textMuted min-w-0">
      {#if blameLines.length > 0}
        <span class="tabular-nums truncate" title="Across the whole file, not just the selected period">
          {summary}
        </span>
      {/if}
      {#if coverageFailed && blameLines.length > 0}
        <span
          class="flex items-center gap-1 text-amber-400/80"
          title="Coverage data could not be loaded; hit badges are unavailable."
        >
          <span class="w-2 h-2 rounded-full bg-amber-400/70"></span> Coverage unavailable
        </span>
      {/if}
    </div>
  </div>

  <!--
    Code age timeline.

    Blame answers "who wrote this line". The strip answers the question the
    gutter could only be scrolled for: how much of this file is actually
    recent. Every column is a share of the WHOLE file, and the lines a time
    axis cannot hold — worktree-only, clock-skewed, undated — ride beside it as
    named chips rather than being quietly left out of the denominator.
  -->
  {#if blameLines.length > 0 && !isLoading && !errorMsg}
    <div class="px-4 py-2 border-b border-border/60 bg-surface/30 font-sans shrink-0 flex flex-col gap-1.5">
      <div class="flex items-end gap-3">
        {#if timeline.periods.length > 0}
          <div class="flex-1 min-w-0 flex flex-col gap-1">
            <!--
              Tiles, not floating bars. Each period owns a full-height cell with
              a faint back, so the axis reads as one continuous strip and an
              empty month is visibly an empty month rather than a gap in the
              chrome. The fill inside still rises to the period's share, so the
              quantity is read off a height and not off an opacity.
            -->
            <div
              class="flex items-stretch gap-px rounded-[3px] overflow-hidden ring-1 ring-border/50"
              style:height={`${AXIS_HEIGHT}px`}
              role="group"
              aria-label="Code age timeline: share of the file by when each line last changed"
            >
              {#each timeline.periods as period (period.key)}
                <button
                  type="button"
                  class="flex-1 min-w-[2px] flex items-end transition-colors bg-background/60
                         focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-accent
                         {isSelected({ kind: 'bucket', key: period.key })
                           ? 'ring-1 ring-inset ring-accent bg-accent/10'
                           : period.lines > 0
                             ? 'hover:bg-surfaceHover'
                             : ''}"
                  aria-pressed={isSelected({ kind: "bucket", key: period.key })}
                  aria-disabled={period.lines === 0}
                  aria-label={describePeriod(period)}
                  title={describePeriod(period)}
                  onclick={() => selectPeriod(period)}
                >
                  {#if period.lines > 0}
                    <span
                      class="block w-full"
                      style:height={`${columnHeight(period.percent, timeline.peakPercent, AXIS_HEIGHT)}px`}
                      style:background-color={bandColor(period.band, "fill")}
                    ></span>
                  {:else}
                    <span class="block w-full h-px bg-border/60"></span>
                  {/if}
                </button>
              {/each}
            </div>
            <div class="flex items-center justify-between text-[10px] text-textMuted/80 tabular-nums">
              <span class="truncate">{axisStartLabel}</span>
              <span class="truncate shrink-0 px-2">
                {formatShare(timeline.peakPercent)} peak · {granularityLabel(timeline.granularity)}
              </span>
              <span class="truncate text-right">{timeline.periods[timeline.periods.length - 1].label}</span>
            </div>
          </div>
        {:else}
          <p class="flex-1 min-w-0 text-[11px] text-textMuted leading-tight">
            No dated lines to place on a timeline.
          </p>
        {/if}

        <!--
          The legend, carrying the distribution and selecting on it.

          It was four static swatches restating a colour scale already visible
          in the rows. Each one now states its band's share of the file and
          filters to it, which is how "what in here has not been touched in
          three months" becomes a glance and a click. Both these and the chips
          come from the one age scale, and both partition the same file.
        -->
        <div class="shrink-0 flex flex-col items-end gap-1.5 text-[10px] text-textMuted">
          <div class="flex items-center gap-1" role="group" aria-label="Filter by code age">
            {#each timeline.bands as share (share.band.id)}
              <button
                type="button"
                aria-pressed={isSelected({ kind: "band", id: share.band.id })}
                aria-disabled={share.lines === 0}
                aria-label="{share.band.label}: {formatShare(share.percent)} of the file, {plural(share.lines, 'line')}"
                title="{formatShare(share.percent)} of the file is {share.band.label}{share.lines > 0 ? ' — click to show only those lines' : ''}"
                onclick={() => selectBand(share)}
                class="flex items-center gap-1 px-1.5 py-0.5 rounded-md border transition-colors tabular-nums
                       {isSelected({ kind: 'band', id: share.band.id })
                         ? 'border-accent/60 bg-accent/15 text-textPrimary'
                         : share.lines > 0
                           ? 'border-transparent hover:bg-surfaceHover'
                           : 'border-transparent opacity-45'}"
              >
                <span class="w-2.5 h-2.5 rounded-full shrink-0" style:background-color={bandColor(share.band, "fill")}></span>
                <span>{share.band.label}</span>
                <span class="text-textMuted/70">{formatShare(share.percent)}</span>
              </button>
            {/each}
          </div>
          {#if timeline.extras.length > 0}
            <div class="flex items-center gap-1.5">
              {#each timeline.extras as extra (extra.key)}
                <button
                  type="button"
                  aria-pressed={isSelected({ kind: "bucket", key: extra.key })}
                  title={extra.explanation}
                  onclick={() => toggle({ kind: "bucket", key: extra.key })}
                  class="px-1.5 py-0.5 rounded-full border transition-colors tabular-nums
                         {isSelected({ kind: 'bucket', key: extra.key })
                           ? 'border-accent/60 bg-accent/15 text-textPrimary'
                           : 'border-border/60 hover:bg-surfaceHover'}"
                >
                  {extra.label} {formatShare(extra.percent)}
                </button>
              {/each}
            </div>
          {/if}
        </div>
      </div>

      {#if activeLabel !== null}
        <div class="flex items-center gap-2 text-[11px] text-textMuted">
          <span class="tabular-nums">
            Showing {plural(visibleLines.length, "line")} of {timeline.totalLines} — {activeLabel}
          </span>
          <button
            type="button"
            onclick={() => { selection = null; listScroll = 0; }}
            class="flex items-center gap-1 px-1.5 py-0.5 rounded-md text-textMuted hover:text-textPrimary hover:bg-surfaceHover transition-colors"
          >
            <X size={11} /> Clear
          </button>
        </div>
      {/if}
    </div>
  {/if}

  <!-- Body -->
  <div class="flex-1 min-h-0 flex">
    {#if explorerOpen}
      <div class="w-72 shrink-0 h-full overflow-hidden">
        <FileTreePanel />
      </div>
    {/if}

    <!-- Blame Lines -->
    <div class="flex-1 min-w-0 flex flex-col">
      {#if isLoading}
        <div class="h-full flex items-center justify-center text-textMuted font-sans text-xs">
          Loading blame for {filePath}...
        </div>
      {:else if errorMsg}
        <div class="h-full flex flex-col items-center justify-center gap-3 text-rose-400 font-sans text-xs p-4 text-center">
          <span class="max-w-md">{errorMsg}</span>
          <button type="button" onclick={retryBlame} class="gp-btn py-1! px-3! text-[11px]">
            Retry
          </button>
        </div>
      {:else if blameLines.length > 0}
      <div class="flex-1 min-h-0 flex">
        <!-- contentWidth: blame rows are source, and a long line used to be
             clipped at the pane edge with no way to reach the rest of it. One
             shared horizontal scrollbar, with every row's age tint running the
             full width of it.

             No vertical padding: the rail beside it measures its own height and
             calls it the list's viewport, and padding inside the scroller would
             make the two disagree by exactly that much. -->
        <VirtualList
          items={visibleLines}
          contentWidth
          bind:scrollTop={listScroll}
          rowHeight={ROW}
          overscan={15}
          class="flex-1 min-w-0 h-full px-1.5"
        >
          {#snippet row(line, index)}
            {#if line}
              {@const uncommitted = isUncommittedLine(line)}
              <!-- Consecutive lines from one commit are one block: the hash and
                   author are stated once at its head and the rest of the block
                   stays quiet, which is what lets the eye see the block at all.
                   The hash stays reachable on every line — hover or focus — so
                   grouping never costs a reader the way back to the commit. -->
              {@const grouped = index > 0 && visibleLines[index - 1]?.commit_id === line.commit_id}
              {@const inHoveredCommit = !uncommitted && hoverCommit === line.commit_id}
              <!-- svelte-ignore a11y_no_static_element_interactions -->
              <!-- Justified: the pointer handlers activate nothing. They tint
                   the rest of the hovered commit's lines, which a reader who
                   is not pointing at a row cannot be missing. Every action in
                   the row is on a real button, and the same grouping is
                   already stated without a pointer — a continuation line
                   leaves its hash and author cells empty.

                   The mark is a bar down the left edge rather than a ring per
                   row: a commit is one block, and fifteen boxes do not read as
                   one. A background class would be dead here anyway — the age
                   tint is inline and would win. -->
              <div
                data-blame-line={line.line_no}
                data-blame-grouped={grouped ? "true" : undefined}
                data-blame-block={inHoveredCommit ? "true" : undefined}
                class="flex items-center w-full rounded-r-md pr-1 border-l-2 {inHoveredCommit
                  ? 'border-accent/70'
                  : 'border-transparent'}"
                style:height={`${ROW}px`}
                style:background-color={blameRowTint(line, timeline.nowMs)}
                onmouseenter={() => (hoverCommit = line.commit_id)}
                onmouseleave={() => (hoverCommit = null)}
              >
                {#if uncommitted}
                  <span
                    class="w-16 px-2 text-[10px] font-mono text-textMuted/50 italic text-left shrink-0 truncate"
                    title="Not committed yet"
                  >{grouped ? "" : "uncommitted"}</span>
                {:else}
                  <button
                    type="button"
                    class="w-16 px-2 text-[10px] text-accent/80 font-mono select-none cursor-pointer hover:underline text-left shrink-0 transition-opacity
                           {grouped ? 'opacity-0 hover:opacity-100 focus-visible:opacity-100' : ''}"
                    title="{shortHash(line.commit_id)} — {line.author_name}, {$timestampFormat.title(line.timestamp)}"
                    onclick={() => {
                      repoStore.inspectCommitInHistory(line.commit_id);
                    }}
                  >{shortHash(line.commit_id)}</button>
                {/if}
                <span class="w-24 px-2 text-[10px] text-textMuted truncate font-sans shrink-0">{grouped ? "" : line.author_name}</span>
                <span
                  class="w-[74px] px-1.5 text-[10px] text-textMuted/70 truncate font-sans shrink-0 tabular-nums"
                  title={$timestampFormat.title(line.timestamp) || undefined}
                >{grouped ? "" : $timestampFormat.text(line.timestamp)}</span>
                <span class="w-8 px-2 text-right text-textMuted/40 text-[10px] select-none shrink-0">{line.line_no}</span>
                <span class={hitBadgeClass(coverageHits.get(line.line_no))}>{coverageHits.get(line.line_no) ?? "·"}</span>
                <span class="gp-diff-text px-3 whitespace-pre text-textPrimary {coverageHitClass(coverageHits.get(line.line_no))}">{line.content}</span>
              </div>
            {/if}
          {/snippet}
        </VirtualList>

        <!--
          The age rail: the whole file's age, as one heat strip.

          The timeline says how much of this file is recent; only the rail says
          WHERE. A virtual list has a screenful rendered at any moment, so no
          amount of looking at the gutter could answer that — you had to
          scroll, and lose your place. Every line of the list being drawn is
          projected here, freshest tone winning each bucket and the intensity
          saying how much of the bucket that tone really is.
        -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <!-- Justified: pointer-only by design and marked presentation so
             assistive tech skips it, exactly as the diff map is. It activates
             nothing a keyboard cannot already do — the list scrolls, and every
             region it points at is reachable from the timeline's own buttons,
             which are focusable and say what they hold. -->
        <div
          data-blame-rail
          bind:clientHeight={railHeight}
          class="group/rail relative h-full w-3 shrink-0 cursor-pointer select-none overflow-hidden
                 border-l border-border/70 bg-surface/90 transition-[width] duration-150 hover:w-4"
          title="Code age map — click or drag to navigate"
          role="presentation"
          onpointerdown={(e) => {
            (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
            onRailPointer(e);
          }}
          onpointermove={(e) => {
            if (e.buttons === 1) onRailPointer(e);
          }}
        >
          {#if scrollBand}
            <div
              data-blame-rail-band
              class="pointer-events-none absolute inset-x-0 rounded-sm border border-accent/40 bg-accent/10"
              style:top={`${scrollBand.topPct}%`}
              style:height={`${scrollBand.heightPct}%`}
            ></div>
          {/if}
          <!-- Square marks, unlike the diff map's sparse rounded ticks: these
               tile the whole rail, and rounding every 6px mark notches a run
               of forty same-age lines into a dotted line. -->
          {#each ageTicks as tick (tick.key)}
            <div
              data-blame-tick={tick.tone}
              class="pointer-events-none absolute left-0.5 right-0.5"
              style:top={`${tick.topPct}%`}
              style:height={`${tick.heightPct}%`}
              style:background-color={toneColor(tick.tone, tick.weight)}
            ></div>
          {/each}
        </div>
      </div>
      {:else}
        <EmptyState
          icon={Search}
          title={filePath ? "Empty file" : "No blame loaded"}
          hint={filePath ? "This file has no lines to annotate." : explorerOpen
            ? "Pick a file in the explorer to see line authorship and code age."
            : `Open the explorer (${shortcutTextLabel("⌘B", $hostPlatform.os)}), or pick a file in Code → Explorer.`}
        />
      {/if}
    </div>
  </div>
</div>
