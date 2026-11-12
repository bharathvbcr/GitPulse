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
  import type { DiffPayload } from "../repos/storeTypes";
  import { parseUnifiedDiff, type AnnotatedDiffLine } from "../diff/wordDiff";
  import type { Guarded } from "../stores/harnessStore";
  import { recordEditorDrafts } from "../files/editorDraftRegistry";
  import { editorFileSaveQueue, fileSaveKey } from "../files/serialSave";
  import { readFileWriteError, saveBase, UnbasedDraft } from "../files/fileWrite";
  import { overwriteNote } from "../files/overwriteNote";
  import { CONTEXT_UNKNOWN, type ContextRead } from "../terminal/sessionContext";
  import { getNeighbors, symbolsForFile } from "../codeintel/client";
  import type { CodeintelFileSymbol } from "../codeintel/types";
  import {
    extractDirectCallers,
    formatSymbolLensForAgent,
    pickEnclosingSymbol,
    type DirectCallerEntry,
  } from "../terminal/symbolQuickLens";
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
    initialViewMode = "code",
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
    initialViewMode?: "code" | "diff";
    onscreen?: boolean;
    onSelectFile?: (path: string, line?: number | null, column?: number | null) => void;
    onResolvedPath?: (resolvedPath: string) => void;
    onOpenInCodeView?: (path: string, line: number | null) => void;
    onSendToAgent?: (reference: string) => void;
  } = $props();

  const draftOwner = $derived(`terminal-file:${tabId}`);

  // Start with the explorer open when opened without a target path or snippet, or compact when opened from a link click / snippet.
  let explorerOpen = $state(untrack(() => !filePath && !snippet));
  let viewMode = $state<"code" | "diff">(untrack(() => initialViewMode));
  let activeBlob = $state<FileBlob | null>(null);
  let isLoading = $state(false);
  let loadError = $state<string | null>(null);
  let diffPayload = $state<DiffPayload | null>(null);
  let diffSource = $state<"unstaged" | "staged">("unstaged");
  let diffLoading = $state(false);
  let diffError = $state<string | null>(null);
  let diffInflight: AsyncGuard | null = null;
  let loadedDiffKey = "";
  let quickLensSymbol = $state<CodeintelFileSymbol | null>(null);
  let quickLensCallers = $state<DirectCallerEntry[]>([]);
  let quickLensTotalCallers = $state(0);
  let lensInflight: AsyncGuard | null = null;
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
  const diffLines = $derived<AnnotatedDiffLine[]>(
    diffPayload?.text ? parseUnifiedDiff(diffPayload.text).filter((r) => r.type !== "meta") : [],
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

  async function loadDiff(targetPath: string | undefined, force = false) {
    if (!repoPath || !targetPath) {
      diffInflight?.cancel();
      diffPayload = null;
      diffError = null;
      loadedDiffKey = "";
      return;
    }
    const key = `${repoPath}\0${targetPath}`;
    if (!force && key === loadedDiffKey && diffPayload !== null) return;
    diffInflight?.cancel();
    const guard = createAsyncGuard();
    diffInflight = guard;
    diffLoading = true;
    diffError = null;
    try {
      let res = await invoke<DiffPayload>("cmd_get_file_diff", {
        repoPath,
        filePath: targetPath,
        isStaged: false,
        ignoreWhitespace: false,
      });
      if (!guard.isLive()) return;
      let source: "unstaged" | "staged" = "unstaged";
      if (!res.text.trim()) {
        const stagedRes = await invoke<DiffPayload>("cmd_get_file_diff", {
          repoPath,
          filePath: targetPath,
          isStaged: true,
          ignoreWhitespace: false,
        });
        if (!guard.isLive()) return;
        if (stagedRes.text.trim()) {
          res = stagedRes;
          source = "staged";
        }
      }
      diffPayload = res;
      diffSource = source;
      loadedDiffKey = key;
    } catch (err: unknown) {
      if (!guard.isLive()) return;
      diffError = formatError(err);
      diffPayload = null;
    } finally {
      if (guard.isLive()) diffLoading = false;
    }
  }

  $effect(() => {
    const target = filePath;
    const repo = repoPath;
    const mode = viewMode;
    void repo;
    untrack(() => {
      void loadBlob(target);
      if (mode === "diff" && target) {
        void loadDiff(target);
      }
    });
  });

  async function loadQuickLens(
    repo: string,
    targetPath: string | undefined,
    line: number | null,
    mode: "code" | "diff",
  ) {
    lensInflight?.cancel();
    if (!repo || !targetPath || mode !== "code" || typeof line !== "number" || line < 1) {
      quickLensSymbol = null;
      quickLensCallers = [];
      quickLensTotalCallers = 0;
      return;
    }
    const guard = createAsyncGuard();
    lensInflight = guard;
    try {
      const headSha = await invoke<string>("cmd_get_head_id", { repoPath: repo });
      if (!guard.isLive() || !headSha) return;
      const page = await symbolsForFile(repo, targetPath, headSha);
      if (!guard.isLive()) return;
      if (!page.available || page.items.length === 0) {
        quickLensSymbol = null;
        quickLensCallers = [];
        quickLensTotalCallers = 0;
        return;
      }
      const enclosing = pickEnclosingSymbol(page.items, line);
      if (!enclosing) {
        quickLensSymbol = null;
        quickLensCallers = [];
        quickLensTotalCallers = 0;
        return;
      }
      quickLensSymbol = enclosing;
      const targetSym = enclosing.qualified_name || enclosing.symbol_name;
      const neighbors = await getNeighbors(repo, [targetSym], 4000, "high");
      if (!guard.isLive()) return;
      const { callers, total } = extractDirectCallers(neighbors, 5);
      quickLensCallers = callers;
      quickLensTotalCallers = total;
    } catch {
      if (!guard.isLive()) return;
      quickLensSymbol = null;
      quickLensCallers = [];
      quickLensTotalCallers = 0;
    }
  }

  $effect(() => {
    const repo = repoPath;
    const target = filePath;
    const line = revealLine;
    const mode = viewMode;
    untrack(() => {
      void loadQuickLens(repo, target, line, mode);
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
    if (viewMode === "diff") {
      await loadDiff(filePath, true);
    }
  }

  export function setViewMode(mode: "code" | "diff") {
    viewMode = mode;
    if (mode === "diff" && filePath) {
      void loadDiff(filePath, true);
    }
  }

  /**
   * Refreshes the open file or working-tree diff from disk when Follow Agent
   * observes an edit, preserving any unsaved user draft in `drafts`.
   */
  export function reloadIfClean() {
    if (!filePath) return;
    if (!drafts[filePath]) {
      void loadBlob(filePath, true);
    }
    if (viewMode === "diff") {
      void loadDiff(filePath, true);
    }
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
    diffInflight?.cancel();
    lensInflight?.cancel();
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
    diffInflight?.cancel();
    lensInflight?.cancel();
    if (copiedTimer) clearTimeout(copiedTimer);
    if (repoPath) recordEditorDrafts(repoPath, [], draftOwner);
  });
</script>

<div
  bind:this={paneRoot}
  class="h-full w-full flex flex-col min-h-0 min-w-0 bg-background text-xs font-sans overflow-hidden"
  data-terminal-file-pane={tabId}
>
  <div class="shrink-0 flex items-center justify-between gap-1.5 px-2 h-8 bg-surface/60 select-none">
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
        <span class="font-mono text-meta text-textPrimary truncate" title={filePath}>{filePath}</span>
        {#if isDirty}
          <span class="text-micro font-semibold text-warn shrink-0" role="status">Unsaved</span>
        {/if}
      {:else if snippet}
        <FileText size={13} class="text-accent shrink-0" />
        <span class="font-mono text-meta text-textPrimary truncate" title={snippet.title}>{snippet.title}</span>
        <span class="text-micro text-textMuted shrink-0">Read-only</span>
      {:else}
        <FileText size={13} class="text-textMuted shrink-0" />
        <span class="text-meta text-textMuted truncate">Select a file</span>
      {/if}
    </div>

    <div class="flex items-center gap-1 shrink-0">
      {#if filePath}
        <div
          class="inline-flex items-center rounded border border-border/65 bg-background/70 p-0.5 text-micro"
          role="group"
          aria-label="File pane view mode"
        >
          <button
            type="button"
            class="px-1.5 py-0.5 rounded-sm transition-colors {viewMode === 'code'
              ? 'bg-accent/15 text-accent font-medium'
              : 'text-textMuted hover:text-textPrimary'}"
            aria-pressed={viewMode === "code"}
            data-testid="terminal-file-code-toggle"
            onclick={() => setViewMode("code")}
          >
            Code
          </button>
          <button
            type="button"
            class="px-1.5 py-0.5 rounded-sm transition-colors {viewMode === 'diff'
              ? 'bg-accent/15 text-accent font-medium'
              : 'text-textMuted hover:text-textPrimary'}"
            aria-pressed={viewMode === "diff"}
            data-testid="terminal-file-diff-toggle"
            onclick={() => setViewMode("diff")}
          >
            Diff
          </button>
        </div>
      {/if}
      {#if filePath && onSendToAgent}
        <button
          type="button"
          class="gp-btn py-0! px-1.5! text-micro! flex items-center gap-1"
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
          {#if copiedPath}<Check size={12} class="text-ok" />{:else}<Copy size={12} />{/if}
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
          {#if copiedPath}<Check size={12} class="text-ok" />{:else}<Copy size={12} />{/if}
        </button>
        <button
          type="button"
          class="gp-icon-btn p-1!"
          aria-label="Reload file from disk"
          title="Reload file from disk"
          onclick={() => void reloadFromDisk()}
        >
          <RotateCw size={12} class={isLoading || diffLoading ? "animate-spin" : ""} />
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
      class="shrink-0 px-3 py-1.5 bg-danger/10 text-danger text-meta flex items-start gap-1.5"
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
      <div class="w-56 max-w-[45%] shrink-0 h-full bg-surface/40 min-h-0 overflow-hidden">
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
      {:else if viewMode === "diff"}
        <div class="h-full w-full flex flex-col min-h-0 overflow-hidden" data-testid="terminal-file-diff-view">
          {#if isDirty}
            <div class="shrink-0 px-2.5 py-1 bg-warn/10 text-warn text-micro">
              Showing working-tree diff on disk (unsaved editor draft is preserved in Code mode).
            </div>
          {/if}
          {#if diffPayload?.truncated}
            <div class="shrink-0 px-2.5 py-1 bg-warn/10 text-warn text-micro">
              Diff truncated{diffPayload.truncation_reason ? `: ${diffPayload.truncation_reason}` : "."}
            </div>
          {/if}
          {#if diffSource === "staged" && diffLines.length > 0}
            <div class="shrink-0 px-2.5 py-1 bg-surface/50 text-textMuted text-micro">
              Showing staged diff (working tree matches index).
            </div>
          {/if}
          {#if diffError}
            <div class="p-3 text-danger text-meta" role="alert">{diffError}</div>
          {:else if diffLoading && !diffPayload}
            <div class="h-full flex items-center justify-center text-textMuted text-xs">
              Loading diff for {filePath}…
            </div>
          {:else if diffLines.length === 0}
            <div class="h-full flex flex-col items-center justify-center gap-1.5 p-4 text-center text-textMuted text-xs">
              <span>No uncommitted working-tree or staged changes in <code class="font-mono text-textPrimary">{filePath}</code>.</span>
              <button
                type="button"
                class="gp-btn py-0.5! px-2! text-micro!"
                onclick={() => setViewMode("code")}
              >
                View full file
              </button>
            </div>
          {:else}
            <div class="flex-1 min-h-0 overflow-auto font-mono text-meta leading-5">
              {#each diffLines as row}
                {#if row.type === "hdr"}
                  <div class="px-2.5 py-0.5 bg-accent/10 text-accent select-text">
                    {row.content}
                  </div>
                {:else if row.type === "binary"}
                  <div class="px-2.5 py-1 text-textMuted italic">{row.content}</div>
                {:else if row.newNo}
                  <button
                    type="button"
                    class="w-full text-left flex items-stretch select-text cursor-pointer hover:brightness-110 {row.type === 'add'
                      ? 'bg-ok/12 text-ok'
                      : 'text-textPrimary/90'}"
                    title={`Jump to line ${row.newNo} in Code view`}
                    onclick={() => {
                      if (row.newNo && filePath) {
                        setViewMode("code");
                        chooseFile(filePath, row.newNo, 1);
                      }
                    }}
                  >
                    <span class="w-10 shrink-0 px-1.5 text-right text-micro text-textMuted/70 tabular-nums select-none bg-surface/30">
                      {row.oldNo ?? ""}
                    </span>
                    <span class="w-10 shrink-0 px-1.5 text-right text-micro text-textMuted/70 tabular-nums select-none bg-surface/40">
                      {row.newNo ?? ""}
                    </span>
                    <span class="w-4 shrink-0 text-center select-none text-textMuted">
                      {row.type === "add" ? "+" : " "}
                    </span>
                    <pre class="m-0 pr-3 whitespace-pre">{row.content}</pre>
                  </button>
                {:else}
                  <div
                    class="flex items-stretch select-text {row.type === 'del'
                      ? 'bg-danger/12 text-danger'
                      : 'text-textPrimary/90'}"
                  >
                    <span class="w-10 shrink-0 px-1.5 text-right text-micro text-textMuted/70 tabular-nums select-none bg-surface/30">
                      {row.oldNo ?? ""}
                    </span>
                    <span class="w-10 shrink-0 px-1.5 text-right text-micro text-textMuted/70 tabular-nums select-none bg-surface/40">
                      {row.newNo ?? ""}
                    </span>
                    <span class="w-4 shrink-0 text-center select-none text-textMuted">
                      {row.type === "del" ? "-" : " "}
                    </span>
                    <pre class="m-0 pr-3 whitespace-pre">{row.content}</pre>
                  </div>
                {/if}
              {/each}
            </div>
          {/if}
        </div>
      {:else if isLoading && !activeBlob}
        <div class="h-full flex items-center justify-center text-textMuted text-xs">
          Loading {filePath}…
        </div>
      {:else if activeBlob}
        <div class="h-full w-full flex flex-col min-h-0 overflow-hidden">
          {#if quickLensSymbol && filePath && !activeBlob.is_binary && !activeBlob.is_image}
            <div
              class="shrink-0 flex items-center justify-between gap-2 px-2.5 py-1 bg-surface/50 text-micro"
              data-testid="terminal-file-quick-lens"
            >
              <div class="flex items-center gap-1.5 min-w-0 overflow-x-auto">
                <span class="font-mono font-medium text-accent shrink-0">
                  {quickLensSymbol.symbol_name}
                </span>
                <span class="text-textMuted shrink-0">
                  L{quickLensSymbol.span_start_line}–{quickLensSymbol.span_end_line}
                </span>
                {#if quickLensCallers.length > 0}
                  <span class="text-textMuted shrink-0">· Callers ({quickLensTotalCallers}):</span>
                  {#each quickLensCallers as caller}
                    <button
                      type="button"
                      class="px-1.5 py-0.5 rounded bg-background/80 border border-border/65 font-mono text-textPrimary hover:border-accent/65 hover:text-accent shrink-0"
                      title={`Open caller ${caller.qualifiedSymbol} (${caller.filePath})`}
                      onclick={() => chooseFile(caller.filePath, null, null)}
                    >
                      {caller.symbol}
                    </button>
                  {/each}
                {:else}
                  <span class="text-textMuted shrink-0">· 0 indexed direct callers</span>
                {/if}
              </div>
              {#if onSendToAgent}
                <button
                  type="button"
                  class="gp-btn py-0! px-1.5! text-micro! shrink-0"
                  title="Send symbol span and direct callers to active agent"
                  data-testid="terminal-file-send-symbol-lens"
                  onclick={() =>
                    filePath &&
                    quickLensSymbol &&
                    onSendToAgent(
                      formatSymbolLensForAgent({
                        filePath,
                        symbol: quickLensSymbol,
                        callers: quickLensCallers,
                        totalCallers: quickLensTotalCallers,
                      }),
                    )}
                >
                  @symbol
                </button>
              {/if}
            </div>
          {/if}
          <div class="flex-1 min-h-0 overflow-hidden">
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
          </div>
        </div>
      {/if}
    </div>
  </div>
</div>
