<script lang="ts">
  import { untrack } from "svelte";
  import { keyedList } from "../../ui/eachKeys";
  import {
    Eye,
    Code,
    Columns,
    ExternalLink,
    Copy,
    Check,
    ListTree,
    BookOpen,
    Clock,
    Sparkles,
    WrapText,
  } from "@lucide/svelte";
  import { openInDefaultApp } from "../../desktop/openInShell";
  import { currentRepoPath, repoStore } from "../../stores/repoStore";
  import type { LineEnding } from "../../files/types";
  import { formatError } from "../../ui/formatError";
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
  import { revealRequest } from "../../files/revealRequests";
  import { docsBacklinks, type DocBacklink } from "../../docs/client";
  import CodeViewer from "./CodeViewer.svelte";
  import MarkDevLogo from "./MarkDevLogo.svelte";
  import MarkdownContent from "../MarkdownContent.svelte";
  import ScrollCue from "../ScrollCue.svelte";

  let {
    filePath,
    blob,
    draftContent = null,
    dirty = false,
    onSave,
    onDraftChange,
    onRequestDiscard,
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
    draftContent?: string | null;
    dirty?: boolean;
    onSave?: (newContent: string) => Promise<void>;
    onDraftChange?: (newContent: string, sourceContent: string) => void;
    onRequestDiscard?: () => Promise<boolean>;
  } = $props();

  export type MarkDevViewMode = "rendered" | "split" | "raw";

  let viewMode = $state<MarkDevViewMode>("rendered");

  /**
   * The rendered page has no lines to land on, so a request naming this file
   * (a search or docs hit, a terminal link) shows the source instead, where
   * the raw CodeViewer collects it.
   */
  $effect(() => {
    const wanted = $revealRequest?.path === filePath;
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
  const repoPath = $derived($currentRepoPath);
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

  let previewContainerEl: HTMLDivElement | undefined = $state();
  let markdevBar: HTMLDivElement | undefined = $state();

  async function handleCopySource() {
    if (!(await copyText(rawContent))) {
      repoStore.setError("Couldn't copy to clipboard");
      return;
    }
    copiedSource = true;
    setTimeout(() => (copiedSource = false), 1800);
  }

  async function openInMarkDev() {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    // Was `${repo}/${filePath}` with no containment check and a silent catch:
    // the one call site that could walk out of the worktree, and the one that
    // never reported that it had failed. Rust proves containment now, and the
    // reason surfaces instead of leaving a dead button.
    try {
      await openInDefaultApp(repo, filePath);
    } catch (err) {
      repoStore.setError(formatError(err));
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

{#snippet page()}
  {#if renderError}
    <div class="gp-md-notice" role="status">
      <span>Couldn't render this note. {renderError} The Raw view shows its source.</span>
    </div>
  {/if}
  {#if rendered.omittedBytes > 0}
    <div class="gp-md-notice" role="status">
      <span>
        This note is longer than GitPulse renders at once ({kib(MAX_RENDER_BYTES)}): its last
        {kib(rendered.omittedBytes)} is not shown here. The Raw view shows all of it.
      </span>
    </div>
  {/if}
  {#if rendered.frontmatter.length > 0}
    <dl class="gp-md-frontmatter" aria-label="Frontmatter">
      {#each keyedList(rendered.frontmatter, (field) => field.key) as { item: field, key: fieldKey } (fieldKey)}
        <dt>{field.key}</dt>
        <dd>{field.value}</dd>
      {/each}
    </dl>
  {/if}
  <MarkdownContent html={rendered.html} {note} />
{/snippet}

<div class="flex flex-col h-full bg-background font-sans text-xs min-h-0 select-text relative">
  <!-- MarkDev Integrated Header Toolbar -->
  <div class="relative shrink-0 bg-surface/80 select-none" data-tip-place="above">
    <div bind:this={markdevBar} class="gp-header-scroll">
      <div class="flex items-center justify-between gap-2 px-3 py-1.5 min-w-max w-full">
    <!-- Left: MarkDev Brand & Outline Toggle -->
    <div class="flex items-center gap-2 shrink-0">
      <div class="flex items-center gap-1.5 py-0.5 px-2 rounded-full bg-surface border border-edge shadow-xs">
        <MarkDevLogo size={15} />
        <span class="font-semibold text-textPrimary text-meta">MarkDev</span>
        <span class="gp-chip font-mono" data-tone="accent" data-kind="count">.MD</span>
      </div>

      {#if outline.length > 0}
        <button
          type="button"
          onclick={() => (showOutline = !showOutline)}
          aria-pressed={showOutline}
          title="{showOutline ? 'Hide' : 'Show'} table of contents"
          class="gp-btn text-meta {showOutline ? 'border-accent/65 bg-accent/15 text-accent font-semibold' : ''}"
          data-size="sm"
        >
          <ListTree size={12} />
          <span>Outline</span>
          <span class="text-micro text-faint font-mono">({outline.length})</span>
        </button>
      {/if}
    </div>

    <!-- Center: MarkDev Tri-Mode Switcher (Rendered / Split / Raw) -->
    <div class="gp-segmented" role="group" aria-label="Markdown view">
      <button
        type="button"
        onclick={() => (viewMode = "rendered")}
        aria-pressed={viewMode === "rendered"}
        class="gp-seg-btn flex items-center gap-1.5"
        data-active={viewMode === "rendered"}
        title="Rendered view: formatted Markdown with rich blocks"
      >
        <Eye size={12} />
        <span>Rendered</span>
      </button>

      <button
        type="button"
        onclick={() => (viewMode = "split")}
        aria-pressed={viewMode === "split"}
        class="gp-seg-btn flex items-center gap-1.5"
        data-active={viewMode === "split"}
        title="Split view: raw source and live preview side by side"
      >
        <Columns size={12} />
        <span>Split</span>
      </button>

      <button
        type="button"
        onclick={() => (viewMode = "raw")}
        aria-pressed={viewMode === "raw"}
        class="gp-seg-btn flex items-center gap-1.5"
        data-active={viewMode === "raw"}
        title="Raw view: source editor with line numbers and find"
      >
        <Code size={12} />
        <span>Raw</span>
      </button>
    </div>

    <!-- Right: Stats Pill & Open in MarkDev / Actions -->
    <div class="flex items-center gap-1.5 shrink-0">
      <!-- Reading Stats Pill -->
      <div class="hidden md:block select-none">
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
        class="gp-btn text-meta"
        data-size="sm"
        title="Copy raw Markdown text"
      >
        {#if copiedSource}
          <Check size={11} class="text-ok" />
          <span class="text-ok font-semibold">Copied</span>
        {:else}
          <Copy size={11} />
          <span>Copy</span>
        {/if}
      </button>

      <button
        type="button"
        onclick={openInMarkDev}
        class="gp-btn text-meta"
        data-variant="primary"
        data-size="sm"
        title="Open in MarkDev desktop application or default editor"
      >
        <ExternalLink size={11} />
        <span>Open in MarkDev</span>
      </button>
    </div>
      </div>
    </div>
    <ScrollCue target={markdevBar} axis="x" />
  </div>

  <!-- Main Viewport Surface -->
  <div class="flex-1 flex min-h-0 overflow-hidden relative">
    <!-- Outline / TOC Drawer -->
    {#if showOutline && outline.length > 0}
      <div class="w-56 shrink-0 h-full bg-surface/50 p-3 overflow-y-auto gp-scroll select-none">
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
    {/if}

    <!-- Content Workspace by View Mode -->
    {#if viewMode === "rendered"}
      <!-- Fully Rendered View -->
      <div
        bind:this={previewContainerEl}
        class="flex-1 min-h-0 min-w-0 p-8 overflow-auto gp-scroll bg-background select-text {wordWrap ? 'wrap-break-word' : ''}"
      >
        <div class="gp-card p-8 max-w-4xl mx-auto shadow-card">
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
      <div class="flex-1 min-h-0 h-full">
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
        />
      </div>
    {:else if viewMode === "split"}
      <!-- Side-by-Side Split View -->
      <div class="flex-1 flex min-h-0 h-full gap-2">
        <!-- Left: Raw Editor Pane -->
        <div class="flex-1 min-w-0 h-full">
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
          />
        </div>

        <!-- Right: Live Rendered Preview Pane -->
        <div
          bind:this={previewContainerEl}
          class="flex-1 min-w-0 h-full p-6 overflow-auto gp-scroll bg-background/60 select-text {wordWrap ? 'wrap-break-word' : ''}"
        >
          <div class="gp-card p-6 max-w-2xl mx-auto shadow-card">
            {@render page()}
          </div>
        </div>
      </div>
    {/if}
  </div>
</div>
