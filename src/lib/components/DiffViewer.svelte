<script lang="ts" module>
  // One cache per app: re-parses only when selectedDiff's string identity
  // changes, so unrelated store publications cost O(1) and the exact parsed
  // row objects survive (keeping memoized word-diff segments attached).
  import { createParseCache, type AnnotatedDiffLine } from "../diff/wordDiff";
  import { composeSpans, shiftMatches, type DiffSpan, type Range } from "../diff/highlight";
  import type { SupportedLanguage } from "../files/syntaxHighlight";
  import { densityStore } from "../stores/densityStore";
  import { rowHeight } from "../ui/density";

  const parseCache = createParseCache();

  /**
   * Composed spans, memoized per parsed line.
   *
   * A virtual scroll re-renders its whole window on every frame, so without
   * this the tokenizer runs on ~100 lines per frame. The parse cache keeps
   * line objects reference-stable across store publications, which is what
   * makes a WeakMap key work here; the signature covers everything that
   * changes what the spans should be.
   */
  const spanCache = new WeakMap<AnnotatedDiffLine, { sig: string; spans: DiffSpan[] }>();

  function spansFor(
    line: AnnotatedDiffLine,
    text: string,
    language: SupportedLanguage,
    syntax: boolean,
    matches: Range[],
  ): DiffSpan[] {
    const sig = `${language}|${syntax ? 1 : 0}|${line.segments ? 1 : 0}|${matches
      .map((r) => `${r.start}:${r.end}`)
      .join(",")}`;
    const hit = spanCache.get(line);
    if (hit && hit.sig === sig) return hit.spans;
    const spans = composeSpans(
      text,
      language,
      line.segments,
      line.type === "del" ? "Removed" : "Added",
      matches,
      { syntax },
    );
    spanCache.set(line, { sig, spans });
    return spans;
  }
</script>

<script lang="ts">
  import type { FileBlob } from "../files/types";
  import { repoStore } from "../stores/repoStore";
  import { graphStore } from "../stores/graphStore";
  import type { CommitFileChange } from "../stores/graphStore";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import DiffFileRail from "./DiffFileRail.svelte";
  import {
    buildFileRail,
    railPosition,
    stepFile,
    type RailEntry,
  } from "../diff/fileRail";
  import { buildCommitRail, type CommitEntry } from "../diff/commitRail";
  import { invoke } from "@tauri-apps/api/core";
  import {
    ArrowDownWideNarrow,
    ArrowUpWideNarrow,
    Check,
    ChevronDown,
    ChevronUp,
    Copy,
    FileCode,
    Loader2,
    Palette,
    PanelLeftOpen,
    Search,
    WrapText,
    X,
  } from "lucide-svelte";
  import LazyMount from "./LazyMount.svelte";
  // Only an image diff reaches this pane; it does not belong in the chunk
  // every launch parses.
  const loadImageDiffViewer = () => import("./ImageDiffViewer.svelte");
  import EmptyState from "./EmptyState.svelte";
  import LanguageLogo from "./LanguageLogo.svelte";
  import VirtualList from "./VirtualList.svelte";
  import {
    annotateRange,
    emptyDiffCopy,
    isImagePath,
    replacementBlockBounds,
  } from "../diff/wordDiff";
  import {
    buildFilePatchForHunk,
    buildFilePatchFromLines,
  } from "../diff/patchBuilder";
  import {
    buildSplitRows,
    lineForSplitRow,
    lineTones,
    nextChangeRow,
    splitRowForLine,
    splitTones,
    type SplitRow,
  } from "../diff/rowModel";
  import {
    buildOutline,
    churnSummary,
    hunkAt,
    languagePathForLine,
    outlineLanguagePath,
    outlineTitle,
    sectionAt,
    sectionStatus,
  } from "../diff/outline";
  import {
    buildTicks,
    fileMarks,
    ratioFromPointer,
    scrollForRatio,
    scrollForRow,
    viewportBand,
  } from "../diff/minimap";
  import { TONE_ADD, TONE_DEL, TONE_FILE, TONE_MOD } from "../diff/rowModel";
  import {
    findMatches,
    firstMatchFrom,
    matchLabel,
    stepMatch,
    type LineMatch,
  } from "../text/lineSearch";
  import { detectLanguageFromPath, tokenClass } from "../files/syntaxHighlight";
  import { getImpact } from "../codeintel/client";
  import { copyText } from "../desktop/clipboard";
  import { toastStore } from "../stores/toastStore";

  // Fixed row geometry keeps the virtualized window math trivial and lets a
  // half-million-line agent diff render exactly like a twenty-line one.
  // Row height follows the Compact/Spacious setting like the branch list
  // and commit table already did; this pane used to ignore it entirely.
  let ROW_HEIGHT = $derived(rowHeight("diff", $densityStore));
  /**
   * How many diff lines may be wrapped at once.
   *
   * Wrapping makes rows variable-height, and variable-height rows cannot be
   * windowed against a fixed `rowHeight` — that is what produced overlapping
   * rows whose first and last wrapped segments were clipped away, hiding code
   * rather than reflowing it. So a wrapped diff renders in full, and the cap
   * is what keeps "renders in full" from meaning a hundred thousand rows.
   *
   * Diffs read closely enough to want wrapping are small; a diff larger than
   * this is being skimmed, and skimming works better unwrapped anyway.
   */
  const WRAP_MAX_LINES = 4_000;
  const OVERSCAN = 20;
  /** Beyond this, even the light parse stops being worth its memory. */
  const MAX_RENDER_LINES = 300_000;
  /**
   * Syntax highlighting is per visible row, but the tokenizer still has to
   * run over every row that scrolls past. Past this the diff is being
   * skimmed and the colour is not what makes it readable.
   */
  const SYNTAX_MAX_LINES = 60_000;

  let viewMode = $state<"unified" | "split">("unified");
  /**
   * The file list travels with the diff.
   *
   * Before this, opening a commit's file switched to this view and left its
   * file list behind in the Graph view that owned it — reading a second file
   * meant going back, finding the commit again, and clicking the next row.
   */
  let railOpen = $state(true);
  let railWidth = $state(248);
  /** Owned here so unfolding the change picker survives a file switch. */
  let commitsOpen = $state(false);
  let wordWrap = $state(false);
  let syntaxOn = $state(true);
  let oldSrc = $state<string | null>(null);
  let newSrc = $state<string | null>(null);
  let selectedLines = $state<Set<number>>(new Set());
  let dragAnchor = $state<number | null>(null);
  let isDragging = $state(false);
  let copiedPatch = $state(false);

  let searchOpen = $state(false);
  let searchQuery = $state("");
  let searchCase = $state(false);
  let searchRegex = $state(false);
  let searchIndex = $state(0);
  let searchInput = $state<HTMLInputElement>();

  let impactEdges = $state(0);
  let impactGuard: AsyncGuard | null = null;

  $effect(() => {
    const repoPath = $repoStore.currentPath;
    const filePath = $repoStore.selectedFilePath;
    impactGuard?.cancel();
    if (!repoPath || !filePath) {
      impactEdges = 0;
      return;
    }
    // Guarded: a slow answer for the file you just left must not overwrite
    // the badge for the file you are now reading.
    const guard = createAsyncGuard();
    impactGuard = guard;
    void getImpact(repoPath, filePath, 20)
      .then((res) => {
        if (!guard.isLive()) return;
        impactEdges = res.available ? res.items.length : 0;
      })
      .catch(() => {
        if (guard.isLive()) impactEdges = 0;
      });
  });

  $effect(() => () => impactGuard?.cancel());

  let allLines = $derived(parseCache.parse($repoStore.selectedDiff));
  // Two independent cuts, one meaning. The backend caps what it reads at its
  // payload budget; this view caps what it renders. Either one makes the rows
  // on screen a prefix, and every consequence — the notice, the staging
  // lockout — follows from that fact rather than from which cut caused it.
  let cutByBackend = $derived($repoStore.selectedDiffTruncated);
  let cutByRenderer = $derived(allLines.length > MAX_RENDER_LINES);
  let truncatedSource = $derived(cutByBackend || cutByRenderer);
  let lines = $derived(cutByRenderer ? allLines.slice(0, MAX_RENDER_LINES) : allLines);
  // Only add/del/ctx rows are diff lines: hdr/meta/binary are chrome, so the
  // "N lines" stat means what a diff reader expects it to mean (and an empty
  // parse — no rows at all — reaches the EmptyState branch).
  let contentLineCount = $derived(
    lines.reduce(
      (count, line) =>
        line.type === "add" || line.type === "del" || line.type === "ctx" ? count + 1 : count,
      0
    )
  );

  /**
   * What this diff is about, read from the diff itself.
   *
   * The header used to print `selectedFilePath` whatever the body held, so a
   * commit-wide or worktree-wide diff was labelled with the last file the
   * reader had clicked — its name, its language icon, its line count — above
   * a body showing something else entirely.
   */
  const outline = $derived(buildOutline(lines));
  const languagePath = $derived(outlineLanguagePath(outline, $repoStore.selectedFilePath));

  /**
   * The language of the file THIS line came from, not the diff's one language.
   *
   * Keyed by path, which is a pure function of the path, so the map never
   * goes stale and is bounded by the number of files in the diff.
   */
  const languageByPath = new Map<string, SupportedLanguage>();
  function languageForLine(index: number): SupportedLanguage {
    const path = languagePathForLine(outline, index, $repoStore.selectedFilePath) ?? "";
    let language = languageByPath.get(path);
    if (language === undefined) {
      language = detectLanguageFromPath(path);
      languageByPath.set(path, language);
    }
    return language;
  }
  const title = $derived(
    outlineTitle(
      outline,
      $repoStore.selectedFilePath ||
        ($repoStore.selectedCommitId ? `commit ${$repoStore.selectedCommitId.slice(0, 8)}` : "Diff"),
    ),
  );
  const churn = $derived(churnSummary(outline.additions, outline.deletions));
  const singleSection = $derived(outline.files.length === 1 && !outline.headerless ? outline.files[0] : null);

  const splitModel = $derived(buildSplitRows(lines));
  const splitRows = $derived(splitModel.rows);

  const unifiedTones = $derived(lineTones(lines));
  const splitToneList = $derived(splitTones(splitModel));
  const tones = $derived(viewMode === "split" ? splitToneList : unifiedTones);
  const renderedRowCount = $derived(viewMode === "split" ? splitRows.length : lines.length);

  /**
   * Widest line number in the diff, so the gutter is sized for the file
   * rather than to a guess. A 40px column silently clipped six-digit numbers.
   */
  const gutterDigits = $derived.by(() => {
    let max = 0;
    for (const line of lines) {
      if (line.oldNo && line.oldNo > max) max = line.oldNo;
      if (line.newNo && line.newNo > max) max = line.newNo;
    }
    return Math.max(3, String(max).length);
  });

  const syntaxAvailable = $derived(lines.length <= SYNTAX_MAX_LINES);
  const syntaxActive = $derived(syntaxOn && syntaxAvailable);

  // --- search --------------------------------------------------------------

  /** The text a row actually shows, which is what a search should match. */
  function renderedText(line: AnnotatedDiffLine | undefined): string {
    if (!line) return "";
    if (line.type === "add" || line.type === "del") return line.content.slice(1);
    if (line.type === "ctx") {
      return line.content.startsWith(" ") ? line.content.slice(1) : line.content;
    }
    return line.content;
  }

  function markerOffset(line: AnnotatedDiffLine | undefined): number {
    if (!line) return 0;
    if (line.type === "add" || line.type === "del") return 1;
    if (line.type === "ctx" && line.content.startsWith(" ")) return 1;
    return 0;
  }

  const search = $derived(
    searchOpen && searchQuery.trim()
      ? findMatches(
          { length: lines.length, at: (i: number) => renderedText(lines[i]) },
          searchQuery,
          { caseSensitive: searchCase, regex: searchRegex },
        )
      : { matches: [] as LineMatch[], truncated: false, invalid: false },
  );

  const matchesByLine = $derived.by(() => {
    const map = new Map<number, LineMatch[]>();
    for (const match of search.matches) {
      const bucket = map.get(match.lineIndex);
      if (bucket) bucket.push(match);
      else map.set(match.lineIndex, [match]);
    }
    return map;
  });

  const activeMatch = $derived(search.matches[searchIndex] ?? null);

  function goToMatch(next: number): void {
    if (search.matches.length === 0) return;
    searchIndex = next;
    const match = search.matches[next];
    if (match) scrollToLine(match.lineIndex);
  }

  function stepSearch(delta: 1 | -1): void {
    goToMatch(stepMatch(searchIndex, search.matches.length, delta));
  }

  function openSearch(): void {
    searchOpen = true;
    // Resume from what the reader is looking at, not from the top of a diff
    // they have already scrolled through.
    queueMicrotask(() => {
      searchInput?.focus();
      searchInput?.select();
    });
  }

  function closeSearch(): void {
    searchOpen = false;
    searchQuery = "";
    searchIndex = 0;
  }

  // Re-anchor on the visible row whenever the result set changes, so typing
  // another character does not throw the reader back to match one.
  let lastMatchSignature = $state("");
  $effect(() => {
    const signature = `${searchQuery}|${searchCase}|${searchRegex}|${search.matches.length}`;
    if (signature === lastMatchSignature) return;
    lastMatchSignature = signature;
    if (search.matches.length === 0) {
      searchIndex = 0;
      return;
    }
    const anchor = firstMatchFrom(search.matches, topLineIndex);
    searchIndex = anchor < 0 ? 0 : anchor;
  });

  // --- minimap -------------------------------------------------------------

  const ticks = $derived(buildTicks(tones));
  const marks = $derived(
    fileMarks(
      outline.files.map((file) => file.index),
      renderedRowCount,
      viewMode === "split" ? (line) => splitRowForLine(splitModel, line) : undefined,
    ),
  );
  function toneClass(tone: number): string {
    if (tone === TONE_ADD) return "bg-emerald-500";
    if (tone === TONE_DEL) return "bg-rose-500";
    if (tone === TONE_MOD) return "bg-amber-500";
    if (tone === TONE_FILE) return "bg-accent/70";
    return "bg-accent/40";
  }

  function onMinimapPointer(event: PointerEvent): void {
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
    const ratio = ratioFromPointer(event.clientY, rect.top, rect.height);
    setScroll(scrollForRatio(ratio, renderedRowCount, ROW_HEIGHT, viewportHeight));
  }

  // --- scrolling -----------------------------------------------------------

  let unifiedScroll = $state(0);
  let splitScroll = $state(0);
  let viewportHeight = $state(0);
  let bodyEl = $state<HTMLElement>();

  const activeScroll = $derived(viewMode === "split" ? splitScroll : unifiedScroll);

  /** The band on the minimap marking what is currently on screen. */
  const band = $derived(
    viewportBand(activeScroll, viewportHeight, renderedRowCount, ROW_HEIGHT),
  );

  function setScroll(next: number): void {
    if (viewMode === "split") splitScroll = next;
    else unifiedScroll = next;
  }

  /** Topmost rendered row, as an index into the source line list. */
  const topLineIndex = $derived.by(() => {
    const row = Math.floor(activeScroll / ROW_HEIGHT);
    return viewMode === "split" ? lineForSplitRow(splitModel, row) : Math.min(row, Math.max(0, lines.length - 1));
  });

  /** Scrolls so a source line is centred, in whichever view is on screen. */
  function scrollToLine(lineIndex: number): void {
    const row =
      viewMode === "split" ? splitRowForLine(splitModel, lineIndex) : lineIndex;
    setScroll(scrollForRow(row, renderedRowCount, ROW_HEIGHT, viewportHeight));
  }

  function stepChange(delta: 1 | -1): void {
    const currentRow =
      viewMode === "split" ? splitRowForLine(splitModel, topLineIndex) : topLineIndex;
    const next = nextChangeRow(tones, currentRow, delta);
    if (next === null) return;
    setScroll(scrollForRow(next, renderedRowCount, ROW_HEIGHT, viewportHeight));
  }

  $effect(() => {
    const el = bodyEl;
    if (!el || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver((entries) => {
      const measured = entries[0]?.contentRect.height;
      viewportHeight = Number.isFinite(measured) && measured > 0 ? measured : 0;
    });
    observer.observe(el);
    return () => observer.disconnect();
  });

  // --- context strip -------------------------------------------------------

  const currentSection = $derived(sectionAt(outline, topLineIndex));
  const currentHunk = $derived(hunkAt(currentSection, topLineIndex));

  // --- selection and staging ----------------------------------------------

  let isWorkingTreeFile = $derived(
    $repoStore.selectedCommitId === null &&
      $repoStore.statuses.some((s) => s.path === $repoStore.selectedFilePath)
  );
  let isStaged = $derived($repoStore.selectedIsStaged);

  const details = $derived($graphStore.selectedCommitDetails);

  /**
   * The commit's files, fetched when the graph store does not already hold
   * them.
   *
   * The rail cannot depend on the Graph view having run first. A restored
   * session opens straight onto a persisted commit selection, and any future
   * caller that selects a commit file without going through
   * `graphStore.selectCommit` lands here too — in both cases the store is
   * empty and the rail would silently render nothing, which looks exactly
   * like a commit that touched one file.
   */
  let fetchedFiles = $state<{ commitId: string; files: CommitFileChange[] } | null>(null);
  let filesGuard: AsyncGuard | null = null;

  $effect(() => {
    const repo = $repoStore.currentPath;
    const commitId = $repoStore.selectedCommitId;
    // Nothing to fetch when there is no commit, or when the graph store's
    // details already cover this one.
    if (!repo || !commitId || details?.id === commitId) return;
    if (fetchedFiles?.commitId === commitId) return;
    filesGuard?.cancel();
    const guard = createAsyncGuard();
    filesGuard = guard;
    void (async () => {
      try {
        const files = await invoke<CommitFileChange[]>("cmd_get_commit_files", {
          repoPath: repo,
          commitId,
        });
        if (!guard.isLive()) return;
        fetchedFiles = { commitId, files };
      } catch {
        // The rail is an aid, not the content. A failed list leaves the diff
        // itself untouched and simply renders no rail, rather than pushing an
        // error banner over a file the reader can already see.
        if (guard.isLive()) fetchedFiles = { commitId, files: [] };
      }
    })();
  });

  $effect(() => () => filesGuard?.cancel());

  /** This commit's files from whichever source has them. */
  const commitFiles = $derived.by<CommitFileChange[] | null>(() => {
    const commitId = $repoStore.selectedCommitId;
    if (!commitId) return null;
    if (details?.id === commitId) return details.changed_files;
    if (fetchedFiles?.commitId === commitId) return fetchedFiles.files;
    return null;
  });

  const rail = $derived(
    buildFileRail({
      selectionKind: $repoStore.selectedCommitId
        ? "commit"
        : $repoStore.selectedFilePath
          ? "file"
          : "range",
      // Only this commit's own file list may be shown: a stale one from the
      // previously selected commit would offer files that are not in the diff
      // on screen.
      commitFiles,
      commitFilesTruncated: details?.files_list_truncated === true,
      commitFilesTotal: details?.files_total_count ?? 0,
      statuses: $repoStore.statuses,
    }),
  );
  const position = $derived(
    railPosition(rail, $repoStore.selectedFilePath, $repoStore.selectedIsStaged),
  );
  const prevFile = $derived(
    stepFile(rail, $repoStore.selectedFilePath, $repoStore.selectedIsStaged, -1),
  );
  const nextFile = $derived(
    stepFile(rail, $repoStore.selectedFilePath, $repoStore.selectedIsStaged, 1),
  );

  /** Recent commits, straight from the rows the graph already drew. */
  const commitRail = $derived(buildCommitRail($graphStore.rows));

  const hasRail = $derived(rail.entries.length > 0 || commitRail.entries.length > 0);

  /**
   * Alt+Arrow steps between files; Alt+PageUp/PageDown between changes.
   *
   * Alt is the modifier because bare arrows scroll the diff and Cmd/Ctrl+Arrow
   * is the OS word/line jump; both are things a reader is doing inside the
   * diff already. Typing targets are excluded so the commit-message box and
   * the search field keep their own arrow behaviour.
   */
  function onWindowKeydown(event: KeyboardEvent): void {
    const target = event.target as HTMLElement | null;
    const tag = target?.tagName;
    const typing =
      target?.isContentEditable === true ||
      tag === "INPUT" ||
      tag === "TEXTAREA" ||
      tag === "SELECT";

    if ((event.metaKey || event.ctrlKey) && !event.altKey && event.key.toLowerCase() === "f") {
      event.preventDefault();
      openSearch();
      return;
    }
    if (event.key === "Escape" && searchOpen) {
      event.preventDefault();
      closeSearch();
      return;
    }
    if (event.key === "F3" || (searchOpen && !typing && event.key === "Enter")) {
      event.preventDefault();
      stepSearch(event.shiftKey ? -1 : 1);
      return;
    }
    if (!event.altKey || event.ctrlKey || event.metaKey) return;
    if (event.key === "PageDown" || event.key === "PageUp") {
      if (typing) return;
      event.preventDefault();
      stepChange(event.key === "PageDown" ? 1 : -1);
      return;
    }
    if (event.key !== "ArrowUp" && event.key !== "ArrowDown") return;
    if (typing) return;
    const entry = event.key === "ArrowDown" ? nextFile : prevFile;
    if (!entry) return;
    event.preventDefault();
    openRailEntry(entry);
  }

  /**
   * Switches the diff to another commit, and opens one of its files.
   *
   * Selecting the commit alone would leave the pane showing the previous
   * commit's file, so the first changed file is opened with it — the reader
   * asked to look at this change, not to be told it is selected.
   */
  async function pickCommit(entry: CommitEntry): Promise<void> {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    let files = fetchedFiles?.commitId === entry.id ? fetchedFiles.files : null;
    if (!files) {
      try {
        files = await invoke<CommitFileChange[]>("cmd_get_commit_files", {
          repoPath: repo,
          commitId: entry.id,
        });
        fetchedFiles = { commitId: entry.id, files };
      } catch {
        files = [];
      }
    }
    const first = files[0];
    if (!first) {
      // A commit that changed nothing (an empty or a merge with no diff) has
      // no file to open; show the whole commit rather than nothing at all.
      void repoStore.selectCommitDiff(entry.id);
      return;
    }
    void repoStore.selectCommitFileDiff(entry.id, first.path);
  }

  /**
   * Returns to uncommitted work, opening its first changed file.
   *
   * A clean tree has nothing to open, so the button does nothing rather than
   * clearing the diff on screen — leaving the reader looking at a blank pane
   * they did not ask for is worse than leaving them where they were. The
   * entry's own "clean" badge already says why.
   */
  function pickWorkingTree(): void {
    const first = $repoStore.statuses[0];
    if (!first) return;
    void repoStore.selectFileDiff(first.path, first.is_staged);
  }

  /** Opens a rail entry through whichever command its source requires. */
  function openRailEntry(entry: RailEntry): void {
    const commitId = $repoStore.selectedCommitId;
    if (rail.source === "commit" && commitId) {
      void repoStore.selectCommitFileDiff(commitId, entry.path);
      return;
    }
    void repoStore.selectFileDiff(entry.path, entry.isStaged);
  }

  function lineSelectable(index: number): boolean {
    const line = lines[index];
    return !!line && (line.type === "add" || line.type === "del");
  }

  function toggleLine(index: number) {
    if (!lineSelectable(index)) return;
    const next = new Set(selectedLines);
    if (next.has(index)) next.delete(index);
    else next.add(index);
    selectedLines = next;
  }

  function selectRange(from: number, to: number) {
    const lo = Math.min(from, to);
    const hi = Math.max(from, to);
    const next = new Set<number>();
    for (let i = lo; i <= hi; i++) {
      if (lineSelectable(i)) next.add(i);
    }
    selectedLines = next;
  }

  function onLinePointerDown(index: number, event: PointerEvent) {
    if (!isWorkingTreeFile || !lineSelectable(index)) return;
    // A drag that starts on the code itself is a TEXT selection, not a
    // line-range selection. Both gestures live on the same row, so the split
    // is by where the pointer went down: the gutter (checkbox, line number,
    // +/- marker) drags a staging range, the text drags a selection. Without
    // this the row's preventDefault below suppresses native selection and the
    // diff stays uncopyable exactly where people copy from most.
    if ((event.target as Element | null)?.closest?.(".gp-diff-text")) return;
    event.preventDefault();
    isDragging = true;
    dragAnchor = index;
    selectedLines = new Set([index]);
    (event.currentTarget as HTMLElement | null)?.setPointerCapture?.(event.pointerId);
  }

  function onLinePointerEnter(index: number) {
    if (!isDragging || dragAnchor === null || !lineSelectable(index)) return;
    selectRange(dragAnchor, index);
  }

  function onLinePointerUp() {
    isDragging = false;
    dragAnchor = null;
  }

  async function stageHunk(hunkIndex: number) {
    if (!$repoStore.selectedFilePath) return;
    const patch = buildFilePatchForHunk(lines, $repoStore.selectedFilePath, hunkIndex);
    if (!patch) return;
    await repoStore.stageSelectivePatch(patch, !isStaged);
    selectedLines = new Set();
  }

  /**
   * Copies the selected lines as plain source, without the +/- markers.
   *
   * The markers are diff notation, not part of the code — pasting them into an
   * editor makes the snippet uncompilable, which is the whole reason someone
   * copies a line out of a diff.
   */
  async function copySelectedLines() {
    if (selectedLines.size === 0) return;
    const text = [...selectedLines]
      .sort((a, b) => a - b)
      .map((index) => {
        const line = lines[index];
        if (!line) return "";
        return line.type === "add" || line.type === "del"
          ? line.content.slice(1)
          : line.content;
      })
      .join("\n");
    const copied = await copyText(text);
    if (copied) toastStore.success(`Copied ${selectedLines.size} line${selectedLines.size === 1 ? "" : "s"}`);
    else toastStore.error("Could not reach the clipboard");
  }

  async function stageSelected(isStaging: boolean) {
    if (!$repoStore.selectedFilePath || selectedLines.size === 0) return;
    const patch = buildFilePatchFromLines(lines, $repoStore.selectedFilePath, selectedLines);
    if (!patch) return;
    await repoStore.stageSelectivePatch(patch, isStaging);
    selectedLines = new Set();
  }

  async function copyPatch(): Promise<void> {
    const text = $repoStore.selectedDiff;
    if (!text) return;
    if (!(await copyText(text))) {
      toastStore.error("Could not copy the patch to the clipboard");
      return;
    }
    copiedPatch = true;
    setTimeout(() => (copiedPatch = false), 1600);
  }

  let selectedGraphRow = $derived.by(() => {
    const id = $repoStore.selectedCommitId;
    if (!id) return null;
    return (
      $graphStore.rows.find((row) => row.id === id) ??
      ($graphStore.selectedCommit?.id === id ? $graphStore.selectedCommit : null)
    );
  });
  let emptyCopy = $derived(
    emptyDiffCopy($repoStore.selectedCommitId !== null && selectedGraphRow?.is_merge === true)
  );

  let lastBoundsBlock: { source: AnnotatedDiffLine[]; start: number; end: number } | null = null;

  /**
   * Annotates the replacement block around one line, once.
   *
   * Both views call this, so the intra-line pairing they show comes from one
   * pass over one block. Split view used to run its own pairing (last
   * deletion against first addition) over the same shared line objects, and
   * whichever view a reader opened first decided what the other one showed.
   */
  function annotateAround(index: number): void {
    const line = lines[index];
    if (!line) return;
    const cached =
      lastBoundsBlock &&
      lastBoundsBlock.source === lines &&
      index >= lastBoundsBlock.start &&
      index < lastBoundsBlock.end
        ? lastBoundsBlock
        : null;
    const bounds = cached ? [cached.start, cached.end] : replacementBlockBounds(lines, index);
    if (!bounds) return;
    if (!cached) lastBoundsBlock = { source: lines, start: bounds[0], end: bounds[1] };
    annotateRange(lines, bounds[0], bounds[1]);
  }

  function unifiedRow(index: number): AnnotatedDiffLine | undefined {
    const line = lines[index];
    if (!line) return undefined;
    annotateAround(index);
    return line;
  }

  function splitRow(index: number): SplitRow | undefined {
    const row = splitRows[index];
    if (!row) return undefined;
    if (row.kind === "code") {
      if (row.leftIndex >= 0) annotateAround(row.leftIndex);
      else if (row.rightIndex >= 0) annotateAround(row.rightIndex);
    }
    return row;
  }

  /** Spans for one rendered line, with its search hits marked. */
  function rowSpans(line: AnnotatedDiffLine | undefined, index: number): DiffSpan[] {
    if (!line) return [];
    const text = renderedText(line);
    const hits = matchesByLine.get(index);
    const ranges = hits ? shiftMatches(hits, markerOffset(line), text.length) : [];
    return spansFor(line, text, languageForLine(index), syntaxActive, ranges);
  }

  /**
   * Whether wrapping is offered at all for this diff.
   *
   * Counted over whichever list is on screen, because split rows pair lines
   * and are therefore fewer than the unified lines they came from.
   */
  const wrapAvailable = $derived(renderedRowCount <= WRAP_MAX_LINES);
  /** Wrapping actually in effect — asked for, and permitted. */
  const wrapping = $derived(wordWrap && wrapAvailable);

  let showingImage = $derived(
    isImagePath(singleSection?.path ?? $repoStore.selectedFilePath) &&
      (singleSection?.binary === true || lines.length === 0 || outline.files.length <= 1),
  );

  /**
   * Two reset keys, not one.
   *
   * `viewKey` covers what the reader chose — a different file, side of the
   * index, or whitespace mode — and resets scroll with the selection, because
   * a new file starts at its top.
   *
   * `contentKey` covers the diff TEXT changing under a selection that did
   * not: the watcher refetches after an external edit, and a mutation
   * refetches after staging. Line indices are what a patch is built from, so
   * a stale selection would stage lines the reader never picked. It clears
   * the selection and leaves the scroll alone, because the reader has not
   * gone anywhere.
   */
  let viewKey = $state<string | null>(null);
  let contentKey = $state<string | null>(null);

  $effect(() => {
    const key = [
      $repoStore.selectedFilePath,
      $repoStore.selectedCommitId,
      $repoStore.selectedIsStaged,
      $repoStore.selectedIgnoreWhitespace,
    ].join("\u0000");
    if (key === viewKey) return;
    viewKey = key;
    selectedLines = new Set();
    isDragging = false;
    dragAnchor = null;
    unifiedScroll = 0;
    splitScroll = 0;
  });

  $effect(() => {
    const text = $repoStore.selectedDiff;
    // Reference identity, not content: the parse cache already relies on the
    // store handing back the same string object for an unchanged diff.
    const key = text === null ? "\u0000null" : text;
    if (key === contentKey) return;
    contentKey = key;
    selectedLines = new Set();
    isDragging = false;
    dragAnchor = null;
  });

  /** Toggling layout keeps the reader's place instead of jumping to the top. */
  function setViewMode(next: "unified" | "split"): void {
    if (next === viewMode) return;
    const anchor = topLineIndex;
    viewMode = next;
    const row = next === "split" ? splitRowForLine(splitModel, anchor) : anchor;
    const count = next === "split" ? splitRows.length : lines.length;
    const offset = Math.max(0, row * ROW_HEIGHT);
    if (next === "split") splitScroll = Math.min(offset, Math.max(0, count * ROW_HEIGHT - viewportHeight));
    else unifiedScroll = Math.min(offset, Math.max(0, count * ROW_HEIGHT - viewportHeight));
  }

  let imageBlobKey = $state<string | null>(null);

  $effect(() => {
    const path = singleSection?.path ?? $repoStore.selectedFilePath;
    const repo = $repoStore.currentPath;
    const commitId = $repoStore.selectedCommitId;
    const key =
      showingImage && path && repo ? `${repo}\u0000${path}\u0000${commitId ?? ""}` : null;
    if (key === imageBlobKey) return;
    imageBlobKey = key;
    if (!key) {
      oldSrc = null;
      newSrc = null;
      return;
    }
    const requestKey = key;
    (async () => {
      const blobUrl = (blob: FileBlob | null): string | null => {
        if (!blob?.base64) return null;
        return `data:${blob.mime || "image/png"};base64,${blob.base64}`;
      };
      try {
        const newBlob = await invoke<FileBlob>("cmd_get_file_blob", {
          repoPath: repo,
          filePath: path,
          commitId: commitId || null,
        });
        let oldBlob: FileBlob | null = null;
        try {
          oldBlob = await invoke<FileBlob>("cmd_get_file_blob", {
            repoPath: repo,
            filePath: path,
            commitId: commitId ? `${commitId}^` : "HEAD",
          });
        } catch {
          oldBlob = null;
        }
        if (imageBlobKey === requestKey) {
          newSrc = blobUrl(newBlob);
          oldSrc = blobUrl(oldBlob);
        }
      } catch {
        if (imageBlobKey === requestKey) {
          oldSrc = null;
          newSrc = null;
        }
      }
    })();
  });

  const pending = $derived($repoStore.selectedDiffPending);
  const showEmpty = $derived(!pending && lines.length === 0);
</script>

<!-- Root-level: `svelte:window` may not sit inside a block, and the
     shortcuts must work in every body branch. -->
<svelte:window onkeydown={onWindowKeydown} />

<div class="flex h-full flex-1 flex-col overflow-hidden bg-background text-xs">
  <!-- Identity: what is on screen, taken from the diff itself. -->
  <div
    class="flex shrink-0 select-none items-center gap-2 border-b border-border/60 bg-surface/60 px-3 py-1.5 font-sans"
  >
    {#if languagePath}
      <LanguageLogo filePath={languagePath} size={15} class="shrink-0" />
    {:else}
      <FileCode size={15} class="shrink-0 text-accent" />
    {/if}
    <button
      type="button"
      class="min-w-0 truncate text-left font-medium text-textPrimary hover:text-accent"
      title={`${title} — click to copy`}
      onclick={() => void copyText(title)}
    >
      {title}
    </button>
    {#if singleSection}
      <span
        class="shrink-0 rounded px-1 font-mono text-[10px] {sectionStatus(singleSection) === 'A'
          ? 'bg-emerald-500/15 text-emerald-600 dark:text-emerald-400'
          : sectionStatus(singleSection) === 'D'
            ? 'bg-rose-500/15 text-rose-600 dark:text-rose-400'
            : sectionStatus(singleSection) === 'R'
              ? 'bg-sky-500/15 text-sky-600 dark:text-sky-400'
              : 'bg-amber-500/15 text-amber-600 dark:text-amber-400'}"
        title={singleSection.oldPath ? `renamed from ${singleSection.oldPath}` : undefined}
      >
        {sectionStatus(singleSection)}
      </span>
    {/if}
    {#if churn}
      <span class="shrink-0 font-mono text-[10px] tabular-nums text-textMuted">{churn}</span>
    {/if}
    {#if contentLineCount > 0}
      <span class="shrink-0 text-[10px] text-textMuted">{contentLineCount.toLocaleString()} lines</span>
    {/if}
    {#if isWorkingTreeFile}
      <span
        class="shrink-0 rounded-full border px-1.5 text-[10px] {isStaged
          ? 'border-emerald-500/40 bg-emerald-500/10 text-emerald-600 dark:text-emerald-400'
          : 'border-border/70 text-textMuted'}"
      >
        {isStaged ? "staged" : "unstaged"}
      </span>
    {/if}
    {#if impactEdges > 0}
      <span
        class="shrink-0 rounded-full border border-accent/30 bg-accent/15 px-2 py-0.5 text-[10px] text-accent"
        title={`${impactEdges} downstream callers/dependencies affected by this file in devmap`}
      >
        {impactEdges} {impactEdges === 1 ? "affected caller" : "affected callers"}
      </span>
    {/if}

    <div class="ml-auto flex shrink-0 items-center gap-2">
      <!-- Step between the files of this commit (or of the working tree)
           without leaving the diff. Disabled rather than wrapping at the
           edges: silently jumping back to the first file reads as a broken
           button, not as the end of a list. -->
      {#if rail.entries.length > 1}
        <div class="flex items-center gap-1">
          <button
            type="button"
            class="gp-btn !py-0.5 !px-1.5 disabled:opacity-40"
            disabled={!prevFile}
            onclick={() => prevFile && openRailEntry(prevFile)}
            title="Previous file (Alt+↑)"
            aria-label="Previous file"
          >
            <ChevronUp size={13} />
          </button>
          <span class="text-[10px] tabular-nums text-textMuted">
            {position.index || "–"}/{position.total}
          </span>
          <button
            type="button"
            class="gp-btn !py-0.5 !px-1.5 disabled:opacity-40"
            disabled={!nextFile}
            onclick={() => nextFile && openRailEntry(nextFile)}
            title="Next file (Alt+↓)"
            aria-label="Next file"
          >
            <ChevronDown size={13} />
          </button>
        </div>
      {/if}

      {#if isWorkingTreeFile}
        <button
          onclick={() => $repoStore.selectedFilePath && (isStaged ? repoStore.unstageFile($repoStore.selectedFilePath) : repoStore.stageFile($repoStore.selectedFilePath))}
          class="gp-btn-primary !py-1"
        >
          <Check size={13} />
          <span>{isStaged ? "Unstage File" : "Stage File"}</span>
        </button>
      {/if}
    </div>
  </div>

  <!-- Controls: how the diff is read. Separated from identity so neither row
       has to shed labels at a narrow width. -->
  <div
    class="flex shrink-0 select-none items-center gap-2 border-b border-border/60 bg-surface/30 px-3 py-1 font-sans"
  >
    {#if !railOpen && hasRail}
      <button
        type="button"
        class="gp-btn !py-0.5 !px-2 flex items-center gap-1.5 text-[11px] text-textMuted"
        onclick={() => (railOpen = true)}
        title="Show the file list"
      >
        <PanelLeftOpen size={13} />
        Files
      </button>
    {/if}

    <button
      type="button"
      class="gp-btn !py-0.5 !px-2 flex items-center gap-1.5 text-[11px] {searchOpen
        ? 'border-accent/60 bg-accent/10 text-accent'
        : 'text-textMuted'}"
      aria-pressed={searchOpen}
      onclick={() => (searchOpen ? closeSearch() : openSearch())}
      title="Find in this diff (⌘F)"
    >
      <Search size={13} />
      <span class="hidden sm:inline">Find</span>
    </button>

    <div class="flex items-center gap-1">
      <button
        type="button"
        class="gp-btn !py-0.5 !px-1.5 text-textMuted"
        onclick={() => stepChange(-1)}
        title="Previous change (Alt+PageUp)"
        aria-label="Previous change"
      >
        <ArrowUpWideNarrow size={13} />
      </button>
      <button
        type="button"
        class="gp-btn !py-0.5 !px-1.5 text-textMuted"
        onclick={() => stepChange(1)}
        title="Next change (Alt+PageDown)"
        aria-label="Next change"
      >
        <ArrowDownWideNarrow size={13} />
      </button>
    </div>

    <div class="ml-auto flex items-center gap-2">
      <button
        type="button"
        class="gp-btn !py-0.5 !px-2 flex items-center gap-1.5 text-[11px] text-textMuted"
        onclick={copyPatch}
        disabled={!$repoStore.selectedDiff}
        title="Copy the whole patch as unified diff text"
      >
        {#if copiedPatch}
          <Check size={13} class="text-emerald-500" />
        {:else}
          <Copy size={13} />
        {/if}
        <span class="hidden md:inline">{copiedPatch ? "Copied" : "Copy patch"}</span>
      </button>

      <button
        type="button"
        onclick={() => (syntaxOn = !syntaxOn)}
        aria-pressed={syntaxActive}
        disabled={!syntaxAvailable}
        title={syntaxAvailable
          ? "Colour the code by language"
          : `Syntax colouring is off above ${SYNTAX_MAX_LINES.toLocaleString()} lines — this diff has ${lines.length.toLocaleString()}.`}
        class="gp-btn !py-0.5 !px-2 flex items-center gap-1.5 text-[11px] disabled:opacity-40 {syntaxActive
          ? 'border-accent/60 bg-accent/10 font-semibold text-accent'
          : 'text-textMuted'}"
        aria-label="Toggle syntax colouring"
      >
        <Palette size={13} />
        <span class="hidden md:inline">Syntax</span>
      </button>

      <button
        type="button"
        onclick={() => (wordWrap = !wordWrap)}
        aria-pressed={wrapping}
        disabled={!wrapAvailable}
        title={wrapAvailable
          ? "Wrap long lines"
          : `Wrapping is unavailable above ${WRAP_MAX_LINES.toLocaleString()} lines — this diff has ${renderedRowCount.toLocaleString()}. Wrapped rows vary in height and cannot be windowed, so the whole diff would render at once.`}
        class="gp-btn !py-0.5 !px-2 flex items-center gap-1.5 text-[11px] disabled:opacity-40 {wrapping
          ? 'border-accent/60 bg-accent/10 font-semibold text-accent'
          : 'text-textMuted'}"
        aria-label="Toggle word wrap"
      >
        <WrapText size={13} />
        <span class="hidden md:inline">Wrap</span>
      </button>

      <label class="flex cursor-pointer items-center gap-1.5 text-[11px] text-textMuted hover:text-textPrimary">
        <input
          type="checkbox"
          checked={$repoStore.selectedIgnoreWhitespace}
          onchange={(e) => repoStore.setIgnoreWhitespace(e.currentTarget.checked)}
          class="rounded border-border bg-surface text-accent"
          aria-label="Ignore whitespace-only changes"
        />
        <span class="hidden lg:inline">Ignore whitespace</span>
      </label>

      <div class="gp-segmented" role="group" aria-label="Diff layout">
        <button
          onclick={() => setViewMode("unified")}
          aria-pressed={viewMode === "unified"}
          data-active={viewMode === "unified" ? "true" : "false"}
          class="gp-seg-btn"
        >
          Unified
        </button>
        <button
          onclick={() => setViewMode("split")}
          aria-pressed={viewMode === "split"}
          data-active={viewMode === "split" ? "true" : "false"}
          class="gp-seg-btn"
        >
          Split
        </button>
      </div>
    </div>
  </div>

  {#if searchOpen}
    <div
      class="flex shrink-0 select-none items-center gap-2 border-b border-border/60 bg-surface/50 px-3 py-1 font-sans"
    >
      <Search size={13} class="shrink-0 text-textMuted" />
      <input
        bind:this={searchInput}
        bind:value={searchQuery}
        type="text"
        class="min-w-0 flex-1 bg-transparent py-0.5 text-[11px] text-textPrimary outline-none placeholder:text-textMuted/60"
        placeholder="Find in this diff…"
        aria-label="Find in this diff"
      />
      <span
        class="shrink-0 tabular-nums text-[10px] {search.invalid ? 'text-rose-500' : 'text-textMuted'}"
      >
        {matchLabel(search, searchIndex)}
      </span>
      <button
        type="button"
        class="gp-btn !py-0.5 !px-1.5 text-[10px] {searchCase ? 'border-accent/60 text-accent' : 'text-textMuted'}"
        aria-pressed={searchCase}
        onclick={() => (searchCase = !searchCase)}
        title="Match case"
      >
        Aa
      </button>
      <button
        type="button"
        class="gp-btn !py-0.5 !px-1.5 font-mono text-[10px] {searchRegex ? 'border-accent/60 text-accent' : 'text-textMuted'}"
        aria-pressed={searchRegex}
        onclick={() => (searchRegex = !searchRegex)}
        title="Regular expression"
      >
        .*
      </button>
      <button
        type="button"
        class="gp-btn !py-0.5 !px-1.5 disabled:opacity-40"
        disabled={search.matches.length === 0}
        onclick={() => stepSearch(-1)}
        title="Previous match (Shift+F3)"
        aria-label="Previous match"
      >
        <ChevronUp size={13} />
      </button>
      <button
        type="button"
        class="gp-btn !py-0.5 !px-1.5 disabled:opacity-40"
        disabled={search.matches.length === 0}
        onclick={() => stepSearch(1)}
        title="Next match (F3)"
        aria-label="Next match"
      >
        <ChevronDown size={13} />
      </button>
      <button
        type="button"
        class="gp-icon-btn !p-1"
        onclick={closeSearch}
        title="Close find"
        aria-label="Close find"
      >
        <X size={13} />
      </button>
    </div>
  {/if}

  {#if truncatedSource}
    <div
      class="mx-3 mt-2 flex shrink-0 items-center gap-2 rounded-xl border border-amber-500/30 bg-amber-500/10 px-3 py-1.5 font-sans text-[11px] text-amber-600 dark:text-amber-300"
    >
      <span>⚠</span>
      <span>
        {#if cutByBackend}
          This diff is larger than GitPulse reads in one go — showing the first
          {contentLineCount.toLocaleString()} lines. Open individual files from the
          rail to see the rest. Staging is disabled here because a partial diff
          would stage less than these rows show.
        {:else}
          Diff exceeds {MAX_RENDER_LINES.toLocaleString()} lines — showing the first {MAX_RENDER_LINES.toLocaleString()}. Use the filter bar or open specific files instead of one massive commit.
        {/if}
      </span>
    </div>
  {/if}

  <!-- The frame is constant: the rail and the toolbars survive an empty diff,
       an image, and a pending fetch. Before this, each of those replaced the
       whole pane, so a clean merge or a `.png` left the reader with no way to
       reach the next file except by leaving for the Graph. -->
  <div class="relative flex min-h-0 flex-1 overflow-hidden">
    {#if railOpen && hasRail}
      <DiffFileRail
        {rail}
        commits={commitRail}
        currentPath={$repoStore.selectedFilePath}
        currentIsStaged={$repoStore.selectedIsStaged}
        selectedCommitId={$repoStore.selectedCommitId}
        workingTreeCount={$repoStore.statuses.length}
        onOpen={openRailEntry}
        onPickCommit={pickCommit}
        onPickWorkingTree={pickWorkingTree}
        onCollapse={() => (railOpen = false)}
        width={railWidth}
        onResize={(next) => (railWidth = next)}
        bind:commitsOpen
      />
    {/if}

    <div class="flex min-w-0 flex-1 flex-col">
      <!-- Where you are, when the file's own header has scrolled away. -->
      {#if currentSection && !showingImage && !showEmpty && !pending && (outline.files.length > 1 || currentHunk)}
        <div
          class="flex shrink-0 select-none items-center gap-2 border-b border-border/60 bg-surfaceHover/60 px-3 py-0.5 font-sans text-[10px] text-textMuted"
        >
          {#if currentSection.path}
            <span class="min-w-0 truncate font-medium text-textPrimary/80">{currentSection.path}</span>
          {/if}
          {#if currentHunk}
            <span class="shrink-0 font-mono opacity-70">{currentHunk.header}</span>
            {#if currentHunk.heading}
              <span class="min-w-0 truncate font-mono opacity-60">{currentHunk.heading}</span>
            {/if}
          {/if}
          <span class="ml-auto shrink-0 font-mono tabular-nums opacity-70">
            {churnSummary(currentSection.additions, currentSection.deletions)}
          </span>
        </div>
      {/if}

      <!-- `aria-busy` on the region, not on the message: a reader that has
           landed inside the diff needs to be told the content under it is
           being replaced, and a spinner is a picture of nothing to a screen
           reader. -->
      <div bind:this={bodyEl} class="relative flex min-h-0 flex-1" aria-busy={pending}>
        {#if pending}
          <div
            class="flex flex-1 items-center justify-center gap-2 text-[11px] text-textMuted font-sans"
            role="status"
          >
            <Loader2 size={14} class="animate-spin" />
            <span>Reading the diff…</span>
          </div>
        {:else if showingImage}
          <!-- Deferred: only an image diff reaches this pane, so it does not
               belong in the chunk every launch parses. Mounted inside the
               frame rather than in place of it, so the rail and the file
               stepper survive a `.png` the way they survive an empty diff. -->
          <LazyMount
            load={loadImageDiffViewer}
            name="The image comparison view"
            props={{
              filePath: singleSection?.path ?? $repoStore.selectedFilePath ?? "image",
              oldSrc,
              newSrc,
            }}
          />
        {:else if showEmpty}
          <div class="flex flex-1 items-center justify-center">
            <EmptyState icon={FileCode} title={emptyCopy.title} hint={emptyCopy.hint} />
          </div>
        {:else if viewMode === "unified"}
          <VirtualList
            items={lines}
            rowHeight={ROW_HEIGHT}
            virtualize={!wrapping}
            overscan={OVERSCAN}
            contentWidth={!wrapping}
            bind:scrollTop={unifiedScroll}
            class="gp-diff-surface flex-1 min-h-0 font-mono"
          >
            {#snippet row(_, index)}
              {@const line = unifiedRow(index)}
              {#if line}
                {@render unifiedLine(line, index)}
              {/if}
            {/snippet}
          </VirtualList>
        {:else}
          <VirtualList
            items={splitRows}
            rowHeight={ROW_HEIGHT}
            virtualize={!wrapping}
            overscan={OVERSCAN}
            contentWidth={!wrapping}
            bind:scrollTop={splitScroll}
            class="gp-diff-surface flex-1 min-h-0 font-mono"
          >
            {#snippet row(_, index)}
              {@const item = splitRow(index)}
              {#if item}
                {@render splitLine(item)}
              {/if}
            {/snippet}
          </VirtualList>
        {/if}

        <!-- Minimap: ticks, file boundaries and the viewport band, all
             projected from the list actually on screen. -->
        {#if ticks.length > 0 && !showingImage && !showEmpty && !pending}
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <!-- Justified: pointer-only by design, and marked presentation so
               assistive tech skips it. Everything it does has a keyboard
               route — Alt+PageUp/PageDown step changes, the list itself
               scrolls, and the file list jumps between files. -->
          <div
            class="group/rail relative h-full w-3 shrink-0 cursor-pointer select-none overflow-hidden border-l border-border/70 bg-surface/90 transition-[width] duration-150 hover:w-4"
            title="Diff map — click or drag to navigate"
            role="presentation"
            onpointerdown={(e) => {
              (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
              onMinimapPointer(e);
            }}
            onpointermove={(e) => {
              if (e.buttons === 1) onMinimapPointer(e);
            }}
          >
            {#if band}
              <div
                class="pointer-events-none absolute inset-x-0 rounded-sm border border-accent/40 bg-accent/15"
                style="top: {band.topPct}%; height: {band.heightPct}%;"
              ></div>
            {/if}
            {#each ticks as tick (tick.key)}
              <div
                class="pointer-events-none absolute left-0.5 right-0.5 rounded-sm {toneClass(tick.tone)}"
                style="top: {tick.topPct}%; height: {tick.heightPct}%;"
              ></div>
            {/each}
            {#each marks as mark, i (i)}
              <div
                class="pointer-events-none absolute inset-x-0 h-px bg-textPrimary/40"
                style="top: {mark}%;"
              ></div>
            {/each}
          </div>
        {/if}
      </div>
    </div>
  </div>

  <!-- Selection bar: only the action that applies to this side of the index. -->
  {#if isWorkingTreeFile && selectedLines.size > 0}
    <div
      class="flex shrink-0 items-center justify-between border-t border-border/80 bg-surface p-2.5 font-sans text-xs shadow-lg"
    >
      <span class="font-mono text-[11px] text-textMuted">
        {selectedLines.size} line{selectedLines.size === 1 ? "" : "s"} selected
      </span>
      <div class="flex items-center gap-2">
        <button onclick={() => (selectedLines = new Set())} class="gp-btn !py-1 !text-xs">
          Clear
        </button>
        <button onclick={copySelectedLines} class="gp-btn !py-1 !text-xs" title="Copy the selected lines without their diff markers">
          <Copy size={12} />
          <span>Copy</span>
        </button>
        {#if isStaged}
          <button onclick={() => stageSelected(false)} class="gp-btn-primary !py-1 !text-xs" disabled={truncatedSource}>
            <Check size={12} />
            <span>Unstage Selected ({selectedLines.size})</span>
          </button>
        {:else}
          <button onclick={() => stageSelected(true)} class="gp-btn-primary !py-1 !text-xs" disabled={truncatedSource}>
            <Check size={12} />
            <span>Stage Selected ({selectedLines.size})</span>
          </button>
        {/if}
      </div>
    </div>
  {/if}
</div>

<!-- ---------------------------------------------------------------------- -->
<!-- Row snippets                                                            -->
<!-- ---------------------------------------------------------------------- -->

{#snippet code(line: AnnotatedDiffLine, index: number)}
  <!-- `gp-diff-text` is what makes the code selectable and what
       `onLinePointerDown` looks for to tell a text drag from a line-range
       drag. Both layouts render through this one snippet, so the marker
       cannot be present on some rows and missing on others. -->
  <span class="gp-diff-text min-w-0 {wrapping ? 'whitespace-pre-wrap break-words' : 'whitespace-pre'}"
    >{#each rowSpans(line, index) as span}<span
        class="{syntaxActive ? tokenClass(span.token) : ''} {span.changed
          ? line.type === 'del'
            ? 'rounded-sm bg-rose-500/35 font-semibold'
            : 'rounded-sm bg-emerald-500/35 font-semibold'
          : ''} {span.match
          ? activeMatch?.lineIndex === index
            ? 'rounded-sm bg-amber-400/70 text-black'
            : 'rounded-sm bg-amber-400/30'
          : ''}">{span.text}</span
      >{/each}</span
  >
{/snippet}

{#snippet stageBox(index: number)}
  <button
    onclick={(e) => {
      e.stopPropagation();
      toggleLine(index);
    }}
    onpointerdown={(e) => e.stopPropagation()}
    class="flex h-3.5 w-3.5 shrink-0 select-none items-center justify-center rounded border {selectedLines.has(
      index,
    )
      ? 'border-accent bg-accent text-white'
      : 'border-border/60 hover:border-accent/80'}"
    title={selectedLines.has(index) ? "Deselect line" : "Select line for patch staging"}
    aria-label={selectedLines.has(index) ? "Deselect line" : "Select line for patch staging"}
  ></button>
{/snippet}

{#snippet unifiedLine(line: AnnotatedDiffLine, index: number)}
  {@const selected = selectedLines.has(index)}
  {@const tint =
    line.type === "add"
      ? "bg-emerald-500/15"
      : line.type === "del"
        ? "bg-rose-500/15"
        : line.type === "hdr" || line.type === "meta"
          ? "bg-surfaceHover/70"
          : line.type === "binary"
            ? "bg-amber-500/10"
            : ""}
  <div
    class="flex w-full {wrapping ? 'items-start' : 'items-center'} {tint} {selected
      ? 'ring-1 ring-inset ring-accent/60'
      : ''} {line.type === 'add' ? 'hover:bg-emerald-500/25' : line.type === 'del' ? 'hover:bg-rose-500/25' : 'hover:bg-surfaceHover/40'}"
    style={wrapping ? `min-height: ${ROW_HEIGHT}px` : `height: ${ROW_HEIGHT}px`}
    role="presentation"
    onpointerdown={(e) => onLinePointerDown(index, e)}
    onpointerenter={() => onLinePointerEnter(index)}
    onpointerup={onLinePointerUp}
  >
    <!-- The gutter is pinned, so scrolling a long line sideways does not take
         the line numbers with it. The outer span is opaque and the tint rides
         on top of it: a sticky layer with a 12%-alpha background lets the code
         scroll visibly underneath the numbers. -->
    <span class="sticky left-0 z-10 flex shrink-0 self-stretch bg-background">
      <span class="flex select-none items-center gap-1 self-stretch px-2 {tint}">
        {#if isWorkingTreeFile}
          {#if line.type === "add" || line.type === "del"}
            {@render stageBox(index)}
          {:else}
            <span class="w-3.5 shrink-0"></span>
          {/if}
        {/if}
        <span
          class="shrink-0 text-right text-[10px] tabular-nums text-textMuted/60"
          style="width: {gutterDigits}ch">{line.oldNo ?? ""}</span
        >
        <span
          class="shrink-0 text-right text-[10px] tabular-nums text-textMuted/60"
          style="width: {gutterDigits}ch">{line.newNo ?? ""}</span
        >
        <span
          class="w-2 shrink-0 text-center font-bold {line.type === 'add'
            ? 'text-emerald-600 dark:text-emerald-400'
            : line.type === 'del'
              ? 'text-rose-600 dark:text-rose-400'
              : 'text-transparent'}"
          >{line.type === "add" ? "+" : line.type === "del" ? "-" : " "}</span
        >
      </span>
    </span>

    {#if line.type === "hdr" && line.content.startsWith("@@")}
      <span class="flex min-w-0 flex-1 items-center gap-2 pr-3 text-[11px] text-textMuted">
        <span class="truncate font-mono">{line.content}</span>
        {#if isWorkingTreeFile}
          <button
            onclick={() => stageHunk(index)}
            disabled={truncatedSource}
            title={truncatedSource
              ? "Partial data — the diff is truncated, so staging would silently stage less than this hunk shows"
              : undefined}
            class="ml-auto shrink-0 rounded border border-border/80 bg-surface px-2 py-0.5 font-sans text-[10px] text-accent transition-colors hover:bg-accent/15 disabled:opacity-40"
          >
            {isStaged ? "Unstage Hunk" : "Stage Hunk"}
          </button>
        {/if}
      </span>
    {:else if line.type === "meta" && line.content.startsWith("diff --git ")}
      {@const section = sectionAt(outline, index)}
      <span class="flex min-w-0 flex-1 items-center gap-2 pr-3 font-sans text-[11px]">
        {#if section?.path}
          <LanguageLogo filePath={section.path} size={12} class="shrink-0" />
          <span class="truncate font-semibold text-textPrimary">{section.path}</span>
          {#if section.oldPath}
            <span class="shrink-0 text-[10px] text-textMuted">← {section.oldPath}</span>
          {/if}
          <span class="ml-auto shrink-0 font-mono text-[10px] tabular-nums text-textMuted">
            {churnSummary(section.additions, section.deletions)}
          </span>
        {:else}
          <span class="truncate font-mono text-textMuted">{line.content}</span>
        {/if}
      </span>
    {:else if line.type === "meta"}
      <span class="min-w-0 pr-3 text-[10px] italic text-textMuted/60 whitespace-pre">{line.content}</span>
    {:else if line.type === "binary"}
      <span class="flex min-w-0 items-center gap-2 pr-3 text-[11px] text-amber-700 dark:text-amber-300/90">
        <span class="shrink-0 rounded-sm bg-amber-500/20 px-1 font-sans">binary</span>
        <span class="whitespace-pre">{line.content}</span>
      </span>
    {:else if line.type === "hdr"}
      <span class="min-w-0 pr-3 text-[11px] text-textMuted whitespace-pre">{line.content}</span>
    {:else}
      <span
        class="min-w-0 pr-3 {line.type === 'add'
          ? 'text-emerald-900 dark:text-emerald-200'
          : line.type === 'del'
            ? 'text-rose-900 dark:text-rose-200'
            : 'text-textPrimary/80'}"
      >
        {@render code(line, index)}
      </span>
    {/if}
  </div>
{/snippet}

{#snippet splitSide(
  line: AnnotatedDiffLine | null,
  index: number,
  side: "old" | "new",
)}
  {@const selected = index >= 0 && selectedLines.has(index)}
  <!-- No sticky gutter here, deliberately: both columns share one horizontal
       scrollbar (which is what keeps the two sides aligned), and two gutters
       pinned to the same scrollport edge would stack on top of each other.
       Unified is the view for reading long lines, and it pins its gutter. -->
  {@const tint =
    !line
      ? "bg-surfaceHover/25"
      : line.type === "add"
        ? "bg-emerald-500/15"
        : line.type === "del"
          ? "bg-rose-500/15"
          : ""}
  <div
    class="flex min-w-0 flex-1 {wrapping ? 'items-start' : 'items-center'} {tint} {selected
      ? 'ring-1 ring-inset ring-accent/60'
      : ''}"
    role="presentation"
    onpointerdown={(e) => index >= 0 && onLinePointerDown(index, e)}
    onpointerenter={() => index >= 0 && onLinePointerEnter(index)}
    onpointerup={onLinePointerUp}
  >
    <span class="flex shrink-0 select-none items-center gap-1 self-stretch px-2">
      {#if isWorkingTreeFile}
        {#if line && (line.type === "add" || line.type === "del") && index >= 0}
          {@render stageBox(index)}
        {:else}
          <span class="w-3.5 shrink-0"></span>
        {/if}
      {/if}
      <span
        class="shrink-0 text-right text-[10px] tabular-nums text-textMuted/60"
        style="width: {gutterDigits}ch"
        >{(side === "old" ? line?.oldNo : line?.newNo) ?? ""}</span
      >
      <span
        class="w-2 shrink-0 text-center font-bold {line?.type === 'add'
          ? 'text-emerald-600 dark:text-emerald-400'
          : line?.type === 'del'
            ? 'text-rose-600 dark:text-rose-400'
            : 'text-transparent'}"
        >{line?.type === "add" ? "+" : line?.type === "del" ? "-" : " "}</span
      >
    </span>
    {#if line}
      <span
        class="min-w-0 flex-1 pr-2 {line.type === 'add'
          ? 'text-emerald-900 dark:text-emerald-200'
          : line.type === 'del'
            ? 'text-rose-900 dark:text-rose-200'
            : 'text-textPrimary/80'}"
      >
        {@render code(line, index)}
      </span>
    {/if}
  </div>
{/snippet}

{#snippet splitLine(row: SplitRow)}
  {#if row.kind === "span"}
    <!-- Chrome spans both columns. It used to fill the left and leave the
         right blank, which put a column-height hole beside every file header
         and made the two sides look misaligned when they were not. -->
    <div
      class="flex w-full items-center gap-2 bg-surfaceHover/70 px-3 {wrapping ? 'items-start' : ''}"
      style={wrapping ? `min-height: ${ROW_HEIGHT}px` : `height: ${ROW_HEIGHT}px`}
    >
      {#if row.line.type === "meta" && row.line.content.startsWith("diff --git ")}
        {@const section = sectionAt(outline, row.index)}
        {#if section?.path}
          <LanguageLogo filePath={section.path} size={12} class="shrink-0" />
          <span class="truncate font-sans text-[11px] font-semibold text-textPrimary">{section.path}</span>
          <span class="ml-auto shrink-0 font-mono text-[10px] tabular-nums text-textMuted">
            {churnSummary(section.additions, section.deletions)}
          </span>
        {:else}
          <span class="truncate text-[11px] text-textMuted">{row.line.content}</span>
        {/if}
      {:else if row.line.type === "hdr" && row.line.content.startsWith("@@")}
        <span class="truncate text-[11px] text-textMuted">{row.line.content}</span>
        {#if isWorkingTreeFile}
          <button
            onclick={() => stageHunk(row.index)}
            disabled={truncatedSource}
            title={truncatedSource
              ? "Partial data — the diff is truncated, so staging would silently stage less than this hunk shows"
              : undefined}
            class="ml-auto shrink-0 rounded border border-border/80 bg-surface px-2 py-0.5 font-sans text-[10px] text-accent transition-colors hover:bg-accent/15 disabled:opacity-40"
          >
            {isStaged ? "Unstage Hunk" : "Stage Hunk"}
          </button>
        {/if}
      {:else if row.line.type === "binary"}
        <span class="shrink-0 rounded-sm bg-amber-500/20 px-1 font-sans text-[10px]">binary</span>
        <span class="truncate text-[11px] text-amber-700 dark:text-amber-300/90">{row.line.content}</span>
      {:else}
        <span class="truncate text-[10px] italic text-textMuted/60">{row.line.content}</span>
      {/if}
    </div>
  {:else}
    <div
      class="flex w-full divide-x divide-border/70"
      style={wrapping ? `min-height: ${ROW_HEIGHT}px` : `height: ${ROW_HEIGHT}px`}
    >
      {@render splitSide(row.left, row.leftIndex, "old")}
      {@render splitSide(row.right, row.rightIndex, "new")}
    </div>
  {/if}
{/snippet}
