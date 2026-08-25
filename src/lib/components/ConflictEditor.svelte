<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { invoke } from "@tauri-apps/api/core";
  import { harnessStore } from "../stores/harnessStore";
  import { Check, ShieldAlert, AlertTriangle } from "lucide-svelte";
  import EmptyState from "./EmptyState.svelte";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";

  interface ConflictChunk {
    chunk_index: number;
    start_line: number;
    end_line: number;
    ours_label: string;
    ours_content: string;
    base_content?: string;
    theirs_label: string;
    theirs_content: string;
    resolution: "Unresolved" | "AcceptOurs" | "AcceptTheirs" | "AcceptBothOursFirst" | "AcceptBothTheirsFirst";
  }

  interface ConflictDoc {
    file_path: string;
    segments: Array<{ Normal?: string; Conflict?: ConflictChunk }>;
    total_conflicts: number;
  }

  let conflictedFiles = $derived($repoStore.statuses.filter((s) => s.is_conflicted));
  let selectedFile = $state<string | null>(null);
  let parsedDoc = $state<ConflictDoc | null>(null);
  let loadError = $state<string | null>(null);
  let resolvedPreview = $state<string>("");
  let isSaving = $state(false);
  let saveError = $state<string | null>(null);
  let loadGuard: AsyncGuard | null = null;
  let previewGuard: AsyncGuard | null = null;
  let saveGuard: AsyncGuard | null = null;

  $effect(() => {
    if (conflictedFiles.length > 0 && !selectedFile) {
      selectedFile = conflictedFiles[0].path;
    }
  });

  $effect(() => {
    return () => {
      loadGuard?.cancel();
      previewGuard?.cancel();
      saveGuard?.cancel();
    };
  });

  $effect(() => {
    const repo = $repoStore.currentPath;
    const file = selectedFile;
    if (!file || !repo) {
      loadGuard?.cancel();
      previewGuard?.cancel();
      saveGuard?.cancel();
      parsedDoc = null;
      resolvedPreview = "";
      isSaving = false;
      saveError = null;
      loadError = null;
      return;
    }
    const guard = createAsyncGuard();
    loadGuard = guard;
    void (async () => {
      try {
        const content = await invoke<string>("cmd_get_file_content", {
          repoPath: repo,
          filePath: file,
          commitId: null,
        });
        if (!guard.isLive()) return;
        const doc = await invoke<ConflictDoc>("cmd_parse_conflict", {
          filePath: file,
          content,
        });
        if (!guard.isLive()) return;
        loadError = null;
        parsedDoc = doc;
        await updatePreview(doc);
      } catch (err) {
        if (!guard.isLive()) return;
        parsedDoc = null;
        resolvedPreview = "";
        loadError = String(err);
        console.error("Failed to load conflict:", err);
      }
    })();
    return () => {
      guard.cancel();
    };
  });

  function setChunkResolution(chunkIndex: number, choice: "AcceptOurs" | "AcceptTheirs" | "AcceptBothOursFirst") {
    if (!parsedDoc) return;
    for (const seg of parsedDoc.segments) {
      if (seg.Conflict && seg.Conflict.chunk_index === chunkIndex) {
        seg.Conflict.resolution = choice;
      }
    }
    void updatePreview(parsedDoc);
  }

  async function updatePreview(doc: ConflictDoc) {
    const repo = $repoStore.currentPath;
    const file = selectedFile;
    previewGuard?.cancel();
    const guard = createAsyncGuard();
    previewGuard = guard;
    try {
      const res = await invoke<string>("cmd_resolve_conflict", { document: doc });
      if (!guard.isLive()) return;
      if ($repoStore.currentPath !== repo || selectedFile !== file || selectedFile !== doc.file_path) return;
      resolvedPreview = res;
    } catch {
      if (!guard.isLive()) return;
      if ($repoStore.currentPath !== repo || selectedFile !== file) return;
      resolvedPreview = "";
    }
  }

  async function saveResolved() {
    const repo = $repoStore.currentPath;
    const file = selectedFile;
    const content = resolvedPreview;
    if (!file || !repo || !content) return;
    saveGuard?.cancel();
    const guard = createAsyncGuard();
    saveGuard = guard;
    isSaving = true;
    saveError = null;
    try {
      await invoke("cmd_write_file_content", {
        repoPath: repo,
        filePath: file,
        content,
      });
      if (!guard.isLive() || $repoStore.currentPath !== repo || selectedFile !== file) return;
      // The write bypasses runMutating, so it journals itself: a conflict save
      // is an edit an agent (or a future you) needs to find in the journal.
      harnessStore.recordAction({ kind: "edit", label: file, ok: true });
      await repoStore.stageFile(file);
      if (!guard.isLive() || $repoStore.currentPath !== repo || selectedFile !== file) return;
      await repoStore.refresh();
    } catch (err) {
      if (!guard.isLive()) return;
      harnessStore.recordAction({ kind: "edit", label: file, ok: false });
      saveError = String(err);
    } finally {
      if (guard.isLive()) isSaving = false;
    }
  }
</script>

<div class="flex-1 flex flex-col bg-background h-full text-xs font-sans select-none overflow-hidden">
  {#if conflictedFiles.length === 0}
    <EmptyState
      icon={Check}
      title="No merge conflicts"
      hint="Your working tree is clean and ready."
    />
  {:else}
    <!-- Top Selector Bar -->
    <div class="px-4 py-2 border-b border-border/60 bg-surface/60 flex items-center justify-between">
      <div class="flex items-center gap-3">
        <ShieldAlert size={16} class="text-amber-400" />
        <select
          bind:value={selectedFile}
          class="bg-background border border-border/80 rounded-full px-3 py-1 text-xs text-textPrimary focus:outline-none focus:border-accent/60 font-mono transition-colors"
        >
          {#each conflictedFiles as f}
            <option value={f.path}>{f.path}</option>
          {/each}
        </select>
        <span class="text-[10px] bg-rose-500/20 text-rose-400 px-2 py-0.5 rounded-full font-mono">
          {parsedDoc?.total_conflicts || 0} Conflict(s)
        </span>
      </div>

      <button
        onclick={saveResolved}
        disabled={!resolvedPreview || isSaving}
        class="gp-btn-success !py-1.5"
      >
        <Check size={14} />
        <span>Save &amp; Stage Resolution</span>
      </button>
    </div>

    {#if saveError}
      <div class="mx-3 mt-2 px-3 py-1.5 rounded-xl bg-rose-500/10 border border-rose-500/30 text-rose-300 text-[11px] flex items-center gap-2">
        <AlertTriangle size={12} class="shrink-0" />
        <span class="truncate">Save failed: {saveError}</span>
      </div>
    {/if}

    {#if loadError}
      <div class="mx-3 mt-2 px-3 py-1.5 rounded-xl bg-rose-500/10 border border-rose-500/30 text-rose-300 text-[11px] flex items-center gap-2">
        <AlertTriangle size={12} class="shrink-0" />
        <span class="truncate">Failed to load conflict: {loadError}</span>
      </div>
    {/if}

    <!-- Conflict Chunks Inspector -->
    <div class="flex-1 grid grid-cols-2 divide-x divide-border/60 min-h-0 overflow-auto p-2 gap-2">
      {#if parsedDoc}
        {#each parsedDoc.segments as seg}
          {#if seg.Conflict}
            {@const chunk = seg.Conflict}
            <!-- Ours (Left) -->
            <div class="p-3 rounded-xl bg-blue-500/5 ring-1 ring-blue-500/15 flex flex-col min-h-0">
              <div class="flex items-center justify-between pb-2 border-b border-blue-500/20 text-blue-400 font-medium gap-2 flex-wrap">
                <span class="truncate">Ours ({chunk.ours_label || "HEAD"})</span>
                <span class="flex items-center gap-1.5 shrink-0">
                  <button
                    onclick={() => setChunkResolution(chunk.chunk_index, "AcceptOurs")}
                    class="px-2.5 py-1 bg-blue-500/20 hover:bg-blue-500/30 rounded-full text-[11px] text-blue-300 transition-colors"
                  >
                    Accept Ours
                  </button>
                  <button
                    onclick={() => setChunkResolution(chunk.chunk_index, "AcceptBothOursFirst")}
                    class="px-2.5 py-1 bg-surfaceHover hover:bg-surface rounded-full text-[11px] text-textPrimary transition-colors"
                  >
                    Accept Both
                  </button>
                </span>
              </div>
              <pre class="mt-2 text-xs font-mono whitespace-pre text-blue-200 overflow-auto">{chunk.ours_content}</pre>
            </div>

            <!-- Theirs (Right) -->
            <div class="p-3 rounded-xl bg-purple-500/5 ring-1 ring-purple-500/15 flex flex-col min-h-0">
              <div class="flex items-center justify-between pb-2 border-b border-purple-500/20 text-purple-400 font-medium gap-2 flex-wrap">
                <span class="truncate">Theirs ({chunk.theirs_label || "Incoming"})</span>
                <button
                  onclick={() => setChunkResolution(chunk.chunk_index, "AcceptTheirs")}
                  class="px-2.5 py-1 bg-purple-500/20 hover:bg-purple-500/30 rounded-full text-[11px] text-purple-300 shrink-0 transition-colors"
                >
                  Accept Theirs
                </button>
              </div>
              <pre class="mt-2 text-xs font-mono whitespace-pre text-purple-200 overflow-auto">{chunk.theirs_content}</pre>
            </div>
          {/if}
        {/each}
      {/if}
    </div>

    <!-- Resolved Output Preview -->
    <div class="h-40 border-t border-border/60 bg-surface p-3 flex flex-col">
      <span class="text-[10px] font-semibold uppercase text-textMuted mb-1">Resolved Preview</span>
      <pre class="flex-1 bg-background border border-border/70 rounded-xl p-2.5 font-mono text-xs overflow-auto text-textPrimary">{resolvedPreview || "<Select resolutions to preview>"}</pre>
    </div>
  {/if}
</div>
