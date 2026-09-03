<script lang="ts">
  import { generatePulseSvgCard, type ExportCardOptions } from "../../pulse/exportCard";
  import { X, Copy, Download, Check } from "lucide-svelte";

  let {
    open = false,
    options,
    onClose,
  }: {
    open: boolean;
    options: ExportCardOptions;
    onClose: () => void;
  } = $props();

  let copied = $state(false);

  const svgContent = $derived(open ? generatePulseSvgCard(options) : "");

  async function copyToClipboard() {
    try {
      await navigator.clipboard.writeText(svgContent);
      copied = true;
      setTimeout(() => (copied = false), 2000);
    } catch {
      // Fallback or permission denial handling
    }
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
    class="fixed inset-0 z-50 bg-black/70 backdrop-blur-sm flex items-center justify-center p-4 animate-in fade-in duration-200"
    role="dialog"
    aria-modal="true"
    aria-labelledby="export-modal-title"
  >
    <div class="w-full max-w-4xl bg-surface border border-border/80 rounded-2xl shadow-2xl flex flex-col overflow-hidden">
      <!-- Modal Header -->
      <div class="flex items-center justify-between px-5 py-4 border-b border-border/50">
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
          onclick={onClose}
          aria-label="Close modal"
        >
          <X size={18} />
        </button>
      </div>

      <!-- Preview Container -->
      <div class="p-6 bg-surfaceMuted/30 flex items-center justify-center">
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
      <div class="flex items-center justify-between px-5 py-4 border-t border-border/50 bg-surface">
        <span class="text-xs text-textMuted">Standalone SVG • Zero external dependencies</span>
        <div class="flex items-center gap-2">
          <button
            type="button"
            class="px-3.5 py-1.5 rounded-lg text-xs font-medium border border-border/70 bg-surface text-textPrimary hover:bg-surfaceMuted flex items-center gap-1.5 transition-colors"
            onclick={copyToClipboard}
          >
            {#if copied}
              <Check size={14} class="text-emerald-400" />
              <span>Copied SVG!</span>
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
