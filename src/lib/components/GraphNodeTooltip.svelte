<script lang="ts">
  import {
    CalendarClock,
    Cloud,
    Compass,
    GitBranch,
    GitCommit,
    GitMerge,
    Tag,
  } from "lucide-svelte";
  import type { GraphHitKind, VisualCommitRow } from "../canvas/GraphRenderer";
  import type { TooltipPlacement } from "../canvas/graphInteraction";
  import type { RefItem } from "./CommitRow.svelte";
  import { authorColor, authorIdentity } from "../authors/authorIdentity";
  import { getBranchColor } from "../canvas/Palette";
  import { formatRelativeTime } from "../format";

  let {
    row,
    refs = [],
    placement = "below",
    caretX = 16,
    hitKind = "node",
    mergeTarget = null,
    authorCommitCount = null,
    mainlineName = null,
    hasMore = false,
  }: {
    row: VisualCommitRow;
    refs?: RefItem[];
    placement?: TooltipPlacement;
    /** Pointer X inside the box; the caret tracks it after clamping. */
    caretX?: number;
    /** What the probe landed on; drives kind-specific chips and fallbacks. */
    hitKind?: GraphHitKind;
    /** The merge point this commit's closing line lands on, when known. */
    mergeTarget?: VisualCommitRow | null;
    /** This author's commit count in the loaded history, when known. */
    authorCommitCount?: number | null;
    /**
     * The branch the straight column-0 rail is anchored on (`main`,
     * `origin/main`, the HEAD branch), when the payload named one. Rows on
     * that rail get a chip so the leftmost line is never an unlabeled
     * colour: the chip says which branch's first-parent history it is.
     */
    mainlineName?: string | null;
    /**
     * Whether older history exists to load. A stub on this row fades because
     * its parent lies past the loaded window — the only reason a payload
     * ever carries one, since filters run in the backend and relink
     * survivors — and a fading line with no explanation reads as a broken
     * branch, so the strip names the parent and, when true, how to reach it.
     */
    hasMore?: boolean;
  } = $props();

  /** Parents this row's dangling connections point at, by the k ↔ parent_ids[k] contract. */
  const missingParents = $derived(
    (row.connections ?? []).flatMap((conn, k) => {
      if (!conn?.is_dangling) return [];
      const id = row.parent_ids?.[k];
      return typeof id === "string" && id.length > 0 ? [id] : [];
    }),
  );

  /** Chip text for a row on the pinned mainline. */
  const mainlineLabel = $derived(
    row.is_mainline === true ? `${mainlineName?.trim() || "mainline"} · first-parent line` : null,
  );

  // The caller clamps the anchor inside its measured box; here it only needs
  // a finite floor so the rotated caret never renders off the left edge.
  const safeCaretX = $derived(Number.isFinite(caretX) ? Math.max(8, caretX) : 16);

  const visibleRefs = $derived(refs.slice(0, 4));
  const hiddenRefCount = $derived(Math.max(0, refs.length - visibleRefs.length));

  const identity = $derived(authorIdentity(row.author_name, row.author_email));

  function formatTimestamp(timestamp: number): string {
    const date = new Date(timestamp * 1000);
    if (!Number.isFinite(timestamp) || Number.isNaN(date.getTime())) return "Unknown time";
    return new Intl.DateTimeFormat(undefined, {
      dateStyle: "medium",
      timeStyle: "short",
    }).format(date);
  }
</script>

<div
  role="tooltip"
  class="relative rounded-2xl border border-border/70 bg-surface text-textPrimary shadow-pop"
>
  <div
    class="absolute h-3 w-3 rotate-45 rounded-[2px] border-border bg-surface {placement === 'above'
      ? '-bottom-1.5 border-b border-r'
      : '-top-1.5 border-l border-t'}"
    style="left: {safeCaretX - 6}px;"
    aria-hidden="true"
  ></div>

  <div class="flex items-start gap-2.5 border-b border-border/50 px-3 py-2.5">
    <div class="mt-0.5 rounded-full bg-accent/15 p-1.5 text-accent shadow-sm">
      {#if row.is_merge}
        <GitMerge size={14} />
      {:else}
        <GitCommit size={14} />
      {/if}
    </div>
    <div class="min-w-0 flex-1">
      <div class="text-xs font-semibold leading-4">{row.summary || "No commit message"}</div>
      <!-- Chips wrap under the id instead of sharing its line: as shrink-0
           siblings of a break-all span they could squeeze the 40-char id to
           a few pixels and wrap it one character per line. -->
      <div class="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 font-mono text-[10px] text-textMuted">
        <span class="select-all break-all">{row.id}</span>
        <span class="shrink-0 rounded-full bg-background px-1.5 py-0.5 text-textPrimary/80">
          {row.is_merge ? "Merge commit" : row.is_root ? "Root commit" : "Commit"}
        </span>
        {#if hitKind === "lane"}
          <span class="shrink-0 rounded-full bg-background px-1.5 py-0.5 text-textMuted">
            Branch line
          </span>
        {/if}
        {#if mainlineLabel}
          <!-- The straight column-0 rail, labelled by the branch it belongs
               to: a leftmost line with no name is just a colour. -->
          <span
            class="shrink-0 rounded-full border px-1.5 py-0.5 font-semibold"
            style="border-color: {getBranchColor(row.color_index)}; color: {getBranchColor(row.color_index)};"
            data-testid="mainline-chip"
          >
            {mainlineLabel}
          </span>
        {/if}
      </div>
    </div>
  </div>

  {#if mergeTarget || hitKind === "connector"}
    <!-- Where this branch's closing line is going. Shown whenever the
         target is known — pointer hover on the descent or the node, and
         keyboard focus alike: context must not be gated behind a pointer. -->
    <div class="flex items-center gap-2 border-b border-border/40 px-3 py-2 text-[10px]">
      <GitMerge size={11} class="shrink-0 text-accent" />
      {#if mergeTarget}
        <span class="min-w-0 truncate text-textPrimary">
          Merges into
          <span class="font-mono text-textMuted">{mergeTarget.id.slice(0, 7)}</span>
          · {mergeTarget.summary || "No commit message"}
        </span>
      {:else}
        <span class="text-textPrimary">Merges into another branch below</span>
      {/if}
    </div>
  {/if}

  {#if missingParents.length > 0}
    <!-- The stub's meaning, in words: which parent is missing and why. -->
    <div class="flex items-center gap-2 border-b border-border/40 px-3 py-2 text-[10px]" data-testid="dangling-strip">
      <GitCommit size={11} class="shrink-0 text-textMuted" />
      <span class="min-w-0 truncate text-textPrimary">
        Parent <span class="font-mono text-textMuted">{missingParents[0].slice(0, 7)}</span>
        {missingParents.length > 1 ? `and ${missingParents.length - 1} more are` : "is"} outside the loaded history{hasMore ? " · load older history to follow it" : ""}
      </span>
    </div>
  {/if}

  {#if authorCommitCount !== null && Number.isFinite(authorCommitCount) && authorCommitCount > 0}
    <div class="flex items-center gap-2 border-b border-border/40 px-3 py-2 text-[10px]">
      <span
        class="inline-flex h-4 w-4 shrink-0 items-center justify-center rounded-full text-[7px] font-bold text-white ring-1 ring-background"
        style="background-color: {authorColor(identity.hue)}"
        aria-hidden="true"
      >{identity.initials}</span>
      <span class="min-w-0 truncate text-textPrimary">
        {authorCommitCount}
        {authorCommitCount === 1 ? "commit" : "commits"} by {row.author_name || "Unknown"} in the
        loaded history
      </span>
    </div>
  {/if}

  {#if refs.length > 0}
    <div class="flex flex-wrap gap-1 border-b border-border/40 px-3 py-2">
      {#each visibleRefs as ref}
        <span class="inline-flex max-w-40 items-center gap-1 rounded-full border border-border bg-background px-1.5 py-0.5 font-mono text-[9px] text-textPrimary">
          {#if ref.kind === "tag"}
            <Tag size={9} class="shrink-0 text-amber-400" />
          {:else if ref.kind === "remote-branch"}
            <Cloud size={9} class="shrink-0 text-sky-400" />
          {:else if ref.kind === "head"}
            <Compass size={9} class="shrink-0 text-accent" />
          {:else}
            <GitBranch size={9} class="shrink-0 text-accent" />
          {/if}
          <span class="truncate">{ref.name}</span>
        </span>
      {/each}
      {#if hiddenRefCount > 0}
        <span class="rounded-full border border-border px-1.5 py-0.5 text-[9px] text-textMuted">
          +{hiddenRefCount} more
        </span>
      {/if}
    </div>
  {/if}

  <dl class="grid grid-cols-[auto_minmax(0,1fr)] gap-x-2 gap-y-1.5 px-3 py-2.5 text-[10px]">
    <dt class="flex items-center gap-1.5 text-textMuted">
      <!-- Same identity module as the canvas avatars: hue and initials always
           agree between gutter column, rows and this card. -->
      <span
        class="inline-flex h-4 w-4 items-center justify-center rounded-full text-[7px] font-bold text-white ring-1 ring-background"
        style="background-color: {authorColor(identity.hue)}"
        aria-hidden="true"
      >{identity.initials}</span>
      Author
    </dt>
    <dd
      class="truncate text-right"
      title={(row.author_name || "Unknown") + (row.author_email ? ` <${row.author_email}>` : "")}
    >
      {row.author_name || "Unknown"}{#if row.author_email}<span class="text-textMuted"> · {row.author_email}</span>{/if}
    </dd>

    <dt class="flex items-center gap-1 text-textMuted"><CalendarClock size={11} /> Created</dt>
    <dd class="text-right">
      <!-- At-a-glance age first; the exact stamp stays one hover away via title. -->
      <span title={formatTimestamp(row.timestamp)}>{formatRelativeTime(row.timestamp) || formatTimestamp(row.timestamp)}</span>
      <span class="text-textMuted"> · {formatTimestamp(row.timestamp)}</span>
    </dd>

    <dt class="flex items-center gap-1 text-textMuted"><GitCommit size={11} /> Parents</dt>
    <dd class="text-right">{row.parent_ids.length} {row.parent_ids.length === 1 ? "parent" : "parents"}</dd>
  </dl>
</div>
