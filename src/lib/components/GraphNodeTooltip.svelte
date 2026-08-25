<script lang="ts">
  import {
    CalendarClock,
    Cloud,
    Compass,
    GitBranch,
    GitCommit,
    GitMerge,
    Tag,
    User,
  } from "lucide-svelte";
  import type { VisualCommitRow } from "../canvas/GraphRenderer";
  import type { TooltipPlacement } from "../canvas/graphInteraction";
  import type { RefItem } from "./CommitRow.svelte";

  let {
    row,
    refs = [],
    placement = "below",
  }: {
    row: VisualCommitRow;
    refs?: RefItem[];
    placement?: TooltipPlacement;
  } = $props();

  const visibleRefs = $derived(refs.slice(0, 4));
  const hiddenRefCount = $derived(Math.max(0, refs.length - visibleRefs.length));

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
  class="gp-pop relative rounded-2xl border border-border/70 bg-surface/95 text-textPrimary shadow-pop backdrop-blur-md"
>
  <div
    class="absolute left-4 h-3 w-3 rotate-45 rounded-[2px] border-border bg-surface {placement === 'above'
      ? '-bottom-1.5 border-b border-r'
      : '-top-1.5 border-l border-t'}"
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
      <div class="mt-1 flex items-center gap-2 font-mono text-[10px] text-textMuted">
        <span class="select-all break-all">{row.id}</span>
        <span class="shrink-0 rounded-full bg-background px-1.5 py-0.5 text-textPrimary/80">
          {row.is_merge ? "Merge commit" : row.is_root ? "Root commit" : "Commit"}
        </span>
      </div>
    </div>
  </div>

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
    <dt class="flex items-center gap-1 text-textMuted"><User size={11} /> Author</dt>
    <dd class="truncate text-right">
      {row.author_name || "Unknown"}{#if row.author_email}<span class="text-textMuted"> · {row.author_email}</span>{/if}
    </dd>

    <dt class="flex items-center gap-1 text-textMuted"><CalendarClock size={11} /> Created</dt>
    <dd class="text-right">{formatTimestamp(row.timestamp)}</dd>

    <dt class="flex items-center gap-1 text-textMuted"><GitCommit size={11} /> Parents</dt>
    <dd class="text-right">{row.parent_ids.length} {row.parent_ids.length === 1 ? "parent" : "parents"}</dd>
  </dl>
</div>
