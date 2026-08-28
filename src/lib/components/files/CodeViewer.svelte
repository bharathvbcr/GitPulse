<script lang="ts">
  import { repoStore } from "../../stores/repoStore";
  import { invoke } from "@tauri-apps/api/core";
  import {
    detectLanguageFromPath,
    tokenizeLine,
    tokenClass,
    type SupportedLanguage,
  } from "../../files/syntaxHighlight";
  import { copyText } from "../../desktop/clipboard";
  import { formatError } from "../../ui/formatError";
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
  } from "lucide-svelte";
  import VirtualList from "../VirtualList.svelte";

  let {
    filePath,
    content,
    onSave,
  }: {
    filePath: string;
    content: string;
    onSave?: (newContent: string) => Promise<void>;
  } = $props();

  const ROW_HEIGHT = 20;
  const OVERSCAN = 20;
  const MAX_RENDER_LINES = 80_000;

  let isEditing = $state(false);
  let editDraft = $state("");
  let isSaving = $state(false);
  let saveSuccess = $state(false);
  let copied = $state(false);

  let wordWrap = $state(false);
  let showWhitespace = $state(false);
  let zoomPercent = $state(100);

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

  let rawLines = $derived.by(() => {
    const text = isEditing ? editDraft : content;
    const lines = text.split("\n");
    return lines.length > MAX_RENDER_LINES ? lines.slice(0, MAX_RENDER_LINES) : lines;
  });
  let linesTruncated = $derived.by(() => {
    const text = isEditing ? editDraft : content;
    return text.split("\n").length > MAX_RENDER_LINES;
  });
  let byteSize = $derived(content.length);

  // Search matches across lines
  let searchMatches = $derived.by<Array<{ lineIdx: number; colStart: number; length: number }>>(() => {
    if (!searchQuery.trim()) return [];
    const matches: Array<{ lineIdx: number; colStart: number; length: number }> = [];
    const lines = rawLines;

    try {
      let matcher: RegExp;
      if (isRegex) {
        matcher = new RegExp(searchQuery, isCaseSensitive ? "g" : "gi");
      } else {
        const escaped = searchQuery.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
        matcher = new RegExp(escaped, isCaseSensitive ? "g" : "gi");
      }

      for (let l = 0; l < lines.length; l++) {
        const line = lines[l];
        matcher.lastIndex = 0;
        let match: RegExpExecArray | null;
        while ((match = matcher.exec(line)) !== null) {
          matches.push({
            lineIdx: l,
            colStart: match.index,
            length: match[0].length,
          });
          if (!matcher.global) break;
        }
      }
    } catch {
      // Invalid regex - gracefully return empty
    }
    return matches;
  });

  let matchCount = $derived(searchMatches.length);

  function nextMatch() {
    if (matchCount === 0) return;
    currentMatchIdx = (currentMatchIdx + 1) % matchCount;
    scrollToMatch(currentMatchIdx);
  }

  function prevMatch() {
    if (matchCount === 0) return;
    currentMatchIdx = (currentMatchIdx - 1 + matchCount) % matchCount;
    scrollToMatch(currentMatchIdx);
  }

  function scrollToMatch(idx: number) {
    const match = searchMatches[idx];
    if (!match) return;
    selectedLine = match.lineIdx + 1;
    selectedLineEnd = null;
    scrollToLine(match.lineIdx);
  }

  function scrollToLine(lineIdx: number) {
    const rowH = Math.round(ROW_HEIGHT * (zoomPercent / 100));
    scrollTop = Math.max(0, lineIdx * rowH - 80);
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
    editDraft = content;
    isEditing = true;
  }

  function cancelEdit() {
    isEditing = false;
    editDraft = content;
  }

  async function saveChanges() {
    if (!isEditing || isSaving) return;
    isSaving = true;
    try {
      if (onSave) {
        await onSave(editDraft);
      } else {
        const repo = $repoStore.currentPath;
        if (!repo) throw new Error("No active repository");
        await invoke("cmd_write_file_content", {
          repoPath: repo,
          filePath,
          content: editDraft,
        });
        await repoStore.refresh();
      }
      isEditing = false;
      saveSuccess = true;
      setTimeout(() => (saveSuccess = false), 2000);
    } catch (err: unknown) {
      repoStore.setError(formatError(err));
    } finally {
      isSaving = false;
    }
  }

  async function handleCopy() {
    const textToCopy = isEditing ? editDraft : content;
    await copyText(textToCopy);
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
    content;
    if (!isEditing) {
      editDraft = content;
    }
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
  class="flex flex-col h-full bg-background font-sans text-xs min-h-0 relative select-text"
  onkeydown={handleKeydown}
  tabindex="0"
  role="region"
  aria-label="Code Viewer"
>
  <!-- Top Editor Actions Bar -->
  <div class="flex items-center justify-between px-3 py-1.5 border-b border-border/70 bg-surface/70 shrink-0 select-none">
    <!-- Left: File stats and Search button -->
    <div class="flex items-center gap-2 min-w-0">
      <span class="text-[11px] font-mono text-textMuted">{rawLines.length}{linesTruncated ? "+" : ""} lines</span>
      <span class="text-textMuted/40">•</span>
      <span class="text-[11px] font-mono text-textMuted">{(byteSize / 1024).toFixed(1)} KB</span>
      <span class="text-textMuted/40">•</span>
      <span class="text-[10px] font-mono uppercase px-1.5 py-0.5 rounded bg-accent/15 text-accent font-semibold">{language}</span>

      <button
        type="button"
        onclick={() => (isSearchOpen = !isSearchOpen)}
        class="gp-btn !py-0.5 !px-2 ml-2 flex items-center gap-1 text-[11px] {isSearchOpen ? 'border-accent/60 bg-accent/15 text-accent' : ''}"
      >
        <Search size={11} />
        <span>Find</span>
        <span class="gp-keycap !text-[9px]">⌘F</span>
      </button>

      <button
        type="button"
        onclick={() => (goToLineOpen = true)}
        class="gp-btn !py-0.5 !px-2 flex items-center gap-1 text-[11px]"
      >
        <Hash size={11} />
        <span>Go to Line</span>
        <span class="gp-keycap !text-[9px]">⌘G</span>
      </button>
    </div>

    <!-- Right: View Controls (Wrap, Whitespace, Zoom, Edit, Copy) -->
    <div class="flex items-center gap-1.5">
      <button
        type="button"
        onclick={() => (wordWrap = !wordWrap)}
        class="gp-icon-btn !p-1.5 {wordWrap ? 'text-accent bg-accent/15' : 'text-textMuted hover:text-textPrimary'}"
        title="Word wrap (edit mode)"
      >
        <WrapText size={13} />
      </button>

      <button
        type="button"
        onclick={() => (showWhitespace = !showWhitespace)}
        class="gp-icon-btn !p-1.5 {showWhitespace ? 'text-accent bg-accent/15' : 'text-textMuted hover:text-textPrimary'}"
        title="Toggle Whitespace Indicators"
      >
        <span class="font-mono text-[11px] font-bold">·_</span>
      </button>

      <!-- Zoom Controls -->
      <div class="flex items-center rounded-full border border-border/70 bg-surface px-1.5 py-0.5 gap-1">
        <button
          type="button"
          onclick={() => (zoomPercent = Math.max(70, zoomPercent - 10))}
          class="text-textMuted hover:text-textPrimary text-[10px] px-1"
          title="Zoom out"
        >−</button>
        <span class="text-[10px] font-mono text-textMuted min-w-8 text-center">{zoomPercent}%</span>
        <button
          type="button"
          onclick={() => (zoomPercent = Math.min(160, zoomPercent + 10))}
          class="text-textMuted hover:text-textPrimary text-[10px] px-1"
          title="Zoom in"
        >+</button>
      </div>

      <div class="h-3.5 w-px bg-border/80 mx-1"></div>

      <!-- Edit & Save Controls -->
      {#if !isEditing}
        <button
          type="button"
          onclick={startEdit}
          class="gp-btn !py-1 !px-2.5 flex items-center gap-1 text-[11px]"
        >
          <Edit3 size={12} class="text-accent" />
          <span>Edit</span>
        </button>
      {:else}
        <button
          type="button"
          onclick={cancelEdit}
          class="gp-btn !py-1 !px-2.5 flex items-center gap-1 text-[11px] text-textMuted"
        >
          <RotateCcw size={12} />
          <span>Cancel</span>
        </button>
        <button
          type="button"
          onclick={saveChanges}
          disabled={isSaving}
          class="gp-btn-primary !py-1 !px-3 flex items-center gap-1 text-[11px]"
        >
          {#if isSaving}
            <span class="animate-spin text-xs">⏳</span>
          {:else}
            <Save size={12} />
          {/if}
          <span>Save (⌘S)</span>
        </button>
      {/if}

      <button
        type="button"
        onclick={handleCopy}
        class="gp-btn !py-1 !px-2.5 flex items-center gap-1 text-[11px]"
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

  <!-- Search Bar Dropdown -->
  {#if isSearchOpen}
    <div class="px-3 py-2 bg-surface border-b border-border/80 flex items-center justify-between gap-3 shrink-0 shadow-md select-none animate-in fade-in duration-100">
      <div class="flex items-center gap-2 flex-1 max-w-md">
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
            class="w-full bg-transparent text-xs text-textPrimary placeholder:text-textMuted/60 focus:outline-none"
          />
          {#if searchQuery}
            <span class="text-[10px] font-mono text-textMuted shrink-0">
              {matchCount > 0 ? `${currentMatchIdx + 1} of ${matchCount}` : "0 matches"}
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
          class="gp-btn !py-1 !px-2 flex items-center gap-1"
          title="Previous match (Shift+Enter)"
        >
          <ChevronUp size={12} />
        </button>
        <button
          type="button"
          onclick={nextMatch}
          disabled={matchCount === 0}
          class="gp-btn !py-1 !px-2 flex items-center gap-1"
          title="Next match (Enter)"
        >
          <ChevronDown size={12} />
        </button>
        <button
          type="button"
          onclick={() => { isSearchOpen = false; searchQuery = ""; }}
          class="gp-icon-btn !p-1 text-textMuted hover:text-textPrimary"
        >✕</button>
      </div>
    </div>
  {/if}

  <!-- Go to Line Modal / Overlay -->
  {#if goToLineOpen}
    <div class="absolute top-10 left-1/2 -translate-x-1/2 gp-pop rounded-xl bg-surface border border-border p-3 shadow-float flex items-center gap-2 z-30">
      <span class="text-xs text-textMuted">Go to line (1–{rawLines.length}):</span>
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
        class="gp-field !w-24"
      />
      <button type="button" class="gp-btn-primary !py-1 !px-3" onclick={handleGoToLine}>Go</button>
      <button type="button" class="gp-btn !py-1 !px-2" onclick={() => (goToLineOpen = false)}>Cancel</button>
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
        bind:value={editDraft}
        spellcheck="false"
        class="flex-1 w-full h-full p-4 bg-background font-mono text-xs text-textPrimary leading-relaxed focus:outline-none resize-none border-none {wordWrap ? 'whitespace-pre-wrap' : 'whitespace-pre overflow-x-auto'}"
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
            rowHeight={Math.round(ROW_HEIGHT * (zoomPercent / 100))}
            overscan={OVERSCAN}
            bind:scrollTop
            class="h-full"
          >
            {#snippet row(line, lineIdx)}
              {@const lineNum = lineIdx + 1}
              {@const isHighlighted = selectedLine !== null &&
                (selectedLineEnd === null
                  ? selectedLine === lineNum
                  : lineNum >= Math.min(selectedLine, selectedLineEnd) && lineNum <= Math.max(selectedLine, selectedLineEnd))}
              {@const tokens = tokenizeLine(line ?? "", language)}
              <div
                class="flex items-center w-full leading-5 transition-colors {isHighlighted
                  ? 'bg-accent/15 border-l-2 border-accent'
                  : 'hover:bg-surface/50 border-l-2 border-transparent'}"
              >
                <!-- Line Number Gutter -->
                <button
                  type="button"
                  onclick={(e) => handleLineClick(lineNum, e)}
                  class="w-12 shrink-0 text-right pr-3 pl-1 select-none text-[11px] font-mono text-textMuted/60 hover:text-textPrimary transition-colors cursor-pointer"
                >
                  {lineNum}
                </button>

                <!-- Code Line Content with Syntax Tokens -->
                <div class="flex-1 min-w-0 pr-4 whitespace-pre">
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
  <div class="flex items-center justify-between px-3 py-1 bg-surface/90 border-t border-border/70 shrink-0 text-[10px] font-mono text-textMuted select-none">
    <div class="flex items-center gap-3">
      <span>Ln {selectedLine ?? 1}, Col 1</span>
      <span>•</span>
      <span>{indentInfo}</span>
      <span>•</span>
      <span>UTF-8</span>
      {#if saveSuccess}
        <span class="text-emerald-400 font-bold flex items-center gap-1">✓ Saved</span>
      {/if}
    </div>
    <div class="flex items-center gap-3">
      <span>{rawLines.length} lines</span>
      <span>•</span>
      <span class="text-accent font-semibold">{language.toUpperCase()}</span>
    </div>
  </div>
</div>
