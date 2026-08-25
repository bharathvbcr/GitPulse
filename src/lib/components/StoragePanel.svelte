<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { invoke } from "@tauri-apps/api/core";
  import {
    HardDrive,
    RefreshCw,
    Clipboard,
    LoaderCircle,
    AlertTriangle,
    Trash2,
    GitBranch,
    Boxes,
    Clock,
    Sparkles,
    FolderTree,
  } from "lucide-svelte";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { copyText } from "../desktop/clipboard";
  import { formatError } from "../ui/formatError";
  import { identityKey, isCaseInsensitiveFs } from "../repos/paths";
  import type { StorageReport, ArtifactDir } from "../storage/types";
  import {
    deltaClass,
    formatAge,
    formatDelta,
    formatSnapshotTime,
    humanBytes,
    pctOf,
  } from "../storage/format";
  import {
    clearRepoHistory,
    deltaOver,
    deltaVsPrevious,
    historyFor,
    loadHistory,
    recordSnapshot,
    saveHistory,
    type StorageHistoryMap,
    type StorageSnapshot,
  } from "../storage/history";
  import type { StorageLike } from "../repos/persist";

  let report = $state<StorageReport | null>(null);
  let loading = $state(false);
  let errorMsg = $state<string | null>(null);
  let copied = $state(false);
  let historyVersion = $state(0);

  const scanned = { path: "" };
  let inflight: AsyncGuard | null = null;
  let copyTimer: number | null = null;

  /** Injected for tests; the app uses localStorage when available. */
  let storageOverride: StorageLike | null | undefined = undefined;

  function persistentStorage(): StorageLike | null {
    if (storageOverride !== undefined) return storageOverride;
    try {
      return typeof localStorage === "undefined" ? null : localStorage;
    } catch {
      return null;
    }
  }

  function repoKey(path: string): string {
    return identityKey(path, { caseInsensitive: isCaseInsensitiveFs() });
  }

  function readHistory(): StorageHistoryMap {
    return loadHistory(persistentStorage());
  }

  async function scan(path?: string) {
    const repoPath = path ?? $repoStore.currentPath;
    if (!repoPath) return;
    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    loading = true;
    errorMsg = null;
    try {
      const next = await invoke<StorageReport>("cmd_storage_scan", { repoPath });
      if (!guard.isLive()) return;
      report = next;
      // Record the snapshot so the usage history grows with real scans.
      const map = recordSnapshot(readHistory(), repoKey(repoPath), toSnapshot(next));
      saveHistory(persistentStorage(), map);
      historyVersion += 1;
    } catch (err) {
      if (!guard.isLive()) return;
      errorMsg = formatError(err);
      report = null;
      scanned.path = "";
    } finally {
      if (guard.isLive()) loading = false;
    }
  }

  function toSnapshot(next: StorageReport): StorageSnapshot {
    return {
      t: Math.max(1, next.generated_at_epoch_secs * 1000 || Date.now()),
      grand: next.totals.grand_bytes,
      git: next.totals.git_dir_bytes,
      build: next.totals.build_artifacts_bytes,
      cache: next.totals.cache_artifacts_bytes,
    };
  }

  let series = $derived.by(() => {
    void historyVersion;
    const path = $repoStore.currentPath;
    if (!path) return [] as StorageSnapshot[];
    return historyFor(readHistory(), repoKey(path));
  });

  let lastDelta = $derived(deltaVsPrevious(series));
  let weekDelta = $derived(deltaOver(series, 7 * 24 * 60 * 60 * 1000));

  // ---- Derived report slices -------------------------------------------

  let otherWorktreeBytes = $derived.by(() => {
    const current = report;
    if (!current) return 0;
    const { worktree_bytes, build_artifacts_bytes, cache_artifacts_bytes } = current.totals;
    return Math.max(0, worktree_bytes - build_artifacts_bytes - cache_artifacts_bytes);
  });

  interface Segment {
    label: string;
    bytes: number;
    class: string;
  }

  let segments = $derived.by(() => {
    const current = report;
    if (!current) return [] as Segment[];
    return [
      { label: "Git data", bytes: current.totals.git_dir_bytes, class: "bg-accent" },
      {
        label: "Build output",
        bytes: current.totals.build_artifacts_bytes,
        class: "bg-amber-400",
      },
      { label: "Caches", bytes: current.totals.cache_artifacts_bytes, class: "bg-sky-400" },
      { label: "Other files", bytes: otherWorktreeBytes, class: "bg-emerald-400" },
    ].filter((s) => s.bytes > 0) as Segment[];
  });

  let hygieneGaps = $derived.by(() => {
    const current = report;
    if (!current) return [] as ArtifactDir[];
    return current.artifacts.filter((a) => a.unignored || a.tracked_files > 0);
  });

  function artifactBar(bytes: number): number {
    const max = report?.artifacts[0]?.bytes ?? 1;
    return pctOf(bytes, max);
  }

  // ---- History sparkline ------------------------------------------------

  let sparkPoints = $derived.by(() => {
    const points = series;
    if (points.length < 2) return "";
    const width = 100;
    const height = 28;
    const minT = points[0].t;
    const maxT = points[points.length - 1].t;
    const spanT = Math.max(1, maxT - minT);
    let minV = Number.POSITIVE_INFINITY;
    let maxV = Number.NEGATIVE_INFINITY;
    for (const p of points) {
      minV = Math.min(minV, p.grand);
      maxV = Math.max(maxV, p.grand);
    }
    const spanV = Math.max(1, maxV - minV);
    return points
      .map((p, i) => {
        const x = i === 0 ? 0 : ((p.t - minT) / spanT) * width;
        const y = height - ((p.grand - minV) / spanV) * (height - 2) - 1;
        return `${x.toFixed(2)},${y.toFixed(2)}`;
      })
      .join(" ");
  });

  function clearHistory() {
    const path = $repoStore.currentPath;
    if (!path) return;
    saveHistory(
      persistentStorage(),
      clearRepoHistory(readHistory(), repoKey(path)),
    );
    historyVersion += 1;
  }

  // ---- Copy-as-text ------------------------------------------------------

  function renderedReport(): string | null {
    const current = report;
    if (!current) return null;
    const lines: string[] = [];
    lines.push(`Storage report — ${current.repo_path}`);
    lines.push(`Scanned ${formatSnapshotTime(current.generated_at_epoch_secs * 1000)} in ${current.scan.elapsed_ms} ms${current.scan.truncated ? " (TRUNCATED)" : ""}`);
    lines.push("");
    lines.push(`Total: ${humanBytes(current.totals.grand_bytes)}`);
    lines.push(`  Git data:     ${humanBytes(current.totals.git_dir_bytes)}`);
    lines.push(`  Build output: ${humanBytes(current.totals.build_artifacts_bytes)}`);
    lines.push(`  Caches:       ${humanBytes(current.totals.cache_artifacts_bytes)}`);
    lines.push(`  Other files:  ${humanBytes(otherWorktreeBytes)}`);
    lines.push("");
    lines.push("Git internals:");
    lines.push(`  packfiles: ${humanBytes(current.git.pack_bytes)} in ${current.git.pack_file_count} pack(s)`);
    lines.push(`  loose objects: ${humanBytes(current.git.loose_bytes)} (${current.git.loose_object_count})${current.git.gc_recommended ? " — git gc recommended" : ""}`);
    lines.push(`  reflogs: ${humanBytes(current.git.reflog_bytes)}`);
    if (current.git.lfs_bytes > 0) lines.push(`  LFS: ${humanBytes(current.git.lfs_bytes)}`);
    if (current.git.modules_bytes > 0) lines.push(`  submodules (.git/modules): ${humanBytes(current.git.modules_bytes)}`);
    if (current.git.worktrees_admin_bytes > 0) lines.push(`  worktree admin: ${humanBytes(current.git.worktrees_admin_bytes)}`);
    lines.push(`  index: ${humanBytes(current.git.index_bytes)}`);
    lines.push(`  other: ${humanBytes(current.git.other_bytes)}`);
    if (current.artifacts.length > 0) {
      lines.push("");
      lines.push("Build & cache directories:");
      for (const a of current.artifacts) {
        const flags = [
          a.unignored ? "NOT IGNORED" : null,
          a.tracked_files > 0 ? `${a.tracked_files} tracked` : null,
        ]
          .filter(Boolean)
          .join(", ");
        lines.push(`  ${a.path}: ${humanBytes(a.bytes)} [${a.kind}]${flags ? ` (${flags})` : ""}`);
      }
    }
    if (current.largest_files.length > 0) {
      lines.push("");
      lines.push("Largest working-tree files:");
      for (const f of current.largest_files) lines.push(`  ${f.path}: ${humanBytes(f.bytes)}`);
    }
    if (current.worktrees.length > 0) {
      lines.push("");
      lines.push("Linked worktrees:");
      for (const w of current.worktrees) {
        lines.push(`  ${w.name} (${w.branch ?? "detached"}): ${humanBytes(w.bytes)}${w.truncated ? " (truncated)" : ""}`);
      }
    }
    lines.push("");
    lines.push(`Branches: ${current.branches.local_count} local, ${current.branches.remote_tracking_count} remote-tracking; ${current.branches.merged_stale_count} merged-stale, ${current.branches.gone_upstream_count} upstream-gone`);
    if (series.length >= 2) {
      lines.push("");
      for (const [label, delta] of [
        ["since previous scan", lastDelta],
        ["past week", weekDelta],
      ] as const) {
        if (delta) lines.push(`${label}: ${formatDelta(delta.bytes)}`);
      }
    }
    return lines.join("\n");
  }

  async function copyReport() {
    const text = renderedReport();
    if (!text) return;
    if (await copyText(text)) {
      copied = true;
      if (copyTimer !== null) window.clearTimeout(copyTimer);
      copyTimer = window.setTimeout(() => (copied = false), 1500);
    }
  }

  function openManviCleanup() {
    repoStore.setActiveTab("manvi");
  }

  $effect(() => {
    return () => {
      inflight?.cancel();
      if (copyTimer !== null) window.clearTimeout(copyTimer);
    };
  });

  $effect(() => {
    const path = $repoStore.currentPath;
    if (!path) {
      inflight?.cancel();
      scanned.path = "";
      report = null;
      errorMsg = null;
      loading = false;
      return;
    }
    if (path === scanned.path) return;
    scanned.path = path;
    void scan(path);
  });
</script>

<div class="flex-1 flex flex-col bg-background h-full text-xs font-sans overflow-hidden">
  <div class="px-4 py-2 border-b border-border/60 bg-surface/60 flex items-center justify-between shrink-0">
    <div class="flex items-center gap-2 min-w-0">
      <HardDrive size={16} class="text-accent shrink-0" />
      <span class="font-semibold text-textPrimary">Storage</span>
      {#if report}
        <span class="text-textMuted truncate">
          {humanBytes(report.totals.grand_bytes)} total
          · {humanBytes(report.totals.git_dir_bytes)} git
          · {humanBytes(report.totals.build_artifacts_bytes + report.totals.cache_artifacts_bytes)} artifacts
        </span>
        {#if report.scan.truncated}
          <span class="px-1.5 py-0.5 rounded-full bg-amber-500/15 text-amber-300 text-[10px] uppercase font-semibold">
            partial scan
          </span>
        {/if}
      {/if}
    </div>
    <div class="flex items-center gap-2">
      {#if report}
        <button
          type="button"
          onclick={copyReport}
          class="gp-btn"
          title="Copy the full storage report as text"
        >
          <Clipboard size={13} />
          {copied ? "Copied" : "Copy report"}
        </button>
      {/if}
      <button type="button" onclick={() => scan()} class="gp-btn" title="Rescan disk usage">
        <RefreshCw size={13} class={loading ? "animate-spin" : ""} />
        Rescan
      </button>
    </div>
  </div>

  <div class="flex-1 overflow-auto p-4 space-y-5">
    {#if loading && !report}
      <div class="flex items-center gap-2 text-textMuted">
        <LoaderCircle size={14} class="animate-spin" />
        Walking the repository and the git directory…
      </div>
    {:else if errorMsg}
      <div class="p-3 rounded-xl border border-rose-500/30 bg-rose-500/10 text-rose-200 max-w-2xl">
        {errorMsg}
      </div>
    {:else if report}
      <!-- Overview -->
      <section class="space-y-3 max-w-4xl">
        <div class="grid grid-cols-2 md:grid-cols-4 gap-2">
          <div class="p-3 rounded-2xl border border-border/70 bg-surface shadow-card">
            <div class="text-[10px] uppercase tracking-wider text-textMuted">Total</div>
            <div class="text-base font-semibold text-textPrimary">{humanBytes(report.totals.grand_bytes)}</div>
          </div>
          <div class="p-3 rounded-2xl border border-border/70 bg-surface shadow-card">
            <div class="text-[10px] uppercase tracking-wider text-textMuted">Git data</div>
            <div class="text-base font-semibold text-textPrimary">{humanBytes(report.totals.git_dir_bytes)}</div>
          </div>
          <div class="p-3 rounded-2xl border border-border/70 bg-surface shadow-card">
            <div class="text-[10px] uppercase tracking-wider text-textMuted">Build + caches</div>
            <div class="text-base font-semibold text-amber-300">
              {humanBytes(report.totals.build_artifacts_bytes + report.totals.cache_artifacts_bytes)}
            </div>
          </div>
          <div class="p-3 rounded-2xl border border-border/70 bg-surface shadow-card">
            <div class="text-[10px] uppercase tracking-wider text-textMuted">Reclaimable hints</div>
            <div class="text-base font-semibold text-textPrimary">
              {hygieneGaps.length}{report.git.gc_recommended ? "+gc" : ""}
            </div>
          </div>
        </div>

        <div class="rounded-2xl border border-border/70 bg-surface shadow-card p-3 space-y-2">
          <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">Where the bytes are</h3>
          <div class="h-3 w-full rounded-full overflow-hidden flex bg-surfaceHover">
            {#each segments as segment (segment.label)}
              <div class={segment.class} style="width: {pctOf(segment.bytes, report.totals.grand_bytes).toFixed(2)}%"></div>
            {/each}
          </div>
          <div class="flex flex-wrap gap-x-4 gap-y-1 text-[11px] text-textMuted">
            {#each segments as segment (segment.label)}
              <span class="inline-flex items-center gap-1.5">
                <span class="inline-block h-2 w-2 rounded-sm {segment.class}"></span>
                {segment.label}
                <span class="font-mono text-textSecondary">{humanBytes(segment.bytes)}</span>
                <span>({pctOf(segment.bytes, report.totals.grand_bytes).toFixed(0)}%)</span>
              </span>
            {/each}
          </div>
        </div>
      </section>

      <!-- Hygiene gaps -->
      {#if hygieneGaps.length > 0}
        <section class="space-y-2 max-w-4xl">
          <h3 class="flex items-center gap-1.5 text-[10px] font-bold uppercase tracking-wider text-textMuted">
            <AlertTriangle size={11} class="text-amber-300" />
            Hygiene gaps ({hygieneGaps.length})
          </h3>
          {#each hygieneGaps as artifact (artifact.path)}
            <div class="px-3 py-2 rounded-xl border border-amber-500/30 bg-amber-500/5">
              <div class="flex items-center gap-2">
                <span class="font-mono text-textPrimary truncate">{artifact.path}</span>
                <span class="text-[10px] uppercase px-1.5 py-0.5 rounded-full bg-surfaceHover text-textMuted">{artifact.kind}</span>
                {#if artifact.unignored}
                  <span class="text-[10px] uppercase font-semibold text-amber-300">not ignored</span>
                {/if}
                {#if artifact.tracked_files > 0}
                  <span class="text-[10px] uppercase font-semibold text-rose-300">
                    {artifact.tracked_files} file{artifact.tracked_files === 1 ? "" : "s"} committed
                  </span>
                {/if}
                <span class="ml-auto font-mono text-textMuted">{humanBytes(artifact.bytes)}</span>
              </div>
              {#if artifact.unignored}
                <p class="mt-1 text-[11px] text-textMuted">
                  No ignore rule covers this directory — it shows up in every status listing and can slip into commits.
                </p>
              {:else}
                <p class="mt-1 text-[11px] text-textMuted">
                  Ignored today, but committed copies stay in history forever. Removing them needs a history rewrite.
                </p>
              {/if}
            </div>
          {/each}
        </section>
      {/if}

      <!-- Git internals -->
      <section class="space-y-2 max-w-4xl">
        <h3 class="flex items-center gap-1.5 text-[10px] font-bold uppercase tracking-wider text-textMuted">
          <FolderTree size={11} />
          Git internals · {humanBytes(report.git.total_bytes)}
        </h3>
        <div class="border border-border/70 rounded-2xl overflow-hidden max-w-4xl shadow-card">
          <table class="w-full text-left">
            <tbody>
              <tr class="border-b border-border/40">
                <td class="px-3 py-1.5 text-textPrimary">Packfiles</td>
                <td class="px-3 py-1.5 font-mono text-textSecondary">{humanBytes(report.git.pack_bytes)}</td>
                <td class="px-3 py-1.5 font-mono text-textMuted">{report.git.pack_file_count} pack(s)</td>
              </tr>
              <tr class="border-b border-border/40">
                <td class="px-3 py-1.5 text-textPrimary">
                  Loose objects
                  {#if report.git.gc_recommended}
                    <span class="ml-1 text-[10px] uppercase font-semibold text-amber-300">git gc advised</span>
                  {/if}
                </td>
                <td class="px-3 py-1.5 font-mono text-textSecondary">{humanBytes(report.git.loose_bytes)}</td>
                <td class="px-3 py-1.5 font-mono text-textMuted">{report.git.loose_object_count} object(s)</td>
              </tr>
              <tr class="border-b border-border/40">
                <td class="px-3 py-1.5 text-textPrimary">Reflogs</td>
                <td class="px-3 py-1.5 font-mono text-textSecondary">{humanBytes(report.git.reflog_bytes)}</td>
                <td class="px-3 py-1.5 font-mono text-textMuted">grows until expired</td>
              </tr>
              {#if report.git.lfs_bytes > 0}
                <tr class="border-b border-border/40">
                  <td class="px-3 py-1.5 text-textPrimary">Git LFS</td>
                  <td class="px-3 py-1.5 font-mono text-textSecondary">{humanBytes(report.git.lfs_bytes)}</td>
                  <td></td>
                </tr>
              {/if}
              {#if report.git.modules_bytes > 0}
                <tr class="border-b border-border/40">
                  <td class="px-3 py-1.5 text-textPrimary">Submodules (.git/modules)</td>
                  <td class="px-3 py-1.5 font-mono text-textSecondary">{humanBytes(report.git.modules_bytes)}</td>
                  <td></td>
                </tr>
              {/if}
              {#if report.git.worktrees_admin_bytes > 0}
                <tr class="border-b border-border/40">
                  <td class="px-3 py-1.5 text-textPrimary">Linked-worktree admin</td>
                  <td class="px-3 py-1.5 font-mono text-textSecondary">{humanBytes(report.git.worktrees_admin_bytes)}</td>
                  <td></td>
                </tr>
              {/if}
              <tr class="border-b border-border/40">
                <td class="px-3 py-1.5 text-textPrimary">Index</td>
                <td class="px-3 py-1.5 font-mono text-textSecondary">{humanBytes(report.git.index_bytes)}</td>
                <td></td>
              </tr>
              <tr>
                <td class="px-3 py-1.5 text-textPrimary">Other (.git)</td>
                <td class="px-3 py-1.5 font-mono text-textSecondary">{humanBytes(report.git.other_bytes)}</td>
                <td class="px-3 py-1.5 font-mono text-textMuted">hooks, info, temp objects</td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>

      <!-- Build & cache dirs -->
      {#if report.artifacts.length > 0}
        <section class="space-y-2 max-w-4xl">
          <h3 class="flex items-center gap-1.5 text-[10px] font-bold uppercase tracking-wider text-textMuted">
            <Boxes size={11} />
            Build &amp; cache directories ({report.artifacts.length})
          </h3>
          <div class="space-y-1">
            {#each report.artifacts as artifact (artifact.path)}
              <div class="flex items-center gap-2 px-3 py-1.5 rounded-xl border border-border/50 bg-surface/60">
                <span class="font-mono text-textPrimary truncate max-w-[40%]" title={artifact.path}>{artifact.path}</span>
                <div class="flex-1 h-1.5 rounded-full bg-surfaceHover overflow-hidden min-w-16">
                  <div
                    class={artifact.kind === "build" ? "bg-amber-400" : "bg-sky-400"}
                    style="width: {artifactBar(artifact.bytes).toFixed(1)}%"
                  ></div>
                </div>
                <span class="font-mono text-textSecondary w-20 text-right">{humanBytes(artifact.bytes)}</span>
                {#if artifact.unignored}<span title="Not covered by .gitignore"><AlertTriangle size={12} class="text-amber-300" /></span>{/if}
                {#if artifact.tracked_files > 0}
                  <span class="text-[10px] font-mono text-rose-300" title="Committed files inside an artifact directory">
                    {artifact.tracked_files}✓
                  </span>
                {/if}
              </div>
            {/each}
          </div>
        </section>
      {/if}

      <!-- Largest files -->
      {#if report.largest_files.length > 0}
        <section class="space-y-2 max-w-4xl">
          <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">
            Largest working-tree files ({report.largest_files.length}, ≥10 MB)
          </h3>
          <div class="border border-border/70 rounded-2xl overflow-hidden max-w-4xl shadow-card">
            <table class="w-full text-left">
              <tbody>
                {#each report.largest_files as file (file.path)}
                  <tr class="border-b border-border/40 last:border-b-0">
                    <td class="px-3 py-1.5 font-mono text-textPrimary truncate max-w-md" title={file.path}>{file.path}</td>
                    <td class="px-3 py-1.5 font-mono text-textSecondary text-right whitespace-nowrap">{humanBytes(file.bytes)}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        </section>
      {/if}

      <!-- Linked worktrees -->
      {#if report.worktrees.length > 0}
        <section class="space-y-2 max-w-4xl">
          <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">
            Linked worktrees ({report.worktrees.length})
          </h3>
          <div class="border border-border/70 rounded-2xl overflow-hidden max-w-4xl shadow-card">
            <table class="w-full text-left">
              <tbody>
                {#each report.worktrees as worktree (worktree.path)}
                  <tr class="border-b border-border/40 last:border-b-0">
                    <td class="px-3 py-1.5 font-mono text-textPrimary truncate" title={worktree.path}>{worktree.name}</td>
                    <td class="px-3 py-1.5 text-textSecondary">{worktree.branch ?? "detached"}</td>
                    <td class="px-3 py-1.5 font-mono text-textSecondary text-right whitespace-nowrap">
                      {humanBytes(worktree.bytes)}{worktree.truncated ? "+" : ""}
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        </section>
      {/if}

      <!-- Branch weight -->
      <section class="space-y-2 max-w-4xl">
        <h3 class="flex items-center gap-1.5 text-[10px] font-bold uppercase tracking-wider text-textMuted">
          <GitBranch size={11} />
          Branch weight
        </h3>
        {#if report.branches.error}
          <div class="px-3 py-2 rounded-xl border border-amber-500/30 bg-amber-500/5 text-amber-200">
            Could not summarize branches: {report.branches.error}
          </div>
        {:else}
          <div class="px-3 py-2 rounded-xl border border-border/70 bg-surface/60 flex flex-wrap items-center gap-x-4 gap-y-1">
            <span><span class="font-semibold text-textPrimary">{report.branches.local_count}</span> <span class="text-textMuted">local</span></span>
            <span><span class="font-semibold text-textPrimary">{report.branches.remote_tracking_count}</span> <span class="text-textMuted">remote-tracking</span></span>
            <span><span class="font-semibold text-amber-300">{report.branches.merged_stale_count}</span> <span class="text-textMuted">merged-stale</span></span>
            <span><span class="font-semibold text-rose-300">{report.branches.gone_upstream_count}</span> <span class="text-textMuted">upstream gone</span></span>
            {#if report.branches.merged_stale_count > 0}
              <button type="button" onclick={openManviCleanup} class="gp-btn !py-0.5 ml-auto" title="Review the conservative cleanup plan in the MANVI view">
                <Sparkles size={11} />
                Clean up in MANVI
              </button>
            {/if}
          </div>
          {#if report.branches.sample_merged_stale.length > 0}
            <p class="text-[11px] text-textMuted font-mono truncate">
              merged-stale: {report.branches.sample_merged_stale.join(", ")}
            </p>
          {/if}
        {/if}
      </section>

      <!-- Usage history -->
      <section class="space-y-2 max-w-4xl pb-4">
        <div class="flex items-center justify-between">
          <h3 class="flex items-center gap-1.5 text-[10px] font-bold uppercase tracking-wider text-textMuted">
            <Clock size={11} />
            Usage history
            {#if series.length > 0}
              <span class="normal-case font-normal text-textMuted">· {series.length} snapshot{series.length === 1 ? "" : "s"}, since {formatAge(series[0].t)}</span>
            {/if}
          </h3>
          {#if series.length > 0}
            <button type="button" onclick={clearHistory} class="gp-btn !py-0.5" title="Forget this repository's stored snapshots">
              <Trash2 size={11} />
              Clear
            </button>
          {/if}
        </div>
        {#if series.length < 2}
          <p class="text-textMuted">
            {series.length === 0
              ? "No snapshots yet. Every completed scan records one here."
              : "One snapshot so far — rescan later (≥15 minutes apart) to start the trend."}
          </p>
        {:else}
          <div class="rounded-2xl border border-border/70 bg-surface shadow-card p-3 space-y-2">
            <svg viewBox="0 0 100 28" preserveAspectRatio="none" class="w-full h-14" role="img" aria-label="Total size over time">
              <polyline
                points={sparkPoints}
                fill="none"
                stroke="var(--accent, #8b5cf6)"
                stroke-width="1.2"
                vector-effect="non-scaling-stroke"
              />
            </svg>
            <div class="flex flex-wrap items-center gap-x-4 gap-y-1 text-[11px]">
              <span class="text-textMuted">
                {formatDelta(lastDelta?.bytes ?? 0)}
                <span class="opacity-70">since {formatAge(series[series.length - 2].t, series[series.length - 1].t) === "just now" ? "previous scan" : `previous scan (${formatSnapshotTime(series[series.length - 2].t)})`}</span>
              </span>
              {#if weekDelta}
                <span class={deltaClass(weekDelta.bytes)}>
                  {formatDelta(weekDelta.bytes)} this week
                </span>
              {/if}
              <span class="ml-auto font-mono text-textMuted">
                now {humanBytes(series[series.length - 1].grand)}
              </span>
            </div>
          </div>
        {/if}
        {#if report.scan.permission_denied > 0 || report.scan.truncated}
          <p class="text-[11px] text-amber-300">
            {report.scan.permission_denied > 0 ? `${report.scan.permission_denied} location(s) unreadable. ` : ""}
            {report.scan.truncated ? "Scan hit a safety budget; totals are floors." : ""}
          </p>
        {:else}
          <p class="text-[11px] text-textMuted">
            {report.scan.files_visited.toLocaleString()} files visited in {report.scan.elapsed_ms} ms.
          </p>
        {/if}
      </section>
    {:else}
      <div class="text-textMuted">Open a repository to measure its disk usage.</div>
    {/if}
  </div>
</div>
