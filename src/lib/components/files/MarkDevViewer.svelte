<script lang="ts">
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
  } from "lucide-svelte";
  import { openPath } from "@tauri-apps/plugin-opener";
  import { repoStore } from "../../stores/repoStore";
  import { copyText } from "../../desktop/clipboard";
  import {
    renderMarkDevMarkdown,
    calculateDocumentStats,
    extractDocumentOutline,
    type DocumentStats,
    type MarkdownHeading,
  } from "../../files/markDevParser";
  import CodeViewer from "./CodeViewer.svelte";
  import MarkDevLogo from "./MarkDevLogo.svelte";

  let {
    filePath,
    blob,
    onSave,
  }: {
    filePath: string;
    blob: {
      path: string;
      is_binary: boolean;
      is_image: boolean;
      mime: string;
      text?: string | null;
      base64?: string | null;
    };
    onSave?: (newContent: string) => Promise<void>;
  } = $props();

  export type MarkDevViewMode = "rendered" | "split" | "raw";

  let viewMode = $state<MarkDevViewMode>("rendered");
  let showOutline = $state(false);
  let wordWrap = $state(true);
  let copiedSource = $state(false);

  let rawContent = $derived(blob.text || "");
  let renderedHtml = $derived(renderMarkDevMarkdown(rawContent));
  let stats = $derived<DocumentStats>(calculateDocumentStats(rawContent));
  let outline = $derived<MarkdownHeading[]>(extractDocumentOutline(rawContent));

  let previewContainerEl: HTMLDivElement | undefined = $state();

  async function handleCopySource() {
    await copyText(rawContent);
    copiedSource = true;
    setTimeout(() => (copiedSource = false), 1800);
  }

  async function openInMarkDev() {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    const fullPath = `${repo}/${filePath}`;
    try {
      await openPath(fullPath);
    } catch {
      // Graceful fallback
    }
  }

  function scrollToHeading(id: string) {
    if (!previewContainerEl) return;
    const target = previewContainerEl.querySelector(`#${id}`);
    if (target) {
      target.scrollIntoView({ behavior: "smooth", block: "start" });
    }
  }

  // Handle copy button clicks inside rendered code blocks via event delegation
  function handlePreviewClick(e: MouseEvent) {
    const target = (e.target as HTMLElement).closest(".copy-code-btn") as HTMLButtonElement | null;
    if (target) {
      const code = target.getAttribute("data-code");
      if (code) {
        void copyText(code);
        const originalText = target.innerText;
        target.innerText = "Copied ✓";
        target.classList.add("!text-emerald-400", "!border-emerald-500/50");
        setTimeout(() => {
          target.innerText = originalText;
          target.classList.remove("!text-emerald-400", "!border-emerald-500/50");
        }, 1800);
      }
    }
  }

  $effect(() => {
    if (!previewContainerEl) return;
    const el = previewContainerEl;
    el.addEventListener("click", handlePreviewClick);
    return () => {
      el.removeEventListener("click", handlePreviewClick);
    };
  });
</script>

<div class="flex flex-col h-full bg-background font-sans text-xs min-h-0 select-text relative">
  <!-- MarkDev Integrated Header Toolbar -->
  <div class="flex items-center justify-between px-3 py-1.5 border-b border-border/70 bg-surface/80 shrink-0 select-none gap-2">
    <!-- Left: MarkDev Brand & Outline Toggle -->
    <div class="flex items-center gap-2 shrink-0">
      <div class="flex items-center gap-1.5 py-0.5 px-2 rounded-full bg-surface border border-border/70 shadow-sm">
        <MarkDevLogo size={15} />
        <span class="font-bold text-textPrimary tracking-tight text-[11px]">MarkDev</span>
        <span class="text-[9px] font-mono uppercase px-1 py-0.2 rounded bg-accent/20 text-accent font-semibold">.MD</span>
      </div>

      {#if outline.length > 0}
        <button
          type="button"
          onclick={() => (showOutline = !showOutline)}
          title="{showOutline ? 'Hide' : 'Show'} Table of Contents"
          class="gp-btn !py-0.5 !px-2 flex items-center gap-1 text-[11px] {showOutline ? 'border-accent/60 bg-accent/15 text-accent font-semibold' : ''}"
        >
          <ListTree size={12} />
          <span>Outline</span>
          <span class="text-[10px] opacity-60 font-mono">({outline.length})</span>
        </button>
      {/if}
    </div>

    <!-- Center: MarkDev Tri-Mode Switcher (Rendered / Split / Raw) -->
    <div class="gp-segmented">
      <button
        type="button"
        onclick={() => (viewMode = "rendered")}
        class="gp-seg-btn flex items-center gap-1.5"
        data-active={viewMode === "rendered"}
        title="Rendered View: Formatted Markdown with rich blocks"
      >
        <Eye size={12} />
        <span>Rendered</span>
      </button>

      <button
        type="button"
        onclick={() => (viewMode = "split")}
        class="gp-seg-btn flex items-center gap-1.5"
        data-active={viewMode === "split"}
        title="Split View: Raw source code and live preview side-by-side"
      >
        <Columns size={12} />
        <span>Split</span>
      </button>

      <button
        type="button"
        onclick={() => (viewMode = "raw")}
        class="gp-seg-btn flex items-center gap-1.5"
        data-active={viewMode === "raw"}
        title="Raw View: Source code editor with line numbers and find"
      >
        <Code size={12} />
        <span>Raw</span>
      </button>
    </div>

    <!-- Right: Stats Pill & Open in MarkDev / Actions -->
    <div class="flex items-center gap-1.5 shrink-0">
      <!-- Reading Stats Pill -->
      <div class="hidden md:flex items-center gap-2 px-2.5 py-0.5 rounded-full bg-surface border border-border/60 text-[10px] font-mono text-textMuted select-none">
        <span class="flex items-center gap-1">
          <BookOpen size={10} class="text-accent" />
          <span>{stats.wordCount} words</span>
        </span>
        <span class="text-textMuted/40">•</span>
        <span class="flex items-center gap-1">
          <Clock size={10} class="text-amber-400" />
          <span>~{stats.readingTimeMinutes} min</span>
        </span>
      </div>

      {#if viewMode !== "raw"}
        <button
          type="button"
          onclick={() => (wordWrap = !wordWrap)}
          class="gp-icon-btn !p-1.5 {wordWrap ? 'text-accent bg-accent/15' : 'text-textMuted hover:text-textPrimary'}"
          title="Toggle Word Wrap"
        >
          <WrapText size={12} />
        </button>
      {/if}

      <button
        type="button"
        onclick={handleCopySource}
        class="gp-btn !py-0.5 !px-2 flex items-center gap-1 text-[11px]"
        title="Copy raw markdown text"
      >
        {#if copiedSource}
          <Check size={11} class="text-emerald-400" />
          <span class="text-emerald-400 font-semibold">Copied</span>
        {:else}
          <Copy size={11} />
          <span>Copy</span>
        {/if}
      </button>

      <button
        type="button"
        onclick={openInMarkDev}
        class="gp-btn-primary !py-0.5 !px-2.5 flex items-center gap-1 text-[11px]"
        title="Open in MarkDev desktop application or default editor"
      >
        <ExternalLink size={11} />
        <span>Open in MarkDev</span>
      </button>
    </div>
  </div>

  <!-- Main Viewport Surface -->
  <div class="flex-1 flex min-h-0 overflow-hidden relative">
    <!-- Outline / TOC Drawer -->
    {#if showOutline && outline.length > 0}
      <div class="w-56 shrink-0 h-full border-r border-border/70 bg-surface/50 p-3 overflow-y-auto gp-scroll select-none">
        <div class="text-[10px] font-mono uppercase tracking-wider text-textMuted mb-2 flex items-center gap-1.5 font-bold">
          <Sparkles size={11} class="text-accent" />
          <span>Document Outline</span>
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
              <span class="text-[10px] font-mono text-accent/70 mr-1">{"#".repeat(h.level)}</span>
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
        class="flex-1 min-h-0 p-8 overflow-y-auto gp-scroll bg-background select-text {wordWrap ? 'break-words' : ''}"
      >
        <div class="gp-card p-8 border-border/60 max-w-4xl mx-auto shadow-card">
          {@html renderedHtml}
        </div>
      </div>
    {:else if viewMode === "raw"}
      <!-- Full Raw Editor View -->
      <div class="flex-1 min-h-0 h-full">
        <CodeViewer {filePath} content={rawContent} {onSave} />
      </div>
    {:else if viewMode === "split"}
      <!-- Side-by-Side Split View -->
      <div class="flex-1 flex min-h-0 h-full divide-x divide-border/70">
        <!-- Left: Raw Editor Pane -->
        <div class="flex-1 min-w-0 h-full">
          <CodeViewer {filePath} content={rawContent} {onSave} />
        </div>

        <!-- Right: Live Rendered Preview Pane -->
        <div
          bind:this={previewContainerEl}
          class="flex-1 min-w-0 h-full p-6 overflow-y-auto gp-scroll bg-background/60 select-text {wordWrap ? 'break-words' : ''}"
        >
          <div class="gp-card p-6 border-border/60 max-w-2xl mx-auto shadow-card">
            {@html renderedHtml}
          </div>
        </div>
      </div>
    {/if}
  </div>
</div>
