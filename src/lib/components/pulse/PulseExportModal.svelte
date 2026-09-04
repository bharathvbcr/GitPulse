<script lang="ts">
  import { generatePulseSvgCard, type ExportCardOptions } from "../../pulse/exportCard";
  import { copyText } from "../../desktop/clipboard";
  import { trapFocus } from "../../ui/focusTrap";
  import { LAYERS } from "../../ui/layers";
  import { X, Copy, Download, Check, TriangleAlert } from "lucide-svelte";

  let {
    open = false,
    options,
    onClose,
  }: {
    open: boolean;
    options: ExportCardOptions;
    onClose: () => void;
  } = $props();

  type CopyState = "idle" | "copied" | "failed";
  let copyState = $state<CopyState>("idle");
  let copyTimer: ReturnType<typeof setTimeout> | undefined;

  const svgContent = $derived(open ? generatePulseSvgCard(options) : "");

  function resetCopyState() {
    if (copyTimer) clearTimeout(copyTimer);
    copyTimer = undefined;
    copyState = "idle";
  }

  $effect(() => {
    if (!open) resetCopyState();
    return () => {
      if (copyTimer) clearTimeout(copyTimer);
    };
  });

  function closeModal() {
    resetCopyState();
    onClose();
  }

  async function copyToClipboard() {
    copyState = (await copyText(svgContent)) ? "copied" : "failed";
    if (copyTimer) clearTimeout(copyTimer);
    copyTimer = setTimeout(() => (copyState = "idle"), 2000);
  }

  function downloadSvg() {
    const blob = new Blob([svgContent], { type: "image/svg+xml" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `${options.repoName || "gitpulse"}-pulse-card.svg`;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
  }
</script>

{#if open}
  <!-- Justified: Accessible backdrop dismisses modal on escape or background click -->
  <div
    class="fixed inset-0 bg-black/70 backdrop-blur-sm flex items-center justify-center p-4 animate-in fade-in duration-200"
    role="dialog"
    aria-modal="true"
    aria-labelledby="export-modal-title"
    tabindex="-1"
    onclick={(e) => e.target === e.currentTarget && closeModal()}
    onkeydown={(e) => e.key === "Escape" && closeModal()}
    style="z-index: {LAYERS.MODAL}"
  >
    <div
      use:trapFocus
      class="w-full max-w-4xl max-h-[calc(100vh-2rem)] min-h-0 bg-surface border border-border/80 rounded-2xl shadow-2xl flex flex-col overflow-hidden"
    >
      <!-- Modal Header -->
      <div class="flex items-center justify-between px-5 py-4 border-b border-border/50 shrink-0">
        <div>
          <h2 id="export-modal-title" class="text-base font-bold text-textPrimary">Export Pulse Summary Card</h2>
          <p class="text-xs text-textMuted mt-0.5">
            Ready for GitHub READMEs, project wikis, and documentation. Every tile carries its own
            definition, and a scan that did not run reads as an em dash rather than a zero.
          </p>
        </div>
        <button
          type="button"
          class="p-1.5 rounded-lg text-textMuted hover:text-textPrimary hover:bg-surfaceMuted transition-colors"
          onclick={closeModal}
          aria-label="Close modal"
        >
          <X size={18} />
        </button>
      </div>

      <!-- Preview Container -->
      <div class="p-6 bg-surfaceMuted/30 flex items-center justify-center min-h-0 flex-1 overflow-y-auto">
        <!-- The card is authored at a fixed 820px; scale it to the dialog rather than
             forcing a horizontal scrollbar over a preview meant to be read at a glance. -->
        <div
          class="w-full rounded-xl shadow-lg border border-border/60 overflow-hidden bg-surface [&>svg]:block [&>svg]:w-full [&>svg]:h-auto"
        >
          <!-- Justified: Pure SVG generated locally from repository metrics without external content -->
          {@html svgContent}
        </div>
      </div>

      <!-- Actions Footer -->
      <div class="flex flex-wrap items-center justify-between gap-3 px-5 py-4 border-t border-border/50 bg-surface shrink-0">
        <span class="text-xs text-textMuted">Standalone SVG • Zero external dependencies</span>
        <div class="flex items-center gap-2">
          <span class="sr-only" role="status" aria-live="polite">
            {copyState === "copied" ? "SVG copied" : copyState === "failed" ? "Copy failed" : ""}
          </span>
          <button
            type="button"
            class="px-3.5 py-1.5 rounded-lg text-xs font-medium border border-border/70 bg-surface text-textPrimary hover:bg-surfaceMuted flex items-center gap-1.5 transition-colors"
            onclick={copyToClipboard}
          >
            {#if copyState === "copied"}
              <Check size={14} class="text-emerald-400" />
              <span>Copied SVG!</span>
            {:else if copyState === "failed"}
              <TriangleAlert size={14} class="text-rose-400" />
              <span class="text-rose-400">Copy failed</span>
            {:else}
              <Copy size={14} />
              <span>Copy SVG</span>
            {/if}
          </button>
          <button
            type="button"
            class="px-3.5 py-1.5 rounded-lg text-xs font-medium bg-accent text-white hover:opacity-90 flex items-center gap-1.5 transition-opacity"
            onclick={downloadSvg}
          >
            <Download size={14} />
            <span>Download .svg</span>
          </button>
        </div>
      </div>
    </div>
  </div>
{/if}
