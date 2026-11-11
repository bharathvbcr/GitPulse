<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import {
    PanelLeftClose,
    PanelLeftOpen,
    ExternalLink,
    Copy,
    Check,
    RotateCw,
    Send,
    FileText,
    AlertCircle,
  } from "@lucide/svelte";
  import { invoke } from "../ipc/invoke";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { copyText } from "../desktop/clipboard";
  import { formatError } from "../ui/formatError";
  import { askConfirm } from "../stores/modalStore";
  import type { FileBlob, SavedFile } from "../files/types";
  import type { Guarded } from "../stores/harnessStore";
  import { recordEditorDrafts } from "../files/editorDraftRegistry";
  import { editorFileSaveQueue, fileSaveKey } from "../files/serialSave";
  import { readFileWriteError, saveBase, UnbasedDraft } from "../files/fileWrite";
  import { overwriteNote } from "../files/overwriteNote";
  import { CONTEXT_UNKNOWN, type ContextRead } from "../terminal/sessionContext";
  import ExplorerRail from "./files/ExplorerRail.svelte";
  import MediaViewer from "./files/MediaViewer.svelte";
  import EditorConflictBanner from "./files/EditorConflictBanner.svelte";
  import LanguageLogo from "./LanguageLogo.svelte";

  interface PaneDraft {
    content: string;
    sourceContent: string;
    base: string | null;
  }

  let {
    tabId,
    repoPath,
    filePath = undefined,
    fallbackPath = undefined,
    snippet = null,
    revealLine = null,
    revealColumn = null,
    revealSeq = 0,
    onscreen = false,
    onSelectFile,
    onResolvedPath,
    onOpenInCodeView,
    onSendToAgent,
  }: {
    tabId: string;
    repoPath: string;
    filePath?: string;
    fallbackPath?: string;
    snippet?: { title: string; content: string } | null;
    revealLine?: number | null;
    revealColumn?: number | null;
    revealSeq?: number;
    onscreen?: boolean;
    onSelectFile?: (path: string, line?: number | null, column?: number | null) => void;
    onResolvedPath?: (resolvedPath: string) => void;
    onOpenInCodeView?: (path: string, line: number | null) => void;
    onSendToAgent?: (reference: string) => void;
  } = $props();

  const draftOwner = $derived(`terminal-file:${tabId}`);

  // Start with the explorer open when opened without a target path or snippet, or compact when opened from a link click / snippet.
  let explorerOpen = $state(untrack(() => !filePath && !snippet));
  let activeBlob = $state<FileBlob | null>(null);
  let isLoading = $state(false);
  let loadError = $state<string | null>(null);
  let copiedPath = $state(false);
  let copiedTimer: ReturnType<typeof setTimeout> | null = null;
  let inflight: AsyncGuard | null = null;
  let loadedKey = "";

  let drafts = $state<Record<string, PaneDraft>>({});
  let saveRefusal = $state<{ path: string; actual: string | null } | null>(null);
  let paneRoot: HTMLDivElement | undefined = $state();

  const currentDraft = $derived(filePath ? drafts[filePath] ?? null : null);
  const isDirty = $derived(Boolean(currentDraft));
  const activeConflict = $derived(
    filePath && currentDraft && saveRefusal?.path === filePath ? saveRefusal : null,
  );
  const revealTarget = $derived(
    revealSeq > 0 ? { line: revealLine, column: revealColumn, seq: revealSeq } : null,
  );
  const snippetBlob = $derived<FileBlob | null>(
    !filePath && snippet
      ? {
          path: snippet.title,
          is_binary: false,
          is_image: false,
          mime: "text/plain",
          text: snippet.content,
          invalid_utf8_bytes: 0,
          disk_base: null,
          eol: "lf",
        }
      : null,
  );

  function syncDraftRegistry(nextDrafts: Record<string, PaneDraft>) {
    if (!repoPath) return;
    recordEditorDrafts(repoPath, Object.keys(nextDrafts), draftOwner);
  }

  async function loadBlob(targetPath: string | undefined, force = false) {
    if (!repoPath || !targetPath) {
      inflight?.cancel();
      activeBlob = null;
      loadError = null;
      loadedKey = "";
      return;
    }
    const key = `${repoPath}\0${targetPath}`;
    if (!force && key === loadedKey && activeBlob?.path === targetPath) return;
    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    isLoading = true;
    loadError = null;
    try {
      const blob = await invoke<FileBlob>("cmd_get_file_blob", {
        repoPath,
        filePath: targetPath,
        commitId: null,
      });
      if (!guard.isLive()) return;
      activeBlob = blob;
      loadedKey = key;
    } catch (err: unknown) {
      if (!guard.isLive()) return;
      if (fallbackPath && fallbackPath !== targetPath) {
        try {
          const fallbackBlob = await invoke<FileBlob>("cmd_get_file_blob", {
            repoPath,
            filePath: fallbackPath,
            commitId: null,
          });
          if (!guard.isLive()) return;
          activeBlob = fallbackBlob;
          loadedKey = `${repoPath}\0${fallbackPath}`;
          onResolvedPath?.(fallbackPath);
          return;
        } catch {
          if (!guard.isLive()) return;
        }
      }
      loadError = formatError(err);
      activeBlob = null;
    } finally {
      if (guard.isLive()) isLoading = false;
    }
  }

  $effect(() => {
    const target = filePath;
    const repo = repoPath;
    void repo;
    untrack(() => {
      void loadBlob(target);
    });
  });

  function chooseFile(path: string, line: number | null = null, column: number | null = null) {
    if (!path) return;
    onSelectFile?.(path, line, column);
  }

  function handleDraftChange(newContent: string, sourceContent: string) {
    const path = filePath;
    if (!path) return;
    if (newContent === sourceContent) {
      const { [path]: _clean, ...rest } = drafts;
      drafts = rest;
      if (saveRefusal?.path === path) saveRefusal = null;
      syncDraftRegistry(rest);
      return;
    }
    const existing = drafts[path];
    const base = existing ? existing.base : (activeBlob?.path === path ? activeBlob.disk_base ?? null : null);
    if (!existing && saveRefusal?.path === path) saveRefusal = null;
    const next = {
      ...drafts,
      [path]: { content: newContent, sourceContent, base },
    };
    drafts = next;
    syncDraftRegistry(next);
  }

  async function saveFile(
    newContent: string,
    overrideBase?: string | null,
    overwrite = false,
  ): Promise<void> {
    const path = filePath;
    const repo = repoPath;
    if (!repo || !path) throw new Error("No active file to save");
    try {
      await editorFileSaveQueue.run(fileSaveKey(repo, path), async () => {
        const current = drafts[path];
        const judged = saveBase(
          current ? { base: current.base } : undefined,
          activeBlob?.path === path ? activeBlob.disk_base ?? null : null,
          overrideBase,
        );
        if (judged === "unbased") throw new UnbasedDraft(path);
        const saved = await invoke<Guarded<SavedFile>>("cmd_write_file_content", {
          repoPath: repo,
          filePath: path,
          content: newContent,
          expectedBase: judged.base,
          ...(overwrite ? { overwrite: true } : {}),
        });
        const latest = drafts[path];
        if (latest && latest.content !== newContent) {
          const updated = {
            ...drafts,
            [path]: {
              content: latest.content,
              sourceContent: newContent,
              base: saved.output.disk_base,
            },
          };
          drafts = updated;
          syncDraftRegistry(updated);
        } else {
          const { [path]: _saved, ...rest } = drafts;
          drafts = rest;
          syncDraftRegistry(rest);
        }
        if (activeBlob?.path === path) {
          activeBlob = {
            ...activeBlob,
            text: newContent,
            disk_base: saved.output.disk_base,
          };
        }
      });
      saveRefusal = null;
    } catch (err: unknown) {
      const typed = readFileWriteError(err);
      if (typed?.code === "changed_on_disk") {
        saveRefusal = { path, actual: typed.actual };
        void loadBlob(path, true);
      } else if (err instanceof UnbasedDraft) {
        saveRefusal = { path, actual: null };
      }
      throw err;
    }
  }

  async function requestDiscardCurrent(): Promise<boolean> {
    const path = filePath;
    if (!path || !drafts[path]) return true;
    await editorFileSaveQueue.whenIdle(fileSaveKey(repoPath, path));
    if (!drafts[path]) return true;
    const ok = await askConfirm({
      title: "Discard unsaved edits?",
      message: `Discard the unsaved editor draft for ${path}?`,
      confirmLabel: "Discard unsaved edits",
      cancelLabel: "Keep editing",
    });
    if (!ok) return false;
    const { [path]: _gone, ...rest } = drafts;
    drafts = rest;
    if (saveRefusal?.path === path) saveRefusal = null;
    syncDraftRegistry(rest);
    return true;
  }

  async function reloadFromDisk() {
    if (!filePath) return;
    if (!(await requestDiscardCurrent())) return;
    saveRefusal = null;
    activeBlob = null;
    await loadBlob(filePath, true);
  }

  async function overwriteCurrent() {
    const path = filePath;
    const draft = currentDraft;
    const conflict = activeConflict;
    if (!path || !draft || !conflict) return;
    if (
      conflict.actual === null &&
      !(await askConfirm({
        title: "Overwrite without a known version?",
        message: `${path} has no recorded disk version. Saving will replace whatever is on disk.`,
        confirmLabel: "Overwrite",
        cancelLabel: "Keep editing",
        destructive: true,
      }))
    ) {
      return;
    }
    try {
      await saveFile(draft.content, conflict.actual, true);
    } catch (err: unknown) {
      loadError = formatError(err);
    }
  }

  async function handleCopyPath() {
    const toCopy = filePath ?? snippet?.content;
    if (!toCopy) return;
    if (await copyText(toCopy)) {
      copiedPath = true;
      if (copiedTimer) clearTimeout(copiedTimer);
      copiedTimer = setTimeout(() => {
        copiedTimer = null;
        copiedPath = false;
      }, 1500);
    }
  }

  export async function confirmClose(): Promise<boolean> {
    const dirtyPaths = Object.keys(drafts);
    if (dirtyPaths.length === 0) return true;
    await editorFileSaveQueue.whenIdle(dirtyPaths.map((p) => fileSaveKey(repoPath, p)));
    const remaining = Object.keys(drafts);
    if (remaining.length === 0) return true;
    const summary = remaining.length <= 3 ? remaining.map((p) => `• ${p}`).join("\n") : `${remaining.length} files`;
    return askConfirm({
      title: "Discard unsaved edits?",
      message: `Closing this file split will discard unsaved drafts:\n${summary}`,
      confirmLabel: "Discard and close",
      cancelLabel: "Keep open",
      destructive: true,
    });
  }

  export async function close(): Promise<void> {
    inflight?.cancel();
    drafts = {};
    if (repoPath) recordEditorDrafts(repoPath, [], draftOwner);
  }

  export function reveal(): void {
    paneRoot?.querySelector<HTMLElement>("[role='textbox'], input, button")?.focus({ preventScroll: true });
  }

  export function readContext(): Promise<ContextRead> {
    return Promise.resolve(CONTEXT_UNKNOWN);
  }

  export function handleViewChord(_event: KeyboardEvent): boolean {
    return false;
  }

  onDestroy(() => {
    inflight?.cancel();
    if (copiedTimer) clearTimeout(copiedTimer);
    if (repoPath) recordEditorDrafts(repoPath, [], draftOwner);
  });
</script>

<div
  bind:this={paneRoot}
  class="h-full w-full flex flex-col min-h-0 min-w-0 bg-background text-xs font-sans overflow-hidden"
  data-terminal-file-pane={tabId}
>
  <div class="shrink-0 flex items-center justify-between gap-1.5 px-2 h-8 border-b border-border/60 gp-section-edge bg-surface/50 select-none">
    <div class="flex items-center gap-1.5 min-w-0">
      <button
        type="button"
        class="gp-icon-btn p-1! {explorerOpen ? 'text-accent bg-accent/15' : ''}"
        aria-label={explorerOpen ? "Hide file explorer" : "Show file explorer"}
        aria-pressed={explorerOpen}
        title={explorerOpen ? "Hide file tree" : "Browse repository files"}
        data-testid="terminal-file-pane-explorer-toggle"
        onclick={() => (explorerOpen = !explorerOpen)}
      >
        {#if explorerOpen}<PanelLeftClose size={13} />{:else}<PanelLeftOpen size={13} />{/if}
      </button>
      {#if filePath}
        <LanguageLogo filePath={filePath} size={13} />
        <span class="font-mono text-[11px] text-textPrimary truncate" title={filePath}>{filePath}</span>
        {#if isDirty}
          <span class="text-[10px] font-semibold text-amber-300 shrink-0" role="status">Unsaved</span>
        {/if}
      {:else if snippet}
        <FileText size={13} class="text-accent shrink-0" />
        <span class="font-mono text-[11px] text-textPrimary truncate" title={snippet.title}>{snippet.title}</span>
        <span class="text-[10px] text-textMuted shrink-0">Read-only</span>
      {:else}
        <FileText size={13} class="text-textMuted shrink-0" />
        <span class="text-[11px] text-textMuted truncate">Select a file</span>
      {/if}
    </div>

    <div class="flex items-center gap-1 shrink-0">
      {#if filePath && onSendToAgent}
        <button
          type="button"
          class="gp-btn py-0! px-1.5! text-[10px]! flex items-center gap-1"
          title="Insert @path into the active agent terminal"
          data-testid="terminal-file-send-path"
          onclick={() => filePath && onSendToAgent(`@${filePath} `)}
        >
          <Send size={10} />
          <span>@file</span>
        </button>
      {/if}
      {#if !filePath && snippet}
        <button
          type="button"
          class="gp-icon-btn p-1!"
          aria-label="Copy command output"
          title="Copy command output"
          onclick={() => void handleCopyPath()}
        >
          {#if copiedPath}<Check size={12} class="text-emerald-400" />{:else}<Copy size={12} />{/if}
        </button>
      {/if}
      {#if filePath}
        <button
          type="button"
          class="gp-icon-btn p-1!"
          aria-label="Copy relative path"
          title="Copy relative path"
          onclick={() => void handleCopyPath()}
        >
          {#if copiedPath}<Check size={12} class="text-emerald-400" />{:else}<Copy size={12} />{/if}
        </button>
        <button
          type="button"
          class="gp-icon-btn p-1!"
          aria-label="Reload file from disk"
          title="Reload file from disk"
          onclick={() => void reloadFromDisk()}
        >
          <RotateCw size={12} class={isLoading ? "animate-spin" : ""} />
        </button>
        {#if onOpenInCodeView}
          <button
            type="button"
            class="gp-icon-btn p-1!"
            aria-label="Open in Code view"
            title="Open in full Code view"
            data-testid="terminal-file-open-code-view"
            onclick={() => filePath && onOpenInCodeView(filePath, revealLine)}
          >
            <ExternalLink size={12} />
          </button>
        {/if}
      {/if}
    </div>
  </div>

  {#if activeConflict && filePath && currentDraft}
    <EditorConflictBanner
      path={filePath}
      disk={activeBlob?.path === filePath ? activeBlob.text ?? "" : ""}
      draft={currentDraft.content}
      unbased={activeConflict.actual === null}
      overwriteNote={activeBlob?.path === filePath ? overwriteNote(activeBlob) : null}
      onReload={() => void reloadFromDisk()}
      onOverwrite={() => void overwriteCurrent()}
    />
  {/if}

  {#if loadError}
    <div
      class="shrink-0 px-3 py-1.5 border-b border-rose-500/30 bg-rose-500/10 text-rose-300 text-[11px] flex items-start gap-1.5"
      role="alert"
      data-testid="terminal-file-pane-error"
    >
      <AlertCircle size={12} class="shrink-0 mt-0.5" />
      <span class="flex-1 wrap-break-word">{loadError}</span>
      <button type="button" class="text-xs hover:text-textPrimary" aria-label="Dismiss" onclick={() => (loadError = null)}>✕</button>
    </div>
  {/if}

  <div class="flex-1 min-h-0 flex overflow-hidden">
    {#if explorerOpen && onscreen}
      <div class="w-56 max-w-[45%] shrink-0 h-full border-r border-border/60 bg-surface/30 min-h-0 overflow-hidden">
        <ExplorerRail
          {repoPath}
          selectedFile={filePath ?? null}
          onSelectFile={(p) => chooseFile(p)}
          onPinFile={(p) => chooseFile(p)}
          onSelectMatch={(p, line, col) => chooseFile(p, line, col)}
        />
      </div>
    {/if}

    <div class="flex-1 min-w-0 min-h-0 h-full overflow-hidden">
      {#if !filePath && snippet && snippetBlob}
        <MediaViewer
          filePath={snippet.title}
          blob={snippetBlob}
          {repoPath}
          {revealTarget}
          onSendSelectionToAgent={onSendToAgent}
        />
      {:else if !filePath}
        <div class="h-full flex flex-col items-center justify-center gap-2 p-4 text-center text-textMuted text-xs">
          <FileText size={18} />
          <span>Choose a file from the explorer, or click any file path in a terminal session.</span>
        </div>
      {:else if isLoading && !activeBlob}
        <div class="h-full flex items-center justify-center text-textMuted text-xs">
          Loading {filePath}…
        </div>
      {:else if activeBlob}
        <MediaViewer
          {filePath}
          blob={activeBlob}
          {repoPath}
          draftContent={currentDraft?.content ?? null}
          dirty={isDirty}
          onSave={(content) => saveFile(content)}
          onDraftChange={handleDraftChange}
          onRequestDiscard={requestDiscardCurrent}
          {revealTarget}
          onSendSelectionToAgent={onSendToAgent}
        />
      {/if}
    </div>
  </div>
</div>
