<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { invoke } from "@tauri-apps/api/core";
  import { Percent, RefreshCw, FileCode } from "lucide-svelte";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import {
    coverageBarColor,
    coverageHitClass,
    formatCoveragePercent,
  } from "../coverage/format";
  import type { CoverageReport, FileCoverage } from "../coverage/types";
  import VirtualList from "./VirtualList.svelte";
  import EmptyState from "./EmptyState.svelte";

  let report: CoverageReport | null = $state(null);
  let isScanning = $state(false);
  let scanError = $state<string | null>(null);
  let selectedPath = $state<string | null>(null);
  let sourceLines: string[] = $state([]);
  let hitMap: Map<number, number> = $state(new Map());
  let fileError = $state<string | null>(null);
  let isLoadingFile = $state(false);
  let scanInflight: AsyncGuard | null = null;

  function beginScan(): AsyncGuard {
    scanInflight?.cancel();
    const guard = createAsyncGuard();
    scanInflight = guard;
    return guard;
  }

  async function scan(repo: string, guard: AsyncGuard) {
    isScanning = true;
    scanError = null;
    try {
      const next = await invoke<CoverageReport>("cmd_scan_coverage", { repoPath: repo });
      if (!guard.isLive()) return;
      report = next;
      if (!selectedPath && next.files.length > 0) {
        selectedPath = next.files[0].path;
      }
    } catch (err: unknown) {
      if (!guard.isLive()) return;
      scanError = String(err);
    } finally {
      if (guard.isLive()) isScanning = false;
    }
  }

  function rescan() {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    const guard = beginScan();
    void scan(repo, guard);
  }

  $effect(() => {
    return () => scanInflight?.cancel();
  });

  $effect(() => {
    const selected = $repoStore.selectedFilePath;
    if (selected) {
      selectedPath = selected;
    }
  });

  $effect(() => {
    const repo = $repoStore.currentPath;
    if (!repo) {
      scanInflight?.cancel();
      report = null;
      selectedPath = null;
      scanError = null;
      isScanning = false;
      return;
    }
    selectedPath = null;
    fileError = null;
    const guard = beginScan();
    void scan(repo, guard);
    return () => {
      if (scanInflight === guard) {
        guard.cancel();
      }
    };
  });

  $effect(() => {
    const repo = $repoStore.currentPath;
    const path = selectedPath;
    if (!repo || !path) {
      sourceLines = [];
      hitMap = new Map();
      return;
    }
    let cancelled = false;
    isLoadingFile = true;
    fileError = null;
    void (async () => {
      try {
        const [detail, content] = await Promise.all([
          invoke<FileCoverage>("cmd_get_file_coverage", {
            repoPath: repo,
            filePath: path,
          }),
          invoke<string>("cmd_get_file_content", {
            repoPath: repo,
            filePath: path,
            commitId: null,
          }).catch(() => ""),
        ]);
        if (cancelled) return;
        const next = new Map<number, number>();
        for (const line of detail.lines) {
          next.set(line.line_no, line.hits);
        }
        hitMap = next;
        sourceLines = content.length > 0 ? content.split("\n") : [];
        if (sourceLines.length === 1 && sourceLines[0] === "") {
          sourceLines = [];
        }
      } catch (err: unknown) {
        if (!cancelled) {
          fileError = String(err);
          sourceLines = [];
          hitMap = new Map();
        }
      } finally {
        if (!cancelled) isLoadingFile = false;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  function selectFile(path: string) {
    selectedPath = path;
    repoStore.selectFilePath(path);
  }
</script>

<div class="flex-1 flex flex-col bg-background h-full text-xs overflow-hidden">
  <div class="px-4 py-2 border-b border-border/60 bg-surface/60 flex items-center justify-between font-sans shrink-0">
    <div class="flex items-center gap-3 min-w-0">
      <Percent size={16} class="text-accent shrink-0" />
      {#if report}
        <div class="flex items-center gap-2">
          <span
            class="font-semibold tabular-nums"
            style="color: {coverageBarColor(report.overall.percentage)}"
          >{formatCoveragePercent(report.overall.percentage)}</span>
          <span class="text-textMuted">
            {report.overall.lines_hit}/{report.overall.lines_found} lines
          </span>
        </div>
      {:else}
        <span class="text-textMuted">Test coverage</span>
      {/if}
    </div>
    <div class="flex items-center gap-3">
      <div class="flex items-center gap-3 text-[11px] text-textMuted">
        <span class="flex items-center gap-1"><span class="w-2.5 h-2.5 rounded-full bg-emerald-500/50"></span> hit</span>
        <span class="flex items-center gap-1"><span class="w-2.5 h-2.5 rounded-full bg-red-500/50"></span> missed</span>
        <span class="flex items-center gap-1"><span class="w-2.5 h-2.5 rounded-full bg-gray-500/30"></span> uninstrumented</span>
      </div>
      <button type="button" class="gp-icon-btn !p-1 hover:text-accent" title="Rescan coverage artifacts" onclick={rescan}>
        <RefreshCw size={13} class={isScanning ? "animate-spin" : ""} />
      </button>
    </div>
  </div>

  {#if report && scanError}
    <div class="px-4 py-1.5 border-b border-border bg-red-500/10 text-red-400 font-sans shrink-0 truncate" title={scanError}>
      Rescan failed: {scanError} — showing previous results.
    </div>
  {/if}

  {#if report && report.families.length > 0}
    <div class="px-4 py-1.5 border-b border-border/40 bg-surface/40 flex items-center gap-3 overflow-x-auto font-sans">
      {#each report.families as family}
        <div class="flex items-center gap-1.5 shrink-0" title="{family.expected_formats.join(', ')} · {family.expected_paths.join(', ')}">
          <span class="w-2 h-2 rounded-full" style="background-color: {family.color_hex}"></span>
          <span class="text-textPrimary/80">{family.languages.join(", ")}</span>
          <span class="text-textMuted/70">{family.family}</span>
          {#if family.found}
            <span class="text-emerald-400/80">report found</span>
          {:else}
            <span class="text-textMuted/60">no report</span>
          {/if}
        </div>
      {/each}
      {#if report.truncated}
        <span class="text-amber-400 shrink-0">scan capped</span>
      {/if}
    </div>
  {/if}

  <div class="flex-1 flex min-h-0">
    <div class="w-72 shrink-0 border-r border-border/60 flex flex-col bg-surface/40 p-1.5">
      {#if isScanning && !report}
        <div class="flex-1 flex items-center justify-center text-textMuted font-sans">Scanning coverage…</div>
      {:else if scanError}
        <div class="p-3 text-rose-400 font-sans">{scanError}</div>
      {:else if report && report.files.length > 0}
        <div class="flex-1 overflow-auto space-y-0.5">
          {#each report.files as file}
            <button
              type="button"
              onclick={() => selectFile(file.path)}
              class="w-full px-2.5 py-1.5 rounded-full text-left flex items-center gap-2 transition-colors {selectedPath === file.path ? 'bg-accent/15 ring-1 ring-inset ring-accent/30' : 'hover:bg-surfaceHover'}"
            >
              <span class="w-2 h-2 rounded-full shrink-0" style="background-color: {file.color_hex}"></span>
              <span class="flex-1 truncate font-mono text-[11px] text-textPrimary">{file.path}</span>
              <span class="tabular-nums shrink-0" style="color: {coverageBarColor(file.percentage)}">{formatCoveragePercent(file.percentage)}</span>
            </button>
          {/each}
        </div>
      {:else if report}
        <div class="p-3 text-textMuted font-sans space-y-2">
          <p>No coverage reports for the detected languages.</p>
          {#each report.families as family}
            <p>
              Looked for {family.family} ({family.expected_formats.join(", ")}):
              {family.expected_paths.join(", ")}
            </p>
          {/each}
          {#if report.families.length === 0}
            <p>No programming languages found to scan.</p>
          {/if}
        </div>
      {:else}
        <EmptyState
          icon={Percent}
          title="No coverage report"
          hint="Open a repository to scan coverage artifacts."
        />
      {/if}

      {#if report && report.artifacts.length > 0}
        <div class="border-t border-border/60 p-2 text-[10px] text-textMuted font-sans max-h-28 overflow-auto">
          {#each report.artifacts as artifact}
            <div class="truncate" title={artifact.skip_reason || artifact.format}>
              {artifact.path}
              <span class="text-textMuted/70">({artifact.format})</span>
              {#if artifact.skipped}
                <span class="text-amber-400"> skipped</span>
              {/if}
            </div>
          {/each}
        </div>
      {/if}
    </div>

    <div class="flex-1 min-h-0 font-mono">
      {#if isLoadingFile}
        <div class="h-full flex items-center justify-center text-textMuted font-sans">Loading {selectedPath}…</div>
      {:else if fileError}
        <div class="h-full flex items-center justify-center text-rose-400 font-sans p-4">{fileError}</div>
      {:else if sourceLines.length > 0}
        <VirtualList items={sourceLines} rowHeight={24} overscan={15} class="h-full">
          {#snippet row(line, index)}
            {#if line !== undefined}
              {@const hits = hitMap.get(index + 1)}
              <div class="flex items-center h-6 hover:bg-surfaceHover/40 {coverageHitClass(hits)}" style="height: 24px;">
                <span class="w-10 px-2 text-right text-textMuted/40 text-[10px] select-none shrink-0">{index + 1}</span>
                <span class="w-8 px-1 text-right text-[10px] tabular-nums shrink-0 {hits === undefined ? 'text-transparent' : hits > 0 ? 'text-emerald-400/80' : 'text-red-400/80'}">{hits === undefined ? "·" : hits}</span>
                <span class="px-3 whitespace-pre overflow-hidden text-textPrimary">{line}</span>
              </div>
            {/if}
          {/snippet}
        </VirtualList>
      {:else if selectedPath}
        <EmptyState
          icon={FileCode}
          title={hitMap.size === 0 ? "Not in any coverage report" : "No source available"}
          hint={hitMap.size === 0
            ? "This file was not instrumented by the coverage artifacts found in the repository."
            : "The source for this path could not be loaded."}
        />
      {:else}
        <EmptyState
          icon={Percent}
          title="Pick a file"
          hint="Select a file on the left to inspect its line coverage."
        />
      {/if}
    </div>
  </div>
</div>
