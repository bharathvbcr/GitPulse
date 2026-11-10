<script lang="ts">
  import { generatePulseSvgCard, type ExportCardOptions } from "../../pulse/exportCard";
  import { copyText } from "../../desktop/clipboard";
  import Dialog from "../Dialog.svelte";
  import { Copy, Download, Check, TriangleAlert } from "@lucide/svelte";

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

  // The card is generated only while the dialog is open. The last card is held
  // (a plain variable, not state) so the exit transition fades the preview out
  // instead of emptying it under the fading card.
  let lastCard = "";
  const svgContent = $derived.by(() => {
    if (open) lastCard = generatePulseSvgCard(options);
    return lastCard;
  });

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

<!-- Dialog owns the scrim, the stacking, the focus trap, Escape, the scrim
     click and the enter and exit transitions. -->
<Dialog {open} size="lg" title="Export Pulse summary card" onclose={closeModal}>
  <p class="m-0 mb-3 text-xs text-textMuted">
    Ready for GitHub READMEs, repository wikis, and documentation. Every tile carries its own
    definition, and a scan that did not run reads as an em dash rather than a zero.
  </p>

  <!-- The card is authored at a fixed 820px; scale it to the dialog rather than
       forcing a horizontal scrollbar over a preview meant to be read at a glance. -->
  <div
    class="w-full rounded-xl shadow-lg border border-edgeSoft overflow-hidden bg-surface [&>svg]:block [&>svg]:w-full [&>svg]:h-auto"
  >
    <!-- Justified: Pure SVG generated locally from repository metrics without external content -->
    {@html svgContent}
  </div>

  {#snippet footer()}
    <span class="mr-auto text-xs text-textMuted">Standalone SVG, no external dependencies</span>
    <span class="sr-only" role="status" aria-live="polite">
      {copyState === "copied" ? "Copied the SVG" : copyState === "failed" ? "Couldn't copy the SVG" : ""}
    </span>
    <button type="button" class="gp-btn" data-size="md" onclick={copyToClipboard}>
      {#if copyState === "copied"}
        <Check size={14} class="text-ok" />
        <span>Copied</span>
      {:else if copyState === "failed"}
        <TriangleAlert size={14} class="text-danger" />
        <span class="text-danger">Couldn't copy the SVG</span>
      {:else}
        <Copy size={14} />
        <span>Copy SVG</span>
      {/if}
    </button>
    <button type="button" class="gp-btn" data-variant="primary" data-size="md" onclick={downloadSvg}>
      <Download size={14} />
      <span>Download .svg</span>
    </button>
  {/snippet}
</Dialog>
