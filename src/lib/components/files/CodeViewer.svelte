<script lang="ts">
  import { untrack } from "svelte";
  import { hostPlatform } from "../../stores/platformStore";
  import { shortcutTextLabel } from "../../ui/platformCopy";
  import { repoStore } from "../../stores/repoStore";
  import { densityStore } from "../../stores/densityStore";
  import { CODE_ZOOM_MAX, CODE_ZOOM_MIN, rowHeight, scaledRowHeight } from "../../ui/density";
  import { invoke } from "@tauri-apps/api/core";
  import {
    detectLanguageFromPath,
    tokenizeLineWithCarry,
    createCarryIndex,
    tokenClass,
    type SupportedLanguage,
    type SyntaxToken,
  } from "../../files/syntaxHighlight";
  import {
    highlightDocument,
    usesTreeSitter,
  } from "../../diff/highlight";
  import { copyText } from "../../desktop/clipboard";
  import { formatError } from "../../ui/formatError";
  import { askConfirm } from "../../stores/modalStore";
  import { createAsyncGuard } from "../../async/guard";
  import {
    Search,
    ChevronUp,
    ChevronDown,
    WrapText,
    Copy,
    Check,
    Edit3,
    Save,
    RotateCcw,
    Hash,
  } from "@lucide/svelte";
  import VirtualList from "../VirtualList.svelte";
  import ScrollCue from "../ScrollCue.svelte";
  import { findMatches, matchLabel, stepMatch } from "../../text/lineSearch";
  import { debounce } from "../../async/debounce";
  import { SEARCH_DEBOUNCE_MS } from "../../files/searchLimits";
  import { consumeReveal } from "../../files/revealRequests";

  let {
    filePath,
    content,
    readOnly = false,
    draftContent = null,
    dirty = false,
    onSave,
    onDraftChange,
    onRequestDiscard,
  }: {
    filePath: string;
    content: string;
    readOnly?: boolean;
    draftContent?: string | null;
    dirty?: boolean;
    onSave?: (newContent: string) => Promise<void>;
    onDraftChange?: (newContent: string, sourceContent: string) => void;
    onRequestDiscard?: () => Promise<boolean>;
  } = $props();

  let ROW_HEIGHT = $derived(rowHeight("code", $densityStore));
  let actionBar: HTMLDivElement | undefined = $state();
  let searchBar: HTMLDivElement | undefined = $state();
  let goToBar: HTMLDivElement | undefined = $state();
  let statusBar: HTMLDivElement | undefined = $state();
  const OVERSCAN = 20;
  const MAX_RENDER_LINES = 80_000;

  let isEditing = $state(false);
  let editDraft = $state("");
  let isSaving = $state(false);
  let saveSuccess = $state(false);
  let copied = $state(false);
  let previousFilePath = "";
  let editorGeneration = 0;

  let wordWrap = $state(false);
  let showWhitespace = $state(false);
  let zoomPercent = $state(100);
  /** One height for the virtual slot and the row box, so they cannot overlap. */
  let rowPx = $derived(scaledRowHeight(ROW_HEIGHT, zoomPercent));

  let selectedLine = $state<number | null>(null);
  let selectedLineEnd = $state<number | null>(null);

  // In-file search state
  let isSearchOpen = $state(false);
  let searchQuery = $state("");
  let isCaseSensitive = $state(false);
  let isRegex = $state(false);
  let currentMatchIdx = $state(0);
  let searchInputEl: HTMLInputElement | undefined = $state();

  let goToLineOpen = $state(false);
  let targetLineInput = $state("");

  let scrollTop = $state(0);

  let language = $derived<SupportedLanguage>(detectLanguageFromPath(filePath));
  let hasUnsavedChanges = $derived(dirty || (isEditing && editDraft !== content));

  // One split, not two. `linesTruncated` used to re-split the whole file just
  // to compare a length and then throw the array away — a second full pass and
  // a second full allocation, on a string that can be 80,000 lines long, every
  // time either input changed.
  let splitFile = $derived.by(() => {
    const text = isEditing ? editDraft : content;
    const lines = text.split("\n");
    const truncated = lines.length > MAX_RENDER_LINES;
    return { lines: truncated ? lines.slice(0, MAX_RENDER_LINES) : lines, truncated };
  });
  let rawLines = $derived(splitFile.lines);
  let linesTruncated = $derived(splitFile.truncated);

  /**
   * Where each line starts: inside a block comment, a template literal, or
   * ordinary code.
   *
   * Rebuilt whenever the text or the language changes, and extended lazily as
   * the window moves — a virtualized row cannot be coloured correctly from its
   * own text alone, because the construct that governs it may have opened
   * thousands of lines earlier.
   */
  let carryAt = $derived.by(() => createCarryIndex(splitFile.lines, language));
  let byteSize = $derived(content.length);

  /**
   * Whole-file tree-sitter tokens when MarkDev has a grammar for this
   * language. `null` means "use the regex tokenizer" — either no grammar, or
   * the IPC call failed / is still in flight. Never treat an empty array as
   * "highlighted": that would paint a tree-sitter language as plain text
   * while the request is outstanding.
   */
  let treeSitterLines = $state<SyntaxToken[][] | null>(null);
  let treeSitterGuard: ReturnType<typeof createAsyncGuard> | null = null;

  $effect(() => {
    const lang = language;
    const text = isEditing ? editDraft : content;
    treeSitterGuard?.cancel();
    treeSitterLines = null;
    if (!usesTreeSitter(lang) || isEditing) return;
    const guard = createAsyncGuard();
    treeSitterGuard = guard;
    void highlightDocument(lang, text).then((lines) => {
      if (!guard.isLive()) return;
      treeSitterLines = lines;
    });
  });

  function tokensForLine(line: string, lineIdx: number): SyntaxToken[] {
    const cached = treeSitterLines?.[lineIdx];
    if (cached) return cached;
    return tokenizeLineWithCarry(line ?? "", language, carryAt(lineIdx)).tokens;
  }

  /**
   * Debounced copy of the query. `searchQuery` is bound to the input, so the
   * scan would otherwise re-run on every keystroke over the whole file.
   */
  let debouncedQuery = $state("");
  const applySearchQuery = debounce((q: string) => (debouncedQuery = q), SEARCH_DEBOUNCE_MS);
  $effect(() => {
    const next = searchQuery;
    // An emptied box clears immediately: waiting to REMOVE highlighting reads
    // as lag, and costs nothing to do now.
    if (next === "") {
      applySearchQuery.cancel();
      debouncedQuery = "";
      return;
    }
    applySearchQuery(next);
  });

  // Search matches across lines. The loop this replaced never advanced past a
  // zero-length match, so a pattern like `a*` or `\b` — anything a user can
  // type into a regex box — spun forever. `text/lineSearch` owns it now, and
  // the diff viewer's find bar runs the same code: it also refuses a pattern
  // whose nesting can backtrack catastrophically, bounds the scan with a
  // deadline, and caps the match list while reporting the cap honestly.
  let searchResult = $derived(
    findMatches(rawLines, debouncedQuery, { caseSensitive: isCaseSensitive, regex: isRegex }),
  );
  let searchMatches = $derived(searchResult.matches);

  let matchCount = $derived(searchMatches.length);

  function nextMatch() {
    const next = stepMatch(currentMatchIdx, matchCount, 1);
    if (next < 0) return;
    currentMatchIdx = next;
    scrollToMatch(next);
  }

  function prevMatch() {
    const next = stepMatch(currentMatchIdx, matchCount, -1);
    if (next < 0) return;
    currentMatchIdx = next;
    scrollToMatch(next);
  }

  function scrollToMatch(idx: number) {
    const match = searchMatches[idx];
    if (!match) return;
    selectedLine = match.lineIndex + 1;
    selectedLineEnd = null;
    scrollToLine(match.lineIndex);
  }

  function scrollToLine(lineIdx: number) {
    scrollTop = Math.max(0, lineIdx * rowPx - 80);
  }

  function handleGoToLine() {
    const num = parseInt(targetLineInput.trim(), 10);
    if (!isNaN(num) && num >= 1 && num <= rawLines.length) {
      selectedLine = num;
      selectedLineEnd = null;
      scrollToLine(num - 1);
      goToLineOpen = false;
      targetLineInput = "";
    }
  }

  function handleLineClick(lineNum: number, event: MouseEvent) {
    if (event.shiftKey && selectedLine !== null) {
      selectedLineEnd = lineNum;
    } else {
      selectedLine = lineNum;
      selectedLineEnd = null;
    }
  }

  function startEdit() {
    if (readOnly) return;
    editDraft = draftContent ?? content;
    isEditing = true;
  }

  async function cancelEdit() {
    const path = filePath;
    const generation = editorGeneration;
    if (hasUnsavedChanges) {
      const confirmed = onRequestDiscard
        ? await onRequestDiscard()
        : await askConfirm({
            title: "Discard Unsaved Edits?",
            message: `Discard the unsaved editor draft for ${path}?`,
            confirmLabel: "Discard Unsaved Edits",
            cancelLabel: "Keep Editing",
          });
      if (!confirmed || filePath !== path || editorGeneration !== generation) return;
    }
    isEditing = false;
    editDraft = content;
  }

  function onEditInput(event: Event) {
    const value = (event.currentTarget as HTMLTextAreaElement).value;
    editDraft = value;
    onDraftChange?.(value, content);
  }

  async function saveChanges() {
    if (readOnly || !isEditing || isSaving) return;
    const path = filePath;
    const generation = editorGeneration;
    const contentToSave = editDraft;
    isSaving = true;
    try {
      if (onSave) {
        await onSave(contentToSave);
      } else {
        const repo = $repoStore.currentPath;
        if (!repo) throw new Error("No active repository");
        await invoke("cmd_write_file_content", {
          repoPath: repo,
          filePath: path,
          content: contentToSave,
        });
        await repoStore.refresh();
      }
      if (filePath !== path || editorGeneration !== generation) return;
      // A newer input should remain visibly dirty even if an older write just
      // completed. The parent applies the same saved-content comparison.
      if (editDraft !== contentToSave) return;
      isEditing = false;
      saveSuccess = true;
      setTimeout(() => (saveSuccess = false), 2000);
    } catch (err: unknown) {
      repoStore.setError(formatError(err));
    } finally {
      if (filePath === path && editorGeneration === generation) isSaving = false;
    }
  }

  async function handleCopy() {
    const textToCopy = isEditing ? editDraft : content;
    if (!(await copyText(textToCopy))) {
      repoStore.setError("Could not copy file content to clipboard");
      return;
    }
    copied = true;
    setTimeout(() => (copied = false), 1800);
  }

  function handleKeydown(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "f") {
      e.preventDefault();
      isSearchOpen = true;
      setTimeout(() => searchInputEl?.focus(), 50);
      return;
    }
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "g") {
      e.preventDefault();
      goToLineOpen = true;
      return;
    }
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "s") {
      if (isEditing) {
        e.preventDefault();
        void saveChanges();
      }
      return;
    }
    if (e.key === "Escape") {
      if (goToLineOpen) {
        goToLineOpen = false;
        return;
      }
      if (isSearchOpen) {
        isSearchOpen = false;
        searchQuery = "";
        return;
      }
    }
  }

  $effect(() => {
    const path = filePath;
    const restored = draftContent;
    const source = content;
    if (path !== previousFilePath) {
      previousFilePath = path;
      untrack(() => {
        editorGeneration += 1;
        editDraft = restored ?? source;
        isEditing = restored !== null;
        isSaving = false;
        saveSuccess = false;
      });
      return;
    }
    if (restored !== null && !isEditing) {
      untrack(() => {
        editDraft = restored;
        isEditing = true;
      });
    } else if (restored === null && !isEditing) {
      untrack(() => {
        editDraft = source;
      });
    }
  });

  /**
   * Lands on a line someone else named — today, a `path:line:col` reference
   * clicked in the terminal.
   *
   * Gated on the line count rather than on the path alone: the request is
   * recorded before the file is read, so acting on arrival would scroll a
   * viewer that has no rows yet and land at zero. `rawLines.length` becoming
   * non-zero is the earliest moment the destination exists.
   *
   * `consumeReveal` is keyed on this viewer's own path, so a request meant for
   * another file is left in place rather than eaten here, and it clears on
   * collection — which is what keeps this effect from re-firing every time the
   * draft changes a keystroke at a time.
   */
  $effect(() => {
    const path = filePath;
    const lineCount = rawLines.length;
    if (!path || lineCount === 0) return;
    const target = consumeReveal(path);
    if (!target) return;
    // A reference can name a line past the end of the file it was written
    // about; clamp rather than refuse, so the file still opens near the mark.
    const line = Math.min(target.line, lineCount);
    untrack(() => {
      selectedLine = line;
      selectedLineEnd = null;
      scrollToLine(line - 1);
    });
  });

  // Calculate indentation stats
  let indentInfo = $derived.by(() => {
    let twoSpaces = 0;
    let fourSpaces = 0;
    let tabs = 0;
    for (const l of rawLines.slice(0, 50)) {
      if (l.startsWith("\t")) tabs++;
      else if (l.startsWith("    ")) fourSpaces++;
      else if (l.startsWith("  ")) twoSpaces++;
    }
    if (tabs > twoSpaces && tabs > fourSpaces) return "Tabs";
    if (fourSpaces > twoSpaces) return "Spaces: 4";
    return "Spaces: 2";
  });
</script>

<div
  class="flex flex-col h-full bg-background font-sans text-xs min-h-0 relative overflow-hidden select-text"
  onkeydown={handleKeydown}
  tabindex="0"
  role="textbox"
  aria-multiline="true"
  aria-readonly={isEditing ? "false" : "true"}
  aria-label="Code Viewer"
>
  <!--
    Editor chrome. The two clusters used to share one justify-between row and
    painted over each other, and over the first source line, once the editor
    was narrower than their content. The row now scrolls, and tips from it
    open above the bar so they do not cover the file.
  -->
  <div
    class="relative shrink-0 border-b border-border/70 gp-section-edge bg-surface/80 select-none"
    data-tip-place="above"
  >
    <div bind:this={actionBar} class="gp-header-scroll">
      <div class="flex items-center justify-between gap-2 px-3 py-1.5 min-w-max w-full">
    <div class="flex items-center gap-2 shrink-0">
      <span class="text-[11px] font-mono text-textMuted">{rawLines.length}{linesTruncated ? "+" : ""} lines</span>
      <span class="text-textMuted/40">•</span>
      <span class="text-[11px] font-mono text-textMuted">{(byteSize / 1024).toFixed(1)} KB</span>
      <span class="text-textMuted/40">•</span>
      <span class="text-[10px] font-mono uppercase px-1.5 py-0.5 rounded bg-accent/15 text-accent font-semibold">{language}</span>
      {#if hasUnsavedChanges}
        <span class="text-[10px] font-semibold text-amber-400" role="status">Unsaved</span>
      {/if}

      <button
        type="button"
        onclick={() => (isSearchOpen = !isSearchOpen)}
        class="gp-btn py-0.5! px-2! ml-2 flex items-center gap-1 text-[11px] {isSearchOpen ? 'border-accent/60 bg-accent/15 text-accent' : ''}"
      >
        <Search size={11} />
        <span>Find</span>
        <span class="gp-keycap text-[9px]!">{shortcutTextLabel("⌘F", $hostPlatform.os)}</span>
      </button>

      <button
        type="button"
        onclick={() => (goToLineOpen = true)}
        class="gp-btn py-0.5! px-2! flex items-center gap-1 text-[11px]"
      >
        <Hash size={11} />
        <span>Go to Line</span>
        <span class="gp-keycap text-[9px]!">{shortcutTextLabel("⌘G", $hostPlatform.os)}</span>
      </button>
    </div>

    <div class="flex items-center gap-1.5 shrink-0">
      <button
        type="button"
        onclick={() => (wordWrap = !wordWrap)}
        class="gp-icon-btn p-1.5! {wordWrap ? 'text-accent bg-accent/15' : 'text-textMuted hover:text-textPrimary'}"
        title="Word wrap (edit mode)"
      >
        <WrapText size={13} />
      </button>

      <button
        type="button"
        onclick={() => (showWhitespace = !showWhitespace)}
        class="gp-icon-btn p-1.5! {showWhitespace ? 'text-accent bg-accent/15' : 'text-textMuted hover:text-textPrimary'}"
        title="Toggle Whitespace Indicators"
      >
        <span class="font-mono text-[11px] font-bold">·_</span>
      </button>

      <!-- Zoom Controls -->
      <div class="flex items-center rounded-full border border-border/70 bg-surface px-1.5 py-0.5 gap-1">
        <button
          type="button"
          onclick={() => (zoomPercent = Math.max(CODE_ZOOM_MIN, zoomPercent - 10))}
          class="text-textMuted hover:text-textPrimary text-[10px] px-1"
          title="Zoom out"
        >−</button>
        <span class="text-[10px] font-mono text-textMuted min-w-8 text-center">{zoomPercent}%</span>
        <button
          type="button"
          onclick={() => (zoomPercent = Math.min(CODE_ZOOM_MAX, zoomPercent + 10))}
          class="text-textMuted hover:text-textPrimary text-[10px] px-1"
          title="Zoom in"
        >+</button>
      </div>

      <div class="h-3.5 w-1 rounded-full bg-border/50 mx-1" aria-hidden="true"></div>

      <!-- Edit & Save Controls -->
      {#if !isEditing && !readOnly}
        <button
          type="button"
          onclick={startEdit}
          class="gp-btn py-1! px-2.5! flex items-center gap-1 text-[11px]"
        >
          <Edit3 size={12} class="text-accent" />
          <span>Edit</span>
        </button>
      {:else if isEditing && !readOnly}
        <button
          type="button"
          onclick={cancelEdit}
          class="gp-btn py-1! px-2.5! flex items-center gap-1 text-[11px] text-textMuted"
        >
          <RotateCcw size={12} />
          <span>Cancel</span>
        </button>
        <button
          type="button"
          onclick={saveChanges}
          disabled={isSaving || !hasUnsavedChanges}
          class="gp-btn-primary py-1! px-3! flex items-center gap-1 text-[11px]"
        >
          {#if isSaving}
            <span class="animate-spin text-xs">⏳</span>
          {:else}
            <Save size={12} />
          {/if}
          <span>Save ({shortcutTextLabel("⌘S", $hostPlatform.os)})</span>
        </button>
      {/if}

      <button
        type="button"
        onclick={handleCopy}
        class="gp-btn py-1! px-2.5! flex items-center gap-1 text-[11px]"
        title="Copy whole file content"
      >
        {#if copied}
          <Check size={12} class="text-emerald-400" />
          <span class="text-emerald-400 font-semibold">Copied</span>
        {:else}
          <Copy size={12} class="text-textMuted" />
          <span>Copy</span>
        {/if}
      </button>
    </div>
      </div>
    </div>
    <ScrollCue target={actionBar} axis="x" />
  </div>

  <!-- Search Bar -->
  {#if isSearchOpen}
    <div class="relative shrink-0 border-b border-border/80 gp-section-edge bg-surface select-none" data-tip-place="above">
      <div bind:this={searchBar} class="gp-header-scroll">
        <div class="flex items-center justify-between gap-3 px-3 py-2 min-w-max w-full">
      <div class="flex items-center gap-2 shrink-0 min-w-48 max-w-md flex-1">
        <div class="flex items-center gap-1.5 bg-background border border-border rounded-full px-2.5 py-1 flex-1 focus-within:border-accent/70">
          <Search size={12} class="text-textMuted shrink-0" />
          <input
            bind:this={searchInputEl}
            type="text"
            bind:value={searchQuery}
            onkeydown={(e) => {
              if (e.key === "Enter") {
                if (e.shiftKey) prevMatch();
                else nextMatch();
              }
            }}
            placeholder="Find in file..."
            class="w-full bg-transparent text-xs text-textPrimary placeholder:text-textMuted/60 focus:outline-hidden"
          />
          {#if searchQuery}
            <span class="text-[10px] font-mono text-textMuted shrink-0">
              {matchLabel(searchResult, currentMatchIdx)}
            </span>
          {/if}
        </div>

        <div class="flex items-center gap-1">
          <button
            type="button"
            onclick={() => (isCaseSensitive = !isCaseSensitive)}
            class="px-2 py-0.5 text-[10px] font-mono rounded border transition-colors {isCaseSensitive
              ? 'bg-accent/20 border-accent/40 text-accent font-bold'
              : 'border-border/60 text-textMuted hover:text-textPrimary'}"
            title="Match Case"
          >Aa</button>
          <button
            type="button"
            onclick={() => (isRegex = !isRegex)}
            class="px-2 py-0.5 text-[10px] font-mono rounded border transition-colors {isRegex
              ? 'bg-accent/20 border-accent/40 text-accent font-bold'
              : 'border-border/60 text-textMuted hover:text-textPrimary'}"
            title="Use Regular Expression"
          >.*</button>
        </div>
      </div>

      <div class="flex items-center gap-1">
        <button
          type="button"
          onclick={prevMatch}
          disabled={matchCount === 0}
          class="gp-btn py-1! px-2! flex items-center gap-1"
          title="Previous match (Shift+Enter)"
        >
          <ChevronUp size={12} />
        </button>
        <button
          type="button"
          onclick={nextMatch}
          disabled={matchCount === 0}
          class="gp-btn py-1! px-2! flex items-center gap-1"
          title="Next match (Enter)"
        >
          <ChevronDown size={12} />
        </button>
        <button
          type="button"
          onclick={() => { isSearchOpen = false; searchQuery = ""; }}
          class="gp-icon-btn p-1! text-textMuted hover:text-textPrimary"
          aria-label="Close find"
        >✕</button>
      </div>
        </div>
      </div>
      <ScrollCue target={searchBar} axis="x" />
    </div>
  {/if}

  {#if goToLineOpen}
    <div class="relative shrink-0 border-b border-border/70 bg-surface select-none">
      <div bind:this={goToBar} class="gp-header-scroll">
        <div class="flex items-center gap-2 px-3 py-2 min-w-max">
      <span class="text-xs text-textMuted shrink-0">Go to line (1–{rawLines.length}):</span>
      <input
        type="number"
        min="1"
        max={rawLines.length}
        bind:value={targetLineInput}
        onkeydown={(e) => {
          if (e.key === "Enter") handleGoToLine();
          if (e.key === "Escape") goToLineOpen = false;
        }}
        placeholder="Line number"
        class="gp-field w-24!"
      />
      <button type="button" class="gp-btn-primary py-1! px-3! shrink-0" onclick={handleGoToLine}>Go</button>
      <button type="button" class="gp-btn py-1! px-2! shrink-0" onclick={() => (goToLineOpen = false)}>Cancel</button>
        </div>
      </div>
      <ScrollCue target={goToBar} axis="x" />
    </div>
  {/if}

  <!-- Main Code Surface -->
  <div class="flex-1 min-h-0 relative overflow-hidden bg-background flex flex-col">
    {#if linesTruncated && !isEditing}
      <div class="shrink-0 px-3 py-1.5 text-[11px] text-amber-300 bg-amber-500/10 border-b border-amber-500/30">
        Showing the first {MAX_RENDER_LINES.toLocaleString()} lines. Open the file externally to view the rest.
      </div>
    {/if}
    {#if isEditing}
      <!-- Inline Code Editor Mode -->
      <textarea
        value={editDraft}
        oninput={onEditInput}
        disabled={isSaving}
        spellcheck="false"
        class="flex-1 w-full h-full p-4 bg-background font-mono text-xs text-textPrimary leading-relaxed focus:outline-hidden resize-none border-none {wordWrap ? 'whitespace-pre-wrap' : 'whitespace-pre overflow-x-auto'}"
        style="font-size: {0.75 * (zoomPercent / 100)}rem;"
      ></textarea>
    {:else}
      <!-- Read-Only Syntax Highlighted View -->
      <div class="flex-1 w-full min-h-0">
        <div
          class="font-mono text-xs min-w-full h-full"
          style="font-size: {0.75 * (zoomPercent / 100)}rem;"
        >
          <VirtualList
            items={rawLines}
            rowHeight={rowPx}
            overscan={OVERSCAN}
            contentWidth
            bind:scrollTop
            class="h-full"
          >
            {#snippet row(line, lineIdx)}
              {@const lineNum = lineIdx + 1}
              {@const isHighlighted = selectedLine !== null &&
                (selectedLineEnd === null
                  ? selectedLine === lineNum
                  : lineNum >= Math.min(selectedLine, selectedLineEnd) && lineNum <= Math.max(selectedLine, selectedLineEnd))}
              {@const tokens = tokensForLine(line ?? "", lineIdx)}
              <div
                class="relative flex items-center w-max min-w-full overflow-hidden {isHighlighted
                  ? 'bg-accent/15'
                  : 'hover:bg-surface/50'}"
                style:height="{rowPx}px"
                style:line-height="{rowPx}px"
              >
                {#if isHighlighted}
                  <span class="absolute inset-y-0 left-0 w-[2px] bg-accent" aria-hidden="true"></span>
                {/if}
                <button
                  type="button"
                  onclick={(e) => handleLineClick(lineNum, e)}
                  class="w-12 shrink-0 self-stretch text-right pr-3 pl-1 select-none text-[11px] font-mono text-textMuted/60 hover:text-textPrimary transition-colors cursor-pointer"
                >
                  {lineNum}
                </button>

                <div class="pr-4 whitespace-pre">
                  {#if tokens.length === 0}
                    <span>&nbsp;</span>
                  {:else}
                    {#each tokens as token}
                      <span class={tokenClass(token.type)}>
                        {#if showWhitespace}
                          {token.text.replace(/ /g, '·').replace(/\t/g, '→   ')}
                        {:else}
                          {token.text}
                        {/if}
                      </span>
                    {/each}
                  {/if}
                </div>
              </div>
            {/snippet}
          </VirtualList>
        </div>
      </div>
    {/if}
  </div>

  <!-- Bottom Status Bar -->
  <div class="relative shrink-0 border-t border-border/70 gp-section-edge bg-surface/90 text-[10px] font-mono text-textMuted select-none">
    <div bind:this={statusBar} class="gp-header-scroll">
      <div class="flex items-center justify-between gap-3 px-3 py-1 min-w-max w-full">
    <div class="flex items-center gap-3 shrink-0">
      <span>Ln {selectedLine ?? 1}, Col 1</span>
      <span>•</span>
      <span>{indentInfo}</span>
      <span>•</span>
      <span>UTF-8</span>
      {#if saveSuccess}
        <span class="text-emerald-400 font-bold flex items-center gap-1">✓ Saved</span>
      {/if}
    </div>
    <div class="flex items-center gap-3 shrink-0">
      <span>{rawLines.length} lines</span>
      <span>•</span>
      <span class="text-accent font-semibold">{language.toUpperCase()}</span>
    </div>
      </div>
    </div>
    <ScrollCue target={statusBar} axis="x" />
  </div>
</div>
