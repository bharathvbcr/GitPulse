<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { hostPlatform } from "../../stores/platformStore";
  import { shortcutTextLabel } from "../../ui/platformCopy";
  import { repoStore } from "../../stores/repoStore";
  import { densityStore } from "../../stores/densityStore";
  import { CODE_ZOOM_MAX, CODE_ZOOM_MIN, rowHeight, scaledRowHeight } from "../../ui/density";
  import { invoke } from "../../ipc/invoke";
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
  import { describeError } from "../../ui/copy/describeFailure";
  import { askConfirm } from "../../stores/modalStore";
  import { createAsyncGuard } from "../../async/guard";
  import {
    WrapText,
    Copy,
    Check,
    Edit3,
    Save,
    RotateCcw,
    Hash,
    Loader2,
    Send,
    ChevronDown,
    ListPlus,
  } from "@lucide/svelte";
  import { portal } from "../../dom/portal";
  import { popover, popoverExit, restoreFocusTo, type PopoverOptions } from "../../ui/popover";
  import { LAYERS } from "../../ui/layers";
  import { displayName } from "../../repos/paths";
  import { toastStore } from "../../stores/toastStore";
  import { chooseRecipient, recipientChoice, recipientDirectory, recipientLabel, recipientOptionLabel, recipientView } from "../../terminal/recipient";
  import { addToTray, MAX_TRAY_ENTRIES } from "../../terminal/contextTray";
  import { composeSelectionContent, formatSelectionRef } from "../../terminal/selectionContent";
  import { clearActiveSelection, setActiveSelection } from "../../terminal/activeSelection";
  import VirtualList from "../VirtualList.svelte";
  import ScrollCue from "../ScrollCue.svelte";
  import FindField from "../FindField.svelte";
  import { pageSearchScope, registerPageSearch } from "../../search/pageSearch.svelte";
  import { findMatches, matchLabel, stepMatch } from "../../text/lineSearch";
  import { debounce } from "../../async/debounce";
  import { SEARCH_DEBOUNCE_MS } from "../../files/searchLimits";
  import { consumeReveal, goToLineTarget, revealRequest } from "../../files/revealRequests";
  import type { LineEnding } from "../../files/types";

  let {
    filePath,
    content,
    eol = null,
    invalidUtf8Bytes = 0,
    diskBase = null,
    readOnly = false,
    draftContent = null,
    dirty = false,
    onSave,
    onDraftChange,
    onRequestDiscard,
    revealTarget = null,
    onSendSelectionToAgent,
    agentSend,
  }: {
    filePath: string;
    content: string;
    /** The file's line endings as read from disk; null when unknown. */
    eol?: LineEnding | null;
    /** Bytes `content` shows as U+FFFD because the file is not valid UTF-8. */
    invalidUtf8Bytes?: number;
    /** The disk version `content` is (`FileBlob.disk_base`); sent with a standalone save. */
    diskBase?: string | null;
    readOnly?: boolean;
    draftContent?: string | null;
    dirty?: boolean;
    onSave?: (newContent: string) => Promise<void>;
    onDraftChange?: (newContent: string, sourceContent: string) => void;
    onRequestDiscard?: () => Promise<boolean>;
    /** Pane-local line reveal target (used by `TerminalFilePane` so split panes never collide on the global `revealRequest` store). */
    revealTarget?: { line: number | null; column?: number | null; seq: number } | null;
    /**
     * Inserts text (`@path#Lstart-Lend` for `⌘L`, a fenced block for `⌘⇧L`) into an
     * agent terminal, unsubmitted. `checkout` is the checkout of this file, so
     * the host can prefer a session working in it. A host that can fail reports
     * it itself; the return value is not read.
     */
    onSendSelectionToAgent?: (reference: string, checkout?: string) => unknown;
    /**
     * Where this file lives, for the send controls: the repository whose terminal
     * and context tray it uses, and the checkout the file is in. Absent, both are
     * the active repository's.
     */
    agentSend?: { repoPath: string; checkout: string };
  } = $props();

  let ROW_HEIGHT = $derived(rowHeight("code", $densityStore));
  let actionBar: HTMLDivElement | undefined = $state();
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
  let restoredBefore: string | null = null;
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
  /**
   * Mounted as the Code view's file pane, the view bar's search field is
   * this file's find: the viewer registers as its provider and draws no Find
   * of its own. Mounted anywhere else (a stash preview) it keeps its own.
   */
  const searchScope = pageSearchScope();
  let searchQuery = $state("");
  let isCaseSensitive = $state(false);
  let isRegex = $state(false);
  let currentMatchIdx = $state(0);
  let searchInputEl: HTMLInputElement | undefined = $state();

  let goToLineOpen = $state(false);
  let targetLineInput = $state<number | null>(null);

  let scrollTop = $state(0);

  let language = $derived<SupportedLanguage>(detectLanguageFromPath(filePath));
  /**
   * Why this file must not be edited here, or null. The textarea cannot hold
   * the bytes a lossy decode replaced, nor keep mixed line endings per line,
   * so saving either would silently rewrite the file. The backend refuses
   * the same writes; this keeps the editor from offering them at all.
   */
  let editBlockedReason = $derived(
    invalidUtf8Bytes > 0
      ? `${invalidUtf8Bytes} byte${invalidUtf8Bytes === 1 ? "" : "s"} in this file ${invalidUtf8Bytes === 1 ? "is" : "are"} not valid UTF-8 and show as \u{FFFD}. Editing is off because saving would replace them permanently.`
      : eol === "mixed"
        ? "This file mixes line endings. Editing is off because saving would rewrite every one of them."
        : null,
  );
  /**
   * The text as the textarea will hand it back: its value turns CRLF into LF.
   * Comparing drafts against `content` itself would mark every CRLF file
   * dirty on the first keystroke; the backend restores CRLF on save.
   */
  let editBaseline = $derived(content.replace(/\r\n/g, "\n"));
  let eolLabel = $derived(eol === "mixed" ? "Mixed EOL" : eol ? eol.toUpperCase() : null);
  let hasUnsavedChanges = $derived(dirty || (isEditing && editDraft !== editBaseline));

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
   * while the request is outstanding. Raw: a whole-document token table,
   * only ever replaced, never edited in place.
   */
  let treeSitterLines = $state.raw<SyntaxToken[][] | null>(null);
  let treeSitterGuard: ReturnType<typeof createAsyncGuard> | null = null;

  $effect(() => {
    const lang = language;
    const text = isEditing ? editDraft : content;
    treeSitterGuard?.cancel();
    treeSitterLines = null;
    // A file past the render cap is drawn only up to the cap, so a
    // whole-document pass would tokenize lines nobody can scroll to.
    // `highlightDocument` itself refuses anything over the backend's 1 MiB.
    if (!usesTreeSitter(lang) || isEditing || linesTruncated) return;
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

  // A file opened as a page's own pane finds through the view bar's field;
  // anywhere else (a stash preview) the toolbar Find above stays. Registered
  // once on mount, as in DiffViewer: the scope is fixed for the viewer's life.
  onMount(() => {
    if (!searchScope) return;
    return registerPageSearch(searchScope, {
      kind: "find",
      label: "Find in file",
      placeholder: "Find in file…",
      query: () => searchQuery,
      setQuery: (next) => (searchQuery = next),
      caseSensitive: () => isCaseSensitive,
      setCaseSensitive: (next) => (isCaseSensitive = next),
      regex: () => isRegex,
      setRegex: (next) => (isRegex = next),
      status: () => ({
        text: matchLabel(searchResult, currentMatchIdx),
        invalid: searchResult.invalid,
        hasMatches: matchCount > 0,
      }),
      step: (delta) => (delta === 1 ? nextMatch() : prevMatch()),
    });
  });

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
    const num = goToLineTarget(targetLineInput, rawLines.length);
    if (num === null) return;
    selectedLine = num;
    selectedLineEnd = null;
    scrollToLine(num - 1);
    goToLineOpen = false;
    targetLineInput = null;
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
    if (readOnly || editBlockedReason) return;
    editDraft = draftContent ?? editBaseline;
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
    editDraft = editBaseline;
  }

  function onEditInput(event: Event) {
    const value = (event.currentTarget as HTMLTextAreaElement).value;
    editDraft = value;
    onDraftChange?.(value, editBaseline);
  }

  async function saveChanges() {
    if (readOnly || !isEditing || isSaving) return;
    if (editBlockedReason) {
      repoStore.setError(`Not saved: ${editBlockedReason}`);
      return;
    }
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
        // Standalone use has no draft owner to remember the base, so the
        // file is saved against the version this viewer was handed.
        await invoke("cmd_write_file_content", {
          repoPath: repo,
          filePath: path,
          content: contentToSave,
          expectedBase: diskBase,
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
      repoStore.setError(describeError(err, "repo"));
    } finally {
      if (filePath === path && editorGeneration === generation) isSaving = false;
    }
  }

  async function handleCopy() {
    const textToCopy = isEditing ? editDraft : content;
    if (!(await copyText(textToCopy))) {
      repoStore.setError("Couldn't copy file content to clipboard");
      return;
    }
    copied = true;
    setTimeout(() => (copied = false), 1800);
  }

  /** The selected range as `lo <= hi`, or null with no line selected. */
  let selectionRange = $derived(
    selectedLine === null
      ? null
      : { lo: Math.min(selectedLine, selectedLineEnd ?? selectedLine), hi: Math.max(selectedLine, selectedLineEnd ?? selectedLine) },
  );
  /** The checkout this file is in, and the repository whose terminal and tray it uses. */
  let sendCheckout = $derived(agentSend?.checkout ?? $repoStore.currentPath ?? "");
  let sendRepo = $derived(agentSend?.repoPath ?? $repoStore.currentPath ?? "");
  /** The one rule (`recipient.ts`) the panel sends by, over the sessions it last published. */
  let recipients = $derived(
    recipientView($recipientDirectory[sendRepo] ?? [], { checkout: sendCheckout || null, stickyId: $recipientChoice[sendRepo] }),
  );
  let recipientText = $derived(recipients.chosen ? recipientLabel(recipients.chosen) : null);
  let rangeText = $derived(
    selectionRange
      ? `L${selectionRange.lo}${selectionRange.hi !== selectionRange.lo ? `–${selectionRange.hi}` : ""}`
      : null,
  );
  let pickerOpen = $state(false);
  let pickerAnchor: HTMLElement | undefined = $state();
  const pickerDismissal = $derived<PopoverOptions>({
    anchor: { kind: "element", element: pickerAnchor, gap: 4, place: "below", align: "end" },
    estimate: { width: 220, height: 120 },
    inset: 8,
    dismiss: { inside: "[data-agent-recipient-picker], [data-agent-recipient-trigger]", pointer: "pointerdown", escape: "capture" },
    onDismiss: () => (pickerOpen = false),
  });

  /** The reference for what is selected, or for the whole file with no line selected. */
  function formatAgentSelectionRef(): string {
    return formatSelectionRef(filePath, selectionRange?.lo ?? null, selectionRange?.hi ?? null);
  }

  function sendSelectionToAgent() {
    if (!onSendSelectionToAgent || !filePath) return;
    onSendSelectionToAgent(formatAgentSelectionRef(), sendCheckout || undefined);
  }

  /** Sends the selected lines themselves: a fenced block, cut at the bound with the cut stated. */
  function sendSelectionContent() {
    if (!onSendSelectionToAgent || !filePath) return;
    if (!selectionRange) {
      toastStore.error("Select lines first: click a line number, shift-click to extend.");
      return;
    }
    const composed = composeSelectionContent({
      path: filePath,
      checkoutName: displayName(sendCheckout || filePath),
      startLine: selectionRange.lo,
      lines: rawLines.slice(selectionRange.lo - 1, selectionRange.hi),
      // The lines are a draft, not the file, only while it differs from what was read. The draft
      // is the shared one when the host supplies it: another view of this file may be the one
      // typing, and this viewer's own buffer only follows it.
      unsaved: draftContent !== null ? draftContent !== editBaseline : isEditing && editDraft !== editBaseline,
    });
    if (!composed.text) {
      toastStore.error("That selection is past the end of the file.");
      return;
    }
    if (composed.omittedLines > 0) {
      toastStore.warning(`The selection is too long to send whole: ${composed.omittedLines} lines were left out, and the text says so.`);
    }
    onSendSelectionToAgent(composed.text, sendCheckout || undefined);
  }

  function addSelectionToTray() {
    if (!onSendSelectionToAgent || !filePath) return;
    if (!selectionRange) {
      toastStore.error("Select lines first: click a line number, shift-click to extend.");
      return;
    }
    const added = addToTray(sendRepo, {
      checkout: sendCheckout,
      path: filePath,
      startLine: selectionRange.lo,
      endLine: selectionRange.hi,
    });
    if (!added.ok) toastStore.error(added.reason);
    else if (added.added) toastStore.info(`Added ${rangeText} of ${filePath} to the context tray (up to ${MAX_TRAY_ENTRIES}).`);
    else toastStore.info("That range is already in the context tray.");
  }

  // The palette's way to this selection; see `activeSelection.ts`.
  const selectionOwner = {};
  $effect(() => {
    const range = selectionRange;
    if (!onSendSelectionToAgent || !range || !filePath) return;
    setActiveSelection(selectionOwner, {
      path: filePath,
      startLine: range.lo,
      endLine: range.hi,
      sendReference: sendSelectionToAgent,
      sendContent: sendSelectionContent,
      addToTray: addSelectionToTray,
    });
    return () => clearActiveSelection(selectionOwner);
  });

  function handleKeydown(e: KeyboardEvent) {
    const sendChord = (e.metaKey || e.ctrlKey) && onSendSelectionToAgent;
    // ⌥ changes `key` on macOS, so the chord with it is matched by the physical key.
    // Ctrl+Alt is AltGr on some Windows layouts, where it types a character instead.
    if (sendChord && e.altKey && !e.shiftKey && e.code === "KeyL" && !e.getModifierState("AltGraph")) {
      e.preventDefault();
      e.stopPropagation();
      addSelectionToTray();
      return;
    }
    if (sendChord && !e.altKey && e.shiftKey && e.key.toLowerCase() === "l") {
      e.preventDefault();
      e.stopPropagation();
      sendSelectionContent();
      return;
    }
    if (sendChord && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "l") {
      e.preventDefault();
      e.stopPropagation();
      sendSelectionToAgent();
      return;
    }
    // As the page's pane, ⌘F is the view bar field's (App routes it there).
    if (!searchScope && (e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "f") {
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
      if (isSearchOpen && !searchScope) {
        isSearchOpen = false;
        searchQuery = "";
        return;
      }
    }
  }

  $effect(() => {
    const path = filePath;
    const restored = draftContent;
    const source = editBaseline;
    if (path !== previousFilePath) {
      previousFilePath = path;
      restoredBefore = restored;
      untrack(() => {
        editorGeneration += 1;
        editDraft = restored ?? source;
        isEditing = restored !== null;
        isSaving = false;
        saveSuccess = false;
      });
      return;
    }
    const droppedDraft = restoredBefore !== null && restored === null;
    restoredBefore = restored;
    if (restored !== null && !isEditing) {
      untrack(() => {
        editDraft = restored;
        isEditing = true;
      });
    } else if (restored === null && !isEditing) {
      untrack(() => {
        editDraft = source;
      });
    } else if (restored !== null) {
      // The draft is shared with another view of this file (the Code page and
      // a terminal split hold one): follow its typing instead of keeping a
      // textarea that could save over it. Our own input already equals it.
      untrack(() => {
        if (editDraft !== restored) editDraft = restored;
      });
    } else if (droppedDraft) {
      // Saved or discarded in the other view. A buffer that now equals the
      // file stays open, as after an Overwrite from the banner; one that does
      // not (a discard) has nothing left to keep and goes back to the file.
      untrack(() => {
        if (editDraft === source) return;
        editDraft = source;
        isEditing = false;
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
    // Read through the store so a request made while this file is already
    // open (a hit in the palette or Search for the shown file) re-runs the
    // effect; `consumeReveal` alone reads the slot untracked.
    const pending = $revealRequest;
    if (!path || lineCount === 0 || !pending || pending.path !== path) return;
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

  let appliedRevealSeq = 0;
  $effect(() => {
    const target = revealTarget;
    const lineCount = rawLines.length;
    if (!target || lineCount === 0 || target.seq === appliedRevealSeq) return;
    appliedRevealSeq = target.seq;
    if (!target.line || target.line <= 0) return;
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
  aria-label="Code viewer"
>
  <!--
    Editor chrome. The two clusters used to share one justify-between row and
    painted over each other, and over the first source line, once the editor
    was narrower than their content. The row now scrolls, and tips from it
    open above the bar so they do not cover the file.
  -->
  <div
    class="relative shrink-0 gp-section-edge select-none"
    data-tip-place="above"
  >
    <div bind:this={actionBar} class="gp-header-scroll">
      <div class="flex items-center justify-between gap-2 px-3 py-1.5 min-w-max w-full">
    <div class="flex items-center gap-2 shrink-0">
      <span class="text-meta font-mono text-textMuted">{rawLines.length}{linesTruncated ? "+" : ""} lines</span>
      <span class="text-faint">•</span>
      <span class="text-meta font-mono text-textMuted">{(byteSize / 1024).toFixed(1)} KB</span>
      <span class="text-faint">•</span>
      <span class="text-micro font-mono uppercase px-1.5 py-0.5 rounded bg-accent/15 text-accent font-semibold">{language}</span>
      {#if eolLabel}
        <span class="text-faint">•</span>
        <span
          class="text-meta font-mono {eol === 'mixed' ? 'text-warn font-semibold' : 'text-textMuted'}"
          title={eol === "mixed" ? editBlockedReason : `Line endings: ${eolLabel}, kept on save`}
          data-testid="code-eol"
        >{eolLabel}</span>
      {/if}
      {#if invalidUtf8Bytes > 0}
        <span
          class="text-micro font-semibold px-1.5 py-0.5 rounded bg-warn/15 text-warn"
          role="status"
          title={editBlockedReason}
          data-testid="code-lossy"
        >Not UTF-8 · {invalidUtf8Bytes} byte{invalidUtf8Bytes === 1 ? "" : "s"} shown as {"\u{FFFD}"}</span>
      {/if}
      {#if hasUnsavedChanges}
        <span class="text-micro font-semibold text-warn" role="status">Unsaved</span>
      {/if}

      {#if !searchScope}
      <div class="ml-2 flex min-w-0 items-center">
        <FindField
          bind:open={isSearchOpen}
          bind:query={searchQuery}
          bind:caseSensitive={isCaseSensitive}
          bind:regex={isRegex}
          bind:input={searchInputEl}
          label="Find in file"
          placeholder="Find in file…"
          shortcut={shortcutTextLabel("⌘F", $hostPlatform.os)}
          status={matchLabel(searchResult, currentMatchIdx)}
          invalid={searchResult.invalid}
          hasMatches={matchCount > 0}
          onstep={(delta) => (delta === 1 ? nextMatch() : prevMatch())}
        />
      </div>
      {/if}

      <button
        type="button"
        onclick={() => (goToLineOpen = true)}
        class="gp-btn px-2! flex items-center gap-1 text-meta" data-size="sm"
      >
        <Hash size={11} />
        <span>Go to Line</span>
        <span class="gp-keycap text-micro!">{shortcutTextLabel("⌘G", $hostPlatform.os)}</span>
      </button>

      {#if onSendSelectionToAgent}
        <button
          type="button"
          onclick={sendSelectionToAgent}
          data-testid="code-send-to-agent"
          class="gp-btn py-0.5! px-2! flex items-center gap-1 text-meta border-accent/35 text-accent"
          title={recipientText
            ? `Send ${rangeText ? `${rangeText} of ` : ""}${filePath} as @file#Lstart-Lend to ${recipientText}. ${shortcutTextLabel("⌘⇧L", $hostPlatform.os)} sends the lines themselves.`
            : $recipientDirectory[sendRepo]
              ? "No live agent or shell session to send to: open one first"
              : "No terminal has reported its sessions yet: this opens the terminal, and says so if it has no live agent or shell to send to"}
        >
          <Send size={11} />
          <span>{rangeText ? `Send ${rangeText}` : "Send @file"}{#if recipientText}<span data-testid="code-send-recipient"> → {recipientText}</span>{/if}</span>
          <span class="gp-keycap text-micro!">{shortcutTextLabel("⌘L", $hostPlatform.os)}</span>
        </button>
        {#if recipients.options.length > 1}
          <button
            type="button"
            bind:this={pickerAnchor}
            onclick={() => (pickerOpen = !pickerOpen)}
            data-agent-recipient-trigger
            data-testid="code-send-recipient-picker"
            class="gp-icon-btn text-accent"
            data-size="sm"
            aria-haspopup="menu"
            aria-expanded={pickerOpen}
            aria-label="Choose which session receives sends"
            title="Choose which session receives sends"
          >
            <ChevronDown size={12} />
          </button>
        {/if}
        <button
          type="button"
          onclick={addSelectionToTray}
          data-testid="code-add-to-tray"
          class="gp-btn py-0.5! px-2! flex items-center gap-1 text-meta"
          title="Set this range aside in the context tray, to insert with others ({shortcutTextLabel('⌥⌘L', $hostPlatform.os)})"
        >
          <ListPlus size={11} />
          <span>Tray</span>
        </button>
        {#if pickerOpen && recipients.options.length > 1}
          <div
            use:portal={"body"}
            use:popover={pickerDismissal} out:popoverExit
            data-agent-recipient-picker
            class="fixed gp-menu gp-pop p-1 min-w-48 max-w-[calc(100vw-16px)] flex flex-col font-sans"
            style="z-index: {LAYERS.MENU}"
            role="menu"
            aria-label="Session that receives sends"
            data-testid="code-send-recipient-menu"
          >
            {#each recipients.options as option (option.id)}
              <button
                type="button"
                role="menuitemradio"
                aria-checked={option.id === recipients.chosen?.id}
                class="gp-menu-item text-meta text-left px-2 py-1 rounded flex items-center gap-2 {option.id === recipients.chosen?.id ? 'text-accent' : 'text-textPrimary'}"
                data-testid="code-send-recipient-option"
                onclick={() => {
                  chooseRecipient(sendRepo, option.id);
                  pickerOpen = false;
                  restoreFocusTo(pickerAnchor);
                }}
              >
                <span class="truncate">{recipientOptionLabel(option)}</span>
                {#if option.id === recipients.chosen?.id}<Check size={11} class="shrink-0" />{/if}
              </button>
            {/each}
          </div>
        {/if}
      {/if}
    </div>

    <div class="flex items-center gap-1.5 shrink-0">
      <button
        type="button"
        onclick={() => (wordWrap = !wordWrap)}
        class="gp-icon-btn {wordWrap ? 'text-accent bg-accent/15' : 'text-textMuted hover:text-textPrimary'}"
        title="Word wrap (edit mode)"
      >
        <WrapText size={13} />
      </button>

      <button
        type="button"
        onclick={() => (showWhitespace = !showWhitespace)}
        class="gp-icon-btn {showWhitespace ? 'text-accent bg-accent/15' : 'text-textMuted hover:text-textPrimary'}"
        title="Show whitespace"
      >
        <span class="font-mono text-meta font-semibold">·_</span>
      </button>

      <!-- Zoom Controls -->
      <div class="flex items-center rounded-full border border-edge bg-surface px-1.5 py-0.5 gap-1">
        <button
          type="button"
          onclick={() => (zoomPercent = Math.max(CODE_ZOOM_MIN, zoomPercent - 10))}
          class="text-textMuted hover:text-textPrimary text-micro px-1"
          title="Zoom out"
        >−</button>
        <span class="text-micro font-mono text-textMuted min-w-8 text-center">{zoomPercent}%</span>
        <button
          type="button"
          onclick={() => (zoomPercent = Math.min(CODE_ZOOM_MAX, zoomPercent + 10))}
          class="text-textMuted hover:text-textPrimary text-micro px-1"
          title="Zoom in"
        >+</button>
      </div>

      <div class="h-3.5 w-1 rounded-full bg-border/50 mx-1" aria-hidden="true"></div>

      <!-- Edit & Save Controls -->
      {#if !isEditing && !readOnly}
        <button
          type="button"
          onclick={startEdit}
          disabled={editBlockedReason !== null}
          title={editBlockedReason ?? undefined}
          class="gp-btn px-2.5! flex items-center gap-1 text-meta" data-size="sm"
        >
          <Edit3 size={12} class="text-accent" />
          <span>Edit</span>
        </button>
      {:else if isEditing && !readOnly}
        <button
          type="button"
          onclick={cancelEdit}
          class="gp-btn px-2.5! flex items-center gap-1 text-meta text-textMuted" data-size="sm"
        >
          <RotateCcw size={12} />
          <span>Cancel</span>
        </button>
        <button
          type="button"
          onclick={saveChanges}
          disabled={isSaving || !hasUnsavedChanges}
          class="gp-btn-primary px-3! flex items-center gap-1 text-meta" data-size="sm"
        >
          {#if isSaving}
            <Loader2 size={12} class="animate-spin" />
          {:else}
            <Save size={12} />
          {/if}
          <span>Save ({shortcutTextLabel("⌘S", $hostPlatform.os)})</span>
        </button>
      {/if}

      <button
        type="button"
        onclick={handleCopy}
        class="gp-btn px-2.5! flex items-center gap-1 text-meta" data-size="sm"
        title="Copy whole file content"
      >
        {#if copied}
          <Check size={12} class="text-ok" />
          <span class="text-ok font-semibold">Copied</span>
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

  {#if goToLineOpen}
    <div class="relative shrink-0 select-none">
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
      <button type="button" class="gp-btn-primary px-3! shrink-0" data-size="sm" onclick={handleGoToLine}>Go</button>
      <button type="button" class="gp-btn px-2! shrink-0" data-size="sm" onclick={() => (goToLineOpen = false)}>Cancel</button>
        </div>
      </div>
      <ScrollCue target={goToBar} axis="x" />
    </div>
  {/if}

  <!-- Main Code Surface -->
  <div class="flex-1 min-h-0 relative overflow-hidden bg-background flex flex-col">
    {#if linesTruncated && !isEditing}
      <div class="shrink-0 px-3 py-1.5 text-meta text-warn bg-warn/10">
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
                  class="w-12 shrink-0 self-stretch text-right pr-3 pl-1 select-none text-meta font-mono text-faint hover:text-textPrimary transition-colors cursor-pointer"
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
  <div class="relative shrink-0 gp-section-edge text-micro font-mono text-textMuted select-none">
    <div bind:this={statusBar} class="gp-header-scroll">
      <div class="flex items-center justify-between gap-3 px-3 py-1 min-w-max w-full">
    <div class="flex items-center gap-3 shrink-0">
      <span>Ln {selectedLine ?? 1}, Col 1</span>
      <span>•</span>
      <span>{indentInfo}</span>
      <span>•</span>
      <span>UTF-8</span>
      {#if saveSuccess}
        <span class="text-ok font-semibold flex items-center gap-1">✓ Saved</span>
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
