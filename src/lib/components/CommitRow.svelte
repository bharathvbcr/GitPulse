<script lang="ts">
  import type { VisualCommitRow } from "../canvas/GraphRenderer";
  import { getBranchColor } from "../canvas/Palette";
  import { formatRelativeTime } from "../format";
  import { authorColor, authorIdentity } from "../authors/authorIdentity";
  import { GitMerge, GitBranch, Cloud, Tag, Compass } from "lucide-svelte";

  export interface RefItem {
    name: string;
    kind: "head" | "current-branch" | "local-branch" | "remote-branch" | "tag";
  }

  let {
    row,
    isSelected = false,
    density = "spacious",
    refs = [],
    onSelect,
  }: {
    row: VisualCommitRow;
    isSelected?: boolean;
    density?: "spacious" | "compact";
    refs?: RefItem[];
    onSelect?: () => void;
  } = $props();

  const isCompact = $derived(density === "compact");

  // Shared identity module — same hue/initials as the canvas gutter column and
  // tooltips. The old inline hash covered name only: two authors sharing a
  // display name collapsed to one colour; email now disambiguates.
  const avatar = $derived(authorIdentity(row.author_name, row.author_email));

  function getConventionalType(msg: string): { type: string; color: string } | null {
    const match = msg.match(/^([a-zA-Z]+)(\([^\)]+\))?(!)?:\s/);
    if (!match) return null;
    const type = match[1].toLowerCase();
    switch (type) {
      case "feat": return { type: "feat", color: "bg-emerald-500/15 text-emerald-400 border-emerald-500/30" };
      case "fix": return { type: "fix", color: "bg-rose-500/15 text-rose-400 border-rose-500/30" };
      case "refactor": return { type: "refactor", color: "bg-purple-500/15 text-purple-400 border-purple-500/30" };
      case "docs": return { type: "docs", color: "bg-sky-500/15 text-sky-400 border-sky-500/30" };
      case "chore": return { type: "chore", color: "bg-slate-500/15 text-slate-400 border-slate-500/30" };
      case "perf": return { type: "perf", color: "bg-amber-500/15 text-amber-400 border-amber-500/30" };
      case "test": return { type: "test", color: "bg-orange-500/15 text-orange-400 border-orange-500/30" };
      // parseQuery's server-fetchable type filter also accepts these two;
      // rendering them generic gray made filtered rows look unclassified.
      case "build": return { type: "build", color: "bg-teal-500/15 text-teal-400 border-teal-500/30" };
      case "ci": return { type: "ci", color: "bg-indigo-500/15 text-indigo-400 border-indigo-500/30" };
      default: return { type, color: "bg-zinc-500/15 text-zinc-400 border-zinc-500/30" };
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key !== "Enter" && e.key !== " ") return;
    // Space on a role=button div scrolls the virtualized list by default;
    // without preventDefault selecting also page-jumps.
    e.preventDefault();
    onSelect?.();
  }

  let conventional = $derived(getConventionalType(row.summary || ""));
</script>

<div
  role="button"
  tabindex="0"
  onclick={onSelect}
  onkeydown={handleKeydown}
  aria-pressed={isSelected}
  class="{isCompact ? 'h-[26px] px-2.5 gap-2 text-[11px]' : 'h-9 px-3 gap-3 text-xs'} flex items-center cursor-pointer select-none transition-[color,background-color,border-color,box-shadow] duration-150 rounded-lg {isSelected ? 'bg-accent/15 text-textPrimary font-medium ring-1 ring-inset ring-accent/35 shadow-sm' : 'hover:bg-surfaceHover/70 text-textPrimary/90'}"
>
  <!-- Short SHA -->
  <span class="font-mono text-accent/80 shrink-0 tracking-tight {isCompact ? 'text-[10px] w-14' : 'text-[11px] w-16'}">
    {row.id.substring(0, 7)}
  </span>


  <!-- Commit Summary, Badges & Ref Pills -->
  <div class="flex-1 flex items-center gap-2 truncate">
    {#if conventional}
      <span class="inline-flex items-center text-[9px] font-semibold px-1.5 py-0.5 rounded-full border {conventional.color}">
        {conventional.type}
      </span>
    {/if}

    {#if row.is_merge}
      <span class="inline-flex items-center gap-1 text-[9px] px-1.5 py-0.5 bg-purple-500/20 text-purple-300 rounded-full border border-purple-500/30 font-medium">
        <GitMerge size={10} />
        merge
      </span>
    {/if}

    {#each refs as r}
      {#if r.kind === "head"}
        <span class="inline-flex items-center gap-1 text-[9px] font-mono font-bold px-1.5 py-0.5 rounded-full border border-accent bg-accent/25 text-accent shadow-sm">
          <Compass size={10} />
          HEAD
        </span>
      {:else if r.kind === "current-branch"}
        <span
          class="inline-flex items-center gap-1 text-[9px] font-mono font-semibold px-2 py-0.5 rounded-full border shadow-sm"
          style="border-color: {getBranchColor(row.color_index)}; color: {getBranchColor(row.color_index)};"
        >
          <GitBranch size={10} />
          {r.name}
        </span>
      {:else if r.kind === "remote-branch"}
        <span class="inline-flex items-center gap-1 text-[9px] font-mono px-1.5 py-0.5 rounded-full border border-sky-500/30 bg-sky-500/10 text-sky-400">
          <Cloud size={10} />
          {r.name}
        </span>
      {:else if r.kind === "tag"}
        <span class="inline-flex items-center gap-1 text-[9px] font-mono px-1.5 py-0.5 rounded-full border border-amber-500/30 bg-amber-500/10 text-amber-300">
          <Tag size={10} />
          {r.name}
        </span>
      {:else}
        <span
          class="inline-flex items-center gap-1 text-[9px] font-mono px-1.5 py-0.5 rounded-full border border-border/80 bg-surfaceHover text-textPrimary"
        >
          <GitBranch size={10} class="text-textMuted" />
          {r.name}
        </span>
      {/if}
    {/each}

    <span class="truncate" title={row.summary || undefined}>{row.summary || "No commit message"}</span>
  </div>

  <!-- Author Name & Relative Date -->
  <span class="{isCompact ? 'text-[10px] w-24' : 'text-[11px] w-28'} text-textMuted shrink-0 truncate text-right font-medium">
    {row.author_name}
  </span>
  <span class="{isCompact ? 'text-[10px] w-14' : 'text-[11px] w-16'} text-textMuted/70 shrink-0 text-right">
    {formatRelativeTime(row.timestamp)}
  </span>

  <!-- Author Initials Avatar -->
  <div
    class="{isCompact ? 'w-3.5 h-3.5 text-[8px]' : 'w-4.5 h-4.5 text-[10px]'} rounded-full flex items-center justify-center text-white font-bold shrink-0 shadow-sm ring-1 ring-background"
    style="background-color: {authorColor(avatar.hue)}"
    title="{row.author_name || 'Unknown'}{row.author_email ? ` <${row.author_email}>` : ''}"
  >
    {avatar.initials}
  </div>
</div>
