<script lang="ts">
  import {
    Image as ImageIcon,
    Binary,
    Maximize2,
    ExternalLink,
  } from "@lucide/svelte";
  import { openInDefaultApp } from "../../desktop/openInShell";
  import { repoStore } from "../../stores/repoStore";
  import type { LineEnding } from "../../files/types";
  import { formatError } from "../../ui/formatError";
  import { bytesFromBase64Prefix, hexDumpRows } from "../../files/hexDump";
  import CodeViewer from "./CodeViewer.svelte";
  import LazyMount from "../LazyMount.svelte";
  import ScrollCue from "../ScrollCue.svelte";
  // The Markdown reader and its 21 KB parser are only reachable through a
  // .md file, and shipped in the startup chunk regardless. Module scope: the
  // loader identity is LazyMount's cache key.
  const loadMarkDevViewer = () => import("./MarkDevViewer.svelte");

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

  // Image viewer state
  let imageZoom = $state(100);
  let imageFit = $state(true);
  let naturalWidth = $state<number | null>(null);
  let naturalHeight = $state<number | null>(null);
  let imageBar: HTMLDivElement | undefined = $state();
  let binaryBar: HTMLDivElement | undefined = $state();

  let isSvg = $derived(filePath.toLowerCase().endsWith(".svg"));
  let isMarkdown = $derived(
    filePath.toLowerCase().endsWith(".md") ||
    filePath.toLowerCase().endsWith(".markdown") ||
    filePath.toLowerCase().endsWith(".mdx")
  );

  let imageSrc = $derived.by(() => {
    if (blob.base64) {
      return `data:${blob.mime};base64,${blob.base64}`;
    }
    if (isSvg && blob.text) {
      return `data:image/svg+xml;utf8,${encodeURIComponent(blob.text)}`;
    }
    return null;
  });

  function onImageLoad(e: Event) {
    const img = e.target as HTMLImageElement;
    naturalWidth = img.naturalWidth;
    naturalHeight = img.naturalHeight;
  }

  let hexRows = $derived.by(() => {
    if (!blob.is_binary || !blob.base64) return [];
    return hexDumpRows(bytesFromBase64Prefix(blob.base64));
  });

  function openMediaInDefaultApp() {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    void openInDefaultApp(repo, filePath).catch((err) =>
      repoStore.setError(formatError(err)),
    );
  }
</script>

{#if blob.is_image || isSvg}
  <!-- Image Viewer Surface -->
  <div class="flex flex-col h-full bg-background font-sans text-xs min-h-0 select-none">
    <!-- Image Top Bar Controls -->
    <div class="relative shrink-0 bg-surface/70 select-none" data-tip-place="above">
      <div bind:this={imageBar} class="gp-header-scroll">
        <div class="flex items-center justify-between gap-2 px-3 py-2 min-w-max w-full">
      <div class="flex items-center gap-2 shrink-0">
        <ImageIcon size={13} class="text-info" />
        <span class="font-medium text-textPrimary">{blob.mime}</span>
        {#if naturalWidth && naturalHeight}
          <span class="text-faint">•</span>
          <span class="font-mono text-textMuted">{naturalWidth} × {naturalHeight} px</span>
        {/if}
      </div>

      <div class="flex items-center gap-2 shrink-0">
        <button
          type="button"
          onclick={() => { imageFit = true; imageZoom = 100; }}
          aria-pressed={imageFit}
          class="gp-btn text-meta {imageFit ? 'border-accent/65 bg-accent/15 text-accent' : ''}"
          data-size="sm"
        >
          <Maximize2 size={11} />
          <span>Fit screen</span>
        </button>

        <button
          type="button"
          onclick={() => { imageFit = false; imageZoom = 100; }}
          aria-pressed={!imageFit && imageZoom === 100}
          class="gp-btn text-meta {!imageFit && imageZoom === 100 ? 'border-accent/65 bg-accent/15 text-accent' : ''}"
          data-size="sm"
        >
          <span>1:1 actual</span>
        </button>

        <div class="flex items-center rounded-full border border-edge bg-surface px-1 gap-1">
          <button
            type="button"
            onclick={() => { imageFit = false; imageZoom = Math.max(25, imageZoom - 25); }}
            aria-label="Zoom out"
            class="gp-icon-btn text-xs"
            data-size="sm"
          >−</button>
          <span class="text-micro font-mono text-textMuted min-w-10 text-center">{imageZoom}%</span>
          <button
            type="button"
            onclick={() => { imageFit = false; imageZoom = Math.min(500, imageZoom + 25); }}
            aria-label="Zoom in"
            class="gp-icon-btn text-xs"
            data-size="sm"
          >+</button>
        </div>
      </div>
        </div>
      </div>
      <ScrollCue target={imageBar} axis="x" />
    </div>

    <!-- Image Canvas Area with Transparency Grid -->
    <div class="flex-1 min-h-0 flex items-center justify-center p-8 overflow-auto bg-background/80 relative">
      <div
        class="rounded-xl border border-edgeStrong shadow-card p-2 bg-[radial-gradient(rgb(var(--c-border)/0.6)_1px,transparent_1px)] bg-size-[16px_16px] max-w-full max-h-full flex items-center justify-center overflow-hidden"
      >
        {#if imageSrc}
          <img
            src={imageSrc}
            alt={filePath}
            onload={onImageLoad}
            class="rounded-lg {imageFit
              ? 'max-w-full max-h-[70vh] object-contain'
              : ''}"
            style={!imageFit ? `width: ${((naturalWidth || 400) * imageZoom) / 100}px;` : ""}
          />
        {/if}
      </div>
    </div>
  </div>
{:else if isMarkdown}
  <!-- MarkDev Integrated Markdown Viewer -->
  <LazyMount
    load={loadMarkDevViewer}
    name="The Markdown reader"
    props={{ filePath, blob, draftContent, dirty, onSave, onDraftChange, onRequestDiscard }}
  />
{:else if blob.is_binary}
  <!-- Binary Hex View & File Inspector -->
  <div class="flex flex-col h-full bg-background font-sans text-xs min-h-0 select-text">
    <div class="relative shrink-0 bg-surface/70 select-none" data-tip-place="above">
      <div bind:this={binaryBar} class="gp-header-scroll">
        <div class="flex items-center justify-between gap-2 px-3 py-2 min-w-max w-full">
      <div class="flex items-center gap-2 shrink-0">
        <Binary size={13} class="text-warn" />
        <span class="font-semibold text-textPrimary">Binary file</span>
        <span class="text-faint">•</span>
        <span class="font-mono text-textMuted">{blob.mime}</span>
      </div>
      <div class="flex items-center gap-2 shrink-0">
        <button
          type="button"
          onclick={openMediaInDefaultApp}
          class="gp-btn text-meta"
          data-size="sm"
        >
          <ExternalLink size={12} />
          <span>Open in external app</span>
        </button>
      </div>
        </div>
      </div>
      <ScrollCue target={binaryBar} axis="x" />
    </div>

    <div class="flex-1 min-h-0 p-4 overflow-auto gp-scroll font-mono text-meta leading-5">
      <div class="space-y-1">
        <div class="flex items-center gap-4 text-faint pb-1 select-none font-semibold min-w-max">
          <span class="w-20">Offset</span>
          <span class="flex-1">00 01 02 03 04 05 06 07  08 09 0A 0B 0C 0D 0E 0F</span>
          <span class="w-36">Decoded ASCII</span>
        </div>
        {#each hexRows as r}
          <div class="flex items-center gap-4 hover:bg-surface/60 transition-colors min-w-max">
            <span class="w-20 text-accent font-semibold select-none">{r.offset}</span>
            <span class="flex-1 text-textPrimary/90 tracking-wider">
              {r.hex.slice(0, 8).join(" ")} &nbsp; {r.hex.slice(8).join(" ")}
            </span>
            <span class="w-36 text-ok font-medium">{r.ascii}</span>
          </div>
        {/each}
      </div>
    </div>
  </div>
{:else}
  <!-- Default Code Viewer -->
  <CodeViewer
    {filePath}
    content={blob.text || ""}
    eol={blob.eol ?? null}
    invalidUtf8Bytes={blob.invalid_utf8_bytes ?? 0}
    diskBase={blob.disk_base ?? null}
    {draftContent}
    {dirty}
    {onSave}
    {onDraftChange}
    {onRequestDiscard}
  />
{/if}
