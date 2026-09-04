<script lang="ts">
  /**
   * One cell of the Fleet grid, in whichever of its three states it is in.
   *
   * The reason this is a component rather than three inline branches repeated
   * across nine columns: the moment "not scanned" is rendered by hand in one
   * place, it eventually gets rendered as an em dash in another, and an em
   * dash is indistinguishable from a zero the reader skimmed past. One
   * component means one vocabulary — a value, a hollow "not scanned", or a
   * red "could not read" carrying its reason — across every column.
   */
  import { AlertTriangle } from "lucide-svelte";
  import { formatAge } from "../storage/format";
  import type { Cell } from "../fleet/types";

  let {
    cell,
    /** What the reader is looking at, for the failure and age tooltips. */
    label,
    /** Why this value may be a floor. Shown only when the cell says partial. */
    partialNote = "This count is a floor: the scan stopped at a budget.",
    align = "right",
    children,
  }: {
    cell: Cell<unknown>;
    label: string;
    partialNote?: string;
    align?: "left" | "right";
    children?: import("svelte").Snippet;
  } = $props();

  const now = Date.now();
  const alignClass = $derived(align === "right" ? "text-right justify-end" : "text-left justify-start");
</script>

{#if cell.kind === "read"}
  <div
    class="flex items-baseline gap-1 {alignClass} tabular-nums"
    title={cell.at !== null ? `${label} — scanned ${formatAge(cell.at, now)}` : undefined}
    data-testid="fleet-cell"
    data-state="read"
    data-partial={cell.partial ? "true" : "false"}
  >
    {@render children?.()}
    {#if cell.partial}
      <!-- A floor rendered like a total is the "capped sample presented as
           complete coverage" failure; the marker is what stops it. -->
      <span
        class="text-amber-600 dark:text-amber-400 text-[10px] font-semibold leading-none"
        title={partialNote}
        aria-label="{label}: partial — {partialNote}"
        data-testid="fleet-cell-partial">≥</span
      >
    {/if}
  </div>
{:else if cell.kind === "unscanned"}
  <div
    class="flex items-center gap-1 {alignClass} text-textMuted/70 text-[11px] italic"
    title="{label} has not been scanned for this repository."
    data-testid="fleet-cell"
    data-state="unscanned"
  >
    not scanned
  </div>
{:else}
  <div
    class="flex items-center gap-1 {alignClass} text-rose-600 dark:text-rose-400 text-[11px]"
    title="{label} could not be read: {cell.reason}"
    data-testid="fleet-cell"
    data-state="failed"
  >
    <AlertTriangle size={11} class="shrink-0" />
    <span class="truncate">could not read</span>
  </div>
{/if}
