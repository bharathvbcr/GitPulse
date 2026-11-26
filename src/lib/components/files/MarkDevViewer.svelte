<script lang="ts">
  import { untrack } from "svelte";
  import { keyedList } from "../../ui/eachKeys";
  import {
    Eye,
    Code,
    Columns,
    Rows2,
    ExternalLink,
    Copy,
    Check,
    ListTree,
    BookOpen,
    Clock,
    Sparkles,
    WrapText,
  } from "@lucide/svelte";
  import { observeResize } from "../../dom/observeResize";
  import { resolveMarkDevLayout } from "../../files/markdevLayout";
  import { openInDefaultApp } from "../../desktop/openInShell";
  import { currentRepoPath, repoStore } from "../../stores/repoStore";
  import type { LineEnding } from "../../files/types";
  import { formatError } from "../../ui/formatError";
  import { describeError } from "../../ui/copy/describeFailure";
  import { copyText } from "../../desktop/clipboard";
  import {
    renderMarkDevMarkdown,
    calculateDocumentStats,
    EMPTY_RENDER,
    MAX_RENDER_BYTES,
    type DocumentStats,
    type RenderedMarkdown,
  } from "../../files/markdevRender";
  import { findFragmentTarget, type MarkdownNote } from "../../files/markdownLinks";
  import { takeAnchor } from "../../files/pendingAnchor";
  import { requestReveal, revealRequest } from "../../files/revealRequests";
  import { docsBacklinks, type DocBacklink } from "../../docs/client";
  import { hostPlatform } from "../../stores/platformStore";
  import { shortcutTextLabel } from "../../ui/platformCopy";
  import { debounce } from "../../async/debounce";
  import { SEARCH_DEBOUNCE_MS } from "../../files/searchLimits";
  import { EMPTY_SEARCH, matchLabel, stepMatch, type SearchResult } from "../../text/lineSearch";
  import {
    applyDomHighlights,
    clearDomHighlights,
    setActiveDomHighlight,
  } from "../../files/domSearch";
  import { extractSelectionSeed } from "../../files/fileSearchTarget";
  import FindField from "../FindField.svelte";
  import CodeViewer from "./CodeViewer.svelte";
  import MarkDevLogo from "./MarkDevLogo.svelte";
  import MarkdownContent from "../MarkdownContent.svelte";
  import ScrollCue from "../ScrollCue.svelte";

  let {
    filePath,
    blob,
    repoPathOverride = null,
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
    blob: {
      path: string;
      is_binary: boolean;
      is_image: boolean;
      mime: string;
      text?: string | null;
      base64?: string | null;
      invalid_utf8_bytes?: number;
      disk_base?: string | null;
      eol?: LineEnding | null;
    };
    repoPathOverride?: string | null;
    draftContent?: string | null;
    dirty?: boolean;
    onSave?: (newContent: string) => Promise<void>;
    onDraftChange?: (newContent: string, sourceContent: string) => void;
    onRequestDiscard?: () => Promise<boolean>;
    revealTarget?: { line: number | null; column?: number | null; seq: number } | null;
    onSendSelectionToAgent?: (reference: string, checkout?: string) => unknown;
    agentSend?: { repoPath: string; checkout: string };
  } = $props();

  export type MarkDevViewMode = "rendered" | "split" | "raw";

  let viewMode = $state<MarkDevViewMode>("rendered");

  /**
   * The rendered page has no lines to land on, so a request naming this file
   * (a search or docs hit, a terminal link) shows the source instead, where
   * the raw CodeViewer collects it.
   */
  $effect(() => {
    const wanted = $revealRequest?.path === filePath || (revealTarget !== null && revealTarget?.line !== null);
    // Raw and Split already mount a CodeViewer that collects the request; only
    // the rendered page needs to give way, and the user's choice stands.
    if (wanted && untrack(() => viewMode) === "rendered") viewMode = "raw";
  });
  let showOutline = $state(false);
  let wordWrap = $state(true);
  let copiedSource = $state(false);

  let sourceContent = $derived(blob.text || "");
  let rawContent = $derived(draftContent ?? sourceContent);
  let rendered = $state<RenderedMarkdown>(EMPTY_RENDER);
  let renderError = $state<string | null>(null);
  let stats = $derived<DocumentStats>(calculateDocumentStats(rawContent));
  let outline = $derived(rendered.headings);
  let backlinks = $state<DocBacklink[]>([]);
  /**
   * The repository as a primitive, so `note` and the effects below depend on
   * a string compared by value rather than on the whole store. repoStore
   * publishes a fresh object per commit-message keystroke and per ~6s status
   * poll; a `note` built straight from `$repoStore.currentPath` was a new
   * object each time, which re-rendered the page (re-encoding its pictures)
   * and refetched the backlinks on every publish.
   */
  const repoPath = $derived(repoPathOverride ?? $currentRepoPath);
  let note = $derived<MarkdownNote | null>(
    repoPath && filePath ? { repoPath, path: filePath } : null,
  );

  /**
   * How long typing must pause before the preview re-renders. A render reads
   * and re-encodes the note's pictures, so one per keystroke would queue
   * work the next keystroke throws away. A file's first render is immediate.
   */
  const RERENDER_DELAY_MS = 150;
  /** The file the page currently shows; deliberately not reactive. */
  let shownFile: string | null = null;

  $effect(() => {
    const content = rawContent;
    const file = filePath;
    const location = note ? { repoPath: note.repoPath, filePath: note.path } : null;
    let cancelled = false;
    const run = () => {
      renderMarkDevMarkdown(content, location)
        .then((result) => {
          if (cancelled) return;
          rendered = result;
          renderError = null;
          shownFile = file;
        })
        .catch((err: unknown) => {
          if (cancelled) return;
          // Never leave the previous note's page up under this note's name.
          rendered = EMPTY_RENDER;
          renderError = formatError(err);
        });
    };
    const timer = shownFile === file ? setTimeout(run, RERENDER_DELAY_MS) : null;
    if (!timer) run();
    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    };
  });

  $effect(() => {
    const repo = repoPath;
    const path = filePath;
    let cancelled = false;
    backlinks = [];
    if (!repo || !path) return;
    void docsBacklinks(repo, path)
      .then((hits) => {
        if (!cancelled) backlinks = hits;
      })
      .catch(() => {
        if (!cancelled) backlinks = [];
      });
    return () => {
      cancelled = true;
    };
  });

  let markdevRootEl: HTMLDivElement | undefined = $state();
  let previewContainerEl: HTMLDivElement | undefined = $state();
  let markdevBar: HTMLDivElement | undefined = $state();
  let viewportEl: HTMLDivElement | undefined = $state();
  let viewportWidth = $state(0);

  let mdSearchOpen = $state(false);
  let mdSearchMode = $state<"find" | "line">("find");
  let mdSearchQuery = $state("");
  let mdDebouncedQuery = $state("");
  let mdCaseSensitive = $state(false);
  let mdRegex = $state(false);
  let mdCurrentMatchIdx = $state(0);
  let mdSearchResult = $state<SearchResult>(EMPTY_SEARCH);
  let mdMarks = $state.raw<HTMLElement[]>([]);
  let mdFindFieldRef: ReturnType<typeof FindField> | undefined = $state();
  let mdSearchInputEl: HTMLInputElement | undefined = $state();

  const applyMdSearchQuery = debounce((q: string) => (mdDebouncedQuery = q), SEARCH_DEBOUNCE_MS);
  $effect(() => {
    const next = mdSearchQuery;
    if (next === "") {
      applyMdSearchQuery.cancel();
      mdDebouncedQuery = "";
      return;
    }
    applyMdSearchQuery(next);
  });

  $effect(() => () => applyMdSearchQuery.cancel());

  $effect(() => {
    const mode = viewMode;
    const open = mdSearchOpen;
    const query = mdDebouncedQuery;
    const caseSensitive = mdCaseSensitive;
    const regex = mdRegex;
    void rendered;
    const root = previewContainerEl?.querySelector<HTMLElement>(".gp-markdown") ?? null;
    if (mode !== "rendered" || !open || !root) {
      clearDomHighlights(root);
      untrack(() => {
        mdMarks = [];
        mdSearchResult = EMPTY_SEARCH;
        mdCurrentMatchIdx = 0;
      });
      return;
    }
    const { result, marks } = applyDomHighlights(root, query, { caseSensitive, regex });
    untrack(() => {
      mdMarks = marks;
      mdSearchResult = result;
      mdCurrentMatchIdx = 0;
    });
    if (marks.length > 0) {
      setActiveDomHighlight(marks, 0, query.trim().length > 0);
    }
    return () => {
      clearDomHighlights(root);
      untrack(() => {
        mdMarks = [];
      });
    };
  });

  function stepMdMatch(delta: 1 | -1) {
    const next = stepMatch(mdCurrentMatchIdx, mdMarks.length, delta);
    if (next < 0) return;
    mdCurrentMatchIdx = next;
    setActiveDomHighlight(mdMarks, next, true);
  }

  function handleRenderedGoToLine(line: number, column?: number | null) {
    requestReveal(filePath, line, column ?? 1);
    viewMode = "raw";
  }

  function handleMarkDevKeydown(e: KeyboardEvent) {
    if (viewMode !== "rendered" || e.defaultPrevented) return;
    if ((e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "f") {
      e.preventDefault();
      e.stopPropagation();
      void mdFindFieldRef?.openFind(extractSelectionSeed(null, markdevRootEl));
      return;
    }
    if ((e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "g") {
      e.preventDefault();
      e.stopPropagation();
      void mdFindFieldRef?.openLine();
      return;
    }
    if (e.key === "F3" && !e.metaKey && !e.ctrlKey && !e.altKey && mdSearchOpen && mdMarks.length > 0) {
      e.preventDefault();
      e.stopPropagation();
      stepMdMatch(e.shiftKey ? -1 : 1);
      return;
    }
    if (e.key === "Escape" && mdSearchOpen) {
      e.preventDefault();
      e.stopPropagation();
      mdFindFieldRef?.close();
    }
  }

  $effect(() => {
    const el = markdevRootEl;
    if (!el) return;
    el.addEventListener("keydown", handleMarkDevKeydown);
    return () => el.removeEventListener("keydown", handleMarkDevKeydown);
  });

  $effect(() => {
    const el = viewportEl;
    if (!el) return;
    const sync = (w: number) => {
      viewportWidth = Number.isFinite(w) && w > 0 ? w : 0;
    };
    sync(el.clientWidth);
    return observeResize(el, (entries) => {
      const rect = entries.find((entry) => entry.target === el)?.contentRect ?? entries[0]?.contentRect;
      if (rect) sync(rect.width);
    });
  });

  let layout = $derived(
    resolveMarkDevLayout({
      width: viewportWidth,
      outlineRequested: showOutline,
      hasOutline: outline.length > 0,
    }),
  );
  let modeButtonClass = $derived(
    layout.toolbar === "stacked"
      ? "gp-seg-btn flex flex-1 min-w-0 items-center justify-center gap-1 px-2!"
      : "gp-seg-btn flex items-center gap-1 @md/markdev:gap-1.5 px-2! @md/markdev:px-2.5!",
  );
  let modeLabelClass = $derived(layout.toolbar === "stacked" ? "hidden @3xs/markdev:inline truncate" : "");

  async function handleCopySource() {
    if (!(await copyText(rawContent))) {
      repoStore.setError("Couldn't copy to clipboard");
      return;
    }
    copiedSource = true;
    setTimeout(() => (copiedSource = false), 1800);
  }

  async function openInMarkDev() {
    const repo = repoPath;
    if (!repo) return;
    // Was `${repo}/${filePath}` with no containment check and a silent catch:
    // the one call site that could walk out of the worktree, and the one that
    // never reported that it had failed. Rust proves containment now, and the
    // reason surfaces instead of leaving a dead button.
    try {
      await openInDefaultApp(repo, filePath);
    } catch (err) {
      repoStore.setError(describeError(err, "repo"));
    }
  }

  // A link from another note lands on its section once this file's page exists.
  $effect(() => {
    void rendered;
    if (!previewContainerEl || !note || shownFile !== filePath || !rendered.html) return;
    const fragment = takeAnchor(note.repoPath, note.path);
    if (fragment) findFragmentTarget(previewContainerEl, fragment)?.scrollIntoView({ block: "start" });
  });

  function scrollToHeading(id: string) {
    if (!previewContainerEl) return;
    // Scoped to this preview and matched by id rather than by `#${id}`
    // selector: a heading "1. Setup" has an id no selector accepts unescaped.
    findFragmentTarget(previewContainerEl, id)?.scrollIntoView({ behavior: "smooth", block: "start" });
  }

  /** KiB, for the omitted-tail notice. */
  function kib(bytes: number): string {
    return `${Math.max(1, Math.round(bytes / 1024))} KiB`;
  }
</script>

<!--
  Rendered / Split / Raw. One instance, placed by `layout.toolbar`: inline in
  the toolbar row when the viewer is wide enough for it, otherwise on a row of
  its own under the toolbar so it is never scrolled out of reach. Labels give
  way to icons only in the narrowest panes; the aria-label keeps each name.
-->
{#snippet modeSwitch()}
  <div class="gp-segmented" role="group" aria-label="Markdown view" data-markdev-modes={layout.toolbar}>
    <button
      type="button"
      onclick={() => (viewMode = "rendered")}
      aria-pressed={viewMode === "rendered"}
      aria-label="Rendered"
      class={modeButtonClass}
      data-active={viewMode === "rendered"}
      title="Rendered view: formatted Markdown with rich blocks"
    >
      <Eye size={12} />
      <span class={modeLabelClass}>Rendered</span>
    </button>

    <button
      type="button"
      onclick={() => (viewMode = "split")}
      aria-pressed={viewMode === "split"}
      aria-label="Split"
      class={modeButtonClass}
      data-active={viewMode === "split"}
      title={layout.splitDirection === "vertical"
        ? "Split view: raw source above live preview"
        : "Split view: raw source and live preview side by side"}
    >
      {#if layout.splitDirection === "vertical"}
        <Rows2 size={12} />
      {:else}
        <Columns size={12} />
      {/if}
      <span class={modeLabelClass}>Split</span>
    </button>

    <button
      type="button"
      onclick={() => (viewMode = "raw")}
      aria-pressed={viewMode === "raw"}
      aria-label="Raw"
      class={modeButtonClass}
      data-active={viewMode === "raw"}
      title="Raw view: source editor with line numbers and find"
    >
      <Code size={12} />
      <span class={modeLabelClass}>Raw</span>
    </button>
  </div>
{/snippet}

{#snippet page()}
  {#if renderError}
    <div class="gp-md-notice" role="status">
      <span>Couldn't render this note. {renderError} The Raw view shows its source.</span>
    </div>
  {/if}
  {#if rendered.omittedBytes > 0}
    <div class="gp-md-notice" role="status">
      <span>
        This note is longer than Waylode renders at once ({kib(MAX_RENDER_BYTES)}): its last
        {kib(rendered.omittedBytes)} is not shown here. The Raw view shows all of it.
      </span>
    </div>
  {/if}
  {#if rendered.frontmatter.length > 0}
    <dl class="gp-md-frontmatter max-w-full overflow-x-auto" aria-label="Frontmatter">
      {#each keyedList(rendered.frontmatter, (field) => field.key) as { item: field, key: fieldKey } (fieldKey)}
        <dt>{field.key}</dt>
        <dd>{field.value}</dd>
      {/each}
    </dl>
  {/if}
  <MarkdownContent html={rendered.html} {note} />
{/snippet}

<div
  bind:this={markdevRootEl}
  class="@container/markdev flex-1 flex flex-col h-full w-full bg-background font-sans text-xs min-h-0 min-w-0 overflow-hidden select-text relative"
  data-markdev-layout={layout.mode}
>
  <!-- MarkDev Integrated Header Toolbar -->
  <div class="relative shrink-0 select-none" data-tip-place="above">
    <div bind:this={markdevBar} class="gp-header-scroll">
      <div class="flex items-center justify-between gap-1.5 @md/markdev:gap-2 px-2 @md/markdev:px-3 py-1.5 min-w-max w-full">
    <!-- Left: MarkDev Brand & Outline Toggle -->
    <div class="flex items-center gap-1.5 @md/markdev:gap-2 shrink-0">
      <div class="hidden @3xs/markdev:flex items-center gap-1.5 py-0.5 px-2 rounded-full bg-surface border border-edge shadow-xs">
        <MarkDevLogo size={15} />
        <span class="hidden @2xl/markdev:inline font-semibold text-textPrimary text-meta">MarkDev</span>
        <span class="hidden @3xl/markdev:inline-flex gp-chip font-mono" data-tone="accent" data-kind="count">.MD</span>
      </div>

      {#if outline.length > 0}
        <button
          type="button"
          onclick={() => (showOutline = !showOutline)}
          aria-pressed={showOutline}
          aria-label="{showOutline ? 'Hide' : 'Show'} table of contents"
          title="{showOutline ? 'Hide' : 'Show'} table of contents"
          class="gp-btn text-meta {showOutline ? 'border-accent/65 bg-accent/15 text-accent font-semibold' : ''}"
          data-size="sm"
        >
          <ListTree size={12} />
          <span class="hidden @xl/markdev:inline">Outline</span>
          <span class="hidden @3xs/markdev:inline text-micro text-faint font-mono">({outline.length})</span>
        </button>
      {/if}

      {#if viewMode === "rendered"}
        <div class="flex min-w-0 items-center gap-1">
          <FindField
            bind:this={mdFindFieldRef}
            bind:open={mdSearchOpen}
            bind:mode={mdSearchMode}
            bind:query={mdSearchQuery}
            bind:caseSensitive={mdCaseSensitive}
            bind:regex={mdRegex}
            bind:input={mdSearchInputEl}
            registerControl
            label="Find in document"
            placeholder="Find in document…"
            shortcut={shortcutTextLabel("⌘F", $hostPlatform.os)}
            goToShortcut={shortcutTextLabel("⌘G", $hostPlatform.os)}
            lineCount={stats.lineCount}
            status={matchLabel(rendered.omittedBytes > 0 ? { ...mdSearchResult, truncated: true } : mdSearchResult, mdCurrentMatchIdx)}
            invalid={mdSearchResult.invalid}
            hasMatches={mdMarks.length > 0}
            onstep={stepMdMatch}
            ongotoline={handleRenderedGoToLine}
          />
        </div>
      {/if}
    </div>

    <!-- Center: the mode switcher, while the row has room for it -->
    {#if layout.toolbar === "inline"}
      {@render modeSwitch()}
    {/if}

    <!-- Right: Stats Pill & Open in MarkDev / Actions -->
    <div class="flex items-center gap-1 @md/markdev:gap-1.5 shrink-0">
      <!-- Reading Stats Pill -->
      <div class="hidden @5xl/markdev:block select-none">
        <span class="gp-chip gap-2 font-mono" data-tone="neutral">
          <span class="flex items-center gap-1">
            <BookOpen size={10} class="text-accent" />
            <span>{stats.wordCount} words</span>
          </span>
          <span class="text-faint">•</span>
          <span class="flex items-center gap-1">
            <Clock size={10} class="text-warn" />
            <span>~{stats.readingTimeMinutes} min</span>
          </span>
        </span>
      </div>

      {#if viewMode !== "raw"}
        <button
          type="button"
          onclick={() => (wordWrap = !wordWrap)}
          aria-pressed={wordWrap}
          aria-label="Toggle word wrap"
          class="gp-icon-btn {wordWrap ? 'text-accent bg-accent/15' : ''}"
          data-size="sm"
          title="Toggle word wrap"
        >
          <WrapText size={12} />
        </button>
      {/if}

      <button
        type="button"
        onclick={handleCopySource}
        aria-label="Copy raw Markdown text"
        class="gp-btn text-meta"
        data-size="sm"
        title="Copy raw Markdown text"
      >
        {#if copiedSource}
          <Check size={11} class="text-ok" />
          <span class="hidden @xl/markdev:inline text-ok font-semibold">Copied</span>
        {:else}
          <Copy size={11} />
          <span class="hidden @xl/markdev:inline">Copy</span>
        {/if}
      </button>

      <button
        type="button"
        onclick={openInMarkDev}
        aria-label="Open in MarkDev"
        class="gp-btn text-meta"
        data-variant="primary"
        data-size="sm"
        title="Open in MarkDev desktop application or default editor"
      >
        <ExternalLink size={11} />
        <span class="hidden @4xl/markdev:inline">Open in MarkDev</span>
      </button>
    </div>
      </div>
    </div>
    <ScrollCue target={markdevBar} axis="x" />
    {#if layout.toolbar === "stacked"}
      <div class="grid px-2 pb-1.5" data-markdev-mode-row>
        {@render modeSwitch()}
      </div>
    {/if}
  </div>

  <!-- Main Viewport Surface -->
  <div
    bind:this={viewportEl}
    class="flex-1 flex {layout.outlineVisible && layout.outlineLayout === 'sheet' ? 'flex-col' : 'flex-row'} min-h-0 min-w-0 overflow-hidden relative"
    data-outline-layout={layout.outlineVisible ? layout.outlineLayout : undefined}
  >
    <!-- Outline / TOC Drawer -->
    {#if showOutline && outline.length > 0}
      <div
        class="gp-float-card {layout.outlineLayout === 'sheet' ? 'max-h-40' : 'w-56'} shrink-0 flex flex-col select-none"
        data-markdev-outline-card
      >
      <div class="flex-1 min-h-0 p-3 overflow-y-auto gp-scroll">
        <div class="gp-label mb-2 flex items-center gap-1.5">
          <Sparkles size={11} class="text-accent" />
          <span>Document outline</span>
        </div>
        <div class="space-y-1">
          {#each outline as h}
            <button
              type="button"
              onclick={() => scrollToHeading(h.id)}
              class="w-full text-left truncate rounded-lg py-1 px-1.5 text-xs text-textMuted hover:text-textPrimary hover:bg-surface transition-colors cursor-pointer block"
              style="padding-left: {(h.level - 1) * 12 + 6}px;"
              title={h.title}
            >
              <span class="text-micro font-mono text-accent mr-1">{"#".repeat(h.level)}</span>
              <span>{h.title}</span>
            </button>
          {/each}
        </div>
      </div>
      </div>
    {/if}

    <!-- Content Workspace by View Mode -->
    {#if viewMode === "rendered"}
      <!-- Fully Rendered View -->
      <div
        bind:this={previewContainerEl}
        class="@container/md-preview flex-1 min-h-0 min-w-0 p-3 @md/md-preview:p-5 @2xl/md-preview:p-8 overflow-auto gp-scroll bg-background select-text {wordWrap ? 'wrap-break-word' : ''}"
      >
        <div class="gp-card min-w-0 p-3.5 @md/md-preview:p-6 @2xl/md-preview:p-8 max-w-4xl mx-auto shadow-card">
          {@render page()}
          {#if backlinks.length > 0}
            <div class="mt-10">
              <div class="gp-label mb-2">
                Backlinks ({backlinks.length})
              </div>
              <ul class="space-y-1.5">
                {#each keyedList(backlinks, (link) => JSON.stringify([link.path, link.line, link.offset])) as { item: link, key: renderKey } (renderKey)}
                  <li class="text-meta">
                    <span class="font-mono text-accent">{link.path}</span>
                    <span class="text-textMuted"> — {link.context}</span>
                  </li>
                {/each}
              </ul>
            </div>
          {/if}
        </div>
      </div>
    {:else if viewMode === "raw"}
      <!-- Full Raw Editor View -->
      <div class="grid grid-rows-[minmax(0,1fr)] grid-cols-[minmax(0,1fr)] flex-1 min-h-0 min-w-0 overflow-hidden">
        <CodeViewer
          {filePath}
          content={sourceContent}
          eol={blob.eol ?? null}
          invalidUtf8Bytes={blob.invalid_utf8_bytes ?? 0}
          diskBase={blob.disk_base ?? null}
          {draftContent}
          {dirty}
          {onSave}
          {onDraftChange}
          {onRequestDiscard}
          {revealTarget}
          {onSendSelectionToAgent}
          {agentSend}
        />
      </div>
    {:else if viewMode === "split"}
      <!-- Adaptive Split View: side by side when wide. Stacked when narrow, the
           two panes share the height and each scrolls itself: an outer
           scroller would trap the wheel (pane scrollers contain overscroll),
           leaving the lower pane out of reach in a short terminal split. -->
      <div
        class="flex-1 flex {layout.splitDirection === 'vertical'
          ? 'flex-col overflow-hidden'
          : 'flex-row overflow-x-auto overflow-y-hidden'} min-h-0 min-w-0 gap-2 gp-scroll"
        data-markdev-split={layout.splitDirection}
      >
        <!-- Raw Editor Pane -->
        <div
          class="grid grid-rows-[minmax(0,1fr)] grid-cols-[minmax(0,1fr)] flex-1 basis-0 overflow-hidden {layout.splitDirection === 'vertical'
            ? 'w-full min-w-0 min-h-0'
            : 'h-full min-h-0 min-w-[260px]'}"
          data-markdev-split-raw
        >
          <CodeViewer
            {filePath}
            content={sourceContent}
            eol={blob.eol ?? null}
            invalidUtf8Bytes={blob.invalid_utf8_bytes ?? 0}
            diskBase={blob.disk_base ?? null}
            {draftContent}
            {dirty}
            {onSave}
            {onDraftChange}
            {onRequestDiscard}
            {revealTarget}
            {onSendSelectionToAgent}
            {agentSend}
          />
        </div>

        <!-- Live Rendered Preview Pane -->
        <div
          bind:this={previewContainerEl}
          class="@container/md-preview flex-1 basis-0 {layout.splitDirection === 'vertical'
            ? 'w-full min-w-0 min-h-0'
            : 'h-full min-h-0 min-w-[260px]'} p-2.5 @md/md-preview:p-4 @xl/md-preview:p-6 overflow-auto gp-scroll bg-background/60 select-text {wordWrap ? 'wrap-break-word' : ''}"
          data-markdev-split-preview
        >
          <div class="gp-card min-w-0 p-3 @md/md-preview:p-5 @xl/md-preview:p-6 max-w-2xl mx-auto shadow-card">
            {@render page()}
          </div>
        </div>
      </div>
    {/if}
  </div>
</div>
