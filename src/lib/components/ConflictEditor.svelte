<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { invoke } from "@tauri-apps/api/core";
  import { harnessStore } from "../stores/harnessStore";
  import { Check, ShieldAlert, AlertTriangle } from "lucide-svelte";
  import EmptyState from "./EmptyState.svelte";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { planConflictSave } from "../diff/conflictSave";

  /**
   * Mirrors the Rust `ConflictResolutionChoice` enum's serde form: unit
   * variants travel as plain strings, the tuple variant as an externally
   * tagged map.
   */
  type ConflictResolution = "Unresolved" | "AcceptOurs" | "AcceptTheirs" | "AcceptBothOursFirst" | "AcceptBothTheirsFirst" | { Custom: string };

  interface ConflictChunk {
    chunk_index: number;
    start_line: number;
    end_line: number;
    ours_label: string;
    ours_content: string;
    base_content?: string;
    theirs_label: string;
    theirs_content: string;
    resolution: ConflictResolution;
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
  let previewError = $state<string | null>(null);
  let resolvedPreview = $state<string>("");
  let isSaving = $state(false);
  let saveError = $state<string | null>(null);
  let customDrafts = $state<Record<number, string>>({});
  let loadGuard: AsyncGuard | null = null;
  let previewGuard: AsyncGuard | null = null;
  let saveGuard: AsyncGuard | null = null;
  const customTimers = new Map<number, ReturnType<typeof setTimeout>>();

  function customText(chunk: ConflictChunk): string {
    if (typeof chunk.resolution === "object") return chunk.resolution.Custom;
    return customDrafts[chunk.chunk_index] ?? "";
  }

  $effect(() => {
    if (conflictedFiles.length > 0) {
      if (!selectedFile || !conflictedFiles.some((f) => f.path === selectedFile)) {
        selectedFile = conflictedFiles[0].path;
      }
    } else {
      selectedFile = null;
    }
  });

  $effect(() => {
    return () => {
      loadGuard?.cancel();
      previewGuard?.cancel();
      saveGuard?.cancel();
      for (const timer of customTimers.values()) clearTimeout(timer);
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
      previewError = null;
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

  function setChunkResolution(chunkIndex: number, choice: ConflictResolution) {
    if (!parsedDoc) return;
    for (const seg of parsedDoc.segments) {
      if (seg.Conflict && seg.Conflict.chunk_index === chunkIndex) {
        seg.Conflict.resolution = choice;
      }
    }
    void updatePreview(parsedDoc);
  }

  function resolveAll(choice: ConflictResolution) {
    if (!parsedDoc) return;
    for (const seg of parsedDoc.segments) {
      if (seg.Conflict) {
        seg.Conflict.resolution = choice;
      }
    }
    void updatePreview(parsedDoc);
  }

  function onCustomInput(chunkIndex: number, value: string) {
    customDrafts[chunkIndex] = value;
    const existing = customTimers.get(chunkIndex);
    if (existing) clearTimeout(existing);
    // Typing must not storm the IPC with a resolve per keystroke; settle first.
    customTimers.set(
      chunkIndex,
      setTimeout(() => {
        customTimers.delete(chunkIndex);
        setChunkResolution(chunkIndex, value.trim() === "" ? "Unresolved" : { Custom: value });
      }, 250)
    );
  }

  async function updatePreview(doc: ConflictDoc) {
    const repo = $repoStore.currentPath;
    const file = selectedFile;
    previewGuard?.cancel();
    const guard = createAsyncGuard();
    previewGuard = guard;
    try {
      const res = await invoke<string>("cmd_preview_conflict", { document: doc });
      if (!guard.isLive()) return;
      if ($repoStore.currentPath !== repo || selectedFile !== file || selectedFile !== doc.file_path) return;
      previewError = null;
      resolvedPreview = res;
    } catch (err) {
      if (!guard.isLive()) return;
      if ($repoStore.currentPath !== repo || selectedFile !== file) return;
      // A failed render means the preview no longer matches the chosen
      // resolutions: clear it (which also disables Save) and say why.
      resolvedPreview = "";
      previewError = String(err);
    }
  }

  let hasUnresolved = $derived(
    parsedDoc?.segments.some((s) => s.Conflict && s.Conflict.resolution === "Unresolved") ?? true
  );

  async function saveResolved() {
    const repo = $repoStore.currentPath;
    const file = selectedFile;
    if (!file || !repo || !parsedDoc || hasUnresolved) return;
    saveGuard?.cancel();
    const guard = createAsyncGuard();
    saveGuard = guard;
    isSaving = true;
    saveError = null;
    try {
      const content = await invoke<string>("cmd_resolve_conflict", { document: parsedDoc });
      if (!guard.isLive() || $repoStore.currentPath !== repo || selectedFile !== file) return;
      await invoke("cmd_write_file_content", {
        repoPath: repo,
        filePath: file,
        content,
      });
      if (!guard.isLive() || $repoStore.currentPath !== repo || selectedFile !== file) return;
      harnessStore.recordAction({ kind: "edit", label: file, ok: true });
      const stageOutcome = await repoStore.stageFile(file);
      if (!guard.isLive() || $repoStore.currentPath !== repo || selectedFile !== file) return;
      const plan = planConflictSave(true, stageOutcome.ok, stageOutcome.error);
      if (!plan.complete) saveError = plan.message;
      await repoStore.refresh();
    } catch (err) {
      if (!guard.isLive() || $repoStore.currentPath !== repo || selectedFile !== file) return;
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
        {#if loadError}
          <!-- The parse failed: claiming "0 Conflict(s)" would be a lie. -->
          <span class="text-[10px] bg-rose-500/20 text-rose-400 px-2 py-0.5 rounded-full font-mono">
            unavailable
          </span>
        {:else}
          <span class="text-[10px] bg-rose-500/20 text-rose-400 px-2 py-0.5 rounded-full font-mono">
            {parsedDoc?.total_conflicts || 0} Conflict(s)
          </span>
        {/if}
        {#if parsedDoc && parsedDoc.total_conflicts > 0}
          <div class="flex items-center gap-1.5 ml-2">
            <button
              onclick={() => resolveAll("AcceptOurs")}
              class="px-2 py-0.5 rounded-md bg-blue-500/20 hover:bg-blue-500/30 text-blue-300 text-[11px] font-sans transition-colors"
            >
              Accept All Current (Ours)
            </button>
            <button
              onclick={() => resolveAll("AcceptTheirs")}
              class="px-2 py-0.5 rounded-md bg-purple-500/20 hover:bg-purple-500/30 text-purple-300 text-[11px] font-sans transition-colors"
            >
              Accept All Incoming (Theirs)
            </button>
          </div>
        {/if}
      </div>

      <button
        onclick={saveResolved}
        disabled={hasUnresolved || isSaving || !resolvedPreview}
        class="gp-btn-success !py-1.5"
        title={hasUnresolved ? "Resolve all conflicts before saving" : "Save and stage resolved file"}
      >
        <Check size={14} />
        <span>{hasUnresolved ? "Unresolved Conflicts" : "Save & Stage Resolution"}</span>
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
                    Both (Ours First)
                  </button>
                </span>
              </div>
              <pre class="mt-2 text-xs font-mono whitespace-pre text-blue-200 overflow-auto">{chunk.ours_content}</pre>
            </div>

            <!-- Theirs (Right) -->
            <div class="p-3 rounded-xl bg-purple-500/5 ring-1 ring-purple-500/15 flex flex-col min-h-0">
              <div class="flex items-center justify-between pb-2 border-b border-purple-500/20 text-purple-400 font-medium gap-2 flex-wrap">
                <span class="truncate">Theirs ({chunk.theirs_label || "Incoming"})</span>
                <span class="flex items-center gap-1.5 shrink-0">
                  <button
                    onclick={() => setChunkResolution(chunk.chunk_index, "AcceptTheirs")}
                    class="px-2.5 py-1 bg-purple-500/20 hover:bg-purple-500/30 rounded-full text-[11px] text-purple-300 transition-colors"
                  >
                    Accept Theirs
                  </button>
                  <button
                    onclick={() => setChunkResolution(chunk.chunk_index, "AcceptBothTheirsFirst")}
                    class="px-2.5 py-1 bg-surfaceHover hover:bg-surface rounded-full text-[11px] text-textPrimary transition-colors"
                  >
                    Both (Theirs First)
                  </button>
                </span>
              </div>
              <pre class="mt-2 text-xs font-mono whitespace-pre text-purple-200 overflow-auto">{chunk.theirs_content}</pre>
            </div>

            <!-- Custom resolution for this chunk -->
            <div class="col-span-2 rounded-xl bg-surfaceHover/40 ring-1 ring-border/70 px-3 py-2">
              <div class="text-[10px] font-semibold uppercase tracking-wide text-textMuted mb-1">Custom resolution</div>
              <textarea
                rows={3}
                placeholder="Type the exact content this conflict should resolve to…"
                class="w-full resize-y bg-background border border-border/80 rounded-lg p-2 text-xs font-mono text-textPrimary focus:outline-none focus:border-accent/60"
                value={customText(chunk)}
                oninput={(event) => onCustomInput(chunk.chunk_index, (event.target as HTMLTextAreaElement).value)}
              ></textarea>
            </div>
          {:else if seg.Normal !== undefined}
            <!-- Uncontested context between conflicts: visible, never editable here -->
            <div class="col-span-2 rounded-xl bg-surfaceHover/30 ring-1 ring-border/50 px-3 py-2 select-text">
              <div class="text-[10px] font-semibold uppercase tracking-wide text-textMuted mb-1">Context</div>
              <pre class="text-xs font-mono whitespace-pre-wrap text-textPrimary/70">{seg.Normal}</pre>
            </div>
          {/if}
        {/each}
      {/if}
    </div>

    <!-- Resolved Output Preview -->
    <div class="h-40 border-t border-border/60 bg-surface p-3 flex flex-col">
      <span class="text-[10px] font-semibold uppercase text-textMuted mb-1">Resolved Preview</span>
      {#if previewError}
        <div class="mb-1 px-2 py-1 rounded-lg bg-rose-500/10 border border-rose-500/30 text-rose-300 text-[11px] flex items-center gap-1.5">
          <AlertTriangle size={11} class="shrink-0" />
          <span class="truncate">Preview failed: {previewError}</span>
        </div>
      {/if}
      <pre class="flex-1 bg-background border border-border/70 rounded-xl p-2.5 font-mono text-xs overflow-auto text-textPrimary">{resolvedPreview || "<Select resolutions to preview>"}</pre>
    </div>
  {/if}
</div>
