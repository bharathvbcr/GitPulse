<script lang="ts">
  import {
    Image as ImageIcon,
    Binary,
    Maximize2,
    ExternalLink,
  } from "lucide-svelte";
  import { openPath } from "@tauri-apps/plugin-opener";
  import { repoStore } from "../../stores/repoStore";
  import { joinWorktreePath } from "../../files/fileTree";
  import { bytesFromBase64Prefix, hexDumpRows } from "../../files/hexDump";
  import CodeViewer from "./CodeViewer.svelte";
  import MarkDevViewer from "./MarkDevViewer.svelte";

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

  // Image viewer state
  let imageZoom = $state(100);
  let imageFit = $state(true);
  let naturalWidth = $state<number | null>(null);
  let naturalHeight = $state<number | null>(null);

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

  function openInDefaultApp() {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    const fullPath = joinWorktreePath(repo, filePath);
    if (!fullPath) {
      repoStore.setError("Cannot open a path outside the repository");
      return;
    }
    void openPath(fullPath);
  }
</script>

{#if blob.is_image || isSvg}
  <!-- Image Viewer Surface -->
  <div class="flex flex-col h-full bg-background font-sans text-xs min-h-0 select-none">
    <!-- Image Top Bar Controls -->
    <div class="flex items-center justify-between px-3 py-2 border-b border-border/70 bg-surface/70 shrink-0">
      <div class="flex items-center gap-2">
        <ImageIcon size={13} class="text-teal-400" />
        <span class="font-medium text-textPrimary">{blob.mime}</span>
        {#if naturalWidth && naturalHeight}
          <span class="text-textMuted/60">•</span>
          <span class="font-mono text-textMuted">{naturalWidth} × {naturalHeight} px</span>
        {/if}
      </div>

      <div class="flex items-center gap-2">
        <button
          type="button"
          onclick={() => { imageFit = true; imageZoom = 100; }}
          class="gp-btn !py-0.5 !px-2 text-[11px] {imageFit ? 'border-accent/60 bg-accent/15 text-accent' : ''}"
        >
          <Maximize2 size={11} />
          <span>Fit Screen</span>
        </button>

        <button
          type="button"
          onclick={() => { imageFit = false; imageZoom = 100; }}
          class="gp-btn !py-0.5 !px-2 text-[11px] {!imageFit && imageZoom === 100 ? 'border-accent/60 bg-accent/15 text-accent' : ''}"
        >
          <span>1:1 Actual</span>
        </button>

        <div class="flex items-center rounded-full border border-border/70 bg-surface px-2 py-0.5 gap-1.5">
          <button
            type="button"
            onclick={() => { imageFit = false; imageZoom = Math.max(25, imageZoom - 25); }}
            class="text-textMuted hover:text-textPrimary text-xs px-1"
          >−</button>
          <span class="text-[10px] font-mono text-textMuted min-w-10 text-center">{imageZoom}%</span>
          <button
            type="button"
            onclick={() => { imageFit = false; imageZoom = Math.min(500, imageZoom + 25); }}
            class="text-textMuted hover:text-textPrimary text-xs px-1"
          >+</button>
        </div>
      </div>
    </div>

    <!-- Image Canvas Area with Transparency Grid -->
    <div class="flex-1 min-h-0 flex items-center justify-center p-8 overflow-auto bg-background/80 relative">
      <div
        class="rounded-xl border border-border/80 shadow-card p-2 bg-[radial-gradient(#334155_1px,transparent_1px)] [background-size:16px_16px] dark:bg-[radial-gradient(#1e293b_1px,transparent_1px)] max-w-full max-h-full flex items-center justify-center overflow-hidden"
      >
        {#if imageSrc}
          <img
            src={imageSrc}
            alt={filePath}
            onload={onImageLoad}
            class="transition-all duration-150 rounded-lg {imageFit
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
  <MarkDevViewer {filePath} {blob} {onSave} />
{:else if blob.is_binary}
  <!-- Binary Hex View & File Inspector -->
  <div class="flex flex-col h-full bg-background font-sans text-xs min-h-0 select-text">
    <div class="flex items-center justify-between px-3 py-2 border-b border-border/70 bg-surface/70 shrink-0">
      <div class="flex items-center gap-2">
        <Binary size={13} class="text-amber-400" />
        <span class="font-semibold text-textPrimary">Binary File</span>
        <span class="text-textMuted/60">•</span>
        <span class="font-mono text-textMuted">{blob.mime}</span>
      </div>
      <div class="flex items-center gap-2">
        <button
          type="button"
          onclick={openInDefaultApp}
          class="gp-btn !py-0.5 !px-2.5 flex items-center gap-1 text-[11px]"
        >
          <ExternalLink size={12} />
          <span>Open in External App</span>
        </button>
      </div>
    </div>

    <div class="flex-1 min-h-0 p-4 overflow-auto gp-scroll font-mono text-[11px] leading-5">
      <div class="space-y-1">
        <div class="flex items-center gap-4 text-textMuted/60 border-b border-border/60 pb-1 select-none font-bold">
          <span class="w-20">Offset</span>
          <span class="flex-1">00 01 02 03 04 05 06 07  08 09 0A 0B 0C 0D 0E 0F</span>
          <span class="w-36">Decoded ASCII</span>
        </div>
        {#each hexRows as r}
          <div class="flex items-center gap-4 hover:bg-surface/60 transition-colors">
            <span class="w-20 text-accent/80 font-bold select-none">{r.offset}</span>
            <span class="flex-1 text-textPrimary/90 tracking-wider">
              {r.hex.slice(0, 8).join(" ")} &nbsp; {r.hex.slice(8).join(" ")}
            </span>
            <span class="w-36 text-emerald-400 font-medium">{r.ascii}</span>
          </div>
        {/each}
      </div>
    </div>
  </div>
{:else}
  <!-- Default Code Viewer -->
  <CodeViewer {filePath} content={blob.text || ""} {onSave} />
{/if}
