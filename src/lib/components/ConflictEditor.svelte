<script module lang="ts">
  import type {
    ConflictChunk,
    ConflictDocument,
    ConflictResolutionChoice,
  } from "../diff/conflict";

  export interface ConflictCustomDraft {
    value: string;
    /** True only while this chunk is actively using its custom resolution. */
    active: boolean;
  }

  export type ConflictCustomDrafts = Record<string, ConflictCustomDraft>;

  /** A write may finalize only if no resolution changed while it was in flight. */
  export function canFinalizeConflictSave(savedRevision: number, currentRevision: number): boolean {
    return savedRevision === currentRevision;
  }

  /** Repository paths and file paths cannot contain NUL, so this identity is collision-free. */
  export function conflictDraftKey(repo: string, file: string, chunkIndex: number): string {
    return `${repo}\u0000${file}\u0000${chunkIndex}`;
  }

  function resolutionForCustomDraft(value: string): ConflictResolutionChoice {
    return { Custom: value };
  }

  /** Choices are reusable only for an identical source document, including EOLs. */
  export function sameConflictSource(next: ConflictDocument, current: ConflictDocument): boolean {
    const source = (doc: ConflictDocument) => JSON.stringify({
      ...doc,
      segments: doc.segments.map((segment) => segment.Conflict
        ? { Conflict: { ...segment.Conflict, resolution: "Unresolved" } }
        : segment),
    });
    return source(next) === source(current);
  }

  export function resolutionLabel(choice: ConflictResolutionChoice): string {
    if (typeof choice === "object") return choice.Custom === "" ? "Custom · empty" : "Custom";
    switch (choice) {
      case "Unresolved": return "Needs resolution";
      case "AcceptOurs": return "Current selected";
      case "AcceptTheirs": return "Incoming selected";
      case "AcceptBothOursFirst": return "Both · current first";
      case "AcceptBothTheirsFirst": return "Both · incoming first";
    }
  }

  /**
   * Copy every active draft for this exact repo/file into the wire document.
   * Save calls this synchronously before serialization, independently of the
   * debounced preview.
   */
  export function flushCustomDrafts(
    document: ConflictDocument,
    repo: string,
    file: string,
    drafts: ConflictCustomDrafts,
  ): ConflictDocument {
    if (document.file_path !== file) return document;
    for (const segment of document.segments) {
      const chunk = segment.Conflict;
      if (!chunk) continue;
      const draft = drafts[conflictDraftKey(repo, file, chunk.chunk_index)];
      if (draft?.active) chunk.resolution = resolutionForCustomDraft(draft.value);
    }
    return document;
  }

  /** Called only after the resolved file has been written successfully. */
  export function clearCustomDraftsForDocument(
    drafts: ConflictCustomDrafts,
    repo: string,
    file: string,
    document: ConflictDocument,
  ): void {
    if (document.file_path !== file) return;
    for (const segment of document.segments) {
      const chunk = segment.Conflict;
      if (chunk) delete drafts[conflictDraftKey(repo, file, chunk.chunk_index)];
    }
  }

  /**
   * A fresh parse lands every chunk back at Unresolved. When it belongs to
   * the file the user is already editing (e.g. they flipped files and came
   * back mid-merge), assigning it wholesale would silently discard their
   * chunk resolutions — so carry choices over only for identical source.
   * A different or externally changed file starts from its own parse untouched.
   */
  export function adoptResolutions(next: ConflictDocument, current: ConflictDocument | null): ConflictDocument {
    if (!current || !sameConflictSource(next, current)) return next;
    const carried = new Map<number, ConflictResolutionChoice>();
    for (const seg of current.segments) {
      const chunk = seg.Conflict;
      if (chunk && chunk.resolution !== "Unresolved") {
        carried.set(chunk.chunk_index, chunk.resolution);
      }
    }
    if (carried.size === 0) return next;
    for (const seg of next.segments) {
      const chunk = seg.Conflict;
      const carriedResolution = chunk ? carried.get(chunk.chunk_index) : undefined;
      if (chunk && carriedResolution) chunk.resolution = carriedResolution;
    }
    return next;
  }
</script>

<script lang="ts">
  import { repoStore, type DiffPayload } from "../stores/repoStore";
  import { tick } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { harnessStore, type Guarded } from "../stores/harnessStore";
  import { Check, GitMerge, AlertTriangle, ChevronUp, ChevronDown, FileCode2, Search, RotateCcw, Loader2 } from "@lucide/svelte";
  import EmptyState from "./EmptyState.svelte";
  import OperationBanner from "./OperationBanner.svelte";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { formatError } from "../ui/formatError";
  import { reportPanelError } from "../diagnostics/report";
  import { conflictSessions, hasResolution, materializeResolution, type ConflictDraft, type ConflictSnapshot, type ConflictSaveRequest, type ConflictSaveOutcome, type ResolutionState, type WholeFileChoice } from "../diff/conflictSession";
  import { editorFileSaveQueue, fileSaveKey } from "../files/serialSave";
  import { askConfirm } from "../stores/modalStore";
  import { copyText } from "../desktop/clipboard";
  import ConflictComparison from "./ConflictComparison.svelte";

  const contentRevisions = repoStore.contentRevisions;
  let conflictedFiles = $derived($repoStore.statuses.filter(s => s.is_conflicted));
  let operationState = $derived($repoStore.operation);
  let parked = $derived(operationState.operation !== null);
  let selectedFile = $state<string | null>(null);
  let parsedDoc = $state<ConflictDocument | null>(null);
  let draft = $state<ConflictDraft | null>(null);
  let sessionVersion = $state(0);
  let retained = $derived.by(() => { sessionVersion; return $repoStore.currentPath ? conflictSessions.summaries($repoStore.currentPath) : []; });
  let storageNotice = $state(conflictSessions.warning);
  let recoveryOpen = $state(false);
  let recoveryFile = $state<string | null>(null);
  let recoveryVersion = $state(0);
  let recoveryDraft = $derived.by(() => { sessionVersion; return recoveryOpen && recoveryFile && $repoStore.currentPath ? conflictSessions.get($repoStore.currentPath, recoveryFile) : null; });
  let recoveryVersions = $derived(recoveryDraft ? recoveryDraft.recovery.length ? recoveryDraft.recovery : [{ snapshot: recoveryDraft.snapshot, state: recoveryDraft.state }] : []);
  let recoveryText = $derived(recoveryVersions[recoveryVersion] ? JSON.stringify(recoveryVersions[recoveryVersion], null, 2) : "");
  $effect(() => conflictSessions.subscribeWarning(value => { storageNotice = value; }));
  let loadError = $state<string | null>(null);
  let previewError = $state<string | null>(null);
  let resolvedPreview = $state<string | null>(null);
  let isLoading = $state(false);
  let isPreviewing = $state(false);
  let fileFilter = $state("");
  let activeChunk = $state<number | null>(null);
  let previewExpanded = $state(false);
  let sourceNotice = $state<string | null>(null);
  let retryRevision = $state(0);
  let inspector = $state<HTMLDivElement>();
  let pageRoot = $state<HTMLDivElement>();
  let visibleFiles = $derived(conflictedFiles.filter(file => file.path.toLowerCase().includes(fileFilter.toLowerCase())));
  let chunks = $derived(parsedDoc?.segments.flatMap(segment => segment.Conflict ? [segment.Conflict] : []) ?? []);
  let remaining = $derived(chunks.filter(chunk => chunk.resolution === "Unresolved").length);
  let resolvedCount = $derived(chunks.length - remaining);
  let isSaving = $state(false);
  let saveError = $state<string | null>(null);
  let customDrafts = $state<ConflictCustomDrafts>({});
  let editRevision = 0;
  let loadGuard: AsyncGuard | null = null;
  let previewGuard: AsyncGuard | null = null;
  let saveGuard: AsyncGuard | null = null;
  const customTimers = new Map<string, ReturnType<typeof setTimeout>>();
  let blockPage = $state(0);
  const PAGE_SIZE = 25;
  let wholeChoice = $derived(draft?.state.whole ?? null);
  let pendingStage = $derived(Boolean(draft?.pending));
  let diagnostics = $derived(parsedDoc?.diagnostics ?? []);
  let hasUnresolved = $derived(!wholeChoice && !pendingStage && (parsedDoc === null || chunks.length === 0 || remaining > 0 || diagnostics.length > 0));
  let canSave = $derived(Boolean(draft) && !isSaving && !isLoading && (wholeChoice !== null || pendingStage || (!hasUnresolved && !isPreviewing && !previewError && resolvedPreview !== null)));
  let reviewPath = $state<string | null>(null);
  let reviewDiff = $state<DiffPayload | null>(null);
  let reviewError = $state<string | null>(null);
  let reviewLoading = $state(false);
  let reviewGuard: AsyncGuard | null = null;

  function customText(chunk: ConflictChunk): string {
    const saved = draft?.state.custom[String(chunk.chunk_index)];
    return saved ?? (typeof chunk.resolution === "object" ? chunk.resolution.Custom : "");
  }
  function applyDraft(next: ConflictDraft) {
    draft = next;
    parsedDoc = materializeResolution(next);
    customDrafts = {};
    for (const [index, value] of Object.entries(next.state.custom)) {
      customDrafts[conflictDraftKey(next.repo, next.snapshot.file_path, Number(index))] = { value, active: typeof next.state.choices[Number(index)] === "object" };
    }
    sessionVersion += 1;
    storageNotice = conflictSessions.warning;
  }
  function persistChoice(group: string | null = null, whole: WholeFileChoice | null = null) {
    if (!draft) return false;
    const state: ResolutionState = { choices: chunks.map(chunk => $state.snapshot(chunk.resolution)), custom: { ...draft.state.custom }, whole };
    for (const chunk of chunks) if (typeof chunk.resolution === "object") state.custom[String(chunk.chunk_index)] = chunk.resolution.Custom;
    try { draft = conflictSessions.edit(draft.repo, draft.snapshot.file_path, state, group); }
    catch (error) {
      const retained = conflictSessions.get(draft.repo, draft.snapshot.file_path);
      if (retained) applyDraft(retained);
      saveError = formatError(error);
      if (parsedDoc) void updatePreview(parsedDoc);
      return false;
    }
    sessionVersion += 1;
    editRevision += 1;
    storageNotice = conflictSessions.warning;
    return true;
  }
  async function acceptSnapshot(repo: string, snapshot: ConflictSnapshot) {
    const next = conflictSessions.open(repo, snapshot);
    if (next.recovery.length) sourceNotice = "This file changed on disk or belongs to a new operation. Previous drafts are retained in Draft recovery below.";
    applyDraft(next);
    activeChunk = chunks.find(chunk => chunk.resolution === "Unresolved")?.chunk_index ?? chunks[0]?.chunk_index ?? null;
    blockPage = Math.floor((activeChunk ?? 0) / PAGE_SIZE);
    if (parsedDoc) await updatePreview(parsedDoc);
  }
  $effect(() => {
    if (!conflictedFiles.some(file => file.path === selectedFile)) selectedFile = conflictedFiles[0]?.path ?? null;
  });
  $effect(() => () => {
    loadGuard?.cancel(); previewGuard?.cancel(); saveGuard?.cancel(); reviewGuard?.cancel();
    for (const timer of customTimers.values()) clearTimeout(timer);
    conflictSessions.flush();
  });

  let loadedKey: string | null = null;
  let loadedRetry = -1;
  $effect(() => {
    const repo = $repoStore.currentPath;
    const file = selectedFile;
    const key = repo && file ? `${repo}\u0000${file}` : null;
    if (key === loadedKey && loadedRetry === retryRevision) return;
    loadedKey = key; loadedRetry = retryRevision;
    loadGuard?.cancel(); previewGuard?.cancel(); saveGuard?.cancel();
    parsedDoc = null; draft = null; resolvedPreview = null;
    isLoading = Boolean(repo && file); isPreviewing = false; isSaving = false;
    loadError = null; saveError = null; previewError = null; sourceNotice = null; activeChunk = null;
    if (!repo || !file) return;
    const guard = createAsyncGuard(); loadGuard = guard;
    void (async () => {
      try {
        // A remounted view must wait for an accepted save before it can edit
        // the same source. App quit observes this same process-lifetime queue.
        await editorFileSaveQueue.whenIdle(fileSaveKey(repo, file));
        if (!guard.isLive()) return;
        const snapshot = await invoke<ConflictSnapshot>("cmd_conflict_snapshot", { repoPath: repo, filePath: file });
        if (!guard.isLive()) return;
        await acceptSnapshot(repo, snapshot);
      } catch (error) {
        if (guard.isLive()) loadError = reportPanelError("conflict", error);
      } finally { if (guard.isLive()) isLoading = false; }
    })();
  });

  // Real repository refreshes may change bytes without changing the UU status.
  // Probe in the background; identical snapshots leave focus and keystrokes alone.
  let checkedContent = "";
  let probeSequence = 0;
  $effect(() => {
    const repo = $repoStore.currentPath;
    const revision = repo ? $contentRevisions[repo] ?? "" : "";
    const file = selectedFile;
    const current = draft?.pending?.revision ?? draft?.snapshot.revision;
    if (!repo || !file || !current || !revision || isSaving || isLoading) return;
    const checkKey = `${repo}\u0000${file}\u0000${revision}`;
    if (checkedContent === checkKey) return;
    checkedContent = checkKey;
    const sequence = ++probeSequence;
    const guard = loadGuard;
    void (async () => {
      try {
        const snapshot = await invoke<ConflictSnapshot>("cmd_conflict_snapshot", { repoPath: repo, filePath: file });
        if (sequence !== probeSequence || !guard?.isLive() || isSaving || $repoStore.currentPath !== repo || selectedFile !== file) return;
        if (snapshot.revision !== (draft?.pending?.revision ?? draft?.snapshot.revision)) await acceptSnapshot(repo, snapshot);
      } catch (error) {
        if (sequence === probeSequence && guard?.isLive() && $repoStore.currentPath === repo && selectedFile === file) sourceNotice = `Source refresh failed: ${formatError(error)}. Your draft is retained; reload before saving.`;
      }
    })();
  });

  async function jumpToConflict(direction: -1 | 1, unresolvedOnly = false) {
    const eligible = chunks.filter(chunk => !unresolvedOnly || chunk.resolution === "Unresolved");
    if (!eligible.length) return;
    const ordered = direction === 1 ? eligible : [...eligible].reverse();
    const next = ordered.find(chunk => direction === 1 ? chunk.chunk_index > (activeChunk ?? -1) : chunk.chunk_index < (activeChunk ?? 0)) ?? ordered[0];
    const document = parsedDoc;
    activeChunk = next.chunk_index; blockPage = Math.floor(next.chunk_index / PAGE_SIZE); previewExpanded = false;
    await tick();
    if (parsedDoc !== document) return;
    const block = inspector?.querySelector<HTMLElement>(`[data-conflict-index="${next.chunk_index}"]`);
    block?.scrollIntoView({ block: "start", behavior: "instant" }); block?.focus({ preventScroll: true });
  }
  function setDocumentChunkResolution(document: ConflictDocument, chunkIndex: number, choice: ConflictResolutionChoice) {
    const chunk = document.segments.find(segment => segment.Conflict?.chunk_index === chunkIndex)?.Conflict;
    if (chunk) chunk.resolution = choice;
  }
  function deactivateCustomDraft(repo: string, file: string, chunkIndex: number) {
    const key = conflictDraftKey(repo, file, chunkIndex);
    const draft = customDrafts[key]; if (draft) customDrafts[key] = { ...draft, active: false };
    clearTimeout(customTimers.get(key)); customTimers.delete(key);
  }
  function setChunkResolution(chunkIndex: number, choice: ConflictResolutionChoice) {
    const repo = $repoStore.currentPath; const document = parsedDoc;
    if (!repo || !document || isSaving || document.file_path !== selectedFile) return;
    deactivateCustomDraft(repo, document.file_path, chunkIndex);
    if (typeof choice === "object") customDrafts[conflictDraftKey(repo, document.file_path, chunkIndex)] = { value: choice.Custom, active: true };
    setDocumentChunkResolution(document, chunkIndex, choice); activeChunk = chunkIndex;
    if (persistChoice()) void updatePreview(document);
  }
  function resolveAll(choice: ConflictResolutionChoice, replace = false) {
    const repo = $repoStore.currentPath; const document = parsedDoc;
    if (!repo || !document || isSaving || document.file_path !== selectedFile) return;
    for (const chunk of chunks) {
      if (!replace && choice !== "Unresolved" && chunk.resolution !== "Unresolved") continue;
      deactivateCustomDraft(repo, document.file_path, chunk.chunk_index); chunk.resolution = choice;
    }
    if (persistChoice()) void updatePreview(document);
  }
  function onCustomInput(chunkIndex: number, value: string) {
    const repo = $repoStore.currentPath; const document = parsedDoc;
    if (!repo || !document || isSaving || selectedFile !== document.file_path) return;
    if (value.length > 1024 * 1024) { saveError = "A custom block is limited to 1 MiB. Copy larger edits to an external editor."; return; }
    const file = document.file_path; const key = conflictDraftKey(repo, file, chunkIndex);
    customDrafts[key] = { value, active: true }; activeChunk = chunkIndex;
    previewGuard?.cancel(); isPreviewing = true; resolvedPreview = null;
    setDocumentChunkResolution(document, chunkIndex, resolutionForCustomDraft(value));
    if (!persistChoice(`custom:${chunkIndex}`)) return;
    clearTimeout(customTimers.get(key));
    customTimers.set(key, setTimeout(() => {
      customTimers.delete(key);
      if ($repoStore.currentPath === repo && selectedFile === file && parsedDoc === document) void updatePreview(document);
      storageNotice = conflictSessions.warning;
    }, 250));
  }
  async function updatePreview(doc: ConflictDocument) {
    const repo = $repoStore.currentPath; const file = selectedFile;
    previewGuard?.cancel(); const guard = createAsyncGuard(); previewGuard = guard;
    isPreviewing = true; resolvedPreview = null;
    try {
      const res = await invoke<string>("cmd_preview_conflict", { document: $state.snapshot(doc) });
      if (!guard.isLive() || $repoStore.currentPath !== repo || selectedFile !== file || selectedFile !== doc.file_path) return;
      previewError = null; resolvedPreview = res;
    } catch (err) {
      if (guard.isLive() && $repoStore.currentPath === repo && selectedFile === file) { resolvedPreview = null; previewError = formatError(err); }
    } finally { if (guard.isLive()) isPreviewing = false; }
  }
  function travel(direction: "undo" | "redo") {
    if (!draft || isSaving) return;
    const next = conflictSessions.travel(draft.repo, draft.snapshot.file_path, direction);
    if (next) { applyDraft(next); editRevision += 1; if (parsedDoc) void updatePreview(parsedDoc); }
  }
  function chooseWhole(choice: WholeFileChoice) {
    if (!draft || isSaving) return;
    persistChoice(null, wholeChoice === choice ? null : choice);
  }
  function handleKeys(event: KeyboardEvent) {
    if (event.defaultPrevented || !(event.target instanceof Node) || !pageRoot?.contains(event.target)) return;
    if (event.isComposing) return;
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "s") { event.preventDefault(); if (canSave) void saveResolved(); }
    if (event.altKey && !event.metaKey && !event.ctrlKey && ["ArrowDown", "ArrowUp"].includes(event.key)) { event.preventDefault(); void jumpToConflict(event.key === "ArrowDown" ? 1 : -1, event.shiftKey); }
    // Textareas retain their native undo/redo; toolbar history is for choices.
    const editing = event.target instanceof HTMLElement && (event.target.matches("input,textarea") || event.target.isContentEditable);
    if (!editing && (event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "z") { event.preventDefault(); travel(event.shiftKey ? "redo" : "undo"); }
  }
  async function discardRetained(file: string) {
    const repo = $repoStore.currentPath; if (!repo || isSaving) return;
    if (!await askConfirm({ title: "Discard retained conflict draft?", message: `This removes the retained choices and recovery copies for ${file}. Files on disk are unchanged.`, confirmLabel: "Discard draft", cancelLabel: "Keep draft" })) return;
    conflictSessions.discard(repo, file); sessionVersion += 1; if (file === selectedFile) retryRevision += 1;
  }
  async function copyRecovery(value: unknown) {
    sourceNotice = await copyText(JSON.stringify(value, null, 2)) ? "Draft recovery copied." : "Could not copy the recovery. Select and copy the displayed text.";
  }
  function reapplyLatestRecovery() {
    if (!draft || !parsedDoc || isSaving) return;
    const previous = draft.recovery.at(-1);
    const recovered = previous ? materializeResolution(previous) : null;
    if (!recovered || !sameConflictSource(parsedDoc, recovered)) {
      sourceNotice = "The previous source differs. Compare the retained version and copy the custom replacements you want to keep.";
      return;
    }
    customDrafts = {};
    parsedDoc = adoptResolutions($state.snapshot(parsedDoc), recovered);
    if (!persistChoice()) return;
    sourceNotice = "Matching previous choices reapplied. Review the preview before saving.";
    void updatePreview(parsedDoc);
  }
  async function saveResolved() {
    const repo = $repoStore.currentPath; const file = selectedFile; const document = parsedDoc;
    if (!file || !repo || !draft || !canSave) return;
    if (document && document.file_path !== file) return;
    if (document) flushCustomDrafts(document, repo, file, customDrafts);
    const request: ConflictSaveRequest = {
      file_path: file, revision: draft.pending?.revision ?? draft.snapshot.revision,
      choice: pendingStage ? "StageOnly" : wholeChoice ?? { Chunks: chunks.map(chunk => $state.snapshot(chunk.resolution)) },
    };
    const saveRevision = editRevision;
    const savedState = $state.snapshot(draft.state);
    saveGuard?.cancel(); const guard = createAsyncGuard(); saveGuard = guard;
    isSaving = true; saveError = null;
    try {
      const outcome = await editorFileSaveQueue.run(fileSaveKey(repo, file), async () => {
        const result = await invoke<Guarded<ConflictSaveOutcome>>("cmd_save_conflict", { repoPath: repo, request });
        harnessStore.recordVerdict(result.policy, repo);
        harnessStore.recordAction({ repoPath: repo, kind: "resolve", label: file, ok: result.output.staged });
        try {
          if (result.output.staged) conflictSessions.complete(repo, file, request.revision, savedState);
          else if (result.output.snapshot) conflictSessions.pending(repo, file, result.output.snapshot, request.revision, savedState);
        } catch (error) {
          result.output.message += ` Draft recovery could not be updated: ${formatError(error)}. Reload and inspect the saved file before another action.`;
        }
        return result.output;
      });
      if (!guard.isLive() || $repoStore.currentPath !== repo || selectedFile !== file) return;
      if (!canFinalizeConflictSave(saveRevision, editRevision)) { saveError = "A newer draft remains in memory. Review the current repository state before another save."; return; }
      if (outcome.staged) {
        if (document) clearCustomDraftsForDocument(customDrafts, repo, file, document);
      } else {
        const pending = conflictSessions.get(repo, file); if (pending) applyDraft(pending);
        saveError = outcome.message + (outcome.recovery_path ? ` Original retained at ${outcome.recovery_path}` : "");
      }
      sessionVersion += 1;
      await repoStore.refresh(repo);
    } catch (error) {
      harnessStore.recordAction({ repoPath: repo, kind: "resolve", label: file, ok: false });
      if (guard.isLive() && $repoStore.currentPath === repo && selectedFile === file) saveError = formatError(error);
    } finally {
      if (guard.isLive()) isSaving = false;
      // A completed save still refreshes its repository after the view unmounts.
      if (!guard.isLive()) void repoStore.refresh(repo);
    }
  }
  async function reviewStaged(path: string) {
    const repo = $repoStore.currentPath; if (!repo) return;
    reviewGuard?.cancel(); const guard = createAsyncGuard(); reviewGuard = guard;
    reviewPath = path; reviewDiff = null; reviewError = null; reviewLoading = true;
    try { const result = await invoke<DiffPayload>("cmd_get_file_diff", { repoPath: repo, filePath: path, isStaged: true, ignoreWhitespace: false }); if (guard.isLive() && $repoStore.currentPath === repo) reviewDiff = result; }
    catch (error) { if (guard.isLive()) reviewError = formatError(error); }
    finally { if (guard.isLive()) reviewLoading = false; }
  }
  let reviewedRepository = "";
  $effect(() => {
    const repo = $repoStore.currentPath;
    const key = JSON.stringify([repo, repo ? $contentRevisions[repo] : null]);
    if (reviewedRepository === key) return;
    reviewedRepository = key;
    reviewGuard?.cancel(); reviewPath = null; reviewDiff = null; reviewError = null; reviewLoading = false;
    recoveryFile = null; recoveryVersion = 0;
  });
</script>

<svelte:window onkeydown={handleKeys} />
<div class="resolve-page" bind:this={pageRoot}>
  <header class="page-heading">
    <div class="heading-copy">
      <span class="heading-icon"><GitMerge size={19} /></span>
      <div><h2>Resolve conflicts</h2><p>Compare changes, choose what to keep, then stage each file.</p></div>
    </div>
    {#if conflictedFiles.length > 0}<span class="file-count">{conflictedFiles.length} file{conflictedFiles.length === 1 ? "" : "s"} to resolve</span>{/if}
  </header>
  {#if storageNotice}<div class="source-notice" role="status">{storageNotice}</div>{/if}
  {#if retained.some(item => item.recoveryCount || !conflictedFiles.some(file => file.path === item.file))}
    <details class="recovery-panel" bind:open={recoveryOpen}><summary>Draft recovery · retained choices</summary>
      {#if recoveryOpen}<div class="recovery-list">
        {#each retained.filter(item => item.recoveryCount || !conflictedFiles.some(file => file.path === item.file)) as item (item.file)}
          <button class="text-action" aria-pressed={recoveryFile === item.file} onclick={() => { recoveryFile = item.file; recoveryVersion = Math.max(0, item.recoveryCount - 1); }}>{item.file} · {item.recoveryCount || 1} retained version{item.recoveryCount > 1 ? "s" : ""}</button>
        {/each}
        {#if recoveryDraft}
          <p>These choices belong to an earlier source or operation. Compare them with the current file before copying a custom replacement.</p>
          <select aria-label="Retained source version" bind:value={recoveryVersion}>{#each recoveryVersions as version, index}<option value={index}>Version {index + 1} · {version.snapshot.revision.slice(0, 12)}</option>{/each}</select>
          {#if recoveryText.length > 250_000}<p role="status">Showing 250,000 of {recoveryText.length.toLocaleString()} characters. Copy this version for the complete recovery.</p>{/if}
          <pre class="source-code">{recoveryText.slice(0, 250_000)}</pre>
          <button class="text-action" onclick={() => copyRecovery(recoveryVersions[recoveryVersion])}>Copy recovery version</button>
          {#if recoveryFile === selectedFile && recoveryDraft.recovery.length}<button class="text-action" disabled={isSaving} onclick={reapplyLatestRecovery}>Reapply latest matching draft</button>{/if}
          <button class="text-action" disabled={isSaving} onclick={() => recoveryFile && discardRetained(recoveryFile)}>Discard retained draft</button>
        {/if}
      </div>{/if}
    </details>
  {/if}
  {#if operationState.operation || operationState.probeFailed}
    <div class="operation" inert={isSaving}><OperationBanner {operationState} /></div>
  {/if}
  {#if conflictedFiles.length === 0}
    <EmptyState icon={Check} title={parked ? "All conflicts resolved" : "No merge conflicts"}
      hint={parked ? "Review the staged changes below, then continue the operation above." : "No conflicted files are listed in this repository."} />
    {#if parked}
      <section class="staged-review" aria-label="Final staged review"><h3>Review staged changes</h3><p>Includes all staged files in this repository.</p>
        <div class="review-files">{#each $repoStore.statuses.filter(file => file.is_staged) as file (file.path)}<button class="text-action" aria-pressed={reviewPath === file.path} onclick={() => reviewStaged(file.path)}>{file.path}</button>{/each}</div>
        {#if reviewLoading}<p role="status">Loading staged diff…</p>{:else if reviewError}<p role="alert">{reviewError}</p><button class="text-action" onclick={() => reviewPath && reviewStaged(reviewPath)}>Retry staged review</button>
        {:else if reviewDiff}{#if reviewDiff.truncated}<p role="status">Partial staged diff: {reviewDiff.truncation_reason ?? "the display limit was reached"}. Review the complete diff in Git before continuing.</p>{/if}<pre class="source-code">{reviewDiff.text || "No textual changes for this file. Check its status and Git mode."}</pre>{/if}
      </section>
    {/if}
  {:else}
    <div class="resolve-workspace">
      <aside class="file-rail" aria-label="Conflicted files">
        <div class="rail-heading"><span>Files to resolve</span><span>{conflictedFiles.length}</span></div>
        <label class="file-search"><Search size={13} /><input type="search" placeholder="Filter files…" aria-label="Filter conflicted files" bind:value={fileFilter} /></label>
        <nav class="file-list" aria-label="Choose a conflicted file">
          {#each visibleFiles as file (file.path)}
            {@const saved = retained.find(item => item.file === file.path)}
            <button class="file-row" class:current={selectedFile === file.path} disabled={isSaving}
              aria-current={selectedFile === file.path ? "true" : undefined} title={file.path}
              onclick={() => selectedFile = file.path}>
              <FileCode2 size={15} />
              <span class="file-name"><strong>{file.path.split("/").pop()}</strong><small>{file.path.includes("/") ? file.path.slice(0, file.path.lastIndexOf("/")) : "Repository root"}</small></span>
              {#if saved}<small>{saved.pending ? "Stage pending" : saved.whole ? "Side chosen" : `${saved.resolved}/${saved.total}`}</small>{:else}<span class="file-dot" aria-label="Unmerged"></span>{/if}
            </button>
          {/each}
          {#if visibleFiles.length === 0}<p class="rail-empty">No matching files.<button class="text-action" onclick={() => fileFilter = ""}>Clear filter</button></p>{/if}
        </nav>
        <p class="rail-note">Drafts survive view changes. ⌘/Ctrl S saves; Alt ↑/↓ navigates. Add Shift to skip resolved blocks.</p>
      </aside>
      <main class="editor-main">
        <div class="file-toolbar">
          <div class="file-title"><FileCode2 size={15} /><span title={selectedFile ?? ""}>{selectedFile}</span></div>
          <select class="mobile-file-select" aria-label="Conflicted file" bind:value={selectedFile} disabled={isSaving}>
            {#each conflictedFiles as file (file.path)}<option value={file.path}>{file.path}</option>{/each}
          </select>
          <button class="text-action" disabled={isSaving || isLoading} onclick={() => retryRevision += 1} title="Reload and compare the source; retains previous drafts">Reload source</button>
          <button onclick={saveResolved} disabled={!canSave}
            class="gp-btn-success save-button" title={hasUnresolved ? "Resolve every block in this file before saving" : "Write the resolved file and stage it"}>
            {#if isSaving}<Loader2 size={14} class="animate-spin" />{:else}<Check size={14} />{/if}
            {isSaving ? "Saving & staging…" : pendingStage ? "Retry staging" : wholeChoice === "WorkingTree" ? "Stage working file" : "Save & stage file"}
          </button>
        </div>
        {#if saveError}<div class="error-notice" role="alert"><AlertTriangle size={14} /><span>{saveError}</span></div>{/if}
        {#if sourceNotice}<div class="source-notice" role="status"><AlertTriangle size={14} /><span>{sourceNotice}</span></div>{/if}
        {#if draft && !isLoading && !loadError}
          <div class="whole-file-actions">
            {#if hasResolution(draft)}<button class="text-action" onclick={() => draft && copyRecovery({ snapshot: draft.snapshot, state: draft.state })}>Copy current draft</button>{/if}
            <details open={!parsedDoc || Boolean(wholeChoice)}><summary>Whole-file resolution {wholeChoice ? "· side chosen" : ""}</summary>
              <p>{draft.snapshot.reason ?? "A complete side replaces this file, including changes outside the text conflict blocks. Git modes and binary bytes are preserved."}</p>
              <div class="whole-choices">{#each ["Ours", "Theirs"] as side (side)}
                {@const stage = draft.snapshot.stages.find(stage => stage.stage === (side === "Ours" ? 2 : 3))}
                <button class="gp-btn" disabled={isSaving || Boolean(stage && stage.size > 16 * 1024 * 1024)} aria-pressed={wholeChoice === side} onclick={() => chooseWhole(side === "Ours" ? "Ours" : "Theirs")}>
                  {side === "Ours" ? "Use current file" : "Use incoming file"}{stage ? ` · ${stage.mode} · ${stage.size.toLocaleString()} bytes` : " · keep deletion"}
                </button>
              {/each}</div>
              {#if wholeChoice && wholeChoice !== "WorkingTree"}
                {@const selected = draft.snapshot.stages.find(stage => stage.stage === (wholeChoice === "Ours" ? 2 : 3))}
                <p role="status">{selected ? `Selected ${wholeChoice === "Ours" ? "current" : "incoming"} Git object ${selected.oid}. This complete side will replace the working file.` : "The selected side deleted this path. Saving will keep the deletion."}</p>
                {#if selected?.text !== null && selected?.text !== undefined}<pre class="source-code">{selected.text || "(Empty file)"}</pre>{/if}
              {/if}
            </details>
            {#if chunks.length === 0 && diagnostics.length === 0 && draft.snapshot.worktree_mode !== "160000"}<button class="text-action" disabled={isSaving} aria-pressed={wholeChoice === "WorkingTree"} onclick={() => chooseWhole("WorkingTree")}>Use the working file resolved externally</button>{/if}
            {#if wholeChoice === "WorkingTree"}<p role="status">Working file selected. Staging preserves the bytes and file type loaded with this source.</p>{/if}
          </div>
          {#if diagnostics.length}<div class="error-notice" role="alert"><span>{diagnostics.join(" · ")}. Repair these markers externally and reload, or choose a complete side.</span></div>{/if}
        {/if}
        {#if isLoading}
          <div class="editor-state" role="status"><Loader2 size={22} class="animate-spin" /><h3>Loading conflicts…</h3><p>Reading the selected file.</p></div>
        {:else if loadError}
          <div class="editor-state" role="alert"><AlertTriangle size={24} /><h3>Could not load this file</h3><p>{loadError}</p><button class="gp-btn" onclick={() => retryRevision += 1}><RotateCcw size={13} />Retry loading</button></div>
        {:else if parsedDoc}
          <div class="resolution-toolbar">
            <div class="progress-copy" role="status"><span class:all-resolved={!hasUnresolved}>{resolvedCount} / {chunks.length} resolved</span><progress max={Math.max(1, chunks.length)} value={resolvedCount} aria-label="Conflicts resolved in this file"></progress></div>
            <div class="chunk-navigation" role="group" aria-label="Conflict navigation">
              <button class="icon-button" title="Previous conflict" aria-label="Previous conflict" disabled={chunks.length === 0} onclick={() => jumpToConflict(-1)}><ChevronUp size={14} /></button>
              <button class="icon-button" title="Next conflict" aria-label="Next conflict" disabled={chunks.length === 0} onclick={() => jumpToConflict(1)}><ChevronDown size={14} /></button>
              <button class="text-action" disabled={remaining === 0} onclick={() => jumpToConflict(1, true)}>Next unresolved <span>{remaining}</span></button>
            </div>
            <button class="text-action" disabled={isSaving || !draft?.undo.length} onclick={() => travel("undo")}>Undo</button>
            <button class="text-action" disabled={isSaving || !draft?.redo.length} onclick={() => travel("redo")}>Redo</button>
            <details class="bulk-actions"><summary>Resolve all…</summary><div class="bulk-menu">
              <p>Applies to unresolved blocks. Existing choices are preserved.</p>
              <button disabled={isSaving} onclick={() => resolveAll("AcceptOurs")}>Accept All Current (Ours)</button>
              <button disabled={isSaving} onclick={() => resolveAll("AcceptTheirs")}>Accept All Incoming (Theirs)</button>
              <button disabled={isSaving} onclick={() => resolveAll("Unresolved")}>Reset all choices</button>
              <button disabled={isSaving} onclick={() => resolveAll("AcceptOurs", true)}>Replace every choice with Current</button>
              <button disabled={isSaving} onclick={() => resolveAll("AcceptTheirs", true)}>Replace every choice with Incoming</button>
            </div></details>
          </div>
          {#if operationState.operation?.kind === "Rebase" || operationState.operation?.kind === "RebaseApply"}
            <p class="rebase-note">During a rebase, Current is the branch being rebased onto; Incoming is the commit being replayed. Check the labels below.</p>
          {/if}
          {#if chunks.length > PAGE_SIZE}<div class="resolution-toolbar"><span>Showing conflicts {blockPage * PAGE_SIZE + 1}–{Math.min(chunks.length, (blockPage + 1) * PAGE_SIZE)} of {chunks.length}</span><button class="text-action" disabled={blockPage === 0} onclick={() => blockPage -= 1}>Previous blocks</button><button class="text-action" disabled={(blockPage + 1) * PAGE_SIZE >= chunks.length} onclick={() => blockPage += 1}>Next blocks</button></div>{/if}
          <div class="editor-panes" class:preview-expanded={previewExpanded}>
            <div class="conflict-inspector" bind:this={inspector}>
              {#if chunks.length === 0}<div class="source-notice">No text conflict blocks were found. Review the file result carefully before staging; this may be a conflict resolved outside GitPulse.</div>{/if}
              {#each parsedDoc.segments.filter(segment => segment.Conflict ? Math.floor(segment.Conflict.chunk_index / PAGE_SIZE) === blockPage : blockPage === 0) as segment, segmentIndex (segmentIndex)}
                {#if segment.Conflict}
                  {@const chunk = segment.Conflict}
                  <section class="conflict-block" class:active-block={activeChunk === chunk.chunk_index} class:resolved-block={chunk.resolution !== "Unresolved"}
                    data-conflict-index={chunk.chunk_index} tabindex="-1" aria-label={`Conflict ${chunk.chunk_index + 1}`}>
                    <div class="block-heading">
                      <div><strong>Conflict {chunk.chunk_index + 1}</strong><span class="line-range">Lines {chunk.start_line}–{chunk.end_line}</span></div>
                      <div><span class="choice-status" class:resolved={chunk.resolution !== "Unresolved"}>{#if chunk.resolution !== "Unresolved"}<Check size={12} />{/if}{resolutionLabel(chunk.resolution)}</span>
                        <button class="icon-button" title="Reset this conflict" aria-label={`Reset conflict ${chunk.chunk_index + 1}`} disabled={isSaving || chunk.resolution === "Unresolved"} onclick={() => setChunkResolution(chunk.chunk_index, "Unresolved")}><RotateCcw size={12} /></button></div>
                    </div>
                    <ConflictComparison {chunk} disabled={isSaving} choose={(choice) => setChunkResolution(chunk.chunk_index, choice)} />
                    {#if chunk.base_content !== undefined && chunk.base_content !== null}<details class="base-context"><summary>Common ancestor</summary><pre class="source-code">{chunk.base_content || "(Empty ancestor)"}</pre></details>{/if}
                    <details class="custom-resolution" open={typeof chunk.resolution === "object"}>
                      <summary>Custom resolution <span>Write the exact replacement</span></summary>
                      <div class="custom-body"><textarea rows={4} maxlength={1024 * 1024} aria-label={`Custom resolution for conflict ${chunk.chunk_index + 1}`} placeholder="Type the exact content this conflict should resolve to…" disabled={isSaving}
                        value={customText(chunk)} oninput={(event) => onCustomInput(chunk.chunk_index, event.currentTarget.value)}></textarea>
                        <div class="custom-footer"><span>Empty content removes this block. Reset clears your choice.</span><button class="text-action" disabled={isSaving} onclick={() => setChunkResolution(chunk.chunk_index, { Custom: "" })}>Use empty resolution</button></div>
                      </div>
                    </details>
                  </section>
                {:else if segment.Normal !== undefined && segment.Normal !== ""}
                  <details class="unchanged-context"><summary>Unchanged context <span>{segment.Normal.split("\n").length} lines</span></summary><pre class="source-code">{segment.Normal}</pre></details>
                {/if}
              {/each}
            </div>
            <section class="result-pane" aria-label="Resolved file preview">
              <div class="result-heading"><div><span class="eyebrow">File result</span><strong>Resolved preview</strong></div><button class="text-action" aria-pressed={previewExpanded} onclick={() => previewExpanded = !previewExpanded}>{previewExpanded ? "Split view" : "Expand"}</button></div>
              <p class="result-description">{wholeChoice ? "A whole-file choice is selected above. This block preview is retained for comparison." : diagnostics.length ? "Malformed markers block text resolution." : hasUnresolved ? "Unresolved blocks still include conflict markers." : "Review this result, then save and stage the file."}</p>
              {#if isPreviewing}<div class="preview-state" role="status"><Loader2 size={16} class="animate-spin" />Updating preview…</div>
              {:else if previewError}<div class="error-notice" role="alert"><span>Preview failed: {previewError}</span><button class="text-action" onclick={() => parsedDoc && updatePreview(parsedDoc)}>Retry preview</button></div>
              {:else if resolvedPreview === ""}<div class="preview-state">Empty file — this resolution removes all content.</div>
              {:else if resolvedPreview !== null}<div class="result-code" role="textbox" aria-readonly="true" aria-multiline="true" tabindex="0" aria-label="Resolved file content">{resolvedPreview}</div>{/if}
              <div class="result-footer"><span>{parsedDoc.crlf ? "Contains CRLF" : "LF"} · {parsedDoc.trailing_newline ? "Final newline" : "No final newline"}</span><span>{isPreviewing ? "Updating…" : previewError ? "Preview unavailable" : hasUnresolved ? `${remaining} remaining` : "Ready to stage"}</span></div>
            </section>
          </div>
        {/if}
      </main>
    </div>
  {/if}
</div>

<style>
  .resolve-page { container-type: inline-size; display: flex; flex-direction: column; height: 100%; min-height: 0; overflow: hidden; background: rgb(var(--c-bg)); color: rgb(var(--c-text)); font-size: 12px; }
  .page-heading { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 20px 22px 16px; flex-shrink: 0; }
  .heading-copy { display: flex; align-items: center; gap: 12px; min-width: 0; }
  .heading-icon { display: grid; place-items: center; width: 38px; height: 38px; border-radius: 12px; color: rgb(var(--c-accent)); background: rgb(var(--c-accent) / .1); }
  h2 { margin: 0; font-size: 17px; font-weight: 650; letter-spacing: -.025em; }
  .heading-copy p, .rail-note { margin: 4px 0 0; color: rgb(var(--c-text-muted)); font-size: 11px; line-height: 1.6; }
  .file-count { border-radius: 20px; padding: 6px 10px; background: rgb(var(--c-surface-hover) / .7); color: rgb(var(--c-text-muted)); white-space: nowrap; font-size: 11px; }
  .operation { padding: 0 16px 12px; }
  .whole-file-actions, .recovery-panel, .staged-review { padding: 10px 18px; border-bottom: 1px solid rgb(var(--c-border) / .4); font-size: 11px; }
  .whole-file-actions p, .recovery-panel p, .staged-review p { color: rgb(var(--c-text-muted)); line-height: 1.6; margin: 8px 0; }
  .whole-choices, .review-files { display: flex; flex-wrap: wrap; gap: 8px; }
  .whole-choices button[aria-pressed="true"] { outline: 2px solid rgb(var(--c-accent)); }
  .whole-file-actions pre { max-height: 160px; }
  .recovery-list { overflow: auto; max-height: 220px; }
  .recovery-list pre { max-height: 140px; }
  .staged-review { flex: 1; min-height: 160px; overflow: auto; }
  .resolve-workspace { display: flex; flex: 1; min-height: 0; border-top: 1px solid rgb(var(--c-border) / .4); }
  .file-rail { width: 210px; flex-shrink: 0; display: flex; flex-direction: column; padding: 16px 10px; border-right: 1px solid rgb(var(--c-border) / .4); background: rgb(var(--c-surface) / .3); }
  .rail-heading { display: flex; justify-content: space-between; padding: 0 6px 12px; font-size: 11px; font-weight: 600; color: rgb(var(--c-text-muted)); }
  .file-search { display: flex; align-items: center; gap: 7px; margin: 0 3px 12px; padding: 7px 8px; border: 1px solid rgb(var(--c-border) / .5); border-radius: 8px; color: rgb(var(--c-text-muted)); }
  .file-search input { width: 100%; min-width: 0; background: transparent; outline: none; font-size: 11px; }
  .file-search:focus-within { outline: 2px solid rgb(var(--c-accent)); }
  .file-list { overflow: auto; flex: 1; }
  .file-row { width: 100%; display: flex; align-items: center; gap: 9px; padding: 11px 9px; border-radius: 9px; margin-bottom: 4px; text-align: left; color: rgb(var(--c-text-muted)); }
  .file-row.current { color: rgb(var(--c-accent)); background: rgb(var(--c-accent) / .12); }
  .file-name { min-width: 0; flex: 1; }
  .file-name strong, .file-name small { display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .file-name strong { font-size: 12px; color: rgb(var(--c-text)); font-weight: 550; }
  .file-name small { margin-top: 4px; font-size: 10px; color: rgb(var(--c-text-muted)); }
  .file-dot { width: 5px; height: 5px; border-radius: 50%; background: #d79b45; }
  .rail-note { padding: 14px 6px 0; font-size: 10px; }
  .rail-empty { padding: 10px; line-height: 1.8; color: rgb(var(--c-text-muted)); }
  .editor-main { flex: 1; display: flex; flex-direction: column; min-width: 0; min-height: 0; overflow: hidden; }
  .file-toolbar { display: flex; align-items: center; justify-content: space-between; flex-wrap: wrap; gap: 10px; padding: 13px 18px; background: rgb(var(--c-surface) / .4); }
  .file-title { display: flex; gap: 8px; align-items: center; min-width: 0; flex: 1; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 11px; }
  .file-title span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .save-button { flex-shrink: 0; font-size: 11px; }
  .mobile-file-select { display: none; }
  .resolution-toolbar { display: flex; align-items: center; flex-wrap: wrap; gap: 12px; padding: 10px 18px; border-bottom: 1px solid rgb(var(--c-border) / .4); font-size: 11px; }
  .progress-copy { display: flex; gap: 10px; align-items: center; font-variant-numeric: tabular-nums; }
  progress { width: 62px; height: 4px; border: 0; border-radius: 4px; overflow: hidden; appearance: none; }
  progress::-webkit-progress-bar { background: rgb(var(--c-border) / .6); }
  progress::-webkit-progress-value { background: rgb(var(--c-accent)); }
  progress::-moz-progress-bar { background: rgb(var(--c-accent)); }
  .all-resolved { color: rgb(var(--c-accent)); }
  .chunk-navigation { display: flex; gap: 4px; align-items: center; }
  .icon-button { display: inline-flex; justify-content: center; align-items: center; padding: 5px; border-radius: 6px; color: rgb(var(--c-text-muted)); }
  .text-action { display: inline-flex; align-items: center; gap: 6px; padding: 4px; border-radius: 5px; color: rgb(var(--c-accent)); font-size: 11px; }
  .text-action span { color: rgb(var(--c-text-muted)); font-variant-numeric: tabular-nums; }
  button { cursor: pointer; transition: background-color 120ms ease; }
  button:hover:not(:disabled) { background-color: rgb(var(--c-accent) / .12); }
  button:disabled { opacity: .4; cursor: default; }
  button:focus-visible, summary:focus-visible, textarea:focus-visible, pre:focus-visible { outline: 2px solid rgb(var(--c-accent)); outline-offset: 2px; }
  .bulk-actions { margin-left: auto; position: relative; }
  summary { cursor: pointer; color: rgb(var(--c-text-muted)); font-size: 11px; }
  .bulk-actions > summary { padding: 5px 7px; border-radius: 6px; }
  .bulk-menu { position: absolute; right: 0; top: 30px; z-index: 5; width: 230px; background: rgb(var(--c-surface)); border: 1px solid rgb(var(--c-border)); border-radius: 10px; padding: 7px; box-shadow: 0 10px 30px #0003; }
  .bulk-menu p { margin: 5px 7px; font-size: 10px; color: rgb(var(--c-text-muted)); }
  .bulk-menu button { display: block; width: 100%; padding: 9px 7px; text-align: left; border-radius: 6px; font-size: 11px; }
  .editor-panes { display: grid; grid-template-columns: minmax(0, 1.65fr) minmax(260px, 1fr); flex: 1; min-height: 0; }
  .conflict-inspector { min-height: 0; overflow: auto; padding: 16px; scroll-padding-top: 16px; }
  .conflict-block { border: 1px solid rgb(var(--c-border) / .75); border-radius: 11px; overflow: hidden; margin-bottom: 14px; background: rgb(var(--c-surface) / .35); }
  .conflict-block.active-block { border-color: rgb(var(--c-accent) / .7); }
  .conflict-block:focus { outline: 2px solid rgb(var(--c-accent) / .5); outline-offset: 2px; }
  .block-heading, .block-heading > div { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .block-heading { justify-content: space-between; padding: 10px 12px; background: rgb(var(--c-surface-hover) / .4); }
  .block-heading strong { font-size: 11px; font-weight: 600; }
  .line-range { font-size: 10px; color: rgb(var(--c-text-muted)); }
  .choice-status { display: inline-flex; gap: 4px; align-items: center; font-size: 10px; color: rgb(var(--c-text-muted)); }
  .choice-status.resolved { color: rgb(var(--c-accent)); }
  .source-code { overflow: auto; padding: 10px 12px; margin: 0; font-size: 11px; line-height: 1.8; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; white-space: pre; user-select: text; }
  .custom-resolution, .base-context { border-top: 1px solid rgb(var(--c-border) / .4); }
  .custom-resolution > summary, .base-context > summary { padding: 11px 12px; }
  .custom-resolution summary span { margin-left: 8px; font-size: 10px; opacity: .8; }
  .custom-body { padding: 0 12px 10px; }
  textarea { width: 100%; resize: vertical; min-height: 78px; border: 1px solid rgb(var(--c-border) / .7); border-radius: 7px; background: rgb(var(--c-bg)); padding: 10px; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 11px; line-height: 1.7; user-select: text; }
  .custom-footer { display: flex; justify-content: space-between; flex-wrap: wrap; align-items: center; gap: 6px; color: rgb(var(--c-text-muted)); font-size: 10px; margin-top: 5px; }
  .unchanged-context { margin: 0 0 14px; }
  .unchanged-context summary { padding: 5px 2px; font-size: 10px; }
  .unchanged-context summary span { margin-left: 8px; opacity: .7; }
  .result-pane { display: flex; flex-direction: column; min-width: 0; min-height: 0; border-left: 1px solid rgb(var(--c-border) / .45); background: rgb(var(--c-surface) / .35); }
  .result-heading { display: flex; justify-content: space-between; align-items: center; padding: 17px 16px 6px; }
  .result-heading strong { display: block; font-size: 12px; font-weight: 550; margin-top: 4px; }
  .eyebrow { font-size: 9px; text-transform: uppercase; letter-spacing: .1em; color: rgb(var(--c-text-muted)); }
  .result-description { color: rgb(var(--c-text-muted)); font-size: 10px; padding: 0 16px 12px; line-height: 1.6; }
  .result-code { flex: 1; min-height: 0; overflow: auto; padding: 15px 16px; margin: 0; background: rgb(var(--c-bg) / .55); font: 11px/1.9 ui-monospace, SFMono-Regular, Menlo, monospace; white-space: pre; user-select: text; }
  .result-footer { display: flex; justify-content: space-between; flex-wrap: wrap; gap: 6px; padding: 10px 14px; font-size: 9px; color: rgb(var(--c-text-muted)); margin-top: auto; }
  .preview-expanded { grid-template-columns: minmax(0, 1fr); }
  .preview-expanded .conflict-inspector { display: none; }
  .editor-state { display: flex; flex: 1; flex-direction: column; justify-content: center; align-items: center; padding: 24px; gap: 10px; color: rgb(var(--c-text-muted)); overflow: auto; }
  .editor-state h3 { color: rgb(var(--c-text)); font-weight: 600; }
  .editor-state p { max-width: 600px; overflow-wrap: anywhere; text-align: center; }
  .error-notice, .source-notice { display: flex; align-items: flex-start; gap: 8px; padding: 11px 13px; margin: 10px 14px; background: rgb(var(--c-accent) / .07); border-radius: 8px; font-size: 11px; line-height: 1.7; overflow-wrap: anywhere; }
  .error-notice { color: light-dark(#a6223e, #fda4af); background: #f43f5e10; }
  .source-notice { color: rgb(var(--c-text-muted)); }
  .preview-state { display: flex; align-items: center; gap: 8px; padding: 22px 16px; font-size: 11px; color: rgb(var(--c-text-muted)); }
  .rebase-note { font-size: 10px; padding: 9px 18px; background: rgb(var(--c-accent) / .06); color: rgb(var(--c-text-muted)); line-height: 1.6; }
  @container (max-width: 1150px) {
    .file-rail { width: 178px; }
    .editor-panes { grid-template-columns: minmax(0, 1fr); grid-template-rows: minmax(150px, 1fr) minmax(150px, 32%); }
    .result-pane { border-left: 0; border-top: 1px solid rgb(var(--c-border) / .5); }
    .result-heading { padding: 10px 14px 4px; }
    .result-heading > div { display: flex; align-items: center; gap: 10px; }
    .result-heading strong { margin: 0; }
    .result-description { padding-bottom: 6px; }
    .preview-expanded { grid-template-rows: minmax(0, 1fr); }
  }
  @container (max-width: 760px) {
    .file-rail { display: none; }
    .file-title { display: none; }
    .mobile-file-select { display: block; min-width: 0; flex: 1; max-width: 100%; padding: 7px 24px 7px 9px; border: 1px solid rgb(var(--c-border) / .6); border-radius: 7px; background-color: rgb(var(--c-surface)); font-size: 11px; }
    .page-heading { padding: 14px; }
    .heading-copy p { display: none; }
    .heading-icon { width: 30px; height: 30px; }
    .file-count { font-size: 10px; }
    .file-toolbar, .resolution-toolbar { padding: 10px 12px; }
  }
  @container (max-width: 480px) {
    .file-count { display: none; }
    .custom-resolution summary span { display: none; }
    progress { display: none; }
  }
</style>
