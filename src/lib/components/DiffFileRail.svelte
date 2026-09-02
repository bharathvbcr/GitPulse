<script lang="ts">
  /**
   * The file list that stays beside the diff.
   *
   * Presentational on purpose: it takes a built rail and reports which entry
   * was clicked. The Diff view owns which store call that turns into, because
   * a commit file and a working-tree file open through different commands and
   * only the view knows which selection is live.
   */
  import { FileCode, PanelLeftClose } from "lucide-svelte";
  import {
    churnLabel,
    displayName,
    entryKey,
    isCurrent,
    truncationNote,
    type FileRail,
    type RailEntry,
  } from "../diff/fileRail";

  let {
    rail,
    currentPath,
    currentIsStaged,
    onOpen,
    onCollapse,
  }: {
    rail: FileRail;
    currentPath: string | null;
    currentIsStaged: boolean;
    onOpen: (entry: RailEntry) => void;
    onCollapse: () => void;
  } = $props();

  const note = $derived(truncationNote(rail));

  /**
   * Status letters get a colour so the list scans without reading every row.
   * Unknown letters fall through to muted rather than to a colour that would
   * assert a meaning this build does not know.
   */
  function statusTone(code: string): string {
    switch (code.charAt(0)) {
      case "A":
        return "text-emerald-600 dark:text-emerald-400";
      case "D":
        return "text-rose-600 dark:text-rose-400";
      case "R":
      case "C":
        return "text-sky-600 dark:text-sky-400";
      case "M":
        return "text-amber-600 dark:text-amber-400";
      default:
        return "text-textMuted";
    }
  }
</script>

<aside
  class="flex w-56 shrink-0 flex-col border-r border-border/60 bg-surface/40 font-sans"
  aria-label="Files in this diff"
>
  <div class="flex items-center gap-1.5 border-b border-border/60 px-2.5 py-1.5">
    <FileCode size={12} class="shrink-0 text-accent" />
    <span class="text-[11px] font-semibold text-textPrimary">
      {rail.source === "commit" ? "Commit files" : "Changed files"}
    </span>
    <span class="ml-auto text-[10px] text-textMuted">{rail.entries.length}</span>
    <button
      type="button"
      class="rounded p-0.5 text-textMuted hover:text-textPrimary"
      onclick={onCollapse}
      title="Hide the file list"
      aria-label="Hide the file list"
    >
      <PanelLeftClose size={12} />
    </button>
  </div>

  <!-- A cut-short list must say so. Rendering the first fifty of three hundred
       as though they were all of them tells the reader they have seen the
       whole commit. -->
  {#if note}
    <p class="border-b border-border/60 px-2.5 py-1 text-[10px] text-amber-600 dark:text-amber-400">
      {note}
    </p>
  {/if}

  <ul class="min-h-0 flex-1 overflow-y-auto py-1">
    {#each rail.entries as entry (entryKey(entry))}
      {@const active = isCurrent(entry, currentPath, currentIsStaged, rail.source)}
      {@const churn = churnLabel(entry)}
      <li>
        <button
          type="button"
          class="flex w-full items-center gap-1.5 px-2.5 py-1 text-left text-[11px] hover:bg-surfaceHover {active
            ? 'bg-accent/10 text-accent'
            : 'text-textMuted'}"
          aria-current={active ? "true" : undefined}
          title={entry.oldPath && entry.oldPath !== entry.path
            ? `${entry.oldPath} → ${entry.path}`
            : entry.path}
          onclick={() => onOpen(entry)}
        >
          <span class="w-3 shrink-0 font-mono text-[10px] {statusTone(entry.statusCode)}">
            {entry.statusCode.charAt(0) || "?"}
          </span>
          <span class="min-w-0 flex-1 truncate">{displayName(entry)}</span>
          {#if rail.source === "worktree" && entry.isStaged}
            <span class="shrink-0 text-[9px] text-emerald-600 dark:text-emerald-400">staged</span>
          {/if}
          {#if churn}
            <span class="shrink-0 font-mono text-[9px] text-textMuted">{churn}</span>
          {/if}
        </button>
      </li>
    {/each}
  </ul>
</aside>
