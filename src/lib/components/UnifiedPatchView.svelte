<script lang="ts">
  import { ChevronDown, ChevronRight, FileDiff, Filter } from "@lucide/svelte";
  import { splitUnifiedPatch, type UnifiedFileStatus } from "../diff/unifiedPatch";

  interface Props {
    patch: string;
    truncated?: boolean;
    redacted?: boolean;
    totalBytes?: number;
    emptyMessage?: string;
    defaultExpandLimit?: number;
  }

  let {
    patch,
    truncated = false,
    redacted = false,
    totalBytes,
    emptyMessage = "No textual changes in this diff.",
    defaultExpandLimit = 6,
  }: Props = $props();

  const files = $derived(splitUnifiedPatch(patch));
  let fileFilter = $state("");
  const indexedFiles = $derived.by(() => {
    const query = fileFilter.trim().toLowerCase();
    return files
      .map((file, idx) => ({ file, key: `${idx}:${file.path}` }))
      .filter(({ file }) => {
        if (!query) return true;
        return (
          file.path.toLowerCase().includes(query) ||
          (file.oldPath?.toLowerCase().includes(query) ?? false)
        );
      });
  });

  const totals = $derived.by(() => {
    let additions = 0;
    let deletions = 0;
    for (const file of files) {
      additions += file.additions;
      deletions += file.deletions;
    }
    return { additions, deletions };
  });

  let expandedMap = $state<Record<string, boolean>>({});
  let showAllLines = $state<Record<string, boolean>>({});

  const MAX_LINES_PER_FILE = 400;

  function formatByteCount(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  }

  $effect(() => {
    const initial: Record<string, boolean> = {};
    const expandAll = files.length <= defaultExpandLimit;
    for (let i = 0; i < files.length; i += 1) {
      initial[`${i}:${files[i].path}`] = expandAll || i < 2;
    }
    expandedMap = initial;
    showAllLines = {};
    fileFilter = "";
  });

  function isExpanded(key: string): boolean {
    return expandedMap[key] ?? false;
  }

  function toggleFile(key: string) {
    expandedMap = { ...expandedMap, [key]: !isExpanded(key) };
  }

  function setAllExpanded(open: boolean) {
    const next: Record<string, boolean> = {};
    for (let i = 0; i < files.length; i += 1) {
      next[`${i}:${files[i].path}`] = open;
    }
    expandedMap = next;
  }

  function statusTone(status: UnifiedFileStatus): string {
    switch (status) {
      case "added":
        return "ok";
      case "deleted":
        return "danger";
      case "renamed":
        return "accent";
      default:
        return "neutral";
    }
  }
</script>

<div class="space-y-2 text-meta">
  {#if redacted || truncated}
    <div class="flex flex-wrap items-center gap-2 rounded border border-warn/35 bg-warn/10 px-2.5 py-1.5 text-micro text-text">
      {#if redacted}
        <span>Potential secrets/env values were redacted before rendering this diff.</span>
      {/if}
      {#if truncated}
        <span>
          {#if totalBytes && totalBytes > 0}
            Diff preview truncated (total size {formatByteCount(totalBytes)}).
          {:else}
            Diff output exceeded the inline preview cap and was truncated.
          {/if}
        </span>
      {/if}
    </div>
  {/if}

  {#if files.length === 0}
    <div class="flex items-center gap-2 rounded border border-border px-3 py-3 text-textMuted">
      <FileDiff size={14} />
      <span>{emptyMessage}</span>
    </div>
  {:else}
    <div class="flex flex-wrap items-center justify-between gap-2 text-micro font-mono text-textMuted">
      <span>
        {files.length} {files.length === 1 ? "file" : "files"} ·
        <span class="text-ok">+{totals.additions}</span>
        <span class="mx-0.5">/</span>
        <span class="text-danger">−{totals.deletions}</span>
      </span>
      {#if files.length > 1}
        <span class="flex items-center gap-1">
          <button type="button" class="gp-btn" data-size="sm" onclick={() => setAllExpanded(true)}>
            Expand all
          </button>
          <button type="button" class="gp-btn" data-size="sm" onclick={() => setAllExpanded(false)}>
            Collapse all
          </button>
        </span>
      {/if}
    </div>

    {#if files.length > 4}
      <div class="flex items-center gap-1.5 rounded border border-border/65 bg-background px-2 py-1">
        <Filter size={11} class="shrink-0 text-textMuted" />
        <input
          bind:value={fileFilter}
          type="text"
          placeholder="Filter files…"
          aria-label="Filter changed files"
          class="flex-1 min-w-0 bg-transparent text-micro text-text outline-none placeholder:text-textMuted"
        />
      </div>
    {/if}

    <div class="space-y-1.5">
      {#each indexedFiles as { file, key } (key)}
        {@const open = isExpanded(key)}
        <div class="rounded border border-border bg-background overflow-hidden">
          <button
            type="button"
            class="w-full flex items-center justify-between gap-2 px-2.5 py-1.5 text-left hover:bg-background/80 transition-colors"
            aria-expanded={open}
            onclick={() => toggleFile(key)}
          >
            <span class="flex items-center gap-1.5 min-w-0 font-mono text-micro text-text">
              {#if open}
                <ChevronDown size={13} class="shrink-0 text-textMuted" />
              {:else}
                <ChevronRight size={13} class="shrink-0 text-textMuted" />
              {/if}
              <span class="truncate">
                {#if file.oldPath && file.status === "renamed"}
                  {file.oldPath} → {file.path}
                {:else}
                  {file.path}
                {/if}
              </span>
            </span>
            <span class="flex items-center gap-2 shrink-0 font-mono text-micro">
              {#if file.status !== "modified"}
                <span class="gp-chip" data-tone={statusTone(file.status)}>{file.status}</span>
              {/if}
              {#if !file.binary}
                <span class="text-ok">+{file.additions}</span>
                <span class="text-danger">−{file.deletions}</span>
              {/if}
            </span>
          </button>

          {#if open}
            {#if file.binary}
              <div class="px-3 py-2 text-micro font-mono text-textMuted">
                Binary file differs.
              </div>
            {:else if file.hunks.length === 0}
              <div class="px-3 py-2 text-micro font-mono text-textMuted">
                No line hunks (mode/rename change only).
              </div>
            {:else}
              {@const fullLines = showAllLines[key] ?? false}
              {@const totalLines = file.hunks.reduce((acc, h) => acc + h.lines.length, 0)}
              {@const limit = fullLines ? Number.MAX_SAFE_INTEGER : MAX_LINES_PER_FILE}
              <div class="overflow-x-auto font-mono text-micro leading-[18px] pb-1">
                {#each file.hunks as hunk, hunkIdx (hunkIdx)}
                  {@const priorLines = file.hunks.slice(0, hunkIdx).reduce((acc, h) => acc + h.lines.length, 0)}
                  {#if priorLines < limit}
                    <div class="bg-accent/5 text-textMuted px-2.5 py-0.5 select-none">
                      {hunk.header}
                    </div>
                    {#each hunk.lines.slice(0, Math.max(0, limit - priorLines)) as line, lineIdx (lineIdx)}
                      <div
                        class="flex items-stretch whitespace-pre {line.kind === 'add'
                          ? 'bg-ok/10 text-text'
                          : line.kind === 'del'
                            ? 'bg-danger/10 text-text'
                            : line.kind === 'meta'
                              ? 'text-textMuted italic'
                              : 'text-textMuted'}"
                      >
                        <span class="w-10 shrink-0 px-1.5 text-right select-none opacity-60">{line.oldLine ?? ""}</span>
                        <span class="w-10 shrink-0 pr-2 pl-1.5 text-right select-none opacity-60">{line.newLine ?? ""}</span>
                        <span class="w-5 shrink-0 text-center select-none">{line.kind === "add" ? "+" : line.kind === "del" ? "-" : line.kind === "meta" ? "\\" : " "}</span>
                        <span class="px-1.5 flex-1">{line.text}</span>
                      </div>
                    {/each}
                  {/if}
                {/each}
                {#if !fullLines && totalLines > MAX_LINES_PER_FILE}
                  <div class="px-2.5 py-1.5 flex items-center justify-between text-micro text-textMuted">
                    <span>Showing first {MAX_LINES_PER_FILE} of {totalLines} lines in {file.path}</span>
                    <button
                      type="button"
                      class="gp-btn"
                      data-size="sm"
                      onclick={() => (showAllLines = { ...showAllLines, [key]: true })}
                    >
                      Show all
                    </button>
                  </div>
                {/if}
              </div>
            {/if}
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>
