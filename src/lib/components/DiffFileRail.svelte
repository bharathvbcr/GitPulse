<script lang="ts">
  /**
   * The file list that stays beside the diff.
   *
   * Presentational on purpose: it takes a built rail and reports which entry
   * was clicked. The Diff view owns which store call that turns into, because
   * a commit file and a working-tree file open through different commands and
   * only the view knows which selection is live.
   */
  import { FileCode, GitCommitHorizontal, PanelLeftClose, Pencil } from "lucide-svelte";
  import {
    churnLabel,
    displayName,
    entryKey,
    isCurrent,
    truncationNote,
    type FileRail,
    type RailEntry,
  } from "../diff/fileRail";
  import {
    commitLabel,
    isCurrentCommit,
    pickerNote,
    type CommitEntry,
    type CommitRail,
  } from "../diff/commitRail";
  import { formatRelativeTime, shortHash } from "../format";

  let {
    rail,
    commits,
    currentPath,
    currentIsStaged,
    selectedCommitId,
    workingTreeCount,
    onOpen,
    onPickCommit,
    onPickWorkingTree,
    onCollapse,
  }: {
    rail: FileRail;
    /** Recent commits, so moving BETWEEN changes needs no trip to Graph. */
    commits: CommitRail;
    currentPath: string | null;
    currentIsStaged: boolean;
    selectedCommitId: string | null;
    /** Uncommitted files; -1 when the count is not known. */
    workingTreeCount: number;
    onOpen: (entry: RailEntry) => void;
    onPickCommit: (entry: CommitEntry) => void;
    onPickWorkingTree: () => void;
    onCollapse: () => void;
  } = $props();

  const note = $derived(truncationNote(rail));
  const commitNote = $derived(pickerNote(commits));
  /** True while the diff on screen is uncommitted work rather than a commit. */
  const onWorkingTree = $derived(selectedCommitId === null);

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
  <!-- Which change: uncommitted work, or one of the recent commits. Without
       this, comparing a file across two commits still meant leaving for the
       Graph view and being switched back — the same round trip the file list
       removed, one level up. -->
  <div class="flex items-center gap-1.5 border-b border-border/60 px-2.5 py-1.5">
    <GitCommitHorizontal size={12} class="shrink-0 text-accent" />
    <span class="text-[11px] font-semibold text-textPrimary">Change</span>
    <button
      type="button"
      class="ml-auto rounded p-0.5 text-textMuted hover:text-textPrimary"
      onclick={onCollapse}
      title="Hide the sidebar"
      aria-label="Hide the sidebar"
    >
      <PanelLeftClose size={12} />
    </button>
  </div>

  <div class="max-h-52 shrink-0 overflow-y-auto border-b border-border/60 py-1">
    <!-- Uncommitted work is first because it is what a reader is most often
         coming back to, and it is the one entry that is not in the graph. -->
    <button
      type="button"
      class="flex w-full items-center gap-1.5 px-2.5 py-1 text-left text-[11px] hover:bg-surfaceHover {onWorkingTree
        ? 'bg-accent/10 text-accent'
        : 'text-textMuted'}"
      aria-current={onWorkingTree ? "true" : undefined}
      onclick={onPickWorkingTree}
    >
      <Pencil size={11} class="shrink-0" />
      <span class="min-w-0 flex-1 truncate">Uncommitted changes</span>
      <!-- -1 is "not counted"; rendering it as 0 would report an unscanned
           working tree as clean. -->
      {#if workingTreeCount > 0}
        <span class="shrink-0 font-mono text-[9px]">{workingTreeCount}</span>
      {:else if workingTreeCount === 0}
        <span class="shrink-0 text-[9px]">clean</span>
      {/if}
    </button>

    {#each commits.entries as commit (commit.id)}
      {@const active = isCurrentCommit(commit, selectedCommitId)}
      <button
        type="button"
        class="flex w-full items-start gap-1.5 px-2.5 py-1 text-left text-[11px] hover:bg-surfaceHover {active
          ? 'bg-accent/10 text-accent'
          : 'text-textMuted'}"
        aria-current={active ? "true" : undefined}
        title={commitLabel(commit)}
        onclick={() => onPickCommit(commit)}
      >
        <span class="mt-px shrink-0 font-mono text-[9px] opacity-70">{shortHash(commit.id)}</span>
        <span class="min-w-0 flex-1">
          <span class="block truncate">{commitLabel(commit)}</span>
          <span class="block truncate text-[9px] opacity-70">
            {commit.authorName} · {formatRelativeTime(commit.timestamp)}{commit.isMerge
              ? " · merge"
              : ""}
          </span>
        </span>
      </button>
    {/each}

    {#if commitNote}
      <p class="px-2.5 py-1 text-[9px] text-textMuted">{commitNote}</p>
    {/if}
  </div>

  <div class="flex items-center gap-1.5 border-b border-border/60 px-2.5 py-1.5">
    <FileCode size={12} class="shrink-0 text-accent" />
    <span class="text-[11px] font-semibold text-textPrimary">
      {rail.source === "commit" ? "Commit files" : "Changed files"}
    </span>
    <span class="ml-auto text-[10px] text-textMuted">{rail.entries.length}</span>
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
